# Governance

How beamr is built, reviewed, merged, and maintained. This is the authoritative reference. All agents and contributors follow this document. Detailed operational docs live in `docs/governance/` and are referenced here.

## 1. Identity

beamr is a ground-up BEAM virtual machine in Rust targeting Gleam bytecode. It will run workflows in Meridian v2 long-term, but is a standalone engine with no dependency on Meridian. It can be compiled into the Meridian binary or run independently.

## 2. Principles

Organised by the Telos four-layer classification. First-class principles are load-bearing for the project's identity. Second-class principles guide daily work. Third-class principles are preferences that can be revised without restructuring.

### Philosophical (why we build this way)

- **P1. Spec before code (first-class).** Every line of code traces to a brief requirement (R#). Code without a spec is unanchored. Briefs are the contract between intent and implementation.
- **P2. Independent verification (first-class).** The builder never judges its own output. Separate agents review with adversarial incentives — one optimises for correctness, one for quality. Trust is earned through evidence, not assertion.
- **P3. Quality over speed (first-class).** No shortcuts through gates. A component that takes three passes to land correctly is preferable to one that lands fast and rots. The cost of rework compounds; the cost of rigour does not.
- **P4. Decisions are immutable records (second-class).** ADRs capture the reasoning at decision time. Never edit an accepted ADR — supersede it with a new one. Future contributors need to see what was decided and why, not a revised history.
- **P5. Standalone by design (first-class).** beamr depends on nothing from Meridian. Import-driven BIF discipline — only build what workflows actually use. The VM must compile and run without any Meridian crate in the dependency tree.

### Architectural (how the system is structured)

- **A1. Term is raw u64 (first-class).** Low-bit tagging per ADR-004. Not a Rust enum, not NaN-boxing. This decision is load-bearing for every component that touches values.
- **A2. No async in the hot path (first-class).** The scheduler and interpreter are synchronous. No Tokio, no async/await in reduction loops. Per ADR-003.
- **A3. No source file over 500 lines (second-class).** Split when approaching the limit. Applies to `crates/*/src/**/*.rs` — enforced by CI file-size gate. Does not apply to docs, markdown, or JSON.
- **A4. Copy semantics for message passing (second-class).** Per ADR-008. Messages are deep-copied between process heaps. No shared-heap optimisation until profiling justifies it.
- **A5. Gleam-emitted opcodes only (second-class).** Per ADR-005. We implement the opcode subset that the Gleam compiler actually emits, not the full BEAM instruction set.

### Methodological (how work gets done)

- **M1. Norn executes, team directs (first-class).** All analysis, builds, and first-pass reviews are executed by norn. Team agents coordinate, review, and make decisions. No agent writes code directly.
- **M2. Both reviewers approve before merge (first-class).** Two independent reviewers with different lenses (correctness and quality). No merge before both sign off. No exceptions without escalation.
- **M3. Mechanical enforcement over manual discipline (second-class).** Where possible, encode rules in CI, linters, and automated checks rather than relying on reviewer memory. CODEOWNERS, branch protection, cargo clippy -D warnings.
- **M4. Brief-per-component (second-class).** Each component gets its own implementation brief. Briefs are the unit of work, review, and tracking. No brief spans multiple components.
- **M5. Land before dependents (second-class).** Each brief's code must be on main before any brief that depends on it is dispatched. Worktrees branch from main.

### Ethical (how we treat the work and each other)

- **E1. No silent failures (first-class).** If a gate fails, document what failed and why. If a norn run produces partial output, report what was dropped. Silent truncation reads as "covered everything" when it didn't.
- **E2. Grounded claims only (first-class).** Architecture docs cite sources (file:line for BEAM source, URLs for alternatives). Research that asserts without evidence is incomplete. Reviewers flag uncited claims.
- **E3. Transparent scope boundaries (second-class).** Each team knows what the other is working on. IN-FLIGHT.md is updated with each PR. No team silently modifies another's scope.
- **E4. Escalation is not failure (third-class).** Reaching the retry limit and escalating to bob or bearup is the process working correctly, not a sign of incompetence.

## 3. Rules

Operational constraints that apply at all times.

### Code rules

- No `.unwrap()` or `.expect()` outside `#[cfg(test)]`
- No `panic!()` outside `#[cfg(test)]`
- No `unsafe` without explicit justification and ADR
- No file over 500 lines
- Crate/module headers use `//!`, item docs use `///`, no blank line between doc and item
- `BEAM:` comment prefix for non-obvious BEAM semantics
- Tests in `#[cfg(test)] mod tests` block within the same file
- Conventional commits for all commit messages

### Process rules

- Every PR to main requires CI green (cargo check + clippy + test)
- Every code PR requires two reviewer approvals (correctness + quality)
- Maximum 2 retry cycles per gate before escalating
- Brief checklist IDs must not be double-claimed across briefs
- Architecture research precedes implementation for every component

### Repo rules

- `josh/dev` is the permanent working branch for bob's team
- PRs go from josh/dev to main
- Tom's team pushes directly to main or their own branches
- No force push to main
- No merge before review

## 4. Roles and Authority

| Agent | Role | Authority |
|-------|------|-----------|
| bearup | Project owner | Final authority (doomsday resort). Governs, ensures bob has what he needs. Does not make daily decisions. |
| bob of dylan | Directional lead | Decision maker for all technical and coordination matters. Last resort before bearup. Owns workflow, reviews architecture docs, creates PRs. |
| Haley Barrows | Brief writer | Writes and updates implementation briefs. |
| Ms. Anastacio Streich | Design doc writer | Architecture and design documents. Telos vault findings. |
| Reverend Chaos | Enforcer | Rule enforcement, brief validation, consistency checks. |
| A Swarm of Bees | Code reviewer #1 | Correctness review: R# compliance, acceptance criteria, checklist, stories. |
| Dame Lisette Frami | Code reviewer #2 | Quality review: conventions, safety, test coverage, module boundaries, wiring. |
| Norn | Builder | Executes all analysis, builds, and first-pass self-review. Headless CLI. |

### Escalation path

```
Team agent → bob of dylan → bearup
```

bob resolves all issues autonomously. bearup is consulted only for:
- Scope expansion beyond the current gate
- External-facing decisions (repo settings, CI config, access control)
- Irreconcilable disagreements between team members
- Decisions that permanently constrain future architecture

### Code ownership

| Path | Owner | Reviewers |
|------|-------|-----------|
| `crates/beamr/` | Tom's team | Swarm + Dame Lisette |
| `crates/beamr-cli/` | bob's team | Swarm + Dame Lisette |
| `docs/governance/` | bob's team | Reverend Chaos |
| `docs/architecture/` | bob's team (via norn) | bob |
| `docs/design/beamr/briefs/` | Tom (authored), Reverend Chaos (validates) | -- |
| `docs/adr/` | Tom (authored) | bob |
| `.github/workflows/` | bob's team | Dame Lisette |
| `scripts/` | bob's team | Dame Lisette |

### Agent spawn protocol

When assigning any agent to a project-specific role, **immediately** set their Focus text:

```bash
collective member update "<member name>" \
  --focus "<role, lens, repo path, key rules, manager>"
```

Focus persists across compactions. Without it, an agent that compacts wakes with no role context and defaults to whatever their prior scope was. This is the standard failure mode for project-specific agent roles.

Check current Focus:
```bash
collective member info "<name>" --text
```

Current beamr Focus text is set on: Swarm (correctness reviewer), Dame Lisette (quality reviewer). Set on all future agents at spawn time.

## 5. Development Pipeline

Five stages. Every component passes through all stages. No shortcuts.

See [WORKFLOW.md](docs/governance/WORKFLOW.md) for detailed how-to with dispatch commands and review protocols.

| Stage | Goal | Executor | Gate |
|-------|------|----------|------|
| 1. Research | Understand BEAM, alternatives, and beamr's approach | Norn | Architecture doc complete, pseudocode sufficient |
| 2. Design | Actionable implementation brief | Norn or Haley | Brief passes ARTIFACT-SCHEMAS.md validation |
| 3. Implement | Code on josh/dev | Norn (onatopp-dev-norn) | cargo check + clippy + test green, all R# met |
| 4. Review | Independent verification | Swarm + Dame Lisette | Both reviewers approve |
| 5. Merge | Code on main | bob (creates PR) | CI green, reviewers verify diff |

Dispatch order (parallel waves per dependency chain):
```
Wave 1: [B-002]
Wave 2: [B-003, B-005]
Wave 3: [B-004, B-006, B-008]
Wave 4: [B-007]
```

## 6. Quality Gates

See [QUALITY-GATES.md](docs/governance/QUALITY-GATES.md) for full gate definitions.

Mechanical checks enforced by CI:
- `cargo check -p beamr-cli` (expand to `--workspace` when core lands)
- `cargo clippy -p beamr-cli --no-deps -- -D warnings`
- `cargo test -p beamr-cli`
- No `.rs` file over 500 lines

## 7. Artifact Schemas

See [ARTIFACT-SCHEMAS.md](docs/governance/ARTIFACT-SCHEMAS.md) for document shapes.

Every document type has a defined shape. Reviewers verify conformance. Key schemas:
- Architecture doc: `docs/architecture/NN-component.md`
- Research doc: `docs/architecture/00-topic.md`
- Implementation brief: `docs/design/beamr/briefs/B-NNN.json`
- ADR: `docs/adr/NNN-kebab-case.md`

## 8. Architecture Decisions

11 ADRs on main (authored by Tom). ADR lifecycle:
- **Proposed** → **Accepted** → **Superseded by ADR-NNN**
- Never edit an accepted ADR. Create a new one that supersedes it.
- Both teams are notified when a new ADR is proposed.

Key ADRs for daily work:
- ADR-003: No async in scheduler/interpreter
- ADR-004: Low-bit term tagging (raw u64)
- ADR-005: Gleam-emitted opcodes only
- ADR-008: Copy semantics for message passing

## 9. Cross-Team Alignment

Two teams push to the same repo. Alignment via shared artifacts on main:

- [IN-FLIGHT.md](docs/governance/IN-FLIGHT.md) — updated each PR, shows active work per team
- [COMPONENT-TRACKER.md](docs/governance/COMPONENT-TRACKER.md) — component status through the pipeline
- Briefs on main — Tom's 21 briefs are the shared contract
- ADRs on main — architectural decisions both teams follow

## 10. Automated Enforcement

| Check | Trigger | Action |
|-------|---------|--------|
| CI (check + clippy + test + file-size) | Every PR to main | Block merge on failure |
| Staleness audit | Every 6 hours (GitHub Actions) | Post GitHub issue |
| Norn PR review | On-demand or every 30 min | Post PR comment |

See `.github/workflows/ci.yml`, `.github/workflows/staleness-audit.yml`, `scripts/norn-pr-review.sh`.

## 11. Risk Register

| Risk | Likelihood | Impact | Mitigation | Owner |
|------|-----------|--------|------------|-------|
| Norn produces code outside brief scope | High | Medium | Reviewer diff check against brief's declared files | Swarm + Dame Lisette |
| Core scaffold warnings break CI | Resolved | -- | CI scoped to beamr-cli with --no-deps | bob |
| Reviewer context loss after compaction | Medium | High | Agent configs in repo (CLAUDE.md), re-brief protocol | bob |
| Term representation wrong (u64 layout) | Low | Critical | Architecture research + ADR-004 + test coverage | bob + Tom |
| Cross-team scope collision | Medium | Medium | CODEOWNERS + IN-FLIGHT.md + explicit boundaries | bob |
| Norn code signing breaks on rebuild | Medium | Low | Post-build codesign step in build script | bearup |

## 12. Contributing

### For new agents

1. Read this document (GOVERNANCE.md)
2. Read [CLAUDE.md](CLAUDE.md) for agent-specific configuration
3. Read [README.md](README.md) for repo navigation
4. Check [IN-FLIGHT.md](docs/governance/IN-FLIGHT.md) for current work status
5. Check [COMPONENT-TRACKER.md](docs/governance/COMPONENT-TRACKER.md) for component status

### For proposing changes

- Code changes: create brief or get assigned a brief, implement via norn, review via Stage 4
- Governance changes: propose in PR description, get bob's approval
- Architecture changes: write ADR, get both teams' input
- Cross-team proposals: update IN-FLIGHT.md with proposed scope change, discuss via collective DM

### Commit conventions

```
type(scope): description

feat(cli): add --verbose flag
fix(loader): handle empty atom table
docs(governance): add risk register
```

## Appendices

- A. [Component Tracker](docs/governance/COMPONENT-TRACKER.md)
- B. [In-Flight Work](docs/governance/IN-FLIGHT.md)
- C. [Dispatch Runbook](docs/WORKFLOW-RUNBOOK.md)
- D. [Workflow Definition](docs/governance/WORKFLOW.md)
- E. [Quality Gates](docs/governance/QUALITY-GATES.md)
- F. [Artifact Schemas](docs/governance/ARTIFACT-SCHEMAS.md)
