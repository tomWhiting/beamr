# beamr-cli

Command-line runner for [beamr](https://crates.io/crates/beamr) — load and execute `.beam` bytecode files compiled from [Gleam](https://gleam.run) (or Erlang) source.

## Installation

```bash
cargo install beamr-cli
```

Or build from source:

```bash
cargo build --release -p beamr-cli
```

The binary is named `beamr`.

## Usage

```bash
# Run a module's main/0 function
beamr my_module.beam

# Run a specific function with arguments
beamr proof.beam proof:factorial/1 -- 12
# => 479001600

# Alternative --entry flag syntax
beamr proof.beam --entry proof:fibonacci/1 -- 30
# => 832040

# Load dependency modules from a directory before running
beamr my_app.beam --dir ./build/dev/erlang/my_app/ebin

# Multiple dependency directories
beamr my_app.beam --dir ./deps/gleam_stdlib/ebin --dir ./deps/gleam_otp/ebin

# Check what imports a module needs before running
beamr imports my_module.beam
```

### Entry point format

Entry points follow the Erlang convention: `module:function/arity`.

```
beamr hello.beam hello:greet/1 -- world
```

Arguments after `--` are passed to the function. Integer literals are parsed as BEAM integers; everything else becomes a binary (string).

### Import checking

`beamr imports` lists any unresolved imports a module needs. If the output is empty, beamr can run the module natively.

```bash
$ beamr imports my_module.beam
some_module:some_function/2
```

Use `--dir` to load the modules that provide those imports.

## Typical Gleam workflow

```bash
# Write Gleam code
cat > hello.gleam << 'GLEAM'
pub fn main() {
  42
}
GLEAM

# Compile with Gleam
gleam build

# Run with beamr
beamr build/dev/erlang/hello/ebin/hello.beam
# => 42
```

## License

Apache-2.0
