# MCP (Model Context Protocol) Integration

MCP is an open standard for AI-tool integrations. MCP servers give Claude Code access to external tools, databases, and APIs.

## Example Use Cases

- Implement features from issue trackers (Jira, GitHub)
- Query databases (PostgreSQL, BigQuery)
- Analyze monitoring data (Sentry, Statsig)
- Integrate designs from Figma
- Automate workflows (Gmail, Slack)

## Installing MCP Servers

### HTTP Server (Recommended for Remote)

```bash
claude mcp add --transport http <name> <url>

# Example: Notion
claude mcp add --transport http notion https://mcp.notion.com/mcp

# With authentication
claude mcp add --transport http secure-api https://api.example.com/mcp \
  --header "Authorization: Bearer your-token"
```

### SSE Server (Deprecated)

```bash
claude mcp add --transport sse asana https://mcp.asana.com/sse
```

### Stdio Server (Local)

```bash
# Basic syntax
claude mcp add [options] <name> -- <command> [args...]

# Example: Airtable
claude mcp add --transport stdio --env AIRTABLE_API_KEY=YOUR_KEY airtable \
  -- npx -y airtable-mcp-server
```

**Note:** Options (`--transport`, `--env`, `--scope`, `--header`) must come before the server name. Use `--` to separate server name from command/args.

**Windows:** Use `cmd /c` wrapper: `claude mcp add --transport stdio myserver -- cmd /c npx -y @some/package`

## Managing Servers

```bash
claude mcp list                  # List all servers
claude mcp get github            # Get server details
claude mcp remove github         # Remove server
/mcp                             # Check status (within Claude Code)
```

## Configuration Scopes

| Scope | Location | Use Case |
|-------|----------|----------|
| **local** (default) | `~/.claude.json` | Personal servers for current project |
| **project** | `.mcp.json` in project root | Team-shared servers (commit to git) |
| **user** | `~/.claude.json` | Personal servers across all projects |

```bash
claude mcp add --transport http stripe --scope local https://mcp.stripe.com
claude mcp add --transport http paypal --scope project https://mcp.paypal.com/mcp
claude mcp add --transport http hubspot --scope user https://mcp.hubspot.com/anthropic
```

## Project-Level Configuration (.mcp.json)

```json
{
  "mcpServers": {
    "shared-server": {
      "command": "/path/to/server",
      "args": [],
      "env": {}
    }
  }
}
```

### Environment Variable Expansion

```json
{
  "mcpServers": {
    "api-server": {
      "type": "http",
      "url": "${API_BASE_URL:-https://api.example.com}/mcp",
      "headers": {
        "Authorization": "Bearer ${API_KEY}"
      }
    }
  }
}
```

Syntax:
- `${VAR}` - Variable value
- `${VAR:-default}` - Value or default

## Authentication

Many remote MCP servers require OAuth:

1. Add the server: `claude mcp add --transport http sentry https://mcp.sentry.dev/mcp`
2. Authenticate in Claude Code: `/mcp` → follow browser prompts

## Plugin-Provided MCP Servers

Plugins can bundle MCP servers that start automatically when the plugin is enabled.

In `.mcp.json` at plugin root:
```json
{
  "database-tools": {
    "command": "${CLAUDE_PLUGIN_ROOT}/servers/db-server",
    "args": ["--config", "${CLAUDE_PLUGIN_ROOT}/config.json"],
    "env": {
      "DB_URL": "${DB_URL}"
    }
  }
}
```

## MCP Tool Search

When MCP tool descriptions exceed 10% of context window, Claude Code automatically enables tool search to load tools on-demand.

| ENABLE_TOOL_SEARCH | Behavior |
|--------------------|----------|
| `auto` (default) | Activates at 10% threshold |
| `auto:<N>` | Custom threshold (e.g., `auto:5` for 5%) |
| `true` | Always enabled |
| `false` | Disabled, load all tools upfront |

```bash
ENABLE_TOOL_SEARCH=auto:5 claude
```

Disable MCPSearch tool:
```json
{
  "permissions": {
    "deny": ["MCPSearch"]
  }
}
```

## MCP Resources

Reference MCP resources using `@` mentions:

```
@github:issue://123
@postgres:schema://users
@docs:file://api/authentication
```

## MCP Prompts as Commands

MCP prompts become available as commands:

```
/mcp__github__list_prs
/mcp__github__pr_review 456
/mcp__jira__create_issue "Bug in login flow" high
```

## Output Limits

- Warning at 10,000 tokens
- Default max: 25,000 tokens
- Configure: `MAX_MCP_OUTPUT_TOKENS=50000`

## Using Claude Code as MCP Server

```bash
claude mcp serve
```

Add to Claude Desktop config:
```json
{
  "mcpServers": {
    "claude-code": {
      "type": "stdio",
      "command": "claude",
      "args": ["mcp", "serve"],
      "env": {}
    }
  }
}
```

## Managed MCP Configuration

### Option 1: Exclusive Control (managed-mcp.json)

Deploy to system directory:
- macOS: `/Library/Application Support/ClaudeCode/managed-mcp.json`
- Linux/WSL: `/etc/claude-code/managed-mcp.json`
- Windows: `C:\Program Files\ClaudeCode\managed-mcp.json`

Users cannot add/modify MCP servers when this file exists.

### Option 2: Allowlists/Denylists

```json
{
  "allowedMcpServers": [
    { "serverName": "github" },
    { "serverCommand": ["npx", "-y", "@modelcontextprotocol/server-filesystem"] },
    { "serverUrl": "https://mcp.company.com/*" }
  ],
  "deniedMcpServers": [
    { "serverName": "dangerous-server" },
    { "serverUrl": "https://*.untrusted.com/*" }
  ]
}
```

Each entry must have exactly one of: `serverName`, `serverCommand`, or `serverUrl`.

**Denylist takes absolute precedence** over allowlist.

## Add Server from JSON

```bash
claude mcp add-json weather-api '{"type":"http","url":"https://api.weather.com/mcp","headers":{"Authorization":"Bearer token"}}'
```

## Import from Claude Desktop

```bash
claude mcp add-from-claude-desktop   # Interactive selection
```

Works on macOS and WSL.
