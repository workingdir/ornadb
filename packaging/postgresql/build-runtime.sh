#!/bin/bash
set -euo pipefail

export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

readonly RECIPE_PATH="/repo/packaging/postgresql/runtime-build.toml"

log() {
    printf '[postgres-runtime] %s\n' "$*"
}

fail() {
    printf '[postgres-runtime] error: %s\n' "$*" >&2
    exit 1
}

reject_signing_inputs() {
    local name

    while IFS='=' read -r name _; do
        case "${name}" in
            *SIGNING*|*_SIGN_KEY*|*_SIGNATURE*|*PRIVATE_KEY*|ED25519*|COSIGN*|GPG_PRIVATE_KEY)
                fail "signing input ${name} is not permitted in the keyless runtime build"
                ;;
        esac
    done < <(env)
}

verify_sha256() {
    local expected="$1"
    local path="$2"
    local actual

    actual="$(sha256sum "${path}")"
    actual="${actual%% *}"
    test "${actual}" = "${expected}"
}

write_recipe_parser() {
    cat > /build/recipe_parser.py <<'PY_RECIPE_PARSER'
import ast
import pathlib
import re


BARE_KEY = re.compile(r"[A-Za-z0-9_-]+")
INTEGER = re.compile(r"-?(?:0|[1-9][0-9]*)")


def canonical_json(value):
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str):
        escaped = []
        escapes = {
            '"': '\\"',
            "\\": "\\\\",
            "\b": "\\b",
            "\f": "\\f",
            "\n": "\\n",
            "\r": "\\r",
            "\t": "\\t",
        }
        for character in value:
            if character in escapes:
                escaped.append(escapes[character])
            elif ord(character) < 0x20:
                escaped.append(f"\\u{ord(character):04x}")
            else:
                escaped.append(character)
        return '"' + "".join(escaped) + '"'
    if isinstance(value, list):
        return "[" + ",".join(canonical_json(item) for item in value) + "]"
    if isinstance(value, dict):
        if any(not isinstance(key, str) for key in value):
            raise TypeError("JSON object keys must be strings")
        return "{" + ",".join(
            canonical_json(key) + ":" + canonical_json(value[key])
            for key in sorted(value)
        ) + "}"
    raise TypeError(f"unsupported JSON value type: {type(value).__name__}")


def parse_value(value, line_number):
    if value.startswith('"'):
        try:
            parsed = ast.literal_eval(value)
        except (SyntaxError, ValueError) as error:
            raise ValueError(f"line {line_number}: invalid basic string") from error
        if not isinstance(parsed, str):
            raise ValueError(f"line {line_number}: expected a string")
        return parsed
    if value.startswith("["):
        try:
            parsed = ast.literal_eval(value)
        except (SyntaxError, ValueError) as error:
            raise ValueError(f"line {line_number}: invalid array") from error
        if not isinstance(parsed, list):
            raise ValueError(f"line {line_number}: expected an array")
        if any(not isinstance(item, str) for item in parsed):
            raise ValueError(f"line {line_number}: arrays can contain only strings")
        return parsed
    if INTEGER.fullmatch(value):
        return int(value)
    raise ValueError(f"line {line_number}: unsupported TOML value {value!r}")


def load_recipe(path):
    recipe_path = pathlib.Path(path)
    lines = recipe_path.read_text(encoding="utf-8").splitlines()
    root = {}
    current = root
    tables = set()
    index = 0

    while index < len(lines):
        line_number = index + 1
        stripped = lines[index].strip()
        index += 1
        if not stripped or stripped.startswith("#"):
            continue
        if "#" in stripped:
            raise ValueError(f"line {line_number}: inline comments are not accepted")
        if stripped.startswith("["):
            if not stripped.endswith("]") or stripped.startswith("[["):
                raise ValueError(f"line {line_number}: invalid or unsupported table")
            table_name = stripped[1:-1]
            parts = table_name.split(".")
            if not parts or any(BARE_KEY.fullmatch(part) is None for part in parts):
                raise ValueError(f"line {line_number}: invalid table name")
            table_key = tuple(parts)
            if table_key in tables:
                raise ValueError(f"line {line_number}: duplicate table {table_name}")
            tables.add(table_key)
            current = root
            for part in parts:
                existing = current.get(part)
                if existing is None:
                    existing = {}
                    current[part] = existing
                if not isinstance(existing, dict):
                    raise ValueError(f"line {line_number}: table conflicts with key {table_name}")
                current = existing
            continue
        if "=" not in stripped:
            raise ValueError(f"line {line_number}: expected a key/value pair")
        key, value = (part.strip() for part in stripped.split("=", 1))
        if BARE_KEY.fullmatch(key) is None:
            raise ValueError(f"line {line_number}: invalid key {key!r}")
        if key in current:
            raise ValueError(f"line {line_number}: duplicate key {key}")
        if value.startswith("[") and not value.endswith("]"):
            array_lines = [value]
            while index < len(lines):
                continuation = lines[index].strip()
                index += 1
                if not continuation or continuation.startswith("#"):
                    continue
                if "#" in continuation:
                    raise ValueError(f"line {index}: inline comments are not accepted")
                array_lines.append(continuation)
                if continuation == "]":
                    break
            if array_lines[-1] != "]":
                raise ValueError(f"line {line_number}: unterminated array")
            value = "\n".join(array_lines)
        current[key] = parse_value(value, line_number)

    return root


def _keys(table, expected, path):
    if type(table) is not dict:
        raise ValueError(f"{path} must be a table")
    actual = set(table)
    expected = set(expected)
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    if missing or unknown:
        raise ValueError(f"{path} keys differ; missing={missing!r} unknown={unknown!r}")


def _typed(value, expected_type, path):
    if type(value) is not expected_type:
        raise ValueError(
            f"{path} has type {type(value).__name__}; expected {expected_type.__name__}"
        )
    return value


def _string(value, path):
    value = _typed(value, str, path)
    if not value:
        raise ValueError(f"{path} must not be empty")
    return value


def _string_list(value, path):
    value = _typed(value, list, path)
    for index, item in enumerate(value):
        _string(item, f"{path}[{index}]")
    return value


def _digest(value, path):
    value = _string(value, path)
    if re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise ValueError(f"{path} must be a lowercase SHA-256 digest")


def validate_recipe(recipe, recipe_path):
    recipe_path = pathlib.Path(recipe_path)
    _keys(
        recipe,
        {
            "format", "runtime_identity", "prefix", "target", "platform",
            "source_date_epoch", "builder_image", "snapshot_timestamp", "patches",
            "postgresql", "zlib", "build", "host_abi", "elf", "apt", "archive",
        },
        "recipe",
    )
    if _typed(recipe["format"], int, "recipe.format") != 1:
        raise ValueError("recipe.format is not supported")
    identity = _string(recipe["runtime_identity"], "recipe.runtime_identity")
    identity_match = re.fullmatch(
        r"postgresql-([0-9]+\.[0-9]+)-debian12-amd64-orna\.([1-9][0-9]*)", identity
    )
    if identity_match is None:
        raise ValueError("recipe.runtime_identity has an unsupported form")
    prefix = _string(recipe["prefix"], "recipe.prefix")
    if prefix != f"/usr/lib/orna/postgresql/{identity}":
        raise ValueError("recipe.prefix does not close over recipe.runtime_identity")
    if recipe["target"] != "debian12-amd64" or recipe["platform"] != "linux/amd64":
        raise ValueError("recipe target/platform is not the supported Debian 12 amd64 target")
    epoch = _typed(recipe["source_date_epoch"], int, "recipe.source_date_epoch")
    if epoch <= 0:
        raise ValueError("recipe.source_date_epoch must be positive")
    builder_image = _string(recipe["builder_image"], "recipe.builder_image")
    if re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", builder_image) is None:
        raise ValueError("recipe.builder_image must use an immutable sha256 digest")
    timestamp = _string(recipe["snapshot_timestamp"], "recipe.snapshot_timestamp")
    if re.fullmatch(r"[0-9]{8}T[0-9]{6}Z", timestamp) is None:
        raise ValueError("recipe.snapshot_timestamp has an unsupported form")
    patches = _typed(recipe["patches"], list, "recipe.patches")
    if patches:
        raise ValueError("recipe.patches must remain empty for the unpatched runtime")

    postgresql = recipe["postgresql"]
    _keys(
        postgresql,
        {"version", "url", "sha256", "disabled_by_default", "configure_flags"},
        "recipe.postgresql",
    )
    pg_version = _string(postgresql["version"], "recipe.postgresql.version")
    if pg_version != identity_match.group(1):
        raise ValueError("recipe.postgresql.version does not match runtime_identity")
    expected_pg_url = f"https://ftp.postgresql.org/pub/source/v{pg_version}/postgresql-{pg_version}.tar.bz2"
    if postgresql["url"] != expected_pg_url:
        raise ValueError("recipe.postgresql.url is not the official versioned source URL")
    _digest(postgresql["sha256"], "recipe.postgresql.sha256")
    disabled_by_default = _string_list(
        postgresql["disabled_by_default"], "recipe.postgresql.disabled_by_default"
    )
    if disabled_by_default != ["ssl", "uuid"]:
        raise ValueError(
            "recipe.postgresql.disabled_by_default must record the exact upstream-default closure"
        )
    configure_flags = _string_list(
        postgresql["configure_flags"], "recipe.postgresql.configure_flags"
    )
    if len(configure_flags) != len(set(configure_flags)):
        raise ValueError("recipe.postgresql.configure_flags contains a duplicate")
    for flag in configure_flags:
        if not flag.startswith("--"):
            raise ValueError(f"invalid PostgreSQL configure flag {flag!r}")
    required_flags = {
        f"--prefix={prefix}",
        "--build=x86_64-linux-gnu",
        "--host=x86_64-linux-gnu",
        "--disable-nls",
        "--disable-rpath",
        "--disable-debug",
        "--disable-profiling",
        "--disable-coverage",
        "--disable-dtrace",
        "--disable-tap-tests",
        "--disable-injection-points",
        "--disable-cassert",
        "--without-tcl",
        "--without-perl",
        "--without-python",
        "--without-gssapi",
        "--without-pam",
        "--without-bsd-auth",
        "--without-ldap",
        "--without-bonjour",
        "--without-selinux",
        "--without-systemd",
        "--without-readline",
        "--without-liburing",
        "--without-libcurl",
        "--without-libnuma",
        "--without-libxml",
        "--without-libxslt",
        "--without-lz4",
        "--without-zstd",
        "--without-icu",
        "--without-llvm",
        "--with-zlib",
        "--with-pgport=5432",
        "--with-blocksize=8",
        "--with-segsize=1",
        "--with-wal-blocksize=8",
    }
    missing_flags = sorted(required_flags - set(configure_flags))
    if missing_flags:
        raise ValueError(f"recipe.postgresql.configure_flags misses {missing_flags!r}")
    unknown_flags = sorted(set(configure_flags) - required_flags)
    if unknown_flags:
        raise ValueError(f"recipe.postgresql.configure_flags has unknown flags {unknown_flags!r}")
    if any("system-tzdata" in flag for flag in configure_flags):
        raise ValueError("system tzdata flags are forbidden; PostgreSQL must retain its private data")

    zlib = recipe["zlib"]
    _keys(zlib, {"version", "url", "sha256", "build_prefix"}, "recipe.zlib")
    zlib_version = _string(zlib["version"], "recipe.zlib.version")
    if zlib["url"] != f"https://zlib.net/zlib-{zlib_version}.tar.gz":
        raise ValueError("recipe.zlib.url is not the official versioned source URL")
    _digest(zlib["sha256"], "recipe.zlib.sha256")
    if zlib["build_prefix"] != "/build/zlib":
        raise ValueError("recipe.zlib.build_prefix must use the fixed /build closure")

    build = recipe["build"]
    _keys(build, {"jobs", "environment"}, "recipe.build")
    if _typed(build["jobs"], int, "recipe.build.jobs") != 1:
        raise ValueError("recipe.build.jobs must be 1 for reproducibility")
    environment = build["environment"]
    environment_keys = {
        "AR", "ARFLAGS", "CC", "CFLAGS", "CONFIG_SITE", "CPPFLAGS", "LANG",
        "LC_ALL", "LDFLAGS", "PKG_CONFIG_LIBDIR", "RANLIB", "TZ",
    }
    _keys(environment, environment_keys, "recipe.build.environment")
    for key in environment_keys:
        _string(environment[key], f"recipe.build.environment.{key}")
    if environment["LANG"] != "C.UTF-8" or environment["LC_ALL"] != "C.UTF-8":
        raise ValueError("recipe.build.environment must pin the C.UTF-8 locale")
    if environment["TZ"] != "UTC0" or environment["CONFIG_SITE"] != "/dev/null":
        raise ValueError("recipe.build.environment must isolate timezone and CONFIG_SITE")
    for token in ("-ffile-prefix-map=/build=.", "-fdebug-prefix-map=/build=.", "-Wdate-time"):
        if token not in environment["CFLAGS"].split():
            raise ValueError(f"recipe.build.environment.CFLAGS misses {token}")
    if "-Wl,--build-id=none" not in environment["LDFLAGS"].split():
        raise ValueError("recipe.build.environment.LDFLAGS must suppress the build id")

    host_abi = recipe["host_abi"]
    _keys(
        host_abi,
        {
            "architecture", "debian", "elf_class", "elf_data", "elf_machine",
            "interpreter", "permitted_unbundled_needed",
        },
        "recipe.host_abi",
    )
    expected_host_abi = {
        "architecture": "amd64",
        "debian": "12",
        "elf_class": "ELF64",
        "elf_data": "2's complement, little endian",
        "elf_machine": "Advanced Micro Devices X86-64",
        "interpreter": "/lib64/ld-linux-x86-64.so.2",
        "permitted_unbundled_needed": [
            "libc.so.6",
            "libdl.so.2",
            "libm.so.6",
            "libpthread.so.0",
            "libresolv.so.2",
            "librt.so.1",
            "libutil.so.1",
        ],
    }
    if host_abi != expected_host_abi:
        raise ValueError("recipe.host_abi does not match the exact accepted Debian 12 amd64 ABI")

    elf = recipe["elf"]
    _keys(elf, {"runpaths"}, "recipe.elf")
    runpaths = elf["runpaths"]
    _keys(runpaths, {"bin", "lib", "postgresql"}, "recipe.elf.runpaths")
    supported_runpaths = {
        "bin": "$ORIGIN/../lib", "lib": "$ORIGIN", "postgresql": "$ORIGIN/..",
    }
    if runpaths != supported_runpaths:
        raise ValueError("recipe.elf.runpaths does not match the private runtime layout")

    apt = recipe["apt"]
    _keys(apt, {"sources", "packages"}, "recipe.apt")
    sources = _string_list(apt["sources"], "recipe.apt.sources")
    if len(sources) != len(set(sources)):
        raise ValueError("recipe.apt.sources contains a duplicate")
    source_pattern = re.compile(
        r"deb http://snapshot\.debian\.org/archive/(debian|debian-security)/"
        + re.escape(timestamp)
        + r" (bookworm|bookworm-updates|bookworm-security) main"
    )
    source_roles = set()
    for source in sources:
        match = source_pattern.fullmatch(source)
        if match is None:
            raise ValueError(f"recipe.apt.sources has a mutable or unsupported source {source!r}")
        source_roles.add(match.groups())
    expected_roles = {
        ("debian", "bookworm"),
        ("debian", "bookworm-updates"),
        ("debian-security", "bookworm-security"),
    }
    if source_roles != expected_roles:
        raise ValueError("recipe.apt.sources does not contain the exact Debian snapshot suites")
    packages = apt["packages"]
    if type(packages) is not dict or not packages:
        raise ValueError("recipe.apt.packages must be a non-empty table")
    required_packages = {
        "gcc", "make", "libc6-dev", "bison", "flex", "perl", "python3-minimal",
        "patchelf", "binutils", "curl", "bzip2", "ca-certificates", "xz-utils", "file",
    }
    missing_packages = sorted(required_packages - set(packages))
    if missing_packages:
        raise ValueError(f"recipe.apt.packages misses {missing_packages!r}")
    for package, version in packages.items():
        if re.fullmatch(r"[a-z0-9][a-z0-9+.-]*", package) is None:
            raise ValueError(f"invalid apt package name {package!r}")
        version = _string(version, f"recipe.apt.packages.{package}")
        if any(character.isspace() for character in version):
            raise ValueError(f"recipe.apt.packages.{package} contains whitespace")

    archive = recipe["archive"]
    _keys(archive, {"format", "compression_level", "threads", "check"}, "recipe.archive")
    if archive["format"] != "tar.xz":
        raise ValueError("recipe.archive.format is not supported")
    level = _typed(archive["compression_level"], int, "recipe.archive.compression_level")
    if level < 0 or level > 9:
        raise ValueError("recipe.archive.compression_level is outside 0..9")
    if archive["threads"] != 1 or archive["check"] != "crc64":
        raise ValueError("recipe.archive must use one xz thread and crc64")

    patch_root = recipe_path.parent / "patches"
    if patch_root.exists():
        unrecorded = sorted(
            str(path.relative_to(recipe_path.parent))
            for path in patch_root.rglob("*")
            if path.is_file() or path.is_symlink()
        )
        if unrecorded:
            raise ValueError("unrecorded PostgreSQL patches are not permitted: " + ", ".join(unrecorded))


def shell_quote(value):
    return "'" + str(value).replace("'", "'\"'\"'") + "'"


def write_shell_exports(recipe, path):
    scalar_values = {
        "RECIPE_FORMAT": recipe["format"],
        "RUNTIME_IDENTITY": recipe["runtime_identity"],
        "RUNTIME_PREFIX": recipe["prefix"],
        "RUNTIME_TARGET": recipe["target"],
        "RUNTIME_PLATFORM": recipe["platform"],
        "SOURCE_DATE_EPOCH_VALUE": recipe["source_date_epoch"],
        "BUILDER_IMAGE": recipe["builder_image"],
        "SNAPSHOT_TIMESTAMP": recipe["snapshot_timestamp"],
        "POSTGRESQL_VERSION": recipe["postgresql"]["version"],
        "POSTGRESQL_URL": recipe["postgresql"]["url"],
        "POSTGRESQL_SHA256": recipe["postgresql"]["sha256"],
        "ZLIB_VERSION": recipe["zlib"]["version"],
        "ZLIB_URL": recipe["zlib"]["url"],
        "ZLIB_SHA256": recipe["zlib"]["sha256"],
        "ZLIB_BUILD_PREFIX": recipe["zlib"]["build_prefix"],
        "BUILD_JOBS": recipe["build"]["jobs"],
        "HOST_ABI_ELF_CLASS": recipe["host_abi"]["elf_class"],
        "HOST_ABI_ELF_DATA": recipe["host_abi"]["elf_data"],
        "HOST_ABI_ELF_MACHINE": recipe["host_abi"]["elf_machine"],
        "HOST_ABI_INTERPRETER": recipe["host_abi"]["interpreter"],
        "ELF_RUNPATH_BIN": recipe["elf"]["runpaths"]["bin"],
        "ELF_RUNPATH_LIB": recipe["elf"]["runpaths"]["lib"],
        "ELF_RUNPATH_POSTGRESQL": recipe["elf"]["runpaths"]["postgresql"],
        "ARCHIVE_COMPRESSION_LEVEL": recipe["archive"]["compression_level"],
        "ARCHIVE_THREADS": recipe["archive"]["threads"],
        "ARCHIVE_CHECK": recipe["archive"]["check"],
        "ARCHIVE_NAME": recipe["runtime_identity"] + ".tar.xz",
    }
    arrays = {
        "POSTGRESQL_CONFIGURE_FLAGS": recipe["postgresql"]["configure_flags"],
        "RECIPE_BUILD_ENVIRONMENT": [
            f"{key}={value}" for key, value in sorted(recipe["build"]["environment"].items())
        ],
        "APT_SOURCES": recipe["apt"]["sources"],
        "APT_PACKAGES": [
            f"{key}={value}" for key, value in sorted(recipe["apt"]["packages"].items())
        ],
        "PERMITTED_UNBUNDLED_NEEDED": recipe["host_abi"]["permitted_unbundled_needed"],
    }
    lines = [f"{key}={shell_quote(value)}" for key, value in scalar_values.items()]
    for key, values in arrays.items():
        lines.append(f"{key}=(" + " ".join(shell_quote(value) for value in values) + ")")
    pathlib.Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
PY_RECIPE_PARSER
    chmod 0644 /build/recipe_parser.py
}

validate_and_load_recipe() {
    python3 - "${RECIPE_PATH}" /build/runtime-recipe.sh <<'PY_VALIDATE_RECIPE'
import pathlib
import sys
from recipe_parser import load_recipe, validate_recipe, write_shell_exports

recipe_path = pathlib.Path(sys.argv[1])
try:
    recipe = load_recipe(recipe_path)
    validate_recipe(recipe, recipe_path)
except ValueError as error:
    raise SystemExit(f"[postgres-runtime] error: invalid runtime-build.toml: {error}") from error
write_shell_exports(recipe, sys.argv[2])
print("[postgres-runtime] validated the complete pinned runtime recipe")
PY_VALIDATE_RECIPE
    # This file is generated only from the strictly typed and validated recipe.
    source /build/runtime-recipe.sh
}

run_recipe_self_tests() {
    python3 - "${RECIPE_PATH}" <<'PY_RECIPE_SELF_TESTS'
import pathlib
import sys
from recipe_parser import load_recipe, validate_recipe

source_path = pathlib.Path(sys.argv[1])
test_root = pathlib.Path("/build/recipe-self-tests")
test_root.mkdir(mode=0o755, exist_ok=False)
source_text = source_path.read_text(encoding="utf-8")


def expect_failure(name, text, add_patch=False):
    case_root = test_root / name
    case_root.mkdir(mode=0o755)
    case_path = case_root / "runtime-build.toml"
    case_path.write_text(text, encoding="utf-8", newline="\n")
    if add_patch:
        patch_root = case_root / "patches"
        patch_root.mkdir(mode=0o755)
        (patch_root / "unrecorded.patch").write_text("not recorded\n", encoding="utf-8")
    try:
        validate_recipe(load_recipe(case_path), case_path)
    except ValueError:
        return
    raise SystemExit(f"[postgres-runtime] error: recipe negative self-test {name} did not fail")


validate_recipe(load_recipe(source_path), source_path)
expect_failure("unknown-key", source_text.replace("format = 1\n", "format = 1\nunknown = 1\n", 1))
expect_failure(
    "prefix-drift",
    source_text.replace(
        'prefix = "/usr/lib/orna/postgresql/',
        'prefix = "/tmp/not-the-private-prefix/',
        1,
    ),
)
expect_failure(
    "default-disabled-drift",
    source_text.replace(
        'disabled_by_default = ["ssl", "uuid"]',
        'disabled_by_default = ["uuid"]',
        1,
    ),
)
expect_failure(
    "enabled-openssl",
    source_text.replace(
        "configure_flags = [\n",
        'configure_flags = [\n    "--with-ssl=openssl",\n',
        1,
    ),
)
expect_failure(
    "abi-allowlist-drift",
    source_text.replace('    "libutil.so.1",\n', "", 1),
)
expect_failure("unrecorded-patch", source_text, add_patch=True)
print("[postgres-runtime] recipe schema, drift, and unrecorded-patch negative self-tests passed")
PY_RECIPE_SELF_TESTS
}

write_elf_closure_module() {
    cat > /build/elf_closure.py <<'PY_ELF_CLOSURE'
import pathlib


class ClosureError(ValueError):
    pass


def normalise_relative(path):
    path = pathlib.PurePosixPath(path)
    if path.is_absolute():
        raise ClosureError(f"absolute private path {path}")
    parts = []
    for part in path.parts:
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                raise ClosureError(f"private path escapes its root: {path}")
            parts.pop()
        else:
            parts.append(part)
    return pathlib.PurePosixPath(*parts)


def follow_symlink_chain(start, inventory):
    current = normalise_relative(start)
    visited = []
    while True:
        current_text = current.as_posix()
        if current_text in visited:
            chain = " -> ".join([*visited, current_text])
            raise ClosureError(f"symbolic-link cycle: {chain}")
        visited.append(current_text)
        entry = inventory.get(current_text)
        if entry is None:
            raise ClosureError(f"dangling symbolic-link chain at {current_text}")
        if entry["type"] != "symbolic-link":
            return current_text
        target = pathlib.PurePosixPath(entry["link_target"])
        if target.is_absolute():
            raise ClosureError(f"{current_text} has an absolute link target")
        current = normalise_relative(current.parent / target)


def expand_runpath(member, runpath):
    member = normalise_relative(member)
    origin = member.parent
    directories = []
    for raw_directory in runpath.split(":"):
        if not raw_directory:
            raise ClosureError(f"{member}: DT_RUNPATH contains an empty directory")
        expanded = raw_directory.replace("${ORIGIN}", origin.as_posix())
        expanded = expanded.replace("$ORIGIN", origin.as_posix())
        if "$" in expanded:
            raise ClosureError(f"{member}: DT_RUNPATH contains an unsupported token")
        directory = normalise_relative(expanded)
        if directory in directories:
            raise ClosureError(f"{member}: DT_RUNPATH contains a duplicate directory")
        directories.append(directory)
    return directories


def resolve_needed(member, runpath, needed, permitted, inventory, elf_members):
    if "/" in needed or not needed:
        raise ClosureError(f"{member}: invalid DT_NEEDED name {needed!r}")
    candidates = []
    for directory in expand_runpath(member, runpath):
        candidate = normalise_relative(directory / needed).as_posix()
        if candidate in inventory:
            candidates.append(candidate)

    if needed in permitted:
        if candidates:
            raise ClosureError(
                f"{member}: permitted system dependency {needed!r} is shadowed by {candidates!r}"
            )
        return {"kind": "system", "name": needed}

    if not candidates:
        raise ClosureError(f"{member}: DT_NEEDED {needed!r} is missing from its DT_RUNPATH")
    if len(candidates) != 1:
        raise ClosureError(
            f"{member}: DT_NEEDED {needed!r} has duplicate RUNPATH candidates {candidates!r}"
        )

    concrete = follow_symlink_chain(candidates[0], inventory)
    if inventory[concrete]["type"] != "file":
        raise ClosureError(f"{member}: DT_NEEDED {needed!r} resolves to non-file {concrete}")
    metadata = elf_members.get(concrete)
    if metadata is None:
        raise ClosureError(f"{member}: DT_NEEDED {needed!r} resolves to non-ELF {concrete}")
    if metadata.get("soname") != needed:
        raise ClosureError(
            f"{member}: DT_NEEDED {needed!r} resolves to {concrete} with DT_SONAME "
            f"{metadata.get('soname')!r}"
        )
    return {
        "kind": "private",
        "member": concrete,
        "name": needed,
        "runpath_member": candidates[0],
    }


def validate_all_symlinks(inventory):
    for path, entry in sorted(inventory.items()):
        if entry["type"] == "symbolic-link":
            follow_symlink_chain(path, inventory)
PY_ELF_CLOSURE
    chmod 0644 /build/elf_closure.py
}

run_elf_closure_self_tests() {
    python3 - <<'PY_ELF_SELF_TESTS'
from elf_closure import ClosureError, resolve_needed, validate_all_symlinks


def expect_failure(name, callback):
    try:
        callback()
    except ClosureError:
        return
    raise SystemExit(f"[postgres-runtime] error: ELF closure negative self-test {name} did not fail")


base_inventory = {
    "bin/postgres": {"type": "file"},
    "lib/libpq.so.5": {"type": "symbolic-link", "link_target": "libpq.so.5.18"},
    "lib/libpq.so.5.18": {"type": "file"},
}
base_elf = {
    "bin/postgres": {"soname": None},
    "lib/libpq.so.5.18": {"soname": "libpq.so.5"},
}
resolved = resolve_needed(
    "bin/postgres", "$ORIGIN/../lib", "libpq.so.5", set(), base_inventory, base_elf
)
if resolved["member"] != "lib/libpq.so.5.18":
    raise SystemExit("[postgres-runtime] error: ELF closure positive self-test resolved wrongly")
validate_all_symlinks(base_inventory)

expect_failure(
    "missing",
    lambda: resolve_needed("bin/postgres", "$ORIGIN/../lib", "libmissing.so.1", set(), base_inventory, base_elf),
)
duplicate_inventory = dict(base_inventory)
duplicate_inventory["alt/libpq.so.5"] = {"type": "file"}
duplicate_elf = dict(base_elf)
duplicate_elf["alt/libpq.so.5"] = {"soname": "libpq.so.5"}
expect_failure(
    "duplicate",
    lambda: resolve_needed(
        "bin/postgres", "$ORIGIN/../lib:$ORIGIN/../alt", "libpq.so.5",
        set(), duplicate_inventory, duplicate_elf,
    ),
)
expect_failure(
    "non-elf",
    lambda: resolve_needed("bin/postgres", "$ORIGIN/../lib", "libpq.so.5", set(), base_inventory, {"bin/postgres": {"soname": None}}),
)
wrong_soname = dict(base_elf)
wrong_soname["lib/libpq.so.5.18"] = {"soname": "libpq.so.6"}
expect_failure(
    "wrong-soname",
    lambda: resolve_needed("bin/postgres", "$ORIGIN/../lib", "libpq.so.5", set(), base_inventory, wrong_soname),
)
dangling = dict(base_inventory)
dangling["lib/libpq.so.5"] = {"type": "symbolic-link", "link_target": "absent.so"}
expect_failure("dangling-link", lambda: validate_all_symlinks(dangling))
cycle = dict(base_inventory)
cycle["lib/libpq.so.5"] = {"type": "symbolic-link", "link_target": "libpq-loop.so"}
cycle["lib/libpq-loop.so"] = {"type": "symbolic-link", "link_target": "libpq.so.5"}
expect_failure("link-cycle", lambda: validate_all_symlinks(cycle))
escape = dict(base_inventory)
escape["lib/libpq.so.5"] = {"type": "symbolic-link", "link_target": "../../outside"}
expect_failure("link-escape", lambda: validate_all_symlinks(escape))
print("[postgres-runtime] ELF missing/duplicate/non-ELF/SONAME/symlink negative self-tests passed")
PY_ELF_SELF_TESTS
}

bootstrap_pinned_python() {
    local python_version
    local source

    log "bootstrapping the recipe parser from the recipe's immutable Debian snapshots"
    if ! awk '
        $0 == "[apt]" { in_apt = 1; next }
        in_apt && /^\[/ { in_apt = 0 }
        in_apt && /^[[:space:]]*sources[[:space:]]*=[[:space:]]*\[[[:space:]]*$/ {
            in_sources = 1
            next
        }
        in_sources {
            line = $0
            sub(/^[[:space:]]*/, "", line)
            sub(/[[:space:]]*$/, "", line)
            if (line == "]") {
                found_end = 1
                in_sources = 0
                next
            }
            if (line !~ /^"[^"]+",?$/) {
                exit 20
            }
            sub(/^"/, "", line)
            sub(/",?$/, "", line)
            print line
            found_source = 1
        }
        END {
            if (!found_source || !found_end || in_sources) {
                exit 21
            }
        }
    ' "${RECIPE_PATH}" > /build/bootstrap-apt-sources; then
        fail "could not parse apt.sources for the pinned-Python bootstrap"
    fi

    if ! python_version="$(
        awk '
            $0 == "[apt.packages]" { in_packages = 1; next }
            in_packages && /^\[/ { in_packages = 0 }
            in_packages && /^[[:space:]]*python3-minimal[[:space:]]*=/ {
                line = $0
                sub(/^[^=]*=[[:space:]]*"/, "", line)
                sub(/"[[:space:]]*$/, "", line)
                print line
                count += 1
            }
            END { if (count != 1) exit 22 }
        ' "${RECIPE_PATH}"
    )"; then
        fail "could not parse the exact python3-minimal version for the recipe bootstrap"
    fi
    test -n "${python_version}" || fail "could not bootstrap the pinned recipe Python"

    rm -f /etc/apt/sources.list.d/debian.sources
    : > /etc/apt/sources.list
    while IFS= read -r source; do
        case "${source}" in
            "deb http://snapshot.debian.org/archive/debian/"*|\
            "deb http://snapshot.debian.org/archive/debian-security/"*)
                ;;
            *)
                fail "bootstrap apt source is not an immutable Debian snapshot: ${source}"
                ;;
        esac
        printf '%s\n' "${source}" >> /etc/apt/sources.list
    done < /build/bootstrap-apt-sources

    apt-get \
        -o Acquire::Check-Valid-Until=false \
        -o Acquire::Retries=3 \
        -o APT::Get::Assume-Yes=true \
        update
    DEBIAN_FRONTEND=noninteractive apt-get \
        -o Acquire::Check-Valid-Until=false \
        -o APT::Get::Assume-Yes=true \
        -o APT::Install-Recommends=false \
        -o Dpkg::Use-Pty=0 \
        install "python3-minimal=${python_version}"
    test "$(dpkg-query -W -f='${Version}' python3-minimal)" = "${python_version}" \
        || fail "the bootstrap recipe Python version drifted"
}

install_build_dependencies() {
    local source
    local -a package_arguments

    log "installing every exact package recorded by the validated recipe"
    : > /etc/apt/sources.list
    for source in "${APT_SOURCES[@]}"; do
        printf '%s\n' "${source}" >> /etc/apt/sources.list
    done
    package_arguments=("${APT_PACKAGES[@]}")
    DEBIAN_FRONTEND=noninteractive apt-get \
        -o Acquire::Check-Valid-Until=false \
        -o APT::Get::Assume-Yes=true \
        -o APT::Install-Recommends=false \
        -o Dpkg::Use-Pty=0 \
        install "${package_arguments[@]}"
}

verify_installed_packages() {
    local package
    local pinned
    local version

    for pinned in "${APT_PACKAGES[@]}"; do
        package="${pinned%%=*}"
        version="${pinned#*=}"
        test "$(dpkg-query -W -f='${Version}' "${package}")" = "${version}" \
            || fail "installed version drift for ${package}"
    done
    log "verified all explicitly installed package versions"
}

recipe_env() {
    env -i \
        PATH="${PATH}" \
        SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        "$@"
}

fetch_sources() {
    log "fetching and hashing PostgreSQL ${POSTGRESQL_VERSION} and zlib ${ZLIB_VERSION}"
    recipe_env \
        curl --fail --location --proto '=https' --tlsv1.2 \
        --output /build/postgresql-source.tar.bz2 "${POSTGRESQL_URL}"
    recipe_env \
        curl --fail --location --proto '=https' --tlsv1.2 \
        --output /build/zlib-source.tar.gz "${ZLIB_URL}"

    verify_sha256 "${POSTGRESQL_SHA256}" /build/postgresql-source.tar.bz2 \
        || fail "PostgreSQL source digest does not match the recipe"
    verify_sha256 "${ZLIB_SHA256}" /build/zlib-source.tar.gz \
        || fail "zlib source digest does not match the recipe"

    cp /build/postgresql-source.tar.bz2 /build/postgresql-source.changed.tar.bz2
    printf '\000' | dd \
        of=/build/postgresql-source.changed.tar.bz2 \
        bs=1 \
        seek=0 \
        count=1 \
        conv=notrunc \
        status=none
    if verify_sha256 "${POSTGRESQL_SHA256}" /build/postgresql-source.changed.tar.bz2; then
        fail "the changed PostgreSQL source passed the source digest gate"
    fi
    rm -f /build/postgresql-source.changed.tar.bz2
    log "proved that one changed PostgreSQL source byte fails before configure"
}

build_zlib() {
    log "building the private shared zlib"
    mkdir -p /build/zlib-source "${ZLIB_BUILD_PREFIX}"
    tar --extract --gzip --file=/build/zlib-source.tar.gz --strip-components=1 --directory=/build/zlib-source
    (
        cd /build/zlib-source
        recipe_env ./configure --shared "--prefix=${ZLIB_BUILD_PREFIX}"
        recipe_env make -j"${BUILD_JOBS}"
        recipe_env make -j"${BUILD_JOBS}" install
    )
}

build_postgresql() {
    log "configuring PostgreSQL out of tree at the fixed /build path"
    mkdir -p /build/postgresql-source /build/postgresql-build /build/stage
    tar --extract --bzip2 --file=/build/postgresql-source.tar.bz2 \
        --strip-components=1 \
        --directory=/build/postgresql-source
    (
        cd /build/postgresql-build
        recipe_env /build/postgresql-source/configure "${POSTGRESQL_CONFIGURE_FLAGS[@]}"
    )

    log "building PostgreSQL reproducibly"
    recipe_env make -C /build/postgresql-build -j"${BUILD_JOBS}"

    log "running make check as uid 65534, not as root"
    chown -R 65534:65534 /build/postgresql-source /build/postgresql-build
    setpriv --reuid=65534 --regid=65534 --clear-groups \
        env -i \
        PATH="${PATH}" \
        SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
        "${RECIPE_BUILD_ENVIRONMENT[@]}" \
        make -C /build/postgresql-build -j"${BUILD_JOBS}" check

    log "installing stripped PostgreSQL files"
    recipe_env make -C /build/postgresql-build -j"${BUILD_JOBS}" install-strip DESTDIR=/build/stage
}

create_evidence() {
    local runtime_root="/build/stage${RUNTIME_PREFIX}"

    log "creating the private dependency and evidence closure"
    mkdir -p "${runtime_root}/lib" "${runtime_root}/libexec"
    cp -a \
        "${ZLIB_BUILD_PREFIX}/lib/libz.so.1" \
        "${ZLIB_BUILD_PREFIX}/lib/libz.so.${ZLIB_VERSION}" \
        "${runtime_root}/lib/"
    rm -rf \
        "${runtime_root}/include" \
        "${runtime_root}/lib/pgxs" \
        "${runtime_root}/lib/pkgconfig"
    find "${runtime_root}" -type f \( -name '*.a' -o -name '*.la' \) -delete

    cp /build/postgresql-source/COPYRIGHT "${runtime_root}/POSTGRESQL-LICENSE"
    cat > "${runtime_root}/libexec/locale" <<'EOF'
#!/bin/sh
exec /usr/bin/locale "$@"
EOF

    python3 - "${runtime_root}" "${RECIPE_PATH}" <<'PY_WRITE_SBOM'
import pathlib
import sys
import time
from recipe_parser import canonical_json, load_recipe

runtime_root = pathlib.Path(sys.argv[1])
recipe_path = pathlib.Path(sys.argv[2])
recipe = load_recipe(recipe_path)

created = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(recipe["source_date_epoch"]))
identity = recipe["runtime_identity"]
document = {
    "SPDXID": "SPDXRef-DOCUMENT",
    "creationInfo": {
        "created": created,
        "creators": ["Tool: Orna deterministic PostgreSQL runtime builder"],
    },
    "dataLicense": "CC0-1.0",
    "documentNamespace": f"https://orna.dev/spdx/{identity}",
    "name": identity,
    "packages": [
        {
            "SPDXID": "SPDXRef-Package-PostgreSQL",
            "checksums": [
                {
                    "algorithm": "SHA256",
                    "checksumValue": recipe["postgresql"]["sha256"],
                }
            ],
            "downloadLocation": recipe["postgresql"]["url"],
            "filesAnalyzed": False,
            "licenseConcluded": "PostgreSQL",
            "licenseDeclared": "PostgreSQL",
            "name": "PostgreSQL",
            "versionInfo": recipe["postgresql"]["version"],
        },
        {
            "SPDXID": "SPDXRef-Package-zlib",
            "checksums": [
                {
                    "algorithm": "SHA256",
                    "checksumValue": recipe["zlib"]["sha256"],
                }
            ],
            "downloadLocation": recipe["zlib"]["url"],
            "filesAnalyzed": False,
            "licenseConcluded": "Zlib",
            "licenseDeclared": "Zlib",
            "name": "zlib",
            "versionInfo": recipe["zlib"]["version"],
        },
    ],
    "relationships": [
        {
            "relatedSpdxElement": "SPDXRef-Package-PostgreSQL",
            "relationshipType": "DESCRIBES",
            "spdxElementId": "SPDXRef-DOCUMENT",
        },
        {
            "relatedSpdxElement": "SPDXRef-Package-zlib",
            "relationshipType": "DESCRIBES",
            "spdxElementId": "SPDXRef-DOCUMENT",
        },
    ],
    "spdxVersion": "SPDX-2.3",
}

sbom_path = runtime_root / "sbom.spdx.json"
with sbom_path.open("w", encoding="utf-8", newline="\n") as sbom_file:
    sbom_file.write(canonical_json(document) + "\n")
PY_WRITE_SBOM
}

set_private_runpaths() {
    local runtime_root="/build/stage${RUNTIME_PREFIX}"
    local path
    local relative
    local runpath

    log "setting private relative ELF RUNPATH values"
    while IFS= read -r -d '' path; do
        if ! readelf --file-header "${path}" >/dev/null 2>&1; then
            continue
        fi
        relative="${path#${runtime_root}/}"
        case "${relative}" in
            bin/*)
                runpath="${ELF_RUNPATH_BIN}"
                ;;
            lib/postgresql/*)
                runpath="${ELF_RUNPATH_POSTGRESQL}"
                ;;
            lib/*)
                runpath="${ELF_RUNPATH_LIB}"
                ;;
            *)
                fail "ELF member has no accepted private location: ${relative}"
                ;;
        esac
        patchelf --remove-rpath "${path}"
        patchelf --set-rpath "${runpath}" "${path}"
    done < <(find "${runtime_root}" -type f -print0 | sort -z)
}

normalise_tree() {
    local runtime_root="/build/stage${RUNTIME_PREFIX}"
    local path

    log "normalising private-tree owners, modes, and timestamps"
    while IFS= read -r -d '' path; do
        chown -h 0:0 "${path}"
    done < <(find "${runtime_root}" -depth -print0)
    find "${runtime_root}" -type d -exec chmod 0755 {} +
    find "${runtime_root}" -type f -exec chmod 0644 {} +
    find "${runtime_root}/bin" -type f -exec chmod 0755 {} +
    chmod 0755 "${runtime_root}/libexec/locale"
    while IFS= read -r -d '' path; do
        touch -h -d "@${SOURCE_DATE_EPOCH_VALUE}" "${path}"
    done < <(find "${runtime_root}" -depth -print0)
}

write_and_validate_manifest() {
    local runtime_root="/build/stage${RUNTIME_PREFIX}"

    log "validating the ELF closure and writing the canonical manifest"
    python3 - \
        "${runtime_root}" \
        "${RECIPE_PATH}" \
        /repo/packaging/postgresql/build-runtime.sh <<'PY_WRITE_MANIFEST'
import os
import pathlib
import re
import stat
import subprocess
import sys
from elf_closure import ClosureError, resolve_needed, validate_all_symlinks
from recipe_parser import canonical_json, load_recipe

runtime_root = pathlib.Path(sys.argv[1])
recipe_path = pathlib.Path(sys.argv[2])
script_path = pathlib.Path(sys.argv[3])
manifest_path = runtime_root / "orna-runtime-manifest.json"
signature_path = runtime_root / "orna-runtime-manifest.sig"

if manifest_path.exists() or signature_path.exists():
    raise SystemExit("[postgres-runtime] error: manifest or signature placeholder exists before inventory")

recipe = load_recipe(recipe_path)
permitted_unbundled = set(recipe["host_abi"]["permitted_unbundled_needed"])


def sha256_bytes(value):
    return subprocess.run(
        ["sha256sum"],
        check=True,
        input=value,
        stdout=subprocess.PIPE,
    ).stdout.decode("ascii").split()[0]


def sha256_file(path):
    return subprocess.run(
        ["sha256sum", str(path)],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout.split()[0]


def inspect_elf(path, relative):
    header = subprocess.run(
        ["readelf", "--file-header", str(path)],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout
    def header_value(name):
        match = re.search(r"^\s*" + re.escape(name) + r":\s*(.*?)\s*$", header, re.MULTILINE)
        if match is None:
            raise ValueError(f"{relative}: readelf omitted {name}")
        return match.group(1)

    expected_header = {
        "Class": recipe["host_abi"]["elf_class"],
        "Data": recipe["host_abi"]["elf_data"],
        "Machine": recipe["host_abi"]["elf_machine"],
    }
    for field, expected in expected_header.items():
        actual = header_value(field)
        if actual != expected:
            raise ValueError(f"{relative}: ELF {field} is {actual!r}; expected {expected!r}")

    program_headers = subprocess.run(
        ["readelf", "--program-headers", "--wide", str(path)],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout
    interpreter_match = re.search(r"Requesting program interpreter: ([^]]+)", program_headers)
    interpreter = interpreter_match.group(1) if interpreter_match else None

    dynamic = subprocess.run(
        ["readelf", "--dynamic", "--wide", str(path)],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout
    if "(RPATH)" in dynamic:
        raise ValueError(f"{relative}: DT_RPATH is not permitted")
    runpath_matches = re.findall(r"\(RUNPATH\).*Library runpath: \[([^]]*)\]", dynamic)
    if len(runpath_matches) != 1:
        raise ValueError(f"{relative}: expected one DT_RUNPATH, found {len(runpath_matches)}")
    runpath = runpath_matches[0]
    needed = re.findall(r"\(NEEDED\).*Shared library: \[([^]]+)\]", dynamic)
    soname_matches = re.findall(r"\(SONAME\).*Library soname: \[([^]]+)\]", dynamic)
    if len(soname_matches) > 1:
        raise ValueError(f"{relative}: expected at most one DT_SONAME, found {len(soname_matches)}")
    soname = soname_matches[0] if soname_matches else None

    relative_text = relative.as_posix()
    if relative_text.startswith("bin/"):
        expected_runpath = recipe["elf"]["runpaths"]["bin"]
        if interpreter != recipe["host_abi"]["interpreter"]:
            raise ValueError(f"{relative}: invalid executable interpreter {interpreter!r}")
    elif relative_text.startswith("lib/postgresql/"):
        expected_runpath = recipe["elf"]["runpaths"]["postgresql"]
        if interpreter is not None:
            raise ValueError(f"{relative}: shared library has an interpreter")
    elif relative_text.startswith("lib/"):
        expected_runpath = recipe["elf"]["runpaths"]["lib"]
        if interpreter is not None:
            raise ValueError(f"{relative}: shared library has an interpreter")
    else:
        raise ValueError(f"{relative}: ELF member is outside the accepted directories")

    if runpath != expected_runpath:
        raise ValueError(f"{relative}: RUNPATH is {runpath!r}; expected {expected_runpath!r}")

    result = {
        "needed": needed,
        "run_path": runpath,
    }
    if soname is not None:
        result["soname"] = soname
    if interpreter is not None:
        result["interpreter"] = interpreter
    return result


payload = []
inventory = {}
paths = [runtime_root, *runtime_root.rglob("*")]
paths.sort(key=lambda path: (b"." if path == runtime_root else path.relative_to(runtime_root).as_posix().encode()))
for path in paths:
    relative = pathlib.PurePosixPath(".") if path == runtime_root else path.relative_to(runtime_root)
    relative_text = relative.as_posix()
    metadata = path.lstat()
    entry = {
        "gid": metadata.st_gid,
        "group": "root",
        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
        "owner": "root",
        "path": relative_text,
        "uid": metadata.st_uid,
    }
    if metadata.st_uid != 0 or metadata.st_gid != 0:
        raise SystemExit(f"[postgres-runtime] error: {relative_text} is not root:root")
    if stat.S_ISDIR(metadata.st_mode):
        entry["type"] = "directory"
    elif stat.S_ISREG(metadata.st_mode):
        entry["type"] = "file"
        entry["size"] = metadata.st_size
        entry["sha256"] = sha256_file(path)
    elif stat.S_ISLNK(metadata.st_mode):
        link_target = os.readlink(path)
        entry["type"] = "symbolic-link"
        entry["size"] = metadata.st_size
        entry["sha256"] = sha256_bytes(os.fsencode(link_target))
        entry["link_target"] = link_target
    else:
        raise SystemExit(f"[postgres-runtime] error: {relative_text} has an unsupported file type")
    if entry["type"] != "symbolic-link" and stat.S_IMODE(metadata.st_mode) & 0o022:
        raise SystemExit(f"[postgres-runtime] error: {relative_text} has unsafe mode {entry['mode']}")
    payload.append(entry)
    inventory[relative_text] = {"type": entry["type"]}
    if entry["type"] == "symbolic-link":
        inventory[relative_text]["link_target"] = entry["link_target"]

try:
    validate_all_symlinks(inventory)
except ClosureError as error:
    raise SystemExit(f"[postgres-runtime] error: {error}") from error

payload_by_path = {entry["path"]: entry for entry in payload}
elf_members = {}
for relative_text, inventory_entry in sorted(inventory.items()):
    if inventory_entry["type"] != "file":
        continue
    path = runtime_root if relative_text == "." else runtime_root / relative_text
    with path.open("rb") as member:
        is_elf = member.read(4) == b"\x7fELF"
    if not is_elf:
        continue
    try:
        elf_metadata = inspect_elf(path, pathlib.PurePosixPath(relative_text))
    except ValueError as error:
        raise SystemExit(f"[postgres-runtime] error: {error}") from error
    payload_by_path[relative_text]["elf"] = elf_metadata
    elf_members[relative_text] = elf_metadata

for relative_text, elf_metadata in sorted(elf_members.items()):
    resolutions = []
    for needed in elf_metadata["needed"]:
        try:
            resolution = resolve_needed(
                relative_text,
                elf_metadata["run_path"],
                needed,
                permitted_unbundled,
                inventory,
                elf_members,
            )
        except ClosureError as error:
            raise SystemExit(f"[postgres-runtime] error: {error}") from error
        resolutions.append(resolution)
    elf_metadata["needed_resolution"] = resolutions

packages = {}
for package, expected_version in sorted(recipe["apt"]["packages"].items()):
    actual_version = subprocess.run(
        ["dpkg-query", "-W", "-f=${Version}", package],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout
    if actual_version != expected_version:
        raise SystemExit(
            f"[postgres-runtime] error: package drift for {package}: {actual_version!r}"
        )
    packages[package] = actual_version

manifest = {
    "archive": recipe["archive"],
    "build_recipe": {
        "builder_image": recipe["builder_image"],
        "script_sha256": sha256_file(script_path),
        "snapshot_timestamp": recipe["snapshot_timestamp"],
        "toml_sha256": sha256_file(recipe_path),
    },
    "configure_environment": {
        **recipe["build"]["environment"],
        "SOURCE_DATE_EPOCH": str(recipe["source_date_epoch"]),
    },
    "format": recipe["format"],
    "host_abi": recipe["host_abi"],
    "patches": recipe["patches"],
    "payload": payload,
    "platform": recipe["platform"],
    "postgresql": recipe["postgresql"],
    "prefix": recipe["prefix"],
    "runtime_identity": recipe["runtime_identity"],
    "source_date_epoch": recipe["source_date_epoch"],
    "target": recipe["target"],
    "toolchain": {
        "apt_packages": packages,
        "apt_sources": recipe["apt"]["sources"],
    },
    "zlib": recipe["zlib"],
}

with manifest_path.open("w", encoding="utf-8", newline="\n") as manifest_file:
    manifest_file.write(canonical_json(manifest) + "\n")
PY_WRITE_MANIFEST

    chmod 0644 "${runtime_root}/orna-runtime-manifest.json"
    chown 0:0 "${runtime_root}/orna-runtime-manifest.json"
    touch -d "@${SOURCE_DATE_EPOCH_VALUE}" "${runtime_root}/orna-runtime-manifest.json"
    test ! -e "${runtime_root}/orna-runtime-manifest.sig" \
        || fail "the keyless candidate contains a signature placeholder"
}

run_version_probes() {
    local runtime_root="/build/stage${RUNTIME_PREFIX}"
    local postgres_version
    local psql_version

    log "running environment-clean absolute version probes without LD_LIBRARY_PATH"
    postgres_version="$(
        env -i \
            "${RECIPE_BUILD_ENVIRONMENT[@]}" \
            SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
            PATH="${runtime_root}/bin:${runtime_root}/libexec" \
            "${runtime_root}/bin/postgres" --version
    )"
    psql_version="$(
        env -i \
            "${RECIPE_BUILD_ENVIRONMENT[@]}" \
            SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH_VALUE}" \
            PATH="${runtime_root}/bin:${runtime_root}/libexec" \
            "${runtime_root}/bin/psql" --version
    )"
    test "${postgres_version}" = "postgres (PostgreSQL) ${POSTGRESQL_VERSION}" \
        || fail "unexpected postgres version: ${postgres_version}"
    test "${psql_version}" = "psql (PostgreSQL) ${POSTGRESQL_VERSION}" \
        || fail "unexpected psql version: ${psql_version}"
    log "version probes passed: ${postgres_version}; ${psql_version}"
}

create_candidate_archive() {
    local runtime_parent="/build/stage${RUNTIME_PREFIX%/*}"
    local runtime_root="/build/stage${RUNTIME_PREFIX}"
    local manifest_digest

    log "creating the deterministic keyless candidate archive"
    mkdir -p /output
    tar \
        --create \
        --format=gnu \
        --sort=name \
        --mtime="@${SOURCE_DATE_EPOCH_VALUE}" \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        --file=- \
        --directory="${runtime_parent}" \
        "${RUNTIME_IDENTITY}" \
        | xz \
            --compress \
            --threads="${ARCHIVE_THREADS}" \
            --check="${ARCHIVE_CHECK}" \
            "-${ARCHIVE_COMPRESSION_LEVEL}" \
            > "/output/${ARCHIVE_NAME}"
    cp "${runtime_root}/orna-runtime-manifest.json" /output/orna-runtime-manifest.json
    manifest_digest="$(sha256sum /output/orna-runtime-manifest.json)"
    manifest_digest="${manifest_digest%% *}"
    printf '%s  orna-runtime-manifest.json\n' "${manifest_digest}" \
        > /output/orna-runtime-manifest.sha256
    chmod 0644 \
        "/output/${ARCHIVE_NAME}" \
        /output/orna-runtime-manifest.json \
        /output/orna-runtime-manifest.sha256

    test "$(find /output -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)" = "$(
        printf '%s\n' \
            "${ARCHIVE_NAME}" \
            orna-runtime-manifest.json \
            orna-runtime-manifest.sha256 \
            | sort
    )" || fail "the container output contains files outside the three-file candidate contract"
    log "candidate manifest sha256 ${manifest_digest}"
}

container_build() {
    test "$(pwd -P)" = "/build" || fail "the container build must use the fixed /build path"
    test -r /repo/packaging/postgresql/runtime-build.toml || fail "the read-only recipe mount is absent"
    test -r /repo/packaging/postgresql/build-runtime.sh || fail "the read-only builder mount is absent"

    trap 'find /build -mindepth 1 -depth -delete >/dev/null 2>&1 || true' EXIT

    bootstrap_pinned_python
    write_recipe_parser
    write_elf_closure_module
    run_recipe_self_tests
    run_elf_closure_self_tests
    validate_and_load_recipe
    install_build_dependencies
    verify_installed_packages
    fetch_sources
    build_zlib
    build_postgresql
    create_evidence
    set_private_runpaths
    normalise_tree
    write_and_validate_manifest
    run_version_probes
    create_candidate_archive
}

bootstrap_top_level_string() {
    local recipe_path="$1"
    local key="$2"

    awk -v wanted="${key}" '
        /^\[/ { in_top = 0 }
        NR == 1 { in_top = 1 }
        in_top {
            line = $0
            separator = index(line, "=")
            if (separator > 0) {
                name = substr(line, 1, separator - 1)
                value = substr(line, separator + 1)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
                if (name == wanted) {
                    if (length(value) < 3 || substr(value, 1, 1) != "\"" || substr(value, length(value), 1) != "\"") {
                        exit 24
                    }
                    value = substr(value, 2, length(value) - 2)
                    if (index(value, "\"") || value == "") {
                        exit 24
                    }
                    print value
                    count += 1
                }
            }
        }
        END { if (count != 1) exit 23 }
    ' "${recipe_path}"
}

host_build() {
    local script_path
    local repository_root
    local output_root
    local temporary_root
    local recipe_path
    local frozen_input
    local frozen_script
    local script_digest
    local recipe_digest
    local builder_image
    local runtime_platform
    local archive_name
    local build_number
    local expected_names
    local actual_names

    command -v docker >/dev/null 2>&1 || fail "docker is required"
    script_path="$(realpath "${BASH_SOURCE[0]}")"
    repository_root="$(realpath "$(dirname "${script_path}")/../..")"
    output_root="${repository_root}/target/postgresql-runtime"
    temporary_root="$(mktemp -d /tmp/orna-postgresql-runtime.XXXXXXXX)"
    trap "rm -rf -- $(printf '%q' "${temporary_root}")" EXIT

    frozen_input="${temporary_root}/input"
    frozen_script="${frozen_input}/packaging/postgresql/build-runtime.sh"
    recipe_path="${frozen_input}/packaging/postgresql/runtime-build.toml"
    mkdir -p "$(dirname "${frozen_script}")"
    install -m 0555 "${script_path}" "${frozen_script}"
    install -m 0444 \
        "${repository_root}/packaging/postgresql/runtime-build.toml" \
        "${recipe_path}"
    if test -d "${repository_root}/packaging/postgresql/patches" && \
        test -n "$(
            find "${repository_root}/packaging/postgresql/patches" \
                -mindepth 1 \( -type f -o -type l \) -print -quit
        )"; then
        fail "unrecorded PostgreSQL patches are not permitted in the frozen build input"
    fi
    script_digest="$(sha256sum "${frozen_script}")"
    script_digest="${script_digest%% *}"
    recipe_digest="$(sha256sum "${recipe_path}")"
    recipe_digest="${recipe_digest%% *}"
    log "froze script sha256 ${script_digest} and recipe sha256 ${recipe_digest}"

    if ! builder_image="$(bootstrap_top_level_string "${recipe_path}" builder_image)"; then
        fail "could not parse builder_image from the frozen runtime recipe"
    fi
    if ! runtime_platform="$(bootstrap_top_level_string "${recipe_path}" platform)"; then
        fail "could not parse platform from the frozen runtime recipe"
    fi
    printf '%s\n' "${builder_image}" | grep -Eq '^[^[:space:]@]+@sha256:[0-9a-f]{64}$' \
        || fail "bootstrap builder_image is not immutable"
    test "${runtime_platform}" = "linux/amd64" \
        || fail "bootstrap platform is not the supported linux/amd64 target"

    log "pulling the pinned Debian amd64 builder"
    docker pull --platform "${runtime_platform}" "${builder_image}"

    for build_number in 1 2; do
        mkdir -p \
            "${temporary_root}/build-${build_number}" \
            "${temporary_root}/output-${build_number}"
        log "starting fresh container build ${build_number} of 2"
        docker run \
            --rm \
            --platform "${runtime_platform}" \
            --network bridge \
            --env ORNA_POSTGRES_RUNTIME_CONTAINER_BUILD=1 \
            --mount "type=bind,src=${frozen_input},dst=/repo,readonly" \
            --mount "type=bind,src=${temporary_root}/build-${build_number},dst=/build" \
            --mount "type=bind,src=${temporary_root}/output-${build_number},dst=/output" \
            --workdir /build \
            "${builder_image}" \
            /repo/packaging/postgresql/build-runtime.sh
    done

    archive_name="$(
        find "${temporary_root}/output-1" \
            -mindepth 1 -maxdepth 1 -type f -name '*.tar.xz' -printf '%f\n'
    )"
    case "${archive_name}" in
        ""|*$'\n'*)
            fail "fresh build 1 did not emit exactly one tar.xz archive"
            ;;
    esac

    log "comparing manifests, digests, and archives from both builds"
    cmp \
        "${temporary_root}/output-1/orna-runtime-manifest.json" \
        "${temporary_root}/output-2/orna-runtime-manifest.json"
    cmp \
        "${temporary_root}/output-1/orna-runtime-manifest.sha256" \
        "${temporary_root}/output-2/orna-runtime-manifest.sha256"
    cmp \
        "${temporary_root}/output-1/${archive_name}" \
        "${temporary_root}/output-2/${archive_name}"
    (
        cd "${temporary_root}/output-1"
        sha256sum --check orna-runtime-manifest.sha256
    )

    mkdir -p "${output_root}"
    expected_names="$(
        printf '%s\n' \
            "${archive_name}" \
            orna-runtime-manifest.json \
            orna-runtime-manifest.sha256 \
            | sort
    )"
    actual_names="$(find "${output_root}" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)"
    if test -n "${actual_names}" && test "${actual_names}" != "${expected_names}"; then
        fail "${output_root} contains files outside the three-file candidate contract"
    fi
    rm -f \
        "${output_root}/${archive_name}" \
        "${output_root}/orna-runtime-manifest.json" \
        "${output_root}/orna-runtime-manifest.sha256"
    install -m 0644 \
        "${temporary_root}/output-1/${archive_name}" \
        "${output_root}/${archive_name}"
    install -m 0644 \
        "${temporary_root}/output-1/orna-runtime-manifest.json" \
        "${output_root}/orna-runtime-manifest.json"
    install -m 0644 \
        "${temporary_root}/output-1/orna-runtime-manifest.sha256" \
        "${output_root}/orna-runtime-manifest.sha256"

    log "two-build byte reproducibility passed"
    log "wrote only ${archive_name}, orna-runtime-manifest.json, and orna-runtime-manifest.sha256"
}

test "$#" -eq 0 || fail "this builder accepts no arguments"
reject_signing_inputs

if test "${ORNA_POSTGRES_RUNTIME_CONTAINER_BUILD:-}" = "1"; then
    container_build
else
    host_build
fi
