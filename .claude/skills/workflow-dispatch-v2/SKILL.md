---
name: workflow-dispatch-v2
description: Dispatch Meridian v2 workflows (orchestrated-dev, review-and-land, etc.) against the Yggdrasil substrate via the unified `meridian` binary. Use when running or monitoring workflows in the v2 repo at /Users/tom/Developer/ablative/yggdrasil. Triggered by terms like dispatch workflow, run workflow, orchestrated-dev, review-and-land, meridian workflow run, worktree dispatch, v2 workflow.
---

# Meridian v2 Workflow Dispatch

Dispatch Meridian v2 workflows via the `meridian workflow` CLI against the Yggdrasil substrate. This skill covers everything you need to run, monitor, and recover from workflows.

## Identity

**Your session ID:** `${CLAUDE_SESSION_ID}` — this is who you are.

**The `--as` identity:** workflow dispatch requires a member with a reporting-tree link to the Yggdrasil workspace. Raw Claude session IDs without that link are rejected with 403. Unless told otherwise, dispatch `--as c9255b2a-5731-4d17-8124-e3bfa2224186` (Tom's member ID).

When your own session has been linked into the workspace ACL, prefer dispatching `--as ${CLAUDE_SESSION_ID}`. The end-of-workflow notification DM is hard-wired to land in both Waffles' and Tom's inboxes regardless of `--as`, so identity here only affects authorisation, not who sees completion.

## Workspace and Binary

- **v2 workspace ID:** `2d5fdd51-1f25-45a4-8f86-4d4c978d1355`
- **Unified binary:** `meridian` (installed on PATH)
- **Server config:** `~/.meridian/v2-config.toml`
- **Workflow source:** `.meridian/workflows/` in the repo (scanned at server startup). Supports YAML files (`*.yaml`), Rhai scripts (`*.rhai`), and Rhai packages (`<name>/workflow.rhai` with co-located templates, schemas, profiles). `.yggdrasil/workflows/` is NOT scanned.

Restart the v2 server after a rebuild or config change:


## CLI Reference

```
meridian workflow list                                   # Available workflow names
meridian workflow show <name> --workspace <id>           # Definition: inputs, steps
meridian workflow run <name> --workspace <id> ...        # Dispatch a workflow
meridian workflow status <execution-id>                  # Status + per-step details
meridian workflow history --workspace <id> --limit N     # Past executions
meridian workflow cancel <execution-id>                  # Cancel a running execution
```

All commands default to JSON output; add `--text` for human-readable. All accept `--as <ID>` for identity.

## Core Workflows

Authoritative list is `meridian workflow list`; this table is the common set at time of writing.

| Workflow | Required inputs | Purpose |
|----------|----------------|---------|
| `onatopp-dev-lean` | `brief` (optional: `design_content`, `checklist_content`, `stories_content`, `notify`, `run-name`) | **Primary workflow.** Scout → Dev → Checks → Review+Harden → Done → Notify. Three action steps. Merged harden+review into a single pass. One check dispatch after dev. |
| `onatopp-dev-compact` | same as lean | Scout → Dev → Checks → Harden → Checks → Review → Checks → Done → Notify. Four action steps, three check dispatches. More thorough but slower. |
| `onatopp-dev-multi` | `brief`, `design_content`, `provider` | Mixed-provider variant (Rhai imperative). `provider=mixed` (default): scout=Codex, dev=Claude, review=Codex. File-scope enforcement + commit steps. |
| `orchestrated-dev` | `brief` (optional: `run-name`) | Legacy. Scout → Plan → Implement → checks → Review → Done → Notify. |
| `review-and-land` | `brief`, `findings`, `commit_prefix` | Apply reviewer findings to an existing worktree, run full checks in a loop, commit. |
| `run-checks-triaged` | (standalone or sub-workflow) | Cargo check → clippy (blocking/deferred split) → test → TS check → biome → file-size → verdict. Used as a dispatch sub-workflow by dev workflows. |
| `notify` | (optional: `workflow`, `run-name`) | End-of-workflow notification helper. Rarely run by hand. |

Always run `meridian workflow show <name> --workspace <id>` before dispatching something unfamiliar.

## Dispatch Patterns

### Fresh worktree (new feature branch from `main`)

```bash
meridian workflow run onatopp-dev-lean \
  --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --worktree --base main \
  --input brief="$(cat /abs/path/to/brief.json)" \
  --input design_content="$(cat /abs/path/to/DESIGN.md)" \
  --input checklist_content="$(cat /abs/path/to/checklist.json)" \
  --input stories_content="$(cat /abs/path/to/stories.json)" \
  --input notify="Marge the All-Knowing" \
  --input run-name="brief 215 — libcorpus loaders"
```

The `brief` input is a JSON object (not a markdown file). `design_content`, `checklist_content`, and `stories_content` are optional but recommended — they feed the design document, checklist C-numbers, and user-story S-numbers into all action step prompts. `notify` triggers a completion DM to the named member. `run-name` labels the notification.

### Existing worktree (follow-up work on a branch)

```bash
meridian workflow run review-and-land \
  --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --worktree /abs/path/to/.yggdrasil-worktrees/workflow/onatopp-dev-lean/<id> \
  --input "brief=$(cat /abs/path/to/brief.json)" \
  --input "findings=$(cat /abs/path/to/findings.md)" \
  --input "commit_prefix=feat: brief 123 (short description)"
```

### Main checkout (no worktree, run directly in the repo)

Omit `--worktree` entirely. Use sparingly; worktree isolation is the default.

### Worktree rules

- `--worktree` with **no value**: provision a fresh branch + worktree under `.yggdrasil-worktrees/workflow/<workflow-name>/<id>/`, based on `--base` (default `main`).
- `--worktree <PATH>`: use an **existing** worktree at that absolute path — the CLI does not provision, start, or tear down.
- `--worktree ""` (empty string): **rejected**. Pass a bare flag, a real path, or omit entirely. There is no silent fallback.
- **Never use `--worksite`.** Worksites are the v1 concept with known bugs (cwd escape). v2 uses worktrees exclusively.

### Always pass inputs as `--input key=value`

One `--input` per named input in the workflow's schema. Inputs that are multi-line or contain shell-special characters should be sourced from files via `$(cat path)`.

## After dispatch — auto-notify

`onatopp-dev-lean` and `onatopp-dev-compact` run a terminal `Notify` step that sends a DM via `collective send` to the member named in the `notify` input. Both `Done`'s success and failure routes go through `Notify`, so the DM fires regardless of whether the workflow succeeded or failed.

Pass `--input notify="Full Member Name"` (must be the exact collective member name — e.g. `"Marge the All-Knowing"`, not `"Marge"`).

## Monitoring

```bash
# List recent executions
meridian workflow history --workspace <id> --limit 10 --text

# Inspect a specific execution (summary + per-step)
meridian workflow status <execution-id> --text

# Filter history by status
meridian workflow history --workspace <id> --status running --text
meridian workflow history --workspace <id> --status failed --text

# Cancel a running workflow
meridian workflow cancel <execution-id>
```

## Workflow Format

Workflows live in `.meridian/workflows/`. Two formats are supported:

### YAML (declarative)

`*.yaml` files. Grammar matches the v1 `workflows` skill (see `.claude/skills/workflows/SKILL.md` for the full grammar — step kinds, templates, parsers). Rules:

- `evaluate` step `criteria:` must be a **list of strings**, not a scalar.
- A workflow's `output:` block is captured on completion and made available to parent dispatches as `{Step.field_name}`.

### Rhai (scripting)

Bare `*.rhai` files or package directories (`<name>/workflow.rhai` with co-located `templates/`, `schemas/`, `profiles/`). Rhai scripts can be:

- **Declarative** — call `add_step`/`set_input`/`set_route`/`set_template`/`set_schema` to build a step graph that the engine walks.
- **Imperative** — call `execute_step`/`commit`/`run_cmd`/`render_template` to drive execution directly from the script. Gives full control flow (conditionals, loops, provider routing).
- **Mixed** — define steps declaratively, then call `execute_step` to dispatch them with script-controlled sequencing.

Imperative builtins: `run_cmd`, `read_file`, `write_file`, `read_json`, `parse_json`, `to_json`, `write_json`, `set_context`, `render_template`, `execute_step`, `commit`, `edit_transcript`.

## Where to Put Things

- **Briefs** (workflow-ready requirement docs): `docs/design/briefs/` in the repo. NOT `/tmp`.
- **Design docs**: `docs/` or `docs/design/` in the repo. CLAUDE.md loads by default.
- **Workflow definitions**: `.meridian/workflows/*.yaml`.
- **Agent profiles**: `.meridian/profiles/*.yaml`.
- **Findings from a review**: temporary files inside the worktree are fine during a run, but anything durable goes in the repo.

## Recovery Patterns

- **Clippy exhausts AI budget during `orchestrated-dev`:** run `cargo clippy --fix` directly in the worktree, commit manually, then dispatch `review-and-land` against the same worktree with the remaining findings. Don't re-dispatch a fresh `orchestrated-dev` for mechanical clippy work.
- **Workflow fails mid-run:** inspect with `meridian workflow status <id> --text` first. The auto-notify DM will have fired regardless, but `status` gives you the per-step detail. Decide whether to resume with `review-and-land` against the existing worktree or cancel and start fresh.
- **403 on dispatch:** check that `--as <id>` resolves to a member with a reporting-tree link to the Yggdrasil workspace. Session IDs without that link are rejected.
- **Workflow not found:** check you're running against the yggdrasil repo — the server scans `.meridian/workflows/` relative to the repo root, not a user-level fallback.

## When to Use Which Workflow

- **Greenfield feature work from a brief (default):** `onatopp-dev-lean` with `--worktree --base main`. Three action steps, one check pass. Fastest cycle.
- **Greenfield with more thorough checking:** `onatopp-dev-compact`. Four action steps, three check passes. Use when quality matters more than speed.
- **Mixed-provider (Codex + Claude):** `onatopp-dev-multi` with `--input provider=mixed`. Scout and review via Codex (cheaper), dev via Claude.
- **Apply reviewer findings on an existing branch:** `review-and-land` with `--worktree <existing-path>`.
- **Verify a branch without touching code:** `mechanical-review` or `smell-and-criterion-review`.
- **Stacked-branch land:** `stack-land`.

Discover the current set with `meridian workflow list --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355`.

## Differences From v1 (the `workflows` Skill)

| v1 (`shape workflow …`) | v2 (`meridian workflow …`) |
|---|---|
| `inspect <name>` | `show <name>` |
| `output <id>` / `output <id> <step>` / `--full` / `--json` | `status <id>` (flat: status + steps together) |
| `peek`, `pause`, `resume` | not available |
| `--worksite <name>` / `--worksite auto` | `--worktree [<PATH>]` (different semantics) |
| `--initiator ${CLAUDE_SESSION_ID}` | no flag; auto-notify DM is hard-wired to Waffles + Tom |
| Runs from v1 Meridian server (port 19876) | Runs from v2 Meridian server (port 29876) |

Do not mix. Running `shape workflow run` against the v2 workspace won't work; running `meridian workflow run` with v1 flags like `--worksite` or `--initiator` won't work either.
