#!/usr/bin/env python3
"""Run the dependency-light validation gate for checked-in editor tooling."""

from __future__ import annotations

import filecmp
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None
from typing import Sequence



LOG_PREFIX = "[editor]"
GRAMMAR_INPUTS = ("grammar.js", "tree-sitter.json")
GENERATED_ARTEFACTS = (
    "src/parser.c",
    "src/grammar.json",
    "src/node-types.json",
    "src/tree_sitter/alloc.h",
    "src/tree_sitter/array.h",
    "src/tree_sitter/parser.h",
)
ACCEPTED_CORPUS_MANIFEST_NAME = "test/accepted-corpus.txt"
DEFERRED_CORPUS_MANIFEST_NAME = "test/deferred-corpus.txt"
CORPUS_CASE_DELIMITER = "=" * 20
ORDER_BY_HIGHLIGHT_FIXTURE_NAME = "accepted_resources_streams.orna"
ORDER_BY_DIRECTION_TEXTS = ("ASC", "DESC")
# Keep the Zed grammar source reproducible: this is the accepted, reviewed tree-sitter revision.
ACCEPTED_ZED_GRAMMAR_REVISION = "f5c9007ee2ba8dcd00784e806a9d9b32be6efe08"
TARGET_HIGHLIGHT_EXPECTATIONS = (
    ("accepted_resources_streams.orna", {"function": ("overdue", "execute_sql"), "property": ("payload",)}),
    ("accepted_actions_inspector.orna", {"function": ("echo",), "property": ("invoke",)}),
)
TEXTMATE_PARITY_COMPARABLE_KEYS = ("scopeName", "patterns", "repository")
# Only editor metadata may differ; every other root and nested grammar rule is compared.
TEXTMATE_PARITY_ALLOWED_ROOT_KEYS = frozenset({"$schema", "name"})



def log(message: str, *, error: bool = False) -> None:
    print(f"{LOG_PREFIX} {message}", file=sys.stderr if error else sys.stdout, flush=True)


def display_path(path: Path, repository: Path) -> str:
    """Render paths relative to the repository, including its sibling spec."""
    try:
        return path.relative_to(repository).as_posix()
    except ValueError:
        return Path(os.path.relpath(path, repository)).as_posix()


def emit_output(label: str, output: str) -> None:
    for line in output.splitlines():
        log(f"{label}: {line}", error=True)


def run_command(
    command: Sequence[str],
    *,
    cwd: Path,
    label: str,
) -> subprocess.CompletedProcess[str] | None:
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        log(f"{label} could not start: {exc}", error=True)
        return None

    emit_output(label, completed.stdout)
    emit_output(label, completed.stderr)
    return completed


def sorted_orna_files(directory: Path) -> list[Path]:
    return sorted(
        (path for path in directory.rglob("*.orna") if path.is_file()),
        key=lambda path: path.as_posix(),
    )


def read_corpus_case_names(corpus_directory: Path, repository: Path) -> list[str] | None:
    """Read exact tree-sitter corpus case names, failing closed on malformed headers."""
    case_names: list[str] = []
    seen_case_names: set[str] = set()
    for corpus_path in sorted(corpus_directory.rglob("*.txt"), key=lambda path: path.as_posix()):
        if not corpus_path.is_file():
            continue
        try:
            lines = corpus_path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeDecodeError) as exc:
            log(
                f"could not read corpus {display_path(corpus_path, repository)}: {exc}",
                error=True,
            )
            return None
        index = 0
        while index < len(lines):
            if lines[index] != CORPUS_CASE_DELIMITER:
                index += 1
                continue
            if (
                index + 2 >= len(lines)
                or not lines[index + 1]
                or lines[index + 2] != CORPUS_CASE_DELIMITER
            ):
                log(
                    f"malformed corpus case header in {display_path(corpus_path, repository)} "
                    f"near line {index + 1}",
                    error=True,
                )
                return None
            case_name = lines[index + 1]
            if case_name in seen_case_names:
                log(f"duplicate corpus case name: {case_name!r}", error=True)
                return None
            seen_case_names.add(case_name)
            case_names.append(case_name)
            index += 3

    if not case_names:
        log("corpus check found no corpus cases", error=True)
        return None
    return case_names


def read_corpus_manifest(
    manifest_path: Path,
    repository: Path,
    *,
    label: str,
) -> list[str] | None:
    """Read one strict corpus classification manifest."""
    try:
        manifest_lines = manifest_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as exc:
        log(
            f"could not read {label} corpus manifest {display_path(manifest_path, repository)}: {exc}",
            error=True,
        )
        return None

    if not manifest_lines:
        log(
            f"{label} corpus manifest is empty: {display_path(manifest_path, repository)}",
            error=True,
        )
        return None

    names: list[str] = []
    seen: set[str] = set()
    for line_number, name in enumerate(manifest_lines, start=1):
        if not name or name != name.strip():
            log(
                f"malformed {label} corpus manifest entry at line {line_number}: {name!r}",
                error=True,
            )
            return None
        if name in seen:
            log(f"duplicate {label} corpus manifest entry: {name!r}", error=True)
            return None
        seen.add(name)
        names.append(name)
    return names


def check_corpus_manifests(
    accepted_manifest_path: Path,
    deferred_manifest_path: Path,
    corpus_directory: Path,
    repository: Path,
) -> tuple[list[str], list[str], int] | None:
    """Require accepted/deferred manifests to classify every discovered corpus case exactly once."""
    accepted_names = read_corpus_manifest(
        accepted_manifest_path, repository, label="accepted"
    )
    deferred_names = read_corpus_manifest(
        deferred_manifest_path, repository, label="deferred"
    )
    if accepted_names is None or deferred_names is None:
        return None

    overlap = sorted(set(accepted_names) & set(deferred_names))
    if overlap:
        log(
            "accepted and deferred corpus manifests overlap: "
            + ", ".join(repr(name) for name in overlap),
            error=True,
        )
        return None

    corpus_names = read_corpus_case_names(corpus_directory, repository)
    if corpus_names is None:
        return None
    manifest_names = accepted_names + deferred_names
    corpus_name_set = set(corpus_names)
    manifest_name_set = set(manifest_names)
    missing = [name for name in corpus_names if name not in manifest_name_set]
    extra = [name for name in manifest_names if name not in corpus_name_set]
    if missing or extra:
        if missing:
            log(
                "corpus cases missing from accepted/deferred manifests: "
                + ", ".join(repr(name) for name in missing),
                error=True,
            )
        if extra:
            log(
                "accepted/deferred manifest names missing from corpus: "
                + ", ".join(repr(name) for name in extra),
                error=True,
            )
        return None

    log(
        f"corpus manifests validated: {len(accepted_names)} accepted + "
        f"{len(deferred_names)} deferred = {len(corpus_names)} discovered cases"
    )
    return accepted_names, deferred_names, len(corpus_names)



def check_accepted_corpus_results(
    summary: object,
    accepted_names: Sequence[str],
) -> bool:
    """Require the Tree-sitter summary to contain exactly the passed manifest cases."""
    if not isinstance(summary, dict):
        log("accepted corpus test returned a non-object JSON summary", error=True)
        return False

    parse_results = summary.get("parse_results")
    if not isinstance(parse_results, list):
        log("accepted corpus test JSON summary has no parse_results array", error=True)
        return False

    executed_names: list[str] = []
    failed_results: list[tuple[str, str]] = []

    def collect_results(results: list[object], path: str) -> bool:
        for index, result in enumerate(results):
            result_path = f"{path}[{index}]"
            if not isinstance(result, dict):
                log(
                    f"accepted corpus test JSON summary has malformed result at {result_path}",
                    error=True,
                )
                return False
            name = result.get("name")
            if not isinstance(name, str) or not name:
                log(
                    f"accepted corpus test JSON summary has malformed name at {result_path}",
                    error=True,
                )
                return False
            if "children" in result:
                children = result["children"]
                if not isinstance(children, list) or not collect_results(
                    children, f"{result_path}.children"
                ):
                    return False
                continue

            outcome = result.get("outcome")
            if not isinstance(outcome, str):
                log(
                    f"accepted corpus test JSON summary has malformed parse outcome at "
                    f"{result_path} ({name!r})",
                    error=True,
                )
                return False
            executed_names.append(name)
            if outcome != "Passed":
                failed_results.append((name, outcome))
        return True

    if not collect_results(parse_results, "parse_results"):
        return False

    expected_names = set(accepted_names)
    executed_name_set = set(executed_names)
    missing = [name for name in accepted_names if name not in executed_name_set]
    unexpected = [name for name in executed_names if name not in expected_names]
    duplicate_names = sorted(
        name for name in executed_name_set if executed_names.count(name) > 1
    )
    if missing or unexpected or duplicate_names:
        details: list[str] = []
        if missing:
            details.append("missing " + ", ".join(repr(name) for name in missing))
        if unexpected:
            details.append("unexpected " + ", ".join(repr(name) for name in unexpected))
        if duplicate_names:
            details.append(
                "duplicate " + ", ".join(repr(name) for name in duplicate_names)
            )
        log("accepted corpus execution did not match manifest: " + "; ".join(details), error=True)
        return False

    if failed_results:
        details = ", ".join(f"{name!r} ({outcome})" for name, outcome in failed_results)
        log(f"accepted corpus cases did not pass: {details}", error=True)
        return False

    return True


def checked_in_editor_json_files(repository: Path) -> list[Path] | None:
    """Return tracked editor JSON files, excluding local dependency trees."""
    try:
        completed = subprocess.run(
            ["git", "-C", str(repository), "ls-files", "-z", "--", "editors"],
            check=False,
            capture_output=True,
        )
    except OSError as exc:
        log(f"could not enumerate checked-in editor JSON files with git: {exc}", error=True)
        return None
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip()
        suffix = f": {detail}" if detail else ""
        log(f"git ls-files failed while enumerating editor JSON files{suffix}", error=True)
        return None

    paths = []
    for filename in completed.stdout.decode("utf-8").split("\0"):
        if filename.endswith(".json"):
            path = repository / filename
            if path.is_file():
                paths.append(path)
    return sorted(paths, key=lambda path: path.as_posix())


def check_generated_artefacts(
    tree_sitter: str,
    tree_sitter_directory: Path,
    repository: Path,
) -> bool:
    """Generate grammar artefacts outside the checkout and compare them byte-for-byte."""
    with tempfile.TemporaryDirectory(prefix="check-editor-tooling-") as temporary:
        temporary_directory = Path(temporary)
        for filename in GRAMMAR_INPUTS:
            source = tree_sitter_directory / filename
            if not source.is_file():
                log(
                    f"required grammar input is missing: {display_path(source, repository)}",
                    error=True,
                )
                return False
            try:
                shutil.copyfile(source, temporary_directory / filename)
            except OSError as exc:
                log(
                    f"could not copy grammar input {display_path(source, repository)}: {exc}",
                    error=True,
                )
                return False

        log("checking generated tree-sitter artefacts in a temporary directory")
        generate_result = run_command(
            [tree_sitter, "generate"],
            cwd=temporary_directory,
            label="tree-sitter generate",
        )
        if generate_result is None or generate_result.returncode != 0:
            status = (
                "could not start"
                if generate_result is None
                else f"exited with status {generate_result.returncode}"
            )
            log(f"tree-sitter generate failed ({status})", error=True)
            return False

        for relative_path in GENERATED_ARTEFACTS:
            checked_in = tree_sitter_directory / relative_path
            generated = temporary_directory / relative_path
            if not checked_in.is_file():
                log(
                    f"checked-in generated artefact is missing: "
                    f"{display_path(checked_in, repository)}",
                    error=True,
                )
                return False
            if not generated.is_file():
                log(
                    f"generated artefact is missing: {display_path(checked_in, repository)}",
                    error=True,
                )
                return False
            if not filecmp.cmp(checked_in, generated, shallow=False):
                log(
                    f"generated artefact differs: {display_path(checked_in, repository)}",
                    error=True,
                )
                return False

    log("generated tree-sitter artefacts match checked-in files")
    return True


def check_highlight_fixture(
    tree_sitter: str,
    tree_sitter_directory: Path,
    repository: Path,
    fixture_name: str,
    expected_assertions: int,
) -> bool:
    """Run one checked-in highlight fixture and validate its assertion count."""
    highlights_path = tree_sitter_directory / "queries" / "highlights.scm"
    fixture_path = tree_sitter_directory / "test" / "highlight" / fixture_name
    for path in (highlights_path, fixture_path):
        if not path.is_file():
            log(
                f"required highlight fixture is missing: {display_path(path, repository)}",
                error=True,
            )
            return False

    log(f"checking highlight captures in {fixture_name}")
    result = run_command(
        [
            tree_sitter,
            "test",
            "--file-name",
            fixture_name,
            "--json-summary",
        ],
        cwd=tree_sitter_directory,
        label=f"tree-sitter {fixture_name} highlight test",
    )
    if result is None or result.returncode != 0:
        status = (
            "could not start" if result is None else f"exited with status {result.returncode}"
        )
        log(f"highlight test failed for {fixture_name} ({status})", error=True)
        return False

    try:
        summary = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        log(f"highlight test returned invalid JSON for {fixture_name}: {exc}", error=True)
        return False

    highlight_results = summary.get("highlight_results") if isinstance(summary, dict) else None
    if not isinstance(highlight_results, list):
        log(f"highlight test returned no result list for {fixture_name}", error=True)
        return False
    matching = [
        result
        for result in highlight_results
        if isinstance(result, dict) and result.get("name") == fixture_name
    ]
    if len(matching) != 1:
        log(
            f"highlight test did not execute exactly one {fixture_name!r} fixture",
            error=True,
        )
        return False

    outcome = matching[0].get("outcome")
    if (
        not isinstance(outcome, dict)
        or not isinstance(outcome.get("AssertionPassed"), dict)
        or outcome["AssertionPassed"].get("assertion_count") != expected_assertions
    ):
        log(
            f"highlight assertions did not pass for {fixture_name} "
            f"(expected {expected_assertions})",
            error=True,
        )
        return False
    log(f"highlight assertions passed for {fixture_name}")
    return True


def check_zed_order_by_highlights(
    tree_sitter: str,
    zed_directory: Path,
    tree_sitter_directory: Path,
    repository: Path,
) -> bool:
    """Execute Zed highlights against the accepted ORDER BY fixture's direction nodes."""
    highlights_path = zed_directory / "languages" / "orna" / "highlights.scm"
    fixture_path = tree_sitter_directory / "test" / "highlight" / ORDER_BY_HIGHLIGHT_FIXTURE_NAME
    for path in (highlights_path, fixture_path):
        if not path.is_file():
            log(
                f"required Zed ORDER BY highlight input is missing: {display_path(path, repository)}",
                error=True,
            )
            return False

    log(f"checking Zed ORDER BY captures in {ORDER_BY_HIGHLIGHT_FIXTURE_NAME}")
    result = run_command(
        [
            tree_sitter,
            "query",
            "--grammar-path",
            str(tree_sitter_directory),
            "--captures",
            str(highlights_path),
            str(fixture_path),
        ],
        cwd=tree_sitter_directory,
        label="tree-sitter Zed ORDER BY highlight query",
    )
    if result is None or result.returncode != 0:
        status = "could not start" if result is None else f"exited with status {result.returncode}"
        log(f"Zed ORDER BY highlight query failed ({status})", error=True)
        return False

    capture_line = re.compile(
        r"capture:\s+\d+\s+-\s+(?P<name>[^,]+),.*text: `(?P<text>[^`]*)`"
    )
    direction_captures = [
        match.group("text")
        for line in result.stdout.splitlines()
        if (match := capture_line.search(line)) is not None
        and match.group("name") == "keyword"
        and match.group("text") in ORDER_BY_DIRECTION_TEXTS
    ]
    if direction_captures != list(ORDER_BY_DIRECTION_TEXTS):
        log(
            "Zed ORDER BY highlight query did not capture the accepted direction nodes as "
            f"keyword in order {ORDER_BY_DIRECTION_TEXTS!r}: observed {direction_captures!r}",
            error=True,
        )
        return False

    log(
        "Zed ORDER BY highlight query captured accepted direction nodes: "
        + ", ".join(ORDER_BY_DIRECTION_TEXTS)
    )
    return True


def check_target_path_highlights(
    tree_sitter: str,
    zed_directory: Path,
    tree_sitter_directory: Path,
    repository: Path,
) -> bool:
    """Check target-final function and intermediate/ordinary property captures.

    This invokes only the tree-sitter query CLI. It deliberately does not launch
    Zed or any GUI/editor runtime.
    """
    query_paths = (
        ("tree-sitter", tree_sitter_directory / "queries" / "highlights.scm"),
        ("Zed", zed_directory / "languages" / "orna" / "highlights.scm"),
    )
    capture_line = re.compile(
        r"capture:\s+\d+\s+-\s+(?P<name>[^,]+),.*text: `(?P<text>[^`]*)`"
    )
    for fixture_name, expected in TARGET_HIGHLIGHT_EXPECTATIONS:
        fixture_path = tree_sitter_directory / "test" / "highlight" / fixture_name
        if not fixture_path.is_file():
            log(
                f"required target highlight fixture is missing: {display_path(fixture_path, repository)}",
                error=True,
            )
            return False
        for label, query_path in query_paths:
            if not query_path.is_file():
                log(
                    f"required {label} highlight query is missing: {display_path(query_path, repository)}",
                    error=True,
                )
                return False
            log(f"checking {label} target captures in {fixture_name}")
            result = run_command(
                [
                    tree_sitter,
                    "query",
                    "--grammar-path",
                    str(tree_sitter_directory),
                    "--captures",
                    str(query_path),
                    str(fixture_path),
                ],
                cwd=tree_sitter_directory,
                label=f"tree-sitter {label} target highlight query",
            )
            if result is None or result.returncode != 0:
                status = "could not start" if result is None else f"exited with status {result.returncode}"
                log(f"{label} target highlight query failed for {fixture_name} ({status})", error=True)
                return False
            captures = [
                (match.group("name"), match.group("text"))
                for line in result.stdout.splitlines()
                if (match := capture_line.search(line)) is not None
            ]
            for capture_name, expected_texts in expected.items():
                observed = [text for name, text in captures if name == capture_name and text in expected_texts]
                if observed != list(expected_texts):
                    log(
                        f"{label} target highlight query for {fixture_name} did not capture "
                        f"{capture_name} texts {expected_texts!r}: observed {observed!r}",
                        error=True,
                    )
                    return False
            log(f"{label} target captures passed for {fixture_name}")
    return True


def check_alter_rename_highlights(
    tree_sitter: str,
    tree_sitter_directory: Path,
    repository: Path,
) -> bool:
    """Run accepted ALTER, CLIENT action/Inspector, and resource/stream highlight fixtures."""
    return (
        check_highlight_fixture(
            tree_sitter,
            tree_sitter_directory,
            repository,
            "alter_type_rename.orna",
            4,
        )
        and check_highlight_fixture(
            tree_sitter,
            tree_sitter_directory,
            repository,
            "accepted_actions_inspector.orna",
            18,
        )
        and check_highlight_fixture(
            tree_sitter,
            tree_sitter_directory,
            repository,
            "accepted_resources_streams.orna",
            14,
        )
    )


def check_tree_sitter_package(tree_sitter_directory: Path, repository: Path) -> bool:
    """Validate the grammar's source-only npm package boundary."""
    package_path = tree_sitter_directory / "package.json"
    try:
        with package_path.open(encoding="utf-8") as stream:
            package = json.load(stream)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        log(f"invalid tree-sitter package metadata: {exc}", error=True)
        return False

    if not isinstance(package, dict):
        log(
            f"{display_path(package_path, repository)} must contain a package object",
            error=True,
        )
        return False

    if package.get("private") is not True:
        log(
            f"{display_path(package_path, repository)} must be marked private "
            "because no Node binding is shipped",
            error=True,
        )
        return False
    if "main" in package:
        log(
            f"{display_path(package_path, repository)} must not advertise a missing Node entrypoint",
            error=True,
        )
        return False
    return True


def check_tree_sitter_metadata(tree_sitter_directory: Path, repository: Path) -> bool:
    """Validate tree-sitter grammar metadata and its package parity."""
    metadata_path = tree_sitter_directory / "tree-sitter.json"
    try:
        with metadata_path.open(encoding="utf-8") as stream:
            metadata = json.load(stream)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        log(f"invalid tree-sitter grammar metadata: {exc}", error=True)
        return False

    if not isinstance(metadata, dict) or not isinstance(metadata.get("grammars"), list):
        log(
            f"{display_path(metadata_path, repository)} must contain a grammars array",
            error=True,
        )
        return False
    grammars = metadata["grammars"]
    if len(grammars) != 1 or not isinstance(grammars[0], dict):
        log(
            f"{display_path(metadata_path, repository)} must contain exactly one grammar object",
            error=True,
        )
        return False

    grammar = grammars[0]
    expected_fields = {
        "name": "orna",
        "scope": "source.orna",
        "path": ".",
        "file-types": ["orna"],
        "highlights": "queries/highlights.scm",
    }
    for field, expected in expected_fields.items():
        actual = grammar.get(field)
        if actual != expected:
            log(
                f"{display_path(metadata_path, repository)} grammar {field!r} must be "
                f"{expected!r}; found {actual!r}",
                error=True,
            )
            return False

    grammar_path = tree_sitter_directory / grammar["path"]
    if not grammar_path.is_dir():
        log(
            f"tree-sitter grammar path is missing: {display_path(grammar_path, repository)}",
            error=True,
        )
        return False
    highlights_path = tree_sitter_directory / grammar["highlights"]
    if not highlights_path.is_file():
        log(
            f"tree-sitter highlights query is missing: {display_path(highlights_path, repository)}",
            error=True,
        )
        return False

    package_path = tree_sitter_directory / "package.json"
    try:
        with package_path.open(encoding="utf-8") as stream:
            package = json.load(stream)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        log(f"invalid tree-sitter package metadata: {exc}", error=True)
        return False

    package_tree_sitter = package.get("tree-sitter") if isinstance(package, dict) else None
    if not isinstance(package_tree_sitter, dict):
        log(
            f"{display_path(package_path, repository)} must contain tree-sitter metadata",
            error=True,
        )
        return False
    expected_package_fields = {
        "name": "tree-sitter-orna",
        "tree-sitter.scopes": {"source.orna": "orna"},
        "tree-sitter.file-types": ["orna"],
        "tree-sitter.highlights": ["queries/highlights.scm"],
    }
    package_fields = {
        "name": package.get("name"),
        "tree-sitter.scopes": package_tree_sitter.get("scopes"),
        "tree-sitter.file-types": package_tree_sitter.get("file-types"),
        "tree-sitter.highlights": package_tree_sitter.get("highlights"),
    }
    for field, expected in expected_package_fields.items():
        actual = package_fields[field]
        if actual != expected:
            log(
                f"{display_path(package_path, repository)} {field} must be "
                f"{expected!r} to match the orna grammar; found {actual!r}",
                error=True,
            )
            return False

    log("tree-sitter grammar metadata and package parity passed")
    return True


def check_textmate_grammar(grammar_path: Path, repository: Path) -> bool:
    """Validate the TextMate grammar shape and its local repository references."""
    try:
        with grammar_path.open(encoding="utf-8") as stream:
            grammar = json.load(stream)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        log(f"invalid TextMate grammar {display_path(grammar_path, repository)}: {exc}", error=True)
        return False

    if not isinstance(grammar, dict) or grammar.get("scopeName") != "source.orna":
        log(
            f"TextMate grammar {display_path(grammar_path, repository)} must declare scopeName source.orna",
            error=True,
        )
        return False

    patterns = grammar.get("patterns")
    repository_entries = grammar.get("repository")
    if not isinstance(patterns, list) or not patterns:
        log(
            f"TextMate grammar {display_path(grammar_path, repository)} must define non-empty patterns",
            error=True,
        )
        return False
    if not isinstance(repository_entries, dict) or not repository_entries:
        log(
            f"TextMate grammar {display_path(grammar_path, repository)} must define a non-empty repository",
            error=True,
        )
        return False

    includes: set[str] = set()

    def collect_includes(value: object) -> None:
        if isinstance(value, dict):
            include = value.get("include")
            if isinstance(include, str) and include.startswith("#"):
                includes.add(include[1:])
            for child in value.values():
                collect_includes(child)
        elif isinstance(value, list):
            for child in value:
                collect_includes(child)

    collect_includes(grammar.get("patterns"))
    collect_includes(grammar.get("repository"))
    missing = sorted(name for name in includes if name not in repository_entries)
    if missing:
        log(
            f"TextMate grammar {display_path(grammar_path, repository)} has dangling local includes: "
            f"{', '.join(f'#{name}' for name in missing)}",
            error=True,
        )
        return False

    log(f"validated TextMate grammar {display_path(grammar_path, repository)}")
    return True


def _normalize_textmate_value(value: object, *, root: bool = False) -> object:
    """Normalize JSON ordering while preserving pattern and capture order."""
    if isinstance(value, dict):
        return {
            key: _normalize_textmate_value(child)
            for key, child in sorted(value.items())
            if not (root and key in TEXTMATE_PARITY_ALLOWED_ROOT_KEYS)
        }
    if isinstance(value, list):
        return [_normalize_textmate_value(child) for child in value]
    return value


def _textmate_parity_mismatches(
    expected: object,
    actual: object,
    path: str = "$",
    *,
    limit: int = 12,
) -> list[str]:
    """Return deterministic, bounded paths for normalized grammar mismatches."""
    mismatches: list[str] = []

    def visit(left: object, right: object, current_path: str) -> None:
        if len(mismatches) >= limit:
            return
        if isinstance(left, dict) and isinstance(right, dict):
            for key in sorted(set(left) | set(right)):
                child_path = f"{current_path}.{key}"
                if key not in left:
                    mismatches.append(f"{child_path}: missing from canonical grammar")
                elif key not in right:
                    mismatches.append(f"{child_path}: missing from VS Code grammar")
                else:
                    visit(left[key], right[key], child_path)
                if len(mismatches) >= limit:
                    return
            return
        if isinstance(left, list) and isinstance(right, list):
            if len(left) != len(right):
                mismatches.append(f"{current_path}: expected {len(left)} entries, found {len(right)}")
                return
            for index, (left_item, right_item) in enumerate(zip(left, right)):
                visit(left_item, right_item, f"{current_path}[{index}]")
                if len(mismatches) >= limit:
                    return
            return
        if type(left) is not type(right) or left != right:
            mismatches.append(f"{current_path}: expected {left!r}, found {right!r}")

    visit(expected, actual, path)
    return mismatches


def check_textmate_grammar_parity(
    canonical_path: Path,
    vscode_path: Path,
    repository: Path,
) -> bool:
    """Compare the stable TextMate subset shared by the canonical and VS Code grammars."""
    grammars: list[object] = []
    for path in (canonical_path, vscode_path):
        try:
            grammars.append(json.loads(path.read_text(encoding="utf-8")))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            log(f"could not read TextMate grammar {display_path(path, repository)}: {exc}", error=True)
            return False

    for label, grammar in zip(("canonical", "VS Code"), grammars):
        grammar_path = canonical_path if label == "canonical" else vscode_path
        if not isinstance(grammar, dict):
            log(
                f"TextMate parity has no stable comparable subset: {label} grammar "
                f"{display_path(grammar_path, repository)} is not a JSON object",
                error=True,
            )
            return False
        missing = [key for key in TEXTMATE_PARITY_COMPARABLE_KEYS if key not in grammar]
        if missing:
            log(
                f"TextMate parity has no stable comparable subset: {label} grammar is missing "
                + ", ".join(missing),
                error=True,
            )
            return False

    normalized = [_normalize_textmate_value(grammar, root=True) for grammar in grammars]
    mismatches = _textmate_parity_mismatches(normalized[0], normalized[1])
    if mismatches:
        log(
            "TextMate grammar parity mismatch after deterministic normalization "
            f"({display_path(canonical_path, repository)} -> {display_path(vscode_path, repository)}): "
            + "; ".join(mismatches),
            error=True,
        )
        return False

    allowed = ", ".join(sorted(TEXTMATE_PARITY_ALLOWED_ROOT_KEYS))
    comparable = ", ".join(TEXTMATE_PARITY_COMPARABLE_KEYS)
    log(
        "validated normalized TextMate parity "
        f"({comparable}; allowed root metadata differences: {allowed})"
    )
    return True




def check_neovim_integration(neovim_directory: Path, repository: Path) -> bool:
    """Validate the checked-in Neovim filetype and native/legacy LSP setup."""
    primary_path = neovim_directory / "lua" / "orna" / "init.lua"
    lua_paths = [primary_path] if primary_path.is_file() else sorted(neovim_directory.rglob("*.lua"))
    if not lua_paths:
        log(
            "required Neovim integration is missing: "
            f"{display_path(primary_path, repository)} or an equivalent Lua source",
            error=True,
        )
        return False

    sources: list[str] = []
    for path in lua_paths:
        try:
            sources.append(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError) as exc:
            log(f"could not read Neovim integration {display_path(path, repository)}: {exc}", error=True)
            return False
    source = "\n".join(sources)

    requirements = (
        (
            "the .orna extension map",
            r"vim\.filetype\.add\s*\(\s*\{\s*extension\s*=\s*\{\s*orna\s*=\s*[\"']orna[\"']\s*\}",
        ),
        (
            "the default orna-lsp command",
            r"defaults\s*=\s*\{(?s:.*?)cmd\s*=\s*\{\s*[\"']orna-lsp[\"']\s*\}",
        ),
        (
            "the native orna filetype",
            r"vim\.lsp\.config\s*\(\s*[\"']orna[\"'](?s:.*?)filetypes\s*=\s*\{\s*[\"']orna[\"']\s*\}",
        ),
        (
            "the native vim.lsp.enable(\"orna\") call",
            r"vim\.lsp\.enable\s*\(\s*[\"']orna[\"']\s*\)",
        ),
        (
            "the legacy orna filetype",
            r"nvim_create_autocmd\s*\(\s*[\"']FileType[\"'](?s:.*?)pattern\s*=\s*[\"']orna[\"']",
        ),
        (
            "the legacy native client fallback",
            r"vim\.lsp\.start_client\s*\(",
        ),
    )
    for description, pattern in requirements:
        if re.search(pattern, source) is None:
            log(
                f"Neovim integration is missing {description}: "
                f"{display_path(primary_path, repository)}",
                error=True,
            )
            return False

    log(f"validated Neovim integration {display_path(primary_path, repository)}")
    return True


def check_vim_filetype_detection(ftdetect_path: Path, repository: Path) -> bool:
    """Validate Vim's accepted .orna filetype detection autocmd."""
    try:
        source = ftdetect_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        log(f"could not read Vim filetype detection {display_path(ftdetect_path, repository)}: {exc}", error=True)
        return False

    pattern = r"(?m)^\s*(?:au|autocmd)\s+BufRead,BufNewFile\s+\*\.orna\s+setfiletype\s+orna\s*$"
    if re.search(pattern, source) is None:
        log(
            "Vim filetype detection must map *.orna on BufRead and BufNewFile "
            f"to filetype orna: {display_path(ftdetect_path, repository)}",
            error=True,
        )
        return False

    log(f"validated Vim filetype detection {display_path(ftdetect_path, repository)}")
    return True


def check_vscode_package(package_path: Path, repository: Path) -> bool:
    """Validate the accepted VS Code package metadata contract."""
    try:
        with package_path.open(encoding="utf-8") as stream:
            package = json.load(stream)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        log(f"invalid VS Code package metadata {display_path(package_path, repository)}: {exc}", error=True)
        return False

    if not isinstance(package, dict) or package.get("main") != "./extension.js":
        log(
            f"VS Code package metadata must set main to ./extension.js: {display_path(package_path, repository)}",
            error=True,
        )
        return False

    activation_events = package.get("activationEvents")
    if not isinstance(activation_events, list) or "onLanguage:orna" not in activation_events:
        log(
            "VS Code package metadata must activate onLanguage:orna: "
            f"{display_path(package_path, repository)}",
            error=True,
        )
        return False

    contributes = package.get("contributes")
    if not isinstance(contributes, dict):
        log(
            f"VS Code package metadata must define contributes: {display_path(package_path, repository)}",
            error=True,
        )
        return False

    languages = contributes.get("languages")
    orna_language = next(
        (entry for entry in languages if isinstance(entry, dict) and entry.get("id") == "orna"),
        None,
    ) if isinstance(languages, list) else None
    if not isinstance(orna_language, dict):
        log(
            f"VS Code package metadata must define the orna language contribution: {display_path(package_path, repository)}",
            error=True,
        )
        return False
    if not isinstance(orna_language.get("extensions"), list) or ".orna" not in orna_language["extensions"]:
        log(
            "VS Code language contribution must map the .orna extension: "
            f"{display_path(package_path, repository)}",
            error=True,
        )
        return False
    if orna_language.get("configuration") != "./language-configuration.json":
        log(
            "VS Code language contribution must use ./language-configuration.json: "
            f"{display_path(package_path, repository)}",
            error=True,
        )
        return False

    grammars = contributes.get("grammars")
    orna_grammar = next(
        (entry for entry in grammars if isinstance(entry, dict) and entry.get("language") == "orna"),
        None,
    ) if isinstance(grammars, list) else None
    if not isinstance(orna_grammar, dict):
        log(
            f"VS Code package metadata must define the orna grammar: {display_path(package_path, repository)}",
            error=True,
        )
        return False
    if orna_grammar.get("scopeName") != "source.orna":
        log(
            "VS Code grammar contribution must use scopeName source.orna: "
            f"{display_path(package_path, repository)}",
            error=True,
        )
        return False
    if orna_grammar.get("path") != "./syntaxes/orna.tmLanguage.json":
        log(
            "VS Code grammar contribution must point to ./syntaxes/orna.tmLanguage.json: "
            f"{display_path(package_path, repository)}",
            error=True,
        )
        return False

    configuration = contributes.get("configuration")
    properties = configuration.get("properties") if isinstance(configuration, dict) else None
    lsp_path = properties.get("orna.lsp.path") if isinstance(properties, dict) else None
    if not isinstance(lsp_path, dict) or lsp_path.get("type") != "string":
        log(
            "VS Code package metadata must define string config orna.lsp.path: "
            f"{display_path(package_path, repository)}",
            error=True,
        )
        return False
    if lsp_path.get("default") != "orna-lsp":
        log(
            "VS Code config orna.lsp.path must default to orna-lsp: "
            f"{display_path(package_path, repository)}",
            error=True,
        )
        return False

    log(f"validated VS Code package metadata {display_path(package_path, repository)}")
    return True


def check_vscode_extension(extension_path: Path, repository: Path) -> bool:
    """Validate the VS Code extension's static selector and server-path strings."""
    try:
        source = extension_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        log(f"could not read VS Code extension {display_path(extension_path, repository)}: {exc}", error=True)
        return False

    requirements = (
        ("getConfiguration(\"orna\")", r"getConfiguration\s*\(\s*[\"']orna[\"']\s*\)"),
        ("the orna.lsp.path lookup", r"\.get\s*\(\s*[\"']lsp\.path[\"']\s*\)"),
        ("the orna-lsp fallback command", r"configuredPath\s*\|\|\s*[\"']orna-lsp[\"']"),
        ("the orna document selector", r"documentSelector\s*:\s*\[\s*\{\s*language\s*:\s*[\"']orna[\"']\s*\}\s*\]"),
    )
    for description, pattern in requirements:
        if re.search(pattern, source) is None:
            log(
                f"VS Code extension is missing {description}: {display_path(extension_path, repository)}",
                error=True,
            )
            return False

    log(f"validated VS Code extension static contract {display_path(extension_path, repository)}")
    return True


def tree_sitter_keywords(grammar_path: Path, repository: Path) -> set[str] | None:
    """Read the canonical tree-sitter keyword list from grammar.js."""
    try:
        source = grammar_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        log(f"could not read tree-sitter grammar {display_path(grammar_path, repository)}: {exc}", error=True)
        return None
    match = re.search(r"const\s+KEYWORDS\s*=\s*\[(?P<body>.*?)\];", source, re.DOTALL)
    if match is None:
        log(f"tree-sitter grammar {display_path(grammar_path, repository)} has no KEYWORDS list", error=True)
        return None
    keywords = {word.upper() for word in re.findall(r"['\"]([A-Za-z][A-Za-z0-9_]*)['\"]", match.group("body"))}
    if not keywords:
        log(f"tree-sitter grammar {display_path(grammar_path, repository)} has an empty KEYWORDS list", error=True)
        return None
    return keywords


def editor_words_from_regex_values(values: Sequence[str]) -> set[str]:
    """Extract words from an editor's keyword/type regex values."""
    return {word.upper() for value in values for word in re.findall(r"[A-Za-z][A-Za-z0-9_]*", value)}


def textmate_surface_words(grammar_path: Path, repository: Path) -> set[str] | None:
    """Read keyword and scalar-type regexes from a TextMate grammar."""
    try:
        grammar = json.loads(grammar_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        log(f"invalid TextMate grammar {display_path(grammar_path, repository)}: {exc}", error=True)
        return None
    entries = grammar.get("repository") if isinstance(grammar, dict) else None
    if not isinstance(entries, dict):
        log(f"TextMate grammar {display_path(grammar_path, repository)} has no repository for keyword parity", error=True)
        return None
    values: list[str] = []
    def collect(value: object) -> None:
        if isinstance(value, dict):
            match = value.get("match")
            if isinstance(match, str):
                values.append(match)
            for child in value.values():
                collect(child)
        elif isinstance(value, list):
            for child in value:
                collect(child)
    # Scalar/type entries count as coverage although they use a non-keyword face.
    for category in ("keywords", "scalar-types"):
        collect(entries.get(category))
    return editor_words_from_regex_values(values)


def vim_surface_words(syntax_path: Path, repository: Path) -> set[str] | None:
    """Read Vim keyword and type declarations, including continuation lines."""
    try:
        lines = syntax_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as exc:
        log(f"could not read Vim syntax {display_path(syntax_path, repository)}: {exc}", error=True)
        return None
    values: list[str] = []
    collecting = False
    for line in lines:
        stripped = line.strip()
        declaration = re.match(r"syntax\s+(?:keyword\s+orna(?:Statement|Boolean|Type)|match\s+ornaType)\s+(.+)", stripped)
        if declaration is not None:
            collecting = True
            values.append(declaration.group(1))
        elif collecting and stripped.startswith("\\"):
            values.append(stripped[1:])
        else:
            collecting = False
    return editor_words_from_regex_values(values)


def emacs_surface_words(integration_path: Path, repository: Path) -> set[str] | None:
    """Read the Emacs keyword and scalar-type variables."""
    try:
        source = integration_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        log(f"could not read Emacs integration {display_path(integration_path, repository)}: {exc}", error=True)
        return None
    values: list[str] = []
    for variable in ("orna-keywords", "orna-types"):
        match = re.search(rf"\(defvar\s+{re.escape(variable)}\s+'\((?P<body>.*?)\)\)", source, re.DOTALL)
        if match is None:
            log(f"Emacs integration {display_path(integration_path, repository)} has no {variable} list", error=True)
            return None
        values.extend(re.findall(r'"([^"]+)"', match.group("body")))
    return editor_words_from_regex_values(values)


def check_fallback_keyword_parity(
    tree_sitter_grammar: Path,
    textmate_grammars: Sequence[Path],
    vim_syntax: Path,
    emacs_integration: Path,
    repository: Path,
) -> bool:
    """Require every tree-sitter keyword on each accepted fallback surface.

    Fallback editors may expose documented supersets; scalar/type sections
    count as coverage even though they use a non-keyword face.
    """
    keywords = tree_sitter_keywords(tree_sitter_grammar, repository)
    if keywords is None:
        return False
    surfaces: list[tuple[str, Path, set[str] | None]] = [
        ("TextMate", path, textmate_surface_words(path, repository)) for path in textmate_grammars
    ]
    surfaces.extend([
        ("Vim", vim_syntax, vim_surface_words(vim_syntax, repository)),
        ("Emacs", emacs_integration, emacs_surface_words(emacs_integration, repository)),
    ])
    for label, path, words in surfaces:
        if words is None:
            return False
        missing = sorted(keywords - words)
        if missing:
            log(f"{label} fallback {display_path(path, repository)} is missing tree-sitter keywords: {', '.join(missing)}", error=True)
            return False
        log(f"validated {label} fallback keyword parity {display_path(path, repository)} ({len(keywords)} tree-sitter keywords; editor supersets allowed)")
    return True

def check_unicode_identifier_parity(
    tree_sitter_grammar: Path,
    textmate_grammars: Sequence[Path],
    vim_syntax: Path,
    repository: Path,
) -> bool:
    """Check exact Unicode rules and Vim's documented bounded fallback.

    Tree-sitter and TextMate expose the accepted ``Alphabetic`` property and ``N`` category. Vim
    does not expose Rust's full ``char.is_alphabetic()``/``is_numeric()``
    predicates as regexp atoms, so its fallback is deliberately bounded to a
    syntax-local ``iskeyword`` value and the documented ``\\k``/``\\K``
    atoms. Explicit lookarounds must provide the identifier boundaries; the
    buffer-local ``\\<``/``\\>`` atoms and ASCII-only ``\\d`` are not
    acceptable substitutes.
    """
    try:
        tree_sitter_source = tree_sitter_grammar.read_text(encoding="utf-8")
        vim_source = vim_syntax.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        log(f"could not read Unicode identifier tooling: {exc}", error=True)
        return False

    tree_sitter_pattern = r"_unquoted_identifier:\s*\(\$\)\s*=>\s*/\[\\p{Alphabetic}_\]\[\\p{Alphabetic}\\p{N}_\]\*/"
    if re.search(tree_sitter_pattern, tree_sitter_source) is None:
        log(
            "tree-sitter unquoted identifier rule must use Unicode alphabetic code points, Unicode number code points, and underscore",
            error=True,
        )
        return False

    textmate_pattern = r"[\\p{Alphabetic}_][\\p{Alphabetic}\\p{N}_]*"
    for grammar_path in textmate_grammars:
        try:
            grammar_source = grammar_path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            log(f"could not read TextMate grammar {display_path(grammar_path, repository)}: {exc}", error=True)
            return False
        if textmate_pattern not in grammar_source or "[A-Za-z_][A-Za-z0-9_]*" in grammar_source:
            log(
                f"TextMate identifier/function rules do not cover Unicode alphabetic and number code points: "
                f"{display_path(grammar_path, repository)}",
                error=True,
            )
            return False

    if re.search(r"(?m)^syntax\s+iskeyword\s+@,48-57,_\s*$", vim_source) is None:
        log(
            "Vim fallback must set syntax-local iskeyword to @,48-57,_ for its bounded keyword class",
            error=True,
        )
        return False

    vim_rule_matches = re.findall(r"(?m)^syntax\s+match\s+orna(Function|Identifier)\s+(.+)$", vim_source)
    vim_rules = dict(vim_rule_matches)
    if len(vim_rule_matches) != 2 or set(vim_rules) != {"Function", "Identifier"}:
        log("Vim fallback must define exactly one Function and one Identifier rule", error=True)
        return False

    for kind, rule in vim_rules.items():
        required = (r"\%#=2", r"\k\@<!", r"\K", r"\k*")
        if any(atom not in rule for atom in required) or r"\<" in rule or r"\>" in rule or r"\d" in rule:
            log(
                f"Vim {kind} fallback must use Unicode-aware \\k/\\K atoms with explicit lookaround boundaries; "
                "buffer word boundaries and ASCII \\d are forbidden",
                error=True,
            )
            return False
        if kind == "Function" and r"\ze\s*(" not in rule:
            log("Vim Function fallback must end the match before optional whitespace and (", error=True)
            return False
        if kind == "Identifier" and r"\k\@!" not in rule:
            log("Vim Identifier fallback must end at an explicit non-identifier boundary", error=True)
            return False

    log(
        "validated Unicode identifier rules across tree-sitter and TextMate; "
        "Vim uses a documented syntax-local iskeyword/\\k bounded fallback, not exact Rust category parity"
    )
    return True


def check_helix_configuration(configuration_path: Path, repository: Path) -> bool:
    """Validate the checked-in Helix language, server, and grammar entries."""
    if tomllib is None:
        log(
            "Helix configuration check unavailable: Python 3.11+ is required; "
            "checked-in Helix TOML was not validated",
            error=True,
        )
        return False
    try:
        with configuration_path.open("rb") as stream:
            configuration = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        log(f"invalid Helix configuration: {exc}", error=True)
        return False

    languages = configuration.get("language")
    language_servers = configuration.get("language-server")
    grammars = configuration.get("grammar")
    if not isinstance(languages, list) or not isinstance(language_servers, list) or not isinstance(grammars, list):
        log("Helix configuration does not contain the required table arrays", error=True)
        return False

    orna_language = next(
        (entry for entry in languages if isinstance(entry, dict) and entry.get("name") == "orna"),
        None,
    )
    if not isinstance(orna_language, dict):
        log("Helix configuration does not define the orna language", error=True)
        return False
    file_types = orna_language.get("file-types")
    language_server_names = orna_language.get("language-servers")
    if (
        orna_language.get("scope") != "source.orna"
        or not isinstance(file_types, list)
        or "orna" not in file_types
    ):
        log("Helix orna language entry has an invalid scope or file type", error=True)
        return False
    if not isinstance(language_server_names, list) or "orna-lsp" not in language_server_names:
        log("Helix orna language does not register orna-lsp", error=True)
        return False

    orna_server = next(
        (entry for entry in language_servers if isinstance(entry, dict) and entry.get("name") == "orna-lsp"),
        None,
    )
    if not isinstance(orna_server, dict) or orna_server.get("command") != "orna-lsp":
        log("Helix configuration has an invalid orna-lsp server entry", error=True)
        return False

    orna_grammar = next(
        (entry for entry in grammars if isinstance(entry, dict) and entry.get("name") == "orna"),
        None,
    )
    if not isinstance(orna_grammar, dict):
        log("Helix configuration does not define the orna grammar", error=True)
        return False
    source = orna_grammar.get("source")
    if not isinstance(source, dict) or source.get("path") != "editors/tree-sitter-orna":
        log("Helix orna grammar does not point to editors/tree-sitter-orna", error=True)
        return False

    log(f"validated Helix configuration {display_path(configuration_path, repository)}")
    return True


def check_zed_extension_metadata(extension_path: Path, repository: Path) -> bool:
    """Validate the accepted static Zed extension metadata contract."""
    if tomllib is None:
        log(
            "Zed extension metadata check unavailable: Python 3.11+ is required; "
            "checked-in Zed TOML was not validated",
            error=True,
        )
        return False
    try:
        with extension_path.open("rb") as stream:
            extension = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        log(f"invalid Zed extension metadata: {exc}", error=True)
        return False

    if (
        extension.get("id") != "orna"
        or type(extension.get("schema_version")) is not int
        or extension.get("schema_version") != 1
    ):
        log("Zed extension metadata has an invalid id or schema_version", error=True)
        return False

    language_servers = extension.get("language_servers")
    orna_lsp = language_servers.get("orna-lsp") if isinstance(language_servers, dict) else None
    if not isinstance(orna_lsp, dict):
        log("Zed extension metadata does not define language_servers.orna-lsp", error=True)
        return False
    if (
        orna_lsp.get("languages") != ["Orna"]
        or orna_lsp.get("language_ids") != {"Orna": "orna"}
    ):
        log("Zed orna-lsp metadata has invalid languages or language_ids", error=True)
        return False

    grammars = extension.get("grammars")
    orna_grammar = grammars.get("orna") if isinstance(grammars, dict) else None
    if not isinstance(orna_grammar, dict):
        log("Zed extension metadata does not define grammars.orna", error=True)
        return False
    if (
        orna_grammar.get("repository") != "https://github.com/workingdir/ornadb"
        or orna_grammar.get("path") != "editors/tree-sitter-orna"
    ):
        log("Zed orna grammar metadata has an invalid repository or path", error=True)
        return False
    revision = orna_grammar.get("rev")
    if not isinstance(revision, str) or re.fullmatch(r"[0-9a-f]{40}", revision) is None:
        log("Zed orna grammar metadata has an invalid pinned revision", error=True)
        return False
    if revision != ACCEPTED_ZED_GRAMMAR_REVISION:
        log(
            "Zed orna grammar metadata has an unaccepted pinned revision: "
            f"expected {ACCEPTED_ZED_GRAMMAR_REVISION}, found {revision}",
            error=True,
        )
        return False

    log(f"validated Zed extension metadata {display_path(extension_path, repository)}")
    return True


def check_zed_language_configuration(
    language_path: Path,
    language_server_path: Path,
    repository: Path,
) -> bool:
    """Validate the checked-in Zed language and language-server TOML contracts."""
    if tomllib is None:
        log(
            "Zed language configuration check unavailable: Python 3.11+ is required; "
            "checked-in Zed TOML was not validated",
            error=True,
        )
        return False

    configurations: list[tuple[str, Path]] = [
        ("Zed language configuration", language_path),
        ("Zed language-server configuration", language_server_path),
    ]
    loaded: dict[Path, object] = {}
    for label, path in configurations:
        if not path.is_file():
            log(
                f"required {label} is missing: {display_path(path, repository)}",
                error=True,
            )
            return False
        try:
            with path.open("rb") as stream:
                loaded[path] = tomllib.load(stream)
        except (OSError, tomllib.TOMLDecodeError) as exc:
            log(
                f"invalid {label.lower()} {display_path(path, repository)}: {exc}",
                error=True,
            )
            return False

    language = loaded[language_path]
    language_server = loaded[language_server_path]
    if not isinstance(language, dict):
        log(
            f"Zed language configuration {display_path(language_path, repository)} "
            "must contain a top-level TOML table",
            error=True,
        )
        return False
    if not isinstance(language_server, dict):
        log(
            f"Zed language-server configuration {display_path(language_server_path, repository)} "
            "must contain a top-level TOML table",
            error=True,
        )
        return False

    language_expectations = (
        ("name", "Orna"),
        ("grammar", "orna"),
        ("path_suffixes", ["orna"]),
    )
    for key, expected in language_expectations:
        actual = language.get(key)
        if actual != expected:
            log(
                f"Zed language configuration {display_path(language_path, repository)} "
                f"key '{key}' must be {expected!r}; found {actual!r}",
                error=True,
            )
            return False

    language_server_expectations = (
        ("name", "orna-lsp"),
        ("language", "Orna"),
        ("command", "orna-lsp"),
    )
    for key, expected in language_server_expectations:
        actual = language_server.get(key)
        if actual != expected:
            log(
                f"Zed language-server configuration {display_path(language_server_path, repository)} "
                f"key '{key}' must be {expected!r}; found {actual!r}",
                error=True,
            )
            return False

    log(
        "validated Zed language configuration "
        f"{display_path(language_path, repository)} and language-server configuration "
        f"{display_path(language_server_path, repository)}"
    )
    return True


def check_emacs_integration(
    emacs: str | None,
    integration_path: Path,
    repository: Path,
) -> bool | None:
    """Load the checked-in Emacs mode when its optional runtime is available."""
    if not integration_path.is_file():
        log(
            f"required Emacs integration is missing: {display_path(integration_path, repository)}",
            error=True,
        )
        return False
    if emacs is None:
        log(
            "Emacs batch load unavailable: emacs was not found on PATH; "
            "checked-in Emacs integration was not runtime-verified",
        )
        return None

    log("checking Emacs Eglot prerequisite")
    eglot_result = run_command(
        [
            emacs,
            "--batch",
            "--quick",
            "--eval",
            "(if (require 'eglot nil t) (princ \"eglot-available\") (kill-emacs 2))",
        ],
        cwd=repository,
        label="Emacs Eglot prerequisite",
    )
    if eglot_result is None or eglot_result.returncode != 0 or "eglot-available" not in eglot_result.stdout:
        status = "could not start" if eglot_result is None else f"exited with status {eglot_result.returncode}"
        log(
            f"Emacs batch load unavailable ({status}): Eglot is not available; "
            "checked-in Emacs integration was not runtime-verified",
        )
        return None

    log("checking editors/emacs/orna-eglot.el with Emacs batch load")
    result = run_command(
        [
            emacs,
            "--batch",
            "--quick",
            "--load",
            str(integration_path),
            "--eval",
            "(progn (unless (fboundp 'orna-mode) (error \"orna-mode is not defined\")) "
            "(unless (fboundp 'orna-setup-eglot) (error \"orna-setup-eglot is not defined\")) "
            "(princ \"orna-mode-loaded\"))",
        ],
        cwd=repository,
        label="Emacs batch load",
    )
    if result is None or result.returncode != 0 or "orna-mode-loaded" not in result.stdout:
        status = "could not start" if result is None else f"exited with status {result.returncode}"
        log(f"Emacs batch load failed ({status})", error=True)
        return False
    log("Emacs batch load passed")
    return True



def check_zed_highlights(zed_directory: Path, repository: Path) -> bool:
    """Require Zed highlights to retain canonical directions and accepted ALTER captures."""
    highlights_path = zed_directory / "languages" / "orna" / "highlights.scm"
    try:
        highlights = highlights_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        log(
            f"could not read Zed highlights {display_path(highlights_path, repository)}: {exc}",
            error=True,
        )
        return False

    canonical_highlights_path = repository / "editors" / "tree-sitter-orna" / "queries" / "highlights.scm"
    try:
        canonical_highlights = canonical_highlights_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        log(
            f"could not read canonical tree-sitter highlights {display_path(canonical_highlights_path, repository)}: {exc}",
            error=True,
        )
        return False

    required_direction_captures = ("(kw_asc)", "(kw_desc)")
    missing_canonical = [
        capture for capture in required_direction_captures if capture not in canonical_highlights
    ]
    if missing_canonical:
        log(
            "canonical tree-sitter highlights are missing accepted ORDER BY direction captures: "
            + ", ".join(repr(capture) for capture in missing_canonical),
            error=True,
        )
        return False
    missing_direction_captures = [
        capture for capture in required_direction_captures if capture not in highlights
    ]
    if missing_direction_captures:
        log(
            "Zed highlights are missing accepted ORDER BY direction captures: "
            + ", ".join(repr(capture) for capture in missing_direction_captures),
            error=True,
        )
        return False

    required_captures = (
        "(alter_type_statement",
        "old_name: (_) @property",
        "new_name: (_) @property",
    )
    missing = [capture for capture in required_captures if capture not in highlights]
    if missing:
        log(
            "Zed highlights are missing accepted ALTER captures: "
            + ", ".join(repr(capture) for capture in missing),
            error=True,
        )
        return False

    log(f"validated Zed highlights {display_path(highlights_path, repository)}")
    return True


def check_zed_extension(
    cargo: str,
    zed_directory: Path,
    repository: Path,
) -> bool:
    """Compile the separate Zed extension workspace."""
    manifest = zed_directory / "Cargo.toml"
    if not manifest.is_file():
        log(
            f"required file is missing: {display_path(manifest, repository)}",
            error=True,
        )
        return False

    log("checking editors/zed with cargo check")
    result = run_command(
        [cargo, "check", "--locked", "--manifest-path", str(manifest)],
        cwd=repository,
        label="zed cargo check",
    )
    if result is None or result.returncode != 0:
        status = "could not start" if result is None else f"exited with status {result.returncode}"
        log(f"Zed extension check failed ({status})", error=True)
        return False
    log("Zed extension check passed")
    return True

def check_lsp_protocol(cargo: str, repository: Path) -> bool:
    """Run the framed LSP protocol test against the checked-in binary."""
    log("checking orna-lsp framed protocol tests")
    result = run_command(
        [cargo, "test", "--package", "orna-lsp", "--test", "lsp_e2e"],
        cwd=repository,
        label="orna-lsp protocol test",
    )
    if result is None or result.returncode != 0:
        status = "could not start" if result is None else f"exited with status {result.returncode}"
        log(f"orna-lsp protocol check failed ({status})", error=True)
        return False
    log("orna-lsp protocol check passed")
    return True



def main() -> int:
    repository = Path(__file__).resolve().parents[1]
    tree_sitter_directory = repository / "editors" / "tree-sitter-orna"
    zed_directory = repository / "editors" / "zed"
    spec_examples = repository.parent / "spec" / "examples"
    helix_configuration = repository / "editors" / "helix" / "languages.toml"
    emacs_integration = repository / "editors" / "emacs" / "orna-eglot.el"
    vim_syntax = repository / "editors" / "vim" / "syntax" / "orna.vim"
    neovim_directory = repository / "editors" / "neovim"
    vim_ftdetect = repository / "editors" / "vim" / "ftdetect" / "orna.vim"
    vscode_package = repository / "editors" / "vscode" / "package.json"
    vscode_extension = repository / "editors" / "vscode" / "extension.js"
    zed_extension_metadata = zed_directory / "extension.toml"
    zed_language_configuration = zed_directory / "languages" / "orna" / "config.toml"
    zed_language_server_configuration = (
        zed_directory / "languages" / "orna" / "language_servers" / "orna_lsp" / "config.toml"
    )


    log(
        "static editor tooling gate does not install editor runtimes; requires CLI prerequisites "
        "Python 3.11+, tree-sitter CLI, node, and cargo"
    )
    tree_sitter = shutil.which("tree-sitter")
    if tree_sitter is None:
        log(
            "missing prerequisite: tree-sitter CLI was not found on PATH; "
            "install tree-sitter-cli before running this gate",
            error=True,
        )
        return 2

    node = shutil.which("node")
    if node is None:
        log("missing prerequisite: node was not found on PATH", error=True)
        return 2
    if tomllib is None:
        log("missing prerequisite: Python 3.11+ is required for Helix and Zed TOML validation", error=True)
        return 2

    cargo = shutil.which("cargo")
    if cargo is None:
        log("missing prerequisite: cargo was not found on PATH", error=True)
        return 2
    emacs = shutil.which("emacs")


    if not tree_sitter_directory.is_dir():
        log(
            f"required directory is missing: {display_path(tree_sitter_directory, repository)}",
            error=True,
        )
        return 1

    if not check_tree_sitter_package(tree_sitter_directory, repository):
        return 1
    if not check_tree_sitter_metadata(tree_sitter_directory, repository):
        return 1
    if not zed_directory.is_dir():
        log(
            f"required directory is missing: {display_path(zed_directory, repository)}",
            error=True,
        )
        return 1
    if not check_helix_configuration(helix_configuration, repository):
        return 1
    emacs_check = check_emacs_integration(emacs, emacs_integration, repository)
    if emacs_check is False:
        return 1
    if not check_neovim_integration(neovim_directory, repository):
        return 1
    if not check_vim_filetype_detection(vim_ftdetect, repository):
        return 1
    if not check_vscode_package(vscode_package, repository):
        return 1
    if not check_vscode_extension(vscode_extension, repository):
        return 1


    if not check_zed_extension_metadata(zed_extension_metadata, repository):
        return 1
    if not check_zed_language_configuration(
        zed_language_configuration,
        zed_language_server_configuration,
        repository,
    ):
        return 1
    if not check_zed_highlights(zed_directory, repository):
        return 1
    if not check_zed_order_by_highlights(
        tree_sitter, zed_directory, tree_sitter_directory, repository
    ):
        return 1
    if not check_target_path_highlights(
        tree_sitter, zed_directory, tree_sitter_directory, repository
    ):
        return 1
    if not check_zed_extension(cargo, zed_directory, repository):
        return 1
    if not check_lsp_protocol(cargo, repository):
        return 1


    if not check_generated_artefacts(tree_sitter, tree_sitter_directory, repository):
        return 1

    required_fixture_roots = (
        repository / "stdlib",
        repository / "crates",
        tree_sitter_directory / "test" / "corpus",
    )
    for directory in required_fixture_roots:
        if not directory.is_dir():
            log(
                f"required fixture directory is missing: {display_path(directory, repository)}",
                error=True,
            )
            return 1

    accepted_manifest_path = tree_sitter_directory / ACCEPTED_CORPUS_MANIFEST_NAME
    deferred_manifest_path = tree_sitter_directory / DEFERRED_CORPUS_MANIFEST_NAME
    corpus_directory = tree_sitter_directory / "test" / "corpus"
    manifest_result = check_corpus_manifests(
        accepted_manifest_path,
        deferred_manifest_path,
        corpus_directory,
        repository,
    )
    if manifest_result is None:
        return 1
    accepted_case_names, deferred_case_names, corpus_case_count = manifest_result
    accepted_regex = "^(?:" + "|".join(re.escape(name) for name in accepted_case_names) + ")$"
    log(f"running tree-sitter accepted corpus ({len(accepted_case_names)} cases)")
    corpus_result = run_command(
        [tree_sitter, "test", "--include", accepted_regex, "--json-summary"],
        cwd=tree_sitter_directory,
        label="tree-sitter accepted corpus",
    )
    if corpus_result is None or corpus_result.returncode != 0:
        status = (
            "could not start"
            if corpus_result is None
            else f"exited with status {corpus_result.returncode}"
        )
        log(f"tree-sitter accepted corpus failed ({status})", error=True)
        return 1
    try:
        corpus_summary = json.loads(corpus_result.stdout)
    except json.JSONDecodeError as exc:
        log(f"accepted corpus test returned invalid JSON: {exc}", error=True)
        return 1
    if not check_accepted_corpus_results(corpus_summary, accepted_case_names):
        return 1
    log(f"accepted corpus evidence passed: {len(accepted_case_names)} cases")
    if not check_alter_rename_highlights(
        tree_sitter, tree_sitter_directory, repository
    ):
        return 1
    remaining_corpus_cases = len(deferred_case_names)
    log(
        f"remaining corpus cases: {remaining_corpus_cases} proposal/deferred grammar coverage; "
        "not accepted-contract evidence"
    )

    parse_paths: list[Path] = []
    if spec_examples.is_dir():
        canonical_paths = sorted_orna_files(spec_examples)
        log(
            f"canonical spec examples: {len(canonical_paths)} proposal/deferred .orna files "
            "excluded from accepted-contract parse evidence"
        )
        for path in canonical_paths:
            log(
                f"excluding canonical proposal/deferred example from accepted-contract parse: "
                f"{display_path(path, repository)}"
            )
    else:
        log("canonical spec examples: ../spec/examples is absent; skipping")

    for directory in required_fixture_roots:
        fixture_paths = sorted_orna_files(directory)
        log(
            f"fixtures under {display_path(directory, repository)}: "
            f"{len(fixture_paths)} .orna files"
        )
        parse_paths.extend(fixture_paths)

    parse_paths.sort(key=lambda path: path.as_posix())
    if parse_paths:
        for path in parse_paths:
            log(f"parsing {display_path(path, repository)}")
        parse_arguments = [os.path.relpath(path, tree_sitter_directory) for path in parse_paths]
        parse_result = run_command(
            [tree_sitter, "parse", "--quiet", *parse_arguments],
            cwd=tree_sitter_directory,
            label="tree-sitter parse",
        )
        parser_error_node = parse_result is not None and (
            "(ERROR" in parse_result.stdout or "(ERROR" in parse_result.stderr
        )
        if parse_result is None or parse_result.returncode != 0 or parser_error_node:
            status = (
                "could not start"
                if parse_result is None
                else f"exited with status {parse_result.returncode}"
            )
            detail = "; parser error nodes detected" if parser_error_node else ""
            log(f"tree-sitter parse failed ({status}{detail})", error=True)
            return 1
    log(f"parsed {len(parse_paths)} .orna files without parser errors")

    json_files = checked_in_editor_json_files(repository)
    if json_files is None:
        return 1
    textmate_grammars = (
        repository / "editors" / "textmate" / "orna.tmLanguage.json",
        repository / "editors" / "vscode" / "syntaxes" / "orna.tmLanguage.json",
    )
    for path in json_files:
        relative_path = display_path(path, repository)
        log(f"validating JSON {relative_path}")
        try:
            with path.open(encoding="utf-8") as stream:
                json.load(stream)
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            log(f"invalid JSON in {relative_path}: {exc}", error=True)
            return 1
    log(f"validated {len(json_files)} editor JSON files")
    if not check_textmate_grammar_parity(*textmate_grammars, repository):
        return 1

    for grammar_path in textmate_grammars:
        if not check_textmate_grammar(grammar_path, repository):
            return 1
    if not check_fallback_keyword_parity(
        tree_sitter_directory / "grammar.js",
        textmate_grammars,
        vim_syntax,
        emacs_integration,
        repository,
    ):
        return 1
    if not check_unicode_identifier_parity(
        tree_sitter_directory / "grammar.js",
        textmate_grammars,
        vim_syntax,
        repository,
    ):
        return 1

    extension = repository / "editors" / "vscode" / "extension.js"
    if not extension.is_file():
        log(f"required file is missing: {display_path(extension, repository)}", error=True)
        return 1
    log("checking editors/vscode/extension.js with node --check")
    node_result = run_command(
        [node, "--check", str(extension)],
        cwd=repository,
        label="node --check",
    )
    if node_result is None or node_result.returncode != 0:
        status = (
            "could not start"
            if node_result is None
            else f"exited with status {node_result.returncode}"
        )
        log(f"node --check failed ({status})", error=True)
        return 1
    log("node --check passed")
    if emacs_check is None:
        log("editor tooling gate completed; optional Emacs runtime check was unavailable")
    else:
        log("editor tooling gate passed")
    log(
        "Neovim/Vim runtime checks were not exercised by this static gate and are unavailable "
        "when the nvim/vim binaries are absent; static integration contracts were validated"
    )
    log("Zed GUI/VSIX runtime launch was not exercised by this static gate")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
