# OrnaDB maintainer and operator runbook

OrnaDB (Object-Relational Native Applications) is the product. `orna` is the
CLI and server executable. This runbook covers the current
checkout; it is not a replacement for the canonical design bundle in a
separate `spec` checkout.

## Before you start

Work from the repository root. The normal local gate is:

```text
just
```

A bare `just` runs the default `check` recipe: formatting, workspace build,
Clippy, and tests. PostgreSQL starts only when a PostgreSQL recipe is selected.

The normal toolchain is Rust 1.95 with `rustfmt` and `clippy`, `just`, Node 22,
`tree-sitter-cli@0.26.5`, Python 3.11 or newer, and Docker with the Compose
Commands that compile the embedded PostgreSQL engine require a Linux x86_64
host; use the Docker-backed engine gate when the host is not Linux x86_64.
The ABI-header/parity checks require `gcc` with C11 support and the canonical
`../spec/spec/orna_runtime_abi_v1.h`. Qt runtime recipes additionally require
CMake 3.21 or newer, CTest, Qt 6 Core and Widgets, and a C++17 compiler. Git,
GNU `make`, `patch`, and the standard Unix file/archive tools are needed by
the checked-in source and evidence recipes.

When using a prebuilt engine instead of the build script's Docker or source
build, `ORNA_POSTGRES_ENGINE_OUTPUT` must be an **absolute** path to a complete
engine output directory, such as
`$PWD/target/postgresql-embedded-native-one/output` after the native lifecycle
recipe has produced it.

This checkout currently has neither `./spec/` nor the sibling `../spec/`
checkout. The latter owns `../spec/spec/orna_runtime_abi_v1.h`; it is required
by `just runtime-abi-header-check`, `just runtime-abi-parity`, and the Qt
runtime CMake project. Do not substitute a generated or local header, and do
not report those gates as passed while the canonical input is absent.

## Fresh-checkout bootstrap

Use a real Git checkout rather than a source archive: the embedded PostgreSQL
engine is a pinned submodule and its input checks reject an absent, modified,
or detached-at-the-wrong-commit source tree. From the parent directory, replace
`<repository-url>` with the repository URL supplied by your hosting service:

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

The superproject status must be empty, the PostgreSQL submodule status must be
clean, and its commit must be the checked-in gitlink
`f5cc81719e6da4cbdb1f797c48b693e91018153a` (the status line has a leading
space for a matching checkout). The initial clone and submodule update may
fetch the pinned source; after bootstrap, the gate commands below must not
silently fetch it.

Install or select the required host tools before running a gate. With rustup,
the accepted toolchain setup is:

```sh
rustup toolchain install 1.95.0 --profile minimal --component rustfmt --component clippy
rustup override set 1.95.0
rustc --version
cargo --version
just --version
python3 --version
node --version
tree-sitter --version
docker --version
docker compose version
cmake --version
gcc --version
```

Warm the locked Cargo cache while network access is available, then use Cargo
offline mode where the cache must be complete:

```sh
cargo fetch --locked
cargo fetch --locked --manifest-path editors/zed/Cargo.toml
CARGO_NET_OFFLINE=true just check
CARGO_NET_OFFLINE=true just editor-tooling-check
CARGO_NET_OFFLINE=true just demo-check
CARGO_NET_OFFLINE=true just sqlite-check
CARGO_NET_OFFLINE=true just sqlite-smoke
```
Commands that compile `orna-server` or `orna-postgres` invoke the embedded
engine build unless `ORNA_POSTGRES_ENGINE_OUTPUT` names a complete prebuilt
engine output directory using an **absolute** path. Record that prerequisite
separately; Cargo offline mode alone does not make the engine build network-free.
`CARGO_NET_OFFLINE=true` makes Cargo registry/git resolution fail closed when
the cache is incomplete, but it does not disable arbitrary build scripts.

Each gate is **pass** only after its command exits zero and its output is
retained. A prerequisite that is intentionally not installed is **skipped** or
**unavailable** only when the evidence record names the gate and missing
prerequisite; it is never a pass. A command that was invoked and exited
non-zero is a **failure**, not a skip. The current missing `./spec`/`../spec`
inputs therefore make the canonical-header-consuming ABI and CMake/CTest Qt
gates unavailable, rather than evidence of success; path-dependent Rust runtime
smokes remain separate.

## Repository map

1. `Cargo.toml`, `Cargo.lock`: workspace definition and locked dependencies.
2. `crates/`: the Rust implementation packages, including the server/CLI,
   client, compiler, protocol, standard library, LSP, and PostgreSQL kernel.
3. `crates/orna-storage/`: backend-neutral application revision and typed
   migration contracts.
4. `crates/orna-sqlite/`: the local Turso revision-store adapter.
5. `stdlib/std/`: source-authored standard-library declarations.
6. `runtimes/qt/`: the separate Qt runtime CMake project.
7. `editors/`: editor integrations and grammar metadata.
8. `scripts/`: static editor-tooling and accepted demo runners.
9. `postgresql/`: the embedded PostgreSQL engine build and lifecycle tooling.
10. `compose.yaml`: the loopback-only PostgreSQL development service.
11. `.github/workflows/`: quality and embedded PostgreSQL workflows.
12. `.beads/`: the tracked issue ledger; preserve it when cleaning other
    repository state.
13. `docs/`: maintained operator guidance and historical design decisions.
14. `packaging/linux/`: deterministic Linux artifact builder, verifier, installer,
    and focused package tests.

The repository intentionally has no website, Debian release package, or
generated root status ledger. The Linux artifact recipe is a local provenance
and install smoke boundary, not a production distribution authority. Keep
planning and issue state in the issue ledger and maintained documentation rather
than restoring removed snapshots.

## Local check flow

Warm dependencies once from the network with `cargo fetch --locked` and
`cargo fetch --locked --manifest-path editors/zed/Cargo.toml`; do not mistake
those provisioning commands for evidence. After the caches are warm, use
Cargo's offline forms where applicable:

```text
just fmt
CARGO_NET_OFFLINE=true just build
CARGO_NET_OFFLINE=true just lint
CARGO_NET_OFFLINE=true just test
CARGO_NET_OFFLINE=true just rustdoc-check
CARGO_NET_OFFLINE=true just editor-tooling-check
CARGO_NET_OFFLINE=true just demo-check
```

`CARGO_NET_OFFLINE=true` makes Cargo registry and Git resolution fail closed,
but it does not disable arbitrary build scripts. `build`, `test`,
`editor-tooling-check`, and `demo-check` include paths that can compile
`orna-server`/`orna-postgres`; those paths need a Linux x86_64 host plus either
a complete `ORNA_POSTGRES_ENGINE_OUTPUT` directory at an **absolute** path or
the environment-dependent embedded-engine build. Keep that prerequisite in the
evidence instead of calling the whole command network-free.

`just fmt` checks formatting without changing files. `just build` checks all
workspace targets. `just lint` runs workspace Clippy with warnings denied.
`just test` runs workspace tests except tests marked `#[ignore]`.
`just rustdoc-check` builds all workspace API documentation with rustdoc
warnings denied and `--no-deps`; it is included in `just check` and does not
claim documentation for external dependencies.
`just editor-tooling-check` is a static gate and does not launch editor
runtimes, but its source-check parity child can compile `orna-server` and
therefore needs the embedded-engine prerequisite described above.
`just demo-check` runs accepted source-check and offline demos in manifest order
and explicitly skips Compose-only entries; source-check entries have the same
embedded-engine prerequisite.

The editor gate requires Python 3.11+, `tree-sitter` CLI 0.26.5, Node, Cargo,
and the checked-in editor trees. It validates JSON, grammar generation,
accepted corpus manifests, LSP tests, the Zed extension, and the VS Code
syntax with `node --check`; it does not launch Neovim, Vim, Zed, VS Code,
Helix, or Sublime. Emacs is optional: when `emacs` and Eglot are available,
the script batch-loads `editors/emacs/orna-eglot.el`; otherwise it records the
Emacs runtime check as unavailable while still requiring the checked-in
integration file. `../spec/examples` is optional proposal/deferred input; the
script logs it as absent and skips it when, as in this checkout, the directory
is missing.

`just demo-suite` combines `demo-check` with the standalone TTY renderer,
client artifact-integrity, and local capability-matching demos. `demo-check`
source-check entries can compile `orna-server`; provide the embedded-engine
output or record that environment-dependent prerequisite. The suite does not
run the local PostgreSQL CLI demo or Qt/Studio smoke commands.

## Application revisions and SQLite boundary

`orna-storage` defines the backend-neutral `ApplicationRevisionStore`
lifecycle and typed migration contracts. It carries compiler-produced
`PhysicalMigrationArtifact` values as exact canonical bytes and digests for
durable ledger entries. `orna-sqlite` opens a local Turso database as a
library-level revision-store adapter. Its persisted state covers source and
catalogue identities/lineage, source units, semantic revision snapshots, the
application migration ledger, and generated object tables with reference
foreign keys for supported object changes. Unsupported value, enum, record,
binding, scalar, and artifact shapes fail closed.

`LocalPath` also has direct routes for SERVER-only `invoke` and `raw-call`,
principal-scoped USER state, security administration, and redacted invocation
inspection. Local invocation evidence stores only bounded structural summaries,
terminal audit fields, and trace records; arguments, result values, source text,
and resource payloads are not persisted. The private SQLite socket continues to
serve protocol raw calls and applies the same local-peer/execute gate.

Migration validation is bounded to typed migration artifacts and deterministic
PostgreSQL/SQLite artifact checks, plus SQLite schema/data lineage,
revision-ledger integrity, semantic snapshot, generated object-table/
foreign-key checks, and bounded runtime evidence. This scope does not prove
full physical or runtime parity between PostgreSQL and SQLite; do not record
that claim without fresh, dedicated evidence. CLIENT/Qt execution, protected
standard transports, and resource transport remain PostgreSQL/runtime-only.

`just sqlite-check` is the dedicated Cargo compile gate for the storage, SQLite,
and local CLI binary targets. Its local CLI target can compile `orna-server`
and therefore needs the embedded-engine output or environment-dependent build;
Cargo offline mode alone is not sufficient. `just sqlite-smoke` runs the
deterministic revision-store example and the focused SQLite process/socket
integration target. These recipes provide a dedicated SQLite adoption proof;
the standalone adapter example remains a library smoke and does not exercise
the socket by itself.

The accepted standard-library compatibility record currently covers V1 through
V9. The implementation contains V10/V11 paths, but they have no accepted 1.0
compatibility promise until the release evidence and product baseline are
reconciled.

## PostgreSQL Compose lifecycle

`compose.yaml` defines the development `postgres` service using
`postgres:18.4-bookworm`. It binds PostgreSQL to `127.0.0.1:55432` and stores
data in the named `ornadb_postgres_data` volume. Its credentials are development
fixtures only.

The basic Compose lifecycle recipes (`postgres-up`, `postgres-status`,
`postgres-health`, and `postgres-stop`) require Docker and the development
image but do not build the embedded PostgreSQL source. The kernel recipes below
also compile `orna-server`/`orna-postgres`; on a clean target they require a
Linux x86_64 host and either `ORNA_POSTGRES_ENGINE_OUTPUT` naming a complete
prebuilt engine output directory at an **absolute** path or the Docker-backed
engine build. The pinned submodule is required when the Docker/source engine
build path is selected; the prebuilt-output branch does not read it.

Use the checked-in recipes:

```text
just postgres-up
just postgres-status
just postgres-health
just postgres-stop
```

`postgres-up` starts the service in detached mode. `postgres-status` reports
container status. `postgres-health` runs `pg_isready` inside the container for
`ornadb_dev`. `postgres-stop` stops PostgreSQL without deleting the volume.
If Docker, the plugin, the image, or the port is unavailable, record this
Compose gate as unavailable before invoking it; a command that was invoked and
failed is a failure.

For the integration kernel, use these commands from the repository root:

```text
just kernel-resource-audit-proof
just kernel-test
```

Both recipes start PostgreSQL with `--wait`, set the checked-in development
connection fixtures, run ignored tests serially, and stop the service on exit.
`kernel-test` uses a unique per-invocation database named with the
`ornadb_kernel_gate_<BASHPID>_<UTC nanoseconds>` prefix and drops it during
cleanup. Their Cargo invocations enforce `--locked`, but compiling the server
or kernel can still run `orna-postgres/build.rs`. With the Docker/source engine
build, ensure the clean pinned submodule is present; with a prebuilt engine,
provide `ORNA_POSTGRES_ENGINE_OUTPUT` as a complete output directory at an
**absolute** path. These environment-gated Compose proofs also require a Linux
x86_64 host.

CI captures the service and gate logs:

```sh
set -euo pipefail
mkdir -p ci-evidence
just kernel-test 2>&1 | tee ci-evidence/kernel-test.log
docker compose logs --no-color postgres | tee ci-evidence/postgres.log
```

Do not remove the named volume during a routine stop. Reset it only after
confirming that data recovery or reset is intentional. A passing test command
without retained gate/log output is not sufficient release evidence.

## Embedded PostgreSQL source gate

The native embedded-engine summary is a separate Docker-isolated gate from the
Compose development service. It requires the clean
`third_party/postgresql` submodule at the pinned gitlink, a running Docker
daemon, and network access while the pinned builder image is prepared. Check
the source boundary first, then run the two-lifecycle reproduction:

```text
make -C postgresql verify-inputs
make -C postgresql verify-lifecycle \
  TARGET_ROOT="$PWD/target/postgresql-embedded-native"
```

`verify-inputs` rejects an absent, modified, dirty, or wrong-commit submodule
and rejects changed or untracked overlays, patches, and build scripts.
`verify-lifecycle` builds the pinned Debian builder image and fetches only the
checksummed sources named by `postgresql/Containerfile`; the image-preparation
step needs network access. Its `prepare-source` prerequisite runs on the host,
using the pinned Git archive, overlays, and patches. Compilation and both
lifecycle probes then run in `docker run --network=none` with the PostgreSQL
source mounted read-only. Do not enable network access for those proof
containers.

The reproducibility evidence is written under
`target/postgresql-embedded-native-one/output/` (and the comparison run under
`target/postgresql-embedded-native-two/`). The output includes the embedded
archives, support data, lifecycle report/stdout, symbol inventories, licence,
and `embedded-engine-manifest.json`; retain the complete `target` subtree when
reviewing a result. CI uploads the first run's `output/*`, not an unrecorded
local summary. A missing submodule, unavailable Docker daemon/image source,
non-zero build, lifecycle verifier failure, or mismatch between the two runs
is a failed or unavailable gate as appropriate; it is never evidence of a
passed embedded build. This checkout has the PostgreSQL source at the checked-in
gitlink `f5cc81719e6da4cbdb1f797c48b693e91018153a`; no native result is claimed
without a newly executed command and retained evidence.

## Runtime and ABI boundaries

The product is `OrnaDB`, the CLI is `orna`, and runtime families use the
`orna-runtime-*` prefix. The checkout contains an offline TTY path and a
separate Qt runtime project. A runtime named in the canonical spec is not
necessarily implemented or proved here.
The TTY and client demos are Cargo-only and their recipes enforce
`--locked --offline`; they do not need PostgreSQL, Qt, or a display:

```text
just runtime-tty-demo
just client-artifact-demo
just client-capability-demo
```

The ABI header/parity checks require a GCC-compatible C11 compiler, Linux
x86_64 for the parity assertions, and the canonical
`../spec/spec/orna_runtime_abi_v1.h`. The CMake/CTest Qt build additionally
requires CMake 3.21 or newer, CTest, a C++17 compiler, and Qt 6.2+ Core and
Widgets development files. The Rust loader and Studio smoke commands only
require an explicit compatible shared-library path. Build and test headlessly
with:

```text
just runtime-qt-build
just runtime-qt-test
just runtime-qt-rust-smoke <runtime-shared-library>
just studio-qt-demo
just studio-qt-smoke <runtime-shared-library>
just runtime-abi-header-check
just runtime-abi-parity
```

`runtime-qt-test` sets `QT_QPA_PLATFORM=offscreen`; the Rust loader and
`studio-qt-smoke` also use offscreen mode and require an explicit compatible
shared-library path (normally produced by `runtime-qt-build`). The build tree
and Qt visual output remain under `target/runtime-qt/` (including the CTest
visual PNG), while the TTY demo writes `target/runtime-tty-demo-output.bin`.
The ABI-header check is GCC C11 syntax-only validation; ABI parity compiles the
Linux x86_64 C assertions against the canonical header. Because this checkout
has neither `./spec/` nor `../spec/`, the canonical-header-dependent ABI
commands are currently unavailable. The CMake/CTest Qt commands also require
their listed native dependencies; no Qt/ABI pass is claimed. The two Rust
smoke commands remain path-dependent.

Display-backed gates are separate and require a live `DISPLAY` or
`WAYLAND_DISPLAY`:

```text
just runtime-qt-demo
just studio-qt-display-smoke
just studio-qt-action-smoke
just runtime-display-suite
```

The checked-in recipes fail with a prerequisite message and status 2 when the
display variables are absent; record that condition as unavailable rather than
as a passing headless result. None of these commands proves a remote
deployment, editor host session, or production sandbox.

## CLI entry points

Start with `orna --help`, then use command-specific help:

```text
orna --help
orna help <topic>
orna --version
orna server run
orna server backend-shell
orna runtime describe <runtime-shared-library>
orna source check <file.orna>
orna source apply <file.orna>
orna source diff <file.orna>
orna [--runtime <family>] invoke <qualified-name | canonical-function-id> [options]
orna raw-call <canonical-function-id> [<canonical-parameter-id> [<canonical-parameter-id-2>]]
orna state get <root-function-id> [options]
orna state set <root-function-id> [options]
orna inspect <invocation-id> [options]
orna security grant-execute <canonical-function-id>
orna security user create|disable <canonical-principal-id>
orna security role create|grant|revoke <canonical-principal-id> [canonical-principal-id]
orna security grants grant|revoke <canonical-principal-id> <class> [canonical-function-id]
orna security grants list <canonical-principal-id>
orna security check can-execute <canonical-principal-id> <canonical-function-id>
orna security check has-privilege <canonical-principal-id> <class> [canonical-function-id]
orna security whoami
```

`orna server run` with the default managed-local endpoint starts the embedded
PostgreSQL instance in the foreground, using user-owned state and runtime
directories selected by XDG environment variables, with safe fallbacks under
`/tmp`. An explicit `LocalPath` instead starts the local SQLite server and
exposes the `<database>.orna.sock` Unix socket described below.

The managed PostgreSQL server supports `invoke`, `state`, `inspect`, `raw-call`,
`backend-shell`, source apply/diff, and security administration through the
same peer-authenticated instance. Explicit `LocalPath` uses direct SQLite
routes for source apply/diff, SERVER-only invoke/raw-call, USER state, security
administration, and redacted invocation inspection. `source check` remains
offline and does not need PostgreSQL, network access, configuration, or writes.

## Endpoint and command-routing boundary

`DatabaseEndpoint::parse` treats a value without `://` as
`DatabaseEndpoint::LocalPath`; `Display` renders that path directly. Parsing
and display do not imply that every command is routed.

An explicit `LocalPath` is accepted for `server run`, `source check`, source
apply/diff, SERVER-only `invoke` and `raw-call`, `state`, `inspect`, and
security administration. Unsupported commands fail before selecting a backend.
`orna server run` opens and bootstraps the database before exposing
`<database>.orna.sock`; the foreground Unix listener enforces mode `0600`.
Its handshake recognises protocol versions v1 through v5. Versions v1 through
v3 have typed handling in the bounded SQLite surface; v4 and v5 use the
protocol fallback when their opaque codec registry is unavailable. The socket
shares the private Orna raw-call wire protocol, but only the bounded
server-plan/parameter-echo execution subset can produce a successful result.

LocalPath inspection is deliberately narrower than PostgreSQL inspection:
successful direct invocations persist a bounded structural summary and trace
event, while value-bearing projections, source text, and resource payloads are
not stored. LocalPath security mutations are authorized by the durable local
peer principal and `SecurityAdmin` privilege; USER state is principal-scoped
and uses canonical ORV5 values with optimistic revisions.

For an explicit endpoint (`--db` or a positional endpoint), the CLI currently
accepts `ManagedLocal` and `LocalPath`. The bounded installed `invoke` route
also accepts only the current managed Orna Unix socket; other explicit
Unix-socket command routes and remote-TLS endpoints remain rejected until their
route or transport wiring is available. ManagedLocal routes installed commands
to the fixed embedded PostgreSQL host; LocalPath routes the supported local
surface to SQLite without a PostgreSQL fallback.

Run the complete local binary demo with:

```text
just local-cli-demo
```

The demo builds the binary, starts a temporary user-owned server, waits for
readiness, invokes `std.invoke.echo`, and removes its temporary state.

## Security, sessions, and recovery

Managed local operations authenticate the operating-system peer. The server
obtains the Unix peer UID, maps it to the session principal, and keeps the
principal out of request payloads. A caller cannot supply a replacement
principal. The bounded SQLite socket relies on its `0600` filesystem mode and
applies the same local-peer and execute checks as the direct LocalPath routes.
SQLite LocalPath does not expose PostgreSQL CLIENT/Qt execution, standard
protected transports, or resource dispatch.

Keep secrets out of source, argument files, state value files, shell history,
CI logs, and evidence artifacts. The Compose password is a repository-visible
development fixture, not a production credential.

Treat source apply, runtime evidence, and recovery as transactional operations:

1. Source apply reads one regular UTF-8 file and fails closed for invalid input.
2. Managed PostgreSQL source apply records its protected audit event and cannot
   choose an audit principal from the request. SQLite source apply records its
   typed migration and snapshot transaction; SQLite runtime commands record
   only bounded redacted invocation/inspection metadata.
3. Recovery must reproduce the candidate source and catalogue hashes. A
   recovery mismatch, session-close failure, or audit invariant failure is an
   operational failure.
4. Retained revision ancestry is validated for parent identity, uniqueness,
   cycles, and exactly one active pair. Do not hand-edit retained revision or
   audit records to force a transition.
5. Inspection and state operations retain authentication, ownership, epoch, and
   privilege checks through the complete operation and rendering path.

Use `just kernel-test` for the Compose-gated apply, rollback, tamper, retained
listing, recovery, and user-state integration matrix.

Resource payloads and results are process-local. In the PostgreSQL resource
transport, `Values` batches and completed result values are owned by the
producer/transport and emitted as connection-local frames; they are not durable
resource payload rows. SQLite LocalPath has no resource-dispatch protocol.

Where implemented, PostgreSQL persisted resource request history/audit is
redacted metadata: request, parent/call-site, target/revision/principal
identities, decision/terminal outcomes, and optional item/byte counts. It
deliberately does not retain arguments or returned values. This is redacted
history/audit, not durable `Resources` streaming; do not document it as durable
payload/result storage.

## Linux distribution artifact

The checked-in `packaging/linux/` command builds the smallest accepted Linux
x86_64 distribution artifact: a deterministic root-relative USTAR archive
containing `orna`, the distribution manifest, and the embedded-engine manifest.
It is a provenance/install smoke artifact, not the Debian 12 release authority
reserved by ADR 0047 and not a production package publication.

Run the focused packaging tests and a deterministic build from Linux x86_64:

```text
PYTHONDONTWRITEBYTECODE=1 packaging/linux/package.sh test
SOURCE_DATE_EPOCH=1700000000 PYTHONDONTWRITEBYTECODE=1 \
  packaging/linux/package.sh build
PYTHONDONTWRITEBYTECODE=1 packaging/linux/package.sh verify \
  target/orna-1.0.0-linux-amd64.tar --source-date-epoch 1700000000
PYTHONDONTWRITEBYTECODE=1 packaging/linux/package.sh install \
  target/orna-1.0.0-linux-amd64.tar --root "$PWD/target/package-root"
```

The default build compiles `orna-server` into a fresh `target/linux-package/`
tree and pairs that executable with its generated engine manifest. A caller
supplying `--executable` and `--engine-manifest` must take both files from the
same build; the installed command still verifies the compiled engine identity
before serving. The output archive is intentionally not a `.deb`, and no
clean-host, signing, SBOM, repository, or publication result follows from
these local commands.

## Issue ledger and status

`.beads/` is the repository's tracked issue ledger. Use the project's Beads
workflow for issue state; do not delete or replace the ledger with a generated
status snapshot.

`docs/decisions/` retains historical architecture decisions, including decisions
about previously considered distribution approaches. Historical references are
not live build inputs. Current operational claims belong in this runbook and in
the executable tests.

The quality workflow uploads:

```text
ci-evidence/tool-versions.txt
ci-evidence/check.log
ci-evidence/editor-tooling.log
ci-evidence/kernel-test.log
ci-evidence/postgres.log
```

The dedicated SQLite workflow uploads:

```text
ci-evidence/sqlite-check.log
ci-evidence/sqlite-smoke.log
```

For a local evidence bundle, create the directory and preserve exit status
while capturing each gate:

```bash
mkdir -p ci-evidence
set -euo pipefail
CARGO_NET_OFFLINE=true just check 2>&1 | tee ci-evidence/check.log
CARGO_NET_OFFLINE=true just editor-tooling-check 2>&1 | tee ci-evidence/editor-tooling.log
CARGO_NET_OFFLINE=true just demo-check 2>&1 | tee ci-evidence/demo-check.log
CARGO_NET_OFFLINE=true just sqlite-check 2>&1 | tee ci-evidence/sqlite-check.log
CARGO_NET_OFFLINE=true just sqlite-smoke 2>&1 | tee ci-evidence/sqlite-smoke.log
```

The embedded PostgreSQL workflow stores its lifecycle output under
`target/postgresql-embedded-native-one/output/`; the comparison run is under
the sibling `-two` directory. Report a result only when a recorded artifact
or a newly run command supports it. A missing prerequisite may be recorded as
skipped/unavailable with the gate and reason (for example, absent `../spec`,
Docker, a display server, Emacs, or the PostgreSQL submodule); a command that
ran and failed remains a failure. This runbook does not claim a fresh
native, Compose, Qt, editor-host, remote, or release result.

## Explicit non-claims

The following remain outside the current implementation claim:

1. Native distribution packages, package-maintainer scripts, and clean-host
   deployment or recovery.
2. Neovim, Vim, Zed, and VSIX host-session parity beyond the static editor gate.
3. Live `REF` field-path evaluation and the same-major PostgreSQL predecessor
   transition.
4. Full Studio workflows, richer model/launch/projection semantics, and remote
   transport proof.
5. Reflective JSON-RPC/MCP gateways, VM proof, additional toolkit/platform
   runtimes, and general Rows/object-value semantics.

Do not document proposal-only Studio, gateway, remote, VM, or distribution
features as shipped behavior.
