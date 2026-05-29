#!/usr/bin/env python3
"""Render all JSON documents in a cluster directory to Markdown.

Usage: render-cluster.py <cluster-dir>

Expects:
  <cluster-dir>/checklist.json
  <cluster-dir>/stories.json
  <cluster-dir>/briefs/*.json

Produces:
  <cluster-dir>/CHECKLIST.md
  <cluster-dir>/USER-STORIES.md
  <cluster-dir>/briefs/*.md (with resolved checklist/story references)
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from importlib import import_module

render_checklist = import_module("render-checklist").render
render_stories = import_module("render-stories").render

render_brief_mod = import_module("render-brief")
render_brief = render_brief_mod.render
build_checklist_lookup = render_brief_mod.build_checklist_lookup
build_stories_lookup = render_brief_mod.build_stories_lookup


def main():
    if len(sys.argv) < 2:
        print("Usage: render-cluster.py <cluster-dir>", file=sys.stderr)
        sys.exit(1)

    cluster_dir = Path(sys.argv[1])
    rendered = 0

    checklist_lookup = None
    stories_lookup = None

    checklist_json = cluster_dir / "checklist.json"
    if checklist_json.exists():
        with open(checklist_json) as f:
            checklist_data = json.load(f)
        checklist_lookup = build_checklist_lookup(checklist_data)
        out = cluster_dir / "CHECKLIST.md"
        out.write_text(render_checklist(checklist_data))
        print(f"  Rendered {out}")
        rendered += 1

    stories_json = cluster_dir / "stories.json"
    if stories_json.exists():
        with open(stories_json) as f:
            stories_data = json.load(f)
        stories_lookup = build_stories_lookup(stories_data)
        out = cluster_dir / "USER-STORIES.md"
        out.write_text(render_stories(stories_data))
        print(f"  Rendered {out}")
        rendered += 1

    briefs_dir = cluster_dir / "briefs"
    if briefs_dir.is_dir():
        for brief_json in sorted(briefs_dir.glob("*.json")):
            with open(brief_json) as f:
                data = json.load(f)
            out = brief_json.with_suffix(".md")
            out.write_text(render_brief(data, checklist_lookup, stories_lookup))
            print(f"  Rendered {out}")
            rendered += 1

    if rendered == 0:
        print(f"  No JSON files found in {cluster_dir}", file=sys.stderr)
        sys.exit(1)

    print(f"Done. {rendered} file(s) rendered.")


if __name__ == "__main__":
    main()
