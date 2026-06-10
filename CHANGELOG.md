# Changelog

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
