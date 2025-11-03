use std::collections::HashMap;

use crate::event::{AllocationEvent, EventKind, StackId};
use crate::snapshot::{Snapshot, SnapshotRecord};

#[derive(Debug, Default, Clone)]
pub struct AllocationStats {
  pub current_bytes: i64,
  pub allocations: u64,
  pub deallocations: u64,
  pub total_allocated: u64,
  pub total_freed: u64,
}

impl AllocationStats {
  fn on_allocation(&mut self, size: usize) {
    let size = size as i64;
    self.current_bytes += size;
    self.allocations += 1;
    self.total_allocated += size as u64;
  }

  fn on_deallocation(&mut self, size: usize) {
    let size = size as i64;
    self.current_bytes -= size;
    self.deallocations += 1;
    self.total_freed += size as u64;
  }
}

/// Aggregates allocation events keyed by stack identifier.
#[derive(Debug, Default)]
pub struct Aggregator {
  stats: HashMap<StackId, AllocationStats>,
  dropped_events: u64,
}

impl Aggregator {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Update the aggregate statistics based on a stream of events.
  pub fn ingest<I>(&mut self, events: I)
  where
    I: IntoIterator<Item = AllocationEvent>,
  {
    for event in events {
      match event.kind {
        EventKind::Allocation => {
          self
            .stats
            .entry(event.stack_id)
            .or_default()
            .on_allocation(event.size);
        }
        EventKind::Deallocation => {
          self
            .stats
            .entry(event.stack_id)
            .or_default()
            .on_deallocation(event.size);
        }
        EventKind::Dropped { count } => {
          self.dropped_events += u64::from(count);
        }
      }
    }
  }

  /// Produce a snapshot that callers can later diff.
  #[must_use]
  pub fn snapshot(&self) -> Snapshot {
    let mut records: Vec<_> = self
      .stats
      .iter()
      .map(|(stack_id, stats)| SnapshotRecord {
        stack_id: *stack_id,
        current_bytes: stats.current_bytes,
        allocations: stats.allocations,
        deallocations: stats.deallocations,
        total_allocated: stats.total_allocated,
        total_freed: stats.total_freed,
      })
      .collect();

    records.sort_by(|a, b| b.current_bytes.cmp(&a.current_bytes));

    Snapshot::new(records, self.dropped_events)
  }

  /// Clears all aggregated statistics.
  pub fn reset(&mut self) {
    self.stats.clear();
    self.dropped_events = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::event::EventKind;

  #[test]
  fn aggregates_allocations_and_deallocations() {
    let mut aggregator = Aggregator::new();
    aggregator.ingest(vec![
      AllocationEvent::new(EventKind::Allocation, 0x1, 128, 42),
      AllocationEvent::new(EventKind::Allocation, 0x2, 64, 42),
      AllocationEvent::new(EventKind::Deallocation, 0x2, 64, 42),
    ]);

    let snapshot = aggregator.snapshot();
    let record = snapshot
      .records()
      .iter()
      .find(|record| record.stack_id == 42)
      .expect("missing stack 42");

    assert_eq!(record.current_bytes, 128);
    assert_eq!(record.allocations, 2);
    assert_eq!(record.deallocations, 1);
    assert_eq!(record.total_allocated, 192);
    assert_eq!(record.total_freed, 64);
  }

  #[test]
  fn tracks_dropped_events() {
    let mut aggregator = Aggregator::new();
    aggregator.ingest(vec![AllocationEvent::new(
      EventKind::Dropped { count: 5 },
      0,
      0,
      0,
    )]);

    let snapshot = aggregator.snapshot();
    assert_eq!(snapshot.dropped_events(), 5);
  }
}
