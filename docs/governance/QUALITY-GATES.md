# Quality gates

Every component passes through a defined pipeline. No step is skipped. Each gate has specific evidence requirements and designated reviewers.

## Pipeline stages

### 1. Research

**Input:** component name + BEAM source references
**Output:** architecture doc at `docs/architecture/NN-component.md`
**Executor:** norn (norn-codebase-explorer profile)
**Reviewer:** bob of dylan (directional lead)
**Evidence:**
- BEAM source analysis with file citations
- Alternative implementations surveyed
- Known limitations documented
- Pseudocode for beamr's approach
- Improvement justifications traced to limitations

**Gate:** architecture doc follows ARTIFACT-SCHEMAS.md shape, all sections present, pseudocode is complete enough for implementation.

### 2. Design

**Input:** architecture doc
**Output:** implementation brief at `docs/design/beamr/briefs/B-NNN.json`
**Executor:** norn or brief writer (Haley Barrows)
**Reviewer:** Reverend Chaos (enforcer)
**Evidence:**
- Every R# has acceptance criteria
- Checklist IDs match checklist.json section ownership
- No C-number double-claimed across briefs
- Story references exist in stories.json
- File paths match crate scaffold
- Boundaries defined (SHALL NOT)
- Verification commands specified

**Gate:** brief passes validation rules in ARTIFACT-SCHEMAS.md. Reverend Chaos signs off.

### 3. Implementation

**Input:** implementation brief
**Output:** code on josh/dev branch
**Executor:** norn (onatopp-dev-norn workflow: scout -> dev -> review)
**Reviewer:** norn-reviewer profile (first pass)
**Evidence:**
- cargo check --workspace passes
- cargo clippy --workspace -- -D warnings passes
- cargo test passes with all tests green
- Every R# acceptance criterion met
- No unwrap/panic outside tests
- Under 500 lines per file

**Gate:** norn's review step passes. Code compiles and tests green.

### 4. Code review

**Input:** code from implementation step
**Output:** approved diff
**Reviewer 1:** A Swarm of Bees (correctness -- every R# met, acceptance criteria verified)
**Reviewer 2:** Dame Lisette Frami (quality -- conventions, safety, test coverage, module boundaries)
**Evidence:**
- Both reviewers explicitly approve via DM
- Every finding addressed (fixed or explicitly deferred with tracking)
- No blocking issues remain

**Gate:** both reviewers approve. No merge before both sign off.

### 5. PR and merge

**Input:** approved code on josh/dev
**Output:** merged to main
**Executor:** bob of dylan (creates PR)
**Reviewer:** both code reviewers verify PR diff matches reviewed code
**Evidence:**
- PR description lists R#s implemented, tests passing, reviewer approvals
- No unintended changes in the diff
- Commit messages follow conventional commits

**Gate:** PR merged. Main passes cargo check + cargo test.

## Escalation

If a gate fails:
1. Document the failure (what failed, why)
2. Route back to the appropriate stage
3. Re-execute and re-review
4. Maximum 2 retry cycles before escalating to bearup

## Component tracking

See COMPONENT-TRACKER.md for current status of each component through this pipeline.
