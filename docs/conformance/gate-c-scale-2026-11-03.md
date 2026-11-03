# Gate C scale evidence: the one-million-row case (2026-11-03)

Scope: the `gate-c-million` job in `.github/workflows/gate-c-scale.yml` and the ignored
case `orp1_row_store_one_million_rows` in `crates/orna-repository-v1/tests/row_store_scale.rs`.

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
