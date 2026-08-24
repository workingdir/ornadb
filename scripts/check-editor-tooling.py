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
CORPUS_CASE_DELIMITER = "=" * 20



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
    for corpus_path in sorted(corpus_directory.glob("*.txt"), key=lambda path: path.as_posix()):
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
        log("accepted corpus check found no corpus cases", error=True)
        return None
    return case_names


def check_accepted_corpus_manifest(
    manifest_path: Path,
    corpus_directory: Path,
    repository: Path,
) -> tuple[list[str], int] | None:
    """Validate the accepted corpus manifest and return names plus total corpus count."""
    try:
        manifest_lines = manifest_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as exc:
        log(
            f"could not read accepted corpus manifest {display_path(manifest_path, repository)}: {exc}",
            error=True,
        )
        return None

    if not manifest_lines:
        log(
            f"accepted corpus manifest is empty: {display_path(manifest_path, repository)}",
            error=True,
        )
        return None

    accepted_names: list[str] = []
    seen: set[str] = set()
    for line_number, name in enumerate(manifest_lines, start=1):
        if not name or name != name.strip():
            log(
                f"malformed accepted corpus manifest entry at line {line_number}: {name!r}",
                error=True,
            )
            return None
        if name in seen:
            log(f"duplicate accepted corpus manifest entry: {name!r}", error=True)
            return None
        seen.add(name)
        accepted_names.append(name)

    corpus_names = read_corpus_case_names(corpus_directory, repository)
    if corpus_names is None:
        return None
    missing = [name for name in accepted_names if name not in corpus_names]
    if missing:
        log(
            "accepted corpus manifest names are missing from the corpus: "
            + ", ".join(repr(name) for name in missing),
            error=True,
        )
        return None

    log(
        f"accepted corpus manifest validated: {len(accepted_names)} cases "
        "(accepted-contract evidence)"
    )
    return accepted_names, len(corpus_names)


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
    vim_syntax = repository / "editors" / "vim" / "syntax" / "orna.vim"


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

    manifest_path = tree_sitter_directory / ACCEPTED_CORPUS_MANIFEST_NAME
    manifest_result = check_accepted_corpus_manifest(
        manifest_path,
        tree_sitter_directory / "test" / "corpus",
        repository,
    )
    if manifest_result is None:
        return 1
    accepted_case_names, corpus_case_count = manifest_result
    accepted_regex = "^(?:" + "|".join(re.escape(name) for name in accepted_case_names) + ")$"
    log(f"running tree-sitter accepted corpus ({len(accepted_case_names)} cases)")
    corpus_result = run_command(
        [tree_sitter, "test", "--include", accepted_regex],
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
    log(f"accepted corpus evidence passed: {len(accepted_case_names)} cases")
    remaining_corpus_cases = corpus_case_count - len(accepted_case_names)
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

    textmate_grammars = (
        repository / "editors" / "textmate" / "orna.tmLanguage.json",
        repository / "editors" / "vscode" / "syntaxes" / "orna.tmLanguage.json",
    )
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
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
