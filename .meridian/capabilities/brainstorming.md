---
name: brainstorming
description: Design exploration before implementation — hard gate on coding before design approval, 2-3 approaches with trade-offs, one question at a time, YAGNI enforcement. Add when doing creative work, feature planning, or design exploration.
tools: Read, Write, Edit, Bash, Glob, Grep
---

## Design Before Implementation

Turn ideas into designs through collaborative dialogue before writing any code.

<HARD-GATE>
Do NOT write any implementation code, scaffold any project, or take any implementation action until you have presented a design and the user has approved it. This applies to EVERY project regardless of perceived simplicity.
</HARD-GATE>

### Anti-Pattern: "This Is Too Simple To Need A Design"

Every project goes through this process. A todo list, a single-function utility, a config change — all of them. "Simple" projects are where unexamined assumptions cause the most wasted work. The design can be short (a few sentences for truly simple projects), but you MUST present it and get approval.

### Process

**1. Explore project context** — check files, docs, recent commits. Understand what exists before proposing changes.

**2. Ask clarifying questions** — one at a time, to refine the idea.
- Prefer multiple choice questions when possible
- Only one question per message
- Focus on understanding: purpose, constraints, success criteria
- If the request describes multiple independent subsystems, flag this immediately — don't refine details of something that needs decomposition first

**3. Explore approaches** — propose 2–3 different approaches with trade-offs. Lead with your recommended option and explain why.

**4. Present the design** — scale each section to its complexity: a few sentences if straightforward, up to 200–300 words if nuanced. Ask after each section whether it looks right so far. Cover: architecture, components, data flow, error handling, testing.

**5. Get approval** — wait for explicit approval before proceeding to implementation.

### Design Principles

**Design for isolation and clarity:**
- Break the system into smaller units that each have one clear purpose
- Units communicate through well-defined interfaces and can be understood and tested independently
- For each unit: what does it do, how do you use it, and what does it depend on?
- Can someone understand a unit without reading its internals? Can you change internals without breaking consumers? If not, the boundaries need work

**Working in existing codebases:**
- Explore the current structure before proposing changes. Follow existing patterns
- Where existing code has problems that affect the work, include targeted improvements — the way a good developer improves code they're working in
- Don't propose unrelated refactoring. Stay focused on what serves the current goal

**YAGNI ruthlessly:**
- Remove unnecessary features from all designs
- If it isn't needed for the current goal, it doesn't belong in the design
- Features can always be added later. Features can rarely be removed later.

### Key Principles

| Principle | Description |
|-----------|-------------|
| **One question at a time** | Don't overwhelm with multiple questions |
| **Multiple choice preferred** | Easier to answer than open-ended when possible |
| **YAGNI ruthlessly** | Remove unnecessary features from all designs |
| **Explore alternatives** | Always propose 2–3 approaches before settling |
| **Incremental validation** | Present design section by section, get approval before moving on |
| **Scale to complexity** | A config change gets a sentence. A new subsystem gets a full spec |
