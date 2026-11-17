#!/usr/bin/env python3
"""Plan or apply a stdlib/std pin bump: regenerate every pinned-source artefact.

A bump changes four artefacts that must agree with the pinned standard library:
the Rust mirror's module rows (crates/orna-standard-sources), the mirror's module
count (the orna-evaluator array literal and the test assertions that count it),
the CURRENT_STD_PIN constant (crates/orna-intrinsics/src/pin.rs), and the fixture
manifest (scripts/fixture-manifest.sha256, refreshed by check_fixture_manifest.py).

The script never moves the submodule and never edits the gitlink. Check the target
commit out inside stdlib/std, then run --apply: it refuses unless that checkout is
exactly the target and clean. The default is a dry run that writes nothing.

Exit status: 0 no changes, or applied; 1 blocked by a mirror invariant; 2 usage or
precondition failure.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from scripts.check_standard_library_pin import (
    MIRROR_SOURCE,
    MODULE_ROW_PATTERN,
    SHA_PATTERN,
    SUBMODULE_PATH,
    UNMIRRORED_SOURCES,
    WORKSPACE_ROOT,
    PinError,
    gitlink_commit,
    mirrored_paths,
    text_git,
    tracked_orna_sources,
)

PIN_SOURCE = WORKSPACE_ROOT / "crates/orna-intrinsics/src/pin.rs"
EVALUATOR_SOURCE = WORKSPACE_ROOT / "crates/orna-evaluator/src/lib.rs"
FIXTURE_MANIFEST_TOOL = WORKSPACE_ROOT / "scripts/check_fixture_manifest.py"
MIRROR_MACRO_OPEN = "reference_standard_modules! {"
PIN_PATTERN = re.compile(r'(pub const CURRENT_STD_PIN: &str = ")([0-9a-f]{40})(";)')
EVALUATOR_COUNT_PATTERN = re.compile(r"(pub fn reference_standard_sources\(\) -> \[\(String, String\); )(\d+)(\])")
# Count assertions over the mirror, e.g. `sources.len(), 77)` or `reference_standard_sources_v1().len(), 77)`.
COUNT_ASSERT_PATTERN = re.compile(r"((?:\bsources|\breference_standard_sources(?:_v1)?\(\))\.len\(\), *)(\d+)(\))")
COUNT_SOURCE_MARKER = "reference_standard_sources"
ROW_LINE_PATTERN = re.compile(r"^\s*(REFERENCE_STANDARD_\w+_PATH_V1)\s*=")

CHECKLIST = (
    "next steps:",
    "  1. stage the gitlink in the superproject: git add stdlib/std",
    "  2. python3 scripts/check_standard_library_pin.py --check",
    "  3. python3 -m unittest scripts.test_standard_library_pin scripts.test_bump_std scripts.test_fixture_manifest",
    "  4. python3 scripts/check_fixture_manifest.py --check",
    "  5. focused cargo proof named in docs/process/std-bump.md (not run by this script)",
    "  6. update CHANGELOG and any stale pin mentions listed above",
)


class UsageError(Exception):
    """The bump cannot start: wrong std repo, unknown commit, or dirty or mismatched checkout."""


@dataclass(frozen=True)
class Row:
    name: str
    relative: str


@dataclass
class Plan:
    current_pin: str
    target_pin: str
    rows_before: int
    removed: list[Row]
    added: list[Row]
    old_count: int
    new_count: int
    count_sites: list[str]
    edits: dict[Path, str]
    blockers: list[str]

    @property
    def changed(self) -> bool:
        return bool(self.edits)


def row_const(relative: str) -> str:
    """Return the path constant a new mirror row for `relative` uses."""
    stem = relative.removesuffix(".orna")
    return "REFERENCE_STANDARD_" + re.sub(r"[^0-9A-Za-z]+", "_", stem).strip("_").upper() + "_PATH_V1"


def _show(path: Path) -> str:
    try:
        return str(path.relative_to(WORKSPACE_ROOT))
    except ValueError:
        return str(path)


def render_mirror(mirror_text: str, removed: list[Row], added: list[Row]) -> str:
    """Drop the removed rows and append the added rows at the end of the macro invocation."""
    removed_names = {row.name for row in removed}
    kept = [
        line
        for line in mirror_text.splitlines(keepends=True)
        if not (match := ROW_LINE_PATTERN.match(line)) or match.group(1) not in removed_names
    ]
    start = next(index for index, line in enumerate(kept) if line.strip() == MIRROR_MACRO_OPEN)
    close = next(index for index in range(start + 1, len(kept)) if kept[index].rstrip("\r\n") == "}")
    inserted = [f'    {row.name} = "std/{row.relative}";\n' for row in added]
    return "".join(kept[:close] + inserted + kept[close:])


def rewrite_counts(text: str, old: int, new: int) -> tuple[str, list[int]]:
    """Rewrite mirror count assertions that still carry the old count; return updated text and line numbers."""
    lines: list[int] = []

    def replace(match: re.Match[str]) -> str:
        if int(match.group(2)) != old:
            return match.group(0)
        lines.append(text.count("\n", 0, match.start()) + 1)
        return f"{match.group(1)}{new}{match.group(3)}"

    updated = COUNT_ASSERT_PATTERN.sub(replace, text)
    return updated, lines


def plan_bump(
    *,
    current_pin: str,
    target_pin: str,
    target_paths: list[str],
    mirror_text: str,
    mirror_path: Path,
    evaluator_text: str,
    evaluator_path: Path,
    pin_text: str,
    pin_path: Path,
    scanned: Mapping[Path, str],
    excused: Mapping[str, str] = UNMIRRORED_SOURCES,
) -> Plan:
    """Compute the artefact edits for a bump without touching the filesystem.

    `scanned` maps other tracked Rust sources to their text; it is searched for
    references to removed constants and for mirror count assertions.
    """
    blockers: list[str] = []
    edits: dict[Path, str] = {}
    rows = [Row(f"REFERENCE_STANDARD_{stem}_PATH_V1", relative) for stem, relative in MODULE_ROW_PATTERN.findall(mirror_text)]
    mirrored = set(mirrored_paths(mirror_text))
    target = set(target_paths)

    removed = [row for row in rows if row.relative not in target]
    added = [Row(row_const(relative), relative) for relative in target_paths if relative not in mirrored and relative not in excused]

    for relative in excused:
        if relative not in target:
            blockers.append(
                f"UNMIRRORED_SOURCES names {relative}, which is not tracked at the target; "
                "remove it from scripts/check_standard_library_pin.py by hand"
            )
    names = {row.name for row in rows} - {row.name for row in removed}
    for row in added:
        if row.name in names:
            blockers.append(f"{row.relative} needs constant {row.name}, which already names another module")
        names.add(row.name)

    for row in removed:
        pattern = re.compile(rf"\b{re.escape(row.name)}\b")
        for path, text in scanned.items():
            if pattern.search(text):
                blockers.append(f"{row.name} ({row.relative}) is still used by {_show(path)}")

    evaluator_match = EVALUATOR_COUNT_PATTERN.search(evaluator_text)
    if evaluator_match is None:
        blockers.append(f"no reference_standard_sources() array literal in {_show(evaluator_path)}")
        old_count = len(rows)
    else:
        old_count = int(evaluator_match.group(2))
        if old_count != len(rows):
            blockers.append(f"{_show(evaluator_path)} literal {old_count} differs from the {len(rows)} mirror rows")
    new_count = len(rows) - len(removed) + len(added)

    if removed or added:
        updated_mirror = render_mirror(mirror_text, removed, added)
        rendered = {relative for _, relative in MODULE_ROW_PATTERN.findall(updated_mirror)}
        if rendered != target - set(excused):
            blockers.append("rendered mirror does not list exactly the tracked non-excused sources")
        edits[mirror_path] = updated_mirror

    if evaluator_match is not None and old_count != new_count:
        edits[evaluator_path] = EVALUATOR_COUNT_PATTERN.sub(rf"\g<1>{new_count}\g<3>", evaluator_text, count=1)

    count_sites: list[str] = []
    if old_count != new_count:
        for path, text in scanned.items():
            if COUNT_SOURCE_MARKER not in text:
                continue
            updated, lines = rewrite_counts(text, old_count, new_count)
            if lines:
                edits[path] = updated
                count_sites.extend(f"{_show(path)}:{line}" for line in lines)

    if PIN_PATTERN.search(pin_text) is None:
        blockers.append(f"no CURRENT_STD_PIN constant in {_show(pin_path)}")
    else:
        updated_pin = PIN_PATTERN.sub(rf"\g<1>{target_pin}\g<3>", pin_text, count=1)
        if updated_pin != pin_text:
            edits[pin_path] = updated_pin

    return Plan(
        current_pin=current_pin,
        target_pin=target_pin,
        rows_before=len(rows),
        removed=removed,
        added=added,
        old_count=old_count,
        new_count=new_count,
        count_sites=count_sites,
        edits=edits,
        blockers=blockers,
    )


def std_repo_head(std_repo: Path) -> str:
    """Return HEAD of the standard library checkout, refusing a directory that is not its own repository."""
    if not std_repo.joinpath(".git").exists():
        raise UsageError(f"{std_repo} is not an initialised checkout; run: git submodule update --init {SUBMODULE_PATH}")
    toplevel = Path(text_git(std_repo, "rev-parse", "--show-toplevel")).resolve()
    if toplevel != std_repo.resolve():
        raise UsageError(f"{std_repo} is not its own repository (toplevel is {toplevel}); initialise {SUBMODULE_PATH}")
    return text_git(std_repo, "rev-parse", "HEAD")


def resolve_target(std_repo: Path, requested: str | None) -> str:
    ref = requested or "HEAD"
    try:
        sha = text_git(std_repo, "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}")
    except PinError as error:
        raise UsageError(f"cannot resolve {ref} in {std_repo}: {error}") from error
    if not SHA_PATTERN.fullmatch(sha):
        raise UsageError(f"{ref} did not resolve to a full commit identifier: {sha}")
    return sha


def rust_sources() -> list[Path]:
    listing = text_git(WORKSPACE_ROOT, "ls-files", "crates")
    skipped = {MIRROR_SOURCE, EVALUATOR_SOURCE, PIN_SOURCE}
    paths = [WORKSPACE_ROOT / line for line in listing.splitlines() if line.endswith(".rs")]
    return [path for path in paths if path not in skipped]


def stale_pin_mentions(old_pin: str) -> list[str]:
    completed = subprocess.run(
        ["git", "grep", "-l", "-F", old_pin, "--", ".", ":(exclude)CHANGELOG.md"],
        cwd=WORKSPACE_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode == 1:
        return []
    if completed.returncode != 0:
        raise UsageError(f"git grep for the old pin failed: {completed.stderr.strip()}")
    return sorted(completed.stdout.split())


def render_report(plan: Plan, head: str, stale: list[str]) -> list[str]:
    lines = [
        f"stdlib/std pin: {plan.current_pin} -> {plan.target_pin}",
        f"stdlib/std checkout HEAD: {head}",
        f"mirror rows: {plan.rows_before} -> {plan.rows_before - len(plan.removed) + len(plan.added)} "
        f"(+{len(plan.added)} -{len(plan.removed)})",
    ]
    lines += [f'  + {row.name} = "std/{row.relative}";' for row in plan.added]
    lines += [f'  - {row.name} = "std/{row.relative}";' for row in plan.removed]
    lines.append(f"module count literal: {plan.old_count} -> {plan.new_count}")
    lines.append(f"test count assertions to rewrite: {len(plan.count_sites)}")
    lines += [f"  {site}" for site in plan.count_sites]
    lines.append(f"CURRENT_STD_PIN: {'changes' if PIN_SOURCE in plan.edits else 'unchanged'}")
    lines.append("files to write:")
    lines += [f"  {_show(path)}" for path in sorted(plan.edits)]
    if stale and plan.changed:
        lines.append("old pin still mentioned (review by hand; not rewritten):")
        lines += [f"  {path}" for path in stale]
    return lines


def run(arguments: argparse.Namespace) -> int:
    std_repo = arguments.std_repo.resolve()
    head = std_repo_head(std_repo)
    target = resolve_target(std_repo, arguments.std_commit)
    current = gitlink_commit(WORKSPACE_ROOT)
    target_paths = tracked_orna_sources(std_repo, target)
    scanned = {path: path.read_text(encoding="utf-8") for path in rust_sources()}
    plan = plan_bump(
        current_pin=current,
        target_pin=target,
        target_paths=target_paths,
        mirror_text=MIRROR_SOURCE.read_text(encoding="utf-8"),
        mirror_path=MIRROR_SOURCE,
        evaluator_text=EVALUATOR_SOURCE.read_text(encoding="utf-8"),
        evaluator_path=EVALUATOR_SOURCE,
        pin_text=PIN_SOURCE.read_text(encoding="utf-8"),
        pin_path=PIN_SOURCE,
        scanned=scanned,
    )
    if plan.blockers:
        for blocker in plan.blockers:
            print(f"blocked: {blocker}", file=sys.stderr)
        return 1
    mode = "apply" if arguments.apply else "dry-run"
    if not plan.changed:
        print(f"{mode}: no changes; stdlib/std {target} is already reflected in every pinned-source artefact")
        return 0
    stale = stale_pin_mentions(current) if current != target else []
    print(f"{mode}: planned changes")
    for line in render_report(plan, head, stale):
        print(line)
    if not arguments.apply:
        print("dry-run: nothing written; check the target out in stdlib/std and re-run with --apply")
        print("\n".join(CHECKLIST))
        return 0
    if head != target:
        raise UsageError(f"stdlib/std HEAD {head} must be the target {target}; check the target out first")
    dirty = text_git(std_repo, "status", "--porcelain", "--untracked-files=all")
    if dirty:
        raise UsageError(f"stdlib/std has local changes; commit or discard them first:\n{dirty}")
    for path, text in plan.edits.items():
        path.write_text(text, encoding="utf-8")
    refresh = subprocess.run(
        [sys.executable, str(FIXTURE_MANIFEST_TOOL), "--update"],
        cwd=WORKSPACE_ROOT,
        check=False,
    )
    if refresh.returncode != 0:
        print("blocked: check_fixture_manifest.py --update failed; the edits above are written", file=sys.stderr)
        return 1
    print("applied: artefacts written and fixture manifest refreshed")
    print("\n".join(CHECKLIST))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--std-repo",
        type=Path,
        default=WORKSPACE_ROOT / SUBMODULE_PATH,
        help="standard library checkout to read (default: stdlib/std)",
    )
    parser.add_argument("--std-commit", help="target commit in the checkout (default: its HEAD)")
    parser.add_argument(
        "--apply",
        action="store_true",
        help="write the edits and refresh the fixture manifest (default: dry-run)",
    )
    arguments = parser.parse_args(argv)
    try:
        return run(arguments)
    except UsageError as error:
        print(f"ornadb: {error}", file=sys.stderr)
        return 2
    except PinError as error:
        print(f"blocked: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
