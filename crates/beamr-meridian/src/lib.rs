/// Beamr-Meridian — the join point (future crate).
///
/// Registers Yggdrasil, Meridian, and filesystem operations as NIFs
/// in the beamr VM. Wires the reduction-boundary hook to norn's
/// conventions engine. Depends on both beamr and Meridian, which is
/// why it's its own crate — the join shouldn't contaminate either side.

pub(crate) fn _scaffold() {}
