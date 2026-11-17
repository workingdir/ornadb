from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from scripts.check_ci_test_coverage import (
    CoverageError,
    Invocation,
    Package,
    Target,
    Workspace,
    evaluate,
    ignored_test_names,
    parse_cargo_test,
    runs_test,
)


def workspace(*targets: Target) -> Workspace:
    package = Package("pkg", {}, tuple(targets))
    return Workspace({"pkg": package}, frozenset({"pkg"}))


def run(*, selectors=frozenset({"test:x"}), **fields) -> Invocation:
    return Invocation(
        where="step",
        pr_triggered=True,
        packages=frozenset({"pkg"}),
        selectors=selectors,
        **fields,
    )


class IgnoredTestNames(unittest.TestCase):
    def test_finds_ignored_functions_and_skips_plain_tests(self) -> None:
        source = """
#[test]
fn plain() {}

#[ignore = "known gap"]
#[tokio::test]
async fn tokio_gap() {}

#[ignore]
fn bare_gap() {}
"""
        with TemporaryDirectory() as directory:
            path = Path(directory) / "x.rs"
            path.write_text(source, encoding="utf-8")
            self.assertEqual(ignored_test_names(path), frozenset({"tokio_gap", "bare_gap"}))


class RunsTest(unittest.TestCase):
    def test_default_run_skips_ignored_tests(self) -> None:
        plain = run()
        self.assertTrue(runs_test(plain, "a", is_ignored=False))
        self.assertFalse(runs_test(plain, "a", is_ignored=True))

    def test_ignored_flag_runs_only_ignored_tests(self) -> None:
        only_ignored = run(ignored=True)
        self.assertFalse(runs_test(only_ignored, "a", is_ignored=False))
        self.assertTrue(runs_test(only_ignored, "a", is_ignored=True))

    def test_include_ignored_runs_both(self) -> None:
        both = run(include_ignored=True)
        self.assertTrue(runs_test(both, "a", is_ignored=False))
        self.assertTrue(runs_test(both, "a", is_ignored=True))

    def test_filter_is_substring_unless_exact(self) -> None:
        substring = run(filters=("replay",))
        self.assertTrue(runs_test(substring, "replay_admission", is_ignored=False))
        self.assertFalse(runs_test(substring, "other", is_ignored=False))
        exact = run(filters=("replay",), exact=True)
        self.assertFalse(runs_test(exact, "replay_admission", is_ignored=False))
        self.assertTrue(runs_test(exact, "replay", is_ignored=False))

    def test_skip_removes_matching_tests(self) -> None:
        skipping = run(skips=("slow",))
        self.assertFalse(runs_test(skipping, "slow_one", is_ignored=False))
        self.assertTrue(runs_test(skipping, "fast", is_ignored=False))

    def test_dynamic_filter_is_never_proven_to_run_a_test(self) -> None:
        self.assertFalse(runs_test(run(dynamic_filter=True), "a", is_ignored=False))


class Evaluate(unittest.TestCase):
    def test_ignored_test_is_unrun_until_some_step_runs_it(self) -> None:
        ws = workspace(Target("pkg", "test:x", frozenset(), frozenset({"gap"})))
        plain = run()
        _, unrun = evaluate(ws, [plain])
        self.assertEqual(unrun, {("pkg", "test:x", "gap"): "#[ignore]d"})

        _, unrun = evaluate(ws, [plain, run(ignored=True, filters=("gap",), exact=True)])
        self.assertEqual(unrun, {})

    def test_skipped_name_must_be_run_by_another_step(self) -> None:
        ws = workspace(Target("pkg", "test:x"))
        skipping = run(skips=("slow",))
        _, unrun = evaluate(ws, [skipping])
        self.assertIn(("pkg", "test:x", "slow"), unrun)

        _, unrun = evaluate(ws, [skipping, run(filters=("slow",))])
        self.assertEqual(unrun, {})


class ParseCargoTest(unittest.TestCase):
    def test_unknown_cargo_option_is_an_error(self) -> None:
        ws = workspace(Target("pkg", "test:x"))
        with self.assertRaises(CoverageError):
            parse_cargo_test("step", ["-p", "pkg", "--frobnicate"], True, ws)


if __name__ == "__main__":
    unittest.main()
