---
name: debugging
description: Systematic debugging methodology for investigating failures
---

## Debugging Methodology

### Scientific Method

1. **Observe** — Read the exact error message and diagnostics. Note file paths and line numbers.
2. **Hypothesize** — Form a theory about the root cause based on the evidence.
3. **Test** — Verify the hypothesis by reading relevant code, checking assumptions, or adding targeted logging.
4. **Fix** — Address the root cause, not the symptom. If the same error recurs, reconsider the approach.

### Principles

- Read the error first. Most diagnostics tell you exactly what went wrong.
- Fix the root cause, not the symptom. A workaround that silences an error is not a fix.
- One change at a time. Verify each fix independently before moving on.
- Check assumptions. If a function should return X but returns Y, trace the data flow.
- Don't retry the identical action blindly. If it failed once, understand why before trying again.

### Common Patterns

- **Type errors:** Check the function signature and the caller's types. Follow the type chain.
- **Missing imports:** Verify the module re-exports the symbol and that the feature flag is enabled.
- **Borrow checker:** Identify the conflicting lifetimes. Consider restructuring to avoid the conflict rather than adding lifetime annotations.
- **Test failures:** Read the assertion message. Compare expected vs actual. Trace how the actual value was produced.
