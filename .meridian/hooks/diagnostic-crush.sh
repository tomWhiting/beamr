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
  *.rs) ;;
  *) exit 0 ;;
esac

[ -f "$FILE_PATH" ] || exit 0

find_crate() {
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

CRATE=$(find_crate "$FILE_PATH") || exit 0
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
REL_PATH="${FILE_PATH#"$REPO_ROOT/"}"

OUTPUT=""
HAD_ISSUES=0

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
  FIX: Split into submodules. mod.rs holds only pub mod + re-exports.
"
fi

# --- BYPASS SCAN: #[allow], #[expect], #[cfg(any())], #[ignore] ---
# Match actual attribute syntax only (line starts with optional whitespace + #[)
# Avoids false positives from comments/strings mentioning these attributes
BYPASSES=$(grep -nE '^\s*#\[(allow|expect)\(|^\s*#\[cfg\(any\(\)\)\]|^\s*#\[ignore' "$FILE_PATH" 2>/dev/null || true)
if [ -n "$BYPASSES" ]; then
  HAD_ISSUES=1
  OUTPUT+="
✗ BYPASS DETECTED [project::no_bypass]
  ${REL_PATH}
$(echo "$BYPASSES" | sed 's/^/  /')

  WHY: Silencing a lint hides risk from reviewers and diagnostics.
  FIX: Remove the attribute and fix the underlying code.
  DO NOT: Move it to a parent scope. That is the same evasion.
"
fi

# --- CARGO CLIPPY: Lint check on affected crate ---
CLIPPY_ISSUES=$(cd "$REPO_ROOT" && cargo clippy -p "$CRATE" --all-targets -q --message-format=json -- -D warnings 2>/dev/null \
  | jq -c --arg rel_path "$REL_PATH" 'select(.reason == "compiler-message") | select(.message.spans[0].file_name == $rel_path) | {lint: (.message.code.code // "compile-error"), msg: .message.message, line: .message.spans[0].line_start, col: .message.spans[0].column_start, snippet: (.message.spans[0].text[0].text // ""), notes: [.message.children[]? | select(.level=="note") | select(.message | test("allow|for further information|visit https") | not) | .message]}' 2>/dev/null || true)

if [ -n "$CLIPPY_ISSUES" ]; then
  while IFS= read -r issue; do
    HAD_ISSUES=1
    LINT=$(echo "$issue" | jq -r '.lint')
    MSG=$(echo "$issue" | jq -r '.msg')
    LINE=$(echo "$issue" | jq -r '.line')
    COL=$(echo "$issue" | jq -r '.col')
    SNIPPET=$(echo "$issue" | jq -r '.snippet')

    case "$LINT" in
      clippy::unwrap_used)
        WHY="Every .unwrap() is a panic in disguise. This codebase serves medical, legal, and financial workloads. A panic mid-transaction is malpractice."
        FIX="Propagate with ?. If None/Err is impossible, make that provable in the type."
        DONOT="No #[allow]. No .expect(). No _var rename."
        ;;
      clippy::expect_used)
        WHY=".expect() is .unwrap() with a tombstone inscription. The string decorates the crash, it does not prevent it."
        FIX="Define a thiserror variant. Use ?."
        DONOT="No #[allow]. No rewording the message."
        ;;
      clippy::panic)
        WHY="panic!() abdicates failure handling to your caller without consent."
        FIX="Return Result."
        DONOT="No #[allow(clippy::panic)]."
        ;;
      clippy::todo)
        WHY="todo!() is a deferred panic. No deferred work."
        FIX="Implement it now or remove the function."
        DONOT=""
        ;;
      *)
        WHY=$(echo "$issue" | jq -r '.notes | join(" -- ")')
        FIX="Fix the underlying code."
        DONOT="No #[allow($LINT)]."
        ;;
    esac

    OUTPUT+="
✗ ${MSG} [$LINT]
  ${REL_PATH}:${LINE}:${COL}
    $SNIPPET
  WHY: $WHY
  FIX: $FIX"
    [ -n "$DONOT" ] && OUTPUT+="
  DO NOT: $DONOT"
    OUTPUT+="
"
  done <<< "$CLIPPY_ISSUES"
fi

# --- EMIT as JSON for Claude Code hook system ---
if [ "$HAD_ISSUES" -eq 1 ]; then
  OUTPUT+="
The compiler told you nicely. I won't."

  ESCAPED=$(echo "$OUTPUT" | jq -Rsa .)
  printf '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":%s}}' "$ESCAPED"

  # Live notification — uses a dedicated subject line for filtering
  ISSUE_COUNT=$(echo "$OUTPUT" | grep -c '✗' || true)
  collective channel send --as "Meridian" --channel diagnostics \
    --message "✗ ${ISSUE_COUNT} issue(s) on ${FILE_PATH} (crate: ${CRATE})" \
    2>/dev/null || true
fi

exit 0
