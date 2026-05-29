# Hooks Reference

Hooks run scripts automatically at specific points during Claude Code sessions.

## Hook Events

| Hook | When it Fires | Matcher Input |
|------|--------------|---------------|
| `SessionStart` | Session begins or resumes | `startup`, `resume`, `clear`, `compact` |
| `UserPromptSubmit` | User submits a prompt | (none) |
| `PreToolUse` | Before tool execution | Tool name |
| `PermissionRequest` | Permission dialog appears | Tool name |
| `PostToolUse` | After tool succeeds | Tool name |
| `PostToolUseFailure` | After tool fails | Tool name |
| `SubagentStart` | When spawning a subagent | Agent type name |
| `SubagentStop` | When subagent finishes | Agent type name |
| `Stop` | Claude finishes responding | (none) |
| `PreCompact` | Before context compaction | `manual`, `auto` |
| `Setup` | `--init`, `--init-only`, or `--maintenance` flags | `init`, `maintenance` |
| `SessionEnd` | Session terminates | (none) |
| `Notification` | Claude Code sends notifications | `permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_dialog` |

## Configuration

Hooks are configured in settings files:
- `~/.claude/settings.json` - User settings
- `.claude/settings.json` - Project settings
- `.claude/settings.local.json` - Local project settings

### Structure

```json
{
  "hooks": {
    "EventName": [
      {
        "matcher": "ToolPattern",
        "hooks": [
          {
            "type": "command",
            "command": "your-command-here",
            "timeout": 60
          }
        ]
      }
    ]
  }
}
```

**Matcher patterns:**
- Simple string: `Write` matches only Write tool
- Regex: `Edit|Write` or `Notebook.*`
- `*` or `""` matches all tools

### Hook Types

| Type | Description |
|------|-------------|
| `command` | Execute bash command |
| `prompt` | LLM-based evaluation (uses `$ARGUMENTS` placeholder) |

### Environment Variables

- `CLAUDE_PROJECT_DIR` - Absolute path to project root
- `CLAUDE_PLUGIN_ROOT` - Plugin directory (for plugin hooks)
- `CLAUDE_ENV_FILE` - File path for persisting env vars (SessionStart/Setup only)
- `CLAUDE_CODE_REMOTE` - `"true"` if running in web environment

## Hook Input (stdin)

All hooks receive JSON via stdin:

```json
{
  "session_id": "abc123",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/Users/...",
  "permission_mode": "default",
  "hook_event_name": "PreToolUse",
  "tool_name": "Bash",
  "tool_input": { ... },
  "tool_use_id": "toolu_01ABC123..."
}
```

### Tool-Specific Input

**Bash:**
```json
{
  "tool_input": {
    "command": "psql -c 'SELECT * FROM users'",
    "description": "Query the users table",
    "timeout": 120000,
    "run_in_background": false
  }
}
```

**Write:**
```json
{
  "tool_input": {
    "file_path": "/path/to/file.txt",
    "content": "file content"
  }
}
```

**Edit:**
```json
{
  "tool_input": {
    "file_path": "/path/to/file.txt",
    "old_string": "original text",
    "new_string": "replacement text",
    "replace_all": false
  }
}
```

**Read:**
```json
{
  "tool_input": {
    "file_path": "/path/to/file.txt",
    "offset": 0,
    "limit": 100
  }
}
```

## Hook Output

### Exit Codes

| Code | Behavior |
|------|----------|
| `0` | Success. stdout shown in verbose mode (or added to context for UserPromptSubmit/SessionStart) |
| `2` | Blocking error. stderr fed back to Claude |
| Other | Non-blocking error. stderr shown in verbose mode |

### Exit Code 2 Behavior by Event

| Event | Behavior |
|-------|----------|
| `PreToolUse` | Blocks tool call, shows stderr to Claude |
| `PermissionRequest` | Denies permission, shows stderr to Claude |
| `PostToolUse` | Shows stderr to Claude |
| `UserPromptSubmit` | Blocks prompt, erases prompt, shows stderr to user |
| `Stop` / `SubagentStop` | Blocks stoppage, shows stderr to Claude |
| `Notification` / `Setup` / `SessionStart` / `SessionEnd` / `PreCompact` | N/A, shows stderr to user only |

### JSON Output (Exit Code 0)

**Common fields:**
```json
{
  "continue": true,
  "stopReason": "string",
  "suppressOutput": true,
  "systemMessage": "warning message"
}
```

**PreToolUse decision control:**
```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Auto-approved",
    "updatedInput": { "field": "new value" },
    "additionalContext": "Context for Claude"
  }
}
```

`permissionDecision`: `"allow"`, `"deny"`, or `"ask"`

**PermissionRequest decision control:**
```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "allow",
      "updatedInput": { "command": "npm run lint" }
    }
  }
}
```

**PostToolUse decision control:**
```json
{
  "decision": "block",
  "reason": "Explanation for Claude",
  "hookSpecificOutput": {
    "hookEventName": "PostToolUse",
    "additionalContext": "Additional info for Claude"
  }
}
```

**UserPromptSubmit:**
```json
{
  "decision": "block",
  "reason": "Shown to user",
  "hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": "Added to context"
  }
}
```

**Stop/SubagentStop:**
```json
{
  "decision": "block",
  "reason": "Must continue because..."
}
```

**SessionStart/Setup:**
```json
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "Context to inject"
  }
}
```

## Prompt-Based Hooks

Use LLM evaluation for context-aware decisions (primarily for Stop/SubagentStop):

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "prompt",
            "prompt": "Evaluate if Claude should stop: $ARGUMENTS. Check if all tasks are complete.",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

LLM must respond with:
```json
{
  "ok": true,
  "reason": "Explanation (required when ok is false)"
}
```

## Persisting Environment Variables

For SessionStart/Setup hooks, write to `CLAUDE_ENV_FILE`:

```bash
#!/bin/bash
if [ -n "$CLAUDE_ENV_FILE" ]; then
  echo 'export NODE_ENV=production' >> "$CLAUDE_ENV_FILE"
  echo 'export API_KEY=your-api-key' >> "$CLAUDE_ENV_FILE"
fi
exit 0
```

## MCP Tool Hooks

MCP tools follow pattern `mcp__<server>__<tool>`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "mcp__memory__.*",
        "hooks": [
          {
            "type": "command",
            "command": "echo 'Memory operation initiated' >> ~/mcp-operations.log"
          }
        ]
      }
    ]
  }
}
```

## Execution Details

- **Timeout:** 60 seconds default, configurable per command
- **Parallelization:** All matching hooks run in parallel
- **Deduplication:** Identical hook commands deduplicated automatically
- **Snapshot:** Hooks captured at startup; external modifications require restart or `/hooks` review

## Debugging

```bash
claude --debug    # See hook execution details
```

Check:
1. Configuration with `/hooks`
2. JSON syntax in settings files
3. Script permissions (`chmod +x`)
4. Command paths (use absolute paths)

## Security

- Hooks execute arbitrary shell commands
- Validate and sanitize inputs
- Always quote shell variables: `"$VAR"` not `$VAR`
- Block path traversal (check for `..`)
- Skip sensitive files (`.env`, `.git/`, keys)
