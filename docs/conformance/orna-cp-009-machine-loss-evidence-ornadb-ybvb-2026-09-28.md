# ORNA-CP-009 machine-loss evidence gap

Documentation-side evidence increment for the [ORNA-CP-008 portable checkpoint snapshots epic (#1886)](https://github.com/workingdir/ornadb/issues/1886), following the clause coverage map in [PR #1887](https://github.com/workingdir/ornadb/pull/1887). This record is limited to ORNA-CP-009 and does not change implementation or tests.

## Frozen requirement

The frozen reference `/home/pbox/dev/ornadb/reference/Orna-1.0.0/source/12-checkpoints.md:102` states:

> **ORNA-CP-009** Recovery of unpublished data after machine loss requires replayable source data or a separately preserved local tail. Git replication alone does not contain unpublished state.

This clause makes either replayable source data or a separately preserved local tail necessary to recover unpublished data after machine loss, and explicitly says Git replication alone is insufficient. It does not prescribe a particular backup protocol or test harness. Evidence for this clause would need to establish the machine-loss recovery scenario and show recovery from one of those two sources; same-worktree restart or preservation before a loss does not establish that outcome.

The frozen reference's `tests/requirement-evidence.json` entry for ORNA-CP-009 says `implementation_result: not executed`, lists the implementation-conformance obligation as `planned`, and sets `full_implementation_coverage_claimed: false`.

## Current repository evidence and disposition

The audit used `origin/main` at `5208098443064499d112d9257893054bec9d479a` (the merge of PR #1887). Focused search across runtime, stream, and conformance code found no direct match for machine-loss recovery or a separately preserved local tail. A broader crate search found adjacent artifacts, but none demonstrates the outcome required by ORNA-CP-009:

- `crates/orna-sys-v1/src/lib.rs:520` describes `sys.admin.commit` as preserving an unpublished tail during commit. This does not establish that the tail is separately preserved or recoverable after machine loss.
- `crates/orna-runtime-v1/src/lib.rs:2340` describes a finite replayable source for the built-in list stream contract. This does not establish recovery of unpublished data after machine loss.
- `crates/orna-runtime-v1/tests/v1_replay_review_regressions.rs:789`, `list_stream_restart_recovers_at_the_durable_successor`, covers durable restart on the same worktree, not machine loss or recovery from an independently preserved tail.

**Disposition: deferred; CP-009 conformance remains unverified.** The bounded searches below located no clause-specific recovery evidence. They cannot establish that no evidence exists outside the searched repository paths. No product recovery guarantee is claimed here. To resolve this evidence gap, capture evidence showing recovery after machine loss using replayable source data or a separately preserved local tail; Git replication by itself does not meet the frozen clause.

## Captured search evidence

Commands ran from the repository root at the revision above:

```text
$ rg -n -i '(machine[ -]?loss|unpublished data after machine loss|unpublished (state|data)|separately preserved local tail|local tail|git replication alone)' crates/orna-runtime-v1 crates/orna-stream-v1 crates/orna-conformance-v1
[no matches]
EXIT_CODE=1
```

The broader search was:

```text
$ rg -n -i '(machine.?loss|machine.?failure|machine.?crash|unpublished (state|data|tail)|local tail|tail preservation|tail archive|separately preserved|replayable source|git replication)' crates
crates/orna-sys-v1/src/lib.rs:520:    purpose: "Commit the validated staged state, preserving unstaged CWD changes and unpublished tail.",
crates/orna-sys-v1/src/lib.rs:2906:                purpose: "Commit the validated staged state, preserving unstaged CWD changes and unpublished tail.",
crates/orna-runtime-v1/src/lib.rs:2340:/// A finite, replayable source for the built-in list stream contract.
EXIT_CODE=0
```

The restart test was separately located with:

```text
$ rg -n 'list_stream_restart_recovers_at_the_durable_successor' crates/orna-runtime-v1/tests/v1_replay_review_regressions.rs
crates/orna-runtime-v1/tests/v1_replay_review_regressions.rs:789:async fn list_stream_restart_recovers_at_the_durable_successor() {
EXIT_CODE=0
```

Cargo tests were not run for this documentation-only increment.
