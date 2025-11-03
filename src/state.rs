use super::*;

thread_local! {
  static THREAD_BUFFERS: RefCell<Vec<ThreadLocalBuffer>> =
    const { RefCell::new(Vec::new()) };
}

struct ThreadLocalBuffer {
  buffer: ThreadBuffer,
  tracer_id: usize,
}

#[derive(Debug, Clone)]
struct AllocationRecord {
  size: usize,
  stack_id: StackId,
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
  pub fn native_skip_frames(mut self, skip: usize) -> Self {
    self.config.native_skip_frames = skip;
    self
  }

  #[must_use]
  pub fn new() -> Self {
    Self {
      config: TracerConfig::default(),
    }
  }

  #[must_use]
  pub fn python_skip_frames(mut self, skip: usize) -> Self {
    self.config.python_skip_frames = skip;
    self
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
  allocation_index: Mutex<HashMap<usize, AllocationRecord>>,
  buffers: Mutex<Vec<Weak<ThreadBufferInner>>>,
  config: TracerConfig,
  enabled: AtomicBool,
  pending_flush: AtomicBool,
  stack_collector: StackCollector,
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

  fn enqueue_event(&self, event: AllocationEvent) {
    let buffer = self.thread_buffer();
    if let DrainAction::FlushPending = buffer.record(event) {
      self.inner.notify_flush();
    }
  }

  #[must_use]
  pub fn new() -> Self {
    Self::with_config(TracerConfig::default())
  }

  /// Fast-path helper that captures the current stack trace and records an
  /// allocation event keyed by the interned stack identifier.
  pub fn record_allocation(&self, address: usize, size: usize) {
    if !self.enabled() {
      return;
    }

    if let Some(event) =
      self
        .inner
        .prepare_hot_event(EventKind::Allocation, address, size, None)
    {
      self.enqueue_event(event);
    }
  }

  /// Variant of `record_allocation` that uses Python-provided frames when
  /// available. This is intended to be called from the `CPython` allocator hooks
  /// once they have captured the interpreter stack.
  pub fn record_allocation_with_frames(
    &self,
    address: usize,
    size: usize,
    frames: &[FrameMetadata],
  ) {
    if !self.enabled() {
      return;
    }

    if let Some(event) = self.inner.prepare_hot_event(
      EventKind::Allocation,
      address,
      size,
      Some(frames),
    ) {
      self.enqueue_event(event);
    }
  }

  /// Fast-path helper that updates aggregated statistics for a released
  /// allocation using the previously recorded stack identifier.
  pub fn record_deallocation(&self, address: usize, size: usize) {
    if !self.enabled() {
      return;
    }

    match self.inner.prepare_hot_event(
      EventKind::Deallocation,
      address,
      size,
      None,
    ) {
      Some(event) => self.enqueue_event(event),
      None => self.record_dropped_event(1),
    }
  }

  /// Deallocation variant that leverages Python-sourced metadata when available.
  pub fn record_deallocation_with_frames(
    &self,
    address: usize,
    size: usize,
    frames: &[FrameMetadata],
  ) {
    if !self.enabled() {
      return;
    }

    match self.inner.prepare_hot_event(
      EventKind::Deallocation,
      address,
      size,
      Some(frames),
    ) {
      Some(event) => self.enqueue_event(event),
      None => self.record_dropped_event(1),
    }
  }

  fn record_dropped_event(&self, count: u64) {
    let dropped = AllocationEvent::new(EventKind::Dropped { count }, 0, 0, 0);
    self.enqueue_event(dropped);
  }

  /// Feed a pre-built event directly into the per-thread buffer. A background worker
  /// will eventually drain it into the aggregator.
  pub fn record_event(&self, event: AllocationEvent) {
    if !self.enabled() {
      return;
    }

    self.enqueue_event(event);
  }

  /// Update aggregated statistics for a reallocation (`PyMem_Realloc` style).
  ///
  /// Generates both a deallocation for the old pointer and an allocation for
  /// the new pointer. Callers should supply the old allocation size when
  /// available; otherwise the tracer falls back to the previously recorded
  /// size.
  pub fn record_reallocation(
    &self,
    old_address: usize,
    old_size: usize,
    new_address: usize,
    new_size: usize,
  ) {
    if !self.enabled() {
      return;
    }

    match self.inner.prepare_reallocation(
      old_address,
      old_size,
      new_address,
      new_size,
      None,
    ) {
      Some((dealloc, alloc)) => {
        self.enqueue_event(dealloc);
        self.enqueue_event(alloc);
      }
      None => self.record_dropped_event(2),
    }
  }

  /// Reallocation helper with Python frame metadata supplied by the caller.
  pub fn record_reallocation_with_frames(
    &self,
    old_address: usize,
    old_size: usize,
    new_address: usize,
    new_size: usize,
    frames: &[FrameMetadata],
  ) {
    if !self.enabled() {
      return;
    }

    let frame_option = (!frames.is_empty()).then_some(frames);

    match self.inner.prepare_reallocation(
      old_address,
      old_size,
      new_address,
      new_size,
      frame_option,
    ) {
      Some((dealloc, alloc)) => {
        self.enqueue_event(dealloc);
        self.enqueue_event(alloc);
      }
      None => self.record_dropped_event(2),
    }
  }

  pub fn reset(&self) {
    self.inner.drain_buffers();

    if let Ok(mut aggregator) = self.inner.aggregator.lock() {
      aggregator.reset();
    }

    if let Ok(mut allocations) = self.inner.allocation_index.lock() {
      allocations.clear();
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

  #[must_use]
  pub fn stack_table(&self) -> Arc<StackTable> {
    Arc::clone(&self.inner.stack_table)
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

    let stack_collector =
      StackCollector::new(Arc::clone(&stack_table), &config);

    Self {
      aggregator: Mutex::new(Aggregator::new(Arc::clone(&stack_table))),
      allocation_index: Mutex::new(HashMap::new()),
      buffers: Mutex::new(Vec::new()),
      config,
      enabled,
      pending_flush: AtomicBool::new(false),
      stack_collector,
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

  fn prepare_hot_event(
    &self,
    kind: EventKind,
    address: usize,
    size: usize,
    frames: Option<&[FrameMetadata]>,
  ) -> Option<AllocationEvent> {
    match kind {
      EventKind::Allocation => {
        let stack_id = self
          .stack_collector
          .capture_and_intern(frames.filter(|f| !f.is_empty()));

        let mut index = match self.allocation_index.lock() {
          Ok(guard) => guard,
          Err(err) => err.into_inner(),
        };

        index.insert(address, AllocationRecord { size, stack_id });

        Some(AllocationEvent::new(kind, address, size, stack_id))
      }
      EventKind::Deallocation => {
        let mut index = match self.allocation_index.lock() {
          Ok(guard) => guard,
          Err(err) => err.into_inner(),
        };

        let record = index.remove(&address)?;

        let size = if size == 0 { record.size } else { size };

        Some(AllocationEvent::new(kind, address, size, record.stack_id))
      }
      EventKind::Dropped { .. } => None,
    }
  }

  fn prepare_reallocation(
    &self,
    old_address: usize,
    old_size: usize,
    new_address: usize,
    new_size: usize,
    frames: Option<&[FrameMetadata]>,
  ) -> Option<(AllocationEvent, AllocationEvent)> {
    let mut index = match self.allocation_index.lock() {
      Ok(guard) => guard,
      Err(err) => err.into_inner(),
    };

    let record = index.remove(&old_address)?;

    let recorded_old_size = if old_size == 0 { record.size } else { old_size };

    let dealloc = AllocationEvent::new(
      EventKind::Deallocation,
      old_address,
      recorded_old_size,
      record.stack_id,
    );

    let stack_id = self
      .stack_collector
      .capture_and_intern(frames.filter(|f| !f.is_empty()));

    index.insert(
      new_address,
      AllocationRecord {
        size: new_size,
        stack_id,
      },
    );

    let alloc = AllocationEvent::new(
      EventKind::Allocation,
      new_address,
      new_size,
      stack_id,
    );

    Some((dealloc, alloc))
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

  #[test]
  fn hot_path_stack_capture_produces_metadata() {
    let tracer = Tracer::builder().capture_native(true).finish();
    tracer.record_allocation(0x1, 32);

    let snapshot = tracer.snapshot();
    let record = snapshot.records().first().expect("missing allocation");
    let stack = record.stack.as_ref().expect("missing captured stack");

    assert!(!stack.frames().is_empty(), "expected captured frames");
  }

  #[test]
  fn python_frames_are_prioritized_when_provided() {
    let tracer = Tracer::builder()
      .capture_native(true)
      .python_skip_frames(1)
      .finish();

    let frames = vec![
      FrameMetadata::new("internal.py", "wrapper", 1),
      FrameMetadata::new("app.py", "handler", 42),
    ];

    tracer.record_allocation_with_frames(0xdead, 24, &frames);

    let snapshot = tracer.snapshot();
    let record = snapshot.records().first().expect("missing allocation");
    let stack = record.stack.as_ref().expect("missing captured stack");

    assert_eq!(stack.frames()[0].filename.as_ref(), "app.py");
    assert_eq!(stack.frames()[0].function.as_ref(), "handler");
  }

  #[test]
  fn deallocation_reuses_allocation_stack_id() {
    let tracer = Tracer::builder().capture_native(true).finish();

    let address = 0xfeed;

    tracer.record_allocation(address, 64);
    tracer.record_deallocation(address, 64);

    let snapshot = tracer.snapshot();

    let record = snapshot
      .records()
      .first()
      .expect("missing allocation record");

    assert_eq!(record.allocations, 1);
    assert_eq!(record.deallocations, 1);
    assert_eq!(record.current_bytes, 0);
  }

  #[test]
  fn reallocation_moves_pointer_and_updates_counters() {
    let tracer = Tracer::builder().capture_native(false).finish();

    tracer.record_allocation(0x1, 32);
    tracer.record_reallocation(0x1, 32, 0x2, 64);

    let snapshot = tracer.snapshot();
    let record = snapshot.records().first().expect("missing allocation");

    assert_eq!(record.allocations, 2);
    assert_eq!(record.deallocations, 1);
    assert_eq!(record.current_bytes, 64);
  }

  #[test]
  fn reallocation_with_python_frames_updates_metadata() {
    let tracer = Tracer::builder().capture_native(false).finish();
    tracer.record_allocation(0x1, 16);

    let frames = vec![FrameMetadata::new("resize.py", "grow", 99)];
    tracer.record_reallocation_with_frames(0x1, 16, 0x2, 32, &frames);

    let snapshot = tracer.snapshot();
    let record = snapshot.records().first().expect("missing allocation");
    let stack = record.stack.as_ref().expect("missing stack metadata");

    assert_eq!(stack.frames()[0].filename.as_ref(), "resize.py");
    assert_eq!(record.current_bytes, 32);
  }

  #[test]
  fn failed_reallocation_counts_as_two_dropped_events() {
    let tracer = Tracer::new();
    tracer.record_reallocation(0xbeef, 10, 0xcafe, 20);

    let snapshot = tracer.snapshot();

    assert_eq!(snapshot.dropped_events(), 2);
    assert!(snapshot.records().is_empty());
  }

  #[test]
  fn unknown_deallocation_counts_as_dropped() {
    let tracer = Tracer::new();
    tracer.record_deallocation(0xbeef, 16);

    let snapshot = tracer.snapshot();

    assert_eq!(snapshot.dropped_events(), 1);
    assert!(snapshot.records().is_empty());
  }
}
