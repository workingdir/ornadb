# ORNA-CP-* clause coverage map

Documentation-side evidence increment for the [ORNA-CP-008 portable checkpoint snapshots epic (#1886)](https://github.com/workingdir/ornadb/issues/1886). This record maps the complete ORNA-CP-* inventory in the frozen checkpoint chapter to current evidence artifacts. It changes no implementation or tests and does not claim checkpoint conformance.

## Inventory and evidence status

The frozen source `source/12-checkpoints.md` contains nine ORNA-CP-* clauses. The same reference's `tests/requirement-evidence.json` records each of ORNA-CP-001 through ORNA-CP-009 as `implementation_result: not executed`, with its implementation-conformance test `planned` and `full_implementation_coverage_claimed: false`. The local code/test observations below identify available evidence artifacts; their presence alone does not change that authoritative register status.

| Clause and frozen requirement | Current evidence artifacts | Current status |
| --- | --- | --- |
| **ORNA-CP-001** — Current checkpoints MUST be stored transactionally in Turso with the table writes produced by the corresponding item or batch. | Runtime tests include `batch_commit_publishes_all_mutations_under_one_checkpoint`, `stream_delivery_commit_publishes_mutations_and_checkpoint_atomically`, and `typed_stream_delivery_publishes_rows_and_checkpoint_atomically` in `crates/orna-runtime-v1/src/lib.rs`. The conformance runner describes a bounded CP-001 runtime-adapter witness in `crates/orna-conformance-v1/src/main.rs:27`. | Atomicity test and bounded-adapter artifacts exist. Frozen register remains planned/not executed; the runner disclaims compiler-produced Orna-engine evidence and a public `sys.Checkpoint` projection. |
| **ORNA-CP-002** — Publication MUST include checkpoint watermarks corresponding exactly to the included rows; it cannot publish a later watermark while omitting required writes. | `crates/orna-conformance-v1/src/main.rs:27` explicitly records CP-002 as skipped because the available witness does not exercise the immutable handler-inserts-then-errors path. The runtime has a checkpoint snapshot encoder and `.orna` golden fixture tests, but the focused call-site search found encoder invocations only in the test module, not a production publication caller. | Explicit skip and missing publication integration evidence. Frozen register remains planned/not executed. |
| **ORNA-CP-003** — `sys.Checkpoint` exposes current checkpoint state and committed historical watermarks through snapshot selection. | Runtime methods `stream_checkpoint` and `stream_checkpoint_history_at` are present in `crates/orna-runtime-v1/src/lib.rs`; `checkpoint_history_retains_complete_and_skip_by_snapshot` exercises history selection. The conformance witness documentation explicitly says it does not prove the public `sys.Checkpoint` projection. | Runtime-level history evidence exists; the public projection is not established by the cited evidence. Frozen register remains planned/not executed. |
| **ORNA-CP-004** — A position-format change MUST carry a version; incompatible formats require explicit migration, reset, or adoption; opaque positions cannot be bytewise/textually ordered as a substitute for a provider comparator. | `ensure_stream_position_format_compatible` rejects a changed format for an existing logical key, and `stream_runner_rejects_position_format_change_before_provider_poll` is present in `crates/orna-runtime-v1/src/lib.rs`. | Incompatible-format rejection test exists. This map found no clause-wide evidence for a versioned migration/reset/adoption path or provider comparator. Frozen register remains planned/not executed. |
| **ORNA-CP-005** — Activation failure MUST roll back both table writes and checkpoint advancement. | Runtime tests include `stream_runner_rolls_back_fail_after_table_without_checkpoint_progress`, `typed_stream_delivery_rolls_back_staged_row_and_checkpoint_on_fault_or_stale_cas`, and `batch_faults_roll_back_every_mutation_and_checkpoint` in `crates/orna-runtime-v1/src/lib.rs`. | Direct rollback test artifacts exist; no test was run for this documentation increment. Frozen register remains planned/not executed. |
| **ORNA-CP-006** — An ordered consumer MUST NOT advance beyond a failed delivery unless that exact delivery is explicitly skipped with a provider-supported successor. | `crates/orna-runtime-v1/tests/v1_replay_review_regressions.rs` constructs a failed delivery, verifies the checkpoint is unchanged, skips that exact failure, and checks advancement to its successor. Runtime history also records Complete/Skip transitions in `checkpoint_history_retains_complete_and_skip_by_snapshot`. | Relevant failure/skip test artifacts exist; no test was run for this documentation increment. Frozen register remains planned/not executed. |
| **ORNA-CP-007** — Restart on the same worktree MUST resume from its durable Turso checkpoint. | `list_stream_restart_recovers_at_the_durable_successor` is in `crates/orna-runtime-v1/tests/v1_replay_review_regressions.rs`; `durable_stream_backend_reopens_with_nullable_checkpoint_and_failure_state` is in `crates/orna-runtime-v1/src/lib.rs`. | Restart/reopen test artifacts exist; no test was run for this documentation increment. Frozen register remains planned/not executed. |
| **ORNA-CP-008** — A different clone MUST resume from the checkpoint present in its selected fetched snapshot, not a guessed local copy of another clone's tail. | The portable codec and selected-commit reader tests plus real `.orna` fixture are in `crates/orna-runtime-v1/src/lib.rs` and `crates/orna-runtime-v1/tests/fixtures/checkpoint_snapshot_v1.orna`. Merged [PR #1439](https://github.com/workingdir/ornadb/pull/1439) captured three selected-snapshot reader tests passing, exit 0. Beads child #1771 remains blocked: stream bootstrap does not yet consume the selected-snapshot reader before its first provider poll. | Codec/reader evidence exists, including historical focused execution. End-to-end cross-clone bootstrap remains blocked; frozen register remains planned/not executed. |
| **ORNA-CP-009** — Recovery of unpublished data after machine loss requires replayable source data or a separately preserved local tail; Git replication alone does not contain unpublished state. | A targeted exact-phrase search over runtime, stream, and conformance implementation/test paths found no direct machine-loss/local-tail recovery test. | No clause-specific implementation evidence was located by this search. Frozen register remains planned/not executed. |

## Captured search evidence

Clause inventory from `/home/pbox/dev/ornadb/reference/Orna-1.0.0`:

```text
$ rg -n '^\*\*ORNA-CP-[0-9]{3}\*\*' source/12-checkpoints.md
7:**ORNA-CP-001**
9:**ORNA-CP-002**
11:**ORNA-CP-003**
13:**ORNA-CP-004**
30:**ORNA-CP-005**
32:**ORNA-CP-006**
98:**ORNA-CP-007**
100:**ORNA-CP-008**
102:**ORNA-CP-009**
```

Register query over `tests/requirement-evidence.json` returned nine matching records; every record has `implementation_result=not executed`, `tests[0].status=planned`, and `full_implementation_coverage_claimed=false`.

Focused implementation searches on audited `origin/main` (`f217eef9f6997184380221312b86b3703421d879`) produced these relevant results:

```text
crates/orna-runtime-v1/src/lib.rs:1989:pub fn encode_checkpoint_snapshot_watermark(
crates/orna-runtime-v1/src/lib.rs:2032:pub fn decode_checkpoint_snapshot_watermark(
crates/orna-runtime-v1/src/lib.rs:2114:pub fn read_checkpoint_snapshot_watermark(
crates/orna-runtime-v1/src/lib.rs:15320:        let encoded = encode_checkpoint_snapshot_watermark(&watermark).unwrap();
... all remaining encode_checkpoint_snapshot_watermark call sites are in the unit-test module ...
crates/orna-conformance-v1/src/main.rs:27: ... CP-002 remains an explicit skip ...
crates/orna-runtime-v1/src/lib.rs:18720:async fn stream_runner_rejects_position_format_change_before_provider_poll()
crates/orna-runtime-v1/src/lib.rs:19509:async fn stream_runner_rolls_back_fail_after_table_without_checkpoint_progress()
crates/orna-runtime-v1/src/lib.rs:20262:async fn durable_stream_backend_reopens_with_nullable_checkpoint_and_failure_state()
crates/orna-runtime-v1/tests/v1_replay_review_regressions.rs:789:async fn list_stream_restart_recovers_at_the_durable_successor()
```

The focused CP-009 search was:

```text
rg -n -i '(machine loss|unpublished data after machine loss|separately preserved local tail|Git replication alone)' crates/orna-runtime-v1 crates/orna-stream-v1 crates/orna-conformance-v1
[no matches]
CP009_EXACT_SEARCH_EXIT_CODE=1
```

This is a bounded search result, not proof that no other recovery evidence exists. The Beads epic is open with two children closed and the stream-bootstrap child #1771 blocked. The historical PR #1439 result is scoped to selected-snapshot reader tests; it does not establish the blocked end-to-end consumer path or change the frozen reference's planned/not-executed evidence state.

Cargo tests were not run for this documentation-only increment.
