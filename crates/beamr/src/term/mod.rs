/// Term representation — what all data is made of.
///
/// A term is a single 64-bit machine word with low-bit tagging.
/// Immediates (small integers, atoms, pids, nil) fit entirely in
/// the word. Boxed values (tuples, lists, binaries, floats, big
/// integers, closures, maps, references) are tagged pointers into
/// the process-local heap.
pub mod binary;
pub mod boxed;
pub mod compare;
