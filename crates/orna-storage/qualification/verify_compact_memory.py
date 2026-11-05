#!/usr/bin/env python3
"""Validate submitted bounded-memory evidence for a compact profile run."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


def _positive_integer(profile: dict[str, Any], field: str) -> int:
    value = profile.get(field)
    # bool is an int subclass in Python, but is not a meaningful byte count.
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"{field} must be a positive integer number of bytes")
    return value


def validate_compact_memory(profile: Any) -> tuple[int, int]:
    """Validate memory evidence; return the declared budget and measured peak RSS."""
    if not isinstance(profile, dict):
        raise ValueError("profile must be a JSON object")

    budget = _positive_integer(profile, "memory_budget_bytes")
    peak_rss = _positive_integer(profile, "peak_rss_bytes")
    return budget, peak_rss


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile", type=Path, help="JSON profile containing memory budget and peak RSS")
    args = parser.parse_args(argv)
    try:
        profile = json.loads(args.profile.read_text(encoding="utf-8"))
        budget, peak_rss = validate_compact_memory(profile)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"compact bounded-memory profile: INVALID: {error}", file=sys.stderr)
        return 2

    if peak_rss > budget:
        print(
            f"compact bounded memory: peak RSS {peak_rss} bytes exceeds "
            f"declared budget {budget} bytes: FAIL"
        )
        return 1

    print(
        f"compact bounded memory: peak RSS {peak_rss} bytes within "
        f"declared budget {budget} bytes: PASS"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
