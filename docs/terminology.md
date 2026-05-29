# Terminology

Shared vocabulary for beamr. If a term is used in a brief, a design doc, a
review, or a conversation — this is what it means.

---

## Gleam Compilation Pipeline

This is worth pinning first because it's changed over time and people carry
different versions in their heads.

**Current pipeline (Gleam 1.x):**

```
Gleam source (.gleam)
    → Gleam compiler (written in Rust)
    → Erlang source (.erl)
    → erlc (the Erlang compiler, part of OTP)
    → .beam bytecode files
```

Gleam does **not** emit `.beam` bytecode directly. It compiles to Erlang
source code, and then the standard Erlang compiler (`erlc`) produces the
`.beam` files. This means `erlc` sits in the build pipeline — you need an
Erlang/OTP installation to compile Gleam for the BEAM target.

Gleam also has a JavaScript target (Gleam source → JS), but that's
irrelevant to beamr.

**Why this matters for us:** The `.beam` files we load are produced by
`erlc`, not by Gleam directly. The bytecode format is Erlang's, on
Erlang's release cadence. This is a more stable target than Gleam's own
intermediate output.

---

## BEAM & Erlang Terms

**BEAM** — Bogdan's Erlang Abstract Machine. The virtual machine that
executes compiled Erlang (and Gleam) code. It's the runtime, not the
language. Think of it as the JVM is to Java — the execution engine.

**ERTS** — Erlang Run-Time System. The full C implementation of the BEAM
plus all its supporting infrastructure (I/O, networking, distribution,
the NIF interface, etc.). ERTS is what ships with Erlang/OTP. We are not
reimplementing ERTS. We are implementing the BEAM's execution model only.

**OTP** — Open Telecom Platform. A collection of libraries, patterns, and
tools that ship with Erlang. Includes supervision trees, gen_server, the
application framework, and much more. In Gleam, a subset of OTP is
available through the `gleam_otp` library. OTP behaviours are library
code that runs on the VM — we don't implement them, we provide the
primitives they need (processes, links, monitors, exit signals).

**Atom** — An interned string. A name that stands for itself: `ok`,
`error`, `my_module`. Stored once in a global table and referred to by
index everywhere. Comparing two atoms is comparing two integers — not
walking two strings character by character. The atom table is the shared
vocabulary of the running system.

**Term** — Domain jargon, not the English word. In the BEAM, a "term" means
*any value the VM can hold*: integers, atoms, lists, tuples, binaries,
pids, function values — all terms. A term is a single 64-bit machine word.
Small values fit directly in the word (*immediates*). Larger values live
on the heap and the word holds a pointer (*boxed terms*). Tag bits in the
word tell you which kind it is.

**Immediate** — A term whose value fits entirely in one machine word. No
heap allocation, no pointer chasing. Small integers, atoms, pids, nil.
Fast because there's nothing to look up.

**Boxed term** — A term that lives on the heap. The machine word holds a
tagged pointer to heap-allocated data: tuples, lists, binaries, big
integers, floats, closures, maps, references. Note: "boxed" here is BEAM
terminology, not Rust's `Box<T>`. Same concept (pointer to heap data),
different system. Boxed terms are what the garbage collector cares about.

**Process** — A lightweight unit of execution inside the VM. Not an OS
thread — the operating system knows nothing about them. Each process has
its own heap, stack, mailbox, and reduction counter. Processes share no
memory. You can have hundreds of thousands of them. Spawning one costs
microseconds.

**Pid** — Process identifier. A value that names a specific process. Used
to send messages. An immediate term.

**Reduction** — One unit of work budget, roughly one function call. Each
process gets a budget (default 4000 reductions). The interpreter
decrements the counter on each function call. When it hits zero, the
process yields. A reduction is a unit of budget, not an event — "the
process used a reduction" means it spent one unit, not that it was
interrupted.

**Reduction boundary** — The moment a process yields: budget exhausted, or
blocking on a receive with no matching message. At this moment the
process is held still and the scheduler decides what happens next. This
is a standard BEAM concept. (See also: *reduction boundary hook*, which
is our addition.)

**Preemptive scheduling** — The scheduler forces processes to yield after
a fixed number of reductions, whether they want to or not. Contrast with
cooperative scheduling (Go goroutines, tokio tasks) where tasks only
yield at explicit points (`.await`, channel ops). A BEAM process running
a tight loop with no I/O WILL be interrupted. No process can starve
others.

**Mailbox** — Each process has an ordered queue of received messages.
Messages are terms, copied into the receiver's heap on delivery. The
copy ensures complete isolation — sender and receiver never share heap
objects.

**Selective receive** — When a process reads its mailbox, it pattern-matches
against the messages. It picks the one it wants and leaves the rest for
later. A save pointer tracks how far it has already scanned, avoiding
rescanning from scratch.

**Link** — A bidirectional bond between two processes. If either dies, the
other receives an exit signal (and dies too, unless it traps exits). Used
to make groups of processes share fate.

**Monitor** — A unidirectional watch. The monitoring process receives a
DOWN message when the monitored process dies, but is not killed itself.
Used when you care about something's fate but aren't bonded to it.

**Exit signal** — A notification that flows along links and monitors when a
process ends. Carries the reason: normal (finished cleanly), a crash
reason (something went wrong), or `kill` (forced termination).

**Trap exit** — A flag on a process. When set, exit signals from linked
processes arrive as messages in the mailbox instead of killing the
process. This is how supervisors survive their children's deaths to
restart them.

**Supervisor** — Not a special VM construct. An ordinary process that traps
exits, links to its children, and restarts them according to a strategy
when they crash. Supervision trees are library code (Gleam's `gleam_otp`)
running on top of the primitives we provide.

**Supervision strategies:**
- *one_for_one* — only restart the crashed child
- *one_for_all* — restart all children when any crashes
- *rest_for_one* — restart the crashed child and all started after it

**BIF** — Built-In Function. A function implemented in the VM itself (in
our case, Rust) rather than in Gleam/Erlang. Things like arithmetic,
list operations, type checks. **BIFs are our responsibility** — the VM
must provide them for bytecode to run.

**NIF** — Native Implemented Function. A function written in a native
language (Rust for us) and registered with the VM by the host. Called
from Gleam code as if it were a normal function. **NIFs are Meridian's
responsibility** — they're the operations (git, AST merge, diagnostics)
plugged in from outside. The distinction matters: BIFs ship with the
engine, NIFs ship with whoever embeds it.

**Dirty scheduler** — A separate thread pool for native functions that
take a long time (a `git push`, a `cargo build`). The name is BEAM
terminology, not a judgement — "dirty" means "not bound by the normal
scheduler's fairness rules." Long-running work goes here so normal
scheduler threads stay free and fair for everyone else.

**Work stealing** — When a scheduler thread's run queue is empty, it takes
processes from a busier thread's queue. Load balances itself with no
central coordinator. A core is never idle while another core has a
backlog.

**.beam file** — The compiled bytecode format. A chunked binary container
with labelled sections: atom table, instruction bytecode, import table,
export table, literal table, etc. Produced by `erlc`. The format is
documented in the BEAM book and changes on Erlang's release cadence.

**Import table** — A section in the `.beam` file listing every external
function the module calls but doesn't define. `lists:map/2`, `erlang:+/2`,
etc. When the loader resolves these, any that remain unresolved tell us
exactly which BIFs and NIFs we need to implement next. (See also:
*import-table leash*.)

**MFA** — Module:Function/Arity. The standard way to identify a function.
`lists:map/2` means the function `map` in module `lists` that takes 2
arguments.

**Arity** — The number of arguments a function takes. In BEAM, functions
with the same name but different arities are completely different
functions.

---

## Beamr-Specific Terms

**beamr** — The project. A Rust runtime with the BEAM's execution model,
targeting Gleam as the primary source language. Not a BEAM clone, not a
port, not a binding. Implements the execution model — and only that.

**The one rule** — beamr depends on nothing of Meridian's. Meridian depends
on beamr. The core is self-contained. External operations plug in from
outside. Everything points inward.

**Import-table leash** — Our metaphor, not a BEAM term. The insight from
doc 02: the loader's unresolved-import report is a demand-driven work
queue that physically cannot grow beyond what the workflows need. We
implement a BIF only when a workflow we actually run needs it. The import
table is the scope constraint mechanism that keeps the project finishable.

**Reduction boundary hook** — Our addition, not a BEAM concept. At the
reduction boundary (when a process yields), the runtime can inspect what
just happened and decide whether to intervene before the process resumes.
Same primitive as norn's tool boundary and norn-memory's resonance, but
at the lowest altitude — the interpreter loop itself. Diagnostics become
a live conscience, not a post-mortem. (See doc 11.)

**Native boundary** — The interface where a Gleam function call crosses
into Rust. Specifically: the registry where Rust functions are registered
under names the bytecode can call. "Native functions" are what cross the
boundary; "the native boundary" is the interface itself. The key property:
because our Rust IS the runtime, this is a plain function call within one
program — no C interface, no serialisation, no IPC. (See doc 10.)

**Validation gate** — A stage of implementation that must pass before the
next begins. Each gate has a defined scope and produces a regression test
suite that never goes away. Gates 1-6 are core scope; gate 7 (hot code
loading) is aspirational.

**Proof of life** — The first moment something actually runs. Not correct,
not complete — just alive. Load a `.beam` file and execute one function.
The baseline that proves the architecture works.

---

## Gleam Terms

**Gleam** — A typed functional language that compiles to Erlang and
JavaScript. Statically typed with full type inference. The compiler is
written in Rust. We use it as the workflow language because it gives us
compile-time type safety, pattern matching with exhaustiveness checking,
and the pipeline operator.

**Pipeline operator** (`|>`) — Passes the result of the left side as the
first argument to the right side. `x |> f |> g` means `g(f(x))`. Makes
data flow read top-to-bottom instead of inside-out.

**Result type** — `Result(value, error)`. Gleam's way of handling success
and failure. Every function that can fail returns a Result. The compiler
forces you to handle both cases. No exceptions, no try/catch, no
swallowed errors.

**Use expression** (`use`) — Syntactic sugar that flattens nested
callbacks. Makes `result.try(fn(x) { ... })` chains read linearly
instead of deeply indented.

**External function** (`@external`) — Gleam's FFI declaration. Tells the
compiler "this function exists in Rust (or Erlang, or JavaScript) —
here's where to find it." How Gleam workflows call into beamr's native
operations.

**gleam_otp** — Gleam's OTP library. Typed wrappers around supervision
trees, actors, and typed processes. Runs as ordinary Gleam code on the
VM. We don't implement it; we provide the primitives it needs.

**gleam_erlang** — Gleam's Erlang interop library. Access to Erlang-level
features (processes, atoms, etc.) from Gleam.

---

## Development Terms

How our team talks about the work. Standardised so nobody's guessing.

**Brief** — A concrete, practical set of instructions for an implementation
task. Clear enough to act on, not so prescriptive it becomes dictation.
Defines what to build, how to know it's done, and what the acceptance
criteria look like.

**Design doc** — Captures the spirit and intention behind a piece of work.
The why, the constraints, what the thing should feel like when it's right.
Not implementation details — the shape and the reasoning.

**Review** — A methodical, end-to-end examination of completed work. Traces
every flow, checks every surface, verifies that what was built matches
what was asked for. Not a skim — a full trace.

**Big-picture gate** — The final check before something lands. Do the
pieces fit together? Is it practical? Can someone actually use what we
produced? Does it make sense in the context of the whole system?

**Scaffold** — Stub code that establishes structure (modules, types,
function signatures) without implementing behaviour. Used to agree on
shape before filling in logic.

**Regression gate** — A test suite produced by a validation gate that must
continue passing through all subsequent work. Gates only accumulate.

**Demand-driven** — Building only what the workflows actually need, as
proven by the loader's unresolved-import report. Opposite of speculative
implementation. The project's core discipline.
