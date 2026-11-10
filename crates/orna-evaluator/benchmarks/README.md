# H0 baseline benchmarks

The three benchmarks of the HIR design (section 7.4), measured on the old
evaluator through its public API. They are the baseline the new executor is
compared against from stage H4 and are re-run unchanged after it.

| benchmark | program | what it measures |
|---|---|---|
| `b1_fib_25` | `fib(25)` (242,785 calls) | recursion and call dispatch |
| `b2_fold_1000000` | a `while` loop summing 1..10^6 | the evaluator, not the list |
| `b3_filter_100000` | 10^5 in-memory rows, `filter(r => r.value > 50) \| count` | the relation operator tree, without SQLite or Git |

The old evaluator memoises admitted named calls inside one activation
(ORNA-CALL-001..003), so `fib(25)` executes 48 calls rather than the design's
242,785 source-level calls. The baseline records executed calls: that is the
count a differential harness observes, and it is stable across runs.

The rows of `b3` are served through the evaluator's relation-source effect
boundary (`admitted_relation_source` / `scan_relation_page`), so the
benchmark measures the operator tree rather than the store.

The design's optional `sum(1..=n)` range variant is not measured: the old
evaluator refuses it with `ORNA-EVAL-TYPE` (a range is not a collection `sum`
accepts), so it has no H0 baseline. Range iteration cost is measured at H6.

## Running

The binary records calls through the `route-trace` feature, so the feature is
required; the example is not built by a default `cargo build`:

```sh
cargo run --release -p orna-evaluator --bin orna_bench --features route-trace \
    > /tmp/bench.json
python3 scripts/check_benchmark_baseline.py \
    --baseline crates/orna-evaluator/benchmarks/baseline.json \
    --results /tmp/bench.json
```

The `hir-benchmarks` job in `.github/workflows/cli-tests.yml` runs both commands
on every pull request and writes the numbers to the job summary.

## What is gated

`baseline.json` records the deterministic counters, `steps` (the evaluator's
`StepBudget` debit) and `calls` (recorded `CallRecord` count). The comparison
fails when a gated counter differs from the baseline or when the set of
benchmarks changes: a new benchmark needs a baseline entry, and a removed one
needs its entry removed in the same change.

Wall time (`wall_ms`) is printed in the results and the job summary and is
never compared: shared CI runners are noisy, and the design gates wall time
loosely only on a pinned runner label.

## Re-recording the baseline

The baseline is a reviewed number, not an output to refresh habitually. Change
it only in a pull request that explains why the counters changed (a deliberate
evaluator change, a moved benchmark, or a design decision), and state the old
and new numbers in the description. Run the benchmark locally first; a single
run is enough because the counters do not depend on machine speed.
