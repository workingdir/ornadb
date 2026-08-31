# Changelog

This file is the user-facing inventory for the OrnaDB source tree. It is not a
Debian changelog, a package manifest, or proof that a production artifact has
been published.

## 1.0.0 — release inventory (publication pending)

**Status:** The Cargo workspace currently identifies itself as `1.0.0`, but this
checkout is not a published production release. The canonical normative
`spec` bundle is absent from both `./spec/` and `../spec/`, so this inventory
must not be read as a conformance claim.

The accepted release-mechanics decision reserves a future Debian 12 amd64
release identity of `orna 1.0.0-1` with a signed `v1.0.0` source tag. That
decision does not declare the product baseline complete. This checkout now has
a minimal `packaging/linux/` provenance/install artifact recipe and focused
tests, but it has no Debian changelog, signed artifact, or clean-host
production-package evidence.
See the [release-mechanics decision](docs/decisions/0047-first-one-zero-release.md)
for the required release authority and the
[maintainer runbook](docs/maintainer-runbook.md) for evidence rules.

### Product boundary

- OrnaDB is a backend-neutral `.orna` source language and typed application
  model. A source file does not select a PostgreSQL or SQLite dialect, and the
  public product does not ask users to write backend-specific SQL.
- The managed local implementation uses a private embedded PostgreSQL kernel.
  It is not a public PostgreSQL server: there is no promise of pgwire, TCP SQL,
  PostgreSQL driver compatibility, external PostgreSQL extensions, or a host
  PostgreSQL process. The [embedded-engine decision](docs/decisions/0019-embedded-postgresql-engine.md)
  records this boundary.
- A filesystem database path selects the direct SQLite adapter. That adapter is
  a bounded implementation/runtime route for the same source model; it does
  not define a second source language. Physical operations the adapter cannot
  represent are rejected rather than given backend-specific semantics.
- `orna://` and `orna+unix://` endpoint forms are parsed, but explicit Unix and
  remote transport routes are not available in this checkout. They fail closed
  before command execution until the corresponding client transport and
  authentication work is implemented.

### Implemented source and server surface

The following behaviors are present in the source tree and are exercised by
checked-in unit, integration, or accepted-fixture coverage. The list describes
implementation boundaries, not a claim that every environment-dependent
release gate has passed.

- `orna source check <file.orna>` checks one regular UTF-8 source file without a
  database, network access, configuration, child process, or write. It is a
  standalone check against the empty application catalogue; it does not prove
  continuity with an installed revision.
- `orna source diff <file.orna>` prepares a read-only semantic comparison with
  the active application revision. `orna source apply <file.orna>` checks and
  prepares one source file, records a typed application migration, and commits
  the candidate atomically. Diagnostics, an active-base race, an unsupported
  physical operation, or failed post-apply hash reproduction fails closed.
- The source compiler and accepted fixtures cover bounded server functions,
  typed scalar/reference/record values, identity-selected mutations, client
  expression bodies, local/session/user state declarations, resources and
  actions, JSON, bounded table presentation, and the accepted structural UI
  constructors. The [accepted demo inventory](examples/accepted-demos.toml)
  is the authoritative list of checked-in examples; it is not a replacement
  for the missing canonical spec.
- The CLI exposes function invocation, the function-backed REPL, daemon/server
  administration, runtime description, source check/diff/apply, raw calls,
  USER state, inspection, and security administration. `--explain`, argument
  files, canonical output, JSON/table/CSV presenters, and bounded invocation
  tracing are implemented where the selected route supports them.
- USER state is principal-scoped and typed. Local peer authentication supplies
  the principal; optimistic revision conflicts and invalid expected types fail
  closed. State values use the canonical ORV5 value representation.
- The Inspector exposes immutable invocation epochs, eight closed projections,
  redacted-by-default records, privilege checks, and sequence-addressable
  traces. Local SQLite inspection is narrower and stores only bounded,
  redacted invocation/inspection metadata; it does not expose durable resource
  payloads or arbitrary stored values.
- The installed `tty` runtime renders terminal documents and byte streams. The
  runtime/value boundary uses canonical ORV5 values and ORV6 for checked sets;
  output framing is validated before bytes are written. The Qt path is not a
  production-support claim for this inventory: its ABI/native checks require
  the absent canonical header and separately gated native dependencies.
- `orna-lsp`, the Tree-sitter grammar, the TextMate grammar, and editor
  integration packages provide the checked-in static tooling surface. LSP
  diagnostics, symbols, hover, references, completion, semantic tokens, and
  pull/push diagnostics are implemented. Static gates do not launch editor
  hosts and do not prove parity with the absent spec or host-session behavior.

### Storage and compatibility contracts

- Internal application migrations use a closed, typed, backend-neutral
  migration representation. The source is lowered by each storage adapter;
  users must not edit generated SQL or internal `_orna_kernel` tables. The
  application migration ledger retains expected and candidate source/catalogue
  revision pairs, canonical bytes, and digests.
- The PostgreSQL migration registry currently contains internal migrations
  through version 47, including the application-migration ledger and its
  baseline data step. The baseline preserves historical source/catalogue
  lineage with empty physical artifacts; it does not reconstruct unrecorded
  pre-ledger physical operations.
- Revision recovery validates lineage, active pointers, ledger order, canonical
  digests, and post-apply reproduction. Historical source and standard
  snapshots are immutable inputs to this verification; hand-editing retained
  rows is unsupported.
- The implementation currently retains and opens standard-library snapshots
  through `orna.std/11`, with code paths for the sequential V1-to-V11 chain.
  However, the current maintainer runbook and decision index still describe the
  accepted chain as V1-to-V9, and no tracked decision record for the V10/V11
  addition is present. V10/V11 are therefore recorded here as implementation
  evidence, not as a 1.0 compatibility promise. This discrepancy must be
  reconciled in the product baseline before publication.
- Local server sockets use the private Orna protocol handshake (versions 1–5,
  with bounded fallback behavior where an opaque codec registry is unavailable)
  and local peer authentication. These are private Orna contracts, not a
  public SQL or remote transport protocol.

### Explicitly deferred, unavailable, or not promised

The following boundaries are intentional and must not be inferred as shipped
from the implementation inventory:

- Full language/spec conformance, canonical examples, and ABI parity: the
  canonical bundle is absent, so no conformance or ABI result is claimed.
- The checked-in accepted corpus is intentionally bounded. Examples recorded
  as deferred include broader ALTER/drop/rename forms, procedural declarations,
  top-level INSERT, list/map/record literals and lambdas, broader SERVER
  expression calls/operators, additional CLIENT return forms, and
  value-type/user/role/grant/revoke declarations.
- Remote Orna TLS sessions, explicit Unix-socket client routing, reflective
  JSON-RPC/MCP gateways, a production CLIENT VM/sandbox, arbitrary toolkit or
  browser runtimes, and richer Studio workflows.
- PostgreSQL/SQLite physical or runtime parity beyond the bounded adapter
  capabilities described above. Unsupported SQLite physical shapes fail
  closed; the source language remains backend-neutral.
- Native embedded-engine, Compose/PostgreSQL, Qt/ABI, editor-host, and
  clean-host distribution checks unless a named command has been run with
  retained evidence. `CARGO_NET_OFFLINE=true` only makes Cargo dependency
  resolution fail closed; it does not make an embedded-engine build host- or
  network-free.
- A production package, repository publication, signing, SBOM/package
  inventory, support period, SLA, backup/DR policy, or recovery guarantee.
  CI retention artifacts and local build outputs are not production
  distribution authorities.
- An in-place upgrade from a 0.x or development engine. The first-release
  decision defines empty accepted predecessor/forward-edge sets; no package
  predecessor migration is implemented. There is no `orna server upgrade`
  command in the current CLI.
- RPM packaging or cross-format upgrade support.

For migration steps and compatibility cautions, see the
[1.0.0 migration guide](docs/migration-1.0.0.md). For the commands, prerequisites,
and evidence vocabulary used by maintainers, see the
[maintainer runbook](docs/maintainer-runbook.md) and
[editor tooling guide](docs/editor-tooling.md).
