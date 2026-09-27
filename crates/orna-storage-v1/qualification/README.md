# Compact stable-storage qualification gate

ORNA-COMPACT-013 limits stable compact storage to at most 1.5 times the same
corpus stored in Prometheus blocks. `verify_stable_storage.py` checks that
threshold using integer arithmetic and rejects measurements whose corpus
SHA-256 identifiers differ.

Provide a JSON file with the measured stable byte counts and the digest of the
logical corpus used to produce each measurement:

```json
{
  "compact": {
    "corpus_sha256": "<64 lowercase hexadecimal characters>",
    "stable_bytes": 1500
  },
  "prometheus_blocks": {
    "corpus_sha256": "<same 64 lowercase hexadecimal characters>",
    "stable_bytes": 1000
  }
}
```

Run the gate with:

```sh
python3 crates/orna-storage-v1/qualification/verify_stable_storage.py profile.json
```

Exit status is `0` for a valid ratio at or below 1.5, `1` when the ratio is
over the limit, and `2` when the evidence file is malformed or does not
identify the same corpus. This gate checks submitted measurements; it does not
collect benchmark results or establish a production qualification by itself.

## Compact row-integrity evidence

`verify_compact_integrity.py` checks the ORNA-COMPACT-013 evidence criterion
that each acknowledged logical row appears exactly once in compact output.
Supply the logical row IDs from the submitted evidence in both arrays:

```json
{
  "acknowledged_rows": ["row-001", "row-002"],
  "compact_rows": ["row-002", "row-001"]
}
```

Run it with:

```sh
python3 crates/orna-storage-v1/qualification/verify_compact_integrity.py profile.json
```

Each array must contain at least one ID. Exit status is `0` when the ID sets
match and both arrays contain unique IDs,
`2` when the submitted evidence is malformed or fails the criterion. The gate
validates submitted IDs; it does not collect rows, establish evidence
provenance, or claim that a 24-hour production profile was run.

## Compact bounded-memory evidence

`verify_compact_memory.py` checks the ORNA-COMPACT-013 bounded-memory
criterion against a budget declared for the same profile run. Provide positive
integer byte counts for the run's declared memory budget and measured peak RSS:

```json
{
  "memory_budget_bytes": 1073741824,
  "peak_rss_bytes": 734003200
}
```

Run it with:

```sh
python3 crates/orna-storage-v1/qualification/verify_compact_memory.py profile.json
```

Exit status is `0` when peak RSS is at or below the declared budget, `1` when
valid evidence shows that peak RSS exceeds the budget, and `2` when the
evidence is malformed. The specification sets no universal byte limit, so the
budget is supplied per run. This gate checks submitted measurements; it does
not collect memory usage, establish evidence provenance, or claim that a
24-hour production profile was run.
