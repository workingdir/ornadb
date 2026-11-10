from __future__ import annotations

import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from scripts import check_benchmark_baseline
from scripts.check_benchmark_baseline import BaselineError, compare, load

BASELINE = {
    "b1_fib_25": {"steps": 1_000, "calls": 242_785, "gated": True},
    "b2_fold_1000000": {"steps": 9_000, "calls": 2, "gated": True},
    "b2b_range_1000000": {"steps": 10_000, "calls": 1, "gated": False},
}
RESULTS = {
    "b1_fib_25": {"steps": 1_000, "calls": 242_785, "wall_ms": 12.0},
    "b2_fold_1000000": {"steps": 9_000, "calls": 2, "wall_ms": 30.0},
    "b2b_range_1000000": {"steps": 10_000, "calls": 1, "wall_ms": 5.0},
}


class BenchmarkBaselineTests(unittest.TestCase):
    def test_matching_counters_pass(self) -> None:
        self.assertEqual(compare(BASELINE, RESULTS), [])

    def test_a_step_regression_fails(self) -> None:
        results = {**RESULTS, "b1_fib_25": {**RESULTS["b1_fib_25"], "steps": 1_001}}
        self.assertEqual(
            compare(BASELINE, results), ["b1_fib_25.steps: 1001 (baseline 1000)"]
        )

    def test_a_call_regression_fails(self) -> None:
        results = {**RESULTS, "b2_fold_1000000": {**RESULTS["b2_fold_1000000"], "calls": 3}}
        self.assertEqual(
            compare(BASELINE, results), ["b2_fold_1000000.calls: 3 (baseline 2)"]
        )

    def test_an_ungated_benchmark_is_not_compared(self) -> None:
        results = {
            **RESULTS,
            "b2b_range_1000000": {**RESULTS["b2b_range_1000000"], "steps": 99_999},
        }
        self.assertEqual(compare(BASELINE, results), [])

    def test_a_missing_benchmark_fails(self) -> None:
        results = {key: value for key, value in RESULTS.items() if key != "b2_fold_1000000"}
        self.assertEqual(
            compare(BASELINE, results),
            ["b2_fold_1000000: missing from the benchmark run"],
        )

    def test_a_new_benchmark_without_a_baseline_fails(self) -> None:
        results = {**RESULTS, "b4_new": {"steps": 1, "calls": 1}}
        self.assertEqual(
            compare(BASELINE, results),
            ["b4_new: no baseline entry; record it before gating it"],
        )

    def test_wall_time_is_not_compared(self) -> None:
        results = {**RESULTS, "b1_fib_25": {**RESULTS["b1_fib_25"], "wall_ms": 9_999.0}}
        self.assertEqual(compare(BASELINE, results), [])

    def test_a_malformed_baseline_is_rejected(self) -> None:
        with TemporaryDirectory() as directory:
            path = Path(directory) / "baseline.json"
            path.write_text(
                json.dumps({"benchmarks": {"b1": {"steps": -1, "calls": 1}}}),
                encoding="utf-8",
            )
            with self.assertRaises(BaselineError):
                load(path, "baseline")

    def test_a_file_without_benchmarks_is_rejected(self) -> None:
        with TemporaryDirectory() as directory:
            path = Path(directory) / "results.json"
            path.write_text(json.dumps({"evaluator": "old"}), encoding="utf-8")
            with self.assertRaises(BaselineError):
                load(path, "results")


if __name__ == "__main__":
    unittest.main()
