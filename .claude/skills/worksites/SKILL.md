---
name: worksites
description: Worksite management for isolated concurrent agent work. Use when provisioning, listing, completing, composing, or landing worksites. Triggered by terms like worksite, provision, compose changes, land changes, isolated work, concurrent work, syntax composition, or layer management.
---

# Worksites

Worksites are isolated working directories backed by git worktrees that enable concurrent agent work. Each worksite is a full checkout where an agent works on files without commits. On completion, the system captures structural AST diffs, auto-positions the worksite in a dependency tree, and lands changes as ordered commits onto the target branch.

**Your session ID:** `${CLAUDE_SESSION_ID}` — use with `--as` in CLI commands that require identity.

---

## Quick Start

```bash
# Provision a new worksite from a branch
shape worksite provision my-feature --base dev

# Provision a child worksite from a parent
shape worksite provision my-child --base my-feature

# Check what's there
shape worksite list
shape worksite files my-feature

# Complete when done
shape worksite complete my-feature --summary "Added search endpoint"

# Confirm (review gate)
shape worksite confirm my-feature

# Land confirmed changes
shape worksite land-confirmed my-feature --target dev
```

---

## Provisioning Flags

The `--base` flag is smart — it detects whether the value is a git branch or an existing worksite:

| Flag | Behaviour |
|------|-----------|
| `--base <name>` | Auto-detects: if `<name>` is a worksite, forks from it (parent-child). If it's a branch, forks from the branch. Errors if ambiguous. |
| `--base-branch <name>` | Force interpretation as a git branch name. |
| `--base-worksite <name>` | Force interpretation as a parent worksite name. |

**Deferred provisioning:** If you provision with `--base <worksite>` and the parent worksite hasn't completed yet, the child is registered as **Pending** — no worktree is created. When the parent completes, the child auto-provisions with the parent's completed state.

---

## CLI Reference

| Command | Purpose |
|---------|---------|
| `shape worksite list` | List all worksites with status |
| `shape worksite provision <name>` | Create a new worksite |
| `shape worksite provision <name> --base <branch-or-worksite>` | Create from a specific base |
| `shape worksite provision <name> --base-branch <branch>` | Create from a git branch (explicit) |
| `shape worksite provision <name> --base-worksite <parent>` | Create as child of another worksite (explicit) |
| `shape worksite provision <name> --description "..."` | Create with description |
| `shape worksite status <id-or-name>` | Show worksite details |
| `shape worksite files <id-or-name>` | Show files changed in a worksite |
| `shape worksite diff <id-or-name>` | Per-file diffs with layer attribution |
| `shape worksite complete <id-or-name>` | Mark worksite as complete |
| `shape worksite complete <id-or-name> --summary "..."` | Complete with summary |
| `shape worksite confirm <name>` | Confirm a positioned worksite (review gate pass) |
| `shape worksite reject <name> -r "reason"` | Reject a positioned/confirmed worksite |
| `shape worksite land-confirmed <name> --target <branch>` | Land a single confirmed change |
| `shape worksite land-confirmed --all --target <branch>` | Land all landable changes in order |
| `shape worksite teardown <id-or-name>` | Remove worksite directory |
| `shape worksite compose` | Compose all completed worksites |
| `shape worksite compose <id1> <id2>` | Compose specific changes |
| `shape worksite graph` | Show the change graph with status icons |
| `shape worksite reposition <name> --under <parent>` | Manually reposition a worksite |
| `shape worksite reconcile` | Fix stale worksite metadata |
| `shape worksite preview-files` | Preview composed files before landing |
| `shape worksite gates <gate-name>` | Run pre-land gates |
| `shape worksite composition state` | Show active composition state |
| `shape worksite composition reorder <layer-ids>` | Reorder composition layers |
| `shape worksite composition toggle <layer-id>` | Toggle a composition layer on/off |

All commands support short ID prefixes (8+ characters) and name-based lookup.

---

## Change Lifecycle

```
Pending → InProgress → Completed → Positioned → Confirmed → Landed
                                                           ↘ Completed (rejection)
```

| Status | Meaning |
|--------|---------|
| **Pending** | Declared but parent hasn't completed. No worktree. Auto-provisions when parent completes. |
| **InProgress** | Agent is working in the worktree. |
| **Completed** | Work done. AST metadata captured, snapshot saved. |
| **Positioned** | Auto-analysed: dependencies determined, base recomputed, layer added to composition. |
| **Confirmed** | Review gate passed. Part of the head composition. Ready to land. |
| **Landed** | Committed to the target branch. Historical record. |

### Graph Display Icons

| Icon | Status |
|------|--------|
| `[~]` | Pending (dim) |
| `[.]` | InProgress (blue) |
| `[+]` | Completed (yellow) |
| `[*]` | Positioned (purple) |
| `[!]` | Confirmed (teal) |
| `[v]` | Landed (green) |

---

## Tree Positioning System

When a worksite completes, the system automatically:

1. **Captures structural metadata** — AST analysis of changed files (functions, types, modules added/modified/deleted)
2. **Analyses dependencies** — compares against all other changes for structural overlap and symbol usage
3. **Positions in the tree** — recomputes the diff against the parent's state via `build_layer`
4. **Adds to composition** — inserts the layer in dependency order

### Two Dependency Modes

- **Declared** (`--base <worksite>`): Explicit parent-child. The child forks from the parent's state.
- **Auto**: Discovered at completion time via structural overlap analysis. The system determines where the worksite fits.

### The Head

The head = composed state of all confirmed work. New worksites provision from the head by default (when no `--base` is specified), getting all confirmed changes in their worktree.

### Selective Landing

Landing is per-worksite: `shape worksite land-confirmed <name> --target <branch>`. A worksite is landable when it's Confirmed and all its dependencies are Landed. Use `--all` to land everything landable in dependency order.

---

## Workflow Integration

```bash
# Run a workflow in a named worksite
shape workflow run dev-cycle --brief "Add search" --worksite my-feature

# Auto-generate worksite name
shape workflow run dev-cycle --brief "Add search" --worksite auto
```

---

## Architecture

- **Crate**: `crates/worksites/` — git backend, change graph, composition, AST engine, placement
- **CLI**: `crates/shapesmith/src/cli/commands/worksite.rs`
- **Server API**: `crates/server/src/api/worksites.rs` (30+ endpoints)
- **UI**: `apps/web/src/features/worksites/` (tree view, graph view, diff panel)

### Key Patterns

- All graph-mutating operations serialize through `graph_mutation_lock` (Tokio Mutex)
- Change IDs are stable worksite names (never change). Git commit hashes stored in `commit_ref`.
- Snapshots at `.meridian/worksites/snapshots/{change_id}/` persist modified files for landing after worktree teardown
- Change graph persisted to `.meridian/worksites/change_graph.json`, rebuilt from metadata on startup
- Landed/Confirmed entries survive worksite teardown (historical record)
