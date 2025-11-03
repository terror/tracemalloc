use std::{
  collections::HashMap,
  sync::{Arc, Mutex, MutexGuard},
};

use crate::event::StackId;

/// Metadata describing a single frame in a stack trace.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FrameMetadata {
  pub filename: Arc<str>,
  pub function: Arc<str>,
  pub lineno: u32,
}

impl FrameMetadata {
  #[must_use]
  pub fn new(
    filename: impl Into<String>,
    function: impl Into<String>,
    lineno: u32,
  ) -> Self {
    Self {
      filename: Arc::<str>::from(filename.into()),
      function: Arc::<str>::from(function.into()),
      lineno,
    }
  }
}

/// Resolved metadata for an interned stack trace.
#[derive(Debug, Clone)]
pub struct StackMetadata {
  frames: Arc<[FrameMetadata]>,
  id: StackId,
}

impl StackMetadata {
  #[must_use]
  pub fn frames(&self) -> &[FrameMetadata] {
    &self.frames
  }

  #[must_use]
  pub fn id(&self) -> StackId {
    self.id
  }
}

#[derive(Debug)]
struct StackTableInner {
  by_frames: HashMap<Vec<FrameMetadata>, StackId>,
  by_id: HashMap<StackId, Arc<StackMetadata>>,
  next_id: StackId,
}

impl Default for StackTableInner {
  fn default() -> Self {
    Self {
      by_frames: HashMap::new(),
      by_id: HashMap::new(),
      next_id: 1,
    }
  }
}

/// Interns stack traces and provides their resolved metadata.
#[derive(Debug, Default)]
pub struct StackTable {
  inner: Mutex<StackTableInner>,
}

impl StackTable {
  /// Explicitly associate metadata with a stack identifier.
  ///
  /// This helper is primarily intended for tests and for plumbing pre-existing
  /// stack identifiers gathered outside of the Rust tracer.
  pub fn insert_with_id<I>(&self, stack_id: StackId, frames: I)
  where
    I: Into<Vec<FrameMetadata>>,
  {
    let frames: Vec<FrameMetadata> = frames.into();
    let mut inner = self.lock_inner();

    let metadata = Arc::new(StackMetadata {
      frames: Arc::from(frames.clone().into_boxed_slice()),
      id: stack_id,
    });
    inner.by_frames.insert(frames, stack_id);
    inner.by_id.insert(stack_id, metadata);

    if inner.next_id <= stack_id {
      inner.next_id = stack_id.saturating_add(1);
    }
  }

  /// Intern the provided stack frames and return their stable identifier.
  pub fn intern<I>(&self, frames: I) -> StackId
  where
    I: Into<Vec<FrameMetadata>>,
  {
    let frames: Vec<FrameMetadata> = frames.into();
    let mut inner = self.lock_inner();
    if let Some(existing) = inner.by_frames.get(&frames).copied() {
      return existing;
    }

    let stack_id = inner.next_id;
    inner.next_id = inner.next_id.saturating_add(1);

    let metadata = Arc::new(StackMetadata {
      frames: Arc::from(frames.clone().into_boxed_slice()),
      id: stack_id,
    });
    inner.by_frames.insert(frames, stack_id);
    inner.by_id.insert(stack_id, metadata);

    stack_id
  }

  fn lock_inner(&self) -> MutexGuard<'_, StackTableInner> {
    match self.inner.lock() {
      Ok(guard) => guard,
      Err(err) => err.into_inner(),
    }
  }

  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Resolve a stack identifier back into its metadata, if known.
  #[must_use]
  pub fn resolve(&self, stack_id: StackId) -> Option<Arc<StackMetadata>> {
    let inner = self.lock_inner();
    inner.by_id.get(&stack_id).cloned()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn interns_and_reuses_stack_ids() {
    let table = StackTable::new();
    let frames = vec![
      FrameMetadata::new("file.py", "func", 10),
      FrameMetadata::new("other.py", "helper", 3),
    ];
    let first = table.intern(frames.clone());
    let second = table.intern(frames);
    assert_eq!(first, second);
  }

  #[test]
  fn resolves_metadata_for_known_stack() {
    let table = StackTable::new();
    let frames = vec![FrameMetadata::new("file.py", "func", 10)];
    let stack_id = table.intern(frames.clone());

    let resolved = table.resolve(stack_id).expect("expected stack metadata");
    assert_eq!(resolved.id(), stack_id);
    assert_eq!(resolved.frames(), frames.as_slice());
  }

  #[test]
  fn allows_explicit_stack_id_registration() {
    let table = StackTable::new();
    table.insert_with_id(42, vec![FrameMetadata::new("file.py", "func", 1)]);

    let resolved = table.resolve(42).expect("missing stack 42");
    assert_eq!(resolved.id(), 42);
    assert_eq!(resolved.frames()[0].lineno, 1);
  }
}
