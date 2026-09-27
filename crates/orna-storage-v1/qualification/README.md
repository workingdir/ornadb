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
