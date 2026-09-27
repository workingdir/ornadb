#!/usr/bin/env python3
"""Validate submitted evidence for compact row loss and duplication."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


def _logical_rows(value: Any, label: str) -> list[str]:
    if not isinstance(value, list):
        raise ValueError(f"{label} must be an array of logical row IDs")
    if not value:
        raise ValueError(f"{label} must contain at least one logical row ID")
    if any(not isinstance(row_id, str) or not row_id for row_id in value):
        raise ValueError(f"{label} must contain non-empty string logical row IDs")
    return value


def validate_compact_integrity(profile: Any) -> tuple[int, int]:
    """Validate row identity evidence; return acknowledged and compact row counts."""
    if not isinstance(profile, dict):
        raise ValueError("profile must be a JSON object")

    acknowledged = _logical_rows(profile.get("acknowledged_rows"), "acknowledged_rows")
    compact = _logical_rows(profile.get("compact_rows"), "compact_rows")

    if len(set(acknowledged)) != len(acknowledged):
        raise ValueError("acknowledged_rows contains duplicate logical row IDs")
    if len(set(compact)) != len(compact):
        raise ValueError("compact_rows contains duplicate logical row IDs")

    lost = set(acknowledged) - set(compact)
    if lost:
        raise ValueError(f"compact output is missing {len(lost)} acknowledged logical row(s)")
    extra = set(compact) - set(acknowledged)
    if extra:
        raise ValueError(f"compact output contains {len(extra)} unacknowledged logical row(s)")
    return len(acknowledged), len(compact)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile", type=Path, help="JSON profile containing acknowledged and compact row IDs")
    args = parser.parse_args(argv)
    try:
        profile = json.loads(args.profile.read_text(encoding="utf-8"))
        acknowledged_count, compact_count = validate_compact_integrity(profile)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"compact row-integrity profile: INVALID: {error}", file=sys.stderr)
        return 2

    print(
        f"compact row integrity: {acknowledged_count} acknowledged, {compact_count} compact; "
        "zero lost and zero duplicate logical rows: PASS"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
