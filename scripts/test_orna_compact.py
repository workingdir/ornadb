"""Unit tests for scripts/orna_compact.py (python3 -m unittest scripts.test_orna_compact).

scripts/golden/compact-storage-v1-vectors.json holds the complete storage tree of the
legacy format-1 golden bundle for four tables: policy, manifest, shards and Parquet
segments. The tests materialise that tree in a temporary root and read it through the
real reader. Expected keys come from a raw Parquet decode (scripts/orna_parquet.py), not
from the reader under test. Each mutation edits one declared fact, re-declares only the
digest the manifest itself carries, and asserts the verification that must reject it.
"""

from __future__ import annotations

import base64
import datetime
import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from scripts import orna_compact as compact
from scripts import orna_parquet
from scripts.orna_extract import Tag, encode

GOLDEN = Path(__file__).resolve().parent / "golden" / "compact-storage-v1-vectors.json"
COUNTER = "018f0000-0000-7100-0000-000000000001"
LABEL = "018f0000-0000-7100-0000-000000000002"
DAY = "018f0000-0000-7100-0000-000000000003"
ASSET = "018f0000-0000-7100-0000-000000000005"
TABLES = (COUNTER, LABEL, DAY, ASSET)


def golden_files() -> dict[str, bytes]:
    raw = json.loads(GOLDEN.read_text(encoding="utf-8"))
    return {path: base64.b64decode(text) for path, text in raw["files"].items()}


def materialise(root: Path, files: dict[str, bytes]) -> Path:
    for relative, data in files.items():
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return root


def raw_key_values(files: dict[str, bytes], table: str) -> list[object]:
    """Oracle: the key column (first column) of every segment, decoded by the Parquet reader."""
    values: list[object] = []
    prefix = f".orna/storage/{table}/data/"
    for path in sorted(files):
        if path.startswith(prefix) and path.endswith(".parquet"):
            values.extend(row[0] for row in orna_parquet.read_parquet(files[path]).rows)
    return values


def typed_oracle_key(table: str, raw: object) -> object:
    """The logical key the reader must produce for one raw Parquet key cell."""
    if table == DAY:
        day = datetime.date(1970, 1, 1) + datetime.timedelta(days=raw)
        return Tag(60001, day.isoformat())
    return raw


class GoldenTableTests(unittest.TestCase):
    def setUp(self) -> None:
        self.files = golden_files()
        self.tmp = tempfile.TemporaryDirectory()
        self.root = materialise(Path(self.tmp.name), self.files)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_every_golden_table_resolves_to_its_raw_keys(self) -> None:
        for table in TABLES:
            with self.subTest(table=table):
                resolved = compact.read_table(self.root, table)
                self.assertEqual(resolved.table, table)
                expected = sorted(
                    encode(typed_oracle_key(table, raw)) for raw in raw_key_values(self.files, table)
                )
                self.assertEqual(sorted(row.key_bytes for row in resolved.rows), expected)
                self.assertEqual(len(resolved.live_rows), len(resolved.rows))

    def test_counter_int_keys_with_no_stored_fields(self) -> None:
        resolved = compact.read_table(self.root, COUNTER)
        self.assertEqual([row.key for row in resolved.rows], [1, 2, 3, 4])
        self.assertEqual([row.fields for row in resolved.rows], [{}, {}, {}, {}])

    def test_label_string_keys_in_canonical_order(self) -> None:
        resolved = compact.read_table(self.root, LABEL)
        self.assertEqual([row.key for row in resolved.rows], ["alpha", "beta", "gamma"])

    def test_day_date_keys_are_tag_60001_and_span_the_epoch(self) -> None:
        resolved = compact.read_table(self.root, DAY)
        self.assertEqual(
            [row.key for row in resolved.rows],
            [Tag(60001, "1969-12-31"), Tag(60001, "1970-01-01"), Tag(60001, "1970-01-02")],
        )

    def test_blob_stored_field_is_carried_as_bytes(self) -> None:
        resolved = compact.read_table(self.root, ASSET)
        self.assertEqual([row.key for row in resolved.rows], [1, 2])
        (blob_field,) = {name for row in resolved.rows for name in row.fields}
        self.assertEqual([row.fields[blob_field] for row in resolved.rows], [b"\x00\xff\x80orna", b""])


class MutationTests(unittest.TestCase):
    """Each test changes one declared fact and asserts the verification that must reject it."""

    def setUp(self) -> None:
        self.files = golden_files()

    def run_read(self, table: str, files: dict[str, bytes]) -> compact.CompactTable:
        with tempfile.TemporaryDirectory() as tmp:
            return compact.read_table(materialise(Path(tmp), files), table)

    def assert_rejected(self, table: str, files: dict[str, bytes], fragment: str) -> None:
        with self.assertRaises(compact.CompactError) as raised:
            self.run_read(table, files)
        self.assertIn(fragment, str(raised.exception))

    def shard_path(self, table: str) -> str:
        return f".orna/storage/{table}/shards/00000000.orna"

    def manifest_path(self, table: str) -> str:
        return f".orna/storage/{table}/manifest.orna"

    def policy_path(self, table: str) -> str:
        return f".orna/storage/{table}/policy.orna"

    def replace_in(self, path: str, old: str, new: str, count: int = -1) -> dict[str, bytes]:
        """Returns a copy of the golden tree with one text fact replaced in one file."""
        files = dict(self.files)
        text = files[path].decode("utf-8")
        self.assertIn(old, text, f"mutation target absent from {path}: {old}")
        files[path] = text.replace(old, new, count).encode("utf-8")
        return files

    def edit_shard(self, table: str, old: str, new: str, count: int = -1) -> dict[str, bytes]:
        """Edits the shard and re-declares its SHA-256 in the manifest, so only the edit is wrong."""
        files = self.replace_in(self.shard_path(table), old, new, count)
        old_hash = hashlib.sha256(self.files[self.shard_path(table)]).hexdigest()
        new_hash = hashlib.sha256(files[self.shard_path(table)]).hexdigest()
        manifest = files[self.manifest_path(table)].decode("utf-8")
        files[self.manifest_path(table)] = manifest.replace(old_hash, new_hash).encode("utf-8")
        return files

    def test_untouched_golden_reads_cleanly(self) -> None:
        resolved = self.run_read(COUNTER, self.files)
        self.assertEqual(len(resolved.rows), 4)

    def test_tampered_segment_bytes_fail_the_declared_sha256(self) -> None:
        files = dict(self.files)
        path = next(p for p in files if p.startswith(f".orna/storage/{COUNTER}/data/") and p.endswith(".parquet"))
        data = bytearray(files[path])
        data[len(data) // 2] ^= 0x01
        files[path] = bytes(data)
        self.assert_rejected(COUNTER, files, "segment sha256 differs")

    def test_tampered_shard_fails_the_manifest_hash(self) -> None:
        files = self.replace_in(self.shard_path(COUNTER), "generation: 1", "generation: 9", 1)
        self.assert_rejected(COUNTER, files, "shard hash differs")

    def test_wrong_git_object_id_is_rejected(self) -> None:
        files = self.edit_shard(
            COUNTER,
            'git_object_id: "6a5dbdaaafb528cfa0fdf342a536e088804d5fdc"',
            'git_object_id: "0000000000000000000000000000000000000000"',
            1,
        )
        self.assert_rejected(COUNTER, files, "Git object id differs")

    def test_wrong_compressed_byte_length_is_rejected(self) -> None:
        files = self.edit_shard(COUNTER, "compressed_bytes: 792", "compressed_bytes: 793", 1)
        self.assert_rejected(COUNTER, files, "byte length differs")

    def test_wrong_row_count_is_rejected(self) -> None:
        files = self.edit_shard(COUNTER, "row_count: 2", "row_count: 3", 1)
        self.assert_rejected(COUNTER, files, "row count differs")

    def test_wrong_segment_key_bounds_are_rejected(self) -> None:
        files = self.edit_shard(COUNTER, 'min_key: "01", max_key: "02"', 'min_key: "01", max_key: "03"', 1)
        self.assert_rejected(COUNTER, files, "key bounds differ")

    def test_wrong_columns_descriptor_is_rejected(self) -> None:
        files = self.edit_shard(COUNTER, "gYWB2CVQ", "gYWB2CVR", 1)
        self.assert_rejected(COUNTER, files, "footer orna.columns.ovb differs")

    def test_wrong_schema_id_is_rejected(self) -> None:
        schema = "3247514fed295d502be47f8dabf1c769aa36bbbbfc4bd7c7cb61649483fdb451"
        files = self.replace_in(self.manifest_path(COUNTER), schema, "0" + schema[1:])
        self.assert_rejected(COUNTER, files, "schema id differs from the manifest")

    def test_next_generation_must_follow_the_segments(self) -> None:
        files = self.replace_in(self.manifest_path(COUNTER), "next_generation: 2", "next_generation: 1")
        self.assert_rejected(COUNTER, files, "next generation does not follow")

    def test_policy_must_place_the_table_in_compact_storage(self) -> None:
        files = self.replace_in(self.policy_path(COUNTER), 'placement: "compact"', 'placement: "inline"')
        self.assert_rejected(COUNTER, files, "does not place this table")

    def test_shard_entry_count_must_match_the_manifest(self) -> None:
        files = self.replace_in(self.manifest_path(COUNTER), "entries: 2", "entries: 3", 1)
        self.assert_rejected(COUNTER, files, "entry count differs")

    def test_segment_role_deletion_removes_its_keys(self) -> None:
        # Only the first segment (generation 1, keys 1 and 2) becomes a deletion. Its keys do not
        # overlap the second segment (generation 2, keys 3 and 4), so exactly those two are deleted.
        files = self.edit_shard(COUNTER, 'role: "data"', 'role: "deletion"', 1)
        resolved = self.run_read(COUNTER, files)
        self.assertEqual([row.key for row in resolved.rows], [1, 2, 3, 4])
        self.assertEqual([row.fields is None for row in resolved.rows], [True, True, False, False])
        self.assertEqual([row.key for row in resolved.live_rows], [3, 4])


class RecordParserTests(unittest.TestCase):
    def parse(self, text: str) -> object:
        return compact.parse_record(text, "test")

    def test_accepts_maps_arrays_strings_integers_booleans_and_null(self) -> None:
        record = self.parse('{a: [1, -2, "x\\"y\\\\z\\n"], b: true, c: false, d: null, e: {}, f: []}')
        self.assertEqual(record, {"a": [1, -2, 'x"y\\z\n'], "b": True, "c": False, "d": None, "e": {}, "f": []})

    def test_rejects_trailing_text(self) -> None:
        with self.assertRaises(compact.CompactError):
            self.parse("{a: 1} {b: 2}")

    def test_rejects_duplicate_keys(self) -> None:
        with self.assertRaises(compact.CompactError):
            self.parse("{a: 1, a: 2}")

    def test_rejects_unquoted_strings_and_unknown_escapes(self) -> None:
        for text in ("{a: word}", '{a: "bad\\q"}', '{a: "open}'):
            with self.subTest(text=text), self.assertRaises(compact.CompactError):
                self.parse(text)

    def test_rejects_trailing_commas(self) -> None:
        for text in ("{a: 1,}", "{a: [1,]}"):
            with self.subTest(text=text), self.assertRaises(compact.CompactError):
                self.parse(text)

    def test_rejects_a_top_level_value_that_is_not_a_map(self) -> None:
        with self.assertRaises(compact.CompactError):
            self.parse("[1]")


class DateKeyTests(unittest.TestCase):
    def test_epoch_and_negative_days(self) -> None:
        self.assertEqual(compact._date_text(0, "t"), "1970-01-01")
        self.assertEqual(compact._date_text(-1, "t"), "1969-12-31")

    def test_days_outside_the_representable_range_are_rejected(self) -> None:
        with self.assertRaises(compact.CompactError):
            compact._date_text(10_000_000_000, "t")


if __name__ == "__main__":
    unittest.main()
