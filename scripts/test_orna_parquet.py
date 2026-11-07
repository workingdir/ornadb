"""Tests for scripts/orna_parquet.py (python3 -m unittest scripts.test_orna_parquet).

The golden tests clone the committed legacy-format1 bundle into a temporary
directory, check every shard oracle entry (sha256, compressed size, row count,
min and max key) against the Parquet bytes, and compare the decoded rows with
the rows the fixture was written from. The mutation tests flip one byte in a
page body and require a CRC refusal, with an unmutated control that decodes.
"""

from __future__ import annotations

import hashlib
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

from scripts import orna_parquet as parquet
from scripts.orna_parquet import ParquetError

REPO = Path(__file__).resolve().parent.parent
BUNDLE = REPO / "crates" / "orna-cli" / "tests" / "fixtures" / "legacy-format1.bundle"

# Rows the legacy-format1 fixture was written from, keyed by segment id.
EXPECTED_ROWS = {
    "018f0000-0000-7000-8000-000000010001": [(1,), (2,)],
    "018f0000-0000-7000-8000-000000010002": [(3,), (4,)],
    "018f0000-0000-7000-8000-000000020001": [("alpha",), ("beta",), ("gamma",)],
    "018f0000-0000-7000-8000-000000030001": [(-1,), (0,), (1,)],
    "018f0000-0000-7000-8000-000000050001": [(1, b"\x00\xff\x80orna"), (2, b"")],
}


def _cbor_key(value: object) -> bytes:
    """Minimal CBOR of the key forms the shard oracle records."""
    if isinstance(value, int):
        assert 0 <= value < 24
        return bytes([value])
    if isinstance(value, str):
        raw = value.encode()
        assert len(raw) < 24
        return bytes([0x60 + len(raw)]) + raw
    raise AssertionError(value)


def _cbor_date(iso: str) -> bytes:
    """Orna's DATE key: the engine's date tag followed by the ISO text."""
    raw = iso.encode()
    return b"\xd9\xea\x61" + bytes([0x60 + len(raw)]) + raw


def _clone_bundle(target: Path) -> Path:
    subprocess.run(
        ["git", "clone", "-q", str(BUNDLE), str(target)],
        check=True,
        capture_output=True,
    )
    return target


def _oracle_entries(shard_text: str) -> list[dict[str, str]]:
    """Each entry of a shard file, as the fields the oracle comparison uses."""
    entries = []
    for chunk in shard_text.split("segment_id: ")[1:]:
        fields = {}
        for name in ("relative_path", "sha256", "min_key", "max_key"):
            match = re.search(rf'{name}: "([^"]*)"', chunk)
            if match:
                fields[name] = match.group(1)
        for name in ("row_count", "compressed_bytes"):
            match = re.search(rf"{name}: (\d+)", chunk)
            if match:
                fields[name] = match.group(1)
        segment = re.match(r'"([^"]*)"', chunk)
        assert segment is not None
        fields["segment_id"] = segment.group(1)
        entries.append(fields)
    return entries


class GoldenFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls._tmp = tempfile.TemporaryDirectory()
        cls.repo = _clone_bundle(Path(cls._tmp.name) / "legacy")
        cls.storage = cls.repo / ".orna" / "storage"

    @classmethod
    def tearDownClass(cls) -> None:
        cls._tmp.cleanup()

    def _entries(self) -> list[dict[str, str]]:
        shards = sorted(self.storage.glob("*/shards/00000000.orna"))
        self.assertTrue(shards, "fixture has no shard oracle files")
        entries: list[dict[str, str]] = []
        for shard in shards:
            entries.extend(_oracle_entries(shard.read_text()))
        return entries

    def test_oracle_matches_every_parquet_file(self) -> None:
        entries = self._entries()
        self.assertEqual(
            sorted(e["segment_id"] for e in entries),
            sorted(EXPECTED_ROWS),
        )
        for entry in entries:
            data = (self.repo / entry["relative_path"]).read_bytes()
            self.assertEqual(
                hashlib.sha256(data).hexdigest(), entry["sha256"], entry["segment_id"]
            )
            self.assertEqual(len(data), int(entry["compressed_bytes"]))
            table = parquet.read_parquet(data)
            self.assertEqual(len(table.rows), int(entry["row_count"]))

    def test_decoded_rows_match_the_fixture(self) -> None:
        for entry in self._entries():
            data = (self.repo / entry["relative_path"]).read_bytes()
            table = parquet.read_parquet(data)
            self.assertEqual(
                list(table.rows), EXPECTED_ROWS[entry["segment_id"]], entry["segment_id"]
            )

    def test_min_and_max_keys_match_the_oracle(self) -> None:
        keys = {
            "018f0000-0000-7000-8000-000000010001": ((1,), (2,)),
            "018f0000-0000-7000-8000-000000010002": ((3,), (4,)),
            "018f0000-0000-7000-8000-000000020001": (("alpha",), ("gamma",)),
            "018f0000-0000-7000-8000-000000050001": ((1,), (2,)),
        }
        dates = {"018f0000-0000-7000-8000-000000030001": ("1969-12-31", "1970-01-02")}
        for entry in self._entries():
            segment = entry["segment_id"]
            if segment in keys:
                low, high = keys[segment]
                want_min = _cbor_key(low[0])
                want_max = _cbor_key(high[0])
            else:
                low_date, high_date = dates[segment]
                want_min = _cbor_date(low_date)
                want_max = _cbor_date(high_date)
            self.assertEqual(entry["min_key"], want_min.hex(), segment)
            self.assertEqual(entry["max_key"], want_max.hex(), segment)


class MutationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls._tmp = tempfile.TemporaryDirectory()
        repo = _clone_bundle(Path(cls._tmp.name) / "legacy")
        cls.counter = (
            repo
            / ".orna/storage/018f0000-0000-7100-0000-000000000001/data/01"
            / "018f0000-0000-7000-8000-000000010001.parquet"
        ).read_bytes()

    @classmethod
    def tearDownClass(cls) -> None:
        cls._tmp.cleanup()

    def test_unmutated_control_decodes(self) -> None:
        table = parquet.read_parquet(self.counter)
        self.assertEqual(list(table.rows), [(1,), (2,)])

    def test_flipped_page_byte_is_refused_by_crc(self) -> None:
        start, end = parquet.page_spans(self.counter)[0]
        self.assertLess(start, end)
        mutated = bytearray(self.counter)
        mutated[start + (end - start) // 2] ^= 0x01
        with self.assertRaisesRegex(ParquetError, "CRC"):
            parquet.read_parquet(bytes(mutated))

    def test_non_parquet_input_is_refused(self) -> None:
        with self.assertRaisesRegex(ParquetError, "PAR1"):
            parquet.read_parquet(b"not parquet at all")

    def test_truncated_file_is_refused(self) -> None:
        with self.assertRaises(ParquetError):
            parquet.read_parquet(self.counter[:-9])

    def test_unsupported_value_encoding_is_refused(self) -> None:
        with self.assertRaisesRegex(ParquetError, "unsupported value encoding 5"):
            parquet._values("INT64", 5, b"", 0, None)


class RleHybridTest(unittest.TestCase):
    def test_rle_run_repeats_its_value(self) -> None:
        # Header 6 = run of 3 (6 >> 1), bit width 1, value 1.
        self.assertEqual(parquet._rle_hybrid(bytes([6, 1]), 1, 3), [1, 1, 1])

    def test_bit_packed_group_unpacks_least_significant_first(self) -> None:
        # Header 3 = one group of 8 bit-packed values, bit width 1.
        self.assertEqual(
            parquet._rle_hybrid(bytes([3, 0b10110010]), 1, 8),
            [0, 1, 0, 0, 1, 1, 0, 1],
        )

    def test_truncated_run_is_refused(self) -> None:
        with self.assertRaisesRegex(ParquetError, "truncated"):
            parquet._rle_hybrid(bytes([6]), 1, 3)

    def test_trailing_bytes_are_refused(self) -> None:
        with self.assertRaisesRegex(ParquetError, "trailing"):
            parquet._rle_hybrid(bytes([6, 1, 0]), 1, 3)


if __name__ == "__main__":
    unittest.main()
