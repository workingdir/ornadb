# 25. Schema evolution, semantic diff and merge {#evolution}


A physical object hash is not a row's identity. A row has a database/table/
typed-key address. Its stored fields have semantic values. Source and schema
revisions define how those values are interpreted; Git identifies the whole
published state.

**ORNA-EVOL-001** Stable table/field identities MUST survive supported renames; a textual name match alone MUST NOT establish continuity.
**ORNA-EVOL-002** Schema changes MUST be admitted against affected rows, references, assertions, indexes and stored codecs before visibility.
**ORNA-EVOL-003** Historical rows MUST be decoded under their historical schema/value profile; current source MUST NOT silently reinterpret them.
**ORNA-EVOL-004** Semantic diff MUST compare logical fields and Blob annotations/content, not storage page or Git-layout IDs.
**ORNA-EVOL-005** A three-way merge MUST validate the merged program/schema/data/content closure and constraints before publishing; a conflict-free Git text merge is not sufficient.
**ORNA-EVOL-006** Conflicting replacements of one Blob MUST be reported as value conflicts unless an explicit application merge function supplies a validated replacement; concatenating chunk lists is not a default merge.
**ORNA-EVOL-007** Explicit rekey MUST use normal immutable-key/reference rules and emit one logical rekey with old/new address, not a filename guess.
**ORNA-EVOL-008** A representation-only rewrite MUST preserve semantic rows and content commitments. Defaulting new annotations in migration is an explicit semantic migration, not falsely an empty semantic diff.
**ORNA-EVOL-009** Required introduction defaults/fallbacks MUST be frozen under the source/schema revision rather than reevaluated on every historical read.
**ORNA-EVOL-010** A failed migration/merge/checkout MUST preserve accepted state and conflicting user edits; no automatic ref reset, inferred winner or unknown-schema overwrite is allowed.

All current new-format writes use one profile. Existing old-format readers are
compatibility code, not user-selectable storage personalities. The precise
format-1/2-to-3 transition is in [upgrade](#upgrade).
