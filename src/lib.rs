#![deny(clippy::all, clippy::pedantic)]
#![forbid(unsafe_code)]

//! Core library entry point for the Rust-based tracemalloc reimplementation.
//!
//! The goal of this crate is to provide a low-overhead, highly concurrent memory
//! trace collector that can be surfaced to Python through an FFI layer. The
//! implementation is intentionally staged: start with a minimal, purely Rust
//! collector and progressively layer in `CPython` integration and advanced
//! snapshot/export features.

pub mod aggregator;
pub mod config;
pub mod event;
pub mod ring_buffer;
pub mod snapshot;
pub mod stack;
mod stack_capture;
pub mod state;

pub use aggregator::Aggregator;
pub use config::TracerConfig;
pub use event::{AllocationEvent, EventKind, StackId};
pub use ring_buffer::{DrainAction, ThreadBuffer};
pub use snapshot::{Snapshot, SnapshotDelta};
pub use stack::{FrameMetadata, StackMetadata, StackTable};
pub use state::{Tracer, TracerBuilder};
