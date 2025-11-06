//! Core library entry point for the Rust-based tracemalloc implementation.
//!
//! The goal of this crate is to provide a low-overhead, highly concurrent memory
//! trace collector that can be surfaced to Python through an FFI layer.

use {
  backtrace::{Frame, SymbolName},
  crossbeam_queue::ArrayQueue,
  dashmap::{DashMap, mapref::entry::Entry},
  memmap2::MmapMut,
  nohash_hasher::BuildNoHashHasher,
  ring_buffer::ThreadBufferInner,
  serde::{Serialize, Serializer, ser::SerializeStruct},
  smallvec::SmallVec,
  stack_capture::StackCollector,
  std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsStr,
    fmt::{self, Display, Formatter},
    fs::OpenOptions,
    io::{self, Write},
    mem::size_of,
    path::Path,
    sync::{
      Arc, Condvar, LazyLock, Mutex, MutexGuard, Weak,
      atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
  },
};

mod aggregator;
mod config;
mod event;
mod export;
mod ring_buffer;
mod snapshot;
mod stack;
mod stack_capture;
mod state;

#[cfg(not(windows))]
use export::build_pprof_profile;

#[cfg(not(windows))]
use pprof::protos::{Function, Line, Location, Profile, Sample, ValueType};

#[cfg(not(windows))]
use prost::Message;

pub use {
  aggregator::Aggregator,
  config::TracerConfig,
  event::{AllocationEvent, EventKind, StackId},
  export::{
    ExportError, JsonLinesWriter, MmapJsonStreamWriter, SnapshotStreamWriter,
  },
  ring_buffer::{DrainAction, ThreadBuffer},
  snapshot::{Snapshot, SnapshotDelta, SnapshotRecord},
  stack::{FrameMetadata, StackMetadata, StackTable},
  state::{Tracer, TracerBuilder},
};
