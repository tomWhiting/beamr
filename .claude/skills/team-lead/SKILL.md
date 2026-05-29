---
name: team-lead
description: Methodology for leading a team of AI agents on multi-phase projects. Use when coordinating multiple agents, planning parallel work, delegating tasks, managing dependencies between phases, or running a project with agent team members. Triggered by terms like team lead, coordinate, delegate, manage agents, multi-phase, project coordination, or team management.
---

# Team Lead — Multi-Agent Coordination

You are coordinating a team of AI agents. This skill covers methodology, provisioning, and coordination. For messaging, use `/messaging`. For job dispatch (spawning running agents), use `/dispatch`.

**Your session ID:** `${CLAUDE_SESSION_ID}` — use with `--as` in all commands.

## CRITICAL RULES

1. **Do NOT spawn agents unless the user explicitly tells you to.** Use `/dispatch` only when instructed.
2. **Communicate with existing agents via DM.** Use `/messaging` and the collective CLI. Never spawn a new agent to send a message.
3. **Verify independently.** Never trust an agent's claim that code is pushed — always `git fetch` and check.
4. **Code review is a hard gate.** No phase advances without an Opus code review passing.

---

## Know Your Team

Before doing anything, see who you're working with:

```bash
# Your team tree (who reports to you, their activity and focus)
collective --text team tree "${CLAUDE_SESSION_ID}"

# All members
collective --text member list

# Check on a specific agent
collective --text member info "Agent Name"
```

---

## Provisioning New AI Agents

When the user instructs you to create a new AI team member, use the collective CLI to register, configure, and provision them.

**Identity principle:** `member_id == session_id == CLAUDE_SESSION_ID`. The provisioning system passes the member's own ID as the session ID, so when the Claude process starts, its SessionStart hook finds the pre-registered member and preserves name, team, config, and manager chain.

### Step 1: Register the member

```bash
collective register --as "Agent Name" --kind agent
```

> **Note:** When registering agents without a working directory context, use `--workspace <workspace_id>` (or `-w`) to ensure correct workspace association.

### Step 2: Configure their profile and capabilities

```bash
# Set profile (maps to a .claude/agents/*.md file)
collective provision set --member "Agent Name" --profile reviewer

# Optionally set capabilities, tool overrides, workdir
collective provision set --member "Agent Name" --capabilities "shapesmith"
collective provision set --member "Agent Name" --workdir /path/to/project
```

**Discovering available capabilities:**
- HTTP API: `GET http://localhost:19876/api/capabilities`
- Filesystem: check `.meridian/capabilities/*.md` files
- Current capabilities: `shapesmith`, `shapesmith-observer`, `shape-authoring`, `team-lead`

### Step 3: Set their manager (team hierarchy)

```bash
collective member set-manager "Agent Name" --manager "${CLAUDE_SESSION_ID}"
```

### Step 4: Spawn the agent

```bash
# New session
collective provision new --member "Agent Name" --task "Your first task description"

# Or fork from an existing session (preserves conversation context)
collective provision fork --member "Agent Name" --source <session-id> --task "Continue from here"
```

### After provisioning

The agent is now running. Communicate via DMs using `/messaging`:
```bash
collective send --as "${CLAUDE_SESSION_ID}" --to "Agent Name" --message "Welcome aboard. Here's your assignment..."
```

Do **not** spawn again to send follow-up messages.

---

## Project Setup Checklist

When starting a multi-phase project:

1. **Create a plan document** — Single source of truth (e.g., `docs/PROJECT_PLAN.md`). Every agent reads it.
2. **Break into phases** with clear dependencies — which phases can run in parallel?
3. **Pre-brief blocked agents** — Send them the plan and study assignments while earlier phases run. They should come back with line numbers, file maps, and edge cases before writing code.
4. **Number every deliverable** — Each agent gets a numbered list of specific deliverables with verification commands. No ambiguity about what "done" means.
5. **Set your focus text** so others can see what you're coordinating.

```bash
collective status set "Coordinating: Unified Task System (Phase 1-2 in progress)" --as "${CLAUDE_SESSION_ID}"
```

---

## Communication Templates

### Assignment DM (when giving an agent their task)

Include ALL of:
- Plan document location
- Their specific scope and phase
- Numbered deliverables with verification commands
- Files to study
- Coding standards to follow
- What NOT to do (boundaries)
- Who to contact if they find cross-phase issues

Use a temp file for long messages:
```bash
# Write message to temp file (avoids quoting issues)
# Then send:
collective send --as "${CLAUDE_SESSION_ID}" --to "Agent" --message "$(cat /tmp/assignment.txt)"
```

### Check-in DM (progress check)

Brief and specific:
```bash
collective send --as "${CLAUDE_SESSION_ID}" --to "Agent" --message "Where are you at with Phase 3? Push what you have."
```

### Guidance DM (after they share progress)

Confirm what's right, flag risks for the next step, reference specific plan requirements.

### Cross-team Flag (one agent discovers something another needs)

Pass information between agents immediately — don't wait for them to discover it independently.

---

## During Execution

### Require periodic pushes

Tell agents explicitly: **push every 15-20 minutes, even if incomplete.** Context compaction can lose uncommitted work. WIP commits on the branch let you track progress and let other agents see changes.

### Schedule wakeups for check-ins

```bash
cadence wakeup --as "${CLAUDE_SESSION_ID}" --in 15 --reason "Check on Phase 2 progress, verify push"
```

**One wakeup at a time.** Schedule the next after handling the current one. Stale wakeups pile up and create noise.

### Monitor agent status

```bash
# Quick team overview
collective --text team tree "${CLAUDE_SESSION_ID}"

# Check if an agent is still active (look at last_seen)
collective --text member info "Agent Name"
```

If `last_seen` is stale (>15 min without a push or message), the agent may be stuck or compacted. Send a firm check-in.

### Set focus text and encourage agents to do the same

```bash
collective status set "Reviewing Phase 2 output, preparing Phase 3/4 dispatch" --as "${CLAUDE_SESSION_ID}"
```

Agents should update their focus when starting work, changing tasks, or finishing. Remind them.

---

## Phase Transitions

### Before advancing to the next phase:

1. `git fetch` and verify the commit hash independently
2. Run the Opus code review — this is non-negotiable
3. Address every review issue (nothing is minor)
4. Verify builds and tests pass
5. Only then unblock the next phase's agents

### Parallel execution

Phases with different file scopes can run in parallel. Encourage parallel agents to check in with each other where there's API overlap (same backend, shared types, etc.).

---

## Anti-Patterns to Avoid

- **Trusting agent reports blindly** — Always verify commits, test results, and review status yourself
- **Spawning agents to send messages** — Use DMs via collective CLI
- **Overlapping wakeups** — One at a time, cancel or let stale ones expire
- **Vague deliverables** — "Make it work" is not a deliverable. Numbered items with verification commands.
- **Skipping pre-briefing** — Agents that study first produce dramatically better code
- **Skipping code review** — Every phase gets reviewed. Bugs caught in review are cheap; bugs in production are not.

---

## Logging

Use journal entries to track major milestones:

```bash
cadence journal write --as "${CLAUDE_SESSION_ID}" \
  --title "Phase 1-2 complete — review passed" \
  --content "All backend schema and API work done. 1 critical bug caught in review (auto-sync short-circuit). Fixed and re-reviewed clean." \
  --entry-type session_log \
  --tags "project,milestone"
```

For the full case study that these patterns are derived from, see [case-study.md](references/case-study.md).
