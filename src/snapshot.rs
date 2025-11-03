use super::*;

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

/// Immutable view of the current tracer state.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
  dropped_events: u64,
  records: Vec<SnapshotRecord>,
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

impl SnapshotDelta {
  #[must_use]
  pub fn dropped_events(&self) -> i64 {
    self.dropped_events
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
