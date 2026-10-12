# Frozen reference tools evidence — ornadb-jcw1

The frozen tree at `/home/pbox/dev/ornadb/reference/Orna-1.0.0` was copied to `/tmp/ornadb-jcw1-reference-run` before running tools because several checks write generated evidence. The frozen source tree was not modified. Python requirements from `tools/requirements.txt` were installed in an isolated `/tmp/ornadb-jcw1-venv`. `run_checks.py` used `PATH=/usr/bin:/bin` so its temporary Git fixtures used the system Git binary; this avoided the local Git proxy rejecting the fixture author.

## Reference check results

| Check | Captured result | Exit | Rust coverage classification |
| --- | --- | ---: | --- |
| `check_float_vectors.py` | `PASS: 10 order values, 3 equality vectors, 4 aggregate vectors, 2 statistics vectors` | 0 | Already covered by `orna-value-v1` float vector tests (`float_vectors`). |
| `check_path_vectors.py` | `PASS: 22 round trips, 10 rejections, 2 collision pairs` | 0 | Already covered by `orna-value-v1` path tests (`fixture_path_vectors`); the five reserved-name extras are also in `tests/path-vectors.json`. |
| `check_sources.py` | `source_blocks=66`, `source_blocks_accepted=66`, `project_modules=5`, `project_modules_accepted=5`, `focused_cases=14`, `focused_agree=14` | 0 | Ported to a new Rust test and `.orna` fixtures; one focused parser divergence is pinned and reported below. |
| `check_navigation.py` | Node `v26.8.1`; `entries=1849`; all targets exist; query hits: `sys.rt.info=2`, `ORNA-CONCUR-001=1`, `Branching with pending changes=1`, empty query `0`, `<img onerror=bad()>` `0` | 0 | Ported in a new Rust test that runs the frozen inline script with the same minimal DOM model. |
| `check_publication.py` | HTML link counts: `index.html=3940`, `sys-reference.html=1438`; PDFs: main `271` pages/`575` bookmarks/`8` font resources, system reference `94` pages/`274` bookmarks/`8` font resources; `10` overview sheets and `30` detailed samples rendered | 0 | Reference-only for publication artifact, PDF, and render checks; no Rust publication renderer/visual-review surface is asserted. The frozen report says human visual inspection is still awaiting record. |

The source port is in `crates/orna-conformance-v1/tests/reference_tool_models.rs`. It parses the 66 frozen documentation snippets from `source-blocks.orna` using the reference checker's module, REPL, then wrapped-module fallback; checks all five `examples/reference/*.orna` modules; and exercises all 14 focused `.orna` probes. The probes and snippets are real fixture files loaded with `include_str!`.

The frozen focused probe `unparenthesised pipeline lambda` expects rejection, while `orna-syntax-v1::parse_module` currently accepts it. The Rust test records that observed divergence explicitly instead of claiming agreement. The frozen specification requires a direct anonymous function used as a pipeline stage to be parenthesized: `ORNA-PIPE-005` at `Orna-1.0.0.md:1194`. This new-test-only slice does not change parser implementation behavior. Source-unit boundaries are specified by `ORNA-SOURCE-001` at `:133`, `ORNA-SOURCE-002` at `:135`, and `ORNA-SOURCE-003` at `:137`.

## Sibling checks invoked by `run_checks.py`

The frozen aggregate command was run with the isolated environment and system Git path:

```text
PATH=/usr/bin:/bin /tmp/ornadb-jcw1-venv/bin/python /tmp/ornadb-jcw1-reference-run/tools/run_checks.py
```

It exited `0`: `build_protocol_registry.py PASS` (13 live messages; 3 closed HTTP structures), `syntax_probe.py PASS` (166/166 bounded EBNF probes), `contract_models.py PASS` (15/15 selected models), `protocol_model.py PASS` (5/5 selected models), and `check_sources.py PASS` (counts above). The registry, syntax EBNF probe, contract-model suite, and protocol-state models are classified reference-only here; the six shipped vector suites and the new source/navigation tests cover their narrower overlapping surfaces, not the full sibling models. These scripts explicitly limit their claims to selected models and do not run a complete Orna implementation.

## Six shipped vector suites

The repository's six `VECTOR_FILES` are float, numeric, path, protocol, snapshot, and value vectors. Existing Rust coverage is in `crates/orna-value-v1/src/lib.rs` (`float_vectors`, numeric/path/snapshot/value fixture tests) and `crates/orna-conformance-v1/tests/protocol_vectors.rs`. They are classified already-covered; they are not replaced by the reference Python checks. The frozen path tool's extra reserved corpus is present in the path vector JSON, so no new duplicate path behavior was introduced.

## Reference alignment and evidence limits

`ORNA-TEST-004` at `Orna-1.0.0.md:4940` distinguishes a specified test, an implementation test, and a passed test. Accordingly, the Python tool results above are reference-state outcomes, not claims that Rust executed the reference publisher or model implementations. The publication requirements `ORNA-PUB-005` at `:3768` and `ORNA-PUB-012` at `:3750` concern durable Git publication behavior; this slice's publication script run is artifact QA evidence only.

## Rust proof

Focused target output captured from `cargo test --locked --offline -p orna-conformance-v1 --test reference_tool_models`:

```text
running 4 tests
test frozen_source_probe_cases_match_parser_acceptance ... ok
test reference_project_modules_parse_as_module_units ... ok
test frozen_documentation_source_blocks_parse_in_a_declared_entry_or_wrapper ... ok
test navigation_search_script_resolves_targets_and_matches_reference_queries ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
CARGO_EXIT_CODE=0
```

The full `orna-conformance-v1` package command was run, but it exited `101` on an unrelated existing order assertion in `tests/runtime_scenarios.rs:742`. Its actual failure was:

```text
---- published_report_declares_bounded_runtime_adapter_scenarios_without_an_orna_engine_witness stdout ----
assertion `left == right` failed
left:  ["ASSERT-CHECKPOINT-091", "CP-001", "EVAL-003", "FAIL-001", "LIVE-001", "LIVE-002", "LIVE-003", "LIVE-004", "REPL-001", "SYS-RT-RENAME-100", "TXN-001", "TXN-002"]
right: ["REPL-001", "TXN-001", "TXN-002", "CP-001", "LIVE-001", "LIVE-002", "LIVE-003", "LIVE-004", "SYS-RT-RENAME-100", "ASSERT-CHECKPOINT-091", "FAIL-001", "EVAL-003"]
test result: FAILED. 15 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
error: test failed, to rerun pass `-p orna-conformance-v1 --test runtime_scenarios`
CARGO_EXIT_CODE=101
```

The changed files are a new test target, its `.orna` fixtures, and this evidence document; the failing pre-existing test was not changed. The focused target above exits `0`.

The already-covered value-vector package was run separately:

```text
$ cargo test --locked --offline -p orna-value-v1
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
... all remaining unit, integration, and doc-test targets reported `ok` ...
CARGO_EXIT_CODE=0
```
