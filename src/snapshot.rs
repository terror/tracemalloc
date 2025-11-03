use crate::event::StackId;

/// A single aggregated entry representing allocations attributed to a stack.
#[derive(Debug, Clone)]
pub struct SnapshotRecord {
  pub stack_id: StackId,
  pub current_bytes: i64,
  pub allocations: u64,
  pub deallocations: u64,
  pub total_allocated: u64,
  pub total_freed: u64,
}

/// Immutable view of the current tracer state.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
  records: Vec<SnapshotRecord>,
  dropped_events: u64,
}

impl Snapshot {
  #[must_use]
  pub(crate) fn new(records: Vec<SnapshotRecord>, dropped_events: u64) -> Self {
    Self {
      records,
      dropped_events,
    }
  }

  #[must_use]
  pub fn records(&self) -> &[SnapshotRecord] {
    &self.records
  }

  #[must_use]
  pub fn dropped_events(&self) -> u64 {
    self.dropped_events
  }
}

/// Lightweight diff between two snapshots.
#[derive(Debug, Clone, Default)]
pub struct SnapshotDelta {
  records: Vec<SnapshotRecord>,
  dropped_events: i64,
}

impl SnapshotDelta {
  #[must_use]
  pub fn new(records: Vec<SnapshotRecord>, dropped_events: i64) -> Self {
    Self {
      records,
      dropped_events,
    }
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
          stack_id: record.stack_id,
          current_bytes: record.current_bytes - prev.current_bytes,
          allocations: record.allocations.saturating_sub(prev.allocations),
          deallocations: record
            .deallocations
            .saturating_sub(prev.deallocations),
          total_allocated: record
            .total_allocated
            .saturating_sub(prev.total_allocated),
          total_freed: record.total_freed.saturating_sub(prev.total_freed),
        },
        None => record.clone(),
      };

      deltas.push(delta_record);
    }

    let dropped_events =
      newer.dropped_events() as i64 - older.dropped_events() as i64;

    Self::new(deltas, dropped_events)
  }

  #[must_use]
  pub fn records(&self) -> &[SnapshotRecord] {
    &self.records
  }

  #[must_use]
  pub fn dropped_events(&self) -> i64 {
    self.dropped_events
  }
}
