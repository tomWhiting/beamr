#!/usr/bin/env python3
"""Render a checklist JSON file to Markdown."""
import json
import sys
from pathlib import Path


def render(data: dict) -> str:
    lines = [f"# {data['cluster'].title()} — Checklist", ""]

    for section in data["sections"]:
        lines.append(f"## {section['name']}")
        lines.append("")
        for item in section["items"]:
            check = "x" if item.get("done", False) else " "
            lines.append(f"- [{check}] **{item['id']}** — {item['text']}")
        lines.append("")

    return "\n".join(lines)


def main():
    if len(sys.argv) < 2:
        print("Usage: render-checklist.py <checklist.json> [output.md]", file=sys.stderr)
        sys.exit(1)

    src = Path(sys.argv[1])
    with open(src) as f:
        data = json.load(f)

    md = render(data)

    if len(sys.argv) >= 3:
        Path(sys.argv[2]).write_text(md)
        print(f"Rendered to {sys.argv[2]}")
    else:
        print(md)


if __name__ == "__main__":
    main()
