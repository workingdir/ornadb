#!/usr/bin/env python3
"""Check Markdown link targets and reject the literal token BROKEN in documents.

Link check scope: every git-tracked `*.md` file (falls back to a walk when git
is absent), minus the exclusions below. Checked forms:

- inline `[text](target)` and reference definitions `[id]: target`;
- backticked `docs/...*.md` paths in prose (the only backticked paths checked).

Exclusions, all deliberate:

- frozen fixtures under `crates/orna-conformance-v1/tests/fixtures/` and
  `crates/orna-traceability-v1/tests/fixtures/` (their bytes are pinned by the
  fixture manifest and must not change; excluded from both checks);
- the archived ADR snapshots under `docs/decisions/archive/` (historical
  records whose references were correct when archived; link check only);
- targets with a URL scheme, absolute targets (`/...`), and pure in-page
  anchors (`#...`); fragments on other targets are stripped and not validated;
- backticked paths outside `docs/` (for example `crates/...`, `spec/...`) are
  not checked; they are prose references, not links.

A target is accepted when it exists or appears in KNOWN_MISSING with a reason.

BROKEN guard: the literal token `BROKEN` must not appear in any tracked `*.md`
outside the frozen fixture trees (this covers `CHANGELOG.md` and `README.md`).
A file is exempt only when its `origin/main` version already contains the
token, so an inherited occurrence does not block unrelated work.

Exit 0 when every check passes, 1 otherwise.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
# Inline links `[text](target)` and reference definitions `[id]: target`.
INLINE = re.compile(r"\[[^\]\n]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
DEFINITION = re.compile(r"^\s*\[[^\]\n]+\]:\s*(\S+)", re.MULTILINE)
# Backticked repository paths to Markdown files, e.g. `docs/decisions/0047-x.md`.
BACKTICK_DOC = re.compile(r"`(docs/[^`\s]+\.md)`")
SCHEME = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")
BROKEN_TOKEN = "BROKEN"
FIXTURES = (
    "crates/orna-conformance-v1/tests/fixtures/",
    "crates/orna-traceability-v1/tests/fixtures/",
)
LINK_EXCLUDED = FIXTURES + ("docs/decisions/archive/",)
# Targets absent from this checkout that are named on purpose or predate this
# check. Each entry needs a reason; remove an entry when its target returns.
KNOWN_MISSING = {
    ("docs/conformance/product-acceptance-baseline-gap-epic89-2026-09-28.md", "docs/releases/1.0-product-acceptance.md"): "records that the baseline is ABSENT",
    ("docs/conformance/v1.0.0-release-readiness-audit-epic89-2026-09-28.md", "docs/releases/1.0-product-acceptance.md"): "records that the baseline is ABSENT",
    ("docs/decisions/0064-sys-inspect-core.md", "docs/31-self-inspection.md"): "spec file not in this checkout (predates check)",
    ("docs/decisions/0087-std-data-rows.md", "docs/49-gap-research-and-contract-plan.md"): "spec file not in this checkout (predates check)",
    ("CHANGELOG.md", "examples/accepted-demos.toml"): "examples manifest not in this checkout (predates check)",
}


def tracked_markdown() -> list[str]:
    """Repo-relative paths of Markdown files in scope for the token guard."""
    try:
        out = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-z", "--", "*.md"],
            check=True, capture_output=True,
        ).stdout.decode("utf-8")
        names = [name for name in out.split("\0") if name]
    except (OSError, subprocess.CalledProcessError):
        names = [str(p.relative_to(ROOT)) for p in ROOT.rglob("*.md")]
    return sorted(name for name in names if not name.startswith(FIXTURES))


def origin_main_text(rel: str) -> str | None:
    """Return the file's origin/main contents, or None when unavailable."""
    try:
        return subprocess.run(
            ["git", "-C", str(ROOT), "show", f"origin/main:{rel}"],
            check=True, capture_output=True,
        ).stdout.decode("utf-8", errors="replace")
    except (OSError, subprocess.CalledProcessError):
        return None


def targets(text: str) -> list[str]:
    return INLINE.findall(text) + DEFINITION.findall(text)


def link_failures(rel: str, text: str) -> list[str]:
    failures = []
    markdown = ROOT / rel
    for doc in BACKTICK_DOC.findall(text):
        if not (ROOT / doc).exists() and (rel, doc) not in KNOWN_MISSING:
            failures.append(f"{rel}: missing backticked path -> {doc}")
    for raw in targets(text):
        if raw.startswith("#") or raw.startswith("/") or SCHEME.match(raw):
            continue
        path_part = unquote(raw.split("#", 1)[0])
        if not path_part:
            continue
        resolved = (markdown.parent / path_part).resolve()
        if not resolved.exists() and (rel, path_part) not in KNOWN_MISSING:
            failures.append(f"{rel}: broken link -> {raw}")
    return failures


def token_failures(rel: str, text: str) -> list[str]:
    if BROKEN_TOKEN not in text:
        return []
    inherited = origin_main_text(rel)
    if inherited is not None and BROKEN_TOKEN in inherited:
        return []
    return [f"{rel}: literal token {BROKEN_TOKEN} (absent from origin/main)"]


def main() -> int:
    failures: list[str] = []
    for rel in tracked_markdown():
        text = (ROOT / rel).read_text(encoding="utf-8")
        if not rel.startswith(LINK_EXCLUDED):
            failures.extend(link_failures(rel, text))
        failures.extend(token_failures(rel, text))
    for failure in failures:
        print(failure, file=sys.stderr)
    if failures:
        print(f"{len(failures)} failure(s): broken link or BROKEN token", file=sys.stderr)
        return 1
    print("all relative Markdown links and backticked docs paths resolve; no BROKEN token")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
