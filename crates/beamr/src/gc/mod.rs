/// Garbage collection — each process cleans its own room.
///
/// Per-process generational copying GC. Young generation (nursery)
/// collected frequently; old generation collected rarely. Heap sizes
/// start small (~2KB) so collection is microseconds. GC affects only
/// the process being collected — no stop-the-world, ever.
pub mod major;
pub mod minor;
