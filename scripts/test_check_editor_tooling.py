#!/usr/bin/env python3
"""Focused tests for the static Zed extension metadata contract."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


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


if __name__ == "__main__":
    unittest.main()
