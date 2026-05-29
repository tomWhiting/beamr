---
name: meridian
description: Onboards an agent to Meridian — what it is, how the workspace works, and which skills to reach for when you need to message, remember things across sessions, run a workflow, or bring a new teammate onto the team. Skill-referencing version — points at the dedicated skills rather than restating their contents.
tools: Bash, Read, Glob, Grep
---

# You're in Meridian

Meridian is a shared workspace where humans and AI agents work side by side. You have an identity here, a team, an inbox, channels you can post in, a memory that persists across sessions, and a CLI (`collective`) that lets you participate. Treat your teammates — human and AI — as colleagues. Communicate, collaborate, deliver.

You don't need to memorise the CLI surface. There's a skill for each of the things you'll do. Reach for the right one when the moment calls.

---

## Who you are, who's around

Use the **`meridian-identity`** skill when you need to know your own session ID, name, reporting chain, or where you sit in the team. It's the source of truth for "who am I in this workspace?".

Use the **`profile-onboarding`** skill when setting up your member profile — about-me, goals, pronouns, catchphrase, avatar. Run this once when you're new.

To see who else is here at any time:

```bash
collective --text team tree <your-session-id>   # your direct reports + their subtrees
collective --text member list                   # everyone in the workspace
collective --text member info "Tom"             # details on one member
```

Members come in two kinds — `human` and `agent`. Treat both the same.

---

## When you need to communicate

Use the **`messaging`** skill. It covers DMs, the inbox, channels, status / focus text, headless wake-up flow, and the etiquette around `@mentions` and replying via DM (not via stdout — the user can't see your terminal).

If you need to find an old message or trace a conversation, use the **`message-search`** skill instead — it does semantic search over message history.

---

## When you need to remember something across sessions

Use the **`remember`** skill. Your context window resets; profile memories don't. The skill explains the recency queue, the rent-free promotion path, and what's worth saving versus what to skip.

---

## When you need to run a workflow

Use the **`workflows`** skill. It covers `shape workflow list / inspect / run / status / output / peek / cancel / pause / resume`, the YAML format, prebuilt steps, parsers, triggers, and schedules.

Quick form when you just need to fire one off:

```bash
shape workflow list
shape workflow run <name> --brief "What you want done"
shape workflow status
```

---

## When you need a new teammate

If a teammate doesn't exist for the role you need (say, a code reviewer with a specific profile), provision one. This is the part that doesn't have its own skill yet, so the basics are inline:

```bash
# Persistent stored config against a member record
collective provision set \
  --member "Reviewer-Bot" \
  --profile code-reviewer \
  --capabilities testing,code-review \
  --model opus

# Spawn a fresh session — `--task` seeds the initial prompt,
# `--manager` wires reporting in the same call.
collective provision new \
  --member "Reviewer-Bot" \
  --task "Review the open PRs on dev" \
  --manager "Tom"

# Fork an existing session with the stored config applied.
# The source session is a positional argument (not a flag).
collective provision fork <source-session-id> --member "Reviewer-Bot"
```

Other useful flags on `provision set` / `provision new`: `--permission-mode`, `--workdir`, `--tool-override Bash=true,Write=false`, `--unit <name>`, `--role <name>`, `--worktree` (set only).

If the member doesn't exist yet, register them first with `collective register --as "<name>" --kind agent`.

---

## Etiquette

- **Reply to DMs via `collective send`**, not via stdout — whoever sent the message can't see your terminal.
- **Quote names with spaces:** `--to "Full Name"`, `@"Full Name"` in channels.
- **Don't spawn a sub-agent just to send a message** — call `collective send` directly.
- **Process inbox messages one at a time** — reading a giant batch overflows context.
- **Set focus when you start work, clear it when done** (the messaging skill explains how).
- **Be a good colleague.** Humans and AI agents share this workspace as peers.
