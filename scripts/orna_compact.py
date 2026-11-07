"""Reader for the legacy compact-storage-v1 table layout (1.0.0 format-1 storage).

A compact table lives under ``.orna/storage/<table>/``: ``policy.orna`` (placement),
``manifest.orna`` (profile, schema id, shard index), ``shards/<n>.orna`` (segment
entries) and ``data/<bucket>/<segment>.parquet`` (one Parquet file per segment).
Manifest, shard and policy files are Orna record text, not JSON. Every digest the
manifest and shard declare is verified against the bytes on disk: the shard hash,
each segment's sha256, Git blob id, byte length, row count, key bounds, columns
descriptor and the footer schema fingerprint. Any mismatch raises CompactError.

Scope of this module: the typed key of every row, the stored field values, and the
generation-ordered winner per key (a winning deletion removes the key). Field kinds
other than Int, Bool, Str, Date and Blob are refused here (CompactError names the
kind); those are the OVB-fallback columns handled by a later slice.
"""

from __future__ import annotations

import base64
import datetime
import functools
import hashlib
import re
from dataclasses import dataclass
from pathlib import Path

from scripts import orna_parquet
from scripts.orna_extract import CborMap, ExtractionError, Tag, compare_keys, decode_canonical, encode
from scripts.orna_legacy import OVB1_TAGS, git_oid

PROFILE = "compact-storage-v1"
PROFILE_VERSION = 1
SCHEMA_DOMAIN = b"orna.schema.v1\0"
DATE_TAG = 60001
COMPOSITE_KEY_TAG = 60015
UUID_FIELD_TAG = 37
ROLE_DATA = "data"
ROLE_DELETION = "deletion"
SUPPORTED_KINDS = frozenset({"Int", "Bool", "Str", "Date", "Blob"})
_IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_INT = re.compile(r"-?(0|[1-9][0-9]*)")
_ESCAPES = {'"': '"', "\\": "\\", "n": "\n", "t": "\t", "r": "\r"}


class CompactError(ExtractionError):
    """A compact-storage-v1 table that fails verification or uses an unsupported kind."""


def _reject(where: str, message: str) -> None:
    raise CompactError(f"{where}: {message}")


class _RecordParser:
    """Orna record text: maps with bare keys, arrays, strings, integers, booleans, null."""

    def __init__(self, text: str, where: str) -> None:
        self.text = text
        self.where = where
        self.pos = 0

    def fail(self, message: str) -> None:
        _reject(self.where, f"{message} at offset {self.pos}")

    def skip_space(self) -> None:
        while self.pos < len(self.text) and self.text[self.pos] in " \t\r\n":
            self.pos += 1

    def peek(self) -> str:
        self.skip_space()
        return self.text[self.pos] if self.pos < len(self.text) else ""

    def expect(self, char: str) -> None:
        if self.peek() != char:
            self.fail(f"expected {char!r}")
        self.pos += 1

    def value(self) -> object:
        char = self.peek()
        if char == "{":
            return self.map()
        if char == "[":
            return self.array()
        if char == '"':
            return self.string()
        if char and (char == "-" or char.isdigit()):
            match = _INT.match(self.text, self.pos)
            if not match:
                self.fail("malformed integer")
            self.pos = match.end()
            return int(match.group(0))
        for word, literal in (("true", True), ("false", False), ("null", None)):
            if self.text.startswith(word, self.pos):
                self.pos += len(word)
                return literal
        self.fail("unexpected token")
        raise AssertionError("unreachable")

    def map(self) -> dict[str, object]:
        self.expect("{")
        out: dict[str, object] = {}
        if self.peek() == "}":
            self.pos += 1
            return out
        while True:
            self.skip_space()
            match = _IDENT.match(self.text, self.pos)
            if not match:
                self.fail("expected a bare map key")
            key = match.group(0)
            self.pos = match.end()
            if key in out:
                self.fail(f"duplicate map key {key}")
            self.expect(":")
            out[key] = self.value()
            separator = self.peek()
            self.pos += 1
            if separator == "}":
                return out
            if separator != ",":
                self.fail("expected ',' or '}'")

    def array(self) -> list[object]:
        self.expect("[")
        out: list[object] = []
        if self.peek() == "]":
            self.pos += 1
            return out
        while True:
            out.append(self.value())
            separator = self.peek()
            self.pos += 1
            if separator == "]":
                return out
            if separator != ",":
                self.fail("expected ',' or ']'")

    def string(self) -> str:
        self.expect('"')
        chars: list[str] = []
        while self.pos < len(self.text):
            char = self.text[self.pos]
            self.pos += 1
            if char == '"':
                return "".join(chars)
            if char == "\\":
                if self.pos >= len(self.text) or self.text[self.pos] not in _ESCAPES:
                    self.fail("unsupported string escape")
                chars.append(_ESCAPES[self.text[self.pos]])
                self.pos += 1
            else:
                chars.append(char)
        self.fail("unterminated string")
        raise AssertionError("unreachable")


def parse_record(text: str, where: str) -> dict[str, object]:
    """Parses one Orna record; the top level must be a map and nothing may follow it."""
    parser = _RecordParser(text, where)
    record = parser.value()
    if not isinstance(record, dict) or parser.peek() != "":
        _reject(where, "record text must be one map")
    return record


def _exact_keys(record: dict[str, object], expected: set[str], where: str) -> None:
    if set(record) != expected:
        _reject(where, f"record keys {sorted(record)} differ from {sorted(expected)}")


def _text(value: object, where: str) -> str:
    if not isinstance(value, str):
        _reject(where, "expected text")
    return value


def _integer(value: object, where: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        _reject(where, "expected integer")
    return value


def _hex(value: object, where: str) -> bytes:
    text = _text(value, where)
    if text != text.lower() or len(text) % 2:
        _reject(where, "expected lowercase even-length hex")
    try:
        return bytes.fromhex(text)
    except ValueError as err:
        raise CompactError(f"{where}: malformed hex") from err


def _date_text(days: int, where: str) -> str:
    try:
        day = datetime.date(1970, 1, 1) + datetime.timedelta(days=days)
    except OverflowError as err:
        raise CompactError(f"{where}: date out of range") from err
    return day.isoformat()


def _typed_value(kind: str, raw: object, where: str) -> object:
    """The canonical typed value of one physical column cell, or None for NULL."""
    if raw is None:
        return None
    if kind == "Int":
        return _integer(raw, where)
    if kind == "Bool":
        if not isinstance(raw, bool):
            _reject(where, "expected boolean")
        return raw
    if kind == "Str":
        return _text(raw, where)
    if kind == "Date":
        return Tag(DATE_TAG, _date_text(_integer(raw, where), where))
    if kind == "Blob":
        if not isinstance(raw, bytes):
            _reject(where, "expected bytes")
        return raw
    _reject(where, f"unsupported field kind {kind}")
    raise AssertionError("unreachable")


@dataclass(frozen=True)
class FieldDescriptor:
    """One field of the schema descriptor: its id, Parquet column, kind and key role."""

    field_id: bytes
    column: str
    kind: str
    is_key: bool


@dataclass(frozen=True)
class CompactRow:
    """The winning state of one key. ``fields`` is None for a deleted key."""

    key: object
    key_bytes: bytes
    fields: dict[str, object] | None


@dataclass(frozen=True)
class CompactTable:
    """Resolved table state: one winner per key in canonical key order."""

    table: str
    schema: bytes
    rows: tuple[CompactRow, ...]

    @property
    def live_rows(self) -> tuple[CompactRow, ...]:
        return tuple(row for row in self.rows if row.fields is not None)


def schema_fingerprint(descriptor_bytes: bytes) -> bytes:
    return hashlib.sha256(SCHEMA_DOMAIN + descriptor_bytes).digest()


def _descriptor_fields(descriptor_bytes: bytes, table: str, where: str) -> list[FieldDescriptor]:
    descriptor = decode_canonical(descriptor_bytes, where, OVB1_TAGS)
    if not isinstance(descriptor, CborMap):
        _reject(where, "schema descriptor is not a map")
    entries: dict[int, object] = {}
    for key, value in descriptor.entries:
        if not isinstance(key, int) or isinstance(key, bool) or key in entries:
            _reject(where, "schema descriptor keys must be unique integers")
        entries[key] = value
    if set(entries) != {0, 1, 2, 3, 4}:
        _reject(where, "schema descriptor must carry keys 0 through 4")
    if entries[0] != 1:
        _reject(where, "unsupported schema descriptor version")
    if entries[1] != Tag(UUID_FIELD_TAG, bytes.fromhex(table.replace("-", ""))):
        _reject(where, "schema descriptor names another table")
    key_ids = entries[2]
    fields_raw = entries[3]
    if not isinstance(key_ids, list) or not isinstance(fields_raw, list):
        _reject(where, "schema descriptor key or field list is malformed")
    key_set = {_uuid_bytes(item, where) for item in key_ids}
    if len(key_set) != len(key_ids):
        _reject(where, "schema descriptor repeats a key field")
    fields: list[FieldDescriptor] = []
    seen: set[bytes] = set()
    for item in fields_raw:
        if not isinstance(item, list) or len(item) != 5:
            _reject(where, "schema field must be a five-item list")
        field_id = _uuid_bytes(item[0], where)
        column, kind_pair, role = item[1], item[2], item[3]
        if column != f"f_{field_id.hex()}":
            _reject(where, "schema field column name differs from its id")
        if not isinstance(kind_pair, list) or len(kind_pair) != 2 or not isinstance(kind_pair[1], str):
            _reject(where, "schema field kind is malformed")
        if field_id in seen:
            _reject(where, "schema descriptor repeats a field")
        seen.add(field_id)
        is_key = field_id in key_set
        if role != (0 if is_key else 1):
            _reject(where, "schema field role contradicts the key list")
        fields.append(FieldDescriptor(field_id, column, kind_pair[1], is_key))
    if key_set - seen:
        _reject(where, "schema key list names an undeclared field")
    if not any(field.is_key for field in fields):
        _reject(where, "schema descriptor has no key field")
    return fields


def _uuid_bytes(value: object, where: str) -> bytes:
    if not isinstance(value, Tag) or value.number != UUID_FIELD_TAG or not isinstance(value.value, bytes):
        _reject(where, "expected a tag-37 field identifier")
    if len(value.value) != 16:
        _reject(where, "field identifier must be 16 bytes")
    return value.value


_PHYSICAL = {
    "Int": ("INT64", None),
    "Bool": ("BOOLEAN", None),
    "Str": ("BYTE_ARRAY", "STRING"),
    "Date": ("INT32", "DATE"),
    "Blob": ("BYTE_ARRAY", None),
}


def _check_columns(
    fields: list[FieldDescriptor], parquet: orna_parquet.ParquetTable, where: str
) -> None:
    by_name = {column.name: column for column in parquet.columns}
    if len(by_name) != len(parquet.columns) or set(by_name) != {field.column for field in fields}:
        _reject(where, "Parquet columns differ from the schema descriptor")
    for field in fields:
        if field.kind not in SUPPORTED_KINDS:
            _reject(where, f"unsupported field kind {field.kind}")
        column = by_name[field.column]
        if (column.physical, column.annotation) != _PHYSICAL[field.kind]:
            _reject(where, f"column {field.column} physical type differs from kind {field.kind}")
        if field.is_key and column.optional:
            _reject(where, "key column must be required")


def _segment_rows(
    fields: list[FieldDescriptor], parquet: orna_parquet.ParquetTable, where: str
) -> list[tuple[object, bytes, dict[str, object]]]:
    index = {column.name: position for position, column in enumerate(parquet.columns)}
    key_fields = [field for field in fields if field.is_key]
    stored_fields = [field for field in fields if not field.is_key]
    out: list[tuple[object, bytes, dict[str, object]]] = []
    for row in parquet.rows:
        components = []
        for field in key_fields:
            value = _typed_value(field.kind, row[index[field.column]], where)
            if value is None:
                _reject(where, "key column holds NULL")
            components.append(value)
        key = components[0] if len(components) == 1 else Tag(COMPOSITE_KEY_TAG, components)
        key_bytes = encode(key)
        stored = {
            field.field_id.hex(): _typed_value(field.kind, row[index[field.column]], where)
            for field in stored_fields
        }
        out.append((key, key_bytes, stored))
    return out


def _load_segment(
    root: Path,
    table: str,
    schema: bytes,
    entry: dict[str, object],
    where: str,
) -> tuple[list[tuple[object, bytes, dict[str, object]]], dict]:
    _exact_keys(
        entry,
        {
            "segment_id", "role", "generation", "schema_id", "profile_version",
            "encoder_version", "relative_path", "git_object_id", "sha256", "min_key",
            "max_key", "min_event_time", "max_event_time", "row_count", "compressed_bytes",
            "columns", "row_group_index", "bloom",
        },
        where,
    )
    segment_id = _text(entry["segment_id"], where)
    role = _text(entry["role"], where)
    if role not in (ROLE_DATA, ROLE_DELETION):
        _reject(where, f"unknown segment role {role}")
    generation = _integer(entry["generation"], where)
    if generation < 1:
        _reject(where, "segment generation must be positive")
    if _hex(entry["schema_id"], where) != schema:
        _reject(where, "segment schema id differs from the manifest")
    if _integer(entry["profile_version"], where) != PROFILE_VERSION:
        _reject(where, "unsupported profile version")
    relative = _text(entry["relative_path"], where)
    expected_prefix = f".orna/storage/{table}/data/"
    if not relative.startswith(expected_prefix) or not relative.endswith(f"/{segment_id}.parquet") or ".." in relative:
        _reject(where, "segment path is outside the table data directory")
    data = (root / relative).read_bytes()
    if hashlib.sha256(data).hexdigest() != _text(entry["sha256"], where):
        _reject(where, "segment sha256 differs from the file")
    if git_oid("blob", data) != _text(entry["git_object_id"], where):
        _reject(where, "segment Git object id differs from the file")
    if _integer(entry["compressed_bytes"], where) != len(data):
        _reject(where, "segment byte length differs from the file")
    footer = orna_parquet.key_value_metadata(data)
    descriptor_bytes = base64.b64decode(_text(footer.get("orna.schema.ovb"), where), validate=True)
    if schema_fingerprint(descriptor_bytes) != schema:
        _reject(where, "footer schema descriptor does not match the schema id")
    expected_footer = {
        "orna.profile": PROFILE,
        "orna.table": table,
        "orna.schema.sha256": schema.hex(),
        "orna.columns.ovb": _text(entry["columns"], where),
        "orna.encoder": _text(entry["encoder_version"], where),
    }
    for name, value in expected_footer.items():
        if footer.get(name) != value:
            _reject(where, f"footer {name} differs from the manifest")
    fields = _descriptor_fields(descriptor_bytes, table, where)
    parquet = orna_parquet.read_parquet(data)
    _check_columns(fields, parquet, where)
    rows = _segment_rows(fields, parquet, where)
    if len(rows) != _integer(entry["row_count"], where):
        _reject(where, "segment row count differs from the file")
    keys = [key_bytes for _, key_bytes, _ in rows]
    if len(set(keys)) != len(keys):
        _reject(where, "segment repeats a key")
    ordered = sorted(rows, key=functools.cmp_to_key(lambda a, b: compare_keys(a[0], b[0], where)))
    if not ordered:
        _reject(where, "segment has no rows")
    if ordered[0][1].hex() != _text(entry["min_key"], where) or ordered[-1][1].hex() != _text(entry["max_key"], where):
        _reject(where, "segment key bounds differ from the rows")
    meta = {"generation": generation, "role": role}
    return rows, meta


def _resolve(
    segments: list[tuple[dict, list[tuple[object, bytes, dict[str, object]]]]], where: str
) -> tuple[CompactRow, ...]:
    winners: dict[bytes, tuple[int, CompactRow]] = {}
    for meta, rows in sorted(segments, key=lambda item: item[0]["generation"]):
        generation = meta["generation"]
        for key, key_bytes, stored in rows:
            current = winners.get(key_bytes)
            if current is not None:
                if current[0] == generation:
                    _reject(where, "two segments hold one key at one generation")
                if current[0] > generation:
                    continue
            fields = None if meta["role"] == ROLE_DELETION else stored
            winners[key_bytes] = (generation, CompactRow(key, key_bytes, fields))
    ordered = sorted(
        (item[1] for item in winners.values()),
        key=functools.cmp_to_key(lambda a, b: compare_keys(a.key, b.key, where)),
    )
    return tuple(ordered)


def _key_from_hex(text: object, where: str) -> object:
    return decode_canonical(_hex(text, where), where, OVB1_TAGS)


def read_table(root: Path, table: str) -> CompactTable:
    """Verifies and resolves one compact-storage-v1 table under ``root``."""
    where = f"compact table {table}"
    base = root / ".orna" / "storage" / table
    policy = parse_record((base / "policy.orna").read_text(encoding="utf-8"), f"{where} policy")
    _exact_keys(policy, {"placement", "table"}, f"{where} policy")
    if policy["placement"] != "compact" or policy["table"] != table:
        _reject(where, "policy does not place this table in compact storage")
    manifest_bytes = (base / "manifest.orna").read_text(encoding="utf-8")
    manifest = parse_record(manifest_bytes, f"{where} manifest")
    _exact_keys(manifest, {"profile", "table", "schema", "next_generation", "shards"}, f"{where} manifest")
    if manifest["profile"] != PROFILE or manifest["table"] != table:
        _reject(where, "manifest profile or table differs")
    schema = _hex(manifest["schema"], f"{where} manifest schema")
    if len(schema) != 32:
        _reject(where, "manifest schema id must be 32 bytes")
    next_generation = _integer(manifest["next_generation"], f"{where} manifest")
    shards = manifest["shards"]
    if not isinstance(shards, list):
        _reject(where, "manifest shards must be a list")
    segments: list[tuple[dict, list[tuple[object, bytes, dict[str, object]]]]] = []
    max_generation = 0
    for position, shard_ref in enumerate(shards):
        if not isinstance(shard_ref, dict):
            _reject(where, "manifest shard must be a map")
        _exact_keys(shard_ref, {"number", "min_key", "max_key", "entries", "file", "hash"}, f"{where} shard")
        if _integer(shard_ref["number"], where) != position:
            _reject(where, "shard numbers must be consecutive from zero")
        shard_path = _text(shard_ref["file"], where)
        if shard_path != f"shards/{position:08d}.orna":
            _reject(where, "shard file name differs from its number")
        shard_bytes = (base / shard_path).read_bytes()
        if hashlib.sha256(shard_bytes).hexdigest() != _text(shard_ref["hash"], where):
            _reject(where, "shard hash differs from the file")
        shard = parse_record(shard_bytes.decode("utf-8"), f"{where} shard {position}")
        _exact_keys(shard, {"entries"}, f"{where} shard {position}")
        entries = shard["entries"]
        if not isinstance(entries, list) or not entries:
            _reject(where, "shard must hold at least one entry")
        if _integer(shard_ref["entries"], where) != len(entries):
            _reject(where, "shard entry count differs from the manifest")
        lows: list[object] = []
        highs: list[object] = []
        for entry_position, entry in enumerate(entries):
            if not isinstance(entry, dict):
                _reject(where, "shard entry must be a map")
            entry_where = f"{where} shard {position} entry {entry_position}"
            rows, meta = _load_segment(root, table, schema, entry, entry_where)
            max_generation = max(max_generation, meta["generation"])
            lows.append(_key_from_hex(entry["min_key"], entry_where))
            highs.append(_key_from_hex(entry["max_key"], entry_where))
            segments.append((meta, rows))
        key_order = functools.cmp_to_key(lambda a, b: compare_keys(a, b, where))
        if _hex(shard_ref["min_key"], where) != encode(min(lows, key=key_order)):
            _reject(where, "shard minimum key differs from its entries")
        if _hex(shard_ref["max_key"], where) != encode(max(highs, key=key_order)):
            _reject(where, "shard maximum key differs from its entries")
    if next_generation <= max_generation:
        _reject(where, "next generation does not follow the segments")
    return CompactTable(table=table, schema=schema, rows=_resolve(segments, where))
