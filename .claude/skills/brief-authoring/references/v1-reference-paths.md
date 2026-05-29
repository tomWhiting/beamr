# v1 Reference Paths

Well-known roots in the v1 Meridian tree for brief-researcher to start from. Using these avoids blind searching at the cluster scale.

v1 root: `/Users/tom/Developer/projects/deno_rust/meridian/`

## Crates of primary interest for libcorpus + storage lifts

| v1 crate / path | v2 target | Why it's relevant |
|---|---|---|
| `crates/storage/src/traits/vector_store.rs` | `meridian-storage-vector` trait | Trait lift. The v2 crate is built around this trait shape. |
| `crates/storage/src/models/vector.rs` | `meridian-storage-vector` model/ | VectorCollectionConfig, VectorParamsConfig, SparseVectorParamsConfig, PayloadIndexConfig, PayloadIndexType, VectorDistance, point types, query types. |
| `crates/storage/src/backends/qdrant_server/` | `meridian-storage-vector` backend/qdrant_server/ | Server-mode backend. |
| `crates/storage/src/backends/qdrant_edge/` | `meridian-storage-vector` backend/qdrant_edge/ (feature-gated, experimental) | Embedded backend. v1 had stability issues — document as experimental. |
| `crates/indexer/src/` | libcorpus walker / watcher / CodeProvider | Walker, watcher, symbol extractor. |
| `crates/indexer/src/markdown.rs` | `crates/syntax/src/markdown.rs` (new) | Markdown link extractor — v2 has no equivalent. |
| `crates/services/vector/messages.rs` | messaging-service (not libcorpus) | Message-search lift. Not in scope for libcorpus, but uses the same meridian-storage-vector trait. |
| `crates/services/search/` | libcorpus search / omni-search composition | v1 fuzzy-filename + ripgrep patterns that libcorpus absorbs. |
| `crates/services/indexer/` | libcorpus lifecycle::ingest | Service-side coordinator (the subprocess/split-brain code that libcorpus explicitly rewrites). |
| `crates/lsp/` | yggdrasil `lsp` crate (already present) | v2 lifted this mostly intact. |
| `crates/storage/src/` generally | `meridian-storage` / `meridian-storage-pg` / `meridian-storage-redis` | Storage-abstraction patterns. |

## v1 test fixtures worth knowing

| Path | Use |
|---|---|
| `crates/indexer/tests/` | Walker + symbol-extraction fixtures. |
| `crates/services/vector/tests/` | Qdrant integration test harnesses. |
| `crates/storage/tests/` | Storage-trait test patterns. |

## v1 design / runbook context

| Path | Use |
|---|---|
| `CLAUDE.md` | v1 coding standards (still canonical). |
| `.claude/rules/` | Per-domain rules for v1 (not all still current). |
| `docs/reference/building-coding-agents/` | Methodology series for agent authoring. Highest-signal: 01, 03, 05, 06, 09, 10, 11. |
| `docs/reference/stripe-minions/` | Stripe blueprint-pattern reference (minions-pt-1.md, minions-pt-2.md). |
| `docs/reference/context-and-hooks/` | Hook + system-prompt material. |

## Rules for brief-researcher

- **Always use absolute paths** under `/Users/tom/Developer/projects/deno_rust/meridian/` when referencing v1 in research artefacts. Relative paths are ambiguous across workspaces.
- **Confirm the path exists.** Don't cite a path you haven't `Glob`ed or `Read`.
- **Extract the minimum needed.** Type signatures, function signatures, public interfaces. Not whole files.
- **Flag lift-vs-rewrite boundaries.** If v1 had a design problem the brief explicitly closes (e.g. the subprocess/split-brain indexer), note the v1 file as a reference, not as a lift target.
- **Never reach outside v1 or yggdrasil** without flagging it. External crates are fine to reference by name; internal source outside these two trees is out of bounds.
