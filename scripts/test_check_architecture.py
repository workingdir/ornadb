"""Unit tests for scripts/check_architecture.py.

Run with `python3 -m unittest scripts.test_check_architecture`. The fixtures build
a tiny synthetic workspace (cargo metadata JSON plus crate directories) so every
rule can be exercised without compiling anything. The last group of tests runs
the checker over this repository, which is the property the stage acceptance
requires: the recorded baseline passes today.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "check_architecture.py"

sys.path.insert(0, str(ROOT))
from scripts.check_architecture import scan_functions, strip_rust  # noqa: E402


def toml_string(value: str) -> str:
    return json.dumps(value)


def function_of_lines(name: str, lines: int) -> str:
    """A free function spanning exactly `lines` lines, body lines balanced."""
    assert lines >= 2
    body = "    ();\n" * (lines - 2)
    return f"fn {name}() {{\n{body}}}\n"


class Fixture:
    """A synthetic workspace for check_architecture.py."""

    def __init__(self, test: unittest.TestCase) -> None:
        holder = tempfile.TemporaryDirectory()
        test.addCleanup(holder.cleanup)
        self.root = Path(holder.name)
        self.crates: list[dict] = []
        self.knowns: list[dict] = []
        self.shipped = ""
        self.forbidden: list[str] = []
        self._written = False

    def crate(
        self,
        name: str,
        layer: int,
        *,
        deps: tuple = (),
        allowed: tuple[str, ...] = (),
        role: str | None = None,
        warning: str | None = None,
        files: dict[str, str] | None = None,
        directory: str | None = None,
    ) -> None:
        package_directory = directory or f"crates/{name}"
        self.crates.append(
            {
                "name": name,
                "layer": layer,
                "deps": list(deps),
                "allowed": list(allowed),
                "role": role,
                "warning": warning,
                "directory": package_directory,
            }
        )
        package_root = self.root / package_directory
        (package_root / "src").mkdir(parents=True, exist_ok=True)
        tables: dict[str, list[str]] = {"dependencies": [], "dev-dependencies": [], "build-dependencies": []}
        for dep in deps:
            if isinstance(dep, str):
                dep = {"name": dep}
            kind = dep.get("kind")
            table = {None: "dependencies", "dev": "dev-dependencies", "build": "build-dependencies"}[kind]
            entry = f'{dep["name"]} = {{ path = "../{dep["name"]}"'
            if dep.get("optional"):
                entry += ", optional = true"
            entry += " }"
            tables[table].append(entry)
        manifest = ["[package]", f"name = {toml_string(name)}", 'version = "0.0.0"', ""]
        for table, entries in tables.items():
            if entries:
                manifest.append(f"[{table}]")
                manifest.extend(entries)
                manifest.append("")
        (package_root / "Cargo.toml").write_text("\n".join(manifest), encoding="utf-8")
        for relative, text in (files or {}).items():
            target = package_root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")

    def known(self, identifier: str, key: str) -> None:
        self.knowns.append({"id": identifier, "key": key})

    def budget(self, text: str) -> None:
        path = self.root / "scripts" / "architecture-budget.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def raises(self, text: str) -> None:
        path = self.root / "scripts" / "architecture-budget-raises.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def _write(self) -> Path:
        if self._written:
            return self.root / "metadata.json"
        if not self.shipped:
            self.shipped = self.crates[0]["name"] if self.crates else "demo"
        layers = ["[meta]", f"shipped_binary = {toml_string(self.shipped)}"]
        forbidden = ", ".join(toml_string(name) for name in self.forbidden)
        layers.append(f"forbidden_in_shipped = [{forbidden}]")
        for crate in self.crates:
            layers.extend(["", "[[crate]]", f"name = {toml_string(crate['name'])}", f"layer = {crate['layer']}"])
            if crate["allowed"]:
                allowed = ", ".join(toml_string(name) for name in crate["allowed"])
                layers.append(f"allowed = [{allowed}]")
            if crate["role"]:
                layers.append(f"role = {toml_string(crate['role'])}")
            if crate["warning"]:
                layers.append(f"warning = {toml_string(crate['warning'])}")
        for entry in self.knowns:
            layers.extend(
                [
                    "",
                    "[[known]]",
                    f"id = {toml_string(entry['id'])}",
                    f"key = {toml_string(entry['key'])}",
                    'reason = "fixture"',
                ]
            )
        path = self.root / "docs" / "design" / "layers.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("\n".join(layers) + "\n", encoding="utf-8")

        packages = []
        members = []
        for crate in self.crates:
            identifier = f"{crate['name']} 0.0.0 (path+file:///{crate['name']})"
            members.append(identifier)
            dependencies = []
            for dep in crate["deps"]:
                if isinstance(dep, str):
                    dep = {"name": dep}
                dependencies.append(
                    {
                        "name": dep["name"],
                        "kind": dep.get("kind"),
                        "optional": bool(dep.get("optional")),
                        "rename": dep.get("rename"),
                        "req": "*",
                    }
                )
            packages.append(
                {
                    "name": crate["name"],
                    "id": identifier,
                    "version": "0.0.0",
                    "manifest_path": str(
                        self.root / crate["directory"] / "Cargo.toml"
                    ),
                    "dependencies": dependencies,
                }
            )
        metadata = {
            "packages": packages,
            "workspace_members": members,
            "workspace_root": str(self.root),
            "target_directory": str(self.root / "target"),
            "version": 1,
            "resolve": None,
            "metadata": None,
        }
        metadata_path = self.root / "metadata.json"
        metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
        self._written = True
        return metadata_path

    def run(self, *args: str) -> subprocess.CompletedProcess[str]:
        metadata = self._write()
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(self.root), "--metadata", str(metadata), *args],
            capture_output=True,
            text=True,
            check=False,
        )


class ScannerTests(unittest.TestCase):
    def test_ignores_braces_in_strings(self) -> None:
        text = 'fn a() {\n    let x = "}}}";\n}\n'
        self.assertEqual(scan_functions(text), [("a", 3)])

    def test_ignores_braces_in_doc_comments(self) -> None:
        text = "/// a }\n// }\nfn b() {\n}\n"
        self.assertEqual(scan_functions(text), [("b", 2)])

    def test_ignores_braces_in_raw_strings(self) -> None:
        text = 'fn a() {\n    let x = r#"}"#;\n}\n'
        self.assertEqual(scan_functions(text), [("a", 3)])

    def test_ignores_braces_in_char_literals(self) -> None:
        text = "fn a() {\n    let c = '}';\n}\n"
        self.assertEqual(scan_functions(text), [("a", 3)])

    def test_skips_trait_declarations(self) -> None:
        text = "trait T {\n    fn declared();\n}\n\nfn real() {\n}\n"
        self.assertEqual(scan_functions(text), [("real", 2)])

    def test_counts_signature_lines(self) -> None:
        text = "pub async fn long(\n    a: u8,\n    b: u8,\n) -> u8 {\n    a + b\n}\n"
        self.assertEqual(scan_functions(text), [("long", 6)])

    def test_strip_rust_preserves_offsets(self) -> None:
        text = 'let a = "x"; // y\n/* z */ let b = 1;\n'
        stripped = strip_rust(text)
        self.assertEqual(len(stripped), len(text))
        self.assertEqual(stripped.count("\n"), text.count("\n"))


class LayerRuleTests(unittest.TestCase):
    def test_upward_edge_fails(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1, deps=["demo-b"])
        fixture.crate("demo-b", 2)
        fixture.shipped = "demo-a"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("layer:demo-a->demo-b", result.stdout)
        self.assertIn("upward", result.stdout)

    def test_lower_edge_passes(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 2, deps=["demo-b"])
        fixture.crate("demo-b", 1)
        fixture.shipped = "demo-a"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_same_layer_edge_requires_allowed_entry(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1, deps=["demo-b"])
        fixture.crate("demo-b", 1)
        fixture.shipped = "demo-a"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("same-layer:demo-a->demo-b", result.stdout)

        fixture_allowed = Fixture(self)
        fixture_allowed.crate("demo-a", 1, deps=["demo-b"], allowed=("demo-b",))
        fixture_allowed.crate("demo-b", 1)
        fixture_allowed.shipped = "demo-a"
        allowed = fixture_allowed.run("--mode=deny")
        self.assertEqual(allowed.returncode, 0, allowed.stdout + allowed.stderr)

    def test_optional_internal_edge_fails_even_when_downward(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 2, deps=[{"name": "demo-b", "optional": True}])
        fixture.crate("demo-b", 1)
        fixture.shipped = "demo-a"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("optional:demo-a->demo-b", result.stdout)
        self.assertIn("R1", result.stdout)

    def test_shipped_closure_rejects_harness(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-cli", 3, deps=["demo-harness"], allowed=("demo-harness",))
        fixture.crate("demo-harness", 3, role="harness")
        fixture.shipped = "demo-cli"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("shipped:demo-cli->demo-harness", result.stdout)
        self.assertIn("R2", result.stdout)

    def test_shipped_closure_rejects_forbidden_name(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-cli", 3, deps=["demo-tool"], allowed=("demo-tool",))
        fixture.crate("demo-tool", 3)
        fixture.shipped = "demo-cli"
        fixture.forbidden = ["demo-tool"]
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("shipped:demo-cli->demo-tool", result.stdout)

    def test_orphan_crate_fails_and_tool_role_exempts_it(self) -> None:
        failing = Fixture(self)
        failing.crate("demo-shipped", 1)
        failing.crate("demo-orphan", 1)
        failing.shipped = "demo-shipped"
        result = failing.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("orphan:demo-orphan", result.stdout)

        exempt = Fixture(self)
        exempt.crate("demo-shipped", 1)
        exempt.crate("demo-orphan", 1, role="tool")
        exempt.shipped = "demo-shipped"
        allowed = exempt.run("--mode=deny")
        self.assertEqual(allowed.returncode, 0, allowed.stdout + allowed.stderr)

    def test_shipped_binary_with_no_dependent_is_exempt(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-cli", 1)
        fixture.shipped = "demo-cli"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_archive_crate_may_not_be_depended_on(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-cli", 3, deps=["demo-archive"], allowed=("demo-archive",))
        fixture.crate("demo-archive", 3, role="archive")
        fixture.shipped = "demo-cli"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("archive:demo-cli->demo-archive", result.stdout)

    def test_standalone_crate_may_not_depend_on_harness(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-cli", 4, deps=["demo-project"])
        fixture.crate("demo-project", 2)
        fixture.crate("demo-tool", 4, deps=["demo-harness"], allowed=("demo-harness",))
        fixture.crate("demo-harness", 4, role="harness")
        fixture.shipped = "demo-cli"
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("tool:demo-tool->demo-harness", result.stdout)

    def test_workspace_member_missing_from_table_fails(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1, deps=["demo-b"])
        fixture.crate("demo-b", 1, allowed=())
        fixture.shipped = "demo-a"
        metadata_path = fixture._write()
        layers = fixture.root / "docs" / "design" / "layers.toml"
        block = '\n[[crate]]\nname = "demo-b"\nlayer = 1\n'
        text = layers.read_text(encoding="utf-8")
        self.assertIn(block, text)
        layers.write_text(text.replace(block, "\n"), encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(fixture.root), "--metadata", str(metadata_path), "--mode=deny"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("missing-crate:demo-b", result.stdout)

    def test_table_entry_naming_missing_crate_fails(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1)
        fixture.shipped = "demo-a"
        metadata_path = fixture._write()
        layers = fixture.root / "docs" / "design" / "layers.toml"
        text = layers.read_text(encoding="utf-8") + '\n[[crate]]\nname = "demo-ghost"\nlayer = 1\n'
        layers.write_text(text, encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(fixture.root), "--metadata", str(metadata_path), "--mode=deny"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("unknown-crate:demo-ghost", result.stdout)

    def test_report_lists_new_violations(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1, deps=["demo-b"])
        fixture.crate("demo-b", 2)
        fixture.shipped = "demo-a"
        result = fixture.run("--mode=report")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("violations: 0 known, 1 new", result.stdout)


class BaselineTests(unittest.TestCase):
    def fixture(self) -> Fixture:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1, deps=["demo-b"])
        fixture.crate("demo-b", 2)
        fixture.shipped = "demo-a"
        fixture.known("V1", "layer:demo-a->demo-b")
        return fixture

    def test_known_violation_passes_deny_and_is_labelled(self) -> None:
        fixture = self.fixture()
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("[V1]", result.stdout)

    def test_strict_deny_ignores_the_baseline(self) -> None:
        fixture = self.fixture()
        result = fixture.run("--mode=deny", "--strict")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("baseline ignored", result.stdout)

    def test_new_violation_fails_with_baseline(self) -> None:
        fixture = self.fixture()
        fixture.crate("demo-c", 2)
        metadata_path = fixture._write()
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        for package in metadata["packages"]:
            if package["name"] == "demo-a":
                package["dependencies"].append({"name": "demo-c", "kind": None, "optional": False, "rename": None})
        metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(fixture.root), "--metadata", str(metadata_path), "--mode=deny"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("layer:demo-a->demo-c", result.stdout)
        self.assertIn("1 new architecture violation", result.stdout)

    def test_stale_known_entry_warns_without_failing(self) -> None:
        fixture = Fixture(self)
        fixture.crate("demo-a", 2, deps=["demo-b"])
        fixture.crate("demo-b", 1)
        fixture.shipped = "demo-a"
        fixture.known("V1", "orphan:demo-gone")
        result = fixture.run("--mode=deny")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("no longer occurs", result.stdout)


class BudgetTests(unittest.TestCase):
    """File and function size ratchets, tolerance, raises, and downward update."""

    CAPS = "[limits]\nfile_lines = 100\nfunction_lines = 120\n"

    def fixture(self, files: dict[str, str]) -> Fixture:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1, files=files)
        fixture.shipped = "demo-a"
        return fixture

    def budget(self, fixture: Fixture, files: dict[str, int] | None = None,
               functions: dict[str, int] | None = None) -> None:
        text = self.CAPS
        text += '\n[[crate]]\nname = "demo-a"\n'
        if files:
            text += "[crate.files]\n" + "".join(f'"{k}" = {v}\n' for k, v in files.items())
        if functions:
            text += "[crate.functions]\n" + "".join(f'"{k}" = {v}\n' for k, v in functions.items())
        fixture.budget(text)

    def test_no_entry_under_the_cap_passes(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        self.budget(fixture)
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_new_file_over_the_cap_fails(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 101})
        self.budget(fixture)
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("no recorded entry or reviewed raise", result.stdout)
        self.assertIn("src/lib.rs", result.stdout)

    def test_new_debt_message_names_the_split_child_escape_hatch(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 101})
        self.budget(fixture)
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("split child whose parent shrank", result.stdout)
        self.assertIn("architecture-budget-raises.toml", result.stdout)

    def test_new_function_over_the_cap_fails(self) -> None:
        fixture = self.fixture({"src/lib.rs": function_of_lines("big", 121)})
        self.budget(fixture)
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("src/lib.rs::big", result.stdout)
        self.assertIn("no recorded entry or reviewed raise", result.stdout)

    def test_growth_within_tolerance_is_a_note(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 525})
        self.budget(fixture, files={"src/lib.rs": 500})
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("[note]", result.stdout)
        self.assertIn("500 -> 525", result.stdout)
        self.assertIn("within tolerance 25", result.stdout)

    def test_growth_beyond_tolerance_fails(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 526})
        self.budget(fixture, files={"src/lib.rs": 500})
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("recorded entry 500 (tolerance 25)", result.stdout)
        self.assertIn("add or update a [[raise]]", result.stdout)

    def test_function_growth_beyond_tolerance_fails(self) -> None:
        fixture = self.fixture({"src/lib.rs": function_of_lines("big", 150)})
        self.budget(fixture, functions={"src/lib.rs::big": 121})
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("recorded entry 121 (tolerance 25)", result.stdout)

    def test_shrinkage_is_a_note_not_a_failure(self) -> None:
        # Another merged PR lowered the file; this PR must not fail for it.
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 400})
        self.budget(fixture, files={"src/lib.rs": 500})
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("[note]", result.stdout)
        self.assertIn("run --mode=budget --update", result.stdout)
        self.assertNotIn("violation", result.stdout.lower())

    def test_raise_permits_a_deliberate_file_growth(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 600})
        self.budget(fixture, files={"src/lib.rs": 500})
        fixture.raises(
            '[[raise]]\npath = "crates/demo-a/src/lib.rs"\nnew_limit = 600\n'
            'reason = "test"\nbead = "ornadb-test"\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_raise_path_names_an_archived_crate(self) -> None:
        fixture = Fixture(self)
        fixture.crate(
            "demo-a",
            1,
            files={"src/lib.rs": "// filler\n" * 600},
            directory="crates/archive/demo-a",
        )
        fixture.shipped = "demo-a"
        self.budget(fixture, files={"src/lib.rs": 500})
        fixture.raises(
            '[[raise]]\npath = "crates/archive/demo-a/src/lib.rs"\nnew_limit = 600\n'
            'reason = "archive move"\nbead = "ornadb-test"\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_raise_below_the_measured_value_still_fails(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 600})
        self.budget(fixture, files={"src/lib.rs": 500})
        fixture.raises(
            '[[raise]]\npath = "crates/demo-a/src/lib.rs"\nnew_limit = 550\n'
            'reason = "test"\nbead = "ornadb-test"\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("reviewed raise 550 (tolerance 28)", result.stdout)

    def test_raise_allows_a_new_over_cap_file(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 130})
        self.budget(fixture)
        fixture.raises(
            '[[raise]]\npath = "crates/demo-a/src/lib.rs"\nnew_limit = 130\n'
            'reason = "test"\nbead = "ornadb-test"\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_raise_for_a_missing_path_is_a_note(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        self.budget(fixture)
        fixture.raises(
            '[[raise]]\npath = "crates/demo-a/src/gone.rs::gone"\nnew_limit = 200\n'
            'reason = "reserved for an in-flight PR"\nbead = "ornadb-test"\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("reserved for an in-flight PR", result.stdout)

    def test_raise_with_missing_fields_is_a_config_error(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        self.budget(fixture)
        fixture.raises(
            '[[raise]]\npath = "crates/demo-a/src/lib.rs"\nnew_limit = 200\nreason = "test"\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("needs a non-empty bead", result.stderr)

    def test_update_lowers_and_removes_entries_without_raising(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\nd\ne\n"})
        fixture.budget(
            "[limits]\nfile_lines = 2\nfunction_lines = 2\n"
            '\n[[crate]]\nname = "demo-a"\n'
            "[crate.files]\n"
            '"src/lib.rs" = 10\n'
            '"src/gone.rs" = 50\n'
            "[crate.functions]\n"
            '"src/lib.rs::gone" = 90\n'
        )
        result = fixture.run("--mode=budget", "--update")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("lowered demo-a/src/lib.rs 10 -> 5", result.stdout)
        self.assertIn("removed demo-a/src/gone.rs", result.stdout)
        self.assertIn("removed demo-a/src/lib.rs::gone", result.stdout)
        updated = (fixture.root / "scripts" / "architecture-budget.toml").read_text(encoding="utf-8")
        self.assertIn('"src/lib.rs" = 5', updated)
        self.assertNotIn("src/gone.rs", updated)
        self.assertNotIn("src/lib.rs::gone", updated)
        self.assertNotIn("src_lines", updated)
        self.assertNotIn("allow_clippy", updated)
        self.assertEqual(fixture.run("--mode=budget").returncode, 0)

    def test_update_never_raises_a_growth_entry(self) -> None:
        fixture = self.fixture({"src/lib.rs": "// filler\n" * 40})
        fixture.budget(
            "[limits]\nfile_lines = 2\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-a"\n'
            "[crate.files]\n"
            '"src/lib.rs" = 5\n'
        )
        result = fixture.run("--mode=budget", "--update")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("recorded entry 5", result.stdout)
        updated = (fixture.root / "scripts" / "architecture-budget.toml").read_text(encoding="utf-8")
        self.assertIn('"src/lib.rs" = 5', updated)

    def test_update_does_not_add_new_debt(self) -> None:
        fixture = self.fixture({"src/big.rs": "// filler\n" * 101})
        self.budget(fixture)
        result = fixture.run("--mode=budget", "--update")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("no recorded entry or reviewed raise", result.stdout)
        updated = (fixture.root / "scripts" / "architecture-budget.toml").read_text(encoding="utf-8")
        self.assertNotIn("src/big.rs", updated)

    def test_update_requires_budget_mode(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        self.budget(fixture)
        result = fixture.run("--mode=report", "--update")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("--update requires --mode=budget", result.stderr)

    def test_budget_entry_for_a_missing_crate_is_a_note(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        fixture.budget(
            "[limits]\nfile_lines = 100\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-ghost"\n'
            "[crate.files]\n"
            '"src/lib.rs" = 500\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("[note]", result.stdout)
        self.assertIn("not a workspace member", result.stdout)
