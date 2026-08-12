#!/usr/bin/env python3
"""Build and verify Orna's deterministic PostgreSQL support-data bundle."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat
import subprocess
import sys
import tarfile


SOURCE_DATE_EPOCH = 1778528675
EXPECTED_TIMEZONE_FILES = 598
GENERATED_INPUTS = {
    "src/backend/snowball/snowball_create.sql",
    "src/include/catalog/postgres.bki",
    "src/include/catalog/system_constraints.sql",
}
STATIC_INPUTS = (
    ("src/include/catalog/postgres.bki", ""),
    ("src/backend/libpq/pg_hba.conf.sample", ""),
    ("src/backend/libpq/pg_ident.conf.sample", ""),
    ("src/backend/utils/misc/postgresql.conf.sample", ""),
    ("src/backend/snowball/snowball_create.sql", ""),
    ("src/backend/catalog/information_schema.sql", ""),
    ("src/backend/catalog/sql_features.txt", ""),
    ("src/include/catalog/system_constraints.sql", ""),
    ("src/backend/catalog/system_functions.sql", ""),
    ("src/backend/catalog/system_views.sql", ""),
    *((f"src/timezone/tznames/{name}", "timezonesets") for name in (
        "Africa.txt",
        "America.txt",
        "Antarctica.txt",
        "Asia.txt",
        "Atlantic.txt",
        "Australia",
        "Australia.txt",
        "Default",
        "Etc.txt",
        "Europe.txt",
        "India",
        "Indian.txt",
        "Pacific.txt",
    )),
    *((f"src/backend/snowball/stopwords/{language}.stop", "tsearch_data")
      for language in (
          "danish",
          "dutch",
          "english",
          "finnish",
          "french",
          "german",
          "hungarian",
          "italian",
          "nepali",
          "norwegian",
          "portuguese",
          "russian",
          "spanish",
          "swedish",
          "turkish",
      )),
)


def regular_bytes(path: Path, *, permit_hard_links: bool) -> bytes:
    source_stat = path.lstat()
    if not stat.S_ISREG(source_stat.st_mode):
        raise SystemExit(f"support source is not a regular file: {path}")
    if not permit_hard_links and source_stat.st_nlink != 1:
        raise SystemExit(f"support source is linked: {path}")
    if source_stat.st_mode & 0o111:
        raise SystemExit(f"support source has executable mode: {path}")

    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        opened_stat = os.fstat(descriptor)
        if (
            opened_stat.st_dev != source_stat.st_dev
            or opened_stat.st_ino != source_stat.st_ino
            or opened_stat.st_size != source_stat.st_size
        ):
            raise SystemExit(f"support source changed before read: {path}")
        content = bytearray()
        while block := os.read(descriptor, 1024 * 1024):
            content.extend(block)
        final_stat = os.fstat(descriptor)
        if (
            final_stat.st_size != opened_stat.st_size
            or final_stat.st_mtime_ns != opened_stat.st_mtime_ns
            or final_stat.st_ctime_ns != opened_stat.st_ctime_ns
        ):
            raise SystemExit(f"support source changed during read: {path}")
    finally:
        os.close(descriptor)
    return bytes(content)


def accepted_path(output_path: str, casefold_paths: set[str]) -> PurePosixPath:
    pure_path = PurePosixPath(output_path)
    if (
        not output_path
        or pure_path.is_absolute()
        or str(pure_path) != output_path
        or ".." in pure_path.parts
        or any(character in output_path for character in "*?[]")
        or any(ord(character) < 0x20 or ord(character) == 0x7F for character in output_path)
        or output_path.casefold() in casefold_paths
    ):
        raise SystemExit(f"support output path is not accepted: {output_path}")

    lowered_parts = [part.casefold() for part in pure_path.parts]
    lowered_name = lowered_parts[-1]
    if "extension" in lowered_parts or "plpgsql" in output_path.casefold():
        raise SystemExit(f"support output contains extension or PL/pgSQL material: {output_path}")
    if lowered_name in {"postgres", "psql", "initdb", "pg_upgrade", "pg_ctl", "pg_resetwal"}:
        raise SystemExit(f"support output has a PostgreSQL executable name: {output_path}")
    if lowered_name.endswith((
        ".a", ".o", ".so", ".tar", ".tar.gz", ".tgz", ".bz2", ".zip", ".control",
    )) or ".so." in lowered_name:
        raise SystemExit(f"support output contains code or an archive: {output_path}")
    return pure_path


def add_member(
    staging_root: Path,
    members: dict[str, dict[str, object]],
    casefold_paths: set[str],
    output_path: str,
    content: bytes,
) -> None:
    pure_path = accepted_path(output_path, casefold_paths)
    destination = staging_root.joinpath(*pure_path.parts)
    destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(
        destination,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        view = memoryview(content)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise SystemExit(f"support output write failed: {output_path}")
            view = view[written:]
        os.fchmod(descriptor, 0o600)
        final_stat = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    if not stat.S_ISREG(final_stat.st_mode) or final_stat.st_nlink != 1:
        raise SystemExit(f"staged support output is not one regular file: {output_path}")

    members[output_path] = {
        "length": len(content),
        "mode": "0600",
        "path": output_path,
        "sha256": hashlib.sha256(content).hexdigest(),
        "type": "file",
    }
    casefold_paths.add(output_path.casefold())


def select_input(source_root: Path, build_root: Path, relative: str) -> bytes:
    source_candidate = source_root / relative
    build_candidate = build_root / relative
    source_exists = os.path.lexists(source_candidate)
    build_exists = os.path.lexists(build_candidate)
    if relative in GENERATED_INPUTS and not build_exists:
        raise SystemExit(f"generated support source is absent: {relative}")
    if not source_exists and not build_exists:
        raise SystemExit(f"support source is absent: {relative}")

    source_content = regular_bytes(source_candidate, permit_hard_links=False) if source_exists else None
    build_content = regular_bytes(build_candidate, permit_hard_links=False) if build_exists else None
    if source_content is not None and build_content is not None and source_content != build_content:
        raise SystemExit(f"build and source support inputs differ: {relative}")
    if build_content is not None:
        return build_content
    assert source_content is not None
    return source_content


def verify_staging(staging_root: Path, expected_paths: set[str]) -> None:
    root_stat = staging_root.lstat()
    if staging_root.is_symlink() or not stat.S_ISDIR(root_stat.st_mode) or stat.S_IMODE(root_stat.st_mode) != 0o700:
        raise SystemExit("staged support root is not one private directory")

    actual_paths = set()
    for directory, directory_names, file_names in os.walk(staging_root, followlinks=False):
        directory_path = Path(directory)
        for name in directory_names:
            candidate = directory_path / name
            candidate_stat = candidate.lstat()
            if candidate.is_symlink() or not stat.S_ISDIR(candidate_stat.st_mode) or stat.S_IMODE(candidate_stat.st_mode) != 0o700:
                raise SystemExit(f"staged support directory is not private: {candidate}")
        for name in file_names:
            candidate = directory_path / name
            candidate_stat = candidate.lstat()
            if not stat.S_ISREG(candidate_stat.st_mode) or candidate_stat.st_nlink != 1 or stat.S_IMODE(candidate_stat.st_mode) != 0o600:
                raise SystemExit(f"staged support member metadata is not accepted: {candidate}")
            actual_paths.add(PurePosixPath(*candidate.relative_to(staging_root).parts).as_posix())
    if actual_paths != expected_paths:
        raise SystemExit("staged support inventory differs from its manifest")


def verify_bundle(bundle_path: Path, members: list[dict[str, object]]) -> None:
    expected = {str(member["path"]): member for member in members}
    with tarfile.open(bundle_path, mode="r:") as bundle:
        actual_members = bundle.getmembers()
        if [member.name for member in actual_members] != list(expected):
            raise SystemExit("support bundle order or inventory is not accepted")
        for member in actual_members:
            expected_member = expected[member.name]
            if (
                not member.isfile()
                or member.mode != 0o600
                or member.uid != 0
                or member.gid != 0
                or member.mtime != SOURCE_DATE_EPOCH
                or member.size != expected_member["length"]
            ):
                raise SystemExit(f"support bundle metadata is not accepted: {member.name}")
            member_file = bundle.extractfile(member)
            if member_file is None or hashlib.sha256(member_file.read()).hexdigest() != expected_member["sha256"]:
                raise SystemExit(f"support bundle member digest is not accepted: {member.name}")


def publish(source: Path, destination: Path) -> None:
    content = regular_bytes(source, permit_hard_links=False)
    descriptor = os.open(
        destination,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o644,
    )
    try:
        view = memoryview(content)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise SystemExit(f"could not publish support output: {destination.name}")
            view = view[written:]
        os.fchmod(descriptor, 0o644)
    finally:
        os.close(descriptor)


def main() -> None:
    if len(sys.argv) != 6:
        raise SystemExit("usage: support_bundle.py SOURCE_ROOT BUILD_ROOT TIMEZONE_ROOT WORK_ROOT OUTPUT_ROOT")
    source_root, build_root, timezone_root, work_root, output_root = map(Path, sys.argv[1:])
    for path in (source_root, build_root, timezone_root, output_root):
        if not path.is_absolute() or not path.is_dir() or path.is_symlink():
            raise SystemExit(f"support path is not an absolute directory: {path}")
    if not work_root.is_absolute() or os.path.lexists(work_root):
        raise SystemExit("support work root must be an absent absolute path")

    os.umask(0o077)
    work_root.mkdir(mode=0o700)
    staging_root = work_root / "root"
    staging_root.mkdir(mode=0o700)
    members: dict[str, dict[str, object]] = {}
    casefold_paths: set[str] = set()

    for relative, prefix in STATIC_INPUTS:
        output_path = PurePosixPath(prefix, PurePosixPath(relative).name).as_posix()
        add_member(staging_root, members, casefold_paths, output_path, select_input(source_root, build_root, relative))

    timezone_stat = timezone_root.lstat()
    if stat.S_IMODE(timezone_stat.st_mode) != 0o700:
        raise SystemExit("generated timezone root is not private")
    timezone_files = []
    for directory, directory_names, file_names in os.walk(timezone_root, followlinks=False):
        directory_path = Path(directory)
        for name in directory_names:
            candidate = directory_path / name
            candidate_stat = candidate.lstat()
            if candidate.is_symlink() or not stat.S_ISDIR(candidate_stat.st_mode) or stat.S_IMODE(candidate_stat.st_mode) != 0o700:
                raise SystemExit(f"generated timezone directory is not private: {candidate}")
        for name in file_names:
            candidate = directory_path / name
            relative = PurePosixPath(*candidate.relative_to(timezone_root).parts).as_posix()
            timezone_files.append((relative, candidate))
    if len(timezone_files) != EXPECTED_TIMEZONE_FILES:
        raise SystemExit(f"generated timezone tree has {len(timezone_files)} files")
    for relative, candidate in sorted(timezone_files):
        add_member(
            staging_root,
            members,
            casefold_paths,
            PurePosixPath("timezone", relative).as_posix(),
            regular_bytes(candidate, permit_hard_links=True),
        )

    manifest_members = [members[path] for path in sorted(members)]
    verify_staging(staging_root, set(members))
    manifest_path = work_root / "embedded-postgresql-support-manifest.json"
    member_list_path = work_root / "members.txt"
    bundle_path = work_root / "embedded-postgresql-support.tar"
    manifest_path.write_text(
        json.dumps({"format": 1, "members": manifest_members}, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    manifest_path.chmod(0o600)
    member_list_path.write_text(
        "".join(f"{member['path']}\n" for member in manifest_members),
        encoding="utf-8",
        newline="\n",
    )
    member_list_path.chmod(0o600)
    subprocess.run(
        (
            "tar", "--create", f"--file={bundle_path}", f"--directory={staging_root}",
            "--no-recursion", "--format=ustar", "--owner=0", "--group=0", "--numeric-owner",
            "--mode=0600", f"--mtime=@{SOURCE_DATE_EPOCH}", "--verbatim-files-from",
            f"--files-from={member_list_path}",
        ),
        check=True,
        env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "SOURCE_DATE_EPOCH": str(SOURCE_DATE_EPOCH)},
    )
    verify_bundle(bundle_path, manifest_members)
    publish(manifest_path, output_root / manifest_path.name)
    publish(bundle_path, output_root / bundle_path.name)


if __name__ == "__main__":
    main()
