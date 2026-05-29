# Design-Doc Conventions

How yggdrasil's design folders are structured, and how `brief-researcher` and `brief-writer` read them.

## Design folder layout

Each major crate or subsystem has a design folder under `docs/design/<name>/` containing three files in a fixed shape:

```
docs/design/<name>/
├── DESIGN.md         — prose architecture
├── CHECKLIST.md      — letter-grouped numbered commitments
└── USER-STORIES.md   — per-persona outcomes
```

Optional sibling under the same folder:

```
docs/design/<name>/briefs/
├── PLAN.md           — live per-brief state for this cluster
├── INDEX.md          — summary index of authored briefs
└── .research/        — per-brief research artefacts (not hand-written)
```

## DESIGN.md — anchors and sections

`DESIGN.md` uses Markdown `##` and `###` headings as section anchors. Sections are stable — brief-writer references them by exact heading text. Common top-level sections:

- The Problem
- What <subject> Is
- Placement / Where It Sits
- Design Principles
- The Storage Model / Storage Abstraction
- Data Model
- Module Layout
- Public API
- What's Out of Scope
- Open Questions
- Summary

Inside each section, code blocks (fenced ```` ``` ````) carry type signatures and module-layout trees. `brief-researcher` extracts these verbatim into the research artefact. `brief-writer` then lifts them into R# bodies, not paraphrases.

## CHECKLIST.md — numbering and coverage

`CHECKLIST.md` is organised by letter-grouped sections (A through T typically) with numbered items:

```
## A. Storage Abstraction Crates

- [ ] **A1. `meridian-storage-vector` crate.** ...
- [ ] **A2. Qdrant server backend.** ...

## B. Core Library Skeleton

- [ ] **B1. ...**
```

Conventions:

- **Ids are `<LETTER><NUMBER>`**, e.g. `A1`, `B3`, `C12`.
- **Each item is one commitment**, not a feature bundle. "Add trait + two backends" is three items.
- **The checklist is the source of truth for brief coverage.** Every item must end up cross-referenced by ≥1 brief's R#.
- **Checkboxes stay unchecked until the brief realising the item ships** — `brief-writer` does NOT tick them; that's the workflow's job post-implementation.

## USER-STORIES.md — personas and ids

`USER-STORIES.md` is organised by persona:

- Human — Developer (H-D-1, H-D-2, ...)
- Human — Researcher / Auditor (H-R-1, ...)
- Human — Team Lead / Admin (H-A-1, ...)
- AI — Autonomous Agent (AI-D-1, AI-W-1, AI-R-1, ...)
- Consumer-specific (L-1 for libcorpus-as-consumer, M-1 for messaging, P-1 for project-items)
- Cross-cutting (X-1, X-2)

Each story uses the "As <persona>, I want <outcome> so that <value>" shape. brief-writer cross-references stories by id, not by restating them.

## Cross-doc linking

- DESIGN.md typically references CHECKLIST.md and USER-STORIES.md by path (`[CHECKLIST.md](./CHECKLIST.md)`), but not by specific id.
- CHECKLIST.md references DESIGN.md sections in its preamble and each item may reference a DESIGN.md section for full context.
- USER-STORIES.md stands mostly alone; the design doc quotes stories by id when relevant.
- Briefs reference all three: `Context:` block cites DESIGN.md section, R#s cite CHECKLIST ids, User Stories block cites USER-STORIES ids.

## How brief-researcher reads these

1. **DESIGN anchor.** Use `Grep` to locate the section heading; read that section and its sub-sections only. Don't scrape the whole doc — brief-writer doesn't need the whole doc either.
2. **CHECKLIST items.** Pull the item text verbatim for every id in the brief spec. If an id is not found, flag it.
3. **USER-STORIES items.** Same shape for story ids.

## How brief-writer uses these

1. **Lift type signatures verbatim** from DESIGN.md code blocks into R# bodies.
2. **Quote checklist items** in the R# `**Realises:**` line by id — don't paraphrase.
3. **Reference user stories** by id in the User Stories block — don't restate them.
4. **Cite DESIGN.md sections** in the `Context:` block with the exact heading text.

## Drift handling

If brief-researcher finds that DESIGN.md says X but CHECKLIST.md says Y, that's drift. It goes in the research artefact's Open Questions section as a `blocker`. brief-authoring does NOT author a brief against drifted content — the human resolves the drift first.
