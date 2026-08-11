#!/bin/bash
set -euo pipefail

export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

readonly SCRIPT_PATH="${BASH_SOURCE[0]}"
readonly SCRIPT_DIRECTORY="$(cd "$(dirname "${SCRIPT_PATH}")" && pwd -P)"
readonly REPOSITORY_ROOT="$(cd "${SCRIPT_DIRECTORY}/../.." && pwd -P)"
readonly RECIPE_PATH="${SCRIPT_DIRECTORY}/embedded-build.toml"

declare -a EMBEDDED_BUILD_CLEANUP_PATHS=()
EMBEDDED_BUILD_PUBLICATION_IN_PROGRESS=0
EMBEDDED_BUILD_PUBLICATION_PREVIOUS=""
EMBEDDED_BUILD_PUBLICATION_RESULT=""

log() {
    printf '[postgres-embedded] %s\n' "$*"
}

fail() {
    printf '[postgres-embedded] error: %s\n' "$*" >&2
    exit 1
}

cleanup_embedded_build_paths() {
    local path

    if [[ "${EMBEDDED_BUILD_PUBLICATION_IN_PROGRESS}" == 1 \
        && -n "${EMBEDDED_BUILD_PUBLICATION_PREVIOUS}" \
        && -d "${EMBEDDED_BUILD_PUBLICATION_PREVIOUS}" ]]; then
        if [[ -e "${EMBEDDED_BUILD_PUBLICATION_RESULT}" ]]; then
            mv -- "${EMBEDDED_BUILD_PUBLICATION_RESULT}" \
                "${EMBEDDED_BUILD_PUBLICATION_PREVIOUS}.interrupted"
        fi
        mv -- "${EMBEDDED_BUILD_PUBLICATION_PREVIOUS}" \
            "${EMBEDDED_BUILD_PUBLICATION_RESULT}"
    fi
    for path in "${EMBEDDED_BUILD_CLEANUP_PATHS[@]}"; do
        if [[ -d "${path}" ]]; then
            rm -rf -- "${path}"
        fi
    done
}

emit_recipe_environment() {
    local recipe_path="${1:-${RECIPE_PATH}}"

    python3 - "${recipe_path}" <<'PY_RECIPE'
import hashlib
import pathlib
import re
import stat
import sys
import tomllib

recipe_path = pathlib.Path(sys.argv[1])
with recipe_path.open("rb") as recipe_file:
    recipe = tomllib.load(recipe_file)

expected_top_level = {
    "format", "identity", "target", "platform", "source_date_epoch",
    "builder_image", "snapshot_timestamp", "patches",
    "postgresql", "postgresql_license", "zlib", "build", "static_archive",
    "initializer_archive", "lifecycle", "sql_guard", "resources", "output",
    "proof", "apt",
}
if set(recipe) != expected_top_level:
    raise SystemExit("embedded recipe has unexpected top-level keys")
if recipe["format"] != 1:
    raise SystemExit("embedded recipe format must be 1")
if recipe["identity"] != "postgresql-18.4-debian12-amd64-orna-embedded.1":
    raise SystemExit("embedded recipe identity is not accepted")
if recipe["target"] != "debian12-amd64" or recipe["platform"] != "linux/amd64":
    raise SystemExit("embedded recipe target must be Debian 12 amd64")
if recipe["source_date_epoch"] != 1778528675:
    raise SystemExit("embedded recipe source date epoch is not accepted")
accepted_builder_image = (
    "docker.io/library/debian@"
    "sha256:a1363ada3b45cb3ebc74c78943558f8b0c2b59aaa194d8224e1b02cfd5d78583"
)
if recipe["builder_image"] != accepted_builder_image:
    raise SystemExit("embedded recipe builder image is not accepted")
if recipe["snapshot_timestamp"] != "20251229T000000Z":
    raise SystemExit("embedded recipe snapshot timestamp is not accepted")

patches = recipe["patches"]
if not isinstance(patches, list) or not patches:
    raise SystemExit("embedded recipe patch series must be a non-empty ordered list")

recipe_directory = recipe_path.parent
casefold_paths = set()
patch_root_relative = None
for patch in patches:
    if not isinstance(patch, dict) or set(patch) != {"path", "sha256"}:
        raise SystemExit("embedded recipe patch record keys are not accepted")
    relative_path = patch["path"]
    expected_sha256 = patch["sha256"]
    if not isinstance(relative_path, str) or not isinstance(expected_sha256, str):
        raise SystemExit("embedded recipe patch record values are not strings")
    if len(expected_sha256) != 64 or any(
        character not in "0123456789abcdef" for character in expected_sha256
    ):
        raise SystemExit("embedded recipe patch digest is not a lowercase SHA-256 digest")
    if "\\" in relative_path:
        raise SystemExit("embedded recipe patch path contains a backslash")
    pure_path = pathlib.PurePosixPath(relative_path)
    if (
        pure_path.is_absolute()
        or pure_path.as_posix() != relative_path
        or any(part in {"", ".", ".."} for part in pure_path.parts)
    ):
        raise SystemExit("embedded recipe patch path is not a normal relative path")
    if pure_path.parent == pathlib.PurePosixPath("."):
        raise SystemExit("embedded recipe patch path has no series directory")
    if any(
        re.fullmatch(r"[a-z0-9][a-z0-9.-]*", part) is None
        for part in pure_path.parts[:-1]
    ):
        raise SystemExit("embedded recipe patch directory name is not accepted")
    if patch_root_relative is None:
        patch_root_relative = pure_path.parent
    elif pure_path.parent != patch_root_relative:
        raise SystemExit("embedded recipe patch path is outside the accepted series directory")
    match = re.fullmatch(r"([0-9]{4})-[a-z0-9][a-z0-9-]*\.patch", pure_path.name)
    if match is None:
        raise SystemExit("embedded recipe patch filename is not accepted")
    casefold_path = relative_path.casefold()
    if casefold_path in casefold_paths:
        raise SystemExit("embedded recipe patch series repeats a path")
    casefold_paths.add(casefold_path)
    patch_path = recipe_directory.joinpath(*pure_path.parts)
    cursor = recipe_directory
    for part in pure_path.parts:
        cursor /= part
        if cursor.is_symlink():
            raise SystemExit("embedded recipe patch path contains a symbolic link")
    try:
        patch_stat = patch_path.stat()
    except FileNotFoundError as error:
        raise SystemExit("embedded recipe patch file is absent") from error
    if not stat.S_ISREG(patch_stat.st_mode):
        raise SystemExit("embedded recipe patch path is not a regular file")
    if stat.S_IMODE(patch_stat.st_mode) != 0o644:
        raise SystemExit("embedded recipe patch mode must be 0644")
    actual_patch_sha256 = hashlib.sha256(patch_path.read_bytes()).hexdigest()
    if actual_patch_sha256 != expected_sha256:
        raise SystemExit("embedded PostgreSQL patch digest does not match the recipe")

patch_root = recipe_directory.joinpath(*patch_root_relative.parts)
if patch_root.is_symlink() or not patch_root.is_dir():
    raise SystemExit("embedded recipe patch series directory is not a directory")
expected_patch_inventory = sorted(patch["path"] for patch in patches)
actual_patch_inventory = []
for path in patch_root.rglob("*.patch"):
    if path.is_symlink() or not path.is_file():
        raise SystemExit("embedded recipe patch series contains a non-regular patch file")
    actual_patch_inventory.append(
        f"{patch_root_relative.as_posix()}/{path.relative_to(patch_root).as_posix()}"
    )
actual_patch_inventory.sort()
if actual_patch_inventory != expected_patch_inventory:
    raise SystemExit("embedded recipe patch series inventory is not accepted")

postgresql = recipe["postgresql"]
expected_postgresql = {"version", "url", "sha256", "configure_flags"}
if set(postgresql) != expected_postgresql:
    raise SystemExit("embedded PostgreSQL recipe keys are not accepted")
if postgresql["version"] != "18.4":
    raise SystemExit("embedded PostgreSQL version is not accepted")
if postgresql["url"] != "https://ftp.postgresql.org/pub/source/v18.4/postgresql-18.4.tar.bz2":
    raise SystemExit("embedded PostgreSQL URL is not accepted")
if postgresql["sha256"] != "81a81ec695fb0c7901407defaa1d2f7973617154cf27ba74e3a7ab8e64436094":
    raise SystemExit("embedded PostgreSQL digest is not accepted")
required_flags = {
    "--build=x86_64-linux-gnu", "--host=x86_64-linux-gnu", "--disable-nls",
    "--disable-rpath", "--disable-debug", "--disable-profiling", "--disable-coverage",
    "--disable-dtrace", "--disable-tap-tests", "--disable-injection-points",
    "--disable-cassert", "--without-tcl", "--without-perl", "--without-python",
    "--without-gssapi", "--without-pam", "--without-bsd-auth", "--without-ldap",
    "--without-bonjour", "--without-selinux", "--without-systemd", "--without-readline",
    "--without-liburing", "--without-libcurl", "--without-libnuma", "--without-libxml",
    "--without-libxslt", "--without-lz4", "--without-zstd", "--without-icu",
    "--without-llvm", "--with-zlib",
}
if set(postgresql["configure_flags"]) != required_flags:
    raise SystemExit("embedded PostgreSQL configure flags are not accepted")

postgresql_license = recipe["postgresql_license"]
expected_postgresql_license = {
    "source_path": "COPYRIGHT",
    "output_path": "POSTGRESQL-LICENSE",
    "sha256": "3d6af92ff8a4c2cdf69afb1cf44edea727922f5cd0cf8b5f72b11cdecac8fdfd",
}
if postgresql_license != expected_postgresql_license:
    raise SystemExit("embedded PostgreSQL licence facts are not accepted")

zlib = recipe["zlib"]
expected_zlib = {
    "version": "1.3.2",
    "url": "https://zlib.net/zlib-1.3.2.tar.gz",
    "sha256": "bb329a0a2cd0274d05519d61c667c062e06990d72e125ee2dfa8de64f0119d16",
    "build_prefix": "/build/zlib",
}
if zlib != expected_zlib:
    raise SystemExit("embedded zlib recipe is not accepted")

build = recipe["build"]
if set(build) != {"jobs", "environment"} or build["jobs"] != 1:
    raise SystemExit("embedded build settings are not accepted")
environment = build["environment"]
expected_environment = {
    "AR": "ar",
    "ARFLAGS": "rcD",
    "CC": "gcc",
    "CFLAGS": "-O2 -g0 -fPIC -ffile-prefix-map=/build=. -fdebug-prefix-map=/build=. -Wdate-time",
    "CONFIG_SITE": "/dev/null",
    "CPPFLAGS": "-I/build/zlib/include",
    "LANG": "C.UTF-8",
    "LC_ALL": "C.UTF-8",
    "LDFLAGS": "-L/build/zlib/lib -Wl,--build-id=none",
    "PKG_CONFIG_LIBDIR": "/nonexistent",
    "RANLIB": "ranlib",
    "TMPDIR": "/build/tmp",
    "TZ": "UTC0",
}
if environment != expected_environment:
    raise SystemExit("embedded deterministic environment is not accepted")

static_archive = recipe["static_archive"]
expected_static_archive = {
    "path": "liborna_postgres18_backend.a",
    "member": "liborna_postgres18_backend.o",
    "entry_symbol": "orna_postgres18_entry",
    "forbidden_symbols": ["main"],
    "flattened_inputs": [
        "src/backend object closure excluding src/backend/main/main.o",
        "src/common/libpgcommon_srv.a",
        "src/port/libpgport_srv.a",
        "/build/zlib/lib/libz.a",
    ],
}
if static_archive != expected_static_archive:
    raise SystemExit("embedded static archive facts are not accepted")

initializer_archive = recipe["initializer_archive"]
expected_initializer_archive = {
    "path": "liborna_postgres18_initdb.a",
    "member": "liborna_postgres18_initdb.o",
    "entry_symbol": "orna_postgres18_initdb_entry",
    "symbol_prefix": "orna_postgres18_initdb_",
    "rename_map_path": "initdb-redefine-symbols.txt",
    "defined_symbols_path": "initdb-defined-symbols.txt",
    "undefined_symbols_path": "initdb-undefined-symbols.txt",
    "allowed_undefined_bridge_symbols": [
        "orna_postgres18_entry",
        "orna_postgres18_install_exec_filter",
        "orna_postgres18_set_system_functions_initialisation_capability",
        "orna_postgres18_set_initialisation_child_capability",
        "orna_postgres18_support_root",
    ],
}
if initializer_archive != expected_initializer_archive:
    raise SystemExit("embedded initialiser archive facts are not accepted")

lifecycle = recipe["lifecycle"]
expected_lifecycle = {
    "backend_entry_symbol": "orna_postgres18_entry",
    "initializer_entry_symbol": "orna_postgres18_initdb_entry",
    "support_root_setter_symbol": "orna_postgres18_set_support_root",
    "system_functions_capability_setter_symbol": "orna_postgres18_set_system_functions_initialisation_capability",
    "initialisation_child_capability_setter_symbol": "orna_postgres18_set_initialisation_child_capability",
    "seccomp_entry_symbol": "orna_postgres18_install_exec_filter",
}
if lifecycle != expected_lifecycle:
    raise SystemExit("embedded lifecycle entry facts are not accepted")

sql_guard = recipe["sql_guard"]
expected_sql_guard = {
    "sqlstate": "0A000",
    "messages": [
        "Orna does not permit SQL LOAD",
        "Orna does not permit COPY PROGRAM",
        "Orna does not permit PostgreSQL extension management",
        "Orna does not permit procedural language creation",
        "Orna does not permit anonymous procedural blocks",
        "Orna does not permit C or internal language function or procedure definitions",
        "Orna does not permit PostgreSQL dynamic loading",
    ],
}
if sql_guard != expected_sql_guard:
    raise SystemExit("embedded SQL guard facts are not accepted")

resources = recipe["resources"]
if set(resources) != {
    "bundle_path", "bundle_sha256", "manifest_path", "manifest_sha256",
    "generation_rules",
}:
    raise SystemExit("embedded support resource keys are not accepted")
if resources["bundle_path"] != "embedded-postgresql-support.tar":
    raise SystemExit("embedded support bundle path is not accepted")
if resources["manifest_path"] != "embedded-postgresql-support-manifest.json":
    raise SystemExit("embedded support manifest path is not accepted")
for digest_name in ("bundle_sha256", "manifest_sha256"):
    digest = resources[digest_name]
    if not isinstance(digest, str) or len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        raise SystemExit(f"embedded support {digest_name} is not a lowercase SHA-256 digest")
expected_generation_rules = [
    {
        "name": "top_level",
        "source_paths": [
            "src/include/catalog/postgres.bki",
            "src/backend/libpq/pg_hba.conf.sample",
            "src/backend/libpq/pg_ident.conf.sample",
            "src/backend/utils/misc/postgresql.conf.sample",
            "src/backend/snowball/snowball_create.sql",
            "src/backend/catalog/information_schema.sql",
            "src/backend/catalog/sql_features.txt",
            "src/include/catalog/system_constraints.sql",
            "src/backend/catalog/system_functions.sql",
            "src/backend/catalog/system_views.sql",
        ],
        "output_prefix": "",
        "mode": "0600",
    },
    {
        "name": "timezone_tree",
        "source_paths": ["src/timezone/data/tzdata.zi"],
        "output_prefix": "timezone",
        "mode": "0600",
    },
    {
        "name": "timezonesets",
        "source_paths": [
            "src/timezone/tznames/Africa.txt",
            "src/timezone/tznames/America.txt",
            "src/timezone/tznames/Antarctica.txt",
            "src/timezone/tznames/Asia.txt",
            "src/timezone/tznames/Atlantic.txt",
            "src/timezone/tznames/Australia",
            "src/timezone/tznames/Australia.txt",
            "src/timezone/tznames/Default",
            "src/timezone/tznames/Etc.txt",
            "src/timezone/tznames/Europe.txt",
            "src/timezone/tznames/India",
            "src/timezone/tznames/Indian.txt",
            "src/timezone/tznames/Pacific.txt",
        ],
        "output_prefix": "timezonesets",
        "mode": "0600",
    },
    {
        "name": "snowball_stopwords",
        "source_paths": [
            f"src/backend/snowball/stopwords/{language}.stop"
            for language in (
                "danish", "dutch", "english", "finnish", "french", "german",
                "hungarian", "italian", "nepali", "norwegian", "portuguese",
                "russian", "spanish", "swedish", "turkish",
            )
        ],
        "output_prefix": "tsearch_data",
        "mode": "0600",
    },
]
if resources["generation_rules"] != expected_generation_rules:
    raise SystemExit("embedded support generation rules are not accepted")

expected_output = {
    "caller_owned_root": True,
    "forbidden_implicit_root": "target/postgresql-embedded/current",
}
if recipe["output"] != expected_output:
    raise SystemExit("embedded caller-owned output facts are not accepted")

proof = recipe["proof"]
expected_proof = {
    "frozen_inputs_directory": "inputs",
    "unpublished_probe_path": "orna-engine-entry-probe",
    "trace_path": "orna-engine-entry-probe.strace",
    "stdout_path": "orna-engine-entry-probe.stdout",
    "deterministic_outputs": [
        "embedded-engine-manifest.json",
        "liborna_postgres18_backend.a",
        "liborna_postgres18_initdb.a",
        "defined-symbols.txt",
        "undefined-symbols.txt",
        "initdb-redefine-symbols.txt",
        "initdb-defined-symbols.txt",
        "initdb-undefined-symbols.txt",
        "embedded-postgresql-support.tar",
        "embedded-postgresql-support-manifest.json",
        "orna-engine-entry-probe.stdout",
        "POSTGRESQL-LICENSE",
        "inputs/embedded-build.toml",
        "inputs/build-embedded.sh",
    ],
}
if proof != expected_proof:
    raise SystemExit("embedded linked-entry proof facts are not accepted")

apt = recipe["apt"]
if set(apt) != {"sources", "packages"}:
    raise SystemExit("embedded apt keys are not accepted")
expected_sources = [
    "deb http://snapshot.debian.org/archive/debian/20251229T000000Z bookworm main",
    "deb http://snapshot.debian.org/archive/debian/20251229T000000Z bookworm-updates main",
    "deb http://snapshot.debian.org/archive/debian-security/20251229T000000Z bookworm-security main",
]
if apt["sources"] != expected_sources:
    raise SystemExit("embedded apt sources are not accepted")
required_packages = {
    "binutils": "2.40-2",
    "bison": "2:3.8.2+dfsg-1+b1",
    "bzip2": "1.0.8-5+b1",
    "ca-certificates": "20230311+deb12u1",
    "curl": "7.88.1-10+deb12u14",
    "file": "1:5.44-3",
    "flex": "2.6.4-8.2",
    "gcc": "4:12.2.0-3",
    "libc6-dev": "2.36-9+deb12u13",
    "make": "4.3-4.1",
    "patch": "2.7.6-7",
    "perl": "5.36.0-7+deb12u3",
    "python3": "3.11.2-1+b1",
    "strace": "6.1-0.1",
}
if apt["packages"] != required_packages:
    raise SystemExit("embedded apt package set is not accepted")

def shell_quote(value):
    return "'" + str(value).replace("'", "'\"'\"'") + "'"

def emit(name, value):
    print(f"{name}={shell_quote(value)}")


def emit_array(name, values):
    print(f"{name}=(" + " ".join(shell_quote(value) for value in values) + ")")


emit("EMBEDDED_IDENTITY", recipe["identity"])
emit("TARGET_PLATFORM", recipe["platform"])
emit("SOURCE_DATE_EPOCH_VALUE", recipe["source_date_epoch"])
emit("BUILDER_IMAGE", recipe["builder_image"])
emit("POSTGRESQL_VERSION", postgresql["version"])
emit("POSTGRESQL_URL", postgresql["url"])
emit("POSTGRESQL_SHA256", postgresql["sha256"])
emit("POSTGRESQL_LICENSE_SOURCE_PATH", postgresql_license["source_path"])
emit("POSTGRESQL_LICENSE_OUTPUT_PATH", postgresql_license["output_path"])
emit("POSTGRESQL_LICENSE_SHA256", postgresql_license["sha256"])
emit("ZLIB_URL", zlib["url"])
emit("ZLIB_SHA256", zlib["sha256"])
emit("ZLIB_PREFIX", zlib["build_prefix"])
emit("BUILD_JOBS", build["jobs"])
emit("STATIC_ARCHIVE_NAME", static_archive["path"])
emit("STATIC_ARCHIVE_MEMBER", static_archive["member"])
emit("STATIC_ENTRY_SYMBOL", static_archive["entry_symbol"])
emit("INITIALIZER_ARCHIVE_NAME", initializer_archive["path"])
emit("INITIALIZER_ARCHIVE_MEMBER", initializer_archive["member"])
emit("INITIALIZER_ENTRY_SYMBOL", initializer_archive["entry_symbol"])
emit("INITIALIZER_SYMBOL_PREFIX", initializer_archive["symbol_prefix"])
emit("INITIALIZER_RENAME_MAP_PATH", initializer_archive["rename_map_path"])
emit("INITIALIZER_DEFINED_SYMBOLS_PATH", initializer_archive["defined_symbols_path"])
emit("INITIALIZER_UNDEFINED_SYMBOLS_PATH", initializer_archive["undefined_symbols_path"])
emit("BACKEND_ENTRY_SYMBOL", lifecycle["backend_entry_symbol"])
emit("SUPPORT_ROOT_SETTER_SYMBOL", lifecycle["support_root_setter_symbol"])
emit("SYSTEM_FUNCTIONS_CAPABILITY_SETTER_SYMBOL", lifecycle["system_functions_capability_setter_symbol"])
emit("INITIALISATION_CHILD_CAPABILITY_SETTER_SYMBOL", lifecycle["initialisation_child_capability_setter_symbol"])
emit("SECCOMP_ENTRY_SYMBOL", lifecycle["seccomp_entry_symbol"])
emit("SQL_GUARD_SQLSTATE", sql_guard["sqlstate"])
emit("FORBIDDEN_IMPLICIT_OUTPUT_ROOT", recipe["output"]["forbidden_implicit_root"])
emit("SUPPORT_BUNDLE_PATH", resources["bundle_path"])
emit("SUPPORT_BUNDLE_SHA256", resources["bundle_sha256"])
emit("SUPPORT_MANIFEST_PATH", resources["manifest_path"])
emit("SUPPORT_MANIFEST_SHA256", resources["manifest_sha256"])
emit("FROZEN_INPUTS_DIRECTORY", proof["frozen_inputs_directory"])
emit("UNPUBLISHED_PROBE_PATH", proof["unpublished_probe_path"])
emit("TRACE_OUTPUT_PATH", proof["trace_path"])
emit("PROBE_STANDARD_OUTPUT_PATH", proof["stdout_path"])
emit_array("POSTGRESQL_CONFIGURE_FLAGS", postgresql["configure_flags"])
emit_array(
    "RECIPE_BUILD_ENVIRONMENT",
    (f"{key}={environment[key]}" for key in sorted(environment)),
)
emit_array("APT_SOURCES", apt["sources"])
emit_array(
    "APT_PACKAGES",
    (f"{name}={apt['packages'][name]}" for name in sorted(apt["packages"])),
)
emit_array("DETERMINISTIC_OUTPUTS", proof["deterministic_outputs"])
emit_array("PATCH_PATHS", (patch["path"] for patch in patches))
emit_array("PATCH_SHA256S", (patch["sha256"] for patch in patches))
emit_array("SQL_GUARD_MESSAGES", sql_guard["messages"])
emit_array(
    "INITIALIZER_ALLOWED_UNDEFINED_BRIDGE_SYMBOLS",
    initializer_archive["allowed_undefined_bridge_symbols"],
)
PY_RECIPE
}

load_recipe() {
    local recipe_path="${1:-${RECIPE_PATH}}"
    local recipe_environment

    recipe_environment="$(emit_recipe_environment "${recipe_path}")" \
        || fail "embedded recipe validation failed"
    eval "${recipe_environment}"
}

verify_sha256() {
    local expected="$1"
    local path="$2"
    local actual

    actual="$(sha256sum "${path}" | awk '{print $1}')"
    [[ "${actual}" == "${expected}" ]]
}

patch_series_path() {
    local recipe_path="$1"
    local relative_path="$2"

    printf '%s/%s\n' "$(dirname "${recipe_path}")" "${relative_path}"
}

verify_patch_series() {
    local recipe_path="$1"
    local patch_index
    local patch_path

    [[ "${#PATCH_PATHS[@]}" -gt 0 \
        && "${#PATCH_PATHS[@]}" == "${#PATCH_SHA256S[@]}" ]] || return 1
    for patch_index in "${!PATCH_PATHS[@]}"; do
        patch_path="$(patch_series_path "${recipe_path}" "${PATCH_PATHS[patch_index]}")"
        [[ -f "${patch_path}" && ! -L "${patch_path}" ]] || return 1
        [[ "$(stat -c '%a' "${patch_path}")" == 644 ]] || return 1
        verify_sha256 "${PATCH_SHA256S[patch_index]}" "${patch_path}" || return 1
    done
}

freeze_patch_series() {
    local recipe_path="$1"
    local frozen_root="$2"
    local destination_path
    local patch_index
    local source_path

    for patch_index in "${!PATCH_PATHS[@]}"; do
        source_path="$(patch_series_path "${recipe_path}" "${PATCH_PATHS[patch_index]}")"
        destination_path="${frozen_root}/${PATCH_PATHS[patch_index]}"
        mkdir -p "$(dirname "${destination_path}")" || return 1
        install -m 0644 "${source_path}" "${destination_path}" || return 1
        [[ "$(stat -c '%a' "${destination_path}")" == 644 ]] || return 1
        verify_sha256 "${PATCH_SHA256S[patch_index]}" "${destination_path}" || return 1
    done
}

apply_patch_series() {
    local recipe_path="$1"
    local source_root="$2"
    local patch_index
    local patch_path

    verify_patch_series "${recipe_path}" || return 1
    for patch_index in "${!PATCH_PATHS[@]}"; do
        patch_path="$(patch_series_path "${recipe_path}" "${PATCH_PATHS[patch_index]}")"
        patch --batch --forward --fuzz=0 -p1 --dry-run \
            --directory="${source_root}" --input="${patch_path}" || return 1
        patch --batch --forward --fuzz=0 -p1 \
            --directory="${source_root}" --input="${patch_path}" || return 1
    done
}

count_defined_symbol() {
    local binary_path="$1"
    local symbol="$2"

    nm --extern-only --defined-only "${binary_path}" \
        | awk -v expected="${symbol}" '$NF == expected { count += 1 } END { print count + 0 }'
}

stage_support_bundle() {
    local source_root="$1"
    local build_root="$2"
    local timezone_root="$3"
    local bundle_path="$4"
    local manifest_path="$5"
    local staging_root="$6"
    local member_list="$7"
    local actual_bundle_sha256
    local actual_manifest_sha256

    python3 - "${RECIPE_PATH}" "${source_root}" "${build_root}" \
        "${timezone_root}" "${staging_root}" "${manifest_path}" \
        "${member_list}" <<'PY_SUPPORT'
import hashlib
import json
import os
import pathlib
import stat
import sys
import tomllib

recipe_path = pathlib.Path(sys.argv[1])
source_root = pathlib.Path(sys.argv[2])
build_root = pathlib.Path(sys.argv[3])
timezone_root = pathlib.Path(sys.argv[4])
staging_root = pathlib.Path(sys.argv[5])
manifest_path = pathlib.Path(sys.argv[6])
member_list_path = pathlib.Path(sys.argv[7])
with recipe_path.open("rb") as recipe_file:
    rules = tomllib.load(recipe_file)["resources"]["generation_rules"]

os.umask(0o077)
required_build_paths = {
    "src/include/catalog/postgres.bki",
    "src/include/catalog/system_constraints.sql",
    "src/backend/snowball/snowball_create.sql",
}
staging_root.mkdir(mode=0o700)
members = {}
casefold_paths = set()


def read_regular(path, *, permit_hard_links):
    source_stat = path.lstat()
    if not stat.S_ISREG(source_stat.st_mode):
        raise SystemExit(f"support source is not a regular file: {path}")
    if not permit_hard_links and source_stat.st_nlink != 1:
        raise SystemExit(f"support source is linked: {path}")
    if source_stat.st_mode & 0o111:
        raise SystemExit(f"support source has an executable mode: {path}")
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
        while True:
            block = os.read(descriptor, 1024 * 1024)
            if not block:
                break
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


def add_member(output_path, content):
    pure_path = pathlib.PurePosixPath(output_path)
    if (
        not output_path
        or pure_path.is_absolute()
        or str(pure_path) != output_path
        or ".." in pure_path.parts
        or any(character in output_path for character in "*?[]")
        or any(ord(character) < 0x20 or ord(character) == 0x7f for character in output_path)
    ):
        raise SystemExit(f"support output path is not clean and relative: {output_path}")
    folded_path = output_path.casefold()
    if output_path in members or folded_path in casefold_paths:
        raise SystemExit(f"support output path is duplicated or case-colliding: {output_path}")
    lowered_parts = [part.casefold() for part in pure_path.parts]
    lowered_name = lowered_parts[-1]
    if "extension" in lowered_parts or "plpgsql" in folded_path:
        raise SystemExit(f"support output contains extension or PL/pgSQL material: {output_path}")
    if lowered_name in {"postgres", "psql", "initdb", "pg_upgrade", "pg_ctl", "pg_resetwal"}:
        raise SystemExit(f"support output has a PostgreSQL executable name: {output_path}")
    if (
        lowered_name.endswith((
            ".a", ".o", ".so", ".tar", ".tar.gz", ".tgz", ".bz2", ".zip", ".control",
        ))
        or ".so." in lowered_name
    ):
        raise SystemExit(f"support output contains code, an archive, or a control file: {output_path}")

    destination = staging_root / output_path
    destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC, 0o600)
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
    casefold_paths.add(folded_path)


for rule in rules:
    if rule["name"] == "timezone_tree":
        continue
    prefix = rule["output_prefix"]
    for source_path_text in rule["source_paths"]:
        build_candidate = build_root / source_path_text
        source_candidate = source_root / source_path_text
        build_exists = build_candidate.exists() or build_candidate.is_symlink()
        source_exists = source_candidate.exists() or source_candidate.is_symlink()
        if source_path_text in required_build_paths and not build_exists:
            raise SystemExit(f"generated support source is absent from the build tree: {source_path_text}")
        if not build_exists and not source_exists:
            raise SystemExit(f"support source is absent: {source_path_text}")
        build_content = read_regular(build_candidate, permit_hard_links=False) if build_exists else None
        source_content = read_regular(source_candidate, permit_hard_links=False) if source_exists else None
        if build_content is not None and source_content is not None and build_content != source_content:
            raise SystemExit(f"build and source support inputs differ: {source_path_text}")
        content = build_content if build_content is not None else source_content
        output_path = pathlib.PurePosixPath(prefix, pathlib.PurePosixPath(source_path_text).name)
        add_member(str(output_path), content)

timezone_root_stat = timezone_root.lstat()
if (
    timezone_root.is_symlink()
    or not stat.S_ISDIR(timezone_root_stat.st_mode)
    or stat.S_IMODE(timezone_root_stat.st_mode) != 0o700
):
    raise SystemExit("generated timezone root is not one private directory")
timezone_files = []
for directory, directory_names, file_names in os.walk(timezone_root, topdown=True, followlinks=False):
    directory_path = pathlib.Path(directory)
    for name in directory_names:
        candidate = directory_path / name
        candidate_stat = candidate.lstat()
        if (
            candidate.is_symlink()
            or not stat.S_ISDIR(candidate_stat.st_mode)
            or stat.S_IMODE(candidate_stat.st_mode) != 0o700
        ):
            raise SystemExit(f"generated timezone tree contains a non-private directory: {candidate}")
    for name in file_names:
        candidate = directory_path / name
        relative_path = candidate.relative_to(timezone_root)
        timezone_files.append((str(pathlib.PurePosixPath(*relative_path.parts)), candidate))
if len(timezone_files) != 598:
    raise SystemExit(
        f"generated timezone tree has {len(timezone_files)} files instead of the accepted 598"
    )
for relative_path, candidate in sorted(timezone_files):
    add_member(
        str(pathlib.PurePosixPath("timezone", relative_path)),
        read_regular(candidate, permit_hard_links=True),
    )

manifest_members = [members[path] for path in sorted(members)]
staging_root_stat = staging_root.lstat()
if (
    staging_root.is_symlink()
    or not stat.S_ISDIR(staging_root_stat.st_mode)
    or stat.S_IMODE(staging_root_stat.st_mode) != 0o700
):
    raise SystemExit("staged support root is not one private directory")
actual_stage_paths = set()
for directory, directory_names, file_names in os.walk(staging_root, topdown=True, followlinks=False):
    directory_path = pathlib.Path(directory)
    for name in directory_names:
        candidate = directory_path / name
        candidate_stat = candidate.lstat()
        if not stat.S_ISDIR(candidate_stat.st_mode) or candidate.is_symlink() or candidate_stat.st_mode & 0o077:
            raise SystemExit(f"staged support directory is not private: {candidate}")
    for name in file_names:
        candidate = directory_path / name
        candidate_stat = candidate.lstat()
        if (
            not stat.S_ISREG(candidate_stat.st_mode)
            or candidate_stat.st_nlink != 1
            or stat.S_IMODE(candidate_stat.st_mode) != 0o600
        ):
            raise SystemExit(f"staged support member metadata is not accepted: {candidate}")
        actual_stage_paths.add(str(pathlib.PurePosixPath(*candidate.relative_to(staging_root).parts)))
if actual_stage_paths != set(members):
    raise SystemExit("staged support inventory differs from the generated manifest")

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
PY_SUPPORT

    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        tar --create --file="${bundle_path}" --directory="${staging_root}" \
            --no-recursion --format=ustar --owner=0 --group=0 --numeric-owner \
            --mode=0600 --mtime="@${SOURCE_DATE_EPOCH_VALUE}" \
            --verbatim-files-from --files-from="${member_list}" \
        || fail "could not create the deterministic support bundle"
    actual_manifest_sha256="$(sha256sum "${manifest_path}" | awk '{print $1}')"
    actual_bundle_sha256="$(sha256sum "${bundle_path}" | awk '{print $1}')"
    if [[ "${actual_manifest_sha256}" != "${SUPPORT_MANIFEST_SHA256}" \
        || "${actual_bundle_sha256}" != "${SUPPORT_BUNDLE_SHA256}" ]]; then
        printf '[postgres-embedded] expected support manifest SHA-256: %s\n' \
            "${SUPPORT_MANIFEST_SHA256}" >&2
        printf '[postgres-embedded] actual support manifest SHA-256:   %s\n' \
            "${actual_manifest_sha256}" >&2
        printf '[postgres-embedded] expected support bundle SHA-256:   %s\n' \
            "${SUPPORT_BUNDLE_SHA256}" >&2
        printf '[postgres-embedded] actual support bundle SHA-256:     %s\n' \
            "${actual_bundle_sha256}" >&2
        fail "generated support manifest or bundle digest does not match the embedded recipe"
    fi

    python3 - "${manifest_path}" "${bundle_path}" \
        "${SOURCE_DATE_EPOCH_VALUE}" <<'PY_VERIFY_SUPPORT'
import hashlib
import json
import pathlib
import sys
import tarfile

manifest_path = pathlib.Path(sys.argv[1])
bundle_path = pathlib.Path(sys.argv[2])
source_date_epoch = int(sys.argv[3])
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if set(manifest) != {"format", "members"} or manifest["format"] != 1:
    raise SystemExit("generated support manifest shape is not accepted")
members = manifest["members"]
if not isinstance(members, list) or not members:
    raise SystemExit("generated support manifest is empty")
expected = {}
casefold_paths = set()
for member in members:
    if set(member) != {"length", "mode", "path", "sha256", "type"}:
        raise SystemExit("generated support manifest member shape is not accepted")
    path = member["path"]
    pure_path = pathlib.PurePosixPath(path)
    if (
        not isinstance(path, str)
        or not path
        or pure_path.is_absolute()
        or str(pure_path) != path
        or ".." in pure_path.parts
        or path.casefold() in casefold_paths
    ):
        raise SystemExit("generated support manifest path is not accepted")
    if member["type"] != "file" or member["mode"] != "0600":
        raise SystemExit(f"generated support manifest type or mode is not accepted: {path}")
    if isinstance(member["length"], bool) or not isinstance(member["length"], int) or member["length"] < 0:
        raise SystemExit(f"generated support manifest length is not accepted: {path}")
    digest = member["sha256"]
    if not isinstance(digest, str) or len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        raise SystemExit(f"generated support manifest digest is not accepted: {path}")
    casefold_paths.add(path.casefold())
    expected[path] = member
if list(expected) != sorted(expected):
    raise SystemExit("generated support manifest is not ordered")

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
            or member.mtime != source_date_epoch
            or member.size != expected_member["length"]
        ):
            raise SystemExit(f"support bundle metadata is not accepted: {member.name}")
        member_file = bundle.extractfile(member)
        if member_file is None:
            raise SystemExit(f"support bundle member cannot be read: {member.name}")
        if hashlib.sha256(member_file.read()).hexdigest() != expected_member["sha256"]:
            raise SystemExit(f"support bundle member digest is not accepted: {member.name}")
PY_VERIFY_SUPPORT
}

container_build() {
    local source_archive="/build/postgresql-source.tar.bz2"
    local source_root="/build/postgresql-source"
    local build_root="/build/postgresql-build"
    local postgresql_license_path
    local zlib_archive="/build/zlib-source.tar.gz"
    local zlib_source="/build/zlib-source"
    local backend_archive_path
    local initializer_archive_path
    local initializer_directory
    local initializer_rename_map
    local initializer_defined_symbols
    local initializer_undefined_symbols
    local probe_source="/build/orna-engine-entry-probe.c"
    local probe_path
    local probe_support_root="/build/entry-probe-support"
    local trace_path
    local standard_output
    local configured_macros="/build/pg-config-manual.macros"
    local defined_symbols="/build/defined-symbols.txt"
    local undefined_symbols="/build/undefined-symbols.txt"
    local generated_timezone_root="/build/generated-timezone"
    local support_staging_root="/build/support-staging"
    local support_member_list="/build/support-members.txt"
    local support_bundle_path
    local support_manifest_path
    local publication_root="/build/verified-publication"
    local expected_output_files="/build/expected-output-files.txt"
    local actual_output_files="/build/actual-output-files.txt"
    local expected_bridges="/build/expected-initdb-bridges.txt"
    local actual_bridges="/build/actual-initdb-bridges.txt"
    local initializer_archive_defined_names="/build/initdb-archive-defined-names.txt"
    local backend_strings="/build/backend-strings.txt"
    local probe_dynamic="/build/entry-probe.dynamic.txt"
    local postgres_executable_pattern
    local unexpected_executable
    local unexpected_link_or_shared_object
    local unexpected_published_entry
    local patch_path
    local symbol

    postgres_executable_pattern='execve(at)?\([^,]*"([^"/]*/)*'
    postgres_executable_pattern+='(postgres|psql|initdb|pg_upgrade|pg_ctl|pg_resetwal)"'

    [[ "${ORNA_HOST_UID:-}" =~ ^[0-9]+$ ]] || fail "host user ID is not a decimal number"
    [[ "${ORNA_HOST_GID:-}" =~ ^[0-9]+$ ]] || fail "host group ID is not a decimal number"
    container_cleanup() {
        chown -R "${ORNA_HOST_UID}:${ORNA_HOST_GID}" /build /output
    }
    trap container_cleanup EXIT

    [[ -r /build/recipe.environment ]] || fail "validated recipe environment is absent"
    # shellcheck disable=SC1091
    source /build/recipe.environment
    backend_archive_path="${build_root}/src/backend/${STATIC_ARCHIVE_NAME}"
    initializer_directory="${build_root}/src/bin/initdb"
    initializer_archive_path="${initializer_directory}/${INITIALIZER_ARCHIVE_NAME}"
    initializer_rename_map="${initializer_directory}/${INITIALIZER_RENAME_MAP_PATH}"
    initializer_defined_symbols="${initializer_directory}/${INITIALIZER_DEFINED_SYMBOLS_PATH}"
    initializer_undefined_symbols="${initializer_directory}/${INITIALIZER_UNDEFINED_SYMBOLS_PATH}"
    postgresql_license_path="${source_root}/${POSTGRESQL_LICENSE_SOURCE_PATH}"
    probe_path="/build/${UNPUBLISHED_PROBE_PATH}"
    trace_path="/build/${TRACE_OUTPUT_PATH}"
    standard_output="/build/${PROBE_STANDARD_OUTPUT_PATH}"
    support_bundle_path="/build/${SUPPORT_BUNDLE_PATH}"
    support_manifest_path="/build/${SUPPORT_MANIFEST_PATH}"
    [[ "$(pwd -P)" == "/build" ]] || fail "container build must use /build"
    [[ -r "${RECIPE_PATH}" ]] || fail "container recipe input is absent"
    mkdir -p /build/tmp

    find /etc/apt/sources.list.d -mindepth 1 -maxdepth 1 -type f -delete
    printf '%s\n' "${APT_SOURCES[@]}" > /etc/apt/sources.list
    apt-get update -o Acquire::Check-Valid-Until=false
    DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends "${APT_PACKAGES[@]}"
    load_recipe
    verify_patch_series "${RECIPE_PATH}" \
        || fail "PostgreSQL patch series does not match the embedded recipe"

    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        curl --fail --location --proto '=https' --tlsv1.2 --output "${source_archive}" "${POSTGRESQL_URL}"
    verify_sha256 "${POSTGRESQL_SHA256}" "${source_archive}" \
        || fail "PostgreSQL source digest does not match embedded recipe"
    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        curl --fail --location --proto '=https' --tlsv1.2 --output "${zlib_archive}" "${ZLIB_URL}"
    verify_sha256 "${ZLIB_SHA256}" "${zlib_archive}" \
        || fail "zlib source digest does not match embedded recipe"
    mkdir -p "${source_root}" "${build_root}" "${zlib_source}"
    tar --extract --gzip --file="${zlib_archive}" --strip-components=1 --directory="${zlib_source}"
    (
        cd "${zlib_source}"
        env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
            "${RECIPE_BUILD_ENVIRONMENT[@]}" \
            ./configure --static --prefix="${ZLIB_PREFIX}"
        env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
            "${RECIPE_BUILD_ENVIRONMENT[@]}" \
            make -j"${BUILD_JOBS}"
        env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
            "${RECIPE_BUILD_ENVIRONMENT[@]}" \
            make install
    )
    [[ -f "${ZLIB_PREFIX}/lib/libz.a" ]] || fail "static zlib archive was not produced"

    tar --extract --bzip2 --file="${source_archive}" --strip-components=1 --directory="${source_root}"
    [[ -f "${postgresql_license_path}" ]] \
        || fail "PostgreSQL licence file is absent from the accepted source archive"
    verify_sha256 "${POSTGRESQL_LICENSE_SHA256}" "${postgresql_license_path}" \
        || fail "PostgreSQL licence digest does not match embedded recipe"
    apply_patch_series "${RECIPE_PATH}" "${source_root}" \
        || fail "PostgreSQL patch series could not apply to its exact predecessor"

    (
        cd "${build_root}"
        env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
            "${RECIPE_BUILD_ENVIRONMENT[@]}" \
            "${source_root}/configure" "${POSTGRESQL_CONFIGURE_FLAGS[@]}"
    )
    if ! printf '#include "pg_config_manual.h"\n' \
        | gcc -I"${build_root}/src/include" -I"${source_root}/src/include" \
            -dM -E -x c - >"${configured_macros}"; then
        fail "could not inspect the configured PostgreSQL process model"
    fi
    if grep -q '^#define EXEC_BACKEND' "${configured_macros}"; then
        fail "embedded build must not define EXEC_BACKEND"
    fi

    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        make -C "${build_root}/src/backend" -j"${BUILD_JOBS}" \
            ORNA_EMBEDDED_ZLIB_ARCHIVE="${ZLIB_PREFIX}/lib/libz.a" \
            orna_postgres18_lifecycle_archives
    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        make -C "${build_root}/src/timezone" -j"${BUILD_JOBS}" zic
    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        make -C "${build_root}/src/backend/snowball" -j"${BUILD_JOBS}" snowball_create.sql

    python3 - "${probe_source}" <<'PY_PROBE'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text(
    "extern int orna_postgres18_entry(int argc, char *argv[]);\n"
    "extern int orna_postgres18_initdb_entry(const char *data_directory);\n"
    "extern int orna_postgres18_set_support_root(const char *absolute_root);\n"
    "static int (* volatile initializer_entry)(const char *) =\n"
    "    orna_postgres18_initdb_entry;\n"
    "int main(int argc, char *argv[])\n"
    "{\n"
    "    if (initializer_entry == 0)\n"
    "        return 124;\n"
    "    if (orna_postgres18_set_support_root(\"/build/entry-probe-support\") != 0)\n"
    "        return 125;\n"
    "    return orna_postgres18_entry(argc, argv);\n"
    "}\n",
    encoding="utf-8",
    newline="\n",
)
PY_PROBE
    mkdir -m 0700 "${probe_support_root}"
    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        make -C "${build_root}/src/backend" -j"${BUILD_JOBS}" \
            ORNA_EMBEDDED_ZLIB_ARCHIVE="${ZLIB_PREFIX}/lib/libz.a" \
            ORNA_EMBEDDED_LIFECYCLE_PROBE_SOURCE="${probe_source}" \
            ORNA_EMBEDDED_LIFECYCLE_PROBE_OUTPUT="${probe_path}" \
            orna_postgres18_lifecycle_probe

    [[ -f "${backend_archive_path}" ]] || fail "embedded backend archive was not produced"
    [[ -f "${initializer_archive_path}" ]] || fail "embedded initialiser archive was not produced"
    [[ -s "${initializer_rename_map}" ]] || fail "initialiser rename-map evidence was not produced"
    [[ -s "${initializer_defined_symbols}" ]] || fail "initialiser defined-symbol evidence was not produced"
    [[ -s "${initializer_undefined_symbols}" ]] || fail "initialiser undefined-symbol evidence was not produced"
    [[ -x "${probe_path}" ]] || fail "embedded dual-archive entry probe was not produced"
    [[ "$(basename "${probe_path}")" != "postgres" ]] || fail "entry probe must not be named postgres"
    [[ "$(count_defined_symbol "${probe_path}" "${INITIALIZER_ENTRY_SYMBOL}")" == "1" ]] \
        || fail "dual-archive entry probe does not retain the linked initialiser entry"
    [[ "$(count_defined_symbol "${probe_path}" "${BACKEND_ENTRY_SYMBOL}")" == "1" ]] \
        || fail "dual-archive entry probe does not retain the linked backend entry"
    [[ "$(ar t "${backend_archive_path}")" == "${STATIC_ARCHIVE_MEMBER}" ]] \
        || fail "embedded backend archive must contain the one accepted flattened member"
    [[ "$(ar t "${initializer_archive_path}")" == "${INITIALIZER_ARCHIVE_MEMBER}" ]] \
        || fail "embedded initialiser archive must contain the one accepted flattened member"
    [[ "$(count_defined_symbol "${backend_archive_path}" "${STATIC_ENTRY_SYMBOL}")" == "1" ]] \
        || fail "embedded backend archive must define one private PostgreSQL entry"
    [[ "$(count_defined_symbol "${initializer_archive_path}" "${INITIALIZER_ENTRY_SYMBOL}")" == "1" ]] \
        || fail "embedded initialiser archive must define one private initialiser entry"
    [[ "$(count_defined_symbol "${backend_archive_path}" main)" == "0" ]] \
        || fail "embedded backend archive must not define C main"
    [[ "$(count_defined_symbol "${backend_archive_path}" deflate)" == "1" ]] \
        || fail "embedded backend archive must contain the pinned static zlib closure"
    for symbol in "${BACKEND_ENTRY_SYMBOL}" "${SUPPORT_ROOT_SETTER_SYMBOL}" \
        "${SYSTEM_FUNCTIONS_CAPABILITY_SETTER_SYMBOL}" \
        "${INITIALISATION_CHILD_CAPABILITY_SETTER_SYMBOL}" \
        "${SECCOMP_ENTRY_SYMBOL}" orna_postgres18_support_root; do
        [[ "$(count_defined_symbol "${backend_archive_path}" "${symbol}")" == "1" ]] \
            || fail "embedded backend archive does not define the required bridge ${symbol} exactly once"
    done
    nm --extern-only --defined-only "${initializer_archive_path}" \
        | awk 'NF >= 2 && $NF !~ /:$/ { print $NF }' \
        | LC_ALL=C sort -u >"${initializer_archive_defined_names}" \
        || fail "could not inspect the initialiser archive defined-symbol closure"
    while IFS= read -r symbol; do
        [[ "${symbol}" == "${INITIALIZER_ENTRY_SYMBOL}" || "${symbol}" == "${INITIALIZER_SYMBOL_PREFIX}"* ]] \
            || fail "initialiser archive exposes an unprefixed symbol: ${symbol}"
    done <"${initializer_archive_defined_names}"
    printf '%s\n' "${INITIALIZER_ALLOWED_UNDEFINED_BRIDGE_SYMBOLS[@]}" \
        | LC_ALL=C sort -u >"${expected_bridges}" \
        || fail "could not write the expected initialiser bridge closure"
    nm --extern-only --undefined-only "${initializer_archive_path}" \
        | awk '$NF ~ /^orna_postgres18_/ { print $NF }' \
        | LC_ALL=C sort -u >"${actual_bridges}" \
        || fail "could not inspect the initialiser archive undefined bridge closure"
    cmp --silent "${expected_bridges}" "${actual_bridges}" \
        || fail "initialiser archive unprefixed bridge closure is not accepted"
    for symbol in "${INITIALIZER_ALLOWED_UNDEFINED_BRIDGE_SYMBOLS[@]}"; do
        grep -E "(^|[[:space:]])${symbol}($|[[:space:]])" \
            "${initializer_undefined_symbols}" >/dev/null \
            || fail "initialiser undefined-symbol evidence omits ${symbol}"
    done
    python3 - "${initializer_rename_map}" "${INITIALIZER_ENTRY_SYMBOL}" \
        "${INITIALIZER_SYMBOL_PREFIX}" "${INITIALIZER_ALLOWED_UNDEFINED_BRIDGE_SYMBOLS[@]}" <<'PY_RENAME_MAP'
import pathlib
import sys

rename_map_path = pathlib.Path(sys.argv[1])
entry_symbol = sys.argv[2]
symbol_prefix = sys.argv[3]
bridge_symbols = set(sys.argv[4:])
rows = [line.split() for line in rename_map_path.read_text(encoding="utf-8").splitlines()]
if not rows or any(len(row) != 2 for row in rows):
    raise SystemExit("initialiser rename map is empty or malformed")
old_symbols = [row[0] for row in rows]
new_symbols = [row[1] for row in rows]
if len(set(old_symbols)) != len(old_symbols) or len(set(new_symbols)) != len(new_symbols):
    raise SystemExit("initialiser rename map repeats a source or destination symbol")
for old_symbol, new_symbol in rows:
    if old_symbol == entry_symbol or old_symbol in bridge_symbols:
        raise SystemExit(f"initialiser rename map renames a public bridge: {old_symbol}")
    if new_symbol != f"{symbol_prefix}{old_symbol}":
        raise SystemExit(f"initialiser rename map has an unexpected destination: {old_symbol}")
PY_RENAME_MAP
    LC_ALL=C nm --format=posix --extern-only --defined-only "${backend_archive_path}" \
        | LC_ALL=C sort >"${defined_symbols}" \
        || fail "could not record the backend defined-symbol closure"
    LC_ALL=C nm --format=posix --extern-only --undefined-only "${backend_archive_path}" \
        | LC_ALL=C sort >"${undefined_symbols}" \
        || fail "could not record the backend undefined-symbol closure"
    strings --all "${backend_archive_path}" >"${backend_strings}" \
        || fail "could not inspect embedded backend archive strings"
    for message in "${SQL_GUARD_MESSAGES[@]}"; do
        grep -F -x "${message}" "${backend_strings}" >/dev/null \
            || fail "embedded backend archive omits SQL guard diagnostic: ${message}"
    done

    mkdir -m 0700 "${generated_timezone_root}"
    (
        umask 077
        env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
            "${RECIPE_BUILD_ENVIRONMENT[@]}" \
            "${build_root}/src/timezone/zic" -d "${generated_timezone_root}" \
                "${source_root}/src/timezone/data/tzdata.zi"
    ) || fail "could not generate the pinned PostgreSQL timezone tree"
    stage_support_bundle "${source_root}" "${build_root}" "${generated_timezone_root}" \
        "${support_bundle_path}" "${support_manifest_path}" \
        "${support_staging_root}" "${support_member_list}"

    strace --follow-forks --quiet --trace=process,file --output="${trace_path}" \
        "${probe_path}" --describe-config >"${standard_output}" \
        || fail "embedded dual-archive describe-config probe failed"
    [[ -s "${standard_output}" ]] || fail "describe-config probe produced no output"
    grep -F "execve(\"${probe_path}\"" "${trace_path}" >/dev/null \
        || fail "entry probe trace does not start from the accepted probe executable"
    if grep -E "${postgres_executable_pattern}" "${trace_path}" >/dev/null; then
        fail "entry probe executed a PostgreSQL executable"
    fi
    if awk '/open(at)?\([^)]*\.(so|so\.[^" ]*)/ && tolower($0) ~ /postgres/ { found = 1 } END { exit !found }' \
        "${trace_path}"; then
        fail "entry probe opened a PostgreSQL shared object"
    fi
    readelf --dynamic "${probe_path}" >"${probe_dynamic}" \
        || fail "could not inspect the dual-archive entry probe dynamic section"
    if grep -E 'Shared library: \[(libz|libpq)\.so' "${probe_dynamic}" >/dev/null; then
        fail "entry probe must use the static zlib and libpq closure in the embedded archives"
    fi

    mkdir -m 0700 "${publication_root}"
    mkdir -m 0700 "${publication_root}/${FROZEN_INPUTS_DIRECTORY}"
    install -m 0644 "${backend_archive_path}" "${publication_root}/${STATIC_ARCHIVE_NAME}"
    install -m 0644 "${initializer_archive_path}" "${publication_root}/${INITIALIZER_ARCHIVE_NAME}"
    install -m 0644 "${defined_symbols}" "${publication_root}/defined-symbols.txt"
    install -m 0644 "${undefined_symbols}" "${publication_root}/undefined-symbols.txt"
    install -m 0644 "${initializer_rename_map}" "${publication_root}/${INITIALIZER_RENAME_MAP_PATH}"
    install -m 0644 "${initializer_defined_symbols}" "${publication_root}/${INITIALIZER_DEFINED_SYMBOLS_PATH}"
    install -m 0644 "${initializer_undefined_symbols}" "${publication_root}/${INITIALIZER_UNDEFINED_SYMBOLS_PATH}"
    install -m 0644 "${support_bundle_path}" "${publication_root}/${SUPPORT_BUNDLE_PATH}"
    install -m 0644 "${support_manifest_path}" "${publication_root}/${SUPPORT_MANIFEST_PATH}"
    install -m 0644 "${trace_path}" "${publication_root}/${TRACE_OUTPUT_PATH}"
    install -m 0644 "${standard_output}" "${publication_root}/${PROBE_STANDARD_OUTPUT_PATH}"
    install -m 0644 "${postgresql_license_path}" "${publication_root}/${POSTGRESQL_LICENSE_OUTPUT_PATH}"
    install -m 0644 "${RECIPE_PATH}" "${publication_root}/${FROZEN_INPUTS_DIRECTORY}/${RECIPE_PATH##*/}"
    install -m 0644 "${SCRIPT_PATH}" "${publication_root}/${FROZEN_INPUTS_DIRECTORY}/${SCRIPT_PATH##*/}"
    freeze_patch_series "${RECIPE_PATH}" "${publication_root}/${FROZEN_INPUTS_DIRECTORY}" \
        || fail "could not freeze the verified PostgreSQL patch series"
    verify_sha256 "${POSTGRESQL_LICENSE_SHA256}" "${publication_root}/${POSTGRESQL_LICENSE_OUTPUT_PATH}" \
        || fail "staged PostgreSQL licence digest does not match embedded recipe"
    verify_sha256 "${SUPPORT_BUNDLE_SHA256}" "${publication_root}/${SUPPORT_BUNDLE_PATH}" \
        || fail "staged support bundle digest does not match embedded recipe"
    verify_sha256 "${SUPPORT_MANIFEST_SHA256}" "${publication_root}/${SUPPORT_MANIFEST_PATH}" \
        || fail "staged support manifest digest does not match embedded recipe"

    python3 - "${RECIPE_PATH}" "${SCRIPT_PATH}" "${probe_path}" \
        "${FROZEN_INPUTS_DIRECTORY}" "${publication_root}" <<'PY_MANIFEST'
import hashlib
import json
import pathlib
import sys
import tomllib

recipe_path = pathlib.Path(sys.argv[1])
script_path = pathlib.Path(sys.argv[2])
probe_path = pathlib.Path(sys.argv[3])
frozen_inputs_directory = sys.argv[4]
output_path = pathlib.Path(sys.argv[5])
with recipe_path.open("rb") as recipe_file:
    recipe = tomllib.load(recipe_file)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


resource_manifest_path = output_path / recipe["resources"]["manifest_path"]
resource_manifest = json.loads(resource_manifest_path.read_text(encoding="utf-8"))
patches = []
for patch in recipe["patches"]:
    frozen_path = output_path / frozen_inputs_directory / patch["path"]
    frozen_sha256 = digest(frozen_path)
    if frozen_sha256 != patch["sha256"]:
        raise SystemExit("frozen PostgreSQL patch digest does not match the recipe")
    patches.append({
        "path": f"{frozen_inputs_directory}/{patch['path']}",
        "sha256": frozen_sha256,
    })
document = {
    "format": 1,
    "identity": recipe["identity"],
    "inputs": {
        "recipe": {
            "path": f"{frozen_inputs_directory}/{recipe_path.name}",
            "sha256": digest(output_path / frozen_inputs_directory / recipe_path.name),
        },
        "script": {
            "path": f"{frozen_inputs_directory}/{script_path.name}",
            "sha256": digest(output_path / frozen_inputs_directory / script_path.name),
        },
        "patches": patches,
        "facts": recipe,
    },
    "static_archives": {
        "backend": {
            "path": recipe["static_archive"]["path"],
            "sha256": digest(output_path / recipe["static_archive"]["path"]),
        },
        "initializer": {
            "path": recipe["initializer_archive"]["path"],
            "sha256": digest(output_path / recipe["initializer_archive"]["path"]),
        },
    },
    "symbol_closure": {
        "backend_defined": {
            "path": "defined-symbols.txt",
            "sha256": digest(output_path / "defined-symbols.txt"),
        },
        "backend_undefined": {
            "path": "undefined-symbols.txt",
            "sha256": digest(output_path / "undefined-symbols.txt"),
        },
        "initializer_rename_map": {
            "path": recipe["initializer_archive"]["rename_map_path"],
            "sha256": digest(output_path / recipe["initializer_archive"]["rename_map_path"]),
        },
        "initializer_defined": {
            "path": recipe["initializer_archive"]["defined_symbols_path"],
            "sha256": digest(output_path / recipe["initializer_archive"]["defined_symbols_path"]),
        },
        "initializer_undefined": {
            "path": recipe["initializer_archive"]["undefined_symbols_path"],
            "sha256": digest(output_path / recipe["initializer_archive"]["undefined_symbols_path"]),
        },
    },
    "support": {
        "bundle": {
            "path": recipe["resources"]["bundle_path"],
            "sha256": digest(output_path / recipe["resources"]["bundle_path"]),
        },
        "manifest": {
            "path": recipe["resources"]["manifest_path"],
            "sha256": digest(resource_manifest_path),
            "members": resource_manifest["members"],
        },
    },
    "entry_probe": {
        "name": probe_path.name,
        "published": False,
        "sha256": digest(probe_path),
    },
    "postgresql_license": {
        "source": {
            "path": recipe["postgresql_license"]["source_path"],
            "sha256": recipe["postgresql_license"]["sha256"],
        },
        "published": {
            "path": recipe["postgresql_license"]["output_path"],
            "sha256": digest(output_path / recipe["postgresql_license"]["output_path"]),
        },
    },
}
(output_path / "embedded-engine-manifest.json").write_text(
    json.dumps(document, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
    newline="\n",
)
PY_MANIFEST
    {
        printf '%s\n' "${DETERMINISTIC_OUTPUTS[@]}"
        printf '%s\n' "${TRACE_OUTPUT_PATH}"
        printf '%s/%s\n' "${FROZEN_INPUTS_DIRECTORY}" "${RECIPE_PATH##*/}"
        printf '%s/%s\n' "${FROZEN_INPUTS_DIRECTORY}" "${SCRIPT_PATH##*/}"
        for patch_path in "${PATCH_PATHS[@]}"; do
            printf '%s/%s\n' "${FROZEN_INPUTS_DIRECTORY}" "${patch_path}"
        done
    } | LC_ALL=C sort -u >"${expected_output_files}" \
        || fail "could not write the expected embedded output inventory"
    find "${publication_root}" -type f -printf '%P\n' \
        | LC_ALL=C sort >"${actual_output_files}" \
        || fail "could not inspect the staged embedded output inventory"
    cmp --silent "${expected_output_files}" "${actual_output_files}" \
        || fail "embedded output file inventory does not match the accepted proof contract"
    unexpected_link_or_shared_object="$(find "${publication_root}" \
        \( -type l -o \( -type f -name '*.so*' \) \) -print -quit)" \
        || fail "could not inspect staged output links and shared objects"
    if [[ -n "${unexpected_link_or_shared_object}" ]]; then
        fail "embedded output contains a link or shared object"
    fi
    unexpected_executable="$(find "${publication_root}" -type f -perm /111 -print -quit)" \
        || fail "could not inspect staged output executable modes"
    if [[ -n "${unexpected_executable}" ]]; then
        fail "embedded output contains an executable"
    fi
    unexpected_published_entry="$(find /output -mindepth 1 -print -quit)" \
        || fail "could not inspect the container output root"
    if [[ -n "${unexpected_published_entry}" ]]; then
        fail "container output became non-empty before all proof gates passed"
    fi
    cp -a "${publication_root}/." /output/ \
        || fail "could not publish the verified embedded output from the container"
}

host_build() {
    local requested_output_root="$1"
    local first_build_root=""
    local first_output_root=""
    local frozen_inputs_root=""
    local host_gid
    local host_uid
    local output_name
    local output_parent
    local output_root
    local patch_index
    local previous_output_root=""
    local result_root
    local second_build_root=""
    local second_output_root=""
    local frozen_recipe_path=""
    local frozen_script_path=""
    local existing_output_entry=""

    run_container_build() {
        local build_root="$1"
        local output_root="$2"

        emit_recipe_environment "${frozen_recipe_path}" >"${build_root}/recipe.environment"
        docker run --rm --platform="${TARGET_PLATFORM}" \
            --mount "type=bind,src=${frozen_inputs_root}/frozen,dst=/frozen,readonly" \
            --mount "type=bind,src=${build_root},dst=/build" \
            --mount "type=bind,src=${output_root},dst=/output" \
            --workdir /build \
            --env ORNA_EMBEDDED_CONTAINER=1 \
            --env ORNA_HOST_GID="${host_gid}" \
            --env ORNA_HOST_UID="${host_uid}" \
            "${BUILDER_IMAGE}" \
            bash "/frozen/${frozen_script_path##*/}" --container-build
    }

    compare_exact() {
        local description="$1"
        local first_path="$2"
        local second_path="$3"
        local first_sha256
        local second_sha256

        if ! cmp --silent "${first_path}" "${second_path}"; then
            first_sha256="$(sha256sum "${first_path}" | awk '{print $1}')"
            second_sha256="$(sha256sum "${second_path}" | awk '{print $1}')"
            fail "${description} differs between the two isolated builds: ${first_sha256} != ${second_sha256}"
        fi
        first_sha256="$(sha256sum "${first_path}" | awk '{print $1}')"
        log "matched ${description}: ${first_sha256}"
    }

    [[ "${requested_output_root}" == /* ]] \
        || fail "embedded output root must be an absolute path"
    [[ "${requested_output_root}" != *$'\n'* && "${requested_output_root}" != *$'\r'* \
        && "${requested_output_root}" != *$'\t'* \
        && "${requested_output_root}" != *','* ]] \
        || fail "embedded output root contains an unsupported character"
    output_name="$(basename -- "${requested_output_root}")"
    output_parent="$(dirname -- "${requested_output_root}")"
    [[ -n "${output_name}" && "${output_name}" != '/' \
        && "${output_name}" != '.' && "${output_name}" != '..' ]] \
        || fail "embedded output root must name one owned directory"
    [[ -d "${output_parent}" && ! -L "${requested_output_root}" ]] \
        || fail "embedded output parent must exist and the output root must not be a symbolic link"
    output_parent="$(cd "${output_parent}" && pwd -P)"
    output_root="${output_parent}/${output_name}"

    load_recipe "${RECIPE_PATH}"
    frozen_inputs_root="$(mktemp -d "${output_parent}/.${output_name}.build.XXXXXXXX")"
    previous_output_root="${frozen_inputs_root}/previous-output"
    first_build_root="${frozen_inputs_root}/build.first"
    first_output_root="${frozen_inputs_root}/output.first"
    second_build_root="${frozen_inputs_root}/build.second"
    second_output_root="${frozen_inputs_root}/output.second"
    mkdir -p "${first_build_root}" "${first_output_root}" \
        "${second_build_root}" "${second_output_root}"
    EMBEDDED_BUILD_CLEANUP_PATHS+=("${frozen_inputs_root}")
    trap cleanup_embedded_build_paths EXIT
    frozen_recipe_path="${frozen_inputs_root}/frozen/${RECIPE_PATH##*/}"
    frozen_script_path="${frozen_inputs_root}/frozen/${SCRIPT_PATH##*/}"
    mkdir -p "${frozen_inputs_root}/frozen"
    install -m 0644 "${RECIPE_PATH}" "${frozen_recipe_path}"
    install -m 0644 "${SCRIPT_PATH}" "${frozen_script_path}"
    freeze_patch_series "${RECIPE_PATH}" "${frozen_inputs_root}/frozen" \
        || fail "could not freeze the PostgreSQL patch series"
    load_recipe "${frozen_recipe_path}"
    verify_patch_series "${frozen_recipe_path}" \
        || fail "frozen PostgreSQL patch series does not match the recipe"
    [[ "${output_root}" != "${REPOSITORY_ROOT}/${FORBIDDEN_IMPLICIT_OUTPUT_ROOT}" ]] \
        || fail "embedded build cannot publish to the obsolete implicit output root"
    if [[ -e "${output_root}" ]]; then
        [[ -d "${output_root}" ]] || fail "embedded output root is not a directory"
        existing_output_entry="$(find "${output_root}" -mindepth 1 -print -quit)" \
            || fail "could not inspect the requested embedded output root"
        if [[ -n "${existing_output_entry}" ]]; then
            [[ -f "${output_root}/embedded-engine-manifest.json" ]] \
                || fail "non-empty embedded output root is not a prior verified output"
        fi
    fi
    command -v docker >/dev/null 2>&1 || fail "docker is required for the embedded build"
    result_root="${output_root}"
    host_uid="$(id -u)"
    host_gid="$(id -g)"

    docker pull --platform="${TARGET_PLATFORM}" "${BUILDER_IMAGE}"
    run_container_build "${first_build_root}" "${first_output_root}"
    run_container_build "${second_build_root}" "${second_output_root}"

    compare_exact "unpublished native entry probe" \
        "${first_build_root}/${UNPUBLISHED_PROBE_PATH}" "${second_build_root}/${UNPUBLISHED_PROBE_PATH}"
    for deterministic_output in "${DETERMINISTIC_OUTPUTS[@]}"; do
        compare_exact "${deterministic_output}" \
            "${first_output_root}/${deterministic_output}" "${second_output_root}/${deterministic_output}"
    done
    compare_exact "published frozen recipe input" \
        "${frozen_recipe_path}" "${first_output_root}/${FROZEN_INPUTS_DIRECTORY}/${RECIPE_PATH##*/}"
    for patch_index in "${!PATCH_PATHS[@]}"; do
        compare_exact "published frozen patch input ${PATCH_PATHS[patch_index]}" \
            "$(patch_series_path "${frozen_recipe_path}" "${PATCH_PATHS[patch_index]}")" \
            "${first_output_root}/${FROZEN_INPUTS_DIRECTORY}/${PATCH_PATHS[patch_index]}"
        compare_exact "two-build frozen patch input ${PATCH_PATHS[patch_index]}" \
            "${first_output_root}/${FROZEN_INPUTS_DIRECTORY}/${PATCH_PATHS[patch_index]}" \
            "${second_output_root}/${FROZEN_INPUTS_DIRECTORY}/${PATCH_PATHS[patch_index]}"
    done
    compare_exact "published frozen script input" \
        "${frozen_script_path}" "${first_output_root}/${FROZEN_INPUTS_DIRECTORY}/${SCRIPT_PATH##*/}"
    [[ -s "${first_output_root}/${TRACE_OUTPUT_PATH}" && -s "${second_output_root}/${TRACE_OUTPUT_PATH}" ]] \
        || fail "each isolated build must retain trace evidence"

    EMBEDDED_BUILD_PUBLICATION_PREVIOUS="${previous_output_root}"
    EMBEDDED_BUILD_PUBLICATION_RESULT="${result_root}"
    EMBEDDED_BUILD_PUBLICATION_IN_PROGRESS=1
    if [[ -e "${result_root}" ]]; then
        mv -- "${result_root}" "${previous_output_root}"
    fi
    if ! mv -- "${first_output_root}" "${result_root}"; then
        if [[ -e "${previous_output_root}" ]]; then
            mv -- "${previous_output_root}" "${result_root}" \
                || fail "could not restore the previous embedded output after publication failure"
        fi
        fail "could not publish the verified embedded output"
    fi
    EMBEDDED_BUILD_PUBLICATION_IN_PROGRESS=0
    first_output_root=""
    log "wrote embedded lifecycle inputs and proof to ${result_root}"
}

main() {
    case "${1:-}" in
        --validate)
            [[ "$#" == 1 ]] || fail "--validate accepts no additional arguments"
            load_recipe
            log "validated embedded PostgreSQL input recipe"
            ;;
        --container-build)
            [[ "$#" == 1 && "${ORNA_EMBEDDED_CONTAINER:-}" == 1 ]] \
                || fail "container build may run only in the embedded builder"
            container_build
            ;;
        '')
            fail "usage: packaging/postgresql/build-embedded.sh [--validate | <absolute-output-root>]"
            ;;
        *)
            [[ "$#" == 1 ]] \
                || fail "embedded build accepts exactly one caller-owned output root"
            host_build "$1"
            ;;
    esac
}

main "$@"
