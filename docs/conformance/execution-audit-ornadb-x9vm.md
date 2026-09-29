# Execution chapter conformance audit (ornadb-x9vm)

Audit baseline: `origin/main` `e02fa05effb86c23afa1a39e641903144e974f45` (2026-09-29). Normative source: `reference/Orna-1.0.0/source/10-execution.md`. Evidence is graded under `source/32-conformance.md:63-71`: a scenario or semantic test is not execution evidence, and an implementation test is not a full Orna-engine claim unless it executes Orna source and observes the specified runtime boundary.

## Evidence grades

- **A — Orna runtime:** fixture-backed source is executed through the relevant runtime boundary and the asserted behavior is observed.
- **B — bounded implementation:** executable fixture or runtime-component test covers a narrower adapter/model; it does not establish the complete normative clause.
- **C — planned/model/semantic only:** scenario metadata, static semantic admission, or isolated protocol model; runtime behavior remains unverified.
- **Gap:** no applicable implementation or execution proof was found in the audited paths.
- **N/A:** guidance or permission rather than an independently executable MUST.

## Requirement-by-requirement findings

| Requirement | Reference | Tip evidence | Grade and finding |
|---|---:|---|---|
| `ORNA-TXN-001` | `source/10-execution.md:7` | `crates/orna-conformance-v1/tests/transactional_source.rs:70-112`; `runtime_scenarios.rs:361-412` | **B.** Real `.orna` rollback/commit fixtures execute in bounded transactional adapters. No claim of full compiler-produced runtime execution. |
| `ORNA-TXN-002` | `:9` | Same rollback fixture and runtime scenario above; cancellation-specific proof is listed under `ORNA-CANCEL-001`. | **B.** Escaping assertion/table errors roll back tested writes; panic/cancellation are not all covered at the Orna activation boundary. |
| `ORNA-TXN-003` | `:11` | Nested child-call fixture in `transactional_source.rs:70-98`. | **B.** Ordinary nested calls share a bounded evaluator transaction; the durable runtime proof exercises the transaction scenario. |
| `ORNA-TXN-004` | `:13` | Stream delivery tests in `crates/orna-runtime-v1/tests/stream_admission_payload.rs` and checkpoint/replay tests in `v1_replay_review_regressions.rs`. | **B.** Per-delivery runtime transactions are exercised. The reference `STREAM-001` remains scenario metadata; an unbounded Orna stream program is not shown executing end-to-end. |
| `ORNA-TXN-005` | `:17` | Evaluator activation-time tests in `crates/orna-evaluator-v1/src/lib.rs` around `4077` and `7156`; runtime snapshot/context seams in `crates/orna-runtime-v1/src/activation.rs`. | **B.** `now()` and runtime activation context have component evidence. No one fixture-backed test demonstrates the same CWD generation, snapshot, captured time, and read-your-writes together across repeated calls. |
| `ORNA-TXN-006` | `:19` | Reference scenario `ACTIVATION-001` in `tests/scenarios.json` specifies the oracle; no cross-activation partial-write visibility test was found in the audited conformance tests. | **Gap.** Two overlapping activations are not shown observing the uncommitted/committed boundary. |
| `ORNA-TXN-007` | `:31` | Reference scenario `TXN-003` is an abstract scenario; no executed `.orna`/mock-host test was found proving an external effect remains externally observed after local rollback. | **Gap.** External effects and rollback are not jointly tested at an implementation boundary. |
| `ORNA-TXN-008` | `:33` | Semantic/effect checks exist in evaluator and semantic packages; no focused execution test was found for table mutation ownership inferred without user-visible annotations. | **B.** Static/effect evidence exists, but the stated runtime ownership behavior lacks focused fixture-backed proof. |
| `ORNA-CONCUR-001` | `:42` | `crates/orna-execution-v1/tests/termination.rs:188-274` and `normal_completion_admission.rs:117-160` test the generic coordinator/supervisor model. | **C.** Model-level cancel/join and admission fencing pass; no actual Orna asynchronous child is executed through that coordinator. |
| `ORNA-TASK-001` | `:73` | `termination.rs:612-650` tests acknowledged cancellation and stale-owner fencing in the coordinator/store model; runtime owner-fence tests also exist. | **B.** Model and runtime pieces are present; cross-process execution and transaction-generation proof for an actual Orna child remains unverified. |
| `ORNA-TASK-002` | `:75` | REPL/session and `sys` lifecycle tests exist, but the audited execution tests do not demonstrate close cancelling only session-owned work while preserving another session and independent run. | **Gap.** Required isolation across all three owner classes is not demonstrated. |
| `ORNA-TASK-003` | `:77` | Durable restart/recovery and stream replay tests exist in runtime tests. | **B.** Recovery paths have component evidence; hard process death, owner lease loss, open transaction recovery, and retained committed data are not covered in one fixture-backed execution scenario. |
| `ORNA-TASK-004` | `:79` | Evaluator cancellation checkpoint tests and stream cancellation tests exist; see evaluator `lib.rs:8015-8095` and `crates/orna-conformance-v1/tests/semantic_runtime_adapter.rs:1270-1376`. | **B.** Bounded checks are exercised; adapter limitations and loss of writable authority for noncooperative code are not proven together. |
| `ORNA-CONCUR-002` | `:85` | Semantic callback/result admission tests at `crates/orna-semantic-v1/tests/semantic_graph.rs:2402-2431`. | **Gap.** Static acceptance is not execution; no `parallel` result-order runtime implementation/test was found. |
| `ORNA-CONCUR-003` | `:87` | Reference scenario `CONCUR-001` is marked “not executed by an Orna engine”; no child-transaction execution proof was found. | **Gap.** Successful child commit preservation after sibling failure is not implemented/tested at the Orna runtime boundary. |
| `ORNA-CONCUR-004` | `:89` | The clause is `SHOULD` guidance for structuring atomic work. | **N/A.** It is not a separate runtime operation; preserve as program-design guidance, not a pass/fail feature claim. |
| `ORNA-CONCUR-005` | `:91` | Semantic callback/result admission test at `semantic_graph.rs:2432-2461` does not execute a race. | **Gap.** First-success selection, loser join, and deterministic all-failed selection lack runtime implementation evidence. |
| `ORNA-CONCUR-006` | `:93` | Semantic timeout callback admission test at `semantic_graph.rs:2462-2490` does not execute a timeout. | **Gap.** Cancel/join-before-timeout failure is not implemented/tested at the runtime boundary. |
| `ORNA-CANCEL-001` | `:99` | `termination.rs` and evaluator cancellation tests exercise cancellation models; stream cancellation source tests cover selected stream boundaries. | **B.** Cancellation is distinct in those components, but no fixture-backed Orna activation test proves `|?` cannot catch cancellation while staged table writes roll back. |
| `ORNA-CANCEL-002` | `:101` | Evaluator loop/relation checkpoint tests at `evaluator/src/lib.rs:8015-8095`; stream cancellation integration at `semantic_runtime_adapter.rs:1270-1376`. | **B.** Multiple bounded checkpoints are tested; one executable fixture does not cover interpreter, loop, and stream operator boundaries together. |
| `ORNA-BACKPRESSURE-001` | `:103` | Stream source/channel policy and admission tests exist in `orna-stream-v1` and `orna-runtime-v1`. | **B.** Component backpressure is tested, but not the full behavior of an unbackpressurable source requiring an explicit policy before Orna use. |
| `ORNA-BACKPRESSURE-002` | `:105` | Sequential stream delivery tests exist in runtime/conformance packages. | **B.** Sequential processing is covered; explicit bounded concurrent processing with connector partition-order/checkpoint consistency is not demonstrated. |

The reference evidence register currently lists these execution requirements as planned/not executed (`reference/Orna-1.0.0/tests/requirement-evidence.json`, entries `ORNA-TXN-001` through `ORNA-BACKPRESSURE-002`). That register is not evidence that the implementation tests above did not run; it means the frozen register has no executed implementation result for them. This report keeps those claims separate as required by `ORNA-TEST-004` and `ORNA-EVIDENCE-001` (`source/32-conformance.md:23,69`).

## Five highest-impact implementation gaps

1. **`ORNA-CONCUR-001` (`:42`)** — connect actual Orna child work to the owner lifecycle, cancellation, and join protocol.
2. **`ORNA-CONCUR-002` (`:85`)** — implement and execute `parallel` with results returned in input order.
3. **`ORNA-CONCUR-003` (`:87`)** — establish separate child activations/transactions and preserve a successful child commit when a sibling fails.
4. **`ORNA-CONCUR-005` (`:91`)** — implement `race`, including loser cancellation/join and lowest-input-index failure when all children fail.
5. **`ORNA-CONCUR-006` (`:93`)** — implement `timeout` with cancellation/join before the timeout diagnostic and no partial success.

Semantic admission evidence for `parallel`, `race`, and `timeout` exists, but the searched executable Rust sources contain no matching runtime operations. The top-five gaps therefore need a runtime implementation before passing fixture-backed execution tests can be added; tests that merely assert failure or are ignored would misstate conformance.

## Remaining scope / path lease

This PR delivers the audit only. No behavior fix or new test is claimed. `crates/orna-runtime-v1/src/activation.rs`, `crates/orna-runtime-v1/src/lib.rs`, `crates/orna-evaluator-v1/src/lib.rs`, and `crates/orna-conformance-v1/tests/runtime_scenarios.rs` had active dirty edits in the protected shared checkout during this audit, and the same activation/lib/runtime-scenario paths were dirty in `wt-fixture-syntax-review-1390`. The audit worktree left those paths untouched. To finish the Beads acceptance, first reconcile the active runtime/evaluator writer; then implement the five gaps and add new, real-fixture `.orna` tests loaded with `include_str!` in a new test file, with actual command output and exit status.

## Focused proof captured during the audit

Command: `cargo test --locked --offline -p orna-execution-v1`

```text
running 10 tests
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 15 tests
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT_CODE=0
```

Captured complete stdout/stderr: `/tmp/ornadb-x9vm-execution-audit-test.log`. This proves the execution coordinator model tests pass; it does not close any of the five runtime gaps above.
