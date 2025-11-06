"""
Demonstration script that activates the Rust tracemalloc shim, drives a synthetic
allocation workload, and prints the heaviest Python call sites.
"""

from __future__ import annotations

import statistics
import time
from dataclasses import dataclass
from typing import Iterable, List

import rust_tracemalloc as tracemalloc


@dataclass
class AllocationStat:
  location: str
  total_bytes: int
  allocations: int


def churn_dataset(batch: int, record_size: int) -> List[bytes]:
  """Allocate and partially discard byte buffers to mimic noisy workloads."""
  arena: List[bytes] = []

  for _ in range(batch):
    arena.append(bytearray(record_size))

  # Drop half of the arena to produce mixed alloc/free traffic.
  del arena[::2]

  return arena


def collect_top_records(
  records: Iterable[dict],
  limit: int = 5,
) -> List[AllocationStat]:
  top: List[AllocationStat] = []
  for record in records:
    frames = record.get('frames') or []

    if not frames:
      location = '<unknown>'
    else:
      frame = frames[0]
      location = f'{frame["filename"]}:{frame["lineno"]} ({frame["function"]})'

    top.append(
      AllocationStat(
        location=location,
        total_bytes=int(record['current_bytes']),
        allocations=int(record['allocations']),
      )
    )

  top.sort(key=lambda stat: stat.total_bytes, reverse=True)

  return top[:limit]


def main() -> None:
  tracemalloc.start(nframe=8, capture_native=False)

  warmup = []

  for size in (128, 256, 512):
    warmup.extend(churn_dataset(batch=50, record_size=size))

  del warmup

  sample_sizes = []

  for size in (1024, 2048, 4096):
    start = time.perf_counter()
    batch = churn_dataset(batch=80, record_size=size)
    sample_sizes.append(len(batch))
    del batch
    print(f'allocated churn batch at {size} bytes in {time.perf_counter() - start:.4f}s')

  snapshot = tracemalloc.take_snapshot()

  stats = collect_top_records(snapshot.records())

  print('\nTop allocation sites:')

  for stat in stats:
    print(f'{stat.total_bytes:>12} B across {stat.allocations:>6} allocs :: {stat.location}')

  if sample_sizes:
    print(
      '\nWorkload samples:',
      f'mean batch size {statistics.mean(sample_sizes):.1f}',
      f'min {min(sample_sizes)}, max {max(sample_sizes)}',
    )


if __name__ == '__main__':
  main()
