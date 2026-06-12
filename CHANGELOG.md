# Changelog

## 0.6.0

### Correctness

- Off-heap (ProcBin) and sub-binary terms survive the whole BIF surface: `byte_size`/`bit_size`/`binary_part`/`is_bitstring`/`iolist_size`, `binary_to_term`, `code:load_binary` bytes, file/TCP/UDP byte and filename extraction, and the JSON bridge previously accepted only inline heap binaries (≤ 64 bytes) and raised `badarg` on anything larger — the cause of "binaries over 64 bytes kill a resumed workflow with bad argument". All now go through the representation-agnostic `BinaryRef` accessor. `byte_size`/`bit_size` additionally accept bs match contexts: OTP 26+ compilers emit the gc_bif on the reused match-context register for match tails (`<<_, Rest/binary>> = B, byte_size(Rest)`) instead of materializing the tail sub-binary.
- Message sends copy ProcBin terms by sharing their refcounted off-heap bytes and copy sub-binaries' visible ranges threshold-aware; both previously failed delivery with `InvalidBoxedTerm`.
- Published host suspension results (`Scheduler::wake_with_result`/`wake_with_result_for` and the IO-bridge completion seam) are deep-copied into owned storage at publish time and materialized on the owning process heap at slice-start apply — a boxed result term no longer points into publisher storage of foreign lifetime across the publish-to-apply window. Heap space is collected/grown before the apply copy on both the host and dirty completion paths, so arbitrarily large results cannot die on `HeapFull`.
- `call_ext_last` native tail calls are suspension-safe: the y-frame pop is deferred until a clean (non-dirty) native call completes, so a suspending native's wake re-execution no longer double-pops the stack — previously the eventual return landed at the caller's own call site with the result in x0, crashing with `bad function term {ok, ...}` whenever the suspending call's argument expression contained a cross-module call (`fn() { ffi.sleep(duration.to_milliseconds(d)) }`). Code targets and dirty natives keep the eager pop.
- Host results applied at tail-call parks (`call_ext_only`/`call_ext_last`) return to the caller — popping the deferred frame first — instead of advancing past the function's last instruction; the suspension record carries the park's resume continuation, chosen at suspend time. Scope: threaded scheduler — the WASM scheduler's completion apply still advances blindly (known follow-up, consistent with its pid-keyed completion map).

### Compatibility

- `SuspensionRecord` gained a `continuation` field and `interpreter::opcodes::trampoline::handle_suspend` takes the parked call's completion shape; embedders constructing these VM-internal types directly must update. The embedder-facing `Scheduler`/`ProcessContext` APIs are unchanged.

## 0.5.0

### Correctness

- Suspension protocol redesign (call-identity gating): every result-gated suspension — host await, dirty native call, hook suspend — now carries a per-process monotonically increasing call id recorded at suspend time. Completions are published keyed by `(pid, call id)` and applied at slice start only when the id matches the process's current suspension at its recorded park position; stale completions are dropped instead of being applied blind (the pid-keyed, position-blind application could advance the instruction pointer at the wrong park position — or twice — desyncing execution into "invalid operand for instruction pointer"). Gated host awaits (`ProcessContext::request_await_suspend`, file/UDP/TCP/inet ring operations, `submit_io_and_suspend`) have a wake guard: plain message arrivals can no longer re-execute the await native and double-submit its host work. `request_suspend` keeps its message-wakeable re-execution semantics for re-entrant natives (select, marker awaits) and now returns the suspension call id; `Scheduler::wake_with_result_for(pid, call_id, term)` is the exact completion API and `wake_with_result`/`wake_with_dirty_result` resolve the id at publish time (and return `bool`). `Scheduler::resume_process` is identity-gated (it can no longer resume an in-flight dirty call) and sticky (a resume racing the hook suspension's park gap is recorded and consumed, never lost). Completion application owns the timed-await lifecycle, so a completion-vs-timeout race can neither re-run the native nor leave stale timeout metadata that a later wait would re-arm. Process exit purges all per-pid suspension state. Resuming native continuations may legally re-suspend or trampoline (previously their requests were silently dropped), dirty natives may re-suspend as host awaits or trampoline closures (requests travel through `DirtyResult`), and pending continuations are position-gated so a re-entered await at equal stack depth cannot re-fire a continuation with garbage x0. Scope: threaded scheduler — the WASM scheduler keeps its single-threaded pid-keyed completion map (known follow-up).
- Wave 1 scheduler/VM fixes: opcode 115 (`is_function2`) decodes with its arity operand instead of crashing every literal-arity `is_function/2` guard; `try_case` consumes the current exception so a caught-and-handled exception no longer surfaces as an exit exception; the Wait arm registers in the wait set before its final mailbox recheck (lost-wakeup race against concurrent delivery); a dirty suspension whose resume raced the park is unparked by a fallback recheck.
- Registered `erlang:is_function/1` and `is_function/2` as callable BIFs — body-position calls and variable-arity guards (which compile to the guard-BIF instruction) previously crashed at call time on the unresolved erlang import.
- `receive ... after` timeouts are delivered per BEAM semantics: timer expiry falls through to the `timeout` instruction (the after-body) instead of re-scanning the receive loop and re-arming forever, and the receive timer stays armed across non-matching message wakeups instead of being cancelled with a stale ref that blocked re-arming. Timer expiry is now mark-and-wake: the owning scheduler thread applies the timeout jump at slice start, closing the expiry-vs-park race (the wait-arm recheck also notices a timer that fired inside the park gap). Scope: threaded scheduler only — the WASM scheduler (cancel-on-enqueue) and the JIT wait path (clear-ref on re-execution) still re-arm the full timeout after a non-matching wake; both are known follow-ups.

### Output

- Lists of printable latin1 character codes format as double-quoted strings (`[104,105]` prints as `"hi"`), matching `io_lib:printable_list/1` semantics and the Erlang shell.

## 0.4.9

- `bs_match` `'=:='` chunks compare as integer values, fixing literal-pattern matches against binary segments.

## 0.4.8

- Dirty-parked processes stay parked across mailbox wakes: a message arriving while a dirty native call is in flight no longer schedules a slice that re-executes the call instruction.

## 0.4.7

- Only dirty results resume dirty-call suspensions; mailbox deliveries can no longer resume a process suspended on an in-flight dirty native call.

## 0.4.6

- NIF private data — the `enif_priv_data` equivalent, carried into continuation resume contexts.
- Closed a lost-wakeup race between host delivery and NIF suspend.

## 0.4.5

- Allocation-list fun entries reserve the full closure base, fixing heap reservation for funs allocated through allocation lists.

## 0.4.4

- Release of the 0.4.3 series (no code changes beyond the version bump).

## 0.4.3

- Removed all remaining `gleam_stdlib`/`gleam@` native stub shadows; OTP-level natives made contract-exact. Fixed seven VM bugs found by extended gate stdlib coverage, plus binary-match opcodes and `string:trim` semantics.
- Deterministic replay: causal message ordering, persisted replay logs, a record/replay CLI, and hardened log validation.
- WASM scheduler: receive timers and async NIF promises bridged, direct JS term conversion, JS message send and callbacks, bundle builder with an edge-worker example.
- Workflow telemetry bridged into process tracing; Aion `with_timeout` trampoline continuation variant.

## 0.4.2

- Release bump for the correctness work documented under 0.4.1 below (core correctness, structural GC rooting, fresh Gleam gate).

## 0.4.1

### Correctness

- Fixed `STRING_EXT` literal materialisation: ETF tag 107 is a compact list of byte-sized integers and now becomes cons cells instead of a binary (root cause of `lists:reverse/1` badarg on list literals).
- Exit results and exceptions are captured as owning deep copies before process heap teardown, fixing use-after-free formatting of CLI results and error reasons.
- Native BIF allocation sequences are now structurally GC-safe: self-rooting allocators, `with_rooted`/`rooted_push` scopes, and native continuation state traced as process roots (previously x-registers above the BIF arity were not roots).
- `bs_create_bin` handles real compiler-emitted segment forms; big-integer literals load through the constant pool; unary minus/`abs` and integer-to-string conversions cover bignums.
- Capability-denied imports bind an explicit `ResolvedImportTarget::Denied` variant instead of comparing function pointers, which broke under release codegen.

### Features

- Export funs (`fun M:F/A`): EXPORT_EXT literals materialise as callable values dispatched by MFA through `call_fun`/`call_fun2` and native trampolines — passing `int.to_string` to `list.map` works.
- Native OTP 27 `json` module (`decode/1`, `encode/1`, `encode_integer/1`, `encode_float/1`, `encode_binary/1`), dependency-free and always on, with the OTP error contract `gleam_json` matches on.
- `beamr imports` also lists deferred module dependencies, so empty output now genuinely means the module runs standalone.

### Fixes

- Removed native stubs that shadowed real Gleam stdlib bytecode with wrong semantics (`gleam@list:map` argument order, `gleam@string_tree:split` returning nil).
- The CLI shares its atom table and BIF registry with the scheduler; spawn failures report resolved MFA names instead of `#<unknown atom>`.
- `io_lib_format:fwrite_g/1` keeps a decimal point in whole floats (`1.0`, not `1`).
- Fixed a whole-suite DashMap self-deadlock and a TCP fd-reuse test flake; the test suite (1,500+ tests) and strict clippy (`-D warnings`) gate the workspace.

## 0.4.0

### Headline features

- Added always-on JIT compilation via Cranelift, including runtime profiling, native-code cache support, and adaptive threshold tuning through scheduler configuration.
- Added AOT/native bundle support for exported module functions with Gleam type sidecars. AOT bundles persist a host-target-validated cache envelope and recorded function metadata; native Cranelift function pointers remain process-local and are recompiled on load.
- Added single-binary packaging support with embedded `.beam` archives and runtime loading APIs for packaged modules.
- Added a differential testing framework for comparing beamr behavior with BEAM/Gleam expectations, including JIT-threshold-forced differential runs.
- Added Criterion benchmark targets for JIT comparison and extended JIT comparison workloads.
- Added the new `gleam-types` crate for extracting, serializing, and loading Gleam type sidecars consumed by beamr's typed JIT/AOT paths.

### Breaking changes

- Runtime/API surface now carries JIT state: `SchedulerConfig` includes `jit_threshold`, `SharedState` owns JIT profiler/cache fields, and `Process` tracks JIT runtime/status fields.
- Process/runtime internals gained additional fields for Phase 4 execution state; code constructing these structures directly must use the updated constructors or provide the new fields.

### Release notes

- Publish order is `gleam-types` first, then `beamr` after the `gleam-types = 0.4.0` dependency is available.
- Actual crates.io publishing and pushing `v0.4.0` require explicit project-lead approval.
