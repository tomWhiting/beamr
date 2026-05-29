/// Bidirectional link management.
///
/// A link bonds two processes: if either dies, the other receives
/// an exit signal. By default the signal is fatal — the linked
/// process dies too. If the linked process traps exits, the signal
/// arrives as a message instead. Links are symmetric: A linking to
/// B is the same as B linking to A.

pub(crate) fn _scaffold() {}
