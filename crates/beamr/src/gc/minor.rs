/// Nursery collection — young generation to old generation copy.
///
/// Walks the root set (stack, registers, mailbox save queue),
/// copies reachable young-generation terms to the old generation,
/// and updates all pointers. The nursery is then reclaimed wholesale.
/// This is the common case — most data dies young.

pub(crate) fn _scaffold() {}
