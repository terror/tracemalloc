use super::*;

#[derive(Serialize)]
struct FrameExport<'a> {
  filename: &'a str,
  function: &'a str,
  lineno: u32,
}

/// A single aggregated entry representing allocations attributed to a stack.
#[derive(Debug, Clone)]
pub struct SnapshotRecord {
  pub allocations: u64,
  pub current_bytes: i64,
  pub deallocations: u64,
  pub stack: Option<Arc<StackMetadata>>,
  pub stack_id: StackId,
  pub total_allocated: u64,
  pub total_freed: u64,
}

impl Serialize for SnapshotRecord {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut state = serializer.serialize_struct("SnapshotRecord", 7)?;
    state.serialize_field("stack_id", &self.stack_id)?;
    state.serialize_field("allocations", &self.allocations)?;
    state.serialize_field("deallocations", &self.deallocations)?;
    state.serialize_field("current_bytes", &self.current_bytes)?;
    state.serialize_field("total_allocated", &self.total_allocated)?;
    state.serialize_field("total_freed", &self.total_freed)?;

    if let Some(stack) = &self.stack {
      let frames = stack
        .frames()
        .iter()
        .map(|frame| FrameExport {
          filename: frame.filename.as_ref(),
          function: frame.function.as_ref(),
          lineno: frame.lineno,
        })
        .collect::<Vec<FrameExport<'_>>>();

      state.serialize_field("frames", &frames)?;
    }

    state.end()
  }
}

/// Immutable view of the current tracer state.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
  dropped_events: u64,
  records: Vec<SnapshotRecord>,
}

impl Serialize for Snapshot {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut state = serializer.serialize_struct("Snapshot", 2)?;
    state.serialize_field("dropped_events", &self.dropped_events)?;
    state.serialize_field("records", &self.records)?;
    state.end()
  }
}

impl Snapshot {
  #[must_use]
  pub fn dropped_events(&self) -> u64 {
    self.dropped_events
  }

  #[must_use]
  pub(crate) fn new(records: Vec<SnapshotRecord>, dropped_events: u64) -> Self {
    Self {
      dropped_events,
      records,
    }
  }

  #[must_use]
  pub fn records(&self) -> &[SnapshotRecord] {
    &self.records
  }
}

/// Lightweight diff between two snapshots.
#[derive(Debug, Clone, Default)]
pub struct SnapshotDelta {
  dropped_events: i64,
  records: Vec<SnapshotRecord>,
}

impl Serialize for SnapshotDelta {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut state = serializer.serialize_struct("SnapshotDelta", 2)?;
    state.serialize_field("dropped_events", &self.dropped_events)?;
    state.serialize_field("records", &self.records)?;
    state.end()
  }
}

impl SnapshotDelta {
  #[must_use]
  pub fn dropped_events(&self) -> i64 {
    self.dropped_events
  }

  /// Serialize the snapshot delta to JSON using the provided writer.
  ///
  /// # Errors
  ///
  /// Returns an error if serialization to JSON fails.
  pub fn export_json<W: Write>(&self, writer: W) -> Result<(), ExportError> {
    serde_json::to_writer(writer, self)?;
    Ok(())
  }

  #[must_use]
  pub fn from_snapshots(newer: &Snapshot, older: &Snapshot) -> Self {
    let mut deltas = Vec::new();

    for record in newer.records() {
      let baseline = older
        .records()
        .iter()
        .find(|candidate| candidate.stack_id == record.stack_id);

      let delta_record = match baseline {
        Some(prev) => SnapshotRecord {
          allocations: record.allocations.saturating_sub(prev.allocations),
          current_bytes: record.current_bytes - prev.current_bytes,
          deallocations: record
            .deallocations
            .saturating_sub(prev.deallocations),
          stack: record.stack.clone(),
          stack_id: record.stack_id,
          total_allocated: record
            .total_allocated
            .saturating_sub(prev.total_allocated),
          total_freed: record.total_freed.saturating_sub(prev.total_freed),
        },
        None => record.clone(),
      };

      deltas.push(delta_record);
    }

    let dropped_events_delta =
      i128::from(newer.dropped_events()) - i128::from(older.dropped_events());

    let dropped_events = match i64::try_from(dropped_events_delta) {
      Ok(value) => value,
      Err(_) if dropped_events_delta.is_negative() => i64::MIN,
      Err(_) => i64::MAX,
    };

    Self::new(deltas, dropped_events)
  }

  #[must_use]
  pub fn new(records: Vec<SnapshotRecord>, dropped_events: i64) -> Self {
    Self {
      dropped_events,
      records,
    }
  }

  #[must_use]
  pub fn records(&self) -> &[SnapshotRecord] {
    &self.records
  }
}
