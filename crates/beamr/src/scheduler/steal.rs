/// Work-stealing logic.
///
/// When a scheduler thread's queues are empty, it scans other
/// schedulers and steals half the processes from the fullest
/// queue. Load balances itself with no central coordinator.
/// A core is never idle while another core has a backlog.

pub(crate) fn _scaffold() {}
