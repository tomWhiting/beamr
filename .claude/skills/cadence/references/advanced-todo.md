# Advanced To-Do Features

## JSON Input Mode

Both `add` and `update` accept `--json` as an alternative to flags. JSON uses todo-friendly field names which get mapped automatically:

| JSON field | API field |
|-----------|-----------|
| `title` | `name` |
| `due` | `end_date` |
| `start` | `start_date` |

API field names (`name`, `end_date`, `start_date`) are also accepted directly.

```bash
# Add via JSON
cadence todo add --as "${CLAUDE_SESSION_ID}" --json '{
  "title": "Deploy staging build",
  "description": "Run full pipeline",
  "due": "2026-02-10T17:00:00",
  "col": "A",
  "tags": ["deploy", "staging"]
}'

# Update via JSON
cadence todo update <id> --as "${CLAUDE_SESSION_ID}" --json '{
  "title": "New title",
  "col": "B"
}'

# JSON from stdin
echo '{"title": "Quick task"}' | cadence todo add --as "${CLAUDE_SESSION_ID}" --json -
```

## Timeline (replaces Comments)

```bash
# Add a comment to a to-do's timeline
cadence todo comment <id> --as "${CLAUDE_SESSION_ID}" "Progress update"

# List timeline entries for a to-do
cadence todo timeline <id>

# Filter by entry type
cadence todo timeline <id> --type comment
cadence todo timeline <id> --type field_change
```

**Timeline entry types:** `comment`, `field_change`, `assignment`, `created`, `moved`

Timeline entries are auto-generated when col, row, or assignee changes. Manual comments use the `comment` subcommand.

## Linking

Link a to-do to journal entries, files, or URLs. This creates item relations in the Unified Task System.

```bash
# Link to a journal entry
cadence todo link <id> --journal <journal_id>

# Link to a file
cadence todo link <id> --file "src/main.rs"

# Link to a URL
cadence todo link <id> --url "https://github.com/org/repo/pull/42"

# Specify relation type (default: "related")
cadence todo link <id> --journal <journal_id> --relation "attachment"

# List all links for a to-do
cadence todo links <id>

# Remove a link
cadence todo unlink <id> --relation-id <rel_id>
```

**Relation types:** `related` (default), `attachment`, `blocks`, `blocked_by`

## Additional Flags

Available on `add` and `update`:

| Flag | Purpose |
|------|---------|
| `--content <text>` | Rich markdown body |
| `--col <letter>` | Column position (A-Z) |
| `--row <number>` | Row position (1-N integer) |
| `--start-date <date>` | Start date (same timezone handling as `--due`) |
| `--parent-id <uuid>` | Nest under a project or task |
| `--tags <csv>` | Comma-separated tags |
| `--json <json>` | JSON input mode (replaces all other flags) |

For `update --parent-id`: use `"null"` to make a to-do root-level again.

## Filtering

```bash
# Filter by column position
cadence todo list --as "${CLAUDE_SESSION_ID}" --col A

# Filter by row position
cadence todo list --as "${CLAUDE_SESSION_ID}" --row 1

# Filter by assignee
cadence todo list --as "${CLAUDE_SESSION_ID}" --assignee "Tom"

# Filter by owner
cadence todo list --as "${CLAUDE_SESSION_ID}" --owner "${CLAUDE_SESSION_ID}"

# Filter by due date
cadence todo list --as "${CLAUDE_SESSION_ID}" --due-before "2026-03-15T00:00:00"

# Filter by tag
cadence todo list --as "${CLAUDE_SESSION_ID}" --tag deploy

# Combine filters
cadence todo list --as "${CLAUDE_SESSION_ID}" --col B --assignee "Tom" --limit 5
```
