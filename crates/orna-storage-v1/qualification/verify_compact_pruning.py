#!/usr/bin/env python3
"""Validate submitted selective-pruning evidence for ORNA-COMPACT-013."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


def _row_ids(value: Any, label: str) -> list[str]:
    if not isinstance(value, list):
        raise ValueError(f"{label} must be an array of row IDs")
    if any(not isinstance(row_id, str) or not row_id for row_id in value):
        raise ValueError(f"{label} must contain only non-empty string row IDs")
    if len(set(value)) != len(value):
        raise ValueError(f"{label} contains duplicate row IDs")
    return value


def _unit_count(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"{label} must be a non-negative integer")
    return value


def validate_compact_pruning(profile: Any) -> tuple[int, int, int, int, int]:
    """Validate submitted query evidence; return query, row, and unit totals."""
    if not isinstance(profile, dict):
        raise ValueError("profile must be a JSON object")
    queries = profile.get("queries")
    if not isinstance(queries, list) or not queries:
        raise ValueError("queries must be a non-empty array")

    query_ids: set[str] = set()
    row_count = eligible_total = read_total = pruned_total = 0
    observed_pruning = False
    for index, query in enumerate(queries):
        label = f"queries[{index}]"
        if not isinstance(query, dict):
            raise ValueError(f"{label} must be an object")

        query_id = query.get("query_id")
        if not isinstance(query_id, str) or not query_id:
            raise ValueError(f"{label}.query_id must be a non-empty string")
        if query_id in query_ids:
            raise ValueError(f"duplicate query_id: {query_id}")
        query_ids.add(query_id)

        oracle = _row_ids(query.get("oracle_row_ids"), f"{label}.oracle_row_ids")
        result = _row_ids(query.get("result_row_ids"), f"{label}.result_row_ids")
        if set(result) != set(oracle):
            missing = len(set(oracle) - set(result))
            extra = len(set(result) - set(oracle))
            raise ValueError(
                f"{label} result row IDs differ from the full-scan oracle "
                f"({missing} missing, {extra} extra)"
            )

        eligible = _unit_count(query.get("eligible_units"), f"{label}.eligible_units")
        read = _unit_count(query.get("units_read"), f"{label}.units_read")
        pruned = _unit_count(query.get("units_pruned"), f"{label}.units_pruned")
        if eligible != read + pruned:
            raise ValueError(
                f"{label} eligible_units must equal units_read + units_pruned"
            )
        if eligible == 0:
            raise ValueError(f"{label}.eligible_units must be positive")

        row_count += len(result)
        eligible_total += eligible
        read_total += read
        pruned_total += pruned
        observed_pruning |= pruned > 0

    if not observed_pruning:
        raise ValueError("at least one query must prune an eligible physical unit")
    return len(queries), row_count, eligible_total, read_total, pruned_total


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path, help="JSON file containing submitted query evidence")
    args = parser.parse_args(argv)
    try:
        profile = json.loads(args.evidence.read_text(encoding="utf-8"))
        query_count, row_count, eligible, read, pruned = validate_compact_pruning(profile)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"compact selective-pruning evidence: INVALID: {error}", file=sys.stderr)
        return 2

    print(
        f"compact selective pruning: {query_count} queries, {row_count} result rows, "
        f"{eligible} eligible physical units = {read} read + {pruned} pruned: PASS"
    )
    print(
        "Validates submitted evidence only; it does not collect or prove a "
        "24-hour production profile and does not alter runtime query planning."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
