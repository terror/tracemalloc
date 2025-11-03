## tracemalloc

Rust-focused reimplementation of Python's `tracemalloc` module. The project
aims to deliver lower overhead, tighter metadata control, and friendlier tooling
for analysing Python allocation behaviour.

### Current status

- Core crate scaffolding with allocation events, per-thread buffer placeholder,
  aggregator, and snapshot types.
- Ergonomic `Tracer` API that can be embedded in tests or future FFI layers.
- Unit tests covering the buffer, aggregator, and tracer enable/disable path.

### Quick start

```bash
cargo run
```

The demo executable records a couple of synthetic allocation events and prints a
snapshot of the aggregated state.

### Next steps

1. Replace the placeholder `ThreadBuffer` with a proper lock-free queue stored in
   thread-local storage. Integrate a background worker that drains buffers into
   the `Aggregator`.
2. Build a stack interning layer: map raw frames → interned IDs, add delta
   compression for snapshots, and surface them through `SnapshotRecord`.
3. Implement CPython hooks (via `pyo3`) that feed real allocation events into the
   tracer. Mirror the stdlib `tracemalloc` API (`start`, `stop`, `take_snapshot`,
   `statistics`).
4. Add exporters: JSON snapshot dump and pprof-compatible heap profile writer.
5. Flesh out configuration (sampling by bytes, native stack capture) and expose
   knobs through Python bindings.
