"""Stdlib readers for the legacy 1.1.0 blob encodings kept by format-1/2 commits.

Covers OVB-1 value decoding (ROV-1), the ROV-2 storage reference (CBOR tag
60100), the ORR-2 logical reference record with its inline padded Base64
Blob form, and OGB-1 Git blob roots (meta, index nodes, chunks).

The byte layout follows the draft chapter source/37 that accompanies the
1.1.0 compatibility archive and is cross-checked against the golden vectors in
scripts/golden/ogb1-blob-vectors.json. It is independent of the Rust engine.

Row units: `parse_row_unit` tokenizes one loose row unit from its committed bytes;
`row_blob_field` reads an ORR-2 Blob field of that row. The grammar basis is the
1.0.0 frozen syntactic grammar `reference/Orna-1.1.0/grammar/orna.ebnf`
(`row_unit = record_expression , [ ";" ]`, with `record_fields`, `string_literal`
and `escape_sequence`), together with ORNA-SOURCE-003 (one record expression, no
declarations). No 1.1.0 clause defines the legacy loose row-unit grammar, so this
reader is written from the EBNF and from the engine's own row parser, not from an
independent conformance vector. Deliberate deviation: the reader admits only
literal values, while the EBNF `record_expression` admits any expression; an
expression is refused and never evaluated. Unverified: whether the engine accepts
a raw newline inside a string literal, which the EBNF's `string_character` permits,
and whether it accepts the `\0` escape. The reader accepts both as the EBNF reads.
"""

from __future__ import annotations

import base64
import datetime
import hashlib
import re
import unicodedata
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Protocol

from scripts.orna_extract import (
    PROFILE_TAGS,
    REGISTERED_TAGS,
    UUID_TAG,
    CborMap,
    ExtractionError,
    Tag,
    UnavailableObject,
    decode_canonical,
    encode,
)

MAX_LENGTH = (1 << 63) - 1
MAX_CHUNK = 1 << 20
MAX_DESCRIPTOR = 1 << 16
MAX_HEIGHT = 8
FANOUT = 256
MAX_READ = 8 << 20
ROV2_STORAGE_TAG = 60100
# ROV-1 is the OVB-1 value codec: the registered value tags minus the profile
# tags (60110..60113). Tag 60100 is not registered, so it is rejected here.
OVB1_TAGS = frozenset(REGISTERED_TAGS - PROFILE_TAGS)
ROV2_TAGS = frozenset({ROV2_STORAGE_TAG})
NO_TAGS: frozenset = frozenset()
_ORR2_BLOB = re.compile(r"sha256:([0-9a-f]{64})")


class LegacyBlobError(ExtractionError):
    """Malformed legacy blob content. Never yields a partial result."""


class ObjectSource(Protocol):
    def get(self, oid: str) -> tuple[str, bytes]:
        """Return (kind, data) for a Git object id; raise UnavailableObject when absent."""


@dataclass(frozen=True)
class StoredBlob:
    digest: str
    length: int


@dataclass(frozen=True)
class Descriptor:
    digest: str
    length: int
    data_oid: str


@dataclass(frozen=True)
class Entry:
    end: int
    oid: str
    digest: bytes | None


def _uint(value: object) -> bool:
    return type(value) is int and value >= 0


def _reject(message: str) -> None:
    raise LegacyBlobError(message)


def git_oid(kind: str, data: bytes, algorithm: str = "sha1") -> str:
    header = f"{kind} {len(data)}".encode("ascii") + b"\0"
    return hashlib.new(algorithm, header + data).hexdigest()


def _git_tree_bytes(entries: dict[str, tuple[str, str]]) -> bytes:
    ordered = sorted(
        entries.items(),
        key=lambda item: item[0].encode("ascii") + (b"/" if item[1][0] == "tree" else b"\0"),
    )
    out = []
    for name, (kind, oid) in ordered:
        mode = b"40000" if kind == "tree" else b"100644"
        out.append(mode + b" " + name.encode("ascii") + b"\0" + bytes.fromhex(oid))
    return b"".join(out)


def parse_git_tree(data: bytes, width: int) -> dict[str, tuple[str, str]]:
    """Parse a Git tree admitted in a blob root: modes 100644/40000, no duplicates, canonical order."""
    entries: dict[str, tuple[str, str]] = {}
    pos = 0
    while pos < len(data):
        sep = data.find(b" ", pos)
        end = data.find(b"\0", sep + 1) if sep >= 0 else -1
        if sep < 0 or end < 0:
            _reject("Git tree entry is malformed")
        mode = data[pos:sep]
        try:
            name = data[sep + 1 : end].decode("ascii")
        except UnicodeDecodeError:
            _reject("Git tree name is not ASCII")
        if mode not in (b"40000", b"100644") or not name or "/" in name or name in entries:
            _reject("Git tree entry mode, name or duplicate is not admitted")
        pos = end + 1
        if pos + width > len(data):
            _reject("Git tree OID is truncated")
        entries[name] = ("tree" if mode == b"40000" else "blob", data[pos : pos + width].hex())
        pos += width
    if _git_tree_bytes(entries) != data:
        _reject("Git tree entries are not in canonical order")
    return entries


def _descriptor(data: bytes, where: str) -> object:
    if len(data) > MAX_DESCRIPTOR:
        _reject(f"{where}: descriptor exceeds 64 KiB")
    return decode_canonical(data, where, NO_TAGS)


def decode_meta(data: bytes, width: int) -> Descriptor:
    """OGB-1 meta: [1, sha256 bytes32, length uint, data tree OID bytes]."""
    value = _descriptor(data, "meta")
    if not (isinstance(value, list) and len(value) == 4 and value[0] == 1 and type(value[0]) is int):
        _reject("meta is not [1, digest, length, data_oid]")
    _, digest, length, data_oid = value
    if not (isinstance(digest, bytes) and len(digest) == 32):
        _reject("meta digest is not 32 bytes")
    if not (_uint(length) and length <= MAX_LENGTH):
        _reject("meta length is outside 0..2^63-1")
    if not (isinstance(data_oid, bytes) and len(data_oid) == width):
        _reject("meta data OID width does not match the repository")
    return Descriptor(digest.hex(), length, data_oid.hex())


def _decode_index(data: bytes) -> tuple[int, int, list]:
    value = _descriptor(data, "index")
    if not (isinstance(value, list) and len(value) == 4 and value[0] == 1 and type(value[0]) is int):
        _reject("index is not [1, height, total, entries]")
    _, height, total, entries = value
    if not (_uint(height) and height <= MAX_HEIGHT):
        _reject("index height is outside 0..8")
    if not (_uint(total) and total <= MAX_LENGTH):
        _reject("index total is outside 0..2^63-1")
    if not isinstance(entries, list) or len(entries) > FANOUT:
        _reject("index entries are not an array of at most 256 entries")
    if not entries and (height != 0 or total != 0):
        _reject("only the empty blob may have an empty index")
    return height, total, entries


class OgbReader:
    """Reads and verifies OGB-1 blob roots through an ObjectSource."""

    def __init__(self, objects: ObjectSource, algorithm: str = "sha1") -> None:
        self._objects = objects
        self._algorithm = algorithm
        self._width = hashlib.new(algorithm).digest_size

    def _load(self, oid: str, kind: str) -> bytes:
        actual, data = self._objects.get(oid)
        if actual != kind:
            _reject(f"object {oid} is a {actual}, not a {kind}")
        if git_oid(kind, data, self._algorithm) != oid:
            _reject(f"object {oid} does not match its Git id")
        return data

    def _tree(self, oid: str) -> dict[str, tuple[str, str]]:
        return parse_git_tree(self._load(oid, "tree"), self._width)

    def descriptor(self, root: str) -> Descriptor:
        """Read the root's meta only. Index nodes and chunks are not read."""
        tree = self._tree(root)
        if set(tree) != {"meta", "data"} or tree["meta"][0] != "blob" or tree["data"][0] != "tree":
            _reject("blob root must contain exactly blob meta and tree data")
        descriptor = decode_meta(self._load(tree["meta"][1], "blob"), self._width)
        if descriptor.data_oid != tree["data"][1]:
            _reject("meta data OID does not match the data tree")
        return descriptor

    def _node(
        self, oid: str, expected_length: int | None, expected_height: int | None
    ) -> tuple[int, list[Entry]]:
        tree = self._tree(oid)
        if "index" not in tree or tree["index"][0] != "blob":
            _reject("data node has no blob index")
        height, total, raw_entries = _decode_index(self._load(tree["index"][1], "blob"))
        if expected_length is not None and total != expected_length:
            _reject("child length does not match its parent interval")
        if expected_height is not None and height != expected_height:
            _reject("child height does not match its parent")
        names = {"index", *(f"{ordinal:03x}" for ordinal in range(len(raw_entries)))}
        if set(tree) != names:
            _reject("data node children are not the consecutive names 000..n")
        kind = "blob" if height == 0 else "tree"
        entries: list[Entry] = []
        begin = 0
        for ordinal, item in enumerate(raw_entries):
            if not (isinstance(item, list) and len(item) == 3 and _uint(item[0])):
                _reject("index entry is not [end, oid, digest]")
            end, target, digest = item
            if not begin < end <= MAX_LENGTH:
                _reject("index entry offsets are not strictly increasing")
            if not (isinstance(target, bytes) and len(target) == self._width):
                _reject("index entry OID width does not match the repository")
            if tree[f"{ordinal:03x}"] != (kind, target.hex()):
                _reject("index entry does not match the Git child at its ordinal")
            if height == 0:
                if not (isinstance(digest, bytes) and len(digest) == 32):
                    _reject("leaf chunk digest is not 32 bytes")
                if end - begin > MAX_CHUNK:
                    _reject("chunk exceeds 1 MiB")
            elif digest is not None:
                _reject("internal entry digest must be null")
            entries.append(Entry(end, target.hex(), digest))
            begin = end
        if begin != total:
            _reject("index entries do not reach the node total")
        return height, entries

    def _walk(
        self,
        oid: str,
        start: int,
        stop: int,
        expected_length: int,
        expected_height: int | None,
    ) -> Iterator[bytes]:
        height, entries = self._node(oid, expected_length, expected_height)
        begin = 0
        for entry in entries:
            size = entry.end - begin
            lo, hi = begin, entry.end
            begin = entry.end
            if hi <= start or lo >= stop:
                continue
            if height == 0:
                data = self._load(entry.oid, "blob")
                if len(data) != size or hashlib.sha256(data).digest() != entry.digest:
                    _reject("chunk does not match its descriptor length or SHA-256")
                yield data[max(start - lo, 0) : min(stop - lo, size)]
            else:
                yield from self._walk(
                    entry.oid,
                    max(0, start - lo),
                    min(stop - lo, size),
                    size,
                    height - 1,
                )

    def read_range(self, root: str, offset: int, length: int) -> bytes:
        """Return bytes [offset, offset+length) clipped to the blob, reading overlapping chunks only."""
        descriptor = self.descriptor(root)
        total = descriptor.length
        if not (_uint(offset) and _uint(length)) or offset > total or length > MAX_READ:
            _reject("range bound is outside the blob or the 8 MiB read limit")
        stop = min(total, offset + length)
        if stop == offset:
            return b""
        data = b"".join(self._walk(descriptor.data_oid, offset, stop, total, None))
        if len(data) != stop - offset:
            _reject("range read is incomplete")
        return data

    def verify(self, root: str) -> str:
        """Full verification: every node, every chunk, and the whole-content SHA-256 and length."""
        descriptor = self.descriptor(root)
        digest = hashlib.sha256()
        size = 0
        for part in self._walk(descriptor.data_oid, 0, descriptor.length, descriptor.length, None):
            digest.update(part)
            size += len(part)
        if size != descriptor.length or digest.hexdigest() != descriptor.digest:
            _reject("whole-content digest or length mismatch")
        return descriptor.digest


def decode_ovb1(data: bytes, where: str = "OVB-1 value") -> object:
    """Strict OVB-1 value (ROV-1). Profile tags 60110..60113 and storage tag 60100 are rejected.

    Every admitted tag is also checked against its registry payload rule (1.1.0 formats table),
    at every nesting level. Descriptor-dependent rules are not checked here.
    """
    value = decode_canonical(data, where, OVB1_TAGS)
    _check_registry_tree(value, where)
    return value


_DATE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}")
_LOCAL_DATETIME = re.compile(r"([0-9]{4}-[0-9]{2}-[0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})\.[0-9]{9}")
_NANOS_LIMIT = 1_000_000_000
_TIME_OF_DAY_MAX = 86_399_999_999_999
_PATH_FLAVOURS = frozenset({"repo", "posix", "windows"})
_UUID_NAMES = frozenset(
    {
        "sys.DatabaseId",
        "sys.FileId",
        "sys.DefinitionId",
        "sys.ObjectId",
        "sys.RuntimeId",
        "sys.TransactionId",
        "sys.InvocationId",
        "sys.RunId",
        "sys.QueryId",
        "sys.SessionId",
        "sys.ClientId",
        "sys.TraceId",
        "sys.SpanId",
        "sys.SegmentId",
        "sys.BuildId",
    }
)
_DIGEST_NAMES = frozenset({"sys.RevisionId", "sys.SnapshotId", "sys.ConsumerIdentity"})
_VERSION_NAMES = frozenset({"sys.CheckpointVersion", "sys.FailureVersion"})


def _is_int(value: object) -> bool:
    return type(value) is int


def _is_bytes(value: object, length: int) -> bool:
    return type(value) is bytes and len(value) == length


def _is_list(value: object, length: int | None = None) -> bool:
    return type(value) is list and (length is None or len(value) == length)


def _is_tagged(value: object, number: int) -> bool:
    return isinstance(value, Tag) and value.number == number


def _is_uuid(value: object) -> bool:
    return _is_tagged(value, UUID_TAG) and _is_bytes(value.value, 16)


def _calendar_date(text: str) -> bool:
    if _DATE.fullmatch(text) is None:
        return False
    try:
        datetime.date.fromisoformat(text)
    except ValueError:
        return False
    return True


def _local_datetime(text: str) -> bool:
    match = _LOCAL_DATETIME.fullmatch(text)
    if match is None or not _calendar_date(match.group(1)):
        return False
    try:
        # datetime.time rejects second 60, so leap-second notation is refused.
        datetime.time(int(match.group(2)), int(match.group(3)), int(match.group(4)))
    except ValueError:
        return False
    return True


def _seconds_nanos(value: object) -> bool:
    return _is_list(value, 2) and _is_int(value[0]) and _is_int(value[1]) and 0 <= value[1] < _NANOS_LIMIT


def _decimal(value: object) -> bool:
    if not (_is_list(value, 2) and _is_int(value[0]) and _is_int(value[1])):
        return False
    coefficient, exponent = value
    if coefficient == 0:
        return exponent == 0
    return coefficient % 10 != 0


def _option(value: object) -> bool:
    if not _is_list(value):
        return False
    if len(value) == 1:
        return _is_int(value[0]) and value[0] == 0
    return len(value) == 2 and _is_int(value[0]) and value[0] == 1


def _cwd_digest(database: Tag, runtime: Tag, generation: int) -> bytes:
    """The CWD snapshot_id: SHA-256 of 'orna.snapshot.v1', a zero byte and the structural encoding of the descriptor."""
    return hashlib.sha256(b"orna.snapshot.v1\x00" + encode([0, database, runtime, generation])).digest()


def _snapshot(value: object) -> bool:
    if not _is_list(value):
        return False
    if len(value) == 5 and _is_int(value[0]) and value[0] == 0:
        return (
            _is_uuid(value[1])
            and _is_uuid(value[2])
            and _uint(value[3])
            and _is_bytes(value[4], 32)
            and value[4] == _cwd_digest(value[1], value[2], value[3])
        )
    if len(value) == 4 and _is_int(value[0]) and value[0] == 1:
        algorithm = value[2]
        return _is_uuid(value[1]) and (
            (algorithm == "sha1" and _is_bytes(value[3], 20)) or (algorithm == "sha256" and _is_bytes(value[3], 32))
        )
    return False


def _pinned_row(value: object) -> bool:
    return _is_list(value, 4) and _is_uuid(value[0]) and _is_uuid(value[1]) and _snapshot(value[3])


def _record(value: object) -> bool:
    if not (_is_list(value, 2) and (value[0] is None or _is_uuid(value[0])) and _is_list(value[1])):
        return False
    if not all(_is_list(field, 2) and (type(field[0]) is str or _is_uuid(field[0])) for field in value[1]):
        return False
    names = [field[0] for field in value[1]]
    # Structural field names are NFC text, and equal field keys are invalid (formats spec, structural records).
    if not all(unicodedata.is_normalized("NFC", name) for name in names if type(name) is str):
        return False
    keys = [name if type(name) is str else name.value for name in names]
    return len(set(keys)) == len(keys)


def _enum(value: object) -> bool:
    return _is_list(value, 3) and _is_uuid(value[0]) and _is_uuid(value[1]) and (value[2] is None or _is_tagged(value[2], 60009))


def _error(value: object) -> bool:
    if not isinstance(value, CborMap):
        return False
    keys = [key for key, _ in value.entries]
    if not all(_is_int(key) for key in keys) or sorted(keys) != [0, 1, 2, 3]:
        return False
    fields = dict(value.entries)
    causes = fields[2]
    details = fields[3]
    return (
        _is_list(causes)
        and all(_is_tagged(cause, 60016) for cause in causes)
        and isinstance(details, CborMap)
        and all(type(key) is str for key, _ in details.entries)
    )


def _zoned_datetime(value: object) -> bool:
    return _is_list(value, 3) and _is_tagged(value[0], 60002) and _is_tagged(value[1], 60004) and _is_int(value[2])


def _range(value: object) -> bool:
    return (
        _is_list(value, 3)
        and _is_tagged(value[0], 60013)
        and _is_tagged(value[1], 60013)
        and type(value[2]) is bool
    )


def _opaque_representation(name: str, representation: object) -> bool:
    if name in _UUID_NAMES:
        return _is_uuid(representation)
    if name in _DIGEST_NAMES:
        return _is_bytes(representation, 32)
    if name in _VERSION_NAMES:
        return _uint(representation)
    if name == "sys.GitOid" and _is_list(representation, 2):
        algorithm, digest = representation
        return (algorithm == "sha1" and _is_bytes(digest, 20)) or (algorithm == "sha256" and _is_bytes(digest, 32))
    return False


def _opaque(value: object) -> bool:
    return _is_list(value, 2) and type(value[0]) is str and _opaque_representation(value[0], value[1])


def _repository_path(text: str) -> bool:
    if not text or text.startswith("/") or "\\" in text or "\0" in text:
        return False
    return all(segment not in ("", ".", "..") for segment in text.split("/"))


def _path(value: object) -> bool:
    if not (_is_list(value, 2) and type(value[0]) is str and value[0] in _PATH_FLAVOURS and type(value[1]) is str):
        return False
    return value[0] != "repo" or _repository_path(value[1])


# One rule per registered OVB-1 tag: the payload shape and the constraints the 1.1.0 table states.
_REGISTRY_RULES = {
    UUID_TAG: lambda p: _is_bytes(p, 16),
    60000: _decimal,
    60001: lambda p: type(p) is str and _calendar_date(p),
    60002: _seconds_nanos,
    60003: lambda p: type(p) is str and _local_datetime(p),
    60004: lambda p: type(p) is str and p != "",
    60005: _seconds_nanos,
    60006: lambda p: _is_list(p, 2) and _is_uuid(p[1]),
    60007: lambda p: _is_list(p, 2) and _is_tagged(p[0], 60000) and _is_uuid(p[1]),
    60008: _enum,
    60009: _record,
    60010: _pinned_row,
    60011: lambda p: isinstance(p, CborMap),
    60012: lambda p: _is_list(p, 4) and _is_list(p[3]),
    60013: _option,
    60014: lambda p: _is_list(p, 0),
    60015: lambda p: _is_list(p) and len(p) > 0,
    60016: _error,
    60017: lambda p: _is_int(p) and 0 <= p <= _TIME_OF_DAY_MAX,
    60018: _zoned_datetime,
    60019: _range,
    60020: lambda p: _is_list(p, 2) and _is_uuid(p[0]),
    60021: lambda p: _is_list(p, 3) and _is_uuid(p[0]) and _is_uuid(p[1]),
    60022: lambda p: _is_list(p, 4) and _is_uuid(p[0]) and _is_uuid(p[1]) and p[3] is False,
    60023: _opaque,
    60024: _path,
    60025: lambda p: _is_list(p, 2) and p[0] == "sha256" and _is_bytes(p[1], 32),
}


def _check_registry_tree(value: object, where: str) -> None:
    """Apply each tag's registry rule to every tag in the decoded tree; unknown tags fail closed."""
    if isinstance(value, Tag):
        rule = _REGISTRY_RULES.get(value.number)
        if rule is None or not rule(value.value):
            _reject(f"{where}: tag {value.number} payload violates the registry")
        _check_registry_tree(value.value, where)
    elif isinstance(value, list):
        for item in value:
            _check_registry_tree(item, where)
    elif isinstance(value, CborMap):
        for key, item in value.entries:
            _check_registry_tree(key, where)
            _check_registry_tree(item, where)


def decode_rov2(data: bytes, where: str = "ROV-2 Blob") -> StoredBlob:
    """A ROV-2 Blob slot holds tag 60100 around [digest32, length]."""
    value = decode_canonical(data, where, ROV2_TAGS)
    if not (isinstance(value, Tag) and value.number == ROV2_STORAGE_TAG):
        _reject(f"{where}: slot is not tag 60100")
    payload = value.value
    if not (
        isinstance(payload, list)
        and len(payload) == 2
        and isinstance(payload[0], bytes)
        and len(payload[0]) == 32
        and _uint(payload[1])
        and payload[1] <= MAX_LENGTH
    ):
        _reject(f"{where}: tag 60100 is not [digest32, length]")
    return StoredBlob(payload[0].hex(), payload[1])


def orr2_reference(pairs: list[tuple[str, object]]) -> StoredBlob:
    """Validate the logical ORR-2 record {blob, length} from already-tokenized field pairs."""
    if sorted(key for key, _ in pairs) != ["blob", "length"]:
        _reject("ORR-2 record fields must be exactly blob and length, without duplicates")
    fields = dict(pairs)
    blob = fields["blob"]
    length = fields["length"]
    if not (isinstance(blob, str) and _ORR2_BLOB.fullmatch(blob)):
        _reject("ORR-2 blob is not sha256:<64 lowercase hex digits>")
    if not (_uint(length) and length <= MAX_LENGTH):
        _reject("ORR-2 length is not a nonnegative integer within 2^63-1")
    return StoredBlob(blob[len("sha256:") :], length)


def orr2_inline_bytes(text: str) -> bytes:
    """Legacy inline Blob string: canonical padded Base64 only."""
    try:
        raw = base64.b64decode(text.encode("ascii"), validate=True)
    except ValueError:
        _reject("ORR-2 inline Blob is not padded Base64")
    if base64.b64encode(raw).decode("ascii") != text:
        _reject("ORR-2 inline Blob is not canonical padded Base64")
    return raw


_ROW_IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_ROW_INT = re.compile(r"-?(?:0|[1-9][0-9]*)")
_ROW_KEYWORDS = {"true": True, "false": False, "null": None}
_ROW_ESCAPES = {'"': '"', "\\": "\\", "n": "\n", "r": "\r", "t": "\t", "0": "\0"}
_ROW_WHITESPACE = " \t\r\n"
_ROW_HEX = frozenset("0123456789abcdefABCDEF")


class _RowUnitParser:
    """Tokenizer for one row unit: one record of literal values (see parse_row_unit)."""

    def __init__(self, text: str, where: str) -> None:
        self.text = text
        self.pos = 0
        self.where = where

    def fail(self, message: str) -> None:
        _reject(f"{self.where}: {message} at offset {self.pos}")

    def skip(self) -> None:
        text = self.text
        while self.pos < len(text):
            if text[self.pos] in _ROW_WHITESPACE:
                self.pos += 1
            elif text.startswith("//", self.pos):
                end = text.find("\n", self.pos)
                self.pos = len(text) if end < 0 else end + 1
            elif text.startswith("/*", self.pos):
                self._block_comment()
            else:
                return

    def _block_comment(self) -> None:
        # Block comments nest, as in the engine lexer.
        depth = 0
        while True:
            if self.text.startswith("/*", self.pos):
                depth += 1
                self.pos += 2
            elif self.text.startswith("*/", self.pos):
                depth -= 1
                self.pos += 2
                if depth == 0:
                    return
            elif self.pos < len(self.text):
                self.pos += 1
            else:
                self.fail("unterminated block comment")

    def peek(self) -> str:
        self.skip()
        return self.text[self.pos] if self.pos < len(self.text) else ""

    def expect(self, char: str) -> None:
        if self.peek() != char:
            self.fail(f"expected {char!r}")
        self.pos += 1

    def value(self) -> object:
        char = self.peek()
        if char == "{":
            return self.record()
        if char == "[":
            return self.array()
        if char == '"':
            return self.string()
        if char and (char == "-" or "0" <= char <= "9"):
            match = _ROW_INT.match(self.text, self.pos)
            if match is None:
                self.fail("malformed integer")
            self.pos = match.end()
            return int(match.group(0))
        match = _ROW_IDENT.match(self.text, self.pos) if char else None
        if match is not None and match.group(0) in _ROW_KEYWORDS:
            self.pos = match.end()
            return _ROW_KEYWORDS[match.group(0)]
        self.fail("expression is not a literal value")
        raise AssertionError("unreachable")

    def record(self) -> dict[str, object]:
        self.expect("{")
        out: dict[str, object] = {}
        if self.peek() == "}":
            self.pos += 1
            return out
        while True:
            self.skip()
            match = _ROW_IDENT.match(self.text, self.pos)
            if match is None or match.group(0) in _ROW_KEYWORDS:
                self.fail("expected a field name")
            name = match.group(0)
            self.pos = match.end()
            if name in out:
                self.fail(f"duplicate field {name}")
            self.expect(":")
            out[name] = self.value()
            separator = self.peek()
            if separator == "}":
                self.pos += 1
                return out
            if separator != ",":
                self.fail("expected ',' or '}'")
            self.pos += 1
            if self.peek() == "}":
                self.pos += 1
                return out

    def array(self) -> list[object]:
        self.expect("[")
        out: list[object] = []
        if self.peek() == "]":
            self.pos += 1
            return out
        while True:
            out.append(self.value())
            separator = self.peek()
            if separator == "]":
                self.pos += 1
                return out
            if separator != ",":
                self.fail("expected ',' or ']'")
            self.pos += 1
            if self.peek() == "]":
                self.pos += 1
                return out

    def string(self) -> str:
        self.expect('"')
        chars: list[str] = []
        text = self.text
        while self.pos < len(text):
            char = text[self.pos]
            self.pos += 1
            if char == '"':
                return "".join(chars)
            if char == "{":
                self.fail("string interpolation is not admitted in a row value")
            if char != "\\":
                chars.append(char)
                continue
            if self.pos >= len(text):
                self.fail("unterminated string escape")
            escape = text[self.pos]
            self.pos += 1
            if escape in _ROW_ESCAPES:
                chars.append(_ROW_ESCAPES[escape])
            elif escape == "u":
                chars.append(self._unicode_escape())
            else:
                self.fail("invalid string escape")
        self.fail("unterminated string")
        raise AssertionError("unreachable")

    def _unicode_escape(self) -> str:
        if self.pos >= len(self.text) or self.text[self.pos] != "{":
            self.fail("unicode escape requires '{'")
        end = self.text.find("}", self.pos)
        digits = self.text[self.pos + 1 : end] if end >= 0 else ""
        if not (1 <= len(digits) <= 6 and all(c in _ROW_HEX for c in digits)):
            self.fail("invalid unicode scalar escape")
        scalar = int(digits, 16)
        if scalar > 0x10FFFF or 0xD800 <= scalar <= 0xDFFF:
            self.fail("invalid unicode scalar escape")
        self.pos = end + 1
        return chr(scalar)


def parse_row_unit(data: bytes, where: str = "row unit") -> dict[str, object]:
    """Tokenizes one loose row unit from its committed bytes.

    The grammar is `row_unit = record_expression , [ ";" ]` from
    grammar/orna.ebnf, narrowed to literal values as a deliberate deviation: the
    record holds only literals, with an optional trailing comma and an optional
    trailing `;`, and only whitespace and comments around it. Strict UTF-8 is
    required; any non-literal expression is refused, never evaluated.
    """
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        _reject(f"{where}: row unit is not UTF-8")
    parser = _RowUnitParser(text, where)
    if parser.peek() != "{":
        parser.fail("row unit must contain one record")
    record = parser.record()
    if parser.peek() == ";":
        parser.pos += 1
    if parser.peek() != "":
        parser.fail("trailing content after the row record")
    return record


def row_blob_field(row: dict[str, object], field: str, where: str = "row unit") -> bytes | StoredBlob:
    """One ORR-2 Blob field of a parsed row: inline padded Base64 bytes or a storage record."""
    if field not in row:
        _reject(f"{where}: Blob field {field} is absent")
    value = row[field]
    if isinstance(value, str):
        return orr2_inline_bytes(value)
    if isinstance(value, dict):
        return orr2_reference(list(value.items()))
    _reject(f"{where}: Blob field {field} is neither inline Base64 nor a storage record")
    raise AssertionError("unreachable")
