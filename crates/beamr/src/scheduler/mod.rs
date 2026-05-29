/// Scheduler — fairness across every core.
///
/// N OS threads, each with a run queue of ready processes. Work
/// stealing keeps all cores busy. No async runtime in the hot
/// path (per D3) — plain OS threads plus lock-free queues.
pub mod dirty;
pub mod run_queue;
pub mod steal;
