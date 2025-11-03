use super::*;

thread_local! {
  static STACK_ID_CACHES: RefCell<Vec<ThreadLocalStackCache>> =
    const { RefCell::new(Vec::new()) };
}

const STACK_CACHE_LIMIT: usize = 1024;

struct ThreadLocalStackCache {
  cache: StackIdCache,
  collector_id: usize,
}

#[derive(Default)]
struct StackIdCache {
  map: HashMap<Vec<FrameMetadata>, StackId>,
}

impl StackIdCache {
  fn insert(&mut self, frames: Vec<FrameMetadata>, stack_id: StackId) {
    if self.map.len() >= STACK_CACHE_LIMIT {
      self.map.clear();
    }

    self.map.insert(frames, stack_id);
  }

  fn lookup(&self, frames: &[FrameMetadata]) -> Option<StackId> {
    self.map.get(frames).copied()
  }
}

/// Captures stack traces and interns them through the shared stack table.
#[derive(Debug)]
pub struct StackCollector {
  capture_native: bool,
  max_depth: usize,
  native_skip_frames: usize,
  python_skip_frames: usize,
  stack_table: Arc<StackTable>,
}

impl StackCollector {
  #[must_use]
  pub fn capture_and_intern(
    &self,
    python_frames: Option<&[FrameMetadata]>,
  ) -> StackId {
    let mut frames = Vec::with_capacity(self.max_depth);

    if let Some(py_frames) = python_frames {
      for frame in py_frames.iter().skip(self.python_skip_frames) {
        if frames.len() >= self.max_depth {
          break;
        }
        frames.push(frame.clone());
      }
    }

    if self.capture_native && frames.len() < self.max_depth {
      for frame in self.capture_native_frames() {
        if frames.len() >= self.max_depth {
          break;
        }
        frames.push(frame);
      }
    }

    if frames.is_empty() {
      frames.push(FrameMetadata::new("<unknown>", "<unknown>", 0));
    }

    if let Some(stack_id) =
      self.with_thread_cache(|cache| cache.lookup(&frames))
    {
      return stack_id;
    }

    let stack_id = self.stack_table.intern(frames.clone());

    self.with_thread_cache(|cache| cache.insert(frames, stack_id));

    stack_id
  }

  #[must_use]
  fn capture_native_frames(&self) -> Vec<FrameMetadata> {
    let mut frames = Vec::with_capacity(self.max_depth);
    let mut remaining_skip = self.native_skip_frames;

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

  fn collector_id(&self) -> usize {
    std::ptr::from_ref(self) as usize
  }

  #[must_use]
  pub fn new(stack_table: Arc<StackTable>, config: &TracerConfig) -> Self {
    let max_depth = usize::from(config.max_stack_depth.max(1));
    Self {
      capture_native: config.capture_native,
      max_depth,
      native_skip_frames: config.native_skip_frames,
      python_skip_frames: config.python_skip_frames,
      stack_table,
    }
  }

  fn with_thread_cache<F, R>(&self, f: F) -> R
  where
    F: FnOnce(&mut StackIdCache) -> R,
  {
    let collector_id = self.collector_id();

    STACK_ID_CACHES.with(|storage| {
      let mut storage = storage.borrow_mut();

      if let Some(entry) = storage
        .iter_mut()
        .find(|entry| entry.collector_id == collector_id)
      {
        return f(&mut entry.cache);
      }

      storage.push(ThreadLocalStackCache {
        cache: StackIdCache::default(),
        collector_id,
      });

      let entry = storage
        .last_mut()
        .expect("thread-local stack cache should contain new entry");

      f(&mut entry.cache)
    })
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::TracerConfig;
  use std::sync::Arc;

  #[test]
  fn uses_python_frames_when_available() {
    let config = TracerConfig {
      capture_native: false,
      max_stack_depth: 3,
      python_skip_frames: 1,
      ..TracerConfig::default()
    };

    let table = Arc::new(StackTable::new());
    let collector = StackCollector::new(Arc::clone(&table), &config);
    let frames = vec![
      FrameMetadata::new("hidden.py", "wrapper", 1),
      FrameMetadata::new("worker.py", "run", 2),
    ];

    let stack_id = collector.capture_and_intern(Some(&frames));
    let stack = table.resolve(stack_id).expect("missing stack");
    assert_eq!(stack.frames().len(), 1);
    assert_eq!(stack.frames()[0].filename.as_ref(), "worker.py");
  }

  #[test]
  fn falls_back_to_unknown_when_no_frames() {
    let config = TracerConfig {
      capture_native: false,
      max_stack_depth: 1,
      ..TracerConfig::default()
    };

    let table = Arc::new(StackTable::new());
    let collector = StackCollector::new(Arc::clone(&table), &config);

    let stack_id = collector.capture_and_intern(Some(&[]));
    let stack = table.resolve(stack_id).expect("missing stack");
    assert_eq!(stack.frames()[0].filename.as_ref(), "<unknown>");
  }
}
