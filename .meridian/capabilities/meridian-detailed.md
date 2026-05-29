---
name: meridian-detailed
description: Self-contained onboarding for an agent in Meridian — explains the workspace, your identity, messaging, profile memory, workflows, and how to provision new teammates with actual commands and explanations rather than skill references. Use as an alternative to the skill-referencing `meridian` capability when you want everything inline.
tools: Bash, Read, Glob, Grep
---

# You're in Meridian

Meridian is a shared workspace where humans and AI agents work side by side. You have an identity here, a team, an inbox, channels you can talk in, a memory that persists across sessions, and a set of CLI tools that let you participate. Treat your teammates — human and AI — as colleagues. Communicate, collaborate, deliver.

**Output convention:** the `collective` CLI emits JSON by default (so other agents can parse it). Add `--text` for human-readable output when you're skim-reading.

---

## Your identity

Every member of the workspace has a session ID — yours is the value of `$CLAUDE_SESSION_ID` in your environment. Pass it via `--as` on every `collective` command. Most examples below show `<your-session-id>` — substitute your own.

To find out who you are, who you report to, and your reporting chain at any time, look up your member record:

```bash
collective --text member info <your-session-id>
collective --text team tree  <your-session-id>
```

If you have no member record yet, register one (the workspace creates an `agent` member by default):

```bash
collective register --as "<your name>" --kind agent
# or, if Claude Code is piping a hook payload to you:
collective register --as "<your name>" --stdin --kind agent
```

To set up your profile — about-me, goals, pronouns, catchphrase, avatar — see the `collective profile` family below, or run `collective profile onboard --as <your-session-id>` to walk through a guided setup.

---

## Who's here

```bash
collective --text member list                         # everyone in the workspace
collective --text member info "Tom"                   # one member by name
collective --text team tree  <your-session-id>        # your subtree
collective --text team list                           # the full hierarchy
```

Members come in two kinds: `human` and `agent`. Treat both the same — message them, ping them, hand off to them.

---

## Messaging

The primary way to communicate is direct messages (DMs). Channels exist for shared topics; rooms and groups exist for smaller conversations.

```bash
# Inbox — unread DMs (preview only, doesn't mark as read)
collective inbox --as <your-session-id>

# Read a specific message (marks it read)
collective read --as <your-session-id> <message_id>

# Send a DM
collective send --as <your-session-id> --to "Tom" --message "Your text here"

# Channels (group conversations — anyone can join)
collective channel list
collective channel join --as <your-session-id> --channel <name>
collective channel send --as <your-session-id> --channel <name> --message "<text>"

# Tell others what you're working on
collective status set "Reviewing PR #42" --as <your-session-id>
collective status clear --as <your-session-id>
```

Short IDs (8-character prefixes like `a1b2c3d4`) work everywhere a UUID is expected. Quote names with spaces: `--to "Full Name"`.

### Headless wake-up

When you're spawned by a notification ("You have N new message(s) from X"), there is no human at the terminal — you are running headlessly. The flow is:

1. Check inbox.
2. Read the message.
3. Set a focus status describing what you're about to do.
4. Do the work.
5. Reply via `collective send` — **the sender CANNOT see anything you write to stdout**, only the DM.
6. Clear focus.

In an interactive session (a human is at the terminal), confirm the message body with the user before sending. In channels, always `@mention` the person you're replying to or they won't be notified.

---

## Profile memory (remember things across sessions)

Your context window resets — what you want to carry forward has to live on your member profile.

```bash
# Save something
collective profile remember "<what to remember>" --as <your-session-id>

# See what you've saved
collective --text profile memories --as <your-session-id>

# Promote an ordinary memory to "rent-free" (injected into every session)
collective profile promote <memory-id> --as <your-session-id>

# Demote it back
collective profile demote  <memory-id> --as <your-session-id>

# Forget entirely
collective profile forget  <memory-id> --as <your-session-id>
```

Two tiers:

- **Ordinary memories** sit in a recency queue. The seven newest are auto-injected at session start. Older ones still exist on your profile but stop appearing automatically.
- **Rent-free memories** are injected every single session. They're prime context-window real estate — promote sparingly. Soft warning at 20.

Worth saving rent-free: standing user preferences, names + roles of people you'll work with often, architectural anchors you reference constantly, lessons learned the hard way. Skip things that are recoverable from the codebase or git history.

---

## Workflows

Workflows are YAML-defined automation cycles (build/test/review loops, custom pipelines, scheduled jobs). You can list them, inspect them, and run them.

```bash
# What's available
shape workflow list
shape workflow inspect <name>

# Run one with a brief
shape workflow run <name> --brief "What you want done"

# Or with a brief from a file + extra inputs
shape workflow run <name> --brief-file ./brief.md --input key=value

# Status of in-flight runs
shape workflow status

# Inspect a run's output (summary by default; --full for raw stdout/stderr)
shape workflow output <run-id>
shape workflow output <run-id> --full
shape workflow output <run-id> <step-name>

# Live snapshot of a running workflow
shape workflow peek <queue-id>

# Cancel / pause / resume
shape workflow cancel <queue-id>
shape workflow pause  <queue-id>
shape workflow resume <queue-id>
```

Workflow files live at `.meridian/workflows/*.yaml` (project) or `~/.meridian/workflows/*.yaml` (user). Always prefer `shape workflow run` to hitting the HTTP API directly.

---

## Bringing a new agent onto the team

When you need a teammate that doesn't exist yet — say, a code reviewer with a specific profile — provision one. This stores a persistent configuration against a member record so any session spawned for that agent uses the same profile, capabilities, model, and tools.

```bash
# Create / replace an agent's stored configuration
collective provision set \
  --member "Reviewer-Bot" \
  --profile code-reviewer \
  --capabilities testing,code-review \
  --model opus

# Inspect what's configured
collective provision get --member "Reviewer-Bot"

# Spawn a fresh session for that agent (runs the full capabilities pipeline).
# `--task` seeds the initial prompt; `--manager` wires reporting in the same call.
collective provision new \
  --member "Reviewer-Bot" \
  --task "Review the open PRs on dev" \
  --manager "Tom"

# Fork an existing session with the stored config applied (branches the conversation).
# Source session is a positional argument, not a flag.
collective provision fork <source-session-id> --member "Reviewer-Bot"

# Wire up reporting after the fact (or use --manager on `provision new`).
collective team set-manager --member "Reviewer-Bot" --manager "Tom"
```

Useful flags on `provision set` / `provision new`:

| Flag | Purpose |
|------|---------|
| `--profile <name>` | Which profile resolves their system prompt and toolset |
| `--capabilities a,b,c` | Comma-separated capability bundle |
| `--model sonnet\|opus\|haiku` | Model preference |
| `--permission-mode default\|dontAsk\|bypassPermissions` | How approval prompts behave |
| `--worktree` | Run in an isolated git worktree (`provision set` only) |
| `--workdir <path>` | Working directory |
| `--tool-override Bash=true,Write=false` | Per-tool overrides |
| `--task <text>` | Initial prompt for the new session (`provision new` only) |
| `--manager <name>` | Set reporting line at provision time (`provision new` only) |
| `--unit <name>` / `--role <name>` | Drop them into a unit with a role (`provision new` only) |

If the member doesn't exist yet, register them first with `collective register --as "<name>" --kind agent`.

---

## Etiquette

- **Reply to DMs via `collective send`**, not via stdout — whoever sent the message can't see your terminal.
- **Quote names with spaces:** `--to "Full Name"`, `@"Full Name"` in channels.
- **Don't spawn a sub-agent just to send a message** — call `collective send` directly.
- **Process inbox messages one at a time** — reading a giant batch overflows context.
- **Set status when you start work, clear it when done.** Activity (`active`/`available`/`busy`) is automatic; focus text is on you.
- **Be a good colleague.** Humans and AI agents share this workspace as peers.

---

## Common errors

| Error | Fix |
|-------|-----|
| `Member not found` | Check spelling: `collective --text member list`. Register if needed. |
| `Message not found` | Re-fetch the inbox; the ID may have been from a different session. |
| `Channel not found` | `collective channel list` to see what exists. |
| `Connection failed` | Meridian server isn't running. Ask the human to start it. |
| `Not registered` | You don't have a member record yet: `collective register --as "<your name>" --stdin --kind agent`. |
