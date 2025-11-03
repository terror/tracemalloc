use std::sync::{
  Arc, Mutex,
  atomic::{AtomicBool, Ordering},
};

use crate::aggregator::Aggregator;
use crate::config::TracerConfig;
use crate::event::AllocationEvent;
use crate::snapshot::Snapshot;

/// Thin builder that customizes `TracerConfig` without exposing all knobs up front.
#[derive(Debug, Default)]
pub struct TracerBuilder {
  config: TracerConfig,
}

impl TracerBuilder {
  #[must_use]
  pub fn new() -> Self {
    Self {
      config: TracerConfig::default(),
    }
  }

  #[must_use]
  pub fn with_config(mut self, config: TracerConfig) -> Self {
    self.config = config;
    self
  }

  #[must_use]
  pub fn max_stack_depth(mut self, depth: u16) -> Self {
    self.config.max_stack_depth = depth;
    self
  }

  #[must_use]
  pub fn sampling_rate(mut self, rate: f64) -> Self {
    self.config.sampling_rate = rate.clamp(0.0, 1.0);
    self
  }

  #[must_use]
  pub fn capture_native(mut self, capture: bool) -> Self {
    self.config.capture_native = capture;
    self
  }

  #[must_use]
  pub fn start_enabled(mut self, enabled: bool) -> Self {
    self.config.start_enabled = enabled;
    self
  }

  #[must_use]
  pub fn finish(self) -> Tracer {
    Tracer::with_config(self.config)
  }
}

#[derive(Debug)]
struct TracerInner {
  config: TracerConfig,
  enabled: AtomicBool,
  aggregator: Mutex<Aggregator>,
}

/// Entry point for recording allocation events and producing snapshots.
///
/// The first milestone keeps the implementation straightforward: events recorded
/// through `record_event` are applied directly to the in-process aggregator.
/// Subsequent milestones will introduce per-thread buffers, background drainers,
/// and a Python-facing FFI surface.
#[derive(Clone, Debug)]
pub struct Tracer {
  inner: Arc<TracerInner>,
}

impl Tracer {
  #[must_use]
  pub fn new() -> Self {
    Self::with_config(TracerConfig::default())
  }

  #[must_use]
  pub fn with_config(config: TracerConfig) -> Self {
    let enabled = AtomicBool::new(config.start_enabled);
    let inner = TracerInner {
      config,
      enabled,
      aggregator: Mutex::new(Aggregator::new()),
    };

    Self {
      inner: Arc::new(inner),
    }
  }

  #[must_use]
  pub fn builder() -> TracerBuilder {
    TracerBuilder::new()
  }

  #[must_use]
  pub fn config(&self) -> &TracerConfig {
    &self.inner.config
  }

  pub fn enable(&self) {
    self.inner.enabled.store(true, Ordering::Release);
  }

  pub fn disable(&self) {
    self.inner.enabled.store(false, Ordering::Release);
  }

  #[must_use]
  pub fn enabled(&self) -> bool {
    self.inner.enabled.load(Ordering::Acquire)
  }

  /// Feed a pre-built event directly into the aggregator.
  ///
  /// This is primarily used for early unit tests before the hot path is wired up.
  pub fn record_event(&self, event: AllocationEvent) {
    if !self.enabled() {
      return;
    }

    if let Ok(mut aggregator) = self.inner.aggregator.lock() {
      aggregator.ingest(std::iter::once(event));
    }
  }

  #[must_use]
  pub fn snapshot(&self) -> Snapshot {
    let guard = self.inner.aggregator.lock().expect("aggregator poisoned");
    guard.snapshot()
  }

  pub fn reset(&self) {
    if let Ok(mut aggregator) = self.inner.aggregator.lock() {
      aggregator.reset();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::event::{AllocationEvent, EventKind};

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
}
