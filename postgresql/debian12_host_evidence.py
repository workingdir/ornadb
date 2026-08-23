#!/usr/bin/env python3
"""Record the evidence boundary for the ADR 0019 native Debian host proof.

This helper only reads already-collected evidence. It never installs a
package, starts a service, invokes Docker, or treats a container run as native
host evidence. A lifecycle claim is accepted only after a passing host
preflight report and explicit process and trace inputs have been supplied.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
from pathlib import Path
from typing import Any


PASS = "PASS"
FAIL = "FAIL"
NOT_PROVEN = "NOT_PROVEN"
PREFLIGHT_KIND = "ADR0019_DEBIAN12_AMD64_HOST_PREFLIGHT"
EVIDENCE_KIND = "ADR0019_DEBIAN12_AMD64_HOST_EVIDENCE"
REQUIRED_PREFLIGHT_CHECKS = frozenset(
    {
        "debian-12",
        "amd64-machine",
        "dpkg-amd64",
        "kernel-6.1-or-newer",
        "network-disabled",
        "docker-absent",
        "host-postgresql-installation-absent",
        "host-postgresql-process-absent",
        "root-observer",
        "package-input",
        "package-inspector",
        "package-identity",
        "package-inventory",
        "package-digest",
    }
)


class EvidenceError(ValueError):
    """Raised when evidence is absent, unsafe, or internally inconsistent."""


def absolute(raw: str, label: str) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        raise EvidenceError(f"{label} must be absolute")
    return path


def regular(path: Path, label: str) -> None:
    try:
        item = path.lstat()
    except OSError as error:
        raise EvidenceError(f"{label} is unavailable: {error}") from error
    if stat.S_ISLNK(item.st_mode):
        raise EvidenceError(f"{label} must not be a symbolic link: {path}")
    if not stat.S_ISREG(item.st_mode):
        raise EvidenceError(f"{label} must be one regular file: {path}")


def digest(path: Path, label: str) -> dict[str, Any]:
    regular(path, label)
    try:
        data = path.read_bytes()
        mode = stat.S_IMODE(path.stat().st_mode)
    except OSError as error:
        raise EvidenceError(f"{label} could not be read: {error}") from error
    return {
        "status": PASS,
        "path": str(path),
        "sha256": hashlib.sha256(data).hexdigest(),
        "size": len(data),
        "mode": f"{mode:04o}",
    }


def optional_digest(path: Path | None, label: str, reason: str) -> dict[str, Any]:
    if path is None:
        return {"path": None, "status": NOT_PROVEN, "reason": reason}
    return {"status": PASS, **digest(path, label)}


def read_json(path: Path, label: str) -> dict[str, Any]:
    regular(path, label)
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"{label} is not valid UTF-8 JSON: {error}") from error
    if not isinstance(value, dict):
        raise EvidenceError(f"{label} must contain a JSON object")
    return value


def validate_preflight(path: Path, package: Path, lifecycle_status: str) -> dict[str, Any]:
    report = read_json(path, "preflight report")
    if report.get("format") != 1 or report.get("kind") != PREFLIGHT_KIND:
        raise EvidenceError("preflight report has an unsupported format or kind")
    if report.get("package") != str(package):
        raise EvidenceError("preflight report package does not match --package")
    checks = report.get("checks")
    if not isinstance(checks, list) or not checks:
        raise EvidenceError("preflight report has no checks")
    if any(
        not isinstance(item, dict)
        or not isinstance(item.get("name"), str)
        or item.get("status") not in {PASS, FAIL, NOT_PROVEN}
        for item in checks
    ):
        raise EvidenceError("preflight report contains malformed checks")
    names = [item["name"] for item in checks]
    if len(names) != len(set(names)) or not REQUIRED_PREFLIGHT_CHECKS.issubset(names):
        raise EvidenceError("preflight report does not contain the canonical checks")
    statuses = {item["status"] for item in checks}
    if report.get("complete_lifecycle_status") != NOT_PROVEN:
        raise EvidenceError("preflight report must not claim lifecycle completion")
    if report.get("fresh_host_status") not in {PASS, NOT_PROVEN}:
        raise EvidenceError("preflight report has a failed fresh-host status")
    if lifecycle_status == "pass" and report.get("fresh_host_status") != PASS:
        raise EvidenceError("PASS requires a passing fresh-host attestation")
    if report.get("preflight_status") != PASS or statuses != {PASS}:
        raise EvidenceError("host preflight did not pass all checks")
    return report


def create_output(path: Path) -> None:
    if path.exists():
        if path.is_symlink() or not path.is_dir() or any(path.iterdir()):
            raise EvidenceError("--evidence-dir must be a new empty non-linked directory")
    else:
        try:
            path.mkdir(mode=0o700, parents=False)
        except OSError as error:
            raise EvidenceError(f"could not create --evidence-dir: {error}") from error
    os.chmod(path, 0o700)


def write_manifest(path: Path, manifest: dict[str, Any]) -> None:
    target = path / "host-evidence.json"
    temporary = path / ".host-evidence.json.tmp"
    if target.exists() or temporary.exists():
        raise EvidenceError("refusing to replace existing host evidence")
    try:
        temporary.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        temporary.replace(target)
        target.chmod(0o600)
    except OSError as error:
        raise EvidenceError(f"could not write host evidence: {error}") from error


def build_manifest(args: argparse.Namespace) -> tuple[Path, dict[str, Any]]:
    package = absolute(args.package, "--package")
    output = absolute(args.evidence_dir, "--evidence-dir")
    preflight_path = absolute(args.preflight_report, "--preflight-report")
    machine_path = absolute(args.machine_evidence, "--machine-evidence")
    process_path = (
        absolute(args.process_evidence, "--process-evidence")
        if args.process_evidence is not None
        else None
    )
    trace_path = (
        absolute(args.trace_evidence, "--trace-evidence")
        if args.trace_evidence is not None
        else None
    )

    # Validate preflight before interpreting --lifecycle-status. A stale,
    # failed, or container-derived report can never unlock a lifecycle claim.
    preflight = validate_preflight(preflight_path, package, args.lifecycle_status)
    package_input = digest(package, "package input")
    machine_input = digest(machine_path, "machine evidence")

    if args.lifecycle_status == "pass":
        if process_path is None or trace_path is None:
            raise EvidenceError("PASS requires explicit process and trace evidence paths")
        process_input = optional_digest(process_path, "process evidence", "not supplied")
        trace_input = optional_digest(trace_path, "trace evidence", "not supplied")
        lifecycle_status = PASS
        lifecycle_reason = "native lifecycle matrix inputs were supplied after passing preflight"
    else:
        process_input = optional_digest(
            process_path, "process evidence", "native lifecycle matrix was not run"
        )
        trace_input = optional_digest(trace_path, "trace evidence", "native lifecycle matrix was not run")
        lifecycle_status = NOT_PROVEN
        lifecycle_reason = "native lifecycle matrix was not run"

    create_output(output)
    manifest = {
        "format": 1,
        "kind": EVIDENCE_KIND,
        "preflight_status": preflight["preflight_status"],
        "fresh_host_status": preflight.get("fresh_host_status", NOT_PROVEN),
        "complete_lifecycle_status": lifecycle_status,
        "lifecycle_reason": lifecycle_reason,
        "inputs": {
            "machine": machine_input,
            "package": package_input,
            "process": process_input,
            "trace": trace_input,
        },
        "preflight_report": digest(preflight_path, "preflight report"),
    }
    return output, manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True)
    parser.add_argument("--evidence-dir", required=True)
    parser.add_argument("--preflight-report", required=True)
    parser.add_argument("--machine-evidence", required=True)
    parser.add_argument("--process-evidence")
    parser.add_argument("--trace-evidence")
    parser.add_argument("--lifecycle-status", choices=("not-run", "pass"), required=True)
    args = parser.parse_args()
    try:
        output, manifest = build_manifest(args)
    except EvidenceError as error:
        parser.error(str(error))
    write_manifest(output, manifest)
    print(
        json.dumps(
            {
                "complete_lifecycle_status": manifest["complete_lifecycle_status"],
                "evidence_dir": str(output),
            },
            sort_keys=True,
        )
    )
    return 0 if manifest["complete_lifecycle_status"] == PASS else 1


if __name__ == "__main__":
    raise SystemExit(main())
