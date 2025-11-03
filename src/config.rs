use std::time::Duration;

/// Controls how the tracer collects allocation events.
#[derive(Debug, Clone)]
pub struct TracerConfig {
  /// Maximum number of Python frames captured per allocation.
  pub max_stack_depth: u16,
  /// Probability-based sampling rate in the range `[0.0, 1.0]`.
  pub sampling_rate: f64,
  /// Sample every N bytes when set (takes precedence over `sampling_rate`).
  pub sampling_bytes: Option<u64>,
  /// Per-thread ring buffer size, expressed in bytes.
  pub ring_buffer_bytes: usize,
  /// Whether to attempt capturing native stack frames in addition to Python.
  pub capture_native: bool,
  /// Whether to enable the tracer immediately once constructed.
  pub start_enabled: bool,
  /// How frequently the background worker drains per-thread buffers.
  pub drain_interval: Duration,
}

impl Default for TracerConfig {
  fn default() -> Self {
    Self {
      max_stack_depth: 1,
      sampling_rate: 1.0,
      sampling_bytes: None,
      ring_buffer_bytes: 256 * 1024,
      capture_native: false,
      start_enabled: true,
      drain_interval: Duration::from_millis(25),
    }
  }
}

impl TracerConfig {
  /// Builder-style helper to adjust the maximum stack depth.
  #[must_use]
  pub fn with_max_stack_depth(mut self, depth: u16) -> Self {
    self.max_stack_depth = depth;
    self
  }

  /// Explicitly disable eager tracer start-up.
  #[must_use]
  pub fn disabled(mut self) -> Self {
    self.start_enabled = false;
    self
  }
}
