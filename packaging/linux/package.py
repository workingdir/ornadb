#!/usr/bin/env python3
"""Build and verify the smallest installed Orna Linux artifact.

The artifact is a deterministic root-relative tar archive containing exactly
one executable and the two provenance manifests required by the installed
command boundary.  It deliberately has no package-manager lifecycle hooks,
network access, or runtime helper executable.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import shutil
from pathlib import Path, PurePosixPath
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from typing import Iterable, NoReturn


ARCHIVE_EXECUTABLE = "usr/bin/orna"
ARCHIVE_DISTRIBUTION_MANIFEST = "usr/share/orna/distribution-manifest.toml"
ARCHIVE_ENGINE_MANIFEST = "usr/share/orna/embedded-engine-manifest.json"
ARCHIVE_MEMBERS = (
    ARCHIVE_EXECUTABLE,
    ARCHIVE_DISTRIBUTION_MANIFEST,
    ARCHIVE_ENGINE_MANIFEST,
)
RUST_PATH_REMAP = "<repository>=/usr/src/orna"
RUST_LINK_FLAGS = "-C link-arg=-Wl,--build-id=none"
MANIFEST_LIMIT = 16 * 1024
TARGET_TRIPLE = "x86_64-unknown-linux-gnu"


class PackageError(RuntimeError):
    """An input, archive, or manifest failed the package contract."""


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
        len(content) >= 20
        and content[:4] == b"\x7fELF"
        and content[4] == 2
        and content[5] == 1
        and int.from_bytes(content[18:20], "little") == 62
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
    """Reject ambient compiler selection and flag overrides before Cargo runs."""
    exact_names = {
        "RUSTC",
        "RUSTFLAGS",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
    }
    rejected: list[str] = []
    for name, value in os.environ.items():
        if not value:
            continue
        target_override = name.startswith("CARGO_TARGET_") and name.endswith(
            ("_RUSTC", "_RUSTC_WRAPPER", "_RUSTC_WORKSPACE_WRAPPER", "_RUSTFLAGS")
        )
        if (
            name in exact_names
            or name.startswith("RUSTC_")
            or name in {"RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
            or name.startswith("CARGO_BUILD_RUSTC_")
            or target_override
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


def builder_image_digest(engine: bytes) -> str:
    try:
        document = json.loads(engine)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"embedded engine manifest is not valid JSON: {error}")
    if not isinstance(document, dict) or document.get("format") != 1:
        fail("embedded engine manifest format is not 1")
    builder = document.get("builder")
    if not isinstance(builder, dict):
        fail("embedded engine manifest builder evidence is absent")
    image_digest = builder.get("image_digest")
    if not valid_digest(image_digest, prefix="sha256:"):
        fail("embedded engine builder image digest is invalid")
    return image_digest


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


def parse_distribution_manifest(
    content: bytes, *, engine_sha256: str, executable_sha256: str
) -> None:
    if len(content) > MANIFEST_LIMIT:
        fail("distribution manifest is too large")
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        fail(f"distribution manifest is not UTF-8: {error}")
    if not text.endswith("\n"):
        fail("distribution manifest has no final newline")
    lines = text[:-1].split("\n")
    if len(lines) != 11 or lines[0] != "format = 1":
        fail("distribution manifest shape is not format 1")
    rustc = parse_quoted(lines[1], "rustc = ")
    cargo = parse_quoted(lines[2], "cargo = ")
    builder = parse_quoted(lines[3], "builder_image = ")
    lock = parse_quoted(lines[4], "cargo_lock_sha256 = ")
    embedded = parse_quoted(lines[7], "embedded_engine_manifest_sha256 = ")
    executable = parse_quoted(lines[10], "executable_sha256 = ")
    if (
        rustc is None
        or not rustc.startswith("rustc ")
        or cargo is None
        or not cargo.startswith("cargo ")
        or builder is None
        or not valid_digest(builder, prefix="sha256:")
        or lock is None
        or not valid_digest(lock)
        or lines[5] != f'rust_path_remap = "{RUST_PATH_REMAP}"'
        or lines[6] != f'rust_link_flags = "{RUST_LINK_FLAGS}"'
        or embedded != engine_sha256
        or lines[8] != "accepted_predecessor_engines = []"
        or lines[9] != "supported_forward_edges = []"
        or executable != executable_sha256
    ):
        fail("distribution manifest evidence does not bind package inputs")


def canonical_manifest(
    *,
    rustc: str,
    cargo: str,
    builder_image: str,
    cargo_lock_sha256: str,
    engine_sha256: str,
    executable_sha256: str,
) -> bytes:
    lines = [
        "format = 1",
        f"rustc = {quote_toml_string(rustc)}",
        f"cargo = {quote_toml_string(cargo)}",
        f"builder_image = {quote_toml_string(builder_image)}",
        f"cargo_lock_sha256 = {quote_toml_string(cargo_lock_sha256)}",
        f"rust_path_remap = {quote_toml_string(RUST_PATH_REMAP)}",
        f"rust_link_flags = {quote_toml_string(RUST_LINK_FLAGS)}",
        f"embedded_engine_manifest_sha256 = {quote_toml_string(engine_sha256)}",
        "accepted_predecessor_engines = []",
        "supported_forward_edges = []",
        f"executable_sha256 = {quote_toml_string(executable_sha256)}",
    ]
    return ("\n".join(lines) + "\n").encode("utf-8")


def source_engine_path(
    repository: Path, explicit: str | None, *, search_root: Path | None = None
) -> Path:
    candidates: list[Path] = []
    if explicit:
        candidates.append(input_path(explicit, "embedded engine manifest"))
    elif os.environ.get("ORNA_POSTGRES_ENGINE_OUTPUT"):
        candidates.append(
            input_path(
                str(Path(os.environ["ORNA_POSTGRES_ENGINE_OUTPUT"]) / "embedded-engine-manifest.json"),
                "embedded engine manifest",
            )
        )
    else:
        target = search_root or (repository / "target")
        if target.is_dir():
            candidates = sorted(
                path
                for path in target.rglob("embedded-engine-manifest.json")
                if path.is_file() and not path.is_symlink()
            )
    if len(candidates) != 1:
        fail("package requires exactly one embedded-engine-manifest.json input")
    return input_path(str(candidates[0]), "embedded engine manifest")


def prepare_build_target(repository: Path) -> Path:
    """Reset the package-owned Cargo target to avoid stale provenance inputs."""
    target = repository / "target" / "linux-package"
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


def build_executable(repository: Path, source_date_epoch: int) -> Path:
    reject_compiler_overrides()
    target = prepare_build_target(repository)
    environment = os.environ.copy()
    environment.update(
        {
            "CARGO_TARGET_DIR": str(target),
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
        "orna-server",
        "--bin",
        "orna",
    ]
    try:
        subprocess.run(command, cwd=repository, env=environment, check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        fail(f"the reproducible Orna build failed: {error}")
    executable = target / "release" / "orna"
    if not executable.is_file() or executable.is_symlink():
        fail(f"the reproducible build did not produce {executable}")
    return executable


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


def write_archive(path: Path, members: Iterable[tuple[str, bytes, int]], epoch: int) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="wb", dir=path.parent, prefix=f".{path.name}.", delete=False
    ) as temporary:
        temporary_path = Path(temporary.name)
    try:
        with temporary_path.open("wb") as stream, tarfile.open(
            fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT
        ) as archive:
            for name, content, mode in members:
                info = tarfile.TarInfo(name)
                info.size = len(content)
                info.mode = mode
                info.mtime = epoch
                info.uid = 0
                info.gid = 0
                info.uname = "root"
                info.gname = "root"
                archive.addfile(info, io.BytesIO(content))
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary_path, 0o644)
        os.replace(temporary_path, path)
    except (OSError, tarfile.TarError) as error:
        temporary_path.unlink(missing_ok=True)
        fail(f"could not write package archive {path}: {error}")


def archive_members(path: Path) -> list[tuple[tarfile.TarInfo, bytes]]:
    try:
        with tarfile.open(path, mode="r:") as archive:
            members = archive.getmembers()
            result: list[tuple[tarfile.TarInfo, bytes]] = []
            for member in members:
                if not member.isfile() or member.issym() or member.islnk():
                    fail(f"package archive member is not a regular file: {member.name}")
                extracted = archive.extractfile(member)
                if extracted is None:
                    fail(f"package archive member has no data: {member.name}")
                content = extracted.read()
                if len(content) != member.size:
                    fail(f"package archive member length changed: {member.name}")
                result.append((member, content))
            return result
    except (OSError, tarfile.TarError) as error:
        fail(f"cannot read package archive {path}: {error}")


def verify_archive(
    path: Path, *, expected_epoch: int | None = None
) -> list[tuple[tarfile.TarInfo, bytes]]:
    require_linux_x86_64()
    members = archive_members(path)
    names = [member.name for member, _ in members]
    if names != list(ARCHIVE_MEMBERS) or len(set(names)) != len(names):
        fail("package archive inventory is not exactly one sorted product payload")
    by_name = {member.name: (member, content) for member, content in members}
    for name, expected_mode in (
        (ARCHIVE_EXECUTABLE, 0o755),
        (ARCHIVE_DISTRIBUTION_MANIFEST, 0o644),
        (ARCHIVE_ENGINE_MANIFEST, 0o644),
    ):
        member, _ = by_name[name]
        if member.uid != 0 or member.gid != 0:
            fail(f"package archive member is not root-owned: {name}")
        if member.mode & 0o7777 != expected_mode:
            fail(f"package archive member has the wrong mode: {name}")
        if expected_epoch is not None and member.mtime != expected_epoch:
            fail(f"package archive member has the wrong timestamp: {name}")
    executable = by_name[ARCHIVE_EXECUTABLE][1]
    engine = by_name[ARCHIVE_ENGINE_MANIFEST][1]
    manifest = by_name[ARCHIVE_DISTRIBUTION_MANIFEST][1]
    if not executable:
        fail("packaged executable is empty")
    parse_distribution_manifest(
        manifest, engine_sha256=digest(engine), executable_sha256=digest(executable)
    )
    if not valid_linux_x86_64_elf(executable):
        fail("packaged executable is not a Linux x86_64 ELF")
    return members


def require_install_owner(
    path: Path, metadata: os.stat_result, *, created: bool
) -> None:
    if os.geteuid() != 0 or (metadata.st_uid == 0 and metadata.st_gid == 0):
        return
    if not created:
        fail(f"package install directory is not root-owned: {path}")
    try:
        os.chown(path, 0, 0, follow_symlinks=False)
        metadata = path.lstat()
    except OSError as error:
        fail(f"cannot set package install directory owner {path}: {error}")
    if metadata.st_uid != 0 or metadata.st_gid != 0:
        fail(f"package install directory is not root-owned: {path}")


def prepare_install_root(root: Path) -> Path:
    """Normalize an install root lexically and reject symlinked ancestors."""
    normalized = Path(os.path.abspath(os.fspath(root)))
    current = Path(normalized.anchor)
    try:
        anchor_metadata = current.lstat()
    except OSError as error:
        fail(f"cannot inspect package install root {current}: {error}")
    if not stat.S_ISDIR(anchor_metadata.st_mode):
        fail(f"package install root is not a directory: {current}")
    require_install_owner(current, anchor_metadata, created=False)
    for part in normalized.parts[1:]:
        current /= part
        created = False
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            try:
                current.mkdir(mode=0o755)
            except FileExistsError:
                pass
            else:
                created = True
            try:
                metadata = current.lstat()
            except OSError as error:
                fail(f"cannot inspect package install root {current}: {error}")
        except OSError as error:
            fail(f"cannot inspect package install root {current}: {error}")
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            fail(f"package install root is not a directory: {current}")
        if created:
            try:
                os.chmod(current, 0o755)
            except OSError as error:
                fail(f"cannot set package install root mode {current}: {error}")
        require_install_owner(current, metadata, created=created)


    return normalized

def ensure_install_parent(root: Path, relative: PurePosixPath) -> Path:
    current = root
    for part in relative.parent.parts:
        current /= part
        created = False
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            try:
                current.mkdir(mode=0o755)
            except FileExistsError:
                pass
            else:
                created = True
            try:
                metadata = current.lstat()
            except OSError as error:
                fail(f"cannot inspect package destination parent {current}: {error}")
        except OSError as error:
            fail(f"cannot inspect package destination parent {current}: {error}")
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            fail(f"package destination parent is not a directory: {current}")
        if created:
            try:
                os.chmod(current, 0o755)
            except OSError as error:
                fail(f"cannot set package destination parent mode {current}: {error}")
        elif stat.S_IMODE(metadata.st_mode) != 0o755:
            fail(f"package destination parent has the wrong mode: {current}")
        require_install_owner(current, metadata, created=created)
    return root.joinpath(*relative.parts)


def install_archive(path: Path, root: Path) -> None:
    members = verify_archive(path)
    root = prepare_install_root(root)
    for member, content in members:
        relative = PurePosixPath(member.name)
        if (
            relative.is_absolute()
            or any(part in ("", ".", "..") for part in relative.parts)
            or member.name.startswith("-")
        ):
            fail(f"unsafe package archive path: {member.name}")
        destination = ensure_install_parent(root, relative)
        if destination.is_symlink() or (destination.exists() and not destination.is_file()):
            fail(f"package destination is not a regular file: {destination}")
        temporary = destination.with_name(f".{destination.name}.orna-install")
        atomic_write(temporary, content, mode=member.mode & 0o7777)
        os.replace(temporary, destination)
        os.chmod(destination, member.mode & 0o7777)
        if os.geteuid() == 0:
            os.chown(destination, 0, 0)


def make_archive(args: argparse.Namespace) -> Path:
    require_linux_x86_64()
    repository = Path(args.repository).resolve(strict=True)
    if not (repository / "Cargo.toml").is_file():
        fail("repository root has no Cargo.toml")
    epoch_value = args.source_date_epoch
    if epoch_value is None:
        epoch_value = os.environ.get("SOURCE_DATE_EPOCH", "0")
    try:
        epoch = int(epoch_value, 10)
    except (TypeError, ValueError):
        fail("SOURCE_DATE_EPOCH must be an unsigned decimal")
    if epoch < 0 or epoch > 2**33 - 1:
        fail("SOURCE_DATE_EPOCH is outside the tar archive range")
    if bool(args.executable) != bool(args.engine_manifest):
        fail("--executable and --engine-manifest must be supplied together")
    built_target: Path | None = None
    if args.executable:
        executable_path = input_path(args.executable, "package executable")
    else:
        executable_path = build_executable(repository, epoch)
        built_target = repository / "target" / "linux-package"
    executable, executable_stat = read_stable_file(executable_path)
    source_mode = stat.S_IMODE(executable_stat.st_mode)
    if source_mode & 0o111 == 0 or source_mode & 0o6000:
        fail("package executable must be a non-empty regular executable file")
    if not valid_linux_x86_64_elf(executable):
        fail("package executable is not a Linux x86_64 ELF")
    engine_path = source_engine_path(
        repository, args.engine_manifest, search_root=built_target
    )
    engine, _ = read_stable_file(engine_path, mode=0o644)
    builder_image = builder_image_digest(engine)
    product_version = cargo_product_version(repository)
    if not product_version:
        fail("workspace product version is empty")
    manifest = canonical_manifest(
        rustc=tool_version("rustc"),
        cargo=tool_version("cargo"),
        builder_image=builder_image,
        cargo_lock_sha256=cargo_lock_digest(repository),
        engine_sha256=digest(engine),
        executable_sha256=digest(executable),
    )
    parse_distribution_manifest(
        manifest, engine_sha256=digest(engine), executable_sha256=digest(executable)
    )
    output = Path(args.output).resolve() if args.output else repository / "target" / f"orna-{product_version}-linux-amd64.tar"
    write_archive(
        output,
        [
            (ARCHIVE_EXECUTABLE, executable, 0o755),
            (ARCHIVE_DISTRIBUTION_MANIFEST, manifest, 0o644),
            (ARCHIVE_ENGINE_MANIFEST, engine, 0o644),
        ],
        epoch,
    )
    verify_archive(output, expected_epoch=epoch)
    print(output)
    return output


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(description=__doc__)
    subcommands = command.add_subparsers(dest="command", required=True)
    build = subcommands.add_parser("build", help="build a deterministic Linux archive")
    build.add_argument("--repository", default=Path(__file__).resolve().parents[2])
    build.add_argument(
        "--executable",
        help="explicit executable input; pair it with an engine manifest from the same build",
    )
    build.add_argument(
        "--engine-manifest",
        help="explicit engine input; pair it with an executable from the same build",
    )
    build.add_argument("--output")
    build.add_argument("--source-date-epoch")
    verify = subcommands.add_parser("verify", help="verify an existing Linux archive")
    verify.add_argument("archive")
    verify.add_argument("--source-date-epoch")
    install = subcommands.add_parser("install", help="install an archive into a root")
    install.add_argument("archive")
    install.add_argument("--root", default="/")
    return command


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        if arguments.command == "build":
            make_archive(arguments)
        elif arguments.command == "verify":
            epoch = None
            if arguments.source_date_epoch is not None:
                epoch = int(arguments.source_date_epoch, 10)
            verify_archive(Path(arguments.archive).resolve(strict=True), expected_epoch=epoch)
            print("verified")
        elif arguments.command == "install":
            install_archive(Path(arguments.archive).resolve(strict=True), Path(arguments.root))
            print("installed")
        else:
            fail("unknown package command")
    except (OSError, PackageError, ValueError) as error:
        print(f"[linux-package] error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
