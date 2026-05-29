# Custom Subagents

Subagents are specialized AI assistants that handle specific tasks. Each runs in its own context window with a custom system prompt, specific tool access, and independent permissions.

## Benefits

- **Preserve context** - Keep exploration/implementation out of main conversation
- **Enforce constraints** - Limit which tools a subagent can use
- **Reuse configurations** - User-level subagents work across projects
- **Specialize behavior** - Focused system prompts for specific domains
- **Control costs** - Route tasks to cheaper/faster models like Haiku

## Built-in Subagents

| Agent | Model | Tools | Purpose |
|-------|-------|-------|---------|
| **Explore** | Haiku | Read-only | File discovery, code search, codebase exploration |
| **Plan** | Inherits | Read-only | Codebase research during plan mode |
| **general-purpose** | Inherits | All | Complex research, multi-step operations |
| **Bash** | Inherits | Bash | Terminal commands in separate context |
| **statusline-setup** | Sonnet | Read, Edit | Configure status line via `/statusline` |
| **Claude Code Guide** | Haiku | Read-only | Answer questions about Claude Code features |

**Explore thoroughness levels:** `quick` (targeted lookups), `medium` (balanced), `very thorough` (comprehensive analysis)

## Creating Subagents

### Using /agents Command

```
/agents
```
- View all available subagents
- Create new subagents with guided setup
- Edit existing configurations
- See which subagents are active when duplicates exist

### Subagent Locations (Priority Order)

| Location | Scope | Priority |
|----------|-------|----------|
| `--agents` CLI flag | Current session | 1 (highest) |
| `.claude/agents/` | Current project | 2 |
| `~/.claude/agents/` | All your projects | 3 |
| Plugin's `agents/` | Where plugin is enabled | 4 (lowest) |

### File Format

Subagent files use YAML frontmatter + markdown system prompt:

```markdown
---
name: code-reviewer
description: Reviews code for quality and best practices
tools: Read, Glob, Grep
model: sonnet
---

You are a code reviewer. Analyze code and provide
specific, actionable feedback on quality, security, and best practices.
```

### Frontmatter Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Unique identifier (lowercase, hyphens) |
| `description` | Yes | When Claude should delegate to this subagent |
| `tools` | No | Tools the subagent can use (inherits all if omitted) |
| `disallowedTools` | No | Tools to deny |
| `model` | No | `sonnet`, `opus`, `haiku`, or `inherit` (default: inherit) |
| `permissionMode` | No | `default`, `acceptEdits`, `dontAsk`, `bypassPermissions`, `plan` |
| `skills` | No | Skills to preload into subagent's context |
| `hooks` | No | Lifecycle hooks scoped to this subagent |

### CLI-Defined Subagents

```bash
claude --agents '{
  "code-reviewer": {
    "description": "Expert code reviewer. Use proactively after code changes.",
    "prompt": "You are a senior code reviewer. Focus on code quality, security, and best practices.",
    "tools": ["Read", "Grep", "Glob", "Bash"],
    "model": "sonnet"
  }
}'
```

## Permission Modes

| Mode | Behavior |
|------|----------|
| `default` | Standard permission checking with prompts |
| `acceptEdits` | Auto-accept file edits |
| `dontAsk` | Auto-deny permission prompts |
| `bypassPermissions` | Skip all permission checks (use with caution) |
| `plan` | Plan mode (read-only exploration) |

## Preloading Skills

```yaml
---
name: api-developer
description: Implement API endpoints following team conventions
skills:
  - api-conventions
  - error-handling-patterns
---

Implement API endpoints. Follow the conventions and patterns from the preloaded skills.
```

## Subagent Hooks

### In Frontmatter

```yaml
---
name: code-reviewer
description: Review code changes with automatic linting
hooks:
  PreToolUse:
    - matcher: "Bash"
      hooks:
        - type: command
          command: "./scripts/validate-command.sh $TOOL_INPUT"
  PostToolUse:
    - matcher: "Edit|Write"
      hooks:
        - type: command
          command: "./scripts/run-linter.sh"
---
```

### In settings.json (Project-Level)

```json
{
  "hooks": {
    "SubagentStart": [
      {
        "matcher": "db-agent",
        "hooks": [
          { "type": "command", "command": "./scripts/setup-db-connection.sh" }
        ]
      }
    ],
    "SubagentStop": [
      {
        "matcher": "db-agent",
        "hooks": [
          { "type": "command", "command": "./scripts/cleanup-db-connection.sh" }
        ]
      }
    ]
  }
}
```

## Running Subagents

### Foreground vs Background

- **Foreground**: Blocks main conversation; permission prompts pass through to user
- **Background**: Runs concurrently; prompts for permissions upfront, auto-denies unpre-approved ones

Press **Ctrl+B** to background a running task.

Disable background tasks: `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1`

### Resuming Subagents

Each invocation creates a new instance. To continue existing work:

```
Continue that code review and now analyze the authorization logic
```

Transcripts stored at: `~/.claude/projects/{project}/{sessionId}/subagents/agent-{agentId}.jsonl`

### Auto-Compaction

Subagents auto-compact at ~95% capacity. Adjust with: `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=50`

## Disabling Subagents

In settings:
```json
{
  "permissions": {
    "deny": ["Task(Explore)", "Task(my-custom-agent)"]
  }
}
```

Or via CLI:
```bash
claude --disallowedTools "Task(Explore)"
```

## Use Cases

**Isolate high-volume operations:**
```
Use a subagent to run the test suite and report only the failing tests
```

**Parallel research:**
```
Research the authentication, database, and API modules in parallel using separate subagents
```

**Chain subagents:**
```
Use the code-reviewer subagent to find performance issues, then use the optimizer to fix them
```

## When to Use Subagents vs Main Conversation

**Use main conversation when:**
- Task needs frequent back-and-forth
- Multiple phases share significant context
- Making a quick, targeted change
- Latency matters

**Use subagents when:**
- Task produces verbose output you don't need in main context
- You want to enforce specific tool restrictions
- Work is self-contained and can return a summary
