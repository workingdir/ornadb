import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from verify_stable_storage import stable_storage_ratio


SCRIPT = Path(__file__).with_name("verify_stable_storage.py")
DIGEST = "a" * 64


def profile(compact_bytes: int, prometheus_bytes: int, *, prometheus_digest: str = DIGEST):
    return {
        "compact": {"corpus_sha256": DIGEST, "stable_bytes": compact_bytes},
        "prometheus_blocks": {
            "corpus_sha256": prometheus_digest,
            "stable_bytes": prometheus_bytes,
        },
    }


class StableStorageGateTests(unittest.TestCase):
    def test_ratio_uses_exact_same_corpus_byte_counts(self):
        self.assertEqual(stable_storage_ratio(profile(150, 100)), (150, 100))

    def test_ratio_rejects_different_corpus_digests(self):
        with self.assertRaisesRegex(ValueError, "same corpus"):
            stable_storage_ratio(profile(150, 100, prometheus_digest="b" * 64))

    def test_ratio_rejects_empty_compact_measurement(self):
        with self.assertRaisesRegex(ValueError, "positive byte count"):
            stable_storage_ratio(profile(0, 100))

    def test_cli_accepts_exact_one_point_five_boundary(self):
        self.assertEqual(self.run_cli(profile(150, 100)).returncode, 0)

    def test_cli_rejects_ratio_above_one_point_five(self):
        result = self.run_cli(profile(151, 100))
        self.assertEqual(result.returncode, 1)
        self.assertIn("> 1.500000: FAIL", result.stdout)

    def test_cli_rejects_empty_compact_measurement(self):
        result = self.run_cli(profile(0, 100))
        self.assertEqual(result.returncode, 2)
        self.assertIn("compact.stable_bytes must be a positive byte count", result.stderr)

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
