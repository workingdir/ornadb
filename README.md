# OrnaDB maintainer and operator runbook

OrnaDB (Object-Relational Native Applications) is the product. `orna` is the
CLI and server executable. This file is a short operating guide for the
current implementation in `work/`; it is not a replacement for the
canonical design bundle in `../spec/`.

## Before you start

Work from the repository root. The normal local gate is:

```text
just
```

A bare `just` runs the default `check` recipe, which expands to `fmt`, `build`,
`lint`, and `test`. The recipes do not start PostgreSQL unless you select a
PostgreSQL recipe.

The CI toolchain currently provisions:

- Rust 1.95 with `rustfmt` and `clippy`.
- `just`.
- Node 22 and `tree-sitter-cli@0.26.5`.
- Python 3.11 or newer for `scripts/check-editor-tooling.py`.
- Docker with the Compose plugin.

Runtime and ABI recipes additionally need the tools they invoke: CMake, CTest,
CPack, and GCC with C11 support. The Debian package recipe also requires an
amd64 environment, Debian packaging tools, Docker, and the builder image
named `orna-postgresql-engine:debian12-amd64-1`.

The sibling canonical spec checkout is an external prerequisite for
`just runtime-abi-header-check` and for the Qt runtime builds. In particular,
the header check reads `../spec/spec/orna_runtime_abi_v1.h`; do not replace it
with a generated or local substitute.

## Repository map

- `Cargo.toml`, `Cargo.lock`: Rust workspace definition and locked
  dependencies.
- `crates/`: Rust implementation packages, including the `orna` server/CLI,
  client, compiler, protocol, standard library, LSP, and PostgreSQL kernel.
- `stdlib/std/`: source-authored standard-library declarations.
- `runtimes/qt/`: the separate Qt runtime CMake project.
- `editors/`: editor integrations and grammar metadata.
- `scripts/`: static editor-tooling and accepted demo runners.
- `postgresql/`: embedded PostgreSQL build, lifecycle, and verifier inputs.
- `packaging/debian/`: package rules, service metadata, and maintainer scripts.
- `compose.yaml`: loopback-only PostgreSQL development service.
- `.github/workflows/`: CI, Debian package, and embedded PostgreSQL workflows.
- `PLAN.md`, `TRACEABILITY_MATRIX.md`: current implementation and evidence
  records; see [Evidence and status](#evidence-and-status).

## Local check flow

Run the individual recipes when you need a narrower gate:

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
`just editor-tooling-check` is a static gate; it does not install or launch
editor runtimes. `just demo-check` runs the accepted source-check and offline
demos in manifest order and skips compose-only entries.

`just demo-suite` runs `demo-check`, the TTY renderer demo, the CLIENT
artifact-integrity demo, and the local capability-matching demo in one
environment-independent command. It does not run the local PostgreSQL CLI
demo or the Qt/Studio smoke commands, which require separate runtime inputs.

`just check` is deliberately broader than package-specific Clippy. It includes
workspace Clippy and may remain blocked by the tracked PostgreSQL lint issue.
The package-specific command below is a separate focused check, and the current
records mark it clean:

```text
cargo clippy -p orna-client --all-targets -- -D warnings
```

Do not read a workspace `just check` block as proof that the client-specific
Clippy result failed. Check the status records before reporting any result;
this README does not claim a fresh run.

## PostgreSQL Compose lifecycle

`compose.yaml` defines one development service, `postgres`, using the
`postgres:18.4-bookworm` image. It binds PostgreSQL to `127.0.0.1:55432` and
stores data in the named `orna_postgres_data` volume. The credentials in that
file are development fixtures only.

Use the checked-in recipes:

```text
just postgres-up
just postgres-status
just postgres-health
just postgres-stop
```

`postgres-up` starts the service in detached mode. `postgres-status` reports
its container status. `postgres-health` runs `pg_isready` inside the container
for database `ornadb_dev`. `postgres-stop` stops PostgreSQL without deleting
the persistent volume.

For the integration kernel, use `just kernel-test`. It starts PostgreSQL with
`--wait`, runs the ignored server and PostgreSQL integration suites, uses the
isolated `ornadb_kernel_gate` database, and drops that database and stops the
service during cleanup. For the narrower resource durability proof, use
`just kernel-resource-audit-proof`; it also starts with `--wait` and stops the
service on exit. Treat both recipes as local test infrastructure, not as a
production database lifecycle.

CI captures Compose logs with:

```text
docker compose logs --no-color postgres
```

Do not remove the named volume as part of a routine stop. Preserve it until
you have confirmed that data recovery or reset is intentional.

## Runtime, ABI, and package boundaries

The current design names the product `OrnaDB`, the CLI `orna`, and runtimes
with the `orna-runtime-*` prefix. The work tree has an explicit Qt runtime
boundary and a TTY runtime path; the existence of a runtime name in the
canonical spec does not by itself mean that runtime is implemented or proved
here.

The TTY renderer demo exercises the accepted document and byte-stream sinks:

```text
just runtime-tty-demo
```

The client artifact demo exercises the accepted local integrity check:

```text
just client-artifact-demo
```

It rejects a server-domain artifact and a payload digest mismatch. It does
not claim production provenance, signatures, or sandbox mediation.

The client capability demo exercises local declaration and component-boundary
matching:

```text
just client-capability-demo
```

It proves that literal and resolved parameter paths are allowed, an unresolved
parameter is denied, and a similarly named sibling path is denied. It covers
local grant matching only, not configuration loading or production sandbox
mediation.

The Qt recipes are separate from the server package:

```text
just runtime-qt-build
just runtime-qt-test
just runtime-qt-package
just runtime-qt-rust-smoke <runtime-shared-library>
just studio-qt-demo
just studio-qt-smoke <runtime-shared-library>
```

`studio-qt-demo` builds the runtime in `target/runtime-qt` and runs the
existing Studio shell smoke against `target/runtime-qt/liborna-runtime-qt.so`.
The `runtime-qt-rust-smoke` and `studio-qt-smoke` forms require an explicit
shared-library path. `studio-qt-smoke` and `studio-qt-demo` are one-shot shell
smoke commands; they are not full Studio proof. All Qt build/test/package paths
use the canonical external ABI input described above. The ABI checks are:

```text
just runtime-abi-header-check
just runtime-abi-parity
```

The header check is C11 syntax-only validation. ABI parity compiles the Linux
x86_64 C assertions against the canonical header and the Rust mirror values.
Neither command supplies a native host, editor, remote, or Studio deployment
proof.

The Debian development package is built and verified with:

```text
cargo fetch --locked
make --file packaging/debian/rules development-package
```

The rules reproduce the build twice, compare the executable and embedded
engine metadata, assemble the package under `target/debian-package/`, and
verify the final inventory. `release-package` is intentionally closed until
the accepted 1.0 baseline permits a release package.

The package boundary is intentionally narrow:

- The product payload has one executable, `/usr/bin/orna`.
- The service starts `/usr/bin/orna server run` as user and group `orna`.
- Package metadata installs the service, sysusers and tmpfiles definitions,
  `etc/orna/instances/default.toml`, the distribution manifest, the embedded
  engine manifest, and the PostgreSQL license.
- The private PostgreSQL engine is consumed during the package build. The
  verifier rejects PostgreSQL executables and development/runtime artifacts
  such as `.so`, `.a`, `.o`, `postgres`, and `libpq` dependencies from the
  product package.
- The Qt runtime is a separate CPack DEB path from the server package; do not
  treat it as an embedded server payload.

## CLI entry points

`orna` uses one command tree for server management and database work. Start
with `orna --help`, then use command-specific help such as
`orna server --help` or `orna invoke --help`.

```text
orna --help
orna help <topic>
orna --color <auto|always|never> --help
orna --version
orna server run
orna server upgrade
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

`--color auto` follows the terminal. Use `always` for coloured help in a
pipeline, or `never` for plain output. JSON and command results remain plain.

`orna server run` starts the server in the foreground. The command works with
the binary from a checkout and does not require a package installation. The
packaged `orna` service account uses the installed service paths; another user
gets a private local instance in user-owned state and runtime directories.

The same command is used by a service manager:

```text
/usr/bin/orna server run
```

The local profile supports `invoke`, `state`, and `inspect` through the same
peer-authenticated server. `server backend-shell`, `source apply`, `source
diff`, security administration, grants, and `server upgrade` remain service
operations and require the packaged Orna service account.

Run the complete local binary demo with:

```text
just local-cli-demo
```

The demo builds the binary, starts a temporary user-owned server, waits for
readiness, invokes `std.invoke.echo`, and removes its temporary state.

- `orna source check <file.orna>` checks one regular UTF-8 source file offline;
  it does not require PostgreSQL, network access, configuration, or writes.
- `orna source diff <file.orna>` reaches the installed source/revision diff
  path and reports the semantic diff without applying it.
- `orna source apply <file.orna>` reaches the installed apply path. A
  successful apply writes the resulting document to standard output; source
  diagnostics and operational errors go to standard error.

`orna invoke` accepts `--arg <parameter>=<value>`, `--args-file <path>`,
`--output <value>`, `--trace <value>`, `--runtime <family>`, `--explain`, and
`--no-progress`. Runtime selection is automatic unless `--runtime` is used.
`--explain` shows the request without dispatching it.

`orna state get` accepts `--profile`, repeated `--instance` with optional
`--instance-key`, and repeated `--expect-type` triples. `orna state set` accepts
`--function`, `--slot`, `--revision <create|revision-number>`, `--type`,
`--value-file`, and optional `--profile` and `--instance-key`.

`orna inspect` accepts `--projection`, `--trace`, `--after`,
`--include-values`, `--include-source`, `--include-security`,
`--include-runtime`, and `--epoch`. Inspection results are JSON lines on
standard output. Use canonical invocation and identity values; malformed
shapes fail as usage errors.

Security administration also exposes `orna security whoami` and canonical-ID
grant/check forms. Use those forms to inspect the authenticated session and
its grants; do not encode a caller identity in an invoke, state, or inspect
request.

## Security, sessions, and secrets

Installed local operations authenticate the operating-system peer. The server
obtains the Unix peer UID, maps it to the session principal, and keeps the
principal out of the request payload. Invoke, state, inspect, raw-call, and
security-admin paths must use that authenticated session; a caller cannot
supply a replacement principal.

The packaged service runs as the non-login `orna` account. Its service unit
uses `UMask=0077` and a private runtime directory. Persistent instance data is
under `/var/lib/orna/instances` with mode `0700` and ownership `orna:orna`.
The Compose service binds only to loopback for local development.

Keep secrets out of source, `--args-file` documents, state value files, shell
history, CI logs, and evidence artifacts. Never commit credentials or paste
them into `PLAN.md` or `TRACEABILITY_MATRIX.md`. The Compose password is a
repository-visible development fixture, not a production credential; use the
production secret-management process outside this repository. Review command
arguments before copying them into a ticket or an artifact.

Package maintenance scripts clear dynamic-loader environment variables and
invoke maintenance with a minimal environment. Preserve that boundary; do not
bypass package maintenance hooks with ambient `LD_*` or preload settings.

## Recovery and audit constraints

Treat source apply and recovery as transactional operations, not file-copy
operations:

- Source apply reads one regular UTF-8 file and fails closed for invalid input.
- The installed apply path records a protected `SourceApply` audit event with a
  fixed service identity; the request cannot choose its audit principal.
- After apply, recovery must reproduce the candidate source and catalogue
  hashes. A recovery mismatch, database session-close failure, or audit
  invariant failure is an operational failure, not a successful apply.
- Recovery validates retained revision ancestry, including parent identity,
  uniqueness, cycles, and exactly one active pair. Do not hand-edit retained
  revision or audit records to force an upgrade.
- Inspection and state operations retain authentication, ownership, epoch, and
  privilege checks through the complete read/write and rendering operation.
  Denials and recovery failures remain auditable.

Use `just kernel-test` for the Compose-gated apply, rollback, tamper, retained
listing, recovery, and user-state integration matrix. The matrix records that
proof separately from package selection and native-host proof. Preserve the
Compose volume and CI evidence when investigating a failure.

## Evidence and status

Use these records in order:

1. `PLAN.md` is the current implementation projection and labels historical
   versus current evidence.
2. `TRACEABILITY_MATRIX.md` cross-references accepted implementation slices,
   focused checks, and environment-gated proof. Its rows do not turn an
   unrun test into a current result.
3. `../TODO.md` and `../GOAL.MD` are external planning records. `../TODO.md`
   is outside `work/` and outside Git; update planning status there only through
   the project workflow.
4. `../spec/README.md` and `../spec/VALIDATION.md` are the sibling canonical
   handoff and release-integrity references. They distinguish locked material
   from proposal, open, future, and rejected material.

The CI workflows upload evidence under these names and paths:

- `ci-evidence/tool-versions.txt`, `ci-evidence/check.log`,
  `ci-evidence/editor-tooling.log`, `ci-evidence/kernel-test.log`, and
  `ci-evidence/postgres.log` from the quality workflow.
- `target/debian-package/` package, manifests, and `orna.dynamic` from the
  Debian workflow.
- `target/postgresql-embedded-native-one/output/*` from the embedded
  PostgreSQL workflow.

This README does not claim a fresh command result. Report a result only when a
recorded artifact or a newly run command supports it.

## Pending gates and explicit non-claims

The following gates remain separate from the accepted local implementation
baseline and must not be inferred from a focused check, Compose run, or smoke
command:

- A clean Debian 12 amd64 host with networking disabled for the native package
  and lifecycle proof. The existing Debian workflows are Docker-isolated
  package/build/verifier summaries, not fresh native Debian host proof.
- Installed package selection, including Qt package selection, and clean-host
  deployment/recovery.
- Neovim and Vim host sessions. The static editor gate does not establish host
  sessions; current records note that Neovim is absent and `/usr/bin/vi` is Vim
  Tiny filetype-only smoke. Manual Zed/VSIX runtime parity is also unclaimed.
- Live `REF` field-path evaluation and the same-major PostgreSQL predecessor
  transition. The latter stays deferred until a successor release declares a
  predecessor edge.
- Full Studio workflows, richer model/launch/projection semantics, and any
  remote transport proof. `studio-qt-smoke` is only a one-shot smoke command.
- Reflective JSON-RPC/MCP gateways, VM proof, additional toolkit/platform
  runtimes, and general Rows/object-value semantics. These remain contract or
  environment gates, not implied features.

Do not implement or document proposal-only Studio, gateway, remote, or VM
features as shipped behavior. Maintain the accepted Qt v1, bounded Inspector,
V8 Rows/table, V9 constructor, and developer-tooling baselines while the
remaining contracts and host evidence are resolved.
