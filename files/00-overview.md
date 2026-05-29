# bearmr — What We're Building (and Why)

*This folder is a set of conceptual guides — one per moving part. They're written
to be **understood**, not implemented-from. No struct fields, no signatures. Read
these first; the code comes later.*

> The name is **bearmr**. It is not REAM. Don't ask.

---

## The one-paragraph version

We are not rewriting the BEAM. We are stealing the single thing the BEAM is
better at than anything else on Earth — **preemptive, self-healing concurrency** —
and welding it to the language we actually want to write, Rust. The Erlang VM gives
you fairness (no task can hog a core), isolation (one crash doesn't sink the ship),
and supervision (the system heals itself). Those are exactly the properties a
workflow-and-agent engine needs, and they're exactly the properties Rust's async
ecosystem still can't give you cleanly. So we build a Rust runtime with the BEAM's
*execution model* — and only that. Not its phone-network heritage, not its 30 years
of stdlib, not distribution. The good bits.

## Why this isn't a toy

The thing that makes it worth the effort is buried and easy to miss, so it goes
first: **when your Rust *is* the VM, your own operations stop being foreigners.**
On the real BEAM, calling Rust means writing a NIF that has to behave across a C
boundary — return in a millisecond or you wreck the scheduler, crash and you take
down the whole node. In bearmr, your git operations, your AST-merge, your
diagnostics checks are *plain function calls inside the runtime*, and **you own the
counter that decides when a process yields.** That ownership is the whole game. It's
what lets the conventions pipeline tap an agent on the shoulder *mid-execution*
instead of after it's already written the file. (See `11-reduction-boundary-hook`.)

## The shape of the system

Eleven bits, in the order they make sense to learn:

1. **Atoms** — the shared vocabulary everything is named in.
2. **The Loader** — reading compiled Gleam off disk.
3. **Terms** — what all data is made of.
4. **Processes** — the unit of life and isolation.
5. **The Interpreter & Reductions** — execution, and the heartbeat of fairness.
6. **The Scheduler** — fairness spread across every core.
7. **Memory & Garbage Collection** — why each process cleans its own room.
8. **Messages & Mailboxes** — the only way processes touch each other.
9. **Supervision** — links, monitors, and "let it crash."
10. **The Native Boundary** — how Gleam reaches into your Rust.
11. **The Reduction-Boundary Hook** — the bit that's uniquely ours.

And `12-crate-structure` is the map of where all of it lives on disk.

## What we are deliberately NOT building

Hot code loading. Distributed Erlang. ETS / Mnesia. The full OTP stdlib. The JIT.
The long tail of built-in functions. We implement a built-in *only when a workflow
we actually run needs it* — and there's a mechanism that enforces that discipline
rather than relying on willpower (see the loader doc). Skipping these is not
laziness; it's the entire reason this is finishable by two people instead of a
graveyard like the projects below.

## What the people who came before teach us

- **enigma** (by the author of the Helix editor) got far enough to run Elixir and a
  REPL. It stalled for lack of *contributors*, not lack of feasibility. Lesson: the
  wall is attrition and the BIF long tail, not the VM. Stay scoped and you don't hit
  it.
- **AtomVM** runs real `.beam` bytecode on microcontrollers in a tiny codebase.
  Lesson: "small but genuinely real" is a proven place to stand — our best minimal
  reference.
- **Lunatic** built the BEAM's process model in Rust on top of Wasmtime, using its
  *fuel* mechanism for preemption. Fuel is reduction-counting under a different
  name. Lesson: the model demonstrably works in Rust, and the preemption trick is
  sound.
- **ErlangRT**'s author summed up the trap: "I know what to do; I have no clue how
  to Rust." The two halves of this — knowing the BEAM and knowing Rust — rarely live
  in one head. Between us, they do.
