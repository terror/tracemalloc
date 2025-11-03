use super::*;

/// Result of attempting to append an event to a thread-local buffer.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DrainAction {
  /// The buffer crossed its threshold and should be drained soon.
  FlushPending,
  /// The buffer still has capacity.
  Noop,
}

/// Shared ring buffer that can be written to without taking locks on the hot path.
#[derive(Debug, Clone)]
pub struct ThreadBuffer {
  inner: Arc<ThreadBufferInner>,
}

impl ThreadBuffer {
  #[must_use]
  pub(crate) fn downgrade(&self) -> Weak<ThreadBufferInner> {
    Arc::downgrade(&self.inner)
  }

  /// Drain all events out of the buffer into the provided collection.
  #[must_use]
  pub fn drain_into(&self, output: &mut Vec<AllocationEvent>) -> u64 {
    self.inner.drain_into(output)
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.inner.is_empty()
  }

  #[must_use]
  pub fn new(config: &TracerConfig) -> Self {
    let event_size = size_of::<AllocationEvent>().max(1);

    let mut max_events = config.ring_buffer_bytes / event_size;

    if max_events == 0 {
      max_events = 1024;
    }

    let flush_threshold = max_events.saturating_sub(max_events / 4).max(1);

    Self {
      inner: Arc::new(ThreadBufferInner::new(max_events, flush_threshold)),
    }
  }

  /// Push a new event into the buffer. Returns whether the caller should
  /// trigger a drain.
  #[must_use]
  pub fn record(&self, event: AllocationEvent) -> DrainAction {
    self.inner.record(event)
  }
}

#[derive(Debug)]
pub(crate) struct ThreadBufferInner {
  dropped: AtomicU64,
  flush_threshold: usize,
  queue: ArrayQueue<AllocationEvent>,
}

impl ThreadBufferInner {
  pub(crate) fn drain_into(&self, output: &mut Vec<AllocationEvent>) -> u64 {
    while let Some(event) = self.queue.pop() {
      output.push(event);
    }

    self.dropped.swap(0, Ordering::AcqRel)
  }

  fn is_empty(&self) -> bool {
    self.queue.is_empty() && self.dropped.load(Ordering::Relaxed) == 0
  }

  fn new(capacity: usize, flush_threshold: usize) -> Self {
    let capacity = capacity.max(1);

    Self {
      dropped: AtomicU64::new(0),
      flush_threshold: flush_threshold.min(capacity).max(1),
      queue: ArrayQueue::new(capacity),
    }
  }

  fn record(&self, event: AllocationEvent) -> DrainAction {
    match self.queue.push(event) {
      Ok(()) => {
        if self.queue.len() >= self.flush_threshold {
          DrainAction::FlushPending
        } else {
          DrainAction::Noop
        }
      }
      Err(_event) => {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        DrainAction::FlushPending
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::event::{AllocationEvent, EventKind};

  #[test]
  fn signals_flush_when_capacity_reached() {
    let mut config = TracerConfig::default();

    config.ring_buffer_bytes = size_of::<AllocationEvent>() * 2;

    let buffer = ThreadBuffer::new(&config);

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

    let buffer = ThreadBuffer::new(&buffer_config);
    let _ = buffer.record(AllocationEvent::new(EventKind::Allocation, 0, 1, 0));

    let mut drained = Vec::new();

    let dropped = buffer.drain_into(&mut drained);

    assert!(buffer.is_empty());

    assert_eq!(drained.len(), 1);
    assert_eq!(dropped, 0);
  }

  #[test]
  fn tracks_dropped_events_when_queue_full() {
    let mut config = TracerConfig::default();

    config.ring_buffer_bytes = size_of::<AllocationEvent>();

    let buffer = ThreadBuffer::new(&config);
    let _ = buffer.record(AllocationEvent::new(EventKind::Allocation, 0, 1, 0));

    // This should be dropped because the queue capacity is 1.
    let _ = buffer.record(AllocationEvent::new(EventKind::Allocation, 0, 1, 0));

    let mut drained = Vec::new();

    let dropped = buffer.drain_into(&mut drained);

    assert_eq!(drained.len(), 1);
    assert_eq!(dropped, 1);
  }
}
