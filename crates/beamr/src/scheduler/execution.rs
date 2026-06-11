//! Scheduler execution loop, wake/resume, and process lifecycle handling.

use std::sync::Arc;
use std::sync::atomic::Ordering;
#[cfg(feature = "telemetry")]
use std::time::Instant;

use crossbeam_queue::SegQueue;

use crate::error::ExecError;
use crate::ets::copy::OwnedTerm;
use crate::process::ExitReason;
use crate::scheduler::dirty::DirtyResult;
use crate::term::Term;

use super::{
    PriorityStealers, RunQueue, Scheduler, SharedState, SpawnRequest, lock_or_recover,
    spawning::materialize_spawn_request, steal, timer_integration,
};

impl Scheduler {
    /// Return a callback suitable for mailbox senders to wake `pid`.
    pub fn wake_notifier(&self, pid: u64) -> impl Fn() + Send + Sync + 'static {
        let shared = Arc::clone(&self.shared);
        move || wake_process(&shared, pid)
    }

    /// Wake a process that is in the Waiting state after message arrival.
    pub fn wake_process(&self, pid: u64) {
        wake_process(&self.shared, pid);
    }

    /// Resume a suspended process, returning true if the process was found in
    /// the wait set and re-enqueued.
    pub fn resume_process(&self, pid: u64) -> bool {
        timer_integration::resume_suspended(&self.shared, pid)
    }

    /// Shut down all worker threads after their current time slice.
    pub fn shutdown(&self) {
        if let Some(bridge) = lock_or_recover(&self.shared.io_bridge).take() {
            bridge.shutdown();
        }
        if let Some(ring) = &self.shared.io_ring {
            ring.shutdown();
        }
        self.shared.dirty_cpu.shutdown();
        self.shared.dirty_io.shutdown();
        self.shared.file_io_ring.shutdown();
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake_condvar.notify_all();
        let mut threads = lock_or_recover(&self.threads);
        for handle in threads.drain(..) {
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
        }
    }

    /// Block until the given process exits, returning its exit reason and
    /// the value in x(0) at the time of exit.
    ///
    /// The result is an owning deep copy made at the exit boundary: it stays
    /// valid after the process heap is freed, for as long as the caller holds
    /// it.
    pub fn run_until_exit(&self, pid: u64) -> (ExitReason, OwnedTerm) {
        loop {
            if let Some(entry) = self.shared.exit_tombstones.get(&pid) {
                let reason = *entry;
                let result = self
                    .shared
                    .exit_results
                    .remove(&pid)
                    .map(|(_, term)| term)
                    .unwrap_or_else(|| OwnedTerm::immediate(Term::NIL));
                return (reason, result);
            }
            let guard = lock_or_recover(&self.shared.wait_set);
            let timeout = std::time::Duration::from_millis(10);
            let _ = self.shared.wake_condvar.wait_timeout(guard, timeout);
        }
    }

    /// Retrieve the execution error that caused a process to exit, if any.
    pub fn take_exit_error(&self, pid: u64) -> Option<ExecError> {
        self.shared.exit_errors.remove(&pid).map(|(_, e)| e)
    }

    /// Retrieve the BEAM exception that caused a process to exit, if any.
    ///
    /// The exception terms are owning deep copies that remain valid after the
    /// process heap is freed.
    pub fn take_exit_exception(&self, pid: u64) -> Option<super::OwnedException> {
        self.shared.exit_exceptions.remove(&pid).map(|(_, e)| e)
    }

    /// Wake a suspended process with a result term.
    pub fn wake_with_result(&self, pid: u64, result: Term) {
        self.shared.async_results.insert(pid, result);
        wake_process(&self.shared, pid);
    }

    /// Wake a suspended process with a dirty native completion result.
    pub fn wake_with_dirty_result(&self, pid: u64, result: DirtyResult) {
        self.shared.dirty_results.insert(pid, result);
        let _resumed = timer_integration::resume_suspended(&self.shared, pid);
    }

    /// Terminate a process externally, writing an exit tombstone so that
    /// `run_until_exit` returns with the given reason.
    pub fn terminate_process(&self, pid: u64, reason: ExitReason) {
        if self.shared.exit_tombstones.contains_key(&pid) {
            return;
        }
        cleanup_exited_process(&self.shared, pid, reason);
    }
}

pub(in crate::scheduler) fn scheduler_loop(
    shared: &Arc<SharedState>,
    queue: &RunQueue,
    my_index: usize,
    stealers: &[PriorityStealers],
    inject: &SegQueue<SpawnRequest>,
) {
    let mut last_victim = my_index;
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        drain_injected(shared, queue, inject);
        if my_index == 0 {
            if shared.replay_mode {
                timer_integration::tick_replay_timers(shared);
            } else {
                timer_integration::tick_timers(shared);
                drain_file_io_completions(shared);
            }
        }
        drain_woken(shared, queue, my_index);
        let pid = if shared.replay_mode {
            match next_replay_pid(shared, my_index) {
                Some(pid) => pid,
                None => {
                    park_thread(shared);
                    continue;
                }
            }
        } else {
            match queue.pop() {
                Some(pid) => pid,
                None => {
                    let (result, next_victim) =
                        steal::try_steal(queue, my_index, stealers, last_victim);
                    last_victim = next_victim;
                    match result {
                        steal::StealResult::Stolen { .. } => match queue.pop() {
                            Some(pid) => pid,
                            None => {
                                park_thread(shared);
                                continue;
                            }
                        },
                        steal::StealResult::Empty => {
                            park_thread(shared);
                            continue;
                        }
                    }
                }
            }
        };
        #[cfg(feature = "telemetry")]
        let executing_started = Instant::now();
        run_process(shared, queue, pid, my_index);
        #[cfg(feature = "telemetry")]
        shared.record_scheduler_executing(executing_started.elapsed());
    }
}

fn drain_injected(shared: &SharedState, queue: &RunQueue, inject: &SegQueue<SpawnRequest>) {
    while let Some(request) = inject.pop() {
        let pid = materialize_spawn_request(shared, request);
        if let Some(priority) = priority_for_pid(shared, pid) {
            queue.push_with_priority(pid, priority);
        }
    }
}

mod core;
pub(in crate::scheduler) use core::cleanup_exited_process;
use core::run_process;
use std::net::SocketAddr;

use super::process_slot::UdpActiveMessage;
use super::{ProcessSlot, ScheduledProcess};
use crate::atom::AtomTable;
use crate::io::IoResult;
use crate::io::resource::{FD_RESOURCE_WORDS, FdInner, FdMode, write_fd_resource};
use crate::process::Process;
use crate::term::boxed::write_tuple;
use crate::term::shared_binary::{alloc_binary, alloc_binary_word_count};
#[cfg(test)]
pub(in crate::scheduler) use core::{
    SliceOutcome, cleanup_if_tombstoned_after_store, execute_slice, store_runnable_process,
    take_runnable_process,
};
pub(in crate::scheduler) fn wake_process(shared: &SharedState, pid: u64) {
    // A process parked for an in-flight dirty call must stay parked: waking
    // it schedules a slice that re-executes the dirty call instruction. The
    // delivery that prompted this wake is already queued; the dirty
    // completion bridge resumes the process and the merged mailbox is
    // observed then. Once the result is published the wake is safe (the
    // resumed slice applies it) even if the in-flight mark is still set.
    if shared.dirty_in_flight.contains(&pid) && !shared.dirty_results.contains_key(&pid) {
        return;
    }
    // The receive timer is deliberately NOT cancelled here. BEAM keeps the
    // receive-after timer armed across message wakeups: if the message does
    // not match, the process re-parks and the original deadline must still
    // fire. The timer is dropped when the receive completes (the
    // remove_message/timeout opcodes clear the ref, and the eventual stale
    // fire is discarded by the id check in `apply_expired_receive_timer`).
    let mut wait_set = lock_or_recover(&shared.wait_set);
    if let Some(scheduler_index) = wait_set.waiting.remove(&pid) {
        wait_set.woken.push((pid, scheduler_index));
        shared.wake_condvar.notify_all();
    }
}

fn drain_file_io_completions(shared: &SharedState) {
    for completion in shared
        .file_io_ring
        .poll_completions(std::time::Duration::from_millis(0))
    {
        let op_id = completion.op_id;
        if let Some((_, (pid, continuation))) = shared.file_io_pending.remove(&op_id) {
            if let crate::native::FileIoContinuation::UdpActiveRecv { fd } = continuation {
                handle_udp_active_completion(shared, fd, completion);
            } else if let crate::native::FileIoContinuation::TcpActiveRecv { fd } = continuation {
                handle_tcp_active_completion(shared, fd, completion);
            } else {
                shared.file_io_results.insert(
                    pid,
                    crate::native::FileIoCompletion {
                        op_id,
                        continuation,
                        completion,
                    },
                );
                wake_process(shared, pid);
            }
        } else if shared.file_io_canceled.remove(&op_id).is_none() {
            shared.file_io_orphans.insert(op_id, completion);
        }
    }
}

fn handle_udp_active_completion(
    shared: &SharedState,
    fd: std::sync::Arc<FdInner>,
    completion: crate::io::IoCompletion,
) {
    if fd.state() != crate::io::resource::FdState::Open {
        return;
    }
    let mode = fd.mode();
    if mode == FdMode::Passive {
        return;
    }
    match completion.result {
        Ok(IoResult::DatagramReceived { bytes, data, addr }) => {
            deliver_udp_active_datagram(shared, &fd, bytes, &data, addr);
            match mode {
                FdMode::Active => {
                    let op_id = shared.file_io_ring.submit(crate::io::IoOp::RecvMsg {
                        fd: fd.fd(),
                        buf_len: 65_535,
                    });
                    shared.file_io_pending.insert(
                        op_id,
                        (
                            fd.controlling_process(),
                            crate::native::FileIoContinuation::UdpActiveRecv { fd },
                        ),
                    );
                }
                FdMode::ActiveOnce => fd.set_mode(FdMode::Passive),
                FdMode::Passive => {}
            }
        }
        Ok(_) | Err(_) => fd.set_mode(FdMode::Passive),
    }
}

fn deliver_udp_active_datagram(
    shared: &SharedState,
    fd: &std::sync::Arc<FdInner>,
    bytes: usize,
    data: &[u8],
    addr: SocketAddr,
) -> Option<Term> {
    let target = fd.controlling_process();
    let entry = shared.process_bodies.get(&target)?;
    let mut slot = super::lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => {
            let message = build_udp_active_message_for_process(
                &shared.atom_table,
                process,
                fd,
                data.get(..bytes)?,
                addr,
            )?;
            process.mailbox_mut().push_owned(message);
        }
        ProcessSlot::Executing(metadata) => {
            metadata.pending_udp_messages.push(UdpActiveMessage {
                fd: std::sync::Arc::clone(fd),
                bytes: data.get(..bytes)?.to_vec(),
                addr,
            });
        }
        ProcessSlot::Absent => return None,
    }
    drop(slot);
    wake_process(shared, target);
    Some(Term::atom(crate::atom::Atom::OK))
}

fn handle_tcp_active_completion(
    shared: &SharedState,
    fd: std::sync::Arc<FdInner>,
    completion: crate::io::IoCompletion,
) {
    if fd.state() != crate::io::resource::FdState::Open {
        return;
    }
    let mode = fd.mode();
    if mode == FdMode::Passive {
        return;
    }
    match completion.result {
        Ok(IoResult::BytesRead(0, _)) => {
            // EOF / peer closed — deliver {tcp_closed, Socket} and stop.
            deliver_tcp_closed(shared, &fd);
        }
        Ok(IoResult::BytesRead(bytes_read, data)) => {
            let chunk = data.get(..bytes_read).unwrap_or(&data);
            deliver_tcp_active_data(shared, &fd, chunk);
            match mode {
                FdMode::Active => {
                    let op_id = shared.file_io_ring.submit(crate::io::IoOp::Read {
                        fd: fd.fd(),
                        buf_len: 64 * 1024,
                        offset: u64::MAX,
                    });
                    shared.file_io_pending.insert(
                        op_id,
                        (
                            fd.controlling_process(),
                            crate::native::FileIoContinuation::TcpActiveRecv { fd },
                        ),
                    );
                }
                FdMode::ActiveOnce => fd.set_mode(FdMode::Passive),
                FdMode::Passive => {}
            }
        }
        Ok(_) | Err(_) => {
            deliver_tcp_closed(shared, &fd);
        }
    }
}

fn deliver_tcp_active_data(
    shared: &SharedState,
    fd: &std::sync::Arc<FdInner>,
    data: &[u8],
) -> Option<Term> {
    let target = fd.controlling_process();
    let entry = shared.process_bodies.get(&target)?;
    let mut slot = super::lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => {
            let message =
                build_tcp_active_message_for_process(&shared.atom_table, process, fd, data)?;
            process.mailbox_mut().push_owned(message);
        }
        ProcessSlot::Executing(metadata) => {
            metadata
                .pending_tcp_messages
                .push(super::process_slot::TcpActiveMessage {
                    fd: std::sync::Arc::clone(fd),
                    bytes: data.to_vec(),
                });
        }
        ProcessSlot::Absent => return None,
    }
    drop(slot);
    wake_process(shared, target);
    Some(Term::atom(crate::atom::Atom::OK))
}

fn deliver_tcp_closed(shared: &SharedState, fd: &std::sync::Arc<FdInner>) -> Option<Term> {
    let target = fd.controlling_process();
    let entry = shared.process_bodies.get(&target)?;
    let mut slot = super::lock_or_recover(&entry);
    match &mut *slot {
        ProcessSlot::Present(ScheduledProcess(process)) => {
            let message = build_tcp_closed_message_for_process(&shared.atom_table, process, fd)?;
            process.mailbox_mut().push_owned(message);
        }
        ProcessSlot::Executing(metadata) => {
            // Queue an empty-data TCP message; the process will see {tcp_closed, Socket}
            // when the scheduler drains it. We use an empty bytes vec as the signal.
            metadata
                .pending_tcp_messages
                .push(super::process_slot::TcpActiveMessage {
                    fd: std::sync::Arc::clone(fd),
                    bytes: Vec::new(),
                });
        }
        ProcessSlot::Absent => return None,
    }
    drop(slot);
    wake_process(shared, target);
    Some(Term::atom(crate::atom::Atom::OK))
}

pub(in crate::scheduler) fn build_tcp_active_message_for_process(
    atom_table: &AtomTable,
    process: &mut Process,
    fd: &std::sync::Arc<FdInner>,
    data: &[u8],
) -> Option<Term> {
    if data.is_empty() {
        return build_tcp_closed_message_for_process(atom_table, process, fd);
    }
    let binary_words = alloc_binary_word_count(data.len());
    // {tcp, Socket, Data} = 1 header + 3 elements = 4 words, plus fd resource + binary
    let needed_words = FD_RESOURCE_WORDS + binary_words + 1 + 3;
    if crate::gc::ensure_space(process, needed_words, 0).is_err() {
        return None;
    }
    let socket = {
        let heap = process.heap_mut().alloc_slice(FD_RESOURCE_WORDS).ok()?;
        write_fd_resource(heap, std::sync::Arc::clone(fd))?
    };
    let binary = {
        let heap = process.heap_mut().alloc_slice(binary_words).ok()?;
        alloc_binary(heap, data)?
    };
    let tcp = Term::atom(atom_table.intern("tcp"));
    let message_terms = [tcp, socket, binary];
    let heap = process
        .heap_mut()
        .alloc_slice(1 + message_terms.len())
        .ok()?;
    write_tuple(heap, &message_terms)
}

fn build_tcp_closed_message_for_process(
    atom_table: &AtomTable,
    process: &mut Process,
    fd: &std::sync::Arc<FdInner>,
) -> Option<Term> {
    // {tcp_closed, Socket} = 1 header + 2 elements = 3 words, plus fd resource
    let needed_words = FD_RESOURCE_WORDS + 1 + 2;
    if crate::gc::ensure_space(process, needed_words, 0).is_err() {
        return None;
    }
    let socket = {
        let heap = process.heap_mut().alloc_slice(FD_RESOURCE_WORDS).ok()?;
        write_fd_resource(heap, std::sync::Arc::clone(fd))?
    };
    let tcp_closed = Term::atom(atom_table.intern("tcp_closed"));
    let message_terms = [tcp_closed, socket];
    let heap = process
        .heap_mut()
        .alloc_slice(1 + message_terms.len())
        .ok()?;
    write_tuple(heap, &message_terms)
}

pub(in crate::scheduler) fn build_udp_active_message_for_process(
    atom_table: &AtomTable,
    process: &mut Process,
    fd: &std::sync::Arc<FdInner>,
    datagram: &[u8],
    addr: SocketAddr,
) -> Option<Term> {
    let SocketAddr::V4(v4) = addr else {
        return None;
    };
    let binary_words = alloc_binary_word_count(datagram.len());
    let needed_words = FD_RESOURCE_WORDS + binary_words + 1 + 4 + 1 + 5;
    if crate::gc::ensure_space(process, needed_words, 0).is_err() {
        return None;
    }
    let socket = {
        let heap = process.heap_mut().alloc_slice(FD_RESOURCE_WORDS).ok()?;
        write_fd_resource(heap, std::sync::Arc::clone(fd))?
    };
    let ip = {
        let octets = v4.ip().octets();
        let terms = [
            Term::try_small_int(i64::from(octets[0]))?,
            Term::try_small_int(i64::from(octets[1]))?,
            Term::try_small_int(i64::from(octets[2]))?,
            Term::try_small_int(i64::from(octets[3]))?,
        ];
        let heap = process.heap_mut().alloc_slice(1 + terms.len()).ok()?;
        write_tuple(heap, &terms)?
    };
    let binary = {
        let heap = process.heap_mut().alloc_slice(binary_words).ok()?;
        alloc_binary(heap, datagram)?
    };
    let udp = Term::atom(atom_table.intern("udp"));
    let port = Term::try_small_int(i64::from(v4.port()))?;
    let message_terms = [udp, socket, ip, port, binary];
    let heap = process
        .heap_mut()
        .alloc_slice(1 + message_terms.len())
        .ok()?;
    write_tuple(heap, &message_terms)
}

fn next_replay_pid(shared: &SharedState, my_index: usize) -> Option<u64> {
    let replay_driver = shared.replay_driver.as_ref()?;
    let guard = match replay_driver.lock() {
        Ok(guard) => guard,
        Err(error) => error.into_inner(),
    };
    let event = guard.peek_schedule()?;
    if event.scheduler_index != my_index || shared.process_table.get(event.pid).is_none() {
        return None;
    }
    Some(event.pid)
}

fn drain_woken(shared: &SharedState, queue: &RunQueue, my_index: usize) {
    let woken = {
        let mut wait_set = lock_or_recover(&shared.wait_set);
        let mut mine = Vec::new();
        wait_set.woken.retain(|(pid, sched_idx)| {
            if *sched_idx == my_index {
                mine.push(*pid);
                false
            } else {
                true
            }
        });
        mine
    };
    for pid in woken {
        if shared.process_table.get(pid).is_some() {
            queue.push_with_priority(pid, priority_for_pid(shared, pid).unwrap_or_default());
        }
    }
}

pub(in crate::scheduler) fn priority_for_pid(
    shared: &SharedState,
    pid: u64,
) -> Option<crate::process::Priority> {
    let entry = shared.process_bodies.get(&pid)?;
    match &*lock_or_recover(&entry) {
        super::ProcessSlot::Present(scheduled) => Some(scheduled.0.priority()),
        super::ProcessSlot::Executing(metadata) => Some(metadata.priority),
        super::ProcessSlot::Absent => None,
    }
}

fn park_thread(shared: &SharedState) {
    #[cfg(test)]
    shared.idle_parks.fetch_add(1, Ordering::Relaxed);
    if shared.shutdown.load(Ordering::Acquire) {
        return;
    }
    let guard = lock_or_recover(&shared.wait_set);
    let timeout = std::time::Duration::from_millis(5);
    #[cfg(feature = "telemetry")]
    let idle_started = Instant::now();
    match shared.wake_condvar.wait_timeout(guard, timeout) {
        Ok(_) => {}
        Err(error) => {
            let _recovered = error.into_inner();
        }
    }
    #[cfg(feature = "telemetry")]
    shared.record_scheduler_idle(idle_started.elapsed());
}
