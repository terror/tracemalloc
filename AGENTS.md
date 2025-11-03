Background on `tracemalloc`:

The `tracemalloc` module is a debug tool to trace memory blocks allocated by Python.
It provides the following information:

- Traceback where an object was allocated
- Statistics on allocated memory blocks per filename and per line number: total size, number and average size of allocated memory blocks
- Compute the differences between two snapshots to detect memory leaks

To trace most memory blocks allocated by Python, the module should be started as early as possible by setting the `PYTHONTRACEMALLOC` environment variable to 1, or by using -X tracemalloc command line option. The tracemalloc.start() function can be called at runtime to start tracing Python memory allocations.

By default, a trace of an allocated memory block only stores the most recent frame (1 frame). To store 25 frames at startup: set the `PYTHONTRACEMALLOC` environment variable to 25, or use the -X tracemalloc=25 command line option.

Context for this project:

This is a Rust re-implementation of the tracemalloc program, with the goal of
improving and extending the pure Python implementation.

Some of the pain points include:
- Capturing a Python traceback and updating global stats on the hot path can be expensive.
- Central maps/counters get hammered under concurrency despite the GIL (lots of allocs happen; plus deallocs can occur off main paths).
- File names, interned strings, and repeated frame tuples balloon memory usage without aggressive interning/dedup.
- Native extensions that bypass CPython’s allocators aren’t visible.
- Snapshots can be large; exporting/streaming and post-processing are clunkier than modern profilers.

How this project aims to help:

1) Lower overhead, better concurrency

- Per-thread ring buffers (fast path):
- On allocation, write a compact event (size, addr, stack-id) into a lock-free/per-thread buffer (e.g., bounded MPSC + epochs). A background worker drains and aggregates. This moves most work off the hot path.

Lock avoidance:

- Use sharded hash maps (by thread or by stack-hash) to aggregate. Rust gives you safe concurrency (e.g., crossbeam, lock-free queues) without GIL semantics.

No-alloc in the hot path:

- Use fixed-capacity bump allocators or pre-allocated slabs for event records so the tracer doesn’t allocate while handling an allocation (avoids recursion).

2) Much tighter metadata

Aggressive interning:

- Intern file names, function names, and code object identities. Store frame IDs not strings on the hot path.

Traceback compression:

- Store stack traces as prefix-compressed vectors of frame IDs.
- Use hash-consing for identical tracebacks.
- Delta-encode across snapshots so diffs are cheap to store.

Configurable depth & sampling:

- Offer probabilistic sampling (like Go’s pprof): record k% of allocations or every Nth byte. This caps overhead for large apps.

3) Better visibility (optionally beyond CPython)

- Allocator interposition (opt-in):
- Provide a mode that wraps the process global allocator (jemalloc/mimalloc/system) inside the Python process to observe native extension allocations too. This is experimental but powerful.

Domain tagging:

- Keep Python’s “domain” concept but extend it: tag allocations by thread, interpreter (PEP 554), and extension module (when detectable via call site).

4) Superior snapshots & tooling

Streaming snapshots:

- Write snapshots incrementally to an mmap’d file or a binary log (e.g., zstd-framed) so huge programs don’t need a giant in-mem structure.

Instant diffs:

- Maintain cumulative counters per (traceback, domain) so compare_to() is O(changed-buckets) rather than O(total).

Open formats:

- Export pprof-compatible heap profiles; integrate with existing viewers (Speedscope, pprof, Perfetto).

Temporal slicing:

- Because events are timestamped as they’re drained, you can query “allocations between t1 and t2” without taking a full snapshot.

5) Robustness & safety

Reentrancy guard:

- A thread-local “in_tracer” flag prevents recursion if the tracer itself needs memory (fallback to a tiny emergency buffer).

Architecture sketch:

Backoff & lossy modes:

- If buffers fill up, drop events with counters (so you know what you missed) rather than blocking application threads.

```
+------------------------------+             +-------------------------------+
| CPython alloc hooks (PEP445) |  ----->     |  Rust tracer core (crate)     |
|  PyMem_* / PyObject_*        |             |                               |
+------------------------------+             |  - TLS ring buffers           |
                                             |  - Stack interning & cache    |
            FFI (pyo3/maturin)               |  - Sharded aggregators        |
      +---------------------------+          |  - Drainers & snapshot store  |
      | rust_tracemalloc Python   |          |  - Exporters (pprof, JSON)    |
      | module: start/stop/snap.. |          +-------------------------------+
      +---------------------------+
```

Hot path (allocation)

Quick check: enabled? sampling hit? → if no, return.
Capture return‐address frames (DWARF) or ask CPython for Python frames (configurable).
Map frames → interned IDs; push (ptr, size, stack_id, tsc) into TLS buffer. No locks.

Background drain

Periodically drain TLS buffers, resolve Python frame objects (if not already), aggregate into sharded maps keyed by (stack_id, domain).

Maintain cumulative stats and optional time buckets.

API design (Python-level)

```python
import rust_tracemalloc as tracemalloc

tracemalloc.start(nframe=25, sampling=0.1, native=True, export="pprof")
# native=True tries allocator interposition to include some C-allocations
# sampling is probability or bytes-per-sample (support both modes)

# … run workload …

snap = tracemalloc.take_snapshot()
for stat in snap.statistics('traceback')[:10]:
    print(stat.traceback.format(), stat.size, stat.count)

diff = snap.compare_to(prev, 'lineno')
tracemalloc.dump('/tmp/heap.pprof')  # open in pprof
```

Extras you can add:

```python
tracemalloc.configure(sampling_bytes=512*1024, ring_buffer_kb=256, max_stacks=5_000_000)
tracemalloc.iter_events(from_ts=..., to_ts=...) for streaming analysis
tracemalloc.set_filter(lambda frame: not frame.filename.endswith('.venv/...'))
```

Implementation tips (Rust):

- FFI layer: pyo3 + maturin for packaging; limited Python object work on the hot path—prefer storing PyCodeObject* IDs and resolve lazily.

Data structures:

- DashMap (or custom shard) for aggregations keyed by (StackId, Domain).
- nohash_hasher::BuildNoHashHasher<u64> if you use stable 64-bit stack IDs.
- stack-vec/small-vec for short traces to avoid heap allocs.

Stack capture:

- For Python frames: PyThreadState_GetFrame (careful with overhead) or CPython’s fast frame APIs when available. Cache mapping from instruction pointer → (filename, lineno) when native=True.
- For native frames: backtrace/unwind crates; symbolize off the hot path.
- Compression: zstd dictionary training over repeated tracebacks; prefix-trie for frame sequences.

Testing & safety:

- Fuzz snapshot readers.
- Stress with allocation storms (e.g., regex parsing, JSON loads).
- Validate that tracer never allocates on hot path under low memory.

What gains to expect:

- Lower CPU overhead under heavy allocation because most work shifts to per-thread buffers + background aggregation.
- Less memory for metadata via interning/compression (tracebacks are highly repetitive).
- Broader coverage (optional native mode) surfaces leaks in C extensions that tracemalloc misses.
- Better operability thanks to streaming logs and pprof/Perfetto exports.

Pitfalls to watch:

- Signal/async safety: don’t do frame walking in signal handlers; coordinate with faulthandler.
- Interpreter shutdown: flush buffers early; keep snapshot writers robust when Python objects are being torn down.
- Multi-interpreter (PEP 554): keep per-interpreter state; don’t share intern tables unless explicitly configured.

Platform quirks: Windows stack unwinding, macOS hardened runtime symbolization, musl vs glibc differences.

A minimal “first milestone”:

- Wrap Py allocators; record (size, ptr) with sampling and nframe=1 (filename:lineno only).
- Per-thread ring buffers + background aggregator keyed by (filename:lineno).
- start/stop/take_snapshot/statistics parity with Python; unit tests mirroring the stdlib tests.
- Basic JSON export + pprof heap export.

General tips:

- Use `cargo add` if you need to add dependencies.
- Make extensive use of `cargo fmt`, `cargo clippy` and `cargo test` in between
  code changes.
- Try to make the code you write testable, and try to test your code.
