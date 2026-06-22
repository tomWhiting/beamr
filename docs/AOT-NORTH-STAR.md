# beamr — Type-Directed AOT Native Compilation (North-Star)

> **Status: north-star / R&D direction, not near-term.** This document captures a long-range
> vision for beamr. The incremental on-ramp (type-directed JIT optimization) is already underway;
> the destination (whole-program AOT to a self-contained native binary) is a large build to walk
> toward, not a current priority. Recorded 2026-06.

## The core observation

Gleam is fully type-checked at compile time. But it compiles to BEAM bytecode, which is
dynamically typed — so **the type information is thrown away before execution.** Crucially, the
real BEAM can't recover it either. That is the opening: beamr can be the *only* runtime that
keeps Gleam's static types and exploits them.

There are two ideas here, one already begun and one a destination.

---

## Idea 1 — Retain the types and optimize with them (on-ramp, partly built)

Carry Gleam's compile-time types through to code generation and use them wherever available to
produce faster code. This is already started: the JIT consumes `.gleam_types` sidecars and does
type-directed specialization (`TypedRegisterState`, `jit/compiler/ir_typed.rs`).

How far it can go:
- Unboxed integers / floats where the type is known.
- Eliding type-test guards the compiler has already proven.
- Direct record-field access instead of tagged-tuple indexing.
- Monomorphizing generic functions; devirtualizing calls.

Stacked, these move Gleam-on-beamr toward statically-compiled-functional-language performance —
and make beamr *faster than running the same Gleam on the real BEAM*, because BEAM discards the
types. This is the fundable, incremental path: every increment delivers value without committing
to the full journey below.

---

## Idea 2 — Whole-program AOT to a single tiny native binary (the destination)

Compile a Gleam program's bytecode (plus its retained types) ahead-of-time to native code, so the
shipped artifact has **no bytecode interpreter and no loader** — just native machine code, in one
small self-contained binary, with no VM to install.

### The one reframe that matters

You do **not** delete "the VM." What people call the BEAM VM is two things bundled:
1. the **bytecode interpreter / loader**, and
2. the **runtime** — lightweight processes, the preemptive scheduler, per-process GC, message
   passing, supervision, fault isolation.

A Gleam program that spawns processes and passes messages still needs (2) at runtime. AOT lets you
**delete (1) entirely** and **tree-shake (2)** down to only the parts the program actually uses,
statically linked. So the correct statement of the dream is:

> **"Compile typed Gleam to a single, tiny, self-contained native binary — no VM to install."**
> Not "no runtime at all" — you embed a minimal, tree-shaken runtime, with no interpreter.

The right industry analogy is **GraalVM Native Image**: it AOT-compiles JVM bytecode to a native
binary with no JVM install, but still bundles a minimal runtime (its own GC + thread support). Same
shape here.

### Why beamr is unusually positioned to do this

The closest prior attempt is **Lumen** (an alternative BEAM-in-Rust that aimed to AOT-compile
Erlang/Elixir to native and WASM). Well-known team, real funding — and it stalled. The lesson: it
tried to support *all* of Erlang/OTP, and reimplementing the entire runtime for the full language
is brutal.

beamr's advantages are exactly the things Lumen lacked:
- **Narrow, predictable, typed scope** — only what Gleam generates (ADR-005). Whole-program AOT is
  tractable on a small typed subset in a way it never is for all of Erlang/OTP. This is the unlock.
- **The hard machinery is already half-built**: a Cranelift JIT, GC stack maps, safepoints, deopt,
  AOT caching (`jit/aot.rs`), demand-driven BIF resolution (ADR-006), and an `embedded` module
  archive feature. AOT is "extend the JIT to compile the whole program ahead of time and link a
  minimal runtime," not a greenfield.
- **Already in Rust + Cranelift** — no new backend needed.

### Honest hard parts

- **The runtime features are the cost, not the codegen.** Preemptive scheduling (reduction counting
  across native calls), per-process GC over AOT'd stacks, message passing, supervision — all must
  work in native code, not just under the interpreter. The JIT's stack maps + safepoints are the
  hardest piece and already exist, but the JIT currently lowers ~51 of 66 instruction variants and
  falls back to the interpreter for the rest. True AOT needs 100% lowering, or a tiny embedded
  interpreter for cold cases.
- **Cranelift vs LLVM.** Staying on Cranelift keeps the toolchain simple and it's already
  integrated, but it optimizes less aggressively than LLVM — fast-but-not-maximal binaries. An
  optional LLVM backend is a "later, for max perf" knob, not a v1 need.
- **The process model is still required.** Green-thread scheduling + per-process heaps don't go
  away; the binary embeds them. (This is why "no VM" is "no VM *install*," not "no runtime.")

### Design note: skip "emit Rust source"

The intuition "turn the bytecode into Rust, then compile the Rust" is natural but probably the
wrong layer. Emitting Rust source and invoking `rustc` buys little and costs a lot: slow compiles,
a rustc dependency, and piles of `unsafe` Rust to model BEAM's dynamic semantics. beamr already has
a *lower, better* target — Cranelift. The clean pipeline is:

```
bytecode + types → enriched IR → Cranelift IR → native object → link (minimal runtime)
```

— which is what the JIT already does, just ahead-of-time instead of at load. "Make Rust from it"
really means "make native code from it," and Cranelift is the better layer. (The only reason to
emit Rust would be auditability of the output — not worth the toolchain cost for a backbone.)

---

## Why it matters (if/when built)

It would make beamr not "a BEAM VM in Rust" but **"a compiler that turns typed Gleam into tiny,
fast, self-contained native binaries with the actor model built in."** That compounds with the rest
of the Ablative thesis:

- **Fastest way to run Gleam** — exploiting types the real BEAM discards.
- **Supercharges embedding** — agent/workflow logic as a tiny native lib linked into a Rust host,
  zero VM-embedding overhead.
- **WASM / edge** — tiny AOT modules (Lumen's original goal; hot for edge compute).
- **Durable agents compiled to single native binaries** — a strong line for the durable-agents pitch.
- **Research / grant angle** — type-preserving AOT via Cranelift is novel enough for a paper or a
  Rust Foundation / Sovereign Tech / fellowship framing.

## Rough phasing (when it becomes a priority)

1. Push the **type-directed JIT** further (unboxing, guard elision, record field access,
   monomorphization) — value at every increment, no AOT commitment.
2. Get JIT instruction-lowering to **100%** (or define the minimal cold-path interpreter fallback).
3. **Tree-shake the runtime**: link only the scheduler/GC/BIFs a program uses (the `embedded` +
   demand-driven-BIF substrate already leans this way).
4. **Whole-program AOT**: compile all functions ahead of time, statically link the minimal runtime,
   emit one binary.
5. Optional **LLVM backend** for maximal performance; optional **AOT-to-WASM** for edge.

## Prerequisite reality check

This sits on top of a healthy core. The two known correctness bugs (cross-process `send` delivery
and `recv_marker` selective receive) and full opcode lowering are on the critical path for anything
that runs real concurrent programs — AOT inherits all of that. Fix the foundation first; the
north-star is the reward for it, not a way around it.
