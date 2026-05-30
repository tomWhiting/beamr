# In-flight work

Current work status for both teams. Updated with each PR to main.

Last updated: 2026-05-30

## Bob's team (coordination + CLI + research)

### Active

| Work item | Owner | Status | Branch | Notes |
|-----------|-------|--------|--------|-------|
| Architecture research: BEAM alternatives | Norn | Done | josh/dev | 1062 lines, 6 projects surveyed |
| Architecture research: term representation | Norn | Blocked | -- | Norn timeout on first attempt, awaiting norn rebuild |
| CLI (B-009) R1/R4/R5 | Norn | Merged | main (PR #1) | Arg parsing, validation, help/version |
| CLI (B-009) R2/R3 | -- | Blocked | -- | Needs core types from Tom's B-001/B-004 |
| Governance: workflow + quality gates | bob | In PR | josh/dev | WORKFLOW.md, QUALITY-GATES.md, ARTIFACT-SCHEMAS.md, COMPONENT-TRACKER.md |
| README | bob | In PR | josh/dev | Repo navigation entry point |

### Queued (in priority order)

1. Architecture research docs 2-11 (atom table through reduction hook) — blocked on norn rebuild
2. Brief validation pass with Reverend Chaos — verify all 21 briefs against ARTIFACT-SCHEMAS.md
3. CLI R2/R3 wiring — blocked on Tom's core types (B-001, B-004)

### Scope boundaries

- We own: CLI crate, architecture research, governance, coordination, reviews
- We do NOT own: core crate implementation (Tom's lane)
- We review: Tom's PRs through Swarm + Dame Lisette
- Tom reviews: our PRs (or his agents do)

## Tom's team (core implementation)

### Known work (from commits on main)

| Work item | Status | Notes |
|-----------|--------|-------|
| 21 briefs (B-001 through B-021) | Done | All on main, dispatch-ready |
| Crate scaffold (37 stub files) | Done | 3 crates, all stubs |
| 11 ADRs | Done | Architecture decisions |
| DESIGN.md + checklist.json + stories.json | Done | Foundation docs |
| Core implementation (B-001 through B-008) | Unknown | Tom owns, status via his commits |

### Dependencies we're waiting on

| Dependency | Brief | Blocks |
|------------|-------|--------|
| Error types | B-001 | Everything in core |
| Atom table | B-002 | Loader, imports |
| Module registry | B-004 | CLI R2 (execution) |
| Term immediates | B-005 | Term research validation |

## Cross-team dependencies

```
Tom's core (B-001..B-008) ──blocks──▶ Our CLI R2/R3 (B-009)
Tom's B-005 (terms) ──────informs──▶ Our term research doc
Our architecture docs ────informs──▶ Tom's implementation choices
Our governance docs ──────governs──▶ Both teams' PR process
```

## How to update this doc

When creating a PR to main, update the relevant section:
- Move completed items from Active to a "## Completed" section (or remove)
- Add new work items to Active
- Update dependency status
- Bump "Last updated" date
