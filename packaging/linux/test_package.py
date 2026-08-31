#!/usr/bin/env python3
"""Focused tests for deterministic package generation and archive rejection."""

from __future__ import annotations

import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import stat
import tarfile
import tempfile
from unittest.mock import patch


PACKAGE_MODULE = Path(__file__).with_name("package.py")
spec = importlib.util.spec_from_file_location("orna_linux_package", PACKAGE_MODULE)
if spec is None or spec.loader is None:
    raise RuntimeError("could not load package generator")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


def expect_failure(action) -> None:
    try:
        action()
    except package.PackageError:
        return
    raise AssertionError("tampered package input was accepted")


def rewrite_archive(
    source: Path,
    destination: Path,
    *,
    remove: str | None = None,
    tamper: str | None = None,
    owner: str | None = None,
) -> None:
    with tarfile.open(source, mode="r:") as archive, tarfile.open(
        destination, mode="w", format=tarfile.USTAR_FORMAT
    ) as rewritten:
        for member in archive.getmembers():
            if member.name == remove:
                continue
            extracted = archive.extractfile(member)
            if extracted is None:
                raise AssertionError("fixture archive member had no data")
            content = extracted.read()
            replacement = copy.copy(member)
            if member.name == tamper:
                content = content + b"tampered"
                replacement.size = len(content)
            if member.name == owner:
                replacement.uid = 1000
            rewritten.addfile(replacement, io.BytesIO(content))


def main() -> None:
    repository = Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix="orna-linux-package-") as scratch_name:
        scratch = Path(scratch_name)
        stale_target = scratch / "build-repository" / "target" / "linux-package"
        stale_target.mkdir(mode=0o700, parents=True)
        (stale_target / "stale-engine-manifest.json").write_text("stale")
        fresh_target = package.prepare_build_target(stale_target.parents[1])
        if (fresh_target / "stale-engine-manifest.json").exists():
            raise AssertionError("package build target retained stale output")
        executable = scratch / "orna"
        shutil.copyfile("/bin/true", executable)
        executable.chmod(0o755)
        engine = scratch / "embedded-engine-manifest.json"
        engine.write_text(
            json.dumps(
                {
                    "format": 1,
                    "builder": {"image_digest": "sha256:" + "3" * 64},
                },
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        engine.chmod(0o644)
        first = scratch / "first.tar"
        second = scratch / "second.tar"
        arguments = type(
            "Arguments",
            (),
            {
                "repository": str(repository),
                "executable": str(executable),
                "engine_manifest": str(engine),
                "output": str(first),
                "source_date_epoch": "1700000000",
            },
        )()
        package.make_archive(arguments)
        first_bytes = first.read_bytes()
        arguments.output = str(second)
        package.make_archive(arguments)
        if first_bytes != second.read_bytes():
            raise AssertionError("identical package inputs produced different archives")
        package.verify_archive(first, expected_epoch=1700000000)
        for override in (
            "RUSTC",
            "RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTC",
        ):
            with patch.dict(package.os.environ, {override: "/tmp/compiler"}):
                expect_failure(package.reject_compiler_overrides)

        install_root = scratch / "install"
        previous_umask = os.umask(0o077)
        try:
            package.install_archive(first, install_root)
        finally:
            os.umask(previous_umask)
        installed_executable = install_root / ARCHIVE_EXECUTABLE
        installed_manifest = install_root / ARCHIVE_DISTRIBUTION_MANIFEST
        if not installed_executable.is_file() or not installed_manifest.is_file():
            raise AssertionError("package install omitted executable or distribution manifest")
        for relative in ("", "usr", "usr/bin", "usr/share", "usr/share/orna"):
            directory = install_root / relative
            if stat.S_IMODE(directory.stat().st_mode) != 0o755:
                raise AssertionError(f"package directory mode changed: {directory}")
        if stat.S_IMODE(installed_executable.stat().st_mode) != 0o755:
            raise AssertionError("installed executable mode changed")
        if stat.S_IMODE(installed_manifest.stat().st_mode) != 0o644:
            raise AssertionError("installed manifest mode changed")
        symlink_target = scratch / "symlink-target"
        symlink_target.mkdir()
        symlink_root = scratch / "symlink-root"
        symlink_root.symlink_to(symlink_target, target_is_directory=True)
        expect_failure(lambda: package.install_archive(first, symlink_root))
        ancestor_root = scratch / "ancestor-root"
        ancestor_root.symlink_to(symlink_target, target_is_directory=True)
        expect_failure(lambda: package.install_archive(first, ancestor_root / "nested"))
        if os.geteuid() != 0:
            owned_root = scratch / "owned-root"
            owned_root.mkdir(mode=0o755)
            with patch.object(package.os, "geteuid", return_value=0):
                expect_failure(lambda: package.install_archive(first, owned_root))
        arguments.engine_manifest = None
        expect_failure(lambda: package.make_archive(arguments))
        arguments.engine_manifest = str(engine)
        arguments.executable = None
        expect_failure(lambda: package.make_archive(arguments))
        arguments.executable = str(executable)

        missing = scratch / "missing.tar"
        rewrite_archive(first, missing, remove=package.ARCHIVE_ENGINE_MANIFEST)
        expect_failure(lambda: package.verify_archive(missing))

        tampered = scratch / "tampered.tar"
        rewrite_archive(first, tampered, tamper=package.ARCHIVE_EXECUTABLE)
        expect_failure(lambda: package.verify_archive(tampered))

        wrong_owner = scratch / "wrong-owner.tar"
        rewrite_archive(first, wrong_owner, owner=package.ARCHIVE_DISTRIBUTION_MANIFEST)
        expect_failure(lambda: package.verify_archive(wrong_owner))

        executable.chmod(0o644)
        expect_failure(lambda: package.make_archive(arguments))

    print("linux package tests passed")


ARCHIVE_EXECUTABLE = package.ARCHIVE_EXECUTABLE
ARCHIVE_DISTRIBUTION_MANIFEST = package.ARCHIVE_DISTRIBUTION_MANIFEST


if __name__ == "__main__":
    main()
