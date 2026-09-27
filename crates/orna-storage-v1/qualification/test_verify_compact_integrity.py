import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from verify_compact_integrity import validate_compact_integrity


SCRIPT = Path(__file__).with_name("verify_compact_integrity.py")


def profile(acknowledged, compact):
    return {"acknowledged_rows": acknowledged, "compact_rows": compact}


class CompactIntegrityGateTests(unittest.TestCase):
    def test_exact_row_identity_match_passes(self):
        self.assertEqual(validate_compact_integrity(profile(["a", "b"], ["b", "a"])), (2, 2))

    def test_rejects_lost_acknowledged_row(self):
        with self.assertRaisesRegex(ValueError, "missing 1 acknowledged"):
            validate_compact_integrity(profile(["a", "b"], ["a"]))

    def test_rejects_duplicate_compact_logical_row(self):
        with self.assertRaisesRegex(ValueError, "compact_rows contains duplicate"):
            validate_compact_integrity(profile(["a"], ["a", "a"]))

    def test_rejects_duplicate_acknowledged_logical_row(self):
        with self.assertRaisesRegex(ValueError, "acknowledged_rows contains duplicate"):
            validate_compact_integrity(profile(["a", "a"], ["a"]))

    def test_rejects_unacknowledged_compact_row(self):
        with self.assertRaisesRegex(ValueError, "unacknowledged logical row"):
            validate_compact_integrity(profile(["a"], ["a", "b"]))

    def test_rejects_non_string_row_ids(self):
        with self.assertRaisesRegex(ValueError, "non-empty string"):
            validate_compact_integrity(profile(["a"], ["a", 2]))

    def test_rejects_empty_evidence(self):
        with self.assertRaisesRegex(ValueError, "at least one logical row"):
            validate_compact_integrity(profile([], []))

    def test_cli_reports_pass_and_zero_exit(self):
        result = self.run_cli(profile(["row-1"], ["row-1"]))
        self.assertEqual(result.returncode, 0)
        self.assertIn("zero lost and zero duplicate logical rows: PASS", result.stdout)

    def test_cli_reports_failure_and_exit_two_for_missing_row(self):
        result = self.run_cli(profile(["row-1", "row-2"], ["row-1"]))
        self.assertEqual(result.returncode, 2)
        self.assertIn("missing 1 acknowledged logical row", result.stderr)

    @staticmethod
    def run_cli(value):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "profile.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(path)],
                check=False,
                capture_output=True,
                text=True,
            )


if __name__ == "__main__":
    unittest.main()
