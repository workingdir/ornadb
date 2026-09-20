#!/usr/bin/env python3
"""Docker-free checks for the embedded engine output-root policy."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile


REPOSITORY = Path(__file__).resolve().parent.parent
POSTGRESQL = REPOSITORY / "postgresql"
TARGET = REPOSITORY / "target"
ACTIONABLE = (
    "for an external CARGO_TARGET_DIR, set ORNA_POSTGRES_ENGINE_OUTPUT "
    "to an absolute complete engine output directory"
)


def run(target_root: str | Path, goal: str = "validate-target-root") -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["make", "-C", str(POSTGRESQL), goal, f"TARGET_ROOT={target_root}"],
        check=False,
        text=True,
        capture_output=True,
    )


def main() -> None:
    accepted = run(TARGET / "postgresql-target-root-policy" / "out")
    if accepted.returncode != 0:
        raise AssertionError(accepted.stderr)

    with tempfile.TemporaryDirectory(prefix="ornadb-target-root-policy-") as temporary:
        external = Path(temporary) / "cargo" / "build"
        rejected = run(external, "prepare-source")
        if rejected.returncode == 0 or ACTIONABLE not in rejected.stderr:
            raise AssertionError(f"external target was not rejected actionably: {rejected.stderr}")

        relative = run("target/postgresql-target-root-policy")
        if relative.returncode == 0 or "must be absolute" not in relative.stderr:
            raise AssertionError(f"relative target was not rejected: {relative.stderr}")

        link = TARGET / "postgresql-target-root-policy-link"
        if link.exists() or link.is_symlink():
            raise AssertionError(f"test link already exists: {link}")
        os.symlink(external, link)
        try:
            escaped = run(link / "out")
            if escaped.returncode == 0 or ACTIONABLE not in escaped.stderr:
                raise AssertionError(f"symlink target was not rejected actionably: {escaped.stderr}")
        finally:
            link.unlink()


if __name__ == "__main__":
    main()
