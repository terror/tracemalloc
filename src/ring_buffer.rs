use std::mem::size_of;

use crate::config::TracerConfig;
use crate::event::AllocationEvent;

/// Result of attempting to append an event to a thread-local buffer.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DrainAction {
  /// The buffer still has capacity.
  Noop,
  /// The buffer crossed its threshold and should be drained soon.
  FlushPending,
}

/// Extremely simple placeholder for the per-thread ring buffer.
///
/// The initial milestone uses a standard `Vec` in lieu of a lock-free ring
/// structure. This keeps the API surface stable while the concurrency story is
/// fleshed out. The `max_events` value is derived from the configuration and
/// ensures that the buffer cannot grow unbounded.
#[derive(Debug)]
pub struct ThreadBuffer {
  events: Vec<AllocationEvent>,
  max_events: usize,
}

impl ThreadBuffer {
  #[must_use]
  pub fn new(config: &TracerConfig) -> Self {
    let max_events = config
      .ring_buffer_bytes
      .checked_div(size_of::<AllocationEvent>())
      .filter(|capacity| *capacity > 0)
      .unwrap_or(1024);

    Self {
      events: Vec::with_capacity(max_events),
      max_events,
    }
  }

  /// Push a new event into the buffer. Returns whether the caller should
  /// trigger a drain.
  pub fn record(&mut self, event: AllocationEvent) -> DrainAction {
    self.events.push(event);
    if self.events.len() >= self.max_events {
      DrainAction::FlushPending
    } else {
      DrainAction::Noop
    }
  }

  /// Drain all events out of the buffer into the provided collection.
  pub fn drain_into(&mut self, output: &mut Vec<AllocationEvent>) {
    output.extend(self.events.drain(..));
  }

  /// Returns true if the buffer currently contains events.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.events.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::TracerConfig;
  use crate::event::{AllocationEvent, EventKind};

  #[test]
  fn signals_flush_when_capacity_reached() {
    let mut config = TracerConfig::default();
    config.ring_buffer_bytes = std::mem::size_of::<AllocationEvent>() * 2;

    let mut buffer = ThreadBuffer::new(&config);
    assert_eq!(
      buffer.record(AllocationEvent::new(EventKind::Allocation, 0, 1, 0)),
      DrainAction::Noop
    );
    assert_eq!(
      buffer.record(AllocationEvent::new(EventKind::Allocation, 0, 1, 0)),
      DrainAction::FlushPending
    );
  }

  #[test]
  fn drain_transfers_and_clears_events() {
    let buffer_config = TracerConfig::default();
    let mut buffer = ThreadBuffer::new(&buffer_config);
    buffer.record(AllocationEvent::new(EventKind::Allocation, 0, 1, 0));

    let mut drained = Vec::new();
    buffer.drain_into(&mut drained);

    assert!(buffer.is_empty());
    assert_eq!(drained.len(), 1);
  }
}
