"""Stdlib readers for the legacy 1.1.0 blob encodings kept by format-1/2 commits.

Covers OVB-1 value decoding (ROV-1), the ROV-2 storage reference (CBOR tag
60100), the ORR-2 logical reference record with its inline padded Base64
Blob form, and OGB-1 Git blob roots (meta, index nodes, chunks).

The byte layout follows the draft chapter source/37 that accompanies the
1.1.0 compatibility archive and is cross-checked against the golden vectors in
scripts/golden/ogb1-blob-vectors.json. It is independent of the Rust engine.

Not implemented: the ORR-2 row-unit tokenizer that finds the logical record
inside a repository row. Callers pass the already-tokenized field pairs.
"""

from __future__ import annotations

import base64
import hashlib
import re
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Protocol

from scripts.orna_extract import (
    PROFILE_TAGS,
    REGISTERED_TAGS,
    ExtractionError,
    Tag,
    UnavailableObject,
    decode_canonical,
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
    """Strict OVB-1 value (ROV-1). Profile tags 60110..60113 and storage tag 60100 are rejected."""
    return decode_canonical(data, where, OVB1_TAGS)


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
