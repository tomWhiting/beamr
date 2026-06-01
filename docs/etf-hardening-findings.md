# ETF literal decoder hardening — findings + applied fixes

Two denial-of-service findings in the `LitT` literal decoder
(`crates/beamr/src/loader/decode/chunks.rs`), both reachable from the literal
chunk of a loaded `.beam` — i.e. from attacker-influenced bytecode. This branch
**applies** minimal, self-contained fixes (the commits here) so they can be
reviewed as a diff; take, adapt, or ignore as suits the upstream design.

Verified against `main` @ `8de842b`. `cargo check -p beamr` clean; all 14 loader
tests pass plus two added regression tests; no observable change to valid-input
decoding.

---

## F1 — Unbounded recursion → stack overflow (DoS)

**Where:** `decode_external_term` recursed on the nesting tags with no depth
limit: `108` (LIST_EXT) → elements + tail, `116` (MAP_EXT) → keys + values,
`113` (EXPORT_EXT) → module/function/arity, `104`/`105` (TUPLE) via
`decode_tuple`.

**Repro:** a `LitT` literal encoding a deeply nested list (`108`-tagged, each
element itself a `108`-tagged list) recurses once per level. A few hundred KB of
crafted-but-well-formed literal overflows the native stack and aborts the
process — before a single instruction runs.

**Fix applied:** thread a `depth` counter with a hard `MAX_ETF_DEPTH = 256` cap,
mirroring the operand decoder's existing `MAX_OPERAND_NEST_DEPTH` guard in
`compact.rs`. Over-deep nesting now returns `LoadError::DecodeError` instead of
overflowing. Regression test: `decode_literal_chunk_rejects_overdeep_nesting`.

## F2 — Unbounded pre-allocation → OOM (DoS)

**Where:** the element-count container tags read a wire-supplied count and
immediately `Vec::with_capacity(count)` before reading any element:

- `108` LIST_EXT — `with_capacity(len)`, `len` an untrusted `u32`
- `116` MAP_EXT — `with_capacity(len)`, `len` an untrusted `u32`
- `decode_tuple` (reached from `105` TUPLE_EXT) — `with_capacity(arity)`,
  `arity` an untrusted `u32`

**Repro:** a `108`/`116`/`105` literal whose count field is `0xFFFFFFFF` but
whose body is truncated. Decode *eventually* fails when the cursor runs dry, but
the `with_capacity` runs first and attempts a multi-GB allocation → OOM / abort.

**Fix applied:** every container element costs ≥ 1 byte, so a count past the
cursor's remaining bytes is provably impossible — reject it before allocating,
and cap the pre-allocation at `ETF_PREALLOC_CAP = 1024` (the vector grows on
demand past the cap for genuinely large, well-formed inputs). Regression test:
`decode_literal_chunk_rejects_oversized_list_count`.

**Not affected:** `107` STRING_EXT, `109` BINARY_EXT, and `110`/`111`
BIG_EXT read their payload via `read_bytes(len)` *before* any `with_capacity`,
and `read_bytes` is already cursor-bounded — so `len` can never exceed the real
input there. The issue is specific to the element-count pre-allocation on
`108`/`116`/`105`.

---

## Note on provenance

These came out of hardening a downstream BEAM-VM fork that closes this class
structurally ("every untrusted input edge is bounded + depth-guarded"); this
branch is the upstream-applicable slice. Happy to open as a PR against `main`.
