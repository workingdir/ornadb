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
- [x] Build the Debian package with one public `/usr/bin/orna` executable and
  embedded PostgreSQL code and support assets.
- [x] Prove installation, initialisation, restart recovery, current upgrade
  boundaries, shell, and
  removal on a clean Debian host with no system PostgreSQL, Docker, or second
  PostgreSQL executable.

## Product expansion

- [x] Complete the first verified CLIENT Boolean function path.
- [x] Add the accepted invocation, authorisation, and public protocol slices
  (sealed `sys.invoke` carriers, protected decisions, invocation audit,
  `orna.std/2` executable source, live dogfooding proof).
- [x] Extend catalogue-backed types with accepted enum identity, syntax,
  catalogue, storage, codec, protocol, and first SERVER execution slices.
- [x] Add a separately accepted record value type.
- [x] Add a separately accepted opaque value type.

## Language tooling

- [x] Expose a context-aware highlight token API from `orna-syntax`.
- [x] Ship the `orna-lsp` language server binary over stdio.
- [x] Prove diagnostics, semantic tokens, symbols, hover, navigation,
  and completion through a framed end-to-end protocol test.
- [x] Land the tree-sitter grammar with corpus tests and queries
  (30 corpus cases pass; every `spec/examples/*.orna` file parses).
- [x] Land the TextMate grammar and VS Code extension (valid package,
  syntaxes, and built `orna-vscode-0.1.0.vsix`).
- [x] Land the Neovim, vim, Helix, Zed, and Emacs integrations
  (configs present; Helix/Zed TOML validated, Emacs loads under batch
  Emacs; Neovim/Vim load checks need their editors at runtime).
- [x] Verify the whole tooling surface against the spec examples
  (tree-sitter parses all ten examples; LSP suite exercises the server).

## Completed foundations

- [x] Implement the accepted SERVER query and mutation slices through required
  unique reference fields.
- [x] Define stable catalogue value-type and binding identities.
- [x] Version standard-library and catalogue hashes without changing version-1
  bytes.
- [x] Define the source-independent standard type manifest.
- [x] Accept the one-executable embedded PostgreSQL boundary.
