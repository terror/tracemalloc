#![allow(unsafe_op_in_unsafe_fn)]

use {
  pyo3::{
    Bound, Py,
    exceptions::{PyRuntimeError, PyTypeError, PyValueError},
    ffi,
    ffi::PyMemAllocatorDomain::{
      PYMEM_DOMAIN_MEM, PYMEM_DOMAIN_OBJ, PYMEM_DOMAIN_RAW,
    },
    prelude::*,
    types::{
      PyAny, PyDict, PyFloat, PyFrame, PyInt, PyList, PyModule, PyString,
    },
  },
  std::{
    cell::Cell,
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    mem::MaybeUninit,
    os::raw::c_void,
    path::PathBuf,
    ptr,
    sync::{Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
  },
  tracemalloc::{
    ExportError, FrameMetadata, MmapJsonStreamWriter, Snapshot, SnapshotDelta,
    SnapshotRecord, Tracer,
  },
};

#[cfg(windows)]
use pyo3::exceptions::PyNotImplementedError;

static STATE: RwLock<Option<Box<ShimState>>> = RwLock::new(None);

thread_local! {
  static IN_TRACER: Cell<bool> = Cell::new(false);
}

#[derive(Default)]
struct SamplingOptions {
  rate: Option<f64>,
  bytes: Option<u64>,
}

fn parse_sampling(value: Option<&Bound<'_, PyAny>>) -> PyResult<SamplingOptions> {
  let mut options = SamplingOptions::default();

  let Some(obj) = value else {
    return Ok(options);
  };

  if obj.is_none() {
    return Ok(options);
  }

  if let Ok(dict) = obj.downcast::<PyDict>() {
    if let Some(rate_obj) = dict.get_item("rate")? {
      options.rate = Some(rate_obj.extract::<f64>()?);
    }

    if let Some(bytes_obj) = dict.get_item("bytes")? {
      options.bytes = Some(bytes_obj.extract::<u64>()?);
    }

    return Ok(options);
  }

  if obj.is_instance_of::<PyFloat>() {
    options.rate = Some(obj.extract::<f64>()?);
    return Ok(options);
  }

  if obj.is_instance_of::<PyInt>() {
    options.bytes = Some(obj.extract::<u64>()?);
    return Ok(options);
  }

  Err(PyTypeError::new_err(
    "sampling must be a float probability, integer byte interval, or mapping with 'rate'/'bytes'",
  ))
}

fn parse_exporters(exporters: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<String>> {
  let Some(obj) = exporters else {
    return Ok(Vec::new());
  };

  if obj.is_none() {
    return Ok(Vec::new());
  }

  if let Ok(py_str) = obj.downcast::<PyString>() {
    return Ok(vec![py_str.to_string_lossy().into_owned()]);
  }

  if let Ok(iterator) = obj.iter() {
    let mut parsed = Vec::new();

    for item in iterator {
      let value = item?;

      let py_str = value.downcast::<PyString>().map_err(|_| {
        PyTypeError::new_err("exporters must be strings or iterables of strings")
      })?;

      parsed.push(py_str.to_string_lossy().into_owned());
    }

    return Ok(parsed);
  }

  Err(PyTypeError::new_err(
    "exporters must be a string or iterable of strings",
  ))
}

#[pyfunction]
#[pyo3(signature = (
  nframe=None,
  capture_native=None,
  sampling=None,
  ring_buffer_bytes=None,
  exporters=None
))]
fn start(
  py: Python<'_>,
  nframe: Option<u16>,
  capture_native: Option<bool>,
  sampling: Option<Bound<'_, PyAny>>,
  ring_buffer_bytes: Option<usize>,
  exporters: Option<Bound<'_, PyAny>>,
) -> PyResult<()> {
  let mut guard = STATE
    .write()
    .map_err(|_| PyRuntimeError::new_err("tracer state poisoned"))?;

  if guard.is_some() {
    return Ok(());
  }

  let sampling_options = parse_sampling(sampling.as_ref())?;
  let exporter_list = parse_exporters(exporters.as_ref())?;

  let mut builder = Tracer::builder().start_enabled(true);

  if let Some(depth) = nframe {
    builder = builder.max_stack_depth(depth);
  }

  if let Some(native) = capture_native {
    builder = builder.capture_native(native);
  } else {
    builder = builder.capture_native(false);
  }

  if let Some(bytes) = sampling_options.bytes {
    builder = builder.sampling_bytes(Some(bytes));
  }

  if sampling_options.bytes.is_none() {
    if let Some(rate) = sampling_options.rate {
      builder = builder.sampling_rate(rate);
    }
  }

  if let Some(bytes) = ring_buffer_bytes {
    builder = builder.ring_buffer_bytes(bytes);
  }

  let tracer = builder.finish();

  let mut state = Box::new(ShimState::new(tracer, exporter_list));

  unsafe {
    state.install(py)?;
  }

  *guard = Some(state);

  Ok(())
}

#[pyfunction]
fn stop(_py: Python<'_>) -> PyResult<()> {
  let mut guard = STATE
    .write()
    .map_err(|_| PyRuntimeError::new_err("tracer state poisoned"))?;

  if let Some(state) = guard.take() {
    unsafe {
      state.restore()?;
    }

    state.tracer.disable();
    state.clear_allocations();
  }

  Ok(())
}

#[pyfunction]
fn is_tracing() -> bool {
  STATE
    .read()
    .ok()
    .and_then(|guard| guard.as_ref().map(|state| state.tracer.enabled()))
    .unwrap_or(false)
}

#[pyfunction]
fn take_snapshot(_py: Python<'_>) -> PyResult<PySnapshot> {
  let guard = STATE
    .read()
    .map_err(|_| PyRuntimeError::new_err("tracer state poisoned"))?;

  let state = guard
    .as_ref()
    .ok_or_else(|| PyRuntimeError::new_err("tracer not started"))?;

  let snapshot = state.tracer.snapshot();

  Ok(PySnapshot::from(snapshot))
}

fn snapshot_records_to_python<'py>(
  py: Python<'py>,
  records: &[SnapshotRecord],
) -> PyResult<Bound<'py, PyList>> {
  let list = PyList::empty_bound(py);

  for record in records {
    let entry = PyDict::new_bound(py);
    entry.set_item("stack_id", record.stack_id)?;
    entry.set_item("current_bytes", record.current_bytes)?;
    entry.set_item("allocations", record.allocations)?;
    entry.set_item("deallocations", record.deallocations)?;
    entry.set_item("total_allocated", record.total_allocated)?;
    entry.set_item("total_freed", record.total_freed)?;

    if let Some(stack) = &record.stack {
      let frames = PyList::empty_bound(py);

      for frame in stack.frames() {
        let frame_dict = PyDict::new_bound(py);
        frame_dict.set_item("filename", frame.filename.as_ref())?;
        frame_dict.set_item("function", frame.function.as_ref())?;
        frame_dict.set_item("lineno", frame.lineno)?;
        frames.append(frame_dict.unbind())?;
      }

      entry.set_item("frames", frames.unbind())?;
    }

    list.append(entry.unbind())?;
  }

  Ok(list)
}

fn snapshot_to_python(py: Python<'_>, snapshot: &Snapshot) -> PyResult<PyObject> {
  let records = snapshot_records_to_python(py, snapshot.records())?;
  let result = PyDict::new_bound(py);
  result.set_item("records", records)?;
  result.set_item("dropped_events", snapshot.dropped_events())?;
  Ok(result.unbind().into())
}

fn snapshot_delta_to_python(
  py: Python<'_>,
  delta: &SnapshotDelta,
) -> PyResult<PyObject> {
  let records = snapshot_records_to_python(py, delta.records())?;
  let result = PyDict::new_bound(py);
  result.set_item("records", records)?;
  result.set_item("dropped_events", delta.dropped_events())?;
  Ok(result.unbind().into())
}

fn extract_path(path: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
  path
    .extract::<PathBuf>()
    .map_err(|_| PyTypeError::new_err("path must be str or os.PathLike"))
}

fn optional_timestamp(seconds: Option<f64>) -> PyResult<Option<SystemTime>> {
  let Some(value) = seconds else {
    return Ok(None);
  };

  if value.is_sign_negative() {
    return Err(PyValueError::new_err(
      "timestamp must be greater than or equal to zero",
    ));
  }

  let duration = Duration::from_secs_f64(value);

  Ok(Some(UNIX_EPOCH + duration))
}

fn export_error(err: ExportError) -> PyErr {
  PyRuntimeError::new_err(err.to_string())
}

fn vec_to_utf8(bytes: Vec<u8>) -> PyResult<String> {
  String::from_utf8(bytes)
    .map_err(|err| PyRuntimeError::new_err(err.to_string()))
}

#[pyclass(name = "Snapshot", module = "rust_tracemalloc")]
pub struct PySnapshot {
  inner: Snapshot,
}

impl From<Snapshot> for PySnapshot {
  fn from(inner: Snapshot) -> Self {
    Self { inner }
  }
}

#[pymethods]
impl PySnapshot {
  #[getter]
  fn dropped_events(&self) -> u64 {
    self.inner.dropped_events()
  }

  fn records(&self, py: Python<'_>) -> PyResult<PyObject> {
    Ok(snapshot_records_to_python(py, self.inner.records())?
      .unbind()
      .into())
  }

  fn to_dict(&self, py: Python<'_>) -> PyResult<PyObject> {
    snapshot_to_python(py, &self.inner)
  }

  fn compare_to(&self, other: &PySnapshot) -> PySnapshotDelta {
    SnapshotDelta::from_snapshots(&self.inner, &other.inner).into()
  }

  fn to_json(&self) -> PyResult<String> {
    let mut buffer = Vec::new();

    self
      .inner
      .export_json(&mut buffer)
      .map_err(export_error)?;

    vec_to_utf8(buffer)
  }

  fn export_json(&self, path: &Bound<'_, PyAny>) -> PyResult<()> {
    let path = extract_path(path)?;

    let file =
      File::create(&path).map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    let mut writer = BufWriter::new(file);

    self
      .inner
      .export_json(&mut writer)
      .map_err(export_error)?;

    writer
      .flush()
      .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    Ok(())
  }

  #[cfg(not(windows))]
  fn export_pprof(&self, path: &Bound<'_, PyAny>) -> PyResult<()> {
    let path = extract_path(path)?;

    let file =
      File::create(&path).map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    let mut writer = BufWriter::new(file);

    self
      .inner
      .export_pprof(&mut writer)
      .map_err(export_error)?;

    writer
      .flush()
      .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    Ok(())
  }

  #[cfg(windows)]
  fn export_pprof(&self, _path: &Bound<'_, PyAny>) -> PyResult<()> {
    Err(PyNotImplementedError::new_err(
      "pprof export is not available on Windows",
    ))
  }

  fn stream_to_mmap(
    &self,
    path: &Bound<'_, PyAny>,
    capacity: usize,
    timestamp: Option<f64>,
  ) -> PyResult<()> {
    let path = extract_path(path)?;

    let mut writer = MmapJsonStreamWriter::create(&path, capacity)
      .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    let ts = optional_timestamp(timestamp)?;

    self
      .inner
      .stream_into(&mut writer, ts)
      .map_err(export_error)?;

    writer
      .flush()
      .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    Ok(())
  }
}

#[pyclass(name = "SnapshotDelta", module = "rust_tracemalloc")]
pub struct PySnapshotDelta {
  inner: SnapshotDelta,
}

impl From<SnapshotDelta> for PySnapshotDelta {
  fn from(inner: SnapshotDelta) -> Self {
    Self { inner }
  }
}

#[pymethods]
impl PySnapshotDelta {
  #[getter]
  fn dropped_events(&self) -> i64 {
    self.inner.dropped_events()
  }

  fn records(&self, py: Python<'_>) -> PyResult<PyObject> {
    Ok(snapshot_records_to_python(py, self.inner.records())?
      .unbind()
      .into())
  }

  fn to_dict(&self, py: Python<'_>) -> PyResult<PyObject> {
    snapshot_delta_to_python(py, &self.inner)
  }

  fn to_json(&self) -> PyResult<String> {
    let mut buffer = Vec::new();

    self
      .inner
      .export_json(&mut buffer)
      .map_err(export_error)?;

    vec_to_utf8(buffer)
  }

  fn export_json(&self, path: &Bound<'_, PyAny>) -> PyResult<()> {
    let path = extract_path(path)?;

    let file =
      File::create(&path).map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    let mut writer = BufWriter::new(file);

    self
      .inner
      .export_json(&mut writer)
      .map_err(export_error)?;

    writer
      .flush()
      .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

    Ok(())
  }
}

#[pyclass(module = "rust_tracemalloc")]
pub struct SnapshotStream {
  writer: Option<MmapJsonStreamWriter>,
}

impl SnapshotStream {
  fn new(writer: MmapJsonStreamWriter) -> Self {
    Self {
      writer: Some(writer),
    }
  }
}

#[pymethods]
impl SnapshotStream {
  fn write(&mut self, snapshot: &PySnapshot, timestamp: Option<f64>) -> PyResult<()> {
    let writer = self
      .writer
      .as_mut()
      .ok_or_else(|| PyRuntimeError::new_err("snapshot stream is closed"))?;

    let ts = optional_timestamp(timestamp)?;

    snapshot
      .inner
      .stream_into(writer, ts)
      .map_err(export_error)?;

    Ok(())
  }

  fn flush(&self) -> PyResult<()> {
    if let Some(writer) = &self.writer {
      writer
        .flush()
        .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;
    }

    Ok(())
  }

  fn close(&mut self) {
    self.writer = None;
  }
}

#[pyfunction]
fn compare_snapshots(
  newer: &PySnapshot,
  older: &PySnapshot,
) -> PySnapshotDelta {
  SnapshotDelta::from_snapshots(&newer.inner, &older.inner).into()
}

#[pyfunction]
#[pyo3(signature = (path, capacity))]
fn open_snapshot_stream(
  path: Bound<'_, PyAny>,
  capacity: usize,
) -> PyResult<SnapshotStream> {
  let path = extract_path(&path)?;

  let writer = MmapJsonStreamWriter::create(&path, capacity)
    .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;

  Ok(SnapshotStream::new(writer))
}

struct ShimState {
  tracer: Tracer,
  frame_depth: usize,
  frame_skip: usize,
  allocations: Mutex<HashMap<usize, usize>>,
  contexts: [AllocatorContext; 3],
  _exporters: Vec<String>,
}

unsafe impl Send for ShimState {}
unsafe impl Sync for ShimState {}

impl ShimState {
  fn new(tracer: Tracer, exporters: Vec<String>) -> Self {
    let config = tracer.config().clone();

    let frame_depth = usize::from(config.max_stack_depth.max(1));

    Self {
      tracer,
      frame_depth,
      frame_skip: config.python_skip_frames,
      allocations: Mutex::new(HashMap::new()),
      contexts: [
        AllocatorContext::new(PYMEM_DOMAIN_RAW),
        AllocatorContext::new(PYMEM_DOMAIN_MEM),
        AllocatorContext::new(PYMEM_DOMAIN_OBJ),
      ],
      _exporters: exporters,
    }
  }

  fn with_guard<F, R>(&self, func: F) -> Option<R>
  where
    F: FnOnce() -> R,
  {
    IN_TRACER.with(|flag| {
      if flag.get() {
        None
      } else {
        flag.set(true);

        struct Reset<'a>(&'a Cell<bool>);

        impl<'a> Drop for Reset<'a> {
          fn drop(&mut self) {
            self.0.set(false);
          }
        }

        let _reset = Reset(flag);

        Some(func())
      }
    })
  }

  unsafe fn install(&mut self, _py: Python<'_>) -> PyResult<()> {
    let state_ptr: *const ShimState = self;

    for context in &mut self.contexts {
      context.state = state_ptr;

      unsafe {
        context.install()?;
      }
    }

    Ok(())
  }

  unsafe fn restore(&self) -> PyResult<()> {
    for context in &self.contexts {
      unsafe {
        context.restore()?;
      }
    }

    Ok(())
  }

  fn clear_allocations(&self) {
    if let Ok(mut guard) = self.allocations.lock() {
      guard.clear();
    }
  }

  fn record_allocation(&self, address: usize, size: usize) {
    if size == 0 {
      return;
    }

    let _ = self.with_guard(|| {
      if !self.tracer.enabled() {
        return;
      }

      let frames = self.collect_python_frames();

      if frames.is_empty() {
        self.tracer.record_allocation(address, size);
      } else {
        self
          .tracer
          .record_allocation_with_frames(address, size, &frames);
      }

      if let Ok(mut guard) = self.allocations.lock() {
        guard.insert(address, size);
      }
    });
  }

  fn record_deallocation(&self, address: usize) {
    let _ = self.with_guard(|| {
      let size = {
        if let Ok(mut guard) = self.allocations.lock() {
          guard.remove(&address).unwrap_or(0)
        } else {
          0
        }
      };

      if self.tracer.enabled() {
        self.tracer.record_deallocation(address, size);
      }
    });
  }

  fn record_reallocation(
    &self,
    old_address: usize,
    new_address: usize,
    new_size: usize,
  ) {
    let _ = self.with_guard(|| {
      let old_size = {
        if let Ok(guard) = self.allocations.lock() {
          guard.get(&old_address).copied().unwrap_or(0)
        } else {
          0
        }
      };

      if self.tracer.enabled() {
        let frames = self.collect_python_frames();

        if frames.is_empty() {
          self.tracer.record_reallocation(
            old_address,
            old_size,
            new_address,
            new_size,
          );
        } else {
          self.tracer.record_reallocation_with_frames(
            old_address,
            old_size,
            new_address,
            new_size,
            &frames,
          );
        }
      }

      if let Ok(mut guard) = self.allocations.lock() {
        guard.remove(&old_address);

        if new_size > 0 {
          guard.insert(new_address, new_size);
        }
      }
    });
  }

  fn collect_python_frames(&self) -> Vec<FrameMetadata> {
    if self.frame_depth == 0 {
      return Vec::new();
    }

    let mut frames = Vec::with_capacity(self.frame_depth);

    let limit = self.frame_depth + self.frame_skip;

    Python::with_gil(|py| unsafe {
      let mut current = ffi::PyEval_GetFrame();

      if current.is_null() {
        return;
      }

      ffi::Py_XINCREF(current.cast());

      let mut depth = 0usize;

      while !current.is_null() && depth < limit {
        let frame: Py<PyFrame> =
          Py::from_owned_ptr(py, current.cast::<ffi::PyObject>());

        let frame_ref: &Bound<'_, PyFrame> = frame.bind(py);

        if depth >= self.frame_skip {
          if let Some(metadata) = metadata_from_frame(frame_ref) {
            frames.push(metadata);

            if frames.len() >= self.frame_depth {
              break;
            }
          }
        }

        depth = depth.saturating_add(1);

        if frames.len() >= self.frame_depth {
          break;
        }

        current = frame_ref
          .getattr("f_back")
          .ok()
          .and_then(|value| value.extract::<Option<Py<PyFrame>>>().ok())
          .and_then(|maybe_frame| {
            maybe_frame.map(|frame| frame.into_ptr() as *mut ffi::PyFrameObject)
          })
          .unwrap_or(ptr::null_mut());
      }
    });

    frames
  }
}

struct AllocatorContext {
  domain: ffi::PyMemAllocatorDomain,
  original: Option<ffi::PyMemAllocatorEx>,
  state: *const ShimState,
}

unsafe impl Send for AllocatorContext {}
unsafe impl Sync for AllocatorContext {}

impl AllocatorContext {
  const fn new(domain: ffi::PyMemAllocatorDomain) -> Self {
    Self {
      domain,
      original: None,
      state: ptr::null(),
    }
  }

  unsafe fn install(&mut self) -> PyResult<()> {
    let mut original = MaybeUninit::<ffi::PyMemAllocatorEx>::uninit();

    unsafe {
      ffi::PyMem_GetAllocator(self.domain, original.as_mut_ptr());
    }

    self.original = Some(unsafe { original.assume_init() });

    let mut shim = ffi::PyMemAllocatorEx {
      ctx: self as *mut _ as *mut c_void,
      malloc: Some(shim_malloc),
      calloc: Some(shim_calloc),
      realloc: Some(shim_realloc),
      free: Some(shim_free),
    };

    unsafe {
      ffi::PyMem_SetAllocator(self.domain, &mut shim);
    }

    Ok(())
  }

  unsafe fn restore(&self) -> PyResult<()> {
    if let Some(original) = &self.original {
      let mut allocator = *original;

      unsafe {
        ffi::PyMem_SetAllocator(self.domain, &mut allocator);
      }
    }

    Ok(())
  }

  fn state(&self) -> &'static ShimState {
    assert!(
      !self.state.is_null(),
      "allocator context missing tracer state"
    );

    unsafe { &*self.state }
  }

  fn original(&self) -> Option<&ffi::PyMemAllocatorEx> {
    self.original.as_ref()
  }
}

extern "C" fn shim_malloc(ctx: *mut c_void, size: usize) -> *mut c_void {
  unsafe {
    let context = &*(ctx as *mut AllocatorContext);

    let Some(original) = context.original() else {
      return ptr::null_mut();
    };

    let func = match original.malloc {
      Some(func) => func,
      None => return ptr::null_mut(),
    };

    let ptr = func(original.ctx, size);

    if !ptr.is_null() {
      context.state().record_allocation(ptr as usize, size);
    }

    ptr
  }
}

extern "C" fn shim_calloc(
  ctx: *mut c_void,
  nelem: usize,
  elsize: usize,
) -> *mut c_void {
  unsafe {
    let context = &*(ctx as *mut AllocatorContext);

    let Some(original) = context.original() else {
      return ptr::null_mut();
    };

    let func = match original.calloc {
      Some(func) => func,
      None => return ptr::null_mut(),
    };

    let total_size = nelem.checked_mul(elsize).unwrap_or(usize::MAX);

    let ptr = func(original.ctx, nelem, elsize);

    if !ptr.is_null() {
      context.state().record_allocation(ptr as usize, total_size);
    }

    ptr
  }
}

extern "C" fn shim_realloc(
  ctx: *mut c_void,
  ptr: *mut c_void,
  new_size: usize,
) -> *mut c_void {
  unsafe {
    let context = &*(ctx as *mut AllocatorContext);

    let Some(original) = context.original() else {
      return ptr::null_mut();
    };

    let func = match original.realloc {
      Some(func) => func,
      None => return ptr::null_mut(),
    };

    if ptr.is_null() {
      let result = func(original.ctx, ptr, new_size);

      if !result.is_null() {
        context.state().record_allocation(result as usize, new_size);
      }

      return result;
    }

    let old_address = ptr as usize;

    if new_size == 0 {
      let result = func(original.ctx, ptr, new_size);
      context.state().record_deallocation(old_address);
      return result;
    }

    let result = func(original.ctx, ptr, new_size);

    if result.is_null() {
      return result;
    }

    context
      .state()
      .record_reallocation(old_address, result as usize, new_size);

    result
  }
}

extern "C" fn shim_free(ctx: *mut c_void, ptr: *mut c_void) {
  if ptr.is_null() {
    return;
  }

  unsafe {
    let context = &*(ctx as *mut AllocatorContext);

    let Some(original) = context.original() else {
      return;
    };

    if let Some(func) = original.free {
      context.state().record_deallocation(ptr as usize);
      func(original.ctx, ptr);
    }
  }
}

fn metadata_from_frame(frame: &Bound<'_, PyFrame>) -> Option<FrameMetadata> {
  let code = frame.getattr("f_code").ok()?;

  let filename = code
    .getattr("co_filename")
    .ok()
    .and_then(|value| extract_bound_string(&value))
    .unwrap_or_else(|| "<unknown>".to_string());

  let function = code
    .getattr("co_name")
    .ok()
    .and_then(|value| extract_bound_string(&value))
    .unwrap_or_else(|| "<unknown>".to_string());

  let lineno = frame
    .getattr("f_lineno")
    .ok()
    .and_then(|value| value.extract::<u32>().ok())
    .unwrap_or(0);

  Some(FrameMetadata::new(filename, function, lineno))
}

fn extract_bound_string(value: &Bound<'_, PyAny>) -> Option<String> {
  value.extract::<String>().ok().or_else(|| {
    value
      .str()
      .ok()
      .map(|owned| owned.to_string_lossy().into_owned())
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn collects_python_frames_from_stack() {
    pyo3::prepare_freethreaded_python();

    let tracer = Tracer::builder().capture_native(false).finish();
    let state = ShimState::new(tracer, Vec::new());
    let frames = state.collect_python_frames();

    assert!(
      frames.len() <= state.frame_depth,
      "collector returned more frames than configured depth"
    );
  }

  #[test]
  fn reallocation_uses_recorded_size_for_deallocation() {
    pyo3::prepare_freethreaded_python();

    let tracer = Tracer::builder().capture_native(false).finish();
    let state = ShimState::new(tracer.clone(), Vec::new());

    let old_ptr = 0x1000usize;
    let new_ptr = 0x2000usize;

    state.record_allocation(old_ptr, 64);
    state.record_reallocation(old_ptr, new_ptr, 32);

    let snapshot = tracer.snapshot();

    let record = snapshot
      .records()
      .first()
      .expect("expected allocation record");

    assert_eq!(record.total_allocated, 96);
    assert_eq!(record.total_freed, 64);
    assert_eq!(record.current_bytes, 32);
  }
}

#[allow(deprecated)]
#[pymodule]
fn rust_tracemalloc(py: Python<'_>, module: &PyModule) -> PyResult<()> {
  module.add_function(wrap_pyfunction!(start, module)?)?;
  module.add_function(wrap_pyfunction!(stop, module)?)?;
  module.add_function(wrap_pyfunction!(is_tracing, module)?)?;
  module.add_function(wrap_pyfunction!(take_snapshot, module)?)?;
  module.add_function(wrap_pyfunction!(compare_snapshots, module)?)?;
  module.add_function(wrap_pyfunction!(open_snapshot_stream, module)?)?;

  module.add_class::<PySnapshot>()?;
  module.add_class::<PySnapshotDelta>()?;
  module.add_class::<SnapshotStream>()?;

  let version = PyDict::new_bound(py);
  version.set_item("major", 0)?;
  version.set_item("minor", 1)?;
  version.set_item("patch", 0)?;

  module.add("version_info", version.unbind())?;

  Ok(())
}
