---
name: brief-authoring
description: Author yggdrasil implementation briefs from the design docs (DESIGN.md + CHECKLIST.md + USER-STORIES.md) in a design folder. Use when transitioning a crate's design into the numbered-requirements briefs that feed orchestrated-dev. Triggered by terms like write briefs, brief authoring, generate briefs, turn design into briefs, enrich briefs, brief cluster. Handles a single brief, a range, or a whole crate. Dispatches brief-researcher and brief-writer sub-agents per brief.
argument-hint: "[design-folder] [brief-range] [target-dir]"
arguments: [design_folder, brief_range, target_dir]
allowed-tools: Read, Glob, Grep, Agent, TaskCreate, TaskGet, TaskList, TaskUpdate
---

# Brief Authoring

Use this skill to turn a crate's design folder — the `DESIGN.md` + `CHECKLIST.md` + `USER-STORIES.md` trio under `docs/design/<crate>/` — into a numbered set of implementation briefs ready for `orchestrated-dev` consumption.

## Orientation

A brief is one workflow-ready unit of work. Every brief contains numbered requirements (R1..RN) with file paths, type signatures, acceptance criteria, checklist cross-refs, user-story cross-refs, and prerequisite-brief references. The `orchestrated-dev` workflow consumes one brief per dispatch; the brief's quality is the ceiling on the workflow output's quality.

A design folder is the brief's source material:

- `DESIGN.md` — prose architecture, data models, module layouts, public API, invariants.
- `CHECKLIST.md` — letter-grouped numbered items (A1, B2, C3, ...) capturing every commitment the design makes.
- `USER-STORIES.md` — per-persona stories (H-D-1, AI-D-1, L-1, M-1, X-1, ...) capturing outcomes the design must deliver.

A brief cluster is the set of briefs that realise one design folder. Three crates = three clusters. The brief-authoring skill handles one cluster per invocation (it can author a range within a cluster or the whole cluster).

## Inputs to Gather Before Dispatching

1. **`design_folder`** — absolute path to the crate's design folder, e.g. `/Users/tom/Developer/ablative/yggdrasil/docs/design/libcorpus/`. Validate it exists and contains all three required docs.

2. **`brief_range`** — the brief numbers to author. Forms: `184` (one brief), `184-190` (inclusive range), `all` (every brief the CHECKLIST's coverage demands, starting from the next free number in the target directory).

3. **`target_dir`** — absolute path of the subdirectory where briefs land, e.g. `/Users/tom/Developer/ablative/yggdrasil/docs/briefs/184+_libcorpus-and-storage/libcorpus/`. Must exist or be creatable.

4. **Existing briefs to avoid renumbering** — scan the yggdrasil `docs/briefs/` tree for the current max brief number. Briefs must be authored in strict ascending order; a new brief N cannot depend on a not-yet-authored brief M > N.

## Procedure

### Step 1 — Plan the cluster

Before dispatching any sub-agent:

1. Read `DESIGN.md`, `CHECKLIST.md`, `USER-STORIES.md` in full. Build a mental map of the crate.
2. Derive a brief-by-brief plan: for each brief in the range, a `brief_spec` containing:
   - `brief_number`
   - `working_title`
   - `design_anchor` — the DESIGN.md section the brief realises
   - `checklist_ids` — the CHECKLIST items the brief commits to
   - `story_ids` — the USER-STORIES the brief must address
   - `v1_hint` — paths under `/Users/tom/Developer/projects/deno_rust/meridian/` likely relevant
   - `prerequisite_briefs` — brief numbers this one depends on
3. Validate MECE: every CHECKLIST item appears in exactly one brief; every USER-STORY is addressed by ≥1 brief; no two briefs own the same file range. Adjust the plan if you find overlap or gap.
4. Write the plan as a tracking document at `{target_dir}/PLAN.md` (human-readable markdown). Keep it updated as briefs are authored.

### Step 2 — Dispatch research + write per brief

For each brief in the range, in ascending number order:

1. **Spawn `brief-researcher`** with the `brief_spec` and wait for the research artifact. Expected output: `{design_folder}/briefs/.research/<NN>-<slug>.md`.
2. **Check the research artifact's gaps section.** If it contains `blocker` entries, stop — report to the caller. Do not dispatch brief-writer on a blocked artifact.
3. **Spawn `brief-writer`** with `brief_number`, `target_path`, `research_artifact_path`, `template_path` (see references/ below), and `design_folder`. Wait for the brief.
4. **Optional: spawn `brief-reviewer`** on the drafted brief. If verdict is `blockers`, loop back to brief-writer with the findings. If `warnings` or `pass`, proceed.
5. **Log the per-brief structured return** (briefs_path, r_count, checklist_refs, story_refs, prereq_briefs, v1_refs, gaps_flagged) in `{target_dir}/PLAN.md` or a sibling log.

### Step 3 — Summary index

After all briefs in the range are authored, produce a summary:

- Number of briefs written.
- Number of blockers raised.
- Checklist coverage (every CHECKLIST item referenced by ≥1 brief, true/false).
- User-story coverage (every USER-STORY addressed by ≥1 brief, true/false).
- List of gaps carried forward (open questions for Tom to resolve).
- Suggested dispatch order (strict ascending; parallel-safe groups based on file-range orthogonality).

Write this summary to `{target_dir}/INDEX.md`.

## Parallelism Rules

- Up to ~5 simultaneous brief triples (researcher + writer ± reviewer) is the sweet spot.
- **No two parallel briefs may share a target file** in "files likely to change". Scan the briefs' suggested-files before dispatching in parallel; serialise any that collide.
- **Prerequisite ordering is strict.** A brief N blocking on brief M cannot be dispatched until M's artifact is written and passed review. The skill enforces this by tracking per-brief state in PLAN.md.
- Research for different briefs in the same cluster can parallel freely — artifacts are filesystem-isolated.
- Writing for different briefs in the same cluster can parallel freely provided file-range orthogonality holds.

## Failure Modes and Recovery

- **Research artifact flags `blocker`.** Stop the pipeline for that brief. Report the blocker to the caller. Do NOT dispatch brief-writer.
- **Writer returns with gaps.** If the gaps are carried from the research artifact, proceed (they propagate into the brief's Open Questions). If the writer discovered new gaps mid-write, escalate to the caller before publishing.
- **Reviewer returns `blockers`.** Loop back to brief-writer with the findings in a second Write call is wrong — instead dispatch a fresh brief-writer invocation with the original artifact plus the reviewer's blockers list; the writer reads both and produces the corrected brief. Log the iteration in PLAN.md.
- **Reviewer returns `warnings`.** Decide per warning whether it blocks publishing; low-risk warnings can land with a note in PLAN.md flagging the soft-debt.
- **Prerequisite brief doesn't exist.** Stop. Report to the caller. The cluster plan was wrong; don't author briefs that depend on nothing.
- **Design doc drift.** If the research artifact flags a drift (e.g. `replace_path` vs `replace_partition` in the current design folders), stop the pipeline for that brief, report to the caller, and do not author briefs against the drifted content. The caller resolves the drift first.

## Output Contract

A successful invocation produces:

1. `{target_dir}/<NN>-<slug>.md` for each brief authored — one file per brief.
2. `{design_folder}/briefs/.research/<NN>-<slug>.md` for each research artifact produced.
3. `{target_dir}/PLAN.md` — live plan tracking per-brief state.
4. `{target_dir}/INDEX.md` — summary index.
5. A final structured return to the caller naming: briefs written, blockers, coverage, suggested dispatch order.

## References

Reference files live alongside this SKILL.md and are read by the skill (and the sub-agents it dispatches) on demand:

- `references/brief-template.md` — the canonical brief template. brief-writer loads this directly.
- `references/design-doc-conventions.md` — how DESIGN.md anchors work, how CHECKLIST items are numbered, how USER-STORIES ids are formatted.
- `references/v1-reference-paths.md` — well-known roots for v1 lifts so brief-researcher doesn't search blindly.
- `references/example-brief.md` — one reference-quality enriched brief to set ground truth for brief-writer.

Load references only when needed. Don't preload into the skill's runtime context at invocation.

## Rules

- **The orchestrator is the product.** This skill's value is in coordinating researcher + writer + reviewer cleanly. Keep the dispatch logic tight.
- **Filesystem is the medium.** Agents pass artifacts by path, not by prompt-argument. At 40-60 briefs the cost of passing research text through prompts is prohibitive.
- **Strict ascending brief order.** If Tom wants brief 190 but 185-189 aren't done, you author 185 through 190 in order.
- **No wall-of-text prompts to sub-agents.** The `brief_spec` to brief-researcher is a small structured object, not a document dump. Researcher pulls context from the filesystem.
- **One brief, one artifact, one file.** Don't bundle multiple briefs into a single artifact or a single output file. The 1:1 shape is load-bearing for the workflow downstream.
