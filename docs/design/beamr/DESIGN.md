---
type: design
cluster: beamr
title: Beamr — BEAM Process Runtime in Rust
---

# Beamr — BEAM Process Runtime in Rust

## Intention

Beamr is the floor Meridian's workflows and agents stand on. When it's
done, a Gleam workflow definition compiles to `.beam` bytecode, loads
into a Rust runtime, and executes with the properties that make the BEAM
the best concurrent runtime ever built: preemptive fairness (no task
starves others), per-process fault isolation (one crash doesn't sink the
ship), supervision trees (the system heals itself), and hot code loading
(deploy without restart).

It should feel inevitable. A workflow author writes typed Gleam, the
compiler catches errors, the runtime handles concurrency and recovery
structurally. The author never thinks about threads, locks, retry logic,
or process lifecycle — those are the runtime's problem. Native Rust
operations (git, AST-merge, dependency graph, diagnostics) are plain
function calls inside the VM, not foreign invocations across a boundary.

The reduction-boundary hook — the thing that makes this more than an
academic exercise — gives Meridian a guaranteed, regular checkpoint on
every running process, regardless of what that process is doing. A live
conscience, not a post-mortem.

## Problem

Workflow definitions today are either configuration (YAML — write-once,
debug-never, no types, no composition, no tests) or dynamically typed
scripts (Rhai — runtime errors 45 minutes into a run, no LSP, no
tooling). Neither gives you compile-time safety, composable pipelines,
or structural fault recovery.

Rust's async ecosystem cannot provide the concurrency model workflows
need. Tokio tasks are cooperatively scheduled — a task that doesn't
`.await` pins a core indefinitely. There is no preemptive fairness, no
per-task fault isolation, no supervision. A panicking task can poison
shared state. A CPU-bound task starves its neighbours.

The BEAM has all of these properties, but it's written in C, has no
clean embedding story for Rust, and carries 30 years of Erlang/OTP
baggage. Existing attempts to rebuild it (Lumen/Firefly — abandoned,
targeted LLVM native compilation; AtomVM — C, targets embedded/IoT;
Lunatic — WASM runtime, not a BEAM interpreter) either aimed too wide
or solved a different problem.

The gap: a Rust runtime with the BEAM's execution model, scoped to what
Gleam workflows actually need, where native Rust operations are
first-class citizens inside the VM rather than foreign functions fighting
across a C boundary.

## Solution

### Design Principles

1. **Beamr depends on nothing of Meridian's. Meridian depends on beamr.**
   The VM is a self-contained machine that knows how to load bytecode,
   run processes, and call functions you hand it. It does not know what
   git is, what Yggdrasil is, or what a diagnostic is. Those plug in
   from outside.

2. **Demand-driven scope.** The loader's unresolved-import report is the
   work queue. Every external function a `.beam` file calls is listed in
   its import table. When the loader can't resolve one, that's a signal:
   "implement this BIF." The scope of BIF implementation physically
   cannot grow beyond what the workflows demand. The import table is the
   leash.

3. **No silent failures.** A process that encounters an error crashes
   visibly. Supervisors handle recovery structurally. No swallowed
   errors, no default fallbacks, no "log and continue."

4. **Per-process isolation is load-bearing.** Every process owns its
   heap, its stack, its mailbox. Nothing is shared. One process crashing
   affects only itself and its linked processes. This property is not
   aspirational — it is the architectural invariant the rest of the
   system depends on.

5. **Fairness is guaranteed, not hoped for.** The interpreter counts
   reductions. When the budget hits zero, the process yields. No process
   can starve others, even in a tight computation loop with no I/O.

### Module Layout

The runtime is organised as modules inside a single `beamr` crate.
These are tightly coupled — the interpreter touches terms, heap,
scheduler, and mailbox constantly — and splitting them into separate
crates would add ceremony without value at this stage.

**Atom table** — global interned string table. Lock-free concurrent map.
Every name in the system (module names, function names, atoms like `ok`
and `error`) lives here exactly once. Comparing two atoms is comparing
two integers. The loader populates it; the interpreter consults it;
terms can be atoms.

**Loader** — reads `.beam` files (the chunked binary format the Gleam
toolchain produces via `erlc`). Parses atom tables, instruction streams,
import/export tables, literal tables, string tables, lambda tables.
Resolves imports against loaded modules and the BIF registry. Produces
runnable `Module` values. Unresolved imports become the demand-driven
work queue.

**Terms** — tagged 64-bit values. Immediates (small integers, atoms,
pids, nil) fit in a single word. Boxed values (tuples, lists, binaries,
big integers, floats, closures, maps, references) are pointers into the
process-local heap. Low-bit tagging, not NaN-boxing — the hot path is
integers, atoms, and pointers, not floats.

**Processes** — the unit of execution and isolation. Each owns a heap,
stack, mailbox, reduction counter, link/monitor sets, trap-exit flag,
and status. Spawning is microsecond-scale — no OS thread, no allocation
beyond the initial small heap. A process cannot reach into another's
memory.

**Interpreter** — the execution loop. Fetch, decode, execute, decrement
reduction counter. When the counter hits zero, save state and yield.
Implements the subset of BEAM opcodes that Gleam actually emits — not
all ~170. The binary-matching instruction family is the hardest subset
and gets focused attention.

**Scheduler** — N OS threads, each with a run queue of ready processes.
Pick the highest-priority process, execute it for a reduction budget,
handle the result (yielded → back of queue; waiting → wait set; exited →
cleanup). Work stealing: idle schedulers take half the processes from
the busiest queue. No async runtime in the scheduler hot path — plain
OS threads plus lock-free work-stealing queues.

**Memory and GC** — per-process generational copying collector. Young
generation (nursery) collected frequently; old generation collected
rarely. Heap sizes start small (~2KB) so collection is microseconds.
GC affects only the process being collected — no stop-the-world, ever.
Messages are copied into the receiver's heap on delivery, preserving
isolation.

**Mailbox** — per-process message queue. Lock-free MPSC. Selective
receive: the process pattern-matches against the mailbox, taking the
first message that matches and deferring the rest. A save pointer tracks
scan progress to avoid rescanning. When no message matches, the process
suspends until mail arrives or a timeout fires.

**Supervision primitives** — links (bidirectional, fatal by default),
monitors (unidirectional, non-fatal), exit signals (carry the exit
reason along links and monitors), and the trap-exit flag (converts fatal
exit signals into messages). The VM provides these four primitives;
actual supervisor behaviour is Gleam library code (gleam_otp) running on
the VM like any other program.

**Native function interface** — a registry where Rust functions are
registered under module:function/arity names. When the interpreter hits
a call to a registered native, it invokes the Rust function directly —
same process, no IPC, no serialisation. BIFs (built-in, ship with the
VM) and NIFs (registered by the host) use the same mechanism but have
different ownership: BIFs are beamr's responsibility, NIFs are
Meridian's.

**Dirty schedulers** — a separate thread pool for native calls that
genuinely take time (git push, cargo build). Long-running work goes
here so normal scheduler threads stay free and fair. Same concept as
the BEAM's dirty schedulers, for the same reason.

**Timer wheel** — handles `receive after` timeouts, periodic ticks, and
step deadline enforcement. Wakes suspended processes when their timeout
fires.

**Reduction-boundary hook** — the seam where Meridian listens. Every
time a process yields (budget exhausted or blocking on receive), the
hook fires. Meridian's conventions-and-diagnostics pipeline can inspect
what just happened and intervene before the process resumes. This is the
same primitive that norn uses at the tool boundary and norn-memory uses
for resonance, but at the lowest altitude — the interpreter loop itself.
A process cannot outrun the tap on the shoulder.

### Integration Points

**Gleam toolchain** — Gleam compiles to Erlang source; `erlc` compiles
that to `.beam` bytecode. Beamr loads the `.beam` output. The bytecode
format is stable on Erlang's release cadence.

**Meridian (via beamr-meridian, later)** — Yggdrasil operations (git,
merge, graph, tracking, worktree), Meridian operations (messaging,
storage, workflow dispatch), and filesystem operations are registered as
NIFs. The reduction-boundary hook is wired to norn's conventions engine.

**Gleam OTP library** — supervision trees, actors, typed processes. Runs
as regular Gleam code on the VM. The VM provides the primitives (links,
monitors, exit signals); gleam_otp builds the patterns.

### Numbered Decisions

**D1 — Loader is a module inside the core crate, not a separate crate.**
The loader's outputs (Module, Term, Atom) are runtime types. Splitting
it out creates a dependency problem: either the loader depends on core
(negating the independence) or a shared-types crate is needed (which
becomes a gravity well). Start with the loader as a module; extract
later only if the seam proves itself. Don't solve a dependency problem
you haven't created yet.

**D2 — Atom table lives in the core, loader accepts a handle.**
The atom table is a runtime concern that the loader happens to populate.
Keeping it in core avoids a micro-crate and keeps ownership clear.

**D3 — No async runtime in the scheduler.** The scheduler uses plain OS
threads and lock-free work-stealing queues. Tokio enters only at the
edges (file I/O, network I/O) via dirty schedulers. The BEAM's green
threads are our interpreter loop, not async tasks. Keeping tokio out of
the hot path keeps the fairness story simple and ours.

**D4 — Low-bit term tagging, not NaN-boxing.** The hot path is integers,
atoms, and pointers. NaN-boxing optimises for float-heavy workloads,
which this is not. Classic low-bit tagging matches the BEAM's own scheme
and the assumptions baked into the compiled bytecode.

**D5 — Implement only the opcodes Gleam emits.** The BEAM has ~170
opcodes. Gleam uses a subset. The loader's import and instruction
analysis tells us exactly which opcodes appear in real `.beam` files
from `gleam build`. Implement those; leave the rest as explicit
unimplemented errors that name the missing opcode.

**D6 — BIFs are demand-driven.** The set of built-in functions to
implement is determined by the loader's unresolved-import report, not
by surveying the BEAM's BIF table. If no workflow imports it, it
doesn't exist yet. The import table is the leash.

**D7 — Supervision is library code, not VM machinery.** The VM provides
four primitives: links, monitors, exit signals, and trap-exit.
Supervisor strategies (one_for_one, one_for_all, rest_for_one, restart
limits) are implemented in Gleam via gleam_otp, running on the VM like
any other code. No special VM support for supervisors.

**D8 — Messages are copied between processes.** Send copies the term
into the receiver's heap. This preserves per-process isolation
absolutely. Large binaries may use reference-counted shared storage
as an optimisation, but the semantic model is always copy.

**D9 — The reduction-boundary hook is a registration point, not a
hard-coded call.** The core provides the seam; what runs in it is
registered from outside (by beamr-meridian). The core does not know
what diagnostics are. This preserves principle 1.

**D10 — Dirty schedulers are a separate thread pool.** Native functions
that may block (I/O, git operations, compilation) run on dirty scheduler
threads, not normal scheduler threads. The normal scheduler never blocks
on native work. The dirty pool size is configurable independently of the
normal scheduler thread count.

**D11 — Mailbox uses lock-free MPSC.** The per-process mailbox is backed
by a lock-free multi-producer single-consumer queue (crossbeam SegQueue
or equivalent). Multiple processes can send to the same mailbox
concurrently without mutex contention. This is the hot path for message
passing — every send touches it — and lock-free is the only choice that
doesn't create a serialisation bottleneck under concurrent sends.

**D12 — Dual-version module registry with Arc-based purge detection.**
The registry holds at most two versions of each module: current and old.
Loading a new version promotes current to old and installs the new as
current. Purge safety is checked via `Arc::strong_count` — if only the
registry holds a reference to the old version, no process is executing
it and it can be safely dropped. This uses Rust's reference counting as
the liveness check instead of building a separate process-scanning
mechanism. A third version cannot be loaded until the old is purged.

**D13 — Process version pinning at external call boundaries.** A running
process holds an `Arc<Module>` pinning it to the module version it
entered. Intra-module calls (call, call_only, call_last) never
re-resolve from the registry — they use the pinned version. Version
transitions happen only on outgoing fully-qualified external calls
(call_ext), where the process picks up the current version of the target
module. Each stack frame stores its own `Arc<Module>` so that returns
resume in the saved version, not the current registry version. This
matches BEAM semantics: the classic upgrade pattern `?MODULE:loop(State)`
is an outgoing fully-qualified self-call that picks up the new version.

**D14 — Closure version binding via deterministic unique ID.** Closures
store the module generation and a deterministic `unique_id` (hash of
module name, function name, arity, num_free) computed at load time. At
call time, if the closure's generation matches the current module, the
lambda index is used directly (fast path). If generations differ, the
closure resolves its lambda by unique_id scan against the current
version's lambda table. If the function was removed, the old version is
checked. This changes the closure heap layout (two extra words for
generation and unique_id), which requires corresponding updates to the
GC walker and the trampoline dispatch path.

## Goals

1. Load and execute a `.beam` file produced by `gleam build` — pure
   Gleam modules (arithmetic, pattern matching, list operations)
   execute correctly.

2. Spawn processes and pass messages between them — mailbox delivery,
   selective receive, and process exit all function correctly.

3. Preemptive scheduling across multiple OS threads — a tight
   computation loop yields after N reductions; idle schedulers steal
   work; every ready process runs at least once per scheduling epoch
   (one full pass through all scheduler run queues).

4. Per-process garbage collection — minor and major collections
   triggered by allocation pressure; GC does not affect other processes;
   all term references survive collection intact.

5. Register a Rust function and call it from Gleam — arguments decode
   correctly, return values encode correctly, errors propagate as
   process exit reasons.

6. Fault isolation via supervision primitives — process linking
   propagates exit signals, monitors deliver DOWN messages, trap-exit
   converts signals to messages. These primitives are sufficient to
   enable OTP-style supervision patterns (one_for_one restart, restart
   limits, supervisor escalation) when driven by gleam_otp library code.

7. The reduction-boundary hook fires on every process yield and can
   be used by an external registrant to inspect and intervene.

8. Hot-load a new module version while processes run the old version —
   new fully-qualified calls use the new version, intra-module calls
   stay on the pinned version, closures resolve across versions via
   unique ID, and the old version is purged after the last reference
   exits.

## Non-Goals

- **Distribution protocol.** Beamr is single-node. No Erlang distribution,
  no inter-node communication, no epmd. Cross-instance communication
  goes through Meridian's exchange, not BEAM distribution.

- **ETS (Erlang Term Storage).** Concurrent term tables are useful but
  not required for workflow execution. If needed, Rust's concurrent
  maps (dashmap) provide the same capability at the native layer.

- **Full OTP stdlib.** Beamr implements the BIFs that Gleam's stdlib
  and gleam_otp actually import, not the full `erlang` module surface.
  Demand-driven per D6.

- **JIT compilation.** The interpreter is sufficient for workflow
  scripts. A JIT (like BEAM's OTP 24+ JIT) would optimise compute-
  heavy paths but adds enormous complexity for marginal workflow
  benefit.

- **Port drivers.** BEAM's port system for external process I/O. Use
  NIFs and dirty schedulers instead.

- **Process dictionary.** The BEAM provides a per-process key-value
  store (`put`/`get`). Gleam discourages its use and gleam_otp does
  not depend on it. If specific `gleam_erlang` functions require it,
  add a simple `HashMap<Term, Term>` to the process struct at that
  point — but it is not in initial scope.

- **Tracing and debugging BIFs.** Add incrementally if needed. Not
  in initial scope.

## Structure

```
crates/
├── beamr/                         — the VM
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                 — public API: Vm::new(), load_module(), spawn()
│       ├── error.rs               — crate error types
│       ├── atom/
│       │   ├── mod.rs             — AtomTable, Atom handle type
│       │   └── table.rs           — lock-free concurrent intern map
│       ├── loader/
│       │   ├── mod.rs             — load_beam() entry point
│       │   ├── parser.rs          — chunked .beam format parser
│       │   ├── decode.rs          — bytecode instruction decoder
│       │   └── validate.rs        — instruction operand validation
│       ├── term/
│       │   ├── mod.rs             — Term type, tag constants, immediate ops
│       │   ├── boxed.rs           — boxed term headers and accessors
│       │   ├── binary.rs          — binary/bitstring representation
│       │   └── compare.rs         — term ordering and equality
│       ├── process/
│       │   ├── mod.rs             — Process struct, lifecycle
│       │   ├── heap.rs            — per-process heap allocator
│       │   ├── stack.rs           — call stack frames
│       │   └── registry.rs        — process name registry
│       ├── interpreter/
│       │   ├── mod.rs             — execute() loop, reduction counting
│       │   ├── opcodes.rs         — opcode dispatch table
│       │   └── pattern.rs         — pattern match instruction support
│       ├── scheduler/
│       │   ├── mod.rs             — scheduler thread pool, main loop
│       │   ├── run_queue.rs       — priority run queues
│       │   ├── steal.rs           — work-stealing logic
│       │   └── dirty.rs           — dirty scheduler thread pool
│       ├── gc/
│       │   ├── mod.rs             — GC entry point, trigger logic
│       │   ├── minor.rs           — nursery → old generation copy
│       │   └── major.rs           — full heap compaction
│       ├── mailbox/
│       │   ├── mod.rs             — Mailbox struct, send/receive
│       │   └── selective.rs       — selective receive + save pointer
│       ├── supervision/
│       │   ├── mod.rs             — link/monitor/exit-signal dispatch
│       │   ├── link.rs            — bidirectional link management
│       │   └── monitor.rs         — unidirectional monitor management
│       ├── native/
│       │   ├── mod.rs             — NIF/BIF registry
│       │   └── bifs.rs            — built-in function implementations
│       ├── hook.rs                — reduction-boundary hook registration
│       ├── module.rs              — module registry (single-version)
│       └── timer.rs               — timer wheel for timeouts
├── beamr-cli/                     — thin runner for bring-up
│   ├── Cargo.toml
│   └── src/
│       └── main.rs               — load .beam, register natives, run
└── beamr-meridian/                — (later) Meridian integration
    ├── Cargo.toml
    └── src/
        ├── lib.rs                 — registration entry point
        ├── yggdrasil.rs           — git, merge, graph, tracking NIFs
        ├── meridian.rs            — messaging, storage, workflow NIFs
        ├── fs.rs                  — filesystem NIFs
        └── hook.rs                — wire reduction hook to conventions
```

## Constraints

- **No file over 500 lines.** If a module approaches this, split by
  responsibility. The interpreter's opcode dispatch is the most likely
  candidate — split by opcode family if needed.

- **No `.unwrap()` or `.expect()` outside `#[cfg(test)]` code.** All
  production paths use `?` or explicit error handling. A process
  encountering an error exits with a reason — it does not panic the
  scheduler thread. This rule is mechanically enforceable via clippy
  lint. The only acceptable panics are in test code and in truly
  unrecoverable states (out of memory, which is `alloc::handle_alloc_error`).

- **No flat module structures.** Related code lives in directories with
  `mod.rs` files, not as a pile of files in `src/`. Single-file modules
  at the `src/` root are acceptable when they have no sub-responsibilities
  (e.g. `hook.rs`, `module.rs`, `timer.rs`). The structure section above
  is the canonical layout.

- **No unsafe without justification.** Unsafe blocks are acceptable for
  term tagging, heap pointer manipulation, and lock-free data
  structures — the core of a VM implementation. Each unsafe block must
  have a safety comment explaining the invariant it relies on.

- **Per-process isolation is non-negotiable.** No shared mutable state
  between processes. No process can read or write another's heap. If a
  design decision would weaken isolation, the decision is wrong.

- **The hook is passive.** The reduction-boundary hook is a registration
  point. The core fires it and waits for a response. It does not make
  decisions about what to do — the registrant does. The core stays
  ignorant of Meridian's concerns.
