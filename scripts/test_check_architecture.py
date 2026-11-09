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
from scripts.check_architecture import (  # noqa: E402
    LAYERS_PATH,
    load_layer_table,
    scan_functions,
    strip_rust,
)


def documented_layers(path: Path) -> dict[str, int]:
    """Map crate name -> layer from a Markdown layer table in a document."""
    documented: dict[str, int] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        cells = [cell.strip() for cell in raw.strip().strip("|").split("|")]
        if len(cells) != 3 or not cells[0].isdigit():
            continue
        layer = int(cells[0])
        for crate in cells[2].split(","):
            crate = crate.strip()
            if not crate or crate.startswith("("):
                continue
            documented[crate] = layer
    return documented


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
    ) -> None:
        self.crates.append(
            {
                "name": name,
                "layer": layer,
                "deps": list(deps),
                "allowed": list(allowed),
                "role": role,
                "warning": warning,
            }
        )
        directory = self.root / "crates" / name
        (directory / "src").mkdir(parents=True, exist_ok=True)
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
        (directory / "Cargo.toml").write_text("\n".join(manifest), encoding="utf-8")
        for relative, text in (files or {}).items():
            target = directory / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")

    def known(self, identifier: str, key: str) -> None:
        self.knowns.append({"id": identifier, "key": key})

    def budget(self, text: str) -> None:
        path = self.root / "scripts" / "architecture-budget.toml"
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
                    "manifest_path": str(self.root / "crates" / crate["name"] / "Cargo.toml"),
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
    def fixture(self, files: dict[str, str]) -> Fixture:
        fixture = Fixture(self)
        fixture.crate("demo-a", 1, files=files)
        fixture.shipped = "demo-a"
        return fixture

    def test_unrecorded_src_growth_within_cap_passes(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        fixture.budget("[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n")
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_recorded_crate_src_line_may_not_grow(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        fixture.budget(
            "[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-a"\nsrc_lines = 3\nallow_clippy = 0\n'
        )
        self.assertEqual(fixture.run("--mode=budget").returncode, 0)
        path = fixture.root / "crates" / "demo-a" / "src" / "lib.rs"
        path.write_text("a\nb\nc\nd\n", encoding="utf-8")
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("budget:demo-a:src_lines", result.stdout)
        self.assertIn("src grew to 4 lines, recorded 3", result.stdout)

    def test_new_crate_over_the_cap_fails(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        fixture.budget("[limits]\ncrate_src_lines = 2\nfile_lines = 1200\nfunction_lines = 120\n")
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("no budget entry records the debt", result.stdout)

    def test_allow_clippy_count_may_not_grow(self) -> None:
        source = "#[allow(clippy::too_many_lines)]\nfn a() {\n}\n"
        fixture = self.fixture({"src/lib.rs": source})
        fixture.budget(
            "[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-a"\nallow_clippy = 1\n'
        )
        self.assertEqual(fixture.run("--mode=budget").returncode, 0)

        path = fixture.root / "crates" / "demo-a" / "src" / "lib.rs"
        path.write_text(source + "#[allow(clippy::type_complexity)]\nfn b() {\n}\n", encoding="utf-8")
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("budget:demo-a:allow_clippy", result.stdout)

    def test_unrecorded_file_over_the_cap_fails_and_recorded_file_passes(self) -> None:
        filler = ("// filler\n" * 1201)
        fixture = self.fixture({"src/lib.rs": filler})
        fixture.budget("[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n")
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("budget:demo-a:src/lib.rs", result.stdout)

        fixture = self.fixture({"src/lib.rs": filler})
        fixture.budget(
            "[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-a"\nallow_clippy = 0\n'
            "[crate.files]\n"
            '"src/lib.rs" = 1201\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unrecorded_function_over_the_cap_fails_and_recorded_function_passes(self) -> None:
        source = function_of_lines("big", 121)
        fixture = self.fixture({"src/lib.rs": source})
        fixture.budget("[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n")
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("src/lib.rs::big", result.stdout)

        fixture = self.fixture({"src/lib.rs": source})
        fixture.budget(
            "[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-a"\nallow_clippy = 0\n'
            "[crate.functions]\n"
            '"src/lib.rs::big" = 121\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_shrunk_entry_warns_but_does_not_fail(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        fixture.budget(
            "[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-a"\nallow_clippy = 0\n'
            "[crate.files]\n"
            '"src/lib.rs" = 10\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("lower the entry", result.stdout)

    def test_budget_entry_for_missing_crate_fails(self) -> None:
        fixture = self.fixture({"src/lib.rs": "a\nb\nc\n"})
        fixture.budget(
            "[limits]\ncrate_src_lines = 25000\nfile_lines = 1200\nfunction_lines = 120\n"
            '\n[[crate]]\nname = "demo-ghost"\nallow_clippy = 0\n'
        )
        result = fixture.run("--mode=budget")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("budget:unknown-crate:demo-ghost", result.stdout)


class DocumentationTests(unittest.TestCase):
    """The Markdown layer tables and docs/design/layers.toml must agree."""

    def table(self) -> dict[str, int]:
        table = load_layer_table(ROOT / LAYERS_PATH)
        return {name: spec.layer for name, spec in table.crates.items()}

    def test_root_table_matches_layers_toml(self) -> None:
        self.assertEqual(documented_layers(ROOT / "ARCHITECTURE.md"), self.table())

    def test_full_document_table_matches_layers_toml(self) -> None:
        self.assertEqual(documented_layers(ROOT / "docs" / "ARCHITECTURE.md"), self.table())


class CurrentRepositoryTests(unittest.TestCase):
    """The stage acceptance: the checked-in baseline passes on this tree."""

    def run_script(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_report_prints_the_ten_recorded_violations_and_the_warning(self) -> None:
        result = self.run_script("--mode=report")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("violations: 10 known, 0 new", result.stdout)
        for number in range(1, 11):
            self.assertIn(f"[V{number}]", result.stdout)
        self.assertIn("[W1] WARNING orna-foundation", result.stdout)
        self.assertIn("budgets:", result.stdout)

    def test_deny_passes_on_the_recorded_baseline(self) -> None:
        result = self.run_script("--mode=deny")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("no new violations", result.stdout)

    def test_strict_deny_reports_the_ten_violations(self) -> None:
        result = self.run_script("--mode=deny", "--strict")
        self.assertEqual(result.returncode, 1, result.stdout)
        numbered = [line for line in result.stdout.splitlines() if line.startswith("[V")]
        self.assertEqual(len(numbered), 10, result.stdout)

    def test_budget_passes_on_the_recorded_baseline(self) -> None:
        result = self.run_script("--mode=budget")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("budgets: no growth", result.stdout)


if __name__ == "__main__":
    unittest.main()
