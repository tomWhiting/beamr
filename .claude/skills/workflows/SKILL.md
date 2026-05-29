---
name: workflows
description: Standalone YAML workflow system for automated execution cycles. Use when listing, inspecting, running, or authoring workflows. Triggered by terms like workflow, run workflow, CI loop, build check, automation, dispatch workflow, prebuilt steps, use keyword, workflow triggers, or scheduled workflow.
---

# Standalone Workflows

Standalone workflows are YAML-based automation definitions that run through the process engine without requiring an active shape. They are always available, a la carte, and composable via prebuilt step libraries.

**Your session ID:** `${CLAUDE_SESSION_ID}` — use with `--as` in CLI commands that require identity.

---

## Quick Start

```bash
shape workflow list
shape workflow inspect build-and-review
shape workflow run build-and-review --brief "Implement JWT auth endpoint" --initiator ${CLAUDE_SESSION_ID}
shape workflow status
```

Always pass `--initiator ${CLAUDE_SESSION_ID}` when running workflows. You will receive a DM from the system member "Meridian" when the workflow completes, which will wake you if you are idle.

---

## CLI Reference

| Command | Purpose |
|---------|---------|
| `shape workflow list` | List workflows from `.meridian/workflows/` and `~/.meridian/workflows/` |
| `shape workflow inspect <name>` | Show steps, inputs, routes, trigger, schedule |
| `shape workflow run <name> --brief "..."` | Queue a workflow for execution |
| `shape workflow run <name> --brief-file ./brief.md` | Run with brief from a file |
| `shape workflow run <name> --brief "..." --context "..."` | Run with additional context |
| `shape workflow run <name> --brief "..." --input key=value` | Named input values |
| `shape workflow run <name> --brief "..." --worksite <name>` | Run in a named worksite |
| `shape workflow run <name> --brief "..." --worksite auto` | Run in auto-named worksite |
| `shape workflow run <name> --brief "..." --worksite-base <branch>` | Base branch for worksite |
| `shape workflow run <name> --brief "..." --workspace <id>` | Workspace ID for scoping |
| `shape workflow run <name> --brief "..." --initiator ${CLAUDE_SESSION_ID}` | Receive a DM when workflow completes |
| `shape workflow status` | Show queue status (pending, active, history) |
| `shape workflow history --limit N --status <status>` | List recent executions from DB |
| `shape workflow output <id>` | Show output summary (accepts execution ID or queue ID) |
| `shape workflow output <id> --full` | Show full detail for all steps (raw stdout/stderr, expanded commands) |
| `shape workflow output <id> <step-name>` | Show full output for a specific step |
| `shape workflow output <id> <step-name> --full` | Show full output with raw stdout/stderr |
| `shape workflow output <id> --json` | Dump entire execution as JSON (pipe to file or jq) |
| `shape workflow peek <queue-id>` | Point-in-time snapshot of running execution |
| `shape workflow cancel <queue-id>` | Cancel a running or pending workflow |
| `shape workflow pause <queue-id>` | Pause execution at the next step boundary |
| `shape workflow resume <queue-id>` | Resume a paused workflow |

`list` and `inspect` work without server. All other commands require the Meridian server.

### Output Modes

| Mode | Command | Use Case |
|------|---------|----------|
| Summary | `shape workflow output <id>` | Quick assessment of all steps |
| Step detail | `shape workflow output <id> <step>` | Inspect a specific step |
| Full dump | `shape workflow output <id> --full` | All steps with raw stdout/stderr, expanded commands |
| JSON export | `shape workflow output <id> --json` | Dump entire execution as JSON for piping to file or jq |

```bash
# List recent executions
shape workflow history --limit 5

# Summary of all steps
shape workflow output 0fa8eea5

# Full detail for all steps (raw stdout/stderr, expanded commands)
shape workflow output 0fa8eea5 --full

# Drill into a specific step
shape workflow output 0fa8eea5 "Check Build"

# Dump to file for offline inspection
shape workflow output 0fa8eea5 --json > execution.json
```

Step detail includes: raw stdout, raw stderr, exit code, expanded command (all templates resolved), AI response text (action steps), parsed output JSON, duration, and routing outcome.

### Execution Control

Control running or pending workflows:

```bash
# Snapshot current state of a running workflow
shape workflow peek abc12345

# Cancel a running or pending workflow
shape workflow cancel abc12345

# Pause at the next step boundary (current step completes)
shape workflow pause abc12345

# Resume a paused workflow
shape workflow resume abc12345
```

---

## YAML Format

Workflow files: `.meridian/workflows/*.yaml` (project) or `~/.meridian/workflows/*.yaml` (user).

```yaml
name: build-and-review
description: Full build, check, fix, review cycle

input:
  brief:
    type: string
    required: true
    description: What to implement

output:
  review_result:
    from: "{Review.response}"

steps:
  - name: Implement
    kind: action
    target: function
    profile: developer
    prompt: |
      Implement: {input.brief}
    routes:
      success: Check Build

  - name: Check Build
    use: cargo-check
    routes:
      success: Run Tests
      failure: Fix Errors

  - name: Run Tests
    use: cargo-test
    routes:
      success: Review
      failure: Fix Errors

  - name: Fix Errors
    kind: action
    target: function
    profile: developer
    prompt: |
      Fix errors: {Check Build.diagnostics} {Run Tests.failures}
    routes:
      success: Check Build
    escalation:
      success[5]: Review

  - name: Review
    kind: action
    target: function
    model: opus
    profile: code-reviewer
    prompt: Review the implementation for {input.brief}
    routes:
      success: Done

  - name: Done
    kind: end
```

---

## Key Concepts

- **Routes use step names**, resolved to indices at parse time
- **`use:`** imports prebuilt steps (routes are per-workflow)
- **`{input.brief}`** references inputs; `{Step Name.field}` references prior step outputs (e.g., `{Check Build.error_count}`)
- **Escalation** (`success[5]: Review`) overrides routes after N visits
- **Input types**: string, text, list, number, boolean, select (with `options:` list). Each supports `required`, `description`, `default`.

## Built-in Steps

| Name | Command | Parser |
|------|---------|--------|
| `cargo-check` | `cargo check --workspace --message-format=json -q` | rust-diagnostics |
| `cargo-test` | `cargo test --workspace` | rust-test-results |
| `cargo-clippy` | `cargo clippy --workspace --message-format=json -q -- -D warnings` | rust-diagnostics |
| `cargo-nextest` | `cargo nextest run --workspace --message-format libtest-json-plus` | rust-test-results |
| `biome-check` | `biome check --reporter=json` | biome |
| `biome-fix` | `biome check --write --reporter=json` | biome |
| `tsc-check` | `tsc --noEmit --pretty false` | tsc (custom parser) |

Custom steps: `.meridian/workflow-steps/*.yaml`

Note: `rust-diagnostics` reports `success` based on `error_count == 0`, not exit code. Warnings-only builds (exit code 101 from `-D warnings`) report success if there are zero actual compilation errors.

## Step Types

| Kind | Description | Key Fields |
|------|-------------|------------|
| `execute` | Run shell command, parse output | `command`, `parser`, `pattern`, `env`, `extract` |
| `action` | Claude Runner function or member DM | `target` (function/member:Name), `profile`, `model`, `prompt`, `output_schema` |
| `evaluate` | Assess and route | `mode` (check/decide/match/authority), `prompt`/`criteria`/`match` |
| `dispatch` | Run a child workflow | `workflow`, `inputs`, `worksite`, `worksite_base`, `worksite_from` |
| `end` | Workflow terminates | (none) |

Task steps are not available in workflows (they require shape records).

### Common Fields (all step types)

- `for_each` — iterate over an array from a prior step (e.g., `for_each: "{Plan.tasks}"`)
- `parallel: true` — run for_each iterations concurrently
- `depends_on_field` / `id_field` — DAG dependency resolution for parallel for_each
- `select` — scoped data context (alias → step output reference)

### Execute Step Fields

- `command` — shell command to run (supports multi-line via YAML `|` blocks, template expansion with `{Step.field}`)
- `parser` — parser name (built-in or custom from `.meridian/parsers/`)
- `pattern` — custom regex pattern (used with `parser: regex`)
- `env` — environment variables as key-value map
- `extract` — list of fields to promote from parsed output to top-level (e.g., `- error_count`, `- diagnostics[].file`)

### Action Step Fields

- `target` — `function` (Claude Runner) or member name for DM
- `profile` — agent profile from `.meridian/profiles/`
- `model` — model override (e.g., `opus`, `sonnet`)
- `prompt` — prompt template with `{Step.field}` references
- `output_schema` — JSON Schema string for structured output. Claude returns data matching this schema. Schema fields are promoted to top-level in step output alongside `outcome`, `response`, `step_type`.
- `capabilities` — list of capabilities to load

Execute steps support an `env` field for environment variables:

```yaml
- name: Run Nextest
  kind: execute
  command: cargo nextest run --workspace --message-format libtest-json-plus
  parser: rust-test-results
  env:
    NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1"
```

## Triggers and Schedules

```yaml
trigger: "Task status changes to done"
schedule: "0 2 * * *"
default_inputs:
  brief: Run nightly verification
```

- **trigger**: Record status change, assignment, document completion, or Manual
- **schedule**: 5-field cron or aliases (`@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`)
- **default_inputs**: Values for `input.*` when no runtime brief is provided

## Template References

- `{input.brief}` — workflow input (any defined input, not just brief)
- `{Step Name.field}` — prior step output (e.g., `{Check Build.error_count}`)
- `{Step Name.field.nested}` — dot-path navigation into nested objects
- `{raw:Step Name.field}` — bypasses shell escaping (use for JSON data piped to jq)
- `{Step Name.field[*]}` — wildcard array expansion (each element separately)
- `{?Step Name.field}...{/}` — conditional block (rendered only if field exists and is truthy)

### Shell Escaping in Execute Steps

Template values in commands are auto-shell-escaped. Use `{raw:...}` to bypass:

```yaml
# Raw values for building flags
command: $(printf -- '-p %s ' {raw:Discover.crate_names})

# Raw JSON for piping to jq (use jq -s to slurp space-separated objects into array)
command: >-
  printf '%s' '{raw:Check Build.diagnostics}'
  | jq -s '[group_by(.file)[] | {file: .[0].file, items: .}]'
```

## Workflow Outputs

Declare named outputs that project step data into workflow-level results:

```yaml
output:
  build_diagnostics:
    from: "{Check Build.diagnostics}"
  test_summary:
    from: "{Run Tests}"
```

Outputs are resolved after workflow completion and persisted with the execution record. Without an `output:` section, no workflow-level outputs are captured.

## Dispatch (Workflow Chaining)

A `dispatch` step runs a child workflow, passing inputs and capturing outputs:

```yaml
- name: Run Verification
  kind: dispatch
  workflow: verify-build
  inputs:
    brief: "{input.brief}"
    diagnostics: "{Check Build.diagnostics}"
  worksite: auto
  routes:
    success: Done
    failure: Handle Failure
```

- **inputs**: Template strings expanded against parent's step outputs. Structured data (objects, arrays) is preserved — not stringified.
- **Child outputs**: The child's declared `output:` fields are spread into the dispatch step's output. Reference as `{Run Verification.field_name}`.
- **Recursive**: A dispatches B dispatches C is supported.

## Execution UI

Workflow executions are tracked in the **Workflows** sidebar tab:

- **Workflow browser**: Lists available workflows, click to run with input form
- **Queue panel**: Shows active + pending executions with cancel buttons
- **History**: Completed executions with status, step count, timing
- **Detail view**: Click any execution -> FloatingWindow with per-step output
- **Live streaming**: AI responses stream in real-time during execution
- **REST API**: `GET /api/executions`, `GET /api/executions/:id` (with steps)
- **Scoped WS**: `/ws/process/:queue_id` for live step events

## Parsers

### Built-in Parsers

| Parser | Use Case | Key Output Fields |
|--------|----------|-------------------|
| `exit-code` | Any command | `exit_code`, `success` |
| `json` | JSON-outputting tools | Parsed JSON object |
| `rust-diagnostics` | `cargo check --message-format=json` | `error_count`, `warning_count`, `diagnostics[]` |
| `rust-test-results` | `cargo test` | `passed`, `failed`, `ignored`, `failures[]` |
| `biome` | `biome check --reporter=json` | `error_count`, `warning_count`, `diagnostics[]` |
| `lines` | Line-based output | `lines[]`, `line_count` |
| `regex` | Custom pattern | Named capture groups become fields |

### Custom Parser Definitions

User-defined parsers in `.meridian/parsers/*.yaml` (project) or `~/.meridian/parsers/*.yaml` (user). Four formats:

- **json** — Single JSON object. Schema validation, return expressions for field extraction.
- **ndjson** — Line-delimited JSON (cargo, nextest). Discriminator field, per-type schemas with conditions, filter/exclude.
- **text** — Regex patterns with named captures. Multiple patterns per parser.
- **ai** — Claude Runner powered parsing with structured output schema.

All formats share a `return` field for data extraction:

```yaml
name: my-parser
format: text
patterns:
  error: '^ERROR: (?P<message>.+) at (?P<file>.+):(?P<line>\d+)$'
return:
  errors: error
  error_count: count(error)
```

Return expressions: `field.path`, `type_name`, `type_name where field == value`, `count(expr)`, `sum(expr.field)`, `first(expr)`.

See `resources/capabilities/shape-authoring.md` (Custom Parser Definitions section) for full format documentation.

## Worksite Integration

Run workflows in isolated worksites for concurrent agent work:

```yaml
name: isolated-build
worksite: my-feature
steps:
  - name: Build
    use: cargo-check
    routes:
      success: Done
  - name: Done
    kind: end
```

- `worksite:` auto-provisions a worksite before execution
- All steps run in the worksite directory
- Success auto-completes the worksite; failure leaves it for inspection

## Pre-Land Gates

Verify code before landing composed worksite changes. Available gates:
`cargo-check`, `cargo-test`, `cargo-clippy`, `biome-check`

Gates are selected in the Source Control > Worksites UI landing dialog, or via API.
