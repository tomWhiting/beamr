#!/bin/bash
# commit-and-report.sh — auto-fix, commit, and report affected crates/files.
# Args: $1 = path to commit message file
# Outputs JSON to stdout. Errors to stderr.
set -uo pipefail

COMMIT_MSG_FILE="${1:-.commit-msg.tmp}"

UNSTAGED=$(git diff --name-only 2>/dev/null || true)
UNTRACKED=$(git ls-files --others --exclude-standard 2>/dev/null || true)
ALL_CHANGED="$UNSTAGED $UNTRACKED"

CRATES=$(echo "$ALL_CHANGED" | tr ' ' '\n' | grep '^crates/.*\.rs$' | cut -d/ -f1-2 | sort -u | while read dir; do
  if [ -f "$dir/Cargo.toml" ]; then
    grep '^name' "$dir/Cargo.toml" | head -1 | sed 's/name *= *"//;s/"//g'
  fi
done | sort -u | tr '\n' ' ')

if [ -z "$(echo "$ALL_CHANGED" | tr -d '[:space:]')" ]; then
  printf '{"committed":false,"crates_affected":"","changed_files":""}\n'
  exit 0
fi

# Snapshot the files the dev agent actually changed (before clippy/fmt).
SCOPE_FILES=$(echo "$ALL_CHANGED" | tr ' ' '\n' | sort -u)

if [ -n "$(echo "$CRATES" | tr -d '[:space:]')" ]; then
  cargo clippy --all-targets --fix --allow-dirty --allow-staged $(printf -- '-p %s ' $CRATES) -- -D warnings >/dev/null 2>&1 || true
  # Scope fmt to changed .rs files only — cargo fmt --all reformats the entire workspace
  RS_FILES=$(echo "$SCOPE_FILES" | tr ' ' '\n' | grep '\.rs$' | tr '\n' ' ')
  if [ -n "$(echo "$RS_FILES" | tr -d '[:space:]')" ]; then
    cargo fmt -- $RS_FILES >/dev/null 2>&1 || true
  fi
fi

# Only stage files the agent changed — clippy/fmt may have touched others.
for f in $SCOPE_FILES; do
  if [ -e "$f" ]; then
    git add "$f" 2>/dev/null || true
  fi
done
# Also stage Cargo.lock if any Cargo.toml was touched.
if echo "$SCOPE_FILES" | grep -q 'Cargo.toml' && [ -f Cargo.lock ]; then
  git add Cargo.lock 2>/dev/null || true
fi

# Revert any out-of-scope modifications from clippy/fmt.
git checkout -- . 2>/dev/null || true

if [ -z "$(git diff --cached --name-only 2>/dev/null)" ]; then
  printf '{"committed":false,"crates_affected":"%s","changed_files":""}\n' "$CRATES"
  exit 0
fi

COMMIT_OUT=$(git commit -F "$COMMIT_MSG_FILE" --no-verify 2>&1)
COMMIT_EXIT=$?
if [ "$COMMIT_EXIT" -ne 0 ]; then
  echo "commit-and-report: git commit failed (exit $COMMIT_EXIT): $COMMIT_OUT" >&2
  exit 1
fi

CHANGED_FILES=$(git diff --name-only HEAD~1 2>/dev/null | tr '\n' ' ')
printf '{"committed":true,"crates_affected":"%s","changed_files":"%s"}\n' "$CRATES" "$CHANGED_FILES"
