/// Process — the unit of life and isolation.
///
/// Each process owns its heap, stack, mailbox, reduction counter,
/// link/monitor sets, and status. Processes share no memory.
/// Spawning costs microseconds. A process that crashes takes only
/// itself down — the rest of the system is unaffected.
pub mod heap;
pub mod registry;
pub mod stack;
