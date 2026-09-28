# Work ADR 0109: Defer Publication Object Encoding in `orna-artifact`

**Status:** Deferred; this ruling adds no publication behavior

## Decision

Do not change `crates/orna-artifact` for ORNA-PUB-005 or PUB-1 step 3. The
crate encodes and decodes executable client/server plans and parameter
artifacts. Its shared `artifact_codec` is a bounded binary reader/writer for
those formats; it is not a compact-segment, manifest, Git-object, ref, or
durability adapter.

The publication-object completeness boundary belongs to the accepted compact
publication path. Beads `ornadb-gov5.10.14` (#916) defines the typed
runtime-to-encoder boundary. Its storage consumer lowers the frozen
mutations, and the repository publication path creates and verifies the
immutable objects before ref advancement. The merged publication work in
PRs #1938, #1946, and #1947 continues at those storage/repository owners.

The exact prerequisite for reopening `orna-artifact` is an accepted contract
that assigns a real immutable-publication-object consumer and its complete
encoding boundary to that crate. Without that consumer, adding a publication
codec here would create an unused format and would not prove completeness
before ref visibility.

## Normative boundary

- PUB-1 step 3 (`source/22-publication.md:39`) requires immutable segments,
  schemas, and manifests for the frozen batch, complete Git objects to be
  written and flushed, and repeated mutations to be folded in transaction
  order.
- `ORNA-PUB-005` (`source/22-publication.md:49`) requires complete objects and
  their required durability barriers before a visible ref names them.
- `ORNA-PUB-006` (`source/22-publication.md:51`) permits unreachable objects
  after a pre-ref failure but requires the pending batch to remain intact.

These clauses do not assign the publication-object format or its consumer to
`orna-artifact`.

## Ownership evidence

- `crates/orna-artifact/src/lib.rs:1-16` declares canonical executable
  artifacts independent of source syntax and storage backend, with client and
  server plan codecs.
- `crates/orna-artifact/src/artifact_codec.rs:1-135` contains only bounded
  byte writing/reading and primitive identity encoding. It has no Git object,
  compact manifest/segment, publication ref, or durability API.
- `crates/orna-compiler/src/relational.rs:1207-1234` calls the server-plan
  encoders. `crates/orna-client/src/lib.rs:2703-2721` dispatches executable
  client-plan decoding. These are plan artifacts, not PUB-1 objects.
- `crates/orna-storage-v1/src/compact.rs:804-925` lowers the typed runtime
  freeze into compact writer input. `crates/orna-repository-v1/src/compact.rs:3555-3622`
  hashes compact segment bytes, adds canonical manifest files, builds the
  private commit, and verifies its manifest binding.

Search evidence on 2026-09-28:

```text
$ rg -n "CompactSegment|CompactManifest|hash_object|git object|fsync|durability|PublicationFreeze|ref.advance" crates/orna-artifact
exit code: 1 (no matches)
```

The absence is scoped to the checked-in `orna-artifact` crate paths. No
publication format or behavior is inferred from its executable-plan codecs.

No Orna source participates in this documentation-only ruling. Cargo tests are
recorded in the implementing issue/PR as a boundary check; no source behavior
or `ORNA-*` requirement is changed here.
