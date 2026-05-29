#!/usr/bin/env python3
"""Render a brief JSON file to Markdown.

Resolves checklist (C-number) and story (S-number) references to their
actual text when the cluster directory is available.
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


def _resolve_checklist_ids(
    ids: list[str], lookup: dict[str, str] | None
) -> list[str]:
    if not lookup:
        return [f"{cid}" for cid in ids]
    resolved = []
    for cid in ids:
        text = lookup.get(cid)
        resolved.append(f"{cid} — {text}" if text else cid)
    return resolved


def _resolve_story_ids(
    ids: list[str], lookup: dict[str, dict] | None
) -> list[str]:
    if not lookup:
        return [f"{sid}" for sid in ids]
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


def render(
    data: dict,
    checklist_lookup: dict[str, str] | None = None,
    stories_lookup: dict[str, dict] | None = None,
) -> str:
    lines = []

    # YAML frontmatter
    lines.append("---")
    lines.append("type: brief")
    lines.append(f"id: {data['id']}")
    lines.append(f"cluster: {data['cluster']}")
    lines.append(f"title: {data['title']}")
    lines.append("---")
    lines.append("")

    # Header
    lines.append(f"# {data['id']}: {data['title']}")
    lines.append("")

    # Metadata block
    lines.append(f"> **Cluster:** {data['cluster']}")
    if data.get("depends_on"):
        lines.append(f"> **Depends on:** {', '.join(data['depends_on'])}")
    if data.get("blocked_by"):
        lines.append(f"> **Blocked by:** {', '.join(data['blocked_by'])}")

    if data.get("checklist"):
        resolved = _resolve_checklist_ids(data["checklist"], checklist_lookup)
        if checklist_lookup:
            lines.append("> **Checklist:**")
            for item in resolved:
                lines.append(f"> - {item}")
        else:
            lines.append(f"> **Checklist:** {', '.join(resolved)}")

    if data.get("stories"):
        resolved = _resolve_story_ids(data["stories"], stories_lookup)
        if stories_lookup:
            lines.append("> **Stories:**")
            for item in resolved:
                lines.append(f"> - {item}")
        else:
            lines.append(f"> **Stories:** {', '.join(resolved)}")

    lines.append("")

    # Purpose
    lines.append("## Purpose")
    lines.append("")
    lines.append(data["purpose"])
    lines.append("")

    # Task
    lines.append("## Task")
    lines.append("")
    lines.append(data["task"])
    lines.append("")

    # Requirements
    lines.append("## Requirements")
    lines.append("")

    for req in data["requirements"]:
        lines.append(f"### {req['id']}: {req['title']}")
        lines.append("")
        lines.append(req["spec"])
        lines.append("")

        lines.append("**Acceptance:**")
        for ac in req["acceptance"]:
            lines.append(f"- {ac}")
        lines.append("")

        files = req.get("files", {})
        has_files = any(files.get(k) for k in ("create", "modify", "delete"))
        if has_files:
            lines.append("**Files:**")
            for path in files.get("create", []):
                lines.append(f"- create: {path}")
            for path in files.get("modify", []):
                lines.append(f"- modify: {path}")
            for path in files.get("delete", []):
                lines.append(f"- delete: {path}")
            lines.append("")

        if req.get("checklist"):
            resolved = _resolve_checklist_ids(req["checklist"], checklist_lookup)
            lines.append("**Checklist:**")
            for item in resolved:
                lines.append(f"- {item}")
            lines.append("")

        if req.get("stories"):
            resolved = _resolve_story_ids(req["stories"], stories_lookup)
            lines.append("**Stories:**")
            for item in resolved:
                lines.append(f"- {item}")
            lines.append("")

    # Boundaries
    if data.get("boundaries"):
        lines.append("## Boundaries")
        lines.append("")
        for boundary in data["boundaries"]:
            lines.append(f"- {boundary}")
        lines.append("")

    # Verification
    if data.get("verification"):
        lines.append("## Verification")
        lines.append("")
        for step in data["verification"]:
            lines.append(f"- {step}")
        lines.append("")

    return "\n".join(lines)


def find_cluster_dir(brief_path: Path) -> Path | None:
    """Auto-detect cluster directory from brief file location.

    Expected layout: docs/design/{cluster}/briefs/{brief}.json
    Cluster dir is the parent of briefs/.
    """
    if brief_path.parent.name == "briefs":
        candidate = brief_path.parent.parent
        if (candidate / "checklist.json").exists() or (
            candidate / "stories.json"
        ).exists():
            return candidate
    return None


def load_lookups(
    cluster_dir: Path,
) -> tuple[dict[str, str] | None, dict[str, dict] | None]:
    checklist_lookup = None
    stories_lookup = None

    checklist_path = cluster_dir / "checklist.json"
    if checklist_path.exists():
        with open(checklist_path) as f:
            checklist_lookup = build_checklist_lookup(json.load(f))

    stories_path = cluster_dir / "stories.json"
    if stories_path.exists():
        with open(stories_path) as f:
            stories_lookup = build_stories_lookup(json.load(f))

    return checklist_lookup, stories_lookup


def main():
    parser = argparse.ArgumentParser(description="Render a brief JSON file to Markdown.")
    parser.add_argument("brief", help="Path to brief JSON file.")
    parser.add_argument("output", nargs="?", help="Output path (stdout if omitted).")
    parser.add_argument(
        "--cluster-dir",
        help="Cluster directory containing checklist.json and stories.json. "
        "Auto-detected from brief path if omitted.",
    )
    parser.add_argument(
        "--no-resolve",
        action="store_true",
        help="Skip reference resolution even if cluster files are available.",
    )
    args = parser.parse_args()

    src = Path(args.brief)
    with open(src) as f:
        data = json.load(f)

    checklist_lookup = None
    stories_lookup = None

    if not args.no_resolve:
        cluster_dir = Path(args.cluster_dir) if args.cluster_dir else find_cluster_dir(src)
        if cluster_dir:
            checklist_lookup, stories_lookup = load_lookups(cluster_dir)

    md = render(data, checklist_lookup, stories_lookup)

    if args.output:
        Path(args.output).write_text(md)
        print(f"Rendered to {args.output}", file=sys.stderr)
    else:
        print(md)


if __name__ == "__main__":
    main()
