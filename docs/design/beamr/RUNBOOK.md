# beamr Dispatch Runbook

All commands run from the beamr repo root. Each creates its own worktree.
Run everything in a code block concurrently.

Script: `.meridian/workflows/onatopp-dev-norn/benchmark.sh`
Args: `<brief.json> [design.json] [worktree-name] [checklist.json] [stories.json] [notify-member]`

Shorthand used below:
```
S=".meridian/workflows/onatopp-dev-norn/benchmark.sh"
B="docs/design/beamr/briefs"
C="docs/design/beamr/checklist.json"
U="docs/design/beamr/stories.json"
```

## Wave 1 — No dependencies (DONE)

```bash
# B-001: workspace, crate structure, error types — LANDED
```

## Wave 2 — Depends on B-001

```bash
bash $S $B/B-002.json "" b-002 $C $U "Bono" &
bash $S $B/B-005.json "" b-005 $C $U "Bono" &
```

## Wave 3 — Depends on B-002 + B-005

```bash
bash $S $B/B-003.json "" b-003 $C $U "Bono" &
bash $S $B/B-006.json "" b-006 $C $U "Bono" &
bash $S $B/B-008.json "" b-008 $C $U "Bono" &
```

## Wave 4 — Depends on B-003 + B-006 + B-008

```bash
bash $S $B/B-004.json "" b-004 $C $U "Bono" &
bash $S $B/B-007.json "" b-007 $C $U "Bono" &
bash $S $B/B-010.json "" b-010 $C $U "Bono" &
```

## Wave 5 — Depends on B-004 + B-010

```bash
bash $S $B/B-009.json "" b-009 $C $U "Bono" &
bash $S $B/B-011.json "" b-011 $C $U "Bono" &
bash $S $B/B-017.json "" b-017 $C $U "Bono" &
bash $S $B/B-021.json "" b-021 $C $U "Bono" &
```

## Wave 6 — Depends on B-011 + B-021

```bash
bash $S $B/B-012.json "" b-012 $C $U "Bono" &
bash $S $B/B-013.json "" b-013 $C $U "Bono" &
```

## Wave 7 — Depends on B-012 + B-013

```bash
bash $S $B/B-014.json "" b-014 $C $U "Bono" &
bash $S $B/B-015.json "" b-015 $C $U "Bono" &
bash $S $B/B-016.json "" b-016 $C $U "Bono" &
bash $S $B/B-018.json "" b-018 $C $U "Bono" &
bash $S $B/B-019.json "" b-019 $C $U "Bono" &
bash $S $B/B-020.json "" b-020 $C $U "Bono" &
```

## Between waves

Each wave must land before the next can start. "Land" means:
1. Verify the branch passes (`cargo clippy --workspace -- -D warnings && cargo test --workspace`)
2. Merge into main: `git merge benchmark/<name>`
3. Clean up: `git worktree remove .yggdrasil-worktrees/<name> && git branch -D benchmark/<name>`

## Cleanup shortcut

```bash
for wt in .yggdrasil-worktrees/b-0*; do
  name=$(basename "$wt")
  cargo clean --manifest-path "$wt/Cargo.toml" 2>/dev/null
  git worktree remove "$wt" 2>/dev/null
  git branch -D "benchmark/$name" 2>/dev/null
done
```
