# beamr async distribution sender — spec (proper fix for distributed-pg findings #1b + #2)

Goal: a single async distribution-sender owning ALL outbound distribution I/O. Callers ENQUEUE and return
immediately — NEVER block_on on scheduler worker threads. Builds ON TOP of Piece-2 checkpoint (commit 3a49170),
same branch finish-distribution. Read each cited file:line before editing.

## Critical architectural facts (grounded)
- Scheduler workers are plain std::thread (scheduler/mod.rs:761-776) — NO ambient tokio runtime. That's why
  block_on_distribution_send builds a throwaway runtime per call (supervision_integration.rs:670-718).
- TWO ConnectionManagers: distribution_connections (mod.rs:616-617, the one pg+control use) has NO owned runtime —
  it rides the AMBIENT runtime via bare tokio::spawn (connection.rs:426,476,495), which exists in #[tokio::test] but
  NOT in production. So the new sender MUST own a Runtime AND the connection read/accept tasks must be bound to it
  (closes a pre-existing prod gap where inbound is also un-driven).
- DistConnection::write_raw(self:&Arc<Self>, &[u8]) -> io::Result<()> (connection.rs:175) is the only send primitive;
  on failure calls mark_down → connection-down hook → purge_remote_node (NO I/O, non-blocking). get_connection(node)
  -> Option<Arc<DistConnection>> (connection.rs:369) is a non-blocking DashMap lookup.

## File 1 NEW: distribution/sender.rs
- `pub enum DistOutbound { ToNode { node: Atom, frame: Arc<[u8]> } }` (pre-encoded; producer does ETF encode on the
  worker thread, drain does ONLY TCP I/O).
- `#[derive(Clone)] pub struct DistSender { tx: mpsc::Sender<DistOutbound>, inner: Arc<DistSenderInner> }`;
  `struct DistSenderInner { runtime: tokio::runtime::Runtime, drain: JoinHandle<()> }`.
- `DistSender::new(connections: ConnectionManager) -> Option<Self>`: build a 1-worker multi_thread Runtime
  (thread_name "beamr-dist-send"); bounded `mpsc::channel(DIST_SEND_QUEUE_CAP=1024)`; spawn drain task on the runtime:
  `while let Some(DistOutbound::ToNode{node,frame}) = rx.recv().await { if let Some(conn)=connections.get_connection(node)
  { let _ = conn.write_raw(&frame).await; } }`  — CONNECTED-ONLY (never reconnect inline); ignore write Err (dead peer
  must not stall drain; mark_down handles cleanup).
- `handle(&self) -> tokio::runtime::Handle` (clone of the owned runtime's handle — for binding conn reads/accepts).
- `enqueue(&self, DistOutbound)`: `let _ = self.tx.try_send(item);` — NON-BLOCKING; on Full|Closed → DROP (slow/dead
  peer must never stall a worker; dropped updates self-correct on next join/leave or node-down purge). NOT blocking_send, NOT send().await.
- `shutdown(&self) { self.inner.drain.abort(); }` (runtime drops when last clone drops).
- Per-node ordering preserved: single drain FIFO + write_raw serializes per-conn behind writer Mutex (connection.rs:133,177).
- HoL caveat: single drain → slow peer's write blocks others until kernel send buffer clears (bounded; OK for low-freq pg).
  Add `// FUTURE: per-node sub-channels if hot`. Do NOT build now.
- distribution/mod.rs: add `pub mod sender;`.

## File 2 MODIFY: scheduler/mod.rs
- Add `SharedState.dist_sender: Option<DistSender>` (find `struct SharedState` decl; add to initializer at :681).
- At :615-617 after building distribution_connections: `let dist_sender = if replay_enabled { None } else {
  DistSender::new(distribution_connections.clone()) };` then `if let Some(s)=&dist_sender { distribution_connections.set_runtime_handle(s.handle()); }`.

## File 2b MODIFY: connection.rs (Option A — REQUIRED, not optional)
- Add `set_runtime_handle(&self, Handle)` storing `Option<Handle>` in ConnectionManagerInner (connection.rs:229-238);
  change the 3 `tokio::spawn(...)` sites (426,476,495) to `handle.spawn(...)` when set, else fall back to tokio::spawn
  (keeps #[tokio::test] ambient-runtime tests working). Without this the SEND side is async but RECEIVE side is still
  un-driven in production — so this MUST be included.

## File 3 MODIFY: scheduler/execution.rs:70-88
- In shutdown(), before `shared.shutdown.store(true,...)` (:80): `if let Some(s)=&self.shared.dist_sender { s.shutdown(); }`.

## File 4 MODIFY: scheduler/pg_propagation.rs — broadcast enqueues (no block_on)
- New broadcast body: upgrade Weak<SharedState>; `let Some(sender)=&shared.dist_sender else {return};` (replay no-op);
  encode frame (encode_pg_update_frame, local_node=shared.local_node.name); `let frame: Arc<[u8]> = Arc::from(frame.into_boxed_slice());`
  `for node in shared.distribution_connections.connected_nodes() { sender.enqueue(DistOutbound::ToNode{node, frame: Arc::clone(&frame)}); }`.
- Drop `use ...block_on_distribution_send`; add `use std::sync::Arc;` + `use crate::distribution::sender::DistOutbound;`.
- pg.rs join/leave/remove_pid_from_all_scopes UNCHANGED (they already broadcast after dropping locks). Async is entirely in broadcast.

## File 5 MODIFY: distribution/pg.rs — split local purge (finding #2)
- Add `pub fn remove_pid_from_all_scopes_local(&self, pid: u64) -> Vec<PgUpdate>`: lock state, for each group remove pid
  from members.local, collect PgUpdate::Leave; return AFTER dropping guard (non-blocking, no broadcast).
- Rewrite `remove_pid_from_all_scopes(pid)` to compose: `let u=self.remove_pid_from_all_scopes_local(pid); let p=self.propagation();
  for x in u { p.broadcast(x); }` (preserves existing semantics + the e2e leave test).

## File 6 MODIFY: scheduler/execution/core.rs — wire process-exit (finding #2)
- In cleanup_exited_process (:1464-1491), after :1490: ONE LINE `shared.pg_registry.remove_pid_from_all_scopes(pid);`
  (local purge runs sync inside; propagation is async via the installed broadcast). dist_sender is installed at
  mod.rs:721-725 before any user process spawns, so this is safe. Empty-map scan for non-pg processes = cheap, no I/O.

## remote-EXIT seam — SCOPE OUT (do NOT half-build)
- ControlRouter::send_exit (remote_link.rs:86-99) buffers into Mutex<Vec> never transmitted. Wiring needs an EXIT/LINK
  control encoder (doesn't exist) + link/monitor semantics (bigger surface). Leave `// FUTURE: wire ControlRouter::send_exit
  onto DistSender once an EXIT-control encoder + link/monitor semantics land` at remote_link.rs:99. The sender is
  frame-agnostic so it reuses the same enqueue path later. Do NOT touch it now.

## KEEP block_on_distribution_send
- Still the path for SchedulerDistributionSendFacility::send_remote (supervision_integration.rs:644-668, the sync BIF
  `!`-to-remote-pid, which must report NoConnection to its caller). Different contract — do NOT migrate it. Only pg
  broadcast moves to async.

## Tests
- sender.rs unit: (1) enqueue non-blocking / drops-on-full (no peer → drain drops, loop completes bounded, no panic);
  (2) per-node FIFO ordering (register_test_connection connection.rs:482 + tokio duplex; enqueue N seq frames; assert FIFO);
  (3) dead-peer doesn't stall (closed write half + a live node; live node still receives; down-hook fired).
- tests/pg_distribution_e2e.rs (extend): existing 3 use eventually() polling → must still pass unchanged (confirm). Add
  (4) connected-only: A joins with NO connection to B; join returns promptly; B never sees member over full poll window;
  then connect + join a 2nd member; B sees ONLY the 2nd (1st dropped, not buffered-replayed). (5) exit → immediate local
  purge + propagated leave: real process on A pg-joins, connect A→B, B sees it, terminate process; A local_members empties,
  B remote_members drops the member.
- core.rs test: (6) cleanup_exited_process local purge (no connection): register pid in pg, cleanup, assert local_members
  empty, no panic.

## HAZARDS (all must be honored)
- Arc CYCLE: the drain closure must capture ONLY ConnectionManager (Arc<Inner>), NEVER Arc<SharedState>. DistSender holds
  ConnectionManager not SharedState. SchedulerPgPropagation still holds Weak<SharedState> and reaches sender via
  shared.dist_sender after upgrade. Zero new strong refs to SharedState. THE SINGLE MOST IMPORTANT INVARIANT.
- Shutdown: abort drain before joining workers; owned Runtime drops with SharedState; no double-runtime-drop.
- e2e timing: eventually() already tolerates async; prior sync-blocking was incidental, not relied on. Run
  `cargo test -p beamr --test pg_distribution_e2e` to confirm.
- Replay: dist_sender=None (no runtime); broadcast + exit-path early-return on None.
