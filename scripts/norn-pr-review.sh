#!/usr/bin/env bash
# norn-pr-review.sh — Run norn reviewer against a PR and post findings as a comment.
#
# Usage:
#   ./scripts/norn-pr-review.sh <PR_NUMBER>
#   ./scripts/norn-pr-review.sh --poll          # Check all open PRs, review unreviewed ones
#
# Requires: norn, gh, jq on PATH. gh auth login done (PAT must have comment rights on repo).
#
# Limitations:
#   - Re-review after new commits: not automatic. Re-run manually or use --force.
#     The marker check only prevents duplicate first reviews.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

REVIEW_DIR=".run/pr-reviews"
PROFILE=".meridian/profiles/norn-reviewer.md"
MARKER="<!-- norn-review -->"

mkdir -p "$REVIEW_DIR"

review_pr() {
    local pr_num="$1"
    local review_file="$REVIEW_DIR/pr-${pr_num}.json"

    echo "=== Reviewing PR #${pr_num} ==="

    # Fetch PR metadata (separate declarations from assignments to preserve exit status)
    local pr_info
    pr_info=$(gh pr view "$pr_num" --json title,body,headRefName,baseRefName,changedFiles,additions,deletions)
    local title
    title=$(echo "$pr_info" | jq -r '.title')
    local base
    base=$(echo "$pr_info" | jq -r '.baseRefName')
    local head
    head=$(echo "$pr_info" | jq -r '.headRefName')
    local additions
    additions=$(echo "$pr_info" | jq -r '.additions')
    local deletions
    deletions=$(echo "$pr_info" | jq -r '.deletions')

    echo "PR: $title ($head -> $base, +$additions -$deletions)"

    # Fetch list of changed files
    local files
    files=$(gh pr view "$pr_num" --json files --jq '.files[].path' | head -50)

    # Write diff to file (too large for CLI arg)
    gh pr diff "$pr_num" > "$REVIEW_DIR/pr-${pr_num}.diff"

    # Build review prompt as a file
    cat > "$REVIEW_DIR/pr-${pr_num}-prompt.md" << PROMPT
Review this pull request for the beamr project (Rust BEAM VM targeting Gleam bytecode).

## PR #${pr_num}: ${title}

Branch: ${head} -> ${base}
Changes: +${additions} -${deletions}

## Changed files
${files}

## Review instructions
The diff is available at: ${REVIEW_DIR}/pr-${pr_num}.diff
Read it with your file tools. Do NOT skip any files in the diff.

## Review criteria
1. Correctness: does the code do what the PR claims?
2. Safety: no unwrap/panic outside tests, explicit errors, no secrets
3. Quality: clippy-clean, conventions followed, no scope creep, no dead code
4. Completeness: are claims substantive (not stubs)? Is new code reachable?
5. File size: no file over 500 lines
6. beamr rules: Term=raw u64 (not Rust enum), no async in hot path, no Meridian deps in core

## Output format
Provide your review as structured findings. For each issue found:
- file path and line range
- severity (blocking / warning / nit)
- what's wrong and how to fix it

End with a summary verdict: APPROVE (no blocking issues), REQUEST_CHANGES (blocking issues found), or COMMENT (non-blocking suggestions only).
PROMPT

    # Run norn reviewer (stderr kept for diagnostics)
    echo "--- NORN REVIEW ---"
    local start
    start=$(date +%s)
    local review_output
    review_output=$(norn -p -q \
        --profile "$PROFILE" \
        --max-turns 30 \
        --timeout 10m \
        -- "$(cat "$REVIEW_DIR/pr-${pr_num}-prompt.md")" 2>&1) || {
        echo "ERROR: norn review failed (exit $?)"
        echo "Output: $review_output"
        return 1
    }
    local elapsed=$(( $(date +%s) - start ))
    echo "Review completed in ${elapsed}s"

    # Guard: don't post empty reviews
    if [ -z "$review_output" ]; then
        echo "WARNING: norn returned empty output, skipping comment"
        return 1
    fi

    # Save review output
    echo "$review_output" > "$review_file"

    # Post as PR comment
    local comment_body="${MARKER}
## Norn Automated Review

${review_output}

---
_Reviewed by norn (${elapsed}s) using profile: norn-reviewer_"

    if gh pr comment "$pr_num" --body "$comment_body"; then
        echo "Posted review comment to PR #${pr_num}"
    else
        echo "WARNING: Failed to post comment (check gh auth / repo permissions)"
        echo "Review saved to: $review_file"
    fi
}

has_norn_review() {
    local pr_num="$1"
    local comments
    comments=$(gh pr view "$pr_num" --json comments --jq '.comments[].body' 2>/dev/null || echo "")
    echo "$comments" | grep -q "$MARKER" && return 0 || return 1
}

poll_prs() {
    echo "=== Polling for unreviewed PRs ==="
    local prs
    prs=$(gh pr list --state open --json number,title --jq '.[] | "\(.number) \(.title)"')

    if [ -z "$prs" ]; then
        echo "No open PRs."
        return 0
    fi

    while IFS= read -r line; do
        local pr_num
        pr_num=$(echo "$line" | awk '{print $1}')
        local pr_title
        pr_title=$(echo "$line" | cut -d' ' -f2-)

        if has_norn_review "$pr_num"; then
            echo "PR #${pr_num} already reviewed, skipping: ${pr_title}"
        else
            echo "PR #${pr_num} needs review: ${pr_title}"
            review_pr "$pr_num" || echo "WARNING: Review of PR #${pr_num} failed"
        fi
    done <<< "$prs"
}

case "${1:-}" in
    --poll)
        poll_prs
        ;;
    --help|-h)
        echo "Usage: $0 <PR_NUMBER> | --poll"
        echo "  <PR_NUMBER>  Review a specific PR"
        echo "  --poll       Check all open PRs, review unreviewed ones"
        ;;
    "")
        echo "Usage: $0 <PR_NUMBER> | --poll"
        exit 1
        ;;
    *)
        review_pr "$1"
        ;;
esac
