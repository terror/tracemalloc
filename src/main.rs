use tracemalloc::Tracer;

fn main() {
  let tracer = Tracer::new();
  tracer.record_allocation(0x1, 128);
  tracer.record_allocation(0x2, 64);
  tracer.record_deallocation(0x2, 64);
  tracer.record_reallocation(0x1, 128, 0x3, 256);

  let snapshot = tracer.snapshot();

  println!("=== demo snapshot ===");
  for record in snapshot.records() {
    if let Some(stack) = &record.stack {
      println!(
        "top frame: {}",
        stack
          .frames()
          .first()
          .map_or("<unknown>", |frame| frame.filename.as_ref())
      );
    }
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
