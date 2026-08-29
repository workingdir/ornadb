# OrnaDB maintainer and operator runbook

OrnaDB (Object-Relational Native Applications) is the product. `orna` is the
CLI and server executable. This runbook covers the current checkout; it is not
a replacement for the canonical design bundle in the sibling `../spec/`
checkout.

## Before you start

Work from the repository root. The normal local gate is:

```text
just
```

A bare `just` runs the default `check` recipe: formatting, workspace build,
Clippy, and tests. PostgreSQL starts only when a PostgreSQL recipe is selected.

The normal toolchain is Rust 1.95 with `rustfmt` and `clippy`, `just`, Node 22,
`tree-sitter-cli@0.26.5`, Python 3.11 or newer, and Docker with the Compose
plugin. Runtime and ABI recipes additionally need CMake, CTest, and GCC with
C11 support.

The sibling canonical spec checkout is required by
`just runtime-abi-header-check` and the Qt runtime build. The header check reads
`../spec/spec/orna_runtime_abi_v1.h`; do not replace it with a generated or
local substitute.

## Repository map

1. `Cargo.toml`, `Cargo.lock`: workspace definition and locked dependencies.
2. `crates/`: the Rust implementation packages, including the server/CLI,
   client, compiler, protocol, standard library, LSP, and PostgreSQL kernel.
3. `stdlib/std/`: source-authored standard-library declarations.
4. `runtimes/qt/`: the separate Qt runtime CMake project.
5. `editors/`: editor integrations and grammar metadata.
6. `scripts/`: static editor-tooling and accepted demo runners.
7. `postgresql/`: the embedded PostgreSQL engine build and lifecycle tooling.
8. `compose.yaml`: the loopback-only PostgreSQL development service.
9. `.github/workflows/`: quality and embedded PostgreSQL workflows.
10. `.beads/`: the tracked issue ledger; preserve it when cleaning other
    repository state.
11. `docs/`: maintained operator guidance and historical design decisions.

The repository intentionally has no website, distribution package, or generated
root status ledger. Keep planning and issue state in the issue ledger and
maintained documentation rather than restoring removed snapshots.

## Local check flow

Run a narrower recipe when needed:

```text
just fmt
just build
just lint
just test
just editor-tooling-check
just demo-check
```

`just fmt` checks formatting without changing files. `just build` checks all
workspace targets. `just lint` runs workspace Clippy with warnings denied.
`just test` runs workspace tests except tests marked `#[ignore]`.
`just editor-tooling-check` is a static gate and does not launch editor
runtimes. `just demo-check` runs accepted source-check and offline demos in
manifest order and skips Compose-only entries.

`just demo-suite` combines the offline checks, TTY renderer demo, client
artifact-integrity demo, and local capability-matching demo. It does not run
the local PostgreSQL CLI demo or Qt/Studio smoke commands.

## PostgreSQL Compose lifecycle

`compose.yaml` defines the development `postgres` service using
`postgres:18.4-bookworm`. It binds PostgreSQL to `127.0.0.1:55432` and stores
data in the named `orna_postgres_data` volume. Its credentials are development
fixtures only.

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

For the integration kernel, use `just kernel-test`. It starts PostgreSQL with
`--wait`, runs the ignored server and PostgreSQL integration suites, uses the
isolated `ornadb_kernel_gate` database, and cleans up the database and service.
`just kernel-resource-audit-proof` is the narrower resource durability proof.
Treat both recipes as local test infrastructure, not a production lifecycle.

CI captures Compose logs with:

```text
docker compose logs --no-color postgres
```

Do not remove the named volume during a routine stop. Reset it only after
confirming that data recovery or reset is intentional.

## Runtime and ABI boundaries

The product is `OrnaDB`, the CLI is `orna`, and runtime families use the
`orna-runtime-*` prefix. The checkout contains a TTY runtime path and a
separate Qt runtime project. A runtime named in the canonical spec is not
necessarily implemented or proved here.

Use the runtime and ABI recipes as follows:

```text
just runtime-tty-demo
just client-artifact-demo
just client-capability-demo
just runtime-qt-build
just runtime-qt-test
just runtime-qt-rust-smoke <runtime-shared-library>
just studio-qt-demo
just studio-qt-smoke <runtime-shared-library>
just runtime-abi-header-check
just runtime-abi-parity
```

The Qt build and test are independent of the server. The Rust and Studio smoke
forms require an explicit shared-library path. The ABI header check is C11
syntax-only validation; ABI parity compiles the Linux x86_64 assertions against
the canonical header and Rust mirror values. None of these commands proves a
remote deployment, editor host session, or production sandbox.

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

`orna server run` starts the embedded PostgreSQL instance in the foreground
using user-owned state and runtime directories selected by XDG environment
variables, with safe fallbacks under `/tmp`. An external process supervisor may
run the foreground command, but no service account or package installation is
required.

The local server supports `invoke`, `state`, `inspect`, `raw-call`,
`backend-shell`, source apply/diff, and security administration through the same
peer-authenticated instance. `source check` remains offline and does not need
PostgreSQL, network access, configuration, or writes.

Run the complete local binary demo with:

```text
just local-cli-demo
```

The demo builds the binary, starts a temporary user-owned server, waits for
readiness, invokes `std.invoke.echo`, and removes its temporary state.

## Security, sessions, and recovery

Local operations authenticate the operating-system peer. The server obtains the
Unix peer UID, maps it to the session principal, and keeps the principal out of
request payloads. A caller cannot supply a replacement principal.

Keep secrets out of source, argument files, state value files, shell history,
CI logs, and evidence artifacts. The Compose password is a repository-visible
development fixture, not a production credential.

Treat source apply and recovery as transactional operations:

1. Source apply reads one regular UTF-8 file and fails closed for invalid input.
2. A successful apply records its protected audit event and cannot choose an
   audit principal from the request.
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

The embedded PostgreSQL workflow stores its lifecycle output under
`target/postgresql-embedded-native-one/output/`. Report a result only when a
recorded artifact or a newly run command supports it; this runbook does not
claim a fresh command result.

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
