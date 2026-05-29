# Case Study: Unified Task System Project

Notes from Waffles the Terrible managing 4 agents across a 5-phase project (schema/backend, frontend, CLI, cleanup) with sequential dependencies.

## Team Setup

- **Waffles** (team lead) — coordination, reviews, phase gating
- **The Stepmother** — Phase 1-2 (schema + backend API)
- **Bees** — Phase 3 (frontend components)
- **Brenda** — Phase 4 (CLI commands)
- **Mortimer** — Phase 5 (cleanup, dead code removal)

## Timeline

Total execution: ~45 minutes wall clock for 5 phases, 7 commits, ~2500 new lines.

## What Worked

### Pre-briefing (highest impact)

While Phase 1-2 was in progress, the blocked agents (Phase 3, 4, 5) were given their assignments as read-only study tasks. Results:
- Brenda (CLI) came back with exact line numbers for every method she'd change
- Bees (Frontend) mapped every file, field mapping, query key, and WebSocket event
- Mortimer (Cleanup) found a schema init race condition from just reading — flagged to The Stepmother to fix in Phase 1-2

This cross-phase coordination only happens when everyone has context before they start coding.

### Clear deliverables

Each agent got a numbered list with verification commands. Phase 4 (CLI) got a clean first-pass review — zero issues. Pre-briefing + unambiguous deliverables = complete work.

### Code review as hard gate

Review results across phases:
- **Phase 1-2**: 1 critical (auto-sync `?` operator silently disabling entire function on non-UUID keys), 1 high (cancel not setting completed_at), 1 high (comparing display text vs option IDs), 1 medium (tags deviation from plan)
- **Phase 3**: 2 critical (wrong date format, unwired components), 2 high, 2 medium — all fixed on first rework
- **Phase 4**: Clean pass (zero issues)
- **Phase 5**: Clean pass (zero issues)

Every critical bug would have caused real production failures. The review paid for itself every time.

### Parallel execution

Phases 3 (frontend) and 4 (CLI) ran simultaneously because:
- Different file scopes — no merge conflicts
- Both consume the same API — shared understanding
- Both were pre-briefed — zero ramp-up after the go signal
- Tom suggested they check in with each other for API overlap — smart

## Problems Encountered

### Context compaction

The Stepmother hit compaction mid-Phase 1-2 and had to re-read the plan. Mitigation: require frequent commits and pushes so progress isn't lost.

### Agents don't push proactively

Had to ask multiple times for WIP pushes. Agents tend to work locally and only push when they think they're "done." This means you can't verify progress and compaction can lose work.

### Commit hash confusion

Bees reported a commit hash that was actually from a different agent's earlier push. The remote HEAD hadn't changed. Always `git fetch` and verify independently — especially as the gate between review and next-phase dispatch.

### Message quoting

The collective CLI has issues with special characters in message content. Workaround: write to a temp file and use `$(cat /tmp/file.txt)`.

### Overlapping wakeups

Multiple wakeups at different intervals led to redundant check-ins. Better: one wakeup at a time, schedule the next after handling the current one.

## Results

- 5 phases, 4 agents, 7 commits
- 3 critical bugs caught by code review (all would have been production failures)
- Phases 4 and 5 both got clean first-pass reviews
- ~180 lines dead code removed, ~2500 lines new code
- Zero errors in any file touched by the project

## Key Patterns Summary

1. Pre-brief blocked agents with study tasks
2. Number every deliverable with verification commands
3. Single plan document as source of truth
4. Code review as hard gate (Opus model, no exceptions)
5. Parallel execution of independent phases
6. Periodic WIP pushes (every 15-20 min)
7. Wakeup-based check-in loop (one at a time)
8. Cross-team flags for shared discoveries
9. Verify commits independently
10. Focus text for visibility

*Source: docs/TEAM_MANAGEMENT_NOTES.md*
*Project: Unified Task System — COMPLETE*
*Lead: Waffles the Terrible (Session: 896955e1)*
