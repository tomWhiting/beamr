# beamr Brief Tracker

Current version: **0.3.15** | Tests: **726** | Published: 2026-06-07

## Team

| Member | Role | Current Focus |
|--------|------|---------------|
| Bono | Quality gate / coordinator | Phase 0.5 landing, Phase 1 planning |
| Dave Evans | Brief writer | Phase 1 briefs (assigned) |
| Adam Clayton | Design doc writer | Phase 1 design + briefs (assigned) |
| Larry Mullen Jr | Reviewer | Phase 1 brief reviews (ready) |

## Phase 0 -- Foundation (COMPLETE)

All briefs landed on main. 45 briefs, v0.1.0 through v0.3.12.

| Brief | Title | Status |
|-------|-------|--------|
| B-001 | Term representation and tagging | Landed |
| B-002 | Atom table and interning | Landed |
| B-003 | Process struct and X/Y registers | Landed |
| B-004 | BEAM loader -- header and atom chunk | Landed |
| B-005 | BEAM loader -- code chunk decoding | Landed |
| B-006 | BEAM loader -- import/export tables | Landed |
| B-007 | Interpreter loop and basic dispatch | Landed |
| B-008 | Arithmetic and comparison BIFs | Landed |
| B-009 | Tuple operations | Landed |
| B-010 | List operations (cons, hd, tl) | Landed |
| B-011 | Pattern matching (select_val, test_arity) | Landed |
| B-012 | Function calls (call, call_only, return) | Landed |
| B-013 | External calls and import resolution | Landed |
| B-014 | Allocate/deallocate stack frames | Landed |
| B-015 | Exception handling (try/catch) | Landed |
| B-016 | Process spawning and PID terms | Landed |
| B-017 | Scheduler -- multi-threaded run loop | Landed |
| B-018 | Message passing (send/receive) | Landed |
| B-019 | Selective receive with save pointer | Landed |
| B-020 | Links and exit signals | Landed |
| B-021 | Monitors and DOWN messages | Landed |
| B-022 | Trap exit and EXIT messages | Landed |
| B-023a | Binary term representation | Landed |
| B-023b | Binary construction (bs_init, bs_put) | Landed |
| B-023c | Binary matching (bs_start_match, bs_get) | Landed |
| B-024 | Map operations | Landed |
| B-025 | Closures and make_fun2 | Landed |
| B-026 | Float terms and boxing | Landed |
| B-027 | Big integer support | Landed |
| B-028 | Reference terms | Landed |
| B-029 | Generational garbage collection | Landed |
| B-030 | Timer wheel and receive timeouts | Landed |
| B-031 | Process registration (register/whereis) | Landed |
| B-032 | Work stealing scheduler | Landed |
| B-033 | Gleam stdlib stubs | Landed |
| B-034 | Gleam FFI (gleam_erlang_ffi) | Landed |
| B-035 | Selector FFI | Landed |
| B-036 | OTP stubs | Landed |
| B-037 | Meridian FFI (NIF bridge) | Landed |
| B-038 | apply/apply_last opcodes | Landed |
| B-039 | beamr-cli runner | Landed |
| B-040 | E2E Gleam workflow proof | Landed |
| B-040a | Gate3 BIFs | Landed |
| B-040b | Code management BIFs | Landed |
| B-041 | Dual module versions with purge | Landed |
| B-042 | Closure version binding | Landed |
| B-043 | Dynamic import resolution | Landed |
| B-044 | Process module version pinning | Landed |
| B-045 | Hot code loading lifecycle | Landed |

## Phase 0.5 -- Hardening (IN PROGRESS)

Bug fixes, hardening, namespace isolation, and memory safety.

| Brief | Title | Status | Issue |
|-------|-------|--------|-------|
| B-046 | Atom term comparison by name, not intern index | **Landed** | #13 |
| B-047 | Ordered link/monitor sets for deterministic exit propagation | **Landed** | #10 |
| B-048 | Loader validating-boundary with BoundedCursor and DecodeBudget | **Landed** | #11 |
| B-049 | Capability-based native imports with deny-by-default policy | **Landed** | #9 |
| B-050 | Per-module constant pool for literals | **Landed** | #12 |
| B-051 | Process-heap allocation for BIF/NIF results | On norn | #12 |
| B-052 | Box::leak audit, removal, and CI prevention gate | Blocked (B-051) | #12 |
| B-053 | Namespace infrastructure (per-namespace module registries) | **Landed** | -- |
| B-054 | Namespace-aware interpreter | **Landed** | -- |
| B-055 | Process slot sentinel (metadata during scheduler execution) | **Landed** | -- |

### Bug fixes landed without briefs (v0.3.2--v0.3.15)

| Version | Fix |
|---------|-----|
| 0.3.2 | init_yregs opcode, executable_line no-op |
| 0.3.3 | JSON decode/encode BIFs (feature-gated) |
| 0.3.4 | Badarg context preservation |
| 0.3.5 | X register widening (256 to 1024, u8 to u16) |
| 0.3.6 | Register::X enum u8 to u16 |
| 0.3.7 | try_case writes to x(0-2) per BEAM spec |
| 0.3.8 | JSON null returns atom 'null' not NIL |
| 0.3.9 | JSON object binary keys (OTP 27 compat) |
| 0.3.10 | take_exit_exception API for error diagnostics |
| 0.3.11 | classify_dynamic returns binaries (Gleam compat) |
| 0.3.12 | call_fun2 full closure dispatch |
| 0.3.13 | Exit signal race fix (tombstone for body-taken processes) |
| 0.3.14 | Three additional body-taken races (spawn_link, take_links, pending_links) |
| 0.3.15 | ProcessSlot sentinel, namespace isolation, capability-gated imports |

## Phase 1 -- Critical Opcode/BIF Gaps (COMPLETE)

All 16 briefs written, reviewed, and cleared for dispatch.

| Brief | Title | Priority | Assign | Status |
|-------|-------|----------|--------|--------|
| B-056 | get_list opcode -- list destructuring | CRITICAL | Dave | **Ready** |
| B-057 | trim opcode -- stack frame trimming in tail calls | CRITICAL | Dave | **Ready** |
| B-058 | swap opcode -- register swap (OTP 27+) | CRITICAL | Dave | **Ready** |
| B-059 | catch/catch_end -- traditional exception handling | CRITICAL | Adam | **Ready** |
| B-060 | erlang:throw/1 -- throw exception type | CRITICAL | Dave | **Ready** |
| B-061 | build_stacktrace -- stack trace construction | HIGH | Adam | **Ready** |
| B-062 | raw_raise -- re-raise exceptions with class/reason/trace | HIGH | Adam | **Ready** |
| B-063 | is_tagged_tuple -- record pattern matching | HIGH | Dave | **Ready** |
| B-064 | Float arithmetic instructions (7 opcodes: 96-102) | HIGH | Dave | **Ready** |
| B-065 | update_record -- record update syntax | HIGH | Dave | **Ready** |
| B-066 | Numeric equality and ordering (==, /=, >, =<) | HIGH | Adam | **Ready** |
| B-067 | Process dictionary (put/get/erase) | HIGH | Adam | **Ready** |
| B-068 | Binary ops expansion (bs_skip, bs_get_float, UTF, bs_match) | HIGH | Adam | **Ready** |
| B-069 | Tail-call BIF return fix (call_ext_only + Native) | MEDIUM | Dave | **Ready** |
| B-070 | recv_marker opcodes (173-176, OTP 24+ selective receive) | MEDIUM | Dave | **Ready** |
| B-071 | Scheduler module splitting (mod.rs over 500 lines) | MEDIUM | Adam | **Ready** |

## Phase 2 -- Platform (30/40 REVIEWED — IO block writing)

Makes OTP libraries work. 40 briefs. 30 reviewed and cleared, IO block (12 briefs) being written by Adam.

### 2a. Refc Binaries (B-072 -- B-076)

| Brief | Title | Depends on |
|-------|-------|------------|
| B-072 | Shared binary heap and ProcBin (off-heap alloc, Arc<SharedBinary>) | -- | **Reviewed** |
| B-073 | Binary size threshold and automatic promotion (<=64 inline, >64 refc) | B-072 | **Reviewed** |
| B-074 | Sub-binary term type (offset+length into parent binary) | B-072 | **Reviewed** |
| B-075 | GC integration for refc binaries (ProcBin sweep, virtual binary heap) | B-072, B-074 | **Reviewed** |
| B-076 | Binary append optimization (in-place append when refcount==1) | B-072 | **Reviewed** |

### 2b. Dirty Schedulers (B-077 -- B-079)

| Brief | Title | Depends on |
|-------|-------|------------|
| B-077 | Dirty scheduler thread pools (CPU + IO pools, work queues) | -- | At Larry |
| B-078 | Dirty NIF dispatch and process migration (suspend, execute, resume) | B-077 | At Larry |
| B-079 | Dirty NIF registration API and scheduling wrapper | B-078 | At Larry |

### 2c. Process Features (B-080 -- B-085)

| Brief | Title | Depends on |
|-------|-------|------------|
| B-080 | process_info/1,2 BIF (all process attribute items) | -- | At Larry |
| B-081 | system_info/1 BIF (VM introspection keys) | -- | At Larry |
| B-082 | Group leader read/write BIFs (erlang:group_leader/0,2) | -- | At Larry |
| B-083 | Process priorities (low/normal/high/max scheduling) | -- | At Larry |
| B-084 | spawn_monitor/1,3 (atomic spawn + monitor) | -- | At Larry |
| B-085 | spawn_opt/2,4 (link, monitor, priority, heap options) | B-083 | At Larry |

### 2d. External Term Format (B-086 -- B-089)

| Brief | Title | Depends on |
|-------|-------|------------|
| B-086 | term_to_binary encoder (all term types to ETF wire format) | -- | At Larry |
| B-087 | binary_to_term decoder (ETF to runtime Terms on process heap) | -- | At Larry |
| B-088 | term_to_binary options and compression (zlib, safe mode) | B-086, B-087 | At Larry |
| B-089 | term_to_iovec scatter-gather encoding (avoid copying refc binaries) | B-086, B-072 | At Larry |

### 2e. ETS (B-090 -- B-099)

| Brief | Title | Depends on |
|-------|-------|------------|
| B-090 | ETS table registry and ownership model (table IDs, access protection) | -- |
| B-091 | Set table type (hash-based, O(1) insert/lookup/delete) | B-090 |
| B-092 | Bag and duplicate_bag table types | B-091 |
| B-093 | Ordered_set table type (B-tree, term ordering on keys) | B-090 |
| B-094 | Core ETS BIFs (ets:new/insert/lookup/delete/member/info) | B-091, B-093 |
| B-095 | ETS iteration and folding (tab2list, foldl, first/next/last/prev) | B-094 |
| B-096 | Match specification compiler (pattern variables, guard BIFs) | B-094 |
| B-097 | ets:match and ets:select (compiled match specs, continuations) | B-096 |
| B-098 | ETS concurrent access (read_concurrency, write_concurrency, striped locks) | B-094 |
| B-099 | ETS ownership transfer (heir, give_away, ETS-TRANSFER message) | B-094 |

### 2f. Port/IO on io_uring (B-100 -- B-111)

Modern async I/O replacing BEAM's legacy port driver model. io_uring is Linux-only; needs platform-abstracted completion IO trait for macOS dev.

| Brief | Title | Depends on |
|-------|-------|------------|
| B-100 | io_uring ring abstraction (SQE/CQE, dedicated thread) | -- |
| B-101 | Async completion bridge (CQE to process wakeup) | B-100, B-077 |
| B-102 | File descriptor resource type (boxed term, refcounted FD lifecycle) | B-101 |
| B-103 | File open/close/read/write BIFs (io_uring ops, process suspension) | B-102 |
| B-104 | File seek, pread, pwrite (positional IO) | B-103 |
| B-105 | File metadata operations (stat, list_dir, mkdir, delete, rename) | B-103 |
| B-106 | TCP listener (socket accept loop, io_uring multishot accept) | B-102 |
| B-107 | TCP client connect and data transfer (connect, send, recv) | B-106 |
| B-108 | TCP active/passive mode and controlling process | B-107 |
| B-109 | UDP socket operations (sendmsg, recvmsg, active/passive) | B-102 |
| B-110 | inet BIF surface (setopts, getopts, peername, sockname) | B-107, B-109 |
| B-111 | Standard I/O and group_leader integration (io:format, io:get_line) | B-103, B-082 |

## Phase 3 -- Full Replacement (NOT STARTED)

Makes Elixir work, enables clustering. 18 briefs.

### 3a. Distribution Protocol (B-112 -- B-125)

| Brief | Title | Depends on |
|-------|-------|------------|
| B-112 | Node naming and node term type (node-qualified PIDs, node/0) | -- |
| B-113 | Name resolution service (pluggable resolver replacing EPMD) | B-112 |
| B-114 | Distribution connection manager (TCP, connection table, reconnect) | B-113 |
| B-115 | Distribution handshake protocol (challenge-response, cookies, flags) | B-114 |
| B-116 | Distribution atom cache (per-connection, short index encoding) | B-115 |
| B-117 | Distribution message encoding (ETF wire format for all term types) | B-116, B-086 |
| B-118 | Remote message send (Pid ! Msg across nodes) | B-117 |
| B-119 | Distribution control messages (LINK, EXIT, MONITOR, SEND, etc.) | B-118 |
| B-120 | Remote spawn (SPAWN_REQUEST/REPLY across nodes) | B-119 |
| B-121 | Remote monitors (MONITOR_P/DEMONITOR_P/MONITOR_P_EXIT) | B-119 |
| B-122 | Remote links (LINK/UNLINK/EXIT across nodes) | B-119 |
| B-123 | Net_kernel and connection supervision (connect_node, nodes/0) | B-119 |
| B-124 | Global name registration (global:register_name, conflict resolution) | B-123 |
| B-125 | Process groups -- pg (scope-based groups, cross-node propagation) | B-123 |

### 3b. Full BIF Coverage (B-126 -- B-129)

Remaining erlang module BIFs not covered by earlier phases.

| Brief | Title | Depends on |
|-------|-------|------------|
| B-126 | erlang module BIFs batch 1 -- type conversion (list_to_atom, integer_to_list, etc.) | -- |
| B-127 | erlang module BIFs batch 2 -- list operations (lists module stubs) | -- |
| B-128 | erlang module BIFs batch 3 -- system and info (statistics, memory, ports) | B-081 |
| B-129 | erlang module BIFs batch 4 -- misc (phash2, unique_integer, monotonic_time) | -- |

## Phase 4 -- Beyond BEAM (NOT STARTED)

Innovations the original BEAM can't do. 25 briefs.

### 4a. JIT Compilation via Cranelift (B-130 -- B-138)

Modern register allocator, SIMD support, adaptive tiering -- vs HiPE's static compilation.

| Brief | Title | Depends on |
|-------|-------|------------|
| B-130 | JIT infrastructure -- Cranelift function builder and module context | -- |
| B-131 | Opcode-to-IR lowering for arithmetic and moves | B-130 |
| B-132 | Opcode-to-IR lowering for calls, returns, and stack frames | B-130 |
| B-133 | Opcode-to-IR lowering for pattern matching and guards | B-130 |
| B-134 | Term tagging in IR (pointer tags, type checks as branch conditions) | B-130 |
| B-135 | GC safepoints in JIT code (stack maps, root enumeration) | B-130 |
| B-136 | Adaptive tiering (interpreter -> JIT promotion based on call count) | B-130 |
| B-137 | JIT deoptimization (bail to interpreter on uncommon paths) | B-136 |
| B-138 | JIT code cache management (invalidation on hot code load) | B-136 |

### 4b. Deterministic Replay (B-139 -- B-143)

Record/replay debugging -- not possible in original BEAM.

| Brief | Title | Depends on |
|-------|-------|------------|
| B-139 | Replay event log format and recording infrastructure | -- |
| B-140 | Deterministic scheduler replay (fixed thread assignment, reduction counts) | B-139 |
| B-141 | Message ordering replay (causal ordering, Lamport timestamps) | B-139 |
| B-142 | Timer and timeout replay (virtual clock) | B-139 |
| B-143 | Replay debugger CLI (step, breakpoint, inspect at any point) | B-140, B-141 |

### 4c. WASM Target (B-144 -- B-148)

BEAM processes in browser/edge -- Rust compiles to WASM natively.

| Brief | Title | Depends on |
|-------|-------|------------|
| B-144 | WASM build target and platform abstraction (no_std core, alloc) | -- |
| B-145 | WASM scheduler (single-threaded cooperative, no atomics) | B-144 |
| B-146 | WASM IO bridge (JS interop for message passing, DOM access) | B-144 |
| B-147 | WASM binary size optimization (feature-gate heavy subsystems) | B-144 |
| B-148 | WASM streaming module loader (fetch .beam over network) | B-144 |

### 4d. Capability Security (B-149 -- B-151)

Per-process capability gates -- foundation already in B-049.

| Brief | Title | Depends on |
|-------|-------|------------|
| B-149 | Per-process capability sets (inherit on spawn, restrict dynamically) | B-049 |
| B-150 | Capability-aware BIF enforcement (check before execution) | B-149 |
| B-151 | Capability policy configuration (TOML/JSON policy files, audit log) | B-149 |

### 4e. Structured Observability (B-152 -- B-155)

OpenTelemetry-native tracing -- built-in, not bolted on.

| Brief | Title | Depends on |
|-------|-------|------------|
| B-152 | OpenTelemetry span integration (process lifecycle, message send/recv) | -- |
| B-153 | Reduction-level tracing (opcode execution, BIF calls) | B-152 |
| B-154 | Scheduler metrics exporter (run queue depth, steal count, utilization) | B-152 |
| B-155 | Trace filter and sampling (per-process, per-module, rate-limited) | B-152 |

## Summary

| Phase | Briefs | Range | Status |
|-------|--------|-------|--------|
| Phase 0 (Foundation) | 48 | B-001 -- B-045 | COMPLETE |
| Phase 0.5 (Hardening) | 10 | B-046 -- B-055 | 8 landed, 2 in progress |
| Phase 1 (Critical gaps) | 16 | B-056 -- B-071 | BRIEF WRITING |
| Phase 2 (Platform) | 40 | B-072 -- B-111 | NOT STARTED |
| Phase 3 (Full replacement) | 18 | B-112 -- B-129 | NOT STARTED |
| Phase 4 (Beyond BEAM) | 26 | B-130 -- B-155 | NOT STARTED |
| **Total** | **158** | | |

### Cross-cutting Dependencies

- Distribution (Phase 3) requires ETF (B-086/B-087) and refc binaries (B-072)
- IO (Phase 2f) requires dirty schedulers (B-077) for fallback ops
- ETS match specs (B-096) shares patterns with interpreter/pattern.rs
- Refc binary GC (B-075) modifies the GC walker -- one of the most sensitive subsystems
- JIT code cache (B-138) must invalidate on hot code load (B-041--B-045)
- WASM target (B-144) needs platform abstraction that also benefits io_uring (Linux-only)

## Open GitHub Issues

| Issue | Title | Severity | Brief | Status |
|-------|-------|----------|-------|--------|
| #9 | meridian_ffi capabilities | MEDIUM | B-049 | **Fixed** |
| #10 | Determinism/replay | LOW | B-047 | **Fixed** |
| #11 | Loader validation boundary | MEDIUM | B-048 | **Fixed** |
| #12 | Box::leak memory leak | HIGH | B-050/051/052 | In progress |
| #13 | Atom ordering by name | LOW | B-046 | **Fixed** |

## Known Bugs (not yet filed)

- **Tail-call BIF return**: `call_ext_only` with Native BIF target returns `Continue` instead of returning from function. Masked by flat code layout falling through to next function. Tracked as B-069.
- **Nested closure dispatch**: `json.parse` + `decode.string` decoder pipeline crashes with nested closures in `gleam@dynamic@decode`. Not blocking (Aion decodes JSON on Rust side).
- **trap_exit + executing process**: Can't check trap_exit flag when body is taken during execution. ProcessSlot sentinel (B-055) mitigates but full correctness needs a signal queue.
