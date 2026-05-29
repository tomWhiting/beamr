# Skills Reference

Skills extend Claude's capabilities with domain knowledge and reusable workflows. Create a `SKILL.md` file with instructions, and Claude uses it when relevant or when invoked with `/skill-name`.

## Skill Locations

| Location | Scope | Applies to |
|----------|-------|------------|
| Enterprise | `managed-settings.json` | All users in organization |
| Personal | `~/.claude/skills/<name>/SKILL.md` | All your projects |
| Project | `.claude/skills/<name>/SKILL.md` | This project only |
| Plugin | `<plugin>/skills/<name>/SKILL.md` | Where plugin is enabled |

**Priority (highest to lowest):** Enterprise → Personal → Project. Plugin skills use `plugin-name:skill-name` namespace.

## File Format

```yaml
---
name: my-skill
description: What this skill does and when to use it
argument-hint: "[filename] [format]"
disable-model-invocation: true
user-invocable: false
allowed-tools: Read, Grep, Glob
model: sonnet
context: fork
agent: Explore
hooks:
  PreToolUse:
    - matcher: "Bash"
      hooks:
        - type: command
          command: "./scripts/security-check.sh"
---

Your skill instructions here...
```

## Frontmatter Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | No | Display name (defaults to directory name). Lowercase letters, numbers, hyphens. Max 64 chars. |
| `description` | Recommended | What the skill does and when to use it. Claude uses this for auto-invocation. |
| `argument-hint` | No | Hint shown during autocomplete (e.g., `[issue-number]`) |
| `disable-model-invocation` | No | `true` prevents Claude from auto-invoking (default: `false`) |
| `user-invocable` | No | `false` hides from `/` menu (default: `true`) |
| `allowed-tools` | No | Tools Claude can use without permission when skill is active |
| `model` | No | Model to use when skill is active |
| `context` | No | `fork` to run in forked subagent context |
| `agent` | No | Subagent type when `context: fork` (`Explore`, `Plan`, `general-purpose`, or custom) |
| `hooks` | No | Hooks scoped to skill's lifecycle |

## String Substitutions

| Variable | Description |
|----------|-------------|
| `$ARGUMENTS` | All arguments passed to skill |
| `$ARGUMENTS[N]` or `$N` | Specific argument by index (0-based) |
| `${CLAUDE_SESSION_ID}` | Current session ID |

```yaml
---
name: fix-issue
description: Fix a GitHub issue
---

Fix GitHub issue $ARGUMENTS following our coding standards.
```

Run with: `/fix-issue 123`

## Invocation Control

| Setting | You can invoke | Claude can invoke | Context loading |
|---------|----------------|-------------------|-----------------|
| (default) | Yes | Yes | Description always, full skill when invoked |
| `disable-model-invocation: true` | Yes | No | Description not in context, loads when you invoke |
| `user-invocable: false` | No | Yes | Description always, loads when Claude invokes |

## Dynamic Context Injection

Use `` !`command` `` to run shell commands before sending to Claude:

```yaml
---
name: pr-summary
description: Summarize pull request changes
context: fork
agent: Explore
---

## Pull request context
- PR diff: !`gh pr diff`
- PR comments: !`gh pr view --comments`

## Your task
Summarize this pull request...
```

Commands execute immediately; output replaces the placeholder.

## Running in Subagent Context

Add `context: fork` to run in isolation. The skill content becomes the prompt for the subagent.

```yaml
---
name: deep-research
description: Research a topic thoroughly
context: fork
agent: Explore
---

Research $ARGUMENTS thoroughly:
1. Find relevant files using Glob and Grep
2. Read and analyze the code
3. Summarize findings with specific file references
```

**Note:** `context: fork` requires task instructions, not just guidelines.

## Skill Directory Structure

```
my-skill/
├── SKILL.md           # Main instructions (required)
├── template.md        # Template for Claude to fill in
├── examples/
│   └── sample.md      # Example output
└── scripts/
    └── validate.sh    # Script Claude can execute
```

Reference supporting files from SKILL.md:
```markdown
## Additional resources
- For complete API details, see [reference.md](reference.md)
- For usage examples, see [examples.md](examples.md)
```

Keep SKILL.md under 500 lines; move detailed reference to separate files.

## Permission Control

**Disable all skills:**
```json
{
  "permissions": {
    "deny": ["Skill"]
  }
}
```

**Allow/deny specific skills:**
```
Skill(commit)        # Exact match
Skill(review-pr *)   # Prefix match with any arguments
```

## Nested Discovery

When editing files in subdirectories, Claude Code also looks for skills in nested `.claude/skills/` directories. Useful for monorepos.

## Troubleshooting

| Issue | Solution |
|-------|----------|
| Skill not triggering | Check description includes keywords; verify with "What skills are available?" |
| Skill triggers too often | Make description more specific; add `disable-model-invocation: true` |
| Claude doesn't see all skills | Check `/context` for excluded skills; increase `SLASH_COMMAND_TOOL_CHAR_BUDGET` |
