use {
  pyo3::{
    Bound, Py,
    exceptions::PyRuntimeError,
    ffi,
    ffi::PyMemAllocatorDomain::{
      PYMEM_DOMAIN_MEM, PYMEM_DOMAIN_OBJ, PYMEM_DOMAIN_RAW,
    },
    prelude::*,
    types::{PyAny, PyDict, PyFrame, PyList, PyModule},
  },
  std::{
    cell::Cell,
    collections::HashMap,
    mem::MaybeUninit,
    os::raw::c_void,
    ptr,
    sync::{Mutex, OnceLock},
  },
  tracemalloc::{FrameMetadata, Snapshot, Tracer},
};

static STATE: OnceLock<Box<ShimState>> = OnceLock::new();

thread_local! {
  static IN_TRACER: Cell<bool> = Cell::new(false);
}

#[pyfunction]
fn start(
  py: Python<'_>,
  nframe: Option<u16>,
  capture_native: Option<bool>,
) -> PyResult<()> {
  if STATE.get().is_some() {
    return Ok(());
  }

  let mut builder = Tracer::builder().start_enabled(true);

  if let Some(depth) = nframe {
    builder = builder.max_stack_depth(depth);
  }

  if let Some(native) = capture_native {
    builder = builder.capture_native(native);
  } else {
    builder = builder.capture_native(false);
  }

  let tracer = builder.finish();

  let mut state = Box::new(ShimState::new(tracer));

  unsafe {
    state.install(py)?;
  }

  STATE
    .set(state)
    .map_err(|_| PyRuntimeError::new_err("tracer already started"))?;

  Ok(())
}

#[pyfunction]
fn stop(_py: Python<'_>) -> PyResult<()> {
  if let Some(state) = STATE.get() {
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
    .get()
    .map(|state| state.tracer.enabled())
    .unwrap_or(false)
}

#[pyfunction]
fn take_snapshot(py: Python<'_>) -> PyResult<PyObject> {
  let state = STATE
    .get()
    .ok_or_else(|| PyRuntimeError::new_err("tracer not started"))?;

  let snapshot = state.tracer.snapshot();

  snapshot_to_python(py, snapshot)
}

fn snapshot_to_python(
  py: Python<'_>,
  snapshot: Snapshot,
) -> PyResult<PyObject> {
  let records = PyList::empty_bound(py);

  for record in snapshot.records() {
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
        let frame_obj = frame_dict.unbind();
        frames.append(frame_obj)?;
      }

      let frames_obj = frames.unbind();

      entry.set_item("frames", frames_obj)?;
    }

    let entry_obj = entry.unbind();

    records.append(entry_obj)?;
  }

  let result = PyDict::new_bound(py);

  let records_obj = records.unbind();

  result.set_item("records", records_obj)?;
  result.set_item("dropped_events", snapshot.dropped_events())?;

  Ok(result.unbind().into())
}

struct ShimState {
  tracer: Tracer,
  frame_depth: usize,
  frame_skip: usize,
  allocations: Mutex<HashMap<usize, usize>>,
  contexts: [AllocatorContext; 3],
}

unsafe impl Send for ShimState {}
unsafe impl Sync for ShimState {}

impl ShimState {
  fn new(tracer: Tracer) -> Self {
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
    let state = ShimState::new(tracer);
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
    let state = ShimState::new(tracer.clone());

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

  let version = PyDict::new_bound(py);
  version.set_item("major", 0)?;
  version.set_item("minor", 1)?;
  version.set_item("patch", 0)?;

  module.add("version_info", version.unbind())?;

  Ok(())
}
