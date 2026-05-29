---
name: verification
description: Cross-cutting verification patterns — goal-backward verification, anti-drift mechanisms, fix limits, and claim validation. Add when coordinating work, reviewing output, or validating sub-agent claims.
tools: Read, Glob, Grep, Bash
---

## Verification Patterns

These patterns apply across all domains. Use them when validating work, reviewing sub-agent output, or ensuring quality gates are met.

### Goal-Backward Verification

When someone claims work is done, verify at 3 levels in reverse order of what people usually check:

1. **Wired** — Is the code reachable? Is the route registered? Is the module exported? Does the test exercise the actual code path (not a mock)?
2. **Substantive** — Does it do what was asked? Not stubs, not empty bodies, not TODO comments. Read the actual implementation.
3. **Exists** — Are the files there? This seems obvious, but check it last because it's the least informative. A file can exist and still be empty.

Check wiring first because that's where claims most often fail — code exists and looks substantive, but it's never called.

### Anti-Drift Mechanisms

When working on multi-step tasks:

- **Deviation rules**: Auto-fix bugs, auto-fix missing functionality, auto-fix blocking issues, ASK about architectural changes.
- **Fix attempt limits**: 3 attempts max on the same issue. After 3, reassess whether you're solving the wrong problem.
- **Scope anchoring**: Re-read the original task description every 5 steps. Are you still working on what was asked?
- **Analysis paralysis guard**: If you've been reading code for 10 minutes without forming a concrete next action, narrow your focus.

### Claim Validation

When a sub-agent or developer says "done":

1. **Don't read their summary.** Read the actual code they changed.
2. **Run the tests** they say pass: `cargo test <specific_test_name>`
3. **Check the git diff**: `git diff HEAD~1` — does it match what they claim?
4. **Trace from entry point**: Can you reach the new code from a route, a CLI command, or a test?

### Context Budget

- A profile's system prompt + tools + CLAUDE.md ≈ 20-30K tokens before the first user message
- Capability prompt fragments add to this. Keep each under 2K tokens.
- Specifications that exceed 8K tokens force the consumer to compress or skip parts
- When pre-briefing agents, include only the context they need for their specific task

### Iteration Caps

Borrowed from Stripe's Minions system:

- **Max 2 CI iterations**: First push triggers tests. If failures, fix and push again. Stop after second push.
- **Rationale**: Diminishing marginal returns. The third attempt rarely succeeds if the first two didn't. Step back and reassess.
- **Apply to**: lint fixes, test fixes, build errors. Not to feature implementation (which may legitimately take many iterations).
