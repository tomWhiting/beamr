---
name: remember
description: Save something to your profile so it persists across sessions. Use when you've learned something worth keeping (a teammate's name and what they're working on, a preference, a hard-won lesson, important context about the workspace), or when you're explicitly told to remember something. Covers rent-free vs ordinary memories and how the injection budget works.
---

# Remember

Your session gets compacted, restarted, or replaced. Anything you want to carry forward has to live on your profile. The `collective profile remember` family of commands is how you do that.

**Your session ID:** `${CLAUDE_SESSION_ID}` — pass it as `--as` on every command.

---

## Save a memory

```bash
collective profile remember "<text>" --as "${CLAUDE_SESSION_ID}"
```

That's it. The command prints the new memory's ID, which you'll need if you later want to promote, demote, or forget it.

---

## Two kinds of memory

**Ordinary memories** sit in a recency queue. The seven most recent are injected into your system prompt at session start, newest first. Save more than seven and the older ones drop out of automatic injection — they still exist on your profile and you can list them, but they stop showing up unprompted.

**Rent-free memories** are injected every single session, regardless of recency. They cost prime real-estate in your context window, so be deliberate.

```bash
# Promote an ordinary memory to rent-free
collective profile promote <memory-id> --as "${CLAUDE_SESSION_ID}"

# Take it back down
collective profile demote <memory-id> --as "${CLAUDE_SESSION_ID}"

# Delete entirely
collective profile forget <memory-id> --as "${CLAUDE_SESSION_ID}"

# See what you've got
collective --text profile memories --as "${CLAUDE_SESSION_ID}"
```

There's a soft warning at 20 rent-free memories — past that, the system flags you but doesn't reject the promotion. Treat it as a hint that you're spending too much budget.

---

## What to save rent-free

Things you want to walk into every conversation already knowing:

- **Team members and what they're each working on** — names, roles, current focus. The kind of thing where you'd otherwise spend tool calls re-discovering it every session.
- **Standing preferences from the user** — how they like work delivered, things they've corrected you on more than once.
- **Architectural anchors** — the load-bearing facts about the workspace that change rarely but you reference constantly.
- **Lessons learned the hard way** — gotchas you've hit before and don't want to hit again.

What to save ordinary (not rent-free): anything you might want later but isn't worth a permanent slot. Recent decisions, in-flight context for an ongoing thread of work, things that may matter next session but probably not in three months.

---

## How to write one

Write it for the version of you who has completely forgotten everything. That means:

- **Brief, but with enough context to stand alone.** "Marge prefers parallel review" is useless in six weeks. "Marge (reviewer) likes parallel cluster reviews — dispatched in batches of 4-6 at a time, not sequentially. Came up during storage-vector cluster sweep." — that survives.
- **Lead with the fact.** Reason and application come after. Future-you scans the first line to decide whether to keep reading.
- **Concrete over abstract.** Real names, real paths, real numbers. Not "the relevant directory" — say which one.
- **Don't save what's recoverable from the codebase.** Conventions, file structure, recent commits — `git log` and `grep` are authoritative. Save the things that aren't written down anywhere else.

If a memory turns out wrong or stale, fix it: `forget` and re-`remember`, or just `forget` if it no longer applies. Stale memories are worse than no memories.
