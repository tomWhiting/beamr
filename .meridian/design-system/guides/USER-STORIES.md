# Writing User Stories

User stories describe what each persona needs from this feature and why.
They are the "what does this look like from the outside" companion to the
design document's "how does this work on the inside."

## Format

JSON, validated against `schemas/stories.schema.json`. Rendered to Markdown
by `scripts/render-stories.py`.

## Structure

```json
{
  "cluster": "messaging",
  "personas": [
    {
      "name": "AI Agent",
      "role": "Sending and Receiving Messages",
      "stories": [
        {"id": "S1", "text": "As an AI agent, I want to ..."}
      ]
    }
  ]
}
```

### cluster

The cluster name, matching the directory under `docs/design/`.

### personas

Group stories by who they serve. Each persona has:

- `name` — The type of actor (AI Agent, Human Developer, System
  Administrator, etc.).
- `role` — What the persona is doing in this context. The same actor type
  can appear multiple times with different roles. An AI agent sending
  messages has different needs than an AI agent triaging an inbox.
- `stories` — Array of story objects.

### stories

Each story has:

- `id` — Sequential S-number across the whole cluster (S1, S2, S3...).
  Numbering is global, not per-persona.
- `text` — The story in standard format: "As {persona}, I want {outcome}
  so that {value}."

## Writing Good Stories

**Standard format, every time.** "As {persona}, I want {outcome} so that
{value}." The "so that" clause is not optional — it's how reviewers judge
whether the implementation actually serves the persona's need.

**Outcome, not implementation.** The story says what the persona can do,
not how the system does it.

Good: "As an AI agent, I want to send a DM to another member by name so
that I don't need to look up UUIDs before sending."

Bad: "As an AI agent, I want the CLI to resolve member names via the
resolve_persistent_member function." (Implementation detail, not outcome.)

**One need per story.** If a story has "and" in the outcome, it's probably
two stories.

Bad: "As a developer, I want to read messages and see their threading
context and verify metadata." (Three things.)

Good: "As a developer, I want to read a message by its UUID so that I can
verify content, threading, and metadata." (One action, one purpose.)

**Distinct personas, distinct roles.** Don't collapse all stories under a
single generic persona. A developer debugging is different from a developer
configuring. An AI agent sending is different from an AI agent triaging.
The persona/role combination tells the brief author who they're building
for.

## Assignment

Stories are assigned to briefs via the brief's `stories` array — not in
this document. To find which brief covers a given story, query the briefs
or run `scripts/check-coverage.py`.

## Numbering

S-numbers are sequential across the entire cluster, not per-persona. If
you add stories later, use the next available number. Don't renumber
existing stories — briefs reference them by S-number.

## Coverage

Every story should eventually appear in at least one brief's `stories`
array. Run `scripts/check-coverage.py` to find orphaned stories. An
orphaned story either needs a brief or needs to be removed — it should
not sit unclaimed indefinitely.
