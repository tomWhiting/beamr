# Artifact schemas

Every document in this repo follows a defined shape. This file defines those shapes so any contributor or agent can produce conforming artifacts and any reviewer can verify them mechanically.

## Architecture doc

Path: `docs/architecture/NN-component-name.md`

```
# Component Name

## How BEAM does it
- Source analysis from OTP/erts (cite files: erl_term.h, beam_emu.c, etc.)
- Data structures and algorithms used
- Performance characteristics

## How alternatives attempted it
- Per-project analysis (Lumen, AtomVM, Lunatic, etc.)
- What they changed from stock BEAM
- Successes and failures

## Known limitations
- Documented flaws in BEAM's approach
- Academic papers, community reports, production incidents
- Performance bottlenecks under specific workloads

## How beamr does it
- Our approach in pseudocode (no Rust)
- Data structures, algorithms, invariants
- What we keep from BEAM, what we change, and why

## Improvements over BEAM
- Specific changes justified by lessons learned
- Each improvement traces to a known limitation or alternative's success

## Component interactions
- What this component depends on
- What depends on this component
- Data flow between components

## Pseudocode
- Complete pseudocode for the component's public API
- Internal algorithms with step-by-step logic
- Error handling paths
```

## Implementation brief

Path: `docs/design/beamr/briefs/B-NNN.json`

Schema: JSON with fields:
- `id`: "B-NNN"
- `cluster`: "beamr"
- `title`: one-line description
- `depends_on`: array of brief IDs this brief requires
- `blocked_by`: array of brief IDs that must complete first
- `checklist`: array of C-numbers from checklist.json (section must match component)
- `stories`: array of S-numbers from stories.json
- `purpose`: why this brief exists (1-2 paragraphs)
- `task`: what to implement (1-2 paragraphs)
- `requirements`: array of R# objects, each with:
  - `id`, `title`, `spec` (SHALL language)
  - `acceptance`: array of testable criteria
  - `files`: `{create: [], modify: []}` with exact paths
  - `checklist`: subset of brief-level checklist
  - `stories`: subset of brief-level stories
- `boundaries`: array of "SHALL NOT" scope limits
- `verification`: array of commands/inspections to verify

Validation rules:
- Brief-level checklist == union of all requirement-level checklists
- Every C-number must exist in checklist.json
- Every C-number must belong to the section matching the brief's component
- No C-number claimed by more than one brief
- Every S-number must exist in stories.json

## Architecture decision record

Path: `docs/adr/NNN-kebab-case-title.md`

```
# ADR-NNN: Title

**Status:** Accepted | Superseded by ADR-MMM
**Date:** YYYY-MM-DD

## Context
What forces are at play. What options were considered.

## Decision
What we chose and why.

## Consequences
**Positive:** what this enables
**Negative:** what this costs or limits
```

## Research document

Path: `docs/architecture/00-topic-name.md`

Survey or analysis documents. Must include:
- Executive summary (1 paragraph)
- Structured per-item analysis
- Lessons for beamr (explicit, actionable)
- Source index (links, citations)

## Governance document

Path: `docs/governance/NAME.md`

Meta-documents about how the project runs. Self-referential: this file is a governance document.

## Code file

Path: `crates/*/src/**/*.rs`

Constraints:
- Under 500 lines
- No `.unwrap()` or `.expect()` outside `#[cfg(test)]`
- Module-level inner doc comments (`//!`) not outer doc comments (`///`) on scaffold functions
- `BEAM:` comment prefix for non-obvious BEAM semantics
- Explicit error types, never panics
- Tests in `#[cfg(test)] mod tests` block within the same file
