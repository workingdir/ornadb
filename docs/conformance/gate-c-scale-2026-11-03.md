# Gate C scale evidence: the one-million-row case (2026-11-03)

Scope: the `gate-c-million` job and the ignored case `orp1_row_store_one_million_rows` in
`crates/orna-repository-v1/tests/row_store_scale.rs`.

History note: sections up to "Direction" describe the job while it lived in
`.github/workflows/gate-c-scale.yml` and wrote its own `orna-gate-c-evidence/1` record. Since
`d85f4d112` the job runs in `.github/workflows/nightly-gates.yml` and writes an
`ornadb-gate-evidence/1` record through `scripts/gate_evidence.py`, so the C items can select
it. See "Resolution" at the end for what changed and what the case costs now.

Every scheduled run of this job from 2026-10-28 to 2026-11-02 was cancelled at the
350-minute job timeout. No scheduled run has produced a passing record.

## Where the evidence is written

The job log of a cancelled run is not readable. GitHub stores only `UNKNOWN STEP` lines for
it. The evidence artifact is still uploaded, because the upload step runs under `always()`.

```sh
gh run list --workflow gate-c-scale.yml --event schedule --limit 1
gh run download <run-id> -n gate-c-million-evidence-<run-id>
```

The artifact holds `gate-c-million-rows.log` and, since `956d2f343`,
`gate-c-million-rows.evidence.json`.

Before `956d2f343` the job installed a TERM handler but ran the command as a foreground
pipeline, `$command 2>&1 | tee "$log"`. Bash runs a trap only after the foreground pipeline
returns, and a cancelled run kills that pipeline, so the handler never ran and the record was
never written. That is why the artifacts from earlier runs hold a log and no record. The
handler now runs the pipeline in its own process group in the background, waits on it, and
kills the group.

## What the cancelled run measured

Run `41436375054`, cancelled at 350 minutes, recorded:

| Phase | Rows | Time |
|---|---|---|
| `build_index` | 1,000,000 | 4,534 ms |
| `build_fixture` | 1,000,000 | 9,448 ms |
| `run_case` | 1,000,000 | not reached in 5 h 40 m |

The runner killed the still-running `row_store_scale` binary, so the case does not deadlock
and does not run out of time in its fixture. It runs out of time in `run_case`.

## Growth of the case, measured

Measured with `ORNA_ORP1_SCALE_ROWS`, which cuts the ignored case down for local runs. The
gated run never sets it, so gate C still runs the full million. Debug figures are from the
same binary gate C uses; the release column shows that optimisation is not the answer.

| Rows | `build_fixture` | `cold_unindexed_scan` | `run_case` | Promisor fetches |
|---|---|---|---|---|
| 5,000 | 130 ms | 3,188 ms | 4,059 ms | 62 |
| 20,000 | 302 ms | 17,199 ms | 18,757 ms | 259 |
| 100,000 debug | 1,253 ms | 161,054 ms | 166,341 ms | 1,307 |
| 100,000 release | 482 ms | 151,151 ms | 153,263 ms | 1,307 |

`cold_unindexed_scan` is 97 per cent of `run_case`. It grows faster than the row count: 5x
the rows costs 9.4x the time, an exponent of about 1.4. Release is 6 per cent faster than
debug at 100,000 rows, so the cost is not CPU.

## What the cost is, from temporary probes

Temporary probes in `range_rows` and `read_checked_object` (since removed) measured the
20,000-row case directly:

| Measurement | Value |
|---|---|
| `range_rows` calls | 79 |
| Rows per call | 253 |
| Product work in the phase | about 11.2 s of 17.5 s |
| Test-side promisor fetches | 6.3 s, 252 spawns at about 25 ms |
| Object reads through `read_checked_object` | about 1,000, none at or above 5 ms |

So the case makes many small calls. Each call reads a handful of objects through the
persistent `git cat-file --batch` session at roughly 4 to 5 ms per read, then decodes 253
rows. Both the number of reads and the per-row decode work grow with the table.

## What is not the fix

- **A longer timeout.** 350 minutes is already close to the hosted-runner ceiling of 360. The
  scan alone exceeds it at one million rows.
- **A bigger runner.** The cost is round-trips and per-row work, not CPU. Release changes it
  by 6 per cent.
- **`--release` in the workflow.** Same reason.

## Direction

Cut the number of exchanges per page rather than the latency of one exchange: read the leaf
page and the branch pages it needs in one exchange, or cache decoded pages for the length of
one read scope. In the harness, batch the promisor fetches into one `cat-file --batch`
process instead of one spawn per missing object.

There is no profiler on the maintainer host used for this note (`perf` is absent,
`strace` and `ltrace` are not installed, `perf_event_paranoid` is 2), so attribution below
the counts above needs a host with a profiler.

## Resolution (2026-11-03)

The direction above was right about what to cut, and the measurement in this note understated
the harness half of it.

`query_with_hydration` in the harness answered every promised object by fetching that one
object, reopening the read scope and rerunning the same page query, about 3.3 times per page.
Two temporary probes split the cost:

| Measurement | Value |
|---|---|
| one `range_rows`, 256 rows, start / middle / end at 200,000 rows | 10 / 14 / 14 ms |
| 20 `open_read_scope` calls on the partial clone | 0 ms |
| 20 fresh-scope-plus-hydration pages on the clone | 240 ms each |
| 20 pages on a shared scope, local repository | 12 ms each |

So the product's page query was flat and cheap; the retry loop around it was the cost.

`#9516` (`e4b8294c0`) hydrates `Fixture::index_data_blobs` through one `git cat-file --batch`
session before the walk. The per-miss path stays as a fallback.

| Rows | `cold_unindexed_scan` before | after | Test total before | after |
|---|---|---|---|---|
| 50,000 | 57,371 ms | 17,373 ms | 63.9 s | 23.8 s |
| 200,000 | 589,972 ms | 87,597 ms | 608 s | 106 s |
| 1,000,000 | not reached in 5 h 40 m | 918,763 ms | - | 1,005 s |

The one-million-row case now passes locally in 16.8 minutes, every assertion satisfied. The
residual is Git's own promisor fetch: 13,121 objects, 869.9 s of the 918.8 s at one million
rows, about 66 ms each, linear in pages. That is where a further cut would come from; it is no
longer the obstacle to the gate.

Rejected on measurement: batching the spawns while keeping one retry per miss cost 56,671 ms
against 57,371 ms at 50,000 rows.

Measured in CI on the first run with the new wiring, nightly dispatch `41614410003` on
`16bf94193`:

| Field | Value |
|---|---|
| Record state | passed, `timed_out: false` |
| Elapsed | 6,737.8 s (112 minutes) against a 330-minute recorder budget |
| `cold_unindexed_scan` | 6,567,319 ms |
| Promisor fetches | 13,121 objects, 6,537,223 ms, about 498 ms each |
| `build_fixture` | 8,212 ms |

So the hosted runner pays 7.5 times the per-object fetch of the maintainer workstation and
still finishes with a factor of three in hand. The C items moved from not-executed to passed
in that run's summary, and Gate C stays `not-executed` only because C2, C3 and C4 declare
limitations.

Record and selector wiring: `#9517` (`d85f4d112`) and `#9518` (`16bf94193`). The job records
`gate-c-million-rows` with `--timeout-minutes 330` under a 350-minute cap, and C1 to C4 select
`orp1_row_store_one_million_rows` from it. A locally recorded run evaluates as:

```
gate-c-million-rows: passed; 1 passed, 0 failed; 1,045.61 s
C1: tests {"passed": 1}, executed_by [{"record": "gate-c-million-rows", ...}]
```
