# Plugins Reference

Plugins bundle skills, hooks, subagents, and MCP servers into installable packages.

## Plugin Components

| Component | Location | Purpose |
|-----------|----------|---------|
| **Skills** | `skills/` or `commands/` | `/name` shortcuts and domain knowledge |
| **Agents** | `agents/` | Specialized subagents |
| **Hooks** | `hooks/hooks.json` | Event handlers |
| **MCP Servers** | `.mcp.json` | External tool integrations |
| **LSP Servers** | `.lsp.json` | Language server configurations |

## Plugin Manifest (plugin.json)

Required location: `.claude-plugin/plugin.json`

```json
{
  "name": "plugin-name",
  "version": "1.2.0",
  "description": "Brief plugin description",
  "author": {
    "name": "Author Name",
    "email": "author@example.com",
    "url": "https://github.com/author"
  },
  "homepage": "https://docs.example.com/plugin",
  "repository": "https://github.com/author/plugin",
  "license": "MIT",
  "keywords": ["keyword1", "keyword2"],
  "commands": ["./custom/commands/special.md"],
  "agents": "./custom/agents/",
  "skills": "./custom/skills/",
  "hooks": "./config/hooks.json",
  "mcpServers": "./mcp-config.json",
  "outputStyles": "./styles/",
  "lspServers": "./.lsp.json"
}
```

**Required fields:** `name` (kebab-case, no spaces)

## Installation Scopes

| Scope | Settings File | Use Case |
|-------|---------------|----------|
| `user` (default) | `~/.claude/settings.json` | Personal plugins across all projects |
| `project` | `.claude/settings.json` | Team plugins via version control |
| `local` | `.claude/settings.local.json` | Project-specific, gitignored |
| `managed` | `managed-settings.json` | Read-only, update only |

## CLI Commands

```bash
# Install
claude plugin install <plugin>
claude plugin install <plugin>@<marketplace> --scope project

# Manage
claude plugin uninstall <plugin> --scope user
claude plugin enable <plugin>
claude plugin disable <plugin>
claude plugin update <plugin>
```

Aliases: `uninstall` = `remove`, `rm`

## Directory Structure

```
enterprise-plugin/
├── .claude-plugin/
│   └── plugin.json          # Required manifest
├── commands/                 # Legacy skill location
│   ├── status.md
│   └── logs.md
├── agents/                   # Subagent definitions
│   ├── security-reviewer.md
│   └── compliance-checker.md
├── skills/                   # Skill directories
│   ├── code-reviewer/
│   │   └── SKILL.md
│   └── pdf-processor/
│       ├── SKILL.md
│       └── scripts/
├── hooks/
│   └── hooks.json           # Hook configurations
├── .mcp.json                # MCP server definitions
├── .lsp.json                # LSP server configs
├── scripts/                 # Hook and utility scripts
│   └── format-code.py
└── LICENSE
```

**Important:** Only `plugin.json` goes in `.claude-plugin/`. All other directories are at plugin root.

## Component Specifications

### Skills

Skills are directories with `SKILL.md`:

```
skills/
├── pdf-processor/
│   ├── SKILL.md
│   ├── reference.md (optional)
│   └── scripts/ (optional)
└── code-reviewer/
    └── SKILL.md
```

### Agents

Markdown files with frontmatter:

```markdown
---
description: What this agent specializes in
capabilities: ["task1", "task2"]
---

# Agent Name

Detailed description of the agent's role...
```

### Hooks

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Write|Edit",
        "hooks": [
          {
            "type": "command",
            "command": "${CLAUDE_PLUGIN_ROOT}/scripts/format-code.sh"
          }
        ]
      }
    ]
  }
}
```

Hook events: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`, `UserPromptSubmit`, `Notification`, `Stop`, `SubagentStart`, `SubagentStop`, `Setup`, `SessionStart`, `SessionEnd`, `PreCompact`

### MCP Servers

```json
{
  "mcpServers": {
    "plugin-database": {
      "command": "${CLAUDE_PLUGIN_ROOT}/servers/db-server",
      "args": ["--config", "${CLAUDE_PLUGIN_ROOT}/config.json"],
      "env": {
        "DB_PATH": "${CLAUDE_PLUGIN_ROOT}/data"
      }
    }
  }
}
```

### LSP Servers

```json
{
  "go": {
    "command": "gopls",
    "args": ["serve"],
    "extensionToLanguage": {
      ".go": "go"
    }
  }
}
```

**Required fields:** `command`, `extensionToLanguage`

**Optional fields:** `args`, `transport` (`stdio`/`socket`), `env`, `initializationOptions`, `settings`, `workspaceFolder`, `startupTimeout`, `shutdownTimeout`, `restartOnCrash`, `maxRestarts`

**Note:** Language server binary must be installed separately.

## Environment Variables

- `${CLAUDE_PLUGIN_ROOT}` - Absolute path to plugin directory
- `${CLAUDE_PROJECT_DIR}` - Project root directory

## Plugin Caching

Plugins are copied to a cache directory. External files referenced with path traversal (`../`) won't work. Options:

1. **Symlinks**: Create links within plugin directory (followed during copy)
2. **Restructure**: Set plugin path to parent directory containing all required files

## Debugging

```bash
claude --debug    # See plugin loading details
```

### Common Issues

| Issue | Cause | Solution |
|-------|-------|----------|
| Plugin not loading | Invalid `plugin.json` | Validate JSON syntax |
| Commands not appearing | Wrong directory structure | Ensure `commands/` at root, not in `.claude-plugin/` |
| Hooks not firing | Script not executable | `chmod +x script.sh` |
| MCP server fails | Missing `${CLAUDE_PLUGIN_ROOT}` | Use variable for all plugin paths |
| Path errors | Absolute paths used | All paths must be relative, start with `./` |
| LSP `Executable not found` | Binary not installed | Install language server binary |

## Versioning

Follow semantic versioning: `MAJOR.MINOR.PATCH`

- **MAJOR**: Breaking changes
- **MINOR**: New features (backward-compatible)
- **PATCH**: Bug fixes (backward-compatible)
