# beamr

A Rust BEAM virtual machine for running compiled Gleam workflows. Built for [Meridian](https://github.com/ablative/yggdrasil) workflow execution.

## Quick start

```bash
cargo build --release -p beamr-cli
```

## Running Gleam programs

Write a Gleam module, compile it with `gleam build`, then run the `.beam` file:

```bash
# Run a module's main/0 function
beamr my_module.beam

# Run a specific function with arguments
beamr proof.beam proof:factorial/1 -- 10
# => 3628800

beamr proof.beam proof:fibonacci/1 -- 20
# => 6765
```

### Loading multi-module projects

Use `--dir` to load dependencies before the main module:

```bash
beamr my_app.beam my_app:run/1 --dir ./build/dev/erlang/my_app/ebin -- hello
```

Multiple `--dir` flags are supported. All `.beam` files in each directory are loaded into the module registry.

### Inspecting imports

Check what a `.beam` file needs before running it:

```bash
beamr imports my_module.beam
```

If the output is empty, beamr can run the module. Listed imports are BIFs or modules not yet implemented.

## Examples

### Factorial

```gleam
// proof.gleam
pub fn factorial(n) {
  case n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}
```

```
$ beamr proof.beam proof:factorial/1 -- 12
479001600
```

### Fibonacci

```gleam
pub fn fibonacci(n) {
  case n {
    0 -> 0
    1 -> 1
    _ -> fibonacci(n - 1) + fibonacci(n - 2)
  }
}
```

```
$ beamr proof.beam proof:fibonacci/1 -- 30
832040
```

### Pattern matching pipeline

```gleam
pub fn classify(n) {
  case n {
    x if x < 0 -> negative
    0 -> zero
    x if x > 100 -> large
    _ -> small
  }
}
```

## Architecture

```
beamr (core)          BEAM bytecode interpreter, term representation,
                      scheduler, GC, module loader, BIF registry

beamr-cli             Command-line runner for .beam files

beamr-meridian        Integration layer for Meridian workflow execution
(yggdrasil repo)      MeridianRuntime, NIF wiring, run_workflow entry point
```

### What works

- BEAM bytecode loading and execution (OTP 26 format)
- Full term representation (integers, atoms, binaries, tuples, lists, maps, pids, floats)
- Preemptive scheduler with work-stealing (configurable thread count)
- Generational copying garbage collector wired to interpreter
- OTP process primitives (spawn, link, monitor, exit signals)
- Supervisor support (start_link, restart on crash)
- 200+ native BIFs across erlang, gleam_stdlib, string, binary, maps, lists modules
- JSON interop bridge (Term to serde_json::Value, behind `json` feature flag)
- All gleam_otp `.beam` modules load with zero unresolved imports

### What's next

- Remaining stdlib BIF coverage (~96 functions for full sample workflow support)
- Async step runner bridge for Meridian agent dispatch
- Full end-to-end Gleam workflow execution through MeridianRuntime

## Testing

```bash
cargo test --workspace                    # 562 tests
cargo test --workspace --features json    # +13 JSON bridge tests
```

## License

Proprietary. Part of the Ablative platform.
