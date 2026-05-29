# Dispatch Runbook — Norn Headless CLI

All commands run from the yggdrasil root. Each creates its own worktree.
Run as many in parallel as the machine can handle.

Script: `.meridian/workflows/onatopp-dev-norn/benchmark.sh`
Args: `<brief.json> [design.json] [worktree-name] [checklist.json] [stories.json] [notify-member]`

Last arg is the member name to DM when the brief completes.

## Wave 1 — No Dependencies (run all in parallel)

### Corpus Integration (Muffin MD reviews, Widget grills)

```bash
# CI-004: Quick-open handler (depends CI-003b — LANDED)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/corpus-integration/briefs/CI-004.json \
  docs/design/corpus-integration/design.json \
  ci-004 \
  docs/design/corpus-integration/checklist.json \
  docs/design/corpus-integration/stories.json \
  "Muffin, MD" &

# CI-005: Full search handler (depends CI-002+CI-003b — both LANDED)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/corpus-integration/briefs/CI-005.json \
  docs/design/corpus-integration/design.json \
  ci-005 \
  docs/design/corpus-integration/checklist.json \
  docs/design/corpus-integration/stories.json \
  "Muffin, MD" &

# CI-008: CLI subcommands (depends CI-003a — LANDED)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/corpus-integration/briefs/CI-008.json \
  docs/design/corpus-integration/design.json \
  ci-008 \
  docs/design/corpus-integration/checklist.json \
  docs/design/corpus-integration/stories.json \
  "Muffin, MD" &

# CI-007a: Graph snapshot REST endpoint (depends CI-003a — LANDED)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/corpus-integration/briefs/CI-007a.json \
  docs/design/corpus-integration/design.json \
  ci-007a \
  docs/design/corpus-integration/checklist.json \
  docs/design/corpus-integration/stories.json \
  "Muffin, MD" &
```

### Exchange Workspaces (Cally Ray reviews, Furious George grills)

```bash
# W-007b: Workspace audit + Merkle tamper evidence (depends W-007a — LANDED)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/exchange-workspaces/briefs/W-007b.json \
  docs/design/exchange-workspaces/design.json \
  w-007b \
  docs/design/exchange-workspaces/checklist.json \
  docs/design/exchange-workspaces/stories.json \
  "Cally Ray" &
```

### LSP-Diagnostics (Xenia reviews, Gumby grills)

```bash
# LD-001: Server core — adapter registry + diagnostic pipeline
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/lsp-diagnostics/briefs/LD-001.json \
  docs/design/lsp-diagnostics/design.json \
  ld-001 \
  docs/design/lsp-diagnostics/checklist.json \
  docs/design/lsp-diagnostics/stories.json \
  "Xenia Onatopp" &

# LD-005: LangDef + conventions relocation
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/lsp-diagnostics/briefs/LD-005.json \
  docs/design/lsp-diagnostics/design.json \
  ld-005 \
  docs/design/lsp-diagnostics/checklist.json \
  docs/design/lsp-diagnostics/stories.json \
  "Xenia Onatopp" &

# LD-006: Rule struct + tool/adapter resolution
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/lsp-diagnostics/briefs/LD-006.json \
  docs/design/lsp-diagnostics/design.json \
  ld-006 \
  docs/design/lsp-diagnostics/checklist.json \
  docs/design/lsp-diagnostics/stories.json \
  "Xenia Onatopp" &

# LD-007: TOML parser unification
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/lsp-diagnostics/briefs/LD-007.json \
  docs/design/lsp-diagnostics/design.json \
  ld-007 \
  docs/design/lsp-diagnostics/checklist.json \
  docs/design/lsp-diagnostics/stories.json \
  "Xenia Onatopp" &
```

## Wave 2 — After Wave 1 Lands

### Corpus Integration

```bash
# CI-007b: WebSocket graph delta events (depends CI-007a)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/corpus-integration/briefs/CI-007b.json \
  docs/design/corpus-integration/design.json \
  ci-007b \
  docs/design/corpus-integration/checklist.json \
  docs/design/corpus-integration/stories.json \
  "Muffin, MD" &
```

### Exchange Workspaces

```bash
# W-007c: UI API + CLI + VM session + focus routing (depends W-007a+W-007b)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/exchange-workspaces/briefs/W-007c.json \
  docs/design/exchange-workspaces/design.json \
  w-007c \
  docs/design/exchange-workspaces/checklist.json \
  docs/design/exchange-workspaces/stories.json \
  "Cally Ray" &
```

### LSP-Diagnostics (remaining 12 briefs)

```bash
# LD-002: Subprocess adapter (depends LD-001)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/lsp-diagnostics/briefs/LD-002.json \
  docs/design/lsp-diagnostics/design.json \
  ld-002 \
  docs/design/lsp-diagnostics/checklist.json \
  docs/design/lsp-diagnostics/stories.json \
  "Xenia Onatopp" &

# LD-008: Debounced file watcher (depends LD-001)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/lsp-diagnostics/briefs/LD-008.json \
  docs/design/lsp-diagnostics/design.json \
  ld-008 \
  docs/design/lsp-diagnostics/checklist.json \
  docs/design/lsp-diagnostics/stories.json \
  "Xenia Onatopp" &

# LD-010: Pattern adapter (depends LD-001)
bash .meridian/workflows/onatopp-dev-norn/benchmark.sh \
  docs/design/lsp-diagnostics/briefs/LD-010.json \
  docs/design/lsp-diagnostics/design.json \
  ld-010 \
  docs/design/lsp-diagnostics/checklist.json \
  docs/design/lsp-diagnostics/stories.json \
  "Xenia Onatopp" &
```

## Wave 3+ — Deep Dependencies

See LD dispatch plan in docs/design/lsp-diagnostics/ for waves 3-5.
Each depends on earlier LD briefs landing first.

## Cleanup

After each brief completes and passes review:
```bash
cargo clean --manifest-path .yggdrasil-worktrees/<name>/Cargo.toml
git worktree remove .yggdrasil-worktrees/<name>
git branch -D benchmark/<name>
```
