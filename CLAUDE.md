# beamr — Agent Instructions

Instructions for all agents working in this repo: Claude Code, norn, and any future agent runtime.

## Project

beamr is a ground-up BEAM virtual machine in Rust targeting Gleam bytecode. Standalone engine — no dependency on Meridian. Will run workflows in Meridian v2 long-term but compiles and runs independently.

Read [GOVERNANCE.md](GOVERNANCE.md) for principles, rules, and process. Read [README.md](README.md) for repo navigation.

## Architecture constraints

These are non-negotiable. Violating any of these is a blocking review finding.

- **Term = raw u64 with low-bit tagging.** Not a Rust enum. Not NaN-boxing. See ADR-004.
- **No async in the hot path.** Scheduler and interpreter are synchronous. No Tokio in reduction loops. See ADR-003.
- **No `.unwrap()` / `.expect()` / `panic!()` outside `#[cfg(test)]`.** Explicit error types everywhere.
- **No `unsafe` without ADR justification.**
- **No file over 500 lines.** Split before you hit the limit.
- **No Meridian/yggdrasil/norn dependencies in `crates/beamr/`.** The core VM crate depends on nothing external.
- **Import-driven BIF discipline.** Only build BIFs that Gleam workflows actually import.
- **Gleam-emitted opcodes only.** Per ADR-005 — implement what the Gleam compiler emits, not the full BEAM instruction set.
- **Copy semantics for message passing.** Per ADR-008 — deep copy between process heaps.

## Code conventions

- Crate/module headers: `//!` (inner doc comments)
- Item docs: `///` (outer doc comments), no blank line between doc and item
- `BEAM:` comment prefix for non-obvious BEAM semantics
- Tests in `#[cfg(test)] mod tests` block within the same file
- Conventional commits: `type(scope): description`

## Crate structure

```
crates/
  beamr/          # Core VM — Tom's team owns. All stubs until B-001..B-008 land.
    src/
      atom.rs     # Atom table (B-002)
      bif.rs      # BIF registry (B-008)
      gc.rs       # Garbage collector (B-017)
      heap.rs     # Process heap (B-010)
      lib.rs      # Crate root
      loader.rs   # .beam parser (B-003)
      mailbox.rs  # Mailbox + selective receive (B-011)
      module.rs   # Module registry (B-004)
      opcode.rs   # Opcode definitions (B-021)
      process.rs  # Process struct (B-010)
      scheduler.rs # Work-stealing scheduler (B-012)
      stack.rs    # Process stack (B-010)
      term.rs     # Tagged u64 terms (B-005, B-006, B-007)
      vm.rs       # VM entry point
  beamr-cli/      # CLI — bob's team owns. Arg parsing done, execution stubbed.
    src/
      main.rs     # 479 lines, 33 tests
```

## Working with this repo

### Branch strategy

- `main` — stable, reviewed code. Both teams merge here.
- `josh/dev` — bob's team working branch. PRs from here to main.
- Tom's team branches directly from main.

### Before writing code

1. Check which brief you're implementing (B-NNN)
2. Read the brief at `docs/design/beamr/briefs/B-NNN.json`
3. Check dependencies — don't start if prerequisite briefs aren't on main
4. Read the architecture doc if one exists at `docs/architecture/`

### After writing code

1. `cargo check -p <crate>` must pass
2. `cargo clippy -p <crate> --no-deps -- -D warnings` must pass
3. `cargo test -p <crate>` must pass
4. Every R# acceptance criterion must be met
5. No file over 500 lines

### Review process

Code goes through two independent reviewers:
- **Swarm** — correctness: R# compliance, acceptance criteria, checklist items
- **Dame Lisette** — quality: conventions, safety, test coverage, wiring

Both must approve before merge. See [WORKFLOW.md](docs/governance/WORKFLOW.md).

## Escalation

```
You (agent) → bob of dylan (directional lead) → bearup (project owner)
```

bob makes all technical and coordination decisions autonomously. Escalate to bearup only for scope expansion, external-facing decisions, or irreconcilable disagreements.

## Key references

| Document | Purpose |
|----------|---------|
| [GOVERNANCE.md](GOVERNANCE.md) | Principles, rules, roles, process |
| [README.md](README.md) | Repo navigation, architecture overview |
| [WORKFLOW.md](docs/governance/WORKFLOW.md) | Development pipeline, dispatch commands |
| [QUALITY-GATES.md](docs/governance/QUALITY-GATES.md) | Gate stages, evidence requirements |
| [ARTIFACT-SCHEMAS.md](docs/governance/ARTIFACT-SCHEMAS.md) | Document shapes |
| [COMPONENT-TRACKER.md](docs/governance/COMPONENT-TRACKER.md) | Component status |
| [IN-FLIGHT.md](docs/governance/IN-FLIGHT.md) | Current work per team |
| [WORKFLOW-RUNBOOK.md](docs/WORKFLOW-RUNBOOK.md) | Dispatch commands, failure decoders |

## 21 briefs

Tom authored all 21 implementation briefs (B-001 through B-021) covering 6 validation gates:

| Gate | Briefs | Scope |
|------|--------|-------|
| 1 | B-001..B-009 | Foundation: errors, atoms, loader, imports, terms, BIFs, CLI |
| 2 | B-010..B-012 | Processes, mailbox, scheduler |
| 3 | B-013..B-016 | Interpreter: guards, send/recv, closures, binaries |
| 4 | B-017..B-018 | GC, supervision |
| 5 | B-019..B-020 | Priority scheduler, reduction hook + timers |
| 6 | B-021 | Interpreter execution loop |
