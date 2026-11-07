"""Unit tests for scripts/orna_legacy.py (python3 -m unittest scripts.test_orna_legacy).

The golden vectors in scripts/golden/ogb1-blob-vectors.json pin the OGB-1 meta
bytes, the blob root, and the chunk object identities. A reference writer in
this file rebuilds each vector from its input, so a passing reader is checked
against the golden bytes and not only against its own output. Tamper hooks
let the tests produce structurally wrong roots and assert that the reader
rejects every one of them.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import tempfile
import unittest
from dataclasses import dataclass
from pathlib import Path

from scripts import orna_legacy as legacy
from scripts.orna_extract import CborMap, ExtractionError, Tag, UnavailableObject, decode_canonical, encode

GOLDEN = Path(__file__).resolve().parent / "golden" / "ogb1-blob-vectors.json"


def deterministic_bytes(count: int) -> bytes:
    """The golden generator: SHA-256 counter expansion with a fixed seed."""
    blocks = (count + 31) // 32
    return b"".join(
        hashlib.sha256(b"orna.blob.fixture.v1\0" + i.to_bytes(8, "big")).digest()
        for i in range(blocks)
    )[:count]


def git_tree_bytes(entries: dict[str, tuple[str, str]]) -> bytes:
    """Independent Git tree serializer (names sort with a trailing slash for subtrees)."""
    ordered = sorted(
        entries.items(),
        key=lambda item: item[0].encode("ascii") + (b"/" if item[1][0] == "tree" else b"\0"),
    )
    out = b""
    for name, (kind, oid) in ordered:
        mode = b"40000" if kind == "tree" else b"100644"
        out += mode + b" " + name.encode("ascii") + b"\0" + bytes.fromhex(oid)
    return out


class MemStore:
    """Git object store with a read log, satisfying the reader's ObjectSource protocol."""

    def __init__(self) -> None:
        self.objects: dict[str, tuple[str, bytes]] = {}
        self.reads: list[str] = []

    def put(self, kind: str, data: bytes) -> str:
        oid = legacy.git_oid(kind, data)
        self.objects[oid] = (kind, data)
        return oid

    def tree(self, entries: dict[str, tuple[str, str]]) -> str:
        return self.put("tree", git_tree_bytes(entries))

    def get(self, oid: str) -> tuple[str, bytes]:
        self.reads.append(oid)
        if oid not in self.objects:
            raise UnavailableObject(f"object {oid} is absent")
        return self.objects[oid]


@dataclass
class Built:
    root: str
    digest: str
    length: int
    data_oid: str
    meta: bytes
    chunks: list[str]


def build(store: MemStore, parts: list[bytes], tamper=None) -> Built:
    """Reference OGB-1 writer. tamper(stage, value) may alter a stage; the default is identity."""
    tamper = tamper or (lambda stage, value: value)
    digest = hashlib.sha256()
    length = 0
    leaves: list[tuple[int, str, bytes | None]] = []
    chunk_oids: list[str] = []
    for part in parts:
        oid = store.put("blob", part)
        chunk_oids.append(oid)
        leaves.append((len(part), oid, hashlib.sha256(part).digest()))
        digest.update(part)
        length += len(part)

    def node(group, height):
        end = 0
        index = []
        tree: dict[str, tuple[str, str]] = {}
        for ordinal, (size, oid, chunk_digest) in enumerate(group):
            end += size
            index.append([end, bytes.fromhex(oid), chunk_digest])
            tree[f"{ordinal:03x}"] = ("blob" if height == 0 else "tree", oid)
        node_height, node_total, node_index = tamper("index", [height, end, index])
        tree["index"] = ("blob", store.put("blob", encode([1, node_height, node_total, node_index])))
        return end, store.tree(tamper("node_tree", tree)), None

    level = [node(leaves[i : i + legacy.FANOUT], 0) for i in range(0, len(leaves), legacy.FANOUT)]
    level = level or [node([], 0)]
    height = 0
    while len(level) > 1:
        height += 1
        level = [node(level[i : i + legacy.FANOUT], height) for i in range(0, len(level), legacy.FANOUT)]
    _, data_oid, _ = level[0]
    meta = encode(
        tamper(
            "meta",
            [1, digest.digest(), length, bytes.fromhex(data_oid)],
        )
    )
    meta_oid = store.put("blob", meta)
    root = store.tree(
        tamper("root_tree", {"meta": ("blob", meta_oid), "data": ("tree", data_oid)})
    )
    return Built(root, digest.hexdigest(), length, data_oid, meta, chunk_oids)


def split(data: bytes, size: int) -> list[bytes]:
    return [data[i : i + size] for i in range(0, len(data), size)]


def vector_input(vector: dict) -> bytes:
    spec = vector["input"]
    prefix = "deterministic_bytes("
    if spec.startswith(prefix):
        return deterministic_bytes(int(spec[len(prefix) : -1]))
    return bytes.fromhex(spec)


class GoldenVectorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.vectors = json.loads(GOLDEN.read_text())["vectors"]

    def test_every_vector_reproduces_the_golden_root(self) -> None:
        self.assertGreaterEqual(len(self.vectors), 4)
        for vector in self.vectors:
            with self.subTest(vector=vector["name"]):
                data = vector_input(vector)
                self.assertEqual(hashlib.sha256(data).hexdigest(), vector["sha256"])
                parts, pos = [], 0
                for chunk in vector["chunks"]:
                    parts.append(data[pos : pos + chunk["length"]])
                    pos += chunk["length"]
                self.assertEqual(pos, len(data))
                store = MemStore()
                built = build(store, parts)
                self.assertEqual(built.meta.hex(), vector["meta_hex"])
                self.assertEqual(built.root, vector["ogb_root_sha1"])
                self.assertEqual(built.chunks, [chunk["git_oid"] for chunk in vector["chunks"]])
                self.assertEqual(legacy.OgbReader(store).verify(built.root), vector["sha256"])
                self.assertEqual(
                    legacy.decode_rov2(bytes.fromhex(vector["stored_reference_cbor"])),
                    legacy.StoredBlob(vector["sha256"], vector["length"]),
                )


class ReaderTests(unittest.TestCase):
    def setUp(self) -> None:
        self.data = deterministic_bytes(300000)
        self.store = MemStore()
        self.built = build(self.store, split(self.data, 65536))
        self.reader = legacy.OgbReader(self.store)

    def test_range_matches_plain_slices(self) -> None:
        for start, count in [(0, 1), (65530, 100), (300000, 100), (299999, 500), (0, 0), (131072, 65536)]:
            with self.subTest(start=start, count=count):
                self.assertEqual(
                    self.reader.read_range(self.built.root, start, count),
                    self.data[start : start + count],
                )

    def test_range_rejects_out_of_bounds_and_oversized_reads(self) -> None:
        for start, count in [(-1, 0), (300001, 0), (0, -1), (0, legacy.MAX_READ + 1)]:
            with self.subTest(start=start, count=count):
                with self.assertRaises(legacy.LegacyBlobError):
                    self.reader.read_range(self.built.root, start, count)

    def test_range_reads_only_overlapping_chunks(self) -> None:
        self.store.reads.clear()
        self.assertEqual(self.reader.read_range(self.built.root, 65530, 20), self.data[65530:65550])
        fetched = set(self.store.reads) & set(self.built.chunks)
        self.assertEqual(fetched, {self.built.chunks[0], self.built.chunks[1]})

    def test_descriptor_reads_no_chunks(self) -> None:
        self.store.reads.clear()
        descriptor = self.reader.descriptor(self.built.root)
        self.assertEqual((descriptor.digest, descriptor.length), (self.built.digest, 300000))
        self.assertFalse(set(self.store.reads) & set(self.built.chunks))

    def test_verify_returns_the_content_digest(self) -> None:
        self.assertEqual(self.reader.verify(self.built.root), hashlib.sha256(self.data).hexdigest())

    def test_empty_blob(self) -> None:
        store = MemStore()
        built = build(store, [])
        reader = legacy.OgbReader(store)
        self.assertEqual(reader.read_range(built.root, 0, 10), b"")
        self.assertEqual(reader.verify(built.root), hashlib.sha256(b"").hexdigest())

    def test_two_level_index_round_trips(self) -> None:
        data = deterministic_bytes(257 * 1024)
        store = MemStore()
        built = build(store, split(data, 1024))
        index_oid = legacy.parse_git_tree(store.objects[built.data_oid][1], 20)["index"][1]
        root_index = decode_canonical(store.objects[index_oid][1], "root index", frozenset())
        self.assertEqual(root_index[1], 1)
        reader = legacy.OgbReader(store)
        self.assertEqual(reader.verify(built.root), hashlib.sha256(data).hexdigest())
        self.assertEqual(
            reader.read_range(built.root, 256 * 1024 - 5, 20),
            data[256 * 1024 - 5 : 256 * 1024 + 15],
        )

    def test_missing_chunk_is_unavailable_not_empty(self) -> None:
        store = MemStore()
        built = build(store, [b"abc"])
        store.objects.pop(built.chunks[0])
        reader = legacy.OgbReader(store)
        self.assertEqual(reader.descriptor(built.root).length, 3)
        with self.assertRaises(UnavailableObject):
            reader.read_range(built.root, 0, 3)
        with self.assertRaises(UnavailableObject):
            reader.verify(built.root)

    def test_substituted_chunk_is_rejected(self) -> None:
        store = MemStore()
        built = build(store, [b"abc"])
        store.objects[built.chunks[0]] = ("blob", b"xyz")
        with self.assertRaises(legacy.LegacyBlobError):
            legacy.OgbReader(store).read_range(built.root, 0, 3)

    def test_wrong_object_kind_is_rejected(self) -> None:
        store = MemStore()
        built = build(store, [b"abc"])
        store.objects[built.chunks[0]] = ("tree", store.objects[built.chunks[0]][1])
        with self.assertRaises(legacy.LegacyBlobError):
            legacy.OgbReader(store).verify(built.root)


class TamperTests(unittest.TestCase):
    """Each case builds a structurally wrong root and requires a rejection."""

    PARTS = [b"abcdef", b"ghi"]

    def expect_rejected(self, tamper, reader_call=lambda reader, root: reader.verify(root)) -> None:
        store = MemStore()
        built = build(store, self.PARTS, tamper=tamper)
        with self.assertRaises(legacy.LegacyBlobError):
            reader_call(legacy.OgbReader(store), built.root)

    def test_meta_length_wrong(self) -> None:
        def tamper(stage, value):
            if stage == "meta":
                value[2] += 1
            return value

        self.expect_rejected(tamper)

    def test_meta_digest_wrong_width(self) -> None:
        def tamper(stage, value):
            if stage == "meta":
                value[1] = bytes(31)
            return value

        self.expect_rejected(tamper)

    def test_meta_length_above_limit(self) -> None:
        def tamper(stage, value):
            if stage == "meta":
                value[2] = legacy.MAX_LENGTH + 1
            return value

        self.expect_rejected(tamper)

    def test_meta_not_version_one(self) -> None:
        def tamper(stage, value):
            if stage == "meta":
                value[0] = 2
            return value

        self.expect_rejected(tamper)

    def test_meta_data_oid_does_not_match_data_tree(self) -> None:
        def tamper(stage, value):
            if stage == "meta":
                value[3] = bytes(20)
            return value

        self.expect_rejected(tamper)

    def test_chunk_digest_wrong(self) -> None:
        def tamper(stage, value):
            if stage == "index" and value[0] == 0:
                value[2][0][2] = bytes(32)
            return value

        self.expect_rejected(tamper)

    def test_index_offsets_not_increasing(self) -> None:
        def tamper(stage, value):
            if stage == "index" and value[0] == 0:
                value[2][1][0] = value[2][0][0]
            return value

        self.expect_rejected(tamper)

    def test_index_total_does_not_reach_entries(self) -> None:
        def tamper(stage, value):
            if stage == "index" and value[0] == 0:
                value[1] += 1
            return value

        self.expect_rejected(tamper)

    def test_node_total_checked_without_parent_expectation(self) -> None:
        # The parent length check (_node expected_length) would catch a wrong total first, so
        # call _node with no expectation to prove the node's own entries-vs-total check fires.
        def tamper(stage, value):
            if stage == "index" and value[0] == 0:
                value[1] += 1
            return value

        store = MemStore()
        built = build(store, self.PARTS, tamper=tamper)
        with self.assertRaises(legacy.LegacyBlobError):
            legacy.OgbReader(store)._node(built.data_oid, None, None)

    def test_index_entry_oid_not_the_child(self) -> None:
        def tamper(stage, value):
            if stage == "index" and value[0] == 0:
                value[2][0][1] = bytes(20)
            return value

        self.expect_rejected(tamper)

    def test_height_above_limit(self) -> None:
        def tamper(stage, value):
            if stage == "index" and value[0] == 0:
                return [9, value[1], value[2]]
            return value

        self.expect_rejected(tamper)

    def test_extra_child_name(self) -> None:
        def tamper(stage, value):
            if stage == "node_tree":
                value["zzz"] = value["000"]
            return value

        self.expect_rejected(tamper)

    def test_missing_child_name(self) -> None:
        def tamper(stage, value):
            if stage == "node_tree":
                del value["001"]
            return value

        self.expect_rejected(tamper)

    def test_extra_root_entry(self) -> None:
        def tamper(stage, value):
            if stage == "root_tree":
                value["x"] = value["meta"]
            return value

        self.expect_rejected(tamper)

    def test_internal_entry_digest_must_be_null(self) -> None:
        parts = [bytes([i % 256]) for i in range(257)]

        def tamper(stage, value):
            if stage == "index" and value[0] == 1:
                value[2][0][2] = bytes(32)
            return value

        store = MemStore()
        built = build(store, parts, tamper=tamper)
        with self.assertRaises(legacy.LegacyBlobError):
            legacy.OgbReader(store).verify(built.root)

    def test_chunk_above_one_mebibyte_is_rejected(self) -> None:
        store = MemStore()
        built = build(store, [bytes(legacy.MAX_CHUNK + 1)])
        with self.assertRaises(legacy.LegacyBlobError):
            legacy.OgbReader(store).verify(built.root)


class TreeTests(unittest.TestCase):
    def test_canonical_tree_parses(self) -> None:
        data = b"100644 a\0" + bytes(20) + b"100644 b\0" + bytes(20)
        self.assertEqual(sorted(legacy.parse_git_tree(data, 20)), ["a", "b"])

    def test_non_canonical_tree_order_is_rejected(self) -> None:
        data = b"100644 b\0" + bytes(20) + b"100644 a\0" + bytes(20)
        with self.assertRaises(legacy.LegacyBlobError):
            legacy.parse_git_tree(data, 20)

    def test_subtree_sorts_with_trailing_slash(self) -> None:
        # "a.b" sorts before the subtree "a" (compared as "a/"), so the reverse is non-canonical.
        ordered = b"100644 a.b\0" + bytes(20) + b"40000 a\0" + bytes(20)
        self.assertEqual(sorted(legacy.parse_git_tree(ordered, 20)), ["a", "a.b"])
        with self.assertRaises(legacy.LegacyBlobError):
            legacy.parse_git_tree(b"40000 a\0" + bytes(20) + b"100644 a.b\0" + bytes(20), 20)

    def test_duplicate_and_unadmitted_modes_are_rejected(self) -> None:
        for data in (
            b"100644 a\0" + bytes(20) + b"100644 a\0" + bytes(20),
            b"100755 a\0" + bytes(20),
            b"100644 \0" + bytes(20),
        ):
            with self.subTest(data=data[:12]):
                with self.assertRaises(legacy.LegacyBlobError):
                    legacy.parse_git_tree(data, 20)


class Rov1Rov2Tests(unittest.TestCase):
    def test_ovb1_accepts_registered_value_tag(self) -> None:
        self.assertEqual(legacy.decode_ovb1(encode(Tag(60000, [1, 0]))), Tag(60000, [1, 0]))

    def test_ovb1_rejects_profile_and_storage_tags(self) -> None:
        for number in (60100, 60110, 60111, 60112, 60113):
            with self.subTest(tag=number):
                with self.assertRaises(ExtractionError):
                    legacy.decode_ovb1(encode(Tag(number, 1)))

    def test_rov2_slot_accepts_only_storage_tag_payload(self) -> None:
        self.assertEqual(
            legacy.decode_rov2(encode(Tag(60100, [bytes(32), 5]))),
            legacy.StoredBlob("00" * 32, 5),
        )
        bad_slots = [
            Tag(60100, [bytes(31), 5]),
            Tag(60100, [bytes(32), -1]),
            Tag(60100, [bytes(32), 2**63]),
            Tag(60100, [bytes(32)]),
            Tag(60100, [bytes(32), 5, 0]),
            Tag(60000, 1),
            b"abc",
        ]
        for value in bad_slots:
            with self.subTest(value=value):
                with self.assertRaises(ExtractionError):
                    legacy.decode_rov2(encode(value))


def cwd_snapshot_digest(database: Tag, runtime: Tag, generation: int) -> bytes:
    """SHA-256 of 'orna.snapshot.v1', a zero byte and the structural encoding of a CWD descriptor."""
    return hashlib.sha256(b"orna.snapshot.v1\x00" + encode([0, database, runtime, generation])).digest()


class RegistryValueTests(unittest.TestCase):
    """Per-tag registry shapes and constraints for decode_ovb1 (ROV-1 value codec)."""

    UU = Tag(37, bytes(16))
    VALID_VALUES = [
        Tag(37, bytes(16)),
        Tag(60000, [0, 0]),
        Tag(60000, [-15, -1]),
        Tag(60001, "2024-02-29"),
        Tag(60002, [0, 0]),
        Tag(60002, [-1, 999999999]),
        Tag(60003, "2024-02-29T23:59:59.123456789"),
        Tag(60004, "Europe/London"),
        Tag(60005, [-1, 5]),
        Tag(60006, [Tag(60000, [1, 0]), Tag(37, bytes(16))]),
        Tag(60007, [Tag(60000, [5, -2]), Tag(37, bytes(16))]),
        Tag(60008, [Tag(37, bytes(16)), Tag(37, bytes(16)), None]),
        Tag(60008, [Tag(37, bytes(16)), Tag(37, bytes(16)), Tag(60009, [None, []])]),
        Tag(60009, [None, []]),
        Tag(60009, [Tag(37, bytes(16)), [["name", Tag(60000, [1, 0])]]]),
        Tag(60010, [Tag(37, bytes(16)), Tag(37, bytes(16)), 7, [0, Tag(37, bytes(16)), Tag(37, bytes(16)), 1, cwd_snapshot_digest(Tag(37, bytes(16)), Tag(37, bytes(16)), 1)]]),
        Tag(60010, [Tag(37, bytes(16)), Tag(37, bytes(16)), 7, [1, Tag(37, bytes(16)), "sha256", bytes(32)]]),
        Tag(60011, CborMap(())),
        Tag(60012, [1, None, CborMap(()), []]),
        Tag(60013, [0]),
        Tag(60013, [1, 7]),
        Tag(60014, []),
        Tag(60015, [1, "x"]),
        Tag(60016, CborMap(((0, "code"), (1, "msg"), (2, []), (3, CborMap((("k", 1),)))))),
        Tag(60017, 0),
        Tag(60017, 86399999999999),
        Tag(60018, [Tag(60002, [0, 0]), Tag(60004, "UTC"), 0]),
        Tag(60019, [Tag(60013, [0]), Tag(60013, [1, Tag(60000, [1, 0])]), True]),
        Tag(60020, [Tag(37, bytes(16)), 5]),
        Tag(60021, [Tag(37, bytes(16)), Tag(37, bytes(16)), "k"]),
        Tag(60022, [Tag(37, bytes(16)), Tag(37, bytes(16)), "k", False]),
        Tag(60023, ["sys.DatabaseId", Tag(37, bytes(16))]),
        Tag(60023, ["sys.RevisionId", bytes(32)]),
        Tag(60023, ["sys.CheckpointVersion", 0]),
        Tag(60023, ["sys.GitOid", ["sha1", bytes(20)]]),
        Tag(60023, ["sys.GitOid", ["sha256", bytes(32)]]),
        Tag(60024, ["repo", "src/main.rs"]),
        Tag(60024, ["posix", "/etc/hosts"]),
        Tag(60024, ["windows", "C:\\Users"]),
        Tag(60025, ["sha256", bytes(32)]),
    ]
    INVALID_VALUES = [
        Tag(37, bytes(15)),
        Tag(60000, [1]),
        Tag(60000, [True, 0]),
        Tag(60000, [0, 1]),
        Tag(60000, [10, 0]),
        Tag(60001, "2023-02-29"),
        Tag(60001, "20240101"),
        Tag(60002, [0, 1000000000]),
        Tag(60002, [0, -1]),
        Tag(60003, "2024-01-01T00:00:00"),
        Tag(60003, "2024-01-01T00:00:00.12345678"),
        Tag(60003, "2024-01-01T00:00:60.000000000"),
        Tag(60003, "2024-01-01T00:00:00.000000000Z"),
        Tag(60004, ""),
        Tag(60005, [0, 1000000000]),
        Tag(60006, [Tag(60000, [1, 0]), b"x"]),
        Tag(60007, [1, Tag(37, bytes(16))]),
        Tag(60008, [Tag(37, bytes(16)), Tag(37, bytes(16)), 1]),
        Tag(60009, [None, [[1]]]),
        Tag(60010, [Tag(37, bytes(16)), Tag(37, bytes(16)), 1, [0]]),
        Tag(60011, [1]),
        Tag(60012, [1, None, CborMap(()), "x"]),
        Tag(60013, [2]),
        Tag(60013, [False]),
        Tag(60014, [0]),
        Tag(60015, []),
        Tag(60016, CborMap(((0, "code"),))),
        Tag(60017, 86400000000000),
        Tag(60018, [Tag(60002, [0, 0]), Tag(60002, [0, 0]), 0]),
        Tag(60019, [Tag(60013, [0]), Tag(60013, [0]), 1]),
        Tag(60020, [b"x", 1]),
        Tag(60021, [Tag(37, bytes(16)), Tag(37, bytes(16))]),
        Tag(60022, [Tag(37, bytes(16)), Tag(37, bytes(16)), "k", True]),
        Tag(60023, ["sys.Unknown", Tag(37, bytes(16))]),
        Tag(60023, ["sys.RevisionId", Tag(37, bytes(16))]),
        Tag(60023, ["sys.CheckpointVersion", -1]),
        Tag(60023, ["sys.GitOid", ["sha1", bytes(32)]]),
        Tag(60024, ["unknown", "x"]),
        Tag(60024, ["repo", "/abs"]),
        Tag(60024, ["repo", "a/../b"]),
        Tag(60024, ["repo", "a//b"]),
        Tag(60024, ["repo", "a\\b"]),
        Tag(60025, ["sha1", bytes(32)]),
        Tag(60025, ["sha256", bytes(31)]),
        Tag(60013, [1, Tag(60000, [0, 1])]),
        Tag(60010, [Tag(37, bytes(16)), Tag(37, bytes(16)), 7, [0, Tag(37, bytes(16)), Tag(37, bytes(16)), 1, bytes(32)]]),
        Tag(60009, [None, [["name", Tag(60000, [1, 0])], ["name", Tag(60000, [2, 0])]]]),
        Tag(60009, [None, [["e\u0301", Tag(60000, [1, 0])]]]),
    ]

    def test_valid_registered_values_are_accepted(self) -> None:
        for value in self.VALID_VALUES:
            with self.subTest(value=value):
                self.assertEqual(legacy.decode_ovb1(encode(value)), value)

    def test_invalid_registered_values_are_rejected(self) -> None:
        for value in self.INVALID_VALUES:
            with self.subTest(value=value):
                with self.assertRaises(legacy.LegacyBlobError):
                    legacy.decode_ovb1(encode(value))


class Orr2Tests(unittest.TestCase):
    DIGEST = "sha256:" + "ab" * 32

    def test_valid_record(self) -> None:
        self.assertEqual(
            legacy.orr2_reference([("blob", self.DIGEST), ("length", 7)]),
            legacy.StoredBlob("ab" * 32, 7),
        )

    def test_malformed_records_are_rejected(self) -> None:
        cases = {
            "missing length": [("blob", self.DIGEST)],
            "extra field": [("blob", self.DIGEST), ("length", 1), ("x", 1)],
            "duplicate field": [("blob", self.DIGEST), ("blob", self.DIGEST), ("length", 1)],
            "uppercase digest": [("blob", "sha256:" + "AB" * 32), ("length", 1)],
            "wrong algorithm": [("blob", "sha1:" + "ab" * 32), ("length", 1)],
            "short digest": [("blob", "sha256:" + "ab" * 31), ("length", 1)],
            "negative length": [("blob", self.DIGEST), ("length", -1)],
            "boolean length": [("blob", self.DIGEST), ("length", True)],
            "text length": [("blob", self.DIGEST), ("length", "7")],
            "length above limit": [("blob", self.DIGEST), ("length", 2**63)],
        }
        for name, pairs in cases.items():
            with self.subTest(case=name):
                with self.assertRaises(legacy.LegacyBlobError):
                    legacy.orr2_reference(pairs)

    def test_inline_blob_is_canonical_padded_base64(self) -> None:
        self.assertEqual(legacy.orr2_inline_bytes("YWJj"), b"abc")
        self.assertEqual(legacy.orr2_inline_bytes("YQ=="), b"a")
        self.assertEqual(legacy.orr2_inline_bytes(""), b"")

    def test_non_canonical_inline_blob_is_rejected(self) -> None:
        for text in ("YQ", "YR==", "YQ==\n", "Y Q==", "YWJj="):
            with self.subTest(text=text):
                with self.assertRaises(legacy.LegacyBlobError):
                    legacy.orr2_inline_bytes(text)


class RowUnitTests(unittest.TestCase):
    REPO = Path(__file__).resolve().parent.parent
    FIXTURES = REPO / "crates" / "orna-cli" / "tests" / "fixtures"
    BUNDLE = FIXTURES / "legacy-format1.bundle"
    ALICE = {"name": "Alice Smith", "emails": ["alice@example.com"]}
    BOB = {"name": "Bob Jones", "emails": []}

    def test_golden_contact_rows_from_the_committed_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            clone = Path(tmp) / "legacy"
            subprocess.run(["git", "clone", "-q", str(self.BUNDLE), str(clone)], check=True)
            for name, expected in (("alice-smith", self.ALICE), ("bob-jones", self.BOB)):
                committed = subprocess.run(
                    ["git", "-C", str(clone), "show", f"HEAD:contacts/Contact/{name}.orna"],
                    check=True,
                    capture_output=True,
                ).stdout
                fixture = (self.FIXTURES / f"legacy-loose-contact-{name}.orna").read_bytes()
                self.assertEqual(committed, fixture)
                self.assertEqual(legacy.parse_row_unit(committed), expected)

    def test_inline_blob_field_of_the_alice_row(self) -> None:
        row = legacy.parse_row_unit(b'{\n    name: "Alice Smith",\n    avatar: "aGVsbG8=",\n}\n')
        self.assertEqual(legacy.row_blob_field(row, "avatar"), b"hello")

    def test_storage_record_blob_field(self) -> None:
        digest = hashlib.sha256(b"hello").hexdigest()
        row = legacy.parse_row_unit(
            ('{ avatar: { blob: "sha256:' + digest + '", length: 5 }, }').encode()
        )
        self.assertEqual(legacy.row_blob_field(row, "avatar"), legacy.StoredBlob(digest, 5))

    def test_accepts_the_engine_row_grammar(self) -> None:
        cases = {
            "empty record": (b"{}", {}),
            "trailing comma": (b"{ a: 1, }", {"a": 1}),
            "trailing semicolon": (b"{ a: 1 };\n", {"a": 1}),
            "nested comments": (
                b"// head\n{ /* x /* nested */ y */ a: 1, // tail\n b: null }\n",
                {"a": 1, "b": None},
            ),
            "literals": (
                b'{ t: true, f: false, n: -0, s: "x", arr: [], nested: { k: [1, "two",], }, }',
                {"t": True, "f": False, "n": 0, "s": "x", "arr": [], "nested": {"k": [1, "two"]}},
            ),
            "escapes": (
                r'{ s: "q\"b\\n\n\t\0\u{1F600}\u{41}" }'.encode(),
                {"s": "q\"b\\n\n\t\0\U0001F600A"},
            ),
        }
        for name, (data, expected) in cases.items():
            with self.subTest(case=name):
                self.assertEqual(legacy.parse_row_unit(data), expected)

    def test_rejects_what_the_engine_row_grammar_does_not_admit(self) -> None:
        cases = {
            "not a record": b"[1]",
            "two records": b"{ a: 1 } { b: 2 }",
            "trailing text": b"{ a: 1 } x",
            "missing colon": b"{ a 1 }",
            "missing comma": b"{ a: 1 b: 2 }",
            "leading comma": b"{ , a: 1 }",
            "duplicate field": b"{ a: 1, a: 2 }",
            "keyword field": b"{ true: 1 }",
            "identifier value": b"{ a: b }",
            "expression value": b"{ a: 1 + 2 }",
            "leading zero": b"{ a: 007 }",
            "float": b"{ a: 1.5 }",
            "interpolated string": b'{ a: "x{1}" }',
            "bad escape": r'{ a: "\q" }'.encode(),
            "unicode without brace": r'{ a: "\u41" }'.encode(),
            "surrogate escape": r'{ a: "\u{D800}" }'.encode(),
            "empty unicode escape": r'{ a: "\u{}" }'.encode(),
            "unterminated string": b'{ a: "x }',
            "unterminated comment": b"{ a: 1 } /* open",
            "unterminated record": b"{ a: 1",
            "not utf-8": b'{ a: "\xff" }',
            "byte order mark": b"\xef\xbb\xbf{ a: 1 }",
        }
        for name, data in cases.items():
            with self.subTest(case=name):
                with self.assertRaises(legacy.LegacyBlobError):
                    legacy.parse_row_unit(data)

    def test_blob_field_rejects_absent_and_non_blob_values(self) -> None:
        digest = "sha256:" + "ab" * 32
        cases = {
            "absent": {},
            "integer": {"avatar": 5},
            "boolean": {"avatar": True},
            "list": {"avatar": []},
            "record with extra field": {"avatar": {"blob": digest, "length": 1, "x": 1}},
            "record without length": {"avatar": {"blob": digest}},
            "non-canonical inline": {"avatar": "YQ"},
        }
        for name, row in cases.items():
            with self.subTest(case=name):
                with self.assertRaises(legacy.LegacyBlobError):
                    legacy.row_blob_field(row, "avatar")


if __name__ == "__main__":
    unittest.main()
