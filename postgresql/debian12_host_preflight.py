#!/usr/bin/env python3
"""Offline, fail-closed prerequisite capture for work ADR 0019.

This preflight reads host state and an existing .deb only. It never installs or
runs the package and never invokes Docker, PostgreSQL, systemd, or a network
client. complete_lifecycle_status is always NOT_PROVEN.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import stat
import subprocess
from pathlib import Path
from typing import Any


PASS = "PASS"
FAIL = "FAIL"
NOT_PROVEN = "NOT_PROVEN"

DOCKER_PATHS = (
    "/usr/bin/docker",
    "/usr/bin/podman",
    "/usr/bin/buildah",
    "/usr/local/bin/docker",
    "/usr/local/bin/podman",
    "/run/docker.sock",
    "/var/run/docker.sock",
    "/run/podman/podman.sock",
    "/var/run/podman/podman.sock",
)
PG_PATHS = (
    "/usr/bin/postgres",
    "/usr/bin/postmaster",
    "/usr/bin/psql",
    "/usr/bin/initdb",
    "/usr/bin/pg_ctl",
    "/usr/bin/pg_upgrade",
    "/usr/bin/pg_resetwal",
    "/usr/lib/postgresql",
    "/etc/postgresql",
    "/var/lib/postgresql",
    "/run/postgresql",
)
PG_NAMES = {
    "postgres",
    "postmaster",
    "psql",
    "initdb",
    "pg_ctl",
    "pg_upgrade",
    "pg_resetwal",
}
FORBIDDEN = {Path(path).name for path in DOCKER_PATHS[:5]} | PG_NAMES


def result(name: str, status: str, reason: str, evidence: str) -> dict[str, str]:
    return {"name": name, "status": status, "reason": reason, "evidence": evidence}


def write_bytes(path: Path, data: bytes) -> None:
    temporary = path.with_name("." + path.name + ".tmp")
    if path.exists() or temporary.exists():
        raise RuntimeError("refusing to replace evidence: " + str(path))
    temporary.write_bytes(data)
    temporary.replace(path)


def write_json(path: Path, value: Any) -> None:
    write_bytes(path, (json.dumps(value, indent=2, sort_keys=True) + "\n").encode())


def read(path: Path) -> tuple[str | None, str]:
    try:
        return path.read_text(encoding="utf-8"), "read directly"
    except (OSError, UnicodeDecodeError) as error:
        return None, str(error)


def regular(path: Path) -> tuple[bool, str]:
    try:
        item = path.lstat()
    except OSError as error:
        return False, str(error)
    if stat.S_ISLNK(item.st_mode):
        return False, "symbolic link"
    if not stat.S_ISREG(item.st_mode):
        return False, "not a regular file"
    return True, "regular non-linked file"


def local(argv: list[str]) -> tuple[int, bytes, bytes]:
    if not argv or Path(argv[0]).name in FORBIDDEN:
        raise RuntimeError("forbidden command refused")
    completed = subprocess.run(
        argv,
        check=False,
        close_fds=True,
        env={"PATH": "/usr/bin:/bin", "LC_ALL": "C", "LANG": "C", "TZ": "UTC0"},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    return completed.returncode, completed.stdout, completed.stderr


def machine(evidence: Path) -> list[dict[str, str]]:
    checks: list[dict[str, str]] = []
    raw, reason = read(Path("/etc/os-release"))
    values: dict[str, str] = {}
    if raw is None:
        checks.append(result("debian-12", NOT_PROVEN, reason, "/etc/os-release"))
    else:
        for line in raw.splitlines():
            if "=" in line and not line.startswith("#"):
                key, value = line.split("=", 1)
                values[key] = value.strip().strip('"')
        observed = (values.get("ID"), values.get("VERSION_ID"))
        good = observed == ("debian", "12")
        checks.append(
            result(
                "debian-12",
                PASS if good else FAIL,
                "ID=debian and VERSION_ID=12"
                if good
                else f"observed ID={observed[0]} VERSION_ID={observed[1]}",
                "/etc/os-release",
            )
        )

    arch = platform.machine()
    checks.append(
        result("amd64-machine", PASS if arch == "x86_64" else FAIL, f"uname machine is {arch}", "uname")
    )

    dpkg = Path("/usr/bin/dpkg")
    dpkg_arch = None
    ok, dpkg_reason = regular(dpkg)
    if ok:
        try:
            code, stdout, stderr = local([str(dpkg), "--print-architecture"])
            if code == 0 and not stderr:
                dpkg_arch = stdout.decode("ascii").strip()
        except (OSError, RuntimeError, subprocess.SubprocessError, UnicodeDecodeError) as error:
            dpkg_reason = str(error)
    checks.append(
        result(
            "dpkg-amd64",
            PASS if dpkg_arch == "amd64" else (FAIL if dpkg_arch else NOT_PROVEN),
            "dpkg reports amd64"
            if dpkg_arch == "amd64"
            else (f"dpkg reports {dpkg_arch}" if dpkg_arch else dpkg_reason),
            str(dpkg),
        )
    )

    release = os.uname().release
    version = re.match(r"^(\d+)\.(\d+)", release)
    kernel_good = version is not None and (int(version.group(1)), int(version.group(2))) >= (6, 1)
    checks.append(
        result(
            "kernel-6.1-or-newer",
            PASS if kernel_good else (FAIL if version else NOT_PROVEN),
            f"kernel release is {release}",
            "uname",
        )
    )

    routes: dict[str, str] = {}
    failures: list[str] = []
    missing = False
    for route_path in (Path("/proc/net/route"), Path("/proc/net/ipv6_route")):
        text, why = read(route_path)
        if text is None:
            missing = True
            routes[str(route_path)] = why
            continue
        routes[str(route_path)] = text
        rows = [line.split() for line in text.splitlines() if line.strip()]
        if route_path.name == "route":
            rows = [row for row in rows if row and row[0] != "Iface"]
            failures.extend(row[0] for row in rows if row[0] != "lo")
        else:
            failures.extend(row[-1] for row in rows if row and row[-1] != "lo")

    resolver, why = read(Path("/etc/resolv.conf"))
    if resolver is None:
        missing = True
        routes["/etc/resolv.conf"] = why
    else:
        routes["/etc/resolv.conf"] = resolver
        if any(
            line.lstrip().startswith(("nameserver ", "search ", "domain "))
            for line in resolver.splitlines()
        ):
            failures.append("/etc/resolv.conf")

    network_status = FAIL if failures else (NOT_PROVEN if missing else PASS)
    if failures:
        network_reason = f"non-loopback network authority: {sorted(set(failures))}"
    elif missing:
        network_reason = "route or resolver evidence unavailable"
    else:
        network_reason = "only loopback routes and no resolver authority"
    checks.append(result("network-disabled", network_status, network_reason, "machine.json"))
    write_json(
        evidence / "machine.json",
        {
            "os_release": values,
            "uname": {"machine": arch, "release": release, "system": os.uname().sysname},
            "route_files": routes,
            "effective_uid": os.geteuid(),
        },
    )
    return checks


def absent(name: str, paths: tuple[str, ...]) -> dict[str, str]:
    present = [path for path in paths if Path(path).exists()]
    reason = f"forbidden paths exist: {present}" if present else "no forbidden paths exist"
    return result(name, FAIL if present else PASS, reason, "fixed host paths")


def pg_processes() -> dict[str, str]:
    try:
        entries = list(Path("/proc").iterdir())
    except OSError as error:
        return result("host-postgresql-process-absent", NOT_PROVEN, str(error), "/proc")
    matches = []
    for entry in entries:
        if entry.name.isdecimal():
            comm, _ = read(entry / "comm")
            if comm and comm.strip() in PG_NAMES:
                matches.append(entry.name + ":" + comm.strip())
    reason = f"processes observed: {matches}" if matches else "no PostgreSQL process names observed"
    return result("host-postgresql-process-absent", FAIL if matches else PASS, reason, "/proc/*/comm")


def package(package_path: Path, evidence: Path) -> list[dict[str, str]]:
    checks: list[dict[str, str]] = []
    ok, reason = regular(package_path)
    if not ok:
        return [result("package-input", NOT_PROVEN, reason, str(package_path))]
    checks.append(result("package-input", PASS, "regular non-linked package", str(package_path)))

    dpkg_deb = Path("/usr/bin/dpkg-deb")
    ok, reason = regular(dpkg_deb)
    if not ok:
        return checks + [result("package-inspector", NOT_PROVEN, reason, str(dpkg_deb))]
    try:
        info_code, info, info_err = local([str(dpkg_deb), "--field", str(package_path)])
        contents_code, contents, contents_err = local([str(dpkg_deb), "--contents", str(package_path)])
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        return checks + [result("package-inspector", NOT_PROVEN, str(error), str(dpkg_deb))]
    if info_code or contents_code or info_err or contents_err:
        return checks + [
            result("package-inspector", NOT_PROVEN, "dpkg-deb rejected package input", str(package_path))
        ]

    write_bytes(evidence / "package.contents", contents)
    fields: dict[str, str] = {}
    for line in info.decode("utf-8", "replace").splitlines():
        if ": " in line:
            key, value = line.split(": ", 1)
            fields[key] = value
    identity_good = fields.get("Package") == "orna" and fields.get("Architecture") == "amd64"
    checks.append(
        result(
            "package-identity",
            PASS if identity_good else FAIL,
            "package is orna:amd64"
            if identity_good
            else f"observed package={fields.get('Package')} architecture={fields.get('Architecture')}",
            str(package_path),
        )
    )

    entries: list[tuple[str, str]] = []
    for line in contents.decode("utf-8", "replace").splitlines():
        match = re.search(r"\s(\./\S*)$", line)
        if not match or len(line) < 10:
            return checks + [
                result("package-inventory", NOT_PROVEN, f"unparseable contents line: {line!r}", "package.contents")
            ]
        entries.append((match.group(1), line[:10]))
    executables = [
        path for path, mode in entries if mode[0] == "-" and any(char in mode[1:10] for char in "xX")
    ]
    forbidden = [
        path
        for path, _ in entries
        if re.search(r"(?:^|/)(?:postgres|postmaster|psql|initdb|pg_ctl|pg_upgrade|pg_resetwal)$", path, re.I)
        or re.search(r"\.so(?:\.|$)|\.a$|\.o$", path, re.I)
    ]
    special = [path for path, mode in entries if mode[0] not in {"d", "-"}]
    inventory_good = executables == ["./usr/bin/orna"] and not forbidden and not special
    if inventory_good:
        inventory_reason = "one executable payload and no PostgreSQL artefact"
    elif forbidden:
        inventory_reason = f"forbidden artefacts: {forbidden}"
    elif special:
        inventory_reason = f"special entries: {special}"
    else:
        inventory_reason = f"executable payload is {executables}"
    checks.append(
        result("package-inventory", PASS if inventory_good else FAIL, inventory_reason, "package.contents")
    )

    try:
        digest = hashlib.sha256(package_path.read_bytes()).hexdigest()
    except OSError as error:
        checks.append(result("package-digest", NOT_PROVEN, str(error), str(package_path)))
    else:
        write_json(
            evidence / "package.json",
            {
                "fields": fields,
                "sha256": digest,
                "contents_sha256": hashlib.sha256(contents).hexdigest(),
            },
        )
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True)
    parser.add_argument("--evidence-dir", required=True)
    args = parser.parse_args()
    package_path = Path(args.package)
    evidence = Path(args.evidence_dir)
    if not package_path.is_absolute() or not evidence.is_absolute():
        raise SystemExit("--package and --evidence-dir must be absolute")
    if evidence.exists():
        if evidence.is_symlink() or not evidence.is_dir() or any(evidence.iterdir()):
            raise SystemExit("--evidence-dir must be a new empty non-linked directory")
    else:
        evidence.mkdir(mode=0o700, parents=False)
    os.chmod(evidence, 0o700)

    checks = machine(evidence)
    checks.extend(
        [
            absent("docker-absent", DOCKER_PATHS),
            absent("host-postgresql-installation-absent", PG_PATHS),
            pg_processes(),
        ]
    )
    observer_status = PASS if os.geteuid() == 0 else NOT_PROVEN
    observer_reason = "running as root" if os.geteuid() == 0 else "root required for complete process evidence"
    checks.append(result("root-observer", observer_status, observer_reason, "geteuid"))
    checks.extend(package(package_path, evidence))

    if all(item["status"] == PASS for item in checks):
        status = PASS
    elif any(item["status"] == FAIL for item in checks):
        status = FAIL
    else:
        status = NOT_PROVEN
    report = {
        "format": 1,
        "kind": "ADR0019_DEBIAN12_AMD64_HOST_PREFLIGHT",
        "preflight_status": status,
        "fresh_host_status": NOT_PROVEN,
        "complete_lifecycle_status": NOT_PROVEN,
        "policy": {
            "network": "NOT_USED",
            "docker": "NOT_USED",
            "host_postgresql": "NOT_USED",
            "deployment": "NOT_USED",
        },
        "checks": checks,
        "package": str(package_path),
    }
    write_json(evidence / "host-preflight.json", report)
    print(
        json.dumps(
            {
                "preflight_status": status,
                "fresh_host_status": NOT_PROVEN,
                "complete_lifecycle_status": NOT_PROVEN,
                "evidence_dir": str(evidence),
            },
            sort_keys=True,
        )
    )
    return 0 if status == PASS else 1


if __name__ == "__main__":
    raise SystemExit(main())
