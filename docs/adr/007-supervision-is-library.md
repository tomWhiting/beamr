# ADR-007: Supervision Is Library Code, Not VM Machinery

**Status:** Accepted
**Date:** 2026-05-29

## Context

Erlang/OTP supervision trees are a defining feature of the platform.
However, supervisor strategies (one_for_one, one_for_all, rest_for_one)
are implemented as library code in OTP, not as VM primitives.

The VM provides four foundational primitives that supervisors are built
on top of:

1. **Links** -- bidirectional crash propagation between processes.
2. **Monitors** -- unidirectional observation of process lifecycle.
3. **Exit signals** -- the mechanism by which crash information travels.
4. **trap_exit** -- allows a process to catch exit signals as messages
   instead of dying.

Gleam uses `gleam_otp` for supervision, which compiles to BEAM bytecode
that uses these four primitives.

## Decision

Supervision is library code. The VM implements links, monitors, exit
signals, and trap_exit as primitives. Supervisor strategies are
implemented in Gleam via `gleam_otp` and run as normal BEAM processes.

## Consequences

**Positive:**
- Less VM complexity. The VM does four things well instead of
  reimplementing OTP's supervision logic in Rust.
- Supervision patterns can evolve in Gleam without VM changes.
- Easier to test: the four primitives have clear, specification-defined
  semantics.

**Negative:**
- The four primitives must be implemented exactly right. A subtle bug in
  link propagation or exit signal delivery will cause silent supervision
  failures. These primitives are the foundation everything else trusts.
- Debugging supervision issues requires understanding both the Gleam
  library layer and the VM primitive layer.
