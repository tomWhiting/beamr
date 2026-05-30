# BEAM Alternatives Survey

> Target path: `docs/architecture/00-beam-alternatives-survey.md`  
> Survey date: 2026-05-30  
> Scope: open-source projects that attempt to rebuild, reimagine, subset, compile around, or replace BEAM/ERTS; plus known BEAM bottlenecks relevant to `beamr`.

---

## Executive summary

The successful projects do **not** try to be a full, bug-compatible BEAM/ERTS/OTP replacement. They pick a narrower axis:

- **AtomVM** succeeds by targeting embedded/IoT with a deliberately small BEAM-compatible subset.
- **Lunatic** succeeds conceptually by taking BEAM’s *process isolation and supervision ideas* but replacing BEAM terms/bytecode with WebAssembly instances.
- **Erjang** showed a JVM-hosted Erlang VM could run substantial Erlang and sometimes outperform BEAM, but Erlang’s process model and per-process GC are awkward on a host VM with global GC.
- **Lumen / Firefly** was the most ambitious Rust attempt: Erlang/Elixir → LLVM/WASM/native executable, with OTP compatibility goals. It was archived in 2024. Its failure mode appears to be scope: compiler + runtime + OTP parity + WASM + native + Rust + LLVM is too much for a small team.
- **LAM** is a useful design warning: a tiny, specified actor VM is compelling, but if it is neither BEAM-compatible nor broadly adopted, it risks becoming an interesting solo experiment.
- **BEAM-on-WASM experiments** fall into two camps: compile/port an Erlang runtime into WASM, or compile a subset of Elixir/Erlang to WASM. Both run into runtime size, GC, threading, syscall, dynamic code loading, and browser integration problems.
- **GraalVM/Truffle BEAM**: I found no significant maintained Truffle-based BEAM implementation. The closest serious historical JVM attempt is **Erjang**, which predates/does not use Truffle.

Main lesson for `beamr`: **do not attempt “full BEAM replacement” first**. Define a narrow compatibility slice, document term/process semantics explicitly, and make bytecode loading, message passing, GC, and scheduler behavior observable from day one.

---

## Comparison table

| Project | Status | Language | Goal | BEAM compatibility posture | Key lesson |
|---|---:|---|---|---|---|
| Lumen / Firefly | Archived/read-only since 2024-06-10 | Rust + LLVM | Erlang/Elixir compiler/runtime targeting WASM, native, static executables | Wanted OTP feature parity except hot upgrades/dynamic code loading | Full OTP-compatible replacement is enormous; avoid combining compiler, runtime, OTP, LLVM, WASM, and native all at once |
| AtomVM | Active | C | Tiny Erlang VM for MCUs/embedded/lightweight runtimes | Runs unmodified compiled BEAM modules, with a subset of BEAM/OTP | Pick a constrained environment and subset; own the tradeoffs |
| Lunatic | Dormant-to-low-activity; latest GitHub release shown 2023-05-03 | Rust | Erlang-inspired runtime for WASM processes | Not BEAM-compatible; BEAM-inspired actor/supervision model | WASM instance-per-process gives strong isolation and capability security |
| LAM | Abandoned/dormant experiment | Rust | Tiny actor VM for native + WASM; “LuaVM meets actor model” | Intended bytecode translation for some Erlang programs, not full BEAM | Specification-first is good; solo VM + language ecosystem is hard |
| BEAM-on-WASM / Erlang-WASM experiments | Fragmented; some active experiments | C/Emscripten, Elixir, Rust, JS glue | Run BEAM/Erlang/Elixir in browser/WASI, or compile subsets to WASM | Usually subset or runtime-in-WASM, not full OTP | WASM is not “just another native target”: GC, threads, dynamic loading, syscalls, and host integration dominate |
| Erjang | Inactive/unmaintained | Java | Erlang VM on JVM; BEAM bytecode to JVM bytecode | Substantial Erlang/OTP support historically | Host VM mismatch: JVM helps JIT/libs, hurts Erlang process model and GC semantics |
| GraalVM/Truffle BEAM | No significant maintained project found | Java/Truffle if attempted | Hypothetical BEAM language implementation on GraalVM | No evidence of production-grade implementation | Truffle gives JIT/instrumentation, but not Erlang’s process/GC/distribution semantics for free |
| HiPE / BEAMJIT / BeamAsm | In-tree or academic, not replacement VM | C/C++/AsmJit/LLVM research | Improve BEAM execution performance | Extends BEAM, not alternative VM | Incremental evolution inside ERTS has succeeded more than external replacement |

---

# 1. Lumen / Firefly

## 1.1 Project name and status

- **Names:** Lumen, later renamed **Firefly**.
- **Repository:** `GetFirefly/firefly`.
- **Status:** Archived/read-only. GitHub reports: “This repository was archived by the owner on Jun 10, 2024.” The repository describes itself as “An alternative BEAM implementation, designed for WebAssembly.” The README says Firefly was previously named Lumen and renamed in 2022. ([github.com](https://github.com/GetFirefly/firefly?utm_source=openai)) ([github.com](https://github.com/GetFirefly/firefly))
- **Language:** Rust, with LLVM as compiler backend.

## 1.2 Goal

Firefly’s README states that the primary motivator was compiling Elixir applications to WebAssembly for frontend use, while also supporting other platforms such as x86 self-contained executables. It explicitly lists differences from BEAM:

- standalone executables,
- WebAssembly and other targets,
- ahead-of-time machine-code compilation and bytecode compilation,
- no planned support for hot code reloading,
- implementation in Rust to understand BEAM internals and the implications of writing such a runtime in a restrictive safe language. ([github.com](https://github.com/GetFirefly/firefly))

Its stated goals were:

- WebAssembly/embedded systems as first-class platforms,
- easy-to-deploy static executables,
- integration with BEAM-language tooling,
- OTP feature parity except listed non-goals. ([github.com](https://github.com/GetFirefly/firefly))

Its non-goals were:

- hot upgrades/downgrades,
- dynamic code loading. ([github.com](https://github.com/GetFirefly/firefly))

## 1.3 Architecture choices

### Compiler pipeline

The README documents a classic multi-IR compiler:

1. Erlang source parser → AST.
2. Lowering through:
   - Core IR, similar to Core Erlang,
   - Kernel IR, similar to Kernel Erlang,
   - SSA IR,
   - Bytecode/MLIR for final optimizations and code generation.
3. Final output produces LLVM IR, then object files, then a linked executable. ([github.com](https://github.com/GetFirefly/firefly))

### Runtime

The runtime is described as “mostly the same as OTP,” with backend-dependent differences. Public README details:

- entry point sets up environment and starts scheduler,
- one scheduler per thread,
- work stealing between schedulers,
- child processes initially spawn on the same scheduler as parent,
- I/O is asynchronous and integrates with scheduler signal management. ([github.com](https://github.com/GetFirefly/firefly))

### Term representation

The public README does not document detailed term layout. Given the OTP-compatibility goal, it presumably needed Erlang-compatible tagged terms, boxed terms, PIDs, refs, lists, tuples, maps, binaries, closures, and off-heap/shared binary equivalents. But this should be treated as an implementation detail to inspect in source, not assumed from README.

### Process model

BEAM-like lightweight processes, scheduler-owned. README says processes are spawned on the same scheduler as the spawning process, but can be stolen for load balancing. ([github.com](https://github.com/GetFirefly/firefly))

### Scheduling

- One scheduler per thread.
- Work stealing.
- Async I/O integrated with scheduler signal management. ([github.com](https://github.com/GetFirefly/firefly))

### GC

Not clearly documented in the README. Likely one of the hardest parts of the project because OTP compatibility implies per-process GC, binary sharing/reference counting, process heaps, reductions, and NIF interactions.

### Message passing

Not deeply documented in public README. A BEAM-compatible runtime would need mailbox semantics, selective receive, monitors/links, local/remote PID semantics, and copy-vs-share binary behavior.

### Bytecode/code loading

Firefly intentionally rejected dynamic code loading as a non-goal. That is a major architectural simplification and a major compatibility break. ([github.com](https://github.com/GetFirefly/firefly))

## 1.4 What it did well

- Clear product angle: Elixir/Erlang in WASM and static executable deployment.
- Rust implementation made internals more approachable than ERTS C.
- Compiler architecture was modern: Core/KIR/SSA/MLIR/LLVM.
- Explicitly declared non-goals around hot upgrades and dynamic loading, avoiding one of BEAM’s hardest compatibility surfaces.
- Recognized deployment pain: static executables and smaller runtime footprint.

## 1.5 What failed or was abandoned

The repository is archived, so the project is no longer maintained. The documented scope was extremely large:

- source compiler,
- runtime,
- scheduler,
- GC,
- NIF/port support,
- OTP compatibility,
- WASM,
- native executables,
- toolchain integration,
- standard library support.

The README itself warned Firefly was not as battle-tested or necessarily as performant as BEAM and was still experimental. ([github.com](https://github.com/GetFirefly/firefly))

Likely failure mode: **scope exceeded sustainable team capacity**. Achieving “drop-in replacement” behavior requires reproducing not just BEAM bytecode execution but ERTS, OTP assumptions, loader behavior, exception semantics, tracing, ports, NIFs, distribution, timers, ETS, binaries, and tooling.

## 1.6 Lessons for beamr

1. **Declare non-goals early.** Firefly’s explicit rejection of hot upgrades/dynamic code loading is a good model.
2. **Do not start with full OTP parity.** It is too large.
3. **Separate compiler and runtime scope.** A BEAM bytecode runtime, a source compiler, and a WASM/native AOT toolchain are each major projects.
4. **Avoid LLVM as a required first milestone unless native performance is the core goal.** LLVM adds build complexity and backend constraints.
5. **Make compatibility testable by slices.** For example: “can load BEAM chunks X/Y/Z,” “can execute opcodes A/B/C,” “supports selective receive subset,” etc.

---

# 2. AtomVM

## 2.1 Project name and status

- **Project:** AtomVM.
- **Repository:** `atomvm/AtomVM`.
- **Status:** Active. README says AtomVM is “no longer just a prototype” and has “reached a solid level of compatibility with the BEAM ecosystem.” GitHub shows latest release `v0.6.6` on 2025-06-23. ([github.com](https://github.com/atomvm/AtomVM))
- **Language:** C.

## 2.2 Goal

AtomVM “brings Erlang, Elixir and other functional languages to really small systems.” It implements from scratch a minimal Erlang VM that supports a subset of Erlang VM features and can run unmodified BEAM binaries on small systems such as MCUs. Supported platforms include generic Unix, ESP32, STM32, Raspberry Pi Pico/Pico 2, and browsers/NodeJS via WebAssembly. ([github.com](https://github.com/atomvm/AtomVM))

## 2.3 Architecture choices

### Term representation

AtomVM’s memory-management docs are unusually explicit.

- Each process is represented internally by a `Context`.
- Stack and heap occupy one malloc’d memory region and grow toward each other.
- Registers are a fixed array of 16 terms.
- A `term` is a single machine word (`C int`-sized integral type).
- Low-order bits tag immediates and pointers. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))

Examples:

- Atoms: low-order 6 bits `0xB`; high bits index the global atom table. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))
- Integers: low-order 4 bits `0xF`; high bits contain integer value. AtomVM currently does **not** support arbitrary bignums. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))
- PIDs: low-order 4 bits `0x03`; high bits store local process id. Global PIDs are not currently supported. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))
- Boxed term pointers: address in high bits plus low tag `0x2`. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))
- Tuples: boxed header with type tag `0x00`, followed by element words. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))
- Maps: boxed header plus tuple of keys and corresponding values. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))

### Process model

Each Erlang process has an independent `Context` with stack, heap, registers, process dictionary, and mailbox. The docs explicitly treat “execution context” and “Erlang process” interchangeably. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))

### Scheduling

AtomVM supports SMP builds; its docs note that in SMP builds AtomVM runs one scheduler thread per core. ([atomvm.net](https://www.atomvm.net/doc/v0.6.0/atomvm-internals.html?utm_source=openai))

Important limitation: AtomVM does not have dirty schedulers, and NIFs/ports run on schedulers and should return quickly. ([doc.atomvm.org](https://doc.atomvm.org/main/differences-with-beam.html))

### GC

AtomVM uses per-process tracing/copying GC, similar in spirit to BEAM:

- Process stack and heap are one region.
- GC allocates a new block, copies root terms from stack/registers/process dictionary, iteratively copies reachable terms, sweeps mark-sweep objects, deletes old heap. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))
- GC is synchronous per `Context`, but conceptually does not impact other contexts except OS allocator locks. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))

AtomVM is optimized for RAM use rather than speed. Its differences doc says it uses much less RAM than BEAM but is much slower, even with JIT enabled; its process heap initial size and growth strategy are more aggressive, causing more frequent GC. ([doc.atomvm.org](https://doc.atomvm.org/main/differences-with-beam.html))

### Message passing

AtomVM mailboxes are linked lists of messages. A sent message is copied into a message structure. Message term representation is identical to heap/heap-fragment representation. When the message is read from the mailbox, it becomes a heap fragment of the receiver and later moves into the main heap during GC. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))

Large binaries are off-heap/reference-counted so large binary blocks can be shared between processes by copying a small term reference. Const binaries can point directly into constant memory such as memory-mapped BEAM literals. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))

### Bytecode loading

AtomVM runs BEAM files generated by Erlang/Elixir compilers, but commonly packages them into `.avm` packbeam files.

Packbeam:

- aggregates BEAM and plain files,
- is uploaded/flashed to embedded media,
- contains headers and file entries,
- runtime scans for first BEAM file exporting `start/0`,
- strips BEAM chunks to reduce flash,
- uncompresses literal table data because runtime does not include zlib decompression. ([doc.atomvm.org](https://doc.atomvm.org/latest/packbeam-format.html))

Supported BEAM chunks include `AtU8`, `Code`, `ExpT`, `LocT`, `ImpT`, `LitU`, `FunT`, `StrT`, `LitT`; other chunks are stripped. ([doc.atomvm.org](https://doc.atomvm.org/latest/packbeam-format.html))

### WASM port

AtomVM has an Emscripten-based WASM port. In browsers, `main` runs in a Web Worker because browser main threads cannot run AtomVM scheduler loops; JS can call/cast messages into Erlang processes. ([doc.atomvm.org](https://doc.atomvm.org/v0.6.6/atomvm-internals.html))

## 2.4 What it did well

- Chose a narrow, valuable domain: embedded Erlang/Elixir.
- Maintains compatibility with normal BEAM files where possible.
- Provides excellent documentation of term layout, process memory, GC, packbeam format, and limitations.
- Makes memory footprint a first-class constraint.
- Embraces missing features as design tradeoffs rather than pretending to be full BEAM.

## 2.5 What failed or was abandoned

AtomVM has not failed, but its limitations are instructive:

- minimal standard library,
- missing OS-dependent features,
- no `on_load`,
- no tracing,
- limited OTP support,
- subset implementations of `gen_server`, `gen_statem`, `supervisor`, `proc_lib`, `sys`,
- no dirty schedulers,
- NIFs/ports must be statically linked in embedded environments,
- not as fast as BEAM. ([github.com](https://github.com/atomvm/AtomVM)) ([doc.atomvm.org](https://doc.atomvm.org/main/differences-with-beam.html))

## 2.6 Lessons for beamr

1. **Document term layout like AtomVM.** This is essential for contributors and debuggability.
2. **Use pack formats deliberately.** A simplified packbeam-like format can avoid full dynamic loader complexity.
3. **Small subset beats vague compatibility.**
4. **Explicitly state unsupported BEAM features.**
5. **Per-process GC and message heap-fragment strategy are proven, understandable choices.**
6. **Memory footprint and scheduler latency should be measurable acceptance criteria, not afterthoughts.**

---

# 3. Lunatic

## 3.1 Project name and status

- **Project:** Lunatic.
- **Repository:** `lunatic-solutions/lunatic`.
- **Status:** Public repo, not archived, but appears low-activity/dormant. GitHub page shows latest release `Lunatic v0.13.2` on 2023-05-03. ([github.com](https://github.com/lunatic-solutions/lunatic))
- **Language:** Rust, plus WebAssembly components.

## 3.2 Goal

Lunatic is an Erlang-inspired runtime for WebAssembly. Its README says it is a universal runtime for fast, robust, scalable server-side apps and can be used from any language that compiles to WebAssembly. ([github.com](https://github.com/lunatic-solutions/lunatic))

It is not an Erlang/BEAM implementation. It takes BEAM ideas:

- lightweight isolated processes,
- supervision,
- fault tolerance,
- message passing,
- distributed nodes,

and implements them over WASM modules/instances.

## 3.3 Architecture choices

### Term representation

Lunatic does not use BEAM terms. Application-level values are language/WASM memory values. Cross-process communication uses Lunatic’s channel/message APIs and serialization between WASM instance boundaries.

### Process model

The key design choice: **each process has its own WebAssembly instance**, including its own stack, heap, and syscalls. The README states each process has its own stack, heap, and syscalls, and failure of one process does not affect the rest of the system. ([github.com](https://github.com/lunatic-solutions/lunatic))

This is stronger isolation than BEAM in some respects: it can sandbox C bindings compiled to WASM so C crashes/vulnerabilities are contained to the current process. ([github.com](https://github.com/lunatic-solutions/lunatic))

An Elixir Forum discussion by Lunatic’s author contrasts it with Lumen/LAM: Lunatic does not run inside one WASM instance; it runs thousands of WASM instances as lightweight processes. This enables per-process memory/CPU limits and preemption even for C code compiled to WASM. ([elixirforum.com](https://elixirforum.com/t/lunatic-actor-based-webassembly-runtime-for-the-backend/35617?utm_source=openai))

### Scheduling

Lunatic processes are preemptively scheduled and executed by a work-stealing async executor. The README claims infinite loops will not permanently block an execution thread and that this works regardless of guest language. ([github.com](https://github.com/lunatic-solutions/lunatic))

### GC

Lunatic delegates memory management to guest WASM/language runtimes and the WASM instance boundary. There is no BEAM-style tagged-term per-process GC unless the guest language implements one.

### Message passing

Lunatic supports channel-based message passing. README lists “channel based message passing” as a supported feature. ([github.com](https://github.com/lunatic-solutions/lunatic))

### Bytecode loading

It loads WASM modules, not BEAM bytecode. The CLI runs WASM modules. It intends eventual WASI compatibility. ([github.com](https://github.com/lunatic-solutions/lunatic))

### Capabilities/security

Lunatic has fine-grained process permissions and per-process access to resources such as filesystem, memory, and network enforced at syscall level. ([github.com](https://github.com/lunatic-solutions/lunatic))

## 3.4 What it did well

- Reframed BEAM processes as **WASM sandbox instances**.
- Strong isolation and capability security.
- Language-agnostic: Rust, AssemblyScript, and any WASM-targeting language.
- Handles unsafe/native-like code better than BEAM NIFs: C compiled to WASM can crash without crashing the whole runtime.
- Preemption across guest languages is a strong design point.

## 3.5 What failed or was abandoned

Lunatic’s main challenge is ecosystem/adoption. It is not BEAM-compatible, so BEAM users cannot bring OTP applications over directly. Language bindings must be maintained. WASI compatibility and host integration are ongoing hard problems.

Latest visible release is 2023, suggesting momentum slowed. ([github.com](https://github.com/lunatic-solutions/lunatic))

## 3.6 Lessons for beamr

1. **Process isolation can be stronger than BEAM if built around sandbox instances.**
2. **Capability-based syscalls are worth stealing.**
3. **NIF safety is a major differentiator.** BEAM’s unsafe NIF story remains a weakness.
4. **But not being BEAM-compatible means building a whole new ecosystem.**
5. **If using WASM, decide whether WASM is the process boundary or merely a codegen target.** Lunatic’s clarity here is valuable.

---

# 4. LAM — Little Actor Machine

## 4.1 Project name and status

- **Project:** LAM, “Little Actor Machine.”
- **Status:** Dormant/abandoned experiment. LibHunt reports last commit about five years ago and low activity. ([libhunt.com](https://www.libhunt.com/compare-leostera--lam-vs-firefly?utm_source=openai))
- **Language:** Rust.

## 4.2 Goal

LAM’s tagline was “A Little Actor Machine that runs on Native and WebAssembly.” It aimed to make actor concurrency available everywhere through a specified, lightweight runtime — “LuaVM meets the Actor Model.” ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

It was motivated by BEAM’s size and lack of a formal JVM-style spec. The author explicitly noted that the BEAM implementation is effectively the spec, making a reliable drop-in alternative unlikely. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

## 4.3 Architecture choices

### Term representation

LAM intended to define its own small bytecode/VM, not execute BEAM bytecode directly. Public interview-level documentation does not fully specify term layout.

### Process model

LAM implements Erlang-like actors:

- processes,
- mailboxes,
- message passing,
- fair scheduling through reduction counting,
- planned process linking and monitoring. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

### Scheduling

LAM planned fair scheduling through reduction counting, directly borrowing from BEAM. The author noted a tension for UI/animation workloads: preemptive scheduling can make it hard to guarantee enough time for animation; “Greedy Processes” or dirty-scheduler-like ideas were considered. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

### GC

LAM acknowledged that WebAssembly lacked GC at the time and that LAM would need its own GC. The intended design was close to BEAM: per-process collections and reference-counted binary strings. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

### Message passing

Bytecode-level operations included `spawn`, `send`, `receive`, `call`, and list construction. Side effects would go through platform-specific FFI/bindings. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

### Bytecode loading

LAM is a higher-level VM: feed it bytecode, run side effects through FFI/bindings. Around it was a tiny compilation toolchain that lowers bytecode and packs it with the VM into a platform-optimized single binary. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

It had only about 35 instructions and aspired for much BEAM code to be bytecode-translatable, but not all. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

## 4.4 What it did well

- Specification-first instinct.
- Tiny instruction set.
- Embeddability as a first-class goal.
- Honest about not being full BEAM.
- Considered native, WASI, browser, and GUI applications.

## 4.5 What failed or was abandoned

The project appears to have stalled. The author described it as solo and needing help across design, FFI layers, emulator optimization, GC, binary bundling, spec, and manual. ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))

Failure mode: a new VM needs not just implementation but language tooling, libraries, docs, tests, adoption path, and compatibility story.

## 4.6 Lessons for beamr

1. **A written spec is a competitive advantage.**
2. **But a spec without compatibility/adoption does not create users.**
3. **Keep instruction count small initially.**
4. **Do not defer GC design too long; it affects term representation, message passing, binaries, and FFI.**
5. **If targeting WASM/browser, scheduling policy may need UI-aware exceptions.**

---

# 5. BEAM-on-WASM and Erlang-WASM experiments

This category includes several approaches:

1. **Compile/port a BEAM-like runtime to WASM**  
   Example: AtomVM via Emscripten; Popcorn using AtomVM runtime in WASM.
2. **Compile Erlang/Elixir source/subsets to WASM**  
   Examples: Firefly/Lumen, Firebird, Orb-like subset approaches.
3. **Run a WASM runtime from BEAM**  
   Example: Wasmex, HyperBEAM `beamr` wrappers around WAMR. These are not BEAM replacements but relevant to BEAM/WASM integration.

## 5.1 AtomVM WASM

AtomVM supports browsers and NodeJS with WebAssembly. ([github.com](https://github.com/atomvm/AtomVM))

Its WASM port uses Emscripten. In the web environment:

- modules can be loaded using FetchAPI,
- files can be preloaded with Emscripten tooling,
- `main` runs in a Web Worker via proxy-to-pthread because the browser main thread cannot run scheduler loops,
- JS can call/cast messages to Erlang processes,
- waiting JS promise calls are bridged through Erlang resources and scheduler-delivered messages. ([doc.atomvm.org](https://doc.atomvm.org/v0.6.6/atomvm-internals.html))

### Lessons

- Browser integration is dominated by threading and host event-loop constraints.
- A BEAM-like scheduler loop does not belong on the browser main thread.
- Message bridge semantics need explicit design: sync call, async cast, promise lifetime, failure when target process missing.

## 5.2 Popcorn

Popcorn describes itself as compiling Elixir to Wasm by bridging Elixir code with a Wasm-compiled AtomVM runtime. ([popcorn.swmansion.com](https://popcorn.swmansion.com/?utm_source=openai))

### Lessons

- Reusing AtomVM is pragmatic.
- But runtime-in-WASM means shipping an Erlang VM plus bytecode, not small standalone WASM functions.
- Startup size and host bindings become central.

## 5.3 Firebird / Orb / subset compilers

Firebird docs describe compiling a “compilable subset” of Elixir to WASM and explicitly avoiding the need for a full BEAM runtime in WASM. ([hexdocs.pm](https://hexdocs.pm/firebird/Firebird.Compiler.html?utm_source=openai))

Orb says it is not trying to take everyday Elixir code and run it in WebAssembly; it aims to produce tiny WASM executables. ([useorb.dev](https://useorb.dev/?utm_source=openai))

### Lessons

- Subset compilers are honest and tractable.
- Full Elixir semantics require dynamic dispatch, BEAM terms, processes, exceptions, module loading, binary matching, maps, closures, and OTP assumptions.
- Tiny WASM and full Elixir are conflicting goals.

## 5.4 Fermyon BEAM-language WASM note

Fermyon’s language note says Lumen was attempting to create a BEAM runtime and compiler so BEAM applications could run in a WebAssembly host environment. ([developer.fermyon.com](https://developer.fermyon.com/wasm-languages/erlang-beam?utm_source=openai))

### Lessons

- WASM platforms want small, capability-oriented modules.
- BEAM applications expect a rich VM/ERTS around them.
- Bridging that semantic gap is the real problem, not emitting `.wasm`.

## 5.5 Lessons for beamr

1. **WASM is not a magic portability layer for BEAM.**
2. **If targeting WASM, choose one model:**
   - BEAM runtime compiled to WASM,
   - BEAM subset compiled to WASM,
   - WASM as isolated process implementation,
   - WASM as foreign-code sandbox inside BEAM-like runtime.
3. **Threading and event-loop integration must be designed early.**
4. **Dynamic code loading and hot upgrades are hostile to many WASM deployment models.**
5. **Tiny output and OTP compatibility pull in opposite directions.**

---

# 6. Erjang

## 6.1 Project name and status

- **Project:** Erjang.
- **Status:** Inactive/unmaintained. OpenHub describes it as “JVM-Based Erlang VM,” mostly Java, maintained by nobody. ([openhub.net](https://openhub.net/p/erjang?utm_source=openai))
- **Language:** Java.
- **Era:** Active around 2010–2014.

## 6.2 Goal

Erjang implemented an Erlang VM on the JVM. A JVM Language Summit abstract says it became a non-trivial project with 65k+ lines of Java, ran substantial Erlang programs, and for some programs ran faster than “Erlang classic.” ([wiki.jvmlangsummit.com](https://wiki.jvmlangsummit.com/Erjang_-_A_JVM-based_Erlang_VM))

Speaker Deck summary says Erjang’s JIT compiler translates BEAM bytecode to Java/JVM bytecode and worked with contemporary Erlang/OTP and Elixir versions at the time. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))

## 6.3 Architecture choices

### Term representation

Erjang represented Erlang values as Java objects. Speaker Deck transcript references immutable/persistent data and “Erlang → JVM” code generation. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))

### Process model

Erjang mapped Erlang processes/messaging onto JVM constructs. Speaker Deck lists:

- Erlang process + messaging,
- coroutine + mailbox,
- Kilim,
- tail calls,
- trampoline,
- state encapsulation. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))

### Scheduling

Because JVM does not natively provide BEAM reductions/processes, Erjang needed coroutine/trampoline machinery and Kilim-style pausable calls.

### GC

This was a major mismatch. Speaker Deck lists JVM cons for Erlang:

- hard to encode Erlang process model,
- garbage collection is global. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))

This contrasts with BEAM’s per-process heaps and per-process GC.

### Message passing

Speaker Deck benchmarks covered small/medium/huge messages and ring messaging. Erjang could be fast after JVM warmup on some workloads, but message/process semantics had to be rebuilt over Java. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))

### Bytecode loading

Erjang loaded BEAM files, performed analysis/type inference, generated JVM code, and emitted `.jar`/classes. Speaker Deck transcript shows:

- BEAM reader,
- analysis,
- JVM codegen,
- `foo.beam` → `foo-...jar`,
- ClassLoader integration. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))

## 6.4 What it did well

- Leveraged JVM JIT, GC engineering, libraries, and ecosystem.
- Demonstrated Erlang could be hosted on another mature VM.
- Java interop was a clear differentiator.
- Some workloads were faster than classic BEAM, especially arithmetic/floating-point style workloads in the historical benchmarks. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))

## 6.5 What failed or was abandoned

- JVM bytecode verification and method-size limits are awkward for arbitrary Erlang code. Speaker Deck notes all code must pass load-time type checking and functions are limited to 64k bytecode size. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))
- Erlang process model is not easy to encode on JVM. ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))
- JVM global GC violates one of BEAM’s core latency advantages.
- Keeping up with Erlang/OTP evolution is a long-term maintenance burden.
- Community adoption remained low compared with standard OTP.

## 6.6 Lessons for beamr

1. **A host VM gives you JIT and tooling, but may fight BEAM semantics.**
2. **Per-process GC is not optional if BEAM-like latency is a goal.**
3. **Tail calls, reductions, selective receive, and process-local heaps are semantic, not implementation details.**
4. **Interop is attractive but cannot compensate for semantic mismatch.**

---

# 7. GraalVM / Truffle-based BEAM implementations

## 7.1 Status

I found no significant maintained open-source Truffle-based BEAM/Erlang implementation.

GraalVM’s own language implementation list includes languages such as Espresso, FastR, Graal.js, GraalPy, Sulong, TruffleRuby, etc., but not Erlang/BEAM. ([graalvm.org](https://www.graalvm.org/dev/graalvm-as-a-platform/language-implementation-framework/Languages/?utm_source=openai))

## 7.2 Goal if attempted

A Truffle BEAM would likely aim to:

- interpret BEAM bytecode or Core Erlang as AST nodes,
- let Graal partially evaluate/JIT hot paths,
- reuse GraalVM instrumentation/profiling,
- possibly use Native Image for deployment.

## 7.3 Architecture implications

### Term representation

Likely Java objects or Truffle-specialized values. This risks allocation overhead unless aggressively specialized.

### Process model

Truffle does not provide Erlang processes, mailboxes, reductions, links, monitors, or OTP semantics. These must be implemented separately.

### Scheduling

A BEAM-on-Truffle implementation would need a custom scheduler on top of JVM threads or continuations. Truffle helps optimize language execution, not actor scheduling.

### GC

Same issue as Erjang: JVM/Graal host GC is global/shared-heap, whereas BEAM’s latency model relies on per-process heaps and per-process GC.

### Message passing

Would need deep runtime support for mailbox copying, selective receive, and binary sharing.

### Bytecode loading

Could implement BEAM loader and Truffle AST generation or bytecode interpreter nodes, but dynamic code loading and hot upgrades would remain difficult.

## 7.4 What it could do well

- Great tooling/instrumentation.
- JIT from a high-level interpreter.
- Easier language implementation than hand-writing a native JIT.
- Polyglot interop.

## 7.5 Why no major BEAM/Truffle success appears

The core value of BEAM is not just dynamic language execution speed. It is the combination of:

- lightweight isolated processes,
- per-process GC,
- reductions/preemption,
- mailbox/selective receive,
- links/monitors,
- distribution,
- hot code loading,
- OTP libraries.

Truffle helps mostly with execution optimization. It does not remove the hard ERTS work.

## 7.6 Lessons for beamr

1. **Do not confuse “JIT framework” with “BEAM runtime framework.”**
2. **If using a host VM, prove process/GC/scheduler semantics first, not arithmetic speed.**
3. **Instrumentation is valuable; copy Graal/Truffle’s observability mindset even if not its architecture.**

---

# 8. HiPE, BEAMJIT, and BeamAsm

These are not replacement VMs, but they are essential because they show what improvements succeeded *inside* BEAM.

## 8.1 HiPE

HiPE was a native-code compiler for Erlang/OTP. The “High Performance Erlang Performance Deconstructed” paper evaluated native-code Erlang against interpreted/emulated Erlang and Haskell, focusing on full applications rather than isolated modules. ([paperswelove.org](https://paperswelove.org/papers/high-performance-erlang-hipe-performance-deconstru-0346b725/?utm_source=openai))

HiPE ultimately lost ground; OTP moved toward BeamAsm JIT instead. Mailing-list discussion around OTP 24 noted HiPE had not been fully functional since OTP 22 and that a new JIT path was planned. ([erlang.org](https://erlang.org/pipermail/erlang-questions/2020-June/099685.html?utm_source=openai))

### Lesson

A native compiler bolted onto a fast-evolving VM is hard to maintain unless it shares most runtime machinery and code-loading semantics.

## 8.2 BEAMJIT

BEAMJIT was a research JIT for Erlang. A listed paper says it was a just-in-time compiling runtime for Erlang and did not yet match HiPE performance. ([ri.diva-portal.org](https://ri.diva-portal.org/smash/get/diva2%3A1043459/FULLTEXT01.pdf?utm_source=openai))

### Lesson

JITs must preserve BEAM semantics, debugging, tracing, loader behavior, and reductions. Raw codegen is the easy part.

## 8.3 BeamAsm

BeamAsm is the modern Erlang JIT. Official docs say it performs load-time conversion of BEAM instructions into native code on x86-64 and aarch64, eliminating instruction dispatch overhead and specializing instructions on argument types. It does little cross-instruction optimization, and `x`/`y` register arrays work like the interpreter, keeping ERTS largely unchanged except loader/tracing/code-related places. ([erlang.org](https://www.erlang.org/docs/26/apps/erts/beamasm?utm_source=openai))

It uses asmjit at runtime. Code loading remains similar to interpreter loading. ([erlang.org](https://www.erlang.org/docs/26/apps/erts/beamasm?utm_source=openai))

### Lesson

The successful JIT strategy was conservative:

- preserve BEAM register model,
- preserve runtime architecture,
- specialize/load-time compile instructions,
- avoid huge semantic rewrites.

For `beamr`, this suggests starting with an interpreter/loader whose semantics are correct, then adding low-risk specialization.

---

# 9. Known BEAM limitations and bottlenecks

## 9.1 Raw numeric and CPU-bound performance

Historically, BEAM was slower than native code for arithmetic because interpreted BEAM instructions must access VM registers in memory, perform operation, write back, dispatch next instruction, etc. A Stack Overflow answer summarized this pre-OTP-24 limitation and noted native compilers can use CPU registers, pipeline optimizations, vectorization, and cache-aware optimizations more directly. ([stackoverflow.com](https://stackoverflow.com/questions/65328475/why-is-math-so-slow-in-erlang-the-beam-vm?utm_source=openai))

BeamAsm mitigates dispatch overhead but intentionally does little cross-instruction optimization. ([erlang.org](https://www.erlang.org/docs/26/apps/erts/beamasm?utm_source=openai))

### Lesson for beamr

If CPU-bound numeric performance matters, a BEAM-like interpreter will not be enough. But optimizing arithmetic first is the wrong priority unless `beamr` targets numeric workloads.

## 9.2 Message fanout cost

Discord’s 2017 scaling post is one of the clearest real-world bottleneck reports. They found that sending messages between Erlang processes was more expensive than expected; `send/2` could take 30–70µs wall-clock due to descheduling, and a large guild fanout could take 900ms–2.1s because a GenServer is effectively single-threaded. ([medium.com](https://medium.com/discord-engineering/scaling-elixir-f9b8e1e7c29b))

Discord solved this not by changing BEAM, but by architectural sharding/fanout distribution via Manifold: group PIDs by remote node and distribute sending work. ([medium.com](https://medium.com/discord-engineering/scaling-elixir-f9b8e1e7c29b))

### Lesson for beamr

- Message passing must be benchmarked as a first-class primitive.
- Single actor fanout is a known bottleneck.
- Runtime should expose fanout diagnostics and maybe offer first-class fanout/broadcast primitives.

## 9.3 Single-process bottlenecks

BEAM processes are lightweight but single-threaded. A hot GenServer can become a bottleneck. Discord’s guild process example is the canonical case. ([medium.com](https://medium.com/discord-engineering/scaling-elixir-f9b8e1e7c29b))

### Lesson for beamr

The runtime should make hot mailboxes and long-running process reductions visible. Consider scheduler-level telemetry for:

- mailbox length,
- time since last receive,
- reductions per process,
- fanout cost,
- per-process GC time,
- scheduler migrations.

## 9.4 Back-pressure and load shedding are not automatic

Discord’s push-notification system hit bursts over a million per minute. The bottleneck was Firebase delivery, not BEAM itself, but the BEAM system needed explicit back-pressure and load shedding via GenStage. They bounded pending requests and dropped notifications when buffers filled. ([discord.com](https://discord.com/blog/how-discord-handles-push-request-bursts-of-over-a-million-per-minute-with-elixirs-genstage))

### Lesson for beamr

Do not market actors as automatic scalability. Runtime primitives should support back-pressure, bounded mailboxes, load shedding, and telemetry.

## 9.5 NIF scheduler blocking

Long-running NIFs block scheduler threads. A common rule is that regular NIFs should complete very quickly, often under ~1ms; otherwise they should use dirty schedulers, yielding NIFs, or external threads. ([hexdocs.pm](https://hexdocs.pm/c3nif/dirty-schedulers.html?utm_source=openai))

Stack Overflow’s explanation is direct: BEAM processes are not OS threads; with roughly one scheduler per core, a blocking C call blocks the scheduler that ran it. ([stackoverflow.com](https://stackoverflow.com/questions/18178542/why-does-the-nif-function-block-the-erlang-vm-from-scheduling-other-processes?utm_source=openai))

### Lesson for beamr

FFI/NIF design is critical:

- unsafe foreign code must not block normal schedulers,
- dirty scheduler model or WASM sandbox model should be designed early,
- runtime should enforce or measure NIF execution time.

Lunatic’s WASM-per-process design is a strong alternative for safe native-like extensions.

## 9.6 Binary memory retention

BEAM and AtomVM both optimize large binaries through reference-counted off-heap storage. This reduces copying but can retain large binaries if small sub-binaries or references survive.

AtomVM docs show the same design tradeoff: non-const binaries are off-heap, dynamically allocated, and reference-counted so large blocks can be shared between processes by copying a small term. ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))

Erlang’s official GC docs discuss message allocation strategies and binary memory not being released soon enough. ([erlang.org](https://www.erlang.org/doc/apps/erts/garbagecollection.html?utm_source=openai))

### Lesson for beamr

Binary lifetime must be observable. Provide tooling to answer:

- which processes hold binary refs,
- refcount and owner info,
- sub-binary retention,
- mailbox-held binary memory,
- per-process off-heap binary footprint.

## 9.7 Memory footprint and allocator behavior

BEAM is optimized for latency/server workloads, often using allocator strategies that reserve memory aggressively. Embedded users often tune allocators/schedulers to reduce idle memory. A 2026 embedded Phoenix writeup reports large memory reductions by tuning BEAM allocators and scheduler behavior. ([mrpopov.com](https://mrpopov.com/posts/elixir-beam-vm-embedded-optimisation/?utm_source=openai))

AtomVM explicitly optimizes for lower RAM and accepts slower speed/more frequent GC. ([doc.atomvm.org](https://doc.atomvm.org/main/differences-with-beam.html))

### Lesson for beamr

Expose memory policy as configuration:

- server/high-throughput profile,
- embedded/low-RAM profile,
- deterministic/low-latency profile,
- debug/diagnostic profile.

## 9.8 Distributed Erlang scaling

Large BEAM systems often avoid naïve full-mesh distributed Erlang at high node counts. Community discussions commonly report that default distributed Erlang becomes problematic around 100–200 nodes depending on workload. ([reddit.com](https://www.reddit.com/r/elixir/comments/gfi7xl?utm_source=openai))

Discord used consistent hashing and custom fanout distribution rather than assuming distribution primitives alone solved scale. ([medium.com](https://medium.com/discord-engineering/scaling-elixir-f9b8e1e7c29b))

### Lesson for beamr

Do not treat distributed Erlang compatibility as a first milestone. If distribution is in scope, design explicit topology, back-pressure, serialization, and failure semantics.

## 9.9 Observability overhead

Discord’s 2026 tracing post shows that adding tracing to message-passing systems can itself become a bottleneck. Instrumenting busy guilds caused performance issues; stacktrace sampling showed trace-context unpacking overhead. Later, session fanout tracing increased CPU usage by 10 percentage points, and gRPC trace context extraction doubled CPU usage until optimized. ([discord.com](https://discord.com/blog/tracing-discords-elixir-systems-without-melting-everything))

### Lesson for beamr

Observability must be cheap, sampled, and scheduler-aware. Runtime tracing should avoid per-message heavy metadata work by default.

---

# 10. Cross-project architecture lessons for beamr

## 10.1 Define the compatibility target precisely

Bad target:

> “A BEAM replacement.”

Good targets:

- “Loads a restricted `.beam` subset with chunks X/Y/Z.”
- “Executes opcodes needed for simple Erlang modules compiled by OTP N.”
- “Supports local processes, mailboxes, selective receive, links, monitors.”
- “No distribution, hot loading, tracing, ports, or NIFs in milestone 1.”
- “Supports binaries but not bignums initially.”
- “Supports maps but not full map optimization.”

AtomVM succeeds by being explicit about subset compatibility. Firefly’s broader target likely contributed to collapse.

## 10.2 Treat ERTS as bigger than BEAM bytecode

A real BEAM-compatible runtime needs:

- bytecode loader and validator,
- atoms,
- tagged terms,
- process heaps,
- scheduler reductions,
- selective receive,
- timers,
- ports,
- NIFs,
- ETS,
- code server,
- distribution,
- tracing,
- error handling,
- stacktraces,
- standard library assumptions,
- OTP behavior assumptions.

Projects that underestimated this either stalled or became narrow subsets.

## 10.3 Per-process heap/GC is central

BEAM’s latency and actor model depend on process-local heaps and per-process GC. Erjang’s JVM/global-GC mismatch is a cautionary tale. AtomVM’s process-local memory model is a good reference.

## 10.4 Message passing is the runtime’s hot path

Discord’s `send/2` experience shows message passing is not “free enough.” For `beamr`, implement early benchmarks for:

- small immediate messages,
- tuples/lists,
- large binaries,
- local fanout,
- remote-like fanout serialization,
- selective receive scanning,
- mailbox growth,
- mailbox GC interaction.

## 10.5 Scheduler design must include foreign code

NIF blocking is one of BEAM’s sharpest edges. `beamr` should decide early:

- dirty schedulers,
- yielding FFI,
- async worker pool,
- WASM sandboxed foreign functions,
- hard execution budgets,
- killable foreign processes.

## 10.6 Dynamic code loading and hot upgrades are massive complexity multipliers

Firefly explicitly excluded them. AtomVM avoids much of their complexity through packbeam. WASM deployment models also dislike dynamic code loading.

Recommendation: exclude initially.

## 10.7 Specification and docs are not optional

LAM’s “BEAM implementation is the spec” critique is valid. AtomVM’s docs are excellent because they describe term layout, GC, message representation, and packaging.

`beamr` should maintain:

- term representation spec,
- bytecode subset spec,
- scheduler semantics,
- mailbox semantics,
- GC invariants,
- FFI safety rules,
- compatibility matrix.

## 10.8 Observability must be built into the runtime

Discord’s scaling lessons repeatedly depended on instrumentation, stacktraces, profiling, and custom metrics. But tracing overhead can melt hot paths.

Build cheap counters first:

- scheduler utilization,
- reductions,
- run queue length,
- mailbox length,
- per-process heap/off-heap binary memory,
- GC count/time/bytes copied,
- message send count/bytes,
- process migration/stealing,
- FFI/dirty time.

---

# 11. Recommended design stance for beamr

## 11.1 Avoid

- Full OTP replacement as initial goal.
- Hot code loading initially.
- Distributed Erlang initially.
- NIF ABI compatibility initially.
- LLVM/WASM/native codegen before interpreter correctness.
- Host-VM dependency that prevents per-process GC/scheduling semantics.
- Unbounded mailboxes without telemetry/back-pressure options.

## 11.2 Emulate

From **AtomVM**:

- explicit subset,
- documented term layout,
- per-process heap/GC,
- packbeam-like packaging,
- honest limitations.

From **Lunatic**:

- capability-based process permissions,
- sandboxed foreign code,
- strong process isolation,
- supervision concepts independent of BEAM bytecode.

From **BeamAsm**:

- preserve runtime semantics,
- optimize at load time later,
- avoid overambitious global optimization early.

From **Discord lessons**:

- design for fanout,
- expose hot process/mailbox metrics,
- support back-pressure/load shedding patterns,
- keep tracing cheap.

From **Firefly**:

- static executable/WASM deployment is valuable,
- but scope must be constrained brutally.

## 11.3 Suggested milestone ladder

### Milestone 0 — spec and harness

- Define term representation.
- Define supported BEAM chunks.
- Define supported opcodes.
- Define process/mailbox semantics.
- Build trace/debug output before performance work.

### Milestone 1 — single-process BEAM subset

- Load simple BEAM modules.
- Execute arithmetic, calls, tuples, lists, atoms, pattern matching.
- No concurrency yet except root process.

### Milestone 2 — local processes

- Spawn.
- PIDs.
- Mailboxes.
- Send/receive.
- Selective receive subset.
- Reductions and preemption.

### Milestone 3 — per-process memory

- Process heap.
- Copying GC.
- Message heap fragments.
- Off-heap binaries/refcounting.
- GC telemetry.

### Milestone 4 — supervision primitives

- Links.
- Monitors.
- Exit signals.
- Timers.
- Minimal OTP compatibility shim.

### Milestone 5 — packaging and embedded profile

- Packbeam-like bundle.
- Static module table.
- No dynamic loading.
- Memory profile knobs.

### Milestone 6 — foreign code story

Choose one:

- dirty scheduler NIF model,
- async workers,
- WASM sandboxed extensions,
- ports-only.

### Milestone 7 — optimization

- Loader specialization.
- Direct-threaded interpreter or threaded dispatch.
- Optional JIT/AOT only after semantics stabilize.

---

# 12. Source index

- Firefly GitHub README / archive status / goals / architecture: ([github.com](https://github.com/GetFirefly/firefly?utm_source=openai)) ([github.com](https://github.com/GetFirefly/firefly))
- Firefly website positioning: ([getfirefly.org](https://getfirefly.org/?utm_source=openai))
- AtomVM README / status / platforms / limitations: ([github.com](https://github.com/atomvm/AtomVM))
- AtomVM memory management / term representation / GC / message representation: ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html)) ([doc.atomvm.org](https://doc.atomvm.org/latest/memory-management.html))
- AtomVM internals / WASM port: ([doc.atomvm.org](https://doc.atomvm.org/v0.6.6/atomvm-internals.html))
- AtomVM packbeam format: ([doc.atomvm.org](https://doc.atomvm.org/latest/packbeam-format.html))
- AtomVM differences from BEAM: ([doc.atomvm.org](https://doc.atomvm.org/main/differences-with-beam.html))
- Lunatic README / architecture / process isolation / scheduling: ([github.com](https://github.com/lunatic-solutions/lunatic))
- Lunatic author discussion comparing with Lumen/LAM: ([elixirforum.com](https://elixirforum.com/t/lunatic-actor-based-webassembly-runtime-for-the-backend/35617?utm_source=openai))
- LAM interview / architecture / GC intent / scheduling: ([blog.lambdaclass.com](https://blog.lambdaclass.com/lam-an-actor-model-vm-for-webassembly-and-native/))
- LAM activity summary: ([libhunt.com](https://www.libhunt.com/compare-leostera--lam-vs-firefly?utm_source=openai))
- Erlang/BEAM languages in WASM, Fermyon: ([developer.fermyon.com](https://developer.fermyon.com/wasm-languages/erlang-beam?utm_source=openai))
- Popcorn AtomVM-in-WASM positioning: ([popcorn.swmansion.com](https://popcorn.swmansion.com/?utm_source=openai))
- Firebird subset-to-WASM docs: ([hexdocs.pm](https://hexdocs.pm/firebird/Firebird.Compiler.html?utm_source=openai))
- Orb subset/tiny WASM positioning: ([useorb.dev](https://useorb.dev/?utm_source=openai))
- Erjang JVM Language Summit abstract: ([wiki.jvmlangsummit.com](https://wiki.jvmlangsummit.com/Erjang_-_A_JVM-based_Erlang_VM))
- Erjang InfoQ summary: ([infoq.com](https://www.infoq.com/presentations/erjang-erlang-vm-jvm/))
- Erjang Speaker Deck transcript: ([speakerdeck.com](https://speakerdeck.com/krestenkrab/erjang-inside-erlang-on-the-jvm))
- GraalVM language implementation list: ([graalvm.org](https://www.graalvm.org/dev/graalvm-as-a-platform/language-implementation-framework/Languages/?utm_source=openai))
- BeamAsm official docs: ([erlang.org](https://www.erlang.org/docs/26/apps/erts/beamasm?utm_source=openai))
- HiPE performance paper summary: ([paperswelove.org](https://paperswelove.org/papers/high-performance-erlang-hipe-performance-deconstru-0346b725/?utm_source=openai))
- HiPE removal discussion: ([erlang.org](https://erlang.org/pipermail/erlang-questions/2020-June/099685.html?utm_source=openai))
- BEAMJIT paper listing: ([ri.diva-portal.org](https://ri.diva-portal.org/smash/get/diva2%3A1043459/FULLTEXT01.pdf?utm_source=openai))
- BEAM arithmetic/JIT limitation discussion: ([stackoverflow.com](https://stackoverflow.com/questions/65328475/why-is-math-so-slow-in-erlang-the-beam-vm?utm_source=openai))
- Erlang GC docs / message allocation and binaries: ([erlang.org](https://www.erlang.org/doc/apps/erts/garbagecollection.html?utm_source=openai))
- NIF scheduler blocking: ([hexdocs.pm](https://hexdocs.pm/c3nif/dirty-schedulers.html?utm_source=openai)) ([stackoverflow.com](https://stackoverflow.com/questions/18178542/why-does-the-nif-function-block-the-erlang-vm-from-scheduling-other-processes?utm_source=openai))
- Discord 5M concurrent users / fanout bottleneck: ([medium.com](https://medium.com/discord-engineering/scaling-elixir-f9b8e1e7c29b))
- Discord GenStage back-pressure/load shedding: ([discord.com](https://discord.com/blog/how-discord-handles-push-request-bursts-of-over-a-million-per-minute-with-elixirs-genstage))
- Discord tracing overhead: ([discord.com](https://discord.com/blog/tracing-discords-elixir-systems-without-melting-everything))
