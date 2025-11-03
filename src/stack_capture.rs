use std::{ffi::OsStr, sync::Arc};

use backtrace::{Frame, SymbolName};

use crate::{
  config::TracerConfig,
  event::StackId,
  stack::{FrameMetadata, StackTable},
};

const DEFAULT_SKIP_FRAMES: usize = 5;

/// Captures stack traces and interns them through the shared stack table.
#[derive(Debug)]
pub struct StackCollector {
  capture_native: bool,
  max_depth: usize,
  skip_frames: usize,
  stack_table: Arc<StackTable>,
}

impl StackCollector {
  #[must_use]
  pub fn capture_and_intern(&self) -> StackId {
    let frames = if self.capture_native {
      self.capture_native_frames()
    } else {
      Vec::new()
    };

    let frames = if frames.is_empty() {
      vec![FrameMetadata::new("<unknown>", "<unknown>", 0)]
    } else {
      frames
    };

    self.stack_table.intern(frames)
  }

  #[must_use]
  fn capture_native_frames(&self) -> Vec<FrameMetadata> {
    let mut frames = Vec::with_capacity(self.max_depth);
    let mut remaining_skip = self.skip_frames;

    backtrace::trace(|frame| {
      if remaining_skip > 0 {
        remaining_skip -= 1;
        return true;
      }

      if frames.len() >= self.max_depth {
        return false;
      }

      frames.push(extract_metadata(frame));
      true
    });

    frames
  }

  #[must_use]
  pub fn new(stack_table: Arc<StackTable>, config: &TracerConfig) -> Self {
    let max_depth = usize::from(config.max_stack_depth.max(1));
    Self {
      capture_native: config.capture_native,
      max_depth,
      skip_frames: DEFAULT_SKIP_FRAMES,
      stack_table,
    }
  }
}

fn extract_metadata(frame: &Frame) -> FrameMetadata {
  let mut filename = None;
  let mut function = None;
  let mut lineno = None;

  backtrace::resolve_frame(frame, |symbol| {
    if filename.is_none() {
      filename = symbol
        .filename()
        .and_then(|path| path_to_string(path))
        .map(str::to_string);
    }

    if function.is_none() {
      function = symbol.name().map(|name| symbol_name_to_string(&name));
    }

    if lineno.is_none() {
      lineno = symbol.lineno();
    }
  });

  FrameMetadata::new(
    filename.unwrap_or_else(|| "<native>".to_string()),
    function.unwrap_or_else(|| "<unknown>".to_string()),
    lineno.unwrap_or(0),
  )
}

fn path_to_string(path: &std::path::Path) -> Option<&str> {
  path
    .to_str()
    .or_else(|| path.file_name().and_then(OsStr::to_str))
}

fn symbol_name_to_string(name: &SymbolName<'_>) -> String {
  format!("{name}")
}
