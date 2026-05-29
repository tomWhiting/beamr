# Brief Template

This is the canonical shape of a yggdrasil implementation brief. `brief-writer` produces files matching this template exactly. Section headings and ordering are load-bearing — do not reorder.

Replace `<NN>`, `<TITLE>`, and all `<PLACEHOLDER>` content. Remove this preamble and template comments when writing a real brief.

---

```markdown
# Brief <NN> — <TITLE>

> **Context:** See `<DESIGN_DOC_RELATIVE_PATH>` section `<DESIGN_ANCHOR>`.
> Realises checklist items <CHECKLIST_IDS>. Satisfies user stories <STORY_IDS>.
> Prerequisite briefs: <PREREQ_BRIEF_LIST_OR_NONE>.

## User Stories

<Write 1-3 short user-story blocks describing the outcomes this brief delivers.
Cross-reference USER-STORIES.md ids where applicable, in the form:

**As <persona>**, I want <outcome> so that <value>. (USER-STORY <ID>)

Minimum one block per distinct persona touched by this brief.>

## Why

<1-3 paragraphs. What problem does this brief close? Why does it come at this
point in the cluster? How does it fit against prerequisite briefs and against
the broader cluster arc? Reference DESIGN.md section(s) by anchor.>

## What Exists Already

<What code is already in place that this brief builds on, modifies, or avoids
duplicating.

- Relevant yggdrasil crates / modules / files with absolute or yggdrasil-relative
  paths, e.g. `crates/libyggd/src/worktree/provision.rs:30-108`.
- Relevant v1 files with full absolute paths under
  `/Users/tom/Developer/projects/deno_rust/meridian/`.
- Sibling-crate patterns this brief should match, with citations.

Do not paraphrase code content here — cite it. Paraphrasing is the brief-writer's
liability; citing is the workflow-developer's responsibility.>

## Rule (non-negotiable)

<One-liner stating the hard constraint the brief enforces. Typical forms:
"No changes to the existing X trait." / "No new dependency on Y."
/ "Tests live in crates/<crate>/tests/ — no unit-test hacks in src/."
Optional section — omit if no single non-negotiable applies.>

## Requirements

### R1. <Short imperative statement>

**File(s):** <Absolute-from-workspace-root or sibling-relative file paths this
requirement creates or modifies.>

<Narrative description of the requirement. Include type signatures, function
signatures, module layouts lifted verbatim from DESIGN.md where possible.

Example:

```rust
pub struct CorpusConfig {
    pub embedding_endpoint: Url,
    pub qdrant_url: Url,
    pub memgraph_url: Option<Url>,
    ...
}
```

Every detail that's load-bearing must be here. If the workflow-developer has to
go back to DESIGN.md to know the type shape, the brief failed.>

**Acceptance criteria:**

- <Testable statement 1, e.g. "`cargo build -p libcorpus` succeeds.">
- <Testable statement 2, e.g. "A unit test at `crates/libcorpus/src/config.rs`
  verifies default values match the design doc.">
- <Additional criteria as needed.>

**Realises:** CHECKLIST <IDS>.
**Satisfies:** USER-STORIES <IDS>.

### R2. <Next requirement>

<Same shape.>

### ... RN.

<Every requirement has the same structure: imperative statement, file(s),
description with type signatures, acceptance criteria, checklist + story
cross-refs. No requirement may skip any subsection.>

## Out of Scope

<Explicit list of things this brief does NOT do, to prevent the workflow-
developer from over-reaching:

- <Thing A is out of scope because it belongs to brief NN+k.>
- <Thing B is not something this brief touches even though it's in the same
  crate — it lands in a future brief.>
- <Thing C is out of scope because the design explicitly defers it.>

This section keeps scope honest at the 40-60-brief scale. Without it, the
workflow-developer's adversarial hardening pass tends to stretch.>

## Acceptance

<Cross-cutting acceptance criteria for the brief as a whole — things no single
R# owns but the brief must deliver. Typical forms:

- `cargo clippy --workspace -- -D warnings` green for the affected crate.
- `cargo test -p <crate>` green.
- No file exceeds 500 lines of code (file size check).
- No code in `mod.rs` beyond `pub mod` declarations + re-exports.
- All new modules referenced from crate `lib.rs`.>

## Files Likely to Change

<Concrete path list the workflow's Scout step can use to bound its exploration.
This also feeds the orchestrator's parallelism check — two parallel briefs
touching overlapping paths serialise.

- `crates/<crate>/src/<file>.rs` (new)
- `crates/<crate>/Cargo.toml` (modified — new dep)
- `crates/<crate>/tests/<test>.rs` (new)
- ...>

## Open Questions

<Anything the brief-writer couldn't resolve from the research artifact. Each
item flagged either "blocker" (human resolves before dispatch) or "flag"
(workflow-developer proceeds with the listed assumption and flags in the
requirement_status).

- Blocker: <question> — <context>
- Flag: <question> — <assumption the workflow can proceed with>
- ...

Empty is fine; in fact empty is good.>

## References

<External or cross-cluster references relevant to this brief:

- DESIGN.md sections beyond the primary anchor that this brief reads.
- Related briefs in other clusters (e.g. a libcorpus brief referencing a
  meridian-storage-graph brief that must land first).
- External documentation for libraries this brief depends on.
- Research artefacts at `docs/research/` if relevant.>
```

---

## Template-Compliance Checklist (for brief-reviewer)

A conformant brief must:

- [ ] Open with `# Brief <NN> — <TITLE>` and a `> **Context:**` block.
- [ ] Have User Stories, Why, What Exists Already, Requirements, Out of Scope, Acceptance, Files Likely to Change, Open Questions, References sections in that order.
- [ ] Every R# has: imperative heading (`### R<N>. <statement>`), `**File(s):**` subsection, narrative with type signatures where applicable, `**Acceptance criteria:**` list, `**Realises:**` checklist cross-refs, `**Satisfies:**` story cross-refs.
- [ ] Every CHECKLIST item listed in the brief's research artifact appears in ≥1 R#.
- [ ] Every USER-STORY listed in the research artifact is addressed in ≥1 User Stories block or R#.
- [ ] v1 references use absolute `/Users/tom/Developer/projects/deno_rust/meridian/...` paths.
- [ ] No R# has empty subsections.
- [ ] No two R#s own the same file range (scan Files Likely to Change for duplicates).
- [ ] No "TODO without reason" — any `TODO(<reason>):` has a cited reason and is mirrored in Open Questions.
- [ ] Prerequisite briefs cited actually exist in the `docs/briefs/` tree.

Non-conformance on any item is a blocker verdict from brief-reviewer.
