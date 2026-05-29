---
name: claude-code-guide
description: Claude Code reference documentation for features, settings, CLI, hooks, skills, plugins, MCP servers, and sub-agents. Use when questions arise about Claude Code capabilities, configuration options, CLI flags, permission rules, hook events, skill authoring, plugin development, or MCP integration.
---

# Claude Code Guide

Reference documentation for Claude Code features and configuration.

## Contents

| Topic | File | Use For |
|:------|:-----|:--------|
| How Claude Code Works | [guide/how-claude-code-works.md](guide/how-claude-code-works.md) | Agentic loop, models, tools, context management, safety |
| Sub-agents | [guide/sub-agents.md](guide/sub-agents.md) | Built-in agents (Explore, Plan), custom agents, Task tool |
| Best Practices | [guide/best-practices.md](guide/best-practices.md) | Verification criteria, context management, failure patterns |
| MCP Integration | [guide/mcp-and-claude-code.md](guide/mcp-and-claude-code.md) | MCP server setup, tool search, managed configuration |
| Settings | [reference/settings.md](reference/settings.md) | All settings, permissions, sandbox, environment variables |
| CLI | [reference/cli.md](reference/cli.md) | Commands, flags, session management, agents flag |
| Hooks | [reference/hooks.md](reference/hooks.md) | Hook events, input/output schemas, exit codes |
| Skills | [reference/skills.md](reference/skills.md) | Skill locations, frontmatter fields, invocation control |
| Skills Overview | [reference/skills/skills-overview.md](reference/skills/skills-overview.md) | Progressive loading, platform availability, limitations |
| Skills Best Practices | [reference/skills/skills-best-practices.md](reference/skills/skills-best-practices.md) | Authoring guidelines, patterns, evaluation |
| Plugins | [reference/plugins.md](reference/plugins.md) | Plugin manifest, components, CLI commands |

## Quick Reference

### Configuration Scopes

| Scope | Location | Shared? |
|:------|:---------|:--------|
| Managed | System `managed-settings.json` | Yes (IT) |
| User | `~/.claude/settings.json` | No |
| Project | `.claude/settings.json` | Yes (git) |
| Local | `.claude/settings.local.json` | No |

### Key Settings

| Setting | Purpose |
|:--------|:--------|
| `model` | Default model override |
| `permissions.allow/deny` | Tool permission rules |
| `hooks` | Pre/post tool execution commands |
| `mcpServers` | MCP server configuration |
| `sandbox.enabled` | Bash sandboxing |

### Built-in Agents

| Agent | Purpose |
|:------|:--------|
| `Explore` | Fast codebase exploration, file search |
| `Plan` | Architecture design, implementation planning |
| `general-purpose` | Multi-step tasks with full tool access |

### Hook Events

| Event | When |
|:------|:-----|
| `PreToolUse` | Before tool execution |
| `PostToolUse` | After successful tool execution |
| `Notification` | Claude sends user notification |
| `Stop` | Before response completes |

### Permission Rule Syntax

```
ToolName              # All uses of tool
ToolName(pattern)     # Specific pattern
Bash(git *)           # Wildcard match
Read(./.env)          # File path
WebFetch(domain:x.com) # Domain filter
```

## When to Use This Skill

- Questions about Claude Code CLI flags or commands
- Configuring settings, permissions, or hooks
- Understanding sub-agents and the Task tool
- Setting up MCP servers
- Authoring custom skills or plugins
- Understanding the agentic loop and context management
- Troubleshooting permission or hook issues
