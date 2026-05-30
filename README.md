# beamr

A ground-up BEAM virtual machine in Rust, targeting Gleam bytecode.

beamr gives Meridian's workflows the execution model that makes the BEAM the best concurrent runtime ever built: preemptive fairness, per-process fault isolation, supervision trees, and a native function interface where Rust operations are zero-cost calls inside the VM.

## Why this exists

Workflow definitions today are either YAML (no types, no composition, no tests) or dynamically typed scripts (runtime errors 45 minutes into a run). Rust's async ecosystem cannot provide preemptive fairness, per-task fault isolation, or supervision. The BEAM has all of these, but it's written in C with no clean Rust embedding story.

beamr fills the gap: a Rust runtime with the BEAM's execution model, scoped to what Gleam workflows actually need, where native Rust operations are first-class citizens inside the VM.

## Status

**Planning phase.** Architecture research, design docs, and implementation briefs are complete. Core crate is scaffolded (37 source files, module structure in place). CLI argument parsing is implemented (33 tests). No VM internals implemented yet.

## Repository map

```
beamr/
├── README.md                          <- you are here
├── docs/
│   ├── design/beamr/
│   │   ├── DESIGN.md                  <- master design document
│   │   ├── CHECKLIST.md               <- 176-item implementation checklist
│   │   ├── checklist.json             <- machine-readable checklist
│   │   ├── USER-STORIES.md            <- 30 user stories (4 personas)
│   │   ├── stories.json               <- machine-readable stories
│   │   └── briefs/
│   │       ├── B-001.json .. B-021.json  <- implementation briefs (JSON)
│   │       └── B-001.md  .. B-021.md     <- rendered brief markdown
│   ├── adr/
│   │   ├── README.md                  <- ADR index
│   │   └── 001..011                   <- 11 architecture decision records
│   ├── architecture/                  <- BEAM analysis + component architecture
│   │   ├── 00-beam-alternatives-survey.md
│   │   └── (01-NN per component, in progress)
│   └── governance/                    <- quality gates, artifact schemas, tracking
├── crates/
│   ├── beamr/                         <- the VM (core crate)
│   │   └── src/
│   │       ├── lib.rs                 <- module declarations only
│   │       ├── error.rs               <- crate error types
│   │       ├── atom/                  <- global atom table
│   │       ├── loader/                <- .beam file parser + decoder
│   │       ├── term/                  <- tagged 64-bit term representation
│   │       ├── process/               <- process struct, heap, stack
│   │       ├── interpreter/           <- bytecode execution loop
│   │       ├── scheduler/             <- thread pool + work stealing
│   │       ├── gc/                    <- per-process generational GC
│   │       ├── mailbox/               <- MPSC + selective receive
│   │       ├── supervision/           <- links, monitors, exit signals
│   │       ├── native/                <- BIF/NIF registries
│   │       ├── hook.rs                <- reduction-boundary hook
│   │       ├── module.rs              <- module registry
│   │       └── timer.rs               <- timer wheel
│   ├── beamr-cli/                     <- thin runner for development
│   │   └── src/main.rs               <- argument parsing, execution pipeline
│   └── beamr-meridian/                <- (future) Meridian integration
└── .meridian/
    ├── workflows/                     <- norn workflow definitions
    ├── profiles/                      <- norn agent profiles
    └── tasks/                         <- task dispatch queue
```

## How to read this repo

1. **Start here** -- this README for purpose and map
2. **Design intent** -- `docs/design/beamr/DESIGN.md` for what beamr is, why, and how
3. **Decisions** -- `docs/adr/` for the 11 architectural choices and their reasoning
4. **What to build** -- `docs/design/beamr/briefs/` for the 21 implementation briefs
5. **Validation** -- `docs/design/beamr/CHECKLIST.md` for the 176-item checklist
6. **User perspective** -- `docs/design/beamr/USER-STORIES.md` for the 30 user stories
7. **BEAM analysis** -- `docs/architecture/` for deep analysis of BEAM internals and alternatives
8. **Code** -- `crates/` for the Rust implementation

## Architecture overview

beamr is a Rust workspace with a core VM crate (`beamr`) and a CLI crate (`beamr-cli`), implementing the BEAM execution model:

- **Atom table** -- global interned string table (lock-free concurrent map)
- **Loader** -- reads `.beam` files, decodes bytecode, resolves imports
- **Terms** -- tagged 64-bit values (low-bit tagging, not NaN-boxing)
- **Processes** -- unit of execution and isolation (own heap, stack, mailbox)
- **Interpreter** -- fetch-decode-execute loop with reduction counting
- **Scheduler** -- N OS threads with work-stealing run queues (no async)
- **GC** -- per-process generational copying collector
- **Mailbox** -- lock-free MPSC with selective receive
- **Supervision** -- links, monitors, exit signals (library, not VM machinery)
- **Native interface** -- BIF/NIF registry (demand-driven from import reports)
- **Reduction hook** -- the seam where Meridian observes every process yield

## Key principles

1. **beamr depends on nothing of Meridian's.** The VM is self-contained.
2. **Demand-driven scope.** Only build what workflows actually import.
3. **No silent failures.** Processes crash visibly; supervisors handle recovery.
4. **Per-process isolation is non-negotiable.** No shared mutable state.
5. **Fairness is guaranteed.** Reduction counting enforces preemptive yields.

## Key constraints

- No file over 500 lines
- No `.unwrap()` or `.expect()` outside `#[cfg(test)]`
- No async runtime in the scheduler hot path
- Term is raw `u64` with low-bit tagging (not a Rust enum)
- Zero dependencies on Meridian/Yggdrasil/Norn from the beamr crate

## Build pipeline

```
Research (BEAM source + alternatives)
  -> Architecture docs (pseudocode, no Rust)
    -> Implementation briefs (numbered requirements)
      -> Norn build (scout -> dev -> review)
        -> Code review (2 reviewers: correctness + quality)
          -> PR to main
```

## Team

| Role | Agent | Responsibility |
|------|-------|---------------|
| Directional lead | bob of dylan | Architecture calls, routing, quality gates |
| Brief writer | Haley Barrows | Implementation briefs |
| Design doc writer | Ms. Anastacio Streich | Purpose-focused design documents |
| Enforcer | Reverend Chaos | Rule enforcement, consistency |
| Code reviewer #1 | A Swarm of Bees | Correctness (brief compliance) |
| Code reviewer #2 | Dame Lisette Frami | Quality (conventions, safety, coverage) |
| Builder | Norn | All execution: analysis, builds, reviews |

## Implementation gates

| Gate | Scope | Briefs | Status |
|------|-------|--------|--------|
| 1 | Load + execute .beam (arithmetic, pattern matching) | B-001..B-009 | Briefed |
| 2 | Processes, mailbox, scheduler | B-010..B-012 | Briefed |
| 3 | Interpreter opcodes (guards, send/receive, closures, binary) | B-013..B-016 | Briefed |
| 4 | GC, supervision | B-017..B-018 | Briefed |
| 5 | Priority scheduling, dirty schedulers, hook, timers | B-019..B-020 | Briefed |
| 6 | Interpreter execution loop + foundational opcodes | B-021 | Briefed |

## Gleam compilation path

```
.gleam source -> Gleam compiler (Rust) -> .erl source -> erlc -> .beam bytecode -> beamr
```

beamr loads the `.beam` output. The bytecode format is stable on Erlang's release cadence.

## Quick start

```bash
cargo check -p beamr-cli       # build check (--workspace once core scaffold is clean)
cargo test -p beamr-cli        # run CLI tests (33 tests)
cargo run -p beamr-cli -- --help   # CLI usage
```
