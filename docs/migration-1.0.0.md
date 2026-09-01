# Migrating to OrnaDB 1.0.0

**Status:** This is migration guidance for the current source tree, not a
promise that a production `1.0.0-1` package is available. The checkout has no
tracked production package or in-place upgrade command. The canonical
normative `spec` bundle is also absent from both `./spec/` and `../spec/`.

## Read this before changing anything

There is no supported 0.x-or-development-engine to 1.0.0 upgrade path in this
checkout. The first-release decision intentionally defines empty accepted
predecessor and forward-edge sets. In particular:

- Do not treat a development or 0.x database as a supported predecessor for a
  1.0 installation.
- Do not run `orna server upgrade`: that command is not present in the current
  CLI. There is no hidden compatibility mode or environment-variable override
  that creates an upgrade path.
- Do not edit `_orna_kernel` tables, retained revision rows, generated SQL, or
  migration ledger entries by hand. Recovery validates their identities and
  digests, and hand edits are unsupported.
- Do not split one application into PostgreSQL-specific and SQLite-specific
  `.orna` files. Orna source is backend-neutral. Keep one source model and let
  the selected adapter accept or reject its currently implemented physical
  subset; an unsupported shape must fail closed, not acquire different
  backend-specific semantics.
- Do not treat a CI retention artifact, a local binary, or a build directory as
  a production distribution authority. The intended release authority and its
  required gates are recorded in the
  [first-release decision](decisions/0047-first-one-zero-release.md).

The distinction below is important: changing an application source revision is
implemented; upgrading the Orna engine/product from a predecessor is not.

## Application source changes

Use this workflow when the database is already on a supported current engine
and you are changing its backend-neutral `.orna` source.

### 1. Keep the source as the portable input

Submit one regular UTF-8 source file. The source compiler, not user-written
SQL, describes functions, values, objects, and application changes. Internal
storage adapters lower the typed migration representation for their own
private schema; those generated statements are not a public migration format.

### 2. Check without touching a database

```text
orna source check app.orna
```

`source check` reads one source file and emits diagnostics without opening a
database, loading configuration, using the network, starting a child process,
or writing files. It checks against the empty application catalogue. A clean
check therefore does **not** prove that the source is continuous with, or
applicable to, an installed revision.

### 3. Review the semantic change

For the managed local instance:

```text
orna source diff app.orna
```

For a direct SQLite path:

```text
orna --db ./data/app.sqlite source diff app.orna
```

`source diff` is read-only and compares the candidate with the active source
and catalogue revision. It is the review step; it does not activate a
candidate. The SQLite route requires an existing database for this comparison.

### 4. Apply only the reviewed candidate

For the managed local instance:

```text
orna source apply app.orna
```

For a direct SQLite path:

```text
orna --db ./data/app.sqlite source apply app.orna
```

The apply path checks and prepares the complete file, constructs a typed
physical migration artifact, and commits the candidate revision and its
application-migration ledger entry atomically. Error diagnostics leave the
active revision and ledger unchanged. A concurrent apply that changes the
expected base fails closed; re-run the check and diff against the new active
revision instead of forcing the old candidate. Post-apply recovery must
reproduce the prepared source/catalogue hashes.

`source apply` is an application revision operation. It is not an engine
upgrade, package installation, SQL import, or general-purpose SQL execution
path.

## Backend-neutral storage boundary

The `.orna` source and its typed application model are the portable contract.
The current adapters do not expose equivalent *implementation capacity* for
every runtime surface yet:

- The managed local route uses a private embedded PostgreSQL kernel. It does
  not provide public PostgreSQL TCP/pgwire/SQL or PostgreSQL driver
  compatibility. See the [embedded PostgreSQL decision](decisions/0019-embedded-postgresql-engine.md).
- A filesystem endpoint uses the direct SQLite adapter. Its current physical
  implementation is bounded: supported object creation and field additions
  use the typed storage model, while unsupported value/enum/record/binding
  shapes, unsupported scalar fields, and other unimplemented physical
  operations fail closed. SQLite does not reinterpret the source as a second
  language or silently fall back to PostgreSQL.
- The same source should remain the input on both backends. A SQLite rejection
  identifies an adapter capability boundary that must be implemented before
  that deployment can use the source shape; it is not permission to fork the
  source or write backend-specific SQL.
- Direct SQLite invocation currently accepts SERVER functions only. It does
  not provide the managed route's CLIENT/Qt/resource transports, `--trace`, or
  a Qt runtime. USER state, security administration, bounded raw calls, and
  redacted inspection have direct local routes.

The internal migration language is deliberately closed and typed rather than
SQL. The checked-in application migration source is
`crates/orna-storage/migrations/0046_application_migrations.orna`; users should
not edit it or its generated adapter artifacts.

## Engine and internal schema migrations

The PostgreSQL bootstrap currently applies a contiguous internal migration
registry through version 47. Version 46 establishes the application-migration
ledger; version 47 adds its baseline data step. These are engine/storage
migrations, not application `.orna` source migrations and not a user-facing
cross-backend SQL format.

The baseline has a deliberate limitation: physical changes made before the
ledger existed did not retain replayable artifacts. The baseline binds the
known source/catalogue ancestry to empty historical artifacts so lineage can be
validated; it does **not** reconstruct those old physical operations. Do not
infer a reversible history from the presence of the baseline row.

SQLite has its own internal schema bootstrap and migration boundary. That is an
adapter implementation detail. It does not alter the source language or create
a supported PostgreSQL-to-SQLite physical/runtime parity guarantee.

## Standard-library compatibility

Standard-library snapshots are content-addressed and immutable. The code in
this checkout defines retained revisions from `orna.std/1` through
`orna.std/11` and contains sequential upgrade preparation through V11. An
upgrade step requires the exact expected parent revision and verifies the
parent before constructing the child; an already-installed or mismatched base
fails closed.

There is an unresolved release-evidence discrepancy: the current maintainer
runbook still describes the accepted chain as V1 through V9, while the decision
index has no complete tracked work decision for the V10/V11 addition and the
implementation/source-apply selection paths include V10 and V11. Consequently:

- Treat V10/V11 as implementation evidence only, not as a 1.0 compatibility
  promise.
- Do not overwrite or rename a historical standard snapshot to make revisions
  appear compatible.
- Before publishing 1.0, reconcile the runbook, decision index, standard
  acceptance record, and product baseline. Until then, preserve the exact
  source/catalogue identities and report an unavailable compatibility result
  rather than claiming conformance.

## Private protocol and runtime compatibility

The local server socket uses the private Orna handshake (versions 1 through 5)
and local peer authentication. Runtime values use canonical ORV5 envelopes;
checked sets use ORV6. These are private Orna contracts, not public SQL or
remote-transport compatibility promises.

Inspector compatibility is similarly strict: inspection epochs use the full
16-byte epoch identity in `ORNA-INSPECT/1`. The superseded low-u64 draft is not
a mixed-width compatibility mode. Readers and writers must move together;
historical inspector helper names are not silently rewritten. Preserve
revision-pinned inspection records and treat an unavailable decoder as an
explicit unavailable result.

The explicit endpoint parser recognizes local paths, managed-local URIs,
Unix-socket URIs, and remote Orna URIs. The bounded installed `invoke` route
accepts only the current managed Orna Unix socket through local authentication;
other explicit Unix command routes and remote routes fail closed because their
session or transport support is unavailable. Parsing an endpoint is not
evidence that migration or a general remote session can use it.

## Runtime and editor limits

The installed runtime boundary is `tty`. The Qt path and native ABI checks are
separately gated and require the canonical ABI header plus native dependencies;
they cannot be called production-supported from this checkout while the
canonical bundle is absent. Direct SQLite does not provide Qt.

`orna-lsp`, Tree-sitter, TextMate, and the editor integration packages cover
the checked-in static tooling surface. The accepted corpus is evidence for that
bounded surface only. Static checks do not launch Neovim, Vim, Helix, Zed,
VS Code, Sublime, or Emacs host sessions and do not establish full grammar
parity with the absent spec.

## Release and evidence boundary

The intended 1.0 release identity is Debian 12 amd64 `1.0.0-1` with a signed
`v1.0.0` source tag. This checkout includes `packaging/linux/`, a minimal
provenance/install artifact recipe, but it does not contain a Debian release
package or production-package evidence. Before treating a future package as
an upgrade source, the product baseline must explicitly accept its language,
commands, protocols, persistence, security, installation, recovery, upgrade,
and compatibility scope.

The [maintainer runbook](maintainer-runbook.md) defines the required
prerequisites and evidence vocabulary. In particular:

- `cargo fetch --locked` is networked dependency provisioning. `CARGO_NET_OFFLINE=true`
  only makes Cargo dependency resolution fail closed; it does not make an
  embedded-engine build host- or network-free.
- The embedded PostgreSQL submodule must be the pinned clean gitlink documented
  by the runbook. Native engine, Compose/PostgreSQL, Qt/ABI, and clean-host
  package gates require their own commands and retained output.
- The absent `./spec` and `../spec` inputs make canonical conformance and ABI
  parity unavailable. A successful local/static command must not be upgraded
  into that claim.
- Backup/DR policy, production recovery guarantees, support period, SLA, and
  later predecessor edges remain deferred. Do not present the application
  ledger or source files as a backup or restore protocol.

## Compatibility summary

| Surface | Current boundary | 1.0 migration claim |
| --- | --- | --- |
| `.orna` source | Backend-neutral typed source model | One portable source; no public backend-specific SQL |
| Managed local endpoint | Private embedded PostgreSQL route | No public PostgreSQL/pgwire/driver compatibility |
| Filesystem endpoint | Bounded direct SQLite route | No full physical/runtime parity claim |
| Explicit Unix/remote endpoint | Bounded current-socket `invoke` only for Unix; remote transport unavailable | No migration or general remote-session support |
| Application source revisions | Typed ledger, hashes, atomic apply | Supported only on a ready compatible engine |
| Engine predecessor upgrade | Empty accepted predecessor/edge sets | No 0.x/development in-place upgrade |
| Standard library | Code through V11; acceptance records through V9 | V10/V11 not promised until evidence is reconciled |
| Package distribution | Tracked minimal `packaging/linux/` artifact recipe; no Debian package | No published production upgrade source |

For command details and the current evidence map, see the
[maintainer runbook](maintainer-runbook.md),
[editor tooling guide](editor-tooling.md),
[1.0.0 release inventory](../CHANGELOG.md), and
[decision index](decisions/README.md).
