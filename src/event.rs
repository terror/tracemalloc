use super::*;

/// Unique identifier for an interned stack trace.
pub type StackId = u64;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EventKind {
  Allocation,
  Deallocation,
  /// Signifies that the tracer dropped one or more events due to back pressure.
  Dropped {
    count: u64,
  },
}

/// Encoding of allocation events drained from per-thread buffers.
#[derive(Debug, Clone)]
pub struct AllocationEvent {
  pub address: usize,
  pub kind: EventKind,
  pub size: usize,
  pub stack_id: StackId,
  pub timestamp: Instant,
}

impl From<EventKind> for AllocationEvent {
  fn from(kind: EventKind) -> Self {
    Self::new(kind)
  }
}

impl AllocationEvent {
  #[must_use]
  pub fn address(mut self, address: usize) -> Self {
    self.address = address;
    self
  }

  #[must_use]
  pub fn new(kind: EventKind) -> Self {
    Self {
      kind,
      address: 0,
      size: 0,
      stack_id: 0,
      timestamp: Instant::now(),
    }
  }

  #[must_use]
  pub fn size(mut self, size: usize) -> Self {
    self.size = size;
    self
  }

  #[must_use]
  pub fn stack_id(mut self, stack_id: StackId) -> Self {
    self.stack_id = stack_id;
    self
  }

  #[must_use]
  pub fn timestamp(mut self, timestamp: Instant) -> Self {
    self.timestamp = timestamp;
    self
  }
}
