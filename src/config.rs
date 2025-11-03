use super::*;

/// Controls how the tracer collects allocation events.
#[derive(Debug, Clone)]
pub struct TracerConfig {
  /// Whether to attempt capturing native stack frames in addition to Python.
  pub capture_native: bool,
  /// How frequently the background worker drains per-thread buffers.
  pub drain_interval: Duration,
  /// Maximum number of Python frames captured per allocation.
  pub max_stack_depth: u16,
  /// Number of native frames to skip before recording the stack.
  pub native_skip_frames: usize,
  /// Number of Python frames to skip (when provided externally).
  pub python_skip_frames: usize,
  /// Per-thread ring buffer size, expressed in bytes.
  pub ring_buffer_bytes: usize,
  /// Sample every N bytes when set (takes precedence over `sampling_rate`).
  pub sampling_bytes: Option<u64>,
  /// Probability-based sampling rate in the range `[0.0, 1.0]`.
  pub sampling_rate: f64,
  /// Whether to enable the tracer immediately once constructed.
  pub start_enabled: bool,
}

impl Default for TracerConfig {
  fn default() -> Self {
    Self {
      capture_native: true,
      drain_interval: Duration::from_millis(25),
      max_stack_depth: 1,
      native_skip_frames: 5,
      python_skip_frames: 0,
      ring_buffer_bytes: 256 * 1024,
      sampling_bytes: None,
      sampling_rate: 1.0,
      start_enabled: true,
    }
  }
}

impl TracerConfig {
  /// Explicitly disable eager tracer start-up.
  #[must_use]
  pub fn disabled(mut self) -> Self {
    self.start_enabled = false;
    self
  }

  /// Builder-style helper to adjust the maximum stack depth.
  #[must_use]
  pub fn with_max_stack_depth(mut self, depth: u16) -> Self {
    self.max_stack_depth = depth;
    self
  }

  /// Builder-style helper to adjust native frame skip depth.
  #[must_use]
  pub fn with_native_skip_frames(mut self, skip: usize) -> Self {
    self.native_skip_frames = skip;
    self
  }

  /// Builder-style helper to adjust Python frame skip depth.
  #[must_use]
  pub fn with_python_skip_frames(mut self, skip: usize) -> Self {
    self.python_skip_frames = skip;
    self
  }
}
