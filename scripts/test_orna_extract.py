"""Unit tests for scripts/orna_extract.py (python3 -m unittest scripts.test_orna_extract).

The tests cover the decoder and encoder of canonical CBOR, the MIME-1 media
registry against the vendored profile, the Gear chunk partition, Git object
reads through ``git cat-file --batch`` (including re-hashing of a substituted
object), tree parsing, and the command-line error paths. The end-to-end
extraction of a real restored snapshot is covered by the Rust integration test
crates/orna-cli-v1/tests/independent_extractor_cli.rs.
"""

from __future__ import annotations

import ast
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

from scripts import orna_extract as extractor
from scripts.orna_extract import (
    CANONICAL_NAN,
    GEAR,
    GEAR_MASK,
    GEAR_MAXIMUM,
    GEAR_MINIMUM,
    CborMap,
    ExtractionError,
    Float,
    GearBoundaries,
    BlobOutput,
    GitObjects,
    Tag,
    UnavailableObject,
    _InvalidMedia,
    Graph,
    canonical_annotation,
    commit_tree,
    decode_canonical,
    encode,
    extract,
    gear_cuts,
    git_directory,
    make_map,
    normalize_media,
    parse_tree,
    main,
)

REPO_ROOT = Path(__file__).resolve().parents[1]
MEDIA_PROFILE = REPO_ROOT / "crates/orna-conformance-v1/tests/fixtures/reference/profiles/media-1.json"
U64 = (1 << 64) - 1

IDENTITY = {
    "GIT_AUTHOR_NAME": "Extractor Test",
    "GIT_AUTHOR_EMAIL": "extractor-test@example.invalid",
    "GIT_COMMITTER_NAME": "Extractor Test",
    "GIT_COMMITTER_EMAIL": "extractor-test@example.invalid",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
}


def git(cwd: Path, *args: str, stdin: bytes | None = None) -> bytes:
    completed = subprocess.run(
        ["git", "-c", "commit.gpgsign=false", *args],
        cwd=cwd,
        input=stdin,
        env={**os.environ, **IDENTITY},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(f"git {' '.join(args)} failed: {completed.stderr.decode()}")
    return completed.stdout


def make_repository(root: Path, files: dict[str, bytes]) -> Path:
    """A repository at ``root`` whose HEAD commit holds exactly ``files``."""
    root.mkdir(parents=True, exist_ok=True)
    git(root, "init", "-q")
    for name, data in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    git(root, "add", "--all")
    git(root, "commit", "-q", "-m", "fixture")
    return root


def reference_first_cut(chunk: bytes) -> int | None:
    """The first OGB-2 cut in ``chunk`` by a direct rolling-hash scan from byte zero.

    The shift discards contributions older than 64 bytes, so a scan from the
    start of the chunk computes the same accumulator as the windowed scan.
    """
    accumulator = 0
    for index, byte in enumerate(chunk):
        accumulator = ((accumulator << 1) + GEAR[byte]) & U64
        position = index + 1
        if position >= GEAR_MINIMUM and (accumulator & GEAR_MASK == 0 or position == GEAR_MAXIMUM):
            return position
    return None


def deterministic_bytes(size: int, seed: bytes = b"orna-extract-test") -> bytes:
    return hashlib.shake_256(seed).digest(size)


class CanonicalCborTests(unittest.TestCase):
    def test_round_trip_of_every_item_kind(self) -> None:
        values = [
            None,
            True,
            False,
            0,
            23,
            24,
            255,
            256,
            65_535,
            65_536,
            2**32,
            2**64 - 1,
            -1,
            -24,
            -25,
            -(2**64),
            b"",
            b"\x00\x01",
            "",
            "héllo",
            [],
            [1, [2, "three"]],
            make_map([(1, "a"), ("b", None)], "round trip"),
            Tag(37, bytes(range(16))),
            Tag(60111, [1, b"x"]),
            Float(CANONICAL_NAN),
            Float(struct.pack(">d", 1.5)),
        ]
        for value in values:
            with self.subTest(value=value):
                self.assertEqual(decode_canonical(encode(value), "round trip"), value)

    def test_shortest_heads_are_used(self) -> None:
        self.assertEqual(encode(23), b"\x17")
        self.assertEqual(encode(24), b"\x18\x18")
        self.assertEqual(encode(256), b"\x19\x01\x00")
        self.assertEqual(encode(-1), b"\x20")

    def test_bignums_round_trip_beyond_a_64_bit_head(self) -> None:
        self.assertEqual(encode(2**64), b"\xc2" + encode((2**64).to_bytes(9, "big")))
        self.assertEqual(decode_canonical(encode(2**64), "big"), 2**64)
        self.assertEqual(decode_canonical(encode(-(2**70)), "big"), -(2**70))

    def test_non_shortest_heads_are_refused(self) -> None:
        for data in (b"\x18\x05", b"\x19\x00\x17", b"\x1a\x00\x00\xff\xff", b"\x1b\x00\x00\x00\x00\x00\x00\x00\x05"):
            with self.subTest(data=data), self.assertRaises(ExtractionError):
                decode_canonical(data, "head")

    def test_indefinite_and_reserved_heads_are_refused(self) -> None:
        for data in (b"\x9f\xff", b"\xbf\xff", b"\x5f\xff", b"\x1c"):
            with self.subTest(data=data), self.assertRaises(ExtractionError):
                decode_canonical(data, "indefinite")

    def test_simple_values_and_short_floats_are_refused(self) -> None:
        for data in (b"\xf8\x20", b"\xf9\x3c\x00", b"\xfa\x3f\x80\x00\x00"):
            with self.subTest(data=data), self.assertRaises(ExtractionError):
                decode_canonical(data, "simple")

    def test_non_canonical_nan_is_refused(self) -> None:
        with self.assertRaises(ExtractionError):
            decode_canonical(b"\xfb\x7f\xf8\x00\x00\x00\x00\x00\x01", "nan")

    def test_unregistered_tags_are_refused(self) -> None:
        with self.assertRaises(ExtractionError):
            decode_canonical(b"\xd9\x03\xe8\x00", "tag")

    def test_map_keys_must_be_in_encoded_order_without_repeats(self) -> None:
        unordered = b"\xa2" + encode("b") + encode(1) + encode("a") + encode(2)
        repeated = b"\xa2\x01\x01\x01\x02"
        for data in (unordered, repeated):
            with self.subTest(data=data), self.assertRaises(ExtractionError):
                decode_canonical(data, "map")

    def test_make_map_sorts_by_encoded_key_and_refuses_duplicates(self) -> None:
        mapping = make_map([("b", 1), ("a", 2)], "sort")
        self.assertEqual([key for key, _ in mapping.entries], ["a", "b"])
        with self.assertRaises(ExtractionError):
            make_map([("a", 1), ("a", 2)], "duplicate")

    def test_truncated_and_trailing_input_are_refused(self) -> None:
        with self.assertRaises(ExtractionError):
            decode_canonical(b"\x18", "truncated head")
        with self.assertRaises(ExtractionError):
            decode_canonical(b"\x85\x00", "truncated array")
        with self.assertRaises(ExtractionError):
            decode_canonical(b"\x00\x00", "trailing")

    def test_nesting_beyond_the_depth_bound_is_refused(self) -> None:
        self.assertEqual(decode_canonical(b"\x81" * 255 + b"\x00", "deep"), _nested(255))
        with self.assertRaises(ExtractionError):
            decode_canonical(b"\x81" * 257 + b"\x00", "too deep")


def _nested(depth: int) -> object:
    value: object = 0
    for _ in range(depth):
        value = [value]
    return value


class MediaRegistryTests(unittest.TestCase):
    def test_registry_matches_the_vendored_media_profile(self) -> None:
        profile = json.loads(MEDIA_PROFILE.read_text(encoding="utf-8"))
        self.assertEqual(profile["profile"], "MIME-1")
        self.assertEqual(extractor.MEDIA_ALIASES, profile["aliases"])
        self.assertEqual(set(extractor.MEDIA_ENTRIES), set(profile["entries"]))
        for essence, entry in profile["entries"].items():
            with self.subTest(essence=essence):
                self.assertEqual(
                    extractor.MEDIA_ENTRIES[essence],
                    (entry["preferred"], tuple(entry["compatible"])),
                )
        self.assertEqual(extractor.MEDIA_TYPE_LIMIT, profile["maximum_media_type_bytes"])
        self.assertEqual(extractor.MEDIA_PARAMETER_LIMIT, profile["maximum_parameters"])
        self.assertEqual(extractor.SUFFIX_LIMIT, profile["maximum_suffix_bytes"])
        self.assertEqual(profile["unknown_preferred"], "bin")

    def test_normalisation_lowercases_aliases_and_sorts_parameters(self) -> None:
        self.assertEqual(normalize_media("Image/PNG"), "image/png")
        self.assertEqual(normalize_media("audio/x-wav"), "audio/wav")
        self.assertEqual(normalize_media("application/javascript"), "text/javascript")
        self.assertEqual(normalize_media('text/plain; Charset="UTF-8"'), "text/plain;charset=utf-8")
        self.assertEqual(normalize_media("a/b;y=2;x=1"), "a/b;x=1;y=2")
        self.assertEqual(normalize_media('a/b; x="p q"'), 'a/b;x="p q"')
        self.assertEqual(normalize_media("  a/b  "), "a/b")

    def test_malformed_media_types_are_refused(self) -> None:
        cases = [
            "image",
            "*/png",
            "image/*",
            "",
            "a/b c",
            "a/b\x01",
            "a/b;x=1;x=2",
            'a/b; x="unterminated',
            "a/" + "b" * 1100,
            "a/b" + "".join(f";p{index}=1" for index in range(33)),
        ]
        for value in cases:
            with self.subTest(value=value), self.assertRaises(_InvalidMedia):
                normalize_media(value)

    def test_annotations_drop_the_preferred_suffix_and_refuse_incompatible_ones(self) -> None:
        self.assertEqual(canonical_annotation("image/png", "PNG"), ("image/png", None))
        self.assertEqual(canonical_annotation("image/jpeg", "jpeg"), ("image/jpeg", "jpeg"))
        self.assertEqual(canonical_annotation("image/jpeg", "jpg"), ("image/jpeg", None))
        self.assertEqual(canonical_annotation("application/x-unknown", "abc"), ("application/x-unknown", "abc"))
        self.assertEqual(canonical_annotation("application/octet-stream", "xyz"), ("application/octet-stream", "xyz"))
        self.assertEqual(canonical_annotation("image/png", None), ("image/png", None))
        for media, suffix in [("image/png", "jpg"), ("image/png", "bad suffix"), ("image/png", 5), ("image/png", "a" * 33)]:
            with self.subTest(media=media, suffix=suffix), self.assertRaises(_InvalidMedia):
                canonical_annotation(media, suffix)


class GearPartitionTests(unittest.TestCase):
    def test_gear_cuts_agree_with_a_direct_scan(self) -> None:
        data = deterministic_bytes(GEAR_MAXIMUM + 1)
        for size in (0, 1, GEAR_MINIMUM - 1, GEAR_MINIMUM, 200_000, GEAR_MAXIMUM):
            chunk = data[:size]
            cut = reference_first_cut(chunk)
            if cut is None:
                expected = (False, False)
            elif cut == size:
                expected = (False, True)
            else:
                expected = (True, False)
            with self.subTest(size=size):
                self.assertEqual(gear_cuts(chunk), expected)

    def test_short_chunks_never_cut(self) -> None:
        self.assertEqual(gear_cuts(b"\x00" * (GEAR_MINIMUM - 1)), (False, False))

    def test_a_canonical_partition_is_accepted_chunk_by_chunk(self) -> None:
        data = deterministic_bytes(1_500_000)
        boundaries = GearBoundaries("partition")
        position = 0
        while position < len(data):
            remainder = data[position:]
            cut = reference_first_cut(remainder)
            chunk = remainder if cut is None else remainder[:cut]
            boundaries.push(chunk, f"chunk-{position}")
            position += len(chunk)
        self.assertEqual(position, len(data))

    def test_a_chunk_spanning_a_cut_is_refused(self) -> None:
        boundaries = GearBoundaries("span")
        with self.assertRaisesRegex(ExtractionError, "spans an OGB-2 cut point"):
            boundaries.push(b"\x00" * (GEAR_MAXIMUM + 1), "spanning")

    def test_a_chunk_that_does_not_end_at_a_cut_must_be_the_last(self) -> None:
        boundaries = GearBoundaries("open")
        boundaries.push(b"\x00" * 100, "first")
        with self.assertRaisesRegex(ExtractionError, "ends before an OGB-2 cut point"):
            boundaries.push(b"\x00", "second")


class GitObjectTests(unittest.TestCase):
    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.root = Path(self._directory.name)
        self.repository = make_repository(self.root / "repo", {"main.orna": b"module main\n", "notes.txt": b"n\n"})

    def tearDown(self) -> None:
        self._directory.cleanup()

    def blob_oid(self, data: bytes) -> str:
        return hashlib.sha1(f"blob {len(data)}".encode("ascii") + b"\0" + data).hexdigest()

    def test_resolve_commit_returns_the_commit_and_its_bytes(self) -> None:
        objects = GitObjects(self.repository, "test")
        try:
            oid, data = objects.resolve_commit("HEAD", "head")
        finally:
            objects.close()
        self.assertEqual(oid, git(self.repository, "rev-parse", "HEAD").decode().strip())
        self.assertEqual(objects.width, 20)
        self.assertEqual(objects.algorithm, "sha1")
        self.assertTrue(data.startswith(b"tree "))
        self.assertIn(oid, objects.verified)

    def test_read_returns_a_verified_blob(self) -> None:
        oid = git(self.repository, "rev-parse", "HEAD:main.orna").decode().strip()
        objects = GitObjects(self.repository, "test", 20)
        try:
            self.assertEqual(objects.read(oid, "blob", 1024, "module"), b"module main\n")
        finally:
            objects.close()

    def test_read_refuses_the_wrong_kind_size_and_width(self) -> None:
        oid = git(self.repository, "rev-parse", "HEAD:main.orna").decode().strip()
        # A refusal after a header abandons the cat-file stream, so each check reads through its own reader.
        for kind, limit, name, pattern in (
            ("tree", 1024, "wrong kind", "is a blob; a tree is required"),
            ("blob", 3, "oversize", "above the 3-byte bound"),
        ):
            objects = GitObjects(self.repository, "test", 20)
            try:
                with self.subTest(name), self.assertRaisesRegex(ExtractionError, pattern):
                    objects.read(oid, kind, limit, name)
            finally:
                objects.close()
        objects = GitObjects(self.repository, "test", 20)
        try:
            with self.assertRaisesRegex(ExtractionError, "not a 20-byte native object id"):
                objects.read("abc", "blob", 1024, "width")
        finally:
            objects.close()

    def test_a_refusal_stops_the_stream_for_later_reads(self) -> None:
        oid = git(self.repository, "rev-parse", "HEAD:main.orna").decode().strip()
        objects = GitObjects(self.repository, "test", 20)
        try:
            with self.assertRaisesRegex(ExtractionError, "is a blob; a tree is required"):
                objects.read(oid, "tree", 1024, "kind")
            # The unread body of the refused object must not be parsed as the next header.
            with self.assertRaisesRegex(ExtractionError, "stopped"):
                objects.read(oid, "blob", 1024, "after")
        finally:
            objects.close()

    def test_a_missing_object_is_unavailable(self) -> None:
        objects = GitObjects(self.repository, "test", 20)
        try:
            with self.assertRaises(UnavailableObject):
                objects.read("f" * 40, "blob", 1024, "missing")
        finally:
            objects.close()

    def test_a_substituted_object_fails_the_rehash(self) -> None:
        # A loose object whose content does not hash to the name it is stored
        # under: git cat-file serves it, so only the extractor's hash check can refuse it.
        claimed = self.blob_oid(b"xyz")
        directory = self.repository / ".git" / "objects" / claimed[:2]
        directory.mkdir(parents=True, exist_ok=True)
        (directory / claimed[2:]).write_bytes(zlib.compress(b"blob 3\0abc"))
        objects = GitObjects(self.repository, "test", 20)
        try:
            with self.assertRaisesRegex(ExtractionError, "does not hash to its id"):
                objects.read(claimed, "blob", 1024, "substituted")
        finally:
            objects.close()

    def test_git_directory_recognises_worktrees_bare_repositories_and_gitdir_files(self) -> None:
        self.assertEqual(git_directory(self.repository), self.repository / ".git")
        plain = self.root / "plain"
        plain.mkdir()
        self.assertIsNone(git_directory(plain))
        bare = self.root / "bare.git"
        git(self.root, "init", "-q", "--bare", str(bare))
        self.assertEqual(git_directory(bare), bare)
        linked = self.root / "linked"
        linked.mkdir()
        (linked / ".git").write_text("gitdir: ../repo/.git\n", encoding="utf-8")
        self.assertEqual(git_directory(linked), (linked / "../repo/.git").resolve())
        (plain / ".git").write_text("not a pointer\n", encoding="utf-8")
        self.assertIsNone(git_directory(plain))


class TreeParsingTests(unittest.TestCase):
    OID = bytes(range(20))

    def entry(self, mode: bytes, name: bytes, oid: bytes = OID) -> bytes:
        return mode + b" " + name + b"\0" + oid

    def test_a_canonical_tree_parses_in_order(self) -> None:
        data = self.entry(b"100644", b"a.orna") + self.entry(b"40000", b"store")
        entries = parse_tree(data, 20, "tree")
        self.assertEqual([(entry.mode, entry.name) for entry in entries], [(b"100644", b"a.orna"), (b"40000", b"store")])
        self.assertEqual(entries[0].oid, self.OID.hex())

    def test_unsupported_modes_names_and_orders_are_refused(self) -> None:
        cases = {
            "mode": self.entry(b"100666", b"a"),
            "slash": self.entry(b"100644", b"a/b"),
            "dot": self.entry(b"100644", b"."),
            "empty": self.entry(b"100644", b""),
            "repeated": self.entry(b"100644", b"a") + self.entry(b"100644", b"a"),
            "unsorted": self.entry(b"100644", b"b") + self.entry(b"100644", b"a"),
            "truncated": b"100644 a\0\x01",
        }
        for label, data in cases.items():
            with self.subTest(label=label), self.assertRaises(ExtractionError):
                parse_tree(data, 20, "tree")

    def test_commit_tree_names_the_root_tree(self) -> None:
        oid = "ab" * 20
        self.assertEqual(commit_tree(f"tree {oid}\nauthor x\n\nmsg\n".encode(), 20, "commit"), oid)
        with self.assertRaises(ExtractionError):
            commit_tree(b"parent " + oid.encode() + b"\n", 20, "commit")
        with self.assertRaises(ExtractionError):
            commit_tree(b"tree abcd\n", 20, "commit")


class StoreRejectionTests(unittest.TestCase):
    """Refusals of the native graph, each built as real git objects (store-format-3.md).

    Every test names a guarded check in orna_extract.py; each was shown to fail
    when its check is disabled by a one-line mutation.
    """

    WIDTH = 20

    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.root = Path(self._directory.name)
        self.repository = make_repository(self.root / "repo", {"main.orna": b"module main\n"})
        self.objects = GitObjects(self.repository, "test", self.WIDTH)
        self.graph = Graph(self.objects, BlobOutput(self.root / "out"))
        self.empty_tree = git(self.repository, "mktree", stdin=b"").decode().strip()

    def tearDown(self) -> None:
        self.objects.close()
        self._directory.cleanup()

    def write_blob(self, data: bytes) -> str:
        return git(self.repository, "hash-object", "-w", "--stdin", stdin=data).decode().strip()

    def write_node(self, data: bytes, children: dict[str, str]) -> str:
        """A node envelope: a data blob, plus a refs tree naming each child by its own id."""
        entries = [f"100644 blob {self.write_blob(data)}\tdata"]
        if children:
            lines = "".join(f"{mode} {oid}\t{oid}\n" for oid, mode in children.items())
            refs = git(self.repository, "mktree", stdin=lines.encode()).decode().strip()
            entries.append(f"040000 tree {refs}\trefs")
        return git(self.repository, "mktree", stdin="".join(f"{entry}\n" for entry in entries).encode()).decode().strip()

    def relation_leaf(self, domain: list, keys: list[bytes]) -> str:
        relation = [bytes.fromhex(self.empty_tree), bytes.fromhex(self.empty_tree), None, 1]
        return self.write_node(encode([1, 1, domain, [[key, relation] for key in keys]]), {self.empty_tree: "040000 tree"})

    def test_a_chunk_that_does_not_match_its_recorded_digest_is_refused(self) -> None:
        chunk = b"the bytes the index names"
        chunk_oid = self.write_blob(chunk)
        recorded = hashlib.sha256(b"other bytes").digest()
        root = self.write_node(
            encode([1, 4, 0, len(chunk), [[len(chunk), bytes.fromhex(chunk_oid), recorded]]]),
            {chunk_oid: "100644 blob"},
        )
        with self.assertRaisesRegex(ExtractionError, "corrupt chunk"):
            self.graph.byte_index(root, len(chunk), lambda _: None, "test")

    def test_keys_out_of_logical_order_in_one_page_are_refused(self) -> None:
        domain = ["relations", bytes(16)]
        root = self.relation_leaf(domain, [b"\x02" * 16, b"\x01" * 16])
        with self.assertRaisesRegex(ExtractionError, "out of logical key order"):
            self.graph.ordered_map(root, domain, "test")

    def test_pages_that_are_not_the_canonical_grouping_are_refused(self) -> None:
        domain = ["relations", bytes(16)]
        first = self.relation_leaf(domain, [b"\x01" * 16])
        second = self.relation_leaf(domain, [b"\x02" * 16])
        root = self.write_node(
            encode([1, 2, domain, 1, [[b"\x01" * 16, bytes.fromhex(first), 1], [b"\x02" * 16, bytes.fromhex(second), 1]]]),
            {first: "040000 tree", second: "040000 tree"},
        )
        # Canonical ORP-1 keeps two small entries in one page; the two single-entry pages are not that grouping.
        with self.assertRaisesRegex(ExtractionError, "not the canonical ORP-1 partition"):
            self.graph.ordered_map(root, domain, "test")

    def test_an_unregistered_tag_inside_a_node_is_refused_by_the_repository_reader(self) -> None:
        domain = ["rows", b"\x11" * 16, b"\x22" * 32]
        unregistered = b"\xd9\xea\x7a\x00"  # tag 60026 around 0; the registry admits 60000..60025 only
        data = b"\x84\x01\x01" + encode(domain) + b"\x81\x82" + encode(1) + b"\x81" + unregistered
        root = self.write_node(data, {})
        with self.assertRaisesRegex(ExtractionError, "unregistered CBOR tag 60026"):
            self.graph.ordered_map(root, domain, "test")


class CommandLineTests(unittest.TestCase):
    def setUp(self) -> None:
        self._directory = tempfile.TemporaryDirectory()
        self.root = Path(self._directory.name)

    def tearDown(self) -> None:
        self._directory.cleanup()

    def run_main(self, arguments: list[str]) -> tuple[int, str, str]:
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = main(arguments)
        return code, stdout.getvalue(), stderr.getvalue()

    def test_a_directory_that_is_not_a_repository_exits_one(self) -> None:
        plain = self.root / "plain"
        plain.mkdir()
        code, stdout, stderr = self.run_main([str(plain), "--output", str(self.root / "out")])
        self.assertEqual(code, 1)
        self.assertEqual(stdout, "")
        self.assertIn("is not a Git repository root", stderr)

    def test_a_commit_without_orna_directory_is_not_a_snapshot(self) -> None:
        repository = make_repository(self.root / "plain-repo", {"readme.txt": b"hello\n"})
        code, _, stderr = self.run_main([str(repository), "--output", str(self.root / "out")])
        self.assertEqual(code, 1)
        self.assertIn("commit has no .orna directory", stderr)

    def test_a_legacy_format_marker_is_refused(self) -> None:
        repository = make_repository(
            self.root / "legacy",
            {".orna/format.orna": b"format 2\n", ".orna/database.orna": b"x\n"},
        )
        code, _, stderr = self.run_main([str(repository), "--output", str(self.root / "out")])
        self.assertEqual(code, 1)
        self.assertIn("legacy format-1/2 snapshot", stderr)

    def test_extra_entries_under_orna_are_refused(self) -> None:
        repository = make_repository(
            self.root / "extra",
            {".orna/database.orna": b"x\n", ".orna/store/placeholder": b"y\n", ".orna/extra.orna": b"z\n"},
        )
        code, _, stderr = self.run_main([str(repository), "--output", str(self.root / "out")])
        self.assertEqual(code, 1)
        self.assertIn("format 3 allows exactly database.orna and store", stderr)

    def test_an_unknown_revision_exits_one(self) -> None:
        repository = make_repository(self.root / "rev", {"main.orna": b"module main\n"})
        code, _, stderr = self.run_main([str(repository), "no-such-revision", "--output", str(self.root / "out")])
        self.assertEqual(code, 1)
        self.assertTrue(stderr.startswith("orna_extract: "), stderr)

    def test_missing_output_and_malformed_dependency_are_usage_errors(self) -> None:
        with self.assertRaises(SystemExit) as missing_output, contextlib.redirect_stderr(io.StringIO()):
            self.run_main([str(self.root)])
        self.assertEqual(missing_output.exception.code, 2)
        with self.assertRaises(SystemExit) as bad_dependency, contextlib.redirect_stderr(io.StringIO()):
            self.run_main([str(self.root), "--output", str(self.root / "out"), "--dependency", "no-equals"])
        self.assertEqual(bad_dependency.exception.code, 2)

    def test_extract_raises_for_a_non_repository(self) -> None:
        with self.assertRaisesRegex(ExtractionError, "is not a Git repository root"):
            extract(self.root, "HEAD", self.root / "out", {})


class IndependenceTests(unittest.TestCase):
    """The extractor must stay outside every orna crate: stdlib imports only."""

    def test_the_extractor_imports_only_the_standard_library(self) -> None:
        tree = ast.parse((REPO_ROOT / "scripts/orna_extract.py").read_text(encoding="utf-8"))
        imported: set[str] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom):
                self.assertEqual(node.level, 0, "the extractor has no relative imports")
                imported.add((node.module or "").split(".")[0])
        non_stdlib = sorted(name for name in imported if name not in sys.stdlib_module_names)
        self.assertEqual(non_stdlib, [], "the extractor depends on non-stdlib modules")
        self.assertFalse(any(name.startswith("orna") for name in imported), "the extractor depends on an orna crate")


if __name__ == "__main__":
    unittest.main()
