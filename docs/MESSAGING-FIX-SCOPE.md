# beamr — Messaging Fix Scope (cross-process send + recv_marker)

> **Status: scoping doc, 2026-06-22.** Produced by two read-only investigations against beamr HEAD.
> Purpose: a concrete plan to make beamr's actor messaging real, so the actor-per-shard (haematite) and
> actor-per-conversation (liminal) thesis stops being faked with synchronous structs.

## TL;DR

The "beamr messaging is broken in two deep places" picture has collapsed to **one medium, well-understood
fix plus some test-fixture cleanup**:

1. **Cross-process `send` silently drops messages between live processes** — REAL, confirmed reproducing.
   But it's an **addressing/routing gap**, not a scheduler or mailbox bug: all the delivery infrastructure
   already exists and is proven by the I/O paths. Fix ≈ **120–200 LOC, MEDIUM**, clear approach.
2. **`recv_marker` opcodes** — **NOT stubs anymore.** Implemented + hardened since June (commit `54686dc`).
   Correctness is fine. Only leftover: two `#[ignore]`d end-to-end tests with broken fixtures. ≈ **small**.

So the actor thesis is gated by **one routing fix**, not a messaging-subsystem rewrite.

---

## Bug 1 — cross-process `send` drops messages (THE blocker)

### Confirmed reproduction
Driving the real `Send` opcode through the real interpreter (`execute_slice`) with a live, registered
receiver and asserting on its mailbox: `receiver mailbox message_count = 0` (expected 1). The message is
silently dropped → a `receive` on the target times out. Matches the June symptom exactly.

Why it survived: every existing "delivery" test bypasses the `Send` opcode (hand-builds a `MailboxSender`
or pushes directly into `process_bodies`). Nothing exercises two live processes sending via `!`.

### Root cause — `crates/beamr/src/interpreter/opcodes/messaging.rs:36`
The local-send branch only delivers when the caller passes the target process by reference:
```
if let Some(receiver) = receiver.filter(|r| r.pid() == target_pid) { ...enqueue + wake (lines 75–84)... }
```
But the runtime **always passes `receiver: None`**: scheduler → `run_with_native_services`
(execution/core.rs:597) → `dispatch_with_services` (interpreter/mod.rs:266) hard-codes `receiver: None`
(opcodes/mod.rs:94); the Send arm forwards that `None` (opcodes/mod.rs:319-326). So the `if let` is always
skipped and `send` falls through to `process.set_x_reg(0, message); Continue` (messaging.rs:86) — the same
silent-drop path as "send to a dead pid". The `receiver: Option<&mut Process>` param is **dead at runtime**
— only non-`None` in unit tests calling `dispatch_with_receiver`. The doc comment (opcodes/mod.rs:103-108)
admits it's a placeholder "until process ownership/run-queue infrastructure provides a richer context."

### The infrastructure to route to ALREADY EXISTS (proven by I/O)
- `SharedState.process_bodies: DashMap<u64, Mutex<ProcessSlot>>` holds live bodies; `ProcessSlot::
  Present/Executing/Absent` is the correct lock-and-deliver pattern.
- TCP/UDP/file I/O **already delivers to a process exactly the needed way**: lock the target slot →
  `process.mailbox_mut().push_owned(msg)` (or stash in `metadata.pending_io_messages` when `Executing`) →
  `wake_process(shared, target)` (execution.rs:405-431, 478-505). `wake_process` + run-queue +
  `MailboxSender::with_wake_notifier` are all present and tested.

The Send opcode simply isn't wired to this. (`process/registry.rs` `ProcessTable` stores only a bare PID,
no mailbox handle, so routing must go through `process_bodies` in the scheduler crate, like the I/O paths.)

### Fix approach — add a `LocalSendFacility` (mirror the existing facility pattern)
`NativeServices` already carries `distribution_send`, `spawn_facility`, links/supervision facilities — but
no local-send. Add one:
1. `trait LocalSendFacility { fn send_local(&self, target_pid: u64, message: Term) -> Result<(), SendError>; }`
   + `local_send: Option<Arc<dyn LocalSendFacility>>` on `NativeServices` (interpreter/mod.rs:39).
2. Implement in the scheduler (alongside `build_native_services`, supervision_integration.rs:538) over
   `Arc<SharedState>`, reusing the I/O delivery logic: lock `process_bodies[target]`; `Present` → deep-copy
   into `process.heap_mut()` + enqueue (`MailboxSender::send`/`mailbox.push_owned`); `Executing` → push to a
   pending vec merged on store-back (merge already happens at execution/core.rs:375-420); `Absent`/missing →
   silent drop (correct BEAM semantics); then `wake_process(shared, target_pid)`.
3. In `messaging::send`, replace the `receiver`-based local branch (messaging.rs:36-85) with a
   `services.local_send` call for the `target.is_local()` case. Retire `receiver`/`dispatch_with_receiver`
   (or keep for the existing unit tests).

No new synchronization primitives — heap-copy + wake-notifier + run-queue requeue all exist.

### Size / risk — MEDIUM (~120–200 LOC)
trait + field (~20) · scheduler impl reusing I/O helpers (~70–100) · messaging.rs rewire (~30) · a real
end-to-end `!`-via-opcode test (the repro belongs in the suite). Tricky parts:
1. **`Executing`-slot pending-message merge + wake ordering** — must match the existing I/O merge and the
   scheduler's careful park-gap ordering (execution.rs:283-309, core.rs:87-92) so a send racing the
   receiver's park isn't lost. The I/O paths are the exact template.
2. **Replay determinism** — the logical-clock observation (messaging.rs:37-40) currently reads the
   receiver's clock directly; through a facility the receiver isn't borrowed, so move that observation into
   the facility under the slot lock. (Trickiest bit.)
3. **Self-send** (target == sender) — the sender's body is taken out of `process_bodies` during its slice
   (`take_process`), so self-send must fall back to the in-hand `process`, not lock its own absent slot.

---

## Bug 2 — recv_marker opcodes: ALREADY IMPLEMENTED (premise was stale)

Opcodes 173–176 (`recv_marker_reserve/bind/clear/use`) were landed as stubs in June (`188c704`) then
**implemented + hardened** (`54686dc`). Current state:
- Decode: loader/decode/code.rs:392-404 · Dispatch: interpreter/opcodes/mod.rs:336-341 · Handlers:
  interpreter/opcodes/recv.rs:17-80 (real, no `unimplemented!()`) · Mailbox state: mailbox/mod.rs:54
  (`recv_marker` slot + `save_pointer`) with bind/use/clear/reserve methods. JIT deopts to interpreter safely.
- **Correctness today: fine.** Selective receive works via the existing `loop_rec`/`loop_rec_end`/
  `remove_message`/`wait` save-pointer cycle (messaging.rs:97-189), independent of markers (markers are a
  pure optimization in real BEAM). recv_marker cannot by itself diverge supervision.

### Only leftovers (both minor, neither blocks actors)
1. **Two end-to-end tests are `#[ignore]`d** (tests/recv_marker.rs:96,114) because their AI-generated `.beam`
   fixtures are broken ("labels 7,9 missing") — so B-070's R4 (a real OTP-24 receive runs end-to-end) was
   never validated. Fix: drop in real OTP-24-compiled fixtures (`receive Msg -> Msg after 0 -> timeout end`),
   un-ignore. **Small (~10 LoC + 2 fixtures).** Best done AFTER the send fix so the tests are meaningful.
2. `bind` uses a single-marker model (operand read as label, single `recv_marker` slot) — explicitly within
   B-070's boundaries ("SHALL NOT implement multiple independent recv markers"). True ref-keyed multi-marker
   table is optional fidelity, **medium (~80–120 LoC)**, only if gen_server call latency ever shows it's needed.

---

## Recommended plan / order

1. **Fix cross-process `send`** (the `LocalSendFacility`) — this is the whole unlock. Land the repro test
   with it.
2. **Regenerate the 2 recv_marker fixtures + un-ignore** — now end-to-end `!` + selective receive can be
   validated together (a gen_server-style ref receive between two live processes).
3. **Then** revisit the faked actor layers: CORE-007 shard actor (haematite) and liminal channel/conversation
   actors → migrate from synchronous structs to real supervised beamr processes.
4. (Optional, later) ref-keyed multi-marker fidelity if profiling demands it.

## Execution plan (agreed with Tom, 2026-06-22 — resume after context compaction)

**Approach: implement the `send` fix DIRECTLY (hands-on), NOT via the brief-dispatch loop.** Tom wants to stay
closer to the coal face on this one (it's delicate VM concurrency), and direct work is faster than a loop
dispatch (Norn dev agents carry heavy by-design diagnostics). Use Claude + **subagents running a rigorous
review system** — i.e. drive the implementation, then fan out review subagents on the concurrency invariants
(park-gap ordering, replay-clock-under-slot-lock, self-send) and the repro/test, exactly like the loop's
review gate but hands-on.

Resume checklist (post-compaction):
1. Re-read this doc + `crates/beamr/src/interpreter/opcodes/messaging.rs:36`, `opcodes/mod.rs:94/319`,
   `scheduler ... execution.rs:405-431/478-505` (the I/O delivery template), `supervision_integration.rs:538`
   (`build_native_services`), `interpreter/mod.rs:39` (`NativeServices`).
2. Implement `LocalSendFacility` per "Fix approach" above; wire `messaging::send` local branch to it.
3. Write the gate test FIRST or alongside: spawn→`!`-via-Send-opcode→receive through the real scheduler,
   assert delivery + timeout path. (This test does not exist today; it is the acceptance gate.)
4. Rigorous subagent review of: the Executing-slot pending merge + wake ordering vs the I/O template; the
   replay-clock observation moved under the slot lock; the self-send fallback. Re-gate (fmt/check/test/clippy).
5. Then: regenerate the 2 recv_marker fixtures + un-ignore; then migrate CORE-007 (haematite) + liminal
   channel/conversation actors from sync structs to real supervised beamr processes.

The brief-dispatch loop (haematite/liminal non-actor briefs) keeps running in parallel and is independent.

## How to verify it's fixed
A test: process A `spawn`s process B (B does `receive Msg -> ... end`), A does `B ! msg` **via the Send
opcode through the real scheduler**, assert B receives it (and the timeout path works). That single test —
which does not exist today — is the gate. Extend to a ref-receive (monitor/gen_server) round-trip for the
recv_marker fixtures.
