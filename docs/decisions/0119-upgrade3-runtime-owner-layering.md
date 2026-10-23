# Work ADR 0119: UPGRADE-3 Runtime Owner, Layering and Annotation Coordinate

**Status:** Accepted

## Decision

This ADR records four choices that the remaining UPGRADE-3 implementation
depends on. They implement UPGRADE-3 steps 3, 7 and 8 and ORNA-UPGRADE-004 and
-005 in `reference/Orna-1.1.0/source/40-upgrade-contract.md`, which leaves the
owning process, the crate layering and the annotation coordinate open. They
change no specification text, reader or on-disk format.

### Runtime owner of activation

The process that runs `orna upgrade --to-format 3` is the publisher for the
migration. It:

* takes the coordination lock and writer lease that `orna publish` takes, with
  lease owner `invocation_owner_id(identity)`
  (`crates/orna-cli-v1/src/main.rs`);
* calls `publish_legacy_format_migration` with the prepared publication journal,
  which carries the migration continuity record and the
  `MigrationAnnotationDefaults` coordinate;
* completes the migration's runtime intent in the same process after the
  protected transition, then releases the lock and the lease.

No daemon owns activation. If the process stops after the journal is durable,
the publication journal is the recovery record, and `recover_publication`
(`crates/orna-repository-v1/src/lib.rs`) finishes it. That function already runs
from `orna commit` after review (`crates/orna-cli-v1/src/cli_commit.rs`) and from
the storage crate's publication path (`crates/orna-storage-v1/src/lib.rs`).
`orna upgrade` is refused in this build (`crates/orna-cli-v1/src/cli_args.rs`),
so the upgrade path reaches recovery only once that surface is enabled.

### Layering

* Legacy row decoding and store-building orchestration live in
  `orna-application-v1`. Its production dependencies already include
  `orna-project-v1` and `orna-repository-v1`.
* `migration_copy.rs` in `orna-repository-v1` receives already-decoded tables,
  rows and relations as input. It decodes no legacy row and calls no project or
  conformance code.
* Conformance row admission is reached through `orna-application-v1`, which may
  gain a production dependency on `orna-conformance-v1`. That edge closes no
  cycle: `orna-cli-v1` is the only crate that depends on `orna-application-v1`.
* `orna-repository-v1` gains no production dependency on `orna-project-v1` or
  `orna-conformance-v1`. Both already depend on `orna-repository-v1` in
  production, so such an edge would close a cycle.

### Annotation-default coordinate

ORNA-UPGRADE-004 requires the legacy Blob defaults to be explicit: octet-stream
and no suffix, applied without downloading or decoding every payload during
metadata migration. ORNA-UPGRADE-005 requires the annotation change to be
reported as a semantic profile transition, because preserving raw bytes does not
prove unchanged annotated value identity.

* The defaults are counted while the metadata of legacy Blob entries is decoded.
  No payload is read. `MigrationAnnotationDefaults::from_legacy_inventory`
  already reports `payload_inspected() == false`.
* The decoded count is the coordinate `annotation_defaults`. It is recorded in
  `.orna/migration.json` (`MIGRATION_RECORD`) and in the publication journal.
* `--annotation-defaults N` keeps its name and becomes an expected-count
  assertion. A mismatch with the decoded count refuses before any copy is
  created. The flag no longer declares the coordinate.
* Format-2 compact input stays outside this decision. The specification names
  Apache Parquet 2.13.0 for legacy compact-input interpretation
  (`reference/Orna-1.1.0/source/35-references.md`) and gives no record shape for
  format-2 compact rows, so no reader for them is added until one is specified.

### Legacy row sources and precedence (D4)

UPGRADE-3 step 3 requires exactly one authoritative legacy row per key. A
format-1 commit can hold rows in two sources, and the decoder reads each one
separately.

* Compact rows are keyed by table UUID and encoded row key.
  `Repository::read_legacy_authoritative_tables`
  (`crates/orna-repository-v1/src/compact.rs`) resolves them. For each key the
  mutation with the greatest generation wins, and a winning deletion leaves the
  key absent. Two mutations of one key at one generation fail closed with
  `InvalidCompactManifest`. Format-2 and format-3 commits are refused.
* Loose rows are identified by their table directory (`table_path`) and key
  path, not by table UUID. Two loose rows of one table that decode to the same
  key fail closed.

The two sources are not merged, and the decoder chooses nothing between them.
`ornadb-5il1.313` (S4) decides the cross-source precedence rule from the
specification before the copy consumes these rows. Until then, no consumer
treats the decoded rows as the complete authoritative set.

## Rationale

* UPGRADE-3 step 3 requires exactly one authoritative legacy row per key,
  decoded purely. Decoding in the application layer keeps the repository crate
  free of project and conformance production dependencies, which would close a
  cycle.
* Reusing the publish lock, lease and journal keeps one writer path and one
  protected transition. The journal already carries the migration coordinates,
  so recovery needs no new state.
* A declared count alone records nothing about the metadata the migration read.
  Recording the decoded count makes the migration report derive from the legacy
  metadata (ORNA-UPGRADE-005). The flag then checks the operator's review
  against it.

## Consequences

* The CLI help (`crates/orna-cli-v1/src/cli_help.rs`) and
  `crates/orna-cli-v1/README.md` describe `--annotation-defaults` as the
  operator-supplied decoded count today. When the decoder lands, both change to
  describe an expected-count assertion.
* Until the decoder lands, `annotation_defaults` records the operator-declared
  count that `migrate copy --annotation-defaults N` takes today.
* Once the `upgrade` surface is enabled, `orna upgrade --to-format 3` holds the
  publish lock and writer lease for the whole protected transition, and a
  concurrent `orna publish` contends for the same lease.
* A crash after the publication journal is durable, but before runtime
  completion, is finished by `recover_publication`. No new recovery command is
  added.
