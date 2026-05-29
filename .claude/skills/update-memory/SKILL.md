---
name: update-memory
description: Update project rules (.claude/rules/) or auto-memory files. Use when learning something new about the codebase, discovering a pattern, fixing a convention, or when asked to remember something. Triggered by terms like update rules, update memory, remember this, add to rules, update conventions.
argument-hint: "[rule|memory|claude] [topic]"
---

# Update Rules & Memory

Update the project's rules or memory based on what was learned or requested. Target is determined by `$ARGUMENTS` or inferred from context.

## Targets

| Target | Path | Use For |
|--------|------|---------|
| **rule** | `.claude/rules/**/*.md` | Path-specific conventions, patterns, gotchas |
| **memory** | `~/.claude/projects/-Users-tom-Developer-projects-deno-rust-meridian/memory/` | Cross-session learnings, architectural decisions |
| **claude** | `CLAUDE.md`, `apps/web/CLAUDE.md` | Project-wide standards (updated rarely) |

## Current Rules

!`for f in .claude/rules/**/*.md; do name=$(basename "$f" .md); scope=$(grep -A1 "^paths:" "$f" 2>/dev/null | tail -1 | sed 's/^ *- *"//;s/"$//'); echo "- $name = $scope"; done`

New rule files need YAML frontmatter with a `paths` field:
```yaml
---
paths:
  - "path/to/match/**/*.ext"
---
```

## Guidelines

1. **Read first** — understand what's there before editing
2. **No duplicates** — don't add what already exists
3. **Pragmatic** — useful patterns and gotchas, no marketing language
4. **Concise** — scannable bullet points, short descriptions
5. **Verify** — explore actual code before documenting
6. **Update in place** — fix outdated content, don't add conflicting info
7. **MEMORY.md stays under 200 lines** — move detail into topic files
