#!/usr/bin/env python3
"""Focused tests for deterministic package generation and package rejection.

The packages themselves come from pkgsmith through ``package.make_packages``,
so these tests also prove the manifest declares what this file expects: the
same two payload paths on all three formats, no lifecycle script, and the same
bytes from the same inputs.
"""

from __future__ import annotations

import copy
import importlib.util
import io
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from unittest.mock import patch


sys.dont_write_bytecode = True

PACKAGE_MODULE = Path(__file__).with_name("package.py")
spec = importlib.util.spec_from_file_location("orna_linux_package", PACKAGE_MODULE)
if spec is None or spec.loader is None:
    raise RuntimeError("could not load package generator")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)

REPOSITORY = Path(__file__).resolve().parents[2]
EPOCH = 1700000000


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
    pax: bool = False,
) -> None:
    with tarfile.open(source, mode="r:") as archive, tarfile.open(
        destination,
        mode="w",
        format=tarfile.PAX_FORMAT if pax else tarfile.USTAR_FORMAT,
        pax_headers={"comment": "untrusted"} if pax else None,
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


def planned_files(scratch: Path, epoch: int = EPOCH) -> dict[str, str]:
    """The file names the manifest declares, planned against an empty prefix."""
    empty = scratch / "empty-prefix"
    empty.mkdir(exist_ok=True)
    return package.planned_files(
        package.plan_rows(
            package.plan_output(
                prefix=empty,
                version=package.cargo_product_version(REPOSITORY),
                source_date_epoch=epoch,
            )
        )
    )


def rpm_scripts(path: Path) -> str:
    return subprocess.run(
        ["rpm", "-qp", "--scripts", str(path)],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    ).stdout


def repack_deb(
    source: Path, destination: Path, *, tamper: bool = False, postinst: bool = False
) -> None:
    """Rebuild a deb with a changed payload or an added maintainer script."""
    work = destination.with_name(destination.name + ".work")
    shutil.rmtree(work, ignore_errors=True)
    subprocess.run(["dpkg-deb", "-x", str(source), str(work)], check=True)
    subprocess.run(["dpkg-deb", "-e", str(source), str(work / "DEBIAN")], check=True)
    if tamper:
        target = work / package.EXECUTABLE
        target.write_bytes(target.read_bytes() + b"tampered")
        target.chmod(0o755)
    if postinst:
        script = work / "DEBIAN" / "postinst"
        script.write_text("#!/bin/sh\nexit 0\n")
        script.chmod(0o755)
    subprocess.run(
        ["dpkg-deb", "--build", "--root-owner-group", str(work), str(destination)],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def replace_package(dist: Path, source: Path, name: str) -> Path:
    """A copy of a package directory with one package replaced."""
    copy_dir = dist.with_name(f"{dist.name}-{name.replace('.', '-')}")
    shutil.rmtree(copy_dir, ignore_errors=True)
    shutil.copytree(dist, copy_dir)
    (copy_dir / name).write_bytes(source.read_bytes())
    return copy_dir


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="orna-linux-package-") as scratch_name:
        scratch = Path(scratch_name)

        # The package-owned target is reset, so no stale input reaches a package.
        stale_target = scratch / "build-repository" / "target" / "linux-package"
        stale_target.mkdir(mode=0o700, parents=True)
        (stale_target / "stale-output").write_text("stale")
        fresh_target = package.prepare_build_target(stale_target.parents[1])
        if (fresh_target / "stale-output").exists():
            raise AssertionError("package build target retained stale output")

        # Staging writes the two payload files, with their modes, and nothing
        # else: a stale prefix is replaced rather than packaged.
        prefix = scratch / "staging" / "prefix"
        (prefix / "usr" / "bin").mkdir(parents=True)
        (prefix / "usr" / "bin" / "orphan").write_text("orphan")
        package.stage_prefix(prefix, b"binary", b"manifest")
        if package.staged_members(prefix) != sorted(package.PREFIX_MEMBERS):
            raise AssertionError(f"staged prefix is {package.staged_members(prefix)}")
        if stat.S_IMODE((prefix / package.EXECUTABLE).stat().st_mode) != 0o755:
            raise AssertionError("staged executable mode changed")
        if stat.S_IMODE((prefix / package.DISTRIBUTION_MANIFEST).stat().st_mode) != 0o644:
            raise AssertionError("staged manifest mode changed")
        symlinked_prefix = scratch / "symlinked-prefix"
        symlinked_prefix.symlink_to(prefix, target_is_directory=True)
        expect_failure(lambda: package.stage_prefix(symlinked_prefix, b"x", b"y"))

        # One real package set, built twice from the same executable.
        executable = scratch / "orna"
        shutil.copyfile("/bin/true", executable)
        executable.chmod(0o755)
        version = package.cargo_product_version(REPOSITORY)
        planned = planned_files(scratch)
        first = scratch / "first-dist"
        second = scratch / "second-dist"
        package.make_packages(
            REPOSITORY,
            executable_path=executable,
            output_dir=first,
            source_date_epoch=EPOCH,
        )
        package.make_packages(
            REPOSITORY,
            executable_path=executable,
            output_dir=second,
            source_date_epoch=EPOCH,
        )
        first_inventory = sorted(entry.name for entry in first.iterdir())
        if first_inventory != sorted(entry.name for entry in second.iterdir()):
            raise AssertionError("identical package inputs produced different files")
        for name in first_inventory:
            if (first / name).read_bytes() != (second / name).read_bytes():
                raise AssertionError(f"identical package inputs produced different bytes: {name}")
        if planned != {
            "tar": f"orna-{version}-linux-amd64.tar",
            "rpm": f"orna-{version}-1.x86_64.rpm",
            "deb": f"orna_{version}-1_amd64.deb",
        }:
            raise AssertionError(f"manifest declared unexpected package names: {planned}")

        # The tar is exactly the two payload files, in order, root-owned, with
        # every timestamp from SOURCE_DATE_EPOCH, and the manifest binds the
        # executable it ships.
        tar_path = first / planned["tar"]
        with tarfile.open(tar_path, mode="r:") as archive:
            members = archive.getmembers()
            names = [member.name for member in members]
            manifest_text = archive.extractfile(
                package.DISTRIBUTION_MANIFEST
            ).read().decode("utf-8")
        if names != list(package.PREFIX_MEMBERS):
            raise AssertionError(f"package tar inventory changed: {names}")
        for member in members:
            expected_mode = 0o755 if member.name == package.EXECUTABLE else 0o644
            if member.uid != 0 or member.gid != 0:
                raise AssertionError(f"package tar member is not root-owned: {member.name}")
            if member.uname != "root" or member.gname != "root":
                raise AssertionError(f"package tar member is not named root: {member.name}")
            if member.mode & 0o7777 != expected_mode:
                raise AssertionError(f"package tar member mode changed: {member.name}")
            if member.mtime != EPOCH:
                raise AssertionError(f"package tar member timestamp changed: {member.name}")
        if "engine" in manifest_text or "builder_image" in manifest_text:
            raise AssertionError("distribution manifest still binds a retired engine input")
        package.parse_distribution_manifest(
            manifest_text.encode("utf-8"),
            executable_sha256=package.digest(executable.read_bytes()),
        )
        if not manifest_text.startswith("format = 1\n"):
            raise AssertionError("distribution manifest is not format 1")

        # rpm and deb carry the same files, and no lifecycle script.
        rpm_path = first / planned["rpm"]
        if rpm_scripts(rpm_path).strip():
            raise AssertionError("rpm carries a lifecycle script")
        deb_path = first / planned["deb"]
        if sorted(package._deb_tar_names(deb_path, "--ctrl-tarfile")) != [
            "control",
            "md5sums",
        ]:
            raise AssertionError("deb control archive gained a member")
        if [
            member.name.removeprefix("./")
            for member, _ in package._deb_data_members(deb_path)
        ] != list(package.PREFIX_MEMBERS):
            raise AssertionError("deb payload inventory changed")

        # Tampering with the tar artifact is refused.
        extra_member = scratch / "extra-member.tar"
        with tarfile.open(tar_path, mode="r:") as archive, tarfile.open(
            extra_member, mode="w", format=tarfile.USTAR_FORMAT
        ) as rewritten:
            for member in archive.getmembers():
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise AssertionError("fixture archive member had no data")
                rewritten.addfile(copy.copy(member), extracted)
            ghost = tarfile.TarInfo("usr/share/orna/embedded-engine-manifest.json")
            ghost.size = 2
            ghost.mode = 0o644
            rewritten.addfile(ghost, io.BytesIO(b"{}"))
        expect_failure(lambda: package.verify_tar(extra_member, expected_epoch=EPOCH))
        for label, kwargs in (
            ("missing manifest", {"remove": package.DISTRIBUTION_MANIFEST}),
            ("tampered executable", {"tamper": package.EXECUTABLE}),
            ("tampered manifest", {"tamper": package.DISTRIBUTION_MANIFEST}),
            ("wrong owner", {"owner": package.DISTRIBUTION_MANIFEST}),
            ("pax metadata", {"pax": True}),
        ):
            rejected = scratch / f"rejected-{label.replace(' ', '-')}.tar"
            rewrite_archive(tar_path, rejected, **kwargs)
            expect_failure(lambda: package.verify_tar(rejected, expected_epoch=EPOCH))
        expect_failure(lambda: package.verify_tar(tar_path, expected_epoch=EPOCH + 1))

        # A deb with a changed payload, or with an added maintainer script, is
        # refused even though dpkg itself would install it.
        changed_deb = scratch / "changed.deb"
        repack_deb(deb_path, changed_deb, tamper=True)
        expect_failure(
            lambda: package.verify_packages(
                replace_package(first, changed_deb, planned["deb"]),
                planned=planned,
                version=version,
                expected_epoch=EPOCH,
            )
        )
        scripted_deb = scratch / "scripted.deb"
        repack_deb(deb_path, scripted_deb, postinst=True)
        expect_failure(
            lambda: package.verify_packages(
                replace_package(first, scripted_deb, planned["deb"]),
                planned=planned,
                version=version,
                expected_epoch=EPOCH,
            )
        )

        # A changed byte inside the rpm payload fails the rpm's own digests.
        changed_rpm = scratch / "changed.rpm"
        rpm_bytes = bytearray(rpm_path.read_bytes())
        rpm_bytes[-500] ^= 0xFF
        changed_rpm.write_bytes(bytes(rpm_bytes))
        expect_failure(
            lambda: package.verify_packages(
                replace_package(first, changed_rpm, planned["rpm"]),
                planned=planned,
                version=version,
                expected_epoch=EPOCH,
            )
        )

        # A package set with a format missing is refused.
        missing_dist = scratch / "missing-dist"
        shutil.copytree(first, missing_dist)
        (missing_dist / planned["deb"]).unlink()
        expect_failure(
            lambda: package.verify_packages(
                missing_dist, planned=planned, version=version, expected_epoch=EPOCH
            )
        )

        # Ambient compiler, target, profile, and flag overrides are refused
        # before Cargo can produce a different executable.
        for override in (
            "RUSTC",
            "RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTC",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_AR",
            "CARGO_PROFILE_RELEASE_LTO",
            "CARGO_BUILD_TARGET",
            "RUSTUP_TOOLCHAIN",
        ):
            with patch.dict(package.os.environ, {override: "/tmp/compiler"}):
                expect_failure(package.reject_compiler_overrides)

        # Installing the tar artifact is idempotent, restores the payload
        # modes, and refuses a foreign, partial, wrong-mode, or replaced root.
        install_root = scratch / "install"
        previous_umask = os.umask(0o077)
        try:
            package.install_archive(tar_path, install_root)
        finally:
            os.umask(previous_umask)
        installed_executable = install_root / package.EXECUTABLE
        installed_manifest = install_root / package.DISTRIBUTION_MANIFEST
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
        package.install_archive(tar_path, install_root)
        foreign_root = scratch / "foreign-root"
        foreign_binary = foreign_root / package.EXECUTABLE
        foreign_binary.parent.mkdir(mode=0o755, parents=True)
        shutil.copyfile("/bin/true", foreign_binary)
        foreign_binary.chmod(0o755)
        expect_failure(lambda: package.install_archive(tar_path, foreign_root))
        if foreign_binary.read_bytes() != Path("/bin/true").read_bytes():
            raise AssertionError("package install overwrote a foreign executable")
        partial_root = scratch / "partial-root"
        partial_manifest = partial_root / package.DISTRIBUTION_MANIFEST
        partial_manifest.parent.mkdir(mode=0o755, parents=True)
        partial_manifest.write_bytes(installed_manifest.read_bytes())
        partial_manifest.chmod(0o644)
        expect_failure(lambda: package.install_archive(tar_path, partial_root))
        wrong_mode_root = scratch / "wrong-mode-root"
        wrong_mode_root.mkdir(mode=0o700)
        wrong_mode_root.chmod(0o700)
        expect_failure(lambda: package.install_archive(tar_path, wrong_mode_root))
        symlink_target = scratch / "symlink-target"
        symlink_target.mkdir()
        symlink_root = scratch / "symlink-root"
        symlink_root.symlink_to(symlink_target, target_is_directory=True)
        expect_failure(lambda: package.install_archive(tar_path, symlink_root))
        ancestor_root = scratch / "ancestor-root"
        ancestor_root.symlink_to(symlink_target, target_is_directory=True)
        expect_failure(lambda: package.install_archive(tar_path, ancestor_root / "nested"))
        replaced_parent_root = scratch / "replaced-parent-root"
        escaped_target = scratch / "escaped-target"
        escaped_target.mkdir(mode=0o755)
        original_atomic_write_at = package.atomic_write_at
        replaced = False

        def replace_parent_after_write(parent, name, content, *, mode):
            nonlocal replaced
            original_atomic_write_at(parent, name, content, mode=mode)
            if replaced:
                return
            replaced = True
            retained_parent = replaced_parent_root / "usr"
            retained_parent.rename(scratch / "detached-usr")
            retained_parent.symlink_to(escaped_target, target_is_directory=True)

        with patch.object(package, "atomic_write_at", replace_parent_after_write):
            expect_failure(lambda: package.install_archive(tar_path, replaced_parent_root))
        if (escaped_target / "bin" / "orna").exists():
            raise AssertionError("replaced install parent redirected package payload")
        if os.geteuid() != 0:
            owned_root = scratch / "owned-root"
            owned_root.mkdir(mode=0o755)
            with patch.object(package.os, "geteuid", return_value=0):
                expect_failure(lambda: package.install_archive(tar_path, owned_root))

        # A non-executable source, or a file that only starts like an ELF, is
        # refused before any package is written.
        executable.chmod(0o644)
        expect_failure(
            lambda: package.make_packages(
                REPOSITORY,
                executable_path=executable,
                output_dir=scratch / "not-executable-dist",
                source_date_epoch=EPOCH,
            )
        )
        executable.chmod(0o755)
        malformed_elf = scratch / "malformed-elf"
        malformed_elf.write_bytes(b"\x7fELF" + b"\x02\x01\x00" + executable.read_bytes()[7:])
        malformed_elf.chmod(0o755)
        expect_failure(
            lambda: package.make_packages(
                REPOSITORY,
                executable_path=malformed_elf,
                output_dir=scratch / "malformed-elf-dist",
                source_date_epoch=EPOCH,
            )
        )

    print("linux package tests passed")


if __name__ == "__main__":
    main()
