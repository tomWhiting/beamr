#!/bin/bash
# run-checks.sh — comprehensive check suite matching run-checks-triaged.
# Args: $1 = crates_affected (space-separated), $2 = changed_files (space-separated)
# Outputs JSON: {"pass":bool,"summary":"...","failure_report":"...","deferred_clippy_report":"..."}
set -uo pipefail

CRATES="${1:-}"
CHANGED_FILES="${2:-}"

PASS=true
FAILURE_REPORT=""
DEFERRED_CLIPPY=""
SUMMARY_PARTS=""

# ── Cargo Check ─────────────────────────────────────────────────────
if [ -n "$(echo "$CRATES" | tr -d '[:space:]')" ]; then
  CHECK_OUT=$(cargo check --all-targets -q $(printf -- '-p %s ' $CRATES) 2>&1) || true
  CHECK_EXIT=${PIPESTATUS[0]:-$?}
else
  CHECK_OUT=$(cargo check --all-targets -q 2>&1) || true
  CHECK_EXIT=${PIPESTATUS[0]:-$?}
fi

if [ "$CHECK_EXIT" -ne 0 ]; then
  PASS=false
  FAILURE_REPORT="${FAILURE_REPORT}### Cargo Check FAILED\n\n${CHECK_OUT}\n\n"
  SUMMARY_PARTS="${SUMMARY_PARTS}cargo-check:FAIL "
else
  SUMMARY_PARTS="${SUMMARY_PARTS}cargo-check:OK "
fi

# ── Cargo Clippy (with blocking/deferred triage) ────────────────────
BLOCKING_CODES='clippy::unwrap_used|clippy::expect_used|clippy::get_unwrap|clippy::panic|clippy::todo|clippy::unimplemented|clippy::unreachable|clippy::future_not_send'

if [ -n "$(echo "$CRATES" | tr -d '[:space:]')" ]; then
  CLIPPY_OUT=$(cargo clippy --all-targets --message-format=short $(printf -- '-p %s ' $CRATES) -- -D warnings 2>&1) || true
  CLIPPY_EXIT=${PIPESTATUS[0]:-$?}
else
  CLIPPY_OUT=$(cargo clippy --all-targets --message-format=short -- -D warnings 2>&1) || true
  CLIPPY_EXIT=${PIPESTATUS[0]:-$?}
fi

if [ "$CLIPPY_EXIT" -ne 0 ]; then
  BLOCKING_LINES=$(echo "$CLIPPY_OUT" | grep -E "$BLOCKING_CODES" || true)
  DEFERRED_LINES=$(echo "$CLIPPY_OUT" | grep -v -E "$BLOCKING_CODES" | grep -E '(warning|error)\[' || true)

  if [ -n "$BLOCKING_LINES" ]; then
    PASS=false
    FAILURE_REPORT="${FAILURE_REPORT}### Cargo Clippy — BLOCKING\n\n${BLOCKING_LINES}\n\n"
    SUMMARY_PARTS="${SUMMARY_PARTS}clippy-blocking:FAIL "
  else
    SUMMARY_PARTS="${SUMMARY_PARTS}clippy-blocking:OK "
  fi

  if [ -n "$DEFERRED_LINES" ]; then
    DEFERRED_CLIPPY="### Cargo Clippy — DEFERRED\n\nStyle/perf issues — passed to reviewer:\n\n${DEFERRED_LINES}"
    SUMMARY_PARTS="${SUMMARY_PARTS}clippy-deferred:$(echo "$DEFERRED_LINES" | wc -l | tr -d ' ') "
  fi
else
  SUMMARY_PARTS="${SUMMARY_PARTS}clippy:OK "
fi

# ── Cargo Test ──────────────────────────────────────────────────────
if [ -n "$(echo "$CRATES" | tr -d '[:space:]')" ]; then
  TEST_OUT=$(cargo test --no-fail-fast $(printf -- '-p %s ' $CRATES) 2>&1) || true
  TEST_EXIT=${PIPESTATUS[0]:-$?}
else
  TEST_OUT=$(cargo test --no-fail-fast 2>&1) || true
  TEST_EXIT=${PIPESTATUS[0]:-$?}
fi

if [ "$TEST_EXIT" -ne 0 ]; then
  PASS=false
  FAILED_TESTS=$(echo "$TEST_OUT" | grep -E '^test .+ FAILED$' || true)
  TEST_SUMMARY=$(echo "$TEST_OUT" | tail -5)
  FAILURE_REPORT="${FAILURE_REPORT}### Cargo Test FAILED\n\n${FAILED_TESTS}\n\n${TEST_SUMMARY}\n\n"
  SUMMARY_PARTS="${SUMMARY_PARTS}test:FAIL "
else
  SUMMARY_PARTS="${SUMMARY_PARTS}test:OK "
fi

# ── TypeScript Check ────────────────────────────────────────────────
if [ -d "apps/web" ]; then
  TS_OUT=$(cd apps/web && npx tsc --noEmit --pretty false 2>&1) || true
  TS_EXIT=${PIPESTATUS[0]:-$?}
  if [ "$TS_EXIT" -ne 0 ]; then
    PASS=false
    FAILURE_REPORT="${FAILURE_REPORT}### TypeScript Check FAILED\n\n${TS_OUT}\n\n"
    SUMMARY_PARTS="${SUMMARY_PARTS}tsc:FAIL "
  else
    SUMMARY_PARTS="${SUMMARY_PARTS}tsc:OK "
  fi
fi

# ── Biome Check ─────────────────────────────────────────────────────
if [ -d "apps/web" ]; then
  BIOME_OUT=$(cd apps/web && npx biome check . 2>&1) || true
  BIOME_EXIT=${PIPESTATUS[0]:-$?}
  if [ "$BIOME_EXIT" -ne 0 ]; then
    PASS=false
    BIOME_ERRORS=$(echo "$BIOME_OUT" | tail -10)
    FAILURE_REPORT="${FAILURE_REPORT}### Biome Check FAILED\n\n${BIOME_ERRORS}\n\n"
    SUMMARY_PARTS="${SUMMARY_PARTS}biome:FAIL "
  else
    SUMMARY_PARTS="${SUMMARY_PARTS}biome:OK "
  fi
fi

# ── File Size Check ─────────────────────────────────────────────────
if [ -n "$(echo "$CHANGED_FILES" | tr -d '[:space:]')" ] && command -v tokei >/dev/null 2>&1; then
  OVERSIZED=""
  for f in $CHANGED_FILES; do
    if [ -f "$f" ]; then
      LOC=$(tokei "$f" --output json 2>/dev/null | jq '[.[][] | .reports[]? | .stats.code] | add // 0' 2>/dev/null || echo 0)
      if [ "$LOC" -gt 500 ] 2>/dev/null; then
        OVERSIZED="${OVERSIZED}- ${f} (${LOC} LoC)\n"
      fi
    fi
  done
  if [ -n "$OVERSIZED" ]; then
    PASS=false
    FAILURE_REPORT="${FAILURE_REPORT}### Files exceeding 500 LoC\n\nSplit each into smaller modules.\n\n${OVERSIZED}\n"
    SUMMARY_PARTS="${SUMMARY_PARTS}filesize:FAIL "
  else
    SUMMARY_PARTS="${SUMMARY_PARTS}filesize:OK "
  fi
fi

# ── Output ──────────────────────────────────────────────────────────
jq -nc \
  --argjson pass "$PASS" \
  --arg summary "$SUMMARY_PARTS" \
  --arg failure_report "$FAILURE_REPORT" \
  --arg deferred_clippy_report "$DEFERRED_CLIPPY" \
  '{pass:$pass, summary:$summary, failure_report:$failure_report, deferred_clippy_report:$deferred_clippy_report}'
