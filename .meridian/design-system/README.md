# Design System

Standard format for design documents, checklists, user stories, and implementation briefs across all clusters.

## Why

We were burning too many tokens passing verbose, redundant context between workflow stages. Design docs, checklists, user stories, and briefs were all Markdown with inconsistent formats across clusters. Cross-referencing required agents to read three separate documents. Structured output from each stage was forwarded wholesale to the next, inflating prompts to 25K+ tokens where 5-8K would do.

This system standardises the formats, makes everything parseable, and establishes a single source of truth for coverage tracking.

## Documents

Each cluster produces four documents:

| Document | Format | Purpose |
|----------|--------|---------|
| DESIGN.md | Markdown with YAML frontmatter | Architectural anchor — intention, problem, solution, goals, non-goals, structure, constraints |
| checklist.json | JSON | Verifiable requirements for the whole cluster, grouped by section. Items numbered C1, C2, C3... |
| stories.json | JSON | User stories grouped by persona. Stories numbered S1, S2, S3... |
| briefs/{ID}.json | JSON | Implementation units. Each brief maps to checklist items and stories via its arrays. Requirements numbered R1, R2, R3 per brief. |

## File Layout

```
docs/design/{cluster}/
  DESIGN.md
  checklist.json
  stories.json
  briefs/
    M-001.json
    M-002.json
    ...
```

## ID Scheme

- **C-numbers**: checklist items, sequential per cluster (C1, C2, C3...)
- **S-numbers**: user stories, sequential per cluster (S1, S2, S3...)
- **R-numbers**: requirements, sequential per brief (R1, R2, R3...)
- **Brief IDs**: cluster prefix + number (M-001, D-005, X-003)

## Guides

Authoring guidance in `guides/`. These explain what goes into each document type, what to include, what to avoid, and what depth to aim for — informed by patterns from 40+ brief reviews across three clusters.

| Guide | What it covers |
|-------|---------------|
| [DESIGN.md](guides/DESIGN.md) | Design doc structure, numbered decisions, file structure, current inventory |
| [BRIEF.md](guides/BRIEF.md) | Brief format, EARS notation, acceptance criteria, scope control, cross-referencing |
| [CHECKLIST.md](guides/CHECKLIST.md) | Checklist items — verifiable requirements, precise scope, stable wording |
| [USER-STORIES.md](guides/USER-STORIES.md) | User stories — standard format, outcome over implementation, persona discipline |

## Scripts

All in `scripts/`. Run with `python3`.

| Script | Usage | What it does |
|--------|-------|-------------|
| render-cluster.py | `render-cluster.py <cluster-dir>` | Renders all JSON in a cluster to Markdown, with resolved C/S references in briefs |
| render-checklist.py | `render-checklist.py <file.json>` | Renders a single checklist |
| render-stories.py | `render-stories.py <file.json>` | Renders a single stories file |
| render-brief.py | `render-brief.py <file.json>` | Renders a single brief with reference resolution |
| check-coverage.py | `check-coverage.py <cluster-dir>` | Reports unmapped checklist items and stories, flags unknown IDs, shows dependency chain |

### Reference Resolution

`render-brief.py` resolves checklist (C-number) and story (S-number) references to their actual text. It auto-detects the cluster directory from the brief's location (parent of `briefs/`) or accepts an explicit `--cluster-dir` argument.

Without resolution: `Checklist: C4`
With resolution: `C4 — StorageError defined in libmessage::storage::error`

Use `--no-resolve` to skip reference resolution.

## Schemas

JSON Schema files in `schemas/` define the structure for validation:

- `checklist.schema.json`
- `stories.schema.json`
- `brief.schema.json`

## Coverage Tracking

The brief's `checklist` and `stories` arrays are the single source of truth for what's assigned. Run `check-coverage.py` against a cluster directory to find:

- Checklist items not in any brief
- User stories not in any brief
- Unknown IDs (brief references something that doesn't exist in the checklist/stories)
- Items in multiple briefs

## Examples

Working examples in `examples/` using messaging cluster data. Run `render-cluster.py examples/` to see the Markdown output with resolved references.
