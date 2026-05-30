---
phase: wave-3
plan: B-030
subsystem: native/selector_ffi
tags: [selector, trampoline, gleam_erlang_ffi, message-passing, re-entry]
dependency-graph:
  requires: [B-024, B-028b]
  provides: [selector-system, trampoline-mechanism, select-facility]
  affects: [interpreter/opcodes/core, native/context, beamr-cli]
tech-stack:
  added: []
  patterns: [trampoline-re-entry, facility-trait, mailbox-snapshot]
key-files:
  created:
    - crates/beamr/src/native/selector_ffi.rs
    - crates/beamr/src/native/selector_ffi_tests.rs
    - crates/beamr/src/native/select.rs
    - crates/beamr/src/interpreter/opcodes/trampoline.rs
  modified:
    - crates/beamr/src/native/context.rs
    - crates/beamr/src/native/mod.rs
    - crates/beamr/src/interpreter/opcodes/core.rs
    - crates/beamr/src/interpreter/opcodes/mod.rs
    - crates/beamr-cli/src/main.rs
    - crates/beamr/tests/stdlib_loading.rs
decisions:
  - id: D-030-1
    title: "ProcessContext trampoline over NativeFn return type change"
    rationale: "Adding a trampoline field to ProcessContext avoids changing the NativeFn type signature (fn(&[Term], &mut ProcessContext) -> Result<Term, Term>) which would require modifying every existing BIF and test. The trampoline is a side-channel checked by the interpreter after each BIF call, following the same pattern used for other facilities."
  - id: D-030-2
    title: "MailboxSnapshot for BIF mailbox access"
    rationale: "BIFs receive ProcessContext, not Process, so they cannot access the mailbox directly. The MailboxSnapshot approach snapshots scan-list messages into a Vec<Term>, provides peek/remove through a SelectFacility trait, and lets the interpreter apply the recorded removal after the BIF returns. This avoids unsafe interior mutability."
  - id: D-030-3
    title: "Leaked heap allocations for selector data structures"
    rationale: "BIFs cannot allocate on the process heap (ProcessContext does not expose it). Selector tuples and cons cells use Box::leak, matching the existing pattern in stdlib_stubs. These are short-lived configuration data acceptable for the current implementation."
metrics:
  duration: "14 minutes"
  completed: "2026-05-30T20:24:14Z"
  tests-added: 25
  tests-total: 475
---

# B-030: gleam_erlang_ffi -- selector system Summary

Selector system with trampoline interpreter re-entry for typed message receiving via gleam_erlang_ffi BIFs.

## What Was Built

### R1: Selector data structure and builders

Implemented in `selector_ffi.rs`:
- `new_selector/0` returns NIL (empty list)
- `insert_selector_handler/3` prepends `{Tag, Handler}` tuple to selector list
- `map_selector/2` wraps each handler with `{mapped, MapFun, OriginalHandler}` for composed invocation
- `merge_selector/2` concatenates two selector lists
- `remove_selector_handler/2` filters out entries matching a tag

All registered under `gleam_erlang_ffi` module atom.

### R2: select/1 and select/2

- `select/1` scans the process mailbox via SelectFacility, tests each message against handlers in order, and on match: removes the message from the mailbox and sets a TrampolineRequest with the handler closure and matched message
- `select/2` adds timeout support: timeout=0 returns `{error, nil}` immediately; timeout>0 sets a SuspendRequest with the timeout for scheduler integration
- Message matching: tuple first-element equality, direct equality, and `anything` atom catch-all

### R3: Trampoline mechanism (generic, reusable)

The trampoline mechanism is designed for ALL future BIFs that need interpreter re-entry:

1. **TrampolineRequest** on ProcessContext: `{ fun: Term, args: Vec<Term> }` -- any BIF can set this
2. **SuspendRequest** on ProcessContext: `{ timeout_ms: Option<u64> }` -- any BIF can request process suspension
3. **Interpreter handling** in `call_ext` (via `trampoline.rs` module):
   - After each BIF call, checks for suspend request (transitions to Waiting)
   - Then checks for trampoline request (sets up closure call: loads args into x-regs, loads free vars, pushes return frame, jumps to lambda)
   - The closure executes as normal BEAM bytecode and its return value naturally ends up in x(0)

### R4: SelectFacility trait

- `SelectFacility` trait provides `message_count()`, `peek_message(index)`, `remove_message(index)`
- `MailboxSnapshot` implementation: interpreter snapshots mailbox messages before BIF call, BIF reads through snapshot, interpreter applies recorded removal after BIF returns
- Clean separation: BIF logic never touches Process or Mailbox directly

## Deviations from Plan

None -- plan executed exactly as written.

## Key Design Decisions

**Trampoline via ProcessContext (Option B from brief):** Chose the ProcessContext field approach over changing NativeFn return type. Zero churn on existing BIFs. The interpreter checks `context.take_trampoline()` after each native call -- one conditional branch on the fast path for non-select BIFs.

**Mailbox snapshot approach:** Rather than giving BIFs mutable mailbox access (which would require unsafe interior mutability through Arc), the interpreter snapshots messages before the BIF call and applies removals afterward. This maintains the invariant that only the interpreter/process-owner mutates the mailbox.

## Test Coverage

25 new tests across 3 test files:
- `selector_ffi_tests.rs` (18 tests): builders, matching, registration, facility integration, suspend, timeout, handler ordering, map_selector
- `select.rs` (3 tests): MailboxSnapshot peek, remove, empty
- `trampoline.rs` (4 tests): snapshot building, mailbox removal, suspend transitions

## Self-Check: PASSED
