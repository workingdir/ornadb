#!/usr/bin/env python3
"""Run the dependency-light validation gate for checked-in editor tooling."""

from __future__ import annotations

import filecmp
import json
import os
from pathlib import Path
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
PROPOSAL_ONLY_SPEC_EXAMPLES = frozenset(
    {
        # The canonical bundle is illustrative proposal material. Classify proposal-only
        # examples separately, while parsing each grammar-compatible one below.
        "01_people_tasks.orna",
        "02_server_functions.orna",
        "03_client_ui.orna",
        "04_studio_shell.orna",
        "05_security_admin.orna",
        "06_inspector.orna",
        "07_jsonrpc_gateway.orna",
        "08_mcp_gateway.orna",
        "09_presenters.orna",
        "10_launch_entries.orna",
    }
)

NON_PARSEABLE_SPEC_EXAMPLES = frozenset(
    {
        # These proposal examples use syntax outside the accepted tree-sitter grammar.
        "03_client_ui.orna",
        "04_studio_shell.orna",
        "05_security_admin.orna",
    }
)



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


def check_tree_sitter_package(tree_sitter_directory: Path, repository: Path) -> bool:
    """Validate the grammar's source-only npm package boundary."""
    package_path = tree_sitter_directory / "package.json"
    try:
        with package_path.open(encoding="utf-8") as stream:
            package = json.load(stream)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        log(f"invalid tree-sitter package metadata: {exc}", error=True)
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
        [cargo, "check", "--manifest-path", str(manifest)],
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
        log("missing prerequisite: Python 3.11+ is required for Helix TOML validation", error=True)
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

    log("running tree-sitter test in editors/tree-sitter-orna")
    corpus_result = run_command(
        [tree_sitter, "test"],
        cwd=tree_sitter_directory,
        label="tree-sitter test",
    )
    if corpus_result is None or corpus_result.returncode != 0:
        status = (
            "could not start"
            if corpus_result is None
            else f"exited with status {corpus_result.returncode}"
        )
        log(f"tree-sitter test failed ({status})", error=True)
        return 1
    log("tree-sitter test passed")

    parse_paths: list[Path] = []
    if spec_examples.is_dir():
        canonical_paths = sorted_orna_files(spec_examples)
        grammar_compatible_paths = [
            path
            for path in canonical_paths
            if path.relative_to(spec_examples).as_posix() not in NON_PARSEABLE_SPEC_EXAMPLES
        ]
        for path in canonical_paths:
            relative_path = path.relative_to(spec_examples).as_posix()
            if relative_path in NON_PARSEABLE_SPEC_EXAMPLES:
                log(
                    f"skipping non-parseable proposal example: {display_path(path, repository)}"
                )
            elif relative_path in PROPOSAL_ONLY_SPEC_EXAMPLES:
                log(
                    f"including proposal-only example in grammar-compatible tree-sitter parse: "
                    f"{display_path(path, repository)}"
                )
        log(f"canonical examples: {len(grammar_compatible_paths)} grammar-compatible .orna files")
        # Tree-sitter coverage includes proposal examples that use accepted syntax,
        # but does not claim coverage for proposal syntax outside the grammar.
        parse_paths.extend(grammar_compatible_paths)
    else:
        log("canonical examples: ../spec/examples is absent; skipping")

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
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
