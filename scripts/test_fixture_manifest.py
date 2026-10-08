from __future__ import annotations

import hashlib
from pathlib import Path
import re
from contextlib import redirect_stderr
from contextlib import redirect_stdout
from io import StringIO
import json
from tempfile import TemporaryDirectory
import unittest
from unittest import mock

from scripts import check_fixture_manifest
from scripts.check_fixture_manifest import (
    fixture_hashes,
    only_changed,
    scan_exit_status,
    render_manifest,
    validate_manifest,
)


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
        self.assertTrue(errors[0].startswith("scripts/fixture-manifest.sha256:3: "), errors[0])

    def test_rejects_added_and_removed_fixture_paths(self) -> None:
        pinned_path = "crates/example/tests/fixtures/pinned.orna"
        removed_path = "crates/example/tests/fixtures/removed.orna"
        added_path = "crates/example/tests/fixtures/added.orna"
        pinned = hashlib.sha256(b"pinned\n").hexdigest()
        removed = hashlib.sha256(b"removed\n").hexdigest()
        added = hashlib.sha256(b"added\n").hexdigest()
        manifest = render_manifest({pinned_path: pinned, removed_path: removed})

        errors = validate_manifest({pinned_path: pinned, added_path: added}, manifest)

        removed_line = next(
            number
            for number, line in enumerate(manifest.splitlines(), start=1)
            if line.endswith(f"  {removed_path}")
        )
        self.assertEqual(
            errors,
            [
                f"{added_path}:1: fixture is missing from manifest: {added_path}",
                f"scripts/fixture-manifest.sha256:{removed_line}: "
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

        # Audited suite inventory: sys_api_generation (2), system_binding_stubs (14),
        # system_provider_abi (6). Every include above is independently path- and hash-checked.
        self.assertEqual(fixture_count, 22, "all parity fixture includes are hash-pinned")

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

    def test_runtime_publication_fixtures_are_local_and_hash_pinned(self) -> None:
        workspace = Path(__file__).resolve().parents[1]
        manifest = (workspace / "scripts/fixture-manifest.sha256").read_text(
            encoding="utf-8"
        )
        suites = {
            "publication_metadata.rs": "publication_metadata.orna",
            "publication_repository_conformance.rs": "publication-repository-main.orna",
        }
        include_pattern = re.compile(r'include_str!\(\s*"([^"\n]+)"\s*\)')
        fixture_paths = set()

        for suite, expected_fixture in suites.items():
            test_path = f"crates/orna-runtime-v1/tests/{suite}"
            source = (workspace / test_path).read_text(encoding="utf-8")
            include_paths = include_pattern.findall(source)
            self.assertEqual(
                include_paths,
                [f"fixtures/{expected_fixture}"],
                f"{test_path} retains its crate-local publication proof input",
            )
            for include_path in include_paths:
                relative_path = Path(include_path)
                self.assertNotIn("..", relative_path.parts)
                fixture_path = (
                    f"crates/orna-runtime-v1/tests/fixtures/{relative_path.name}"
                )
                fixture_paths.add(fixture_path)

        self.assertEqual(len(fixture_paths), 2, "both runtime publication fixtures stay pinned")
        for fixture_path in sorted(fixture_paths):
            fixture = workspace / fixture_path
            self.assertTrue(fixture.is_file(), f"missing crate-local fixture: {fixture_path}")
            digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
            self.assertIn(
                f"{digest}  {fixture_path}\n",
                manifest,
                f"runtime publication fixture hash is pinned: {fixture_path}",
            )

    def test_runtime_checkpoint_audit_fixture_is_local_and_hash_pinned(self) -> None:
        workspace = Path(__file__).resolve().parents[1]
        manifest = (workspace / "scripts/fixture-manifest.sha256").read_text(
            encoding="utf-8"
        )
        test_path = "crates/orna-runtime-v1/tests/checkpoints_conformance_audit.rs"
        source = (workspace / test_path).read_text(encoding="utf-8")
        include_pattern = re.compile(r'include_str!\(\s*"([^"\n]+)"\s*\)')
        include_paths = include_pattern.findall(source)

        self.assertEqual(
            include_paths,
            ["fixtures/checkpoint-audit-replay-handler.orna"],
            f"{test_path} keeps its replay input local to the runtime crate",
        )
        fixture_path = (
            "crates/orna-runtime-v1/tests/fixtures/checkpoint-audit-replay-handler.orna"
        )
        fixture = workspace / fixture_path
        self.assertTrue(fixture.is_file(), f"missing crate-local fixture: {fixture_path}")
        digest = hashlib.sha256(fixture.read_bytes()).hexdigest()
        self.assertIn(
            f"{digest}  {fixture_path}\n",
            manifest,
            f"runtime checkpoint audit fixture hash is pinned: {fixture_path}",
        )


if __name__ == "__main__":
    unittest.main()


class FixtureManifestJsonFormatTests(unittest.TestCase):
    def run_check(self, extra_args: list[str], hashes: dict[str, str], manifest: str) -> tuple[int, str]:
        with TemporaryDirectory() as directory:
            manifest_path = Path(directory) / "fixture-manifest.sha256"
            manifest_path.write_text(manifest, encoding="utf-8")
            stdout = StringIO()
            with (
                mock.patch.object(check_fixture_manifest, "MANIFEST_PATH", manifest_path),
                mock.patch.object(check_fixture_manifest, "fixture_hashes", return_value=(hashes, [])),
                mock.patch("sys.argv", ["check_fixture_manifest.py", "--check", *extra_args]),
                redirect_stdout(stdout),
            ):
                status = check_fixture_manifest.main()
        return status, stdout.getvalue()

    def test_format_json_reports_drift_as_one_object(self) -> None:
        path = "crates/example/tests/fixtures/input.orna"
        pinned = hashlib.sha256(b"original\n").hexdigest()
        changed = hashlib.sha256(b"changed\n").hexdigest()
        status, output = self.run_check(
            ["--format", "json"], {path: changed}, render_manifest({path: pinned})
        )

        result = json.loads(output)
        self.assertEqual(status, 1)
        self.assertEqual(output.count("\n"), 1)
        self.assertFalse(result["ok"])
        self.assertEqual(result["files"], 1)
        self.assertEqual(result["scan_errors"], [])
        self.assertEqual(len(result["errors"]), 1)
        self.assertIn(path, result["errors"][0])

    def test_json_alias_matches_format_json(self) -> None:
        path = "crates/example/tests/fixtures/input.orna"
        pinned = hashlib.sha256(b"original\n").hexdigest()
        manifest = render_manifest({path: pinned})
        by_format = self.run_check(["--format", "json"], {path: pinned}, manifest)
        by_alias = self.run_check(["--json"], {path: pinned}, manifest)

        self.assertEqual(by_format, by_alias)
        self.assertEqual(by_format[0], 0)
        self.assertTrue(json.loads(by_format[1])["ok"])


class FixtureManifestListModeTests(unittest.TestCase):
    def test_list_prints_only_drift_entries_on_stdout(self) -> None:
        path = "crates/example/tests/fixtures/input.orna"
        pinned = hashlib.sha256(b"original\n").hexdigest()
        changed = hashlib.sha256(b"changed\n").hexdigest()
        with TemporaryDirectory() as directory:
            manifest_path = Path(directory) / "fixture-manifest.sha256"
            manifest_path.write_text(render_manifest({path: pinned}), encoding="utf-8")
            stdout = StringIO()
            with (
                mock.patch.object(check_fixture_manifest, "MANIFEST_PATH", manifest_path),
                mock.patch.object(
                    check_fixture_manifest, "fixture_hashes", return_value=({path: changed}, [])
                ),
                mock.patch("sys.argv", ["check_fixture_manifest.py", "--list"]),
                redirect_stdout(stdout),
            ):
                status = check_fixture_manifest.main()

        self.assertEqual(status, 1)
        lines = stdout.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        self.assertTrue(lines[0].startswith("scripts/fixture-manifest.sha256:3: "))
        self.assertIn(path, lines[0])
        self.assertNotIn("drift total", stdout.getvalue())


class FixtureManifestLimitTests(unittest.TestCase):
    def drift_hashes(self) -> tuple[dict[str, str], str]:
        paths = [f"crates/example/tests/fixtures/input-{index}.orna" for index in range(3)]
        pinned = {path: hashlib.sha256(b"original\n").hexdigest() for path in paths}
        changed = {path: hashlib.sha256(b"changed\n").hexdigest() for path in paths}
        return changed, render_manifest(pinned)

    def run_main(self, argv: list[str], hashes: dict[str, str], manifest: str) -> tuple[int, str, str]:
        with TemporaryDirectory() as directory:
            manifest_path = Path(directory) / "fixture-manifest.sha256"
            manifest_path.write_text(manifest, encoding="utf-8")
            stdout, stderr = StringIO(), StringIO()
            with (
                mock.patch.object(check_fixture_manifest, "MANIFEST_PATH", manifest_path),
                mock.patch.object(check_fixture_manifest, "fixture_hashes", return_value=(hashes, [])),
                mock.patch("sys.argv", ["check_fixture_manifest.py", *argv]),
                redirect_stdout(stdout),
                redirect_stderr(stderr),
            ):
                status = check_fixture_manifest.main()
        return status, stdout.getvalue(), stderr.getvalue()

    def test_limit_caps_text_entries_but_keeps_the_total(self) -> None:
        hashes, manifest = self.drift_hashes()
        status, _, stderr = self.run_main(["--check", "--limit", "1"], hashes, manifest)

        self.assertEqual(status, 1)
        entries = [line for line in stderr.splitlines() if line.startswith("  ")]
        self.assertEqual(len(entries), 1)
        self.assertIn("fixture manifest drift total: 3", stderr)

    def test_limit_caps_list_entries(self) -> None:
        hashes, manifest = self.drift_hashes()
        status, stdout, _ = self.run_main(["--list", "--limit", "2"], hashes, manifest)

        self.assertEqual(status, 1)
        self.assertEqual(len(stdout.splitlines()), 2)

    def test_limit_rejects_values_below_one(self) -> None:
        with mock.patch("sys.argv", ["check_fixture_manifest.py", "--limit", "0"]):
            with redirect_stderr(StringIO()), self.assertRaises(SystemExit) as raised:
                check_fixture_manifest.main()
        self.assertEqual(raised.exception.code, 2)


class FixtureManifestSinceTests(unittest.TestCase):
    def test_only_changed_keeps_errors_naming_a_changed_path(self) -> None:
        changed_path = "crates/example/tests/fixtures/input.orna"
        other_path = "crates/example/tests/fixtures/other.orna"
        pinned = {changed_path: hashlib.sha256(b"a\n").hexdigest(), other_path: hashlib.sha256(b"b\n").hexdigest()}
        actual = {changed_path: hashlib.sha256(b"x\n").hexdigest(), other_path: hashlib.sha256(b"y\n").hexdigest()}
        errors = validate_manifest(actual, render_manifest(pinned))
        self.assertEqual(len(errors), 2)

        kept = only_changed(errors, {changed_path})

        self.assertEqual(len(kept), 1)
        self.assertIn(changed_path, kept[0])
        self.assertNotIn(other_path, kept[0])

    def test_only_changed_does_not_match_path_prefixes(self) -> None:
        short = "crates/example/tests/fixtures/input.orna"
        longer = "crates/example/tests/fixtures/input.orna.bak"
        errors = [f"{longer}:1: fixture is missing from manifest: {longer}"]
        self.assertEqual(only_changed(errors, {short}), [])


class FixtureManifestMissingManifestTests(unittest.TestCase):
    def test_missing_manifest_exits_2_not_drift_1(self) -> None:
        with TemporaryDirectory() as directory:
            missing = Path(directory) / "fixture-manifest.sha256"
            stderr = StringIO()
            with (
                mock.patch.object(check_fixture_manifest, "MANIFEST_PATH", missing),
                mock.patch.object(check_fixture_manifest, "fixture_hashes", return_value=({}, [])),
                mock.patch("sys.argv", ["check_fixture_manifest.py", "--check"]),
                redirect_stderr(stderr),
            ):
                status = check_fixture_manifest.main()

        self.assertEqual(status, 2)
        self.assertIn("fixture manifest does not exist", stderr.getvalue())


class FixtureManifestTsvFormatTests(unittest.TestCase):
    def run_check(self, extra_args: list[str], hashes: dict[str, str], manifest: str) -> tuple[int, str]:
        with TemporaryDirectory() as directory:
            manifest_path = Path(directory) / "fixture-manifest.sha256"
            manifest_path.write_text(manifest, encoding="utf-8")
            stdout = StringIO()
            with (
                mock.patch.object(check_fixture_manifest, "MANIFEST_PATH", manifest_path),
                mock.patch.object(check_fixture_manifest, "fixture_hashes", return_value=(hashes, [])),
                mock.patch("sys.argv", ["check_fixture_manifest.py", "--check", *extra_args]),
                redirect_stdout(stdout),
            ):
                status = check_fixture_manifest.main()
        return status, stdout.getvalue()

    def test_tsv_clean_tree_has_header_and_ok_row(self) -> None:
        path = "crates/example/tests/fixtures/input.orna"
        pinned = hashlib.sha256(b"original\n").hexdigest()
        status, output = self.run_check(["--format", "tsv"], {path: pinned}, render_manifest({path: pinned}))

        self.assertEqual(status, 0)
        rows = output.splitlines()
        self.assertEqual(rows[0], "ok\tfiles\ttrees\terrors")
        self.assertEqual(rows[1].split("\t")[::3], ["true", "0"])

    def test_tsv_drift_row_reports_error_count(self) -> None:
        path = "crates/example/tests/fixtures/input.orna"
        pinned = hashlib.sha256(b"original\n").hexdigest()
        changed = hashlib.sha256(b"changed\n").hexdigest()
        status, output = self.run_check(["--format", "tsv"], {path: changed}, render_manifest({path: pinned}))

        self.assertEqual(status, 1)
        rows = output.splitlines()
        self.assertEqual(rows[0], "ok\tfiles\ttrees\terrors")
        self.assertEqual(rows[1].split("\t")[0], "false")
        self.assertEqual(rows[1].split("\t")[3], "1")


class FixtureManifestHelpTests(unittest.TestCase):
    def test_help_lists_examples_and_exit_status(self) -> None:
        stdout = StringIO()
        with (
            mock.patch("sys.argv", ["check_fixture_manifest.py", "--help"]),
            redirect_stdout(stdout),
            self.assertRaises(SystemExit) as raised,
        ):
            check_fixture_manifest.main()

        self.assertEqual(raised.exception.code, 0)
        help_text = stdout.getvalue()
        self.assertIn("examples:", help_text)
        self.assertIn("--since origin/main", help_text)
        self.assertIn("exit status:", help_text)
        self.assertIn("2  the pinned manifest is missing", help_text)


class FixtureManifestUnreadableTests(unittest.TestCase):
    def test_unreadable_fixture_is_a_scan_error_with_exit_4(self) -> None:
        with TemporaryDirectory() as directory:
            workspace = Path(directory)
            fixtures = workspace / "crates" / "example" / "tests" / "fixtures"
            fixtures.mkdir(parents=True)
            (fixtures / "locked.orna").write_bytes(b"locked\n")
            original_read_bytes = Path.read_bytes

            def read_bytes(path: Path) -> bytes:
                if path.name == "locked.orna":
                    raise PermissionError(13, "Permission denied")
                return original_read_bytes(path)

            with mock.patch.object(Path, "read_bytes", read_bytes):
                hashes, errors = fixture_hashes(workspace)

        self.assertEqual(hashes, {})
        self.assertEqual(len(errors), 1)
        self.assertTrue(errors[0].startswith("fixture file is unreadable: "))
        self.assertIn("crates/example/tests/fixtures/locked.orna", errors[0])
        self.assertEqual(scan_exit_status(errors), 4)

    def test_other_scan_errors_still_exit_1(self) -> None:
        self.assertEqual(scan_exit_status(["fixture file symlink is not supported: x"]), 1)


class FixtureManifestRootsTests(unittest.TestCase):
    def test_roots_prints_each_fixture_root_after_the_summary(self) -> None:
        path = "crates/example/tests/fixtures/input.orna"
        pinned = hashlib.sha256(b"original\n").hexdigest()
        with TemporaryDirectory() as directory:
            manifest_path = Path(directory) / "fixture-manifest.sha256"
            manifest_path.write_text(render_manifest({path: pinned}), encoding="utf-8")
            stdout = StringIO()
            with (
                mock.patch.object(check_fixture_manifest, "MANIFEST_PATH", manifest_path),
                mock.patch.object(check_fixture_manifest, "fixture_hashes", return_value=({path: pinned}, [])),
                mock.patch.object(
                    check_fixture_manifest,
                    "fixture_roots",
                    return_value=[check_fixture_manifest.WORKSPACE_ROOT / "crates/example/tests/fixtures"],
                ),
                mock.patch("sys.argv", ["check_fixture_manifest.py", "--check", "--roots"]),
                redirect_stdout(stdout),
            ):
                status = check_fixture_manifest.main()

        self.assertEqual(status, 0)
        self.assertEqual(
            stdout.getvalue().splitlines(),
            [
                "fixture manifest matches 1 files across 1 trees",
                "crates/example/tests/fixtures",
            ],
        )
