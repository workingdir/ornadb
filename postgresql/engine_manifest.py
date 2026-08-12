#!/usr/bin/env python3
"""Record evidence from a completed embedded PostgreSQL build."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat
import subprocess
import sys


MANIFEST_NAME = "embedded-engine-manifest.json"


def command(*arguments: str, cwd: Path | None = None) -> bytes:
    return subprocess.run(
        arguments,
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
    ).stdout


def digest(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def regular_file(path: Path) -> tuple[bytes, os.stat_result]:
    source_stat = path.lstat()
    if not stat.S_ISREG(source_stat.st_mode) or source_stat.st_nlink != 1:
        raise SystemExit(f"manifest input is not one regular file: {path}")
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        opened_stat = os.fstat(descriptor)
        content = bytearray()
        while block := os.read(descriptor, 1024 * 1024):
            content.extend(block)
        final_stat = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    if (
        opened_stat.st_dev != source_stat.st_dev
        or opened_stat.st_ino != source_stat.st_ino
        or final_stat.st_size != source_stat.st_size
        or final_stat.st_mtime_ns != opened_stat.st_mtime_ns
        or final_stat.st_ctime_ns != opened_stat.st_ctime_ns
    ):
        raise SystemExit(f"manifest input changed while reading: {path}")
    return bytes(content), final_stat


def file_record(path: Path, relative: str) -> dict[str, object]:
    content, file_stat = regular_file(path)
    return {
        "length": len(content),
        "mode": f"{stat.S_IMODE(file_stat.st_mode):04o}",
        "path": relative,
        "sha256": digest(content),
    }


def tracked_inputs(repository: Path) -> tuple[list[dict[str, object]], list[dict[str, object]], list[dict[str, object]]]:
    records = command("git", "ls-files", "--stage", "-z", "--", "postgresql", cwd=repository)
    integration: list[dict[str, object]] = []
    overlays: list[dict[str, object]] = []
    patches: list[dict[str, object]] = []
    seen_casefold: set[str] = set()
    for raw_record in records.split(b"\0"):
        if not raw_record:
            continue
        metadata, raw_path = raw_record.split(b"\t", 1)
        mode, _, stage = metadata.decode("ascii").split()
        relative = raw_path.decode("utf-8")
        if mode != "100644" or stage != "0" or PurePosixPath(relative).as_posix() != relative:
            raise SystemExit(f"manifest build input is not one ordinary tracked file: {relative}")
        if relative.casefold() in seen_casefold:
            raise SystemExit(f"manifest build input case-collides: {relative}")
        seen_casefold.add(relative.casefold())
        record = file_record(repository / relative, relative)
        if relative.startswith("postgresql/overlays/"):
            overlays.append(record)
        elif relative.startswith("postgresql/patches/"):
            patches.append(record)
        else:
            integration.append(record)
    if not integration or not overlays or not patches:
        raise SystemExit("manifest build input inventory is incomplete")
    return integration, overlays, patches


def upstream_inventory(source: Path) -> tuple[int, str]:
    records = command("git", "ls-tree", "-r", "-z", "--full-tree", "HEAD", cwd=source)
    inventory = bytearray()
    count = 0
    for raw_record in records.split(b"\0"):
        if not raw_record:
            continue
        metadata, raw_path = raw_record.split(b"\t", 1)
        mode, kind, object_id = metadata.decode("ascii").split()
        if kind != "blob":
            raise SystemExit("upstream source tree contains a non-blob entry")
        path = raw_path.decode("utf-8")
        if PurePosixPath(path).as_posix() != path:
            raise SystemExit(f"upstream source path is not canonical: {path}")
        content = command("git", "cat-file", "blob", object_id, cwd=source)
        inventory.extend(f"{mode} {digest(content)} {path}\n".encode())
        count += 1
    if count == 0:
        raise SystemExit("upstream source inventory is empty")
    return count, digest(bytes(inventory))


def output_inventory(output_root: Path) -> list[dict[str, object]]:
    outputs = []
    casefold_names = set()
    for path in sorted(output_root.iterdir(), key=lambda candidate: candidate.name):
        if path.name == MANIFEST_NAME:
            continue
        if path.name.casefold() in casefold_names:
            raise SystemExit(f"build output case-collides: {path.name}")
        content, file_stat = regular_file(path)
        mode = stat.S_IMODE(file_stat.st_mode)
        if mode != 0o644 or mode & 0o111 or path.name.endswith((".so", ".o")) or ".so." in path.name:
            raise SystemExit(f"build output is not accepted data or a static input: {path.name}")
        outputs.append({
            "length": len(content),
            "mode": "0644",
            "path": path.name,
            "sha256": digest(content),
        })
        casefold_names.add(path.name.casefold())
    if not outputs:
        raise SystemExit("build output inventory is empty")
    return outputs


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: engine_manifest.py REPOSITORY_ROOT TARGET_ROOT IMAGE")
    repository = Path(sys.argv[1]).resolve(strict=True)
    target_root = Path(sys.argv[2]).resolve(strict=True)
    image = sys.argv[3]
    if target_root.parent != repository / "target":
        raise SystemExit("manifest target root must be directly below the repository target directory")
    source = repository / "third_party/postgresql"
    prepared_inventory = target_root / "prepared-source-inventory.txt"
    output_root = target_root / "output"
    if not source.is_dir() or not output_root.is_dir() or output_root.is_symlink():
        raise SystemExit("manifest source or output root is absent")
    if os.path.lexists(output_root / MANIFEST_NAME):
        raise SystemExit("embedded engine manifest already exists")

    integration, overlays, patches = tracked_inputs(repository)
    upstream_count, upstream_digest = upstream_inventory(source)
    prepared_content, _ = regular_file(prepared_inventory)
    prepared_count = len(prepared_content.splitlines())
    if prepared_count != upstream_count + len(overlays):
        raise SystemExit("prepared source inventory count is not accepted")

    gitlink = command(
        "git", "ls-files", "--stage", "--", "third_party/postgresql", cwd=repository,
    ).decode().split()
    commit = command("git", "rev-parse", "HEAD", cwd=source).decode().strip()
    if len(gitlink) < 2 or gitlink[0] != "160000" or gitlink[1] != commit:
        raise SystemExit("upstream source does not match the superproject gitlink")
    tree = command("git", "rev-parse", "HEAD^{tree}", cwd=source).decode().strip()
    source_url = command(
        "git", "config", "--file", ".gitmodules", "--get", "submodule.third_party/postgresql.url",
        cwd=repository,
    ).decode().strip()
    containerfile_record = next(
        record for record in integration if record["path"] == "postgresql/Containerfile"
    )
    containerfile = (repository / "postgresql/Containerfile").read_text(encoding="utf-8")
    first_line = containerfile.splitlines()[0]
    if not first_line.startswith("FROM ") or "@sha256:" not in first_line:
        raise SystemExit("Containerfile does not select one immutable base image")
    base_image = first_line.removeprefix("FROM ")
    package_lines = command(
        "docker", "run", "--rm", "--network=none", image,
        "dpkg-query", "-W", "-f=${binary:Package}=${Version}\\n",
    ).decode().splitlines()

    manifest = {
        "build_inputs": {
            "integration": integration,
            "overlays": overlays,
            "patches": patches,
        },
        "builder": {
            "base_image": base_image,
            "containerfile_sha256": containerfile_record["sha256"],
            "image": image,
            "packages": sorted(package_lines),
        },
        "format": 1,
        "outputs": output_inventory(output_root),
        "prepared_source": {
            "file_count": prepared_count,
            "inventory_sha256": digest(prepared_content),
        },
        "upstream": {
            "commit": commit,
            "file_count": upstream_count,
            "inventory_sha256": upstream_digest,
            "path": "third_party/postgresql",
            "tree": tree,
            "url": source_url,
        },
    }
    content = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    destination = output_root / MANIFEST_NAME
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
                raise SystemExit("could not write the embedded engine manifest")
            view = view[written:]
        os.fchmod(descriptor, 0o644)
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    main()
