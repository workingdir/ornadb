#!/usr/bin/env python3
"""Focused tests for static editor metadata contracts."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess

import sys
import tempfile
import unittest
from unittest import mock



REPOSITORY = Path(__file__).resolve().parents[1]
SCRIPT_MODULE = Path(__file__).with_name("check-editor-tooling.py")
spec = importlib.util.spec_from_file_location("orna_check_editor_tooling", SCRIPT_MODULE)
if spec is None or spec.loader is None:
    raise RuntimeError("could not load editor tooling checker")
checker = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = checker
spec.loader.exec_module(checker)


class ZedExtensionMetadataTests(unittest.TestCase):
    extension_path = REPOSITORY / "editors" / "zed" / "extension.toml"

    def test_checked_in_metadata_is_valid(self) -> None:
        self.assertTrue(
            checker.check_zed_extension_metadata(self.extension_path, REPOSITORY),
            "checked-in Zed extension metadata was rejected",
        )

    def test_rejects_missing_or_malformed_registration_fields(self) -> None:
        metadata = self.extension_path.read_text(encoding="utf-8")
        accepted_revision = checker.ACCEPTED_ZED_GRAMMAR_REVISION
        cases = (
            ("missing id", 'id = "orna"\n', ""),
            ("malformed id", 'id = "orna"', "id = 7"),
            ("wrong id", 'id = "orna"', 'id = "other"'),
            ("missing schema_version", "schema_version = 1\n", ""),
            ("malformed schema_version", "schema_version = 1", 'schema_version = "1"'),
            ("wrong schema_version", "schema_version = 1", "schema_version = 2"),
            (
                "missing languages",
                'languages = ["Orna"]\n',
                "",
            ),
            (
                "malformed languages",
                'languages = ["Orna"]',
                'languages = "Orna"',
            ),
            (
                "wrong languages",
                'languages = ["Orna"]',
                'languages = ["Other"]',
            ),
            (
                "missing language_ids",
                'language_ids = { "Orna" = "orna" }\n',
                "",
            ),
            (
                "malformed language_ids",
                'language_ids = { "Orna" = "orna" }',
                'language_ids = ["orna"]',
            ),
            (
                "wrong language_ids value",
                'language_ids = { "Orna" = "orna" }',
                'language_ids = { "Orna" = "other" }',
            ),
            (
                "extra language_ids entry",
                'language_ids = { "Orna" = "orna" }',
                'language_ids = { "Orna" = "orna", "Other" = "other" }',
            ),
            (
                "missing language server table",
                '[language_servers.orna-lsp]\nname = "Orna Language Server"\nlanguages = ["Orna"]\nlanguage_ids = { "Orna" = "orna" }\n',
                "",
            ),
            (
                "missing grammar table",
                f'[grammars.orna]\nrepository = "https://github.com/workingdir/ornadb"\nrev = "{accepted_revision}"\npath = "editors/tree-sitter-orna"\n',
                "",
            ),
            (
                "missing grammar repository",
                '[grammars.orna]\nrepository = "https://github.com/workingdir/ornadb"\n',
                "[grammars.orna]\n",
            ),
            (
                "malformed grammar repository",
                '[grammars.orna]\nrepository = "https://github.com/workingdir/ornadb"',
                '[grammars.orna]\nrepository = ["https://github.com/workingdir/ornadb"]',
            ),
            (
                "wrong grammar repository",
                '[grammars.orna]\nrepository = "https://github.com/workingdir/ornadb"',
                '[grammars.orna]\nrepository = "https://github.com/workingdir/other"',
            ),
            (
                "missing grammar path",
                'path = "editors/tree-sitter-orna"\n',
                "",
            ),
            (
                "malformed grammar path",
                'path = "editors/tree-sitter-orna"',
                'path = ["editors/tree-sitter-orna"]',
            ),
            (
                "wrong grammar path",
                'path = "editors/tree-sitter-orna"',
                'path = "editors/tree-sitter-other"',
            ),
            ("missing grammar revision", f'rev = "{accepted_revision}"\n', ""),
            (
                "malformed grammar revision",
                f'rev = "{accepted_revision}"',
                'rev = "not-a-revision"',
            ),
            (
                "wrong exact grammar revision",
                f'rev = "{accepted_revision}"',
                'rev = "ffffffffffffffffffffffffffffffffffffffff"',
            ),
        )

        with tempfile.TemporaryDirectory(prefix="orna-zed-metadata-tests-") as scratch_name:
            candidate = Path(scratch_name) / "extension.toml"
            for label, original, replacement in cases:
                with self.subTest(case=label):
                    self.assertEqual(
                        metadata.count(original),
                        1,
                        f"test fixture replacement for {label!r} was ambiguous",
                    )
                    candidate.write_text(metadata.replace(original, replacement, 1), encoding="utf-8")
                    self.assertFalse(
                        checker.check_zed_extension_metadata(candidate, REPOSITORY),
                        f"validator accepted {label}",
                    )


class ZedLanguageConfigurationTests(unittest.TestCase):
    language_path = REPOSITORY / "editors" / "zed" / "languages" / "orna" / "config.toml"
    language_server_path = (
        REPOSITORY
        / "editors"
        / "zed"
        / "languages"
        / "orna"
        / "language_servers"
        / "orna_lsp"
        / "config.toml"
    )

    def _copy_fixture(self, scratch: Path) -> tuple[Path, Path]:
        language_candidate = scratch / "language.toml"
        language_server_candidate = scratch / "language-server.toml"
        shutil.copyfile(self.language_path, language_candidate)
        shutil.copyfile(self.language_server_path, language_server_candidate)
        return language_candidate, language_server_candidate

    def _check(self, language_path: Path, language_server_path: Path) -> tuple[bool, str]:
        errors = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(errors):
            accepted = checker.check_zed_language_configuration(
                language_path,
                language_server_path,
                REPOSITORY,
            )
        return accepted, errors.getvalue()

    def test_checked_in_metadata_is_valid(self) -> None:
        accepted, diagnostics = self._check(self.language_path, self.language_server_path)
        self.assertTrue(accepted, f"checked-in Zed language metadata was rejected: {diagnostics}")

    def test_rejects_language_field_mutations(self) -> None:
        metadata = self.language_path.read_text(encoding="utf-8")
        cases = (
            ("missing name", 'name = "Orna"\n', "", "key 'name'"),
            ("wrong grammar", 'grammar = "orna"', 'grammar = "other"', "key 'grammar'"),
            (
                "malformed path suffixes",
                'path_suffixes = ["orna"]',
                'path_suffixes = "orna"',
                "key 'path_suffixes'",
            ),
            (
                "wrong line comments",
                'line_comments = ["--"]',
                'line_comments = ["//"]',
                "key 'line_comments'",
            ),
            (
                "malformed block comment",
                'block_comment = ["/*", "*/"]',
                'block_comment = "/*"',
                "key 'block_comment'",
            ),
            (
                "wrong brackets",
                '    { start = "(", end = ")", close = true, newline = false },',
                '    { start = "(", end = ")", close = false, newline = false },',
                "key 'brackets'",
            ),
            (
                "integer bracket close",
                '    { start = "(", end = ")", close = true, newline = false },',
                '    { start = "(", end = ")", close = 1, newline = false },',
                "key 'brackets'",
            ),
            (
                "integer bracket newline",
                '    { start = "(", end = ")", close = true, newline = false },',
                '    { start = "(", end = ")", close = true, newline = 0 },',
                "key 'brackets'",
            ),
        )

        with tempfile.TemporaryDirectory(prefix="orna-zed-language-tests-") as scratch_name:
            language_candidate, language_server_candidate = self._copy_fixture(Path(scratch_name))
            for label, original, replacement, diagnostic in cases:
                with self.subTest(case=label):
                    self.assertEqual(
                        metadata.count(original),
                        1,
                        f"test fixture replacement for {label!r} was ambiguous",
                    )
                    language_candidate.write_text(
                        metadata.replace(original, replacement, 1),
                        encoding="utf-8",
                    )
                    accepted, diagnostics = self._check(language_candidate, language_server_candidate)
                    self.assertFalse(accepted, f"validator accepted {label}")
                    self.assertIn(diagnostic, diagnostics)

    def test_rejects_language_server_field_mutations(self) -> None:
        metadata = self.language_server_path.read_text(encoding="utf-8")
        cases = (
            ("missing name", 'name = "orna-lsp"\n', "", "key 'name'"),
            (
                "wrong language",
                'language = "Orna"',
                'language = "Other"',
                "key 'language'",
            ),
            (
                "malformed command",
                'command = "orna-lsp"',
                'command = ["orna-lsp"]',
                "key 'command'",
            ),
            ("wrong args", "args = []", 'args = ["--stdio"]', "key 'args'"),
        )

        with tempfile.TemporaryDirectory(prefix="orna-zed-language-server-tests-") as scratch_name:
            language_candidate, language_server_candidate = self._copy_fixture(Path(scratch_name))
            for label, original, replacement, diagnostic in cases:
                with self.subTest(case=label):
                    self.assertEqual(
                        metadata.count(original),
                        1,
                        f"test fixture replacement for {label!r} was ambiguous",
                    )
                    language_server_candidate.write_text(
                        metadata.replace(original, replacement, 1),
                        encoding="utf-8",
                    )
                    accepted, diagnostics = self._check(language_candidate, language_server_candidate)
                    self.assertFalse(accepted, f"validator accepted {label}")
                    self.assertIn(diagnostic, diagnostics)

    def test_rejects_missing_configuration_files(self) -> None:
        with tempfile.TemporaryDirectory(prefix="orna-zed-missing-config-tests-") as scratch_name:
            language_candidate, language_server_candidate = self._copy_fixture(Path(scratch_name))
            language_candidate.unlink()
            accepted, diagnostics = self._check(language_candidate, language_server_candidate)
            self.assertFalse(accepted, "validator accepted a missing language configuration")
            self.assertIn("required Zed language configuration is missing", diagnostics)

            shutil.copyfile(self.language_path, language_candidate)
            language_server_candidate.unlink()
            accepted, diagnostics = self._check(language_candidate, language_server_candidate)
            self.assertFalse(accepted, "validator accepted a missing language-server configuration")
            self.assertIn("required Zed language-server configuration is missing", diagnostics)

    def test_rejects_malformed_toml(self) -> None:
        with tempfile.TemporaryDirectory(prefix="orna-zed-malformed-config-tests-") as scratch_name:
            language_candidate, language_server_candidate = self._copy_fixture(Path(scratch_name))
            language_candidate.write_text('name = "Orna"\npath_suffixes = [\n', encoding="utf-8")
            accepted, diagnostics = self._check(language_candidate, language_server_candidate)
            self.assertFalse(accepted, "validator accepted malformed language TOML")
            self.assertIn("invalid zed language configuration", diagnostics.lower())

            shutil.copyfile(self.language_path, language_candidate)
            language_server_candidate.write_text(
                'name = "orna-lsp"\nlanguage = "Orna"\ncommand = "orna-lsp\nargs = []\n',
                encoding="utf-8",
            )
            accepted, diagnostics = self._check(language_candidate, language_server_candidate)
            self.assertFalse(accepted, "validator accepted malformed language-server TOML")
            self.assertIn("invalid zed language-server configuration", diagnostics.lower())


class TreeSitterMetadataTests(unittest.TestCase):
    tree_sitter_directory = REPOSITORY / "editors" / "tree-sitter-orna"
    metadata_path = tree_sitter_directory / "tree-sitter.json"
    package_path = tree_sitter_directory / "package.json"
    highlights_path = tree_sitter_directory / "queries" / "highlights.scm"

    def _copy_fixture(self, scratch: Path) -> Path:
        candidate = scratch / "tree-sitter-orna"
        candidate.mkdir()
        (candidate / "queries").mkdir()
        shutil.copyfile(self.metadata_path, candidate / "tree-sitter.json")
        shutil.copyfile(self.package_path, candidate / "package.json")
        shutil.copyfile(self.highlights_path, candidate / "queries" / "highlights.scm")
        return candidate

    def test_checked_in_metadata_is_valid(self) -> None:
        self.assertTrue(
            checker.check_tree_sitter_metadata(self.tree_sitter_directory, REPOSITORY),
            "checked-in Tree-sitter metadata was rejected",
        )

    def test_rejects_wrong_grammar_metadata_fields(self) -> None:
        metadata = json.loads(self.metadata_path.read_text(encoding="utf-8"))
        cases = (
            ("grammar name", "name", "other", "grammar 'name'"),
            ("scope", "scope", "source.other", "grammar 'scope'"),
            ("parser path", "path", "src", "grammar 'path'"),
            ("file types", "file-types", ["sql"], "grammar 'file-types'"),
            (
                "highlights query path",
                "highlights",
                "queries/other.scm",
                "grammar 'highlights'",
            ),
        )

        with tempfile.TemporaryDirectory(prefix="orna-tree-sitter-metadata-tests-") as scratch_name:
            candidate = self._copy_fixture(Path(scratch_name))
            for label, field, value, diagnostic in cases:
                with self.subTest(case=label):
                    candidate_metadata = json.loads(json.dumps(metadata))
                    candidate_metadata["grammars"][0][field] = value
                    (candidate / "tree-sitter.json").write_text(
                        json.dumps(candidate_metadata),
                        encoding="utf-8",
                    )
                    with contextlib.redirect_stderr(io.StringIO()) as errors:
                        accepted = checker.check_tree_sitter_metadata(candidate, REPOSITORY)
                    self.assertFalse(accepted, f"validator accepted {label}")
                    self.assertIn(diagnostic, errors.getvalue())

    def test_rejects_missing_highlights_query(self) -> None:
        with tempfile.TemporaryDirectory(prefix="orna-tree-sitter-metadata-tests-") as scratch_name:
            candidate = self._copy_fixture(Path(scratch_name))
            (candidate / "queries" / "highlights.scm").unlink()
            with contextlib.redirect_stderr(io.StringIO()) as errors:
                accepted = checker.check_tree_sitter_metadata(candidate, REPOSITORY)
            self.assertFalse(accepted, "validator accepted a missing highlights query")
            self.assertIn("tree-sitter highlights query is missing", errors.getvalue())

    def test_rejects_package_metadata_parity_drift(self) -> None:
        package = json.loads(self.package_path.read_text(encoding="utf-8"))
        cases = (
            ("package name", ("name",), "tree-sitter-other", "name must be"),
            (
                "package scope",
                ("tree-sitter", "scopes"),
                {"source.other": "other"},
                "tree-sitter.scopes must be",
            ),
            (
                "package file types",
                ("tree-sitter", "file-types"),
                ["sql"],
                "tree-sitter.file-types must be",
            ),
            (
                "package highlights query path",
                ("tree-sitter", "highlights"),
                ["queries/other.scm"],
                "tree-sitter.highlights must be",
            ),
        )

        with tempfile.TemporaryDirectory(prefix="orna-tree-sitter-metadata-tests-") as scratch_name:
            candidate = self._copy_fixture(Path(scratch_name))
            for label, field_path, value, diagnostic in cases:
                with self.subTest(case=label):
                    candidate_package = json.loads(json.dumps(package))
                    target = candidate_package
                    for key in field_path[:-1]:
                        target = target[key]
                    target[field_path[-1]] = value
                    (candidate / "package.json").write_text(
                        json.dumps(candidate_package),
                        encoding="utf-8",
                    )
                    with contextlib.redirect_stderr(io.StringIO()) as errors:
                        accepted = checker.check_tree_sitter_metadata(candidate, REPOSITORY)
                    self.assertFalse(accepted, f"validator accepted {label}")
                    self.assertIn(diagnostic, errors.getvalue())



class SourceCheckParityTests(unittest.TestCase):
    def _fixtures(self) -> list[checker.CorpusSourceFixture]:
        return [
            checker.CorpusSourceFixture(
                name=name,
                source="x" * 64,
                expected_rejection=False,
                path=REPOSITORY / "test-fixture.orna",
            )
            for name in checker.SOURCE_CHECK_CORPUS_CASE_NAMES
        ]

    def _run_parity(self, *, returncode: int, output: str) -> bool:
        result = subprocess.CompletedProcess(
            args=["cargo"],
            returncode=returncode,
            stdout=output,
            stderr="",
        )
        with (
            mock.patch.object(checker, "run_command", return_value=result),
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            return checker.check_source_check_parity("cargo", REPOSITORY, self._fixtures())

    def test_accepts_warning_only_success(self) -> None:
        self.assertTrue(
            self._run_parity(
                returncode=0,
                output="accepted.orna:10..20: ORNA0401: unreachable statement\n",
            )
        )

    def test_rejects_error_diagnostic_even_if_command_succeeds(self) -> None:
        self.assertFalse(
            self._run_parity(
                returncode=0,
                output="accepted.orna:10..20: ORNA0101: unknown schema app\n",
            )
        )

    def test_rejects_error_diagnostic_and_failed_command(self) -> None:
        self.assertFalse(
            self._run_parity(
                returncode=1,
                output="accepted.orna:10..20: ORNA0101: unknown schema app\n",
            )
        )


if __name__ == "__main__":
    unittest.main()
