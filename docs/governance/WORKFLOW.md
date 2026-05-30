# Workflow

How beamr gets built. Every component follows this pipeline. No shortcuts.

## Roles

| Agent | Role | Executes? | Reviews? |
|-------|------|-----------|----------|
| bob of dylan | Directional lead | No | Architecture docs |
| Haley Barrows | Brief writer | No | -- |
| Ms. Anastacio Streich | Design doc writer | No | -- |
| Reverend Chaos | Enforcer | No | Briefs (rule compliance) |
| A Swarm of Bees | Code reviewer #1 | No | Code (correctness, R# compliance) |
| Dame Lisette Frami | Code reviewer #2 | No | Code (quality, conventions, safety) |
| Norn | Builder | ALL execution | First-pass (self-review in workflow) |
| bearup | Project owner | No | Final authority, escalation target |

**Rule:** Team agents direct and coordinate. Norn executes all analysis, builds, and first-pass self-review. Team reviewers (Swarm, Dame Lisette) perform independent gate reviews. No agent writes code directly.

## Pipeline

### Stage 1: Research

**Goal:** Understand how BEAM does it, how alternatives tried it, and what beamr should do differently.

**Executor:** Norn (norn-codebase-explorer profile)

**How:**
```bash
norn -p -q \
  --profile norn-codebase-explorer \
  -- "Research [component]: analyze BEAM source (cite files), survey alternatives, document limitations, write pseudocode for beamr's approach. Output as architecture doc following docs/governance/ARTIFACT-SCHEMAS.md shape."
```

**Output:** `docs/architecture/NN-component-name.md`

**Review:** bob of dylan checks: claims grounded in source, pseudocode complete, ADR-consistent, improvements justified.

**Gate:** Architecture doc follows ARTIFACT-SCHEMAS.md. All sections present. Pseudocode sufficient for implementation.

### Stage 2: Design

**Goal:** Turn architecture research into an actionable implementation brief.

**Executor:** Norn or Haley Barrows

**How (if brief needs writing):**
```bash
norn -p -q \
  --profile norn-developer \
  -- "Write implementation brief B-NNN for [component]. Follow docs/governance/ARTIFACT-SCHEMAS.md brief schema. Reference architecture doc at docs/architecture/NN-component.md. Checklist IDs from docs/design/beamr/checklist.json, stories from docs/design/beamr/stories.json."
```

**Output:** `docs/design/beamr/briefs/B-NNN.json`

**Review:** Reverend Chaos validates: checklist ownership, no double-claiming, acceptance criteria on every R#, file paths match scaffold.

**Gate:** Brief passes ARTIFACT-SCHEMAS.md validation rules.

**Note:** Tom has authored all 21 briefs (B-001 through B-021). This stage only applies if a brief needs revision or a new brief is added.

### Stage 3: Implement

**Goal:** Code the brief requirements on josh/dev branch.

**Executor:** Norn via onatopp-dev-norn workflow

**How:**

For exact dispatch commands, see [WORKFLOW-RUNBOOK.md](../WORKFLOW-RUNBOOK.md). The canonical form:

```bash
meridian workflow run onatopp-dev-norn \
  --workspace "$WS" --as "$ME" \
  --worktree --base main \
  --input brief="$(cat docs/design/beamr/briefs/B-NNN.json)" \
  --input checklist_content="$(cat docs/design/beamr/checklist.json)" \
  --input stories_content="$(cat docs/design/beamr/stories.json)" \
  --input notify="<member name>" \
  --input run-name="beamr B-NNN" \
  --text
```

Do NOT pass `design_content` — beamr has no design.json (only DESIGN.md markdown). The workflow handles this correctly when omitted.

This runs: scout (read-only exploration) → dev (implement all R#s) → commit → review (harden + verify) → commit.

**Output:** Code on josh/dev branch. Structured JSON at `.run/dev-output.json` and `.run/review-output.json`.

**Quality bar (norn must pass before submitting):**
- `cargo check -p <crate>` green (use `-p beamr-cli` for CLI briefs; `--workspace` once core scaffold is clean)
- `cargo clippy -p <crate> --no-deps -- -D warnings` green
- `cargo test -p <crate>` all pass
- Every R# acceptance criterion met
- No `.unwrap()` / `panic!()` outside `#[cfg(test)]`
- No file over 500 lines
- Term = raw u64 (ADR-004), not Rust enum

**Gate:** Norn's review step reports `pass=true`.

### Stage 4: Team code review

**Goal:** Independent verification of norn's implementation by two reviewers with different lenses.

**Reviewers:**
- **A Swarm of Bees** — correctness: every R# met, acceptance criteria verified against actual code (not norn's summary), checklist items ticked, stories satisfied
- **Dame Lisette Frami** — quality: conventions followed, safety (no unwrap, explicit errors), test coverage (happy + error paths), module boundaries clean, wiring connected

**How:**
```
bob sends norn's output + diff to both reviewers via collective DM:

collective send --as "<bob-session>" --to "A Swarm of Bees" --subject "Review B-NNN" \
  --message "<diff summary + norn output>"

collective send --as "<bob-session>" --to "Dame Lisette Frami" --subject "Review B-NNN" \
  --message "<diff summary + norn output>"
```

**Review protocol:**
1. Both reviewers independently assess
2. Findings reported via DM to bob
3. Blocking issues → route back to norn for fix (Stage 3 re-run with findings)
4. Maximum 2 retry cycles before escalating to bearup

**Gate:** BOTH reviewers explicitly approve. No merge before both sign off.

### Stage 5: PR and merge

**Goal:** Get reviewed code from josh/dev onto main.

**Executor:** bob of dylan creates PR

**How:**
```bash
git push origin josh/dev
gh pr create --base main --head josh/dev \
  --title "feat(component): implement B-NNN" \
  --body "$(cat <<'EOF'
## Summary
- Implements B-NNN: [component name]
- R#s: [list]
- Tests: [count] new

## Reviewers
- Swarm: approved (DM [date])
- Dame Lisette: approved (DM [date])

## Checklist
- [x] cargo check -p <crate>
- [x] cargo clippy -p <crate> --no-deps -- -D warnings
- [x] cargo test -p <crate>
- [x] Both reviewers approved
- [x] No unwrap outside tests
- [x] All files < 500 lines
EOF
)"
```

**CI runs on PR:** cargo check, clippy, test (see `.github/workflows/ci.yml`).

**Gate:** CI green + both reviewers verify PR diff matches reviewed code → merge.

## Autonomous checks

### CI on every PR

GitHub Actions runs cargo check + clippy + test on every PR to main. See `.github/workflows/ci.yml`.

### Staleness audit (every 6 hours)

GitHub Actions checks for:
- Open PRs with no review decision
- josh/dev more than 3 commits ahead of main without a PR
- Architecture research docs that are empty
- Components with no research or implementation started

Results posted as a GitHub issue with label `staleness-audit`.

See `.github/workflows/staleness-audit.yml`.

### Norn PR review (on-demand or scheduled)

`scripts/norn-pr-review.sh` runs norn with the norn-reviewer profile against a PR diff and posts findings as a PR comment.

```bash
./scripts/norn-pr-review.sh 3        # review specific PR
./scripts/norn-pr-review.sh --poll   # review all unreviewed open PRs
```

Can be scheduled via LaunchAgent (`scripts/com.beamr.norn-pr-review.plist`, every 30 min). Team reviewers (Swarm, Dame Lisette) perform gate review per Stage 4.

## Dispatch reference

For exact commands, workspace IDs, dependency ordering, failure decoders, and worktree inspection:

See **[docs/WORKFLOW-RUNBOOK.md](../WORKFLOW-RUNBOOK.md)** — the operational runbook for dispatching briefs via Meridian v2.

Key dispatch order (parallel waves):
```
Wave 1: [B-002]
Wave 2: [B-003, B-005]
Wave 3: [B-004, B-006, B-008]
Wave 4: [B-007]
```

Each wave must land to main before the next starts.

## Communication

All coordination happens via collective DMs. No Slack, no email, no Telegram for build coordination.

```
collective send --as "<session-id>" --to "<recipient>" --subject "<topic>" --message "<content>"
```

Escalation path: team agent → bob → bearup.

## Cross-team alignment

Both teams (bob's and Tom's) push to the same repo. Alignment via:

1. **IN-FLIGHT.md** — updated each PR, shows what each team is working on
2. **COMPONENT-TRACKER.md** — shows component status through the pipeline
3. **Briefs on main** — Tom's 21 briefs are the shared contract
4. **ADRs on main** — architectural decisions both teams follow

Tom pulls main → sees our governance artifacts → knows our scope and approach.
We pull main → see Tom's code + briefs → know what's implemented.
