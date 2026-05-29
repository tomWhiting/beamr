/// Call stack frames.
///
/// The stack holds return addresses and Y-register slots.
/// `allocate` pushes a frame with N Y-register slots; `deallocate`
/// pops it. Tail calls (`call_last`, `call_ext_last`) deallocate
/// before jumping, preventing stack growth in recursive functions.

pub(crate) fn _scaffold() {}
