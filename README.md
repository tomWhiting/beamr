# beamr

A ground-up BEAM virtual machine written in Rust, targeting [Gleam](https://gleam.run) as its primary source language. Compile your Gleam code with `gleam build`, then run the resulting `.beam` bytecode directly on beamr — no Erlang/OTP runtime required.

Built for [Meridian](https://github.com/ablative/yggdrasil) workflow execution, where Gleam's type system provides compile-time validation: if the workflow compiles, the types are correct.

## Getting started

```bash
# Build the CLI
cargo build --release -p beamr-cli

# Run a compiled Gleam module
./target/release/beamr my_module.beam

# Run a specific function with arguments
./target/release/beamr proof.beam proof:factorial/1 -- 10
# => 3628800
```

## How it works

beamr loads OTP 26 format `.beam` bytecode files and executes them on a preemptive scheduler with work-stealing, generational garbage collection, and a full BEAM term representation. Gleam compiles to Erlang bytecode, so beamr runs Gleam programs by implementing the subset of the BEAM instruction set and standard library that Gleam actually uses.

The key insight: Gleam doesn't use the full breadth of BEAM/OTP. It generates a predictable, well-structured subset of bytecode. beamr targets that subset precisely, implementing native Rust stubs for the Erlang and Gleam stdlib functions that compiled Gleam code calls rather than loading the original Erlang `.beam` implementations.

## CLI usage

```bash
# Run a module's main/0 function (default entry point)
beamr my_module.beam

# Run a specific function with arguments
beamr proof.beam proof:factorial/1 -- 12
# => 479001600

# Alternative --entry flag syntax
beamr proof.beam --entry proof:fibonacci/1 -- 30
# => 832040

# Load dependencies from directories before running
beamr my_app.beam my_app:run/1 --dir ./build/dev/erlang/my_app/ebin -- hello

# Multiple dependency directories
beamr my_app.beam --dir ./deps/gleam_stdlib/ebin --dir ./deps/gleam_otp/ebin

# Check what imports a module needs
beamr imports my_module.beam
```

`beamr imports` lists everything the module needs that beamr does not provide natively — both unresolved BIFs and module dependencies that must be supplied with `--dir`. Empty output means all imports resolve to built-in functions, so the module runs standalone.

## Examples

### Pure computation

```gleam
// proof.gleam
pub fn factorial(n) {
  case n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}

pub fn fibonacci(n) {
  case n {
    0 -> 0
    1 -> 1
    _ -> fibonacci(n - 1) + fibonacci(n - 2)
  }
}
```

```
$ gleam build
$ beamr proof.beam proof:factorial/1 -- 20
2432902008176640000

$ beamr proof.beam proof:fibonacci/1 -- 30
832040
```

### Multi-module Gleam project

beamr loads entire Gleam projects by pointing `--dir` at the compiled `.beam` output. All modules in the directory are loaded into the module registry before the entry module runs, so cross-module calls resolve normally.

```bash
# After gleam build, the .beam files are in build/dev/erlang/<project>/ebin/
beamr build/dev/erlang/my_app/ebin/my_app.beam --dir build/dev/erlang/my_app/ebin
```

### End-to-end workflow execution

beamr can run complete Gleam workflows that read files, execute shell commands, and write output — the full path from source to execution that Meridian uses for workflow orchestration.

```gleam
// sample_workflow.gleam — reads input, runs a command, writes output
import gleam/io
import gleam/string

@external(erlang, "meridian_ffi", "read_file")
pub fn read_file(path: String) -> String

@external(erlang, "meridian_ffi", "write_file")
pub fn write_file(path: String, content: String) -> Nil

@external(erlang, "meridian_ffi", "run_command")
pub fn run_command(cmd: String, args: List(String)) -> String

pub fn main() {
  let input = read_file("input.txt")
  let result = run_command("echo", ["processed: " <> input])
  write_file("output.txt", string.trim(result))
}
```

The `meridian_ffi` functions are native Rust BIFs registered in beamr — they're not Erlang code. This is how Meridian exposes host capabilities to Gleam workflows while keeping the type-safe boundary.

## Architecture

```
crates/
  beamr/              Core VM library (~22k lines of Rust)
    src/
      atom/           Atom table — interned strings with fast integer lookup
      gc/             Generational copying garbage collector (minor + major)
      interpreter/    Bytecode interpreter, opcode dispatch, pattern matching
      loader/         .beam file parser, decoder, module loader
      mailbox/        Lock-free process mailboxes with selective receive
      native/         200+ BIF implementations across:
        bifs           Core erlang BIFs (arithmetic, comparison, type checks)
        gate3_bifs     Extended erlang BIFs (type conversion, bitwise, math)
        gleam_ffi      Gleam-specific FFI functions
        otp_stubs      OTP module stubs (gleam_erlang, gleam_otp)
        stdlib_stubs   Standard library BIFs (collections, strings, IO, encoding)
        process_bifs   Process management BIFs (spawn, link, monitor)
      process/        Process state, heap, stack, registry
      scheduler/      Preemptive scheduler with work-stealing and dirty schedulers
      supervision/    OTP-style links, monitors, exit signal propagation
      term/           Tagged term representation (integers, atoms, binaries,
                      tuples, lists, maps, pids, floats, closures)
    tests/            Integration tests (OTP loading, GC, supervision, e2e)

  beamr-cli/          Command-line .beam runner
```

The Meridian integration layer (`beamr-meridian`) lives in the [yggdrasil](https://github.com/ablative/yggdrasil) repository, where it wires `MeridianRuntime`, the async NIF bridge, and `run_workflow` into the Meridian orchestration engine.

## What's implemented

- **Bytecode execution**: OTP 26 format `.beam` loading and execution, covering the instruction subset Gleam generates
- **Term representation**: Full BEAM term system — small integers, atoms, heap binaries, tuples, lists (cons cells), maps, pids, floats, closures — all using a 64-bit tagged pointer scheme
- **Preemptive scheduling**: Configurable thread pool with work-stealing (crossbeam deques), reduction counting, dirty scheduler pool for long-running BIFs
- **Garbage collection**: Generational copying GC wired to the interpreter via `test_heap` instructions, Fibonacci heap growth
- **Process primitives**: Spawn, link, monitor, exit signals, process registry — the core of OTP's actor model
- **Supervision**: Start-link, restart-on-crash, exit signal propagation through supervision trees
- **Mailboxes**: Lock-free process mailboxes with selective receive (the `select/1` pattern Gleam uses)
- **200+ native BIFs**: Covering erlang, lists, maps, string, binary, io, math, unicode, rand, uri, and all Gleam stdlib FFI modules
- **JSON**: Native OTP 27 `json` module (`decode/1`, `encode/1`, `encode_integer/1`, `encode_float/1`, `encode_binary/1`) so `gleam_json` works out of the box; Term to `serde_json::Value` bridging remains behind the `json` feature flag
- **Async NIF support**: `wake_with_result` for suspending a BEAM process and delivering results from host-side async operations
- **Zero unresolved imports**: All `gleam_otp` `.beam` modules load cleanly

## Testing

```bash
cargo test --workspace                    # 1,500+ tests
```

## Design decisions

Key architectural choices are documented as ADRs in `docs/adr/`:

| ADR | Decision |
|-----|----------|
| 001 | Loader lives inside core crate (not separate) |
| 002 | Global atom table in core |
| 003 | No async in the scheduler — synchronous reduction loop |
| 004 | Low-bit term tagging (3-bit tag in low bits of u64) |
| 005 | Only implement opcodes Gleam generates |
| 006 | BIFs are demand-driven via import table |
| 007 | Supervision is a library layer, not baked into scheduler |
| 008 | Message passing copies terms between process heaps |
| 009 | Reduction boundary hook is a registration point |
| 010 | Dirty scheduler pool for long-running operations |
| 011 | Lock-free mailbox implementation |

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
