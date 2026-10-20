from __future__ import annotations

import os
import subprocess
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from scripts.check_standard_library_pin import (
    PinError,
    check_const_pairing,
    check_mirror_bytes,
    check_reachable_from_main,
    gitlink_commit,
    mirrored_paths,
)

IDENTITY = {
    "GIT_AUTHOR_NAME": "Pin Test",
    "GIT_AUTHOR_EMAIL": "pin-test@example.invalid",
    "GIT_COMMITTER_NAME": "Pin Test",
    "GIT_COMMITTER_EMAIL": "pin-test@example.invalid",
}


def git(cwd: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args],
        cwd=cwd,
        env={**os.environ, **IDENTITY},
        capture_output=True,
        text=True,
        check=True,
    )
    return completed.stdout.strip()


def commit_file(repo: Path, relative: str, content: str, message: str) -> str:
    target = repo / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content, encoding="utf-8")
    git(repo, "add", "--", relative)
    git(repo, "commit", "--no-gpg-sign", "-q", "-m", message)
    return git(repo, "rev-parse", "HEAD")


class MirrorParsingTests(unittest.TestCase):
    def test_extracts_stdlib_relative_paths_in_source_order(self) -> None:
        text = (
            'const A: &str = include_str!("../../../stdlib/std/math.orna");\n'
            '    include_str!(\n        "../../../stdlib/std/encoding/base64.orna"\n    );\n'
            'const B: &str = include_str!("../fixtures/other.orna");\n'
        )

        self.assertEqual(mirrored_paths(text), ["math.orna", "encoding/base64.orna"])

    def test_rejects_a_source_mirrored_twice(self) -> None:
        text = (
            'include_str!("../../../stdlib/std/math.orna");\n'
            'include_str!("../../../stdlib/std/math.orna");\n'
        )

        with self.assertRaisesRegex(PinError, "twice: math.orna"):
            mirrored_paths(text)


PAIRED_MIRROR = """
pub const REFERENCE_STANDARD_MATH_PATH_V1: &str = "std/math.orna";
const REFERENCE_STANDARD_MATH_SOURCE_V1: &str = include_str!("../../../stdlib/std/math.orna");
pub const REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1: &str = "std/encoding/base64.orna";
const REFERENCE_STANDARD_ENCODING_BASE64_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/encoding/base64.orna");
fn reference_standard_sources_v1() -> [(String, String); 2] {
    [
        (
            REFERENCE_STANDARD_MATH_PATH_V1.into(),
            REFERENCE_STANDARD_MATH_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1.into(),
            REFERENCE_STANDARD_ENCODING_BASE64_SOURCE_V1.into(),
        ),
    ]
}
"""


class ConstPairingTests(unittest.TestCase):
    def test_accepts_matching_path_and_source_consts(self) -> None:
        check_const_pairing(PAIRED_MIRROR)

    def test_rejects_a_path_const_that_names_another_module(self) -> None:
        text = PAIRED_MIRROR.replace('"std/math.orna"', '"std/bits.orna"')
        with self.assertRaisesRegex(PinError, "MATH_PATH_V1 is 'std/bits.orna'"):
            check_const_pairing(text)

    def test_rejects_a_source_const_without_a_path_const(self) -> None:
        text = PAIRED_MIRROR.replace("pub const REFERENCE_STANDARD_MATH_PATH_V1", "const UNUSED_PATH")
        with self.assertRaisesRegex(PinError, "MATH_SOURCE_V1 has no path const"):
            check_const_pairing(text)

    def test_rejects_a_swapped_array_pair(self) -> None:
        text = PAIRED_MIRROR.replace(
            "REFERENCE_STANDARD_MATH_SOURCE_V1.into()",
            "REFERENCE_STANDARD_ENCODING_BASE64_SOURCE_V1.into()",
        )
        with self.assertRaisesRegex(PinError, "array pairs MATH_PATH_V1 with ENCODING_BASE64_SOURCE_V1"):
            check_const_pairing(text)


class GitlinkTests(unittest.TestCase):
    def test_reads_the_gitlink_commit_from_head(self) -> None:
        with TemporaryDirectory() as directory:
            superproject = Path(directory)
            git(superproject, "init", "-q")
            commit_file(superproject, "README.md", "root\n", "root")
            pinned = "a98702481c9d2dd92de3e160336024068a9f071c"
            git(superproject, "update-index", "--add", "--cacheinfo", f"160000,{pinned},stdlib/std")
            git(superproject, "commit", "--no-gpg-sign", "-q", "-m", "pin submodule")

            self.assertEqual(gitlink_commit(superproject), pinned)

    def test_rejects_a_missing_gitlink(self) -> None:
        with TemporaryDirectory() as directory:
            superproject = Path(directory)
            git(superproject, "init", "-q")
            commit_file(superproject, "README.md", "root\n", "root")

            with self.assertRaisesRegex(PinError, "is not a gitlink"):
                gitlink_commit(superproject)


class SubmoduleInvariantTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temporary = TemporaryDirectory()
        root = Path(self._temporary.name)
        self.upstream = root / "upstream"
        self.submodule = root / "submodule"
        self.upstream.mkdir()
        git(self.upstream, "init", "-q", "-b", "main")
        self.pinned = commit_file(self.upstream, ".orna/database.orna", "database\n", "first")
        self.main_tip = commit_file(self.upstream, "math.orna", "pub fn one(): Int { 1 }\n", "second")
        git(root, "clone", "-q", str(self.upstream), str(self.submodule))

    def tearDown(self) -> None:
        self._temporary.cleanup()

    def test_accepts_a_pin_that_is_an_ancestor_of_main(self) -> None:
        git(self.submodule, "checkout", "-q", "--detach", self.pinned)

        check_reachable_from_main(self.submodule, self.pinned)

    def test_rejects_a_pin_that_main_does_not_contain(self) -> None:
        git(self.submodule, "checkout", "-q", "--detach", self.main_tip)
        orphan = commit_file(self.submodule, "side.orna", "pub fn two(): Int { 2 }\n", "side")

        with self.assertRaisesRegex(PinError, "not reachable from ornadb-std main"):
            check_reachable_from_main(self.submodule, orphan)

    def test_mirror_bytes_must_equal_the_pinned_blob(self) -> None:
        git(self.submodule, "checkout", "-q", "--detach", self.main_tip)
        check_mirror_bytes(self.submodule, self.main_tip, ["math.orna"])

        (self.submodule / "math.orna").write_text("pub fn one(): Int { 9 }\n", encoding="utf-8")

        with self.assertRaisesRegex(PinError, "math.orna differs from the pinned blob"):
            check_mirror_bytes(self.submodule, self.main_tip, ["math.orna"])

    def test_mirror_must_not_reference_an_untracked_source(self) -> None:
        git(self.submodule, "checkout", "-q", "--detach", self.main_tip)

        with self.assertRaisesRegex(PinError, "not tracked at the pin"):
            check_mirror_bytes(self.submodule, self.main_tip, ["missing.orna"])


if __name__ == "__main__":
    unittest.main()
