# ADR-011: Mailbox Uses Lock-Free MPSC Queue

**Status:** Accepted
**Date:** 2026-05-29

## Context

Every BEAM process has a mailbox. Messages arrive from any scheduler
thread (multiple producers) and are consumed by the owning process on
its scheduler thread (single consumer). The mailbox is the hottest
inter-thread communication path in the VM.

Two approaches were considered:

- **Mutex-protected VecDeque:** Simple to implement. Under contention
  from multiple senders, the mutex serialises all sends to a given
  process, creating a bottleneck.

- **Lock-free MPSC queue (e.g., crossbeam SegQueue):** Multiple threads
  can enqueue concurrently without blocking each other. Well-trodden
  in the Rust ecosystem with battle-tested implementations.

## Decision

Mailboxes use a lock-free MPSC queue. The primary candidate is
crossbeam's `SegQueue` or a similar lock-free structure from the
crossbeam family.

## Consequences

**Positive:**
- No serialisation bottleneck under concurrent sends. Multiple scheduler
  threads sending to the same popular process (e.g., a gen_server) do
  not contend on a lock.
- Lock-free queues are well-trodden in Rust. Crossbeam is mature,
  widely used, and thoroughly tested.
- Matches the MPSC access pattern exactly: many senders, one receiver.

**Negative:**
- Slightly more complex than a Mutex-protected queue. Debugging lock-
  free data structures requires understanding memory ordering guarantees.
- Memory reclamation in lock-free structures can be subtle. Crossbeam
  handles this via epoch-based reclamation, but it is additional
  machinery compared to a simple allocator.
- Cache behaviour under low contention may be slightly worse than a
  simple mutex (more atomic operations per enqueue). The win materialises
  under concurrent sends, which is the case we care about.
