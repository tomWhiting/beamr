# ADR-005: Implement Only the Opcodes Gleam Emits

**Status:** Accepted
**Date:** 2026-05-29

## Context

The BEAM instruction set contains approximately 170 opcodes. Many exist
to support Erlang-specific features (funs with complex closures, binary
comprehensions, map update chains) or are optimisation variants that the
Gleam compiler does not emit.

Implementing the full instruction set would be a multi-month effort with
no payoff for the target use case: running Gleam-compiled workflow code.

The loader already performs instruction analysis on every BEAM file it
loads, giving us a precise inventory of which opcodes any given module
actually uses.

## Decision

We implement only the opcodes that Gleam's compiler emits. The loader's
instruction analysis report is the authoritative source for which
opcodes are required.

When the VM encounters an unimplemented opcode, it produces an explicit
error naming the missing opcode and the module that requires it. This
makes the gap visible and actionable rather than silent.

## Consequences

**Positive:**
- Dramatically reduced implementation surface. Early analysis suggests
  Gleam uses roughly 40-60 of the ~170 opcodes.
- Development effort focuses on correctness of the opcodes that matter.
- Unknown opcodes fail loudly with actionable diagnostics.

**Negative:**
- Cannot run arbitrary Erlang modules. This is by design -- beamr
  targets Gleam, not the full Erlang ecosystem.
- If Gleam's compiler starts emitting new opcodes (e.g., after an OTP
  upgrade), we must implement them. The loader's error reporting makes
  this immediately obvious.
