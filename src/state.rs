use std::{
  cell::RefCell,
  sync::{
    Arc, Condvar, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
  },
  thread,
};

use crate::{
  aggregator::Aggregator,
  config::TracerConfig,
  event::{AllocationEvent, EventKind},
  ring_buffer::{DrainAction, ThreadBuffer, ThreadBufferInner},
  snapshot::Snapshot,
  stack::StackTable,
};

thread_local! {
  static THREAD_BUFFERS: RefCell<Vec<ThreadLocalBuffer>> =
    const { RefCell::new(Vec::new()) };
}

struct ThreadLocalBuffer {
  buffer: ThreadBuffer,
  tracer_id: usize,
}

/// Thin builder that customizes `TracerConfig` without exposing all knobs up front.
#[derive(Debug, Default)]
pub struct TracerBuilder {
  config: TracerConfig,
}

impl TracerBuilder {
  #[must_use]
  pub fn capture_native(mut self, capture: bool) -> Self {
    self.config.capture_native = capture;
    self
  }

  #[must_use]
  pub fn finish(self) -> Tracer {
    Tracer::with_config(self.config)
  }

  #[must_use]
  pub fn max_stack_depth(mut self, depth: u16) -> Self {
    self.config.max_stack_depth = depth;
    self
  }

  #[must_use]
  pub fn new() -> Self {
    Self {
      config: TracerConfig::default(),
    }
  }

  #[must_use]
  pub fn sampling_rate(mut self, rate: f64) -> Self {
    self.config.sampling_rate = rate.clamp(0.0, 1.0);
    self
  }

  #[must_use]
  pub fn start_enabled(mut self, enabled: bool) -> Self {
    self.config.start_enabled = enabled;
    self
  }

  #[must_use]
  pub fn with_config(mut self, config: TracerConfig) -> Self {
    self.config = config;
    self
  }
}

#[derive(Debug)]
struct TracerInner {
  aggregator: Mutex<Aggregator>,
  buffers: Mutex<Vec<Weak<ThreadBufferInner>>>,
  config: TracerConfig,
  enabled: AtomicBool,
  pending_flush: AtomicBool,
  stack_table: Arc<StackTable>,
  stop_flag: AtomicBool,
  worker_handle: Mutex<Option<thread::JoinHandle<()>>>,
  worker_sync: WorkerSync,
}

/// Entry point for recording allocation events and producing snapshots.
///
/// The fast path records allocation events into per-thread lock-free buffers that a
/// background worker drains into the global aggregator.
#[derive(Clone, Debug)]
pub struct Tracer {
  inner: Arc<TracerInner>,
}

impl Default for Tracer {
  fn default() -> Self {
    Self::new()
  }
}

impl Tracer {
  #[must_use]
  pub fn builder() -> TracerBuilder {
    TracerBuilder::new()
  }

  #[must_use]
  pub fn config(&self) -> &TracerConfig {
    &self.inner.config
  }

  #[must_use]
  pub fn stack_table(&self) -> Arc<StackTable> {
    Arc::clone(&self.inner.stack_table)
  }

  pub fn disable(&self) {
    self.inner.enabled.store(false, Ordering::Release);
  }

  pub fn enable(&self) {
    self.inner.enabled.store(true, Ordering::Release);
  }

  #[must_use]
  pub fn enabled(&self) -> bool {
    self.inner.enabled.load(Ordering::Acquire)
  }

  #[must_use]
  pub fn new() -> Self {
    Self::with_config(TracerConfig::default())
  }

  /// Feed a pre-built event directly into the per-thread buffer. A background worker
  /// will eventually drain it into the aggregator.
  pub fn record_event(&self, event: AllocationEvent) {
    if !self.enabled() {
      return;
    }

    let buffer = self.thread_buffer();
    if let DrainAction::FlushPending = buffer.record(event) {
      self.inner.notify_flush();
    }
  }

  pub fn reset(&self) {
    self.inner.drain_buffers();
    if let Ok(mut aggregator) = self.inner.aggregator.lock() {
      aggregator.reset();
    }
  }

  /// Produce a point-in-time snapshot of the aggregated allocations.
  ///
  /// # Panics
  ///
  /// Panics if the internal aggregator mutex is poisoned.
  #[must_use]
  pub fn snapshot(&self) -> Snapshot {
    self.inner.drain_buffers();
    let guard = self.inner.aggregator.lock().expect("aggregator poisoned");
    guard.snapshot()
  }

  fn thread_buffer(&self) -> ThreadBuffer {
    let tracer_id = Arc::as_ptr(&self.inner) as usize;
    THREAD_BUFFERS.with(|storage| {
      let mut storage = storage.borrow_mut();
      if let Some(entry) =
        storage.iter().find(|entry| entry.tracer_id == tracer_id)
      {
        return entry.buffer.clone();
      }

      let buffer = self.inner.register_thread_buffer();
      storage.push(ThreadLocalBuffer {
        tracer_id,
        buffer: buffer.clone(),
      });
      buffer
    })
  }

  #[must_use]
  pub fn with_config(config: TracerConfig) -> Self {
    let inner = Arc::new(TracerInner::new(config));
    TracerInner::start_worker(&inner);
    Self { inner }
  }
}

impl Drop for Tracer {
  fn drop(&mut self) {
    if Arc::strong_count(&self.inner) == 2 {
      self.inner.request_shutdown();
    }
  }
}

#[derive(Debug)]
struct WorkerSync {
  condvar: Condvar,
  lock: Mutex<()>,
}

impl WorkerSync {
  fn new() -> Self {
    Self {
      condvar: Condvar::new(),
      lock: Mutex::new(()),
    }
  }
}

impl TracerInner {
  fn drain_buffers(&self) {
    let mut events = Vec::new();

    {
      let mut buffers = self.buffers.lock().expect("buffers poisoned");
      buffers.retain(|weak| {
        if let Some(buffer) = weak.upgrade() {
          let dropped = buffer.drain_into(&mut events);
          if dropped > 0 {
            events.push(AllocationEvent::new(
              EventKind::Dropped { count: dropped },
              0,
              0,
              0,
            ));
          }
          true
        } else {
          false
        }
      });
    }

    if !events.is_empty()
      && let Ok(mut aggregator) = self.aggregator.lock()
    {
      aggregator.ingest(events.drain(..));
    }
  }

  fn new(config: TracerConfig) -> Self {
    let enabled = AtomicBool::new(config.start_enabled);
    let stack_table = Arc::new(StackTable::new());
    Self {
      aggregator: Mutex::new(Aggregator::new(Arc::clone(&stack_table))),
      buffers: Mutex::new(Vec::new()),
      config,
      enabled,
      pending_flush: AtomicBool::new(false),
      stack_table,
      stop_flag: AtomicBool::new(false),
      worker_handle: Mutex::new(None),
      worker_sync: WorkerSync::new(),
    }
  }

  fn notify_flush(&self) {
    if !self.pending_flush.swap(true, Ordering::AcqRel) {
      self.worker_sync.condvar.notify_one();
    }
  }

  fn register_thread_buffer(&self) -> ThreadBuffer {
    let buffer = ThreadBuffer::new(&self.config);
    let weak = buffer.downgrade();
    let mut buffers = self.buffers.lock().expect("buffers poisoned");
    buffers.push(weak);
    buffer
  }

  fn request_shutdown(&self) {
    if !self.stop_flag.swap(true, Ordering::AcqRel) {
      let guard = match self.worker_sync.lock.lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
      };
      drop(guard);
      self.worker_sync.condvar.notify_all();

      let mut handle = match self.worker_handle.lock() {
        Ok(handle) => handle,
        Err(err) => err.into_inner(),
      };
      if let Some(join_handle) = handle.take() {
        let _ = join_handle.join();
      }
    }
  }

  fn run_worker(self: Arc<Self>) {
    loop {
      if self.stop_flag.load(Ordering::Acquire) {
        break;
      }

      self.drain_buffers();

      if self.stop_flag.load(Ordering::Acquire) {
        break;
      }

      let mut guard =
        self.worker_sync.lock.lock().expect("worker lock poisoned");
      if self.stop_flag.load(Ordering::Acquire) {
        drop(guard);
        break;
      }

      if !self.pending_flush.swap(false, Ordering::AcqRel) {
        let (g, _) = self
          .worker_sync
          .condvar
          .wait_timeout(guard, self.config.drain_interval)
          .expect("worker condvar poisoned");
        guard = g;
      }
      drop(guard);
    }

    self.drain_buffers();
  }

  fn start_worker(this: &Arc<Self>) {
    let worker = Arc::clone(this);
    let handle = thread::Builder::new()
      .name("tracemalloc-drain".into())
      .spawn(move || worker.run_worker())
      .expect("failed to spawn tracemalloc drain worker");

    let mut slot = this.worker_handle.lock().expect("worker handle poisoned");
    *slot = Some(handle);
  }
}

impl Drop for TracerInner {
  fn drop(&mut self) {
    self.request_shutdown();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::event::{AllocationEvent, EventKind};
  use crate::stack::FrameMetadata;

  #[test]
  fn disabled_tracer_drops_events() {
    let tracer = Tracer::builder().start_enabled(false).finish();
    tracer.record_event(AllocationEvent::new(
      EventKind::Allocation,
      0x1,
      16,
      7,
    ));

    assert!(tracer.snapshot().records().is_empty());
  }

  #[test]
  fn enabled_tracer_collects_events() {
    let tracer = Tracer::new();
    tracer.record_event(AllocationEvent::new(
      EventKind::Allocation,
      0x1,
      16,
      7,
    ));

    assert_eq!(tracer.snapshot().records().len(), 1);
  }

  #[test]
  fn snapshot_drains_thread_buffers() {
    let tracer = Tracer::new();
    tracer.record_event(AllocationEvent::new(
      EventKind::Allocation,
      0x1,
      8,
      99,
    ));

    // Snapshot should force a synchronous drain even if the worker has not run yet.
    let snapshot = tracer.snapshot();
    assert!(
      snapshot
        .records()
        .iter()
        .any(|record| record.stack_id == 99),
      "expected stack 99 in snapshot"
    );
  }

  #[test]
  fn snapshot_includes_stack_metadata_when_available() {
    let tracer = Tracer::new();
    tracer
      .stack_table()
      .insert_with_id(123, vec![FrameMetadata::new("file.py", "fn", 1)]);
    tracer.record_event(AllocationEvent::new(
      EventKind::Allocation,
      0x1,
      8,
      123,
    ));

    let snapshot = tracer.snapshot();
    let record = snapshot
      .records()
      .iter()
      .find(|record| record.stack_id == 123)
      .expect("missing stack 123");
    assert!(record.stack.is_some(), "expected stack metadata");
  }
}
