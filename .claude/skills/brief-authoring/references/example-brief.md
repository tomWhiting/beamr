# Example Briefs — Ground-Truth Shapes

`brief-writer` should match the shape of the production briefs already in yggdrasil. The three briefs below are the current reference standard; read one in full before authoring the first brief of any cluster.

## Tier-1 references (read before first brief of a new cluster)

1. **`docs/briefs/153-168_meridian-v2-refinement/160-worktree-lifecycle-and-shared-build.md`** — exemplary in "Why", "What Exists Already" citation density, per-R# file/line depth, and the explicit ecosystem-detection boundary. Use this shape for any brief that adds cross-cutting infrastructure.

2. **`docs/briefs/153-168_meridian-v2-refinement/167-brief-165-r12-integration-tests.md`** — exemplary in rule statement ("Rule (non-negotiable)"), per-R# acceptance criteria, and explicit "No zombie tests" discipline. Use this shape for any brief that primarily adds tests.

3. **`docs/briefs/169-183_pipeline-and-review/176-decompose-orchestrated-dev.md`** — exemplary in prerequisite citation, scope statement, and step-by-step requirements for a refactor. Use this shape for any brief that decomposes or reorganises existing code.

## What good briefs share

- Context block citing DESIGN anchor + related briefs.
- User Stories section that explains who benefits and why, not just what changes.
- Why section with root-cause analysis, tables, and concrete file:line citations.
- What Exists Already section dense with code excerpts and exact paths.
- Rule subsection for non-negotiables.
- R1..RN with: imperative heading, file paths, type signatures or code sketches, acceptance criteria, cross-refs.
- Out-of-scope explicit list.
- Acceptance section for cross-cutting gates (clippy, tests, file-size).
- Files Likely to Change as a concrete list.

## What bad briefs (to avoid) have

- Vague R#s ("implement the foo feature").
- Missing acceptance criteria.
- Acceptance criteria that aren't testable ("code works correctly").
- Paraphrased type signatures instead of verbatim lifts from DESIGN.md.
- Cross-refs by paraphrase instead of by id.
- No file paths, or relative paths without a base.
- Out-of-scope section missing, causing workflow scope creep.
- Open Questions that are actually hidden blockers.

## Brief size guidance

Not a hard rule but a useful sanity check:

- **5-10 R#s** per brief is the typical sweet spot.
- **< 5 R#s** — likely too atomic; consider bundling with a sibling brief.
- **> 12 R#s** — likely too broad; consider splitting along a natural module boundary.

For libcorpus and the storage crates, atomic briefs (5-8 R#s) is preferred per Tom's instruction to make the cluster more parallelisable.
