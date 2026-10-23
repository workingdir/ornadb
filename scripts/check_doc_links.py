#!/usr/bin/env python3
"""Check that every relative Markdown link in tracked Markdown resolves.

Scope: every git-tracked `*.md` file (falls back to a walk when git is absent),
except frozen fixtures (their bytes are pinned by the fixture manifest) and the
archived ADR snapshots under docs/decisions/archive/ (historical records whose
references were correct when archived). Checked: inline and reference-style
relative links, and backticked `docs/...*.md` paths in prose. Links with a URL
scheme, absolute links, and pure in-page anchors are skipped. Fragments are
stripped before the target is checked. Exit 0 when every target exists (or is
in KNOWN_MISSING with a stated reason), 1 otherwise.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
# Inline links `[text](target)` and reference definitions `[id]: target`.
INLINE = re.compile(r"\[[^\]\n]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
DEFINITION = re.compile(r"^\s*\[[^\]\n]+\]:\s*(\S+)", re.MULTILINE)
# Backticked repository paths to Markdown files, e.g. `docs/decisions/0047-x.md`.
BACKTICK_DOC = re.compile(r"`(docs/[^`\s]+\.md)`")
EXCLUDED = (
    "crates/orna-conformance-v1/tests/fixtures/",
    "crates/orna-traceability-v1/tests/fixtures/",
    "docs/decisions/archive/",
)
# Targets absent from this checkout that are named on purpose or predate this
# check. Each entry needs a reason; remove an entry when its target returns.
KNOWN_MISSING = {
    ("docs/conformance/product-acceptance-baseline-gap-epic89-2026-09-28.md", "docs/releases/1.0-product-acceptance.md"): "records that the baseline is ABSENT",
    ("docs/conformance/v1.0.0-release-readiness-audit-epic89-2026-09-28.md", "docs/releases/1.0-product-acceptance.md"): "records that the baseline is ABSENT",
    ("docs/decisions/0064-sys-inspect-core.md", "docs/31-self-inspection.md"): "spec file not in this checkout (predates check)",
    ("docs/decisions/0087-std-data-rows.md", "docs/49-gap-research-and-contract-plan.md"): "spec file not in this checkout (predates check)",
    ("CHANGELOG.md", "examples/accepted-demos.toml"): "examples manifest not in this checkout (predates check)",
}
SCHEME = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")


def targets(text: str) -> list[str]:
    return INLINE.findall(text) + DEFINITION.findall(text)


def tracked_markdown() -> list[Path]:
    try:
        out = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-z", "--", "*.md"],
            check=True, capture_output=True,
        ).stdout.decode("utf-8")
    except (OSError, subprocess.CalledProcessError):
        return sorted(
            m for m in ROOT.rglob("*.md")
            if not str(m.relative_to(ROOT)).startswith(EXCLUDED)
        )
    return sorted(
        ROOT / name for name in out.split("\0")
        if name and not name.startswith(EXCLUDED)
    )


def broken_links(markdown: Path) -> list[str]:
    failures = []
    text = markdown.read_text(encoding="utf-8")
    for doc in BACKTICK_DOC.findall(text):
        if not (ROOT / doc).exists() and (str(markdown.relative_to(ROOT)), doc) not in KNOWN_MISSING:
            failures.append(f"{markdown.relative_to(ROOT)}: missing backticked path -> {doc}")
    for raw in targets(text):
        if raw.startswith("#") or raw.startswith("/") or SCHEME.match(raw):
            continue
        path_part = unquote(raw.split("#", 1)[0])
        if not path_part:
            continue
        resolved = (markdown.parent / path_part).resolve()
        if not resolved.exists() and (str(markdown.relative_to(ROOT)), path_part) not in KNOWN_MISSING:
            failures.append(f"{markdown.relative_to(ROOT)}: broken link -> {raw}")
    return failures


def main() -> int:
    failures: list[str] = []
    for markdown in tracked_markdown():
        failures.extend(broken_links(markdown))
    for failure in failures:
        print(failure, file=sys.stderr)
    if failures:
        print(f"{len(failures)} broken relative link(s)", file=sys.stderr)
        return 1
    print("all relative Markdown links and backticked docs paths resolve")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
