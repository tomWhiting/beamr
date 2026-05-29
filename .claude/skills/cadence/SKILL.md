---
name: cadence
description: Personal organization system for AI agents and humans. Use when managing calendar events, to-do items, journal entries, project tasks, or scheduling agent wakeups. Triggered by terms like calendar, schedule, to-do, task, reminder, journal, note, wakeup, organize, project, cadence.
---

# Cadence — Personal Organization

Use the `cadence` CLI to manage to-do items, calendar events, journal entries, project tasks, and agent wakeups.

**Your session ID:** `${CLAUDE_SESSION_ID}` — use with `--as` in all commands.

**Output:** JSON by default. Use `cadence --text <command>` for human-readable output.

**Timestamps:** Bare datetimes (no `Z` or `+HH:MM`) are converted to UTC using your registered timezone. Date-only strings (`2026-02-20`) are left as-is.

---

## To-Do Items

Personal task tracking with positional fields (col/row) and due dates.

> Backend: `--title` maps to API field `name`, `--due` maps to `end_date`. Responses mapped back automatically.

```bash
# List your todos
cadence todo list --as "${CLAUDE_SESSION_ID}"
cadence todo list --as "${CLAUDE_SESSION_ID}" --col A --limit 10
cadence todo list --as "${CLAUDE_SESSION_ID}" --assignee "${CLAUDE_SESSION_ID}"

# Add a todo
cadence todo add --as "${CLAUDE_SESSION_ID}" --title "Review PR #42"
cadence todo add --as "${CLAUDE_SESSION_ID}" --title "Deploy staging" --col A --due "2026-02-10T17:00:00"

# Get / update / delete
cadence todo get <id>
cadence todo update <id> --title "New title" --col B
cadence todo delete <id>

# Assign to a team member (sends DM notification)
cadence todo assign <id> --to "Tom" --as "${CLAUDE_SESSION_ID}"

# Timeline (replaces comments)
cadence todo comment <id> --as "${CLAUDE_SESSION_ID}" "Progress update"
cadence todo timeline <id>
cadence todo timeline <id> --type comment
```

**Positional fields:** Items use `col` (A-Z letter) and `row` (1-N integer) instead of status/priority. Column labels are configured per-project (e.g., A=Todo, B=In Progress, C=Done).

For advanced features (JSON input, linking, `--content`, `--start-date`, `--parent-id`), see [advanced-todo.md](references/advanced-todo.md).

---

## Project Tasks

Full project management with positional fields, field configuration, timeline, subtasks, and relations. JSON-first — uses API field names directly.

```bash
# List field definitions for a project (discover col/row labels and mappings)
cadence project fields <project-id>

# Create a task with subtasks
cadence project create --as "${CLAUDE_SESSION_ID}" --json '{
  "name": "Implement auth",
  "workspace_id": "<ws-uuid>",
  "col": "A",
  "children": [{"name": "Add middleware"}, {"name": "Write tests"}]
}'

# List / filter
cadence project list --workspace-id "<ws-uuid>" --parent-id null
cadence project list --assignee "Tom" --col B

# Timeline, assignment, field config
cadence project comment <id> --as "${CLAUDE_SESSION_ID}" "Review complete"
cadence project timeline <id>
cadence project assign <id> --to "Name" --as "${CLAUDE_SESSION_ID}"
cadence project labels <project-id> --field col
cadence project mappings <project-id> --field col

# Item-to-item relations
cadence project link <id> --item <other-id> --relation blocks
```

For complete reference (all commands, field configuration, JSON schemas, relation types), see [project-tasks.md](references/project-tasks.md).

---

## Calendar Events

```bash
# Upcoming events (next 24h)
cadence calendar upcoming --as "${CLAUDE_SESSION_ID}"
cadence calendar upcoming --as "${CLAUDE_SESSION_ID}" --hours 8

# Create an event (local time — converted to UTC via your timezone)
cadence calendar create --as "${CLAUDE_SESSION_ID}" \
  --title "Stand-up sync" \
  --start "2026-02-10T09:00:00" \
  --end "2026-02-10T09:30:00" \
  --event-type meeting

# List / filter
cadence calendar list --as "${CLAUDE_SESSION_ID}"
cadence calendar list --as "${CLAUDE_SESSION_ID}" --event-type meeting

# Manage
cadence calendar get <id>
cadence calendar update <id> --title "New title"
cadence calendar delete <id>

# Invitations
cadence calendar invite --member "Tom" <event_id>
cadence calendar respond --as "${CLAUDE_SESSION_ID}" <event_id> --status accepted

# Recurring events (RFC 5545 RRULE)
cadence calendar create --as "${CLAUDE_SESSION_ID}" \
  --title "Weekly review" \
  --start "2026-02-10T16:00:00" \
  --recurrence "FREQ=WEEKLY;BYDAY=MO"
```

**Event types:** `meeting`, `reminder`, `focus_time`, `wakeup`

---

## Journal Entries

```bash
# Write an entry
cadence journal write --as "${CLAUDE_SESSION_ID}" \
  --content "Discovered the auth bug is in the token refresh logic"

# With metadata
cadence journal write --as "${CLAUDE_SESSION_ID}" \
  --title "Session summary" \
  --content "Completed LSP review. Remaining: integration tests." \
  --entry-type session_log \
  --tags "lsp,review"

# List / search
cadence journal list --as "${CLAUDE_SESSION_ID}"
cadence journal list --as "${CLAUDE_SESSION_ID}" --search "auth bug"
cadence journal list --as "${CLAUDE_SESSION_ID}" --entry-type session_log

# Read / update / delete
cadence journal read <id>
cadence journal update <id> --content "Updated content"
cadence journal delete <id>
```

**Entry types:** `session_log`, `note` (default), `reflection`, `reference`

---

## Wakeup Scheduling

Schedule a short-term self-wakeup (1-120 minutes). You'll be woken with the reason as context.

```bash
cadence wakeup --as "${CLAUDE_SESSION_ID}" --in 30 --reason "Check if CI pipeline completed"
```

---

## Error Handling

| Error | Fix |
|-------|-----|
| "Member not found" | Check `--as` value, use `collective --text member list` |
| "Item not found" | List items to find valid IDs |
| Connection failed | Check if Meridian server is up |
