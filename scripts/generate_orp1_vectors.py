#!/usr/bin/env python3
"""Generate the ORP-1 golden vectors from the Orna 1.1.0 reference model.

Usage:
    python3 scripts/generate_orp1_vectors.py REFERENCE_DIR OUTPUT_JSON

REFERENCE_DIR is the unpacked Orna 1.1.0 release; its tools/final_model.py
builds every map with `ordered_map`, the pinned ORP-1 partition
(store-format-3 section 3). Each case is described by a recipe that the Rust
test rebuilds byte for byte. The output records the model's root and the
object id of every node by height, so an equal root proves byte-identical
node data and Git envelopes for the whole map.
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

ROWS_DOMAIN_RELATION = bytes([0x11]) * 16
ROWS_DOMAIN_SCHEMA = bytes([0x22]) * 32
RELATIONS_DATABASE = bytes([0x33]) * 16
BLOB_MEDIA_TYPE = "application/octet-stream"


def blob_pool_entry(index: int, algorithm: str):
    """The stored Blob reference of one pool member: ROV-3 tag 60111."""
    width = 20 if algorithm == "sha1" else 32
    oid = hashlib.sha256(b"orna orp1 vector descriptor %d" % index).digest()[:width]
    return [
        index + 1,
        hashlib.sha256(b"orna orp1 vector payload %d" % index).digest(),
        BLOB_MEDIA_TYPE,
        None,
        oid,
    ]


def relation_value(index: int, algorithm: str):
    width = 20 if algorithm == "sha1" else 32
    schema = hashlib.sha256(b"orna orp1 vector schema %d" % index).digest()[:width]
    primary = hashlib.sha256(b"orna orp1 vector primary %d" % index).digest()[:width]
    return [schema, primary, None, index]


def case_entries(case: dict, model) -> tuple[list, list]:
    """The domain and the `[key, value]` entries one recipe names."""
    Tag = model.Tag
    algorithm = case["algorithm"]
    recipe = case["recipe"]
    rows_domain = ["rows", ROWS_DOMAIN_RELATION, ROWS_DOMAIN_SCHEMA]
    if recipe == "int-rows":
        rows = case["rows"]
        return rows_domain, [[i, [i % 251, "row %d" % i]] for i in range(rows)]
    if recipe == "text-lengths":
        return rows_domain, [
            [i, [chr(97 + i % 26) * length]]
            for i, length in enumerate(case["text_lengths"])
        ]
    if recipe == "shared-blobs":
        pool = case["pool"]
        per_row = case["refs_per_row"]
        stride = case["stride"]
        entries = []
        for i in range(case["rows"]):
            members = [(i * stride + k) % pool for k in range(per_row)]
            entries.append(
                [i, [Tag(60111, blob_pool_entry(member, algorithm)) for member in members]]
            )
        return rows_domain, entries
    if recipe == "long-text-keys":
        width = case["key_length"]
        return rows_domain, [
            [("%05d" % i) + "k" * (width - 5), [i]] for i in range(case["rows"])
        ]
    if recipe == "relations":
        entries = [
            [hashlib.sha256(b"orna orp1 vector relation %d" % i).digest()[:16],
             relation_value(i, algorithm)]
            for i in range(case["rows"])
        ]
        return ["relations", RELATIONS_DATABASE], entries
    raise SystemExit("unknown recipe " + recipe)


def node_levels(store, model, root: str) -> list[list[str]]:
    """Every node id of the map below `root`, grouped by height, in key order."""
    levels: dict[int, list[str]] = {}
    frontier = [root]
    while frontier:
        following = []
        for oid in frontier:
            node = store.inspect_node(oid)
            height = 0 if node[1] == 1 else node[3]
            levels.setdefault(height, []).append(oid)
            if node[1] == 2:
                following.extend(child.hex() for _, child, _ in node[4])
        frontier = following
    return [levels[height] for height in sorted(levels)]


def size_bound_lengths() -> list[int]:
    """Text lengths that keep each row inline yet cut leaves by node size."""
    return [6000 + (i * 997) % 2100 for i in range(48)]


def exact_limit_lengths(model) -> list[int]:
    """A first leaf of exactly 65,536 encoded bytes, then a leaf whose ninth
    row would make it 65,537 bytes, so that row opens a third leaf."""
    encode = model.encode
    domain = encode(["rows", ROWS_DOMAIN_RELATION, ROWS_DOMAIN_SCHEMA])
    fixed = 3 + len(domain) + 1  # [1, 1, domain, entries] with fewer than 24 entries

    def entry_size(index: int, length: int) -> int:
        return 1 + len(encode(index)) + len(encode([chr(97 + index % 26) * length]))

    lengths: list[int] = []
    # First leaf: nine rows filling the node to exactly 65,536 bytes.
    for index in range(8):
        lengths.append(7200)
    used = fixed + sum(entry_size(i, lengths[i]) for i in range(8))
    last = 7200
    while used + entry_size(8, last) < 65536:
        last += 1
    while used + entry_size(8, last) > 65536:
        last -= 1
    assert used + entry_size(8, last) == 65536
    lengths.append(last)
    # Second leaf: eight rows, then a ninth row one byte too large for it.
    start = len(lengths)
    for offset in range(8):
        lengths.append(7300)
    used = fixed + sum(entry_size(start + i, lengths[start + i]) for i in range(8))
    spill = 7000
    while used + entry_size(start + 8, spill) < 65537:
        spill += 1
    while used + entry_size(start + 8, spill) > 65537:
        spill -= 1
    assert used + entry_size(start + 8, spill) == 65537
    lengths.append(spill)
    lengths.append(100)
    return lengths


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: generate_orp1_vectors.py REFERENCE_DIR OUTPUT_JSON")
    reference = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2])
    sys.path.insert(0, str(reference / "tools"))
    import final_model as model  # noqa: E402  (the reference model is located at run time)

    cases = [
        {"id": "empty", "algorithm": "sha1", "recipe": "int-rows", "rows": 0},
        {"id": "int-rows-sha1", "algorithm": "sha1", "recipe": "int-rows", "rows": 3000},
        {"id": "int-rows-sha256", "algorithm": "sha256", "recipe": "int-rows", "rows": 3000},
        {
            "id": "size-bound",
            "algorithm": "sha1",
            "recipe": "text-lengths",
            "text_lengths": size_bound_lengths(),
        },
        {
            "id": "exact-node-limit",
            "algorithm": "sha1",
            "recipe": "text-lengths",
            "text_lengths": exact_limit_lengths(model),
        },
        {
            "id": "shared-blob-refs",
            "algorithm": "sha1",
            "recipe": "shared-blobs",
            "rows": 60,
            "pool": 600,
            "refs_per_row": 30,
            "stride": 13,
        },
        {
            "id": "shared-blob-refs-sha256",
            "algorithm": "sha256",
            "recipe": "shared-blobs",
            "rows": 60,
            "pool": 600,
            "refs_per_row": 30,
            "stride": 13,
        },
        {
            "id": "long-text-keys",
            "algorithm": "sha1",
            "recipe": "long-text-keys",
            "rows": 700,
            "key_length": 3000,
        },
        {"id": "relations", "algorithm": "sha1", "recipe": "relations", "rows": 300},
    ]
    for case in cases:
        store = model.Store(case["algorithm"])
        domain, entries = case_entries(case, model)
        root = model.ordered_map(store, domain, entries)
        case["root"] = root
        case["levels"] = node_levels(store, model, root)
    document = {
        "scope": "ORP-1 golden roots from tools/final_model.py ordered_map; "
        "recipes are rebuilt by orp.rs tests",
        "rows_domain": {
            "relation_hex": ROWS_DOMAIN_RELATION.hex(),
            "schema_digest_hex": ROWS_DOMAIN_SCHEMA.hex(),
        },
        "relations_database_hex": RELATIONS_DATABASE.hex(),
        "cases": cases,
    }
    output.write_text(json.dumps(document, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
