# beamr

A Rust implementation of the BEAM virtual machine, targeting [Gleam](https://gleam.run) as its primary source language. Load compiled `.beam` bytecode and execute it with preemptive scheduling, per-process isolation, garbage collection, and OTP-style supervision — no Erlang runtime required.

## Usage

Add beamr to your `Cargo.toml`:

```toml
[dependencies]
beamr = "0.1"
```

### Running a Gleam module

```rust
use std::sync::Arc;
use beamr::atom::AtomTable;
use beamr::loader::load_module;
use beamr::module::ModuleRegistry;
use beamr::native::{BifRegistryImpl, bifs::register_gate1_bifs};
use beamr::scheduler::{Scheduler, SchedulerConfig};
use beamr::process::ExitReason;

// Set up the VM
let atom_table = AtomTable::with_common_atoms();
let bif_registry = BifRegistryImpl::new();
register_gate1_bifs(&bif_registry, &atom_table).unwrap();

// Load a .beam file
let bytes = std::fs::read("my_module.beam").unwrap();
let module_registry = ModuleRegistry::new();
let (module, unresolved) = load_module(
    &bytes, &atom_table, &module_registry, &bif_registry,
).unwrap();

// Spawn and run
let registry = Arc::new(module_registry);
let scheduler = Scheduler::new(
    SchedulerConfig { thread_count: Some(1) },
    Arc::clone(&registry),
).unwrap();

let main_fn = atom_table.intern("main");
let pid = scheduler.spawn(module.name, main_fn, vec![]).unwrap();
let (reason, result) = scheduler.run_until_exit(pid);
scheduler.shutdown();

assert_eq!(reason, ExitReason::Normal);
```

### Key types

| Type | Description |
|------|-------------|
| `Scheduler` | Preemptive scheduler with work-stealing. Spawn processes, run them, deliver async results. |
| `Term` | Tagged 64-bit BEAM term — integers, atoms, binaries, tuples, lists, maps, pids, floats, closures. |
| `AtomTable` | Interned string table for atoms. Thread-safe, used throughout the VM. |
| `ModuleRegistry` | Loaded module storage. Modules are registered here during loading and looked up during execution. |
| `BifRegistryImpl` | Registry of native Rust functions callable from BEAM bytecode. |

### Scheduler API

```rust
// Spawn a process calling module:function(args)
let pid = scheduler.spawn(module_atom, function_atom, args)?;

// Block until a process exits, returning its exit reason and result
let (reason, result) = scheduler.run_until_exit(pid);

// Deliver an async result to a suspended process (for NIF bridges)
scheduler.wake_with_result(pid, result_term);

// Kill a process from the host side
scheduler.terminate_process(pid, ExitReason::Kill);

// Configure I/O output destination
scheduler.set_output_sink(Arc::new(my_sink));

// Clean shutdown
scheduler.shutdown();
```

### I/O sink

By default, beamr discards all I/O output (`NullSink`). To capture or redirect output, implement the `IoSink` trait:

```rust
use beamr::io::IoSink;

struct MyLogSink;
impl IoSink for MyLogSink {
    fn write(&self, bytes: &[u8]) {
        log::info!("{}", String::from_utf8_lossy(bytes));
    }
}

scheduler.set_output_sink(Arc::new(MyLogSink));
```

### Feature flags

| Flag | Description |
|------|-------------|
| `json` | Enables bidirectional `Term` to `serde_json::Value` conversion. Adds `base64` and `serde_json` dependencies. |

```toml
[dependencies]
beamr = { version = "0.1", features = ["json"] }
```

## What's included

- **Bytecode interpreter**: OTP 26 `.beam` format, covering the opcode subset Gleam generates
- **Term system**: Full BEAM term representation with 64-bit tagged pointers
- **Preemptive scheduler**: Configurable thread pool, work-stealing, reduction counting, dirty schedulers
- **Garbage collector**: Generational copying GC with Fibonacci heap growth
- **Process primitives**: Spawn, link, monitor, exit signals, process registry
- **Supervision**: OTP-style links, monitors, restart-on-crash
- **Mailboxes**: Lock-free with selective receive
- **200+ native BIFs**: erlang, lists, maps, string, binary, io, math, and all Gleam stdlib FFI modules

## License

Apache-2.0
