#!/usr/bin/env bash
# benchmark.sh — Run onatopp-dev-norn pipeline through norn headless CLI
# Usage: ./benchmark.sh <brief.json> [design.json] [worktree-name] [checklist.json] [stories.json] [notify]
# Creates a worktree, runs scout->dev->review (max 2 review attempts), reports timings.
# Notifies the named member via collective DM when complete.

set -euo pipefail

BRIEF="$1"
DESIGN="${2:-}"
WT_NAME="${3:-benchmark-$(date +%Y%m%d-%H%M%S)}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
WT_DIR="$REPO_ROOT/.yggdrasil-worktrees/$WT_NAME"
WT_BRANCH="benchmark/$WT_NAME"

echo "=== Benchmark: norn headless CLI ==="
echo "Brief: $BRIEF"
echo "Worktree: $WT_DIR"
echo "Branch: $WT_BRANCH"

git worktree add -b "$WT_BRANCH" "$WT_DIR" HEAD
echo "Worktree created."

[[ "$BRIEF" = /* ]] || BRIEF="$REPO_ROOT/$BRIEF"
[[ -z "$DESIGN" || "$DESIGN" = /* ]] || DESIGN="$REPO_ROOT/$DESIGN"

cd "$WT_DIR"
OUTDIR=".benchmark"
mkdir -p "$OUTDIR"
echo ""

# Optional checklist/stories/notify inputs (4th, 5th, 6th args)
CHECKLIST="${4:-}"
STORIES="${5:-}"
NOTIFY="${6:-}"
[[ -z "$CHECKLIST" || "$CHECKLIST" = /* ]] || CHECKLIST="$REPO_ROOT/$CHECKLIST"
[[ -z "$STORIES" || "$STORIES" = /* ]] || STORIES="$REPO_ROOT/$STORIES"

# Extract brief fields
BRIEF_ID=$(jq -r '.id' "$BRIEF")
BRIEF_TITLE=$(jq -r '.title' "$BRIEF")
BRIEF_PURPOSE=$(jq -r '.purpose // ""' "$BRIEF")
BRIEF_TASK=$(jq -r '.task // ""' "$BRIEF")
CLUSTER=$(jq -r '.cluster // ""' "$BRIEF")

# Resolve C# and S# references — embed actual text from checklist/stories
# into the brief requirements (same as workflow.rhai resolve_checklist/resolve_stories)
RESOLVED_BRIEF="$OUTDIR/brief-resolved.json"
cp "$BRIEF" "$RESOLVED_BRIEF"
if [[ -n "$CHECKLIST" && -f "$CHECKLIST" ]]; then
    jq --slurpfile cl "$CHECKLIST" '
        ($cl[0].sections // [] | [.[].items[]] | map({(.id): .text}) | add // {}) as $lookup |
        .requirements |= [.[] | .checklist = [(.checklist // [])[] |
            ($lookup[.] // null) as $text |
            if $text then {id: ., text: $text} else {id: ., text: ""} end
        ]]
    ' "$RESOLVED_BRIEF" > "$OUTDIR/brief-tmp.json" && mv "$OUTDIR/brief-tmp.json" "$RESOLVED_BRIEF"
fi
if [[ -n "$STORIES" && -f "$STORIES" ]]; then
    jq --slurpfile st "$STORIES" '
        ($st[0].personas // [] | [.[].stories[]] | map({(.id): .text}) | add // {}) as $lookup |
        .requirements |= [.[] | .stories = [(.stories // [])[] |
            ($lookup[.] // null) as $text |
            if $text then {id: ., text: $text} else {id: ., text: ""} end
        ]]
    ' "$RESOLVED_BRIEF" > "$OUTDIR/brief-tmp.json" && mv "$OUTDIR/brief-tmp.json" "$RESOLVED_BRIEF"
fi
BRIEF="$RESOLVED_BRIEF"

# Build design context if provided
DESIGN_CONTEXT=""
if [[ -n "$DESIGN" && -f "$DESIGN" ]]; then
    INTENTION=$(jq -r '.intention // ""' "$DESIGN")
    if [[ -n "$INTENTION" ]]; then
        DESIGN_CONTEXT+="**Intention:** $INTENTION\n\n"
    fi
    DESIGN_CONTEXT+=$(jq -r '
        if .constraints and (.constraints | length) > 0 then
            "**Constraints:**\n" + ([.constraints[] | "- \(.id): \(.text // .description // "")"] | join("\n")) + "\n\n"
        else "" end
    ' "$DESIGN")
    DESIGN_CONTEXT+=$(jq -r '
        if .goals and (.goals | length) > 0 then
            "**Goals:**\n" + ([.goals[] | "- \(.)"] | join("\n")) + "\n\n"
        else "" end
    ' "$DESIGN")
    DESIGN_ANCHOR=$(jq -r '.design_anchor // ""' "$BRIEF")
    if [[ -n "$DESIGN_ANCHOR" ]]; then
        DESIGN_CONTEXT+=$(jq -r --arg anchor "$DESIGN_ANCHOR" '
            if .decisions and (.decisions | length) > 0 then
                "**Key Decisions:**\n" + ([.decisions[] | select(.status == "active") | select($anchor | contains(.id)) | "- \(.id): \(.title)" + (if .choice then " — \(.choice)" else "" end)] | join("\n")) + "\n\n"
            else "" end
        ' "$DESIGN")
    fi
fi

DESIGN_FILE_REF=""
if [[ -n "$CLUSTER" ]]; then
    DESIGN_FILE_REF="Full design document: docs/design/$CLUSTER/DESIGN.md"
fi

# Render requirements as markdown (with resolved C#/S# text)
REQUIREMENTS=$(jq -r '
    [.requirements[] |
        "### \(.id): \(.title)\n\n\(.spec)\n\n" +
        (if .files.create and (.files.create | length) > 0 then "Create: " + (.files.create | join(" ")) + "\n" else "" end) +
        (if .files.modify and (.files.modify | length) > 0 then "Modify: " + (.files.modify | join(" ")) + "\n" else "" end) +
        (if .files.delete and (.files.delete | length) > 0 then "Delete: " + (.files.delete | join(" ")) + "\n" else "" end) +
        "\nAcceptance:\n" + ([.acceptance[] | "- \(.)"] | join("\n")) + "\n" +
        (if .checklist and (.checklist | length) > 0 then
            "\nChecklist:\n" + ([.checklist[] | if type == "object" then "- \(.id): \(.text)" else "- \(.)" end] | join("\n")) + "\n"
        else "" end) +
        (if .stories and (.stories | length) > 0 then
            "\nStories:\n" + ([.stories[] | if type == "object" then "- \(.id): \(.text)" else "- \(.)" end] | join("\n")) + "\n"
        else "" end)
    ] | join("\n")
' "$BRIEF")

BOUNDARIES=$(jq -r '
    if .boundaries and (.boundaries | length) > 0 then
        "## Boundaries\n\n" + ([.boundaries[] | "- \(.)"] | join("\n")) + "\n\n"
    else "" end
' "$BRIEF")

# =====================================================================
# SCHEMAS (same as workflow)
# =====================================================================

cat > "$OUTDIR/scout-schema.json" << 'SCHEMA'
{
    "type": "object",
    "properties": {
        "summary": {"type": "string", "description": "2-3 sentences orienting the implementer."},
        "enrichments": {"type": "array", "description": "One entry per R#.", "items": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "R# id from the brief."},
                "files": {"type": "array", "items": {"type": "string"}, "description": "Key files relevant to this R# (path:line-range). 2-5 per R#."},
                "context": {"type": "array", "items": {"type": "string"}, "description": "Key findings: conventions to match, type signatures, gotchas. 2-4 per R#."},
                "approach": {"type": "string", "description": "How to implement this R#."},
                "notes": {"type": "string", "description": "Edge cases, gotchas. Empty if none."}
            },
            "required": ["id", "files", "context", "approach", "notes"],
            "additionalProperties": false
        }},
        "verification": {"type": "array", "items": {"type": "string"}, "description": "Concrete checks to run after implementation."}
    },
    "required": ["summary", "enrichments", "verification"],
    "additionalProperties": false
}
SCHEMA

cat > "$OUTDIR/dev-schema.json" << 'SCHEMA'
{
    "type": "object",
    "properties": {
        "summary": {"type": "string", "description": "1-2 sentences on what was done."},
        "commit_message": {"type": "string", "description": "Conventional-commits style."},
        "enrichments": {"type": "array", "description": "One entry per R#.", "items": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "R# id."},
                "status": {"type": "string", "enum": ["implemented", "blocked"]},
                "files_changed": {"type": "array", "items": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}, "change": {"type": "string", "enum": ["created", "modified", "deleted"]}, "note": {"type": "string"}},
                    "required": ["path", "change", "note"], "additionalProperties": false
                }},
                "how": {"type": "string", "description": "How this requirement was met."},
                "deviation": {"type": "string", "description": "Empty if followed plan."},
                "checklist": {"type": "array", "items": {
                    "type": "object", "properties": {"id": {"type": "string"}, "done": {"type": "boolean"}, "note": {"type": "string"}},
                    "required": ["id", "done", "note"], "additionalProperties": false
                }},
                "stories": {"type": "array", "items": {
                    "type": "object", "properties": {"id": {"type": "string"}, "satisfied": {"type": "boolean"}, "note": {"type": "string"}},
                    "required": ["id", "satisfied", "note"], "additionalProperties": false
                }}
            },
            "required": ["id", "status", "files_changed", "how", "deviation", "checklist", "stories"],
            "additionalProperties": false
        }},
        "attestation": {"type": "object", "properties": {
            "no_panics": {"type": "boolean"}, "no_unsafe": {"type": "boolean"},
            "boundaries_respected": {"type": "boolean"}, "tests_pass": {"type": "boolean"}
        }, "required": ["no_panics", "no_unsafe", "boundaries_respected", "tests_pass"], "additionalProperties": false}
    },
    "required": ["summary", "commit_message", "enrichments", "attestation"],
    "additionalProperties": false
}
SCHEMA

cat > "$OUTDIR/review-schema.json" << 'SCHEMA'
{
    "type": "object",
    "properties": {
        "summary": {"type": "string"},
        "commit_message": {"type": "string"},
        "enrichments": {"type": "array", "description": "One entry per R#.", "items": {
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "alignment": {"type": "string", "enum": ["aligned", "drifted", "fixed"]},
                "acceptance_met": {"type": "boolean"},
                "checklist": {"type": "array", "items": {"type": "string"}},
                "stories": {"type": "array", "items": {"type": "string"}},
                "issues": {"type": "array", "items": {"type": "string"}},
                "fixes": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["id", "alignment", "acceptance_met", "checklist", "stories", "issues", "fixes"],
            "additionalProperties": false
        }},
        "verification": {"type": "array", "items": {
            "type": "object",
            "properties": {"criterion": {"type": "string"}, "passed": {"type": "boolean"}, "note": {"type": "string"}},
            "required": ["criterion", "passed", "note"], "additionalProperties": false
        }}
    },
    "required": ["summary", "commit_message", "enrichments", "verification"],
    "additionalProperties": false
}
SCHEMA

# =====================================================================
# STEP 1: SCOUT
# =====================================================================

cat > "$OUTDIR/scout-prompt.md" << PROMPT
Explore the codebase and gather implementation context for each R# in this brief. You are read-only — do not modify files.

For each R#, find:
- 2-5 key files the implementer should look at (with line ranges)
- Conventions to match (sibling patterns, naming, error handling)
- A concrete implementation approach
- Any gotchas or edge cases the brief might not have considered

The implementing agent has the same tools you do — focus on saving them time, not cataloguing every file. Be concise.

## Brief: $BRIEF_ID — $BRIEF_TITLE

$BRIEF_PURPOSE

## Requirements

$REQUIREMENTS

$BOUNDARIES

$DESIGN_FILE_REF
PROMPT

echo "--- SCOUT ---"
SCOUT_START=$(date +%s)
norn -p --no-session \
    --profile norn-codebase-explorer \
    -s "$OUTDIR/scout-schema.json" \
    -f json \
    -o "$OUTDIR/scout-output.json" \
    -- "$(cat "$OUTDIR/scout-prompt.md")"
SCOUT_END=$(date +%s)
SCOUT_ELAPSED=$((SCOUT_END - SCOUT_START))
echo "Scout: ${SCOUT_ELAPSED}s"

# Extract scout structured output
SCOUT_SUMMARY=$(jq -r '.output.summary // "no summary"' "$OUTDIR/scout-output.json")
echo "Scout summary: $SCOUT_SUMMARY"

# Build enriched requirements for dev
SCOUT_ENRICHMENTS=$(jq -r '.output.enrichments' "$OUTDIR/scout-output.json")
SCOUT_VERIFICATION=$(jq -r '.output.verification // []' "$OUTDIR/scout-output.json")

# =====================================================================
# STEP 2: DEV
# =====================================================================

# Build dev prompt with scout enrichments inlined
cat > "$OUTDIR/dev-prompt.md" << PROMPT
Implement every R# in this brief. Run cargo check, cargo clippy -- -D warnings, and cargo test on affected crates. Fix any failures before submitting.

## Brief: $BRIEF_ID — $BRIEF_TITLE

$BRIEF_TASK

## Requirements

$REQUIREMENTS

## Scout Context

$(echo "$SCOUT_ENRICHMENTS" | jq -r '.[] | "### \(.id)\nFiles: \(.files | join(", "))\nApproach: \(.approach)\nNotes: \(.notes)\n"')

## Verification

$(echo "$SCOUT_VERIFICATION" | jq -r '.[] | "- \(.)"')

$BOUNDARIES

For each R#, report: status, files changed, how satisfied, any deviation. For each C# and S# assigned to the R#, report whether delivered. Attest: no panics/unwraps in library code, no unsafe, boundaries respected, tests pass.
PROMPT

echo ""
echo "--- DEV ---"
DEV_START=$(date +%s)
norn -p --no-session \
    --profile norn-developer \
    -s "$OUTDIR/dev-schema.json" \
    -f json \
    -o "$OUTDIR/dev-output.json" \
    -- "$(cat "$OUTDIR/dev-prompt.md")"
DEV_END=$(date +%s)
DEV_ELAPSED=$((DEV_END - DEV_START))
echo "Dev: ${DEV_ELAPSED}s"

DEV_SUMMARY=$(jq -r '.output.summary // "no summary"' "$OUTDIR/dev-output.json")
echo "Dev summary: $DEV_SUMMARY"

# Commit after dev
COMMIT_MSG=$(jq -r '.output.commit_message // "feat: implement brief"' "$OUTDIR/dev-output.json")
if [[ -n "$(git status --porcelain)" ]]; then
    git add -A && git commit -m "$COMMIT_MSG"
    echo "Committed: $COMMIT_MSG"
fi

# =====================================================================
# STEP 3: REVIEW
# =====================================================================

DEV_ENRICHMENTS=$(jq -r '.output.enrichments' "$OUTDIR/dev-output.json")
DEV_ATTESTATION=$(jq -r '.output.attestation | "panics=\(.no_panics), unsafe=\(.no_unsafe), boundaries=\(.boundaries_respected), tests=\(.tests_pass)"' "$OUTDIR/dev-output.json")

cat > "$OUTDIR/review-prompt.md" << PROMPT
Review and harden the implementation. You have two jobs:

1. HARDEN: Fix naming drift, missing error handling, convention violations, edge cases. Use Edit and Write directly.
2. REVIEW: Verify acceptance criteria for each R#. Check the ACTUAL CODE (use git diff HEAD~1), not the dev summary. Tick checklist items. Confirm stories.

## Brief: $BRIEF_ID — $BRIEF_TITLE

## Requirements

$REQUIREMENTS

## Dev Results

$(echo "$DEV_ENRICHMENTS" | jq -r '.[] | "### \(.id): \(.status) — \(.how)\nFiles: \(if .files_changed then (.files_changed | map(.path) | join(", ")) else "none" end)\n"')

## Verification Criteria

$(echo "$SCOUT_VERIFICATION" | jq -r '.[] | "- \(.)"')

Dev attestation: $DEV_ATTESTATION

PROMPT

echo ""
echo "--- REVIEW ---"
REVIEW_START=$(date +%s)
norn -p --no-session \
    --profile norn-reviewer \
    -s "$OUTDIR/review-schema.json" \
    -f json \
    -o "$OUTDIR/review-output.json" \
    -- "$(cat "$OUTDIR/review-prompt.md")"
REVIEW_END=$(date +%s)
REVIEW_ELAPSED=$((REVIEW_END - REVIEW_START))
echo "Review: ${REVIEW_ELAPSED}s"

REVIEW_SUMMARY=$(jq -r '.output.summary // "no summary"' "$OUTDIR/review-output.json")
echo "Review: $REVIEW_SUMMARY"

# Commit review fixes
if [[ -n "$(git status --porcelain)" ]]; then
    REVIEW_COMMIT_MSG=$(jq -r '.output.commit_message // "fix: address review findings"' "$OUTDIR/review-output.json")
    git add -A && git commit -m "$REVIEW_COMMIT_MSG"
    echo "Review committed: $REVIEW_COMMIT_MSG"
fi

# =====================================================================
# SUMMARY
# =====================================================================

TOTAL=$((SCOUT_ELAPSED + DEV_ELAPSED + REVIEW_ELAPSED))
echo ""
echo "=== BENCHMARK COMPLETE ==="
echo "Scout:  ${SCOUT_ELAPSED}s"
echo "Dev:    ${DEV_ELAPSED}s"
echo "Review: ${REVIEW_ELAPSED}s"
echo "Total:  ${TOTAL}s"
echo "Branch: $WT_BRANCH"
echo "Worktree: $WT_DIR"
echo ""
echo "To inspect: cd $WT_DIR"
echo "To clean:   cargo clean --manifest-path $WT_DIR/Cargo.toml && git worktree remove $WT_DIR && git branch -D $WT_BRANCH"

# Notify via collective DM
if [[ -n "$NOTIFY" ]]; then
    REPORT="Benchmark complete: $BRIEF_ID — $BRIEF_TITLE
Scout: ${SCOUT_ELAPSED}s | Dev: ${DEV_ELAPSED}s | Review: ${REVIEW_ELAPSED}s | Total: ${TOTAL}s
Branch: $WT_BRANCH
Worktree: $WT_DIR"
    collective send --as Meridian --to "$NOTIFY" --subject "benchmark complete: $BRIEF_ID" --message "$REPORT" 2>/dev/null || true
fi
