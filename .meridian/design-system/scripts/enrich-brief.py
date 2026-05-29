#!/usr/bin/env python3
"""Resolve checklist and story references in a brief JSON.

Reads brief JSON from stdin. Outputs enriched JSON with resolved text
for each C/S code, suitable for the workflow merge directive.

Usage:
  cat brief.json | python3 enrich-brief.py --cluster-dir docs/design/messaging/
  echo '{...}' | python3 enrich-brief.py --cluster-dir .meridian/test-fixtures/
"""
import argparse
import json
import sys
from pathlib import Path


def build_checklist_lookup(data: dict) -> dict[str, str]:
    lookup = {}
    for section in data.get("sections", []):
        for item in section.get("items", []):
            lookup[item["id"]] = item["text"]
    return lookup


def build_stories_lookup(data: dict) -> dict[str, dict]:
    lookup = {}
    for persona in data.get("personas", []):
        for story in persona.get("stories", []):
            lookup[story["id"]] = {
                "text": story["text"],
                "persona": persona["name"],
                "role": persona["role"],
            }
    return lookup


def resolve_checklist(ids: list[str], lookup: dict[str, str]) -> list[str]:
    resolved = []
    for cid in ids:
        text = lookup.get(cid)
        resolved.append(f"{cid} — {text}" if text else cid)
    return resolved


def resolve_stories(ids: list[str], lookup: dict[str, dict]) -> list[str]:
    resolved = []
    for sid in ids:
        entry = lookup.get(sid)
        if entry:
            resolved.append(
                f"{sid} ({entry['persona']}, {entry['role']}) — {entry['text']}"
            )
        else:
            resolved.append(sid)
    return resolved


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cluster-dir", required=True)
    args = parser.parse_args()

    brief = json.load(sys.stdin)
    cluster_dir = Path(args.cluster_dir)

    cl_lookup = {}
    st_lookup = {}

    cl_path = cluster_dir / "checklist.json"
    if cl_path.exists():
        with open(cl_path) as f:
            cl_lookup = build_checklist_lookup(json.load(f))

    st_path = cluster_dir / "stories.json"
    if st_path.exists():
        with open(st_path) as f:
            st_lookup = build_stories_lookup(json.load(f))

    enrichments = []
    for req in brief.get("requirements", []):
        entry = {"id": req["id"]}
        if req.get("checklist"):
            entry["checklist_resolved"] = resolve_checklist(req["checklist"], cl_lookup)
        if req.get("stories"):
            entry["stories_resolved"] = resolve_stories(req["stories"], st_lookup)
        enrichments.append(entry)

    output = {
        "brief_checklist": resolve_checklist(brief.get("checklist", []), cl_lookup),
        "brief_stories": resolve_stories(brief.get("stories", []), st_lookup),
        "enrichments": enrichments,
    }

    json.dump(output, sys.stdout)


if __name__ == "__main__":
    main()
