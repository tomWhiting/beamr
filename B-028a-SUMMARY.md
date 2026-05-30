---
phase: B
plan: "028a"
subsystem: native/stdlib_stubs
tags: [bifs, maps, lists, timer, stdlib]
dependency-graph:
  requires: [B-029]
  provides: [maps:from_list/1, maps:merge/2, maps:remove/2, lists:reverse/1, timer:sleep/1]
  affects: [native/stdlib_stubs]
tech-stack:
  patterns: [leaked-box-heap-allocation, flatmap-construction, cons-cell-building]
key-files:
  created:
    - crates/beamr/src/native/stdlib_stubs/collection_bifs.rs
    - crates/beamr/src/native/stdlib_stubs/collection_bifs_tests.rs
  modified:
    - crates/beamr/src/native/stdlib_stubs/mod.rs
    - crates/beamr/src/native/stdlib_stubs/tests.rs
decisions:
  - Split collection BIFs into separate file to stay under 500-line limit
  - Used leaked Box allocations for heap terms (consistent with B-029 pattern)
  - maps:from_list uses last-occurrence-wins for duplicate keys (OTP semantics)
  - timer:sleep uses std::thread::sleep (blocking) for single-process CLI path
metrics:
  duration: 448s
  completed: 2026-05-30T19:29:37Z
---

# Phase B Plan 028a: Non-higher-order stdlib stubs as native BIFs Summary

Maps, lists, and timer BIFs implemented as native Rust functions using leaked-box heap allocation for term construction, registered under OTP module names.

## Tasks Completed

| Task | Description | Commit | Key Files |
|------|-------------|--------|-----------|
| R1 | maps:from_list/1, maps:merge/2, maps:remove/2 | 01dd4c0 | collection_bifs.rs |
| R2 | lists:reverse/1 | 01dd4c0 | collection_bifs.rs |
| R3 | timer:sleep/1 | 01dd4c0 | collection_bifs.rs |

## Implementation Details

### R1: maps module stubs

- **maps:from_list/1**: Parses a list of 2-tuples, deduplicates (last-occurrence-wins), sorts by key for flatmap ordering, allocates via `write_map` on leaked heap.
- **maps:merge/2**: Reads entries from both maps via `Map` accessor, merges with second-overrides-first semantics, sorts and writes new flatmap.
- **maps:remove/2**: Iterates map entries, filters out target key, writes new flatmap from remaining entries.

### R2: lists:reverse/1

Collects list elements via `Cons` accessor traversal, then rebuilds the list in forward order (which produces reversed output since elements are already collected front-to-back). Each cons cell allocated via `Box::leak`.

### R3: timer:sleep/1

Validates input is a non-negative small integer, converts to milliseconds, calls `std::thread::sleep`. Returns atom `ok`. Blocking behavior is acceptable for the single-process CLI path.

### File Organization

Split new BIFs into `collection_bifs.rs` (190 lines) with corresponding `collection_bifs_tests.rs` (397 lines) to keep all files under the 500-line limit. Updated `mod.rs` with module declarations and registration entries.

## Test Coverage

26 new tests covering:
- maps:from_list/1: basic construction, empty list, duplicate keys, non-list rejection, non-tuple rejection, wrong arity
- maps:merge/2: combination, collision override, empty maps, non-map rejection, wrong arity
- maps:remove/2: key removal, missing key, single-entry to empty, non-map rejection, wrong arity
- lists:reverse/1: proper list reversal, empty list, single element, non-list rejection, wrong arity
- timer:sleep/1: zero sleep, small duration, negative rejection, non-integer rejection, wrong arity
- Registration: all 10 stdlib stubs (5 original + 5 new) registered correctly

## Deviations from Plan

None - plan executed exactly as written.
