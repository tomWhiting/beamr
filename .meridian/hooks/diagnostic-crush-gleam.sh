#!/usr/bin/env bash
set -euo pipefail

if [ -n "${1:-}" ]; then
  INPUT="$1"
elif [ ! -t 0 ]; then
  INPUT=$(cat)
else
  echo "Usage: $0 '<json>' or echo '<json>' | $0" >&2
  exit 1
fi
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty')

case "$TOOL_NAME" in
  Edit|Write) ;;
  *) exit 0 ;;
esac

FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')

case "$FILE_PATH" in
  *.gleam) ;;
  *) exit 0 ;;
esac

[ -f "$FILE_PATH" ] || exit 0

find_gleam_root() {
  local dir
  dir=$(dirname "$1")
  while [ "$dir" != "/" ]; do
    if [ -f "$dir/gleam.toml" ]; then
      echo "$dir"
      return 0
    fi
    dir=$(dirname "$dir")
  done
  return 1
}

PROJECT_ROOT=$(find_gleam_root "$FILE_PATH") || exit 0
REL_PATH="${FILE_PATH#"$PROJECT_ROOT/"}"

OUTPUT=""
HAD_ISSUES=0

strip_ansi() {
  sed 's/\x1B\[[0-9;]*m//g'
}

# --- TOKEI: Line count (500-line limit, this file only) ---
if command -v tokei &>/dev/null; then
  LINE_COUNT=$(tokei "$FILE_PATH" --output json 2>/dev/null | jq '[.. | .code? // empty] | add // 0')
else
  LINE_COUNT=$(wc -l < "$FILE_PATH" | tr -d ' ')
fi
if [ "$LINE_COUNT" -gt 500 ] 2>/dev/null; then
  HAD_ISSUES=1
  OUTPUT+="
✗ FILE TOO LONG [project::max-file-length]
  ${REL_PATH} — ${LINE_COUNT} lines (limit: 500)

  WHY: Files over 500 lines become unreadable. Bugs hide. Context is lost.
  FIX: Split into smaller modules. Gleam modules are cheap — use them.
"
fi

# --- BYPASS DETECTION: todo / panic / let assert ---
TODO_HITS=$(grep -nE '^\s*todo\b' "$FILE_PATH" 2>/dev/null || true)
if [ -n "$TODO_HITS" ]; then
  HAD_ISSUES=1
  OUTPUT+="
✗ TODO USAGE [gleam::todo]
  ${REL_PATH}
$(echo "$TODO_HITS" | sed 's/^/  /')

  WHY: todo crashes at runtime. It is a deferred panic, not a placeholder.
  FIX: Implement the function now or remove it.
  DO NOT: Leave it in \"just for now.\" There is no \"for now.\"
"
fi

PANIC_HITS=$(grep -nE '^\s*panic\b' "$FILE_PATH" 2>/dev/null || true)
if [ -n "$PANIC_HITS" ]; then
  HAD_ISSUES=1
  OUTPUT+="
✗ PANIC USAGE [gleam::panic]
  ${REL_PATH}
$(echo "$PANIC_HITS" | sed 's/^/  /')

  WHY: panic abdicates error handling. The caller did not consent to an abort.
  FIX: Return Result with a meaningful error type. Use case expressions to handle all variants.
  DO NOT: Use panic as a shortcut for error handling you haven't designed yet.
"
fi

ASSERT_HITS=$(grep -nE '^\s*let assert\b' "$FILE_PATH" 2>/dev/null || true)
if [ -n "$ASSERT_HITS" ]; then
  HAD_ISSUES=1
  OUTPUT+="
✗ PARTIAL PATTERN MATCH [gleam::let_assert]
  ${REL_PATH}
$(echo "$ASSERT_HITS" | sed 's/^/  /')

  WHY: let assert panics if the pattern doesn't match. Syntactic sugar for a crash.
  FIX: Use a case expression and handle all variants. If the pattern always matches, prove it in the type.
  DO NOT: Use let assert because \"it should always be Ok.\" That is what .unwrap() people say in Rust.
"
fi

# --- GLEAM CHECK: Compiler diagnostics (text parsed from stderr) ---
BUILD_OUTPUT=$(cd "$PROJECT_ROOT" && gleam check 2>&1 || true)
CLEAN_OUTPUT=$(echo "$BUILD_OUTPUT" | strip_ansi)

CURRENT_SEV=""
CURRENT_MSG=""
CURRENT_LOC=""
CURRENT_FILE=""
IN_BLOCK=0

while IFS= read -r line; do
  if echo "$line" | grep -qE '^(error|warning):'; then
    if [ "$IN_BLOCK" -eq 1 ] && [ "$CURRENT_FILE" = "$REL_PATH" ]; then
      HAD_ISSUES=1
      OUTPUT+="
✗ ${CURRENT_MSG} [gleam::${CURRENT_SEV}]
  ${CURRENT_LOC}

  WHY: The Gleam compiler rejected this code.
  FIX: Address the compiler's feedback. Gleam's type system found a real problem.
  DO NOT: Restructure code to sidestep the check. Fix the actual issue.
"
    fi
    CURRENT_SEV=$(echo "$line" | grep -oE '^(error|warning)')
    CURRENT_MSG=$(echo "$line" | sed 's/^[a-z]*: //')
    CURRENT_LOC=""
    CURRENT_FILE=""
    IN_BLOCK=1
  elif echo "$line" | grep -qE '^\s*┌─'; then
    CURRENT_LOC=$(echo "$line" | sed 's/.*┌─ //')
    CURRENT_FILE=$(echo "$CURRENT_LOC" | sed 's/:[0-9]*:[0-9]*$//')
  fi
done <<< "$CLEAN_OUTPUT"

if [ "$IN_BLOCK" -eq 1 ] && [ "$CURRENT_FILE" = "$REL_PATH" ]; then
  HAD_ISSUES=1
  OUTPUT+="
✗ ${CURRENT_MSG} [gleam::${CURRENT_SEV}]
  ${CURRENT_LOC}

  WHY: The Gleam compiler rejected this code.
  FIX: Address the compiler's feedback. Gleam's type system found a real problem.
  DO NOT: Restructure code to sidestep the check. Fix the actual issue.
"
fi

# --- GLEAM TEST: Run tests (text parsed) ---
TEST_OUTPUT=$(cd "$PROJECT_ROOT" && gleam test 2>&1 || true)
CLEAN_TEST=$(echo "$TEST_OUTPUT" | strip_ansi)

FAIL_COUNT=$(echo "$CLEAN_TEST" | grep -oE '[0-9]+ failures' | grep -oE '[0-9]+' || echo "0")
if [ "$FAIL_COUNT" -gt 0 ] 2>/dev/null; then
  FAILURE_BLOCK=$(echo "$CLEAN_TEST" | sed -n '/^Failures:/,/^Finished/p' | head -20)
  HAD_ISSUES=1
  OUTPUT+="
✗ TEST FAILURES ($FAIL_COUNT) [gleam::test]
$(echo "$FAILURE_BLOCK" | sed 's/^/  /')

  FIX: Fix the implementation. The test is correct.
  DO NOT: Delete the test. Do NOT weaken assertions. Do NOT skip it.
"
fi

# --- RELAY SECURITY: HMAC secret + payload logging ---
HMAC_LOG=$(grep -nEi '(io\.println|io\.debug|string\.inspect|logging\.(log|info|debug|error|warning)).*hmac|hmac.*(io\.println|io\.debug|string\.inspect|logging\.(log|info|debug|error|warning))' "$FILE_PATH" 2>/dev/null || true)
if [ -n "$HMAC_LOG" ]; then
  HAD_ISSUES=1
  OUTPUT+="
✗ HMAC SECRET IN LOG PATH [relay::secret_leak]
  ${REL_PATH}
$(echo "$HMAC_LOG" | sed 's/^/  /')

  WHY: The HMAC pre-shared secret is the relay's single authentication credential.
  Logging it — even via io.debug — exposes it to anyone with log access.
  FIX: Remove the log statement or ensure the HMAC value is never interpolated.
  DO NOT: Log a \"redacted\" version. Do not log anything adjacent to the secret.
"
fi

PAYLOAD_LOG=$(grep -nEi '(io\.println|io\.debug|string\.inspect|logging\.(log|info|debug|error|warning)).*payload|payload.*(io\.println|io\.debug|string\.inspect|logging\.(log|info|debug|error|warning))' "$FILE_PATH" 2>/dev/null || true)
if [ -n "$PAYLOAD_LOG" ]; then
  HAD_ISSUES=1
  OUTPUT+="
✗ PAYLOAD CONTENT IN LOG PATH [relay::payload_leak]
  ${REL_PATH}
$(echo "$PAYLOAD_LOG" | sed 's/^/  /')

  WHY: The relay is crypto-blind by design. Payloads are opaque encrypted blobs.
  Logging payload content violates the relay's trust model.
  FIX: Log envelope metadata (sender, recipient, byte size) if needed. Never log content.
"
fi

# --- EMIT ---
if [ "$HAD_ISSUES" -eq 1 ]; then
  OUTPUT+="
The compiler told you nicely. I won't."

  ESCAPED=$(echo "$OUTPUT" | jq -Rsa .)
  printf '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":%s}}' "$ESCAPED"

  ISSUE_COUNT=$(echo "$OUTPUT" | grep -c '✗' || true)
  collective channel send --as "Meridian" --channel diagnostics \
    --message "✗ ${ISSUE_COUNT} issue(s) on ${FILE_PATH}" \
    2>/dev/null || true
fi

exit 0
