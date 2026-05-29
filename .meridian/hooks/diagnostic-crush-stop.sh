#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0

OUTPUT=""
HAD_ISSUES=0

# --- Determine affected crates from modified .rs files ---
find_crate_for_file() {
  local dir
  dir=$(dirname "$1")
  while [ "$dir" != "/" ]; do
    if [ -f "$dir/Cargo.toml" ]; then
      grep -m1 '^name' "$dir/Cargo.toml" | sed 's/name *= *"\(.*\)"/\1/' | tr -d ' '
      return 0
    fi
    dir=$(dirname "$dir")
  done
  return 1
}

MODIFIED_RS=$(cd "$REPO_ROOT" && git diff --name-only HEAD 2>/dev/null | grep '\.rs$' || true)
UNTRACKED_RS=$(cd "$REPO_ROOT" && git ls-files --others --exclude-standard 2>/dev/null | grep '\.rs$' || true)
ALL_RS=$(printf '%s\n%s' "$MODIFIED_RS" "$UNTRACKED_RS" | sort -u | grep -v '^$' || true)

AFFECTED_CRATES=""
if [ -n "$ALL_RS" ]; then
  AFFECTED_CRATES=$(while IFS= read -r f; do
    find_crate_for_file "$REPO_ROOT/$f"
  done <<< "$ALL_RS" | sort -u | tr '\n' ' ')
fi

# --- CARGO CHECK: Does the workspace compile? ---
CHECK_OUTPUT=$(cd "$REPO_ROOT" && cargo check --workspace --all-targets -q --message-format=json 2>/dev/null || true)

if [ -n "$CHECK_OUTPUT" ]; then
  ERROR_COUNT=$(echo "$CHECK_OUTPUT" | jq -c 'select(.reason == "compiler-message") | select(.message.level == "error")' 2>/dev/null | wc -l | tr -d ' ')

  if [ "$ERROR_COUNT" -gt 0 ] 2>/dev/null; then
    HAD_ISSUES=1

    ERRORS=$(echo "$CHECK_OUTPUT" | jq -c 'select(.reason == "compiler-message") | select(.message.level == "error") | {file: .message.spans[0].file_name, line: .message.spans[0].line_start, msg: .message.message}' 2>/dev/null | head -20)

    OUTPUT+="
✗ WORKSPACE DOES NOT COMPILE [cargo::check]
  ${ERROR_COUNT} compile error(s) at session end.

"
    while IFS= read -r err; do
      FILE=$(echo "$err" | jq -r '.file // "unknown"')
      LINE=$(echo "$err" | jq -r '.line // "?"')
      MSG=$(echo "$err" | jq -r '.msg')
      OUTPUT+="  ${FILE}:${LINE} — ${MSG}
"
    done <<< "$ERRORS"

    OUTPUT+="
  WHY: Code that does not compile cannot be tested, reviewed, or shipped.
  FIX: Resolve all compile errors before ending your session.
"
  fi
fi

# --- CARGO CLIPPY: Lint check on affected crates only ---
if [ -n "$(echo "$AFFECTED_CRATES" | tr -d '[:space:]')" ]; then
  CLIPPY_FLAGS=$(printf -- '-p %s ' $AFFECTED_CRATES)
  CLIPPY_OUTPUT=$(cd "$REPO_ROOT" && cargo clippy $CLIPPY_FLAGS --all-targets -q --message-format=json -- -D warnings 2>/dev/null || true)

  if [ -n "$CLIPPY_OUTPUT" ]; then
    CLIPPY_ERRORS=$(echo "$CLIPPY_OUTPUT" | jq -c 'select(.reason == "compiler-message") | select(.message.level == "error" or .message.level == "warning") | {file: .message.spans[0].file_name, line: .message.spans[0].line_start, lint: (.message.code.code // "compile-error"), msg: .message.message}' 2>/dev/null || true)

    if [ -n "$CLIPPY_ERRORS" ]; then
      CLIPPY_COUNT=$(echo "$CLIPPY_ERRORS" | wc -l | tr -d ' ')
      HAD_ISSUES=1

      OUTPUT+="
✗ CLIPPY LINT FAILURES [cargo::clippy]
  ${CLIPPY_COUNT} issue(s) in affected crate(s): ${AFFECTED_CRATES}

"
      while IFS= read -r issue; do
        FILE=$(echo "$issue" | jq -r '.file // "unknown"')
        LINE=$(echo "$issue" | jq -r '.line // "?"')
        LINT=$(echo "$issue" | jq -r '.lint')
        MSG=$(echo "$issue" | jq -r '.msg')
        OUTPUT+="  ${FILE}:${LINE} [$LINT] — ${MSG}
"
      done <<< "$(echo "$CLIPPY_ERRORS" | head -20)"

      OUTPUT+="
  WHY: Clippy lints in your affected crates must be resolved before session end.
  FIX: Fix each lint. Do not silence with #[allow] or #[expect].
"
    fi
  fi
fi

# --- CARGO NEXTEST: Test the workspace ---
NEXTEST_FAILS=$(cd "$REPO_ROOT" && NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --workspace --cargo-quiet --message-format libtest-json-plus --no-fail-fast 2>/dev/null \
  | jq -c 'select(.type == "test" and .event == "failed")' 2>/dev/null || true)

if [ -n "$NEXTEST_FAILS" ]; then
  FAIL_COUNT=$(echo "$NEXTEST_FAILS" | wc -l | tr -d ' ')
  HAD_ISSUES=1

  OUTPUT+="
✗ TEST FAILURES [nextest::failed]
  ${FAIL_COUNT} test(s) failed at session end.

"
  while IFS= read -r fail; do
    TEST_NAME=$(echo "$fail" | jq -r '.name')
    STDOUT=$(echo "$fail" | jq -r '.stdout // ""' | head -3)
    OUTPUT+="  ${TEST_NAME}
    ${STDOUT}
"
  done <<< "$NEXTEST_FAILS"

  OUTPUT+="
  WHY: Tests must pass before you end your session.
  FIX: Fix the implementation. The test is correct.
  DO NOT: Do NOT #[ignore]. Do NOT delete. Do NOT weaken assertions.
"
fi

# --- EMIT ---
if [ "$HAD_ISSUES" -eq 1 ]; then
  OUTPUT+="
The compiler told you nicely. I won't."

  ESCAPED=$(echo "$OUTPUT" | jq -Rsa .)
  printf '{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":%s}}' "$ESCAPED"

  ISSUE_COUNT=$(echo "$OUTPUT" | grep -c '✗' || true)
  collective channel send --as "Meridian" --channel diagnostics \
    --message "✗ Session end: ${ISSUE_COUNT} issue(s). (${REPO_ROOT})" \
    2>/dev/null || true
fi

exit 0
