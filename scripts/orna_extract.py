#!/usr/bin/env python3
"""Independent, read-only extractor for one Orna repository-format-3 snapshot.

Orna 1.1.0 requires that a complete snapshot can be recovered by "an
independent read-only extractor" that recovers modules, schema, rows and raw
content without running application code (source/40-upgrade-contract.md;
implementation/ACCEPTANCE.md, Gate F). This program is that extractor. It is
written from the published format alone (profiles/store-format-3.md,
source/23-storage.md, source/29-formats.md and the reference model
tools/final_model.py) and shares nothing with the engine:

* Git objects are read only through ``git cat-file --batch``, and every object
  is re-hashed here before it is used, so a damaged object database cannot
  pass. Replacement refs and lazy fetching are disabled.
* Canonical CBOR is decoded by this file's own strict decoder.
* The OGS-1 node graph, the ORP-1 ordered maps and the OGB-2 byte indexes are
  walked and checked here: node envelopes and their exact ``refs`` children,
  logical key order, exact row counts, canonical page and chunk boundaries,
  chunk and whole-content SHA-256 and lengths, schema digests and value
  overflow digests.
* Nothing is imported from an Orna crate and the ``orna`` binary is never run.

Usage::

    orna_extract.py REPOSITORY [COMMIT] --output DIR [--dependency PATH=REPOSITORY]...

REPOSITORY is a repository's top-level directory or a bare repository, for
example the result of ``orna export ARCHIVE --restore DEST``. COMMIT defaults to
``HEAD``. Gitlink dependencies are read from ``--dependency`` mappings, from a
nested repository at the gitlink's path, or from ``.git/modules/<path>``.

The JSON report is written to stdout with sorted keys. Each Blob's verified
bytes are written to ``DIR/blobs/<sha256>``. A structural violation, an
unavailable object or a legacy format-1/2 snapshot exits 1 with one line on
stderr naming the object and the rule it breaks; invalid arguments exit 2.

Values are decoded structurally. Executing schema assertions and refinements
is engine admission, which independent extraction need not repeat
(store-format-3.md section 6). Secondary indexes are derived data and are
reported by root only.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
from typing import Callable
import unicodedata

# Format-3 constants (profiles/store-3.json, profiles/store-format-3.md).
REPOSITORY_FORMAT = 3
NODE_DATA_LIMIT = 65_536
MAX_REFS = 256
MAX_GRAPH_HEIGHT = 64
MIN_PAGE_ENTRIES = 16
MAX_PAGE_ENTRIES = 256
ANCHOR_MASK = (1 << 6) - 1
ORP_ANCHOR_DOMAIN = b"orna.orp.boundary.v1\0"
OGB_ANCHOR_DOMAIN = b"orna.ogb.index.v2\0"
GEAR_SEED = b"orna.gear.v1\0"
GEAR_MINIMUM = 65_536
GEAR_MASK = 262_143
GEAR_MAXIMUM = 1_048_576
ROW_INLINE_LIMIT = 8_192
ROW_INLINE_REFERENCES = 32
KEY_CANONICAL_LIMIT = 1 << 20
ROW_CANONICAL_LIMIT = 1 << 30
MAX_LENGTH = (1 << 63) - 1
DATABASE_RECORD_LIMIT = 65_536
CANONICAL_NAN = bytes.fromhex("7ff8000000000000")
EMPTY_SHA256 = hashlib.sha256(b"").digest()

# Resource bounds of this tool rather than format rules. They keep a hostile
# repository from exhausting memory or the interpreter stack.
SCHEMA_BYTES_LIMIT = 64 << 20
SOURCE_BYTES_LIMIT = 64 << 20
SOURCE_TREE_BYTES_LIMIT = 64 << 20
NODE_TREE_LIMIT = 4_096
REFS_TREE_LIMIT = 65_536
MAX_CBOR_DEPTH = 256
MAX_CBOR_ITEMS = 4_000_000
MAX_DIRECTORY_DEPTH = 256
MAX_DEPENDENCY_DEPTH = 32

UUID_TAG = 37
OPTION_TAG = 60013
STORED_REFERENCE_TAG = 60021
PORTABLE_BLOB_TAG = 60110
STORED_BLOB_TAG = 60111
SEMANTIC_BLOB_TAG = 60112
OVERFLOW_TAG = 60113
PROFILE_TAGS = frozenset({PORTABLE_BLOB_TAG, STORED_BLOB_TAG, SEMANTIC_BLOB_TAG, OVERFLOW_TAG})
# Registered tags admitted in format-3 node data: bignums, UUID, the portable
# value tags and the format-3 profile tags (final_model.decode).
REGISTERED_TAGS = frozenset({2, 3, UUID_TAG, *range(60000, 60026), *PROFILE_TAGS})

NODE_KIND_NAMES = {
    0: "store root",
    1: "ordered leaf",
    2: "ordered branch",
    3: "value overflow",
    4: "byte index",
    5: "Blob descriptor",
    6: "schema",
    7: "dependency index",
}
FIELD_ROLES = {0: "key", 1: "stored", 2: "computed"}
PRIMITIVE_TYPES = frozenset(
    {
        "Bool",
        "Int",
        "Float",
        "Decimal",
        "Str",
        "Blob",
        "Uuid",
        "Date",
        "Instant",
        "LocalDateTime",
        "TimeOfDay",
        "Duration",
        "TimeZone",
        "ZonedDateTime",
        "Unit",
    }
)

# MIME-1 registry (profiles/media-1.json); scripts/test_orna_extract.py checks
# this copy against the vendored profile.
MEDIA_ALIASES = {
    "application/javascript": "text/javascript",
    "application/ecmascript": "text/javascript",
    "text/ecmascript": "text/javascript",
    "audio/x-wav": "audio/wav",
    "audio/wave": "audio/wav",
    "audio/x-flac": "audio/flac",
}
MEDIA_ENTRIES = {
    "application/gzip": ("gz", ("gz", "tar.gz", "tgz")),
    "application/json": ("json", ("json",)),
    "application/octet-stream": ("bin", ("bin",)),
    "application/pdf": ("pdf", ("pdf",)),
    "application/wasm": ("wasm", ("wasm",)),
    "application/zip": ("zip", ("zip",)),
    "audio/flac": ("flac", ("flac",)),
    "audio/mp4": ("m4a", ("m4a", "m4b", "mp4", "mpg4")),
    "audio/mpeg": ("mp3", ("mp1", "mp2", "mp3")),
    "audio/ogg": ("ogg", ("oga", "ogg", "opus")),
    "audio/wav": ("wav", ("wav",)),
    "image/gif": ("gif", ("gif",)),
    "image/jpeg": ("jpg", ("jpe", "jpeg", "jpg")),
    "image/png": ("png", ("png",)),
    "image/svg+xml": ("svg", ("svg",)),
    "image/webp": ("webp", ("webp",)),
    "text/css": ("css", ("css",)),
    "text/javascript": ("js", ("js", "mjs")),
    "text/plain": ("txt", ("text", "txt")),
    "video/mp4": ("mp4", ("m4v", "mp4")),
    "video/webm": ("webm", ("webm",)),
}
MEDIA_TYPE_LIMIT = 1_024
MEDIA_PARAMETER_LIMIT = 32
SUFFIX_LIMIT = 32

GIT_ROUTING_VARIABLES = (
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
)
TREE_MODES = frozenset({b"40000", b"100644", b"100755", b"120000", b"160000"})
_HEX_RE = re.compile(r"[0-9a-f]+\Z")
_UUID4_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\Z")


class ExtractionError(Exception):
    """A structural violation, an unavailable object or an unsupported snapshot."""


class UnavailableObject(ExtractionError):
    """A required object is absent from the repository being read."""


def fail(message: str) -> None:
    raise ExtractionError(message)


# --------------------------------------------------------------------------
# Canonical CBOR (store-format-3.md section 1; source/29-formats.md OVB-1)
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Tag:
    number: int
    value: object


@dataclass(frozen=True)
class CborMap:
    """A canonical map, kept as pairs in encoded-key order so any key type survives."""

    entries: tuple


@dataclass(frozen=True)
class Float:
    """An Orna Float: one binary64 item, kept as its exact bits."""

    bits: bytes


def _head(major: int, argument: int) -> bytes:
    prefix = major << 5
    if argument < 24:
        return bytes([prefix | argument])
    if argument < 1 << 8:
        return bytes([prefix | 24, argument])
    if argument < 1 << 16:
        return bytes([prefix | 25]) + argument.to_bytes(2, "big")
    if argument < 1 << 32:
        return bytes([prefix | 26]) + argument.to_bytes(4, "big")
    if argument < 1 << 64:
        return bytes([prefix | 27]) + argument.to_bytes(8, "big")
    raise ValueError("CBOR argument above 64 bits")


def encode(value: object) -> bytes:
    """The canonical encoding of a decoded value (definite, shortest heads)."""
    if value is None:
        return b"\xf6"
    if value is False:
        return b"\xf4"
    if value is True:
        return b"\xf5"
    if isinstance(value, int):
        if value >= 0:
            if value < 1 << 64:
                return _head(0, value)
            return b"\xc2" + encode(value.to_bytes((value.bit_length() + 7) // 8, "big"))
        magnitude = -1 - value
        if magnitude < 1 << 64:
            return _head(1, magnitude)
        return b"\xc3" + encode(magnitude.to_bytes((magnitude.bit_length() + 7) // 8, "big"))
    if isinstance(value, Float):
        return b"\xfb" + value.bits
    if isinstance(value, bytes):
        return _head(2, len(value)) + value
    if isinstance(value, str):
        data = value.encode("utf-8")
        return _head(3, len(data)) + data
    if isinstance(value, list):
        return _head(4, len(value)) + b"".join(encode(item) for item in value)
    if isinstance(value, CborMap):
        return _head(5, len(value.entries)) + b"".join(
            encode(key) + encode(item) for key, item in value.entries
        )
    if isinstance(value, Tag):
        return _head(6, value.number) + encode(value.value)
    raise TypeError(type(value).__name__)


def make_map(pairs: list, where: str) -> CborMap:
    """A canonical map from pairs: sorted by encoded key, duplicate keys refused."""
    encoded = sorted(((encode(key), key, value) for key, value in pairs), key=lambda item: item[0])
    for previous, current in zip(encoded, encoded[1:]):
        if previous[0] == current[0]:
            fail(f"{where}: map holds a duplicate key")
    return CborMap(tuple((key, value) for _, key, value in encoded))


class _Reader:
    __slots__ = ("data", "position", "items", "where")

    def __init__(self, data: bytes, where: str) -> None:
        self.data = data
        self.position = 0
        self.items = 0
        self.where = where

    def take(self, count: int) -> bytes:
        end = self.position + count
        if end > len(self.data):
            fail(f"{self.where}: CBOR item is truncated")
        chunk = self.data[self.position : end]
        self.position = end
        return chunk

    def item(self, depth: int) -> object:
        if depth > MAX_CBOR_DEPTH:
            fail(f"{self.where}: CBOR nesting exceeds {MAX_CBOR_DEPTH} levels")
        self.items += 1
        if self.items > MAX_CBOR_ITEMS:
            fail(f"{self.where}: CBOR item count exceeds {MAX_CBOR_ITEMS}")
        initial = self.take(1)[0]
        major, info = initial >> 5, initial & 31
        if major == 7:
            if info == 20:
                return False
            if info == 21:
                return True
            if info == 22:
                return None
            if info == 27:
                bits = self.take(8)
                exponent = (int.from_bytes(bits, "big") >> 52) & 0x7FF
                mantissa = int.from_bytes(bits, "big") & ((1 << 52) - 1)
                if exponent == 0x7FF and mantissa and bits != CANONICAL_NAN:
                    fail(f"{self.where}: NaN is not the canonical 0x7ff8000000000000")
                return Float(bits)
            fail(f"{self.where}: CBOR simple value or short float {initial:#04x} is not canonical")
        if info < 24:
            argument = info
        elif info <= 27:
            width = 1 << (info - 24)
            argument = int.from_bytes(self.take(width), "big")
            if argument < {1: 24, 2: 1 << 8, 4: 1 << 16, 8: 1 << 32}[width]:
                fail(f"{self.where}: CBOR head is not the shortest form")
        else:
            fail(f"{self.where}: indefinite-length or reserved CBOR head {initial:#04x}")
        if major == 0:
            return argument
        if major == 1:
            return -1 - argument
        if major == 2:
            return self.take(argument)
        if major == 3:
            try:
                return self.take(argument).decode("utf-8")
            except UnicodeDecodeError:
                fail(f"{self.where}: CBOR text is not well-formed UTF-8")
        if major == 4:
            if argument > len(self.data) - self.position:
                fail(f"{self.where}: CBOR array is truncated")
            return [self.item(depth + 1) for _ in range(argument)]
        if major == 5:
            if 2 * argument > len(self.data) - self.position:
                fail(f"{self.where}: CBOR map is truncated")
            pairs = []
            previous = None
            for _ in range(argument):
                start = self.position
                key = self.item(depth + 1)
                raw = self.data[start : self.position]
                if previous is not None and raw <= previous:
                    fail(f"{self.where}: CBOR map keys are not in encoded order or repeat")
                previous = raw
                pairs.append((key, self.item(depth + 1)))
            return CborMap(tuple(pairs))
        # Major type 6: a tag.
        if argument not in REGISTERED_TAGS:
            fail(f"{self.where}: unregistered CBOR tag {argument}")
        payload = self.item(depth + 1)
        if argument in (2, 3):
            if not isinstance(payload, bytes) or len(payload) < 9 or payload[0] == 0:
                fail(f"{self.where}: bignum is not minimal or fits a 64-bit head")
            magnitude = int.from_bytes(payload, "big")
            return magnitude if argument == 2 else -1 - magnitude
        return Tag(argument, payload)


def decode_canonical(data: bytes, where: str) -> object:
    reader = _Reader(data, where)
    value = reader.item(0)
    if reader.position != len(data):
        fail(f"{where}: trailing bytes after the CBOR item")
    return value


def is_int(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def uuid_text(raw: bytes) -> str:
    text = raw.hex()
    return f"{text[0:8]}-{text[8:12]}-{text[12:16]}-{text[16:20]}-{text[20:32]}"


def uuid_bytes(value: object, where: str) -> bytes:
    if not isinstance(value, Tag) or value.number != UUID_TAG:
        fail(f"{where}: expected a tag-37 UUID")
    if not isinstance(value.value, bytes) or len(value.value) != 16:
        fail(f"{where}: a tag-37 UUID holds exactly 16 bytes")
    return value.value


# --------------------------------------------------------------------------
# MIME-1 annotations (profiles/media-1.json; final_model.normalize_media)
# --------------------------------------------------------------------------

_TOKEN = r"[!#$%&'*+.^_`|~0-9A-Za-z-]+"
_TOKEN_RE = re.compile(_TOKEN)
_SUFFIX_RE = re.compile(r"[a-z0-9_-]+(?:\.[a-z0-9_-]+)*\Z")


class _InvalidMedia(ValueError):
    pass


def normalize_media(value: str) -> str:
    """MIME-1 normalisation, ported from the reference model."""
    if not isinstance(value, str) or len(value.encode("utf-8")) > MEDIA_TYPE_LIMIT:
        raise _InvalidMedia
    if any(ord(character) < 32 or ord(character) > 126 for character in value):
        raise _InvalidMedia
    text = value.strip(" ")
    position = 0

    def token() -> str:
        nonlocal position
        match = _TOKEN_RE.match(text, position)
        if not match:
            raise _InvalidMedia
        position = match.end()
        return match[0]

    kind = token()
    if position >= len(text) or text[position] != "/":
        raise _InvalidMedia
    position += 1
    subtype = token()
    essence = (kind + "/" + subtype).lower()
    if kind == "*" or subtype == "*":
        raise _InvalidMedia
    essence = MEDIA_ALIASES.get(essence, essence)
    parameters: dict[str, str] = {}
    while position < len(text):
        while position < len(text) and text[position] == " ":
            position += 1
        if position == len(text):
            break
        if text[position] != ";":
            raise _InvalidMedia
        position += 1
        while position < len(text) and text[position] == " ":
            position += 1
        name = token().lower()
        if position >= len(text) or text[position] != "=":
            raise _InvalidMedia
        position += 1
        if position < len(text) and text[position] == '"':
            position += 1
            parts = []
            closed = False
            while position < len(text):
                character = text[position]
                position += 1
                if character == '"':
                    closed = True
                    break
                if character == "\\":
                    if position == len(text):
                        raise _InvalidMedia
                    character = text[position]
                    position += 1
                parts.append(character)
            if not closed:
                raise _InvalidMedia
            parameter = "".join(parts)
        else:
            parameter = token()
        if name in parameters or len(parameters) >= MEDIA_PARAMETER_LIMIT:
            raise _InvalidMedia
        parameters[name] = parameter.lower() if name == "charset" else parameter

    def quoted(parameter: str) -> str:
        if re.fullmatch(_TOKEN, parameter):
            return parameter
        return '"' + parameter.replace("\\", "\\\\").replace('"', '\\"') + '"'

    return essence + "".join(f";{name}={quoted(item)}" for name, item in sorted(parameters.items()))


def canonical_annotation(media_type: str, suffix: str | None) -> tuple[str, str | None]:
    """The canonical (media type, suffix hint) pair; raises _InvalidMedia when invalid."""
    media_type = normalize_media(media_type)
    essence = media_type.split(";", 1)[0]
    entry = MEDIA_ENTRIES.get(essence)
    preferred = entry[0] if entry else "bin"
    if suffix is not None:
        if not isinstance(suffix, str):
            raise _InvalidMedia
        suffix = suffix.lower()
        if len(suffix) > SUFFIX_LIMIT or not _SUFFIX_RE.fullmatch(suffix):
            raise _InvalidMedia
        if entry and essence != "application/octet-stream" and suffix not in entry[1]:
            raise _InvalidMedia
        if suffix == preferred:
            suffix = None
    return media_type, suffix


# --------------------------------------------------------------------------
# Logical key order (source/23-storage.md ORNA-ROW-002; 07-tables.md)
# --------------------------------------------------------------------------

_KEY_TAG_RANKS = {UUID_TAG: 5, 60000: 6, 60001: 7, 60002: 8, 60008: 9, 60015: 10, STORED_REFERENCE_TAG: 11}


def _key_rank(value: object, where: str) -> int:
    if value is None:
        return 0
    if isinstance(value, bool):
        return 1
    if isinstance(value, int):
        return 2
    if isinstance(value, str):
        return 3
    if isinstance(value, bytes):
        return 4
    if isinstance(value, list):
        return 12
    if isinstance(value, Tag) and value.number in _KEY_TAG_RANKS:
        return _KEY_TAG_RANKS[value.number]
    fail(f"{where}: key holds a value that is not a permitted key component")


def _sign(number: int) -> int:
    return (number > 0) - (number < 0)


def _decimal(value: Tag, where: str) -> tuple[int, int]:
    parts = value.value
    if not isinstance(parts, list) or len(parts) != 2 or not all(is_int(part) for part in parts):
        fail(f"{where}: Decimal key is not [coefficient, exponent]")
    coefficient, exponent = parts
    if (coefficient == 0 and exponent != 0) or (coefficient != 0 and coefficient % 10 == 0):
        fail(f"{where}: Decimal key is not normalised")
    return coefficient, exponent


def _compare_decimal(left: Tag, right: Tag, where: str) -> int:
    (left_coefficient, left_exponent) = _decimal(left, where)
    (right_coefficient, right_exponent) = _decimal(right, where)
    left_sign, right_sign = _sign(left_coefficient), _sign(right_coefficient)
    if left_sign != right_sign or left_sign == 0:
        return _sign(left_sign - right_sign)
    left_digits, right_digits = str(abs(left_coefficient)), str(abs(right_coefficient))
    # The position of the most significant digit orders magnitudes without
    # materialising 10**exponent for an adversarial exponent.
    left_magnitude = len(left_digits) + left_exponent
    right_magnitude = len(right_digits) + right_exponent
    if left_magnitude != right_magnitude:
        ordering = _sign(left_magnitude - right_magnitude)
    else:
        width = max(len(left_digits), len(right_digits))
        ordering = _sign(int(left_digits.ljust(width, "0")) - int(right_digits.ljust(width, "0")))
    return ordering * left_sign


def _pair(value: Tag, where: str, length: int) -> list:
    if not isinstance(value.value, list) or len(value.value) != length:
        fail(f"{where}: key component tag {value.number} has the wrong shape")
    return value.value


def compare_keys(left: object, right: object, where: str) -> int:
    """Compares two keys of one map by logical value, never by encoded bytes."""
    left_rank, right_rank = _key_rank(left, where), _key_rank(right, where)
    if left_rank != right_rank:
        fail(f"{where}: one ordered map holds keys of different types")
    if isinstance(left, list):
        for left_item, right_item in zip(left, right):
            ordering = compare_keys(left_item, right_item, where)
            if ordering:
                return ordering
        return _sign(len(left) - len(right))
    if left is None:
        return 0
    if isinstance(left, (bool, int, str, bytes)):
        if isinstance(left, str):
            return _sign((left > right) - (left < right))
        return (left > right) - (left < right)
    number = left.number
    if number == UUID_TAG:
        return _bytes_order(uuid_bytes(left, where), uuid_bytes(right, where))
    if number == 60000:
        return _compare_decimal(left, right, where)
    if number == 60001:
        if not isinstance(left.value, str) or not isinstance(right.value, str):
            fail(f"{where}: Date key is not text")
        return (left.value > right.value) - (left.value < right.value)
    if number == 60002:
        left_instant, right_instant = _pair(left, where, 2), _pair(right, where, 2)
        if not all(is_int(part) for part in left_instant + right_instant):
            fail(f"{where}: Instant key is not [seconds, nanosecond]")
        return (left_instant > right_instant) - (left_instant < right_instant)
    if number == 60008:
        left_enum, right_enum = _pair(left, where, 3), _pair(right, where, 3)
        if left_enum[2] is not None or right_enum[2] is not None:
            fail(f"{where}: an enum key variant carries a payload")
        return _bytes_order(
            uuid_bytes(left_enum[0], where) + uuid_bytes(left_enum[1], where),
            uuid_bytes(right_enum[0], where) + uuid_bytes(right_enum[1], where),
        )
    if number == 60015:
        if not isinstance(left.value, list) or not isinstance(right.value, list):
            fail(f"{where}: tuple key is not an array")
        return compare_keys(left.value, right.value, where)
    # Stored table reference: database, table, then the referenced key.
    left_reference, right_reference = _pair(left, where, 3), _pair(right, where, 3)
    ordering = _bytes_order(
        uuid_bytes(left_reference[0], where) + uuid_bytes(left_reference[1], where),
        uuid_bytes(right_reference[0], where) + uuid_bytes(right_reference[1], where),
    )
    return ordering or compare_keys(left_reference[2], right_reference[2], where)


def _bytes_order(left: bytes, right: bytes) -> int:
    return (left > right) - (left < right)


# --------------------------------------------------------------------------
# Canonical partition replay (store-format-3.md sections 3 and 4)
# --------------------------------------------------------------------------


def replay_partition(
    sizes: list[int],
    references: list[frozenset],
    tokens: list[bytes],
    level: int,
    anchor_domain: bytes,
    fixed: int,
    spans: list[int] | None = None,
) -> list[int]:
    """Group sizes the ORP-1 rule produces for one level of entries.

    This is ``final_model.partition``: before an entry would push a node past
    64 KiB of data, 256 entries or 256 distinct native refs the open group is
    emitted; after adding an entry the group is emitted on an anchor once it
    holds 16 entries, or at exactly 256. ``fixed`` is the node's encoded size
    apart from the entry-array head and the entries; ``spans`` adds a byte
    index's running total to that size.
    """
    groups: list[int] = []
    count = 0
    entry_bytes = 0
    total = 0
    open_references: set = set()

    def data_size(entries: int, payload: int, span_total: int) -> int:
        size = fixed + len(_head(4, entries)) + payload
        if spans is not None:
            size += len(encode(span_total))
        return size

    for index, (size, entry_references, token) in enumerate(zip(sizes, references, tokens)):
        span = spans[index] if spans is not None else 0
        if count and (
            count + 1 > MAX_PAGE_ENTRIES
            or data_size(count + 1, entry_bytes + size, total + span) > NODE_DATA_LIMIT
            or len(open_references | entry_references) > MAX_REFS
        ):
            groups.append(count)
            count, entry_bytes, total, open_references = 0, 0, 0, set()
        count += 1
        entry_bytes += size
        total += span
        open_references |= entry_references
        if data_size(count, entry_bytes, total) > NODE_DATA_LIMIT or len(open_references) > MAX_REFS:
            fail("an entry exceeds the node bounds on its own; it needed overflow lowering")
        digest = hashlib.sha256(anchor_domain + level.to_bytes(4, "big") + token).digest()
        if count == MAX_PAGE_ENTRIES or (count >= MIN_PAGE_ENTRIES and digest[-1] & ANCHOR_MASK == 0):
            groups.append(count)
            count, entry_bytes, total, open_references = 0, 0, 0, set()
    if count:
        groups.append(count)
    return groups


def _describe_groups(groups: list[int]) -> str:
    shown = ", ".join(str(group) for group in groups[:12])
    return f"[{shown}{', ...' if len(groups) > 12 else ''}]"


# --------------------------------------------------------------------------
# OGB-2 chunk boundaries (store-format-3.md section 4)
# --------------------------------------------------------------------------

GEAR = tuple(int.from_bytes(hashlib.sha256(GEAR_SEED + bytes([byte])).digest()[:8], "big") for byte in range(256))
_U64 = (1 << 64) - 1


def gear_cuts(chunk: bytes) -> tuple[bool, bool]:
    """Whether OGB-2 cuts inside ``chunk`` and whether it cuts exactly at its end.

    The accumulator restarts at every cut, so a chunk is checked on its own.
    Each step shifts the accumulator left by one bit, so only the last 64
    bytes reach any 64-bit value; the scan therefore starts 64 bytes before
    the first position where a cut is allowed.
    """
    length = len(chunk)
    if length < GEAR_MINIMUM:
        return False, False
    accumulator = 0
    gear = GEAR
    for byte in chunk[GEAR_MINIMUM - 64 : GEAR_MINIMUM - 1]:
        accumulator = ((accumulator << 1) + gear[byte]) & _U64
    position = GEAR_MINIMUM - 1
    for byte in chunk[GEAR_MINIMUM - 1 :]:
        accumulator = ((accumulator << 1) + gear[byte]) & _U64
        position += 1
        if accumulator & GEAR_MASK == 0 or position == GEAR_MAXIMUM:
            return position != length, position == length
    return False, False


class GearBoundaries:
    """Checks that consecutive chunks are exactly the canonical Gear partition."""

    def __init__(self, where: str) -> None:
        self.where = where
        self.open_chunk: str | None = None

    def push(self, chunk: bytes, oid: str) -> None:
        if self.open_chunk is not None:
            fail(
                f"{self.where}: chunk {self.open_chunk} ends before an OGB-2 cut point "
                "but is not the final chunk"
            )
        interior, at_end = gear_cuts(chunk)
        if interior:
            fail(f"{self.where}: chunk {oid} spans an OGB-2 cut point; chunk boundaries are not canonical")
        if not at_end:
            self.open_chunk = oid


# --------------------------------------------------------------------------
# Git object access through `git cat-file --batch` only
# --------------------------------------------------------------------------


def git_environment() -> dict[str, str]:
    environment = {name: value for name, value in os.environ.items() if name not in GIT_ROUTING_VARIABLES}
    # Read the recorded objects themselves: never a replacement, never a lazy
    # fetch from a promisor remote, never an interactive prompt.
    environment["GIT_NO_REPLACE_OBJECTS"] = "1"
    environment["GIT_NO_LAZY_FETCH"] = "1"
    environment["GIT_TERMINAL_PROMPT"] = "0"
    return environment


def git_directory(path: Path) -> Path | None:
    """The administrative directory of a repository rooted at ``path``, if any."""
    marker = path / ".git"
    if marker.is_dir():
        return marker
    if marker.is_file():
        try:
            text = marker.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return None
        if text.startswith("gitdir: "):
            return (path / text[len("gitdir: ") :].strip()).resolve()
        return None
    if (path / "objects").is_dir() and (path / "HEAD").is_file():
        return path
    return None


class GitObjects:
    """Verified object reads from one repository's object database."""

    def __init__(self, repository: Path, scope: str, width: int | None = None) -> None:
        self.scope = scope
        self.width = width
        self.verified: set[str] = set()
        try:
            self._process = subprocess.Popen(
                ["git", "-C", str(repository), "cat-file", "--batch"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=git_environment(),
            )
        except OSError as error:
            raise ExtractionError(f"{scope}: git could not be started: {error}") from error

    @property
    def algorithm(self) -> str:
        return "sha1" if self.width == 20 else "sha256"

    def close(self) -> None:
        process = self._process
        if process.poll() is None:
            try:
                process.stdin.close()
            except OSError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None and not stream.closed:
                stream.close()

    def _stopped(self) -> ExtractionError:
        self._process.kill()
        self._process.wait()
        detail = self._process.stderr.read().decode("utf-8", "replace").strip()
        return ExtractionError(f"{self.scope}: git cat-file stopped: {detail or 'no diagnostic'}")

    def _header(self, name: str, what: str) -> tuple[str, str, int]:
        if not name or "\n" in name:
            fail(f"{what}: invalid object name {name!r}")
        try:
            self._process.stdin.write(name.encode("utf-8") + b"\n")
            self._process.stdin.flush()
        except OSError:
            raise self._stopped() from None
        line = self._process.stdout.readline()
        if not line:
            raise self._stopped()
        fields = line.rstrip(b"\n").split(b" ")
        if len(fields) == 2 and fields[1] == b"missing":
            raise UnavailableObject(f"{what}: object {name} is unavailable in {self.scope}")
        if len(fields) == 2 and fields[1] == b"ambiguous":
            fail(f"{what}: object name {name} is ambiguous in {self.scope}")
        if len(fields) != 3 or not fields[2].isdigit():
            fail(f"{what}: unexpected git cat-file reply {line!r}")
        return fields[0].decode("ascii"), fields[1].decode("ascii"), int(fields[2])

    def _body(self, size: int) -> bytes:
        data = self._process.stdout.read(size)
        terminator = self._process.stdout.read(1)
        if len(data) != size or terminator != b"\n":
            raise self._stopped()
        return data

    def _check_hash(self, oid: str, kind: str, data: bytes, what: str) -> None:
        digest = hashlib.new(self.algorithm, f"{kind} {len(data)}".encode("ascii") + b"\0" + data).hexdigest()
        if digest != oid:
            fail(f"{what}: object {oid} does not hash to its id (corrupt or substituted object)")
        self.verified.add(oid)

    def resolve_commit(self, revision: str, what: str) -> tuple[str, bytes]:
        oid, kind, size = self._header(f"{revision}^{{commit}}", what)
        if kind != "commit":
            fail(f"{what}: {revision} does not name a commit")
        if len(oid) not in (40, 64) or not _HEX_RE.fullmatch(oid):
            fail(f"{what}: {oid!r} is not a SHA-1 or SHA-256 object id")
        self.width = len(oid) // 2
        if size > SOURCE_TREE_BYTES_LIMIT:
            fail(f"{what}: commit {oid} is larger than {SOURCE_TREE_BYTES_LIMIT} bytes")
        data = self._body(size)
        self._check_hash(oid, kind, data, what)
        return oid, data

    def read(self, oid: str, kind: str, limit: int, what: str) -> bytes:
        if len(oid) != 2 * self.width or not _HEX_RE.fullmatch(oid):
            fail(f"{what}: {oid!r} is not a {self.width}-byte native object id")
        found, actual, size = self._header(oid, what)
        if found != oid:
            fail(f"{what}: git returned {found} for {oid}")
        if actual != kind:
            fail(f"{what}: object {oid} is a {actual}; a {kind} is required")
        if size > limit:
            fail(f"{what}: {kind} {oid} holds {size} bytes, above the {limit}-byte bound")
        data = self._body(size)
        self._check_hash(oid, kind, data, what)
        return data


@dataclass(frozen=True)
class TreeEntry:
    mode: bytes
    name: bytes
    oid: str


def parse_tree(data: bytes, width: int, where: str) -> list[TreeEntry]:
    """One Git tree, refusing unknown modes, bad names and non-canonical order."""
    entries: list[TreeEntry] = []
    names: set[bytes] = set()
    previous = None
    position = 0
    while position < len(data):
        space = data.find(b" ", position)
        terminator = data.find(b"\0", space + 1) if space >= 0 else -1
        if space < 0 or terminator < 0 or terminator + 1 + width > len(data):
            fail(f"{where}: malformed Git tree")
        mode = data[position:space]
        name = data[space + 1 : terminator]
        if mode not in TREE_MODES:
            fail(f"{where}: tree entry {name!r} has unsupported mode {mode.decode('ascii', 'replace')}")
        if not name or b"/" in name or name in (b".", b"..") or name in names:
            fail(f"{where}: tree entry name {name!r} is empty, repeated or not one path component")
        sort_key = name + (b"/" if mode == b"40000" else b"\0")
        if previous is not None and sort_key <= previous:
            fail(f"{where}: tree entries are not in Git order")
        previous = sort_key
        names.add(name)
        oid = data[terminator + 1 : terminator + 1 + width].hex()
        entries.append(TreeEntry(mode, name, oid))
        position = terminator + 1 + width
    return entries


def commit_tree(data: bytes, width: int, where: str) -> str:
    first_line = data.split(b"\n", 1)[0]
    if not first_line.startswith(b"tree "):
        fail(f"{where}: commit does not start with a tree line")
    oid = first_line[5:].decode("ascii", "replace")
    if len(oid) != 2 * width or not _HEX_RE.fullmatch(oid):
        fail(f"{where}: commit names a malformed tree id")
    return oid


# --------------------------------------------------------------------------
# OGS-1 node graph
# --------------------------------------------------------------------------


@dataclass
class Node:
    oid: str
    kind: int
    data: list


def _oid(value: object, width: int, where: str) -> str:
    if not isinstance(value, bytes) or len(value) != width:
        fail(f"{where}: native object id is not a {width}-byte string")
    return value.hex()


def _length(value: object, where: str) -> int:
    if not is_int(value) or not 0 <= value <= MAX_LENGTH:
        fail(f"{where}: length or count is not an unsigned integer up to 2^63-1")
    return value


def _digest(value: object, where: str) -> bytes:
    if not isinstance(value, bytes) or len(value) != 32:
        fail(f"{where}: digest is not 32 bytes")
    return value


def _height(value: object, lowest: int, where: str) -> int:
    if not is_int(value) or not lowest <= value <= MAX_GRAPH_HEIGHT:
        fail(f"{where}: height {value!r} is outside {lowest}..{MAX_GRAPH_HEIGHT}")
    return value


def _array(value: object, length: int | None, where: str) -> list:
    if not isinstance(value, list) or (length is not None and len(value) != length):
        expected = "an array" if length is None else f"a {length}-element array"
        fail(f"{where}: expected {expected}")
    return value


def stored_blob_fields(payload: object, width: int, where: str) -> tuple[int, bytes, str, str | None, str]:
    """A ROV-3 tag-60111 reference: [length, raw_sha256, media_type, suffix_or_null, descriptor_oid]."""
    fields = _array(payload, 5, f"{where}: stored Blob reference")
    length = _length(fields[0], f"{where}: stored Blob length")
    digest = _digest(fields[1], f"{where}: stored Blob SHA-256")
    media_type, suffix = fields[2], fields[3]
    if not isinstance(media_type, str) or not (suffix is None or isinstance(suffix, str)):
        fail(f"{where}: stored Blob annotation is not (text, text-or-null)")
    try:
        canonical = canonical_annotation(media_type, suffix)
    except _InvalidMedia:
        fail(f"{where}: stored Blob annotation {media_type!r}/{suffix!r} is not valid MIME-1")
    if canonical != (media_type, suffix):
        fail(f"{where}: stored Blob annotation {media_type!r}/{suffix!r} is not in canonical MIME-1 form")
    return length, digest, media_type, suffix, _oid(fields[4], width, f"{where}: stored Blob descriptor")


def overflow_oid(payload: object, width: int, where: str) -> str:
    fields = _array(payload, 1, f"{where}: value overflow reference")
    return _oid(fields[0], width, f"{where}: value overflow reference")


def _option_payload(value: Tag, where: str) -> list:
    fields = value.value
    if not isinstance(fields, list) or not fields or not is_int(fields[0]):
        fail(f"{where}: Option is not [0] or [1, value]")
    if (fields[0] == 0 and len(fields) == 1) or (fields[0] == 1 and len(fields) == 2):
        return fields[1:]
    fail(f"{where}: Option is not [0] or [1, value]")


def value_references(value: object, width: int, where: str, references: dict[str, str]) -> None:
    """Adds the native children a stored ROV-3 value names: Blob descriptors and overflow roots."""
    if isinstance(value, Tag):
        number = value.number
        if number == STORED_BLOB_TAG:
            _add_reference(references, stored_blob_fields(value.value, width, where)[4], "tree", where)
        elif number == OVERFLOW_TAG:
            _add_reference(references, overflow_oid(value.value, width, where), "tree", where)
        elif number in (PORTABLE_BLOB_TAG, SEMANTIC_BLOB_TAG):
            fail(f"{where}: tag {number} is a foreign value profile inside a stored ROV-3 value")
        elif number == OPTION_TAG:
            for item in _option_payload(value, where):
                value_references(item, width, where, references)
        elif number == STORED_REFERENCE_TAG:
            fields = _array(value.value, 3, f"{where}: stored table reference")
            uuid_bytes(fields[0], f"{where}: stored table reference database")
            uuid_bytes(fields[1], f"{where}: stored table reference table")
            value_references(fields[2], width, where, references)
        else:
            value_references(value.value, width, where, references)
    elif isinstance(value, list):
        for item in value:
            value_references(item, width, where, references)
    elif isinstance(value, CborMap):
        for key, item in value.entries:
            value_references(key, width, where, references)
            value_references(item, width, where, references)


def _add_reference(references: dict[str, str], oid: str, kind: str, where: str) -> None:
    if references.setdefault(oid, kind) != kind:
        fail(f"{where}: object {oid} is required both as a blob and as a tree")


def direct_blob_count(value: object) -> int:
    """Blob references belonging to this value itself, not to a nested overflow value."""
    if isinstance(value, Tag):
        if value.number == STORED_BLOB_TAG:
            return 1
        if value.number == OVERFLOW_TAG:
            return 0
        return direct_blob_count(value.value)
    if isinstance(value, list):
        return sum(direct_blob_count(item) for item in value)
    if isinstance(value, CborMap):
        return sum(direct_blob_count(key) + direct_blob_count(item) for key, item in value.entries)
    return 0


def ordered_domain(domain: object, where: str) -> str:
    """The purpose of an ORP-1 map domain: ``relations`` or ``rows``."""
    if isinstance(domain, list) and domain:
        if domain[0] == "relations" and len(domain) == 2 and isinstance(domain[1], bytes) and len(domain[1]) == 16:
            return "relations"
        if (
            domain[0] == "rows"
            and len(domain) == 3
            and isinstance(domain[1], bytes)
            and len(domain[1]) == 16
            and isinstance(domain[2], bytes)
            and len(domain[2]) == 32
        ):
            return "rows"
    fail(f"{where}: ordered-map domain is neither [\"relations\", database] nor [\"rows\", table, schema digest]")


def relation_value(value: object, width: int, where: str) -> tuple[str, str, str | None, int]:
    fields = _array(value, 4, f"{where}: relation value")
    schema = _oid(fields[0], width, f"{where}: relation schema")
    primary = _oid(fields[1], width, f"{where}: relation primary map")
    secondary = None if fields[2] is None else _oid(fields[2], width, f"{where}: relation secondary map")
    return schema, primary, secondary, _length(fields[3], f"{where}: relation row count")


def entry_references(purpose: str, key: object, value: object, width: int, where: str) -> dict[str, str]:
    references: dict[str, str] = {}
    if purpose == "relations":
        if not isinstance(key, bytes) or len(key) != 16:
            fail(f"{where}: relation map key is not a 16-byte relation id")
        schema, primary, secondary, _ = relation_value(value, width, where)
        for oid in (schema, primary, secondary):
            if oid is not None:
                _add_reference(references, oid, "tree", where)
        return references
    if not isinstance(value, list) and not (isinstance(value, Tag) and value.number == OVERFLOW_TAG):
        fail(f"{where}: stored row value is neither a field array nor a tag-60113 overflow reference")
    value_references(key, width, where, references)
    value_references(value, width, where, references)
    return references


def node_tuple(value: object, width: int, where: str) -> int:
    """Checks one node's ``[1, kind, ...]`` tuple shape and returns its kind."""
    if not isinstance(value, list) or len(value) < 2:
        fail(f"{where}: node data is not a [version, kind, ...] tuple")
    version, kind = value[0], value[1]
    if not is_int(version) or version != 1:
        fail(f"{where}: unsupported node version {version!r}")
    if not is_int(kind) or kind not in NODE_KIND_NAMES:
        fail(f"{where}: unknown node kind {kind!r}")
    if kind == 0:
        _array(value, 3, where)
        _oid(value[2], width, f"{where}: relation map")
    elif kind == 1:
        _array(value, 4, where)
        entries = _array(value[3], None, f"{where}: leaf entries")
        if len(entries) > MAX_PAGE_ENTRIES:
            fail(f"{where}: leaf holds {len(entries)} entries, above {MAX_PAGE_ENTRIES}")
        for entry in entries:
            _array(entry, 2, f"{where}: leaf entry")
    elif kind == 2:
        _array(value, 5, where)
        _height(value[3], 1, f"{where}: branch height")
        entries = _array(value[4], None, f"{where}: branch entries")
        if not 1 <= len(entries) <= MAX_PAGE_ENTRIES:
            fail(f"{where}: branch holds {len(entries)} entries; 1..{MAX_PAGE_ENTRIES} are allowed")
        for entry in entries:
            fence, child, count = _array(entry, 3, f"{where}: branch entry")
            _oid(child, width, f"{where}: branch child")
            _length(count, f"{where}: branch row count")
    elif kind == 3:
        _array(value, 6, where)
        _length(value[2], f"{where}: overflow encoded length")
        _digest(value[3], f"{where}: overflow semantic digest")
        _oid(value[4], width, f"{where}: overflow byte root")
        if value[5] is not None:
            _oid(value[5], width, f"{where}: overflow dependency root")
    elif kind == 4:
        _array(value, 5, where)
        height = _height(value[2], 0, f"{where}: byte index height")
        total = _length(value[3], f"{where}: byte index total length")
        entries = _array(value[4], None, f"{where}: byte index entries")
        if not 1 <= len(entries) <= MAX_PAGE_ENTRIES:
            fail(f"{where}: byte index holds {len(entries)} entries; 1..{MAX_PAGE_ENTRIES} are allowed")
        spans = 0
        for entry in entries:
            span, target, digest = _array(entry, 3, f"{where}: byte index entry")
            if _length(span, f"{where}: span length") == 0:
                fail(f"{where}: byte index holds a zero-length span")
            _oid(target, width, f"{where}: byte index target")
            if height == 0:
                _digest(digest, f"{where}: chunk SHA-256")
            elif digest is not None:
                fail(f"{where}: a byte index above height 0 records a chunk SHA-256")
            spans += span
        if spans != total:
            fail(f"{where}: byte index spans sum to {spans}; it records a total of {total}")
    elif kind == 5:
        _array(value, 5, where)
        length = _length(value[2], f"{where}: Blob length")
        digest = _digest(value[3], f"{where}: Blob SHA-256")
        if length == 0:
            if value[4] is not None or digest != EMPTY_SHA256:
                fail(f"{where}: an empty Blob descriptor needs SHA-256 of no bytes and a null byte root")
        else:
            _oid(value[4], width, f"{where}: Blob byte root")
    elif kind == 6:
        _array(value, 5, where)
        _length(value[2], f"{where}: schema encoded length")
        _digest(value[3], f"{where}: schema digest")
        _oid(value[4], width, f"{where}: schema byte root")
    else:
        _array(value, 4, where)
        _height(value[2], 0, f"{where}: dependency index height")
        children = _array(value[3], None, f"{where}: dependency index children")
        if not 1 <= len(children) <= MAX_PAGE_ENTRIES:
            fail(f"{where}: dependency index holds {len(children)} children; 1..{MAX_PAGE_ENTRIES} are allowed")
        for child in children:
            _oid(child, width, f"{where}: dependency index child")
        if any(left >= right for left, right in zip(children, children[1:])):
            fail(f"{where}: dependency index children are not sorted and distinct")
    return kind


def node_references(node: Node, width: int, where: str) -> dict[str, str]:
    """The exact native children a node's typed payload requires (store-format-3.md section 1)."""
    data = node.data
    references: dict[str, str] = {}
    kind = node.kind
    if kind == 0:
        _add_reference(references, data[2].hex(), "tree", where)
    elif kind == 1:
        purpose = ordered_domain(data[2], where)
        for key, value in data[3]:
            for oid, child_kind in entry_references(purpose, key, value, width, where).items():
                _add_reference(references, oid, child_kind, where)
    elif kind == 2:
        ordered_domain(data[2], where)
        for fence, child, _ in data[4]:
            value_references(fence, width, where, references)
            _add_reference(references, child.hex(), "tree", where)
    elif kind == 3:
        _add_reference(references, data[4].hex(), "tree", where)
        if data[5] is not None:
            _add_reference(references, data[5].hex(), "tree", where)
    elif kind == 4:
        child_kind = "blob" if data[2] == 0 else "tree"
        for _, target, _ in data[4]:
            _add_reference(references, target.hex(), child_kind, where)
    elif kind in (5, 6):
        if data[4] is not None:
            _add_reference(references, data[4].hex(), "tree", where)
    else:
        for child in data[3]:
            _add_reference(references, child.hex(), "tree", where)
    return references


# --------------------------------------------------------------------------
# Schema descriptors (source/29-formats.md "Closed storage schema descriptor")
# --------------------------------------------------------------------------


@dataclass
class Field:
    field_id: bytes
    name: str
    type_node: object
    role: int
    fallback: list


@dataclass
class Schema:
    oid: str
    digest: bytes
    encoded_length: int
    table_id: bytes
    key_ids: list[bytes]
    fields: list[Field]
    definitions: list

    def key_fields(self) -> list[Field]:
        by_id = {field.field_id: field for field in self.fields}
        return [by_id[field_id] for field_id in self.key_ids]

    def stored_fields(self) -> list[Field]:
        return [field for field in self.fields if field.role == 1]


def _text(value: object, where: str) -> str:
    if not isinstance(value, str):
        fail(f"{where}: expected text")
    return value


def _unsigned(value: object, where: str) -> int:
    if not is_int(value) or value < 0:
        fail(f"{where}: expected an unsigned integer")
    return value


def check_type_node(node: object, where: str) -> None:
    parts = _array(node, None, f"{where}: type node")
    if not parts or not is_int(parts[0]):
        fail(f"{where}: type node has no type code")
    code = parts[0]
    if code == 0:
        _array(parts, 2, where)
        if parts[1] not in PRIMITIVE_TYPES:
            fail(f"{where}: unknown primitive type {parts[1]!r}")
    elif code in (1, 2, 9):
        _array(parts, 2, where)
        check_type_node(parts[1], where)
    elif code == 3:
        _array(parts, 2, where)
        for component in _array(parts[1], None, f"{where}: tuple components"):
            check_type_node(component, where)
    elif code == 4:
        _array(parts, 2, where)
        previous = None
        for pair in _array(parts[1], None, f"{where}: record fields"):
            name, field_type = _array(pair, 2, f"{where}: record field")
            name = _text(name, f"{where}: record field name")
            if unicodedata.normalize("NFC", name) != name or (previous is not None and name <= previous):
                fail(f"{where}: structural record names are not NFC and strictly sorted")
            previous = name
            check_type_node(field_type, where)
    elif code in (5, 8):
        _array(parts, 2, where)
        uuid_bytes(parts[1], f"{where}: type reference")
    elif code == 6:
        _array(parts, 4, where)
        uuid_bytes(parts[1], f"{where}: referenced database")
        uuid_bytes(parts[2], f"{where}: referenced table")
        for component in _array(parts[3], None, f"{where}: referenced key"):
            check_type_node(component, where)
    elif code == 7:
        _array(parts, 3, where)
        check_type_node(parts[1], where)
        uuid_bytes(parts[2], f"{where}: unit reference")
    else:
        fail(f"{where}: unknown type code {code}")


def _type_references(node: list, definitions: set[bytes], units: set[bytes], where: str) -> None:
    code = node[0]
    if code in (5, 8) and node[1].value not in definitions:
        fail(f"{where}: type node names a definition the descriptor does not hold")
    if code == 7 and node[2].value not in units:
        fail(f"{where}: Quantity names a unit the descriptor does not define")
    if code in (1, 2, 9, 7):
        _type_references(node[1], definitions, units, where)
    elif code == 3:
        for component in node[1]:
            _type_references(component, definitions, units, where)
    elif code == 4:
        for _, field_type in node[1]:
            _type_references(field_type, definitions, units, where)
    elif code == 6:
        for component in node[3]:
            _type_references(component, definitions, units, where)


def _field(entry: object, nested: bool, where: str) -> Field:
    field_id, name, field_type, role, fallback = _array(entry, 5, f"{where}: field descriptor")
    field_id = uuid_bytes(field_id, f"{where}: field id")
    name = _text(name, f"{where}: field name")
    if nested and not name:
        fail(f"{where}: a nested field has an empty name")
    check_type_node(field_type, f"{where}: field {name!r}")
    role = _unsigned(role, f"{where}: field role")
    if role not in FIELD_ROLES:
        fail(f"{where}: field {name!r} has unknown role {role}")
    fallback = _array(fallback, None, f"{where}: field fallback")
    if not fallback or not is_int(fallback[0]):
        fail(f"{where}: field {name!r} fallback has no kind")
    kind = fallback[0]
    if not (
        (kind == 0 and len(fallback) == 1)
        or (kind == 1 and len(fallback) == 2 and role == 1)
        or (kind == 2 and len(fallback) == 2 and role == 2 and isinstance(fallback[1], bytes) and len(fallback[1]) == 32)
    ):
        fail(f"{where}: field {name!r} fallback is not [0], [1, value] or [2, digest] for its role")
    return Field(field_id, name, field_type, role, fallback)


def _field_list(entries: object, nested: bool, where: str) -> list[Field]:
    fields = [_field(entry, nested, where) for entry in _array(entries, None, f"{where}: fields")]
    if any(left.field_id >= right.field_id for left, right in zip(fields, fields[1:])):
        fail(f"{where}: fields are not ordered by unsigned field-id bytes")
    return fields


def _definition(entry: object, where: str) -> tuple[bytes, int, list[Field]]:
    type_id, kind, body = _array(entry, 3, f"{where}: nominal definition")
    type_id = uuid_bytes(type_id, f"{where}: definition id")
    kind = _unsigned(kind, f"{where}: definition kind")
    nested: list[Field] = []
    if kind == 0:
        nested = _field_list(body, True, f"{where}: record definition")
    elif kind == 1:
        previous = None
        for variant in _array(body, None, f"{where}: enum variants"):
            variant_id, name, payload = _array(variant, 3, f"{where}: enum variant")
            variant_id = uuid_bytes(variant_id, f"{where}: variant id")
            if previous is not None and variant_id <= previous:
                fail(f"{where}: enum variants are not ordered by variant id")
            previous = variant_id
            _text(name, f"{where}: variant name")
            nested.extend(_field_list(payload, True, f"{where}: variant payload"))
    elif kind == 2:
        base, assertions = _array(body, 2, f"{where}: refined definition")
        check_type_node(base, f"{where}: refined base")
        for digest in _array(assertions, None, f"{where}: assertion digests"):
            _digest(digest, f"{where}: assertion digest")
    elif kind == 3:
        parts = _array(body, 6, f"{where}: unit definition")
        if not all(is_int(item) for item in _array(parts[0], None, f"{where}: dimension vector")):
            fail(f"{where}: unit dimension vector holds a non-integer")
        if not all(is_int(item) for item in parts[1:5]) or not isinstance(parts[5], bool):
            fail(f"{where}: unit scale/offset is not integers plus an affine flag")
    elif kind == 4:
        code, digits = _array(body, 2, f"{where}: currency definition")
        if len(_text(code, f"{where}: currency code")) != 3 or _unsigned(digits, f"{where}: minor digits") > 18:
            fail(f"{where}: currency definition is not [three-letter code, minor digits up to 18]")
    else:
        fail(f"{where}: unknown nominal definition kind {kind}")
    return type_id, kind, nested


def parse_descriptor(raw: object, oid: str, digest: bytes, length: int, relation_id: bytes, where: str) -> Schema:
    if not isinstance(raw, CborMap):
        fail(f"{where}: schema descriptor is not a map")
    entries = {}
    for key, value in raw.entries:
        if not is_int(key):
            fail(f"{where}: schema descriptor key {key!r} is not an integer")
        entries[key] = value
    if sorted(entries) != [0, 1, 2, 3, 4]:
        fail(f"{where}: schema descriptor keys are {sorted(entries)}; exactly 0..4 are defined")
    if entries[0] != 1 or not is_int(entries[0]):
        fail(f"{where}: unsupported schema descriptor version {entries[0]!r}")
    table_id = uuid_bytes(entries[1], f"{where}: table id")
    if table_id != relation_id:
        fail(f"{where}: schema describes table {table_id.hex()}, not relation {relation_id.hex()}")
    fields = _field_list(entries[3], False, where)
    field_ids = {field.field_id for field in fields}
    key_ids = [uuid_bytes(item, f"{where}: key field id") for item in _array(entries[2], None, f"{where}: key fields")]
    key_roles = [field.field_id for field in fields if field.role == 0]
    if (
        not key_ids
        or len(set(key_ids)) != len(key_ids)
        or not set(key_ids) <= field_ids
        or set(key_ids) != set(key_roles)
        or len(key_ids) != len(key_roles)
    ):
        fail(f"{where}: primary-key field ids are not exactly the role-0 fields, each once")
    definitions = [_definition(entry, where) for entry in _array(entries[4], None, f"{where}: definitions")]
    if any(left[0] >= right[0] for left, right in zip(definitions, definitions[1:])):
        fail(f"{where}: nominal definitions are not ordered by unsigned type-id bytes")
    definition_ids = {definition[0] for definition in definitions}
    units = {definition[0] for definition in definitions if definition[1] == 3}
    for field in fields + [nested for definition in definitions for nested in definition[2]]:
        _type_references(field.type_node, definition_ids, units, f"{where}: field {field.name!r}")
    for definition in definitions:
        if definition[1] == 2:
            refined = next(entry for entry in entries[4] if entry[0].value == definition[0])
            _type_references(refined[2][0], definition_ids, units, f"{where}: refined definition")
    return Schema(oid, digest, length, table_id, key_ids, fields, entries[4])


# --------------------------------------------------------------------------
# `.orna/database.orna` (store-format-3.md section 1; 19-repository.md)
# --------------------------------------------------------------------------


def canonical_database_text(database_id: str) -> bytes:
    return f'{{\n    repository_format: {REPOSITORY_FORMAT},\n    database_id: "{database_id}",\n}}\n'.encode("utf-8")


def parse_database_record(data: bytes, where: str) -> tuple[str, bool]:
    """The database UUID of a format-3 ``database.orna`` and whether its text is canonical.

    The record is one literal row with exactly ``repository_format`` and
    ``database_id``: whitespace and comments are allowed, imports, calls and
    defaults are not.
    """
    if len(data) > DATABASE_RECORD_LIMIT:
        fail(f"{where}: database.orna exceeds {DATABASE_RECORD_LIMIT} bytes")
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        fail(f"{where}: database.orna is not UTF-8")
    tokens = _record_tokens(text, where)
    position = 0

    def expect(kind: str) -> tuple[str, object]:
        nonlocal position
        if position >= len(tokens) or tokens[position][0] != kind:
            fail(f"{where}: database.orna is not one literal record ({kind} expected)")
        token = tokens[position]
        position += 1
        return token

    expect("{")
    fields: dict[str, object] = {}
    while position < len(tokens) and tokens[position][0] != "}":
        name = expect("name")[1]
        expect(":")
        if position >= len(tokens) or tokens[position][0] not in ("int", "str"):
            fail(f"{where}: database.orna field {name} is not a literal")
        literal = tokens[position][1]
        position += 1
        if name in fields:
            fail(f"{where}: database.orna repeats field {name}")
        fields[name] = literal
        if position < len(tokens) and tokens[position][0] == ",":
            position += 1
        elif position < len(tokens) and tokens[position][0] != "}":
            fail(f"{where}: database.orna fields are not comma separated")
    expect("}")
    if position != len(tokens):
        fail(f"{where}: database.orna holds more than one record")
    if set(fields) != {"repository_format", "database_id"}:
        fail(f"{where}: database.orna fields are {sorted(fields)}; exactly repository_format and database_id are allowed")
    if fields["repository_format"] != REPOSITORY_FORMAT:
        fail(f"{where}: database.orna records repository_format {fields['repository_format']!r}, not {REPOSITORY_FORMAT}")
    database_id = fields["database_id"]
    if not isinstance(database_id, str) or not _UUID4_RE.fullmatch(database_id):
        fail(f"{where}: database_id is not a canonical lowercase UUIDv4")
    return database_id, data == canonical_database_text(database_id)


def _record_tokens(text: str, where: str) -> list[tuple[str, object]]:
    tokens: list[tuple[str, object]] = []
    position = 0
    while position < len(text):
        character = text[position]
        if character in " \t\r\n":
            position += 1
        elif text.startswith("//", position):
            end = text.find("\n", position)
            position = len(text) if end < 0 else end
        elif text.startswith("/*", position):
            depth, position = 1, position + 2
            while depth:
                if position >= len(text):
                    fail(f"{where}: database.orna has an unterminated block comment")
                if text.startswith("/*", position):
                    depth, position = depth + 1, position + 2
                elif text.startswith("*/", position):
                    depth, position = depth - 1, position + 2
                else:
                    position += 1
        elif character in "{}:,":
            tokens.append((character, None))
            position += 1
        elif character == '"':
            value, position = _string_literal(text, position + 1, where)
            tokens.append(("str", value))
        elif character.isdigit():
            match = re.compile(r"0|[1-9][0-9]*").match(text, position)
            if match is None or (match.end() < len(text) and (text[match.end()].isalnum() or text[match.end()] == "_")):
                fail(f"{where}: database.orna holds a malformed integer literal")
            tokens.append(("int", int(match[0])))
            position = match.end()
        else:
            match = re.compile(r"[A-Za-z_][A-Za-z0-9_]*").match(text, position)
            if match is None:
                fail(f"{where}: database.orna holds unexpected character {character!r}")
            tokens.append(("name", match[0]))
            position = match.end()
    return tokens


def _string_literal(text: str, position: int, where: str) -> tuple[str, int]:
    escapes = {'"': '"', "\\": "\\", "n": "\n", "r": "\r", "t": "\t", "0": "\0"}
    parts = []
    while True:
        if position >= len(text):
            fail(f"{where}: database.orna has an unterminated string")
        character = text[position]
        if character == '"':
            return "".join(parts), position + 1
        if character == "{" or ord(character) < 32:
            fail(f"{where}: database.orna string is not a plain literal")
        if character == "\\":
            escape = text[position + 1 : position + 2]
            if escape in escapes:
                parts.append(escapes[escape])
                position += 2
                continue
            match = re.compile(r"u\{([0-9A-Fa-f]{1,6})\}").match(text, position + 1)
            if match is None:
                fail(f"{where}: database.orna string has an invalid escape")
            scalar = int(match[1], 16)
            if scalar > 0x10FFFF or 0xD800 <= scalar <= 0xDFFF:
                fail(f"{where}: database.orna string escapes a non-scalar value")
            parts.append(chr(scalar))
            position = match.end()
            continue
        parts.append(character)
        position += 1


# --------------------------------------------------------------------------
# Extraction
# --------------------------------------------------------------------------


class BlobOutput:
    """Content-addressed Blob files under ``DIR/blobs``."""

    def __init__(self, directory: Path) -> None:
        self.directory = directory / "blobs"
        try:
            self.directory.mkdir(parents=True, exist_ok=True)
        except OSError as error:
            raise ExtractionError(f"output directory {directory} cannot be created: {error}") from error
        self.records: dict[str, dict] = {}

    def write(self, descriptor: str, length: int, digest: bytes, produce: Callable[[Callable[[bytes], None]], None], where: str) -> dict:
        name = digest.hex()
        handle = tempfile.NamedTemporaryFile(dir=self.directory, prefix=".partial-", delete=False)
        partial = Path(handle.name)
        hasher = hashlib.sha256()
        written = 0
        try:
            with handle:

                def sink(chunk: bytes) -> None:
                    nonlocal written
                    handle.write(chunk)
                    hasher.update(chunk)
                    written += len(chunk)

                produce(sink)
            if written != length or hasher.digest() != digest:
                fail(
                    f"{where}: Blob descriptor {descriptor} reassembles to {written} bytes with SHA-256 "
                    f"{hasher.hexdigest()}; it records {length} bytes and {name}"
                )
            target = self.directory / name
            if target.exists():
                # Equal SHA-256 alone never overrules unequal bytes.
                if target.read_bytes() != partial.read_bytes():
                    fail(f"{where}: {target} already holds different bytes with SHA-256 {name}")
                partial.unlink()
            else:
                os.replace(partial, target)
        finally:
            if partial.exists():
                partial.unlink()
        record = self.records.setdefault(
            name, {"sha256": name, "length": length, "path": f"blobs/{name}", "descriptors": []}
        )
        if descriptor not in record["descriptors"]:
            record["descriptors"].append(descriptor)
        return record


class Graph:
    """Verified reads of one repository's format-3 native graph."""

    def __init__(self, objects: GitObjects, blobs: BlobOutput) -> None:
        self.objects = objects
        self.width = objects.width
        self.blobs = blobs
        self.nodes: dict[str, Node] = {}
        self.descriptors: dict[str, dict] = {}
        self.overflows: dict[str, object] = {}

    def node(self, oid: str, where: str) -> Node:
        cached = self.nodes.get(oid)
        if cached is not None:
            return cached
        here = f"{where}: node {oid}"
        envelope = {
            entry.name: entry for entry in parse_tree(self.objects.read(oid, "tree", NODE_TREE_LIMIT, here), self.width, here)
        }
        unexpected = sorted(set(envelope) - {b"data", b"refs"})
        if unexpected:
            fail(f"{here}: envelope holds {unexpected[0].decode('utf-8', 'replace')!r}; only data and refs are allowed")
        data_entry = envelope.get(b"data")
        if data_entry is None or data_entry.mode != b"100644":
            fail(f"{here}: envelope needs a 100644 data blob")
        refs_entry = envelope.get(b"refs")
        if refs_entry is not None and refs_entry.mode != b"40000":
            fail(f"{here}: refs is not a 040000 tree")
        data = decode_canonical(self.objects.read(data_entry.oid, "blob", NODE_DATA_LIMIT, f"{here} data"), f"{here} data")
        kind = node_tuple(data, self.width, here)
        node = Node(oid, kind, data)
        actual: dict[str, str] = {}
        if refs_entry is not None:
            for entry in parse_tree(self.objects.read(refs_entry.oid, "tree", REFS_TREE_LIMIT, f"{here} refs"), self.width, f"{here} refs"):
                if entry.name != entry.oid.encode("ascii"):
                    fail(f"{here}: reference {entry.name!r} is not named by its own lowercase object id")
                if entry.mode == b"100644":
                    actual[entry.oid] = "blob"
                elif entry.mode == b"40000":
                    actual[entry.oid] = "tree"
                else:
                    fail(
                        f"{here}: reference {entry.oid} has mode {entry.mode.decode('ascii')}; "
                        "children are 100644 blobs or 040000 trees, never executable, symlink or gitlink"
                    )
            if not actual:
                fail(f"{here}: refs tree is empty; it must be absent when a node has no dependencies")
            if len(actual) > MAX_REFS:
                fail(f"{here}: node names {len(actual)} children, above {MAX_REFS}")
        expected = node_references(node, self.width, here)
        for child, child_kind in sorted(expected.items()):
            if child not in actual:
                fail(f"{here}: missing native reference {child} ({child_kind}) in refs")
            if actual[child] != child_kind:
                fail(f"{here}: reference {child} is a {actual[child]} entry; the payload needs a {child_kind}")
        for child in sorted(set(actual) - set(expected)):
            fail(f"{here}: refs holds unreferenced child {child}")
        self.nodes[oid] = node
        return node

    def node_of_kind(self, oid: str, kind: int, where: str) -> Node:
        node = self.node(oid, where)
        if node.kind != kind:
            fail(f"{where}: node {oid} is a {NODE_KIND_NAMES[node.kind]} node; a {NODE_KIND_NAMES[kind]} node is required")
        return node

    # ---- ORP-1 ordered maps -------------------------------------------------

    def ordered_map(self, root_oid: str, domain: list, where: str) -> list[tuple[object, object]]:
        """Every entry of one ORP-1 map in logical key order, after verifying the whole map."""
        domain_bytes = encode(domain)
        purpose = ordered_domain(domain, where)
        root = self.node(root_oid, where)
        if root.kind not in (1, 2):
            fail(f"{where}: map root {root_oid} is a {NODE_KIND_NAMES[root.kind]} node")
        if root.kind == 2 and len(root.data[4]) < 2:
            fail(f"{where}: map root {root_oid} is a branch with one child; a one-child root collapses")
        levels = [[root]]
        while levels[-1][0].kind == 2:
            height = levels[-1][0].data[3]
            children = []
            for parent in levels[-1]:
                self._check_page(parent, domain_bytes, height, where)
                for fence, child_bytes, count in parent.data[4]:
                    child = self.node(child_bytes.hex(), where)
                    if (height == 1 and child.kind != 1) or (height > 1 and (child.kind != 2 or child.data[3] != height - 1)):
                        fail(f"{where}: branch {parent.oid} at height {height} names {child.oid}, which is not a page at height {height - 1}")
                    child_entries = child.data[3] if child.kind == 1 else child.data[4]
                    if not child_entries:
                        fail(f"{where}: page {child.oid} is empty; only an empty map's root may be empty")
                    if encode(child_entries[-1][0]) != encode(fence):
                        fail(
                            f"{where}: branch {parent.oid} fence does not equal the last key of child {child.oid}; "
                            "child ranges overlap or leave gaps"
                        )
                    child_count = len(child_entries) if child.kind == 1 else sum(entry[2] for entry in child_entries)
                    if child_count != count:
                        fail(f"{where}: branch {parent.oid} counts {count} rows under {child.oid}; it holds {child_count}")
                    children.append(child)
            levels.append(children)
        entries: list[tuple[object, object]] = []
        previous = None
        for leaf in levels[-1]:
            self._check_page(leaf, domain_bytes, 0, where)
            for key, value in leaf.data[3]:
                if previous is not None and compare_keys(previous, key, where) >= 0:
                    fail(
                        f"{where}: page {leaf.oid} is out of logical key order: key {_short(key)} "
                        f"does not follow {_short(previous)}"
                    )
                previous = key
                entries.append((key, value))
        self._check_map_partition(levels, domain_bytes, purpose, where)
        return entries

    def _check_page(self, node: Node, domain_bytes: bytes, height: int, where: str) -> None:
        if encode(node.data[2]) != domain_bytes:
            fail(f"{where}: page {node.oid} belongs to another map domain")
        if node.kind == 2:
            if node.data[3] != height:
                fail(f"{where}: branch {node.oid} records height {node.data[3]}; it sits at height {height}")
            fences = [entry[0] for entry in node.data[4]]
            for left, right in zip(fences, fences[1:]):
                if compare_keys(left, right, where) >= 0:
                    fail(f"{where}: branch {node.oid} fences are out of logical key order")
        elif height != 0:
            fail(f"{where}: leaf {node.oid} sits at height {height}")

    def _check_map_partition(self, levels: list[list[Node]], domain_bytes: bytes, purpose: str, where: str) -> None:
        for height, level in enumerate(reversed(levels)):
            if height == 0:
                entries = [entry for leaf in level for entry in leaf.data[3]]
                if not entries:
                    continue
                sizes = [1 + len(encode(key)) + len(encode(value)) for key, value in entries]
                tokens = [encode(key) for key, _ in entries]
                references = [frozenset(entry_references(purpose, key, value, self.width, where)) for key, value in entries]
                fixed = 3 + len(domain_bytes)
            else:
                entries = [entry for branch in level for entry in branch.data[4]]
                sizes = [1 + len(encode(fence)) + len(encode(child)) + len(encode(count)) for fence, child, count in entries]
                tokens = [encode(fence) for fence, _, _ in entries]
                references = []
                for fence, child, _ in entries:
                    fence_references: dict[str, str] = {}
                    value_references(fence, self.width, where, fence_references)
                    references.append(frozenset(fence_references) | {child.hex()})
                fixed = 3 + len(domain_bytes) + len(encode(height))
            expected = replay_partition(sizes, references, tokens, height, ORP_ANCHOR_DOMAIN, fixed)
            actual = [len(node.data[3] if node.kind == 1 else node.data[4]) for node in level]
            if expected != actual:
                fail(
                    f"{where}: pages at height {height} are not the canonical ORP-1 partition: "
                    f"expected entry counts {_describe_groups(expected)}, found {_describe_groups(actual)}"
                )

    # ---- OGB-2 byte indexes -------------------------------------------------

    def byte_index(self, root_oid: str, length: int, sink: Callable[[bytes], None], where: str) -> None:
        """Streams the bytes under one byte index after checking every chunk and index node."""
        root = self.node_of_kind(root_oid, 4, where)
        if root.data[3] != length:
            fail(f"{where}: byte index {root_oid} totals {root.data[3]} bytes; its owner records {length}")
        if root.data[2] > 0 and len(root.data[4]) < 2:
            fail(f"{where}: byte index root {root_oid} has one child; a one-child root collapses")
        levels: dict[int, list[Node]] = {}
        boundaries = GearBoundaries(where)

        def walk(node: Node) -> None:
            height = node.data[2]
            levels.setdefault(height, []).append(node)
            for span, target, digest in node.data[4]:
                target_oid = target.hex()
                if height == 0:
                    if span > GEAR_MAXIMUM:
                        fail(f"{where}: byte index {node.oid} records a {span}-byte chunk, above {GEAR_MAXIMUM}")
                    chunk = self.objects.read(target_oid, "blob", GEAR_MAXIMUM, f"{where}: chunk")
                    if len(chunk) != span:
                        fail(f"{where}: chunk {target_oid} holds {len(chunk)} bytes; byte index {node.oid} records {span}")
                    if hashlib.sha256(chunk).digest() != digest:
                        fail(f"{where}: chunk {target_oid} does not match the SHA-256 byte index {node.oid} records (corrupt chunk)")
                    boundaries.push(chunk, target_oid)
                    sink(chunk)
                else:
                    child = self.node_of_kind(target_oid, 4, where)
                    if child.data[2] != height - 1:
                        fail(f"{where}: byte index {node.oid} at height {height} names {target_oid} at height {child.data[2]}")
                    if child.data[3] != span:
                        fail(f"{where}: byte index {node.oid} records {span} bytes for {target_oid}, which totals {child.data[3]}")
                    walk(child)

        walk(root)
        for height in sorted(levels):
            entries = [entry for node in levels[height] for entry in node.data[4]]
            if height == 0:
                tokens = [encode([span, digest]) for span, _, digest in entries]
            else:
                tokens = [target for _, target, _ in entries]
            sizes = [1 + len(encode(span)) + len(encode(target)) + len(encode(digest)) for span, target, digest in entries]
            references = [frozenset({target.hex()}) for _, target, _ in entries]
            expected = replay_partition(
                sizes,
                references,
                tokens,
                height,
                OGB_ANCHOR_DOMAIN,
                3 + len(encode(height)),
                [span for span, _, _ in entries],
            )
            actual = [len(node.data[4]) for node in levels[height]]
            if expected != actual:
                fail(
                    f"{where}: byte index nodes at height {height} are not the canonical OGB-2 grouping: "
                    f"expected entry counts {_describe_groups(expected)}, found {_describe_groups(actual)}"
                )

    def full_bytes(self, root_oid: str, length: int, limit: int, where: str) -> bytes:
        if length > limit:
            fail(f"{where}: {length} bytes exceed this extractor's {limit}-byte bound")
        buffer = bytearray()
        self.byte_index(root_oid, length, buffer.extend, where)
        return bytes(buffer)

    def dependency_index(self, root_oid: str, where: str) -> list[str]:
        """The sorted native children one kind-7 dependency index lists."""
        root = self.node_of_kind(root_oid, 7, where)
        if root.data[2] > 0 and len(root.data[3]) < 2:
            fail(f"{where}: dependency index root {root_oid} has one child; a one-child root collapses")
        levels: dict[int, list[Node]] = {}
        children: list[bytes] = []

        def walk(node: Node) -> None:
            height = node.data[2]
            levels.setdefault(height, []).append(node)
            for child in node.data[3]:
                if height == 0:
                    if children and child <= children[-1]:
                        fail(f"{where}: dependency index {root_oid} is not sorted by object id and deduplicated")
                    children.append(child)
                else:
                    nested = self.node_of_kind(child.hex(), 7, where)
                    if nested.data[2] != height - 1:
                        fail(f"{where}: dependency index {node.oid} names {nested.oid} at the wrong height")
                    walk(nested)

        walk(root)
        for height in sorted(levels):
            entries = [child for node in levels[height] for child in node.data[3]]
            expected = replay_partition(
                [len(encode(child)) for child in entries],
                [frozenset({child.hex()}) for child in entries],
                entries,
                height,
                ORP_ANCHOR_DOMAIN,
                3 + len(encode(height)),
            )
            actual = [len(node.data[3]) for node in levels[height]]
            if expected != actual:
                fail(f"{where}: dependency index nodes at height {height} are not the canonical partition")
        return [child.hex() for child in children]

    # ---- Blobs, overflow and values ----------------------------------------

    def blob(self, payload: object, where: str) -> dict:
        length, digest, media_type, suffix, descriptor = stored_blob_fields(payload, self.width, where)
        record = self.descriptors.get(descriptor)
        if record is None:
            node = self.node_of_kind(descriptor, 5, where)
            if node.data[2] != length or node.data[3] != digest:
                fail(f"{where}: Blob reference and descriptor {descriptor} disagree on (length, SHA-256)")
            here = f"{where}: Blob descriptor {descriptor}"

            def produce(sink: Callable[[bytes], None]) -> None:
                if length:
                    self.byte_index(node.data[4].hex(), length, sink, here)

            record = self.blobs.write(descriptor, length, digest, produce, here)
            self.descriptors[descriptor] = record
        return {
            "descriptor": descriptor,
            "length": length,
            "media_type": media_type,
            "path": record["path"],
            "sha256": digest.hex(),
            "suffix": suffix,
        }

    def overflow(self, oid: str, where: str, active: frozenset) -> object:
        """The verified value one kind-3 overflow node stores, with nested overflow expanded."""
        if oid in active:
            fail(f"{where}: value overflow {oid} refers back to itself")
        cached = self.overflows.get(oid)
        if cached is not None:
            return cached
        here = f"{where}: value overflow {oid}"
        node = self.node_of_kind(oid, 3, where)
        _, _, length, digest, byte_root, dependency_root = node.data
        encoded = self.full_bytes(byte_root.hex(), length, ROW_CANONICAL_LIMIT, here)
        stored = decode_canonical(encoded, here)
        direct: dict[str, str] = {}
        value_references(stored, self.width, here, direct)
        if length <= ROW_INLINE_LIMIT and direct_blob_count(stored) <= ROW_INLINE_REFERENCES:
            fail(f"{here}: holds a value that fits inline; ROV-3 lowers only values above 8 KiB or 32 Blob references")
        listed = set(self.dependency_index(dependency_root.hex(), here)) if dependency_root is not None else set()
        if listed != set(direct):
            fail(f"{here}: dependency index does not list exactly the references inside the overflow bytes")
        expanded = self.expand(stored, here, active | {oid})
        if hashlib.sha256(encode(semantic(expanded, here))).digest() != digest:
            fail(f"{here}: semantic digest does not match the SOV-3 value of its bytes")
        self.overflows[oid] = expanded
        return expanded

    def expand(self, value: object, where: str, active: frozenset) -> object:
        if isinstance(value, Tag):
            if value.number == OVERFLOW_TAG:
                return self.overflow(overflow_oid(value.value, self.width, where), where, active)
            if value.number == STORED_BLOB_TAG:
                return value
            return Tag(value.number, self.expand(value.value, where, active))
        if isinstance(value, list):
            return [self.expand(item, where, active) for item in value]
        if isinstance(value, CborMap):
            return make_map([(self.expand(key, where, active), self.expand(item, where, active)) for key, item in value.entries], where)
        return value

    def json_value(self, value: object, where: str) -> object:
        """A deterministic, lossless JSON rendering of one decoded stored value."""
        if value is None or isinstance(value, (bool, int, str)):
            return value
        if isinstance(value, bytes):
            return {"bytes": value.hex()}
        if isinstance(value, Float):
            number = struct.unpack(">d", value.bits)[0]
            return {"float": repr(number)}
        if isinstance(value, list):
            return [self.json_value(item, where) for item in value]
        if isinstance(value, CborMap):
            return {"map": [[self.json_value(key, where), self.json_value(item, where)] for key, item in value.entries]}
        if value.number == UUID_TAG:
            return {"uuid": uuid_text(uuid_bytes(value, where))}
        if value.number == STORED_BLOB_TAG:
            return {"blob": self.blob(value.value, where)}
        return {"tag": value.number, "value": self.json_value(value.value, where)}

    # ---- Relations ----------------------------------------------------------

    def schema(self, oid: str, relation_id: bytes, where: str) -> Schema:
        here = f"{where}: schema {oid}"
        node = self.node_of_kind(oid, 6, where)
        _, _, length, digest, byte_root = node.data
        encoded = self.full_bytes(byte_root.hex(), length, SCHEMA_BYTES_LIMIT, here)
        if hashlib.sha256(encoded).digest() != digest:
            fail(f"{here}: schema digest is not the SHA-256 of its descriptor bytes")
        raw = decode_canonical(encoded, here)
        if _holds_profile_tag(raw):
            fail(f"{here}: descriptor holds a format-3 profile tag")
        return parse_descriptor(raw, oid, digest, length, relation_id, here)

    def relation(self, relation_id: bytes, value: object, where: str) -> dict:
        schema_oid, primary, secondary, row_count = relation_value(value, self.width, where)
        here = f"{where}: relation {relation_id.hex()}"
        schema = self.schema(schema_oid, relation_id, here)
        rows = self.ordered_map(primary, ["rows", relation_id, schema.digest], f"{here}: row map {primary}")
        if len(rows) != row_count:
            fail(f"{here}: relation map records {row_count} rows; the row map holds {len(rows)}")
        key_fields = schema.key_fields()
        stored_fields = schema.stored_fields()
        extracted = []
        for key, value in rows:
            row_where = f"{here}: row {_short(key)}"
            if len(encode(key)) > KEY_CANONICAL_LIMIT:
                fail(f"{row_where}: canonical key exceeds {KEY_CANONICAL_LIMIT} bytes")
            if isinstance(value, list):
                inline: dict[str, str] = {}
                value_references(value, self.width, row_where, inline)
                if len(encode(value)) > ROW_INLINE_LIMIT or len(inline) > ROW_INLINE_REFERENCES:
                    fail(f"{row_where}: inline value exceeds 8 KiB or 32 references; ROV-3 requires overflow")
            fields = self.expand(value, row_where, frozenset())
            if not isinstance(fields, list):
                fail(f"{row_where}: stored value is not the stored-field array")
            if len(fields) != len(stored_fields):
                fail(f"{row_where}: holds {len(fields)} stored fields; the schema declares {len(stored_fields)}")
            components = [key] if len(key_fields) == 1 else key
            if len(key_fields) == 1 and isinstance(key, list):
                fail(f"{row_where}: a one-field key is encoded as a composite array")
            if not isinstance(components, list) or len(components) != len(key_fields):
                fail(f"{row_where}: key has {len(components) if isinstance(components, list) else 1} components; the schema declares {len(key_fields)}")
            extracted.append(
                {
                    "key": self.json_value(key, row_where),
                    "key_fields": [
                        {"field_id": uuid_text(field.field_id), "name": field.name, "value": self.json_value(component, row_where)}
                        for field, component in zip(key_fields, components)
                    ],
                    "fields": [
                        {
                            "field_id": uuid_text(field.field_id),
                            "index": index,
                            "name": field.name,
                            "value": self.json_value(item, row_where),
                        }
                        for index, (field, item) in enumerate(zip(stored_fields, fields))
                    ],
                }
            )
        return {
            "primary_root": primary,
            "relation_id": relation_id.hex(),
            "row_count": row_count,
            "rows": extracted,
            "schema": {
                "definitions": self.json_value(schema.definitions, here),
                "digest": schema.digest.hex(),
                "encoded_length": schema.encoded_length,
                "fields": [
                    {
                        "fallback": self.json_value(field.fallback, here),
                        "field_id": uuid_text(field.field_id),
                        "name": field.name,
                        "role": FIELD_ROLES[field.role],
                        "type": self.json_value(field.type_node, here),
                    }
                    for field in schema.fields
                ],
                "key": [field.name for field in key_fields],
                "node": schema.oid,
                "table_id": uuid_text(schema.table_id),
            },
            # Secondary maps are derived indexes; no persisted index is
            # trusted against a different row root, so rows never need one.
            "secondary_root": secondary,
        }

    def store(self, store_root: str, database_id: bytes, where: str) -> list[dict]:
        root = self.node_of_kind(store_root, 0, where)
        relation_map = root.data[2].hex()
        relations = self.ordered_map(relation_map, ["relations", database_id], f"{where}: relation map {relation_map}")
        return [self.relation(relation_id, value, where) for relation_id, value in relations]


def semantic(value: object, where: str) -> object:
    """The SOV-3 form of an expanded stored value: each tag-60111 reference becomes tag 60112."""
    if isinstance(value, Tag):
        if value.number == STORED_BLOB_TAG:
            return Tag(SEMANTIC_BLOB_TAG, value.value[:4])
        if value.number == OVERFLOW_TAG:
            fail(f"{where}: an overflow reference survived expansion")
        return Tag(value.number, semantic(value.value, where))
    if isinstance(value, list):
        return [semantic(item, where) for item in value]
    if isinstance(value, CborMap):
        return make_map([(semantic(key, where), semantic(item, where)) for key, item in value.entries], where)
    return value


def _holds_profile_tag(value: object) -> bool:
    if isinstance(value, Tag):
        return value.number in PROFILE_TAGS or _holds_profile_tag(value.value)
    if isinstance(value, list):
        return any(_holds_profile_tag(item) for item in value)
    if isinstance(value, CborMap):
        return any(_holds_profile_tag(key) or _holds_profile_tag(item) for key, item in value.entries)
    return False


def _short(value: object) -> str:
    text = repr(value)
    return text if len(text) <= 80 else text[:77] + "..."


def module_namespace(path: str) -> str:
    """The namespace a source file defines (source/03-source-modules.md ORNA-NS-001..003)."""
    *directories, filename = path.split("/")
    stem = filename[: -len(".orna")]
    return ".".join(directories if stem == "main" else directories + [stem])


class Extraction:
    def __init__(self, output: Path, dependencies: dict[str, Path]) -> None:
        self.blobs = BlobOutput(output)
        self.dependency_paths = dependencies
        self.objects_verified = 0

    def snapshot(self, repository: Path, objects: GitObjects, commit: str, commit_data: bytes, prefix: str, depth: int) -> dict:
        where = f"snapshot {commit}" if not prefix else f"dependency {prefix} at {commit}"
        tree = commit_tree(commit_data, objects.width, where)
        modules: list[dict] = []
        gitlinks: list[tuple[str, str]] = []
        orna_tree = None
        pending = [(tree, "", 0)]
        while pending:
            tree_oid, directory, level = pending.pop()
            if level > MAX_DIRECTORY_DEPTH:
                fail(f"{where}: directories nest deeper than {MAX_DIRECTORY_DEPTH} levels")
            entries = parse_tree(objects.read(tree_oid, "tree", SOURCE_TREE_BYTES_LIMIT, f"{where}: tree {directory or '/'}"), objects.width, f"{where}: tree {directory or '/'}")
            for entry in entries:
                try:
                    name = entry.name.decode("utf-8")
                except UnicodeDecodeError:
                    if entry.mode == b"160000" or entry.name.endswith(b".orna"):
                        fail(f"{where}: path {entry.name!r} under {directory or '/'} is not UTF-8")
                    continue
                path = f"{directory}{name}"
                if directory == "" and name == ".orna":
                    if entry.mode != b"40000":
                        fail(f"{where}: .orna is not a directory")
                    orna_tree = entry.oid
                elif entry.mode == b"40000":
                    pending.append((entry.oid, f"{path}/", level + 1))
                elif entry.mode == b"160000":
                    gitlinks.append((path, entry.oid))
                elif entry.mode in (b"100644", b"100755") and name.endswith(".orna"):
                    source = objects.read(entry.oid, "blob", SOURCE_BYTES_LIMIT, f"{where}: module {path}")
                    try:
                        text = source.decode("utf-8")
                    except UnicodeDecodeError:
                        fail(f"{where}: module {path} is not UTF-8")
                    modules.append(
                        {
                            "namespace": module_namespace(path),
                            "object": entry.oid,
                            "path": path,
                            "sha256": hashlib.sha256(source).hexdigest(),
                            "size": len(source),
                            "text": text,
                        }
                    )
        report: dict = {"commit": commit, "tree": tree, "modules": sorted(modules, key=lambda item: item["path"].encode("utf-8"))}
        graph = Graph(objects, self.blobs)
        if orna_tree is None:
            if not prefix:
                fail(f"{where}: commit has no .orna directory; it is not an Orna database snapshot")
            report.update({"database": None, "relations": [], "store_root": None})
        else:
            report.update(self.database(objects, graph, orna_tree, where))
        report["dependencies"] = [
            self.dependency(repository, objects.width, path, oid, prefix, depth, where) for path, oid in sorted(gitlinks, key=lambda item: item[0].encode("utf-8"))
        ]
        return report

    def database(self, objects: GitObjects, graph: Graph, orna_tree: str, where: str) -> dict:
        entries = {entry.name: entry for entry in parse_tree(objects.read(orna_tree, "tree", SOURCE_TREE_BYTES_LIMIT, f"{where}: .orna"), objects.width, f"{where}: .orna")}
        if b"format.orna" in entries:
            fail(
                f"{where}: .orna/format.orna marks a legacy format-1/2 snapshot; "
                "this extractor reads repository format 3 only"
            )
        names = sorted(name.decode("utf-8", "replace") for name in entries)
        if names != ["database.orna", "store"]:
            fail(f"{where}: tracked .orna/ holds {names}; format 3 allows exactly database.orna and store (ORNA-META-001)")
        record = entries[b"database.orna"]
        store = entries[b"store"]
        if record.mode != b"100644":
            fail(f"{where}: .orna/database.orna is not a regular file")
        if store.mode != b"40000":
            fail(f"{where}: .orna/store is not a native Git tree")
        database_id, canonical = parse_database_record(
            objects.read(record.oid, "blob", DATABASE_RECORD_LIMIT, f"{where}: .orna/database.orna"), f"{where}: .orna/database.orna"
        )
        relations = graph.store(store.oid, bytes.fromhex(database_id.replace("-", "")), f"{where}: store {store.oid}")
        return {
            "database": {"canonical_text": canonical, "database_id": database_id, "repository_format": REPOSITORY_FORMAT},
            "relations": relations,
            "store_root": store.oid,
        }

    def dependency(self, repository: Path, width: int, path: str, commit: str, prefix: str, depth: int, where: str) -> dict:
        full_path = f"{prefix}/{path}" if prefix else path
        if depth >= MAX_DEPENDENCY_DEPTH:
            fail(f"{where}: dependencies nest deeper than {MAX_DEPENDENCY_DEPTH} levels")
        candidates = []
        if full_path in self.dependency_paths:
            candidates.append(self.dependency_paths[full_path])
        candidates.append(repository / path)
        administrative = git_directory(repository)
        if administrative is not None:
            candidates.append(administrative / "modules" / path)
        for candidate in candidates:
            if git_directory(candidate) is None:
                continue
            objects = GitObjects(candidate, f"dependency {full_path}", width)
            try:
                try:
                    commit_data = objects.read(commit, "commit", SOURCE_TREE_BYTES_LIMIT, f"dependency {full_path}")
                except UnavailableObject:
                    continue
                snapshot = self.snapshot(candidate, objects, commit, commit_data, full_path, depth + 1)
                self.objects_verified += len(objects.verified)
                return {"commit": commit, "path": path, "snapshot": snapshot}
            finally:
                objects.close()
        fail(
            f"{where}: dependency {full_path} at {commit} is unavailable; "
            f"pass --dependency {full_path}=REPOSITORY naming a repository that holds it"
        )


def extract(repository: Path, revision: str, output: Path, dependencies: dict[str, Path]) -> dict:
    """Extracts one commit of ``repository``; raises ExtractionError on any violation."""
    if git_directory(repository) is None:
        fail(f"{repository} is not a Git repository root")
    extraction = Extraction(output, dependencies)
    objects = GitObjects(repository, f"repository {repository}")
    try:
        commit, commit_data = objects.resolve_commit(revision, f"commit {revision}")
        snapshot = extraction.snapshot(repository, objects, commit, commit_data, "", 0)
        extraction.objects_verified += len(objects.verified)
        algorithm = objects.algorithm
    finally:
        objects.close()
    return {
        "blobs": sorted(
            ({**record, "descriptors": sorted(record["descriptors"])} for record in extraction.blobs.records.values()),
            key=lambda record: record["sha256"],
        ),
        "extractor": "orna_extract.py",
        "hash_algorithm": algorithm,
        "objects_verified": extraction.objects_verified,
        "profile": {"blobs": "OGB-2", "graph": "OGS-1", "repository_format": REPOSITORY_FORMAT, "rows": "ORP-1", "values": "ROV-3"},
        "snapshot": snapshot,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="orna_extract.py",
        description="Independently extract modules, schema, rows and Blob bytes from one Orna format-3 snapshot.",
        epilog="exit status: 0 extracted and verified; 1 structural violation, unavailable object or unsupported snapshot; 2 invalid arguments",
    )
    parser.add_argument("repository", type=Path, help="repository top-level directory or bare repository")
    parser.add_argument("commit", nargs="?", default="HEAD", help="commit to extract (default HEAD)")
    parser.add_argument("--output", type=Path, required=True, help="directory that receives blobs/<sha256>")
    parser.add_argument(
        "--dependency",
        action="append",
        default=[],
        metavar="PATH=REPOSITORY",
        help="repository holding the gitlink dependency at PATH (repeatable)",
    )
    arguments = parser.parse_args(argv)
    dependencies: dict[str, Path] = {}
    for mapping in arguments.dependency:
        path, separator, location = mapping.partition("=")
        if not separator or not path or not location:
            parser.error(f"--dependency {mapping!r} is not PATH=REPOSITORY")
        dependencies[path.strip("/")] = Path(location)
    try:
        report = extract(arguments.repository, arguments.commit, arguments.output, dependencies)
    except ExtractionError as error:
        print(f"orna_extract: {error}", file=sys.stderr)
        return 1
    sys.stdout.write(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
