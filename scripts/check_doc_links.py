#!/usr/bin/env python3
"""Check that every relative Markdown link under docs/ resolves to an existing path.

Links with a URL scheme, absolute links, and pure in-page anchors are skipped.
Fragments are stripped before the target path is checked. Exit 0 when every
relative link resolves, 1 otherwise.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
# Inline links `[text](target)` and reference definitions `[id]: target`.
INLINE = re.compile(r"\[[^\]\n]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
DEFINITION = re.compile(r"^\s*\[[^\]\n]+\]:\s*(\S+)", re.MULTILINE)
SCHEME = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")


def targets(text: str) -> list[str]:
    return INLINE.findall(text) + DEFINITION.findall(text)


def broken_links(markdown: Path) -> list[str]:
    failures = []
    text = markdown.read_text(encoding="utf-8")
    for raw in targets(text):
        if raw.startswith("#") or raw.startswith("/") or SCHEME.match(raw):
            continue
        path_part = unquote(raw.split("#", 1)[0])
        if not path_part:
            continue
        resolved = (markdown.parent / path_part).resolve()
        if not resolved.exists():
            failures.append(f"{markdown.relative_to(ROOT)}: broken link -> {raw}")
    return failures


def main() -> int:
    failures: list[str] = []
    for markdown in sorted(DOCS.rglob("*.md")):
        failures.extend(broken_links(markdown))
    for failure in failures:
        print(failure, file=sys.stderr)
    if failures:
        print(f"{len(failures)} broken relative link(s)", file=sys.stderr)
        return 1
    print("all relative docs links resolve")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
