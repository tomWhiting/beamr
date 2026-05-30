//! Priority run queues.
//!
//! Three priority levels: max, high, normal. The scheduler always
//! drains max before high, high before normal. Within a priority,
//! processes are served FIFO. Each scheduler thread owns its own
//! set of queues.

pub(crate) fn _scaffold() {}
