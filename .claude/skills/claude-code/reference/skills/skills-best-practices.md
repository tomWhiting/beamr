# Skill Authoring Best Practices

Practical guidance for writing effective Skills that Claude can discover and use.

## Core Principles

### Be Concise

Context is shared between system prompt, conversation history, other Skills, and your request. Only add context Claude doesn't already have.

**Guideline questions:**
- Does Claude need this explanation?
- Can Claude already know this?
- Does this token cost justify itself?

**Good** (~50 tokens):
```markdown
## Extract PDF text
Use pdfplumber:
import pdfplumber
with pdfplumber.open("file.pdf") as pdf:
    text = pdf.pages[0].extract_text()
```

**Bad** (~150 tokens): Explaining what PDFs are and how libraries work.

### Set Appropriate Freedom Levels

| Freedom | Use When | Example |
|---------|----------|---------|
| **High** (text instructions) | Multiple valid approaches, context-dependent | Code review: analyze structure, check bugs, suggest improvements |
| **Medium** (pseudocode/params) | Preferred pattern exists, some variation OK | Template with customizable parameters |
| **Low** (exact scripts) | Fragile operations, consistency critical | `python scripts/migrate.py --verify --backup` - do not modify |

### Test Across Models

| Model | Testing Focus |
|-------|---------------|
| Haiku | Does Skill provide enough guidance? |
| Sonnet | Is Skill clear and efficient? |
| Opus | Does Skill avoid over-explaining? |

## Skill Structure

### YAML Frontmatter Requirements

| Field | Constraints |
|-------|-------------|
| `name` | Max 64 chars, lowercase letters/numbers/hyphens only, no XML tags, no "anthropic"/"claude" |
| `description` | Non-empty, max 1024 chars, no XML tags, describe what + when to use |

### Naming Conventions

**Recommended (gerund form):** `processing-pdfs`, `analyzing-spreadsheets`, `testing-code`

**Acceptable:** `pdf-processing`, `process-pdfs`

**Avoid:** `helper`, `utils`, `tools`, `documents`, `anthropic-*`, `claude-*`

### Writing Effective Descriptions

**Requirements:**
- Write in third person ("Processes Excel files", not "I can help you")
- Include both what it does AND when to use it
- Be specific with key terms

**Examples:**
```yaml
# Good
description: Extract text and tables from PDF files, fill forms, merge documents. Use when working with PDF files or when the user mentions PDFs, forms, or document extraction.

# Bad
description: Helps with documents
```

### Progressive Disclosure Structure

```
pdf-skill/
├── SKILL.md              # Main instructions (loaded when triggered)
├── FORMS.md              # Form-filling guide (loaded as needed)
├── reference.md          # API reference (loaded as needed)
└── scripts/
    └── fill_form.py      # Utility script (executed, not loaded)
```

**Guidelines:**
- Keep SKILL.md under 500 lines
- Keep references one level deep from SKILL.md
- Include table of contents for files >100 lines
- Scripts execute without loading contents into context

## Workflows and Feedback Loops

### Workflow Pattern

Break complex tasks into clear steps with trackable checklists:

```markdown
## PDF form filling workflow

Task Progress:
- [ ] Step 1: Analyze form (run analyze_form.py)
- [ ] Step 2: Create field mapping (edit fields.json)
- [ ] Step 3: Validate mapping (run validate_fields.py)
- [ ] Step 4: Fill form (run fill_form.py)
- [ ] Step 5: Verify output (run verify_output.py)
```

### Feedback Loop Pattern

**Common pattern:** Run validator → fix errors → repeat

```markdown
1. Make edits to document
2. Validate immediately: `python scripts/validate.py`
3. If validation fails: fix issues, run validation again
4. Only proceed when validation passes
5. Rebuild output
```

## Content Guidelines

### Avoid Time-Sensitive Information

**Bad:**
```markdown
If before August 2025, use old API. After August 2025, use new API.
```

**Good:** Use "Current method" and "Old patterns" sections with deprecated content in collapsible details.

### Use Consistent Terminology

Choose one term and use it throughout:
- Always "API endpoint" (not mixing with "URL", "route", "path")
- Always "field" (not "box", "element", "control")
- Always "extract" (not "pull", "get", "retrieve")

## Common Patterns

### Template Pattern

```markdown
## Report structure
ALWAYS use this exact template:
# [Analysis Title]
## Executive summary
## Key findings
## Recommendations
```

### Examples Pattern

Provide input/output pairs for output quality:

```markdown
**Input:** Added user authentication with JWT tokens
**Output:**
feat(auth): implement JWT-based authentication
Add login endpoint and token validation middleware
```

### Conditional Workflow Pattern

```markdown
**Creating new content?** → Follow "Creation workflow"
**Editing existing content?** → Follow "Editing workflow"
```

## Evaluation and Iteration

### Evaluation-Driven Development

1. Identify gaps: Run Claude without Skill, document failures
2. Create evaluations: Build 3+ scenarios testing gaps
3. Establish baseline: Measure performance without Skill
4. Write minimal instructions: Address gaps only
5. Iterate: Execute evaluations, refine

**Evaluation structure:**
```json
{
  "skills": ["pdf-processing"],
  "query": "Extract all text from this PDF",
  "files": ["test-files/document.pdf"],
  "expected_behavior": ["Reads PDF", "Extracts all pages", "Saves to output.txt"]
}
```

### Iterative Development with Claude

1. Complete task without Skill, noting context you provide
2. Ask Claude to create Skill capturing the pattern
3. Review for conciseness, remove unnecessary explanations
4. Test with fresh Claude instance
5. Iterate based on observed behavior

## Anti-Patterns

| Anti-Pattern | Problem | Solution |
|--------------|---------|----------|
| Windows paths | Errors on Unix systems | Use forward slashes: `reference/guide.md` |
| Too many options | Decision paralysis | Provide default with escape hatch |
| Nested references | Incomplete reads | Keep references one level deep |
| Magic numbers | Unexplained configuration | Document all constants |
| Punting errors | Unclear failure handling | Handle errors explicitly in scripts |

## Advanced: Executable Code

### Script Guidelines

**Handle errors explicitly:**
```python
def process_file(path):
    try:
        with open(path) as f:
            return f.read()
    except FileNotFoundError:
        print(f"File {path} not found, creating default")
        with open(path, 'w') as f:
            f.write('')
        return ''
```

**Document constants:**
```python
REQUEST_TIMEOUT = 30  # HTTP requests typically complete within 30s
MAX_RETRIES = 3       # Most intermittent failures resolve by second retry
```

### Utility Script Benefits

- More reliable than generated code
- Save tokens (no code in context)
- Ensure consistency across uses
- Execute without loading contents

**Make execution intent clear:**
- "Run `analyze_form.py`" (execute)
- "See `analyze_form.py` for algorithm" (read as reference)

### Verifiable Intermediate Outputs

For complex tasks: analyze → **create plan file** → **validate plan** → execute → verify

**Benefits:** Catches errors early, machine-verifiable, reversible planning, clear debugging.

### Package Dependencies

| Platform | Capability |
|----------|------------|
| claude.ai | Can install from npm, PyPI, GitHub |
| Anthropic API | No network access, no runtime installation |

### MCP Tool References

Always use fully qualified names: `ServerName:tool_name`

```markdown
Use BigQuery:bigquery_schema to retrieve table schemas.
Use GitHub:create_issue to create issues.
```

## Checklist

### Core Quality
- [ ] Description includes what + when to use
- [ ] SKILL.md under 500 lines
- [ ] Additional details in separate files
- [ ] No time-sensitive information
- [ ] Consistent terminology
- [ ] File references one level deep
- [ ] Workflows have clear steps

### Code and Scripts
- [ ] Scripts handle errors explicitly
- [ ] All constants documented
- [ ] Required packages listed and verified
- [ ] Forward slashes in all paths
- [ ] Validation steps for critical operations

### Testing
- [ ] 3+ evaluations created
- [ ] Tested with Haiku, Sonnet, Opus
- [ ] Tested with real scenarios
