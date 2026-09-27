import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from verify_compact_pruning import validate_compact_pruning


SCRIPT = Path(__file__).with_name("verify_compact_pruning.py")
FIXTURE = Path(__file__).with_name("fixtures") / "compact_pruning_evidence.orna"


def fixture_row_id():
    source = FIXTURE.read_text(encoding="utf-8")
    match = re.search(r'pub fn evidence_row_id\(\): Str = "([^"]+)";', source)
    if match is None:
        raise AssertionError(".orna evidence fixture must declare evidence_row_id")
    return match.group(1)


def profile(queries=None):
    return {
        "queries": queries
        if queries is not None
        else [
            {
                "query_id": "recent-errors",
                "oracle_row_ids": [fixture_row_id()],
                "result_row_ids": [fixture_row_id()],
                "eligible_units": 4,
                "units_read": 1,
                "units_pruned": 3,
            },
            {
                "query_id": "healthy-window",
                "oracle_row_ids": [],
                "result_row_ids": [],
                "eligible_units": 2,
                "units_read": 2,
                "units_pruned": 0,
            },
        ]
    }


class CompactPruningEvidenceTests(unittest.TestCase):
    def test_evidence_row_is_read_from_real_orna_fixture(self):
        self.assertIn("pub table Sample", FIXTURE.read_text(encoding="utf-8"))
        self.assertEqual(fixture_row_id(), "sample-2")

    def test_validates_oracle_results_and_reconciles_physical_units(self):
        self.assertEqual(validate_compact_pruning(profile()), (2, 1, 6, 3, 3))

    def test_rejects_missing_oracle_result(self):
        query = profile()["queries"][0]
        query["result_row_ids"] = []
        with self.assertRaisesRegex(ValueError, "1 missing"):
            validate_compact_pruning(profile([query]))

    def test_rejects_extra_result(self):
        query = profile()["queries"][0]
        query["result_row_ids"] = ["sample-2", "unexpected"]
        with self.assertRaisesRegex(ValueError, "1 extra"):
            validate_compact_pruning(profile([query]))

    def test_rejects_duplicate_result_ids(self):
        query = profile()["queries"][0]
        query["result_row_ids"] = ["sample-2", "sample-2"]
        with self.assertRaisesRegex(ValueError, "duplicate row IDs"):
            validate_compact_pruning(profile([query]))

    def test_rejects_duplicate_oracle_ids(self):
        query = profile()["queries"][0]
        query["oracle_row_ids"] = ["sample-2", "sample-2"]
        with self.assertRaisesRegex(ValueError, "duplicate row IDs"):
            validate_compact_pruning(profile([query]))

    def test_rejects_unit_count_mismatch(self):
        query = profile()["queries"][0]
        query["units_read"] = 2
        with self.assertRaisesRegex(ValueError, "must equal units_read"):
            validate_compact_pruning(profile([query]))

    def test_rejects_boolean_and_negative_unit_counts(self):
        for value in (True, -1):
            query = profile()["queries"][0]
            query["eligible_units"] = value
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "non-negative integer"):
                validate_compact_pruning(profile([query]))

    def test_requires_at_least_one_pruned_unit(self):
        query = profile()["queries"][1]
        with self.assertRaisesRegex(ValueError, "at least one query must prune"):
            validate_compact_pruning(profile([query]))

    def test_rejects_duplicate_query_ids(self):
        queries = profile()["queries"]
        queries[1]["query_id"] = queries[0]["query_id"]
        with self.assertRaisesRegex(ValueError, "duplicate query_id"):
            validate_compact_pruning(profile(queries))

    def test_cli_passes_and_states_scope(self):
        result = self.run_cli(profile())
        self.assertEqual(result.returncode, 0)
        self.assertIn("6 eligible physical units = 3 read + 3 pruned: PASS", result.stdout)
        self.assertIn("does not alter runtime query planning", result.stdout)

    def test_cli_returns_two_for_inconsistent_evidence(self):
        query = profile()["queries"][0]
        query["units_pruned"] = 2
        result = self.run_cli(profile([query]))
        self.assertEqual(result.returncode, 2)
        self.assertIn("eligible_units must equal", result.stderr)

    @staticmethod
    def run_cli(value):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "evidence.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(path)],
                check=False,
                capture_output=True,
                text=True,
            )


if __name__ == "__main__":
    unittest.main()
