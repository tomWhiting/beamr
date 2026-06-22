# beamr — Native-Process Abstraction (design)

> **Status: design, 2026-06-22.** Drives the NATIVE-001/002/003 briefs. Built HANDS-ON
> (coordinated + reviewed, not via the Aion dispatch loop) because it is delicate scheduler-internal
> concurrency. Companion to `MESSAGING-FIX-SCOPE.md` (the send fix, shipped in 0.7.0) and the terrain
> map produced 2026-06-22.

## Goal

Let a **Rust struct/closure run as a first-class, scheduler-supervised beamr process** — real pid,
mailbox, links/monitors, send/receive, supervision/restart — **without being BEAM bytecode**. This is
the missing primitive that unblocks the actor thesis everywhere it is currently faked: haematite's
shard actor (CORE-007) and liminal's channel/conversation actors. Today beamr `spawn` only creates
bytecode processes (MFA / lambda → code label); the 0.7.0 `LocalSendFacility` routes `Term`s *between*
bytecode processes but gives Rust code no process identity.

## Decision: a native process IS a `Process` carrying a Rust handler (NOT a new ProcessSlot variant)

Two shapes were considered:

- **Shape A — `ProcessSlot::NativeProcess(Box<dyn NativeBody>)`.** Conceptually clean, but the terrain map
  found **~35 slot-match sites** across `execution/core.rs`, `execution.rs`, and `supervision_integration.rs`,
  every one of which would need a new arm — and each native arm would have to *re-derive* the
  concurrency-correct behaviour of the BEAM path (the `ProcessMetadata` swap, the park-gap protocol, the
  clock-under-slot-lock observation, exit-tombstones). High blast radius on the exact code where subtle
  divergence is catastrophic.

- **Shape B (CHOSEN) — reuse `crate::process::Process` as the carrier.** A native process is a real
  `Process` (so it keeps heap, `Mailbox`, `logical_clock`, links/monitors metadata, `ProcessMetadata`
  swap, park-gap, supervision, exit-tombstones — all **unchanged and already correct**). It additionally
  carries an optional Rust handler. The **only** new behaviour is *what executes during a slice*: if the
  process has a native handler, the scheduler runs the handler instead of the bytecode interpreter.

Shape B is the call. It confines all genuinely-new code to one dispatch branch + a spawn path + a trait,
and **neutralises four of the five landmines by construction** (see below) because the native process
travels the same concurrency machinery as a bytecode process.

### Why this is safe to graft onto `Process`

A native process simply never sets a code position / x-registers / stack — those stay default/empty. Its
status transitions (New → Running → Waiting → Exiting) reuse the existing machinery. `send_local`'s
Present arm calls `process.observe_message_clock(...)` with **no new arm** because the receiver genuinely
is a `Process`. The park `Wait` arm, the `Executing`-slot `pending_local_messages` merge at store-back,
and `cleanup_exited_process` all apply verbatim.

## The model (NATIVE-001)

```rust
/// What a native process does when the scheduler gives it a slice.
pub trait NativeHandler: Send + 'static {
    /// Called when the process is scheduled (it has mail, was woken, or just spawned).
    /// Drain & handle messages via ctx, optionally send replies, return an outcome.
    fn handle(&mut self, ctx: &mut NativeContext<'_>) -> NativeOutcome;
}

pub enum NativeOutcome {
    /// Re-queue immediately (more work to do this turn).
    Continue,
    /// Nothing to do; park until a message arrives (uses the existing 3-phase park-gap path).
    Wait,
    /// Terminate this process with the given reason (drives cleanup_exited_process → supervision).
    Stop(ExitReason),
}

/// The capability surface a handler is given for one slice. Borrows the running Process + services.
pub struct NativeContext<'a> { /* self_pid, mailbox access, send(), spawn_native(), … */ }
```

`NativeContext` exposes: `self_pid() -> u64`; mailbox draining (`recv()` / iterate the mailbox using the
**existing** `Mailbox` API — `current_message`/`advance`/`remove_current`); `send(target_pid, Term)`
routed through the **existing** `LocalSendFacility` (so sender-clock ticking + replay validation are reused
verbatim); and `spawn_native(...)`. No new synchronisation primitives.

### Dispatch seam

Branch in `run_process` (`execution/core.rs:46-62`), immediately after `take_runnable_process`:

```
let mut process = take_runnable_process(...);
let outcome = if process.is_native() {
    run_native_slice(shared, &mut process)   // NEW
} else if shared.replay_mode {
    ...
} else {
    execute_slice(shared, &mut process)      // bytecode path, untouched
};
// existing Requeue / Wait / Exited handling — shared by both paths
```

`run_native_slice`: (1) **check exit-tombstone first** (resolves landmine #4) — if a kill is pending for
this pid, return `SliceOutcome::Exited(reason)`; (2) build the native services the handler needs
(`local_send`, `spawn`) over `Arc<SharedState>`; (3) construct `NativeContext`; (4) call
`handler.handle(ctx)`; (5) map `NativeOutcome` → `SliceOutcome` (`Continue→Requeue`, `Wait→Wait`,
`Stop→Exited`). The native path stays **out of the bytecode hot path** entirely.

### Spawn path

`spawn_native` mirrors `SchedulerSpawnFacility::spawn` (`supervision_integration.rs:1130-1217`):
next_pid → `process_table.spawn_with_pid` → build a `Process` that carries the handler (instead of
`build_process`'s bytecode setup) → insert `ProcessSlot::Present(ScheduledProcess(process))` →
`woken.push((pid,0))`. Links via the existing `add_link_to_slot`. **Identical** to the BEAM spawn except
the body constructor.

## The five seams (from the terrain map) and how each is handled

1. **`ProcessSlot` enum** — *no change to the enum.* (Shape B avoids the variant.) New code is a flag/field
   on `Process` indicating a native handler.
2. **`run_process` dispatch** — one new branch → `run_native_slice` (above). The only new control-flow site.
3. **`store_/take_runnable_process` swap** — *unchanged.* A native process is a `Process`, so the
   `ProcessMetadata` shadow, pending-queue drain, and clock merge already apply.
4. **`SpawnFacility`** — add `spawn_native`; scheduler impl mirrors the existing `spawn`.
5. **`send_local` Present arm** — *unchanged.* Receiver is a real `Process`; `observe_message_clock` under
   the slot lock just works.

## Landmines and resolutions (from the terrain map)

- **Replay clock, receiver side** — *dissolved.* Native process is a `Process`; `send_local` calls its
  `observe_message_clock` under the slot lock with no new arm.
- **Slot-lock / park-gap ordering** — *dissolved.* Native uses the **same** `Wait` arm (store → register
  in waiting → recheck). `NativeOutcome::Wait` routes there; we do **not** invent a separate park path.
- **Self-send while Executing** — handled. Self-sends land in `pending_local_messages`, merged at
  store-back (`core.rs:388-397`) the same as for BEAM; the handler reads them next slice via the mailbox.
- **Supervision kill at Executing** — handled by the **tombstone check at the top of `run_native_slice`**
  (seam #2 step 1). Without it, kills are silently ignored — this is the one native-specific obligation,
  so it is an explicit acceptance criterion in NATIVE-001.
- **ETF closure-encoding gap (Executing receiver, `num_free>0`)** — pre-existing limitation, out of scope.
  Document it: a native actor should exchange immediates/refs/scalars (the gen_server pattern), not raw
  closures, for guaranteed delivery while Executing. Track as a follow-up, not a blocker.
- **Process structural clone silently dropping the handler** (surfaced during brief review). `Process` has a
  structural `clone` (`process/mod.rs:207`); a `Box<dyn NativeHandler>` isn't `Clone`, so the handler field
  clones to `None` → the clone is non-native. That's the right *behaviour*, BUT it's a trap: if any LIVE
  scheduler path clones a native process (replay snapshot, spawn, store/take), the clone becomes a dead
  no-op process. Obligation (NATIVE-001 R2 acceptance): audit that `Process::clone` is never invoked on a
  live native process; restart reconstructs via the NATIVE-002 factory, never by cloning a handler instance.

## Replay-determinism stance

Native **sends** tick the sender clock and pass `sender_clock` through `LocalSendRequest` exactly like
`messaging::send` (`messaging.rs:105-116`), so the replay log validates them. Native **receives** are
clock-observed under the slot lock for free (Shape B). A handler MUST therefore route all sends through
`NativeContext::send` (never a side channel) so the clock discipline holds. NATIVE-001 includes a test
that a native↔BEAM message exchange replays deterministically.

## Decomposition

### NATIVE-001 — Native process core (the delicate one; hand-built, heaviest review)
`NativeHandler` trait + `NativeContext` + `NativeOutcome`; `Process` carries an optional handler +
`is_native()`; `run_native_slice` dispatch branch with **tombstone-check-first**; `spawn_native` facility
+ scheduler impl; send→receive→reply round-trip via the existing `LocalSendFacility`; replay-safe sends.
**Gate test (the acceptance bar):** a native "echo/ping" process spawned through the real scheduler
receives a `Term` (from a BEAM process AND from Rust), replies, the reply is observed; plus the park/wait
path (no message → parks → wakes on delivery), the self-send path, and a native↔BEAM replay-determinism
test. No bytecode-path behaviour changes.

### NATIVE-002 — Supervision (links, monitors, exit signals, restart)
Native process participates in links/monitors (mostly free — it lives in a slot; `propagate_exit`,
`deliver_down_messages` are pid-keyed). `NativeOutcome::Stop` → `cleanup_exited_process` → exit-signal
propagation. `trap_exit` for native: drain `pending_exit_messages`, deliver `{EXIT, source, reason}` to
the handler via the mailbox. **Restart:** `spawn_native` must accept a *factory* (`Fn() -> Box<dyn
NativeHandler>` or a clonable spec) so a supervisor can reconstruct a crashed native child. Tests: link a
native↔BEAM pair, crash one, assert propagation; monitor a native proc, assert `DOWN`; a supervisor
restarts a crashed native child.

### NATIVE-003 — Ergonomic actor API + public surface + docs (lower-risk; delegable with lighter review)
A nicer `Actor`/gen_server-style layer over `NativeHandler`: `call` (request/reply with a ref) and `cast`
(fire-and-forget); a public spawn entry returning a `u64` pid + a typed sender handle; re-exports from
`lib.rs`; docs + a worked example = the **actor-per-shard skeleton** haematite/liminal will copy.

## Test / gate bar (every brief)
`cargo fmt --check`; `cargo check -p beamr`; `cargo test -p beamr` (incl. the new round-trip / park /
supervision tests); `cargo clippy -p beamr --all-targets -- -D warnings`; no file >500 lines; no
`unwrap()/expect()` outside `#[cfg(test)]`; **no behavioural change to the bytecode path** (existing
suite stays green). Plus, per the hands-on model: an **independent/adversarial review subagent** on the
concurrency invariants (tombstone-check, park-gap reuse, clock discipline, self-send, no-bytecode-regression)
before any piece is accepted.

## After it lands
Migrate CORE-007 (haematite shard actor) and liminal channel/conversation actors from faked sync structs
to real native beamr processes; then revise + re-dispatch WASM-001 (OPFS) against the settled actor model.
