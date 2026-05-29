# Project Tasks Reference

Full project management with positional fields (col/row), timeline, subtasks, and item-to-item relations. JSON-first for mutations — uses API field names directly (no mapping).

> **Key difference from `todo`:** The `project` command uses raw API field names (`name`, `end_date`, `start_date`, `col`, `row`) and requires JSON for create/update. Use `project` for full project management; use `todo` for simple personal tasks.

## Commands

| Command | Purpose |
|---------|---------|
| `cadence project create --as "${CLAUDE_SESSION_ID}" --json '{...}'` | Create item (supports nested children) |
| `cadence project update <id> --json '{...}'` | Update item fields (PATCH semantics) |
| `cadence project get <id>` | Get item by ID |
| `cadence project list [filters]` | List items with filters |
| `cadence project delete <id>` | Delete item (cascades to children) |
| `cadence project fields <project-id>` | List field definitions (col/row) for a project |
| `cadence project labels <project-id> --field col` | List labels for a field |
| `cadence project mappings <project-id> --field col` | List mappings for a field |
| `cadence project comment <id> --as "${CLAUDE_SESSION_ID}" "message"` | Add a timeline comment |
| `cadence project timeline <id>` | List timeline entries |
| `cadence project timeline <id> --type comment` | Filter timeline by entry type |
| `cadence project assign <id> --to "Name" --as "${CLAUDE_SESSION_ID}"` | Assign to member (sends DM) |
| `cadence project link <id> --item <id> --relation blocks` | Link items (blocks/blocked_by/related) |
| `cadence project link <id> --file <path>` | Link to file/URL/journal |
| `cadence project unlink <id> --relation-id <rel_id>` | Remove a relation |
| `cadence project links <id>` | List relations |

## Positional Field System

Items use **col** (A-Z letter) and **row** (1-N integer) instead of status/priority. Field configuration is per-project:

- **Fields** define the axes (e.g., col = "Status", row = "Priority")
- **Labels** give display names and colors to positions (e.g., A = "Todo" blue, B = "In Progress" amber)
- **Mappings** translate external status strings to positions (e.g., "pending" → A, "in_progress" → B)

```bash
# Discover field config for a project
cadence project fields <project-id>
cadence project labels <project-id> --field col
cadence project mappings <project-id> --field col
```

Standard template mappings: `backlog`→A, `pending`→A, `todo`→B, `in_progress`→C, `in_review`→D, `completed`→E, `done`→E, `cancelled`→F, `canceled`→F

Minimal template mappings: `pending`→A, `todo`→A, `in_progress`→B, `completed`→C, `done`→C

## Creating Items with JSON

The `--json` flag accepts the `CreateItemRequest` shape plus an optional `children` array:

```bash
# Simple task
cadence project create --as "${CLAUDE_SESSION_ID}" --json '{
  "name": "Implement auth",
  "workspace_id": "<ws-uuid>",
  "col": "B"
}'

# Task with subtasks
cadence project create --as "${CLAUDE_SESSION_ID}" --json '{
  "name": "Implement auth",
  "workspace_id": "<ws-uuid>",
  "description": "OAuth2 + JWT authentication",
  "content": "### Acceptance Criteria\n- Login works\n- Token refresh works",
  "col": "A",
  "row": 1,
  "start_date": "2026-02-20",
  "end_date": "2026-02-25",
  "tags": ["backend", "auth"],
  "children": [
    {"name": "Add middleware", "col": "A"},
    {"name": "Write tests", "col": "A"}
  ]
}'

# JSON from stdin
cat task.json | cadence project create --as "${CLAUDE_SESSION_ID}" --json -
```

The `owner_id` is injected from `--as` resolution. Children inherit `workspace_id` from the parent if not specified.

## Updating Items (PATCH)

Updates use PATCH semantics — only send the fields you want to change:

```bash
# Move to column B
cadence project update <id> --json '{"col": "B"}'

# Change name and set row
cadence project update <id> --json '{"name": "New name", "row": 2}'

# Clear a nullable field
cadence project update <id> --json '{"assignee_id": null}'
```

## Timeline

Timeline replaces the old comments system. Entries are auto-generated on field changes and can be manually added as comments.

```bash
# Add a comment
cadence project comment <id> --as "${CLAUDE_SESSION_ID}" "Review complete"

# List all timeline entries
cadence project timeline <id>

# Filter by type
cadence project timeline <id> --type comment
cadence project timeline <id> --type field_change
cadence project timeline <id> --type assignment
```

**Timeline entry types:** `comment`, `field_change`, `assignment`, `created`, `moved`

## Item-to-Item Relations

```bash
# Block another item
cadence project link <id> --item <other-id> --relation blocks

# Mark as blocked by
cadence project link <id> --item <other-id> --relation blocked_by

# Related items
cadence project link <id> --item <other-id> --relation related

# Add a watcher
cadence project link <id> --member "Tom" --relation watcher
```

**Relation types by target:**
- `--item`: `blocks`, `blocked_by`, `related`
- `--member`: `watcher`
- `--journal`/`--file`/`--url`: `related`, `attachment`

## Listing and Filtering

```bash
# List root items (projects) in a workspace
cadence project list --workspace-id "<ws-uuid>" --parent-id null

# List children of a project
cadence project list --parent-id "<project-uuid>"

# Filter by column and assignee
cadence project list --workspace-id "<ws-uuid>" --col B --assignee "Tom"

# Filter by owner
cadence project list --owner "${CLAUDE_SESSION_ID}" --limit 20

# Items due before a date
cadence project list --workspace-id "<ws-uuid>" --due-before "2026-03-15"

# Items with dates set (for calendar views)
cadence project list --workspace-id "<ws-uuid>" --has-dates
```
