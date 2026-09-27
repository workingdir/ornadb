#!/usr/bin/env python3
"""Validate submitted 24-hour compact-throughput evidence against its workload."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any


FIXTURE = Path(__file__).with_name("fixtures") / "compact_throughput_profile.orna"
MIN_DURATION_SECONDS = 86_400
SAMPLES_PER_SECOND = 10_000
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


class CriterionFailure(ValueError):
    """Well-formed submitted claims that do not meet the throughput gate."""


def _timestamp(profile: dict[str, Any], field: str) -> datetime:
    value = profile.get(field)
    if not isinstance(value, str) or not value:
        raise ValueError(f"{field} must be a UTC timestamp")
    # Require an explicit UTC marker; naive/local timestamps cannot establish
    # the duration of the submitted run unambiguously.
    normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        parsed = datetime.fromisoformat(normalized)
    except ValueError as error:
        raise ValueError(f"{field} must be a UTC timestamp") from error
    if parsed.tzinfo is None or parsed.utcoffset() != timezone.utc.utcoffset(parsed):
        raise ValueError(f"{field} must be a UTC timestamp")
    return parsed


def _positive_integer(profile: dict[str, Any], field: str) -> int:
    value = profile.get(field)
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"{field} must be a positive integer")
    return value


def _fixture_sha256(path: Path) -> str:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError as error:
        raise ValueError(f"workload fixture cannot be read: {path}") from error


def validate_compact_throughput(profile: Any, fixture_path: Path = FIXTURE) -> tuple[str, float, int, str]:
    """Validate claims and return hardware, duration, samples, and fixture digest."""
    if not isinstance(profile, dict):
        raise ValueError("profile must be a JSON object")

    hardware = profile.get("hardware_identity")
    if not isinstance(hardware, str) or not hardware.strip():
        raise ValueError("hardware_identity must be a nonempty string")

    start = _timestamp(profile, "run_start_utc")
    end = _timestamp(profile, "run_end_utc")
    duration = (end - start).total_seconds()
    if duration < MIN_DURATION_SECONDS:
        raise CriterionFailure("run duration must be at least 86400 seconds")

    samples = _positive_integer(profile, "sample_count")
    if samples < SAMPLES_PER_SECOND * duration:
        raise CriterionFailure("sample_count must be at least 10000 times elapsed seconds")

    declared_digest = profile.get("workload_fixture_sha256")
    if not isinstance(declared_digest, str) or not SHA256_RE.fullmatch(declared_digest):
        raise ValueError("workload_fixture_sha256 must be 64 lowercase hexadecimal characters")
    actual_digest = _fixture_sha256(fixture_path)
    if declared_digest != actual_digest:
        raise ValueError("workload_fixture_sha256 does not match the workload fixture")

    return hardware.strip(), duration, samples, actual_digest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile", type=Path, help="JSON file containing submitted throughput evidence")
    args = parser.parse_args(argv)
    try:
        profile = json.loads(args.profile.read_text(encoding="utf-8"))
        hardware, duration, samples, digest = validate_compact_throughput(profile)
    except CriterionFailure as error:
        print(f"compact throughput evidence claims: FAIL: {error}")
        return 1
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"compact throughput profile: INVALID: {error}", file=sys.stderr)
        return 2

    print(
        "compact throughput evidence claims: "
        f"hardware {hardware}; duration {duration:g} seconds; "
        f"samples {samples}; workload SHA-256 {digest}: PASS"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
