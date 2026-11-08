#!/usr/bin/env python3
"""Mutation harness for the repository-format-1 walker in scripts/orna_extract.py.

Each mutant is one exact textual patch to a fresh temporary copy of the
scripts directory. The patch must match exactly once, the mutated module must
compile, and the focused suite (scripts.test_orna_extract) must fail with real
test failures. A mutant that passes, or that dies at import or compile time, is
not a kill. The unmutated copy must pass first.

Run from anywhere:  python3 scripts/mutation_orna_extract_legacy.py
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
SOURCE = Path("scripts") / "orna_extract.py"
SUITE = "scripts.test_orna_extract"
FIXTURES = Path("crates") / "orna-cli" / "tests" / "fixtures"
PROFILES = Path("crates") / "orna-conformance" / "tests" / "fixtures" / "reference" / "profiles"

# name -> list of (exact original text, replacement text). Each original must
# occur exactly once in scripts/orna_extract.py.
MUTANTS: dict[str, list[tuple[str, str]]] = {
    "reserved-name-escape-dropped": [("and not (index == 0 and reserved)", "and True")],
    "trailing-dot-kept-literal": [('literal_end = len(raw.rstrip(b"."))', "literal_end = len(raw)")],
    "hyphen-not-readable": [
        (
            'b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-"',
            'b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._"',
        )
    ],
    "decode-canonical-check-dropped": [("if key_path_encode(key) != component:", "if False:")],
    "decode-length-limit-dropped": [('if len(component.encode("utf-8")) > KEY_COMPONENT_LIMIT:', "if False:")],
    "row-trailing-content-accepted": [
        ('    if parser.peek() != "":\n        parser.fail("trailing content after the row record")', "    if False:\n        parser.fail(\"trailing content after the row record\")"),
    ],
    "missing-field-accepted": [
        ('        if name not in record:\n            fail(f"{where}: missing field {name}")', '        if False:\n            fail(f"{where}: missing field {name}")'),
    ],
    "unknown-field-accepted": [
        ('        if name not in fields:\n            fail(f"{where}: unknown field {name}")', '        if False:\n            fail(f"{where}: unknown field {name}")'),
    ],
    "int-field-accepts-bool": [
        ("valid = isinstance(value, int) and not isinstance(value, bool)", "valid = isinstance(value, int)"),
    ],
    "ascii-case-collision-accepted": [("if folded_seen.get(folded, component) != component:", "if False:")],
    "duplicate-table-accepted": [
        ('if directory in schemas or any(table["row_directory"] == directory for table in tables):', "if False:"),
    ],
    "composite-key-not-refused": [('    if "," in key_text:', "    if False:")],
    "key-type-not-checked": [('    if key.group(2) != "Str":', "    if False:")],
    "row-directory-not-namespaced": [
        ('directory = f"{module_directory}/{name}" if module_directory else name', "directory = name"),
    ],
    "row-sha256-blanked": [('"sha256": hashlib.sha256(data).hexdigest(),', '"sha256": "",')],
    "other-repository-format-accepted": [
        ('if format_record.get("repository_format") != LEGACY_REPOSITORY_FORMAT:', "if False:"),
    ],
    "editable-contents-reported-extracted": [
        ('"contents": "extracted" if policy["placement"] == "compact" else "editable-policy-only",', '"contents": "extracted",'),
    ],
    "compact-storage-rows-dropped": [('if entry["placement"] == "compact":', "if False:")],
    "compact-read-error-swallowed": [('            fail(f"{where}: compact table {table}: {error}")', "            return []")],
    "blob-content-reported": [('{"blob": {"length": len(item), "sha256": hashlib.sha256(item).hexdigest()}}', '{"blob": item.hex()}')],
    "blob-sha256-blanked": [('"sha256": hashlib.sha256(item).hexdigest()', '"sha256": ""')],
    "tag-form-dropped": [('{"tag": item.number, "value": value(item.value)}', "value(item.value)")],
    "policy-extra-field-accepted": [
        ('if set(policy) != {"placement", "table"} or policy["placement"]', 'if policy["placement"]'),
    ],
    "rows-not-sorted": [('table["rows"] = sorted(rows, key=lambda row: row["key"].encode("utf-8"))', 'table["rows"] = rows')],
}


def stage(workdir: Path, source_text: str) -> None:
    """Copies the scripts tree, the test fixtures and the media profile, then writes the source under test."""
    shutil.copytree(REPO / "scripts", workdir / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
    shutil.copytree(REPO / FIXTURES, workdir / FIXTURES)
    shutil.copytree(REPO / PROFILES, workdir / PROFILES)
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
    with tempfile.TemporaryDirectory(prefix="orna-extract-legacy-mut-") as tmp:
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
