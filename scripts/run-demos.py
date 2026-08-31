#!/usr/bin/env python3
"""Run the accepted API demos in manifest order."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import stat
import subprocess
import sys
import tomllib
from typing import Any


LOG_PREFIX = "[demo]"
REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = REPOSITORY_ROOT / "examples" / "accepted-demos.toml"
CARGO_TIMEOUT_SECONDS = 600
DEMO_MODES = frozenset({"source-check", "offline-test", "compose-only"})


class DemoError(Exception):
    """A manifest or source-file error that should stop the demo run."""


@dataclass(frozen=True)
class Demo:
    """One accepted API demo registered in the manifest."""

    name: str
    mode: str
    source: str
    description: str
    test: str | None


def load_demos() -> list[Demo]:
    """Load and validate the ordered demo registry."""
    try:
        with MANIFEST_PATH.open("rb") as manifest_file:
            manifest = tomllib.load(manifest_file)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise DemoError(f"could not read {MANIFEST_PATH}: {exc}") from exc

    raw_demos = manifest.get("demos")
    if not isinstance(raw_demos, list) or not raw_demos:
        raise DemoError("manifest must contain a non-empty [[demos]] array")

    demos: list[Demo] = []
    names: set[str] = set()
    for index, raw_demo in enumerate(raw_demos, start=1):
        if not isinstance(raw_demo, dict):
            raise DemoError(f"manifest demo {index} must be a table")
        name = _required_string(raw_demo, "name", index)
        mode = _required_string(raw_demo, "mode", index)
        source = _required_string(raw_demo, "source", index)
        description = _required_string(raw_demo, "description", index)
        if name in names:
            raise DemoError(f"manifest demo {index} repeats name {name!r}")
        if mode not in DEMO_MODES:
            allowed_modes = ", ".join(sorted(DEMO_MODES))
            raise DemoError(
                f"manifest demo {name!r} has unsupported mode {mode!r}; "
                f"expected one of {allowed_modes}"
            )
        if Path(source).is_absolute():
            raise DemoError(f"manifest demo {name!r} must use a repository-relative source path")
        test = raw_demo.get("test")
        if mode == "offline-test":
            if not isinstance(test, str) or not test.strip():
                raise DemoError(f"manifest demo {name!r} requires a test for offline-test mode")
            if test != test.strip():
                raise DemoError(f"manifest demo {name!r} test may not have surrounding whitespace")
        elif test is not None:
            raise DemoError(f"manifest demo {name!r} may not define test for {mode} mode")
        names.add(name)
        demos.append(
            Demo(name=name, mode=mode, source=source, description=description, test=test)
        )
    return demos


def _required_string(table: dict[str, Any], field: str, index: int) -> str:
    value = table.get(field)
    if not isinstance(value, str) or not value.strip():
        raise DemoError(f"manifest demo {index} requires a non-empty string {field!r}")
    if value != value.strip():
        raise DemoError(f"manifest demo {index} {field!r} may not have surrounding whitespace")
    return value


def resolve_source(demo: Demo) -> Path:
    """Resolve a manifest source and reject paths escaping the repository."""
    try:
        source_path = (REPOSITORY_ROOT / demo.source).resolve()
        source_path.relative_to(REPOSITORY_ROOT)
    except (OSError, RuntimeError, ValueError) as exc:
        raise DemoError(
            f"source {demo.source!r} for {demo.name!r} is outside the repository"
        ) from exc
    return source_path


def validate_source(demo: Demo) -> Path:
    """Resolve one source and verify that it is a regular UTF-8 file."""
    source_path = resolve_source(demo)
    try:
        source_mode = source_path.stat().st_mode
    except OSError as exc:
        raise DemoError(f"source {demo.source!r} is not readable: {exc}") from exc
    if not stat.S_ISREG(source_mode):
        raise DemoError(f"source {demo.source!r} is not a regular file")
    try:
        source_path.read_bytes().decode("utf-8")
    except OSError as exc:
        raise DemoError(f"source {demo.source!r} is not readable: {exc}") from exc
    except UnicodeDecodeError as exc:
        raise DemoError(f"source {demo.source!r} is not valid UTF-8: {exc}") from exc
    return source_path


def relative_source(source_path: Path) -> str:
    """Return a stable repository-relative path for the Cargo invocation."""
    return source_path.relative_to(REPOSITORY_ROOT).as_posix()


def cargo_command(demo: Demo, source_path: Path) -> list[str]:
    """Build the explicit Cargo command for a runnable demo mode."""
    if demo.mode == "source-check":
        return [
            "cargo",
            "--locked",
            "--offline",
            "run",
            "-p",
            "orna-server",
            "--",
            "source",
            "check",
            relative_source(source_path),
        ]
    if demo.mode == "offline-test":
        # Focused offline proofs are exact test names; no Compose service is needed.
        return [
            "cargo",
            "--locked",
            "--offline",
            "test",
            "-p",
            "orna-server",
            "--test",
            "standard_database",
            demo.test or "",
            "--",
            "--exact",
        ]
    raise DemoError(f"demo {demo.name!r} is not runnable in {demo.mode} mode")


def log(message: str, *, error: bool = False) -> None:
    """Write a flushed status line with the demo prefix."""
    print(f"{LOG_PREFIX} {message}", file=sys.stderr if error else sys.stdout, flush=True)


def emit_process_output(demo: Demo, label: str, output: str | bytes | None) -> None:
    """Keep diagnostic output prefixed so the runner's output remains readable."""
    if not output:
        return
    if isinstance(output, bytes):
        output = output.decode("utf-8", errors="replace")
    for line in output.splitlines():
        log(f"{demo.name}: {label}: {line}", error=True)


def run_demo(demo: Demo) -> bool:
    """Validate and run one demo, returning false after its first failure."""
    log(f"{demo.name}: start ({demo.mode})")
    try:
        source_path = validate_source(demo)
    except DemoError as exc:
        log(f"{demo.name}: fail: {exc}", error=True)
        return False

    if demo.mode == "compose-only":
        log(f"{demo.name}: skip: compose-only (not run by demo-check)")
        return True

    try:
        command = cargo_command(demo, source_path)
    except DemoError as exc:
        log(f"{demo.name}: fail: {exc}", error=True)
        return False
    try:
        completed = subprocess.run(
            command,
            cwd=REPOSITORY_ROOT,
            check=False,
            capture_output=True,
            stdin=subprocess.DEVNULL,
            text=True,
            timeout=CARGO_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as exc:
        emit_process_output(demo, "stdout", exc.stdout)
        emit_process_output(demo, "stderr", exc.stderr)
        log(
            f"{demo.name}: fail: cargo timed out after {CARGO_TIMEOUT_SECONDS} seconds",
            error=True,
        )
        return False
    except OSError as exc:
        log(f"{demo.name}: fail: could not start cargo: {exc}", error=True)
        return False

    if completed.returncode != 0:
        emit_process_output(demo, "stdout", completed.stdout)
        emit_process_output(demo, "stderr", completed.stderr)
        log(f"{demo.name}: fail: cargo exited with status {completed.returncode}", error=True)
        return False

    log(f"{demo.name}: pass")
    return True


def parse_arguments(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list",
        action="store_true",
        dest="list_only",
        help="list registered demos and modes without invoking Cargo",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_arguments(argv)
    try:
        demos = load_demos()
        # Resolve paths even in list mode so an unsafe manifest cannot be hidden by --list.
        for demo in demos:
            resolve_source(demo)
    except DemoError as exc:
        log(f"manifest: fail: {exc}", error=True)
        return 1

    if arguments.list_only:
        for demo in demos:
            log(f"{demo.name} [{demo.mode}]: {demo.source} - {demo.description}")
        return 0

    for demo in demos:
        if not run_demo(demo):
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
