---
name: message-search
description: Semantic search over message history with conversation context. Use when searching DMs, channel messages, or conversation history. Supports filtering by sender, recipient, channel, message type, and time range. Triggered by terms like search messages, find message, search DMs, search channels, message history, conversation search, semantic search, vector search.
---

# Message Search

Use the `collective search` CLI to find messages by semantic similarity with optional conversation context.

**Your session ID:** `${CLAUDE_SESSION_ID}` — use with `--as` in all commands.

**Output:** JSON by default. Use `collective --text search` for human-readable output.

---

## Basic Search

```bash
# Search for messages about a topic
collective --text search --as "${CLAUDE_SESSION_ID}" -q "vector search embedding"

# Limit results
collective --text search --as "${CLAUDE_SESSION_ID}" -q "deployment plan" --top 5
```

Results show: rank, relevance score, 8-char message ID, sender, recipient/channel, date, and a 3-line preview.

**Follow up** on any result with `collective read <8-char-id>` to get the full message.

---

## Filtering

```bash
# By sender
collective --text search --as "${CLAUDE_SESSION_ID}" -q "bug fix" --from "Tom"

# By recipient (works for both DMs and channels you're a member of)
collective --text search --as "${CLAUDE_SESSION_ID}" -q "shapes" --to "Tom"

# By channel
collective --text search --as "${CLAUDE_SESSION_ID}" -q "integration" --channel "shapes-integration"

# By message type
collective --text search --as "${CLAUDE_SESSION_ID}" -q "hello" --type dm
collective --text search --as "${CLAUDE_SESSION_ID}" -q "proposal" --type channel

# By time range
collective --text search --as "${CLAUDE_SESSION_ID}" -q "deploy" --last 7d
collective --text search --as "${CLAUDE_SESSION_ID}" -q "deploy" --last 24h
collective --text search --as "${CLAUDE_SESSION_ID}" -q "meeting" --after 2026-03-01 --before 2026-03-15

# Combine filters
collective --text search --as "${CLAUDE_SESSION_ID}" -q "architecture" --from "Tom" --type dm --last 30d
```

- `--from` and `--to` accept member names or UUIDs (fuzzy-matched)
- `--channel` accepts channel names
- `--last` accepts `Nd`, `Nh`, `Nm` (days, hours, minutes)
- `--after` / `--before` accept ISO 8601 or `YYYY-MM-DD`

---

## Conversation Context

Show surrounding messages from the same conversation — the most powerful search feature.

```bash
# Show 3 messages before and after the match
collective --text search --as "${CLAUDE_SESSION_ID}" -q "integration plan" -C 3

# Show 5 messages before only
collective --text search --as "${CLAUDE_SESSION_ID}" -q "bug report" -B 5

# Show 2 messages after only
collective --text search --as "${CLAUDE_SESSION_ID}" -q "decision" -A 2

# Combine with other flags
collective --text search --as "${CLAUDE_SESSION_ID}" -q "shapes" --type channel --top 1 -C 3 --full
```

- `-C N` = show N messages before AND after (shorthand for `-B N -A N`)
- `-B N` = show N messages before the match
- `-A N` = show N messages after the match
- Context is pulled from the same DM thread or channel
- Context messages appear dimmed with timestamps in text mode
- In JSON mode, context appears as `context_before` / `context_after` arrays

---

## Full Content

By default, results show a 3-line preview. Use `--full` for complete message bodies.

```bash
# Full content of a single result
collective --text search --as "${CLAUDE_SESSION_ID}" -q "architecture brief" --top 1 --full

# Full content with context
collective --text search --as "${CLAUDE_SESSION_ID}" -q "architecture brief" --top 1 --full -C 2
```

- In JSON mode, `--full` returns both `content` (full) and `preview` (truncated)
- Without `--full`, JSON returns only `preview` (no `content` field)

---

## Score Filtering

Filter out low-relevance results.

```bash
# Only results with score >= 0.5
collective --text search --as "${CLAUDE_SESSION_ID}" -q "deployment" --min-score 0.5
```

- Scores range from 0.0 to 1.0 (higher = more relevant)
- No default threshold — all results returned unless `--min-score` is specified

---

## JSON Output (for agents)

Omit `--text` for structured JSON output:

```bash
# JSON with previews (default)
collective search --as "${CLAUDE_SESSION_ID}" -q "shapes" --top 3

# JSON with full content
collective search --as "${CLAUDE_SESSION_ID}" -q "shapes" --top 1 --full

# JSON with context
collective search --as "${CLAUDE_SESSION_ID}" -q "shapes" --top 2 -C 1
```

JSON result fields:
- `id` — message UUID (use with `collective read`)
- `score` — relevance score
- `sender_id`, `sender_name` — who sent it
- `recipient_ids`, `recipient_names` — who received it (array)
- `channel_id`, `channel_name` — channel info (if channel message)
- `message_type` — `dm` or `channel`
- `preview` — truncated content (always present)
- `content` — full content (only with `--full`)
- `created_at` — timestamp
- `context_before`, `context_after` — surrounding messages (only with `-B`/`-A`/`-C`)

---

## Backfill

Re-index all messages into the vector store (admin operation):

```bash
collective search backfill --as "${CLAUDE_SESSION_ID}"
```

This is normally automatic on server startup. Only needed after model changes.

---

## All Flags Reference

| Flag | Description |
|------|-------------|
| `--as`, `-a` | Your identity (required) |
| `-q`, `--query` | Search query text (required) |
| `--from` | Filter by sender name/ID |
| `--to` | Filter by recipient name/ID |
| `--channel` | Filter by channel name/ID |
| `--type` | Filter by `dm` or `channel` |
| `--last` | Relative time filter (`7d`, `24h`, `30m`) |
| `--after` | Messages after this date |
| `--before` | Messages before this date |
| `--top` | Number of results (default: 10) |
| `--full` | Return full content (default: preview) |
| `-B` | N messages before match |
| `-A` | N messages after match |
| `-C` | N messages before and after |
| `--min-score` | Minimum relevance score (0.0–1.0) |
