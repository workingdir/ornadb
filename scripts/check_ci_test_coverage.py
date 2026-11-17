#!/usr/bin/env python3
"""Check that every workspace test target is run by some CI workflow step.

Targets come from `cargo metadata`: every target with `test = true` (lib, bin and
integration test targets). Each `cargo test` and `scripts/gate_evidence.py test|run`
command in .github/workflows/*.yml is parsed after matrix values are expanded. Shell
commands are split on `&&`, `||`, `;`, `|` and newlines, and gate-c-scale.yml's
`run_record <id> <args>` helper is read as `cargo test --locked <args>`.

A target is covered when some invocation selects it with no test-name filter and no
`--ignored`. Feature-gated targets count only when the invocation enables their
features. The known-red gate scripts count as whole-package coverage (PACKAGE_SCRIPTS).

Test names are checked separately. A `--skip NAME` hides every test whose name contains
NAME (exactly NAME under `--exact`), so each skipped name must be run by another invocation of
the same target. A `#[ignore]` test runs only under `--ignored` or `--include-ignored`, so each
ignored test (names read from the target's root source file) must be run by some invocation.
Test names come from the command line and source files, not from running the binaries.

Intentional gaps are listed in scripts/ci-test-coverage-allow.toml, each with a reason.
An entry whose target or test is now covered fails, so the list can only shrink.

Commands the parser does not model fail the check: an unknown cargo or gate_evidence
option, a shell command that cannot be tokenised, or a script that runs `cargo test`
without a PACKAGE_SCRIPTS entry. Package or target names built from `$VAR` or
`${{ ... }}` at run time cannot be resolved; they are listed as dynamic and not counted.
"""

from __future__ import annotations

import argparse
import dataclasses
import itertools
import json
import re
import shlex
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parent.parent
WORKFLOWS = REPO / ".github" / "workflows"
SCRIPTS = REPO / "scripts"
ALLOWLIST = SCRIPTS / "ci-test-coverage-allow.toml"

# Shell scripts that run every test target of one package. Each must still contain
# its `PKG=<package>` line, which is checked when a workflow invokes it.
PACKAGE_SCRIPTS = {
    "check-conformance-known-red.sh": "orna-conformance",
    "check-semantic-known-red.sh": "orna-check",
}

LIB_KINDS = frozenset({"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"})
PR_TRIGGERS = frozenset({"pull_request", "push"})
MATRIX_REFERENCE = re.compile(r"\$\{\{\s*matrix\.([A-Za-z_][A-Za-z0-9_-]*)\s*\}\}")
GITHUB_EXPRESSION = re.compile(r"\$\{\{.*?\}\}")
RELEVANT_COMMAND = re.compile(r"cargo\s+test|gate_evidence\.py\s+test|run_record")
SEGMENT_SEPARATOR = re.compile(r"&&|\|\||[;|\n]")

# Options the parser models. Anything else is an error, so a new option cannot silently
# change what counts as covered.
CARGO_VALUE_OPTIONS = frozenset({
    "-p", "--package", "--exclude", "--bin", "--test", "--example", "--bench",
    "--features", "-F", "--target", "--profile", "--manifest-path", "--target-dir",
    "-j", "--jobs", "--color", "--message-format", "--config", "-Z",
})
CARGO_SELECTION_VALUE_OPTIONS = frozenset({
    "-p", "--package", "--exclude", "--bin", "--test", "--features", "-F",
})
CARGO_FLAG_OPTIONS = frozenset({
    "--locked", "--frozen", "--offline", "--release", "--no-fail-fast", "--workspace",
    "--all", "--all-features", "--no-default-features", "--lib", "--bins", "--tests",
    "--all-targets", "--examples", "--benches", "--doc", "--no-run", "--keep-going",
    "-q", "--quiet", "-v", "--verbose", "--ignore-rust-version",
})
LIBTEST_VALUE_OPTIONS = frozenset({"--skip", "--test-threads", "--format", "--color", "-Z", "--logfile"})
LIBTEST_FLAG_OPTIONS = frozenset({
    "--exact", "--list", "--ignored", "--include-ignored", "--nocapture", "--show-output",
    "--quiet", "-q", "--force-run-in-place",
})
GATE_VALUE_OPTIONS = frozenset({
    "-p", "--package", "--target", "--exclude-target", "--features", "--timeout-minutes",
    "--skip-test", "--timeout-override", "--covered-by", "--baseline",
})
GATE_FLAG_OPTIONS = frozenset({"--integration"})


class CoverageError(Exception):
    """A workflow command, config, or script the checker cannot account for."""


@dataclass(frozen=True)
class Target:
    package: str
    label: str  # "lib", "bin:NAME" or "test:NAME"
    required_features: frozenset[str] = frozenset()
    ignored_tests: frozenset[str] = frozenset()  # names of the #[ignore]d tests in the root source file


@dataclass(frozen=True)
class Package:
    name: str
    features: dict[str, list[str]]
    targets: tuple[Target, ...]

    @property
    def labels(self) -> frozenset[str]:
        return frozenset(target.label for target in self.targets)


@dataclass(frozen=True)
class Workspace:
    packages: dict[str, Package]
    default_members: frozenset[str]


@dataclass(frozen=True)
class Invocation:
    """One `cargo test` or `gate_evidence.py test` run, resolved as far as statically possible."""

    where: str
    pr_triggered: bool
    packages: frozenset[str] = frozenset()
    selectors: frozenset[str] = frozenset()  # "all", "lib", "bin:*", "bin:NAME", "test:*", "test:NAME"
    excluded: frozenset[str] = frozenset()  # target labels this run does not execute
    features: frozenset[str] = frozenset()
    all_features: bool = False
    filters: tuple[str, ...] = ()
    exact: bool = False
    skips: tuple[str, ...] = ()
    ignored: bool = False  # --ignored: runs only #[ignore]d tests
    include_ignored: bool = False  # --include-ignored: runs them too
    dynamic_filter: bool = False
    runs: bool = True
    dynamic: bool = False  # package or target names are not static; not counted

    @property
    def full(self) -> bool:
        """True when this run executes every test of each target it selects (skips aside)."""
        return not (self.filters or self.dynamic_filter or (self.ignored and not self.include_ignored))


@dataclass
class TargetResult:
    target: Target
    covering: list[Invocation] = field(default_factory=list)
    partial: list[Invocation] = field(default_factory=list)


@dataclass(frozen=True)
class Allowlist:
    targets: dict[tuple[str, str], str]
    tests: dict[tuple[str, str, str], str]


IGNORED_TEST = re.compile(
    r"#\[ignore(?:\s*=\s*\"[^\"]*\")?\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"
)


def ignored_test_names(path: Path) -> frozenset[str]:
    """Names of the `#[ignore]`d test functions in one source file."""
    return frozenset(IGNORED_TEST.findall(path.read_text(encoding="utf-8")))


def load_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode:
        raise CoverageError(f"cargo metadata failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def load_workspace(metadata: dict) -> Workspace:
    member_ids = set(metadata["workspace_members"])
    packages: dict[str, Package] = {}
    for package in metadata["packages"]:
        if package["id"] not in member_ids:
            continue
        targets = []
        for target in package["targets"]:
            if not target.get("test"):
                continue
            kinds = set(target["kind"])
            if kinds & LIB_KINDS:
                label = "lib"
            elif "bin" in kinds:
                label = f"bin:{target['name']}"
            elif "test" in kinds:
                label = f"test:{target['name']}"
            else:
                raise CoverageError(f"{package['name']}: unsupported test target kind {sorted(kinds)}")
            ignored = ignored_test_names(Path(target["src_path"]))
            targets.append(Target(package["name"], label, frozenset(target.get("required-features") or []), ignored))
        packages[package["name"]] = Package(package["name"], package.get("features", {}), tuple(targets))
    default_ids = set(metadata["workspace_default_members"])
    default_members = frozenset(p["name"] for p in metadata["packages"] if p["id"] in default_ids)
    return Workspace(packages, default_members)


def split_double_dash(args: list[str]) -> tuple[list[str], list[str]]:
    if "--" in args:
        index = args.index("--")
        return args[:index], args[index + 1 :]
    return args, []


def option_pairs(where: str, args: list[str], value_options, flag_options):
    """Yield (name, value) per option; a positional token yields (None, token)."""
    index = 0
    while index < len(args):
        token = args[index]
        if token.startswith("--") and "=" in token:
            name, _, value = token.partition("=")
            if name in value_options:
                yield name, value
                index += 1
                continue
        if token in value_options:
            if index + 1 >= len(args):
                raise CoverageError(f"{where}: {token} has no value")
            yield token, args[index + 1]
            index += 2
            continue
        if token in flag_options:
            yield token, None
            index += 1
            continue
        if token.startswith("-"):
            raise CoverageError(f"{where}: unsupported option {token!r}")
        yield None, token
        index += 1


def split_features(value: str) -> list[str]:
    return [name for name in re.split(r"[,\s]+", value) if name]


def resolve_packages(where: str, ws: Workspace, named: list[str], workspace: bool, excluded: list[str]) -> frozenset[str]:
    for name in [*named, *excluded]:
        if name not in ws.packages:
            raise CoverageError(f"{where}: {name!r} is not a workspace package with test targets")
    if workspace:
        selected = set(ws.packages)
    elif named:
        selected = set(named)
    else:
        selected = set(ws.default_members)
    return frozenset(selected - set(excluded))


def parse_cargo_test(where: str, args: list[str], pr_triggered: bool, ws: Workspace) -> Invocation:
    cargo_args, libtest_args = split_double_dash(args)
    named: list[str] = []
    excluded_packages: list[str] = []
    features: set[str] = set()
    selectors: set[str] = set()
    selection_flags = False
    workspace = all_features = no_run = dynamic = dynamic_filter = False
    filters: list[str] = []
    for name, value in option_pairs(where, cargo_args, CARGO_VALUE_OPTIONS, CARGO_FLAG_OPTIONS):
        if name is None:
            if "$" in value:
                dynamic_filter = True
            else:
                filters.append(value)
            continue
        if value is not None and name in CARGO_SELECTION_VALUE_OPTIONS and "$" in value:
            dynamic = True
            continue
        if name in ("-p", "--package"):
            named.extend(value.split(","))
        elif name == "--exclude":
            excluded_packages.extend(value.split(","))
        elif name in ("--features", "-F"):
            features.update(split_features(value))
        elif name == "--workspace" or name == "--all":
            workspace = True
        elif name == "--all-features":
            all_features = True
        elif name == "--no-run":
            no_run = True
        elif name == "--lib":
            selectors.add("lib")
            selection_flags = True
        elif name == "--bins":
            selectors.add("bin:*")
            selection_flags = True
        elif name == "--bin":
            selectors.add(f"bin:{value}")
            selection_flags = True
        elif name == "--tests":
            selectors.add("test:*")
            selection_flags = True
        elif name == "--test":
            selectors.add(f"test:{value}")
            selection_flags = True
        elif name == "--all-targets":
            selectors.add("all")
            selection_flags = True
        elif name in ("--example", "--examples", "--benches", "--bench", "--doc"):
            # These select no test target, so they stop the default all-test-targets selection.
            selection_flags = True
    exact = ignored = include_ignored = listing = False
    skips: list[str] = []
    for name, value in option_pairs(where, libtest_args, LIBTEST_VALUE_OPTIONS, LIBTEST_FLAG_OPTIONS):
        if name is None:
            if "$" in value:
                dynamic_filter = True
            else:
                filters.append(value)
        elif name == "--exact":
            exact = True
        elif name == "--list":
            listing = True
        elif name == "--ignored":
            ignored = True
        elif name == "--include-ignored":
            include_ignored = True
        elif name == "--skip":
            if "$" in value:
                dynamic_filter = True
            else:
                skips.append(value)
    if dynamic:
        return Invocation(where=where, pr_triggered=pr_triggered, dynamic=True, runs=False)
    packages = resolve_packages(where, ws, named, workspace, excluded_packages)
    if not selection_flags:
        selectors.add("all")
    return Invocation(
        where=where,
        pr_triggered=pr_triggered,
        packages=packages,
        selectors=frozenset(selectors),
        features=frozenset(features),
        all_features=all_features,
        filters=tuple(filters),
        exact=exact,
        skips=tuple(skips),
        ignored=ignored,
        include_ignored=include_ignored,
        dynamic_filter=dynamic_filter,
        runs=not (no_run or listing),
    )


def parse_gate_test(where: str, args: list[str], pr_triggered: bool, ws: Workspace) -> Invocation:
    named: list[str] = []
    targets: list[str] = []
    excluded: set[str] = set()
    features: set[str] = set()
    skips: list[str] = []
    integration = dynamic = False
    for name, value in option_pairs(where, args, GATE_VALUE_OPTIONS, GATE_FLAG_OPTIONS):
        if name is None:
            raise CoverageError(f"{where}: unexpected gate_evidence argument {value!r}")
        if value is not None and name in ("-p", "--package", "--target", "--exclude-target", "--features", "--skip-test") and "$" in value:
            dynamic = True
            continue
        if name in ("-p", "--package"):
            named.extend(value.split(","))
        elif name == "--target":
            targets.append(value)
        elif name == "--exclude-target":
            excluded.add(value)
        elif name == "--covered-by":
            label, separator, _job = value.partition("=")
            if not separator:
                raise CoverageError(f"{where}: --covered-by expects TARGET=JOB, got {value!r}")
            excluded.add(label)
        elif name == "--features":
            features.update(split_features(value))
        elif name == "--skip-test":
            skips.append(value)
        elif name == "--integration":
            integration = True
        # --timeout-minutes, --timeout-override and --baseline do not change coverage.
    if dynamic:
        return Invocation(where=where, pr_triggered=pr_triggered, dynamic=True, runs=False)
    if not named:
        raise CoverageError(f"{where}: gate_evidence test needs -p")
    packages = resolve_packages(where, ws, named, False, [])
    for package in packages:
        labels = ws.packages[package].labels
        for label in [*targets, *excluded]:
            if label not in labels:
                raise CoverageError(f"{where}: {package} has no test target {label!r}")
    selectors = frozenset(targets) if targets else frozenset({"test:*"} if integration else {"all"})
    return Invocation(
        where=where,
        pr_triggered=pr_triggered,
        packages=packages,
        selectors=selectors,
        excluded=frozenset(excluded),
        features=frozenset(features),
        skips=tuple(skips),
        runs=True,
    )


def normalize_token(token: str) -> str:
    """Strip `$(` subshell wrapping so `listing=$(cargo test ...)` tokenises as `cargo test ...`."""
    if "$(" in token:
        token = token.split("$(")[-1]
    return token.lstrip("(`").rstrip(")`")


def segment_invocations(where: str, segment: str, pr_triggered: bool, ws: Workspace) -> list[Invocation]:
    if not RELEVANT_COMMAND.search(segment):
        return []
    try:
        tokens = [normalize_token(token) for token in shlex.split(segment, comments=True)]
    except ValueError as error:
        raise CoverageError(f"{where}: cannot tokenise shell command {segment.strip()!r}: {error}") from error
    if not tokens:
        return []
    if tokens[0] == "run_record":
        if len(tokens) < 2:
            raise CoverageError(f"{where}: run_record needs a record id")
        tokens = ["cargo", "test", "--locked", *tokens[2:]]
    found = []
    for index in range(len(tokens) - 1):
        if tokens[index] == "cargo" and tokens[index + 1] == "test":
            found.append(parse_cargo_test(where, tokens[index + 2 :], pr_triggered, ws))
        if tokens[index].endswith("gate_evidence.py") and tokens[index + 1] == "test":
            found.append(parse_gate_test(where, tokens[index + 2 :], pr_triggered, ws))
    return found


def run_invocations(where: str, text: str, pr_triggered: bool, ws: Workspace) -> list[Invocation]:
    found: list[Invocation] = []
    for script, package in PACKAGE_SCRIPTS.items():
        if f"scripts/{script}" in text:
            found.append(script_invocation(where, script, package, pr_triggered))
    for segment in SEGMENT_SEPARATOR.split(text.replace("\\\n", " ")):
        found.extend(segment_invocations(where, segment, pr_triggered, ws))
    return found


def script_invocation(where: str, script: str, package: str, pr_triggered: bool) -> Invocation:
    path = SCRIPTS / script
    if not path.is_file() or f"PKG={package}" not in path.read_text(encoding="utf-8"):
        raise CoverageError(f"{where}: scripts/{script} no longer runs {package} tests")
    return Invocation(
        where=f"{where} (scripts/{script})",
        pr_triggered=pr_triggered,
        packages=frozenset({package}),
        selectors=frozenset({"all"}),
    )


def matrix_combinations(path: Path, job_id: str, job: dict) -> list[dict]:
    matrix = (job.get("strategy") or {}).get("matrix")
    if not isinstance(matrix, dict):
        return [{}]
    if "exclude" in matrix:
        raise CoverageError(f"{path.name}:{job_id}: matrix exclude is not modelled")
    dimensions = {key: value for key, value in matrix.items() if key != "include"}
    for key, values in dimensions.items():
        if not isinstance(values, list):
            raise CoverageError(f"{path.name}:{job_id}: matrix.{key} is not a literal list")
    keys = list(dimensions)
    combinations = [
        dict(zip(keys, values))
        for values in itertools.product(*(dimensions[key] for key in keys))
    ] if keys else []
    combinations.extend(dict(item) for item in matrix.get("include", []))
    return combinations or [{}]


def substitute_matrix(where: str, text: str, combination: dict) -> str:
    def replace(match: re.Match[str]) -> str:
        key = match.group(1)
        if key not in combination:
            raise CoverageError(f"{where}: matrix.{key} is not a value of this job")
        return str(combination[key])

    # Expressions other than matrix values are only known at run time. `$DYNAMIC` keeps the
    # shell splittable and makes any package or target built from it count as dynamic.
    return GITHUB_EXPRESSION.sub("$DYNAMIC", MATRIX_REFERENCE.sub(replace, text))


def workflow_invocations(path: Path, ws: Workspace) -> list[Invocation]:
    document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    # PyYAML reads the bare key `on` as the boolean True.
    triggers = document.get("on", document.get(True, {}))
    if isinstance(triggers, str):
        trigger_names = {triggers}
    elif isinstance(triggers, dict):
        trigger_names = set(triggers)
    else:
        trigger_names = set(triggers)
    pr_triggered = bool(trigger_names & PR_TRIGGERS)
    found: list[Invocation] = []
    seen: set[Invocation] = set()
    for job_id, job in (document.get("jobs") or {}).items():
        for combination in matrix_combinations(path, job_id, job):
            for index, step in enumerate(job.get("steps") or []):
                run = step.get("run")
                if not run:
                    continue
                where = f"{path.name}:{job_id}:{step.get('name', f'step {index + 1}')}"
                text = substitute_matrix(where, run, combination)
                for invocation in run_invocations(where, text, pr_triggered, ws):
                    # The same command repeated across matrix combinations is one invocation.
                    key = invocation if invocation.dynamic else dataclasses.replace(invocation, where="")
                    if key not in seen:
                        seen.add(key)
                        found.append(invocation)
    return found


def all_invocations(ws: Workspace) -> list[Invocation]:
    found: list[Invocation] = []
    for path in sorted(WORKFLOWS.glob("*.yml")):
        found.extend(workflow_invocations(path, ws))
    return found


def check_script_mapping() -> None:
    for path in sorted(SCRIPTS.glob("*.sh")):
        if "cargo test" in path.read_text(encoding="utf-8") and path.name not in PACKAGE_SCRIPTS:
            raise CoverageError(f"scripts/{path.name} runs cargo test but is not in PACKAGE_SCRIPTS")


def enabled_features(package: Package, requested: frozenset[str]) -> set[str]:
    pending = [name.rsplit("/", 1)[-1] for name in requested]
    enabled: set[str] = set()
    while pending:
        name = pending.pop()
        if name in enabled:
            continue
        enabled.add(name)
        for member in package.features.get(name, []):
            if "/" not in member and not member.startswith("dep:"):
                pending.append(member)
    return enabled


def label_selected(selectors: frozenset[str], label: str) -> bool:
    kind = label.partition(":")[0]
    return (
        "all" in selectors
        or label in selectors
        or (kind == "bin" and "bin:*" in selectors)
        or (kind == "test" and "test:*" in selectors)
    )


def selects(ws: Workspace, invocation: Invocation, target: Target) -> bool:
    if not invocation.runs or invocation.dynamic:
        return False
    if target.package not in invocation.packages or target.label in invocation.excluded:
        return False
    if not label_selected(invocation.selectors, target.label):
        return False
    if target.required_features and not invocation.all_features:
        package = ws.packages[target.package]
        if not target.required_features <= enabled_features(package, invocation.features):
            return False
    return True


def _libtest_matches(pattern: str, name: str, exact: bool) -> bool:
    return name == pattern if exact else pattern in name


def runs_test(invocation: Invocation, name: str, is_ignored: bool) -> bool:
    """Whether this invocation runs the test called `name`, under libtest's ignore, filter and skip rules."""
    if invocation.dynamic_filter:
        return False
    if not (invocation.include_ignored or invocation.ignored == is_ignored):
        return False
    if any(_libtest_matches(skip, name, invocation.exact) for skip in invocation.skips):
        return False
    if not invocation.filters:
        return True
    return any(_libtest_matches(text, name, invocation.exact) for text in invocation.filters)


def evaluate(ws: Workspace, invocations: list[Invocation]) -> tuple[list[TargetResult], dict[tuple[str, str, str], str]]:
    """Target results, and the tests no step runs: skipped names and `#[ignore]`d tests, keyed by (package, label, name)."""
    results: list[TargetResult] = []
    unrun: dict[tuple[str, str, str], str] = {}
    for package in ws.packages.values():
        for target in package.targets:
            selecting = [inv for inv in invocations if selects(ws, inv, target)]
            results.append(
                TargetResult(
                    target,
                    covering=[inv for inv in selecting if inv.full],
                    partial=[inv for inv in selecting if not inv.full],
                )
            )
            for invocation in selecting:
                for skip in invocation.skips:
                    if not any(
                        runs_test(other, skip, skip in target.ignored_tests)
                        for other in selecting
                        if other is not invocation
                    ):
                        unrun.setdefault((package.name, target.label, skip), f"skipped in {invocation.where}")
            for test in sorted(target.ignored_tests):
                if not any(runs_test(inv, test, True) for inv in selecting):
                    unrun.setdefault((package.name, target.label, test), "#[ignore]d")
    return results, unrun


def load_allowlist(ws: Workspace) -> Allowlist:
    if not ALLOWLIST.exists():
        return Allowlist({}, {})
    data = tomllib.loads(ALLOWLIST.read_text(encoding="utf-8"))
    unknown = set(data) - {"uncovered", "uncovered_test"}
    if unknown:
        raise CoverageError(f"{ALLOWLIST.name}: unknown tables {sorted(unknown)}")
    targets: dict[tuple[str, str], str] = {}
    for entry in data.get("uncovered", []):
        key = allow_key(entry, ws, ("package", "target"))
        if key[:2] in targets:
            raise CoverageError(f"{ALLOWLIST.name}: duplicate uncovered entry {key[:2]}")
        targets[key[:2]] = entry["reason"]
    tests: dict[tuple[str, str, str], str] = {}
    for entry in data.get("uncovered_test", []):
        key = allow_key(entry, ws, ("package", "target", "test"))
        if key in tests:
            raise CoverageError(f"{ALLOWLIST.name}: duplicate uncovered_test entry {key}")
        tests[key] = entry["reason"]
    return Allowlist(targets, tests)


def allow_key(entry: dict, ws: Workspace, fields: tuple[str, ...]) -> tuple[str, ...]:
    values = tuple(entry.get(name) for name in fields)
    if not all(isinstance(value, str) and value for value in values):
        raise CoverageError(f"{ALLOWLIST.name}: an entry needs string {', '.join(fields)}: {entry}")
    package, label = values[0], values[1]
    if package not in ws.packages or label not in ws.packages[package].labels:
        raise CoverageError(f"{ALLOWLIST.name}: {package} {label} is not a test target")
    reason = entry.get("reason")
    if not isinstance(reason, str) or not reason.strip():
        raise CoverageError(f"{ALLOWLIST.name}: {package} {label} needs a reason")
    return values


def report(
    ws: Workspace,
    invocations: list[Invocation],
    results: list[TargetResult],
    unrun: dict[tuple[str, str, str], str],
    allow: Allowlist,
    listing: bool,
) -> int:
    failures: list[str] = []
    stale: list[str] = []
    allowed: list[str] = []
    covered = [r for r in results if r.covering]
    nightly_only = [r for r in covered if not any(inv.pr_triggered for inv in r.covering)]
    dynamic = [inv.where for inv in invocations if inv.dynamic]
    for result in results:
        key = (result.target.package, result.target.label)
        name = f"{result.target.package} {result.target.label}"
        if result.covering:
            if key in allow.targets:
                stale.append(f"{name}: covered now, remove its uncovered entry")
        elif key in allow.targets:
            allowed.append(f"{name}: {allow.targets[key]}")
        else:
            partial = ", ".join(sorted({inv.where for inv in result.partial}))
            note = f" (filtered runs only: {partial})" if partial else ""
            failures.append(f"uncovered: {name}{note}")
    used: set[tuple[str, str, str]] = set()
    for key, why in sorted(unrun.items()):
        package, label, test = key
        if key in allow.tests:
            used.add(key)
            allowed.append(f"{package} {label} test {test!r}: {allow.tests[key]}")
        else:
            failures.append(f"test not run by any workflow step: {package} {label} {test!r} ({why})")
    for key in sorted(set(allow.tests) - used):
        stale.append(f"{key[0]} {key[1]} test {key[2]!r}: now run by a step, remove its entry")

    total = sum(len(package.targets) for package in ws.packages.values())
    print(
        f"ci test coverage: {total} test targets in {len(ws.packages)} packages; "
        f"{len(covered)} covered ({len(covered) - len(nightly_only)} by a push or pull_request workflow, "
        f"{len(nightly_only)} only by other workflows)"
    )
    if listing:
        for result in results:
            steps = sorted({inv.where for inv in result.covering}) or ["-"]
            status = "covered" if result.covering else ("filtered" if result.partial else "uncovered")
            print(f"  {result.target.package} {result.target.label} [{status}] {'; '.join(steps)}")
    print(f"allowlisted: {len(allowed)}")
    for line in allowed:
        print(f"  allow: {line}")
    if dynamic:
        print(f"dynamic invocations, not counted: {len(dynamic)}")
        for where in sorted(set(dynamic)):
            print(f"  dynamic: {where}")
    for line in failures:
        print(f"error: {line}", file=sys.stderr)
    for line in stale:
        print(f"error: stale allowlist entry: {line}", file=sys.stderr)
    return 1 if failures or stale else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check that every workspace test target runs in some CI workflow step.")
    parser.add_argument("--list", action="store_true", help="print every test target with its covering steps")
    arguments = parser.parse_args(argv)
    try:
        ws = load_workspace(load_metadata())
        check_script_mapping()
        invocations = all_invocations(ws)
        allow = load_allowlist(ws)
        results, unrun = evaluate(ws, invocations)
    except CoverageError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return report(ws, invocations, results, unrun, allow, arguments.list)


if __name__ == "__main__":
    sys.exit(main())
