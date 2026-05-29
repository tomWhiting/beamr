# Beamr — Gleam Workflow Engine on a Rust Process Runtime

## Why

Workflow definitions should be programs, not configuration. Programs have types, tests, composition, and refactoring tools. Configuration has string interpolation and prayer.

YAML workflows are write-once, debug-never. You can't type-check them, compose them, or test a step in isolation. Rhai is a step up (it's a real language) but it's dynamically typed with no tooling — no LSP, no formatter, no test framework. A runtime error 45 minutes into a run that says a string isn't a map is not an acceptable developer experience.

Gleam on a Rust process runtime gives us everything we need: compile-time type safety, pattern matching with exhaustiveness checking, the pipeline operator for readable data flow, lightweight concurrent processes with supervision, and native integration with our Rust crates.

## What Gleam Workflows Look Like

A workflow is a typed function. Each step's output type is the next step's input type. If they don't match, it's a compile error — not a runtime surprise.

```gleam
pub fn onatopp_dev(brief: Brief, design: Design) -> Result(WorkflowResult, WorkflowError) {
  brief
  |> scout(design)
  |> result.try(fn(context) {
    context
    |> dev(brief, _)
    |> result.try(fn(dev_output) {
      dev_output
      |> review(brief, context, _)
      |> result.map(fn(review_output) {
        WorkflowResult(
          scout: context,
          dev: dev_output,
          review: review_output,
        )
      })
    })
  })
}
```

That's the entire pipeline. Type-checked. Composable. Testable.

## Step Definitions

```gleam
pub type StepConfig {
  StepConfig(
    provider: Provider,
    profile: String,
    schema: Schema,
    timeout: Duration,
    retries: RetryPolicy,
  )
}

pub type StepResult(output) {
  Completed(output: output, duration: Duration, tokens: TokenUsage)
  Failed(error: StepError, attempts: Int)
  Cancelled(reason: String)
}
```

Every step has a typed output. The workflow author defines the output type and the runtime enforces it. No more parsing JSON blobs and hoping `.id` exists.

## Core Features

### Compile-Time Route Safety

Route conditions are pattern matches, not string expressions evaluated at runtime:

```gleam
case dev_result {
  Completed(output, ..) if output.tests_pass -> review(output)
  Completed(output, ..) -> dev_retry(output, attempt: 2)
  Failed(error, attempts) if attempts < max_retries ->
    dev_retry(error.last_output, attempt: attempts + 1)
  Failed(error, _) ->
    Error(WorkflowError(step: "dev", cause: error))
}
```

The compiler tells you if you forgot a case. No silent fallthrough. No default-to-crash.

### Step Composition

Workflows are functions. Functions compose. Shared pipelines are extracted once and called from any workflow:

```gleam
pub fn run_checks(worktree: Path) -> Result(CheckResult, CheckError) {
  worktree
  |> cargo_check
  |> result.try(cargo_clippy(worktree, _))
  |> result.try(cargo_test(worktree, _))
  |> result.try(cargo_fmt_check(worktree, _))
}

pub fn dev_with_checks(brief, context) {
  brief
  |> dev(context, _)
  |> result.try(fn(output) {
    run_checks(output.worktree)
    |> result.map(fn(checks) { DevVerified(output, checks) })
  })
}
```

No copy-pasting YAML blocks. No fragile template inheritance. Just functions calling functions.

### Typed Enrichments

Scout context, dev output, review findings — all typed. The dev step knows exactly what the scout produced because the compiler enforces the shape:

```gleam
pub type ScoutContext {
  ScoutContext(
    existing_files: List(FilePath),
    module_structure: ModuleTree,
    dependency_graph: DepGraph,
    design_decisions: List(Decision),
    relevant_tests: List(TestPath),
  )
}
```

No "Unknown property id" at runtime. Ever.

### Pipeline Operator

Gleam's killer feature for workflow readability:

```gleam
brief
|> resolve_checklist(checklist)
|> enrich_with_design(design)
|> scout(provider: gpt5_5)
|> validate_scout_output
|> dev(provider: gpt5_5)
|> commit_changes
|> run_checks
|> review(provider: gpt5_5)
|> apply_review_fixes
|> run_checks
|> notify(recipient)
```

You can hand this to someone who's never seen Gleam and they can follow the flow. It reads like a spec because it is one — a spec that also executes.

### Result-Based Error Handling

Every step returns `Result(output, error)`. You decide at each point: retry, skip, escalate, or abort. No try-catch-and-hope. No swallowed errors. The type system forces you to handle every failure path.

### Hot Code Reloading

Update a workflow definition and running workflows continue on the old version while new dispatches pick up the new version. No restart, no lost state. No coordination overhead.

## The Process Runtime

### Lightweight Processes

Each workflow step runs in its own lightweight process. Thousands of concurrent workflows, each isolated. A step that blows up doesn't take down the scheduler, doesn't corrupt another workflow's state, doesn't leak resources.

### Message Passing

Steps communicate via typed messages. The scout sends its context to the dev step. The dev step sends its output to review. No shared mutable state. No locks. No races.

### Supervision Trees

If a step crashes, the supervisor decides: restart it, retry with different parameters, or escalate to the workflow-level supervisor. Supervision strategy is declarative — one-for-one (restart the failed child), one-for-all (restart all children), rest-for-one (restart the failed child and everything started after it).

This replaces ad-hoc retry logic with a structural guarantee: every process has a supervisor, every failure has a handler.

### Process Linking

When a workflow is cancelled, all its child step processes receive the kill signal through process links. No orphan processes. No zombie accumulation. Cancellation propagates structurally through the process tree.

### Fair Scheduling

The scheduler distributes CPU time fairly across concurrent workflows. One slow workflow (waiting on an LLM API call) doesn't starve the others. Preemptive scheduling with reduction counting ensures responsiveness across all running processes.

## Why Rust Underneath

The BEAM's weakness is single-threaded CPU-bound compute. An LLM API call returns a 50KB JSON blob that needs parsing, validation, and schema extraction — that's real CPU work. Rust gives us:

- **Actual parallelism** (not just concurrency) for CPU-bound work across cores
- **Zero-cost abstractions** for the message passing and scheduling layers
- **Memory safety** without garbage collection pauses
- **Native integration** with our existing Rust crates (meridian-trust, meridian-vm, libyggd, norn)

The process runtime provides the concurrency model. Rust provides the execution speed. Gleam provides the type safety and developer experience. Each layer does what it's best at.

## Architecture

```
┌─────────────────────────────────────┐
│          Gleam Workflows            │  Type-checked workflow definitions
│  (pipelines, steps, enrichments)    │  Compiled to native via Rust FFI
├─────────────────────────────────────┤
│        Workflow Engine Library      │  Step dispatch, enrichment threading,
│     (Gleam, calls into runtime)     │  route evaluation, state management
├─────────────────────────────────────┤
│      Beamr Process Runtime          │  Actors, supervisors, message passing,
│           (Rust crate)              │  scheduling, process linking, registry
├─────────────────────────────────────┤
│     Meridian Integration Layer      │  meridian-trust, meridian-vm, norn,
│           (Rust crates)             │  libyggd, meridian-services
└─────────────────────────────────────┘
```

### Runtime Components

- **Process scheduler** — work-stealing across OS threads, preemptive with reduction counting, fair across all running processes
- **Mailbox system** — per-process message queues with selective receive and pattern matching
- **Supervisor framework** — one-for-one, one-for-all, rest-for-one strategies with configurable restart intensity
- **Process registry** — name processes so they can find each other without PID tracking
- **Process links and monitors** — bidirectional crash propagation (links) and unidirectional crash notification (monitors)
- **Timer service** — after-timeout messages, periodic ticks, step deadline enforcement
- **Hot code swap** — module-level versioning so running processes continue on the old version while new spawns use the new code

### Gleam FFI Surface

```gleam
// Core process operations
@external(rust, "beamr_ffi", "spawn")
pub fn spawn(f: fn() -> a) -> Pid

@external(rust, "beamr_ffi", "send")
pub fn send(pid: Pid, msg: message) -> Nil

@external(rust, "beamr_ffi", "receive")
pub fn receive(timeout: Duration) -> Result(message, ReceiveError)

// Supervision
@external(rust, "beamr_ffi", "start_supervisor")
pub fn start_supervisor(spec: SupervisorSpec) -> Result(Pid, StartError)

// Registry
@external(rust, "beamr_ffi", "register")
pub fn register(name: String, pid: Pid) -> Result(Nil, AlreadyRegistered)

@external(rust, "beamr_ffi", "whereis")
pub fn whereis(name: String) -> Result(Pid, NotFound)
```

## Integration With Meridian

The workflow engine runs inside the Meridian server process. Workflows are compiled Gleam modules loaded at startup (or hot-swapped at runtime). The engine dispatches steps as lightweight processes, each with access to the Meridian service layer through typed Gleam bindings.

Norn agent steps call through to the existing Rust Norn runtime via FFI. The process runtime manages the concurrency — when to run which step, how to handle failures, when to retry — while Norn handles the actual agent execution.

For VM-dispatched workflows, the engine sends sealed dispatch envelopes through the exchange to the target VM's dispatch receiver, which runs its own local process runtime. Events stream back through the exchange to the dispatching instance's process runtime, which feeds them into the home instance's broadcast channel.

The existing worktree isolation model carries over: each workflow gets its own git worktree, managed by the process runtime's supervisor. Worktree provisioning and cleanup are supervised operations — if cleanup fails, the supervisor retries. No orphan worktrees.
