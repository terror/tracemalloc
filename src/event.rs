use std::time::Instant;

/// Unique identifier for an interned stack trace.
pub type StackId = u64;

/// Work-in-progress encoding of allocation events drained from per-thread buffers.
#[derive(Debug, Clone)]
pub struct AllocationEvent {
  pub kind: EventKind,
  pub address: usize,
  pub size: usize,
  pub stack_id: StackId,
  pub timestamp: Instant,
}

impl AllocationEvent {
  #[must_use]
  pub fn new(
    kind: EventKind,
    address: usize,
    size: usize,
    stack_id: StackId,
  ) -> Self {
    Self {
      kind,
      address,
      size,
      stack_id,
      timestamp: Instant::now(),
    }
  }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EventKind {
  Allocation,
  Deallocation,
  /// Signifies that the tracer dropped one or more events due to back pressure.
  Dropped {
    count: u32,
  },
}
