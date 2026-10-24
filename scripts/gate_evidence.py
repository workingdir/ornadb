#!/usr/bin/env python3
"""Record Orna 1.1.0 acceptance-gate evidence from `cargo test` runs.

`run` executes one `cargo test` command and writes a JSON record next to its
log: the tested commit, the exact command, host and tool versions, inputs, the
exit status, every executed test target with per-test outcomes, the log's
SHA-256, and the failures split into those listed in the known-failure
baseline and new ones. It exits 1 when a failure is not in the baseline, when
the command fails in a way the log does not explain, when no test ran, or when
a prerequisite is missing.

`summarize` merges records into one evidence summary with one entry per gate
item from the gate-item manifest. Items and gates take the four ACCEPTANCE
states: passed, failed, not-executed and unavailable. A gate is passed only
when every item passed and no item declares a limitation, so whole-gate
completion is never derived from a passing subset.
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable

WORKSPACE_ROOT = Path(__file__).resolve().parents[1]
BASELINE = WORKSPACE_ROOT / "scripts/known-test-failures.toml"
ITEMS = WORKSPACE_ROOT / "scripts/gate-evidence-items.toml"
SCHEMA = "ornadb-gate-evidence/1"
# Bump when flags, record fields or output formats change, so CI logs show which interface ran.
RECORDER_VERSION = "2"
STATES = ("passed", "failed", "not-executed", "unavailable")
# Worst first: a gate or item takes the first state any of its parts has.
STATE_PRECEDENCE = ("failed", "unavailable", "not-executed", "passed")
GATES = tuple("ABCDEFGH")
BEAD_PATTERN = re.compile(r"ornadb-[0-9a-z]+(?:\.[0-9]+)*")
TARGET_PATTERN = re.compile(r"(?:lib|doc|(?:bin|test|example|bench):[A-Za-z0-9_-]+|check:[a-z0-9-]+)")
RECORD_PATTERN = re.compile(r"[a-z0-9][a-z0-9-]{0,63}")
CHECK_PATTERN = re.compile(r"[a-z0-9][a-z0-9-]{0,31}")
# A GitHub Actions job ID.
JOB_PATTERN = re.compile(r"[A-Za-z_][A-Za-z0-9_-]*")
DEFAULT_INPUT_ENV = (
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_DEBUG",
    "RUSTFLAGS",
    "RUST_TEST_THREADS",
)
LIB_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}
# How long the reader waits for the output pipe to close once the command has exited.
OUTPUT_GRACE_SECONDS = 60

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
RUNNING = re.compile(r"^\s*Running (?:unittests )?(?P<source>\S+) \((?P<executable>[^()]+)\)\s*$")
DOC_TESTS = re.compile(r"^\s*Doc-tests (?P<crate>\S+)\s*$")
TEST_LINE = re.compile(r"^test (?P<name>.+?)(?: - should panic)? \.\.\. (?P<outcome>ok|FAILED|ignored)\b")
RESULT_LINE = re.compile(
    r"^test result: (?:ok|FAILED)\. (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
    r"(?P<ignored>\d+) ignored; (?P<measured>\d+) measured; (?P<filtered_out>\d+) filtered out"
)
OUTPUT_BLOCK = re.compile(r"^---- (?P<name>.+) stdout ----$")
LIST_HEADER = re.compile(r"^(?P<kind>successes|failures):$")
LIST_ENTRY = re.compile(r"^    (?P<name>\S.*)$")
RUNNING_COUNT = re.compile(r"^running (?P<count>\d+) tests?$")
RERUN = re.compile(r"^error: test failed, to rerun pass `(?P<args>[^`]+)`")
BUILD_ERROR = re.compile(
    r"^error: (?:could not compile `(?P<package>[^`]+)`(?: \((?P<what>[^)]*)\))?"
    r"|failed to run custom build command for `(?P<script>[^`\s]+)[^`]*`)"
)
RESULT_COUNTS = re.compile(r"^test result: (?:ok|FAILED)\. (?P<passed>\d+) passed; (?P<failed>\d+) failed")
# Tests in this workspace report an early return as `SKIP: ...` or `... skipped: set NAME=1 ...`.
SKIP_LINE = re.compile(r"^SKIP: |\bskipped: set [A-Z][A-Z0-9_]*=")
COUNTER_PAIR = re.compile(r"=(?P<value>\S*)")
OUTCOMES = {"ok": "passed", "FAILED": "failed", "ignored": "ignored"}


class EvidenceError(Exception):
    """A record, baseline or manifest violates the evidence contract."""


@dataclass
class TargetRun:
    package: str
    target: str
    executable: str
    completed: bool = False
    counts: dict[str, int] = field(default_factory=dict)
    tests: dict[str, str] = field(default_factory=dict)
    # The `running N tests` count of this binary; tests that re-run their own binary
    # as a child print further `running`/`test result` lines into the same section.
    expected: int | None = field(default=None, compare=False)

    def as_json(self) -> dict:
        return {
            "package": self.package,
            "target": self.target,
            "executable": self.executable,
            "completed": self.completed,
            "counts": self.counts,
            "tests": dict(sorted(self.tests.items())),
        }


@dataclass
class ParsedLog:
    targets: list[TargetRun] = field(default_factory=list)
    reruns: list[tuple[str, str]] = field(default_factory=list)
    build_errors: list[tuple[str, str]] = field(default_factory=list)


@dataclass(frozen=True)
class Failure:
    package: str
    target: str
    test: str
    reason: str = field(default="", compare=False)

    def as_json(self) -> dict:
        return {"package": self.package, "target": self.target, "test": self.test, "reason": self.reason}


@dataclass(frozen=True)
class KnownFailure:
    package: str
    target: str
    test: str
    bead: str
    reason: str
    record: str = "*"

    def matches(self, failure: Failure, record: str) -> bool:
        return fnmatch.fnmatchcase(record, self.record) and (self.package, self.target, self.test) == (
            failure.package,
            failure.target,
            failure.test,
        )

    def as_json(self) -> dict:
        return {
            "package": self.package,
            "target": self.target,
            "test": self.test,
            "bead": self.bead,
            "reason": self.reason,
            "record": self.record,
        }


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def target_flags(target: str) -> list[str]:
    """Return the cargo flags that select one target label."""
    if target in ("lib", "doc"):
        return [f"--{target}"]
    kind, _, name = target.partition(":")
    return [f"--{kind}", name]


def cargo_test_command(package: str, target: str, features: str | None, skips: list[str]) -> list[str]:
    """Return the cargo command that runs one test target, skipping the named libtest tests exactly."""
    command = ["cargo", "test", "--locked", "-p", package, *target_flags(target)]
    if features:
        command += ["--features", features]
    return command + ["--", "--exact", *[argument for name in skips for argument in ("--skip", name)], "--nocapture"]


def parse_timeout_override(text: str) -> tuple[str, float]:
    """Parse TARGET=MINUTES, for example test:media_import_query=150."""
    label, separator, minutes = text.rpartition("=")
    if not separator or not TARGET_PATTERN.fullmatch(label):
        raise argparse.ArgumentTypeError(f"{text!r} is not TARGET=MINUTES, for example test:NAME=150")
    try:
        value = float(minutes)
    except ValueError:
        raise argparse.ArgumentTypeError(f"{text!r}: minutes must be a number") from None
    if value <= 0:
        raise argparse.ArgumentTypeError(f"{text!r}: minutes must be positive")
    return label, value


def parse_covered_by(text: str) -> tuple[str, str]:
    """Parse TARGET=JOB, for example test:publication_crash_matrix=publication-crash-matrix."""
    label, separator, job = text.rpartition("=")
    if not separator or not TARGET_PATTERN.fullmatch(label) or not JOB_PATTERN.fullmatch(job):
        raise argparse.ArgumentTypeError(f"{text!r} is not TARGET=JOB, for example test:NAME=job-id")
    return label, job


def timeout_for(target: str, default: float | None, overrides: dict[str, float]) -> float | None:
    """Return the watchdog minutes for one target label: its override, else the default."""
    return overrides.get(target, default)


def target_label(kinds: list[str], name: str) -> str:
    if LIB_KINDS.intersection(kinds):
        return "lib"
    for kind in ("bin", "test", "example", "bench"):
        if kind in kinds:
            return f"{kind}:{name}"
    return f"{'+'.join(kinds) or 'unknown'}:{name}"


def package_name(package_id: str) -> str:
    """Return the package name from a Cargo package ID in either the old or the spec format."""
    if " (" in package_id:
        return package_id.split(" ", 1)[0]
    source, _, fragment = package_id.partition("#")
    if "@" in fragment:
        return fragment.split("@", 1)[0]
    # `path+file:///.../name#1.0.0` omits the name when it equals the last path segment.
    return source.rstrip("/").rsplit("/", 1)[-1]


def index_artifact(message: dict, executables: dict[str, tuple[str, str]], libraries: dict[str, str]) -> None:
    """Map test executables and library crate names to their package and target."""
    if message.get("reason") != "compiler-artifact":
        return
    target = message.get("target") or {}
    kinds = list(target.get("kind") or [])
    name = str(target.get("name", ""))
    package = package_name(str(message.get("package_id", "")))
    if LIB_KINDS.intersection(kinds):
        libraries[name.replace("-", "_")] = package
    executable = message.get("executable")
    if executable and (message.get("profile") or {}).get("test"):
        executables[Path(executable).name] = (package, target_label(kinds, name))


def fallback_target(source: str, executable_name: str) -> tuple[str, str]:
    """Label a test binary that no build message described."""
    stem = executable_name.rsplit("-", 1)[0]
    if source == "src/lib.rs":
        return "<unknown>", "lib"
    if source.startswith("tests/"):
        return "<unknown>", f"test:{stem}"
    return "<unknown>", f"bin:{stem}"


def parse_rerun(arguments: str) -> tuple[str, str] | None:
    """Parse Cargo's `-p PACKAGE --test NAME` rerun hint into a package and target label."""
    words = shlex.split(arguments)
    package = None
    target = None
    index = 0
    while index < len(words):
        word = words[index]
        if word in ("-p", "--package") and index + 1 < len(words):
            package = words[index + 1]
            index += 2
            continue
        if word in ("--lib", "--doc"):
            target = word[2:]
        elif word in ("--bin", "--test", "--example", "--bench") and index + 1 < len(words):
            target = f"{word[2:]}:{words[index + 1]}"
            index += 1
        index += 1
    if package is None or target is None:
        return None
    return package, target


def build_target(what: str | None) -> str:
    """Label the target named by `error: could not compile `pkg` (test "name")`."""
    if not what:
        return "<build>"
    match = re.match(r'(bin|test|example|bench) "([^"]+)"', what)
    if match:
        return f"{match.group(1)}:{match.group(2)}"
    if what.startswith("lib"):
        return "lib"
    return what.replace(" ", "-")


def parse_log(
    lines: Iterable[str],
    executables: dict[str, tuple[str, str]],
    libraries: dict[str, str],
) -> ParsedLog:
    """Parse cargo and libtest human output into per-target, per-test outcomes.

    Libtest must run with captured output and `--show-output`, so each test's
    output is printed in its own `---- NAME stdout ----` block and the
    `successes:` and `failures:` name lists close each binary's run. Those lists
    are authoritative: a child process writing to the shared stderr can split a
    `test NAME ... ok` line, but not the lists libtest prints after every test
    finished. A passing test whose output reports an early return is recorded as
    `skipped`.
    """
    parsed = ParsedLog()
    current: TargetRun | None = None
    block: str | None = None
    listing: str | None = None
    outputs: dict[str, list[str]] = {}

    def close() -> None:
        nonlocal current, block, listing, outputs
        if current is not None:
            for name, text in outputs.items():
                if current.tests.get(name) == "passed" and any(SKIP_LINE.search(line) for line in text):
                    current.tests[name] = "skipped"
            parsed.targets.append(current)
        current = None
        block = None
        listing = None
        outputs = {}

    for raw in lines:
        line = ANSI.sub("", raw.rstrip("\r\n"))
        if block is not None:
            header = OUTPUT_BLOCK.match(line)
            if header:
                block = header.group("name")
                outputs.setdefault(block, [])
                continue
            heading = LIST_HEADER.match(line)
            if heading:
                block = None
                listing = heading.group("kind")
                continue
            if not RESULT_LINE.match(line):
                outputs[block].append(line)
                continue
            block = None
        if listing is not None:
            entry = LIST_ENTRY.match(line)
            if entry and current is not None:
                current.tests[entry.group("name")] = "passed" if listing == "successes" else "failed"
                continue
            listing = None
        rerun = RERUN.match(line)
        if rerun:
            selected = parse_rerun(rerun.group("args"))
            if selected is not None:
                parsed.reruns.append(selected)
            continue
        failed_build = BUILD_ERROR.match(line)
        if failed_build:
            script = failed_build.group("script")
            if script:
                parsed.build_errors.append((script.split(" ", 1)[0], "<build>"))
            else:
                parsed.build_errors.append((failed_build.group("package"), build_target(failed_build.group("what"))))
            continue
        running = RUNNING.match(line)
        if running:
            close()
            executable = Path(running.group("executable").strip()).name
            package, target = executables.get(executable) or fallback_target(running.group("source"), executable)
            current = TargetRun(package, target, executable)
            continue
        doc_tests = DOC_TESTS.match(line)
        if doc_tests:
            close()
            crate = doc_tests.group("crate")
            current = TargetRun(libraries.get(crate, f"<unknown:{crate}>"), "doc", f"doc:{crate}")
            continue
        if current is None:
            continue
        header = OUTPUT_BLOCK.match(line)
        if header:
            block = header.group("name")
            outputs.setdefault(block, [])
            continue
        heading = LIST_HEADER.match(line)
        if heading:
            listing = heading.group("kind")
            continue
        running_count = RUNNING_COUNT.match(line)
        if running_count:
            if current.expected is None:
                current.expected = int(running_count.group("count"))
            continue
        test = TEST_LINE.match(line)
        if test:
            current.tests[test.group("name")] = OUTCOMES[test.group("outcome")]
            continue
        result = RESULT_LINE.match(line)
        if result:
            counts = {key: int(value) for key, value in result.groupdict().items()}
            ran = counts["passed"] + counts["failed"] + counts["ignored"] + counts["measured"]
            if current.expected is not None and ran != current.expected:
                continue
            current.counts = counts
            current.completed = True
            close()
    close()
    return parsed


def executed_tests(text: str) -> int:
    """Count the tests cargo actually ran (passed or failed) from its `test result:` lines."""
    total = 0
    for line in text.splitlines():
        counts = RESULT_COUNTS.match(line.strip())
        if counts:
            total += int(counts.group("passed")) + int(counts.group("failed"))
    return total


def observed_failures(parsed: ParsedLog, exit_status: int | None, timed_out: bool) -> list[Failure]:
    """Return every failure the log shows, plus one for any failure it does not explain."""
    failures: list[Failure] = []
    for target in parsed.targets:
        failed = [name for name, outcome in target.tests.items() if outcome == "failed"]
        failures.extend(Failure(target.package, target.target, name, "test failed") for name in sorted(failed))
        if not target.completed:
            failures.append(Failure(target.package, target.target, "*", "test binary did not finish (crash, abort or kill)"))
        elif target.counts.get("failed", 0) > len(failed):
            failures.append(Failure(target.package, target.target, "*", "failed count exceeds the failed tests the log names"))
    reported = {(failure.package, failure.target) for failure in failures}
    # A binary no build message described carries an <unknown> package; its rerun hint names the same target.
    unlabelled = {failure.target for failure in failures if failure.package.startswith("<unknown")}
    for package, target in parsed.reruns:
        if (package, target) not in reported and target not in unlabelled:
            failures.append(Failure(package, target, "*", "cargo reported the target failed"))
            reported.add((package, target))
    for package, target in parsed.build_errors:
        failures.append(Failure(package, target, "<build>", "build failed"))
    if timed_out and not any(not target.completed for target in parsed.targets):
        failures.append(Failure("*", "*", "<timeout>", "the recorder killed the command at its timeout"))
    elif not timed_out and exit_status not in (0, None) and not (parsed.build_errors or parsed.reruns):
        failures.append(Failure("*", "*", "<exit>", f"cargo exited {exit_status} without a failure the log explains"))
    return failures


def load_baseline(path: Path) -> list[KnownFailure]:
    """Load and validate the known-failure baseline.

    An entry names one test with `test`, or several tests of one target that share
    a bead and reason with `tests`.
    """
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    unexpected = set(data) - {"failure"}
    if unexpected:
        raise EvidenceError(f"{path.name}: unexpected top-level keys: {', '.join(sorted(unexpected))}")
    entries: list[KnownFailure] = []
    seen: set[tuple[str, str, str, str]] = set()
    for index, entry in enumerate(data.get("failure", []), 1):
        where = f"{path.name}: failure {index}"
        keys = set(entry)
        missing = {"package", "target", "bead", "reason"} - keys
        extra = keys - {"package", "target", "test", "tests", "bead", "reason", "record"}
        if missing or extra or ("test" in keys) == ("tests" in keys):
            raise EvidenceError(
                f"{where}: needs package, target, bead, reason and one of test or tests; "
                f"missing {sorted(missing)}, unexpected {sorted(extra)}"
            )
        package, target, bead, reason = (str(entry[key]) for key in ("package", "target", "bead", "reason"))
        tests = [str(entry["test"])] if "test" in entry else [str(name) for name in entry["tests"]]
        if not package or package == "*":
            raise EvidenceError(f"{where}: package must name one Cargo package")
        if not TARGET_PATTERN.fullmatch(target):
            raise EvidenceError(f"{where}: target {target!r} is not lib, doc or KIND:NAME")
        if not tests or any(not test or test.startswith("<") for test in tests):
            raise EvidenceError(f"{where}: tests must be libtest names, or * for a whole-target failure")
        if not BEAD_PATTERN.fullmatch(bead):
            raise EvidenceError(f"{where}: bead {bead!r} is not an ornadb bead ID")
        if not reason.strip():
            raise EvidenceError(f"{where}: reason must say why the test fails")
        record = str(entry.get("record", "*"))
        if not record or not re.fullmatch(r"[a-z0-9*?-]+", record):
            raise EvidenceError(f"{where}: record must be a record ID or glob")
        for test in tests:
            key = (package, target, test, record)
            if key in seen:
                raise EvidenceError(f"{where}: duplicate entry for {package} {target} {test}")
            seen.add(key)
            entries.append(KnownFailure(package, target, test, bead, reason.strip(), record))
    return entries


def classify(
    failures: list[Failure], baseline: list[KnownFailure], record: str
) -> tuple[list[Failure], list[tuple[Failure, KnownFailure]]]:
    new: list[Failure] = []
    known: list[tuple[Failure, KnownFailure]] = []
    for failure in failures:
        entry = next((candidate for candidate in baseline if candidate.matches(failure, record)), None)
        if entry is None:
            new.append(failure)
        else:
            known.append((failure, entry))
    return new, known


def baseline_status(entry: KnownFailure, record: str, targets: list[dict], failures: list[dict]) -> str | None:
    """Return observed, passed, or None when this record did not execute the entry's target."""
    if not fnmatch.fnmatchcase(record, entry.record):
        return None
    key = (entry.package, entry.target, entry.test)
    if any((failure["package"], failure["target"], failure["test"]) == key for failure in failures):
        return "observed"
    for target in targets:
        if (target["package"], target["target"]) != (entry.package, entry.target) or not target["completed"]:
            continue
        if entry.test == "*":
            if "passed" in target["tests"].values():
                return "passed"
        elif target["tests"].get(entry.test) == "passed":
            return "passed"
    return None


def prepare_command(command: list[str]) -> list[str]:
    """Return the cargo test argv with JSON build messages and per-test output blocks."""
    if len(command) < 2 or Path(command[0]).name != "cargo" or "test" not in command[1:]:
        raise EvidenceError("the command must be a `cargo test` invocation")
    if "--" in command:
        split = command.index("--")
        cargo_args, test_args = command[:split], command[split + 1 :]
    else:
        cargo_args, test_args = list(command), []
    for argument in cargo_args:
        if argument in ("-q", "--quiet") or argument.startswith("--message-format"):
            raise EvidenceError(f"cargo option {argument} changes the output the recorder parses")
    for argument in test_args:
        if argument.split("=", 1)[0] in ("--nocapture", "--no-capture", "--format", "-q", "--quiet", "--logfile"):
            raise EvidenceError(f"test harness option {argument} changes the output the recorder parses")
    if "--show-output" not in test_args:
        test_args = [*test_args, "--show-output"]
    return [*cargo_args, "--message-format=json-render-diagnostics", "--", *test_args]


def first_line(argv: list[str]) -> str:
    """Return the first non-empty output line of a version command, or why it is unavailable."""
    if shutil.which(argv[0]) is None:
        return "unavailable: not installed"
    try:
        completed = subprocess.run(argv, capture_output=True, text=True, errors="replace", timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        return f"unavailable: {error}"
    for line in (completed.stdout + "\n" + completed.stderr).splitlines():
        if line.strip():
            return line.strip()
    return f"unavailable: no output (exit {completed.returncode})"


def git_text(*args: str) -> str:
    completed = subprocess.run(["git", *args], capture_output=True, text=True, check=False)
    return completed.stdout.strip() if completed.returncode == 0 else ""


def submodule_pins() -> dict[str, str]:
    pins = {}
    for line in git_text("submodule", "status", "--recursive").splitlines():
        parts = line[1:].split()
        if len(parts) >= 2:
            state = {"-": "not initialized", "+": "checkout differs from gitlink", "U": "conflicted"}.get(line[0], "")
            pins[parts[1]] = parts[0] + (f" ({state})" if state else "")
    return pins


def environment_facts(tools: list[str], input_env: list[str]) -> dict:
    uname = platform.uname()
    facts = {
        "host": {
            "system": f"{uname.system} {uname.release} {uname.version} {uname.machine}",
            "runner_image": " ".join(filter(None, (os.environ.get("ImageOS"), os.environ.get("ImageVersion")))) or None,
            "cpus": os.cpu_count(),
        },
        "tools": {
            "recorder": f"gate_evidence.py {RECORDER_VERSION}",
            "python": sys.version.split()[0],
        },
        "inputs": {
            "env": {name: os.environ.get(name) for name in (*DEFAULT_INPUT_ENV, *input_env)},
            "submodules": submodule_pins(),
        },
    }
    for command in ("rustc -V", "cargo -V", "git --version", *tools):
        argv = shlex.split(command)
        facts["tools"][command] = first_line(argv)
    return facts


def workflow_run() -> dict | None:
    if not os.environ.get("GITHUB_RUN_ID"):
        return None
    server = os.environ.get("GITHUB_SERVER_URL", "https://github.com")
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    return {
        "repository": repository,
        "workflow": os.environ.get("GITHUB_WORKFLOW"),
        "job": os.environ.get("GITHUB_JOB"),
        "event": os.environ.get("GITHUB_EVENT_NAME"),
        "run_id": os.environ.get("GITHUB_RUN_ID"),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
        "url": f"{server}/{repository}/actions/runs/{os.environ['GITHUB_RUN_ID']}",
    }


def execute(argv: list[str], log_path: Path, timeout_minutes: float | None) -> tuple[int | None, bool, dict, dict]:
    """Run argv, stream its human output to the console and the log, and index build messages.

    A process the command leaves behind can keep the output pipe open after the
    command exits. The reader then gets OUTPUT_GRACE_SECONDS before the leftover
    process group is killed and reading stops, so the record is still written.
    """
    executables: dict[str, tuple[str, str]] = {}
    libraries: dict[str, str] = {}
    environment = {**os.environ, "CARGO_TERM_COLOR": "never"}
    lock = threading.Lock()
    detached = False
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.Popen(
            argv,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=environment,
            start_new_session=True,
        )

        def kill_group() -> None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

        def pump() -> None:
            assert process.stdout is not None
            for line in process.stdout:
                with lock:
                    if detached:
                        return
                    if line.startswith('{"reason":'):
                        try:
                            index_artifact(json.loads(line), executables, libraries)
                            continue
                        except json.JSONDecodeError:
                            pass
                    log.write(line)
                    sys.stdout.write(line)

        reader = threading.Thread(target=pump, daemon=True)
        reader.start()
        timed_out = False
        try:
            status = process.wait(timeout=timeout_minutes * 60 if timeout_minutes else None)
        except subprocess.TimeoutExpired:
            timed_out = True
            kill_group()
            status = process.wait()
        reader.join(timeout=OUTPUT_GRACE_SECONDS)
        if reader.is_alive():
            kill_group()
            reader.join(timeout=OUTPUT_GRACE_SECONDS)
        with lock:
            if reader.is_alive():
                detached = True
                log.write("gate_evidence: a leftover process still holds the command output; stopped reading\n")
    return status, timed_out, executables, libraries


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=False) + "\n", encoding="utf-8")


def check_outcome(name: str, argv: list[str], status: int | None, timed_out: bool) -> tuple[ParsedLog, list[Failure]]:
    """Record a non-test command (formatting, lints) as one target with one outcome."""
    passed = status == 0 and not timed_out
    target = TargetRun(
        "workspace",
        f"check:{name}",
        Path(argv[0]).name,
        completed=not timed_out,
        tests={name: "passed" if passed else "failed"},
    )
    if passed:
        return ParsedLog(targets=[target]), []
    reason = "the recorder killed the command at its timeout" if timed_out else f"command exited {status}"
    return ParsedLog(targets=[target]), [Failure("workspace", f"check:{name}", name, reason)]


def command_run(arguments: argparse.Namespace) -> int:
    if not RECORD_PATTERN.fullmatch(arguments.record):
        raise EvidenceError(f"record ID {arguments.record!r} must be lowercase letters, digits and hyphens")
    if arguments.check is not None and not CHECK_PATTERN.fullmatch(arguments.check):
        raise EvidenceError(f"check name {arguments.check!r} must be lowercase letters, digits and hyphens")
    if not arguments.command:
        raise EvidenceError("no command given after --")
    baseline = load_baseline(Path(arguments.baseline))
    argv = list(arguments.command) if arguments.check else prepare_command(arguments.command)
    out = Path(arguments.out)
    out.mkdir(parents=True, exist_ok=True)
    log_path = out / f"{arguments.record}.log"
    record_path = out / f"{arguments.record}.record.json"
    prerequisites = [
        *({"requirement": f"command {name}", "available": shutil.which(name) is not None} for name in arguments.require_command),
        *({"requirement": f"path {path}", "available": Path(path).exists()} for path in arguments.require_path),
    ]
    record = {
        "schema": SCHEMA,
        "record": arguments.record,
        "title": arguments.title,
        "state": "not-executed",
        "commit": git_text("rev-parse", "HEAD") or None,
        "tree_clean": git_text("status", "--porcelain", "--untracked-files=no") == "",
        "command": shlex.join(argv),
        "argv": argv,
        "workflow_run": workflow_run(),
        **environment_facts(arguments.tool_version, arguments.input_env),
        "prerequisites": prerequisites,
        "started_at": utc_now(),
        "finished_at": None,
        "elapsed_seconds": None,
        "exit_status": None,
        "timed_out": False,
        "timeout_minutes": arguments.timeout_minutes,
        "log": {"file": log_path.name, "sha256": None, "bytes": None},
        "totals": {},
        "targets": [],
        "failures": {"new": [], "known": []},
        "baseline": [],
        "reviewer": None,
    }
    missing = [entry["requirement"] for entry in prerequisites if not entry["available"]]
    started = time.monotonic()
    failures: list[Failure] = []
    new: list[Failure] = []
    known: list[tuple[Failure, KnownFailure]] = []
    parsed = ParsedLog()
    if missing:
        record["state"] = "unavailable"
        log_path.write_text("".join(f"prerequisite unavailable: {item}\n" for item in missing), encoding="utf-8")
        print(f"ornadb: {arguments.record}: prerequisites unavailable: {', '.join(missing)}", file=sys.stderr)
        exit_code = 1
    else:
        status, timed_out, executables, libraries = execute(argv, log_path, arguments.timeout_minutes)
        record["exit_status"] = status
        record["timed_out"] = timed_out
        if arguments.check:
            parsed, failures = check_outcome(arguments.check, argv, status, timed_out)
        else:
            with log_path.open(encoding="utf-8", errors="replace") as log:
                parsed = parse_log(log, executables, libraries)
            failures = observed_failures(parsed, status, timed_out)
        new, known = classify(failures, baseline, arguments.record)
        exit_code = 1 if new else 0
    record["finished_at"] = utc_now()
    record["elapsed_seconds"] = round(time.monotonic() - started, 1)
    record["log"].update(sha256=sha256_file(log_path), bytes=log_path.stat().st_size)
    record["targets"] = [target.as_json() for target in parsed.targets]
    outcomes = [outcome for target in parsed.targets for outcome in target.tests.values()]
    record["totals"] = {
        "targets": len(parsed.targets),
        "targets_incomplete": sum(not target.completed for target in parsed.targets),
        **{state: outcomes.count(state) for state in ("passed", "failed", "skipped", "ignored")},
    }
    record["failures"] = {
        "new": [failure.as_json() for failure in new],
        "known": [{**failure.as_json(), "bead": entry.bead} for failure, entry in known],
    }
    failure_rows = [failure.as_json() for failure in failures]
    for entry in baseline:
        status_in_record = baseline_status(entry, arguments.record, record["targets"], failure_rows)
        if status_in_record is not None:
            record["baseline"].append({**entry.as_json(), "status": status_in_record})
    executed = record["totals"]["passed"] + record["totals"]["failed"]
    if record["state"] != "unavailable":
        if failures:
            record["state"] = "failed"
        elif executed == 0:
            record["state"] = "not-executed"
            exit_code = 1
        else:
            record["state"] = "passed"
    write_json(record_path, record)
    print(
        f"ornadb: {arguments.record}: {record['state']}; {record['totals'].get('passed', 0)} passed, "
        f"{record['totals'].get('failed', 0)} failed, {record['totals'].get('skipped', 0)} skipped, "
        f"{len(known)} known and {len(new)} new failures; log sha256 {record['log']['sha256']}",
        file=sys.stderr,
    )
    for failure in new:
        print(f"ornadb: new failure: {failure.package} {failure.target} {failure.test}: {failure.reason}", file=sys.stderr)
    if record["state"] == "not-executed":
        print(f"ornadb: {arguments.record}: the command executed no test", file=sys.stderr)
    return exit_code


@dataclass
class Manifest:
    publication: dict
    expected_records: list[str]
    items: list[dict]


SELECTOR_KEYS = {"record", "package", "target", "test", "prefix"}
ITEM_KEYS = {"gate", "id", "title", "select", "counters", "limitations", "conditional"}


def load_items(path: Path) -> Manifest:
    """Load and validate the gate-item manifest."""
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    unexpected = set(data) - {"publication", "records", "item"}
    if unexpected:
        raise EvidenceError(f"{path.name}: unexpected top-level keys: {', '.join(sorted(unexpected))}")
    publication = data.get("publication") or {}
    for key in ("version", "revision", "archive_sha256"):
        if not publication.get(key):
            raise EvidenceError(f"{path.name}: publication.{key} is required")
    expected = list((data.get("records") or {}).get("expected", []))
    for record in expected:
        if not RECORD_PATTERN.fullmatch(record):
            raise EvidenceError(f"{path.name}: expected record {record!r} is not a record ID")
    items = list(data.get("item", []))
    seen: set[str] = set()
    for item in items:
        where = f"{path.name}: item {item.get('id', '?')}"
        extra = set(item) - ITEM_KEYS
        if extra:
            raise EvidenceError(f"{where}: unexpected keys {sorted(extra)}")
        if item.get("gate") not in GATES or not str(item.get("id", "")).startswith(item.get("gate", "?")):
            raise EvidenceError(f"{where}: gate must be A-H and prefix the item ID")
        if item["id"] in seen:
            raise EvidenceError(f"{where}: duplicate item ID")
        seen.add(item["id"])
        if not str(item.get("title", "")).strip():
            raise EvidenceError(f"{where}: title is required")
        selectors = item.get("select", [])
        if not selectors and not item.get("limitations"):
            raise EvidenceError(f"{where}: an item without tests must state its limitation")
        for selector in selectors:
            extra = set(selector) - SELECTOR_KEYS
            if extra or not selector:
                raise EvidenceError(f"{where}: selector keys must be from {sorted(SELECTOR_KEYS)}")
            if "test" in selector and "prefix" in selector:
                raise EvidenceError(f"{where}: a selector uses test or prefix, not both")
            if ("test" in selector or "prefix" in selector or "target" in selector) and "package" not in selector:
                raise EvidenceError(f"{where}: a target or test selector must name its package")
            if "target" in selector and not TARGET_PATTERN.fullmatch(selector["target"]):
                raise EvidenceError(f"{where}: target {selector['target']!r} is not lib, doc or KIND:NAME")
            pattern = selector.get("record", "*")
            if expected and not fnmatch.filter(expected, pattern):
                raise EvidenceError(f"{where}: record {pattern!r} matches no expected record")
        for pattern in item.get("counters", []):
            re.compile(pattern)
    return Manifest(publication, expected, items)


def test_selected(selector: dict, name: str) -> bool:
    if "test" in selector:
        return name == selector["test"] or name.endswith("::" + selector["test"])
    if "prefix" in selector:
        return name.startswith(selector["prefix"])
    return True


def worst(states: Iterable[str]) -> str:
    present = set(states)
    for state in STATE_PRECEDENCE:
        if state in present:
            return state
    return "not-executed"


def evaluate_selector(selector: dict, records: dict[str, dict], expected: list[str]) -> dict:
    """Resolve one selector against the records into a state and the tests it covered."""
    pattern = selector.get("record", "*")
    names = sorted(name for name in records if fnmatch.fnmatchcase(name, pattern))
    missing = sorted(name for name in fnmatch.filter(expected, pattern) if name not in records)
    outcomes: dict[str, list[str]] = {"passed": [], "failed": [], "skipped": [], "ignored": []}
    incomplete: list[str] = []
    used: list[str] = []
    test_names: set[str] = set()
    for name in names:
        for target in records[name]["targets"]:
            if "package" in selector and target["package"] != selector["package"]:
                continue
            if "target" in selector and target["target"] != selector["target"]:
                continue
            label = f"{target['package']} {target['target']}"
            selected = [(test, outcome) for test, outcome in target["tests"].items() if test_selected(selector, test)]
            if not target["completed"]:
                incomplete.append(label)
            if selected or not target["completed"]:
                used.append(name)
            for test, outcome in selected:
                outcomes[outcome].append(f"{label} {test}")
                test_names.add(test)
    matched = any(outcomes[state] for state in ("passed", "skipped", "ignored"))
    if outcomes["failed"] or incomplete:
        state, reason = "failed", "a selected test failed or its binary did not finish"
    elif not matched and any(records[name]["state"] == "unavailable" for name in names):
        state, reason = "unavailable", "a prerequisite of the producing run was unavailable"
    elif "test" in selector and len(test_names) > 1:
        state, reason = "not-executed", f"selector {selector['test']!r} matches {len(test_names)} tests; name the full libtest name"
    elif missing or not names:
        state, reason = "not-executed", f"no record from {', '.join(missing) or pattern}"
    elif outcomes["skipped"]:
        state, reason = "not-executed", "a selected test returned early and reported a skip"
    elif not outcomes["passed"]:
        state, reason = "not-executed", "no selected test ran"
    else:
        state, reason = "passed", ""
    return {
        "state": state,
        "reason": reason,
        "records": sorted(set(used)),
        "incomplete_targets": incomplete,
        "test_names": sorted(test_names),
        **outcomes,
    }


def focused_command(selector: dict, records: dict[str, dict], resolved: dict) -> str:
    """Return the narrowest command that reruns the tests a selector covered."""
    if "package" not in selector or selector.get("target", "").startswith("check:"):
        return "; ".join(records[name]["command"] for name in resolved["records"]) or "(no record)"
    words = ["cargo", "test", "--locked", "-p", selector["package"]]
    source = records.get(resolved["records"][0]) if resolved["records"] else None
    prefix = []
    if source is not None:
        argv = source["argv"]
        for index, word in enumerate(argv):
            if word in ("--features", "-F") and index + 1 < len(argv):
                words += [word, argv[index + 1]]
            elif word.startswith("--features=") or word == "--all-features":
                words.append(word)
        prefix = [f"{name}={value}" for name, value in source["inputs"]["env"].items() if value is not None and name.startswith("ORNA_")]
    if "target" in selector:
        words += target_flags(selector["target"])
    names = resolved["test_names"]
    if "test" in selector:
        if len(names) == 1:
            words += [names[0], "--", "--exact"]
        else:
            words.append(selector["test"])
    elif "prefix" in selector:
        words.append(selector["prefix"])
    return shlex.join(prefix + words)


def counter_values(line: str) -> dict[str, int]:
    """Read `key=integer` pairs from a measurement line.

    Segments split on `;`, `,` and `:`. Inside a segment, a key runs from the end
    of the previous value, so `large network bytes=10; rows=2 ms=3` yields
    `large_network_bytes`, `rows` and `ms`.
    """
    values: dict[str, int] = {}
    for segment in re.split(r"[;,:]", line):
        start = 0
        for match in COUNTER_PAIR.finditer(segment):
            key = segment[start : match.start()].strip()
            start = match.end()
            if key and re.fullmatch(r"-?\d+", match.group("value")):
                values[re.sub(r"[\s-]+", "_", key).lower()] = int(match.group("value"))
    return values


def counter_lines(patterns: list[str], records: list[tuple[dict, Path]]) -> list[dict]:
    """Collect the measurement lines an item's tests printed, with their integer values."""
    compiled = [re.compile(pattern) for pattern in patterns]
    found = []
    for record, directory in records:
        log = directory / record["log"]["file"]
        if not compiled or not log.exists():
            continue
        with log.open(encoding="utf-8", errors="replace") as handle:
            for raw in handle:
                line = ANSI.sub("", raw.rstrip("\r\n"))
                if any(pattern.search(line) for pattern in compiled):
                    found.append({"record": record["record"], "line": line, "values": counter_values(line)})
    return found


def evaluate_item(item: dict, records: dict[str, dict], directories: dict[str, Path], expected: list[str]) -> dict:
    selectors = item.get("select", [])
    resolved = [evaluate_selector(selector, records, expected) for selector in selectors]
    if resolved:
        state = worst(entry["state"] for entry in resolved)
        reasons = sorted({entry["reason"] for entry in resolved if entry["reason"] and entry["state"] == state})
    else:
        state, reasons = "not-executed", ["no executable test is mapped to this item"]
    used = sorted({name for entry in resolved for name in entry["records"]})
    return {
        "gate": item["gate"],
        "id": item["id"],
        "title": item["title"],
        "conditional": bool(item.get("conditional", False)),
        "state": state,
        "reason": "; ".join(reasons),
        "commands": [focused_command(selector, records, entry) for selector, entry in zip(selectors, resolved)],
        "executed_by": [
            {"record": name, "command": records[name]["command"], "log_sha256": records[name]["log"]["sha256"]}
            for name in used
        ],
        "tests": {
            "passed": sum(len(entry["passed"]) for entry in resolved),
            "failed": sorted({test for entry in resolved for test in entry["failed"]}),
            "skipped": sorted({test for entry in resolved for test in entry["skipped"]}),
            "ignored": sorted({test for entry in resolved for test in entry["ignored"]}),
            "incomplete_targets": sorted({target for entry in resolved for target in entry["incomplete_targets"]}),
        },
        "counters": counter_lines(item.get("counters", []), [(records[name], directories[name]) for name in used]),
        "limitations": list(item.get("limitations", [])),
    }


def gate_rollup(items: list[dict]) -> list[dict]:
    gates = []
    for gate in GATES:
        members = [item for item in items if item["gate"] == gate]
        if not members:
            gates.append(
                {
                    "gate": gate,
                    "conditional": False,
                    "state": "not-executed",
                    "reason": "no items mapped",
                    "items": {s: [] for s in STATES},
                    "items_with_limitations": [],
                }
            )
            continue
        state = worst(item["state"] for item in members)
        limited = [item["id"] for item in members if item["limitations"]]
        if state == "passed" and limited:
            state = "not-executed"
        gates.append(
            {
                "gate": gate,
                "conditional": all(item["conditional"] for item in members),
                "state": state,
                "items": {s: [item["id"] for item in members if item["state"] == s] for s in STATES},
                "items_with_limitations": limited,
            }
        )
    return gates


def load_records(root: Path) -> tuple[dict[str, dict], dict[str, Path], list[str]]:
    records: dict[str, dict] = {}
    directories: dict[str, Path] = {}
    problems: list[str] = []
    for path in sorted(root.rglob("*.record.json")):
        record = json.loads(path.read_text(encoding="utf-8"))
        name = record.get("record")
        if record.get("schema") != SCHEMA or not name:
            problems.append(f"{path.name}: not a {SCHEMA} record")
            continue
        if name in records:
            problems.append(f"{path.name}: duplicate record {name}")
            continue
        log = path.parent / record["log"]["file"]
        if not log.exists():
            problems.append(f"{name}: log {record['log']['file']} is missing")
        elif sha256_file(log) != record["log"]["sha256"]:
            problems.append(f"{name}: log {record['log']['file']} does not match its recorded SHA-256")
        records[name] = record
        directories[name] = path.parent
    return records, directories, problems


def markdown(summary: dict) -> str:
    lines = [
        f"## Orna {summary['publication']['version']} gate evidence",
        "",
        f"Commit `{summary['commit'] or 'unknown'}`; publication `{summary['publication']['revision']}` "
        f"(archive SHA-256 `{summary['publication']['archive_sha256'][:12]}…`). "
        "A gate is passed only when every item passed and no item declares a limitation.",
        "",
        "| Gate | State | Passed | Failed | Not executed | Unavailable |",
        "|---|---|---|---|---|---|",
    ]
    for gate in summary["gates"]:
        label = gate["gate"] + (" (conditional)" if gate["conditional"] else "")
        counts = [", ".join(gate["items"][state]) or "–" for state in STATES]
        lines.append(f"| {label} | {gate['state']} | " + " | ".join(counts) + " |")
    lines += ["", "| Record | State | Passed | Failed | Skipped | Known | New | Log SHA-256 |", "|---|---|---|---|---|---|---|---|"]
    for name, record in sorted(summary["records"].items()):
        totals = record["totals"]
        lines.append(
            f"| {name} | {record['state']} | {totals.get('passed', 0)} | {totals.get('failed', 0)} | "
            f"{totals.get('skipped', 0)} | {len(record['failures']['known'])} | {len(record['failures']['new'])} | "
            f"`{(record['log']['sha256'] or '')[:12]}` |"
        )
    if summary["new_failures"]:
        lines += ["", "### New failures (not in scripts/known-test-failures.toml)", ""]
        lines += [f"- `{f['package']} {f['target']} {f['test']}` in {f['record']}: {f['reason']}" for f in summary["new_failures"]]
    if summary["problems"]:
        lines += ["", "### Evidence problems", ""]
        lines += [f"- {problem}" for problem in summary["problems"]]
    lines += ["", "### Known failures", "", "| Entry | Bead | Status |", "|---|---|---|"]
    for entry in summary["baseline"]:
        status = entry["status"]
        lines.append(f"| `{entry['package']} {entry['target']} {entry['test']}` | {entry['bead']} | {status} |")
    return "\n".join(lines) + "\n"


def command_summarize(arguments: argparse.Namespace) -> int:
    manifest = load_items(Path(arguments.items))
    baseline = load_baseline(Path(arguments.baseline))
    records, directories, problems = load_records(Path(arguments.records))
    missing = [name for name in manifest.expected_records if name not in records]
    problems += [f"{name}: no record (the job did not finish)" for name in missing]
    problems += [f"{name}: prerequisites unavailable" for name, record in records.items() if record["state"] == "unavailable"]
    problems += [f"{name}: executed no test" for name, record in records.items() if record["state"] == "not-executed"]
    commits = sorted({record["commit"] for record in records.values() if record.get("commit")})
    if len(commits) > 1:
        problems.append(f"records test different commits: {', '.join(commits)}")
    if arguments.expect_commit and commits != [arguments.expect_commit]:
        problems.append(f"records must test {arguments.expect_commit}, found {', '.join(commits) or 'none'}")
    items = [evaluate_item(item, records, directories, manifest.expected_records) for item in manifest.items]
    new_failures = [{**failure, "record": name} for name, record in sorted(records.items()) for failure in record["failures"]["new"]]
    entries = []
    for entry in baseline:
        statuses = [
            row["status"]
            for record in records.values()
            for row in record["baseline"]
            if (row["package"], row["target"], row["test"], row["record"]) == (entry.package, entry.target, entry.test, entry.record)
        ]
        status = "observed" if "observed" in statuses else "passed" if "passed" in statuses else "not-executed"
        entries.append({**entry.as_json(), "status": status})
    if arguments.expect_commit:
        problems += [
            f"known failure {entry['test']} passed on {arguments.expect_commit}: remove its baseline entry"
            for entry in entries
            if entry["status"] == "passed"
        ]
    summary = {
        "schema": SCHEMA,
        "recorder": RECORDER_VERSION,
        "generated_at": utc_now(),
        "states": list(STATES),
        "commit": commits[0] if len(commits) == 1 else None,
        "publication": manifest.publication,
        "workflow_run": workflow_run(),
        "reviewer": None,
        "limitations": [
            "Each item records only the tests mapped to it; its own limitations list what those tests do not show.",
            "A passing subset never establishes whole-gate or whole-product completion.",
            "No reviewer is recorded by an automated run.",
        ],
        "gates": gate_rollup(items),
        "items": items,
        "new_failures": new_failures,
        "baseline": entries,
        "problems": problems,
        "records": {name: {key: value for key, value in record.items() if key != "targets"} for name, record in records.items()},
    }
    out = Path(arguments.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    write_json(out, summary)
    text = markdown(summary)
    if arguments.markdown:
        with Path(arguments.markdown).open("a", encoding="utf-8") as handle:
            handle.write(text)
    print(text)
    for problem in problems:
        print(f"ornadb: {problem}", file=sys.stderr)
    for failure in new_failures:
        print(f"ornadb: new failure in {failure['record']}: {failure['package']} {failure['target']} {failure['test']}", file=sys.stderr)
    return 1 if new_failures or problems else 0


def command_check(arguments: argparse.Namespace) -> int:
    baseline = load_baseline(Path(arguments.baseline))
    manifest = load_items(Path(arguments.items))
    print(f"known-failure baseline lists {len(baseline)} entries; manifest maps {len(manifest.items)} gate items")
    return 0


def cargo_metadata() -> dict:
    completed = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=WORKSPACE_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise EvidenceError(f"cargo metadata failed: {completed.stderr.strip()}")
    return json.loads(completed.stdout)


def select_test_targets(metadata: dict, packages: list[str], targets: list[str], integration: bool) -> list[tuple[str, str]]:
    """Return (package, target label) for the test targets of the named packages, in metadata order.

    `targets` keeps only the named labels, and every named package must have each of them.
    `integration` keeps only `test:` targets, for jobs whose library suite runs in another job.
    """
    members = {str(package["name"]): package for package in metadata.get("packages", [])}
    unknown = sorted(set(packages) - set(members))
    if unknown:
        raise EvidenceError(f"no workspace package named {', '.join(unknown)}")
    selected: list[tuple[str, str]] = []
    for name in dict.fromkeys(packages):
        labels = [
            target_label(list(target.get("kind") or []), str(target["name"]))
            for target in members[name].get("targets", [])
            if target.get("test")
        ]
        if integration:
            labels = [label for label in labels if label.startswith("test:")]
        for wanted in targets:
            if wanted not in labels:
                raise EvidenceError(f"{name} has no test target {wanted}")
        if targets:
            labels = [label for label in labels if label in targets]
        selected.extend((name, label) for label in labels)
    return selected


def enabled_features(package: dict, requested: str | None) -> set[str]:
    """Return the features of one `cargo metadata` package that `--features requested` turns on.

    These are the requested features, `default`, and every feature they enable in turn. Entries that
    enable a dependency or a dependency's feature (`dep:NAME`, `NAME/FEATURE`) are not followed.
    """
    declared = package.get("features") or {}
    pending = [name for name in re.split(r"[\s,]+", requested or "") if name]
    if "default" in declared:
        pending.append("default")
    enabled: set[str] = set()
    while pending:
        name = pending.pop()
        if name not in enabled:
            enabled.add(name)
            pending.extend(entry for entry in declared.get(name, []) if ":" not in entry and "/" not in entry)
    return enabled


def feature_gates(
    metadata: dict, selected: list[tuple[str, str]], requested: str | None, covered: dict[str, str]
) -> dict[tuple[str, str], str]:
    """Return why each selected target whose `required-features` this run leaves off is skipped.

    Cargo refuses to build such a target, so it must run in another job that enables the features.
    `covered` maps its target label to that job. A gated target without a job, or a job for a label
    that is not gated, is an error, so no target drops out of CI unnoticed.
    """
    members = {str(package["name"]): package for package in metadata.get("packages", [])}
    reasons: dict[tuple[str, str], str] = {}
    for package, label in selected:
        member = members[package]
        targets = {
            target_label(list(target.get("kind") or []), str(target["name"])): target
            for target in member.get("targets", [])
            if target.get("test")
        }
        enabled = enabled_features(member, requested)
        missing = [name for name in targets[label].get("required-features") or [] if name not in enabled]
        if not missing:
            continue
        if label not in covered:
            raise EvidenceError(
                f"{package} {label} requires features {', '.join(missing)}, which this run does not enable: "
                f"pass --features {','.join(missing)}, or name the job that runs it with them as --covered-by {label}=JOB"
            )
        reasons[(package, label)] = f"requires features {', '.join(missing)}, covered by {covered[label]}"
    stale = sorted(set(covered) - {label for _, label in reasons})
    if stale:
        raise EvidenceError(f"--covered-by {', '.join(stale)}: no selected target needs features this run leaves off")
    return reasons


def known_failures_for(baseline: list[KnownFailure], package: str, target: str) -> tuple[list[str], bool]:
    """Return the libtest names to skip in one target, and whether a whole-binary failure removes the target.

    Only entries that apply to every record count; record-limited entries belong to their own gate job.
    """
    names: set[str] = set()
    whole = False
    for entry in baseline:
        if entry.package == package and entry.target == target and entry.record == "*":
            if entry.test == "*":
                whole = True
            else:
                names.add(entry.test)
    return sorted(names), whole


def command_test(arguments: argparse.Namespace) -> int:
    for target in arguments.target:
        if not TARGET_PATTERN.fullmatch(target):
            raise EvidenceError(f"target {target!r} is not lib, doc or KIND:NAME")
    baseline = load_baseline(Path(arguments.baseline))
    overrides = dict(arguments.timeout_override)
    metadata = cargo_metadata()
    selected = select_test_targets(metadata, arguments.package, arguments.target, arguments.integration)
    gated = feature_gates(metadata, selected, arguments.features, dict(arguments.covered_by))
    failed: list[str] = []
    for package, target in selected:
        if (package, target) in gated:
            print(f"ornadb: skipped {package} {target}: {gated[(package, target)]}", file=sys.stderr)
            continue
        names, whole = known_failures_for(baseline, package, target)
        if whole:
            print(f"ornadb: skipped {package} {target}: a known failure aborts the whole binary", file=sys.stderr)
            continue
        skips = sorted(set(names) | set(arguments.skip_test))
        command = cargo_test_command(package, target, arguments.features, skips)
        print(f"ornadb: {shlex.join(command)}", flush=True)
        collected: list[str] = []
        timed_out = [False]
        process = subprocess.Popen(
            command,
            cwd=WORKSPACE_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
        )

        def kill_on_timeout() -> None:
            timed_out[0] = True
            process.kill()

        minutes = timeout_for(target, arguments.timeout_minutes, overrides)
        watchdog = threading.Timer(minutes * 60, kill_on_timeout) if minutes else None
        if watchdog:
            watchdog.start()
        assert process.stdout is not None
        for line in process.stdout:
            print(line, end="", flush=True)
            collected.append(line)
        status = process.wait()
        if watchdog:
            watchdog.cancel()
        if timed_out[0]:
            print(f"ornadb: {package} {target} was killed at --timeout-minutes", file=sys.stderr)
            failed.append(f"{package} {target}")
        elif status != 0:
            failed.append(f"{package} {target}")
        elif executed_tests("".join(collected)) == 0:
            print(f"ornadb: {package} {target} ran zero tests; a filter or ignore hides every test", file=sys.stderr)
            failed.append(f"{package} {target}")
    for item in failed:
        print(f"ornadb: test failed: {item}", file=sys.stderr)
    return 1 if failed else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", action="version", version=RECORDER_VERSION)
    commands = parser.add_subparsers(dest="action", required=True)

    run = commands.add_parser("run", help="run one command and write its evidence record")
    run.add_argument("--record", required=True, help="record ID, for example workspace-cli")
    run.add_argument("--title", default="", help="what the command is evidence for")
    run.add_argument("--out", required=True, help="directory for the record and its log")
    run.add_argument("--baseline", default=str(BASELINE), help="known-failure baseline (TOML)")
    run.add_argument("--timeout-minutes", type=float, default=None, help="kill the command after this long")
    run.add_argument("--input-env", action="append", default=[], help="environment variable to record as an input")
    run.add_argument("--tool-version", action="append", default=[], help="version command to record, for example 'fusermount3 -V'")
    run.add_argument("--require-command", action="append", default=[], help="executable that must be on PATH")
    run.add_argument("--require-path", action="append", default=[], help="path that must exist")
    run.add_argument(
        "--check",
        metavar="NAME",
        help="record a non-test command (for example cargo fmt) as one pass/fail check named NAME",
    )
    run.add_argument("command", nargs=argparse.REMAINDER, help="-- cargo test ...")

    summarize = commands.add_parser("summarize", help="merge records into one evidence summary")
    summarize.add_argument("--records", required=True, help="directory searched for *.record.json")
    summarize.add_argument("--out", required=True, help="summary JSON path")
    summarize.add_argument("--markdown", help="append a Markdown summary to this file")
    summarize.add_argument("--items", default=str(ITEMS), help="gate-item manifest (TOML)")
    summarize.add_argument("--baseline", default=str(BASELINE), help="known-failure baseline (TOML)")
    summarize.add_argument("--expect-commit", help="the commit every record must test; a known failure that passed is then a problem")

    check = commands.add_parser("check", help="validate the baseline and the gate-item manifest")
    check.add_argument("--items", default=str(ITEMS), help="gate-item manifest (TOML)")
    check.add_argument("--baseline", default=str(BASELINE), help="known-failure baseline (TOML)")

    tests = commands.add_parser("test", help="run each test target with the known failures of the baseline skipped")
    tests.add_argument("-p", "--package", action="append", required=True, help="Cargo package to test (repeatable)")
    tests.add_argument("--target", action="append", default=[], help="only this target: lib, test:NAME or bin:NAME (repeatable)")
    tests.add_argument("--integration", action="store_true", help="only integration test targets (test:NAME)")
    tests.add_argument("--features", help="Cargo features to enable")
    tests.add_argument("--timeout-minutes", type=float, default=None, help="kill each test binary after this long")
    tests.add_argument("--skip-test", action="append", default=[], help="libtest name to skip in every selected target (repeatable)")
    tests.add_argument(
        "--timeout-override",
        action="append",
        default=[],
        type=parse_timeout_override,
        metavar="TARGET=MINUTES",
        help="watchdog minutes for one target label, for example test:media_import_query=150 (repeatable)",
    )
    tests.add_argument(
        "--covered-by",
        action="append",
        default=[],
        type=parse_covered_by,
        metavar="TARGET=JOB",
        help="TARGET needs features this run does not enable and job JOB runs it with them; skip it here (repeatable)",
    )
    tests.add_argument("--baseline", default=str(BASELINE), help="known-failure baseline (TOML)")

    arguments = parser.parse_args(argv)
    try:
        if arguments.action == "run":
            if arguments.command and arguments.command[0] == "--":
                arguments.command = arguments.command[1:]
            return command_run(arguments)
        if arguments.action == "summarize":
            return command_summarize(arguments)
        if arguments.action == "test":
            return command_test(arguments)
        return command_check(arguments)
    except (EvidenceError, OSError, tomllib.TOMLDecodeError, json.JSONDecodeError, re.error) as error:
        print(f"ornadb: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
