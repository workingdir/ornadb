# Embedded integration and conformance evidence

Issue: `ornadb-gov5.138` (GitHub #1220)<br>
Base: `origin/main` at `5b81be772`<br>
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`, release `1.0.0`

This report maps the frozen publication assets and the current repository consumers. It records evidence levels without promoting fixture inventory, plans, adapter results, or manifest checks into implementation passes. That boundary follows ORNA-TEST-004, ORNA-TEST-006, ORNA-TEST-010, ORNA-TEST-011, and ORNA-EVIDENCE-001. This is a reporting-layer change only; it adds no runtime behavior.

## Publication pin and inventory

The checked reference has SHA-256 `d12cf5d86b9337ccbe45f257bcb8c25bc769e0505500bdc68e21a1b67d728d7d` for `Orna-1.0.0.md`. A read-only hash pass found 0 mismatches in the 415 entries of `file-manifest.json` and 0 mismatches in the 46 payload entries in `release.json`'s `normative_payload_sha256` map. The reference corpus index records 86 valid fixtures, 80 invalid fixtures, and one complete project (167 total); its requirement evidence contains 870 entries and its scenario index contains 144 authored scenarios.

These counts describe the published bundle. The requirement evidence marks implementation results `not executed`; scenario metadata says the scenarios are authored and only selected separate models are executed. They do not establish an Orna implementation pass (ORNA-TEST-004, ORNA-TEST-010, ORNA-EVIDENCE-001).

## Asset-to-consumer map

| Frozen asset | Repository consumer found | Evidence boundary |
| --- | --- | --- |
| `release.json` and its 46 normative payload digests | `orna-traceability-v1` reads and verifies the release inventory in `generate_inner`; `orna-conformance-v1` reads release digests while loading the corpus. | Integrity verification confirms the selected publication inputs match. It does not prove requirements were executed. |
| `file-manifest.json` (415 distribution files) and `SHA256SUMS` | The frozen publisher's `tools/package_release.py` generates both files and verifies the extracted ZIP against `SHA256SUMS`. No Rust consumer was found under repository `crates/`. A search of reference `source/`, `tests/`, and `api/` found no mention of these names. | Deferred: no ORNA-* requirement in the searched normative source assigns product or Rust conformance-runner behavior to the distribution inventory. The reference's Python packaging checks remain the consumer. The local hash audit here is evidence collection, not a new runtime contract. |
| `tests/conformance-manifest.json`, `tests/invalid-metadata.json`, `tests/requirements.json`, `tests/requirement-evidence.json`, `tests/scenarios.json`, and `evidence/contract-models.json` | `orna-conformance-v1::Corpus::load` reads the corpus, expected project metadata, and all vector files. `orna-traceability-v1::generate_inner` reads the release, requirements, evidence, manifest, invalid metadata, scenarios, and contract models. | The corpus tests validate indexes, links, and status boundaries. Requirement-to-test links are plans unless the named test has actually executed (ORNA-TEST-009..011). |
| `examples/reference/expectations.json` and five project modules (`main.orna`, `library.orna`, `warehouse.orna`, `sensors.orna`, `values.orna`) | `orna-conformance-v1` loads expectations with the corpus. The CLI integration test copies these actual reference `.orna` files into a temporary repository and runs `Check`, `Seed`, `Exercise`, and `SensorsIngest`. | This is a passing selected project execution. The checked project folder has no loose row files, so this run is not a complete `load_rows` proof for ORNA-TEST-003/009. The project must remain self-contained or pin exact interfaces under ORNA-EVIDENCE-002. |
| Six vector JSON files | `orna-conformance-v1` loads all six and requires each to be present and non-null. `orna-value-v1` has direct `include_str!` consumers for float, numeric, path, snapshot, and value vectors. The reference `tools/protocol_model.py` produces `protocol-vectors.json`; `tools/run_checks.py` executes that selected model and the reference evidence records it. | A Rust search found `protocol-vectors.json` only in the conformance inventory list; no direct Rust vector runner or `include_str!` consumer was found. Reference source searches for that filename returned no matches. Deferred: no ORNA-* requirement names a Rust consumer for this publication JSON. The generic machine-testable behavior evidence obligation remains ORNA-TEST-011; selected model execution is not an Orna implementation pass (ORNA-TEST-004, ORNA-EVIDENCE-001). |

The searches above were read-only. They do not change or amend the reference publication.

## Embedded route map

| Requirement | Current route located | Evidence and remaining limit |
| --- | --- | --- |
| ORNA-EMBED-001 | CLI commands discover the repository and open `RuntimeState` in-process; `authoritative_reference_project_runs_seed_exercise_and_sensor_stream` exercises local CLI commands without starting an Orna daemon. | The selected command path passes. There is no claim here for every CLI command or every startup condition. |
| ORNA-EMBED-002 | The same CLI path checks the project, runs the seed/exercise/sensor invocations, then reopens runtime state to inspect the completed observation. | Evidence covers the tested reference project and these commands. It does not promote every command listed by the requirement to passed. |
| ORNA-EMBED-003 | `Repository::runtime_socket()` exposes the worktree-local `runtime.sock` path; repository and runtime code also expose runtime-owner locks and writer leases. | The normative socket/temporary-owner route is permitted, not required. A source search found no socket server/client use of this path. The repository owner-lock and runtime competing-owner focused tests could not execute in this environment because the Git identity proxy rejected their temporary-repository identity setup; both commands exited 101 before useful test evidence. No overlapping-process conformance claim is made. |
| ORNA-EMBED-004 | Repository code has a local runtime-owner lock and runtime state has a writer-lease path. | The available focused lock/lease tests were blocked as noted above. Code-path presence alone does not establish the required process-level correctness boundary, so this report leaves that conformance claim unverified. |

The only normative behavior attributed to the reference here is what ORNA-EMBED-001..004 states: local usability without an Orna daemon, in-process opening when no local owner exists, optional local coordination for overlapping writable handles without a network server, and no correctness dependence on experimental multi-process Turso access. This report does not choose additional coordination behavior.

## Captured test evidence

Commands were run with `--locked --offline` from this worktree. Output was captured in `/tmp/ornadb-gov5-138-*-current.log` during the run; the concise result lines below are the relevant captured output and include each cargo exit code.

| Command | Captured result | Exit |
| --- | --- | ---: |
| `cargo test --locked --offline -p orna-cli-v1 authoritative_reference_project_runs_seed_exercise_and_sensor_stream -- --nocapture` | `test ... ok`; `1 passed; 0 failed; 64 filtered out` | 0 |
| `cargo test --locked --offline -p orna-conformance-v1 --test reference_corpus` | `44 passed; 0 failed` | 0 |
| `cargo test --locked --offline -p orna-traceability-v1` | `23 passed; 1 failed`; `digest_bound_engine_witnesses_add_only_explicit_executed_boundaries` rejects `valid/minimal-root.orna` because that fixture is not declared by the frozen requirement evidence. | 101 |
| `cargo test --locked --offline -p orna-traceability-v1 -- --skip digest_bound_engine_witnesses_add_only_explicit_executed_boundaries` | `23 passed; 0 failed; 1 filtered out`; its three integration targets add 5 passed, 0 failed. This run intentionally filters the failing test and is not a full package pass. | 0 |
| `cargo test --locked --offline -p orna-repository-v1 runtime_owner_lock_excludes_a_second_owner_until_drop -- --nocapture` | `0 passed; 1 failed`; the Git identity proxy rejected the test's temporary-repository identity setup. | 101 |
| `cargo test --locked --offline -p orna-runtime-v1 --lib stale_capture_and_competing_owner_are_distinct -- --nocapture` | `0 passed; 1 failed`; the same Git identity proxy rejected the temporary-repository identity setup. | 101 |

The traceability failure is tracked separately by open GitHub issue #900, “Bind engine witnesses to declared fixture coverage.” This slice did not modify the traceability producer or the frozen evidence inputs. ORNA-TEST-004 and ORNA-EVIDENCE-001 require keeping that failure and the filtered pass distinct.

No Rust test or Orna fixture was added by this documentation-only slice. The passing CLI test reads the reference project's actual `.orna` files; no Orna source is embedded in the new report.

## Deferred claims

1. A product consumer or normative behavior for verifying `file-manifest.json` / `SHA256SUMS` is deferred. Search evidence: `rg -n 'file-manifest\.json|SHA256SUMS'` across frozen `source/`, `tests/`, and `api/` returned no matches; `rg` across Rust `crates/` found no consumer.
2. A dedicated protocol-vector execution consumer is deferred. Search evidence: filename searches across frozen `source/`, `tests/`, and `api/` returned no matches; Rust matches were the six-file inventory declaration and generic corpus loader only.
3. Full ORNA-EMBED-003/004 overlap/process correctness evidence remains unverified. The available focused tests were blocked by the test environment's identity proxy; no production or test-path ownership was transferred as part of this issue.
4. The complete implementation-conformance claim remains open. ORNA-TEST-003/009 require complete project loading including loose rows, while the executed example has no loose row files; ORNA-TEST-011 additionally requires applicable implementation evidence to pass.

This report closes only the evidence-orchestration work in issue #1220. It is not a general Orna conformance declaration.
