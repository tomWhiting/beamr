# Agent Skills Overview

Agent Skills are modular capabilities that extend Claude's functionality. Skills package instructions, metadata, and optional resources (scripts, templates) that Claude uses automatically when relevant.

## Why Use Skills

- **Specialize Claude**: Tailor capabilities for domain-specific tasks
- **Reduce repetition**: Create once, use automatically
- **Compose capabilities**: Combine Skills to build complex workflows

Skills load on-demand, eliminating the need to repeatedly provide the same guidance across conversations.

## Skill Structure

Every Skill requires a `SKILL.md` file with YAML frontmatter:

```yaml
---
name: your-skill-name
description: Brief description of what this Skill does and when to use it
---

# Your Skill Name

## Instructions
[Clear, step-by-step guidance for Claude to follow]

## Examples
[Concrete examples of using this Skill]
```

**Required fields:** `name` and `description`

**Field requirements:**

`name`:
- Maximum 64 characters
- Only lowercase letters, numbers, and hyphens
- No XML tags or reserved words ("anthropic", "claude")

`description`:
- Non-empty, maximum 1024 characters
- No XML tags
- Should include both what the Skill does and when to use it

## How Skills Work: Progressive Loading

Skills leverage Claude's filesystem access to load information in stages as needed.

### Level 1: Metadata (Always Loaded)

The YAML frontmatter provides discovery information:

```yaml
---
name: pdf-processing
description: Extract text and tables from PDF files, fill forms, merge documents. Use when working with PDF files.
---
```

~100 tokens per skill. Loaded at startup for all skills.

### Level 2: Instructions (Loaded When Triggered)

The main SKILL.md body contains procedural knowledge and workflows. Loaded when you request something matching the skill's description.

Target: Under 5k tokens.

### Level 3: Resources (Loaded As Needed)

Additional files: reference docs, scripts, templates. Loaded only when referenced. Scripts execute via bash without loading contents into context.

```
pdf-skill/
├── SKILL.md (main instructions)
├── FORMS.md (form-filling guide)
├── REFERENCE.md (detailed API reference)
└── scripts/
    └── fill_form.py (utility script)
```

**Effective unlimited** - no context penalty for bundled content until accessed.

## Where Skills Work

### Claude Code

Filesystem-based. Create skills as directories with `SKILL.md` files:
- Personal: `~/.claude/skills/<name>/SKILL.md`
- Project: `.claude/skills/<name>/SKILL.md`

### Claude API

Specify `skill_id` in the `container` parameter. Upload custom skills via `/v1/skills` endpoints.

Required beta headers:
- `code-execution-2025-08-25`
- `skills-2025-10-02`
- `files-api-2025-04-14`

### Claude.ai

Pre-built skills work automatically. Upload custom skills as zip files via Settings > Features (Pro, Max, Team, Enterprise).

### Claude Agent SDK

Create skills in `.claude/skills/`. Enable by including `"Skill"` in `allowed_tools`.

## Pre-built Agent Skills

| Skill | Capabilities |
|-------|--------------|
| **PowerPoint (pptx)** | Create presentations, edit slides, analyze content |
| **Excel (xlsx)** | Create spreadsheets, analyze data, generate charts |
| **Word (docx)** | Create documents, edit content, format text |
| **PDF (pdf)** | Generate formatted PDF documents and reports |

## Limitations

### Cross-Surface Availability

Skills do not sync across surfaces:
- Claude.ai skills must be separately uploaded to API
- Claude Code skills are filesystem-based and separate

### Sharing Scope

- **Claude.ai**: Individual user only
- **Claude API**: Workspace-wide
- **Claude Code**: Personal (`~/.claude/skills/`) or project-based (`.claude/skills/`)

### Runtime Constraints

**Claude.ai:**
- Varying network access depending on settings

**Claude API:**
- No network access
- No runtime package installation
- Only pre-installed packages

**Claude Code:**
- Full network access
- Global package installation discouraged

## Security

Use Skills only from trusted sources. Skills can direct Claude to invoke tools or execute code.

**Key considerations:**
- Audit all files: SKILL.md, scripts, images, other resources
- External sources that fetch data pose particular risk
- Skills with sensitive data access could leak information
- Treat like installing software
