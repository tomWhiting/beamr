# Beamr — User Stories

## Workflow Author — Writing Gleam Workflows

**S1.** As a workflow author, I want to write workflow steps as typed Gleam functions that compile to .beam bytecode so that I get compile-time type checking on my entire pipeline.

**S2.** As a workflow author, I want each workflow step to run in its own isolated process so that a crash in one step does not corrupt the state of another.

**S3.** As a workflow author, I want to use Gleam's pattern matching and case expressions for routing so that the compiler catches unhandled cases at build time, not at runtime.

**S4.** As a workflow author, I want to call Rust-native operations (git, AST merge, dependency graph) from Gleam via registered functions so that I get zero-overhead access to the host's capabilities.

**S5.** As a workflow author, I want workflow steps to communicate via typed message passing so that data flows between steps without shared mutable state or serialization.

**S6.** As a workflow author, I want to define supervision trees over my workflow steps so that failed steps are automatically restarted according to a declared strategy.

**S7.** As a workflow author, I want to set timeouts on receive expressions so that a step waiting for input does not hang indefinitely if its dependency fails silently.

**S32.** As a workflow author, I want the VM not to leak memory monotonically when my workflow calls BIFs or materialises literals in a loop, so that long-running workflows don't OOM the host.

## Meridian Runtime — Embedding beamr for Workflow Execution

**S8.** As the Meridian runtime, I want to create a beamr VM instance, load .beam modules, and spawn processes from Rust so that I can embed workflow execution in the server process.

**S9.** As the Meridian runtime, I want to register Rust functions as callable BIFs and NIFs before loading modules so that Gleam code can call Yggdrasil, git, and Meridian operations.

**S10.** As the Meridian runtime, I want a reduction-boundary hook that fires at every process yield so that the conventions-and-diagnostics pipeline can inspect and intervene in running workflows.

**S11.** As the Meridian runtime, I want long-running native calls to execute on a dirty scheduler pool so that they do not block the normal scheduler threads or starve other processes.

**S12.** As the Meridian runtime, I want the scheduler to use one thread per CPU core with work-stealing so that all cores are utilized and no core sits idle while others are backlogged.

**S13.** As the Meridian runtime, I want the VM to guarantee preemptive fairness via reduction counting so that no single workflow can monopolize CPU and starve other workflows.

**S14.** As the Meridian runtime, I want beamr to have zero dependencies on Meridian, Yggdrasil, or norn crates so that the VM is testable and buildable in isolation.

**S15.** As the Meridian runtime, I want to shut down the VM cleanly, terminating all scheduler threads and processes, so that there are no leaked threads or orphan resources.

**S31.** As the Meridian runtime, I want native function access gated by a capability policy so that untrusted Gleam code cannot reach shell commands or filesystem operations without explicit host authorization.

## Implementation Agent — Building beamr from Briefs

**S16.** As an implementation agent, I want the loader to be a well-encapsulated module within the beamr crate so that I can test .beam parsing via module-level tests without needing external harnesses.

**S17.** As an implementation agent, I want the loader's unresolved-import report to tell me exactly which BIFs a given .beam file needs so that I build only the natives required, not the entire erlang: module.

**S18.** As an implementation agent, I want each validation gate to have a concrete test suite so that I can verify my work against defined pass/fail criteria before moving to the next gate.

**S19.** As an implementation agent, I want the term representation, heap, and GC to be defined in a single crate with clear module boundaries so that I can understand the term lifecycle without cross-crate indirection.

**S20.** As an implementation agent, I want the interpreter's opcode dispatch to be organized by instruction category so that I can implement opcodes incrementally (calls first, then branching, then binary matching) without touching unrelated code.

**S21.** As an implementation agent, I want the native function interface to accept standard Rust closures so that registering a new BIF requires only one function call with a module name, function name, arity, and closure.

**S22.** As an implementation agent, I want explicit error types rather than panics for all loader and interpreter failures so that test failures produce actionable diagnostics.

## Reviewer — Verifying beamr Implementation

**S23.** As a reviewer, I want each validation gate's test suite to run as a standard cargo test target so that I can verify gate passage with a single command.

**S24.** As a reviewer, I want property tests on the GC that verify all reachable terms survive collection unchanged so that I can have confidence in the collector's correctness beyond handwritten cases.

**S25.** As a reviewer, I want a fairness test that spawns N processes in a tight computational loop and measures that all N make progress within 2x the expected time so that I can verify no process is starved.

**S26.** As a reviewer, I want the loader to produce snapshot-testable output from known .beam files so that I can diff the decoded instructions against a verified baseline.

**S27.** As a reviewer, I want to verify that beamr's Cargo.toml has no dependency on any Meridian, Yggdrasil, or norn crate so that the isolation rule is machine-checkable.

**S28.** As a reviewer, I want to run real Gleam test suites (compiled to .beam) on beamr so that correctness is validated against the language's own expectations, not just handwritten bytecode.

**S29.** As a reviewer, I want the exit signal propagation to be tested with multi-level link chains so that I can verify cascading failures propagate correctly through supervision trees.

**S30.** As a reviewer, I want the reduction boundary hook to be tested with a callback that records every yield so that I can verify the hook fires at every yield point and receives correct process metadata.

**S33.** As a reviewer, I want to verify that the loader rejects adversarial .beam files (deep nesting, huge counts, zlib bombs) within bounded resource usage, so that untrusted bytecode cannot crash or OOM the host during decode.
