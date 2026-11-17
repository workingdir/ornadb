from __future__ import annotations

import os
import subprocess
import sys
import unittest
from collections.abc import Mapping
from pathlib import Path

from scripts.bump_std import (
    EVALUATOR_SOURCE,
    MIRROR_SOURCE,
    PIN_SOURCE,
    Row,
    plan_bump,
    render_mirror,
    row_const,
)
from scripts.check_standard_library_pin import SUBMODULE_PATH, WORKSPACE_ROOT

OLD_PIN = "a" * 40
NEW_PIN = "b" * 40

MIRROR = """use crate::x;

reference_standard_modules! {
    REFERENCE_STANDARD_A_PATH_V1 = "std/a.orna";
    REFERENCE_STANDARD_B_PATH_V1 = "std/b.orna";
}

pub const REFERENCE_STANDARD_PRELUDE_EXPORTS_V1: &[&str] = &[];
"""

EVALUATOR = "pub fn reference_standard_sources() -> [(String, String); 2] {\n    todo!()\n}\n"
PIN = f'pub const CURRENT_STD_PIN: &str = "{OLD_PIN}";\n'
COUNT_TEST = "assert_eq!(sources.len(), 2);\n"


def plan(
    target: list[str],
    *,
    mirror: str = MIRROR,
    evaluator: str = EVALUATOR,
    scanned: Mapping[Path, str] | None = None,
    target_pin: str = NEW_PIN,
    excused: Mapping[str, str] | None = None,
):
    return plan_bump(
        current_pin=OLD_PIN,
        target_pin=target_pin,
        target_paths=target,
        mirror_text=mirror,
        mirror_path=MIRROR_SOURCE,
        evaluator_text=evaluator,
        evaluator_path=EVALUATOR_SOURCE,
        pin_text=PIN,
        pin_path=PIN_SOURCE,
        scanned=scanned if scanned is not None else {},
        excused=excused if excused is not None else {},
    )


class RowConstTests(unittest.TestCase):
    def test_matches_existing_naming_convention(self) -> None:
        self.assertEqual(row_const("math.orna"), "REFERENCE_STANDARD_MATH_PATH_V1")
        self.assertEqual(row_const("io/reader.orna"), "REFERENCE_STANDARD_IO_READER_PATH_V1")
        self.assertEqual(row_const("format.orna"), "REFERENCE_STANDARD_FORMAT_PATH_V1")


class PlanTests(unittest.TestCase):
    def test_same_sources_and_same_pin_plan_no_edits(self) -> None:
        result = plan(["a.orna", "b.orna"], target_pin=OLD_PIN)
        self.assertEqual(result.blockers, [])
        self.assertFalse(result.changed)
        self.assertEqual(result.added, [])
        self.assertEqual(result.removed, [])

    def test_added_module_appends_row_and_bumps_count(self) -> None:
        result = plan(["a.orna", "b.orna", "c.orna"])
        self.assertEqual(result.blockers, [])
        self.assertEqual([row.relative for row in result.added], ["c.orna"])
        self.assertEqual((result.old_count, result.new_count), (2, 3))
        self.assertIn('    REFERENCE_STANDARD_C_PATH_V1 = "std/c.orna";\n}', result.edits[MIRROR_SOURCE])
        self.assertIn("[(String, String); 3]", result.edits[EVALUATOR_SOURCE])
        self.assertIn(f'CURRENT_STD_PIN: &str = "{NEW_PIN}"', result.edits[PIN_SOURCE])

    def test_removed_module_drops_row(self) -> None:
        result = plan(["a.orna"])
        self.assertEqual([row.relative for row in result.removed], ["b.orna"])
        self.assertNotIn("REFERENCE_STANDARD_B_PATH_V1", result.edits[MIRROR_SOURCE])
        self.assertEqual(result.new_count, 1)

    def test_excused_source_is_not_mirrored(self) -> None:
        result = plan(
            ["a.orna", "b.orna", "main.orna"],
            excused={"main.orna": "root module"},
        )
        self.assertEqual(result.blockers, [])
        self.assertEqual(result.added, [])
        self.assertNotIn(MIRROR_SOURCE, result.edits)

    def test_removed_constant_still_used_blocks(self) -> None:
        result = plan(["a.orna"], scanned={Path("/x/user.rs"): "use REFERENCE_STANDARD_B_PATH_V1;"})
        self.assertTrue(any("REFERENCE_STANDARD_B_PATH_V1" in blocker for blocker in result.blockers))

    def test_stale_evaluator_count_blocks(self) -> None:
        result = plan(["a.orna", "b.orna"], evaluator=EVALUATOR.replace("; 2]", "; 5]"))
        self.assertTrue(any("differs" in blocker for blocker in result.blockers))

    def test_constant_clash_blocks(self) -> None:
        clashing = MIRROR.replace(
            'REFERENCE_STANDARD_B_PATH_V1 = "std/b.orna";',
            'REFERENCE_STANDARD_B_PATH_V1 = "std/b.orna";\n    REFERENCE_STANDARD_C_PATH_V1 = "std/other.orna";',
        )
        result = plan(["a.orna", "b.orna", "c.orna", "other.orna"], mirror=clashing)
        self.assertTrue(any("already names another module" in blocker for blocker in result.blockers))

    def test_test_count_assertions_are_rewritten(self) -> None:
        test_file = Path("/x/tests/count.rs")
        result = plan(
            ["a.orna", "b.orna", "c.orna"],
            scanned={test_file: COUNT_TEST + "let _ = reference_standard_sources();\n"},
        )
        self.assertEqual(result.count_sites, [f"{test_file}:1"])
        self.assertIn("assert_eq!(sources.len(), 3);", result.edits[test_file])

    def test_render_mirror_keeps_macro_closing_brace_last(self) -> None:
        rendered = render_mirror(
            MIRROR,
            [Row("REFERENCE_STANDARD_B_PATH_V1", "b.orna")],
            [Row("REFERENCE_STANDARD_C_PATH_V1", "c.orna")],
        )
        lines = rendered.splitlines()
        close = lines.index("}")
        self.assertEqual(lines[close - 1], '    REFERENCE_STANDARD_C_PATH_V1 = "std/c.orna";')
        self.assertNotIn("REFERENCE_STANDARD_B_PATH_V1", rendered)


class CliDryRunTests(unittest.TestCase):
    def test_dry_run_against_current_pin_reports_no_changes(self) -> None:
        std_repo = Path(os.environ.get("BUMP_STD_REPO", WORKSPACE_ROOT / SUBMODULE_PATH))
        if not (std_repo / ".git").exists():
            self.skipTest(f"{std_repo} is not an initialised checkout; run: git submodule update --init {SUBMODULE_PATH}")
        completed = subprocess.run(
            [sys.executable, "-m", "scripts.bump_std", "--std-repo", str(std_repo)],
            cwd=WORKSPACE_ROOT,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("dry-run: no changes", completed.stdout)


if __name__ == "__main__":
    unittest.main()
