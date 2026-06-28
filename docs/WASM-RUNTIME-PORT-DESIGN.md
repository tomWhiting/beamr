# beamr — WASM Runtime Port: Design + Decomposition

> **Status: design doc, 2026-06-27.** READ-ONLY analysis of the current tree; no production code
> changed. This is the build plan for porting beamr's **native-actor execution model** (the
> `NativeHandler` / `Actor` / `spawn_actor` surface that haematite and liminal consume) to run under
> `wasm32-unknown-unknown` in a browser. It is the BLOCKER for haematite WASM-002/003 (OPFS WAL +
> browser transport) and liminal's browser transport (tracked as task #46 / #42).
>
> **The single most important finding up front:** a working single-threaded cooperative **bytecode**
> scheduler for wasm *already exists* (`crates/beamr/src/scheduler/wasm.rs`, `WasmScheduler`), wired to
> the browser event loop (`setTimeout`/`clearTimeout`, Promise microtasks) through `crates/beamr-wasm`.
> What it does **not** do is run **native (Rust) processes** — `NativeHandler` / `Actor` — which is
> exactly the API haematite and liminal use. So this is **not** a green-field scheduler port; it is
> "teach the existing `WasmScheduler` to drive native-process slices, and provide single-threaded
> equivalents of the few facilities a native slice needs." That is a much smaller, much lower-risk job
> than the prompt's framing assumed.

---

## 1. Current-state map: threading / blocking / scheduler model

beamr has **two parallel scheduler implementations** in the same crate, selected by Cargo feature,
**not** by target:

| Scheduler | File | Concurrency | Drives bytecode? | Drives native processes? |
|---|---|---|---|---|
| **Threaded** (`Scheduler`) | `scheduler/mod.rs` + `execution/*` + `dirty.rs` | work-stealing, N OS threads | ✅ | ✅ (`run_native_slice`) |
| **Cooperative** (`WasmScheduler`) | `scheduler/wasm.rs` | single-threaded, host-driven | ✅ | ❌ **(the gap)** |

The feature gate is the key lever (`crates/beamr/Cargo.toml:53-63`):

```
default = ["std", "threads", "net", "fs", "jit", "embedded"]
threads = ["std", dep:crossbeam-channel, dep:crossbeam-deque, dep:crossbeam-queue,
           dep:io-uring, dep:num_cpus, dep:tokio]
```

And the module gating (`crates/beamr/src/lib.rs:20-51`): `io`, `hook`, `replay`, `timer`, **and the
entire native-actor re-export** (`Actor`, `ActorContext`, `spawn_actor`, `NativeContext`,
`NativeHandler`, `NativeOutcome`) are all behind `#[cfg(feature = "threads")]`. So in a threadless
build today, **the very API haematite/liminal import does not exist**. (`distribution` is behind
`feature = "net"`; `io` behind `feature = "fs"`/`"threads"`.)

### 1a. The threaded scheduler — every OS-thread spawn

- **Scheduler worker threads**, one per CPU (default `available_parallelism()`):
  `scheduler/mod.rs:795-809` — `std::thread::Builder::new().name("beamr-sched-{i}").spawn(...)`, each
  builds a local `RunQueue` and enters `scheduler_loop` (`execution.rs:222-281`).
  Thread-count default resolved at `mod.rs:573-577,1025-1031`; replay mode forces 1.
- **Dirty CPU pool** (default `num_cpus`) and **dirty IO pool** (default 10):
  `scheduler/dirty.rs:197-228` (`std::thread::Builder...spawn(worker_loop)`); pools live in
  `SharedState` (`mod.rs:146-147`). Counts at `dirty.rs:583,589`.
- **IO completion poller thread**: `io/bridge.rs:69-100` spawns a dedicated thread that loops on
  `ring.poll_completions(...)` and dispatches wakeups.
- **IO thread-pool fallback** (non-Linux): `io/thread_pool.rs` (`DEFAULT_POOL_SIZE = 4`).
- **io_uring submission** (Linux): `io/uring.rs` (Linux syscalls).

### 1b. The threaded run-loop & slice model

- `scheduler_loop` (`execution.rs:222-281`): drain spawn requests → (sched 0) tick timers → drain
  woken → pop local queue or steal → **park if idle** → execute one slice → repeat until shutdown.
- A slice = `run_process` → `core::execute_slice` (`execution/core.rs:46-68`): branches on
  `process.is_native()` (`core.rs:53`): **native** → `run_native_slice` (`native_slice.rs:27`);
  **bytecode** → `interpreter::run_with_native_services` (`core.rs:614`); **replay** path also exists.
- Reduction budget `DEFAULT_REDUCTION_BUDGET = 4000` (`process/types.rs:30`); preemptive yield at
  budget exhaustion. Process-slot state machine `Present/Executing/Absent`
  (`process_slot.rs:98-105`) gives the running thread exclusive ownership of the `Process` body during
  a slice; cross-thread updates land in `Executing` metadata and merge at store-back
  (`core.rs:281-321`).
- Run queues: per-thread priority `crossbeam_deque::Worker` (Max/High/Normal/Low)
  (`run_queue.rs:26-34`); work-stealing `steal::try_steal` steals ~half (`run_queue.rs:118-144`).

### 1c. Every BLOCKING point (the wasm-hostile sites)

| Site | File:line | Kind | Notes |
|---|---|---|---|
| Idle scheduler park | `execution.rs:716` | `Condvar::wait_timeout` (5 ms) | the work-stealing idle wait |
| Embedder wait-for-exit | `execution.rs:114` | `Condvar::wait_timeout` (10 ms) | `run_until_exit` |
| Dirty worker | `dirty.rs:311` | `Receiver::recv()` | blocks for next dirty job |
| Init barrier | `mod.rs:802,823` | `Barrier::wait` | scheduler boot sync |
| Init handle pull | `mod.rs:815` | `mpsc::recv` | boot only |
| IO poller | `io/bridge.rs` | blocking `poll_completions` | dedicated thread |
| **`SenderHandle::call`** | `native/actor.rs:410,418-420` | `crossbeam_channel::bounded(1)` + `recv_timeout` | **external request/reply — blocks the caller** |
| Timer-wheel lock | `native/native_process.rs:280-282` | `Mutex<TimerWheel>.lock()` | per `send_after` |

**All of `std::thread`, `Condvar`, `Barrier`, `crossbeam`, `tokio`, `io_uring` are unavailable or
forbidden on `wasm32-unknown-unknown` on the main thread.** None of these are reachable from the
`WasmScheduler` path — they are all in the `threads`/`net`/`fs`-gated code.

### 1d. The pieces that are ALREADY platform-neutral (reusable as-is)

These compile and behave identically in wasm because they do not touch threads:

- **Mailbox.** Arrival queue is a lock-free `crossbeam_queue::SegQueue` (`mailbox/mod.rs:48-55`); send
  deep-copies into the receiver heap and never blocks (`mailbox/mod.rs:278`); selective receive is an
  owner-only scan that returns `None` when empty (`mailbox/selective.rs:16-32`) — the scheduler parks
  the process, the *thread* never blocks. (Note: `SegQueue` comes from `crossbeam-queue`, currently
  pulled in only by the `threads` feature — see Decision D2.)
- **Timer wheel** (`timer.rs`). **Passive** — it is *polled* via `tick()/tick_at()`
  (`timer.rs:199-217`), returning expired entries. No thread, no `tokio::sleep`. (It is `threads`-gated
  today only by association, not by necessity.)
- **The native-slice body itself** (`native_slice.rs:27-114`). It touches `SharedState` for only four
  things: `exit_tombstones`, `replay_driver`, `timers`, and `build_native_services` (which needs
  `local_send` + `spawn_facility`). The actual `NativeContext::new(...)` + `handler.handle(&mut ctx)`
  (`native_slice.rs:76-77`) and the outcome mapping (`Continue→Requeue`, `Wait→Wait`, `Stop→Exited`,
  `native_slice.rs:95-113`) are **platform-agnostic**.
- **`NativeContext`** capability surface (`native/native_process.rs:129-295`): `self_pid`, `recv`
  (non-blocking, `:164-175`), `send` (non-blocking, `:184-208`), `alloc_tuple`, `spawn_native`,
  `schedule`/`send_after` — all non-blocking against the *thread*; they mutate process/heap/timer
  state. Only `send_after` touches a `Mutex<TimerWheel>` (single-threaded → uncontended).

### 1e. The existing `WasmScheduler` (what already works)

`scheduler/wasm.rs` (542 lines) is a complete cooperative kernel for **bytecode** processes:

- Single-threaded; processes in `BTreeMap<u64,Process>`; `ReadyQueues` by priority
  (`wasm.rs:64-65,508-537`).
- `run_until_idle()` (`wasm.rs:333-423`): one bounded turn — pop, reset reductions, run one slice via
  `run_with_native_services` (`wasm.rs:371`), then `Yielded→requeue`, `Waiting→park`,
  `Exited`/`errored` capture. **DirtyCall is rejected** (`wasm.rs:403-411`).
- Timers re-expressed onto the host: scheduler records `pending_timer_schedules` /
  `pending_timer_cancellations` (`wasm.rs:128-136`); the host drains them and arms `setTimeout`, then
  calls `timer_fired(pid,timer_id)` (`wasm.rs:139-154`) to wake the process. A message arriving before
  the timeout cancels the pending timer (`wasm.rs:319-329`).
- Async host calls (fetch/IO/JS) re-expressed as Promises: `WasmAsyncNifFacility`
  (`native/context/mod.rs:309-317`) lets a NIF start host work and suspend; `complete_async`
  (`wasm.rs:157-163`) injects the resolved value into x(0) and wakes.
- The `beamr-wasm` crate (`crates/beamr-wasm/src/lib.rs`) is the host seam: `WasmVm`
  (`#[wasm_bindgen]`), `spawn` (`:173`), `run_step` → `run_until_idle` (`:188`), `send_message`
  (`:116`), `setTimeout` bridge (`:246-268`), Promise→async-NIF bridge via
  `wasm_bindgen_futures::spawn_local` + `JsFuture` (`:346-370`), and a BEAM-term↔`JsValue` converter
  (`convert.rs`).

**Conclusion of the map:** the cooperative engine, the event-loop timer bridge, the async/Promise
suspend-resume, and the JS host seam are all built and tested. The missing piece is a **native-process
branch** in `run_until_idle` plus single-threaded versions of `local_send` + `spawn_facility` and a
poll-based timer. Everything else the consumers need is already platform-neutral.

---

## 2. Constraints — the wasm execution model, concretely

`wasm32-unknown-unknown` in a browser tab:

1. **No OS threads by default.** `std::thread::spawn` is unsupported. (The wasm threads proposal +
   `wasm-bindgen-rayon` exist but require Web Workers, `SharedArrayBuffer`, atomics, and **cross-origin
   isolation** — `COOP`/`COEP` headers. Out of scope for v1; see Decision D1.)
2. **No blocking on the main thread.** `Condvar`, `Barrier`, `channel.recv()`, `recv_timeout`,
   `thread::sleep`, and any spin-wait will hang or panic. The thread *is* the browser event loop —
   blocking it freezes the page. **`SenderHandle::call` (the blocking request/reply,
   `actor.rs:400-421`) cannot run on the main thread.**
3. **Time is event-loop based.** Delays come from `setTimeout`/`setInterval`
   (macrotasks) and `queueMicrotask`/Promise (microtasks); `requestAnimationFrame` for frame-aligned
   work. There is no synchronous sleep. The host calls back *into* wasm when a timer fires.
4. **Concurrency = the single event loop** (cooperative) unless you adopt Web Workers +
   `SharedArrayBuffer` for true parallelism (separate wasm instances communicating via `postMessage`
   or shared memory + atomics; far more complex, and needs cross-origin isolation).
5. **I/O is async host calls.** No `io_uring`, no sockets, no blocking file API. Browser equivalents:
   `fetch`, **OPFS** (`navigator.storage.getDirectory`, sync-access handles only inside a Worker),
   IndexedDB, WebSocket — all Promise/event-callback shaped. These map onto the existing
   `WasmAsyncNifFacility` seam, not onto beamr's `io` module.
6. **Single-threaded `Rc`/`RefCell` is the idiom** (the `WasmScheduler` already uses
   `Rc<RefCell<WasmScheduler>>`, `beamr-wasm/src/lib.rs:48`). `Send`/`Sync` bounds are vacuous but
   currently *required* by the traits (`NativeHandler: Send`, `Actor: Send`,
   `ActorMessage: Clone+Send+Sync`) — harmless to keep, see Decision D3.

---

## 3. The three execution-model options, assessed

### (a) Single-threaded cooperative scheduler on the wasm event loop — **RECOMMENDED**
Processes run one bounded slice each per host tick and cooperatively yield; the host pumps
`run_until_idle()` from a microtask/`setTimeout(0)`/`requestAnimationFrame`. **This is what already
exists** for bytecode; we extend it to native processes.

- **Pro:** matches the existing `WasmScheduler`, the existing timer/async bridges, and the consumers'
  actual workload (see §6 — both haematite shards and liminal channels/conversations are wake-on-demand
  coordination, not CPU-bound parallel compute). No `SharedArrayBuffer`, no cross-origin isolation, no
  atomics, ships in any browser/worker. Determinism is a bonus (replay-friendly, single-threaded).
- **Con:** no parallelism — a long CPU-bound native slice blocks the turn (mitigated by the reduction
  budget for bytecode; native handlers must stay short, which they already are by design — one message
  per slice, `actor.rs:188-193`). The blocking `SenderHandle::call` is unusable on the main thread
  (Decision D4 gives an async alternative).

### (b) Web Workers + SharedArrayBuffer for real parallelism — **NOT for v1**
Multiple wasm instances in Workers, an atomics-based work-stealing scheduler over shared linear memory.
This is essentially re-implementing the threaded scheduler with `Atomics.wait`/`notify` instead of
`Condvar`, plus a shared-heap term representation.

- **Pro:** real multicore in the browser.
- **Con:** enormous. Requires `SharedArrayBuffer` (cross-origin isolation, which breaks some embeds),
  a shared-memory allocator/heap (beamr heaps are per-process `Vec<u64>`, not shared), atomics-based
  parking, and a rewrite of `SegQueue`/run-queues onto shared memory. The consumers do not need it
  (§6). High risk, low payoff now.

### (c) Hybrid — cooperative main scheduler + Workers for offload — **the natural later step**
Keep the cooperative scheduler as the model; push genuinely heavy/blocking work (a CPU-bound store
compaction, OPFS sync-access-handle file I/O that must block) into a **Worker** behind the existing
async-NIF seam (start work → Promise/`postMessage` → `complete_async` on resume). haematite already
gestures at this: it has a `wasm` module with `WasmRuntime` that spawns Web Workers and a
`WasmShardRuntime` wrapping `beamr_wasm::WasmVm` per worker. This is option (a) for the runtime + Workers
as *async resources*, **not** shared-memory parallel scheduling — the right long-term shape.

**Recommendation: (a) now, designed so (c) is reachable** — i.e. don't bake in any assumption that
breaks running each scheduler instance inside its own Worker, and route all blocking/heavy work through
the async-NIF seam.

---

## 4. Recommended approach

**A `cfg(target_arch = "wasm32")` (more precisely, a threadless-build) native-process backend behind the
same `beamr::` API, by extending the existing `WasmScheduler` to drive native slices.**

### 4.1 Make the native-actor API exist in a threadless build
Today `Actor`/`NativeHandler`/`spawn_actor`/`NativeContext` are `#[cfg(feature = "threads")]`
(`lib.rs:46-51`). Re-gate so they are available when either `threads` **or** a new `cooperative`
feature (enabled for wasm) is on. The `native`, `timer`, and the lock-free `SegQueue` mailbox bits must
move out from under the bare `threads` gate (Decision D2). `io`, `distribution`, `jit`, `dirty` stay
native-only.

### 4.2 Provide single-threaded facilities the native slice needs
`run_native_slice` needs `local_send` + `spawn_facility` (via `build_native_services`,
`native_slice.rs:66`), plus `timers`, `exit_tombstones`, `replay_driver`. Provide a cooperative
implementation of each that operates against the `WasmScheduler`'s own state instead of `SharedState`:

- **`SpawnFacility` (cooperative):** `spawn_native(parent, factory, link_to)` allocates a pid, builds a
  `Process` carrying the `NativeBody` (factory + handler), records the link, pushes to `ReadyQueues`.
  This mirrors `WasmScheduler::spawn` (`wasm.rs:187-223`) but stores a native body rather than a
  bytecode entry. (`SpawnFacility` trait: `native/spawn.rs:32-126`.)
- **`LocalSendFacility` (cooperative):** deliver into the target mailbox and wake if waiting — the
  `WasmScheduler` already has exactly this in `send`/`enqueue_owned_message`/`after_successful_enqueue`
  (`wasm.rs:284-329`). Wrap it as the facility the `NativeContext` calls.
- **Timers:** either keep the existing host-`setTimeout` bridge (extend `register_receive_timer` to
  also serve `NativeContext::send_after`'s `Deliver` timers), or run the **passive `TimerWheel`** and
  `tick()` it once per host turn. Recommended: reuse the host-`setTimeout` bridge that already works
  (`wasm.rs:128-154`, `beamr-wasm/lib.rs:246-268`) so delays are real wall-clock and don't require the
  host to poll on a fixed cadence.
- **`exit_tombstones` / supervision:** a single-threaded `BTreeSet`/`BTreeMap` on the `WasmScheduler`;
  on `NativeOutcome::Stop`, run link/monitor propagation and (for supervised children) re-invoke the
  retained `factory` to restart — the same factory path NATIVE-002 uses (`native_slice.rs:52-56`).

### 4.3 Add the native branch to the cooperative loop
In `run_until_idle` (`wasm.rs:333-423`), before/instead of the unconditional
`run_with_native_services` call, branch on `process.is_native()` (mirroring `core::execute_slice`,
`core.rs:53`) and call a cooperative `run_native_slice` that uses the §4.2 facilities. Map outcomes the
same way the threaded path does (`Continue→requeue`, `Wait→park`, `Stop→exit+supervise`).

### 4.4 Re-express the blocking request/reply
`SenderHandle::call` (`actor.rs:400-421`) blocks on a `crossbeam_channel` — illegal on the main
thread. Provide, under wasm, a **non-blocking `call_async` that returns a JS `Promise`** (or a Rust
`Future`): spawn the transient `call_handler` sender, register the reply ref, and resolve the Promise
from the reply cast on a later turn. Casts (`SenderHandle::cast`, `actor.rs:384-392`) are already
non-blocking and need no change. `cast` from inside a handler (`ActorContext::cast`,
`actor.rs:159-167`) is already the recommended intra-actor pattern and works unchanged.

### 4.5 Supervision, message passing, blocking-await — summary of re-expression
- **Message passing:** unchanged (lock-free `SegQueue` mailbox; cooperative `LocalSendFacility` wakes).
- **Supervision:** single-threaded link/monitor sets + factory restart on the cooperative scheduler.
- **Timers:** host `setTimeout` (already wired) or `tick()`ed passive wheel.
- **Blocking await (`call`):** replaced by `call_async`/Promise; internal `Wait` parking is unchanged
  (the scheduler parks the *process*, never the event loop).

This keeps `NativeHandler`, `NativeOutcome`, `NativeContext`, `Actor`, `ActorContext`, `ActorMessage`,
`ActorRef`, `spawn_actor`, `cast` **byte-for-byte API-identical**. The only consumer-visible delta is
`call` → `call_async` under wasm (a small, well-isolated change for liminal's routing path,
`liminal/.../execute/actor.rs:40-45`).

---

## 5. KEY DECISIONS for Tom (each with a recommendation)

### D1 — Parallelism model: single-thread cooperative vs Web Workers + SharedArrayBuffer
**Recommendation: single-thread cooperative (option a).** The consumers are coordination/IO-bound, not
parallel-compute-bound (§6). Cooperative needs no cross-origin isolation, ships everywhere, reuses the
existing engine, and is deterministic. Keep the door open to the **hybrid (c)** — Workers as async
resources behind the async-NIF seam — which is what haematite's `WasmRuntime`/`WasmShardRuntime` already
sketch. Defer true SharedArrayBuffer parallelism indefinitely unless a profiled need appears.

### D2 — Feature gating: how the threadless build exposes the native-actor API
**Recommendation:** introduce a `cooperative` feature (auto-on for `target_arch="wasm32"`). Re-gate
`native::actor`, `native::native_process`, the `timer` module, and the `SegQueue` mailbox path to
`any(feature="threads", feature="cooperative")`. Leave `io`, `distribution`, `jit`, `dirty`,
work-stealing `scheduler/mod.rs` under `threads`/`net`/`fs`. Move `crossbeam-queue` (for `SegQueue`)
out of the `threads`-only dep list into the cooperative path too. This is the cleanest way to get one
crate that builds either a full native runtime or a cooperative wasm runtime from the same source.

### D3 — `Send`/`Sync` bounds on the public traits
**Recommendation: keep them.** `NativeHandler: Send` (`native_process.rs:42`), `Actor: Send`
(`actor.rs:111`), `ActorMessage: Clone+Send+Sync` (`actor.rs:83`) are vacuously satisfiable under
single-threaded wasm and keeping them means **haematite/liminal handlers compile unchanged** on both
targets. Relaxing them would be a churny API change for zero benefit. (The factory type
`NativeHandlerFactory = Box<dyn Fn()->… + Send + Sync>`, `native_process.rs:34`, likewise stays.)

### D4 — Blocking `SenderHandle::call`
**Recommendation:** provide `call_async` (Promise/Future) under wasm and leave `cast` unchanged;
document that `call` is `#[cfg(feature="threads")]`-only. Liminal's routing path is the one caller and
the change is local. Inside handlers the cast-with-self-pid pattern is already the prescribed approach.

### D5 — How much of the existing scheduler is reusable
**Finding (not really a choice):** ~80% is already there. Reusable as-is: `WasmScheduler` loop, mailbox,
passive timer wheel, async-NIF/Promise bridge, JS host seam, the platform-neutral *core* of
`run_native_slice` (the `handle()` call + outcome mapping). Net-new: cooperative `SpawnFacility` +
`LocalSendFacility`, a native branch in `run_until_idle`, single-threaded supervision/tombstones,
`call_async`. The threaded `scheduler/mod.rs`, `dirty.rs`, `io/*`, `distribution/*` are **not** ported —
they stay native-only.

### D6 — Effort tier
**Recommendation/assessment: Medium (M).** Roughly **2–4 focused weeks** for a solo build to reach "a
native `Actor` runs in the browser, exchanges messages, supervises a child, and survives a child
restart, proven by `wasm-bindgen-test`," plus integration glue for one real haematite shard and one
liminal channel. This is *not* a from-scratch scheduler (which would be L/XL); the hard, risky parts
(event-loop integration, Promise suspend/resume, term↔JS marshalling) are already solved and tested.

---

## 6. Is single-threaded cooperative *enough* for haematite/liminal? (honest assessment)

**Yes, for the targeted browser use cases.** Evidence from the consumers:

- **haematite** uses `NativeHandler`/`NativeContext`/`NativeOutcome`, a per-shard `AtomTable`, factory
  restart, and `ExitReason` (`shard/actor/native.rs:482`, `ttl/sweep/mod.rs:272`,
  `sync/scheduler.rs`). Shard actors are **wake-on-demand**: pop one command per wake, do a sync Rust
  storage op, reply, return `NativeOutcome::Wait`. No CPU-bound Erlang bytecode, no cross-shard locks
  during a command. It already has a `wasm` module (`WasmRuntime` spawning Workers, `WasmShardRuntime`
  over `beamr_wasm::WasmVm`, IndexedDB store) — but **not yet driving real shard actors** in wasm,
  which is exactly this port. Its distribution/`ConnectionManager` use
  (`sync/endpoint.rs:44`, `sync/protocol/wire.rs`) is **server-side only** and not needed in the
  browser (single-node per tab/worker).
- **liminal** uses the high-level `Actor`/`spawn_actor`/`call_timeout`/`ActorError`
  (`.../execute/actor.rs:40-45`) and `NativeHandler` (`channel/subscription.rs:62-79`). Channels,
  conversations, participants are all mailbox-driven coordination; a routing function is a pure Rust
  closure run inside `catch_unwind`. No parallel compute. Its `pg`/`ConnectionManager`/`ProcessContext`
  use is **`liminal-server` only** (`cluster/sync.rs:27-32`), not browser.

The one genuine "needs real blocking/parallelism" risk is **OPFS sync-access file handles**, which are
only usable *inside a Worker* and can block there. That is precisely the **hybrid (c)** case: run the
haematite store's blocking file work in a Worker behind the async-NIF seam, keep the actor scheduler
cooperative. So even the hardest consumer requirement does not argue for SharedArrayBuffer scheduling —
it argues for Workers-as-async-resources, which (a)+(c) already accommodate.

**Where single-threaded would *not* be enough** (and is explicitly out of scope): CPU-bound parallel
Gleam workloads, multi-core throughput goals, or a shared in-browser heap across processes. None are on
haematite/liminal's browser roadmap.

---

## 7. Decomposition — spike-first numbered increments

Each is independently verifiable, mostly via `wasm-bindgen-test` (headless Chrome/Firefox) and native
`cargo test` for the cooperative facilities.

- **WR-0 — Spike: prove a native `Actor` can run under the existing engine.**
  *Native test first* (no wasm): in a threadless/`cooperative` build, drive a trivial `NativeHandler`
  through a cooperative `run_native_slice` against an in-memory cooperative `SpawnFacility` +
  `LocalSendFacility`. Then the **wasm proof**: a `#[wasm_bindgen_test]` where a trivial native actor
  spawns, receives one message (sent via `WasmVm::send_message`), and exits with a captured result.
  *Verify:* test goes green in headless wasm. This de-risks the whole port.

- **WR-1 — Feature re-gating (D2).** Add `cooperative` feature; move `native::actor`,
  `native::native_process`, `timer`, and the `SegQueue` mailbox path to
  `any(threads, cooperative)`; ensure `crates/beamr` builds for `wasm32-unknown-unknown` with
  `--no-default-features --features cooperative,std`. *Verify:* `cargo build --target wasm32-unknown-unknown`
  succeeds; `Actor`/`NativeHandler` symbols exist in the wasm build.

- **WR-2 — Cooperative `SpawnFacility` + `LocalSendFacility`.** Implement against `WasmScheduler` state
  (reuse `wasm.rs:187-223` spawn shape and `:284-329` send/wake). *Verify:* native test — spawn a child
  from a parent handler via `spawn_native`, deliver a message, observe it run.

- **WR-3 — Native branch in `run_until_idle`.** Branch on `process.is_native()` and call the
  cooperative `run_native_slice`; map `Continue/Wait/Stop`. *Verify:* mixed test — a bytecode process
  and a native process coexist and both make progress in the same `WasmVm`.

- **WR-4 — Native timers on the event loop.** Wire `NativeContext::send_after`/`schedule` `Deliver`
  timers into the existing host `setTimeout` bridge (extend `register_receive_timer`/`timer_fired`,
  `wasm.rs:139-154,453-468`). *Verify:* `wasm-bindgen-test` — a native actor schedules a self-tick and
  is rescheduled when it fires.

- **WR-5 — Supervision + restart.** Single-threaded link/monitor sets + `exit_tombstones`; on `Stop`,
  propagate to linked, and restart supervised children via the retained factory. *Verify:* test — a
  child crashes (`Stop(Error)`), supervisor restarts it via factory, new child receives a message.

- **WR-6 — `call_async` (D4).** Promise-returning request/reply on `SenderHandle` under wasm; resolve
  from the reply cast on a later turn. *Verify:* `wasm-bindgen-test` — JS `await vm.call(...)` resolves
  with the actor's reply; a timeout rejects.

- **WR-7 — Async host I/O via the seam (hybrid groundwork).** Confirm a native handler can start host
  async work (fetch/OPFS-in-Worker) through `WasmAsyncNifFacility` + `complete_async`
  (`wasm.rs:157-163`) and resume. *Verify:* test — handler issues a host async call, suspends, resumes
  with the result.

- **WR-8 — haematite integration smoke.** Run one real haematite shard `NativeHandler`
  (`shard/actor/native.rs`) in `beamr-wasm` against the existing IndexedDB/OPFS store; one put/get
  round-trip. *Verify:* headless wasm test stores and reads a value through a real shard actor.

- **WR-9 — liminal integration smoke.** Run one liminal channel `SubscriberProcess`
  (`channel/subscription.rs`) and one routing `Actor` (via `call_async`) in `beamr-wasm`. *Verify:*
  subscribe → publish → deliver; routing call returns a reply.

- **WR-10 — Driver ergonomics + docs.** A small host-side pump (`requestAnimationFrame`/microtask loop)
  in `beamr-wasm` so consumers don't hand-pump `run_step`; document the cooperative model + `call_async`
  migration. *Verify:* an example browser page runs an actor loop without manual stepping.

WR-0..WR-3 are the spine (cooperative native execution). WR-4..WR-6 complete the actor semantics.
WR-7..WR-9 are integration. WR-10 is polish.

---

## 8. Risks / prerequisites / open questions

**Risks**
- **Reaching into `SharedState`.** `build_native_services` (`native_slice.rs:66`,
  `supervision_integration::build_native_services`) and the slice's tombstone/replay/timer reads assume
  the threaded `SharedState`. The mitigation is to abstract the *facilities* (already trait objects:
  `SpawnFacility`, `LocalSendFacility`) and provide cooperative impls — but any hidden direct
  `SharedState` field access inside the native path is friction. **WR-0 must flush these out.**
- **Reduction budget for native slices.** Bytecode is preempted by reductions; a native `handle()` runs
  to completion. A pathological handler that loops blocks the turn. Consumers honor one-message-per-slice
  by convention (`actor.rs:188-193`); document it as a hard rule for wasm.
- **`call` deadlock surface.** If any consumer calls `SenderHandle::call` from the browser main thread it
  hangs the page. WR-6 + a `#[cfg]` removal of `call` under wasm makes this a compile error instead.
- **Bundle size.** The VM + bytecode interpreter in wasm is chunky (noted in `BROWSER-OTP-NORTH-STAR.md`).
  Native-only consumers (haematite/liminal *Rust* actors, no Gleam bytecode) may be able to build with
  `--no-default-features` minus `jit`/`embedded` to slim it. Measure.
- **Crate-feature entanglement.** `net`/`fs`/`threads` deps (`tokio`, `io-uring`, `libc`, `rustix`)
  must not leak into the wasm build. WR-1 is where this is proven; expect some dependency-graph surgery.

**Prerequisites**
- beamr 0.8.0 native-process API is landed (it is: `NativeHandler`/`Actor`/`spawn_actor`/supervision,
  per the changelog and `native/*`).
- A `wasm32-unknown-unknown` toolchain + `wasm-pack`/`wasm-bindgen-test` runner (the `beamr-wasm` crate
  already depends on `wasm-bindgen`, `js-sys`, `wasm-bindgen-futures`).

**Open questions**
1. Should the cooperative scheduler **also** run each instance inside its own Worker now (per
   haematite's `WasmShardRuntime`), or stay main-thread for v1 and add Workers in the hybrid phase?
   (Recommend: main-thread for WR-0..WR-9, Workers as a follow-on; nothing in the design precludes it.)
2. Do we want a **passive-`TimerWheel` tick** model (host pumps on a cadence) as a fallback to the
   `setTimeout` bridge, for environments where many fine-grained timers make per-timer `setTimeout`
   wasteful? (Defer; `setTimeout` is fine for the expected timer volumes.)
3. Replay under wasm — `replay` is `threads`-gated and not needed for the browser use cases. Confirm we
   can compile the native path with replay **disabled** (the slice reads `shared.replay_driver`, so the
   cooperative facility must supply a `None`/no-op).

---

## 9. One-paragraph bottom line

The scary version of this task — "port a work-stealing OS-thread scheduler to wasm" — is not the task.
A single-threaded cooperative scheduler, the event-loop timer bridge, Promise-based async suspend/resume,
and the JS host seam **already exist and are tested** (`scheduler/wasm.rs`, `crates/beamr-wasm`). They
just don't run **native (Rust) `NativeHandler`/`Actor` processes**, which is the one thing haematite and
liminal use. The port is: re-gate the native-actor API to a `cooperative` build, provide single-threaded
`SpawnFacility`/`LocalSendFacility`/supervision, add a `is_native()` branch to `run_until_idle`, and
replace the one blocking `call` with a `Promise`-returning `call_async`. The public API
(`NativeHandler`/`Actor`/`spawn_actor`/`cast`/supervision) stays byte-for-byte compatible behind
`cfg(target_arch="wasm32")`, so haematite/liminal don't fork. Single-threaded cooperative is **enough**
for both consumers' browser use cases (coordination/IO-bound, not parallel-compute), with Workers used as
async resources (the hybrid) for the one genuinely-blocking case (OPFS file handles). Effort: **Medium,
~2–4 weeks** to a `wasm-bindgen-test`-proven native actor with supervision, plus integration smoke tests.

---

## 10. Using the cooperative runtime (WR-10)

This section is the consumer-facing guide to the merged cooperative runtime: the
execution model, the `call` → `call_async`/`Promise` migration, the WR-10 host
pump, and the wasm time base. It describes the API as merged on `main`, not a
proposal.

### 10.1 The cooperative model in one paragraph

On native targets beamr runs a multi-threaded, reduction-preempted scheduler.
On `wasm32-unknown-unknown` there are no OS threads, so the runtime is a
**single-threaded cooperative scheduler** (`WasmScheduler`, `scheduler/wasm.rs`)
driven by the JavaScript host. The host advances the runtime one *turn* at a
time: each turn first expires any due native `Deliver` timers, then gives every
ready process one reduction-bounded slice. Bytecode processes are preempted by
the reduction budget; a **native `handle()` runs to completion within its slice**,
so a native handler must process at most one message per slice and then return
`Wait`/`Stop` (the one-message-per-slice rule — a handler that loops forever
blocks the whole turn and hangs the page). Nothing is parallel: a single
`Rc<RefCell<WasmScheduler>>` lives on the browser main thread, and every host
entry point takes a short, scoped `borrow_mut`.

### 10.2 `call` (blocking, threaded) → `call_async` / `Promise` (cooperative, wasm)

The threaded `SenderHandle::call` blocks the calling thread until the reply
arrives. **On wasm that would deadlock the page** — the reply can only be
produced by a later turn, which the same (blocked) thread would have to drive.
The cooperative surface replaces it:

- **Rust native callers (haematite/liminal actors):** use
  `CoopSenderHandle::call_async`, which returns a host-pumpable `CallFuture`
  instead of blocking. It is ref-correlated, so concurrent in-flight calls never
  cross replies. `cast` (fire-and-forget) is unchanged.
- **JavaScript hosts:** `WasmVm::call(pid, request)` returns a real JS
  `Promise` (the `CallFuture` wrapped via `future_to_promise`). `await vm.call(...)`
  resolves with the actor's reply once the host keeps pumping; a timeout self-tick
  rejects it. `WasmVm::cast(pid, message)` is the non-blocking send (a cast to a
  dead pid is silently dropped, exactly like a BEAM send).

Migration rule of thumb: every threaded `handle.call(req)?` becomes, under wasm,
`handle.call_async(req).await` (Rust) or `await vm.call(pid, req)` (JS), and the
host must be pumping turns for the future/Promise to make progress.

### 10.3 Driving the runtime: manual stepping vs the WR-10 host pump

There are two ways to advance turns; both live on `WasmVm` and are additive.

**Manual stepping (unchanged, for tests / custom hosts):**

- `vm.run_step()` runs one turn to quiescence and returns a JSON summary
  (executed / yielded / waiting / exited / errored + captured exit results), then
  reflects any newly-armed receive-timers into host `setTimeout`s.
- `vm.pump_once()` is the same per-turn body but returns a plain `bool` —
  *whether the scheduler still has pending work* — instead of the JSON summary,
  so it is cheap to call every frame. Use `run_step` when you want the summary,
  `pump_once`/the pump when you just want to drive to idle.

**The WR-10 pump (recommended for browser apps):**

```js
import init, { create_vm } from "./pkg/beamr_wasm.js";
await init();
const vm = create_vm();
const pid = vm.spawn_actor((request) => ({ result: request.n + 1 }));

// Start a requestAnimationFrame-driven pump: no more hand-calling run_step.
const pump = vm.start_pump();

const reply = await vm.call(pid, { n: 41 }); // resolves as the pump drives turns
console.log(reply.result);                   // 42

pump.stop(); // or let the returned PumpHandle be GC'd / dropped
```

`vm.start_pump()` installs a `requestAnimationFrame` loop that runs `pump_once`
each frame — expiring native timers off a real wasm clock
(`web_time::Instant::now()`, see §10.4), running every ready process, and
draining pending timer schedules — then yields the browser and reschedules
itself **while work remains**. When a turn leaves the scheduler idle (no ready
process and no armed native timer) the pump stops requesting frames rather than
burning a rAF slot every frame on an idle VM. The events that re-enqueue a
parked process — an inbound `send_message`/`cast`, a fired host timer, or an
async-NIF completion — already wake the target, so a host that delivers such an
event simply (re)starts a pump to resume. `start_pump` returns a `PumpHandle`;
call `handle.stop()` (idempotent) or drop it to cancel the loop.

**Borrow discipline (why the pump is safe):** the rAF closure captures only
cloned `Rc`s (the scheduler and the host-timer map) plus a stop flag — never
`&mut self`. Each turn's scheduler access is a short scoped `borrow_mut` inside
`pump_once` / `sync_host_timers_inner` that is dropped before the closure
reschedules itself, so no `RefCell` borrow is ever held across the rAF callback
(the classic nested-`borrow_mut` panic source). A turn that errors stops the
pump cleanly rather than panicking across the wasm boundary.

**Idle predicate:** the pump's keep-going decision is
`WasmScheduler::has_pending_work()` — true iff a process is ready OR a native
`Deliver` timer is armed. It deliberately does *not* count processes parked in
`waiting` with no armed timer (those are blocked on a host-delivered external
event). This predicate is plain scheduler state, so it is unit-tested natively
(no browser) by the `has_pending_work_*` tests in `scheduler/wasm_native_tests.rs`,
which exercise the exact branch the rAF loop runs.

### 10.4 The wasm time base (`web-time`)

The native timer wheel (`timer.rs`) and the cooperative timer seam read a
monotonic clock via `web_time::Instant` rather than `std::time::Instant`. On
native targets `web_time::Instant` is a re-export of `std::time::Instant`, so
there is **zero behavior change** (the full native suite is unchanged-green). On
`wasm32-unknown-unknown`, `std::time::Instant::now()` panics (no time source); a
single armed native timer would otherwise panic the page on the next turn, since
`run_until_idle` ticks the wheel each turn. `web_time::Instant` is backed by
`performance.now()` there, so native timers (`NativeContext::schedule` /
`send_after`, `WasmVm` host `setTimeout` receive-timeouts, and the rAF pump's
per-turn tick) work in the browser. `Duration` stays `std::time::Duration`
(portable). The deterministic test/host seam `tick_native_timers_at(now)` takes a
`web_time::Instant` (which a browser host derives from `performance.now()`); the
threaded scheduler's own timing is untouched.

### 10.5 What is and isn't headless-testable

The pure logic — the idle predicate, the timer wheel, the cooperative
actor/`call_async` path — is proven by native `cargo test`. The
`requestAnimationFrame` loop, the JS `Promise` resolution, and the actual
`spawn_actor`/`call` round-trip are `#[wasm_bindgen_test]`s that require a
browser/Node wasm runner (`wasm-pack test` / `wasm-bindgen-test-runner`); they
are compile-gated in CI here but execute under a real runner. rAF does not fire
under a bare wasm test harness, so the pump's executable proof of its *pure*
logic is the native `has_pending_work` suite.
