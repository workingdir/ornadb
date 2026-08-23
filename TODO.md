# OrnaDB delivery checklist

This checklist tracks user-visible delivery. The work ADRs contain the exact
contracts and small commit sequences. A checked item means that the slice is
implemented, reviewed, committed, and verified locally. Publication is a
separate release action.

## Current focus

- [x] Build and reproduce the pinned PostgreSQL 18.4 backend and initialiser
  archive inputs without producing a PostgreSQL executable or shared object.
- [x] Select the embedded process, resource, shell, upgrade, error, command,
  lifecycle, and complete proof contracts.
- [x] Pin the exact unmodified upstream PostgreSQL 18.4 commit as the
  `third_party/postgresql` submodule.
- [x] Add the top-level `postgresql/` module with reviewed added-file overlays,
  the seven sparse existing-file patches, and one explicit source-update target.
- [x] Add the Make-based prepared-source build beside the selected legacy proof
  and bind its toolchain, upstream tree, overlays, patches, support data,
  lifecycle probe, and verifier.
- [x] Prove exact legacy/prepared-source archive, support, symbol, licence, and
  entry evidence parity while independently running the new lifecycle twice.
- [x] Cut over atomically to `postgresql/Makefile`, delete the legacy builder
  entry point, then remove the inert `packaging/postgresql` prototype files in
  small commits.
- [x] Accept the offline `orna source check <file.orna>` contract.
- [x] Add the Orna-owned instance model, cluster initialisation, private Unix
  socket authentication, and foreground PostgreSQL supervision.

## First usable Orna workflow

- [x] Retain and verify the exact `std` source needed by application checking.
- [x] Resolve application type names through the verified standard catalogue.
- [x] Implement `orna source check <file.orna>` without PostgreSQL, network
  access, configuration, or filesystem writes.
- [x] Prove exact diagnostics, byte spans, exit statuses, and no dependency on
  a running server.

## Embedded distribution

- [x] Bind `orna server backend-shell` to the Orna-owned private SQL console.
- [x] Define the first release's current-engine no-op and fail-closed
  unsupported-engine upgrade boundary.
- [ ] Add the durable same-major transition when a release first declares a
  real predecessor edge, then define the later multi-major transition without
  an external `pg_upgrade` executable.
- [x] Exercise installation, initialisation, restart recovery, current
  engine upgrade boundaries, shell, and removal in the isolated
  network-disabled Debian container scenario.
- [ ] Prove the complete lifecycle on a fresh network-disabled Debian 12
  amd64 host or VM without Docker, a host PostgreSQL installation, or a
  second PostgreSQL executable; archive the machine, package, manifest,
  process-closure, trace, and lifecycle evidence required by work ADR 0019.

## Product expansion

- [x] Complete the first verified CLIENT Boolean function path.
- [x] Add the accepted invocation, authorisation, and public protocol slices
  (sealed `sys.invoke` carriers, protected decisions, invocation audit,
  `orna.std/2` executable source, live dogfooding proof).
- [x] Extend catalogue-backed types with accepted enum identity, syntax,
  catalogue, storage, codec, protocol, and first SERVER execution slices.
- [x] Add a separately accepted record value type.
- [x] Add a separately accepted opaque value type.
- [x] Add the installed `orna source diff` semantic diff surface
  (identity-keyed add/drop/rename report, no apply, live proof;
  work ADR 0066).
- [x] Register `std.ui.UI`, the deterministic TTY runtime offer, and the
  server-side `sys.inspect` core with installed proofs (work ADRs 0062-0064).
- [x] Add security administration and the sealed CSV output presenter
  (work ADRs 0065 and 0067).
- [x] Add CLIENT expression bodies and external `RUNTIME CONTRACT` clauses,
  including version-three plans, closed evaluation, and installed proof
  (work ADR 0068).
- [x] Add CLIENT STATE declarations, version-four plans, and the authenticated
  USER state lifecycle with conflict handling (work ADRs 0069-0070).
- [x] Add the executor-independent CLIENT resource lifecycle and runtime-only
  executor seam (work ADRs 0071 and 0074).
- [x] Add sealed system identity calls, ORV6 SET transport, and the
  `std.json.Value` standard snapshot with persisted V5 executable recovery
  (work ADRs 0072, 0073, and 0075).
- [x] Add the test-only headless runtime conformance fixture and focused
  lifecycle proof (work ADR 0076).
- [x] Add the CLIENT-to-SERVER resource language surface, including typed
  resource and stream constructors with `AWAIT` (work ADR 0077).
- [x] Add authenticated `ORNA-RESOURCE/1` transport, stream credits,
  cancellation, terminal ordering, and bounded scheduling (work ADR 0078).
- [x] Add CLIENT action values and `std.action.call`, including checked plans
  and trigger lifecycle proof (work ADR 0079).
- [x] Deliver the headless ordinary CLIENT Inspector v1 accepted by work ADR
  0080 and the generic standard render contract `std.inspect.render@1` accepted
  by work ADR 0081. The production graphical runtime/UI sink, populated
  resource/UI projections beyond the accepted headless scope, and reflective
  gateways remain proposal-only.
- [ ] Implement reflective JSON-RPC and MCP gateway programs after the
  canonical Exposure and Service value contracts become accepted executable
  specifications.

## Language tooling

- [x] Expose a context-aware highlight token API from `orna-syntax`.
- [x] Ship the `orna-lsp` language server binary over stdio.
- [x] Prove diagnostics, semantic tokens, symbols, hover, navigation,
  and completion through a framed end-to-end protocol test.
- [x] Land the tree-sitter grammar with corpus tests and queries
  (the executable spec examples and repository fixtures parse without errors;
  proposal-only UI examples remain outside the executable language gate).
- [x] Land the TextMate grammar and VS Code extension (valid package metadata
  and syntax files; use the documented command to build a VSIX when needed).
- [x] Land the Helix, Zed, and Emacs integrations (TOML validated;
  Emacs loads under batch Emacs).
- [ ] Verify the Neovim and Vim integrations at runtime
  (blocked because those editor binaries are not installed on this workstation).
- [x] Verify the executable tooling surface against the spec examples
  (tree-sitter parses executable examples; LSP suite exercises the server).

## Completed foundations

- [x] Implement the accepted SERVER query and mutation slices through required
  unique reference fields.
- [x] Define stable catalogue value-type and binding identities.
- [x] Version standard-library and catalogue hashes without changing version-1
  bytes.
- [x] Define the source-independent standard type manifest.
- [x] Accept the one-executable embedded PostgreSQL boundary.
