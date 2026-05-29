# Settings Reference

Configure Claude Code with `/config` in the interactive REPL.

## Configuration Scopes

| Scope | Location | Who it affects | Shared with team? |
|-------|----------|----------------|-------------------|
| **Managed** | System-level `managed-settings.json` | All users on machine | Yes (deployed by IT) |
| **User** | `~/.claude/` directory | You, across all projects | No |
| **Project** | `.claude/` in repository | All collaborators | Yes (committed to git) |
| **Local** | `.claude/*.local.*` files | You, in this repository | No (gitignored) |

**When to use each scope:**
- **Managed**: Security policies, compliance requirements, standardized configs
- **User**: Personal preferences, cross-project tools and plugins
- **Project**: Team conventions, shared hooks, custom skills
- **Local**: Personal overrides for current project, credentials

## Settings Files

| File | Scope | Purpose |
|------|-------|---------|
| `~/.claude/settings.json` | User | Personal preferences |
| `.claude/settings.json` | Project | Team-shared settings |
| `.claude/settings.local.json` | Local | Personal project overrides |
| `managed-settings.json` | Managed | Enterprise policies |

**Merge order (lowest to highest priority):** Managed → User → Project → Local

## Available Settings

| Key | Description | Example |
|:----|:------------|:--------|
| `apiKeyHelper` | Custom script to generate auth value (sent as `X-Api-Key` and `Authorization: Bearer` headers) | `/bin/generate_temp_api_key.sh` |
| `cleanupPeriodDays` | Sessions inactive longer than this are deleted at startup. `0` = delete all. Default: 30 | `20` |
| `companyAnnouncements` | Announcements displayed at startup (cycled randomly if multiple) | `["Welcome to Acme Corp!"]` |
| `env` | Environment variables applied to every session | `{"FOO": "bar"}` |
| `attribution` | Customize attribution for git commits and PRs. See [Attribution settings](#attribution-settings) | `{"commit": "🤖 Generated with Claude Code", "pr": ""}` |
| `includeCoAuthoredBy` | **Deprecated**: Use `attribution`. Include co-authored-by in commits/PRs. Default: `true` | `false` |
| `permissions` | Permission rules. See [Permission settings](#permission-settings) | |
| `hooks` | Commands to run before/after tool executions. See [Hooks Reference](./hooks.md) | `{"PreToolUse": {...}}` |
| `disableAllHooks` | Disable all hooks | `true` |
| `allowManagedHooksOnly` | (Managed only) Prevent user/project/plugin hooks, allow only managed hooks | `true` |
| `model` | Override default model | `"claude-sonnet-4-5-20250929"` |
| `otelHeadersHelper` | Script to generate dynamic OpenTelemetry headers | `/bin/generate_otel_headers.sh` |
| `statusLine` | Custom status line configuration | `{"type": "command", "command": "~/.claude/statusline.sh"}` |
| `fileSuggestion` | Custom script for `@` file autocomplete | `{"type": "command", "command": "~/.claude/file-suggestion.sh"}` |
| `respectGitignore` | Whether `@` file picker respects `.gitignore`. Default: `true` | `false` |
| `outputStyle` | Output style to adjust system prompt | `"Explanatory"` |
| `forceLoginMethod` | Restrict login: `claudeai` (Claude.ai) or `console` (API billing) | `claudeai` |
| `forceLoginOrgUUID` | Auto-select organization during login (requires `forceLoginMethod`) | `"xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"` |
| `enableAllProjectMcpServers` | Auto-approve all MCP servers in project `.mcp.json` | `true` |
| `enabledMcpjsonServers` | Specific MCP servers from `.mcp.json` to approve | `["memory", "github"]` |
| `disabledMcpjsonServers` | Specific MCP servers from `.mcp.json` to reject | `["filesystem"]` |
| `allowedMcpServers` | (Managed) Allowlist of MCP servers. Undefined = no restrictions, `[]` = lockdown | `[{ "serverName": "github" }]` |
| `deniedMcpServers` | (Managed) Denylist of MCP servers. Takes precedence over allowlist | `[{ "serverName": "filesystem" }]` |
| `strictKnownMarketplaces` | (Managed) Allowlist of plugin marketplaces. Undefined = no restrictions, `[]` = lockdown | `[{ "source": "github", "repo": "acme-corp/plugins" }]` |
| `awsAuthRefresh` | Script to modify `.aws` directory for credentials | `aws sso login --profile myprofile` |
| `awsCredentialExport` | Script outputting JSON with AWS credentials | `/bin/generate_aws_grant.sh` |
| `alwaysThinkingEnabled` | Enable extended thinking by default | `true` |
| `plansDirectory` | Where plan files are stored (relative to project root). Default: `~/.claude/plans` | `"./plans"` |
| `showTurnDuration` | Show turn duration messages. Default: `true` | `false` |
| `spinnerVerbs` | Customize spinner action verbs. `mode`: `"replace"` or `"append"` | `{"mode": "append", "verbs": ["Pondering"]}` |
| `language` | Claude's preferred response language | `"japanese"` |
| `autoUpdatesChannel` | Release channel: `"stable"` (week-old, skips regressions) or `"latest"` (default) | `"stable"` |
| `spinnerTipsEnabled` | Show tips in spinner. Default: `true` | `false` |
| `terminalProgressBarEnabled` | Terminal progress bar (Windows Terminal, iTerm2). Default: `true` | `false` |

## Permission Settings

| Key | Description | Example |
|:----|:------------|:--------|
| `allow` | Permission rules to allow tool use | `["Bash(git diff *)"]` |
| `ask` | Permission rules requiring confirmation | `["Bash(git push *)"]` |
| `deny` | Permission rules to deny tool use | `["WebFetch", "Bash(curl *)", "Read(./.env)"]` |
| `additionalDirectories` | Additional working directories Claude can access | `["../docs/"]` |
| `defaultMode` | Default permission mode | `"acceptEdits"` |
| `disableBypassPermissionsMode` | Set to `"disable"` to prevent `bypassPermissions` mode | `"disable"` |

### Permission Rule Syntax

**Rule evaluation order:** Deny → Ask → Allow (first match wins, deny takes precedence)

**Matching all uses:**
| Rule | Effect |
|:-----|:-------|
| `Bash` | All Bash commands |
| `WebFetch` | All web fetch requests |
| `Read` | All file reads |

`Bash(*)` is equivalent to `Bash`.

**Specifiers for fine-grained control:**
| Rule | Effect |
|:-----|:-------|
| `Bash(npm run build)` | Exact command match |
| `Read(./.env)` | Specific file |
| `WebFetch(domain:example.com)` | Specific domain |

**Wildcard patterns:** `*` supported at any position.
```json
{
  "permissions": {
    "allow": ["Bash(npm run *)", "Bash(git commit *)", "Bash(* --version)"],
    "deny": ["Bash(git push *)"]
  }
}
```

**Note:** Space before `*` matters: `Bash(ls *)` matches `ls -la` but not `lsof`. Legacy `:*` suffix is deprecated.

**Warning:** Bash patterns constraining arguments are fragile. `Bash(curl http://github.com/ *)` won't match flags before URL, different protocols, or shell variables. Don't rely on argument patterns as security boundaries.

## Sandbox Settings

| Key | Description | Example |
|:----|:------------|:--------|
| `enabled` | Enable bash sandboxing (macOS, Linux, WSL2). Default: `false` | `true` |
| `autoAllowBashIfSandboxed` | Auto-approve bash when sandboxed. Default: `true` | `true` |
| `excludedCommands` | Commands that run outside sandbox | `["git", "docker"]` |
| `allowUnsandboxedCommands` | Allow `dangerouslyDisableSandbox` escape hatch. Default: `true` | `false` |
| `network.allowUnixSockets` | Unix socket paths accessible in sandbox | `["~/.ssh/agent-socket"]` |
| `network.allowLocalBinding` | Allow binding to localhost (macOS only). Default: `false` | `true` |
| `network.httpProxyPort` | HTTP proxy port (if bringing own proxy) | `8080` |
| `network.socksProxyPort` | SOCKS5 proxy port (if bringing own proxy) | `8081` |
| `enableWeakerNestedSandbox` | Weaker sandbox for unprivileged Docker (Linux/WSL2). **Reduces security.** Default: `false` | `true` |

**Example configuration:**
```json
{
  "sandbox": {
    "enabled": true,
    "autoAllowBashIfSandboxed": true,
    "excludedCommands": ["docker"],
    "network": {
      "allowUnixSockets": ["/var/run/docker.sock"],
      "allowLocalBinding": true
    }
  }
}
```

## Attribution Settings

| Key | Description |
|:----|:------------|
| `commit` | Attribution for git commits (including trailers). Empty string hides it |
| `pr` | Attribution for PR descriptions. Empty string hides it |

**Defaults:**
- Commit: `🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>`
- PR: `🤖 Generated with [Claude Code](https://claude.com/claude-code)`

```json
{
  "attribution": {
    "commit": "Generated with AI\n\nCo-Authored-By: AI <ai@example.com>",
    "pr": ""
  }
}
```

## Environment Variables

| Variable | Description |
|:---------|:------------|
| `ANTHROPIC_API_KEY` | API key for Claude |
| `ANTHROPIC_BASE_URL` | Custom API base URL |
| `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS` | Set to `1` to disable background tasks |
| `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` | Context percentage before auto-compact (default: 95) |
| `CLAUDE_CODE_USE_BEDROCK` | Set to `1` to use Amazon Bedrock |
| `CLAUDE_CODE_USE_VERTEX` | Set to `1` to use Google Vertex AI |
| `ENABLE_TOOL_SEARCH` | MCP tool search mode: `auto`, `true`, `false` |
| `MAX_MCP_OUTPUT_TOKENS` | Max tokens for MCP tool output (default: 25000) |
| `MCP_TIMEOUT` | MCP server startup timeout in ms |
| `SLASH_COMMAND_TOOL_CHAR_BUDGET` | Character budget for skill descriptions |
| `DISABLE_PROMPT_CACHING` | Set to `1` to disable prompt caching |
| `HTTP_PROXY` / `HTTPS_PROXY` | Proxy configuration |
| `NO_PROXY` | Hosts to bypass proxy |

## CLAUDE.md Files

Project-specific instructions loaded every session.

| Location | Applies to |
|:---------|:-----------|
| `~/.claude/CLAUDE.md` | All sessions |
| `./CLAUDE.md` | Current project (commit to git) |
| `./CLAUDE.local.md` | Current project (gitignored) |
| Parent directories | Inherited in monorepos |
| Child directories | Loaded on demand |

**Import syntax:**
```markdown
See @README.md for project overview.
See @docs/git-instructions.md for workflow.
See @~/.claude/my-instructions.md for personal overrides.
```

**Include:** Bash commands Claude can't guess, non-standard code style rules, testing instructions, repo etiquette, architectural decisions, environment quirks.

**Exclude:** Anything inferable from code, standard conventions, detailed API docs (link instead), long explanations.

## Tools Available to Claude

| Category | Tools |
|:---------|:------|
| File Operations | `Read`, `Edit`, `Write`, `MultiEdit`, `Glob`, `Grep`, `LS` |
| Execution | `Bash`, `Task`, `Skill` |
| Web | `WebFetch`, `WebSearch` |
| Notebook | `NotebookRead`, `NotebookEdit` |
| Orchestration | `AskUserQuestion`, `EnterPlanMode`, `ExitPlanMode`, `TodoRead`, `TodoWrite` |
| MCP | Pattern: `mcp__<server>__<tool>` |

## Managed Settings (Enterprise)

System-level settings deployed by IT.

**File locations:**
- macOS: `/Library/Application Support/ClaudeCode/managed-settings.json`
- Linux/WSL: `/etc/claude-code/managed-settings.json`
- Windows: `C:\Program Files\ClaudeCode\managed-settings.json`

**Key managed settings:**
```json
{
  "allowedMcpServers": [...],
  "deniedMcpServers": [...],
  "allowManagedHooksOnly": true,
  "permissions": {
    "deny": ["Task(dangerous-agent)"]
  }
}
```

`allowManagedHooksOnly`: When `true`, blocks user/project/plugin hooks. Only managed hooks run.
