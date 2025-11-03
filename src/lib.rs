//! Core library entry point for the Rust-based tracemalloc implementation.
//!
//! The goal of this crate is to provide a low-overhead, highly concurrent memory
//! trace collector that can be surfaced to Python through an FFI layer.

mod aggregator;
mod config;
mod event;
mod ring_buffer;
mod snapshot;
mod stack;
mod stack_capture;
mod state;

use {
  backtrace::{Frame, SymbolName},
  crossbeam_queue::ArrayQueue,
  dashmap::DashMap,
  nohash_hasher::BuildNoHashHasher,
  ring_buffer::ThreadBufferInner,
  snapshot::SnapshotRecord,
  stack_capture::StackCollector,
  std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsStr,
    mem::size_of,
    sync::{
      Arc, Condvar, Mutex, MutexGuard, Weak,
      atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
  },
};

pub use {
  aggregator::Aggregator,
  config::TracerConfig,
  event::{AllocationEvent, EventKind, StackId},
  ring_buffer::{DrainAction, ThreadBuffer},
  snapshot::{Snapshot, SnapshotDelta},
  stack::{FrameMetadata, StackMetadata, StackTable},
  state::{Tracer, TracerBuilder},
};
