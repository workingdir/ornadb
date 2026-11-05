#!/usr/bin/env python3
"""Build, verify, and install the Orna Linux artifact.

pkgsmith writes every package from one manifest, ``pkgsmith.star``, over the
files of an install prefix.  It builds nothing and runs no command, so this
driver supplies the two things the artifact contract needs and pkgsmith
deliberately does not do:

* a reproducible release build of the single executable -- workspace path
  remap, no build id, incremental compilation off, every timestamp from
  ``SOURCE_DATE_EPOCH``; and
* the distribution manifest that binds that executable to its build inputs.

The artifact is exactly one executable and its distribution manifest, with no
package-manager lifecycle hooks, no network access, no runtime helper
executable, and no external database engine.  Two runs over the same inputs
produce identical package bytes.

pkgsmith writes three packages from that one prefix: an uncompressed tar (the
install artifact of release 1.0.0), an rpm, and a deb.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from typing import Iterable, NoReturn


EXECUTABLE = "usr/bin/orna"
DISTRIBUTION_MANIFEST = "usr/share/orna/distribution-manifest.toml"
PREFIX_MEMBERS = (EXECUTABLE, DISTRIBUTION_MANIFEST)
PACKAGE_FORMATS = ("tar", "rpm", "deb")
PACKAGE_NAME = "orna"
PACKAGING_SCRIPT = Path(__file__).with_name("pkgsmith.star")
DIST_EPOCH_RECORD = ".source-date-epoch"
RUST_PATH_REMAP = "<repository>=/usr/src/orna"
RUST_LINK_FLAGS = "-C link-arg=-Wl,--build-id=none"
MANIFEST_LIMIT = 16 * 1024
TARGET_TRIPLE = "x86_64-unknown-linux-gnu"


class PackageError(RuntimeError):
    """An input, package, or manifest failed the package contract."""


def fail(message: str) -> "NoReturn":
    raise PackageError(message)


def digest(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def valid_digest(value: object, *, prefix: str = "") -> bool:
    return (
        isinstance(value, str)
        and value.startswith(prefix)
        and len(value) == len(prefix) + 64
        and all(character in "0123456789abcdef" for character in value[len(prefix) :])
    )


def require_linux_x86_64() -> None:
    if sys.platform != "linux" or os.uname().machine != "x86_64":
        fail(f"package target must be Linux x86_64 ({TARGET_TRIPLE})")


def valid_linux_x86_64_elf(content: bytes) -> bool:
    return (
        len(content) >= 24
        and content[:4] == b"\x7fELF"
        and content[4] == 2
        and content[5] == 1
        and content[6] == 1
        and int.from_bytes(content[16:18], "little") in (2, 3)
        and int.from_bytes(content[18:20], "little") == 62
        and int.from_bytes(content[20:24], "little") == 1
    )


def read_stable_file(path: Path, *, mode: int | None = None) -> tuple[bytes, os.stat_result]:
    """Read one regular file while detecting replacement/races."""
    try:
        source_stat = path.lstat()
    except OSError as error:
        fail(f"cannot stat package input {path}: {error}")
    if not stat.S_ISREG(source_stat.st_mode):
        fail(f"package input is not one regular file: {path}")
    source_mode = stat.S_IMODE(source_stat.st_mode)
    if mode is not None and source_mode != mode:
        fail(f"package input has mode {source_mode:04o}, expected {mode:04o}: {path}")
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    except OSError as error:
        fail(f"cannot open package input {path}: {error}")
    try:
        opened_stat = os.fstat(descriptor)
        content = bytearray()
        while True:
            block = os.read(descriptor, 1024 * 1024)
            if not block:
                break
            content.extend(block)
        final_stat = os.fstat(descriptor)
    except OSError as error:
        fail(f"cannot read package input {path}: {error}")

    finally:
        os.close(descriptor)
    if (
        opened_stat.st_dev != source_stat.st_dev
        or opened_stat.st_ino != source_stat.st_ino
        or opened_stat.st_nlink != source_stat.st_nlink
        or final_stat.st_size != source_stat.st_size
        or final_stat.st_mtime_ns != opened_stat.st_mtime_ns
        or final_stat.st_ctime_ns != opened_stat.st_ctime_ns
    ):
        fail(f"package input changed while reading: {path}")
    if mode is not None and stat.S_IMODE(final_stat.st_mode) != mode:
        fail(f"package input mode changed while reading: {path}")
    return bytes(content), final_stat


def input_path(value: str, label: str) -> Path:
    path = Path(value)
    if path.is_symlink():
        fail(f"{label} is a symbolic link: {path}")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        fail(f"{label} is absent: {path}: {error}")
    if resolved.is_symlink():
        fail(f"{label} resolves to a symbolic link: {path}")
    return resolved


def quote_toml_string(value: str) -> str:
    if not value or any(character in value for character in "\x00\r\n"):
        fail("manifest string contains an invalid control character")
    # JSON's double-quoted string escaping is the same subset needed by basic
    # TOML strings and keeps the generated representation canonical.
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))


def tool_version(command: str) -> str:
    try:
        result = subprocess.run(
            [command, "--version"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        fail(f"could not read {command} version: {error}")
    value = result.stdout.rstrip("\n")
    if not value or any(character in value for character in "\x00\r\n"):
        fail(f"{command} returned an invalid version")
    return value


def reject_compiler_overrides() -> None:
    """Reject ambient compiler, target, profile, and flag overrides before Cargo runs."""
    exact_names = {
        "RUSTC",
        "RUSTFLAGS",
        "RUSTUP_TOOLCHAIN",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
    }
    rejected: list[str] = []
    for name, value in os.environ.items():
        if not value:
            continue
        target_override = name.startswith("CARGO_TARGET_") and name.endswith(
            (
                "_RUSTC",
                "_RUSTC_WRAPPER",
                "_RUSTC_WORKSPACE_WRAPPER",
                "_RUSTFLAGS",
                "_LINKER",
                "_AR",
            )
        )
        if (
            name in exact_names
            or name.startswith("RUSTC_")
            or name in {"RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
            or name.startswith("CARGO_BUILD_RUSTC_")
            or target_override
            or name.startswith("CARGO_PROFILE_")
        ):
            rejected.append(name)
    if rejected:
        fail(
            "reproducible build refuses compiler environment overrides: "
            + ", ".join(sorted(rejected))
        )


def cargo_lock_digest(repository: Path) -> str:
    content, _ = read_stable_file(repository / "Cargo.lock", mode=0o644)
    return digest(content)


def cargo_product_version(repository: Path) -> str:
    try:
        with (repository / "Cargo.toml").open("rb") as source:
            document = tomllib.load(source)
    except (OSError, tomllib.TOMLDecodeError) as error:
        fail(f"cannot read workspace Cargo.toml: {error}")
    try:
        version = document["workspace"]["package"]["version"]
    except (KeyError, TypeError):
        fail("workspace Cargo.toml has no package version")
    if not isinstance(version, str) or not version or any(
        character in version for character in "\x00\r\n"
    ):
        fail("workspace package version is invalid")
    return version


def parse_quoted(line: str, key: str) -> str | None:
    if not line.startswith(key):
        return None
    value = line[len(key) :]
    if len(value) < 2 or not value.startswith('"') or not value.endswith('"'):
        return None
    value = value[1:-1]
    if '"' in value or any(character in value for character in "\x00\r\n"):
        return None
    return value


def parse_distribution_manifest(content: bytes, *, executable_sha256: str) -> None:
    if len(content) > MANIFEST_LIMIT:
        fail("distribution manifest is too large")
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        fail(f"distribution manifest is not UTF-8: {error}")
    if not text.endswith("\n"):
        fail("distribution manifest has no final newline")
    lines = text[:-1].split("\n")
    if len(lines) != 7 or lines[0] != "format = 1":
        fail("distribution manifest shape is not format 1")
    rustc = parse_quoted(lines[1], "rustc = ")
    cargo = parse_quoted(lines[2], "cargo = ")
    lock = parse_quoted(lines[3], "cargo_lock_sha256 = ")
    executable = parse_quoted(lines[6], "executable_sha256 = ")
    if (
        rustc is None
        or not rustc.startswith("rustc ")
        or cargo is None
        or not cargo.startswith("cargo ")
        or lock is None
        or not valid_digest(lock)
        or lines[4] != f'rust_path_remap = "{RUST_PATH_REMAP}"'
        or lines[5] != f'rust_link_flags = "{RUST_LINK_FLAGS}"'
        or executable != executable_sha256
    ):
        fail("distribution manifest evidence does not bind package inputs")


def canonical_manifest(
    *,
    rustc: str,
    cargo: str,
    cargo_lock_sha256: str,
    executable_sha256: str,
) -> bytes:
    lines = [
        "format = 1",
        f"rustc = {quote_toml_string(rustc)}",
        f"cargo = {quote_toml_string(cargo)}",
        f"cargo_lock_sha256 = {quote_toml_string(cargo_lock_sha256)}",
        f"rust_path_remap = {quote_toml_string(RUST_PATH_REMAP)}",
        f"rust_link_flags = {quote_toml_string(RUST_LINK_FLAGS)}",
        f"executable_sha256 = {quote_toml_string(executable_sha256)}",
    ]
    return ("\n".join(lines) + "\n").encode("utf-8")


def atomic_write(path: Path, content: bytes, *, mode: int = 0o644) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        descriptor = os.open(
            path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
            mode,
        )
    except OSError as error:
        fail(f"cannot create package output {path}: {error}")
    try:
        view = memoryview(content)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                fail(f"could not write package output {path}")
            view = view[written:]
        os.fchmod(descriptor, mode)
        os.fsync(descriptor)
    except OSError as error:
        fail(f"could not write package output {path}: {error}")
    finally:
        os.close(descriptor)


# --------------------------------------------------------------------------
# The release build
# --------------------------------------------------------------------------


def package_target(repository: Path) -> Path:
    return repository / "target" / "linux-package"


def prepare_build_target(repository: Path) -> Path:
    """Reset the package-owned target so no stale input can reach a package."""
    target = package_target(repository)
    try:
        metadata = target.lstat()
    except FileNotFoundError:
        metadata = None
    except OSError as error:
        fail(f"cannot inspect package build target {target}: {error}")
    if metadata is not None:
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            fail(f"package build target is not a directory: {target}")
        try:
            shutil.rmtree(target)
        except OSError as error:
            fail(f"cannot reset package build target {target}: {error}")
    try:
        target.mkdir(mode=0o700, parents=True, exist_ok=True)
    except OSError as error:
        fail(f"cannot create package build target {target}: {error}")
    return target


def build_executable(repository: Path, target: Path, source_date_epoch: int) -> Path:
    """Build the one release executable reproducibly inside the package target."""
    reject_compiler_overrides()
    cargo_target = target / "cargo"
    try:
        cargo_target.mkdir(mode=0o700, parents=True, exist_ok=True)
    except OSError as error:
        fail(f"cannot create package build target {cargo_target}: {error}")
    environment = os.environ.copy()
    environment.update(
        {
            "CARGO_TARGET_DIR": str(cargo_target),
            "CARGO_INCREMENTAL": "0",
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "SOURCE_DATE_EPOCH": str(source_date_epoch),
            "TZ": "UTC0",
        }
    )
    required_flags = [
        f"--remap-path-prefix={repository}=/usr/src/orna",
        RUST_LINK_FLAGS,
    ]
    environment["RUSTFLAGS"] = " ".join(required_flags)
    command = [
        "cargo",
        "build",
        "--locked",
        "--release",
        "--manifest-path",
        str(repository / "Cargo.toml"),
        "--package",
        "orna-cli",
        "--bin",
        "orna",
    ]
    try:
        subprocess.run(command, cwd=repository, env=environment, check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        fail(f"the reproducible Orna build failed: {error}")
    executable = cargo_target / "release" / "orna"
    if not executable.is_file() or executable.is_symlink():
        fail(f"the reproducible build did not produce {executable}")
    return executable


# --------------------------------------------------------------------------
# pkgsmith
# --------------------------------------------------------------------------


def pkgsmith_path() -> Path:
    configured = os.environ.get("PKGSMITH")
    candidate = configured if configured else shutil.which("pkgsmith")
    if not candidate:
        fail(
            "pkgsmith was not found: set PKGSMITH to the pkgsmith binary, or put "
            "pkgsmith on PATH (build it with `cargo build --release --workspace` "
            "in a clone of https://github.com/kierandrewett/pkgsmith)"
        )
    try:
        resolved = Path(candidate).resolve(strict=True)
    except OSError as error:
        fail(f"pkgsmith is absent: {candidate}: {error}")
    if not resolved.is_file():
        fail(f"pkgsmith is not a file: {resolved}")
    return resolved


def run_pkgsmith(arguments: list[str], *, prefix: Path, source_date_epoch: int) -> str:
    """Run one pkgsmith command over the prefix and return its standard output."""
    environment = os.environ.copy()
    environment.update(
        {
            "SOURCE_DATE_EPOCH": str(source_date_epoch),
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "TZ": "UTC0",
        }
    )
    command = [str(pkgsmith_path()), *arguments, "--prefix", str(prefix)]
    try:
        result = subprocess.run(
            command,
            env=environment,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except OSError as error:
        fail(f"cannot run pkgsmith: {error}")
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or "").strip() or (error.stdout or "").strip()
        fail(f"pkgsmith {arguments[0]} failed: {detail or error.returncode}")
    return result.stdout


def plan_output(*, prefix: Path, version: str, source_date_epoch: int) -> str:
    return run_pkgsmith(
        ["plan", "--json", "--build-arg", f"VERSION={version}", str(PACKAGING_SCRIPT)],
        prefix=prefix,
        source_date_epoch=source_date_epoch,
    )


def plan_rows(output: str) -> list[dict]:
    """The packages the manifest declares, as pkgsmith plans them.

    `plan` reports a broken package in a zero-exit run, so the rows are checked
    here rather than trusting the exit status.
    """
    try:
        rows = json.loads(output)
    except json.JSONDecodeError as error:
        fail(f"pkgsmith plan is not JSON: {error}")
    if not isinstance(rows, list) or not rows:
        fail("pkgsmith plan declared no package")
    for row in rows:
        if not isinstance(row, dict):
            fail("pkgsmith plan row is not an object")
        if row.get("error"):
            fail(f"pkgsmith plan failed for {row.get('format')}: {row['error']}")
    return rows


def planned_files(rows: Iterable[dict]) -> dict[str, str]:
    planned: dict[str, str] = {}
    for row in rows:
        format = row.get("format")
        name = row.get("file")
        if format in planned:
            fail(f"pkgsmith plan declared format {format} twice")
        if not isinstance(format, str) or not format:
            fail("pkgsmith plan row has no format")
        if not isinstance(name, str) or not name or "/" in name:
            fail(f"pkgsmith plan declared no plain file name for {format}")
        if row.get("arch") != "amd64":
            fail(f"pkgsmith plan declared architecture {row.get('arch')} for {format}")
        values = row.get("values") or {}
        if values.get("name") != PACKAGE_NAME:
            fail(f"pkgsmith plan declared package {values.get('name')}, not {PACKAGE_NAME}")
        planned[format] = name
    if set(planned) != set(PACKAGE_FORMATS):
        fail(
            f"pkgsmith plan declares {sorted(planned)}, not {list(PACKAGE_FORMATS)}"
        )
    return planned


def plan_packages(
    *, prefix: Path, version: str, source_date_epoch: int
) -> tuple[dict[str, str], str]:
    """Plan the packages, returning their file names and the readable plan."""
    output = plan_output(
        prefix=prefix, version=version, source_date_epoch=source_date_epoch
    )
    planned = planned_files(plan_rows(output))
    readable = run_pkgsmith(
        ["plan", "--build-arg", f"VERSION={version}", str(PACKAGING_SCRIPT)],
        prefix=prefix,
        source_date_epoch=source_date_epoch,
    )
    return planned, readable


# --------------------------------------------------------------------------
# Staging the prefix
# --------------------------------------------------------------------------


def staged_members(prefix: Path) -> list[str]:
    try:
        return sorted(
            path.relative_to(prefix).as_posix()
            for path in prefix.rglob("*")
            if not path.is_dir()
        )
    except OSError as error:
        fail(f"cannot read package prefix {prefix}: {error}")


def stage_prefix(prefix: Path, executable: bytes, manifest: bytes) -> None:
    """Write the two payload files into a fresh prefix and nothing else."""
    try:
        if prefix.is_symlink():
            fail(f"package prefix is a symbolic link: {prefix}")
        if prefix.exists():
            if not prefix.is_dir():
                fail(f"package prefix is not a directory: {prefix}")
            shutil.rmtree(prefix)
        prefix.mkdir(mode=0o700, parents=True)
    except OSError as error:
        fail(f"cannot create package prefix {prefix}: {error}")
    for relative, content, mode in (
        (EXECUTABLE, executable, 0o755),
        (DISTRIBUTION_MANIFEST, manifest, 0o644),
    ):
        target = prefix / relative
        target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
        atomic_write(target, content, mode=mode)
    staged = staged_members(prefix)
    if staged != sorted(PREFIX_MEMBERS):
        fail(f"staged prefix holds {staged}, not {list(PREFIX_MEMBERS)}")


# --------------------------------------------------------------------------
# Verification
# --------------------------------------------------------------------------


def run_tool(argv: list[str], *, what: str) -> str:
    try:
        result = subprocess.run(
            argv, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
    except OSError as error:
        fail(f"cannot run {argv[0]} to {what}: {error}")
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or b"").decode("utf-8", "replace").strip()
        fail(f"{argv[0]} could not {what}: {detail or error.returncode}")
    return result.stdout.decode("utf-8", "replace")


def require_tool(name: str, *, what: str) -> None:
    if not shutil.which(name):
        fail(f"{name} is required to {what} but is not on PATH")


def one_timestamp(timestamps: Iterable[int], epoch: int | None, *, label: str) -> int:
    distinct = set(timestamps)
    if len(distinct) != 1:
        fail(f"{label} carries more than one file timestamp: {sorted(distinct)}")
    observed = distinct.pop()
    if epoch is not None and observed != epoch:
        fail(f"{label} file timestamps are {observed}, not SOURCE_DATE_EPOCH {epoch}")
    return observed


def tar_members(path: Path, *, label: str) -> list[tuple[tarfile.TarInfo, bytes]]:
    try:
        with tarfile.open(path, mode="r:") as archive:
            if archive.pax_headers:
                fail(f"{label} has unsupported PAX metadata")
            members: list[tuple[tarfile.TarInfo, bytes]] = []
            for member in archive.getmembers():
                if member.pax_headers:
                    fail(f"{label} member has unsupported PAX metadata: {member.name}")
                if not member.isfile() or member.issym() or member.islnk():
                    fail(f"{label} member is not a regular file: {member.name}")
                extracted = archive.extractfile(member)
                if extracted is None:
                    fail(f"{label} member has no data: {member.name}")
                content = extracted.read()
                if len(content) != member.size:
                    fail(f"{label} member length changed: {member.name}")
                members.append((member, content))
            return members
    except (OSError, tarfile.TarError) as error:
        fail(f"cannot read {label} {path}: {error}")


def payload_of(
    members: Iterable[tuple[tarfile.TarInfo, bytes]],
) -> dict[str, tuple[bytes, int]]:
    return {member.name: (content, member.mode & 0o7777) for member, content in members}


def verify_tar(
    path: Path, *, expected_epoch: int | None
) -> list[tuple[tarfile.TarInfo, bytes]]:
    """Check the install artifact: exactly the two payload files, in order."""
    require_linux_x86_64()
    members = tar_members(path, label="package tar")
    names = [member.name for member, _ in members]
    if names != list(PREFIX_MEMBERS):
        fail(f"package tar inventory is {names}, not {list(PREFIX_MEMBERS)}")
    for member, _ in members:
        expected_mode = 0o755 if member.name == EXECUTABLE else 0o644
        if member.uid != 0 or member.gid != 0:
            fail(f"package tar member is not root-owned: {member.name}")
        if member.uname != "root" or member.gname != "root":
            fail(f"package tar member is not named root:root: {member.name}")
        if member.mode & 0o7777 != expected_mode:
            fail(f"package tar member has the wrong mode: {member.name}")
    one_timestamp([member.mtime for member, _ in members], expected_epoch, label="package tar")
    payload = payload_of(members)
    executable = payload[EXECUTABLE][0]
    if not executable:
        fail("packaged executable is empty")
    parse_distribution_manifest(
        payload[DISTRIBUTION_MANIFEST][0], executable_sha256=digest(executable)
    )
    if not valid_linux_x86_64_elf(executable):
        fail("packaged executable is not a Linux x86_64 ELF")
    return members


def verify_rpm(
    path: Path,
    payload: dict[str, tuple[bytes, int]],
    *,
    version: str,
    expected_epoch: int | None,
) -> None:
    """Check an rpm carries the same payload, and no lifecycle scripts."""
    require_tool("rpm", what="verify the rpm package")
    header = run_tool(
        [
            "rpm",
            "-qp",
            "--qf",
            "%{NAME}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\t%{LICENSE}\t%{GROUP}\t%{BUILDTIME}\n",
            str(path),
        ],
        what="read the rpm header",
    ).strip("\n")
    columns = header.split("\t")
    if len(columns) != 7:
        fail(f"rpm header is not the expected seven fields: {header!r}")
    observed = dict(
        zip(("name", "version", "release", "arch", "license", "group"), columns[:6])
    )
    try:
        build_time = int(columns[6], 10)
    except ValueError as error:
        fail(f"rpm build time is not a decimal: {columns[6]!r}: {error}")
    expected_header = {
        "name": PACKAGE_NAME,
        "version": version,
        "release": "1",
        "arch": "x86_64",
        "license": "Apache-2.0",
        "group": "Applications/Databases",
    }
    if observed != expected_header:
        fail(f"rpm header is {observed}, not {expected_header}")
    if expected_epoch is not None and build_time != expected_epoch:
        fail(f"rpm build time is {build_time}, not SOURCE_DATE_EPOCH {expected_epoch}")
    requires = {
        line.strip()
        for line in run_tool(
            ["rpm", "-qp", "--requires", str(path)], what="read the rpm requirements"
        ).splitlines()
        if line.strip() and not line.startswith("rpmlib(")
    }
    if requires != {"glibc", "libgcc"}:
        fail(f"rpm requirements are {sorted(requires)}, not ['glibc', 'libgcc']")
    for query in ("--scripts", "--triggers"):
        written = run_tool(
            ["rpm", "-qp", query, str(path)], what=f"read rpm {query.lstrip('-')}"
        ).strip()
        if written:
            fail(f"rpm carries a lifecycle script ({query}): {written}")
    run_tool(["rpm", "-K", "--nosignature", str(path)], what="check the rpm digests")
    dump: dict[str, dict[str, object]] = {}
    for line in run_tool(
        ["rpm", "-qp", "--dump", str(path)], what="dump the rpm files"
    ).splitlines():
        fields = line.split()
        if len(fields) < 7:
            fail(f"rpm file dump row is malformed: {line!r}")
        name = fields[0].lstrip("/")
        if name in dump:
            fail(f"rpm lists the payload file twice: {name}")
        try:
            dump[name] = {
                "size": int(fields[1], 10),
                "mtime": int(fields[2], 10),
                "digest": fields[3],
                "mode": int(fields[4], 8) & 0o7777,
                "user": fields[5],
                "group": fields[6],
            }
        except ValueError as error:
            fail(f"rpm file dump row is malformed: {line!r}: {error}")
    if set(dump) != set(payload):
        fail(f"rpm payload is {sorted(dump)}, not {sorted(payload)}")
    for name, (content, mode) in payload.items():
        row = dump[name]
        if row["size"] != len(content):
            fail(f"rpm payload length differs for {name}")
        if row["digest"] != digest(content):
            fail(f"rpm payload digest differs for {name}")
        if row["mode"] != mode:
            fail(f"rpm payload mode differs for {name}")
        if row["user"] != "root" or row["group"] != "root":
            fail(f"rpm payload is not root:root for {name}")
    one_timestamp([row["mtime"] for row in dump.values()], expected_epoch, label="rpm")


def verify_deb(
    path: Path,
    payload: dict[str, tuple[bytes, int]],
    *,
    version: str,
    expected_epoch: int | None,
) -> None:
    """Check a deb carries the same payload, and no lifecycle scripts."""
    require_tool("dpkg-deb", what="verify the deb package")
    fields: dict[str, str] = {}
    current: str | None = None
    for line in run_tool(
        ["dpkg-deb", "-f", str(path)], what="read the deb control file"
    ).splitlines():
        if line.startswith((" ", "\t")):
            if current is None:
                fail(f"deb control has a continuation before any field: {line!r}")
            fields[current] += "\n" + line
            continue
        key, separator, value = line.partition(":")
        if not separator or not key or key != key.strip():
            fail(f"deb control line is malformed: {line!r}")
        fields[key] = value.strip()
        current = key
    expected_fields = {
        "Package": PACKAGE_NAME,
        "Version": f"{version}-1",
        "Architecture": "amd64",
        "Section": "database",
        "Priority": "optional",
        "Homepage": "https://github.com/workingdir/ornadb",
        "Depends": "libc6, libgcc-s1",
    }
    for key, value in expected_fields.items():
        if fields.get(key) != value:
            fail(f"deb {key} is {fields.get(key)!r}, not {value!r}")
    description = fields.get("Description", "").splitlines()
    if not description or not description[0].strip():
        fail("deb has no description summary")
    control = sorted(
        name.removeprefix("./") for name in _deb_tar_names(path, "--ctrl-tarfile")
    )
    if control != ["control", "md5sums"]:
        fail(f"deb control archive is {control}, not ['control', 'md5sums']")
    members = _deb_data_members(path)
    names: list[str] = []
    timestamps: list[int] = []
    for member, content in members:
        name = member.name.removeprefix("./")
        if name not in payload:
            fail(f"deb carries an undeclared payload file: {name}")
        expected, expected_mode = payload[name]
        if content != expected:
            fail(f"deb payload differs from the tar payload: {name}")
        if member.mode & 0o7777 != expected_mode:
            fail(f"deb payload mode differs from the tar payload: {name}")
        if member.uid != 0 or member.gid != 0:
            fail(f"deb payload is not root-owned: {name}")
        if member.uname != "root" or member.gname != "root":
            fail(f"deb payload is not named root:root: {name}")
        names.append(name)
        timestamps.append(member.mtime)
    if names != list(PREFIX_MEMBERS):
        fail(f"deb payload is {names}, not {list(PREFIX_MEMBERS)}")
    one_timestamp(timestamps, expected_epoch, label="deb payload")


def _deb_data_members(path: Path) -> list[tuple[tarfile.TarInfo, bytes]]:
    """The regular files of a deb data archive, with its directories checked."""
    try:
        data = subprocess.run(
            ["dpkg-deb", "--fsys-tarfile", str(path)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout
    except OSError as error:
        fail(f"cannot run dpkg-deb to read the deb data archive: {error}")
    except subprocess.CalledProcessError as error:
        fail(f"dpkg-deb could not read the data archive: {error.returncode}")
    try:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as archive:
            if archive.pax_headers:
                fail("deb data archive has unsupported PAX metadata")
            members: list[tuple[tarfile.TarInfo, bytes]] = []
            for member in archive.getmembers():
                name = member.name.removeprefix("./")
                if member.pax_headers:
                    fail(f"deb data member has unsupported PAX metadata: {name}")
                if member.isdir():
                    if member.mode & 0o7777 != 0o755:
                        fail(f"deb directory has the wrong mode: {name}")
                    if member.uid != 0 or member.gid != 0:
                        fail(f"deb directory is not root-owned: {name}")
                    continue
                if not member.isfile() or member.issym() or member.islnk():
                    fail(f"deb data member is not a regular file: {name}")
                extracted = archive.extractfile(member)
                if extracted is None:
                    fail(f"deb data member has no data: {name}")
                members.append((member, extracted.read()))
            return members
    except (OSError, tarfile.TarError) as error:
        fail(f"cannot read the deb data archive: {error}")


def _deb_tar_names(path: Path, option: str) -> list[str]:
    try:
        archive_bytes = subprocess.run(
            ["dpkg-deb", option, str(path)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout
    except OSError as error:
        fail(f"cannot run dpkg-deb to read the deb control archive: {error}")
    except subprocess.CalledProcessError as error:
        fail(f"dpkg-deb could not read the control archive: {error.returncode}")
    try:
        with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:") as archive:
            return archive.getnames()
    except (OSError, tarfile.TarError) as error:
        fail(f"cannot read the deb control archive: {error}")


def verify_packages(
    dist: Path,
    *,
    planned: dict[str, str],
    version: str,
    expected_epoch: int | None,
) -> list[tuple[str, str, int, str]]:
    """Verify every planned package in one output directory."""
    if not dist.is_dir():
        fail(f"package directory is absent: {dist}")
    declared = set(planned.values()) | {DIST_EPOCH_RECORD}
    foreign = sorted(entry.name for entry in dist.iterdir() if entry.name not in declared)
    if foreign:
        fail(f"package directory holds files the manifest does not declare: {foreign}")
    for name in planned.values():
        path = dist / name
        if path.is_symlink() or not path.is_file():
            fail(f"package {name} is absent from {dist}")
    members = verify_tar(dist / planned["tar"], expected_epoch=expected_epoch)
    payload = payload_of(members)
    verify_rpm(
        dist / planned["rpm"], payload, version=version, expected_epoch=expected_epoch
    )
    verify_deb(
        dist / planned["deb"], payload, version=version, expected_epoch=expected_epoch
    )
    result: list[tuple[str, str, int, str]] = []
    for format in PACKAGE_FORMATS:
        path = dist / planned[format]
        content, _ = read_stable_file(path)
        result.append((format, path.name, len(content), digest(content)))
    return result


# --------------------------------------------------------------------------
# Installation
# --------------------------------------------------------------------------


def require_install_owner(
    descriptor: int, path: Path, metadata: os.stat_result, *, created: bool
) -> os.stat_result:
    if os.geteuid() != 0 or (metadata.st_uid == 0 and metadata.st_gid == 0):
        return metadata
    if not created:
        fail(f"package install directory is not root-owned: {path}")
    try:
        os.fchown(descriptor, 0, 0)
        metadata = os.fstat(descriptor)
    except OSError as error:
        fail(f"cannot set package install directory owner {path}: {error}")
    if metadata.st_uid != 0 or metadata.st_gid != 0:
        fail(f"package install directory is not root-owned: {path}")
    return metadata


def open_directory_at(parent: int, part: str, path: Path, *, label: str) -> tuple[int, bool]:
    """Open one no-follow directory component, creating it only beneath parent."""
    created = False
    try:
        os.mkdir(part, mode=0o755, dir_fd=parent)
    except FileExistsError:
        pass
    except OSError as error:
        fail(f"cannot create {label} {path}: {error}")
    else:
        created = True
    try:
        descriptor = os.open(
            part, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW, dir_fd=parent
        )
    except OSError as error:
        fail(f"cannot open {label} {path}: {error}")
    return descriptor, created


def prepare_install_root(root: Path) -> tuple[Path, int]:
    """Open an install root component-by-component without following replacements."""
    normalized = Path(os.path.abspath(os.fspath(root)))
    current = Path(normalized.anchor)
    try:
        descriptor = os.open(current, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW)
        anchor_metadata = os.fstat(descriptor)
    except OSError as error:
        fail(f"cannot inspect package install root {current}: {error}")
    if not stat.S_ISDIR(anchor_metadata.st_mode):
        os.close(descriptor)
        fail(f"package install root is not a directory: {current}")
    try:
        require_install_owner(descriptor, current, anchor_metadata, created=False)
        for part in normalized.parts[1:]:
            current /= part
            next_descriptor, created = open_directory_at(
                descriptor, part, current, label="package install root"
            )
            os.close(descriptor)
            descriptor = next_descriptor
            metadata = os.fstat(descriptor)
            if not stat.S_ISDIR(metadata.st_mode):
                fail(f"package install root is not a directory: {current}")
            if created:
                os.fchmod(descriptor, 0o755)
            require_install_owner(descriptor, current, metadata, created=created)
        root_metadata = os.fstat(descriptor)
        if stat.S_IMODE(root_metadata.st_mode) != 0o755:
            fail(f"package install root has the wrong mode: {normalized}")
        return normalized, descriptor
    except OSError as error:
        os.close(descriptor)
        fail(f"cannot prepare package install root {normalized}: {error}")
    except PackageError:
        os.close(descriptor)
        raise


def ensure_install_parent(root: Path, root_descriptor: int, relative: PurePosixPath) -> tuple[int, str]:
    """Return a descriptor-backed parent and leaf for a product payload path."""
    descriptor = os.dup(root_descriptor)
    current = root
    try:
        for part in relative.parent.parts:
            current /= part
            next_descriptor, created = open_directory_at(
                descriptor, part, current, label="package destination parent"
            )
            os.close(descriptor)
            descriptor = next_descriptor
            metadata = os.fstat(descriptor)
            if not stat.S_ISDIR(metadata.st_mode):
                fail(f"package destination parent is not a directory: {current}")
            if created:
                os.fchmod(descriptor, 0o755)
            elif stat.S_IMODE(metadata.st_mode) != 0o755:
                fail(f"package destination parent has the wrong mode: {current}")
            require_install_owner(descriptor, current, metadata, created=created)
        return descriptor, relative.name
    except Exception:
        os.close(descriptor)
        raise


def atomic_write_at(parent: int, name: str, content: bytes, *, mode: int) -> None:
    temporary = f".{name}.orna-install"
    try:
        descriptor = os.open(
            temporary,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
            mode,
            dir_fd=parent,
        )
    except OSError as error:
        fail(f"cannot create package output {name}: {error}")
    try:
        view = memoryview(content)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                fail(f"could not write package output {name}")
            view = view[written:]
        os.fchmod(descriptor, mode)
        os.fsync(descriptor)
    except OSError as error:
        fail(f"could not write package output {name}: {error}")
    finally:
        os.close(descriptor)
    try:
        os.replace(temporary, name, src_dir_fd=parent, dst_dir_fd=parent)
    except OSError as error:
        try:
            os.unlink(temporary, dir_fd=parent)
        except OSError:
            pass
        fail(f"cannot install package output {name}: {error}")


def destination_is_regular(parent: int, name: str) -> bool:
    try:
        metadata = os.stat(name, dir_fd=parent, follow_symlinks=False)
    except FileNotFoundError:
        return True
    except OSError as error:
        fail(f"cannot inspect package destination {name}: {error}")
    return stat.S_ISREG(metadata.st_mode)


def read_installed_payload(root: int, relative: PurePosixPath) -> tuple[bytes, os.stat_result] | None:
    """Read one existing payload file without resolving an untrusted path."""
    descriptor = os.dup(root)
    try:
        for part in relative.parent.parts:
            try:
                next_descriptor = os.open(
                    part,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                    dir_fd=descriptor,
                )
            except FileNotFoundError:
                return None
            os.close(descriptor)
            descriptor = next_descriptor
        try:
            metadata = os.stat(relative.name, dir_fd=descriptor, follow_symlinks=False)
        except FileNotFoundError:
            return None
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            fail(f"installed package payload is not one regular file: {relative}")
        try:
            file_descriptor = os.open(
                relative.name,
                os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=descriptor,
            )
        except OSError as error:
            fail(f"cannot open installed package payload {relative}: {error}")
        try:
            opened = os.fstat(file_descriptor)
            content = bytearray()
            while block := os.read(file_descriptor, 1024 * 1024):
                content.extend(block)
            final = os.fstat(file_descriptor)
        finally:
            os.close(file_descriptor)
        if (
            opened.st_dev != metadata.st_dev
            or opened.st_ino != metadata.st_ino
            or opened.st_nlink != metadata.st_nlink
            or final.st_size != metadata.st_size
            or final.st_mtime_ns != opened.st_mtime_ns
            or final.st_ctime_ns != opened.st_ctime_ns
        ):
            fail(f"installed package payload changed while reading: {relative}")
        return bytes(content), final
    finally:
        os.close(descriptor)


def verify_replaceable_payload(root: int) -> None:
    """Refuse to overwrite a partial or foreign product installation."""
    present = {
        relative: read_installed_payload(root, PurePosixPath(relative))
        for relative in PREFIX_MEMBERS
    }
    if all(payload is None for payload in present.values()):
        return
    if any(payload is None for payload in present.values()):
        fail("package replacement found a partial installed payload")
    executable, executable_metadata = present[EXECUTABLE]  # type: ignore[misc]
    manifest, manifest_metadata = present[DISTRIBUTION_MANIFEST]  # type: ignore[misc]
    if (
        stat.S_IMODE(executable_metadata.st_mode) != 0o755
        or stat.S_IMODE(manifest_metadata.st_mode) != 0o644
        or not valid_linux_x86_64_elf(executable)
    ):
        fail("installed package payload has an invalid mode or executable")
    parse_distribution_manifest(manifest, executable_sha256=digest(executable))


def parent_is_current(root: int, relative: PurePosixPath, expected: int) -> bool:
    """Prove a retained destination descriptor is still reachable from root."""
    descriptor = os.dup(root)
    try:
        for part in relative.parent.parts:
            next_descriptor = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=descriptor,
            )
            os.close(descriptor)
            descriptor = next_descriptor
        observed = os.fstat(descriptor)
        retained = os.fstat(expected)
        return (observed.st_dev, observed.st_ino) == (retained.st_dev, retained.st_ino)
    except OSError:
        return False
    finally:
        os.close(descriptor)


def install_archive(path: Path, root: Path) -> None:
    """Install the tar artifact into a root, refusing an unsafe destination."""
    members = verify_tar(path, expected_epoch=None)
    root, root_descriptor = prepare_install_root(root)
    try:
        verify_replaceable_payload(root_descriptor)
        for member, content in members:
            relative = PurePosixPath(member.name)
            if (
                relative.is_absolute()
                or any(part in ("", ".", "..") for part in relative.parts)
                or member.name.startswith("-")
            ):
                fail(f"unsafe package archive path: {member.name}")
            parent, name = ensure_install_parent(root, root_descriptor, relative)
            try:
                if not destination_is_regular(parent, name):
                    fail(f"package destination is not a regular file: {root / relative}")
                if not parent_is_current(root_descriptor, relative, parent):
                    fail(f"package destination parent changed before installation: {root / relative}")
                atomic_write_at(parent, name, content, mode=member.mode & 0o7777)
                if not parent_is_current(root_descriptor, relative, parent):
                    fail(f"package destination parent changed during installation: {root / relative}")
                if os.geteuid() == 0:
                    os.chown(name, 0, 0, dir_fd=parent, follow_symlinks=False)
            finally:
                os.close(parent)
    finally:
        os.close(root_descriptor)


# --------------------------------------------------------------------------
# Commands
# --------------------------------------------------------------------------


def parse_epoch(value: str | None) -> int:
    if value is None:
        value = os.environ.get("SOURCE_DATE_EPOCH", "0")
    try:
        epoch = int(value, 10)
    except (TypeError, ValueError):
        fail("SOURCE_DATE_EPOCH must be an unsigned decimal")
    if epoch < 0 or epoch > 2**33 - 1:
        fail("SOURCE_DATE_EPOCH is outside the tar archive range")
    return epoch


def resolve_repository(value: str) -> Path:
    try:
        repository = Path(value).resolve(strict=True)
    except OSError as error:
        fail(f"repository root is absent: {value}: {error}")
    if not (repository / "Cargo.toml").is_file():
        fail(f"repository root has no Cargo.toml: {repository}")
    return repository


def report(packages: Iterable[tuple[str, str, int, str]], *, note: str) -> None:
    for format, name, size, checksum in packages:
        print(f"  {format:<4} {name:<31} {size:>8} bytes  sha256 {checksum}")
    print(note)


def make_packages(
    repository: Path,
    *,
    executable_path: Path | None,
    output_dir: Path | None,
    source_date_epoch: int,
) -> tuple[Path, str, list[tuple[str, str, int, str]]]:
    """Stage the prefix from one executable and build every package with pkgsmith."""
    require_linux_x86_64()
    version = cargo_product_version(repository)
    target = prepare_build_target(repository)
    prefix = target / "prefix"
    dist = output_dir if output_dir is not None else target / "dist"
    if executable_path is None:
        executable_path = build_executable(repository, target, source_date_epoch)
    executable, executable_stat = read_stable_file(executable_path)
    source_mode = stat.S_IMODE(executable_stat.st_mode)
    if source_mode & 0o111 == 0 or source_mode & 0o6000:
        fail("package executable must be a non-empty regular executable file")
    if not valid_linux_x86_64_elf(executable):
        fail("package executable is not a Linux x86_64 ELF")
    manifest = canonical_manifest(
        rustc=tool_version("rustc"),
        cargo=tool_version("cargo"),
        cargo_lock_sha256=cargo_lock_digest(repository),
        executable_sha256=digest(executable),
    )
    parse_distribution_manifest(manifest, executable_sha256=digest(executable))
    stage_prefix(prefix, executable, manifest)
    planned, readable_plan = plan_packages(
        prefix=prefix, version=version, source_date_epoch=source_date_epoch
    )
    if dist.exists():
        if dist.is_symlink() or not dist.is_dir():
            fail(f"package output is not a directory: {dist}")
        for entry in dist.iterdir():
            if entry.name not in planned.values() and entry.name != DIST_EPOCH_RECORD:
                fail(f"package output holds files the manifest does not declare: {entry}")
        for name in (*planned.values(), DIST_EPOCH_RECORD):
            (dist / name).unlink(missing_ok=True)
    else:
        try:
            dist.mkdir(mode=0o755, parents=True)
        except OSError as error:
            fail(f"cannot create package output {dist}: {error}")
    run_pkgsmith(
        [
            "build",
            "--quiet",
            "--output",
            str(dist),
            "--build-arg",
            f"VERSION={version}",
            str(PACKAGING_SCRIPT),
        ],
        prefix=prefix,
        source_date_epoch=source_date_epoch,
    )
    atomic_write(dist / DIST_EPOCH_RECORD, f"{source_date_epoch}\n".encode("utf-8"))
    for name in planned.values():
        try:
            os.chmod(dist / name, 0o644)
        except OSError as error:
            fail(f"cannot set the mode of package {name}: {error}")
    packages = verify_packages(
        dist, planned=planned, version=version, expected_epoch=source_date_epoch
    )
    return dist, readable_plan, packages


def build_command(args: argparse.Namespace) -> None:
    repository = resolve_repository(args.repository)
    epoch = parse_epoch(args.source_date_epoch)
    pkgsmith_path()  # fail before the release build when pkgsmith is missing
    executable_path = (
        input_path(args.executable, "package executable") if args.executable else None
    )
    output_dir = Path(args.output_dir).resolve() if args.output_dir else None
    dist, plan, packages = make_packages(
        repository,
        executable_path=executable_path,
        output_dir=output_dir,
        source_date_epoch=epoch,
    )
    print(plan.rstrip("\n"))
    print(f"packages in {dist}:")
    report(
        packages,
        note=f"  payload {list(PREFIX_MEMBERS)} on every format, no lifecycle scripts",
    )


def verify_command(args: argparse.Namespace) -> None:
    repository = resolve_repository(args.repository)
    version = cargo_product_version(repository)
    dist = Path(args.dist).resolve() if args.dist else package_target(repository) / "dist"
    if not dist.is_dir():
        fail(f"package directory is absent: {dist} (run `package.sh build` first)")
    if args.source_date_epoch is not None:
        epoch: int | None = parse_epoch(args.source_date_epoch)
    elif "SOURCE_DATE_EPOCH" in os.environ:
        epoch = parse_epoch(os.environ["SOURCE_DATE_EPOCH"])
    else:
        epoch = None
        record = dist / DIST_EPOCH_RECORD
        if record.is_file():
            content, _ = read_stable_file(record, mode=0o644)
            epoch = parse_epoch(content.decode("utf-8", "replace").strip())
    staged = package_target(repository) / "prefix"
    if staged.is_dir():
        if staged_members(staged) != sorted(PREFIX_MEMBERS):
            fail(f"staged prefix holds {staged_members(staged)}, not {list(PREFIX_MEMBERS)}")
        prefix = staged
        temporary = None
    else:
        prefix = Path(tempfile.mkdtemp(prefix="orna-prefix-"))
        temporary = prefix
    try:
        planned = planned_files(
            plan_rows(
                plan_output(
                    prefix=prefix, version=version, source_date_epoch=epoch or 0
                )
            )
        )
        packages = verify_packages(
            dist, planned=planned, version=version, expected_epoch=epoch
        )
    finally:
        if temporary is not None:
            shutil.rmtree(temporary, ignore_errors=True)
    stamp = "not checked" if epoch is None else str(epoch)
    print(f"verified {dist} (orna {version}, timestamps {stamp})")
    report(
        packages,
        note=f"  payload {list(PREFIX_MEMBERS)} on every format, no lifecycle scripts",
    )


def install_command(args: argparse.Namespace) -> None:
    install_archive(Path(args.archive).resolve(strict=True), Path(args.root))
    print("installed")


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(description=__doc__)
    subcommands = command.add_subparsers(dest="command", required=True)
    build = subcommands.add_parser("build", help="build the executable and every package")
    build.add_argument("--repository", default=Path(__file__).resolve().parents[2])
    build.add_argument(
        "--executable",
        help="explicit Linux x86_64 executable input instead of a reproducible build",
    )
    build.add_argument("--output-dir")
    build.add_argument("--source-date-epoch")
    verify = subcommands.add_parser("verify", help="verify a directory of built packages")
    verify.add_argument("dist", nargs="?")
    verify.add_argument("--repository", default=Path(__file__).resolve().parents[2])
    verify.add_argument("--source-date-epoch")
    install = subcommands.add_parser("install", help="install the tar artifact into a root")
    install.add_argument("archive")
    install.add_argument("--root", default="/")
    return command


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        if arguments.command == "build":
            build_command(arguments)
        elif arguments.command == "verify":
            verify_command(arguments)
        elif arguments.command == "install":
            install_command(arguments)
        else:
            fail("unknown package command")
    except (OSError, PackageError, ValueError) as error:
        print(f"[linux-package] error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
