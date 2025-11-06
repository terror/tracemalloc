use super::*;

/// Errors that can occur when exporting or streaming snapshots.
#[derive(Debug)]
pub enum ExportError {
  Encode(prost::EncodeError),
  Io(io::Error),
  Json(serde_json::Error),
}

impl Display for ExportError {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      Self::Io(err) => write!(f, "i/o error during export: {err}"),
      Self::Json(err) => write!(f, "failed to encode snapshot as json: {err}"),
      Self::Encode(err) => {
        write!(f, "failed to encode snapshot as pprof: {err}")
      }
    }
  }
}

impl std::error::Error for ExportError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Io(err) => Some(err),
      Self::Json(err) => Some(err),
      Self::Encode(err) => Some(err),
    }
  }
}

impl From<io::Error> for ExportError {
  fn from(value: io::Error) -> Self {
    Self::Io(value)
  }
}

impl From<serde_json::Error> for ExportError {
  fn from(value: serde_json::Error) -> Self {
    Self::Json(value)
  }
}

impl From<prost::EncodeError> for ExportError {
  fn from(value: prost::EncodeError) -> Self {
    Self::Encode(value)
  }
}

/// Streaming interface for snapshot consumers.
pub trait SnapshotStreamWriter {
  /// # Errors
  ///
  /// Returns an `ExportError` if the snapshot cannot be serialized or if
  /// the underlying writer fails to persist the data.
  fn write_snapshot(
    &mut self,
    snapshot: &Snapshot,
    timestamp: Option<SystemTime>,
  ) -> Result<(), ExportError>;
}

/// JSON lines exporter that writes one JSON object per snapshot.
pub struct JsonLinesWriter<W: Write> {
  writer: W,
}

impl<W: Write> SnapshotStreamWriter for JsonLinesWriter<W> {
  fn write_snapshot(
    &mut self,
    snapshot: &Snapshot,
    timestamp: Option<SystemTime>,
  ) -> Result<(), ExportError> {
    let chunk = StreamChunk::new(snapshot, timestamp);
    serde_json::to_writer(&mut self.writer, &chunk)?;
    self.writer.write_all(b"\n")?;
    Ok(())
  }
}

impl<W: Write> JsonLinesWriter<W> {
  pub fn into_inner(self) -> W {
    self.writer
  }

  pub fn new(writer: W) -> Self {
    Self { writer }
  }
}

/// Streaming writer backed by an mmap'd file.
pub struct MmapJsonStreamWriter {
  mmap: MmapMut,
  position: usize,
}

impl SnapshotStreamWriter for MmapJsonStreamWriter {
  fn write_snapshot(
    &mut self,
    snapshot: &Snapshot,
    timestamp: Option<SystemTime>,
  ) -> Result<(), ExportError> {
    let chunk = StreamChunk::new(snapshot, timestamp);
    let mut encoded = serde_json::to_vec(&chunk)?;
    encoded.push(b'\n');
    self.write_bytes(&encoded)?;
    Ok(())
  }
}

impl MmapJsonStreamWriter {
  /// # Errors
  ///
  /// Returns an error if the backing file cannot be created, resized, or
  /// mapped into memory.
  pub fn create(path: impl AsRef<Path>, capacity: usize) -> io::Result<Self> {
    let capacity = capacity.max(1);

    let file = OpenOptions::new()
      .create(true)
      .write(true)
      .read(true)
      .truncate(true)
      .open(path)?;

    let capacity_u64 = u64::try_from(capacity)
      .map_err(|_| io::Error::other("capacity exceeds u64"))?;

    file.set_len(capacity_u64)?;

    // SAFETY: the file handle remains open for the lifetime of the mapping.
    let mmap = unsafe { MmapMut::map_mut(&file)? };

    Ok(Self { mmap, position: 0 })
  }

  /// # Errors
  ///
  /// Returns an error if flushing the memory-mapped region fails.
  pub fn flush(&self) -> io::Result<()> {
    self.mmap.flush_async()?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if the write would exceed the reserved capacity.
  fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
    let Some(end) = self.position.checked_add(data.len()) else {
      return Err(io::Error::other("mmap position overflow"));
    };

    if end > self.mmap.len() {
      return Err(io::Error::new(
        io::ErrorKind::WriteZero,
        "mmap capacity exceeded",
      ));
    }

    self.mmap[self.position..end].copy_from_slice(data);

    self.position = end;

    Ok(())
  }
}

#[derive(Serialize)]
struct StreamChunk<'a> {
  dropped_events: u64,
  records: &'a [SnapshotRecord],
  #[serde(skip_serializing_if = "Option::is_none")]
  timestamp_ns: Option<u128>,
}

impl<'a> StreamChunk<'a> {
  fn new(snapshot: &'a Snapshot, timestamp: Option<SystemTime>) -> Self {
    Self {
      dropped_events: snapshot.dropped_events(),
      records: snapshot.records(),
      timestamp_ns: timestamp.and_then(system_time_to_nanos),
    }
  }
}

#[cfg(not(windows))]
struct StringTable {
  entries: Vec<String>,
  index: HashMap<String, i64>,
}

#[cfg(not(windows))]
impl StringTable {
  fn intern(&mut self, value: &str) -> i64 {
    if let Some(index) = self.index.get(value) {
      return *index;
    }

    let index = i64::try_from(self.entries.len()).unwrap_or(i64::MAX);

    self.entries.push(value.to_string());
    self.index.insert(value.to_string(), index);

    index
  }

  fn into_vec(self) -> Vec<String> {
    self.entries
  }

  fn new() -> Self {
    Self {
      entries: vec![String::new()],
      index: HashMap::from([(String::new(), 0)]),
    }
  }
}

#[cfg(not(windows))]
#[derive(Hash, Eq, PartialEq, Copy, Clone)]
struct FunctionKey {
  filename: i64,
  function: i64,
  lineno: u32,
}

#[cfg(not(windows))]
#[derive(Hash, Eq, PartialEq, Copy, Clone)]
struct LocationKey {
  function_id: u64,
  lineno: u32,
}

#[cfg(not(windows))]
fn next_id(len: usize) -> u64 {
  u64::try_from(len.saturating_add(1)).unwrap_or(u64::MAX)
}

#[cfg(not(windows))]
fn intern_location_for_frame(
  string_table: &mut StringTable,
  function_ids: &mut HashMap<FunctionKey, u64>,
  location_ids: &mut HashMap<LocationKey, u64>,
  functions: &mut Vec<Function>,
  locations: &mut Vec<Location>,
  filename: &str,
  function: &str,
  lineno: u32,
) -> u64 {
  let filename_idx = string_table.intern(filename);
  let function_name_idx = string_table.intern(function);

  let function_id = *function_ids
    .entry(FunctionKey {
      filename: filename_idx,
      function: function_name_idx,
      lineno,
    })
    .or_insert_with(|| {
      let id = next_id(functions.len());

      let function = Function {
        id,
        name: function_name_idx,
        system_name: function_name_idx,
        filename: filename_idx,
        start_line: i64::from(lineno),
      };

      functions.push(function);

      id
    });

  *location_ids
    .entry(LocationKey {
      function_id,
      lineno,
    })
    .or_insert_with(|| {
      let id = next_id(locations.len());

      let line = Line {
        function_id,
        line: i64::from(lineno),
      };

      let location = Location {
        id,
        mapping_id: 0,
        address: 0,
        line: vec![line],
        is_folded: false,
      };

      locations.push(location);

      id
    })
}

fn system_time_to_nanos(ts: SystemTime) -> Option<u128> {
  ts.duration_since(SystemTime::UNIX_EPOCH)
    .ok()
    .map(|duration| duration.as_nanos())
}

#[cfg(not(windows))]
pub fn build_pprof_profile(snapshot: &Snapshot) -> Profile {
  let mut string_table = StringTable::new();

  let (mut functions, mut locations, mut samples) =
    (Vec::new(), Vec::new(), Vec::new());

  let (mut function_ids, mut location_ids) = (HashMap::new(), HashMap::new());

  for record in snapshot.records() {
    let mut stack_location_ids = Vec::new();

    if let Some(stack) = &record.stack {
      for frame in stack.frames() {
        stack_location_ids.push(intern_location_for_frame(
          &mut string_table,
          &mut function_ids,
          &mut location_ids,
          &mut functions,
          &mut locations,
          frame.filename.as_ref(),
          frame.function.as_ref(),
          frame.lineno,
        ));
      }
    }

    if stack_location_ids.is_empty() {
      // Provide a synthetic frame when we have no metadata so tools still show
      // the allocation bucket.
      stack_location_ids.push(intern_location_for_frame(
        &mut string_table,
        &mut function_ids,
        &mut location_ids,
        &mut functions,
        &mut locations,
        "<unknown>",
        "<unknown>",
        0,
      ));
    }

    samples.push(Sample {
      location_id: stack_location_ids,
      value: vec![
        record.current_bytes,
        i64::try_from(record.allocations).unwrap_or(i64::MAX),
      ],
      label: Vec::new(),
    });
  }

  Profile {
    sample_type: vec![
      ValueType {
        ty: string_table.intern("space"),
        unit: string_table.intern("bytes"),
      },
      ValueType {
        ty: string_table.intern("allocations"),
        unit: string_table.intern("count"),
      },
    ],
    sample: samples,
    mapping: Vec::new(),
    location: locations,
    function: functions,
    string_table: string_table.into_vec(),
    drop_frames: 0,
    keep_frames: 0,
    time_nanos: 0,
    duration_nanos: 0,
    period_type: Some(ValueType { ty: 0, unit: 0 }),
    period: 1,
    comment: Vec::new(),
    default_sample_type: 0,
  }
}

#[cfg(all(test, not(windows)))]
mod tests {
  use super::*;

  fn make_stack(
    frames: Vec<FrameMetadata>,
    stack_id: StackId,
  ) -> Arc<StackMetadata> {
    let table = StackTable::new();
    table.insert_with_id(stack_id, frames);
    table.resolve(stack_id).expect("stack metadata available")
  }

  fn snapshot_record(
    stack: Option<Arc<StackMetadata>>,
    current_bytes: i64,
    allocations: u64,
  ) -> SnapshotRecord {
    let stack_id = stack
      .as_ref()
      .map_or(StackId::default(), |metadata| metadata.id());

    SnapshotRecord {
      allocations,
      current_bytes,
      deallocations: 0,
      stack,
      stack_id,
      total_allocated: current_bytes.try_into().unwrap_or_default(),
      total_freed: 0,
    }
  }

  fn build_snapshot(records: Vec<SnapshotRecord>) -> Snapshot {
    Snapshot::new(records, 0)
  }

  #[test]
  fn build_pprof_profile_deduplicates_frames() {
    let stack = make_stack(vec![FrameMetadata::new("app.py", "render", 42)], 1);

    let snapshot = build_snapshot(vec![
      snapshot_record(Some(stack.clone()), 256, 1),
      snapshot_record(Some(stack), 128, 2),
    ]);

    let profile = build_pprof_profile(&snapshot);

    assert_eq!(profile.function.len(), 1);
    assert_eq!(profile.location.len(), 1);
    assert_eq!(profile.sample.len(), 2);
    assert_eq!(profile.sample_type.len(), 2);

    let location_id = profile.location[0].id;

    for (sample, expected) in profile
      .sample
      .iter()
      .zip([[256_i64, 1_i64], [128, 2]].into_iter())
    {
      assert_eq!(sample.location_id, vec![location_id]);
      assert_eq!(sample.value.as_slice(), expected);
    }

    let function = &profile.function[0];

    let filename =
      &profile.string_table[usize::try_from(function.filename).unwrap()];

    let name = &profile.string_table[usize::try_from(function.name).unwrap()];

    assert_eq!(filename, "app.py");
    assert_eq!(name, "render");
  }

  #[test]
  fn build_pprof_profile_inserts_unknown_frame() {
    let snapshot = build_snapshot(vec![snapshot_record(None, 512, 3)]);

    let profile = build_pprof_profile(&snapshot);

    assert_eq!(profile.function.len(), 1);
    assert_eq!(profile.location.len(), 1);
    assert_eq!(profile.sample.len(), 1);

    let function = &profile.function[0];
    let name = &profile.string_table[usize::try_from(function.name).unwrap()];

    let filename =
      &profile.string_table[usize::try_from(function.filename).unwrap()];

    assert_eq!(name, "<unknown>");
    assert_eq!(filename, "<unknown>");

    let location = &profile.location[0];

    assert_eq!(location.line.len(), 1);
    assert_eq!(location.line[0].line, 0);
    assert_eq!(location.line[0].function_id, function.id);

    assert_eq!(profile.sample[0].location_id, vec![profile.location[0].id]);
    assert_eq!(profile.sample[0].value, vec![512, 3]);
  }
}
