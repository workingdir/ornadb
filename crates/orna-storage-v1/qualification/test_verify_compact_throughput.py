import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from verify_compact_throughput import FIXTURE, validate_compact_throughput


SCRIPT = Path(__file__).with_name("verify_compact_throughput.py")
START = "2026-01-01T00:00:00Z"
END = "2026-01-02T00:00:00Z"


def evidence(**overrides):
    profile = {
        "hardware_identity": "server-model X / 16-core CPU / 128 GiB RAM",
        "run_start_utc": START,
        "run_end_utc": END,
        "sample_count": 864_000_000,
        "workload_fixture_sha256": hashlib.sha256(FIXTURE.read_bytes()).hexdigest(),
    }
    profile.update(overrides)
    return profile


class CompactThroughputGateTests(unittest.TestCase):
    def test_threshold_passes_and_hashes_real_orna_fixture(self):
        fixture_bytes = FIXTURE.read_bytes()
        expected = hashlib.sha256(fixture_bytes).hexdigest()
        self.assertIn(b"pub fn main()", fixture_bytes)
        self.assertEqual(
            validate_compact_throughput(evidence()),
            ("server-model X / 16-core CPU / 128 GiB RAM", 86400.0, 864_000_000, expected),
        )

    def test_one_sample_below_rate_fails(self):
        with self.assertRaisesRegex(ValueError, "sample_count must be at least"):
            validate_compact_throughput(evidence(sample_count=863_999_999))

    def test_duration_short_by_one_second_fails(self):
        with self.assertRaisesRegex(ValueError, "duration must be at least"):
            validate_compact_throughput(evidence(run_end_utc="2026-01-01T23:59:59Z"))

    def test_rejects_malformed_timestamps(self):
        for timestamp in ("yesterday", "2026-01-01T00:00:00", "2026-01-01T00:00:00+01:00"):
            with self.subTest(timestamp=timestamp):
                with self.assertRaisesRegex(ValueError, "run_start_utc must be a UTC timestamp"):
                    validate_compact_throughput(evidence(run_start_utc=timestamp))

    def test_rejects_invalid_counts(self):
        for count in (0, -1, 1.5, True, "864000000"):
            with self.subTest(count=count):
                with self.assertRaisesRegex(ValueError, "sample_count must be a positive integer"):
                    validate_compact_throughput(evidence(sample_count=count))

    def test_rejects_missing_or_blank_hardware_identity(self):
        for hardware in (None, "", "   ", 5):
            with self.subTest(hardware=hardware):
                with self.assertRaisesRegex(ValueError, "hardware_identity must be a nonempty string"):
                    validate_compact_throughput(evidence(hardware_identity=hardware))
        missing = evidence()
        del missing["hardware_identity"]
        with self.assertRaisesRegex(ValueError, "hardware_identity must be a nonempty string"):
            validate_compact_throughput(missing)

    def test_rejects_fixture_hash_mismatch(self):
        with self.assertRaisesRegex(ValueError, "does not match the workload fixture"):
            validate_compact_throughput(evidence(workload_fixture_sha256="0" * 64))

    def test_cli_passes_and_reads_workload_fixture(self):
        result = self.run_cli(evidence())
        expected_hash = hashlib.sha256(FIXTURE.read_bytes()).hexdigest()
        self.assertEqual(result.returncode, 0)
        self.assertEqual(
            result.stdout.strip(),
            "compact throughput evidence claims: hardware server-model X / 16-core CPU / 128 GiB RAM; "
            f"duration 86400 seconds; samples 864000000; workload SHA-256 {expected_hash}: PASS",
        )
        self.assertEqual(result.stderr, "")

    def test_cli_fails_well_formed_below_rate_claim(self):
        result = self.run_cli(evidence(sample_count=863_999_999))
        self.assertEqual(result.returncode, 1)
        self.assertEqual(
            result.stdout.strip(),
            "compact throughput evidence claims: FAIL: sample_count must be at least 10000 times elapsed seconds",
        )
        self.assertEqual(result.stderr, "")

    def test_cli_returns_two_for_malformed_evidence(self):
        result = self.run_cli(evidence(run_start_utc="not-a-timestamp"))
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("compact throughput profile: INVALID: run_start_utc must be a UTC timestamp", result.stderr)

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
