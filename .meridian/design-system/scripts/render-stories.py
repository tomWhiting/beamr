#!/usr/bin/env python3
"""Render a user stories JSON file to Markdown."""
import json
import sys
from pathlib import Path


def render(data: dict) -> str:
    lines = [f"# {data['cluster'].title()} — User Stories", ""]

    for persona in data["personas"]:
        lines.append(f"## {persona['name']} — {persona['role']}")
        lines.append("")
        for story in persona["stories"]:
            lines.append(f"**{story['id']}.** {story['text']}")
            lines.append("")

    return "\n".join(lines)


def main():
    if len(sys.argv) < 2:
        print("Usage: render-stories.py <stories.json> [output.md]", file=sys.stderr)
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
