#!/usr/bin/env python3
"""Focused contract checks for debian12_host_evidence.py."""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("debian12_host_evidence.py")


class Debian12HostEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.package = self.root / "orna.deb"
        self.machine = self.root / "machine.json"
        self.preflight = self.root / "host-preflight.json"
        self.package.write_bytes(b"package bytes")
        self.machine.write_text('{"os_release":{"ID":"debian"}}\n', encoding="utf-8")
        self.preflight.write_text(
            json.dumps(
                {
                    "format": 1,
                    "kind": "ADR0019_DEBIAN12_AMD64_HOST_PREFLIGHT",
                    "preflight_status": "PASS",
                    "complete_lifecycle_status": "NOT_PROVEN",
                    "fresh_host_status": "NOT_PROVEN",
                    "package": str(self.package),
                    "checks": [
                        {"name": "debian-12", "status": "PASS"},
                        {"name": "amd64-machine", "status": "PASS"},
                        {"name": "dpkg-amd64", "status": "PASS"},
                        {"name": "kernel-6.1-or-newer", "status": "PASS"},
                        {"name": "network-disabled", "status": "PASS"},
                        {"name": "docker-absent", "status": "PASS"},
                        {"name": "host-postgresql-installation-absent", "status": "PASS"},
                        {"name": "host-postgresql-process-absent", "status": "PASS"},
                        {"name": "root-observer", "status": "PASS"},
                        {"name": "package-input", "status": "PASS"},
                        {"name": "package-inspector", "status": "PASS"},
                        {"name": "package-identity", "status": "PASS"},
                        {"name": "package-inventory", "status": "PASS"},
                        {"name": "package-digest", "status": "PASS"},
                    ],
                }
            )
            + "\n",
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def run_helper(self, output: Path, *extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--package",
                str(self.package),
                "--evidence-dir",
                str(output),
                "--preflight-report",
                str(self.preflight),
                "--machine-evidence",
                str(self.machine),
                *extra,
            ],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_not_run_is_explicit_and_records_all_input_slots(self) -> None:
        output = self.root / "not-run"
        completed = self.run_helper(output, "--lifecycle-status", "not-run")
        self.assertEqual(completed.returncode, 1, completed.stderr)
        manifest = json.loads((output / "host-evidence.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["complete_lifecycle_status"], "NOT_PROVEN")
        self.assertEqual(manifest["lifecycle_reason"], "native lifecycle matrix was not run")
        self.assertEqual(manifest["inputs"]["package"]["size"], len(b"package bytes"))
        self.assertEqual(manifest["inputs"]["process"]["status"], "NOT_PROVEN")
        self.assertEqual(manifest["inputs"]["trace"]["status"], "NOT_PROVEN")

    def test_pass_requires_explicit_process_and_trace_files(self) -> None:
        self.preflight.write_text(
            self.preflight.read_text(encoding="utf-8").replace(
                '"fresh_host_status": "NOT_PROVEN"', '"fresh_host_status": "PASS"'
            ),
            encoding="utf-8",
        )
        completed = self.run_helper(self.root / "missing", "--lifecycle-status", "pass")
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("PASS requires explicit process and trace", completed.stderr)
        self.assertFalse((self.root / "missing").exists())

    def test_pass_requires_fresh_host_attestation(self) -> None:
        process = self.root / "process.json"
        trace = self.root / "trace.txt"
        process.write_text('{"pids":[1]}\n', encoding="utf-8")
        trace.write_text("lifecycle\n", encoding="utf-8")
        completed = self.run_helper(
            self.root / "unattested",
            "--process-evidence",
            str(process),
            "--trace-evidence",
            str(trace),
            "--lifecycle-status",
            "pass",
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("fresh-host attestation", completed.stderr)
        self.assertFalse((self.root / "unattested").exists())

    def test_relative_package_is_rejected_without_output(self) -> None:
        completed = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--package",
                "orna.deb",
                "--evidence-dir",
                str(self.root / "relative"),
                "--preflight-report",
                str(self.preflight),
                "--machine-evidence",
                str(self.machine),
                "--lifecycle-status",
                "not-run",
            ],
            text=True,
            capture_output=True,
            check=False,
            cwd=self.root,
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("--package must be absolute", completed.stderr)
        self.assertFalse((self.root / "relative").exists())

    def test_failed_preflight_cannot_unlock_lifecycle(self) -> None:
        self.preflight.write_text(
            self.preflight.read_text(encoding="utf-8").replace('"preflight_status": "PASS"', '"preflight_status": "FAIL"'),
            encoding="utf-8",
        )
        completed = self.run_helper(self.root / "failed", "--lifecycle-status", "not-run")
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("host preflight did not pass", completed.stderr)
        self.assertFalse((self.root / "failed").exists())

    def test_pass_records_process_and_trace_inputs(self) -> None:
        process = self.root / "process.json"
        trace = self.root / "trace.txt"
        process.write_text('{"pids":[1]}\n', encoding="utf-8")
        trace.write_text("execve absent\n", encoding="utf-8")
        self.preflight.write_text(
            self.preflight.read_text(encoding="utf-8").replace(
                '"fresh_host_status": "NOT_PROVEN"', '"fresh_host_status": "PASS"'
            ),
            encoding="utf-8",
        )
        completed = self.run_helper(
            self.root / "pass",
            "--process-evidence",
            str(process),
            "--trace-evidence",
            str(trace),
            "--lifecycle-status",
            "pass",
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        manifest = json.loads((self.root / "pass" / "host-evidence.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["complete_lifecycle_status"], "PASS")
        expected = hashlib.sha256(process.read_bytes()).hexdigest()
        self.assertEqual(manifest["inputs"]["process"]["sha256"], expected)
        self.assertEqual(manifest["inputs"]["trace"]["size"], len("execve absent\n"))


if __name__ == "__main__":
    unittest.main()
