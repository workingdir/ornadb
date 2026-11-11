#!/usr/bin/env python3
"""Fail on a regression of the H0 benchmark counters (HIR design section 7.4).

The three baseline benchmarks are measured on the old evaluator by
`crates/orna-evaluator/examples/orna_bench.rs`, which prints one JSON record.
Their deterministic counters -- evaluator steps and recorded call count -- must
equal the checked-in baseline exactly. Wall time is printed for the job summary
and is never gated: shared CI runners are noisy, and the design gates it only
loosely on a pinned runner label.

A benchmark whose baseline entry sets `"gated": false` is printed but not
compared; its counters are still recorded so its history stays visible.

Usage:
    python3 scripts/check_benchmark_baseline.py \
        --baseline crates/orna-evaluator/benchmarks/baseline.json \
        --results /tmp/bench.json

Exit status: 0 when every gated counter matches and the two files name the same
benchmarks, 1 on a counter regression or a benchmark-set change, 2 when a file
cannot be read or does not have the expected shape.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

COUNTERS = ("steps", "calls")
# Counters gated only where a baseline entry records them, so a suite that does not
# count them (the evaluator counts neither) keeps its exact entries. `allocations` is
# the exec suite's heap count; `rows_read` is the rows a table store reported as read.
OPTIONAL_COUNTERS = ("allocations", "rows_read")
Benchmarks = dict[str, dict]


class BaselineError(Exception):
    """A baseline or results file is unreadable or malformed."""


def load(path: Path, label: str) -> Benchmarks:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise BaselineError(f"cannot read {label} {path}: {error}") from error
    except json.JSONDecodeError as error:
        raise BaselineError(f"{label} {path} is not JSON: {error}") from error
    benchmarks = data.get("benchmarks")
    if not isinstance(benchmarks, dict):
        raise BaselineError(f"{label} {path} has no 'benchmarks' object")
    for name, entry in benchmarks.items():
        if not isinstance(entry, dict):
            raise BaselineError(f"{label} {path}: benchmark {name!r} is not an object")
        for counter in COUNTERS + tuple(c for c in OPTIONAL_COUNTERS if c in entry):
            value = entry.get(counter)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise BaselineError(
                    f"{label} {path}: benchmark {name!r} needs a non-negative integer "
                    f"{counter!r}"
                )
    return benchmarks


def compare(baseline: Benchmarks, results: Benchmarks) -> list[str]:
    """Returns one message per mismatch; an empty list is a pass."""
    problems: list[str] = []
    for name in sorted(set(baseline) - set(results)):
        problems.append(f"{name}: missing from the benchmark run")
    for name in sorted(set(results) - set(baseline)):
        problems.append(f"{name}: no baseline entry; record it before gating it")
    for name in sorted(set(baseline) & set(results)):
        expected = baseline[name]
        actual = results[name]
        if not expected.get("gated", True):
            continue
        for counter in COUNTERS + tuple(c for c in OPTIONAL_COUNTERS if c in expected):
            if counter not in actual:
                problems.append(f"{name}.{counter}: missing from the benchmark run")
            elif expected[counter] != actual[counter]:
                problems.append(
                    f"{name}.{counter}: {actual[counter]} (baseline {expected[counter]})"
                )
    return problems


def summary_table(baseline: Benchmarks, results: Benchmarks) -> str:
    """A Markdown table of the run, for the CI job summary."""
    rows = [
        "| benchmark | steps | calls | wall ms | gated |",
        "|---|---|---|---|---|",
    ]
    for name in sorted(results):
        actual = results[name]
        gated = baseline.get(name, {}).get("gated", True)
        rows.append(
            f"| {name} | {actual['steps']} | {actual['calls']} | "
            f"{actual.get('wall_ms', 'n/a')} | {'yes' if gated else 'no'} |"
        )
    return "\n".join(rows)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument(
        "--summary",
        type=Path,
        help="write the Markdown summary table to this file as well as stdout",
    )
    arguments = parser.parse_args(argv)

    try:
        baseline = load(arguments.baseline, "baseline")
        results = load(arguments.results, "results")
    except BaselineError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    table = summary_table(baseline, results)
    print(table)
    if arguments.summary is not None:
        arguments.summary.write_text(table + "\n", encoding="utf-8")

    problems = compare(baseline, results)
    for problem in problems:
        print(f"counter regression: {problem}", file=sys.stderr)
    if problems:
        print(
            f"{len(problems)} benchmark counter mismatch(es); the baseline is "
            "deliberate and may only be re-recorded with a reviewed change",
            file=sys.stderr,
        )
        return 1
    print("benchmark counters match the checked-in baseline")
    return 0


if __name__ == "__main__":
    sys.exit(main())
