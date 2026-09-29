# Serving, formats, protocol, and examples audit (Orna 1.0.0)

Audit target: `origin/main` at `6134a76e31999d4577cdd54b71926ee9bcc558bf`.
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`.

Evidence grades: **A** is a focused passing test of the clause behavior; **B** is
passing component evidence that does not prove the executable integration;
**C** is an observed missing integration or an unverified clause. A gap is not
reported as fixed merely because a lower layer has a passing test.

## Clause map

| Clause and reference location | Current evidence | Grade |
|---|---|---|
| ORNA-SERVE-001, `source/28-serving.md:7` | `crates/orna-cli-v1/src/cli_args.rs` has no `Serve` command. Its `parser_preserves_option_and_argument_diagnostic_text` test currently expects `serve` to be rejected as an unknown command. The live transport primitives in `orna-live-v1` are not an executable serving path. | C — gap |
| ORNA-SERVE-002, `source/28-serving.md:9` | Since the CLI has no serve path, there is no executable host behavior to verify for avoiding root `main()` or unrelated streams. | C — blocked by missing serve path |
| ORNA-SERVE-003, `source/28-serving.md:11` | No executable serve path selects a clone's HEAD/CWD. The lower-level host tests do not establish clone selection or CWD locality. | C — blocked by missing serve path |
| ORNA-SERVE-004, `source/28-serving.md:13` | `LiveHost::bind_default_listener` binds a loopback address; `listener_policy_reports_loopback_rejects_exposure_and_releases_on_drop` passes. The CLI does not wire this policy into `orna serve`. | B — component behavior passes; integration gap remains |
| ORNA-SERVE-005, `source/28-serving.md:19` | This is an explicit scope boundary: the core does not define principals, groups, grants, per-device roles, or row permissions. No missing authorization feature is inferred. | A — reference boundary verified |
| ORNA-SERVE-006, `source/28-serving.md:23` | Live listener policy has explicit exposure and status data, with a focused passing policy test. The executable CLI integration is absent. | B — component behavior passes; integration gap remains |
| ORNA-SERVE-007, `source/28-serving.md:29` | The serve command and Git/web frontend orchestration are absent from the CLI. Git transport independence cannot be demonstrated through `orna serve`. | C — gap |
| ORNA-SERVE-008/009, `source/28-serving.md:31-33` | No default frontend selection or installed root frontend is present in the CLI package. These are also `SHOULD` clauses, not reinterpreted here as new MUSTs. | C — unimplemented surface |
| ORNA-FORMAT-001, `source/29-formats.md:13` | OVB-1 strict decoding and canonicality are covered by value and protocol tests, including duplicate-key and noncanonical-form rejection. The focused protocol status suite passes. | A — focused protocol evidence |
| ORNA-FORMAT-002, `source/29-formats.md:61` | Runtime regression tests exercise generation-pinned CWD references; the requirement is also named in `crates/orna-runtime-v1/tests/v1_review_regressions.rs`. | B — existing focused regression coverage; not rerun in this audit |
| ORNA-FORMAT-003, `source/29-formats.md:63` | Typed value/schema paths exist in `orna-value-v1`; this audit did not find a single focused test proving all database/type/table/key agreement and no-context-replacement cases together. | C — coverage remains unverified |
| ORNA-FORMAT-004, `source/29-formats.md:131` | Typed decoders and text conversion helpers exist across value/core/storage code, but this audit did not establish a focused schema-directed non-execution/refinement test for the complete clause. | C — coverage remains unverified |
| ORNA-PROTO-001, `source/30-protocol.md:9` | `request_status_result` rejects duplicate nested keys, noncanonical encodings, malformed result bodies, and mismatched fingerprints. All 10 tests pass. | A — focused protocol evidence |
| ORNA-PROTO-002, `source/30-protocol.md:23` | Live host tests exercise retained terminal outcomes and session resumption; this audit did not rerun the recovery cases. | B — existing coverage |
| ORNA-PROTO-003, `source/30-protocol.md:92` | Client watch regression coverage checks atomic patch application/resync, but server and client integration is not covered by the focused tests run here. | B — component evidence |
| ORNA-PROTO-004, `source/30-protocol.md:113` | Serving/session reservation code distinguishes reservation from terminal outcome. A dedicated focused test covering reservation-only replay prevention was not established in this audit. | C — coverage remains unverified |
| ORNA-TEST-003, `source/32-conformance.md:21`; worked example requirements in `source/31-examples.md:3-15` | `reference_example_project_loads_and_type_checks_as_a_complete_project` loads all five real reference `.orna` modules with `include_str!`, parses and type-checks them together, and validates discovered rows. It passes with five modules and zero row failures. | A — direct complete-project evidence |

The worked-example behaviors and expected results are declared in
`examples/reference/expectations.json`; the CLI and conformance adapters also
contain runtime-adapter paths. The focused test run in this audit verifies the
`ORNA-TEST-003` parse/typecheck/load-rows contract, not full-engine execution or
serving behavior.

## Highest-impact gaps

1. **No `orna serve` executable path** (ORNA-SERVE-001). The CLI parser rejects
   the command. Existing listener/session components do not provide the
   required Git transport, page/query endpoints, and presentation WebSocket as
   one runnable host.
2. **No no-auto-run guarantee at the host boundary** (ORNA-SERVE-002). The
   absence of a host path prevents an end-to-end check that the root `main()`
   and unrelated streams remain idle.
3. **No per-clone HEAD/CWD selection** (ORNA-SERVE-003). The current evidence
   does not show a listener bound to the target clone's repository/runtime.
4. **Listener policy is not wired to a serving CLI** (ORNA-SERVE-004/006).
   The component binds loopback by default and reports explicit exposure, but
   the command is absent.
5. **No default web frontend integration** (ORNA-SERVE-007/008/009). There is
   no executable route that keeps Git transport available without a frontend
   or provides the described root frontend.

These five findings share one missing executable host integration. Adding
fixture tests that only call the low-level listener would falsely upgrade
component evidence into serving conformance; fabricating the missing behavior
would exceed the frozen contract. The format/protocol items graded C also need
separate narrowly targeted test work if no behavior mismatch is demonstrated.

## Captured focused proof

- `cargo test -p orna-cli-v1 --bin orna-cli-v1 parser_preserves_option_and_argument_diagnostic_text -- --nocapture` — exit 0; 1 test passed. The test confirms `serve` is rejected as unknown.
- `cargo test -p orna-conformance-v1 --test valid_fixtures reference_example_project_loads_and_type_checks_as_a_complete_project -- --nocapture` — exit 0; 1 test passed. It reports modules `library.orna`, `main.orna`, `sensors.orna`, `values.orna`, `warehouse.orna`; semantic check PASS; rows 0, failures 0.
- `cargo test -p orna-protocol-v1 --test request_status_result -- --nocapture` — exit 0; 10 tests passed.
- `cargo test -p orna-live-v1 --test live_host listener_policy_reports_loopback_rejects_exposure_and_releases_on_drop -- --nocapture` — exit 0; 1 test passed.
- `cargo test -p orna-value-v1 --test typed_map_codec -- --nocapture` — exit 0; 8 tests passed, including canonical key ordering, duplicate keys, and noncanonical forms.

No gates or formatters were run.
