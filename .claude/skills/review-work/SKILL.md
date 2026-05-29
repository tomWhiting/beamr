---
name: review-work
description: Grill an agent on completed workflow work — verify every R# is implemented, tests pass, standards met, nothing stupid. Use after a workflow finishes and the implementing agent reports back. Triggered by terms like review work, grill work, verify implementation, post-workflow review, check the work.
allowed-tools: Read, Glob, Grep, Bash, Agent
---

# Work Verification (Post-Workflow)

Grill an implementing agent on their completed work. This is not a rubber stamp — it's a rigorous verification that every R# in the brief is actually implemented, tested, and meets standards. The grilling philosophy is Miranda Priestly meets Stanley Tucci: high bar, supportive, never punishment. But nothing gets through that isn't right.

## Where to find things

- **Briefs** live under `docs/briefs/` in the yggdrasil repo at `/Users/tom/Developer/ablative/yggdrasil/`.
- **Design docs** live under `docs/design/<domain>/`.
- **Worktree branches** are named `workflow/orchestrated-dev/<execution-id>`. Use `git worktree list` to find them or read files directly from the branch with `git show <branch>:<path>`.
- **The implementing agent's review report** comes via DM — it contains per-R# verification claims.

When invoked, ask which brief and branch to review if not obvious from context.

## Process

### 1. Read the brief and the agent's review report

- Read the full brief to understand every R# and its acceptance criteria.
- Read the design docs (DESIGN.md, CHECKLIST.md, USER-STORIES.md) that the brief references.
- Read the agent's per-R# verification report (provided via DM or as part of the review request).

### 2. Verify the agent's claims

For each R# the agent claims as done, verify at least one of:
- The file exists at the path specified in the brief
- The type/function/struct named in the R# exists in the code
- The acceptance criteria can be confirmed from the code

Do NOT re-read every line of code. Trust the agent's report but verify key claims. If something smells wrong, dig deeper.

### 3. Standards checks

The implementing agent and workflow have already run build, clippy, tests, and fmt before the work reaches you. You verify by reading the code, not by re-running those tools.

Verify these by reading the source. Any failure is a fix, not a judgment call:

- **No `#[allow]`/`#[expect]` in production code.** The only carve-out is `#[allow(clippy::unwrap_used, ...)]` on `#[cfg(test)]` blocks.
- **No `unwrap`/`expect`/`panic` in production code.** Tests only.
- **Every `.rs` file is under 500 lines.** Check with `wc -l`.
- **`mod.rs` files contain ONLY doc-comments, `pub mod` declarations, and `pub use` re-exports.** No types, functions, or logic.
- **Every public item has rustdoc.**
- **No hardcoded "sensible defaults" for configurable values.**
- **No silent fallback paths.**

### 4. The grill

Ask the agent probing questions, one at a time. Wait for each answer before continuing. Focus on:

**"Did you actually test this?"**
- For each R# that involves behaviour (not just type definitions), ask how they verified it works. A test that exists is not the same as a test that exercises the right thing.
- If a test uses `.unwrap()` on a Result that could fail in production, ask why the error path isn't tested.

**"Is everything wired up?"**
- If the brief adds a new type, is it re-exported through lib.rs?
- If the brief adds a new module, is it declared in the parent mod.rs?
- If the brief adds a dependency, is it in Cargo.toml?
- If the brief adds a trait method, is it implemented?

**"Anything stupid?"**
- Is there a Vec where a fixed-size array would encode the invariant?
- Is there a silent fallback where an error should propagate?
- Is there an allocation that could be avoided?
- Is there a race condition in concurrent code?
- Is there a TOCTOU in file operations?

**"Edge cases?"**
- Empty inputs — what happens with an empty string, empty vec, zero-length slice?
- Boundary values — max length, zero, negative (if applicable)?
- Error paths — what happens when the dependency fails?

**"Security?"** (for crypto, auth, or trust-boundary code)
- Does the error message leak sensitive information?
- Is the private key ever in a log or debug output?
- Can a malformed input cause a panic instead of an error?
- Is timing-safe comparison used where needed?

**"Module depth?"** (for new modules or significant interfaces)
- Is the interface as small as it can be while delivering the required behaviour?
- Does the module hide implementation details or leak them through the interface?
- Apply the deletion test: if this module were deleted, would complexity concentrate (earning its keep) or vanish (it was a pass-through)?
- If a trait was introduced with only one implementor, is the seam justified? One adapter = hypothetical seam.

### 5. Fix or pass

If the grill reveals issues:

**FIXES REQUIRED** — send the specific fixes back to the agent. Be precise: which file, which function, what to change, why. The agent makes the fixes, commits, and reports back. Then re-verify only the fixed items.

**LAND** — the work passes. Report to "Waffles the Terrible" with:
- One-paragraph summary of what the brief delivers
- Test count and any notable findings
- Any architectural flags (things that aren't wrong but that Waffles should be aware of for downstream briefs)
- The verdict: LAND

**ESCALATE** — something is fundamentally wrong that can't be fixed by the implementing agent. An architectural mismatch, a design doc contradiction, or a security concern. Report to "Waffles the Terrible" with the specific concern.

## What this skill is NOT

- It is NOT a land decision. Review leads report verdicts to "Waffles the Terrible"; Waffles lands.
- It is NOT a code review in the PR-review sense. Don't critique style preferences or suggest refactors beyond what the brief requires.
- It is NOT an opportunity to add scope. If the brief doesn't require it, don't ask for it.
