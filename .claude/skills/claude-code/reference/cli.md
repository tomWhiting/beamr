# CLI Reference

## Commands

| Command | Description |
|---------|-------------|
| `claude` | Start interactive REPL |
| `claude "query"` | Start REPL with initial prompt |
| `claude -p "query"` | Print mode (non-interactive) |
| `cat file \| claude -p "query"` | Process piped content |
| `claude -c` | Continue most recent conversation |
| `claude -c -p "query"` | Continue in print mode |
| `claude -r "<session>" "query"` | Resume session by ID or name |
| `claude update` | Update to latest version |
| `claude mcp` | Configure MCP servers |

## Flags

### Session Management

| Flag | Description |
|------|-------------|
| `--continue`, `-c` | Load most recent conversation |
| `--resume`, `-r` | Resume session by ID or name |
| `--fork-session` | Create new session ID when resuming |
| `--session-id` | Use specific session ID (must be valid UUID) |
| `--no-session-persistence` | Disable session persistence (print mode only) |

### Model & System Prompt

| Flag | Description |
|------|-------------|
| `--model` | Model alias (`sonnet`, `opus`) or full name |
| `--fallback-model` | Fallback model when default is overloaded (print mode only) |
| `--system-prompt` | Replace entire system prompt |
| `--system-prompt-file` | Replace system prompt from file (print mode only) |
| `--append-system-prompt` | Append to default system prompt |
| `--append-system-prompt-file` | Append from file (print mode only) |

### Tools & Permissions

| Flag | Description |
|------|-------------|
| `--allowedTools` | Tools that execute without permission prompt |
| `--disallowedTools` | Tools removed from model's context |
| `--tools` | Restrict available tools (`""`, `"default"`, or `"Bash,Edit,Read"`) |
| `--permission-mode` | Start in specific mode (`plan`, `default`, etc.) |
| `--dangerously-skip-permissions` | Skip all permission prompts |
| `--allow-dangerously-skip-permissions` | Enable bypass option without activating |
| `--permission-prompt-tool` | MCP tool for permission prompts (non-interactive) |

### Agents & Configuration

| Flag | Description |
|------|-------------|
| `--agent` | Specify agent for current session |
| `--agents` | Define custom subagents via JSON |
| `--mcp-config` | Load MCP servers from JSON files/strings |
| `--strict-mcp-config` | Only use MCP servers from `--mcp-config` |
| `--plugin-dir` | Load plugins from directories (repeatable) |
| `--settings` | Path to settings JSON file or JSON string |
| `--setting-sources` | Comma-separated setting sources (`user`, `project`, `local`) |

### Output & Debugging

| Flag | Description |
|------|-------------|
| `--print`, `-p` | Print response without interactive mode |
| `--output-format` | Output format: `text`, `json`, `stream-json` |
| `--input-format` | Input format: `text`, `stream-json` |
| `--include-partial-messages` | Include partial streaming events |
| `--json-schema` | Get validated JSON output matching schema |
| `--verbose` | Enable verbose logging |
| `--debug` | Enable debug mode (optional category filter) |

### Execution Limits

| Flag | Description |
|------|-------------|
| `--max-turns` | Limit agentic turns (print mode only) |
| `--max-budget-usd` | Maximum API spend before stopping (print mode only) |

### Other

| Flag | Description |
|------|-------------|
| `--add-dir` | Add additional working directories |
| `--betas` | Beta headers for API requests |
| `--chrome` / `--no-chrome` | Enable/disable Chrome integration |
| `--ide` | Auto-connect to IDE on startup |
| `--init` | Run Setup hooks and start interactive mode |
| `--init-only` | Run Setup hooks and exit |
| `--maintenance` | Run Setup hooks with maintenance trigger and exit |
| `--remote` | Create web session on claude.ai |
| `--teleport` | Resume web session locally |
| `--disable-slash-commands` | Disable all skills and slash commands |
| `--version`, `-v` | Output version number |

## Agents Flag Format

```bash
claude --agents '{
  "code-reviewer": {
    "description": "Expert code reviewer. Use proactively after code changes.",
    "prompt": "You are a senior code reviewer...",
    "tools": ["Read", "Grep", "Glob", "Bash"],
    "model": "sonnet"
  }
}'
```

| Field | Required | Description |
|-------|----------|-------------|
| `description` | Yes | When the subagent should be invoked |
| `prompt` | Yes | System prompt for the subagent |
| `tools` | No | Array of allowed tools |
| `model` | No | `sonnet`, `opus`, `haiku`, or `inherit` |

## System Prompt Flags

| Flag | Behavior | Modes |
|------|----------|-------|
| `--system-prompt` | Replaces entire default prompt | Interactive + Print |
| `--system-prompt-file` | Replaces with file contents | Print only |
| `--append-system-prompt` | Appends to default prompt | Interactive + Print |
| `--append-system-prompt-file` | Appends file contents | Print only |

`--system-prompt` and `--system-prompt-file` are mutually exclusive. Append flags can be used with either.

## Permission Rule Syntax

For `--allowedTools`:
- `Read` - Exact tool match
- `Bash(git log *)` - Bash with pattern matching
- `Bash(git diff *)` - Multiple patterns

For `--disallowedTools`:
- Same syntax, removes tools from context
