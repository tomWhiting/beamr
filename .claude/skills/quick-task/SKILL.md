---
name: quick-task
description: Dispatch a focused code change to Norn via the quick-task workflow. Use when you need to make a small, well-defined code change — a bug fix, a wiring change, a config update — without the full brief pipeline. Triggered by terms like quick task, quick fix, dispatch task, quick change, make this change, norn task.
---

# Quick Task Dispatch

Dispatch a focused code change to Norn via the `quick-task` workflow. This is the lightweight alternative to `onatopp-dev` — one step, no profile, no scout/plan/review pipeline. You describe the task with requirements and acceptance criteria, Norn makes the changes, runs verification, commits, and reports back.

## When to Use

- Small, well-defined code changes (bug fixes, wiring, config)
- Changes where you already know what needs to happen
- Tasks that don't need the full brief/scout/plan/dev/review pipeline
- When a team member needs a code change made quickly

## When NOT to Use

- Large multi-requirement briefs (use `onatopp-dev-norn` or `onatopp-dev-hybrid`)
- Changes requiring design review or architectural decisions
- Work that needs a worktree (quick-task runs on main directly)

## Command

```sh
meridian workflow run quick-task \
  --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --input "task=$(cat task.json)" \
  --input "notify=${CLAUDE_SESSION_ID}"
```

## Task JSON Format

Save the task file at `.meridian/tasks/<name>.json` relative to the repository root. Name it after the change — lowercase, hyphenated (e.g. `guard-boundaries-loop.json`, `wire-jsonl-persistence.json`). This keeps task definitions versioned with the repo and discoverable by other team members.

```json
{
  "title": "Short description of the change",
  "task": "Clear articulation of the task and desired outcome",
  "requirements": [
    {
      "id": "R1",
      "description": "What needs to change",
      "acceptance": "How to verify it worked"
    },
    {
      "id": "R2",
      "description": "Second change if needed",
      "acceptance": "Verification criteria"
    }
  ],
  "context": "Relevant context — files to look at, related code, motivation for the change"
}
```

The dispatch command references the file relative to the repo root:

```sh
meridian workflow run quick-task \
  --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --input "task=$(cat .meridian/tasks/guard-boundaries-loop.json)" \
  --input "notify=${CLAUDE_SESSION_ID}"
```

### Fields

- **title** (required): Short name for the task, used in commit messages and notifications
- **task** (required): Clear articulation of what needs to happen and why — the overall objective in plain language
- **requirements** (required): Array of requirements, each with:
  - **id**: Identifier (R1, R2, etc.)
  - **description**: What needs to be done
  - **acceptance**: How to verify it's correct
- **context** (required): Relevant background — file paths, line numbers, related code, motivation for the change

## What Happens

1. Norn receives the task with all requirements
2. Makes the code changes
3. Runs `cargo check`, `cargo clippy -- -D warnings`, `cargo test` on affected crates
4. Produces structured output reporting pass/fail against each requirement
5. DMs the results back to you (the dispatcher) via collective
6. Changes are left uncommitted — you review with `git diff` and commit when satisfied

## Structured Output

The workflow returns per-requirement reporting:

- **summary**: What was done (one paragraph)
- **requirements**: Array with id, status (pass/fail/partial), what_changed, files_touched
- **concerns**: Anything the agent couldn't resolve
- **commit_message**: The commit message used
- **verification**: cargo check/clippy/test status (pass/fail/skipped)

## Important Notes

- **Runs on main directly** — no worktree, no branch. Changes are left uncommitted for you to review.
- **No review step** — you are the reviewer. Run `git diff` after the notification arrives, then commit when satisfied.
- **No profile** — vanilla Norn (gpt-5.5). The structured output schema enforces the reporting format.
- **Notify goes to the dispatcher** — use `--input "notify=${CLAUDE_SESSION_ID}"` so the results come back to you.

## Example

Save as `.meridian/tasks/guard-boundaries-loop.json`:

```json
{
  "title": "Guard brief.boundaries iteration in review instruction builder",
  "task": "Both onatopp workflow scripts crash when a brief has no boundaries field. Add a null guard around the boundaries for-loop in the review instruction section, matching the pattern already used for brief.verification.",
  "requirements": [
    {
      "id": "R1",
      "description": "Add null check before iterating brief.boundaries in onatopp-dev-norn/workflow.rhai review section",
      "acceptance": "for loop wrapped in if brief.boundaries != () guard"
    },
    {
      "id": "R2",
      "description": "Apply same fix to onatopp-dev-hybrid/workflow.rhai",
      "acceptance": "Both workflows have matching guards"
    }
  ],
  "context": "Line 655 in onatopp-dev-norn, line 660 in onatopp-dev-hybrid. The verification section (brief.verification) already has this guard — match that pattern."
}
```
