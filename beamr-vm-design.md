# BEAMR — BEAM Runtime in Rust

A ground-up implementation of the BEAM virtual machine in Rust, targeting
Gleam as the primary source language. Not a port, not a binding, not an
FFI bridge — a native Rust VM that loads `.beam` bytecode and runs it
with full BEAM semantics: preemptive scheduling, per-process garbage
collection, supervision trees, hot code loading, and fault isolation.

---

## Why

The BEAM is the best concurrent runtime ever built. It solves problems
that Rust's async ecosystem still struggles with: preemptive fairness
(no task can starve others), per-process fault isolation (one crash
doesn't take the world), supervision trees (self-healing systems), and
hot code loading (deploy without restart). These properties are exactly
what a long-running workflow execution system needs.

But the BEAM is written in C, has no clean embedding story for Rust,
and carries 30 years of Erlang/OTP baggage. Meanwhile:

- **Gleam's compiler is already written in Rust.** It produces `.beam` bytecode.
- **Gleam's stdlib is small and clean.** No legacy Erlang modules to support.
- **The .beam bytecode format is well-documented.** Loader is straightforward.
- **The core VM concepts are well-bounded.** Scheduler + GC + processes + messages.

A Rust BEAM that targets Gleam only gives you the full BEAM execution
model without the full ERTS implementation burden. The entire stack
becomes Rust: Gleam compiler (Rust) → `.beam` bytecode → BEAMR (Rust) →
native Yggdrasil/Meridian operations via registered functions.

---

## Architecture

### Core Components

```
┌─────────────────────────────────────────────────────────┐
│                        BEAMR                              │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌───────────────────────┐  │
│  │  Loader  │  │ Registry │  │     Scheduler         │  │
│  │          │  │          │  │                       │  │
│  │ .beam    │  │ Module   │  │  N OS threads         │  │
│  │ parser   │  │ table    │  │  M run queues         │  │
│  │          │  │          │  │  Work stealing        │  │
│  │ Bytecode │  │ BIF/NIF  │  │  Reduction counting   │  │
│  │ validate │  │ registry │  │  Preemptive switch    │  │
│  └──────────┘  └──────────┘  └───────────────────────┘  │
│                                                          │
│  ┌──────────────────────┐  ┌──────────────────────────┐  │
│  │     Process          │  │     Message Passing      │  │
│  │                      │  │                          │  │
│  │  Own heap (small)    │  │  Lock-free mailbox       │  │
│  │  Own stack           │  │  Selective receive       │  │
│  │  Own reduction ctr   │  │  Monitor / link          │  │
│  │  Own GC (generational│  │  Exit signals            │  │
│  │    copying, no STW)  │  │                          │  │
│  └──────────────────────┘  └──────────────────────────┘  │
│                                                          │
│  ┌──────────────────────┐  ┌──────────────────────────┐  │
│  │  Supervision         │  │  Hot Code Loading        │  │
│  │                      │  │                          │  │
│  │  one_for_one         │  │  Two module versions     │  │
│  │  one_for_all         │  │  (current + old)         │  │
│  │  rest_for_one        │  │  Fully qualified calls   │  │
│  │  Restart strategies  │  │  switch to new version   │  │
│  │  Max restarts/time   │  │  GC old when unreferenced│  │
│  └──────────────────────┘  └──────────────────────────┘  │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │              Native Function Interface               ││
│  │                                                      ││
│  │  Register Rust functions as BIFs/NIFs                ││
│  │  Yggdrasil ops: git, merge, graph, tracking          ││
│  │  Meridian ops: messaging, storage, workflow          ││
│  │  Zero-cost: same process, no IPC, no serialisation   ││
│  └──────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────┘
```

### 1. Bytecode Loader

The `.beam` file format (documented in the BEAM book and Erlang/OTP
source) uses a chunked binary layout:

- `Atom` chunk — atom table (interned strings)
- `Code` chunk — bytecode instructions
- `StrT` chunk — string table
- `ImpT` chunk — import table (external function references)
- `ExpT` chunk — export table (public functions)
- `FunT` chunk — lambda/closure table
- `LitT` chunk — literal term table (compressed)
- `Line` chunk — source line info (debugging)
- `Type` chunk — type information (Gleam uses this)

**Loader responsibilities:**
1. Parse the chunked format, validate checksums
2. Decode atoms into the global atom table (lock-free concurrent map)
3. Decode bytecode into an internal instruction representation
4. Resolve imports against loaded modules and BIF registry
5. Validate instruction operands (type checking, arity)
6. Store the compiled module in the module registry

**Key Rust types:**
```
BeamFile { chunks: Vec<Chunk> }
Module { name: Atom, exports: Vec<MFA>, code: Vec<Instruction>, literals: Vec<Term> }
Atom — interned via a global concurrent map (dashmap or similar)
MFA — Module:Function/Arity triple
```

**Reference:** The `beam_file` Rust crate (existing, MIT) already parses
`.beam` files. Could be used directly or as reference.

### 2. Term Representation

BEAM terms are tagged values. Everything is a term: integers, floats,
atoms, tuples, lists, binaries, pids, references, funs.

**Tagging scheme (64-bit):**
```
Immediate values (fit in a machine word):
  Small integer:  value | TAG_SMALL_INT
  Atom:           index | TAG_ATOM
  Pid:            data  | TAG_PID
  Port:           data  | TAG_PORT
  Nil:            special constant

Boxed values (pointer to heap):
  Tuple:          ptr | TAG_BOXED  →  [arity, elem0, elem1, ...]
  List/Cons:      ptr | TAG_LIST   →  [head, tail]
  Binary:         ptr | TAG_BOXED  →  binary header + data
  Big integer:    ptr | TAG_BOXED  →  bignum header + limbs
  Float:          ptr | TAG_BOXED  →  float header + f64
  Fun/Closure:    ptr | TAG_BOXED  →  fun header + env + code ptr
  Map:            ptr | TAG_BOXED  →  flatmap or hashmap
  Reference:      ptr | TAG_BOXED  →  ref header + id
```

**Rust implementation:** A `Term` type that is a `u64` with tag bits.
Pattern matching on the tag to dispatch operations. Boxed terms point
into the process-local heap.

### 3. Process Model

Each BEAM process is a lightweight unit of execution with its own:
- **Heap** — small (default 233 words), grows as needed, GC'd independently
- **Stack** — call stack frames, grows downward from top of heap area
- **Mailbox** — queue of received messages
- **Reduction counter** — decremented on each function call
- **Process dictionary** — per-process key-value store (rarely used in Gleam)
- **Links and monitors** — fault propagation connections
- **Trap flag** — whether the process traps exit signals
- **Status** — running, waiting, suspended, exiting

**Process creation:** `spawn(Module, Function, Args)` creates a new
process with a fresh heap, pushes the initial call onto the stack,
and adds it to a scheduler's run queue. Process creation is
microsecond-scale — no OS thread, no allocation beyond the initial
small heap.

**Rust implementation:**
```
struct Process {
    pid: Pid,
    heap: Heap,              // owned, small, growable
    stack: Vec<StackFrame>,
    mailbox: Mailbox,        // lock-free MPSC queue
    reductions: u32,         // countdown, reset on schedule
    status: ProcessStatus,
    links: HashSet<Pid>,
    monitors: HashMap<Reference, Pid>,
    trap_exit: bool,
    group_leader: Pid,
}
```

### 4. Scheduler

The scheduler is the heart of BEAM's concurrency model. It provides
**preemptive** fairness — no process can monopolise a core.

**How it works:**
1. N scheduler threads (one per CPU core by default)
2. Each thread has a run queue of ready processes
3. Pick the highest-priority process from the queue
4. Execute it for up to N reductions (default 4000)
5. On reduction exhaustion: save state, put back on queue, pick next
6. On `receive` with no matching message: move to wait queue
7. On message arrival to waiting process: move back to run queue
8. Work stealing: idle schedulers steal from busy schedulers' queues

**Reduction counting:** A "reduction" is roughly one function call.
The interpreter decrements the counter on each call instruction. When
it hits zero, the process yields. This is cooperative at the
instruction level but preemptive at the process level — no process
can run more than N reductions without yielding, regardless of what
it's doing.

**Rust implementation:**
```
struct Scheduler {
    id: usize,
    run_queue: VecDeque<ProcessRef>,    // normal priority
    high_queue: VecDeque<ProcessRef>,   // high priority
    max_queue: VecDeque<ProcessRef>,    // max priority
    wait_set: HashMap<Pid, WaitReason>,
}

// The main loop per scheduler thread:
loop {
    let process = steal_or_dequeue();
    process.reductions = REDUCTION_BUDGET;
    let result = interpreter.execute(process);
    match result {
        Yielded => run_queue.push_back(process),
        Waiting(reason) => wait_set.insert(process.pid, reason),
        Exited(reason) => handle_exit(process, reason),
    }
}
```

**Work stealing:** When a scheduler's queues are empty, it checks
other schedulers' queues and steals half the processes from the
fullest one. This balances load across cores without central
coordination.

### 5. Garbage Collection

BEAM uses **generational copying GC**, per-process. This is one of
its most important properties — GC pauses affect only the individual
process being collected, not the entire system.

**How it works:**
1. Each process has a young generation (nursery) and old generation
2. When the nursery fills up, minor GC: copy live objects from young to old
3. When the old generation fills up, major GC: compact everything
4. Heap sizes are small (starting at ~2KB), so GC is fast (microseconds)
5. Messages are copied into the receiver's heap on delivery

**Why this matters for workflows:** A workflow step that allocates a
lot of temporary data gets its own GC pauses. Other processes —
other workflow steps, the scheduler, message handlers — are
unaffected. No stop-the-world, ever.

**Rust implementation:** The GC is a copying collector operating on
the process's `Heap`. Since terms are tagged words and boxed values
are heap pointers, the GC walks the root set (stack, registers,
mailbox) and copies reachable terms to a new heap, updating pointers.

### 6. Message Passing

Processes communicate exclusively via asynchronous message passing.
No shared memory, no locks, no data races.

**Send:** Copy the term into the receiver's heap, append to mailbox.
The copy ensures complete isolation — the sender and receiver never
share heap objects.

**Receive (selective):** The `receive` expression pattern-matches
against the mailbox. If no message matches, the process suspends
(moves to wait queue). When a new message arrives, the scheduler
wakes the process to retry matching. The save pointer tracks which
messages have already been tried, avoiding rescanning.

**Rust implementation:**
```
struct Mailbox {
    queue: SegQueue<Term>,    // crossbeam lock-free queue
    save_pointer: usize,     // for selective receive
}
```

### 7. Supervision Trees

The OTP supervision tree is BEAM's self-healing mechanism. A
supervisor process monitors child processes and restarts them
according to a strategy when they crash.

**Strategies:**
- `one_for_one` — only restart the crashed child
- `one_for_all` — restart all children when any crashes
- `rest_for_one` — restart the crashed child and all children started after it
- `simple_one_for_one` — dynamic pool of identical workers

**Restart limits:** `max_restarts` in `max_seconds`. If exceeded,
the supervisor itself crashes (propagating up the tree). This
prevents infinite restart loops.

**Links and monitors:**
- A **link** is bidirectional: if either process crashes, the other
  receives an exit signal (and crashes too, unless it traps exits)
- A **monitor** is unidirectional: the monitoring process receives a
  `DOWN` message when the monitored process exits, but is not killed

**Gleam integration:** Gleam's OTP library provides typed supervision
trees. The supervisor pattern is a regular Gleam module that BEAMR
runs like any other — no special VM support beyond links, monitors,
and exit signals.

### 8. Hot Code Loading

BEAM supports loading a new version of a module while the old version
is still running. This enables zero-downtime upgrades.

**How it works:**
1. The module registry holds up to two versions: `current` and `old`
2. Loading a new module: `old` is purged (processes running it are killed),
   `current` becomes `old`, new code becomes `current`
3. Local calls (`foo()`) stay on the same version
4. Fully qualified calls (`module:foo()`) switch to `current` version
5. A process running old code will switch to new code on the next
   fully qualified call

**For workflows:** This means you can update a workflow definition
while other workflows are running. The running workflow finishes on
the old code. New dispatches use the new code. No restart needed.

**Rust implementation:**
```
struct ModuleRegistry {
    modules: DashMap<Atom, ModuleVersions>,
}

struct ModuleVersions {
    current: Arc<Module>,
    old: Option<Arc<Module>>,
}
```

### 9. Native Function Interface

BEAMR exposes Rust functions to Gleam code as BIFs (Built-In Functions)
and NIFs (Native Implemented Functions).

**Registration:**
```rust
ream.register_nif("yggdrasil", "merge", 2, |args, process| {
    let source = args[0].decode_binary()?;
    let target = args[1].decode_binary()?;
    let result = libyggd::merge::structural_merge(&source, &target)?;
    Ok(Term::from_binary(result, &process.heap))
});
```

**Categories of native functions to expose:**

Yggdrasil operations:
- `yggdrasil:branch_create/2`, `branch_merge/2`, `branch_restack/1`
- `yggdrasil:tree_add/3`, `tree_children/1`, `tree_status/1`
- `yggdrasil:merge/2`, `merge_check/2` (AST-aware)
- `yggdrasil:graph_affected/2`, `graph_module_for_path/1`
- `yggdrasil:tracking_log/3`, `tracking_query/2`
- `yggdrasil:worktree_provision/2`, `worktree_teardown/1`

Git operations:
- `git:commit/2`, `git:push/2`, `git:fetch/1`
- `git:diff/2`, `git:status/1`, `git:log/2`
- `git:staging_add/2`, `git:staging_reset/1`

Meridian operations:
- `meridian:message_send/3`, `meridian:message_read/1`
- `meridian:workflow_dispatch/3`, `meridian:workflow_status/1`
- `meridian:storage_query/2`, `meridian:storage_write/3`

Filesystem:
- `fs:read_file/1`, `fs:write_file/2`, `fs:list_dir/1`
- `fs:watch/2` (returns a process that sends change messages)

**Long-running NIFs:** BEAM requires NIFs to return quickly (or the
scheduler stalls). For operations that take time (git push, cargo
build), use dirty schedulers — dedicated OS threads for long-running
work that don't block the normal scheduler.

### 10. I/O and Ports

BEAM handles I/O through ports — OS-level pipes to external processes.
For BEAMR, the primary I/O patterns are:

- **File I/O:** Async via dirty schedulers or io_uring integration
- **Network I/O:** TCP/UDP via kqueue/epoll (or Rust's mio)
- **Process I/O:** stdin/stdout/stderr of spawned OS processes
- **Timer:** Timer wheel for `receive after` and periodic events

Gleam's stdlib uses these for file operations and TCP. BEAMR would
implement the minimum set needed by Gleam's `gleam/erlang`,
`gleam/otp`, and `gleam/io` modules.

---

## What We Implement vs. What We Skip

### Implement (Core)

| Component | Priority | Complexity | Notes |
|-----------|----------|------------|-------|
| .beam loader | P0 | Low | Well-documented format, `beam_file` crate exists |
| Term representation | P0 | Medium | Tagged 64-bit words, heap allocation |
| Bytecode interpreter | P0 | High | ~170 opcodes, but many are variants |
| Process model | P0 | Medium | Struct with heap, stack, mailbox |
| Scheduler | P0 | High | Work-stealing, reduction counting |
| Per-process GC | P0 | High | Generational copying collector |
| Message passing | P0 | Medium | Lock-free mailbox, term copying |
| Pattern matching | P0 | Medium | BEAM compiles patterns to instructions |
| BIF/NIF registration | P0 | Low | Function pointer table |
| Links and monitors | P1 | Medium | Exit signal propagation |
| Supervision trees | P1 | Medium | OTP behaviour in Gleam, but VM needs links/monitors |
| Dirty schedulers | P1 | Medium | Separate thread pool for long NIFs |
| Hot code loading | P2 | Medium | Two-version module registry |
| ETS (basic) | P2 | Medium | Concurrent term storage, use dashmap |
| Timer wheel | P1 | Low | For `receive after` and timeouts |
| Binary handling | P0 | Medium | BEAM has sophisticated binary matching |

### Skip (Not needed for Gleam workflows)

| Component | Reason |
|-----------|--------|
| Distribution protocol | No clustering needed — single-node |
| Port drivers | Use NIFs instead |
| Legacy Erlang modules | Target Gleam stdlib only |
| BEAM JIT (OTP 24+) | Interpreter is fine for workflow scripts |
| Tracing/debugging BIFs | Add incrementally if needed |
| `dets` / `mnesia` | Use Rust storage (SQLite, Meridian storage) |
| Dialyzer types | Gleam has its own type system |
| `erl_interface` / `ei` | No Erlang node interop needed |
| Code purging callbacks | Simplified hot code loading |

---

## Key BEAM Concepts — Quick Reference

**Reduction:** One unit of work, roughly one function call. Each
process gets a budget (default 4000). When exhausted, the process
yields to the scheduler. This is what makes BEAM preemptive.

**Preemptive scheduling:** Unlike Go goroutines (cooperative,
yield at channel ops) or tokio tasks (cooperative, yield at .await),
BEAM processes are preempted at reduction boundaries. A process
running a tight loop of pure computation WILL be preempted. No
process can starve others.

**Fault isolation:** Each process has its own heap. One process
crashing (division by zero, pattern match failure, explicit exit)
only affects that process and its linked processes. The rest of
the system is unaffected. This is the "let it crash" philosophy.

**Let it crash:** Don't write defensive code. Let processes crash
on unexpected input. The supervisor restarts them. This leads to
simpler code that handles the happy path, with fault recovery
handled structurally (supervision trees) rather than inline
(try/catch everywhere).

**Mailbox:** Each process has an ordered queue of received messages.
`receive` pattern-matches against the mailbox in order. Unmatched
messages stay in the mailbox. This is "selective receive" — a
process can choose which messages to handle now and which to defer.

**OTP behaviours:** Standard patterns (gen_server, supervisor,
gen_statem) that encode common process lifecycle patterns. In Gleam,
these are provided by the `gleam_otp` library as typed wrappers.
BEAMR doesn't need to implement behaviours — they're Gleam library
code that runs on the VM like any other code.

---

## Validation Gates

The implementation is validated in stages. Each gate must pass before
the next begins. No gate is optional.

**Gate 1 — Bytecode execution.** Load a `.beam` file produced by the
Gleam compiler. Execute a pure Gleam module (fibonacci, list
operations, pattern matching). Every opcode the module uses must
execute correctly. Validate term representation, heap allocation,
and the interpreter loop.

**Gate 2 — Process model.** Spawn processes and pass messages between
them. Validate: process creation, mailbox delivery, selective receive,
process exit and cleanup. A process that crashes must not leak heap
memory or leave stale entries in the scheduler's queues.

**Gate 3 — Scheduler.** Reduction-based scheduling across multiple OS
threads. Validate: preemptive yielding (a tight loop yields after N
reductions), work stealing (idle schedulers take from busy ones),
priority queues (high-priority processes run before normal). No
process starvation under load.

**Gate 4 — Garbage collection.** Per-process generational copying GC
triggered by allocation pressure. Validate: minor GC (nursery to old),
major GC (full compaction), GC does not affect other processes, GC
correctly updates all term references (stack, registers, mailbox).

**Gate 5 — Native function interface.** Register a Rust function and
call it from Gleam. Validate: argument passing (terms decoded
correctly), return value (term encoded correctly), error propagation
(Rust error becomes process exit reason). Long-running NIFs run on
dirty schedulers without blocking normal schedulers.

**Gate 6 — Fault isolation.** Links and monitors. A linked process
receives exit signals. A monitoring process receives DOWN messages.
Supervisor strategies (one_for_one, one_for_all, rest_for_one) work
via Gleam's OTP library running on BEAMR. Validate: a crashed child
is restarted, restart limits are enforced, supervisor escalation
propagates correctly.

**Gate 7 — Hot code loading.** Load a new version of a module while
the old version is running. Validate: local calls stay on the old
version, fully qualified calls switch to the new version, old version
is purged when no process references it.

Each gate produces a test suite that becomes a regression gate for
all subsequent work.

---

## Research Pointers

**BEAM internals:**
- *The BEAM Book* (happi.github.io/theBeamBook) — the definitive
  guide to BEAM internals, covers everything from bytecode to
  scheduler to GC
- Erlang/OTP source: `erts/emulator/beam/` — the C implementation,
  particularly `beam_emu.c` (interpreter), `erl_gc.c` (GC),
  `erl_process.c` (scheduler)
- Robert Virding's talks on BEAM internals (original BEAM co-author)

**Existing implementations:**
- **AtomVM** (github.com/atomvm/AtomVM) — minimal BEAM in C for
  embedded/IoT, ~30k lines, MIT. Best reference for a minimal
  implementation. Supports a subset of Erlang.
- **Firefly/Lumen** (github.com/lumen/lumen) — attempted native
  compilation of BEAM via LLVM, largely abandoned. Architecture
  docs are valuable.
- **Lunatic** (github.com/lunatic-solutions/lunatic) — WASM runtime
  with BEAM-style process model, written in Rust. Not a BEAM
  interpreter but the process/scheduler architecture is relevant.

**Bytecode format:**
- `beam_file` Rust crate — existing `.beam` parser
- Erlang `beam_lib` module docs — format specification
- `beam_disasm` module — for verifying instruction decoding

**Gleam specifics:**
- Gleam compiler source (github.com/gleam-lang/gleam) — Rust,
  shows exactly what `.beam` output Gleam produces
- Gleam OTP library — supervision trees, actors, typed processes
- Gleam stdlib — the minimum set of BIFs needed

**Scheduler design:**
- Go runtime scheduler source — similar work-stealing design
- Tokio work-stealing scheduler — Rust reference for the pattern
- BEAM scheduler docs in *The BEAM Book* chapter 8

**GC design:**
- *The BEAM Book* chapter 11 — generational copying GC
- Immix GC paper — alternative considered by Firefly
- Erlang process heap management (`erl_gc.c`)

---

## Crate Structure

```
crates/
  beamr/                    # The VM
    src/
      lib.rs               # Public API: Vm::new(), load_module(), spawn()
      atom.rs              # Global atom table (interned strings)
      beam/                # .beam file loading
        loader.rs          # Chunked format parser
        decode.rs          # Bytecode instruction decoder
        validate.rs        # Instruction validation
      term.rs              # Tagged term representation
      heap.rs              # Process heap + allocator
      gc.rs                # Generational copying GC
      process.rs           # Process struct + lifecycle
      mailbox.rs           # Lock-free message queue
      scheduler/
        mod.rs             # Scheduler thread pool
        run_queue.rs       # Priority queues
        steal.rs           # Work stealing logic
      interpreter.rs       # Bytecode execution loop
      module.rs            # Module registry + hot code loading
      bif.rs               # Built-in function registry
      nif.rs               # Native function interface
      supervisor.rs        # Supervision tree support
      timer.rs             # Timer wheel
      dirty.rs             # Dirty scheduler thread pool
      io.rs                # Basic I/O operations
      error.rs             # VM error types
```

---

## Integration with Yggdrasil

A Gleam workflow running on BEAMR:

```gleam
import yggdrasil/branch
import yggdrasil/graph
import yggdrasil/pipeline
import gleam/otp/actor
import gleam/otp/supervisor

pub fn run_orchestrated_dev(brief: String) -> Result(Nil, String) {
  // All of these call into Rust via registered NIFs
  let affected = graph.affected_modules(brief)
  let branch_name = branch.create("feature/" <> brief)

  // Implement
  use result <- pipeline.run_step("implement", brief)

  // Run checks — scoped to affected modules only
  use check_result <- pipeline.run_checks(affected)

  case check_result.passed {
    True -> {
      branch.commit(branch_name, "feat: " <> brief)
      branch.push(branch_name)
      Ok(Nil)
    }
    False -> {
      // Loop back — supervisor handles restart
      Error("checks failed: " <> check_result.summary)
    }
  }
}
```

The Gleam code is clean, typed, and expressive. The heavy lifting
(git operations, AST merge, dependency graph) happens in Rust via
zero-cost NIF calls. The BEAM process model handles concurrency,
fault isolation, and supervision. No IPC, no serialisation overhead,
no separate process to manage.
