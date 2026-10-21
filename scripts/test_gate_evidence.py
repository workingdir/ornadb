from __future__ import annotations

import hashlib
import io
import json
import sys
import textwrap
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from scripts.gate_evidence import (
    BASELINE,
    ITEMS,
    EvidenceError,
    Failure,
    KnownFailure,
    ParsedLog,
    TargetRun,
    classify,
    evaluate_selector,
    gate_rollup,
    index_artifact,
    load_baseline,
    load_items,
    main,
    observed_failures,
    package_name,
    parse_log,
    prepare_command,
    skip_arguments,
)

EXECUTABLES = {
    "orna_demo_v1-1111": ("orna-demo-v1", "lib"),
    "storage_scale-2222": ("orna-demo-v1", "test:storage_scale"),
    "storage_scale-3333": ("orna-other-v1", "test:storage_scale"),
}
LIBRARIES = {"orna_demo_v1": "orna-demo-v1"}

LOG = textwrap.dedent(
    """\
       Compiling orna-demo-v1 v1.0.0 (/work/crates/orna-demo-v1)
        Finished `test` profile [unoptimized + debuginfo] target(s) in 1.00s
         Running unittests src/lib.rs (target/debug/deps/orna_demo_v1-1111)

    running 4 tests
    test codec::tests::round_trips ... ok
    test codec::tests::rejects_bad_tag ... FAILED
    test codec::tests::needs_wasm ... ignored, requires the wasm32 target
    test codec::tests::panics_on_overflow - should panic ... ok

    successes:

    ---- codec::tests::round_trips stdout ----
    Gate X: bytes=10; elapsed_ms=4

    successes:
        codec::tests::panics_on_overflow
        codec::tests::round_trips

    failures:

    ---- codec::tests::rejects_bad_tag stdout ----
    thread 'codec::tests::rejects_bad_tag' panicked at src/codec.rs:9:5:
    test result: this text is panic output, not a summary

    failures:
        codec::tests::rejects_bad_tag

    test result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s

    error: test failed, to rerun pass `-p orna-demo-v1 --lib`
         Running tests/storage_scale.rs (/abs/target/debug/deps/storage_scale-2222)

    running 2 tests
    test million_rows ... ok
    test ten_rows ... ok

    successes:

    ---- million_rows stdout ----
    ORP-1 one-million-row case skipped: set ORNA_ORP1_MILLION_ROWS=1 to run

    successes:
        million_rows
        ten_rows

    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

         Running tests/storage_scale.rs (/abs/target/debug/deps/storage_scale-3333)

    running 1 test
    thread 'main' has overflowed its stack
    fatal runtime error: stack overflow
    error: test failed, to rerun pass `-p orna-other-v1 --test storage_scale`

    Caused by:
      process didn't exit successfully: `/abs/target/debug/deps/storage_scale-3333` (signal: 6, SIGABRT: process abort signal)
       Doc-tests orna_demo_v1

    running 1 test
    test crates/orna-demo-v1/src/lib.rs - codec::encode (line 12) ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s

    error: 2 targets failed:
        `-p orna-demo-v1 --lib`
        `-p orna-other-v1 --test storage_scale`
    """
)


def parsed_demo() -> ParsedLog:
    return parse_log(io.StringIO(LOG), EXECUTABLES, LIBRARIES)


def record(name: str, state: str, targets: list[dict]) -> dict:
    return {
        "record": name,
        "state": state,
        "command": f"cargo test ({name})",
        "argv": ["cargo", "test"],
        "inputs": {"env": {}},
        "log": {"file": f"{name}.log", "sha256": "0" * 64},
        "targets": targets,
    }


def target(package: str, label: str, tests: dict[str, str], completed: bool = True) -> dict:
    return {"package": package, "target": label, "executable": "x", "completed": completed, "counts": {}, "tests": tests}


class LogParsingTests(unittest.TestCase):
    def test_maps_each_binary_to_its_package_and_target(self) -> None:
        labels = [(run.package, run.target, run.completed) for run in parsed_demo().targets]

        self.assertEqual(
            labels,
            [
                ("orna-demo-v1", "lib", True),
                ("orna-demo-v1", "test:storage_scale", True),
                ("orna-other-v1", "test:storage_scale", False),
                ("orna-demo-v1", "doc", True),
            ],
        )

    def test_records_outcomes_and_reports_early_returns_as_skipped(self) -> None:
        library, scale, _, doc = parsed_demo().targets

        self.assertEqual(
            library.tests,
            {
                "codec::tests::round_trips": "passed",
                "codec::tests::rejects_bad_tag": "failed",
                "codec::tests::needs_wasm": "ignored",
                "codec::tests::panics_on_overflow": "passed",
            },
        )
        self.assertEqual(library.counts["failed"], 1)
        self.assertEqual(scale.tests, {"million_rows": "skipped", "ten_rows": "passed"})
        self.assertEqual(doc.tests, {"crates/orna-demo-v1/src/lib.rs - codec::encode (line 12)": "passed"})

    def test_collects_cargo_rerun_hints_and_build_errors(self) -> None:
        build_log = io.StringIO(
            "error: could not compile `orna-demo-v1` (test \"storage_scale\") due to 2 previous errors\n"
            "error: could not compile `orna-other-v1` (lib test) due to 1 previous error\n"
        )

        self.assertEqual(parsed_demo().reruns, [("orna-demo-v1", "lib"), ("orna-other-v1", "test:storage_scale")])
        self.assertEqual(
            parse_log(build_log, {}, {}).build_errors,
            [("orna-demo-v1", "test:storage_scale"), ("orna-other-v1", "lib")],
        )

    def test_reads_package_names_from_both_package_id_formats(self) -> None:
        self.assertEqual(package_name("path+file:///w/crates/orna-demo-v1#1.0.0"), "orna-demo-v1")
        self.assertEqual(package_name("path+file:///w/crates/demo#orna-demo-v1@1.0.0"), "orna-demo-v1")
        self.assertEqual(package_name("orna-demo-v1 1.0.0 (path+file:///w/crates/orna-demo-v1)"), "orna-demo-v1")

    def test_indexes_test_executables_and_library_names(self) -> None:
        executables: dict = {}
        libraries: dict = {}
        index_artifact(
            {
                "reason": "compiler-artifact",
                "package_id": "path+file:///w/crates/orna-demo-v1#1.0.0",
                "target": {"kind": ["rlib", "cdylib"], "name": "orna_demo_v1"},
                "profile": {"test": True},
                "executable": "/w/target/debug/deps/orna_demo_v1-1111",
            },
            executables,
            libraries,
        )
        index_artifact(
            {
                "reason": "compiler-artifact",
                "package_id": "path+file:///w/crates/orna-demo-v1#1.0.0",
                "target": {"kind": ["bin"], "name": "orna-demo"},
                "profile": {"test": False},
                "executable": "/w/target/debug/orna-demo",
            },
            executables,
            libraries,
        )

        self.assertEqual(executables, {"orna_demo_v1-1111": ("orna-demo-v1", "lib")})
        self.assertEqual(libraries, {"orna_demo_v1": "orna-demo-v1"})


class FailureTests(unittest.TestCase):
    def test_lists_failed_tests_and_unfinished_binaries(self) -> None:
        failures = observed_failures(parsed_demo(), 101, False)

        self.assertEqual(
            failures,
            [
                Failure("orna-demo-v1", "lib", "codec::tests::rejects_bad_tag"),
                Failure("orna-other-v1", "test:storage_scale", "*"),
            ],
        )

    def test_reports_failures_the_log_does_not_explain(self) -> None:
        self.assertEqual(observed_failures(ParsedLog(), 101, False), [Failure("*", "*", "<exit>")])
        self.assertEqual(observed_failures(ParsedLog(), None, True), [Failure("*", "*", "<timeout>")])
        killed = ParsedLog(targets=[TargetRun("orna-demo-v1", "test:storage_scale", "storage_scale-2222")])
        self.assertEqual(observed_failures(killed, -9, True), [Failure("orna-demo-v1", "test:storage_scale", "*")])
        hinted = ParsedLog(reruns=[("orna-demo-v1", "test:storage_scale")])
        self.assertEqual(observed_failures(hinted, 101, False), [Failure("orna-demo-v1", "test:storage_scale", "*")])
        built = ParsedLog(build_errors=[("orna-demo-v1", "lib")])
        self.assertEqual(observed_failures(built, 101, False), [Failure("orna-demo-v1", "lib", "<build>")])

    def test_baseline_matches_exact_tests_and_whole_target_entries_only_for_target_failures(self) -> None:
        baseline = [
            KnownFailure("orna-demo-v1", "lib", "codec::tests::rejects_bad_tag", "ornadb-demo.1", "pinned"),
            KnownFailure("orna-other-v1", "test:storage_scale", "*", "ornadb-demo.2", "stack overflow"),
            KnownFailure("orna-demo-v1", "test:storage_scale", "*", "ornadb-demo.3", "slow", record="gate-c"),
        ]
        failures = [
            Failure("orna-demo-v1", "lib", "codec::tests::rejects_bad_tag"),
            Failure("orna-other-v1", "test:storage_scale", "*"),
            Failure("orna-other-v1", "test:storage_scale", "ten_rows"),
            Failure("orna-other-v1", "test:storage_scale", "<build>"),
            Failure("orna-demo-v1", "test:storage_scale", "*"),
        ]

        new, known = classify(failures, baseline, "workspace-rest")
        self.assertEqual(new, failures[2:])
        self.assertEqual([entry.bead for _, entry in known], ["ornadb-demo.1", "ornadb-demo.2"])

        new, known = classify(failures[4:], baseline, "gate-c")
        self.assertEqual((new, [entry.bead for _, entry in known]), ([], ["ornadb-demo.3"]))

    def test_skip_arguments_cover_named_failures_of_the_tested_packages_only(self) -> None:
        baseline = [
            KnownFailure("orna-demo-v1", "lib", "codec::tests::rejects_bad_tag", "ornadb-demo.1", "pinned"),
            KnownFailure("orna-demo-v1", "test:storage_scale", "*", "ornadb-demo.2", "stack overflow"),
            KnownFailure("orna-demo-v1", "test:storage_scale", "million_rows", "ornadb-demo.3", "slow", record="gate-c"),
            KnownFailure("orna-other-v1", "lib", "tests::other", "ornadb-demo.4", "pinned"),
            KnownFailure("workspace", "check:fmt", "fmt", "ornadb-demo.5", "drift"),
        ]

        self.assertEqual(skip_arguments(baseline, ["orna-demo-v1"]), ["--skip", "codec::tests::rejects_bad_tag"])


class BaselineFileTests(unittest.TestCase):
    def load(self, text: str) -> list[KnownFailure]:
        with TemporaryDirectory() as directory:
            path = Path(directory) / "baseline.toml"
            path.write_text(textwrap.dedent(text), encoding="utf-8")
            return load_baseline(path)

    def test_accepts_a_valid_entry(self) -> None:
        entries = self.load(
            """
            [[failure]]
            package = "orna-demo-v1"
            target = "test:storage_scale"
            test = "ten_rows"
            bead = "ornadb-5il1.145"
            reason = "pins the old fold identity"
            """
        )

        self.assertEqual(entries[0].bead, "ornadb-5il1.145")

    def test_rejects_entries_without_a_bead_target_reason_or_unique_key(self) -> None:
        entry = 'package = "orna-demo-v1"\ntarget = "lib"\ntest = "t"\nbead = "ornadb-x"\nreason = "r"\n'
        for broken, message in (
            (entry.replace('"ornadb-x"', '"5il1.145"'), "bead"),
            (entry.replace('"lib"', '"library"'), "target"),
            (entry.replace('reason = "r"', 'reason = " "'), "reason"),
            (entry.replace('reason = "r"\n', ""), "missing"),
            (entry + "[[failure]]\n" + entry, "duplicate"),
        ):
            with self.subTest(message=message), self.assertRaisesRegex(EvidenceError, message):
                self.load("[[failure]]\n" + broken)

    def test_checked_in_baseline_and_gate_items_are_valid(self) -> None:
        baseline = load_baseline(BASELINE)
        manifest = load_items(ITEMS)

        self.assertTrue(all(entry.bead.startswith("ornadb-") for entry in baseline))
        self.assertEqual(manifest.publication["revision"], "final-2026-10-05")
        self.assertEqual({item["gate"] for item in manifest.items}, set("ABCDEF"))


class CommandTests(unittest.TestCase):
    def test_adds_json_build_messages_and_per_test_output(self) -> None:
        self.assertEqual(
            prepare_command(["cargo", "test", "-p", "orna-demo-v1", "--", "--skip", "slow"]),
            [
                "cargo",
                "test",
                "-p",
                "orna-demo-v1",
                "--message-format=json-render-diagnostics",
                "--",
                "--skip",
                "slow",
                "--show-output",
            ],
        )

    def test_rejects_options_that_change_the_parsed_output(self) -> None:
        for command in (
            ["cargo", "test", "--", "--nocapture"],
            ["cargo", "test", "-q"],
            ["cargo", "test", "--", "--format=json"],
            ["cargo", "build"],
        ):
            with self.subTest(command=command), self.assertRaises(EvidenceError):
                prepare_command(command)


class SelectorTests(unittest.TestCase):
    RECORDS = {
        "workspace-rest": record(
            "workspace-rest",
            "failed",
            [
                target("orna-demo-v1", "test:storage_scale", {"ten_rows": "passed", "million_rows": "skipped"}),
                target("orna-demo-v1", "lib", {"codec::tests::round_trips": "passed", "codec::tests::bad": "failed"}),
            ],
        ),
        "gate-c": record("gate-c", "passed", [target("orna-demo-v1", "test:storage_scale", {"million_rows": "passed"})]),
        "gate-e": record("gate-e", "unavailable", []),
    }
    EXPECTED = ["workspace-rest", "gate-c", "gate-e", "gate-d"]

    def state(self, selector: dict) -> str:
        return evaluate_selector(selector, self.RECORDS, self.EXPECTED)["state"]

    def test_states_follow_execution_not_association(self) -> None:
        scale = {"package": "orna-demo-v1", "target": "test:storage_scale"}

        self.assertEqual(self.state({**scale, "record": "workspace-rest", "test": "ten_rows"}), "passed")
        self.assertEqual(self.state({**scale, "record": "workspace-rest", "test": "million_rows"}), "not-executed")
        self.assertEqual(self.state({**scale, "record": "gate-c", "test": "million_rows"}), "passed")
        self.assertEqual(self.state({"package": "orna-demo-v1", "target": "lib", "prefix": "codec::"}), "failed")
        self.assertEqual(self.state({"package": "orna-demo-v1", "target": "lib", "test": "round_trips"}), "passed")
        self.assertEqual(self.state({"record": "gate-e"}), "unavailable")
        self.assertEqual(self.state({"record": "gate-d"}), "not-executed")
        self.assertEqual(self.state({"record": "workspace-*", "package": "orna-missing-v1"}), "not-executed")
        self.assertEqual(self.state({"package": "orna-missing-v1"}), "unavailable")

    def test_a_gate_with_a_declared_limitation_is_never_passed(self) -> None:
        items = [
            {"gate": "A", "id": "A1", "state": "passed", "limitations": [], "conditional": False},
            {"gate": "A", "id": "A2", "state": "passed", "limitations": ["no test for X"], "conditional": False},
            {"gate": "B", "id": "B1", "state": "passed", "limitations": [], "conditional": False},
            {"gate": "C", "id": "C1", "state": "failed", "limitations": [], "conditional": False},
        ]

        self.assertEqual([gate["state"] for gate in gate_rollup(items)], ["not-executed", "passed", "failed"])


FAKE_CARGO = textwrap.dedent(
    """\
    #!/usr/bin/env python3
    import json, sys
    sys.stdout.reconfigure(line_buffering=True)
    print(json.dumps({"reason": "compiler-artifact", "package_id": "path+file:///w/crates/orna-demo-v1#1.0.0",
                      "target": {"kind": ["test"], "name": "storage_scale"}, "profile": {"test": True},
                      "executable": "/w/target/debug/deps/storage_scale-2222"}), flush=True)
    print("     Running tests/storage_scale.rs (target/debug/deps/storage_scale-2222)", file=sys.stderr, flush=True)
    print("running 2 tests")
    print("test ten_rows ... ok")
    print("test wide_rows ... FAILED")
    print("")
    print("failures:")
    print("    wide_rows")
    print("")
    print("test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s")
    print("error: test failed, to rerun pass `-p orna-demo-v1 --test storage_scale`", file=sys.stderr)
    sys.exit(101)
    """
)


class EndToEndTests(unittest.TestCase):
    def run_main(self, argv: list[str]) -> int:
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            return main(argv)

    def test_run_records_known_failures_and_fails_only_on_new_ones(self) -> None:
        with TemporaryDirectory() as directory:
            root = Path(directory)
            cargo = root / "bin/cargo"
            cargo.parent.mkdir()
            cargo.write_text(f"#!{sys.executable}\n" + FAKE_CARGO.split("\n", 1)[1], encoding="utf-8")
            cargo.chmod(0o755)
            baseline = root / "baseline.toml"
            baseline.write_text("", encoding="utf-8")
            command = ["run", "--record", "demo", "--out", str(root / "out"), "--baseline", str(baseline), "--", str(cargo), "test"]

            self.assertEqual(self.run_main(command), 1)
            first = json.loads((root / "out/demo.record.json").read_text(encoding="utf-8"))
            self.assertEqual(first["state"], "failed")
            self.assertEqual(first["failures"]["new"][0]["test"], "wide_rows")
            self.assertEqual(first["targets"][0]["package"], "orna-demo-v1")

            baseline.write_text(
                '[[failure]]\npackage = "orna-demo-v1"\ntarget = "test:storage_scale"\ntest = "wide_rows"\n'
                'bead = "ornadb-demo.1"\nreason = "pinned"\n',
                encoding="utf-8",
            )
            self.assertEqual(self.run_main(command), 0)
            second = json.loads((root / "out/demo.record.json").read_text(encoding="utf-8"))
            self.assertEqual(second["state"], "failed")
            self.assertEqual(second["failures"]["known"][0]["bead"], "ornadb-demo.1")
            self.assertEqual(second["baseline"][0]["status"], "observed")

    def test_check_records_a_non_test_command_and_missing_prerequisites(self) -> None:
        with TemporaryDirectory() as directory:
            root = Path(directory)
            baseline = root / "baseline.toml"
            baseline.write_text("", encoding="utf-8")
            failing = ["run", "--record", "fmt", "--check", "fmt", "--out", str(root), "--baseline", str(baseline)]

            self.assertEqual(self.run_main([*failing, "--", sys.executable, "-c", "raise SystemExit(1)"]), 1)
            fmt = json.loads((root / "fmt.record.json").read_text(encoding="utf-8"))
            self.assertEqual((fmt["state"], fmt["failures"]["new"][0]["target"]), ("failed", "check:fmt"))

            missing = ["run", "--record", "fuse", "--out", str(root), "--baseline", str(baseline)]
            missing += ["--require-path", str(root / "no-such-device"), "--", "cargo", "test"]
            self.assertEqual(self.run_main(missing), 1)
            fuse = json.loads((root / "fuse.record.json").read_text(encoding="utf-8"))
            self.assertEqual(fuse["state"], "unavailable")

    def test_summarize_reports_items_and_missing_records(self) -> None:
        with TemporaryDirectory() as directory:
            root = Path(directory)
            records = root / "records"
            records.mkdir()
            log = records / "workspace-rest.log"
            log.write_text("ORP-1 phase label=run_case rows=10 ms=3\n", encoding="utf-8")
            value = record("workspace-rest", "passed", [target("orna-demo-v1", "test:storage_scale", {"ten_rows": "passed"})])
            value.update(
                schema="ornadb-gate-evidence/1",
                commit="a" * 40,
                totals={"passed": 1},
                failures={"new": [], "known": []},
                baseline=[],
            )
            value["log"]["sha256"] = hashlib.sha256(log.read_bytes()).hexdigest()
            (records / "workspace-rest.record.json").write_text(json.dumps(value), encoding="utf-8")
            items = root / "items.toml"
            items.write_text(
                textwrap.dedent(
                    """\
                    [publication]
                    version = "1.1.0"
                    revision = "final-2026-10-05"
                    archive_sha256 = "fe4a35dc81c2c2994e0a18d7c7931ea0bbd298ab23248496cbe880f264c5db56"

                    [records]
                    expected = ["workspace-rest", "gate-c-million-rows"]

                    [[item]]
                    gate = "C"
                    id = "C1"
                    title = "Ten and one million rows use ORP-1"
                    counters = ["^ORP-1 "]
                    select = [
                      { record = "workspace-rest", package = "orna-demo-v1", target = "test:storage_scale", test = "ten_rows" },
                      { record = "gate-c-million-rows", package = "orna-demo-v1", target = "test:storage_scale", test = "million_rows" },
                    ]
                    """
                ),
                encoding="utf-8",
            )
            baseline = root / "baseline.toml"
            baseline.write_text("", encoding="utf-8")
            out = root / "summary.json"
            argv = ["summarize", "--records", str(records), "--out", str(out), "--items", str(items), "--baseline", str(baseline)]

            self.assertEqual(self.run_main(argv), 1)
            summary = json.loads(out.read_text(encoding="utf-8"))
            item = summary["items"][0]
            self.assertEqual(item["state"], "not-executed")
            self.assertEqual(item["counters"][0]["values"], {"rows": 10, "ms": 3})
            self.assertEqual(summary["problems"], ["gate-c-million-rows: no record (the job did not finish)"])
            self.assertEqual(summary["gates"][0]["state"], "not-executed")


if __name__ == "__main__":
    unittest.main()
