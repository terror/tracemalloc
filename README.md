## tracemalloc

[![CI](https://github.com/terror/tracemalloc/actions/workflows/ci.yaml/badge.svg)](https://github.com/terror/tracemalloc/actions/workflows/ci.yaml)
[![codecov](https://codecov.io/gh/terror/tracemalloc/graph/badge.svg?token=7CH4XDXO7Z)](https://codecov.io/gh/terror/tracemalloc)

**tracemalloc** is a Rust re-implementation of Python's
[tracemalloc](https://docs.python.org/3/library/tracemalloc.html) module.

This project aims to improve upon the standard library implementation in the
following ways:

- Lower overhead and better concurrency by moving allocation work into
  per-thread [ring buffers](https://en.wikipedia.org/wiki/Circular_buffer),
  draining asynchronously, and using sharded aggregators so the hot path avoids
  locks and allocator recursion.
- Tighter metadata via aggressive
  [interning](<https://en.wikipedia.org/wiki/Interning_(computer_science)>),
  prefix-compressed stack traces, and delta-encoded snapshots so repeated
  file/frame info doesn’t explode memory.
- Broader visibility by optionally interposing on the native allocator, tagging
  allocations by domain/ thread/interpreter, and observing some extension-level
  allocations traditional tracemalloc misses.
- Stronger tooling with streaming and
  [mmap](https://en.wikipedia.org/wiki/Mmap)-backed snapshots, instantaneous
  diffs, open exports ([pprof](https://github.com/google/pprof) / Perfetto), and
  time-sliced queries for modern profiling workflows.
- Robustness upgrades like reentrancy guards, lossy backoff when buffers fill,
  and careful shutdown handling to keep tracing safe even under stress.

## Prior Art

This project is heavily inspired by the standard library implementation of
[tracemalloc](https://github.com/python/cpython/blob/3.14/Lib/tracemalloc.py),
so do go check it out!
