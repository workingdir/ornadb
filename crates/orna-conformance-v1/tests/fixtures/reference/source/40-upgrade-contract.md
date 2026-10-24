# 37. Compatibility, migration and verification {#upgrade}


## The explicit coordinates

The language/specification version is **1.1.0**. This final source publication
is identified by `final-2026-10-05` and the exact artifact checksum. It replaces
the supplied earlier 1.1.0 authoring draft rather than pretending those
incompatible drafts have identical normative content.

The new writer coordinate is **repository format 3**. Its single pinned set
is OGS-1 / ORP-1 / OGB-2 / ROV-3 / SOV-3 / OVB-2 / VFS-1 / MIME-1. These are
format names, not user-selectable per-table storage strategies.

**ORNA-UPGRADE-001** Implementations MUST dispatch readers by recorded repository/value formats, not the CLI version or the current host's defaults.
**ORNA-UPGRADE-002** Existing format-1/2 commits MUST remain immutable and readable under their original decoders and equality/digest semantics.
**ORNA-UPGRADE-003** New-format writes MUST use one canonical row store; loose/compact/hybrid dual-writing is forbidden.
**ORNA-UPGRADE-004** Legacy Blob annotation defaults MUST be explicit: octet-stream and no suffix, without downloading or decoding every payload during metadata migration.
**ORNA-UPGRADE-005** Annotation migration MUST be reported as a semantic profile transition; preservation of raw bytes is not proof of unchanged annotated value identity.

## UPGRADE-3 (explicit, no background conversion)

1. Inspect the actual checkout, uncommitted work, gitlinks, runtime tail and
   format readers. Do not reuse a stale commit hash from a previous audit.
2. Freeze/pin a coherent source/schema/data candidate. Preserve unrelated
   staged and unstaged source and dirty submodule work. Require explicit
   resolution of incompatible staged store/schema changes.
3. Build the new graph privately. Decode exactly one authoritative legacy row
   per key; validate continuity, schema, assertions and references. Reuse exact
   existing payload objects/chunks where compatible; rebuild metadata/index
   layout. Keep aliases/legacy roots until all needed data is protected.
4. Convert published system records into typed store relations. Preserve
   non-rewindable allocator refs and current ownership separately. Map
   persistent consumer/checkpoint identities with recorded predecessor links
   so codec changes cannot silently rerun an already processed stream.
5. Compute format-3 schema/row/consumer domains and explicit old-to-new value
   comparison. Write `database.orna` and the new root; no `format.orna` or old
   current authoritative row folders remain in the candidate.
6. Validate closure and pure decoding. If old layout transformation needs
   unavailable row bytes, report them and stop that migration; lazy payloads
   may remain promised only with inherited verified descriptor/provenance.
7. Prepare one reviewed migration Git commit through PUB-3. Journal both
   sides and preserve the original commit/tail for recovery. Do not auto-push.
8. Verify old and new snapshots independently. Install the new writable
   reader/root only after the protected transition completes. Rollback means
   selecting an old compatible snapshot, not pretending format-3 is format-1.

**ORNA-UPGRADE-006** A migration failure MUST preserve the old authoritative snapshot/tail and all user edits; it MUST NOT partially change the active writer.
**ORNA-UPGRADE-007** Source and API changes MUST be atomic with the format transition; removed storage-preference APIs MUST not secretly choose legacy layouts.
**ORNA-UPGRADE-008** Consumers with Blob-containing arguments MUST not reset progress because the argument encoding domain changed; require an explicit validated migration map or fail closed.
**ORNA-UPGRADE-009** New protocols MUST explicitly negotiate new value codecs; old live endpoints and fingerprints MUST remain byte-for-byte legacy-compatible for their admitted values.
**ORNA-UPGRADE-010** Unsupported annotated values on legacy wire MUST fail before effects or use the specified retained-without-value result after an already committed operation. No implicit downcast to bytes.

## Upgrade of the implementation itself

Keep current parser/evaluator/embedded ownership/partial-clone code where
correct. Move production work out of conformance-only wrappers when necessary,
not into another parallel engine. Archive old write formats but retain their
readers and golden vectors. The current `api/sys.json` is the final contract;
its retired storage operations are recorded in `api/retired.json`.

An independent read-only extractor MUST recover modules, schema, rows and raw
content from a complete snapshot without running old application code or
contacting an AI provider. Compile/cache artifacts are optional for recovery.
Ten-year recoverability is a design/test obligation, not a promise that old
native executables will run forever.
