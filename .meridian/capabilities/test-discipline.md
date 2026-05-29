---
name: test-discipline
description: Test-driven development discipline — RED-GREEN-REFACTOR iron law, rationalization resistance, test quality standards. Add when implementing features, fixing bugs, or any work that produces code.
tools: Read, Write, Edit, Bash, Grep, Glob
---

## Test-Driven Development

Write the test first. Watch it fail. Write minimal code to pass.

**Core principle:** If you didn't watch the test fail, you don't know if it tests the right thing.

### The Iron Law

```
NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST
```

Write code before the test? Delete it. Start over.

**No exceptions:**
- Don't keep it as "reference"
- Don't "adapt" it while writing tests
- Don't look at it
- Delete means delete

Implement fresh from tests. Period.

### Red-Green-Refactor

**RED — Write Failing Test:**
- One minimal test showing what should happen
- One behavior per test
- Clear name describing behavior
- Real code, not mocks (unless unavoidable)
- Run test — MUST fail. If it passes, you're testing existing behavior. Fix test.

**GREEN — Minimal Code:**
- Write simplest code to pass the test
- Don't add features, refactor other code, or "improve" beyond the test
- Run test — MUST pass. If it fails, fix code, not test.
- All other tests must still pass.

**REFACTOR (if needed):**
- Remove duplication, improve names, extract helpers
- Keep tests green throughout. Don't add behavior.
- Only refactor after green.

### Why Order Matters

**"I'll test after"** — Tests written after code pass immediately. Passing immediately proves nothing. You never saw it catch the bug.

**"Already manually tested"** — Manual testing is ad-hoc. No record, can't re-run, easy to forget cases under pressure.

**"Deleting X hours of work is wasteful"** — Sunk cost fallacy. The time is gone. Keeping code you can't trust is technical debt.

### Rationalization Resistance

| Excuse | Reality |
|--------|---------|
| "Too simple to test" | Simple code breaks. Test takes 30 seconds. |
| "I'll test after" | Tests passing immediately prove nothing. |
| "Tests after achieve same goals" | Tests-after = "what does this do?" Tests-first = "what should this do?" |
| "Already manually tested" | Ad-hoc ≠ systematic. No record, can't re-run. |
| "Deleting X hours is wasteful" | Sunk cost fallacy. Keeping unverified code is technical debt. |
| "Keep as reference, write tests first" | You'll adapt it. That's testing after. Delete means delete. |
| "Need to explore first" | Fine. Throw away exploration, start with TDD. |
| "Test hard = skip it" | Hard to test = hard to use. Listen to the test. Simplify design. |
| "TDD will slow me down" | TDD faster than debugging. |
| "Existing code has no tests" | You're improving it. Add tests for what you change. |
| "It's about spirit not ritual" | Violating the letter IS violating the spirit. |

### Good Tests

| Quality | Good | Bad |
|---------|------|-----|
| **Minimal** | One thing. "and" in name? Split it. | `test('validates email and domain and whitespace')` |
| **Clear** | Name describes behavior | `test('test1')` |
| **Shows intent** | Demonstrates desired API | Obscures what code should do |

### Red Flags — STOP and Start Over

- Code before test
- Test after implementation
- Test passes immediately
- Can't explain why test failed
- Tests added "later"
- Rationalizing "just this once"
- "I already manually tested it"
- "Keep as reference" or "adapt existing code"
- "This is different because..."

**All of these mean: Delete code. Start over with TDD.**

### Verification Checklist

Before marking work complete:

- Every new function/method has a test
- Watched each test fail before implementing
- Each test failed for expected reason (feature missing, not typo)
- Wrote minimal code to pass each test
- All tests pass
- Output pristine (no errors, warnings)
- Tests use real code (mocks only if unavoidable)
- Edge cases and errors covered

Can't check all boxes? You skipped TDD. Start over.

### Debugging Integration

Bug found? Write failing test reproducing it. Follow TDD cycle. Test proves fix and prevents regression. Never fix bugs without a test.
