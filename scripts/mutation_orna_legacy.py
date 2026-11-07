#!/usr/bin/env python3
"""Mutation harness for the legacy row-unit reader in scripts/orna_legacy.py.

Each mutant is one exact textual patch to a fresh temporary copy of the
scripts directory. The patch must match exactly once, the mutated module must
compile, and the focused suite (scripts.test_orna_legacy) must fail with real
test failures. A mutant that passes, or that dies at import or compile time, is
not a kill. The unmutated copy must pass first.

Run from anywhere:  python3 scripts/mutation_orna_legacy.py
Exit status is 0 only when the baseline passes and every mutant is killed.
"""

from __future__ import annotations

import py_compile
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SOURCE = Path("scripts") / "orna_legacy.py"
SUITE = "scripts.test_orna_legacy"
FIXTURES = Path("crates") / "orna-cli" / "tests" / "fixtures"

# name -> list of (exact original text, replacement text). Each original must
# occur exactly once in scripts/orna_legacy.py.
MUTANTS: dict[str, list[tuple[str, str]]] = {
    "block-comment-not-nested": [("if depth == 0:", "if True:")],
    "trailing-content-accepted": [('if parser.peek() != "":', "if False:")],
    "interpolation-accepted": [
        ('self.fail("string interpolation is not admitted in a row value")', 'chars.append("{")'),
    ],
    "duplicate-field-accepted": [("if name in out:", "if False:")],
    "surrogate-escape-accepted": [("or 0xD800 <= scalar <= 0xDFFF", "or False")],
    "record-trailing-comma-rejected": [
        (
            '            self.pos += 1\n            if self.peek() == "}":\n                self.pos += 1\n                return out\n',
            '            self.pos += 1\n            if False:\n                self.pos += 1\n                return out\n',
        ),
    ],
    "inline-blob-not-canonical-checked": [
        ("        return orr2_inline_bytes(value)", "        return value.encode()"),
    ],
    "leading-zero-accepted": [
        ('_ROW_INT = re.compile(r"-?(?:0|[1-9][0-9]*)")', '_ROW_INT = re.compile(r"-?[0-9]+")'),
    ],
}


def stage(workdir: Path, source_text: str) -> None:
    """Copies the scripts tree and the test fixtures, then writes the source under test."""
    shutil.copytree(REPO / "scripts", workdir / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
    shutil.copytree(REPO / FIXTURES, workdir / FIXTURES)
    (workdir / SOURCE).write_text(source_text, encoding="utf-8")


def run_suite(workdir: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-B", "-m", "unittest", SUITE],
        cwd=workdir,
        capture_output=True,
        text=True,
    )


def apply(original: str, patches: list[tuple[str, str]]) -> str:
    text = original
    for old, new in patches:
        count = text.count(old)
        if count != 1:
            raise SystemExit(f"patch target occurs {count} times, expected 1: {old[:60]!r}")
        text = text.replace(old, new)
    return text


def main() -> int:
    original = (REPO / SOURCE).read_text(encoding="utf-8")
    failures = 0
    with tempfile.TemporaryDirectory(prefix="orna-legacy-mut-") as tmp:
        base = Path(tmp) / "baseline"
        base.mkdir()
        stage(base, original)
        result = run_suite(base)
        print(f"baseline: exit={result.returncode}")
        if result.returncode != 0:
            print(result.stderr.strip())
            return 1

        for name, patches in MUTANTS.items():
            work = Path(tmp) / name
            work.mkdir()
            mutated = apply(original, patches)
            stage(work, mutated)
            compiled = True
            try:
                py_compile.compile(str(work / SOURCE), doraise=True, cfile=str(work / "mutant.pyc"))
            except py_compile.PyCompileError:
                compiled = False
            if not compiled:
                failures += 1
                print(f"{name}: DID NOT COMPILE (not a kill)")
                continue
            result = run_suite(work)
            summary = result.stderr.strip().splitlines()[-1] if result.stderr.strip() else ""
            killed = result.returncode != 0 and "FAILED (" in result.stderr
            if not killed:
                failures += 1
            verdict = "KILLED" if killed else "SURVIVED"
            print(f"{name}: exit={result.returncode} {verdict} {summary}")

    print(f"mutants: {len(MUTANTS)} failures: {failures}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
