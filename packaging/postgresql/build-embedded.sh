#!/usr/bin/env bash
set -euo pipefail

readonly SCRIPT_PATH="${BASH_SOURCE[0]}"
readonly SCRIPT_DIRECTORY="$(cd "$(dirname "${SCRIPT_PATH}")" && pwd -P)"
readonly REPOSITORY_ROOT="$(cd "${SCRIPT_DIRECTORY}/../.." && pwd -P)"
readonly RECIPE_PATH="${SCRIPT_DIRECTORY}/embedded-build.toml"
readonly PATCH_PATH="${SCRIPT_DIRECTORY}/embedded-postgresql-18.4.patch"
readonly TARGET_ROOT="${REPOSITORY_ROOT}/target/postgresql-embedded"

declare -a EMBEDDED_BUILD_CLEANUP_PATHS=()

log() {
    printf '[postgres-embedded] %s\n' "$*"
}

fail() {
    printf '[postgres-embedded] error: %s\n' "$*" >&2
    exit 1
}

cleanup_embedded_build_paths() {
    local path

    for path in "${EMBEDDED_BUILD_CLEANUP_PATHS[@]}"; do
        if [[ -d "${path}" ]]; then
            rm -rf -- "${path}"
        fi
    done
}

emit_recipe_environment() {
    local recipe_path="${1:-${RECIPE_PATH}}"
    local patch_path="${2:-${PATCH_PATH}}"

    python3 - "${recipe_path}" "${patch_path}" <<'PY_RECIPE'
import hashlib
import pathlib
import sys
import tomllib

recipe_path = pathlib.Path(sys.argv[1])
patch_path = pathlib.Path(sys.argv[2])
with recipe_path.open("rb") as recipe_file:
    recipe = tomllib.load(recipe_file)

expected_top_level = {
    "format", "identity", "target", "platform", "source_date_epoch",
    "builder_image", "snapshot_timestamp", "patch", "patch_sha256",
    "postgresql", "postgresql_license", "zlib", "build", "static_archive",
    "resources", "proof", "apt",
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
if recipe["builder_image"] != "docker.io/library/debian@sha256:a1363ada3b45cb3ebc74c78943558f8b0c2b59aaa194d8224e1b02cfd5d78583":
    raise SystemExit("embedded recipe builder image is not accepted")
if recipe["snapshot_timestamp"] != "20251229T000000Z":
    raise SystemExit("embedded recipe snapshot timestamp is not accepted")
if recipe["patch"] != patch_path.name:
    raise SystemExit("embedded recipe patch name is not accepted")
expected_patch_sha256 = recipe["patch_sha256"]
if len(expected_patch_sha256) != 64 or any(
    character not in "0123456789abcdef" for character in expected_patch_sha256
):
    raise SystemExit("embedded recipe patch digest is not a lowercase SHA-256 digest")
actual_patch_sha256 = hashlib.sha256(patch_path.read_bytes()).hexdigest()
if actual_patch_sha256 != expected_patch_sha256:
    raise SystemExit("embedded PostgreSQL patch digest does not match the recipe")

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
if recipe["resources"] != {"embedded_support_assets": []}:
    raise SystemExit("the entry tracer must record an empty support-asset input set")

proof = recipe["proof"]
expected_proof = {
    "frozen_inputs_directory": "inputs",
    "unpublished_probe_path": "orna-engine-entry-probe",
    "trace_path": "orna-engine-entry-probe.strace",
    "stdout_path": "orna-engine-entry-probe.stdout",
    "deterministic_outputs": [
        "embedded-engine-manifest.json",
        "liborna_postgres18_backend.a",
        "defined-symbols.txt",
        "undefined-symbols.txt",
        "orna-engine-entry-probe.stdout",
        "POSTGRESQL-LICENSE",
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
emit("PATCH_SHA256", expected_patch_sha256)
emit("STATIC_ARCHIVE_NAME", static_archive["path"])
emit("STATIC_ARCHIVE_MEMBER", static_archive["member"])
emit("STATIC_ENTRY_SYMBOL", static_archive["entry_symbol"])
emit("FROZEN_INPUTS_DIRECTORY", proof["frozen_inputs_directory"])
emit("UNPUBLISHED_PROBE_PATH", proof["unpublished_probe_path"])
emit("TRACE_OUTPUT_PATH", proof["trace_path"])
emit("PROBE_STANDARD_OUTPUT_PATH", proof["stdout_path"])
print("POSTGRESQL_CONFIGURE_FLAGS=(" + " ".join(shell_quote(flag) for flag in postgresql["configure_flags"]) + ")")
print("RECIPE_BUILD_ENVIRONMENT=(" + " ".join(shell_quote(f"{key}={environment[key]}") for key in sorted(environment)) + ")")
print("APT_SOURCES=(" + " ".join(shell_quote(source) for source in apt["sources"]) + ")")
print("APT_PACKAGES=(" + " ".join(shell_quote(f"{name}={apt['packages'][name]}") for name in sorted(apt["packages"])) + ")")
print("DETERMINISTIC_OUTPUTS=(" + " ".join(shell_quote(path) for path in proof["deterministic_outputs"]) + ")")
PY_RECIPE
}

load_recipe() {
    local patch_path="${2:-${PATCH_PATH}}"
    local recipe_path="${1:-${RECIPE_PATH}}"
    local recipe_environment

    recipe_environment="$(emit_recipe_environment "${recipe_path}" "${patch_path}")" \
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

container_build() {
    local source_archive="/build/postgresql-source.tar.bz2"
    local source_root="/build/postgresql-source"
    local build_root="/build/postgresql-build"
    local postgresql_license_path
    local zlib_archive="/build/zlib-source.tar.gz"
    local zlib_source="/build/zlib-source"
    local archive_path
    local probe_source="/build/orna-engine-entry-probe.c"
    local probe_path
    local trace_path
    local standard_output
    local configured_macros="/build/pg-config-manual.macros"
    local defined_symbols="/build/defined-symbols.txt"
    local undefined_symbols="/build/undefined-symbols.txt"

    [[ "${ORNA_HOST_UID:-}" =~ ^[0-9]+$ ]] || fail "host user ID is not a decimal number"
    [[ "${ORNA_HOST_GID:-}" =~ ^[0-9]+$ ]] || fail "host group ID is not a decimal number"
    container_cleanup() {
        chown -R "${ORNA_HOST_UID}:${ORNA_HOST_GID}" /build /output
    }
    trap container_cleanup EXIT

    [[ -r /build/recipe.environment ]] || fail "validated recipe environment is absent"
    # shellcheck disable=SC1091
    source /build/recipe.environment
    archive_path="${build_root}/src/backend/${STATIC_ARCHIVE_NAME}"
    postgresql_license_path="${source_root}/${POSTGRESQL_LICENSE_SOURCE_PATH}"
    probe_path="/build/${UNPUBLISHED_PROBE_PATH}"
    trace_path="/build/${TRACE_OUTPUT_PATH}"
    standard_output="/build/${PROBE_STANDARD_OUTPUT_PATH}"
    [[ "$(pwd -P)" == "/build" ]] || fail "container build must use /build"
    [[ -r "${RECIPE_PATH}" && -r "${PATCH_PATH}" ]] || fail "container source inputs are absent"
    mkdir -p /build/tmp

    find /etc/apt/sources.list.d -mindepth 1 -maxdepth 1 -type f -delete
    printf '%s\n' "${APT_SOURCES[@]}" > /etc/apt/sources.list
    apt-get update -o Acquire::Check-Valid-Until=false
    DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends "${APT_PACKAGES[@]}"
    load_recipe

    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        curl --fail --location --proto '=https' --tlsv1.2 --output "${source_archive}" "${POSTGRESQL_URL}"
    verify_sha256 "${POSTGRESQL_SHA256}" "${source_archive}" \
        || fail "PostgreSQL source digest does not match embedded recipe"
    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        curl --fail --location --proto '=https' --tlsv1.2 --output "${zlib_archive}" "${ZLIB_URL}"
    verify_sha256 "${ZLIB_SHA256}" "${zlib_archive}" \
        || fail "zlib source digest does not match embedded recipe"
    verify_sha256 "${PATCH_SHA256}" "${PATCH_PATH}" \
        || fail "PostgreSQL patch digest does not match embedded recipe"

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
    patch --batch --forward --fuzz=0 --strip=1 \
        --directory="${source_root}" --input="${PATCH_PATH}"

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

    python3 - "${probe_source}" <<'PY_PROBE'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text(
    "extern int orna_postgres18_entry(int argc, char *argv[]);\n"
    "int main(int argc, char *argv[])\n"
    "{\n"
    "    return orna_postgres18_entry(argc, argv);\n"
    "}\n",
    encoding="utf-8",
    newline="\n",
)
PY_PROBE

    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        make -C "${build_root}/src/backend" -j"${BUILD_JOBS}" \
            ORNA_EMBEDDED_ZLIB_ARCHIVE="${ZLIB_PREFIX}/lib/libz.a" \
            ORNA_EMBEDDED_PROBE_SOURCE="${probe_source}" \
            ORNA_EMBEDDED_PROBE_OUTPUT="${probe_path}" \
            orna_postgres18_entry_probe

    [[ -f "${archive_path}" ]] || fail "embedded backend archive was not produced"
    [[ -x "${probe_path}" ]] || fail "embedded entry probe was not produced"
    [[ "$(basename "${probe_path}")" != "postgres" ]] || fail "entry probe must not be named postgres"
    [[ "$(ar t "${archive_path}")" == "${STATIC_ARCHIVE_MEMBER}" ]] \
        || fail "embedded archive must contain the one accepted flattened member"
    [[ "$(nm --extern-only --defined-only "${archive_path}" | awk -v symbol="${STATIC_ENTRY_SYMBOL}" '$NF == symbol { count += 1 } END { print count + 0 }')" == "1" ]] \
        || fail "embedded archive must define one private PostgreSQL entry"
    [[ "$(nm --extern-only --defined-only "${archive_path}" | awk '$NF == "main" { count += 1 } END { print count + 0 }')" == "0" ]] \
        || fail "embedded archive must not define C main"
    [[ "$(nm --extern-only --defined-only "${archive_path}" | awk '$NF == "deflate" { count += 1 } END { print count + 0 }')" == "1" ]] \
        || fail "embedded archive must contain the pinned static zlib closure"
    LC_ALL=C nm --format=posix --extern-only --defined-only "${archive_path}" | LC_ALL=C sort >"${defined_symbols}"
    LC_ALL=C nm --format=posix --extern-only --undefined-only "${archive_path}" | LC_ALL=C sort >"${undefined_symbols}"

    strace --follow-forks --quiet --trace=process,file --output="${trace_path}" \
        "${probe_path}" --describe-config >"${standard_output}"
    [[ -s "${standard_output}" ]] || fail "describe-config probe produced no output"
    grep -F "execve(\"${probe_path}\"" "${trace_path}" >/dev/null \
        || fail "entry probe trace does not start from the accepted probe executable"
    if grep -E 'execve(at)?\([^,]*"([^"/]*/)*(postgres|psql|initdb|pg_upgrade|pg_ctl|pg_resetwal)"' "${trace_path}" >/dev/null; then
        fail "entry probe executed a PostgreSQL executable"
    fi
    if grep -E 'open(at)?\([^)]*\.(so|so\.[^" ]*)' "${trace_path}" | grep -i postgres >/dev/null; then
        fail "entry probe opened a PostgreSQL shared object"
    fi
    if readelf --dynamic "${probe_path}" | grep -E 'Shared library: \[libz\.so' >/dev/null; then
        fail "entry probe must use the zlib code in the embedded archive"
    fi

    mkdir -p "/output/${FROZEN_INPUTS_DIRECTORY}"
    install -m 0644 "${archive_path}" /output/liborna_postgres18_backend.a
    install -m 0644 "${defined_symbols}" /output/defined-symbols.txt
    install -m 0644 "${undefined_symbols}" /output/undefined-symbols.txt
    install -m 0644 "${trace_path}" "/output/${TRACE_OUTPUT_PATH}"
    install -m 0644 "${standard_output}" "/output/${PROBE_STANDARD_OUTPUT_PATH}"
    install -m 0644 "${postgresql_license_path}" "/output/${POSTGRESQL_LICENSE_OUTPUT_PATH}"
    install -m 0644 "${RECIPE_PATH}" "/output/${FROZEN_INPUTS_DIRECTORY}/${RECIPE_PATH##*/}"
    install -m 0644 "${SCRIPT_PATH}" "/output/${FROZEN_INPUTS_DIRECTORY}/${SCRIPT_PATH##*/}"
    install -m 0644 "${PATCH_PATH}" "/output/${FROZEN_INPUTS_DIRECTORY}/${PATCH_PATH##*/}"
    verify_sha256 "${POSTGRESQL_LICENSE_SHA256}" "/output/${POSTGRESQL_LICENSE_OUTPUT_PATH}" \
        || fail "published PostgreSQL licence digest does not match embedded recipe"
    python3 - "${RECIPE_PATH}" "${PATCH_PATH}" "${SCRIPT_PATH}" "${probe_path}" \
        "${FROZEN_INPUTS_DIRECTORY}" /output <<'PY_MANIFEST'
import hashlib
import json
import pathlib
import sys
import tomllib

recipe_path = pathlib.Path(sys.argv[1])
patch_path = pathlib.Path(sys.argv[2])
script_path = pathlib.Path(sys.argv[3])
probe_path = pathlib.Path(sys.argv[4])
frozen_inputs_directory = sys.argv[5]
output_path = pathlib.Path(sys.argv[6])
with recipe_path.open("rb") as recipe_file:
    recipe = tomllib.load(recipe_file)

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

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
        "patch": {
            "path": f"{frozen_inputs_directory}/{patch_path.name}",
            "sha256": digest(output_path / frozen_inputs_directory / patch_path.name),
        },
        "facts": recipe,
    },
    "static_archive": {
        "path": "liborna_postgres18_backend.a",
        "sha256": digest(output_path / "liborna_postgres18_backend.a"),
    },
    "symbol_closure": {
        "defined": {
            "path": "defined-symbols.txt",
            "sha256": digest(output_path / "defined-symbols.txt"),
        },
        "undefined": {
            "path": "undefined-symbols.txt",
            "sha256": digest(output_path / "undefined-symbols.txt"),
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
        printf '%s/%s\n' "${FROZEN_INPUTS_DIRECTORY}" "${PATCH_PATH##*/}"
    } | LC_ALL=C sort >/build/expected-output-files.txt
    find /output -type f -printf '%P\n' | LC_ALL=C sort >/build/actual-output-files.txt
    cmp --silent /build/expected-output-files.txt /build/actual-output-files.txt \
        || fail "embedded output file inventory does not match the accepted proof contract"
    if find /output -type f -name '*.so*' -print -quit | grep -q .; then
        fail "embedded output contains a shared object"
    fi
    if find /output -type f -perm /111 -print -quit | grep -q .; then
        fail "embedded output contains an executable"
    fi
}

host_build() {
    local first_build_root=""
    local first_output_root=""
    local frozen_inputs_root=""
    local host_gid
    local host_uid
    local result_root
    local second_build_root=""
    local second_output_root=""
    local frozen_patch_path=""
    local frozen_recipe_path=""
    local frozen_script_path=""

    run_container_build() {
        local build_root="$1"
        local output_root="$2"

        emit_recipe_environment "${frozen_recipe_path}" "${frozen_patch_path}" >"${build_root}/recipe.environment"
        docker run --rm --platform="${TARGET_PLATFORM}" \
            --mount "type=bind,src=${frozen_inputs_root},dst=/frozen,readonly" \
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

        cmp --silent "${first_path}" "${second_path}" \
            || fail "${description} differs between the two isolated builds: $(sha256sum "${first_path}" | awk '{print $1}') != $(sha256sum "${second_path}" | awk '{print $1}')"
        log "matched ${description}: $(sha256sum "${first_path}" | awk '{print $1}')"
    }

    mkdir -p "${TARGET_ROOT}"
    frozen_inputs_root="$(mktemp -d "${TARGET_ROOT}/inputs.XXXXXXXX")"
    EMBEDDED_BUILD_CLEANUP_PATHS+=("${frozen_inputs_root}")
    trap cleanup_embedded_build_paths EXIT
    frozen_recipe_path="${frozen_inputs_root}/${RECIPE_PATH##*/}"
    frozen_patch_path="${frozen_inputs_root}/${PATCH_PATH##*/}"
    frozen_script_path="${frozen_inputs_root}/${SCRIPT_PATH##*/}"
    install -m 0644 "${RECIPE_PATH}" "${frozen_recipe_path}"
    install -m 0644 "${PATCH_PATH}" "${frozen_patch_path}"
    install -m 0644 "${SCRIPT_PATH}" "${frozen_script_path}"
    load_recipe "${frozen_recipe_path}" "${frozen_patch_path}"
    command -v docker >/dev/null 2>&1 || fail "docker is required for the embedded build"
    first_build_root="$(mktemp -d "${TARGET_ROOT}/build.first.XXXXXXXX")"
    EMBEDDED_BUILD_CLEANUP_PATHS+=("${first_build_root}")
    first_output_root="$(mktemp -d "${TARGET_ROOT}/output.first.XXXXXXXX")"
    EMBEDDED_BUILD_CLEANUP_PATHS+=("${first_output_root}")
    second_build_root="$(mktemp -d "${TARGET_ROOT}/build.second.XXXXXXXX")"
    EMBEDDED_BUILD_CLEANUP_PATHS+=("${second_build_root}")
    second_output_root="$(mktemp -d "${TARGET_ROOT}/output.second.XXXXXXXX")"
    EMBEDDED_BUILD_CLEANUP_PATHS+=("${second_output_root}")
    result_root="${TARGET_ROOT}/current"
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
    compare_exact "published frozen patch input" \
        "${frozen_patch_path}" "${first_output_root}/${FROZEN_INPUTS_DIRECTORY}/${PATCH_PATH##*/}"
    compare_exact "published frozen script input" \
        "${frozen_script_path}" "${first_output_root}/${FROZEN_INPUTS_DIRECTORY}/${SCRIPT_PATH##*/}"
    for frozen_input in "${RECIPE_PATH##*/}" "${PATCH_PATH##*/}" "${SCRIPT_PATH##*/}"; do
        compare_exact "published frozen ${frozen_input}" \
            "${first_output_root}/${FROZEN_INPUTS_DIRECTORY}/${frozen_input}" \
            "${second_output_root}/${FROZEN_INPUTS_DIRECTORY}/${frozen_input}"
    done
    [[ -s "${first_output_root}/${TRACE_OUTPUT_PATH}" && -s "${second_output_root}/${TRACE_OUTPUT_PATH}" ]] \
        || fail "each isolated build must retain trace evidence"

    rm -rf "${result_root}"
    mv "${first_output_root}" "${result_root}"
    first_output_root=""
    log "wrote embedded entry probe evidence to ${result_root}"
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
            host_build
            ;;
        *)
            fail "usage: packaging/postgresql/build-embedded.sh [--validate]"
            ;;
    esac
}

main "$@"
