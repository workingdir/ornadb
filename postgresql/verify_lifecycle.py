#!/usr/bin/env python3
"""Verify one linked PostgreSQL lifecycle without trusting runtime binaries."""

from __future__ import annotations

import json
from pathlib import Path
import re
import stat
import sys


BASE_MARKERS = (
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
ESCALATION_MARKER = "postmaster-sigquit"


REPORT_FIELDS = {
    "cluster_assertions": True,
    "credential_drop": True,
    "format": 1,
    "hostile_authority_rejected": True,
    "one_executable": True,
    "postmaster_clean_stop": True,
    "support_members": 620,
    "x32_syscall_rejected": True,
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


def trace_pid(path: Path, trace_prefix: Path) -> int:
    prefix = f"{trace_prefix.name}."
    if not path.name.startswith(prefix):
        raise SystemExit(f"lifecycle trace shard has the wrong prefix: {path}")
    suffix = path.name[len(prefix):]
    if not suffix.isdecimal() or suffix == "0":
        raise SystemExit(f"lifecycle trace shard has no stable process identity: {path}")
    return int(suffix)


def marker_indexes(lines: list[str], marker: str) -> list[int]:
    needle = f'write(2, "{marker}", '
    result = re.compile(rf"\)\s+=\s+{len(marker)}$")
    return [
        index
        for index, line in enumerate(lines)
        if needle in line and result.search(line.rstrip()) is not None
    ]


def signal_kills(lines: list[str], signal: str) -> list[tuple[int, int]]:
    pattern = re.compile(rf"^kill\((\d+), {signal}\)\s+=\s+0$")
    return [
        (index, int(match.group(1)))
        for index, line in enumerate(lines)
        if (match := pattern.match(line.strip())) is not None
    ]


def terminal_wait_indexes(lines: list[str], process: int) -> list[int]:
    pattern = re.compile(rf"^(?:wait4|waitpid)\({process},.*\)\s+=\s+{process}(?:\s|$)")
    return [
        index
        for index, line in enumerate(lines)
        if pattern.match(line.strip()) is not None
    ]


def signal_delivery_indexes(lines: list[str], signal: str, sender: int) -> list[int]:
    needle = f"--- {signal} {{"
    sender_needle = f"si_pid={sender},"
    return [
        index
        for index, line in enumerate(lines)
        if needle in line and sender_needle in line and line.rstrip().endswith(" ---")
    ]


def verify_stop_evidence(
    trace_prefix: Path,
    traces: list[tuple[Path, str]],
    markers: tuple[str, ...],
    parent_path: Path,
    parent_trace: str,
) -> bool:
    parent_pid = trace_pid(parent_path, trace_prefix)
    parent_lines = parent_trace.splitlines()
    observed_markers = tuple(
        marker
        for line in parent_lines
        for marker in BASE_MARKERS[:-1] + (ESCALATION_MARKER, BASE_MARKERS[-1])
        if marker_indexes([line], marker)
    )
    if observed_markers != markers:
        raise SystemExit("lifecycle markers are not correlated with the parent trace")

    sigint_kills = signal_kills(parent_lines, "SIGINT")
    if len(sigint_kills) != 1:
        raise SystemExit("lifecycle trace does not contain one postmaster SIGINT")
    sigint_index, postmaster_pid = sigint_kills[0]
    sigint_marker = marker_indexes(parent_lines, "postmaster-sigint")
    if len(sigint_marker) != 1 or sigint_index <= sigint_marker[0]:
        raise SystemExit("lifecycle trace does not order SIGINT after its marker")

    sigquit_kills = signal_kills(parent_lines, "SIGQUIT")
    if len(sigquit_kills) > 1:
        raise SystemExit("lifecycle trace contains repeated postmaster SIGQUIT requests")
    observed_escalated = bool(sigquit_kills)
    if observed_escalated:
        if sigquit_kills[0][1] != postmaster_pid:
            raise SystemExit("lifecycle escalation targets a non-postmaster process")
        sigquit_index, _ = sigquit_kills[0]
        sigquit_marker = marker_indexes(parent_lines, ESCALATION_MARKER)
        if len(sigquit_marker) != 1 or sigquit_index <= sigquit_marker[0]:
            raise SystemExit("lifecycle trace does not order SIGQUIT after its marker")
    elif marker_indexes(parent_lines, ESCALATION_MARKER):
        raise SystemExit("lifecycle escalation marker lacks a SIGQUIT request")

    trace_by_pid = {trace_pid(path, trace_prefix): trace for path, trace in traces}
    postmaster_trace = trace_by_pid.get(postmaster_pid)
    if postmaster_trace is None:
        raise SystemExit("lifecycle trace is missing the postmaster process")
    postmaster_lines = postmaster_trace.splitlines()
    sigint_delivery = signal_delivery_indexes(postmaster_lines, "SIGINT", parent_pid)
    if len(sigint_delivery) != 1:
        raise SystemExit("lifecycle trace does not observe postmaster SIGINT delivery")
    stop_index = sigint_index
    if observed_escalated:
        stop_index = sigquit_kills[0][0]
    terminal_waits = terminal_wait_indexes(parent_lines, postmaster_pid)
    if not any(index > stop_index for index in terminal_waits):
        raise SystemExit("lifecycle trace does not observe the terminal postmaster wait")
    clean_exit = re.compile(r"^exit_group\(0\)\s+=\s+\?$")
    exit_indexes = [
        index
        for index, line in enumerate(postmaster_lines)
        if line.strip() == "+++ exited with 0 +++" or clean_exit.match(line.strip())
    ]
    if not exit_indexes or exit_indexes[-1] <= sigint_delivery[0]:
        raise SystemExit("lifecycle trace does not observe a clean postmaster exit")

    if observed_escalated:
        sigquit_delivery = signal_delivery_indexes(postmaster_lines, "SIGQUIT", parent_pid)
        if len(sigquit_delivery) != 1:
            raise SystemExit("lifecycle escalation lacks postmaster SIGQUIT delivery")
        if sigquit_delivery[0] <= sigint_delivery[0]:
            raise SystemExit("lifecycle escalation delivered SIGQUIT before SIGINT")
        if not exit_indexes or exit_indexes[-1] <= sigquit_delivery[0]:
            raise SystemExit("lifecycle escalation lacks a terminal postmaster exit")
    return observed_escalated


def main() -> None:
    if len(sys.argv) != 6:
        raise SystemExit("usage: verify_lifecycle.py TRACE_PREFIX MARKERS REPORT STDOUT PROBE")
    trace_prefix, markers_path, report_path, stdout_path, probe_path = map(Path, sys.argv[1:])
    for path in (trace_prefix, markers_path, report_path, stdout_path, probe_path):
        if not path.is_absolute():
            raise SystemExit(f"lifecycle evidence path is not absolute: {path}")

    markers = tuple(regular_bytes(markers_path).decode("utf-8").splitlines())
    escalated_markers = BASE_MARKERS[:-1] + (ESCALATION_MARKER, BASE_MARKERS[-1])
    if markers == BASE_MARKERS:
        marker_escalated = False
    elif markers == escalated_markers:
        marker_escalated = True
    else:
        raise SystemExit("lifecycle phase markers are not exact")
    report = json.loads(regular_bytes(report_path))
    if not regular_bytes(stdout_path):
        raise SystemExit("linked describe-config output is empty")
    probe = regular_bytes(probe_path, executable=True)
    if not probe.startswith(b"\x7fELF"):
        raise SystemExit("lifecycle probe is not an ELF executable")

    trace_paths = sorted(
        trace_prefix.parent.glob(f"{trace_prefix.name}.*"),
        key=lambda path: path.name,
    )
    if len(trace_paths) < 2:
        raise SystemExit("lifecycle trace does not contain linked child processes")
    traces = [(path, regular_bytes(path).decode("utf-8")) for path in trace_paths]
    trace_ids = [trace_pid(path, trace_prefix) for path, _ in traces]
    if len(set(trace_ids)) != len(trace_ids):
        raise SystemExit("lifecycle trace contains duplicate process identities")
    combined = "".join(trace for _, trace in traces)
    parent_traces = [
        (path, trace)
        for path, trace in traces
        if len(marker_indexes(trace.splitlines(), "preload-complete")) == 1
    ]
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
    observed_escalated = verify_stop_evidence(
        trace_prefix, traces, markers, parent_path, parent_trace
    )
    if observed_escalated != marker_escalated:
        raise SystemExit("lifecycle escalation evidence disagrees with the markers")
    expected_report = {**REPORT_FIELDS, "postmaster_sigquit_escalation": observed_escalated}
    if report != expected_report:
        raise SystemExit("lifecycle report is not exact")


if __name__ == "__main__":
    main()
