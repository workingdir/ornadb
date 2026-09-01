# OrnaDB

OrnaDB is a database platform for typed applications, with SERVER functions beside the data and CLIENT functions in the application.

## Features

- One function model covers queries, mutations, user interfaces, presenters, and integrations.
- Stable identities and revisions apply across code, data, and dependencies.
- PostgreSQL-backed storage provides transactions, constraints, and server-side functions.
- Client runtimes provide application interfaces, state, resources, actions, and local presentation.
- Structured invocation traces show execution details and security decisions.
- Source checking, semantic diffs, and editor integrations support local development.

## Build

Requirements:

- Linux x86_64 is required for commands that compile the embedded PostgreSQL
  engine, including the workspace server build; use the Docker-backed engine
  gate when the host itself is not Linux x86_64
- Rust 1.95 or later, with `rustfmt` and `clippy`
- Cargo and `just`
- Python 3.11 or newer, Node 22, and `tree-sitter-cli@0.26.5` for editor checks
- Git, GNU `make`, `patch`, and standard Unix file/archive tools
- Docker Engine with the Compose plugin for PostgreSQL gates
- GCC with C11 support for the ABI checks
- CMake 3.21 or later, CTest, a C++17 compiler, and Qt 6.2+ Core/Widgets
  development files for Qt gates

`ORNA_POSTGRES_ENGINE_OUTPUT`, when used instead of the build script's Docker
or source build, must be an **absolute** path to a complete prebuilt engine
output directory (for example, `$PWD/target/postgresql-embedded-native-one/output`
after the native lifecycle recipe has produced it).

Build from a source checkout after warming the locked dependency cache:

```sh
cargo fetch --locked
cargo build --locked --workspace
cargo run --locked -p orna-server -- --help
```

`cargo fetch --locked` provisions Cargo registry and Git dependencies and may
use the network; `CARGO_NET_OFFLINE=true` only makes Cargo dependency
resolution fail closed when the cache is incomplete. The direct build/run
commands above and recipes that compile `orna-server` or `orna-postgres` also
run the embedded-engine build script unless `ORNA_POSTGRES_ENGINE_OUTPUT`
points to a complete prebuilt engine output directory at an **absolute** path.
Linux x86_64 is required for a host build; Cargo offline mode alone does not
make those build steps network-free.

## Fresh checkout and release gates

Start from a real Git checkout. Replace `<repository-url>` with the URL provided
by the repository host:

```sh
git clone --recurse-submodules <repository-url> ornadb
cd ornadb
git submodule sync --recursive
git submodule update --init --recursive --checkout
git status --short
git submodule status -- third_party/postgresql
git -C third_party/postgresql rev-parse HEAD
git -C third_party/postgresql status --porcelain=v1 --untracked-files=all
```

The superproject and submodule status must be clean. The PostgreSQL submodule
must resolve to the checked-in gitlink
`f5cc81719e6da4cbdb1f797c48b693e91018153a`. Submodule initialisation is a
networked provisioning step; the embedded source checks must not fetch or
modify the submodule after it is checked out.

Before offline gates, install the host prerequisites listed above and warm the
locked Cargo cache:

```sh
rustup toolchain install 1.95.0 --profile minimal --component rustfmt --component clippy
rustup override set 1.95.0
cargo fetch --locked
cargo fetch --locked --manifest-path editors/zed/Cargo.toml
```

Run deterministic local evidence after the cache is warm:

```sh
CARGO_NET_OFFLINE=true just check
CARGO_NET_OFFLINE=true just editor-tooling-check
CARGO_NET_OFFLINE=true just demo-suite
CARGO_NET_OFFLINE=true just sqlite-check
CARGO_NET_OFFLINE=true just sqlite-smoke
```

These commands make Cargo dependency resolution offline. The grammar/LSP checks
and standalone TTY/client demos do not need a database, but
`editor-tooling-check` also runs source-check parity and can compile
`orna-server`, as can `check`, `demo-check`, `sqlite-check`, `sqlite-smoke`,
and `local-cli-demo`. Those paths need a Linux x86_64 host plus either a
complete `ORNA_POSTGRES_ENGINE_OUTPUT` directory at an **absolute** path or the
environment-dependent embedded-engine build. Record that prerequisite with the
evidence; do not call a failed engine build an offline cache miss.

The retained static `just editor-tooling-check` result dated 2026-08-25 is
the current documented editor-tooling baseline. It is bounded evidence for
only the checked-in static contracts listed for that gate, not a full
editor-parity or runtime result. The static command does not launch editor
hosts: Neovim/Vim host sessions are unavailable and not proven in the current
environment. No manual Zed host launch was run; VSIX packaging, installation,
and launch were not run and remain unclaimed.

The remaining gates are environment-dependent:

```sh
# Development PostgreSQL service and ignored integration matrix.
just postgres-up
just postgres-health
just kernel-resource-audit-proof
just kernel-test
just postgres-stop

# Docker-isolated embedded PostgreSQL source/lifecycle reproduction.
make -C postgresql verify-inputs
make -C postgresql verify-lifecycle \
  TARGET_ROOT="$PWD/target/postgresql-embedded-native"

# Canonical-header/Qt gates.
just runtime-abi-header-check
just runtime-abi-parity
just runtime-qt-build
just runtime-qt-test
just runtime-qt-rust-smoke <runtime-shared-library>
just studio-qt-demo
just studio-qt-smoke <runtime-shared-library>
```

Compose needs a Docker daemon, the Compose plugin, port `127.0.0.1:55432`,
and either network access to pull `postgres:18.4-bookworm` or that image
already cached. The embedded reproduction additionally needs the clean pinned
PostgreSQL submodule and network access only while its checksummed builder
image is prepared; its source build and lifecycle probes run with
`--network=none`. Its output is under
`target/postgresql-embedded-native-one/output/` and the comparison run is
under the sibling `-two` directory.

The ABI header/parity checks require a GCC-compatible C11 compiler, Linux
x86_64 for the parity assertions, and `../spec/spec/orna_runtime_abi_v1.h`.
The CMake/CTest Qt build additionally requires CMake 3.21 or later, CTest, a
C++17 compiler, and Qt 6.2+ Core and Widgets development files. The Rust
loader and Studio smoke commands use the offscreen platform and only require
an explicit compatible shared-library path.
Display-backed `runtime-qt-demo`, `studio-qt-display-smoke`,
`studio-qt-action-smoke`, and `runtime-display-suite` additionally require
`DISPLAY` or `WAYLAND_DISPLAY`.

The checked-in `packaging/linux/` command produces a deterministic Linux
x86_64 root-relative artifact and verifies the installed-command provenance
boundary. It is not a Debian package or publication authority:

```sh
PYTHONDONTWRITEBYTECODE=1 packaging/linux/package.sh test
SOURCE_DATE_EPOCH=1700000000 PYTHONDONTWRITEBYTECODE=1 \
  packaging/linux/package.sh build
PYTHONDONTWRITEBYTECODE=1 packaging/linux/package.sh verify \
  target/orna-1.0.0-linux-amd64.tar --source-date-epoch 1700000000
```

The default package build uses a fresh `target/linux-package/` Cargo target
and its generated embedded-engine manifest. It requires Linux x86_64 and the
same engine build prerequisite described above; no clean-host, signing, SBOM,
repository, or production-package claim follows from this local artifact.

Record a gate as **pass** only when the command exits zero and its output is
retained. A manifest-declared `compose-only` demo, absent optional
`../spec/examples`, or unavailable optional Emacs/host runtime is an explicit
skip or unavailable result, not a pass. A command that ran and exited
non-zero is a failure. The expected CI/local evidence files are
`ci-evidence/tool-versions.txt`, `check.log`, `editor-tooling.log`,
`kernel-test.log`, `postgres.log`, `sqlite-check.log`, and
`sqlite-smoke.log`; retain the complete embedded `target` output when that
gate runs.

This checkout currently has neither `./spec/` nor sibling `../spec/`, so the
canonical ABI/Qt gates are unavailable. Its PostgreSQL submodule is currently
initialized at the checked-in gitlink
`f5cc81719e6da4cbdb1f797c48b693e91018153a`; a fresh checkout must verify that
same commit and a clean submodule before running embedded-native gates. No
native PostgreSQL, Compose, Qt, remote-host, or editor-host result is claimed
without a newly executed command and retained evidence. See the [maintainer
runbook](docs/maintainer-runbook.md) and [editor-tooling
guide](docs/editor-tooling.md) for gate-specific failure interpretation.

## Try it

Run the included examples. Standalone TTY/client demos need no PostgreSQL; the
manifest source-check demos also compile `orna-server` and therefore require
the embedded engine output or the environment-dependent engine build:

```sh
just demo-suite
```

For a complete local server and invocation example:

```sh
just local-cli-demo
```

## Command line

```sh
target/debug/orna --help
target/debug/orna --version
```

The no-argument form targets the local function-backed session, and `orna repl`
is its explicit spelling. The parser accepts `orna invoke <function>` for one
stored function call:

```sh
orna
orna repl
orna invoke std.invoke.echo --arg p_value=hello
orna --db orna+unix:///run/orna/orna.sock invoke tasks.overdue
orna --db orna://db.example.test/work invoke tasks.overdue
orna --daemon
```

The examples above cover parser-supported command shapes. They do not prove
that a database host is available or that the session completes successfully.
The `--db` option selects a managed local database, an explicit Orna Unix
socket, or a remote Orna URI. Remote invocation is parsed by the CLI but is
not available until the authenticated TLS session contract is accepted.

Use `orna inspect`, `orna source check`, and `orna source apply` for normal
inspection and source workflows. Server administration, security management,
runtime metadata, and `raw-call` are explicit operational or recovery paths.
Use command-specific help for their options.

## Desktop runtime

OrnaDB supports terminal output and an optional Qt desktop runtime. Run the demos with:

```sh
just runtime-tty-demo
just studio-qt-demo
```

The Qt runtime is built separately from the OrnaDB server. It requires the
canonical header and native dependencies described in the fresh-checkout gate
section; absent prerequisites are not silently replaced by the TTY runtime.

## Development

After `cargo fetch --locked` and
`cargo fetch --locked --manifest-path editors/zed/Cargo.toml`, run common checks
from the repository root with Cargo's dependency resolution kept offline:

```sh
CARGO_NET_OFFLINE=true just test
CARGO_NET_OFFLINE=true just editor-tooling-check
CARGO_NET_OFFLINE=true just demo-suite
```

The `orna-server` paths in these commands can invoke the embedded PostgreSQL
build script; on a host build, use Linux x86_64 and provide a complete
`ORNA_POSTGRES_ENGINE_OUTPUT` directory at an **absolute** path, or record the
Docker-backed engine build as an environment-dependent prerequisite.

## Documentation

See the [maintainer and operator documentation](docs/maintainer-runbook.md) for environment setup, runtime development, and operations. See [editor tooling](docs/editor-tooling.md) for editor-specific checks.

## Project layout

The repository contains the Rust workspace, standard library declarations, runtimes, editor integrations, and repository tooling.

## License

OrnaDB is licensed under the [Apache License 2.0](LICENSE).
