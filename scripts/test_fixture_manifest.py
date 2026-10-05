from __future__ import annotations

import hashlib
from pathlib import Path
import re
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

    def test_sys_binding_parity_fixtures_are_local_and_hash_pinned(self) -> None:
        workspace = Path(__file__).resolve().parents[1]
        manifest = (workspace / "scripts/fixture-manifest.sha256").read_text(
            encoding="utf-8"
        )
        include_pattern = re.compile(r'include_str!\(\s*"fixtures/([^"\n]+)"\s*\)')
        parity_suites = (
            "sys_api_generation.rs",
            "system_binding_stubs.rs",
            "system_provider_abi.rs",
        )
        fixture_count = 0

        for suite in parity_suites:
            test_path = f"crates/orna-sys-v1/tests/{suite}"
            source = (workspace / test_path).read_text(encoding="utf-8")
            fixture_names = include_pattern.findall(source)
            self.assertTrue(fixture_names, f"{test_path} includes crate-local fixtures")

            for fixture_name in fixture_names:
                fixture_count += 1
                self.assertNotIn("..", Path(fixture_name).parts)
                fixture_path = f"crates/orna-sys-v1/tests/fixtures/{fixture_name}"
                fixture = workspace / fixture_path
                self.assertTrue(fixture.is_file(), f"missing crate-local fixture: {fixture_path}")
                digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
                self.assertIn(
                    f"{digest}  {fixture_path}\n",
                    manifest,
                    f"sys binding fixture hash is pinned: {fixture_path}",
                )

        self.assertEqual(fixture_count, 18, "all parity fixture includes are hash-pinned")

    def test_standard_snapshot_replay_fixtures_are_local_and_hash_pinned(self) -> None:
        workspace = Path(__file__).resolve().parents[1]
        manifest = (workspace / "scripts/fixture-manifest.sha256").read_text(
            encoding="utf-8"
        )
        test_path = "crates/orna-evaluator-v1/tests/standard_snapshot_replay.rs"
        source = (workspace / test_path).read_text(encoding="utf-8")
        include_pattern = re.compile(r'include_str!\(\s*"([^"]+)"\s*\)')
        include_paths = include_pattern.findall(source)
        self.assertTrue(include_paths, f"{test_path} includes replay fixtures")

        fixture_names = set()
        for include_path in include_paths:
            self.assertTrue(
                include_path.startswith("fixtures/"),
                f"snapshot replay inputs stay inside the owning crate: {include_path}",
            )
            relative_path = Path(include_path)
            self.assertNotIn("..", relative_path.parts)
            fixture_names.add(relative_path.relative_to("fixtures").as_posix())

        self.assertTrue(
            {
                "snapshot-replay-std-main.orna",
                "snapshot-replay-std-v1.orna",
                "snapshot-replay-std-v2.orna",
                "snapshot-matrix-std-main.orna",
            }.issubset(fixture_names),
            "the snapshot replay proofs retain their core std snapshot inputs",
        )
        for fixture_name in sorted(fixture_names):
            fixture_path = f"crates/orna-evaluator-v1/tests/fixtures/{fixture_name}"
            fixture = workspace / fixture_path
            self.assertTrue(fixture.is_file(), f"missing crate-local fixture: {fixture_path}")
            digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
            self.assertIn(
                f"{digest}  {fixture_path}\n",
                manifest,
                f"std snapshot replay fixture hash is pinned: {fixture_path}",
            )

    def test_evaluator_hash_and_random_snapshot_fixtures_are_local_and_hash_pinned(self) -> None:
        workspace = Path(__file__).resolve().parents[1]
        manifest = (workspace / "scripts/fixture-manifest.sha256").read_text(
            encoding="utf-8"
        )
        include_pattern = re.compile(r'include_str!\(\s*"([^"\n]+)"\s*\)')
        suites = {
            "standard_library_hash_snapshot_27d5k.rs": 5,
            "standard_library_random_snapshot_27d5k.rs": 8,
        }
        fixture_names = set()

        for suite, expected_include_count in suites.items():
            test_path = f"crates/orna-evaluator-v1/tests/{suite}"
            source = (workspace / test_path).read_text(encoding="utf-8")
            include_paths = include_pattern.findall(source)
            self.assertEqual(
                len(include_paths),
                expected_include_count,
                f"{test_path} retains all expected snapshot behavior inputs",
            )

            for include_path in include_paths:
                self.assertTrue(
                    include_path.startswith("fixtures/"),
                    f"snapshot inputs stay inside the owning crate: {include_path}",
                )
                relative_path = Path(include_path)
                self.assertNotIn("..", relative_path.parts)
                fixture_names.add(relative_path.relative_to("fixtures").as_posix())

        self.assertEqual(
            len(fixture_names),
            12,
            "hash and random snapshot proofs retain their full unique fixture set",
        )
        for fixture_name in sorted(fixture_names):
            fixture_path = f"crates/orna-evaluator-v1/tests/fixtures/{fixture_name}"
            fixture = workspace / fixture_path
            self.assertTrue(fixture.is_file(), f"missing crate-local fixture: {fixture_path}")
            digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
            self.assertIn(
                f"{digest}  {fixture_path}\n",
                manifest,
                f"evaluator snapshot fixture hash is pinned: {fixture_path}",
            )

    def test_runtime_historical_snapshot_fixtures_are_local_and_hash_pinned(self) -> None:
        workspace = Path(__file__).resolve().parents[1]
        manifest = (workspace / "scripts/fixture-manifest.sha256").read_text(
            encoding="utf-8"
        )
        test_path = "crates/orna-runtime-v1/tests/historical_snapshots.rs"
        source = (workspace / test_path).read_text(encoding="utf-8")
        include_pattern = re.compile(r'include_str!\(\s*"([^"\n]+)"\s*\)')
        include_paths = include_pattern.findall(source)
        self.assertEqual(
            len(include_paths),
            185,
            f"{test_path} retains all historical snapshot behavior inputs",
        )

        fixture_names = set()
        for include_path in include_paths:
            self.assertTrue(
                include_path.startswith("fixtures/"),
                f"historical snapshot inputs stay in the owning crate: {include_path}",
            )
            relative_path = Path(include_path)
            self.assertNotIn("..", relative_path.parts)
            fixture_names.add(relative_path.relative_to("fixtures").as_posix())

        self.assertEqual(
            len(fixture_names),
            39,
            "historical snapshot behavior keeps its full unique fixture set",
        )
        for fixture_name in sorted(fixture_names):
            fixture_path = f"crates/orna-runtime-v1/tests/fixtures/{fixture_name}"
            fixture = workspace / fixture_path
            self.assertTrue(fixture.is_file(), f"missing crate-local fixture: {fixture_path}")
            digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
            self.assertIn(
                f"{digest}  {fixture_path}\n",
                manifest,
                f"runtime historical snapshot fixture hash is pinned: {fixture_path}",
            )


if __name__ == "__main__":
    unittest.main()
