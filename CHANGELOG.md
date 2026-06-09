# Changelog

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
