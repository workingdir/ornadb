# OrnaDB delivery checklist

This checklist tracks user-visible delivery. The work ADRs contain the exact
contracts and small commit sequences. A checked item means that the slice is
implemented, reviewed, committed, and pushed.

## Current focus

- [ ] Produce the deterministic private PostgreSQL 18.4 runtime twice and
  prove identical payload and manifest bytes.
- [x] Accept the offline `orna source check <file.orna>` contract.
- [ ] Verify and accept the first signed private PostgreSQL runtime.
- [ ] Add the Orna-owned instance model, cluster initialisation, private Unix
  socket authentication, and foreground PostgreSQL supervision.

## First usable Orna workflow

- [ ] Retain and verify the exact `std` source needed by application checking.
- [ ] Resolve application type names through the verified standard catalogue.
- [ ] Implement `orna source check <file.orna>` without PostgreSQL, network
  access, configuration, or filesystem writes.
- [ ] Prove exact diagnostics, byte spans, exit statuses, and no dependency on
  a running server.

## Self-contained distribution

- [ ] Bind `orna server backend-shell` to the verified private `psql` binary.
- [ ] Implement safe same-major and major PostgreSQL runtime upgrades.
- [ ] Build the Debian package with one public `/usr/bin/orna` command and the
  signed private PostgreSQL tree.
- [ ] Prove installation, initialisation, restart recovery, upgrade, shell, and
  removal on a clean Debian host with no system PostgreSQL or Docker.

## Product expansion

- [ ] Complete the first verified CLIENT Boolean function path.
- [ ] Add the accepted invocation, authorisation, and public protocol slices.
- [ ] Extend catalogue-backed types beyond the standard primitive set through
  separate accepted decisions for enum, record, and opaque value types.

## Completed foundations

- [x] Implement the accepted SERVER query and mutation slices through required
  unique reference fields.
- [x] Define stable catalogue value-type and binding identities.
- [x] Version standard-library and catalogue hashes without changing version-1
  bytes.
- [x] Define the source-independent standard type manifest.
- [x] Accept the private PostgreSQL runtime ownership and distribution contract.
