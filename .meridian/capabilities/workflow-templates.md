---
name: workflow-templates
description: Template engine capabilities for YAML workflows — variable references, iteration, conditionals, file inclusion, command execution, variable bindings, merge directives, and task list priming. Add when authoring or debugging workflow YAML files.
---

## Workflow Template Engine

The template engine in `crates/ygg-runner/src/runner/templates/` expands template constructs in workflow step prompts and commands before execution. Processing order: file inclusions → command executions → variable bindings → iteration blocks → conditionals → variable references.

### Variable References

Access step outputs and inputs via dot-path notation.

```yaml
prompt: |
  Brief title: {input.brief.title}
  Scout said: {Scout.summary}
  Nested: {Scout.enrichments[0].id}
```

- `{StepName.field}` — access a field from a completed step's output
- `{StepName.field.nested.path}` — nested dot-path traversal
- `{StepName.array[0]}` — array index access
- `{StepName.array[*].field}` — wildcard: collect all values, space-joined
- `{raw:StepName.field}` — raw JSON output (no shell escaping)
- `{input.field}` — access workflow inputs (parsed as JSON at seed time)

### Iteration Blocks

Loop over arrays with per-element formatting.

```yaml
prompt: |
  {#each input.brief.requirements as r}
  ### {r.id}: {r.title}
  {r.spec}
  Acceptance:
  {#each r.acceptance as a}
  - {a}
  {/each}
  {/each}
```

- `{#each path as binding}...{/each}` — iterate over a JSON array
- Nesting supported: inner `{#each}` blocks work inside outer ones
- The binding (`r`, `a`, etc.) is available inside the block for field access
- Non-array or unresolved paths produce empty output + `tracing::warn`

### Conditional Blocks

Gate content on truthiness or comparisons.

```yaml
prompt: |
  {?Scout.summary}Scout found: {Scout.summary}{/}
  {?r.patterns}Patterns: {r.patterns[*]}{/}
  {?@resume == false}First visit — full briefing{/}
  {?@visit > 1}Retry — focus on failures only{/}
  {?Step.error_count > 0}Errors found!{/}
```

- `{?path}...{/}` — truthy check (non-null, non-empty, non-zero)
- `{?path == value}...{/}` — equality comparison
- `{?path > N}...{/}` — numeric comparison (>=, <=, >, <, !=)
- `{?@resume}...{/}` — runtime: true on revisits (session already exists)
- `{?@visit > N}...{/}` — runtime: visit counter for progressive narrowing
- Nesting supported

### File Inclusion

Inline file contents at template expansion time.

```yaml
prompt: |
  ## Design Reference
  {{@docs/design/messaging/DESIGN.md}}

  ## Another file
  {{@.meridian/design-system/README.md}}
```

- `{{@path/to/file}}` — read file relative to working directory, insert contents
- Missing files produce empty string + `tracing::warn`
- Runs BEFORE all other template processing (included content can contain template constructs)

### Command Execution

Run shell commands and inline stdout.

```yaml
prompt: |
  ## Current Structure
  {{!`ls crates/ | head -10`}}

  ## Git Status
  {{!`git log --oneline -5`}}
```

- `` {{!`command`}} `` — run via `sh -c`, insert stdout
- Working directory is the workflow's working dir
- Failed commands still produce whatever stdout was emitted + `tracing::warn` with exit code
- Runs AFTER file inclusions, BEFORE other processing

### Variable Bindings

Execute a source, parse result as JSON, bind to a local variable for later use.

```yaml
prompt: |
  {{$diagnostics=!`cargo clippy --message-format=json 2>/dev/null | jq -s '[.[] | select(.reason == "compiler-message")]'`}}
  {{$config=@.meridian/config.json}}

  {#each diagnostics as d}
  - {d.message.message}
  {/each}

  Config value: {config.some_key}
```

- `{{$name=@path}}` — read file, parse as JSON (or store as string), bind to `name`
- `` {{$name=!`cmd`}} `` — run command, parse stdout as JSON (or string), bind to `name`
- Produces NO output in the rendered template (side-effect only)
- Bound variables are available to subsequent iteration/conditional/variable references
- Local bindings shadow step outputs (local scope wins)
- Invalid JSON results in the raw text stored as `Value::String`

### Merge Directive

After a step completes, merge an array from its output into an existing array in execution state.

```yaml
- name: Scout
  kind: action
  merge:
    from: enrichments              # field in this step's output
    into: input.brief.requirements # dot-path to target array in state
    key: id                        # match elements by this field
    nest: context                  # optional: put fields under a sub-key
```

- Enables progressive enrichment: each stage adds fields to a shared data structure
- `key` is configurable (not hardcoded to "id")
- Without `nest`: fields merge flat into matched target elements
- With `nest`: fields go under a sub-key (e.g., `target[n].context.field`)
- Unmatched source elements are skipped (debug log, no error)
- Missing target path: warn and skip (no crash)

### Task List Priming

Pre-load tasks for a Claude agent via the `task_list_id` field.

```yaml
- name: Dev
  kind: action
  task_list_id: "my-task-list-id"
  prompt: |
    Your task list has been populated. Use TaskList to see tasks.
```

- `task_list_id` on action steps sets `CLAUDE_CODE_ENABLE_TASKS=true` and `CLAUDE_CODE_TASK_LIST_ID=<value>`
- The spawned agent starts with tasks from `~/.claude/tasks/<id>/`
- Each task file is a JSON object: `{id, subject, description, activeForm, status, blocks, blockedBy}`
- Use an execute step before the action to write task files to the directory

### Key Files

- `crates/ygg-runner/src/runner/templates/mod.rs` — module declarations, `TemplateRuntimeContext`
- `crates/ygg-runner/src/runner/templates/expansion.rs` — main expansion pipeline, `{#each}` blocks
- `crates/ygg-runner/src/runner/templates/conditionals.rs` — `{?condition}...{/}` blocks
- `crates/ygg-runner/src/runner/templates/inclusions.rs` — `{{@file}}`, `` {{!`cmd`}} ``, `{{$var=...}}`
- `crates/ygg-runner/src/runner/templates/paths.rs` — `resolve_json_path`, wildcard resolution
- `crates/ygg-runner/src/runner/templates/escaping.rs` — shell escaping, JSON-to-string
- `crates/ygg-orchestrator/src/engine/merge.rs` — `merge:` directive implementation
- `crates/ygg-orchestrator/src/engine/action.rs` — `task_list_id` env var wiring
- `crates/meridian-services/src/workflow/executor.rs` — server-side input seeding (JSON parsing)

### Processing Order

1. `{{@path}}` — file inclusions (so included files can contain templates)
2. `` {{!`cmd`}} `` — command execution (inline results)
3. `{{$name=...}}` — variable binding extraction (parse + bind, emit nothing)
4. `{#each ... as x}...{/each}` — iteration blocks (recursive: each body gets full pipeline)
5. `{?condition}...{/}` — conditional blocks
6. `{Step.field}` / `{raw:Step.field}` — variable references

### Common Patterns

**Progressive enrichment workflow:**
1. Input: brief JSON with requirements array
2. Scout: enriches each requirement, `merge:` adds context back
3. Planner: reads enriched requirements, outputs ordered tasks
4. Execute step: writes tasks to `~/.claude/tasks/<id>/`
5. Dev: spawns with `task_list_id`, gets enriched requirements in prompt via `{#each}`

**Passing JSON between steps without breaking shell quoting:**
```yaml
command: |
  cat > /tmp/data.json <<'__END__'
  {raw:Step.output_field}
  __END__
```

Use a quoted heredoc (`<<'MARKER'`) — the template engine expands `{raw:...}` before the shell runs, and the heredoc safely contains any resulting content.
