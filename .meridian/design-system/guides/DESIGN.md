# Writing a Design Document

A design document is the architectural anchor for a cluster. Every brief,
every implementation decision, and every review question traces back to this
document. It should tell you WHY the system works the way it does, not just
WHAT it does.

## Format

Markdown with YAML frontmatter. Target around 400 lines. Long enough to be
a genuine reference; short enough that an agent or human can read the whole
thing before writing a brief.

```yaml
---
type: design
cluster: {cluster-name}
title: {Human-Readable Title}
---
```

## Sections

The sections below are in the order they should appear. Every section is
required unless marked optional.

### Intention

The spirit of the work — what we're trying to bring about beyond the
immediate technical outcome. What does the world look like when this is
done? How should it feel to use?

This is the philosophical anchor. When a brief author has to make a
judgment call that the design doc doesn't explicitly address, this section
is what they check against. One to three paragraphs.

### Problem

What's broken or missing. Who it affects. Why it matters now rather than
later. Be specific about the pain — vague problems produce vague solutions.

### Solution

The design itself. This is the largest section and the one most frequently
referenced during review.

**What to include:**

- Module layout and how the pieces fit together.
- Integration points with other crates or systems.
- Key decisions. Every decision that involves a meaningful trade-off should
  state what was chosen, why, and what was rejected. If there is no rejected
  alternative, it wasn't a real decision — it was an obvious step that
  doesn't need discussion.

**What to avoid:**

- Code snippets and type signatures. Those belong in the implementation and
  in briefs. The design doc describes structure and rationale, not syntax.
- Rationalising standard dependency choices (serde, thiserror, uuid). Only
  discuss dependencies when the choice is non-obvious or constrained.
- Repeating information from other sections. If the constraint is stated in
  Constraints, don't restate it in the solution — reference it.

**Numbered decisions:**

If the cluster has more than a handful of design decisions, number them
(D1, D2, ... or use a prefix like "Decision:"). Numbered decisions are the
single most referenced artefact during brief review — they settle arguments
by reference rather than re-litigation. Every brief can cite "per D7" and
the reviewer can verify in seconds.

**Design principles** (optional within Solution):

If the cluster has a small set of governing principles (5-10), state them
explicitly. Principles like "contract is source of truth" or "no silent
fallbacks" resolve review questions before they're asked.

### Goals

What success looks like, in concrete terms. Each goal should be verifiable —
you can tell whether it was achieved. Keep to 3-7 goals.

Good: "All 19 individual store traits compile and pass existing PG tests
against the new import paths."

Bad: "The system should be well-structured." (Not verifiable.)

### Non-Goals

Reasonable things we are deliberately not doing in this round. Each non-goal
should be something a reader might expect to be in scope. Include why it's
excluded if the reason isn't obvious.

Non-goals protect scope. When a brief author wonders "should I also do X?",
they check here first.

### Structure

The file layout the implementation should follow. A tree diagram with one-
line descriptions per file or module.

```
crates/{crate-name}/
├── src/
│   ├── lib.rs              — public API surface
│   ├── error.rs            — crate error types
│   ├── {module}/
│   │   ├── mod.rs          — pub mod + pub use only
│   │   └── ...
│   └── ...
└── Cargo.toml
```

This section is essential, not optional. During review, every file path in
every brief is verified against this structure. If the structure section
doesn't exist or is incomplete, every file path is an unverifiable claim.

**Annotate which brief introduces which file** when the cluster has more
than a few briefs. This is invaluable for planning the cluster split and
checking that no two briefs claim to create the same file.

### Current Inventory (if applicable)

When the cluster builds on existing code, document what exists before you
document what's new. Tables showing what exists per entity per layer are
how reviewers verify "What Exists Already" claims in briefs.

This section prevents the most common existing-code mistake: authors writing
briefs from the design doc's file structure rather than from the actual
codebase, then declaring files as NEW when they already exist.

Optional for greenfield clusters. Essential for porting or extraction work.

### Constraints

Things that must not change. Security requirements. Performance
expectations. Compatibility guarantees. Cross-cutting concerns
(observability, error handling conventions, workspace scoping).

Be specific. "Good performance" is not a constraint. "Sub-millisecond path
normalization for files under 1000 characters" is.

## What Makes a Design Doc Useful

From 40+ brief reviews across three clusters, the patterns are clear:

1. **Decisions with rationale are the most-referenced section.** Without
   numbered decisions, every review devolves into "was this intentional?"
   With them, the reviewer cites D7 and moves on.

2. **File structure is the second most useful section.** Every R# file path
   is checked against it. Missing or incomplete structure means unverifiable
   briefs.

3. **Intention anchors judgment calls.** Brief authors make dozens of
   small decisions the design doc doesn't explicitly cover. The Intention
   section tells them what the right default is.

4. **Current inventory prevents false claims.** "File X is NEW" when it
   already exists is the second most common review finding. An inventory
   section kills this category entirely.

## What Makes a Design Doc Useless

- Prose that describes what the code does without saying why.
- Solutions that read like a feature list without integration context.
- Missing file structure (forces reviewers to guess at paths).
- Decisions buried in paragraphs instead of being scannable.
- Repeating the same information in multiple sections.
- Code snippets that belong in briefs, not design docs.
