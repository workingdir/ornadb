from __future__ import annotations

import hashlib
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from scripts.check_fixture_manifest import fixture_hashes, render_manifest, validate_manifest


class FixtureManifestTests(unittest.TestCase):
    def test_accepts_a_sorted_manifest_with_the_pinned_sha256(self) -> None:
        path = "crates/example/tests/fixtures/empty.orna"
        empty_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        manifest = render_manifest({path: empty_sha256})

        self.assertEqual(validate_manifest({path: empty_sha256}, manifest), [])

    def test_rejects_fixture_content_changes(self) -> None:
        path = "crates/example/tests/fixtures/input.orna"
        pinned = hashlib.sha256(b"original input\n").hexdigest()
        changed = hashlib.sha256(b"changed input\n").hexdigest()
        errors = validate_manifest({path: changed}, render_manifest({path: pinned}))

        self.assertEqual(len(errors), 1)
        self.assertIn("fixture content hash changed", errors[0])
        self.assertIn(path, errors[0])

    def test_rejects_added_and_removed_fixture_paths(self) -> None:
        pinned_path = "crates/example/tests/fixtures/pinned.orna"
        removed_path = "crates/example/tests/fixtures/removed.orna"
        added_path = "crates/example/tests/fixtures/added.orna"
        pinned = hashlib.sha256(b"pinned\n").hexdigest()
        removed = hashlib.sha256(b"removed\n").hexdigest()
        added = hashlib.sha256(b"added\n").hexdigest()
        manifest = render_manifest({pinned_path: pinned, removed_path: removed})

        errors = validate_manifest({pinned_path: pinned, added_path: added}, manifest)

        self.assertEqual(
            errors,
            [
                f"fixture is missing from manifest: {added_path}",
                f"manifest fixture is missing from checkout: {removed_path}",
            ],
        )

    def test_rejects_paths_that_escape_crate_fixture_trees(self) -> None:
        manifest = "a" * 64 + "  crates/example/tests/fixtures/../../outside.orna\n"

        errors = validate_manifest({}, manifest)

        self.assertEqual(len(errors), 1)
        self.assertIn("is not a crate fixture path", errors[0])

    def test_scans_nested_crate_fixture_files_and_hashes_their_bytes(self) -> None:
        with TemporaryDirectory() as temporary_directory:
            workspace = Path(temporary_directory)
            fixture = workspace / "crates/example/src/tests/fixtures/nested/input.json"
            fixture.parent.mkdir(parents=True)
            fixture.write_bytes(b'{"fixture":true}\n')

            hashes, errors = fixture_hashes(workspace)

        self.assertEqual(errors, [])
        self.assertEqual(
            hashes,
            {
                "crates/example/src/tests/fixtures/nested/input.json": hashlib.sha256(
                    b'{"fixture":true}\n'
                ).hexdigest()
            },
        )

    def test_editor_attachment_input_is_local_and_hash_pinned(self) -> None:
        workspace = Path(__file__).resolve().parents[1]
        fixture_path = "crates/orna-lsp/tests/fixtures/ji3t0-lsp-v1.orna"
        attachment_test = workspace / "crates/orna-lsp/tests/editor_attachment_completion.rs"
        fixture = workspace / fixture_path
        source = attachment_test.read_text(encoding="utf-8")
        digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
        pinned_line = f"{digest}  {fixture_path}\n"

        self.assertIn('include_str!("fixtures/ji3t0-lsp-v1.orna")', source)
        self.assertIn(
            pinned_line,
            (workspace / "scripts/fixture-manifest.sha256").read_text(encoding="utf-8"),
        )


if __name__ == "__main__":
    unittest.main()
