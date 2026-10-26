# ORP-1 golden vectors

`orp1-vectors.json` holds the map roots that the Orna 1.1.0 reference model
(`tools/final_model.py`, `ordered_map`, publication `final-2026-10-05`) builds
for nine recipes. The tests in `crates/orna-repository-v1/src/orp.rs` rebuild
each recipe through the repository's ORP-1 builder and require the same root
and the same node object IDs at every height. A root is a Git object ID over
the node's canonical data and its `refs` tree, so an equal root means the
whole map is byte-identical to the model (store-format-3 §3, ORNA-ROW-003).

The recipes cover:

| Case | What it pins |
|---|---|
| `empty` | an empty table is one empty leaf |
| `int-rows-sha1`, `int-rows-sha256` | anchor and 16-entry minimum cuts over 3,000 rows, three heights, both Git hashes |
| `size-bound` | leaves cut before a row would push the node past 65,536 bytes |
| `exact-node-limit` | a leaf of exactly 65,536 bytes, then a row that would make the next leaf 65,537 bytes |
| `shared-blob-refs`, `shared-blob-refs-sha256` | leaves cut by 256 *distinct* native refs when rows share Blob descriptors |
| `long-text-keys` | 3,000-byte keys: leaf and branch nodes cut by size |
| `relations` | the store-root relation map, cut by 256 refs at 128 relations |

## Regenerating

Run from the workspace root, with `REFERENCE_DIR` the unpacked Orna 1.1.0
release directory:

```
python3 scripts/generate_orp1_vectors.py "$REFERENCE_DIR" crates/orna-repository-v1/tests/fixtures/orp1-vectors/orp1-vectors.json
python3 scripts/check_fixture_manifest.py --update
```

The checked-in output was produced with Python 3.14.4 from these release
files (SHA-256 as listed in the release's `SHA256SUMS`):

```
9b09d609f85254303e72b246a40745120edb088962e32a5503945918fe5cb09d  tools/final_model.py
e4b80917780c26a3e54ada43061c73d25a2da4e2dcd4427617136959f4ac66a2  tools/value_model.py
fba09c3f2dac00f5341d3a225bc3d1de7dd38b7640aaf5a977b80ef51c5c4acf  tools/blob_model.py
7b232d68b7e4a6a2e9c225339639165569896b7ee8e1b8bd06e3331af2f73120  profiles/media-1.json
```

Output, reproduced on a second run:

```
b46e6cff6c5641a8b47483c732df6a0dc7e7f02fff540fb74342cfcd5ac5b1f0  orp1-vectors.json
```
