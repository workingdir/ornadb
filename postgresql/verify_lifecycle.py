#!/usr/bin/env python3
"""Verify one linked PostgreSQL lifecycle without trusting runtime binaries."""

from __future__ import annotations

import json
from pathlib import Path
import stat
import sys


MARKERS = (
    "preload-complete",
    "reference-entry-start",
    "reference-entry-complete",
    "hostile-entry-start",
    "hostile-entry-complete",
    "initdb-start",
    "initdb-complete",
    "postmaster-start",
    "pgwire-ready",
    "query-complete",
    "postmaster-sigint",
    "complete",
)

REPORT = {
    "cluster_assertions": True,
    "credential_drop": True,
    "format": 1,
    "hostile_authority_rejected": True,
    "one_executable": True,
    "postmaster_clean_stop": True,
    "postmaster_sigquit_escalation": False,
    "support_members": 620,
}


def regular_bytes(path: Path, *, executable: bool = False) -> bytes:
    path_stat = path.lstat()
    mode = stat.S_IMODE(path_stat.st_mode)
    if not stat.S_ISREG(path_stat.st_mode) or path_stat.st_nlink != 1:
        raise SystemExit(f"lifecycle evidence is not one regular file: {path}")
    if executable != bool(mode & 0o111):
        raise SystemExit(f"lifecycle evidence has the wrong executable mode: {path}")
    return path.read_bytes()


def reject(trace: str, terms: tuple[str, ...], reason: str) -> None:
    if any(term in trace for term in terms):
        raise SystemExit(reason)


def main() -> None:
    if len(sys.argv) != 6:
        raise SystemExit("usage: verify_lifecycle.py TRACE_PREFIX MARKERS REPORT STDOUT PROBE")
    trace_prefix, markers_path, report_path, stdout_path, probe_path = map(Path, sys.argv[1:])
    for path in (trace_prefix, markers_path, report_path, stdout_path, probe_path):
        if not path.is_absolute():
            raise SystemExit(f"lifecycle evidence path is not absolute: {path}")

    markers = regular_bytes(markers_path).decode("utf-8").splitlines()
    if tuple(markers) != MARKERS:
        raise SystemExit("lifecycle phase markers are not exact")
    report = json.loads(regular_bytes(report_path))
    if report != REPORT:
        raise SystemExit("lifecycle report is not exact")
    if not regular_bytes(stdout_path):
        raise SystemExit("linked describe-config output is empty")
    probe = regular_bytes(probe_path, executable=True)
    if not probe.startswith(b"\x7fELF"):
        raise SystemExit("lifecycle probe is not an ELF executable")

    trace_paths = sorted(trace_prefix.parent.glob(f"{trace_prefix.name}.*"))
    if len(trace_paths) < 2:
        raise SystemExit("lifecycle trace does not contain linked child processes")
    traces = [(path, regular_bytes(path).decode("utf-8")) for path in trace_paths]
    combined = "".join(trace for _, trace in traces)
    parent_traces = [(path, trace) for path, trace in traces if 'write(2, "preload-complete"' in trace]
    if len(parent_traces) != 1:
        raise SystemExit("lifecycle trace does not contain one preload boundary")
    parent_path, parent_trace = parent_traces[0]
    before_preload, after_preload = parent_trace.split('write(2, "preload-complete"', 1)

    exec_lines = [line for _, trace in traces for line in trace.splitlines()
                  if line.startswith("execve(") or line.startswith("execveat(")]
    if len(exec_lines) != 1 or not exec_lines[0].startswith("execve("):
        raise SystemExit("lifecycle trace executed another program")
    if "PROT_EXEC" not in before_preload:
        raise SystemExit("lifecycle trace did not observe the initial ELF mappings")
    reject(after_preload, ("PROT_EXEC", "memfd_create(", "execve(", "execveat(", "SHM_EXEC", ".so"),
           "lifecycle parent gained executable authority after preload")
    for path, trace in traces:
        if path != parent_path:
            reject(trace, ("PROT_EXEC", "memfd_create(", "execve(", "execveat(", "SHM_EXEC", ".so"),
                   "a linked PostgreSQL child gained executable authority")
    reject(combined, ("AF_INET,", "AF_INET6,", '"/tmp/', "postgres.so"),
           "lifecycle trace used forbidden network, temporary, or shared-object authority")
    if "socket(AF_UNIX" not in combined:
        raise SystemExit("lifecycle trace did not use the private Unix socket")
    trace_lines = combined.splitlines()
    no_new_privileges = sum(
        "prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)" in line and line.rstrip().endswith("= 0")
        for line in trace_lines
    )
    filters = sum(
        "seccomp(SECCOMP_SET_MODE_FILTER, 0," in line and line.rstrip().endswith("= 0")
        for line in trace_lines
    )
    if no_new_privileges < 7 or filters < 7:
        raise SystemExit("lifecycle trace did not install the linked-entry filters")


if __name__ == "__main__":
    main()
