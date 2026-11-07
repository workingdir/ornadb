"""Stdlib-only Parquet reader for Orna's legacy storage fixtures.

Decodes flat Parquet files (one value per row; REQUIRED or OPTIONAL leaves)
written by the Orna storage engine. It is independent of the Rust engine and
refuses, rather than guesses at, anything outside that subset: unknown page
types, nested schemas, unsupported encodings, and checksum mismatches.
"""

from __future__ import annotations

from dataclasses import dataclass
import struct
import zlib

try:
    from compression import zstd as _zstd
except ImportError:  # Python < 3.14: ZSTD pages are refused at read time.
    _zstd = None

MAGIC = b"PAR1"
_MAX_DEPTH = 16

# Thrift compact-protocol element types.
_CT_TRUE = 1
_CT_FALSE = 2
_CT_BYTE = 3
_CT_I16 = 4
_CT_I32 = 5
_CT_I64 = 6
_CT_DOUBLE = 7
_CT_BINARY = 8
_CT_LIST = 9
_CT_SET = 10
_CT_MAP = 11
_CT_STRUCT = 12

# parquet.thrift enumerations this reader accepts.
_TYPE_BOOLEAN = 0
_TYPE_INT32 = 1
_TYPE_INT64 = 2
_TYPE_BYTE_ARRAY = 6
_PHYSICAL = {
    _TYPE_BOOLEAN: "BOOLEAN",
    _TYPE_INT32: "INT32",
    _TYPE_INT64: "INT64",
    _TYPE_BYTE_ARRAY: "BYTE_ARRAY",
}
_REPETITION_REQUIRED = 0
_REPETITION_OPTIONAL = 1
_ENC_PLAIN = 0
_ENC_RLE = 3
_ENC_RLE_DICTIONARY = 8
# RLE is accepted only inside the levels section; a chunk that declares any other
# encoding is refused before its pages are read.
_ALLOWED_CHUNK_ENCODINGS = frozenset({_ENC_PLAIN, _ENC_RLE, _ENC_RLE_DICTIONARY})
_CODEC_ZSTD = 6
_PAGE_DICTIONARY = 2
_PAGE_DATA_V2 = 3
_CONVERTED_UTF8 = 0
_CONVERTED_DATE = 6
_LOGICAL_STRING = 1
_LOGICAL_DATE = 6
_FIXED_WIDTH = {_TYPE_INT32: ("i", 4), _TYPE_INT64: ("q", 8)}


class ParquetError(Exception):
    """The input is not a Parquet file this reader decodes exactly."""


@dataclass(frozen=True)
class Column:
    """One flat leaf column. ``annotation`` is "STRING", "DATE" or None."""

    name: str
    physical: str
    optional: bool
    annotation: str | None


@dataclass(frozen=True)
class ParquetTable:
    """Decoded rows in file order; a NULL OPTIONAL value is None."""

    columns: tuple[Column, ...]
    rows: tuple[tuple[object, ...], ...]


@dataclass(frozen=True)
class _Leaf:
    name: str
    code: int
    physical: str
    optional: bool
    annotation: str | None


class _Compact:
    """Thrift compact-protocol reader over ``data[pos:end]``."""

    def __init__(self, data: bytes, pos: int = 0, end: int | None = None) -> None:
        self.data = data
        self.pos = pos
        self.end = len(data) if end is None else end

    def take(self, count: int) -> bytes:
        if count < 0 or count > self.end - self.pos:
            raise ParquetError("truncated Thrift data")
        out = self.data[self.pos : self.pos + count]
        self.pos += count
        return out

    def byte(self) -> int:
        return self.take(1)[0]

    def uvarint(self) -> int:
        result = 0
        shift = 0
        while True:
            value = self.byte()
            result |= (value & 0x7F) << shift
            if not value & 0x80:
                return result
            shift += 7
            if shift > 63:
                raise ParquetError("Thrift varint is too long")

    def zigzag(self) -> int:
        encoded = self.uvarint()
        return (encoded >> 1) ^ -(encoded & 1)

    def struct(self, depth: int = 0) -> dict[int, object]:
        if depth > _MAX_DEPTH:
            raise ParquetError("Thrift structs are nested too deeply")
        fields: dict[int, object] = {}
        last = 0
        while True:
            head = self.byte()
            if head == 0:
                return fields
            ftype = head & 0x0F
            delta = head >> 4
            fid = last + delta if delta else self.zigzag()
            if fid in fields:
                raise ParquetError(f"duplicate Thrift field {fid}")
            last = fid
            if ftype in (_CT_TRUE, _CT_FALSE):
                fields[fid] = ftype == _CT_TRUE
            else:
                fields[fid] = self.value(ftype, depth + 1)

    def value(self, ftype: int, depth: int) -> object:
        if ftype == _CT_BYTE:
            return struct.unpack("<b", self.take(1))[0]
        if ftype in (_CT_I16, _CT_I32, _CT_I64):
            return self.zigzag()
        if ftype == _CT_DOUBLE:
            return struct.unpack("<d", self.take(8))[0]
        if ftype == _CT_BINARY:
            return self.take(self.uvarint())
        if ftype in (_CT_LIST, _CT_SET):
            head = self.byte()
            size = head >> 4
            etype = head & 0x0F
            if size == 15:
                size = self.uvarint()
            self._bound(size)
            return [self.element(etype, depth) for _ in range(size)]
        if ftype == _CT_MAP:
            size = self.uvarint()
            if size == 0:
                return []
            self._bound(size)
            kv = self.byte()
            ktype = kv >> 4
            vtype = kv & 0x0F
            return [
                (self.element(ktype, depth), self.element(vtype, depth))
                for _ in range(size)
            ]
        if ftype == _CT_STRUCT:
            return self.struct(depth)
        raise ParquetError(f"unsupported Thrift type {ftype}")

    def element(self, etype: int, depth: int) -> object:
        if etype in (_CT_TRUE, _CT_FALSE):
            return self.byte() == _CT_TRUE
        return self.value(etype, depth)

    def _bound(self, size: int) -> None:
        # Every element occupies at least one byte, so a larger count is corrupt.
        if size > self.end - self.pos:
            raise ParquetError("Thrift collection is longer than its data")


def _get(fields: dict[int, object], fid: int, what: str) -> object:
    if fid not in fields:
        raise ParquetError(f"missing {what}")
    return fields[fid]


def _footer(data: bytes) -> dict[int, object]:
    if len(data) < 12 or data[:4] != MAGIC or data[-4:] != MAGIC:
        raise ParquetError("not a Parquet file (PAR1 magic)")
    footer_len = struct.unpack_from("<I", data, len(data) - 8)[0]
    if footer_len > len(data) - 12:
        raise ParquetError("Parquet footer length exceeds the file")
    reader = _Compact(data, len(data) - 8 - footer_len, len(data) - 8)
    meta = reader.struct()
    if reader.pos != len(data) - 8:
        raise ParquetError("trailing bytes after the Parquet footer")
    return meta


def _annotation(element: dict[int, object]) -> str | None:
    logical = element.get(10)
    if logical is not None:
        if _LOGICAL_STRING in logical:
            return "STRING"
        if _LOGICAL_DATE in logical:
            return "DATE"
        raise ParquetError("unsupported logical type")
    converted = element.get(6)
    if converted is None:
        return None
    if converted == _CONVERTED_UTF8:
        return "STRING"
    if converted == _CONVERTED_DATE:
        return "DATE"
    raise ParquetError(f"unsupported converted type {converted}")


def _leaves(meta: dict[int, object]) -> list[_Leaf]:
    schema = _get(meta, 2, "schema")
    if not isinstance(schema, list) or not schema:
        raise ParquetError("empty Parquet schema")
    if _get(schema[0], 5, "root child count") != len(schema) - 1:
        raise ParquetError("nested Parquet schemas are not supported")
    leaves: list[_Leaf] = []
    for element in schema[1:]:
        if 5 in element:
            raise ParquetError("nested Parquet schemas are not supported")
        code = _get(element, 1, "physical type")
        if code not in _PHYSICAL:
            raise ParquetError(f"unsupported physical type {code}")
        repetition = _get(element, 3, "repetition")
        if repetition not in (_REPETITION_REQUIRED, _REPETITION_OPTIONAL):
            raise ParquetError("repeated columns are not supported")
        name = _utf8(_get(element, 4, "column name"), "column name")
        annotation = _annotation(element)
        if annotation == "STRING" and code != _TYPE_BYTE_ARRAY:
            raise ParquetError("STRING annotation on a non-BYTE_ARRAY column")
        if annotation == "DATE" and code != _TYPE_INT32:
            raise ParquetError("DATE annotation on a non-INT32 column")
        leaves.append(
            _Leaf(
                name=name,
                code=code,
                physical=_PHYSICAL[code],
                optional=repetition == _REPETITION_OPTIONAL,
                annotation=annotation,
            )
        )
    if not leaves:
        raise ParquetError("Parquet schema has no columns")
    return leaves


def _utf8(raw: object, what: str) -> str:
    try:
        return raw.decode("utf-8")  # type: ignore[union-attr]
    except UnicodeDecodeError as err:
        raise ParquetError(f"{what} is not UTF-8") from err


def _row_groups(
    meta: dict[int, object], leaves: list[_Leaf]
) -> list[tuple[int, list[tuple[_Leaf, dict[int, object]]]]]:
    groups = []
    for group in _get(meta, 4, "row groups"):
        chunks = _get(group, 1, "column chunks")
        if len(chunks) != len(leaves):
            raise ParquetError("column chunk count differs from the schema")
        columns = [
            (leaf, _chunk_metadata(chunk, leaf)) for leaf, chunk in zip(leaves, chunks)
        ]
        groups.append((_get(group, 3, "row group rows"), columns))
    return groups


def _chunk_metadata(chunk: dict[int, object], leaf: _Leaf) -> dict[int, object]:
    if 1 in chunk:
        raise ParquetError("column data in external files is not supported")
    md = _get(chunk, 3, "column metadata")
    if _get(md, 1, "column type") != leaf.code:
        raise ParquetError("column chunk type differs from the schema")
    if _get(md, 3, "column path") != [leaf.name.encode("utf-8")]:
        raise ParquetError("column chunk path differs from the schema")
    if _get(md, 4, "codec") != _CODEC_ZSTD:
        raise ParquetError("only ZSTD column chunks are supported")
    encodings = _get(md, 2, "encodings")
    if not set(encodings) <= _ALLOWED_CHUNK_ENCODINGS:
        raise ParquetError(f"unsupported chunk encodings {sorted(set(encodings))}")
    return md


def _pages(data: bytes, md: dict[int, object]):
    """Yields (page header, body start, body end) for each page of one chunk.

    The page CRC-32, when the writer sets it, is checked before a page is yielded.
    """
    start = md[11] if 11 in md else _get(md, 9, "data page offset")
    end = start + _get(md, 7, "total compressed size")
    if start < 0 or end > len(data) or start >= end:
        raise ParquetError("column chunk lies outside the file")
    pos = start
    while pos < end:
        reader = _Compact(data, pos, end)
        header = reader.struct()
        body_start = reader.pos
        body_end = body_start + _get(header, 3, "compressed page size")
        if body_end > end:
            raise ParquetError("page extends past its column chunk")
        if 4 in header and (header[4] & 0xFFFFFFFF) != zlib.crc32(data[body_start:body_end]):
            raise ParquetError("page CRC-32 mismatch")
        yield header, body_start, body_end
        pos = body_end


def page_spans(data: bytes) -> list[tuple[int, int]]:
    """Body byte ranges of every page of every column chunk, in file order."""
    meta = _footer(data)
    spans = []
    for _, columns in _row_groups(meta, _leaves(meta)):
        for _, md in columns:
            spans.extend((start, end) for _, start, end in _pages(data, md))
    return spans


def _decompress(body: bytes, size: int) -> bytes:
    if _zstd is None:
        raise ParquetError("ZSTD pages need Python 3.14 (compression.zstd)")
    if size < 0:
        raise ParquetError("negative uncompressed page size")
    decoder = _zstd.ZstdDecompressor()
    try:
        out = decoder.decompress(body, max_length=size + 1)
    except _zstd.ZstdError as err:
        raise ParquetError(f"corrupt ZSTD page: {err}") from err
    if len(out) != size or not decoder.eof or decoder.unused_data:
        raise ParquetError("ZSTD page size differs from its header")
    return out


def _rle_hybrid(data: bytes, bit_width: int, count: int) -> list[int]:
    """Decodes the RLE / bit-packed hybrid of Parquet levels and dictionary indices.

    The whole input must be consumed; bit-packed groups may pad past ``count``.
    """
    if not 0 <= bit_width <= 32:
        raise ParquetError("RLE bit width out of range")
    if bit_width == 0:
        if data:
            raise ParquetError("RLE data with zero bit width")
        return [0] * count
    mask = (1 << bit_width) - 1
    byte_width = (bit_width + 7) // 8
    out: list[int] = []
    pos = 0
    while len(out) < count:
        header = 0
        shift = 0
        while True:
            if pos >= len(data):
                raise ParquetError("truncated RLE run header")
            value = data[pos]
            pos += 1
            header |= (value & 0x7F) << shift
            if not value & 0x80:
                break
            shift += 7
            if shift > 63:
                raise ParquetError("RLE run header is too long")
        if header & 1:
            groups = header >> 1
            if groups == 0:
                raise ParquetError("empty bit-packed RLE run")
            nbytes = groups * bit_width
            if pos + nbytes > len(data):
                raise ParquetError("truncated bit-packed RLE run")
            bits = int.from_bytes(data[pos : pos + nbytes], "little")
            pos += nbytes
            take = min(groups * 8, count - len(out))
            out.extend((bits >> (i * bit_width)) & mask for i in range(take))
        else:
            run = header >> 1
            if run == 0:
                raise ParquetError("empty RLE run")
            if pos + byte_width > len(data):
                raise ParquetError("truncated RLE run value")
            value = int.from_bytes(data[pos : pos + byte_width], "little")
            pos += byte_width
            if value > mask:
                raise ParquetError("RLE value exceeds its bit width")
            out.extend([value] * min(run, count - len(out)))
    if pos != len(data):
        raise ParquetError("trailing bytes after RLE levels")
    return out


def _plain(physical: str, raw: bytes, count: int) -> list[object]:
    if physical == "BOOLEAN":
        if len(raw) != (count + 7) // 8:
            raise ParquetError("PLAIN BOOLEAN length differs from its count")
        return [bool((raw[i >> 3] >> (i & 7)) & 1) for i in range(count)]
    if physical == "BYTE_ARRAY":
        out: list[object] = []
        pos = 0
        for _ in range(count):
            if pos + 4 > len(raw):
                raise ParquetError("truncated PLAIN BYTE_ARRAY length")
            size = struct.unpack_from("<I", raw, pos)[0]
            pos += 4
            if pos + size > len(raw):
                raise ParquetError("truncated PLAIN BYTE_ARRAY value")
            out.append(raw[pos : pos + size])
            pos += size
        if pos != len(raw):
            raise ParquetError("trailing bytes in PLAIN BYTE_ARRAY page")
        return out
    fmt, width = _FIXED_WIDTH[_PHYSICAL_CODE[physical]]
    if len(raw) != width * count:
        raise ParquetError(f"PLAIN {physical} length differs from its count")
    return list(struct.unpack(f"<{count}{fmt}", raw))


_PHYSICAL_CODE = {name: code for code, name in _PHYSICAL.items()}


def _values(
    physical: str,
    encoding: int,
    raw: bytes,
    count: int,
    dictionary: list[object] | None,
) -> list[object]:
    """Decodes ``count`` non-null values of one data page."""
    if encoding == _ENC_PLAIN:
        return _plain(physical, raw, count)
    if encoding == _ENC_RLE_DICTIONARY:
        if dictionary is None:
            raise ParquetError("dictionary-encoded page without a dictionary page")
        if not raw:
            if count:
                raise ParquetError("dictionary indices are missing")
            return []
        width = raw[0]
        indices = _rle_hybrid(raw[1:], width, count)
        out = []
        for index in indices:
            if index >= len(dictionary):
                raise ParquetError("dictionary index is out of range")
            out.append(dictionary[index])
        return out
    raise ParquetError(f"unsupported value encoding {encoding}")


def _data_page_v2(
    leaf: _Leaf,
    header: dict[int, object],
    body: bytes,
    dictionary: list[object] | None,
) -> list[object]:
    dh = _get(header, 8, "data page v2 header")
    count = _get(dh, 1, "page value count")
    nulls = _get(dh, 2, "page null count")
    rows = _get(dh, 3, "page row count")
    encoding = _get(dh, 4, "page encoding")
    def_len = _get(dh, 5, "definition levels length")
    rep_len = _get(dh, 6, "repetition levels length")
    compressed = dh.get(7, True)
    if not isinstance(compressed, bool):
        raise ParquetError("data page is_compressed is not a boolean")
    if rep_len != 0 or rows != count:
        raise ParquetError("only flat, one-value-per-row pages are supported")
    if not 0 <= def_len <= len(body):
        raise ParquetError("definition levels length exceeds the page")
    max_def = 1 if leaf.optional else 0
    if max_def == 0 and (def_len != 0 or nulls != 0):
        raise ParquetError("REQUIRED column has levels or nulls")
    if max_def:
        defs = _rle_hybrid(body[:def_len], max_def.bit_length(), count)
    else:
        defs = [1] * count
    if sum(1 for level in defs if level == 0) != nulls:
        raise ParquetError("page null count differs from its levels")
    uncompressed = _get(header, 2, "uncompressed page size")
    values_bytes = body[def_len:]
    if compressed:
        raw = _decompress(values_bytes, uncompressed - def_len)
    else:
        if len(values_bytes) != uncompressed - def_len:
            raise ParquetError("uncompressed page size differs from its body")
        raw = values_bytes
    decoded = iter(
        _values(leaf.physical, encoding, raw, count - nulls, dictionary)
    )
    return [next(decoded) if level else None for level in defs]


def _finish(leaf: _Leaf, value: object) -> object:
    if value is None or leaf.annotation != "STRING":
        return value
    assert isinstance(value, bytes)
    return _utf8(value, f"STRING column {leaf.name}")


def _read_chunk(data: bytes, leaf: _Leaf, md: dict[int, object]) -> list[object]:
    dictionary: list[object] | None = None
    values: list[object] = []
    for header, body_start, body_end in _pages(data, md):
        page_type = _get(header, 1, "page type")
        body = data[body_start:body_end]
        if page_type == _PAGE_DICTIONARY:
            if dictionary is not None or values:
                raise ParquetError("dictionary page is out of place")
            dh = _get(header, 7, "dictionary page header")
            if _get(dh, 2, "dictionary encoding") != _ENC_PLAIN:
                raise ParquetError("dictionary page must be PLAIN")
            raw = _decompress(body, _get(header, 2, "uncompressed page size"))
            dictionary = _plain(leaf.physical, raw, _get(dh, 1, "dictionary count"))
        elif page_type == _PAGE_DATA_V2:
            values.extend(_data_page_v2(leaf, header, body, dictionary))
        else:
            raise ParquetError(f"unsupported page type {page_type}")
    if len(values) != _get(md, 5, "chunk value count"):
        raise ParquetError("column chunk value count differs from its pages")
    return [_finish(leaf, value) for value in values]


def read_parquet(data: bytes) -> ParquetTable:
    """Decodes a flat Parquet file exactly, or raises ParquetError."""
    meta = _footer(data)
    leaves = _leaves(meta)
    total = _get(meta, 3, "row count")
    rows: list[tuple[object, ...]] = []
    for group_rows, columns in _row_groups(meta, leaves):
        decoded = []
        for leaf, md in columns:
            if _get(md, 5, "chunk value count") != group_rows:
                raise ParquetError("column value count differs from the row group")
            decoded.append(_read_chunk(data, leaf, md))
        rows.extend(zip(*decoded))
    if len(rows) != total:
        raise ParquetError("decoded row count differs from the footer")
    table_columns = tuple(
        Column(leaf.name, leaf.physical, leaf.optional, leaf.annotation)
        for leaf in leaves
    )
    return ParquetTable(columns=table_columns, rows=tuple(rows))


def key_value_metadata(data: bytes) -> dict[str, str]:
    """The footer's key-value metadata as text; a duplicate key is rejected."""
    meta = _footer(data)
    out: dict[str, str] = {}
    for pair in meta.get(5, []):  # type: ignore[union-attr]
        key = _utf8(_get(pair, 1, "metadata key"), "metadata key")  # type: ignore[arg-type]
        if key in out:
            raise ParquetError(f"duplicate footer metadata key {key}")
        out[key] = _utf8(_get(pair, 2, "metadata value"), "metadata value")  # type: ignore[arg-type]
    return out
