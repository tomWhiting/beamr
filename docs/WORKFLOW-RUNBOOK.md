# beamr — Workflow Runbook (team)

How to build beamr brief-by-brief with the `onatopp-dev-norn` workflow on Meridian v2.
Battle-tested on B-001 (run `d2307df0`, completed clean). Read this before dispatching.

---

## Fixed facts (beamr)

| Thing | Value |
|---|---|
| Workspace ID | `d5bde9ad-210f-47d3-a670-82cb7653e1a6` |
| `--as` member | your own member UUID (must be in this workspace). `bearup` = `0ad0fab7-df26-4b8c-a5f2-8083d35c1167` works. Tom's `c9255b2a…` is **not** registered here. |
| Workflow | `onatopp-dev-norn` (all-norn dev cycle: scout → dev → review/harden → done → notify) |
| Briefs | `docs/design/beamr/briefs/B-00N.json` (JSON objects, not markdown) |
| Inputs | `checklist.json`, `stories.json` (both JSON, keep them) |
| Server | v2 on `:29876` |

---

## One-time per repo (already done — don't repeat)

The `--worktree` path needs yggdrasil's branch tree initialised:

```bash
cd /Users/joshua/Developer/beamr
yg init           # initialise yggdrasil in the repo
yg tree init      # root = current branch (main)
```

Done already. If you see `yggdrasil is not initialized`, run the two lines above.

---

## Dispatch a brief (the working command)

```bash
cd /Users/joshua/Developer/beamr
WS=d5bde9ad-210f-47d3-a670-82cb7653e1a6
ME=<your-member-uuid>            # e.g. bearup 0ad0fab7-df26-4b8c-a5f2-8083d35c1167
D=docs/design/beamr
B=B-002                          # the brief to build

meridian workflow run onatopp-dev-norn \
  --workspace "$WS" --as "$ME" \
  --worktree --base main \
  --input brief="$(cat "$D/briefs/$B.json")" \
  --input checklist_content="$(cat "$D/checklist.json")" \
  --input stories_content="$(cat "$D/stories.json")" \
  --input notify="<your member name>" \
  --input run-name="beamr $B" \
  --text
```

Grab the `execution_id` from the output. You get a DM on completion (success **or** failure).

### Rules
- **Pass `brief` as a file via `$(cat …)`.** Missing/empty brief → `Unknown property 'id' on type '()'`.
- **Do NOT pass `design_content`.** beamr has no design JSON (only `DESIGN.md` markdown). The Rhai script reads `design.intention` expecting a parsed object; a markdown string throws `Unknown property 'intention' on type 'string'`. Omitting it is correct — `build_design_context` returns "" when absent.
- `notify` must be your **exact** full member name (e.g. `"bearup"`, not a nickname).
- `--worktree --base main` provisions a fresh isolated branch under `.yggdrasil-worktrees/workflow/onatopp-dev-norn/<id>/`. Never use `--worksite` (v1, buggy).

---

## Dependency order — land before dispatching dependents

**Critical:** each run branches a fresh worktree off `main`. A brief only sees prior code if that code is **already on `main`**. So land each brief before dispatching anything that `depends_on` it.

```
B-001 ✓ (done)
  └─ B-002 (atom table)
       ├─ B-003 (.beam parser)  ──> B-004 (import resolution, modules)
       └─ B-005 (terms: tagging/immediates)
            ├─ B-006 (terms: boxed)  ──> B-007 (term comparison)
            └─ B-008 (BIF/NIF registries, Gate 1)
```

| Brief | depends_on | can start once landed |
|---|---|---|
| B-002 | B-001 | now |
| B-003 | B-001, B-002 | after B-002 |
| B-005 | B-001, B-002 | after B-002 (parallel with B-003) |
| B-004 | B-003 | after B-003 |
| B-006 | B-005 | after B-005 |
| B-008 | B-005 | after B-005 (parallel with B-006) |
| B-007 | B-005, B-006 | after B-006 |

Parallel-safe waves: **[B-002] → [B-003, B-005] → [B-004, B-006, B-008] → [B-007]**. Each wave's branches must be landed to `main` before the next wave starts.

---

## Monitor a run

```bash
meridian workflow status <execution-id> --workspace $WS --as $ME --text
```

- Steps progress: `scout` → `dev` → `review-1` → done. Each `outcome: success` as it clears.
- A norn step takes minutes (scout reads the whole codebase). `steps_executed: 0` is a counter quirk — trust per-step `outcome`.
- `meridian workflow history --workspace $WS --as $ME --limit 10 --text` lists past runs.

### Ignore this noise
- **`GET …/pipelines/local-worktree → 500`** spamming the log / pipeline UI panel: display-endpoint bug in the v2 server. Execution is unaffected — the pipeline binds and runs fine.
- **`relay control connect/auth failed: HTTP 530`**: exchange federation to the remote relay. Irrelevant to local runs.

---

## After a run — inspect & clean the output

Output lands in the worktree branch (NOT main):
- Path: `.yggdrasil-worktrees/workflow/onatopp-dev-norn/<id>/`
- Branch: `workflow/onatopp-dev-norn/<id>`, usually one commit.

```bash
WT=.yggdrasil-worktrees/workflow/onatopp-dev-norn/<id>
git -C "$WT" log --oneline main..HEAD
git -C "$WT" diff --stat main..HEAD -- ':!target'
cd "$WT" && cargo check -p beamr-cli && cargo clippy -p beamr-cli --no-deps -- -D warnings
# (use --workspace once core scaffold is clean post B-001..B-008)
```

### Known norn output hygiene issues — check every run
1. **Build artifacts.** Norn commits everything, including `target/`. The repo `.gitignore` now covers `target/`, `.yggdrasil-worktrees/`, `.commit-msg.tmp` — but if a run still committed them, untrack:
   ```bash
   git -C "$WT" rm -r --cached --quiet target; git -C "$WT" rm --cached --quiet .commit-msg.tmp
   git -C "$WT" checkout main -- .gitignore && git -C "$WT" commit --amend --no-edit
   ```
2. **Scope creep.** Norn sometimes edits files outside the brief's scope (B-001 wrongly edited `B-006/7/8.json`). Diff against the brief's declared `files:` and revert strays:
   ```bash
   git -C "$WT" restore --source=main -- <stray-file>
   ```

Verify the final diff = only the brief's intended files, `cargo check` + `clippy -D warnings` clean, **then** land.

---

## Land to main

Once clean + green, fast-forward / merge the branch to `main` (or use `meridian stack land` / your agreed land flow). Only landed code is visible to dependent briefs — see dependency order above.

---

## Quick failure decoder

| Error | Cause | Fix |
|---|---|---|
| `Unknown property 'id' … type '()'` | `brief` input missing/empty | pass `--input brief="$(cat …json)"`, verify path |
| `Unknown property 'intention' … type 'string'` | `design_content` was markdown | omit `design_content` |
| `yggdrasil is not initialized` | branch tree missing | `yg init && yg tree init` |
| 403 on dispatch | `--as` not a workspace member | use your beamr member UUID |
| `pipelines/local-worktree 500` | server display bug | ignore — run is fine |
