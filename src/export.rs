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

fn system_time_to_nanos(ts: SystemTime) -> Option<u128> {
  ts.duration_since(SystemTime::UNIX_EPOCH)
    .ok()
    .map(|duration| duration.as_nanos())
}

#[cfg(not(windows))]
pub fn build_pprof_profile(snapshot: &Snapshot) -> Profile {
  let mut string_table = StringTable::new();

  let mut functions = Vec::new();
  let mut locations = Vec::new();
  let mut samples = Vec::new();

  let sample_type = ValueType {
    ty: string_table.intern("space"),
    unit: string_table.intern("bytes"),
  };

  let count_type = ValueType {
    ty: string_table.intern("allocations"),
    unit: string_table.intern("count"),
  };

  let mut function_ids = HashMap::new();
  let mut location_ids = HashMap::new();

  let mut next_function_id = 1;
  let mut next_location_id = 1;

  for record in snapshot.records() {
    let mut stack_location_ids = Vec::new();

    if let Some(stack) = &record.stack {
      for frame in stack.frames() {
        let filename_idx = string_table.intern(frame.filename.as_ref());

        let function_name_idx = string_table.intern(frame.function.as_ref());

        let function_id = *function_ids
          .entry((filename_idx, function_name_idx, frame.lineno))
          .or_insert_with(|| {
            let function = Function {
              id: next_function_id,
              name: function_name_idx,
              system_name: function_name_idx,
              filename: filename_idx,
              start_line: i64::from(frame.lineno),
            };

            functions.push(function);

            next_function_id += 1;
            next_function_id - 1
          });

        let location_id = *location_ids
          .entry((function_id, frame.lineno))
          .or_insert_with(|| {
            let line = Line {
              function_id,
              line: i64::from(frame.lineno),
            };

            let location = Location {
              id: next_location_id,
              mapping_id: 0,
              address: 0,
              line: vec![line],
              is_folded: false,
            };

            locations.push(location);

            next_location_id += 1;
            next_location_id - 1
          });

        stack_location_ids.push(location_id);
      }
    }

    if stack_location_ids.is_empty() {
      // Provide a synthetic frame when we have no metadata so tools still show
      // the allocation bucket.
      let unknown_label = string_table.intern("<unknown>");

      let function_id = *function_ids
        .entry((unknown_label, unknown_label, 0))
        .or_insert_with(|| {
          let function = Function {
            id: next_function_id,
            name: unknown_label,
            system_name: unknown_label,
            filename: unknown_label,
            start_line: 0,
          };

          functions.push(function);

          next_function_id += 1;
          next_function_id - 1
        });

      let location_id =
        *location_ids.entry((function_id, 0)).or_insert_with(|| {
          let line = Line {
            function_id,
            line: 0,
          };

          let location = Location {
            id: next_location_id,
            mapping_id: 0,
            address: 0,
            line: vec![line],
            is_folded: false,
          };

          locations.push(location);

          next_location_id += 1;
          next_location_id - 1
        });

      stack_location_ids.push(location_id);
    }

    let bytes_value = record.current_bytes;

    let count_value = i64::try_from(record.allocations).unwrap_or(i64::MAX);

    let sample = Sample {
      location_id: stack_location_ids,
      value: vec![bytes_value, count_value],
      label: Vec::new(),
    };

    samples.push(sample);
  }

  Profile {
    sample_type: vec![sample_type, count_type],
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
