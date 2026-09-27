#!/usr/bin/env python3
"""Gate ORNA-COMPACT-013 stable-storage measurements for one shared corpus."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


CORPUS_DIGEST = re.compile(r"^[0-9a-f]{64}$")


def _byte_count(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{label} must be an integer byte count")
    if value <= 0:
        raise ValueError(f"{label} must be a positive byte count")
    return value


def stable_storage_ratio(profile: Any) -> tuple[int, int]:
    """Return compact/prometheus bytes after validating same-corpus evidence."""
    if not isinstance(profile, dict):
        raise ValueError("profile must be a JSON object")
    compact = profile.get("compact")
    prometheus = profile.get("prometheus_blocks")
    if not isinstance(compact, dict) or not isinstance(prometheus, dict):
        raise ValueError("profile requires compact and prometheus_blocks objects")

    compact_digest = compact.get("corpus_sha256")
    prometheus_digest = prometheus.get("corpus_sha256")
    if not isinstance(compact_digest, str) or not CORPUS_DIGEST.fullmatch(compact_digest):
        raise ValueError("compact.corpus_sha256 must be 64 lowercase hexadecimal characters")
    if not isinstance(prometheus_digest, str) or not CORPUS_DIGEST.fullmatch(prometheus_digest):
        raise ValueError("prometheus_blocks.corpus_sha256 must be 64 lowercase hexadecimal characters")
    if compact_digest != prometheus_digest:
        raise ValueError("compact and Prometheus measurements must identify the same corpus")

    compact_bytes = _byte_count(compact.get("stable_bytes"), "compact.stable_bytes")
    prometheus_bytes = _byte_count(
        prometheus.get("stable_bytes"), "prometheus_blocks.stable_bytes"
    )
    return compact_bytes, prometheus_bytes


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile", type=Path, help="JSON profile containing both stable byte counts")
    args = parser.parse_args(argv)
    try:
        profile = json.loads(args.profile.read_text(encoding="utf-8"))
        compact_bytes, prometheus_bytes = stable_storage_ratio(profile)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"compact stable-storage profile: INVALID: {error}", file=sys.stderr)
        return 2

    # Compare integers exactly so an exact 1.5 ratio cannot fail through float
    # rounding and a ratio above the limit cannot slip through rounding down.
    if compact_bytes * 2 <= prometheus_bytes * 3:
        print(
            f"compact stable-storage ratio: {compact_bytes}/{prometheus_bytes} "
            f"({compact_bytes / prometheus_bytes:.6f}) <= 1.500000: PASS"
        )
        return 0
    print(
        f"compact stable-storage ratio: {compact_bytes}/{prometheus_bytes} "
        f"({compact_bytes / prometheus_bytes:.6f}) > 1.500000: FAIL"
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
