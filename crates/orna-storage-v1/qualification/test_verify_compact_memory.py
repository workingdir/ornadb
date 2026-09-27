import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from verify_compact_memory import validate_compact_memory


SCRIPT = Path(__file__).with_name("verify_compact_memory.py")


class CompactMemoryGateTests(unittest.TestCase):
    def test_peak_rss_at_budget_passes(self):
        self.assertEqual(
            validate_compact_memory({"memory_budget_bytes": 1024, "peak_rss_bytes": 1024}),
            (1024, 1024),
        )

    def test_peak_rss_below_budget_is_valid_evidence(self):
        self.assertEqual(
            validate_compact_memory({"memory_budget_bytes": 2048, "peak_rss_bytes": 1024}),
            (2048, 1024),
        )

    def test_rejects_non_object_profile(self):
        with self.assertRaisesRegex(ValueError, "profile must be a JSON object"):
            validate_compact_memory([])

    def test_rejects_missing_budget(self):
        with self.assertRaisesRegex(ValueError, "memory_budget_bytes must be a positive integer"):
            validate_compact_memory({"peak_rss_bytes": 1})

    def test_rejects_missing_peak_rss(self):
        with self.assertRaisesRegex(ValueError, "peak_rss_bytes must be a positive integer"):
            validate_compact_memory({"memory_budget_bytes": 1})

    def test_rejects_zero_negative_fractional_and_boolean_values(self):
        for value in (0, -1, 1.5, True):
            with self.subTest(value=value):
                with self.assertRaisesRegex(ValueError, "must be a positive integer"):
                    validate_compact_memory({"memory_budget_bytes": value, "peak_rss_bytes": 1})
                with self.assertRaisesRegex(ValueError, "must be a positive integer"):
                    validate_compact_memory({"memory_budget_bytes": 1, "peak_rss_bytes": value})

    def test_cli_passes_with_zero_exit_when_at_budget(self):
        result = self.run_cli({"memory_budget_bytes": 1024, "peak_rss_bytes": 1024})
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "compact bounded memory: peak RSS 1024 bytes within declared budget 1024 bytes: PASS")
        self.assertEqual(result.stderr, "")

    def test_cli_fails_with_one_exit_for_valid_over_budget_measurement(self):
        result = self.run_cli({"memory_budget_bytes": 1024, "peak_rss_bytes": 1025})
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout.strip(), "compact bounded memory: peak RSS 1025 bytes exceeds declared budget 1024 bytes: FAIL")
        self.assertEqual(result.stderr, "")

    def test_cli_returns_two_for_malformed_evidence(self):
        result = self.run_cli({"memory_budget_bytes": 0, "peak_rss_bytes": 1025})
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("compact bounded-memory profile: INVALID: memory_budget_bytes must be a positive integer", result.stderr)

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
