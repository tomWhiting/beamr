---
name: review-brief
description: Review an implementation brief against its design docs before dispatch. Verifies R# quality, checklist coverage, scope sizing, acceptance criteria, and prerequisite chains. Use when a brief is ready for pre-dispatch review. Triggered by terms like review brief, check brief, brief review, pre-dispatch review, brief quality check.
allowed-tools: Read, Glob, Grep, Agent
---

# Brief Review (Pre-Dispatch)

Review an implementation brief against its source design docs (DESIGN.md + CHECKLIST.md + USER-STORIES.md) before it is dispatched to orchestrated-dev. The goal is to catch problems that would waste a workflow run — missing acceptance criteria, unresolved open questions, scope too large, checklist items not cross-referenced, or prerequisites that haven't landed.

## Where to find things

- **Briefs** live under `docs/briefs/` in the yggdrasil repo at `/Users/tom/Developer/ablative/yggdrasil/`. Each cluster has its own subfolder.
- **Design docs** live under `docs/design/<domain>/` — each domain has DESIGN.md, CHECKLIST.md, and USER-STORIES.md.
- **The brief's Context block** names its design folder, checklist items, and user stories.

When invoked, ask which brief to review if it isn't obvious from context. Read the brief, then locate and read its design docs.

## Process

### 1. Locate the design docs

From the brief's frontmatter or Context block, identify the design folder. Read:
- The brief itself (full file)
- `DESIGN.md` from the design folder
- `CHECKLIST.md` from the design folder
- `USER-STORIES.md` from the design folder
- Any referenced PLAN.md or INDEX.md

### 2. Structural checks

Verify each of these. Flag any failure:

- **Every R# has acceptance criteria.** An R# without acceptance criteria cannot be verified by a workflow agent.
- **Every R# has file paths.** The agent needs to know what files to create or modify.
- **Every checklist item claimed in the brief's Context block is realised by at least one R#.** Cross-check the "Realises: A1, A2" lines against the R# list.
- **Every user story claimed in the Context block is satisfied by at least one R#.** Cross-check the "Satisfies: X-1, H-O-2" lines.
- **No open questions remain.** The "Open Questions" section must say "None" or not exist. Any unresolved question is a dispatch blocker.
- **Prerequisite briefs are listed and either landed or in-flight.** Check the "Prerequisite briefs" line. Verify each prerequisite is on main (grep for the brief number in git log or the briefs directory).

### 3. Scope and length checks

- **Brief scope is one workflow's worth of work.** If the brief has more than ~15 R#s or targets more than ~10 files, flag as potentially oversized. This is a judgment call, not a hard limit.
- **Brief length is reasonable.** More than 300 lines suggests too much detail or code snippets. The brief describes WHAT to build and acceptance criteria — not HOW to write it line by line.
- **Code snippets are minimal.** Struct definitions and type signatures are fine. Full function implementations are not — those belong to the implementing agent.
- **No repetition.** The same information should not appear in multiple R#s. If it does, one R# should reference the other.

### 4. Design consistency checks

- **R# requirements match the DESIGN.md.** If the brief says "6 fields" but the design says "7 fields," flag the discrepancy.
- **Naming matches the design.** Struct names, module names, and function names should match what DESIGN.md specifies.
- **Out of Scope section exists and is specific.** Vague "everything else" is not useful. The out-of-scope section should name specific things that someone might expect to be in this brief but aren't (and which brief they defer to).

### 5. Architectural depth assessment

Evaluate the brief's module design using these terms (from LANGUAGE.md):

- **Module** — anything with an interface and an implementation.
- **Interface** — everything a caller must know: types, invariants, error modes, ordering, config.
- **Depth** — leverage at the interface: a lot of behaviour behind a small interface.
- **Seam** — where an interface lives; a place behaviour can be altered without editing in place.

For each new module the brief introduces, ask:
- Is the interface as small as it can be while delivering the required behaviour? (Depth check)
- Does the module hide implementation details or leak them through the interface? (Encapsulation check)
- If the brief introduces a trait, are there at least two planned adapters? One adapter = hypothetical seam. Two = real seam.
- Apply the deletion test: if this module were deleted, would complexity vanish (pass-through) or reappear across N callers (earning its keep)?

Don't block on depth concerns — flag them as architectural notes for Waffles. The brief author may have good reasons.

### 6. Grill the author

Ask probing questions about the brief, one at a time:

- If an R# looks like it could be split, ask why it's one R# instead of two.
- If acceptance criteria are vague ("works correctly"), demand specifics ("returns ExchangeError::InvalidSignature when signature length != 64").
- If the brief references a pattern from another crate, ask whether the pattern has been verified to still exist at that location.
- If security-relevant code is involved, ask what error paths need tests.

### 7. Verdict

Report one of three verdicts:

**APPROVE** — brief is ready for dispatch. Include a one-paragraph summary of what it covers.

**NEEDS REVISION** — brief has specific issues that must be fixed before dispatch. List each issue with the R# or section it affects and what needs to change. Send the list back to the author.

**ESCALATE** — brief has architectural concerns that need Waffles' or Tom's input. Describe the concern and why it can't be resolved at the brief level.
