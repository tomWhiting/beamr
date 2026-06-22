# beamr — Local Send Facility: Implementation Brief

> Companion to `MESSAGING-FIX-SCOPE.md`. This is the *precise, ratified* design to implement,
> validated against beamr HEAD (b4f8b13) on 2026-06-22 with two read-only investigations.
> Goal: make cross-process local `send` actually deliver, so the actor-per-shard (haematite) and
> actor-per-conversation (liminal) thesis stops being faked with synchronous structs.

## The bug (one sentence)
`messaging::send` only delivers to a receiver passed by reference (`messaging.rs:36`,
`if let Some(receiver) = receiver.filter(|r| r.pid() == target_pid)`), but the scheduler always
passes `receiver: None` (`opcodes/mod.rs:68,93`), so every real cross-process `!` silently drops
at `messaging.rs:86` (`process.set_x_reg(0, message); Continue`).

## The fix (one sentence)
Add a `LocalSendFacility` (mirroring the existing `DistributionSendFacility`), implemented in the
scheduler over `Arc<SharedState>`, that delivers to the target via `shared.process_bodies` using the
**exact** lock-slot/Present-Executing-Absent/wake template the I/O paths already use; route
`messaging::send`'s local branch to it when no in-hand `receiver` is supplied.

---

## Key facts established by investigation (do not re-derive; cite if needed)

1. **I/O delivery template** = `deliver_udp_active_datagram` (execution.rs:398-431) and
   `deliver_tcp_active_data` (execution.rs:478-505). Pattern:
   `let entry = shared.process_bodies.get(&target)?;` → `let mut slot = super::lock_or_recover(&entry);`
   → match `&mut *slot`:
   - `ProcessSlot::Present(ScheduledProcess(process))` → build/copy message into `process` heap →
     `process.mailbox_mut().push_owned(message);`
   - `ProcessSlot::Executing(metadata)` → push a heap-independent payload onto a `pending_*` vec.
   - `ProcessSlot::Absent` → `return None` (silent drop — correct BEAM semantics for dead pid).
   → `drop(slot);` → `wake_process(shared, target);`
   **Message is pushed BEFORE wake** — this is the park-gap-race invariant. Do not reorder.

2. **Deferred (Executing) payloads cross heaps via ETF.** The receiver in `Executing` is running on
   another scheduler thread; its heap is unavailable. The existing analogue is
   `pending_distribution_payloads: Vec<Vec<u8>>` (process_slot.rs:59): pushed as ETF bytes while
   Executing, decoded onto the receiver heap at store-back (`store_runnable_process`,
   core.rs:377-386, via `crate::etf::decode::decode_term` with a `ProcessContext`). Mirror this.

3. **Replay mode is single-threaded** (`mod.rs:550-554`: `thread_count = if replay_enabled {1}`).
   Therefore during replay a cross-process (`target != sender`) send ALWAYS finds the receiver
   `Present` (no other pid can be `Executing` on a single thread) or `Absent`. **The
   `Executing`+replay combination is impossible.** ⇒ the replay/clock dance only ever runs in the
   `Present`/self-send cases. The `Executing` deferred path is live-mode-only and needs NO clock or
   replay work (matching the I/O paths, which the investigation confirmed do no replay/clock work).

4. **Self-send:** during P's slice, P's own slot is `ProcessSlot::Executing` (its body is taken out
   by `take_runnable_process`, core.rs:306). So a facility locking `process_bodies[P]` will NOT find
   P's live body. Self-send (`target_pid == process.pid()`) MUST deliver to the in-hand
   `&mut process`, never via the slot.

5. **Logical clock + replay semantics (Present case only):** the current opcode (messaging.rs:37-67)
   does, in order: snapshot both clocks; `sender_clock = process.tick_logical_clock()`;
   `receiver_clock = receiver.observe_message_clock(sender_clock)`; if `replay_driver` is `Some`,
   `guard.next_message_delivery(RecordedDeliveryKind::Message, Some(process.pid()), target_pid, message)`
   and assert recorded sender/receiver clocks match the live ones, else roll BOTH clocks back and
   return `ExecError::ReplayMismatch`. This must be preserved for delivery through the facility's
   `Present` branch.

6. **Existing tests that MUST keep passing** (all drive the in-hand `receiver: Some(&mut Process)`
   path, none exercises the scheduler): messaging.rs `send_delivers_to_matching_pid...` (~285),
   `replay_send_consumes_recorded_delivery...` (~302), `replay_send_mismatch_does_not_enqueue...`
   (~330), `send_to_missing_pid_is_silent_drop` (~360),
   `send_to_remote_pid_without_distribution_returns_noconnection` (~372),
   `dispatch_send_delivers_to_resolved_process...` (~387),
   `run_wait_suspends_and_send_wakes_waiting_receiver` (~517). **KEEP the in-hand `receiver` branch of
   `send` intact** so these pass unchanged.

---

## Change set (file by file)

### A. `crates/beamr/src/scheduler/process_slot.rs`
- Add field to `ProcessMetadata` (after `pending_distribution_payloads`, ~line 59):
  `pub(super) pending_local_messages: Vec<Vec<u8>>,`  // ETF payloads for local sends while Executing

### B. `crates/beamr/src/scheduler/execution/core.rs`
- In the `Executing` metadata constructor (~line 301, beside `pending_distribution_payloads: Vec::new(),`):
  `pending_local_messages: Vec::new(),`
- In `store_runnable_process` (~after the `pending_distribution_payloads` drain, lines 377-386), add a
  drain that decodes each ETF payload onto the receiver heap and enqueues — copy the
  distribution-payload block verbatim, swapping the vec name:
  ```rust
  for payload in metadata.pending_local_messages.drain(..) {
      let mut context = crate::native::ProcessContext::new();
      context.attach_process(&mut process, 0);
      let Ok(message) =
          crate::etf::decode::decode_term(&payload, &mut context, &shared.atom_table)
      else {
          continue;
      };
      process.mailbox_mut().push_owned(message);
  }
  ```

### C. Define the `LocalSendFacility` trait
- Mirror `SpawnFacility`/`DistributionSendFacility`. Put it in a small new module
  `crates/beamr/src/native/local_send.rs` (or alongside the other native facilities) and re-export so
  `interpreter/mod.rs` can name it. Shape:
  ```rust
  /// Delivers a local message term to a live target process body held by the scheduler.
  /// Returns Ok(()) whether or not the target exists (a dead/absent pid is a silent drop,
  /// matching BEAM semantics); Err only for a genuine replay-determinism mismatch.
  pub trait LocalSendFacility: Send + Sync {
      fn send_local(&self, ctx: LocalSendRequest<'_>) -> Result<(), LocalSendError>;
  }
  ```
  `LocalSendRequest` must carry what the facility needs to perform delivery AND the Present-case
  clock/replay work under the slot lock: `target_pid: u64`, `sender_pid: u64`, `message: Term`,
  `sender_clock: u64` (already ticked by the caller), and the `replay_driver:
  Option<&Arc<Mutex<ReplayDriver>>>`. `LocalSendError::ReplayMismatch(String)` maps to
  `ExecError::ReplayMismatch`. (Borrow vs owning is the implementer's call; keep it simple.)

  NOTE on Term across the trait boundary: `Term` is a tagged word that references the *sender's*
  heap. The facility receives it while the caller still holds the sender body alive (the call is
  synchronous, inside the sender's slice), so reading/encoding the term is sound. The facility must
  NOT store the raw Term past the call: Present → copy into receiver heap immediately; Executing →
  ETF-encode to bytes immediately.

### D. `crates/beamr/src/interpreter/mod.rs` (`NativeServices`, ~line 39)
- Add: `pub local_send: Option<Arc<dyn crate::native::local_send::LocalSendFacility>>,`
  (placed near `distribution_send`). `#[derive(Default)]` keeps it `None` by default — preserves all
  existing `NativeServices::default()` / `..default()` construction sites.

### E. `crates/beamr/src/interpreter/opcodes/messaging.rs` (`send`, lines 16-88)
- Extend the signature with `local_send: Option<&dyn crate::native::local_send::LocalSendFacility>`.
- Keep the remote branch (25-34) and the in-hand-`receiver` branch (36-85) UNCHANGED.
- After the in-hand branch, when `receiver` was `None` (or didn't match the target) AND the target is
  local, add routing:
  - **self-send** (`target_pid == process.pid()`): deliver to the in-hand `process` directly —
    tick/observe clocks on `process`, run the replay check if `replay_driver` is `Some` (single
    process, both clocks are `process`'s — mirror the existing block but with one process), copy the
    message into `process.heap_mut()` + enqueue via the mailbox sender, no wake needed (it's running).
  - **cross-process**: `if let Some(facility) = local_send { facility.send_local(LocalSendRequest{
    target_pid, sender_pid: process.pid(), message, sender_clock: process.tick_logical_clock(),
    replay_driver })?; }` On `Err(LocalSendError::ReplayMismatch(_))`, roll back the sender clock
    (`process.set_logical_clock(previous)`) and return `ExecError::ReplayMismatch`. If `local_send`
    is `None` (e.g. bare `run()` with no scheduler), fall through to the existing silent set-x0 path —
    preserves current non-scheduler behaviour.
  - Always end with `process.set_x_reg(0, message); Ok(Continue)` (BEAM: `!` returns the message).
- Update `opcodes/mod.rs` Send dispatch arm (~321) to pass `services.local_send.as_deref()` (and it
  already has access to `services` / the replay driver — thread them through consistently with how
  `distribution` and `replay_driver` are currently passed to `send`).

### F. `crates/beamr/src/scheduler/supervision_integration.rs` (`build_native_services`, ~538)
- Add a `SchedulerLocalSendFacility { shared: Arc<SharedState> }` beside
  `SchedulerDistributionSendFacility` (~629) implementing `LocalSendFacility::send_local`:
  - `if target_pid == sender_pid { /* unreachable: self-send handled in-hand by messaging::send */ }`
    — defensively deliver to slot anyway or debug_assert; document why it shouldn't happen.
  - `let Some(entry) = shared.process_bodies.get(&target_pid) else { return Ok(()) };` (absent = drop)
  - `let mut slot = lock_or_recover(&entry);`
  - `ProcessSlot::Present(ScheduledProcess(process))` →
    - clock + replay (this is the ONLY place replay can occur for cross-process, per fact #3):
      snapshot receiver clock; `let receiver_clock = process.observe_message_clock(sender_clock);`
      if `replay_driver` is `Some`, run `next_message_delivery(...)` + clock assertions exactly like
      messaging.rs:41-67; on mismatch restore the receiver clock and return
      `Err(LocalSendError::ReplayMismatch(..))` (caller rolls back the sender clock).
    - copy message into `process` heap + enqueue (mirror messaging.rs:68-79's
      `mailbox().sender().send(message, process.heap_mut())`, telemetry-gated like the original).
    - if `process.status() == Waiting` → `process.transition_to(Running)`.
  - `ProcessSlot::Executing(metadata)` → ETF-encode the message and push onto
    `metadata.pending_local_messages` (use the same encoder the distribution path uses to produce
    `pending_distribution_payloads`; find it near `encode_send_frame`/`crate::etf::encode`). No clock
    work (live-mode-only path).
  - `ProcessSlot::Absent` → drop.
  - `drop(slot);` then `wake_process(&self.shared, target_pid);` (after releasing the slot lock).
- Wire it into the returned `NativeServices`: `local_send: Some(Arc::new(SchedulerLocalSendFacility{
  shared: Arc::clone(shared) }))`.

### G. THE GATE TEST (new — this is the acceptance criterion; it does not exist today)
Add an end-to-end scheduler test (in `crates/beamr/tests/` or the scheduler test module) that:
1. Boots a real scheduler (live mode, default threads).
2. Process A `spawn`s process B; B runs `receive Msg -> <record/store Msg> end` (or a known
   receive-and-reply), A does `B ! Msg` **via the real `Send` opcode dispatched through the
   scheduler** (NOT `dispatch_with_receiver`, NOT a hand-built MailboxSender).
3. Asserts B actually receives the message (the bug = B times out).
4. Asserts the timeout path still works (a `receive ... after T -> timeout end` with no send fires
   `timeout`).
Prefer compiling a tiny real Gleam/OTP-style fixture OR hand-assembling the opcode sequence the same
way existing scheduler integration tests set up processes. This test MUST fail on current `main` and
pass after the fix.

---

## Invariants the review team will check (build it to survive these)
1. **Park-gap / wake ordering** — message pushed (Present mailbox OR Executing pending vec) strictly
   BEFORE `wake_process`; slot lock dropped before wake. Matches execution.rs:417/429, 490/503 and
   the park-register-then-recheck logic in core.rs:77-150.
2. **Replay determinism** — receiver-clock observation + `next_message_delivery` happen UNDER the slot
   lock in the `Present` branch; on mismatch BOTH clocks roll back (receiver in facility, sender in
   caller) and `ExecError::ReplayMismatch` propagates. Executing path does no clock/replay work and
   that's provably safe (fact #3).
3. **Self-send heap isolation** — `target == sender` delivers to the in-hand `&mut process`; the
   facility is never asked to lock the sender's own (Executing) slot for delivery.
4. **Heap isolation** — Present copies the term into the receiver heap; Executing ETF-round-trips it;
   no raw cross-heap Term is ever enqueued or stored.
5. **No regressions** — all 7 existing send tests pass unchanged; the in-hand `receiver` branch is
   untouched.

## Definition of done (ratification bar — ALL required)
- `cargo fmt --check` clean.
- `cargo check --all-features` clean.
- `cargo test -p beamr --all-features` green (incl. the new gate test; incl. the 7 existing send tests).
- `cargo clippy --all-targets --all-features -- -D warnings` clean (NO new `#[allow]`).
- No `unwrap()`/`expect()` in non-test code; no touched file > 500 lines (split if needed).
- The gate test demonstrably fails on `main` and passes on the branch.
- Independent review team (per-invariant) returns clean; Claude reads the full diff and ratifies.

## Out of scope for THIS change (follow-ups)
- recv_marker fixture regeneration + un-ignore (2 tests) — separate small PR after this lands.
- Migrating CORE-007 (haematite) + liminal channel/conversation actors to real beamr processes —
  the downstream payoff, done after this ships in a release.
- ref-keyed multi-marker fidelity (optional, only if profiling demands).
