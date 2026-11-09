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

## Production-written graph at one million rows (2026-11-09)

C3's remaining limitation was a production-written graph at one million rows. The ten-row
production-writer probe could not be raised: it admits every row as a separate runtime request
through the media-capture fixture. That path costs about 70 ms per row and the cost is flat in
table size, so a million rows is about 19 hours; it also derives request and row identities
from a u16 ordinal, so it cannot name more than 65,536 rows. Stage attribution at 100 rows
(debug, 16-core workstation) puts the product half at about 7 ms per row:

| Stage per row | ms |
|---|---|
| reserve request | 0.8 |
| start request | 0.9 |
| begin activation | 0.15 |
| evaluate the capture expression | 47 |
| accept the captured Blob | 23 |
| commit the request | 5 |

The 70 ms is test-side expression evaluation and Blob capture, not row admission; the three
durable runtime steps are flat (72, 74 and 67 ms per row end to end at 10, 100 and 1,000 rows).

`orp1_row_store_one_million_rows_production_written` admits all rows in one runtime request,
the batch shape a bulk import uses, derives mutation ids from a u64 index, and publishes
through `RuntimePublicationCoordinator`. Measured locally (debug build, the same binary as
gate C):

| Rows | Admission | Publication | Read + node decode | Leaves | Branches | Peak RSS |
|---|---|---|---|---|---|---|
| 1,000 | 0.61 s | 0.70 s | 0.06 s | 7 | 1 | - |
| 10,000 | 4.69 s | 3.78 s | 0.06 s | 121 | 6 | - |
| 50,000 | 21.3 s | 17.3 s | - | 644 | 12 | - |
| 200,000 | 90.4 s | 67.9 s | 0.47 s | 2,568 | 29 | 238 MB |
| 1,000,000 | 564.8 s | 343.1 s | 2.09 s | 12,958 | 163 | 816 MB |

The one-million-row case takes 911.9 s (15.2 minutes) end to end and its index shape matches
the test-side fixture (12,958 leaves, 163 branches). Admission and publication exponents from
10,000 to 200,000 rows are 0.99 and 0.97; publication from 200,000 to one million is 1.01, and
admission is 1.14 (SQLite writes three rows per mutation, so the constant rises slightly).

The nightly `gate-c-million` record runs both ignored cases in one cargo invocation with one
test thread. On a hosted runner the test-side case used 112 minutes, 6.7 times this
workstation; even that factor on the new case (102 minutes) leaves both cases at 214 minutes
inside the 330-minute recorder and the 350-minute job cap.

Index counts are decoded from the committed node data blobs (`[1, kind, ...]` through one
`git cat-file --batch` session) because `NativeGraphContext::object_stats` bounds its walk at
`MAX_RANGE_GRAPH_OBJECTS` (2,048) and a one-million-row store has about 13,130 nodes. That
bound is why the probe decodes the committed nodes itself rather than calling the product
walk. Payload traffic on a production-written graph at one million rows is still unmeasured:
those rows carry no Blob.
