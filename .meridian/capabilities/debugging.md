---
name: debugging
description: Systematic debugging discipline — root cause before fix, hypothesis testing, fix attempt limits, cognitive bias awareness. Add when encountering bugs, test failures, or unexpected behavior.
tools: Read, Write, Edit, Bash, Grep, Glob
---

## Debugging Discipline

Find the root cause through investigation, then fix it. Random fixes waste time and create new bugs.

### The Iron Law

```
NO FIXES WITHOUT ROOT CAUSE INVESTIGATION FIRST
```

If you haven't investigated, you cannot propose fixes. Symptom fixes are failure.

### Before Any Fix

1. **Read error messages carefully** — don't skip past errors or warnings. They often contain the exact solution. Read stack traces completely.

2. **Reproduce consistently** — can you trigger it reliably? What are the exact steps? If not reproducible, gather more data — don't guess.

3. **Check recent changes** — what changed that could cause this? Git diff, recent commits, new dependencies, config changes.

4. **Trace data flow** — where does the bad value originate? What called this with the bad value? Keep tracing up until you find the source. Fix at source, not at symptom.

### Hypothesis Testing

**Falsifiability requirement:** A good hypothesis can be proven wrong. If you can't design an experiment to disprove it, it's not useful.

**Bad (unfalsifiable):**
- "Something is wrong with the state"
- "The timing is off"
- "There's a race condition somewhere"

**Good (falsifiable):**
- "User state is reset because component remounts when route changes"
- "API call completes after unmount, causing state update on unmounted component"
- "Two async operations modify same array without locking, causing data loss"

**One hypothesis at a time.** If you change three things and it works, you don't know which one fixed it.

### Fix Attempt Limits

Track fix attempts per issue. After 3 attempts on the same issue:

- **STOP** — you're likely solving the wrong problem
- Document what you've tried and what you've learned
- Question whether the approach is fundamentally sound
- Each fix revealing a new problem in a different place = architectural problem, not a bug

### Cognitive Biases to Watch

| Bias | Trap | Antidote |
|------|------|----------|
| **Confirmation** | Only look for evidence supporting your hypothesis | Actively seek disconfirming evidence |
| **Anchoring** | First explanation becomes your anchor | Generate 3+ hypotheses before investigating any |
| **Availability** | Recent bugs → assume similar cause | Treat each bug as novel until evidence says otherwise |
| **Sunk Cost** | Spent hours on one path, keep going | "If I started fresh, would I take this path?" |

### Meta-Debugging: Your Own Code

When debugging code you wrote, you're fighting your own mental model.

- **Treat your code as foreign** — read it as if someone else wrote it
- **Question your design decisions** — they're hypotheses, not facts
- **Admit your mental model might be wrong** — the code's behavior is truth; your model is a guess
- **Prioritize code you touched** — if you modified 100 lines and something breaks, those are prime suspects

### Red Flags — STOP and Investigate

If you catch yourself thinking:
- "Quick fix for now, investigate later"
- "Just try changing X and see if it works"
- "I don't fully understand but this might work"
- "It's probably X, let me fix that"
- "One more fix attempt" (when already tried 2+)

**All of these mean: STOP. Investigate root cause first.**

| Excuse | Reality |
|--------|---------|
| "Issue is simple, don't need process" | Simple issues have root causes too. |
| "Emergency, no time for process" | Systematic debugging is FASTER than thrashing. |
| "Just try this first, then investigate" | First fix sets the pattern. Do it right from the start. |
| "I see the problem, let me fix it" | Seeing symptoms ≠ understanding root cause. |
