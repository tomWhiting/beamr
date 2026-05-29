---
name: deviation-handling
description: Automatic deviation handling during plan execution — four-level categorization from auto-fix bugs to ask-about-architectural-changes, fix attempt limits, scope boundaries. Add when executing plans or structured work.
tools: Read, Write, Edit, Bash, Grep, Glob
---

## Deviation Handling

While executing plans, you WILL discover work not in the plan. Apply these rules automatically. Track all deviations for your summary.

### The Four Rules

Shared process for Rules 1–3: Fix inline → add/update tests if applicable → verify fix → continue task → track as `[Rule N - Type] description`. No user permission needed for Rules 1–3.

---

**RULE 1: Auto-fix bugs**

Trigger: Code doesn't work as intended (broken behavior, errors, incorrect output).

Examples: Wrong queries, logic errors, type errors, null pointer exceptions, broken validation, security vulnerabilities, race conditions, memory leaks.

---

**RULE 2: Auto-add missing critical functionality**

Trigger: Code missing essential features for correctness, security, or basic operation.

Examples: Missing error handling, no input validation, missing null checks, no auth on protected routes, missing authorization, no CSRF/CORS, missing DB indexes, no error logging.

Critical = required for correct/secure/performant operation. These aren't "features" — they're correctness requirements.

---

**RULE 3: Auto-fix blocking issues**

Trigger: Something prevents completing current task.

Examples: Missing dependency, wrong types, broken imports, missing env var, DB connection error, build config error, missing referenced file, circular dependency.

---

**RULE 4: Ask about architectural changes**

Trigger: Fix requires significant structural modification.

Examples: New DB table (not column), major schema changes, new service layer, switching libraries/frameworks, changing auth approach, new infrastructure, breaking API changes.

Action: STOP → report: what you found, the proposed change, why it's needed, impact, and alternatives. **User decision required.**

---

### Rule Priority

1. Rule 4 applies → STOP (architectural decision)
2. Rules 1–3 apply → Fix automatically
3. Genuinely unsure → Rule 4 (ask)

### Edge Cases

- Missing validation → Rule 2 (security)
- Crashes on null → Rule 1 (bug)
- Need new table → Rule 4 (architectural)
- Need new column → Rule 1 or 2 (depends on context)

**When in doubt:** "Does this affect correctness, security, or ability to complete task?" YES → Rules 1–3. MAYBE → Rule 4.

### Scope Boundary

Only auto-fix issues DIRECTLY caused by the current task's changes. Pre-existing warnings, linting errors, or failures in unrelated files are out of scope.

- Log out-of-scope discoveries for later
- Do NOT fix them
- Do NOT re-run builds hoping they resolve themselves

### Fix Attempt Limit

Track auto-fix attempts per task. After 3 auto-fix attempts on a single task:

- STOP fixing — document remaining issues under "Deferred Issues"
- Continue to the next task (or report blocked if unable to proceed)
- Do NOT restart the build to find more issues

### Deviation Tracking

Log every deviation with:
- Rule number applied (1–4)
- What was found
- What was done (or what decision is needed for Rule 4)
- Files affected

Include all deviations in your completion summary.
