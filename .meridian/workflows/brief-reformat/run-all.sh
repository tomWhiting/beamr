#!/usr/bin/env bash
# Run all 16 lsp-diagnostics brief reformats in parallel.
# All are independent — each touches a different file.
#
# Usage: bash .meridian/workflows/brief-reformat/run-all.sh

WS="2d5fdd51-1f25-45a4-8f86-4d4c978d1355"
AS="c9255b2a-5731-4d17-8124-e3bfa2224186"
DIR="docs/design/lsp-diagnostics/briefs"

# --- Batch 1 (Wave 1 briefs: no implementation deps) ---
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-001.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-005.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-010.json" &

# --- Batch 2 (Wave 2 briefs: depend on Wave 1 for implementation) ---
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-002.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-006.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-007.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-008.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-011.json" &

# --- Batch 3 (Wave 3-6 briefs) ---
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-003.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-004.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-009.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-012.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-013.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-014.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-015.json" &
meridian workflow run brief-reformat --workspace "$WS" --as "$AS" --input brief_path="$DIR/LD-016.json" &

wait
echo "All 16 brief reformats complete."
