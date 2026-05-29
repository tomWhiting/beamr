#!/usr/bin/env python3
"""Check coverage of checklist items and user stories across briefs.

Usage: check-coverage.py <cluster-dir>

Reports:
  - Checklist items not assigned to any brief
  - User stories not assigned to any brief
  - Checklist items assigned to multiple briefs (for awareness, not an error)
  - Brief dependency chain (ordered by depends_on)
"""
import json
import sys
from collections import defaultdict
from pathlib import Path


def main():
    if len(sys.argv) < 2:
        print("Usage: check-coverage.py <cluster-dir>", file=sys.stderr)
        sys.exit(1)

    cluster_dir = Path(sys.argv[1])

    all_checklist_ids = set()
    all_story_ids = set()

    checklist_json = cluster_dir / "checklist.json"
    if checklist_json.exists():
        with open(checklist_json) as f:
            data = json.load(f)
        for section in data["sections"]:
            for item in section["items"]:
                all_checklist_ids.add(item["id"])

    stories_json = cluster_dir / "stories.json"
    if stories_json.exists():
        with open(stories_json) as f:
            data = json.load(f)
        for persona in data["personas"]:
            for story in persona["stories"]:
                all_story_ids.add(story["id"])

    brief_checklist = defaultdict(list)
    brief_stories = defaultdict(list)
    briefs = {}

    briefs_dir = cluster_dir / "briefs"
    if briefs_dir.is_dir():
        for brief_json in sorted(briefs_dir.glob("*.json")):
            with open(brief_json) as f:
                data = json.load(f)
            brief_id = data["id"]
            briefs[brief_id] = data

            for cid in data.get("checklist", []):
                brief_checklist[cid].append(brief_id)
            for sid in data.get("stories", []):
                brief_stories[sid].append(brief_id)

    assigned_checklist = set(brief_checklist.keys())
    assigned_stories = set(brief_stories.keys())

    unassigned_checklist = sorted(all_checklist_ids - assigned_checklist,
                                  key=lambda x: int(x[1:]))
    unassigned_stories = sorted(all_story_ids - assigned_stories,
                                 key=lambda x: int(x[1:]))
    unknown_checklist = sorted(assigned_checklist - all_checklist_ids)
    unknown_stories = sorted(assigned_stories - all_story_ids)
    multi_checklist = {k: v for k, v in brief_checklist.items() if len(v) > 1}

    print(f"Cluster: {cluster_dir.name}")
    print(f"  Checklist items: {len(all_checklist_ids)}")
    print(f"  User stories: {len(all_story_ids)}")
    print(f"  Briefs: {len(briefs)}")
    print()

    if unassigned_checklist:
        print(f"  Checklist items NOT in any brief ({len(unassigned_checklist)}):")
        for cid in unassigned_checklist:
            print(f"    - {cid}")
        print()

    if unassigned_stories:
        print(f"  User stories NOT in any brief ({len(unassigned_stories)}):")
        for sid in unassigned_stories:
            print(f"    - {sid}")
        print()

    if unknown_checklist:
        print(f"  WARNING: briefs reference unknown checklist items:")
        for cid in unknown_checklist:
            print(f"    - {cid} (in {', '.join(brief_checklist[cid])})")
        print()

    if unknown_stories:
        print(f"  WARNING: briefs reference unknown story IDs:")
        for sid in unknown_stories:
            print(f"    - {sid} (in {', '.join(brief_stories[sid])})")
        print()

    if multi_checklist:
        print(f"  Checklist items in multiple briefs ({len(multi_checklist)}):")
        for cid, bids in sorted(multi_checklist.items()):
            print(f"    - {cid}: {', '.join(bids)}")
        print()

    if not unassigned_checklist and not unassigned_stories:
        print("  All items covered.")

    # Dependency order
    if briefs:
        print()
        print("  Brief dependencies:")
        for bid, bdata in sorted(briefs.items()):
            deps = bdata.get("depends_on", [])
            dep_str = f" (depends on: {', '.join(deps)})" if deps else ""
            print(f"    {bid}: {bdata['title']}{dep_str}")


if __name__ == "__main__":
    main()
