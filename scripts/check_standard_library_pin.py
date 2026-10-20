#!/usr/bin/env python3
"""Check the stdlib/std gitlink against ornadb-std main and the Rust source mirror.

The superproject pins `stdlib/std` by commit. This checker proves that the pinned
commit is an ancestor of the submodule's `origin/main`, that the submodule checkout
is exactly that commit with no local edits, and that every file the Rust standard
source mirror includes with `include_str!` is byte-identical to the pinned blob. It
also requires every tracked `.orna` file at the pin to be mirrored or listed in
`UNMIRRORED_SOURCES` with a reason, so a new standard module cannot slip past the
Rust side silently.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parents[1]
SUBMODULE_PATH = "stdlib/std"
MIRROR_SOURCE = WORKSPACE_ROOT / "crates/orna-standard-sources/src/lib.rs"
UPSTREAM_MAIN_REF = "refs/remotes/origin/main"
# Bump when the checker's flags or output formats change, so CI logs show which interface ran.
CHECKER_VERSION = "1"
INCLUDE_PATTERN = re.compile(r'include_str!\(\s*"\.\./\.\./\.\./stdlib/std/([^"]+)"\s*\)')
GITLINK_PATTERN = re.compile(r"^160000 commit ([0-9a-f]{40})\t(.+)$")
SHA_PATTERN = re.compile(r"[0-9a-f]{40}")
# Tracked standard sources that intentionally have no Rust mirror entry. Each needs a reason.
UNMIRRORED_SOURCES: dict[str, str] = {
    ".orna/database.orna": "pinned database root identity; repository metadata, not a standard unit",
    ".orna/format.orna": "repository format marker; repository metadata, not a standard unit",
    "main.orna": "root module with use declarations and version(); not in the Rust reference bundle",
}


class PinError(Exception):
    """A pin, mirror, or upstream invariant does not hold."""


def run_git(cwd: Path, *args: str) -> bytes:
    """Run git in cwd without touching any git configuration and return raw stdout."""
    completed = subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise PinError(f"git {' '.join(args)} failed in {cwd}: {detail}")
    return completed.stdout


def text_git(cwd: Path, *args: str) -> str:
    return run_git(cwd, *args).decode("utf-8").strip()


def gitlink_commit(superproject: Path, submodule_path: str = SUBMODULE_PATH) -> str:
    """Return the commit the superproject HEAD pins for the submodule path."""
    listing = text_git(superproject, "ls-tree", "HEAD", "--", submodule_path)
    match = GITLINK_PATTERN.match(listing)
    if match is None or match.group(2) != submodule_path:
        raise PinError(f"{submodule_path} is not a gitlink at HEAD: {listing or 'missing'}")
    return match.group(1)


def mirrored_paths(mirror_text: str) -> list[str]:
    """Return the stdlib/std-relative paths the Rust mirror includes, in source order."""
    paths = INCLUDE_PATTERN.findall(mirror_text)
    if len(paths) != len(set(paths)):
        duplicates = sorted({path for path in paths if paths.count(path) > 1})
        raise PinError(f"mirror includes the same source twice: {', '.join(duplicates)}")
    return paths


def ensure_upstream_main(submodule: Path) -> None:
    """Fetch ornadb-std main with full history so ancestry can be decided."""
    if text_git(submodule, "rev-parse", "--is-shallow-repository") == "true":
        run_git(
            submodule,
            "fetch",
            "--unshallow",
            "--no-tags",
            "origin",
            f"+refs/heads/main:{UPSTREAM_MAIN_REF}",
        )
    else:
        run_git(submodule, "fetch", "--no-tags", "origin", f"+refs/heads/main:{UPSTREAM_MAIN_REF}")


def check_reachable_from_main(submodule: Path, pin: str) -> None:
    """Require the pinned commit to be an ancestor of the upstream main ref."""
    ensure_upstream_main(submodule)
    completed = subprocess.run(
        ["git", "merge-base", "--is-ancestor", pin, UPSTREAM_MAIN_REF],
        cwd=submodule,
        capture_output=True,
        check=False,
    )
    if completed.returncode == 1:
        raise PinError(f"pinned commit {pin} is not reachable from ornadb-std main")
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise PinError(f"ancestry check failed for {pin}: {detail}")


def check_checkout_matches_pin(submodule: Path, pin: str) -> None:
    """Require the initialised submodule to be exactly the pinned commit and clean."""
    if not submodule.joinpath(".git").exists():
        raise PinError(f"submodule is not initialised at {submodule}; run git submodule update --init")
    head = text_git(submodule, "rev-parse", "HEAD")
    if head != pin:
        raise PinError(f"submodule HEAD {head} differs from the pinned gitlink {pin}")
    dirty = text_git(submodule, "status", "--porcelain", "--untracked-files=all")
    if dirty:
        raise PinError(f"submodule has local changes at the pin:\n{dirty}")


def tracked_orna_sources(submodule: Path, pin: str) -> list[str]:
    listing = text_git(submodule, "ls-tree", "-r", "--name-only", pin)
    return sorted(path for path in listing.splitlines() if path.endswith(".orna"))


def check_mirror_bytes(submodule: Path, pin: str, mirrored: list[str]) -> None:
    """Require each mirrored include to be byte-identical to its blob at the pin."""
    tracked = set(tracked_orna_sources(submodule, pin))
    for relative in mirrored:
        if relative not in tracked:
            raise PinError(f"mirror includes {relative}, which is not tracked at the pin")
        blob = run_git(submodule, "show", f"{pin}:{relative}")
        on_disk = (submodule / relative).read_bytes()
        if blob != on_disk:
            raise PinError(f"mirror source {relative} differs from the pinned blob")


def check_every_source_accounted_for(submodule: Path, pin: str, mirrored: list[str]) -> None:
    """Require each tracked .orna file to be mirrored or explicitly excused."""
    covered = set(mirrored) | set(UNMIRRORED_SOURCES)
    unaccounted = [path for path in tracked_orna_sources(submodule, pin) if path not in covered]
    if unaccounted:
        raise PinError(
            "tracked standard sources are neither mirrored nor excused: " + ", ".join(unaccounted)
        )
    stale = sorted(path for path in UNMIRRORED_SOURCES if path in set(mirrored))
    if stale:
        raise PinError("excused sources are mirrored; drop them from UNMIRRORED_SOURCES: " + ", ".join(stale))


def run_checks(superproject: Path, mirror_source: Path, submodule_path: str = SUBMODULE_PATH) -> str:
    """Run every pin invariant and return a one-line summary on success."""
    submodule = superproject / submodule_path
    pin = gitlink_commit(superproject, submodule_path)
    if not SHA_PATTERN.fullmatch(pin):
        raise PinError(f"gitlink is not a full commit identifier: {pin}")
    check_checkout_matches_pin(submodule, pin)
    check_reachable_from_main(submodule, pin)
    mirrored = mirrored_paths(mirror_source.read_text(encoding="utf-8"))
    check_mirror_bytes(submodule, pin, mirrored)
    check_every_source_accounted_for(submodule, pin, mirrored)
    return (
        f"standard library pin {pin} is reachable from ornadb-std main; "
        f"{len(mirrored)} mirrored sources match the pinned blobs"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="verify the pin and mirror (default)")
    parser.add_argument(
        "--version",
        action="store_true",
        help=f"print the checker interface version ({CHECKER_VERSION}) and exit",
    )
    arguments = parser.parse_args()
    if arguments.version:
        print(CHECKER_VERSION)
        return 0
    try:
        print(run_checks(WORKSPACE_ROOT, MIRROR_SOURCE))
    except PinError as error:
        print(f"ornadb: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
