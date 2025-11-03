use tracemalloc::{AllocationEvent, EventKind, Tracer};

fn main() {
  let tracer = Tracer::new();

  tracer.record_event(AllocationEvent::new(EventKind::Allocation, 0x1, 128, 1));
  tracer.record_event(AllocationEvent::new(EventKind::Allocation, 0x2, 64, 1));
  tracer.record_event(AllocationEvent::new(
    EventKind::Deallocation,
    0x2,
    64,
    1,
  ));

  let snapshot = tracer.snapshot();

  println!("=== demo snapshot ===");
  for record in snapshot.records() {
    println!(
      "stack_id={} current={}B allocs={} frees={}",
      record.stack_id,
      record.current_bytes,
      record.allocations,
      record.deallocations
    );
  }
  println!("dropped events: {}", snapshot.dropped_events());
}
