# 23. Physical storage and representation {#storage}

## Logical transparency

**ORNA-STORAGE-001** Editable and compact storage MUST expose the same logical table, query and mutation interface.

**ORNA-STORAGE-002** Callers MUST NOT need to know the physical profile to query a table.

## Loose profile

The loose profile stores one row per `.orna` row unit and is optimized for human editing and Git-style browsing.

**ORNA-STORAGE-003** Loose profile row paths MUST follow the table namespace, table name and key path encoding.

**ORNA-STORAGE-004** Direct row-file edits MUST remain reviewable through ordinary Git diffs.

## Compact storage profile

Large or high-rate rows use **compact storage**: immutable, key-sorted batches encoded as **Apache Parquet with Zstandard compression** under the repository-format-1 profile.

**ORNA-COMPACT-001** Compact storage MUST expose exactly the same logical ordered key-to-row table, query, mutation, history and merge semantics as editable row storage.

**ORNA-COMPACT-002** Every compact data file MUST be a valid Parquet file using data-page version 2, Zstandard compression, page checksums and complete rows sorted by the table's full canonical primary key.

**ORNA-COMPACT-003** Stable Orna column `ObjectId` values, logical types, units, currency identities, schema revision and physical encodings MUST be recorded in file metadata. Readers MUST resolve columns by stable identity rather than current display name.

**ORNA-COMPACT-004** Computed fields MUST NOT be physically stored as authoritative row values. They are evaluated from stored dependencies under the requested snapshot.

**ORNA-COMPACT-005** The writer target is 16 MiB compressed per data file, with a normal range of 8-32 MiB and a hard maximum of 64 MiB. A row group closes at 65,536 rows or 16 MiB uncompressed, whichever occurs first. An age-triggered publication MAY produce a smaller valid file.

**ORNA-COMPACT-006** New rows create immutable `data` files. Changes to existing keys create immutable complete-row `replacement` files. Deletions create immutable key-only `deletion` files. Within one branch lineage, the visible mutation for a key with the greatest publication generation wins. Numeric generations MUST NOT be used to choose a winner between concurrent branch mutations; semantic three-way merge resolves or conflicts them.

**ORNA-COMPACT-007** Compact files MUST NOT be routinely rewritten merely to combine older batches. An explicit consolidation is permitted only when overlays materially harm performance; its estimated extra retained history MUST be shown before execution and its semantic diff MUST be empty.

**ORNA-COMPACT-008** Each table's compact state MUST be described by a sharded canonical manifest. A shard contains at most 256 entries. Every entry records segment identity, role, profile and encoder version, schema fingerprint, Git object ID, key bounds, applicable time bounds, row count, compressed size, column descriptors and index availability; physical statistics are in the verified Parquet metadata.

**ORNA-COMPACT-009** Query planning MUST prune manifest shards, files, row groups and pages whose key/statistic bounds cannot satisfy the query, and MUST read only required columns where the physical format supports projection.

**ORNA-COMPACT-010** Unknown required profile or encoder versions MUST be rejected before partial logical results are returned.

**ORNA-COMPACT-011** Publication MUST follow [PUB-1](#publication), including ordinary-index reconciliation and local watermark recovery. A visible committed snapshot MUST never refer to an incomplete file or leave a staged inverse of managed publication data.

**ORNA-COMPACT-012** The format MUST support exact Orna values. Common primitive columns use standard Parquet logical/physical encodings. Values outside a selected optimized physical representation use a versioned lossless fallback encoding; they MUST NOT be rounded or passed through a binary `Float` implicitly.

**ORNA-COMPACT-013** A production compact-storage claim requires the published benchmark/fault profile: at least 10,000 samples/second for 24 hours on declared hardware, zero lost acknowledged rows, zero duplicate logical rows, bounded memory, stable storage no larger than 1.5 times the same corpus in Prometheus blocks, selective pruning, and all publication fault points passing. Failure blocks the production claim; it does not change the normative format or table semantics.

**ORNA-COMPACT-014** A publication attempt MUST read the table manifest from its expected base HEAD and use that manifest's `next_generation` as the candidate generation. All files created by the attempt share that generation. Before encoding, pending mutations are collapsed to one final mutation per key, so one generation cannot contain two authoritative mutations for the same key. Compact-storage precedence has no separate semantic `sequence` number.

**ORNA-COMPACT-015** If branch compare-and-swap fails, the candidate generation was never committed. The publisher MUST reload the current HEAD and table manifest and rebuild the manifest using the generation obtained from that base. Immutable data files MAY be reused because generation is manifest metadata; a stale candidate generation MUST NOT be forced onto a changed table manifest.

**ORNA-COMPACT-016** Independent branches MAY assign the same generation to disjoint mutations. During semantic merge, generation never resolves concurrent changes to the same key: identical mutations coalesce, different mutations create a typed row conflict, and disjoint mutations coexist. The merged manifest sets `next_generation` greater than every retained generation; an explicit conflict resolution is written in a fresh generation.

**ORNA-COMPACT-017** A compact `Float` column that emits bounds MUST declare Parquet `IEEE_754_TOTAL_ORDER`. Every emitted column-chunk `Statistics` object MUST contain exact `nan_count`, and every emitted `ColumnIndex` MUST contain one exact `nan_counts` entry per page. Bounds MUST be the smallest and largest non-NaN values under `FLOAT-TOTAL-1`; if all non-null values in the scope are NaN, they MUST instead be the smallest and largest NaN values under that order. Missing, malformed or inconsistent NaN counts make NaN presence unknown and MUST disable every pruning decision that is not safe under that uncertainty. Float statistics MUST never cause a false-negative query result.

**ORNA-COMPACT-018** A compact file's `schema_id` identifies the schema revision used to encode it. Reading that file through another compatible snapshot MUST project columns by stable field `ObjectId` and apply the schema-evolution rules: renamed fields retain identity, missing optional fields become `null`, frozen introduction fallbacks are read from committed schema metadata, and computed fields are evaluated rather than loaded.

**ORNA-COMPACT-019** In a valid committed manifest lineage, two authoritative mutations for the same key and generation are corruption. The condition does not apply to unresolved inputs from independent branches before semantic merge; those inputs are compared as branch changes and either coalesced or reported as conflicts.

The following sections define physical mappings, manifest schemas and [cross-reader tests](#storage-cross-reader-obligation).

## Hybrid storage and placement

One table may contain some keys as editable `.orna` row files and other keys in compact batches. This avoids a destructive whole-table mode switch.

**ORNA-STORAGE-005** In a valid snapshot, one logical key MUST have exactly one authoritative physical representation. A loose row and compact row for the same key is corruption unless an in-progress publication shadow is hidden from snapshot readers and has an identical canonical row hash.

**ORNA-STORAGE-006** Direct creation of a valid row file creates an editable row. Updating an existing editable row through the table API preserves editable placement. Updating or deleting an existing compact row produces a compact replacement or deletion.

**ORNA-STORAGE-007** Every table has a committed placement preference with one of three values: `automatic`, `editable` or `compact`. The default is `automatic`. The preference is operational metadata, not part of the table's logical type.

**ORNA-STORAGE-008** Under `automatic`, a table with no compact data publishes programmatic rows as editable while the resulting table has at most 10,000 rows, the publication contains at most 8 MiB of canonical row bodies and every row path is representable. Otherwise that publication uses compact storage. Once compact data exists, later programmatic insertions default to compact storage; existing editable rows remain editable.

**ORNA-STORAGE-009** `editable` requires programmatic new rows to use editable files and fails before mutation when a key/path cannot be represented. `compact` directs programmatic new rows to compact storage. Neither preference prevents an explicit valid direct row-file edit, so a table can remain hybrid.

**ORNA-STORAGE-010** Advanced storage preference and rewrite operations use explicit typed administration. A caller obtains the table's `sys.TableRef`, then calls `sys.admin.set_storage_preference(table.reference, sys.StoragePreference.editable)` or `sys.admin.rewrite_storage(table.reference, to: sys.StorageRewriteTarget.compact)`. `sys.storage` remains a read-only grouping namespace; these operations add no language grammar or built-in CLI command family.

**ORNA-STORAGE-011** A rewrite between editable and compact placement is one recoverable storage-only operation. It MUST preserve every logical key/value and snapshot ordering, produce an empty semantic row diff, and refuse conversion to editable form when any key/path or resource bound would be violated.

**ORNA-STORAGE-012** Storage placement is visible through `sys.Storage`, but ordinary table reads and writes MUST NOT require callers to branch on physical placement.

**ORNA-STORAGE-013** Editable/compact key disjointness MUST be checked when a program inserts a row, when direct row files are discovered or reconciled, when a snapshot is opened or checked out, when a row is re-keyed, when storage is rewritten, when compact data is published, when repositories are verified, and when semantic merge constructs a candidate result.

**ORNA-STORAGE-014** A disjointness check MUST establish exact key existence. Manifest and shard bounds MAY reject impossible overlaps quickly, but range overlap alone MUST NOT be treated as an exact duplicate. If a range can contain the key, the implementation MUST use an exact index lookup or scan sufficient to prove presence or absence; it MUST NOT decompress every compact row when indexed lookup can decide the question.

**ORNA-STORAGE-015** If an editable/compact duplicate is found during insert, discovery, checkout, publication, re-key or verification, no representation may silently overwrite the other. A merge reports a typed conflict and leaves CWD unchanged when the conflict budget is exceeded. A recoverable rewrite MAY hold temporary physical shadows only behind one generation barrier and only when their canonical row hashes are identical.


## Compact repository layout

For table ObjectId `T`, committed metadata is rooted at:

```text
.orna/storage/T/
├── policy.orna
├── manifest.orna
├── shards/
│   └── <shard-number>.orna
└── data/
    └── <first-two-id-chars>/<segment-id>.parquet
```

`segment-id` is a UUIDv7 generated before file creation. The manifest stores the actual Git object ID and a SHA-256 content checksum; path names are locators, not integrity identities.

A table may also have editable rows under its ordinary table directory. One key may not be authoritative in both places.


## Segment closure

Writer defaults:

```text
target compressed file       16 MiB
normal writer range            8-32 MiB
hard file maximum             64 MiB
row-group maximum             65,536 rows
row-group uncompressed max    16 MiB
age publication default       60 seconds
```

A file closes when the target is met at a row-group boundary, at the hard maximum, or when age-triggered publication flushes complete pending rows. A single valid row larger than the normal limits may occupy one file up to the hard maximum; larger rows are not supported by this profile and remain editable or receive a typed error.


## Generations and visibility

Each file has one role:

- `data`: newly inserted complete rows;
- `replacement`: complete replacement rows for keys already visible in an ancestor layer;
- `deletion`: complete primary keys only.

A **publication generation** is the precedence unit within one branch lineage.

1. A publication attempt reads the compact table manifest from its expected base `HEAD`.
2. Its candidate generation is exactly that manifest's `next_generation`.
3. Every data, replacement and deletion file created by that publication shares the candidate generation.
4. Before files are encoded, all pending mutations in the frozen publication batch are folded in transaction order to one final mutation per logical key. An insert followed by deletion of a key absent from the base produces no compact mutation; other chains produce one complete final row or one deletion.
5. Within one lineage, the visible mutation for a key with the greatest generation wins.
6. A generation has no separate semantic sequence number. Segment ordering is metadata ordering only and cannot change row precedence.
7. Two authoritative mutations for the same key and generation in one valid committed lineage are corruption.

Generations do not decide between concurrent Git branches. Two branches may independently allocate the same numeric generation. Semantic three-way merge compares their logical key changes against the merge base:

- identical changes to one key coalesce;
- different changes to one key create a row conflict;
- disjoint key changes coexist even when their numeric generations are equal.

A conflict resolution is written as a new mutation in a fresh generation of the merged manifest.

### Publication allocation and retry

The manifest's `next_generation` is the authority for that table in that snapshot. One local publisher serializes publication attempts for a table/branch within a clone.

If branch compare-and-swap fails, the candidate generation was never committed. The publisher reloads current `HEAD` and the current table manifest, then obtains a candidate generation from that base. It may reuse already completed immutable Parquet files when their logical contents remain valid, because generation is carried only by manifest metadata. It must rebuild the manifest and must not force a stale generation onto a changed table manifest. The number may remain unchanged only when the table manifest itself is unchanged by the intervening commit.

After a successful semantic merge, `next_generation` is one greater than the greatest generation retained by the merged table. Generation numbers need not be contiguous; failed attempts and merge normalization may leave gaps.


## Exact physical encoding profile

This profile adopts the Apache `parquet-format` **2.13.0** Thrift and logical-type definitions. `2.13.0` is the specification release, not the file's `FileMetaData.version`. Writers set that field to 1, use Data Page V2 headers, and advertise actual encodings in column metadata. A library option named `version="2.6"` is not a file-format identifier or a substitute for this profile.

Value pages use PLAIN or RLE_DICTIONARY encoding; definition/repetition levels use the standard RLE encoding. Readers must support both value encodings. Other value encodings require a separately negotiated storage profile and are not silently emitted here. Each compressed page is a standard Zstandard frame without an out-of-band dictionary, with the page checksum required by the profile. Readers apply configured decoded-size bounds before allocating buffers.

Every file contains UTF-8 key-value metadata:

| Key | Exact value |
|---|---|
| `orna.profile` | `compact-storage-v1` |
| `orna.table` | Lowercase canonical table UUID. |
| `orna.schema.sha256` | Lowercase 64-hex logical schema fingerprint. |
| `orna.schema.ovb` | Padded standard Base64 of the exact OVB-1 schema descriptor. |
| `orna.columns.ovb` | Padded standard Base64 of the physical column descriptors. |
| `orna.encoder` | Producer/version text, diagnostic only. |

The physical column descriptor array is ordered by physical leaf path. Each entry is `[field_id_path, physical_path, logical_type_node, encoding_kind, parameters]`. `field_id_path` contains field UUIDs and reserved structural components `list`, `element`, `value` or tuple ordinals; `physical_path` is the exact Parquet path. Encoding kinds and parameters are fixed below. Unknown mappings fail before data interpretation.

A top-level field's physical name is `f_` followed by its lowercase 32-hex UUID without hyphens. Nested nominal fields use the same form; structural fields use `n_` followed by lowercase hex UTF-8 bytes of the NFC name. A Parquet numeric `field_id`, when supplied, is only a file-local ordinal; stable Orna field identity comes from the UUID mapping. Adding a column may change ordinals but cannot change its logical identity.

| Orna type | Encoding kind | Physical representation and parameters |
|---|---|---|
| Bool | `bool` | BOOLEAN; parameters `[]`. |
| Float | `float64` | DOUBLE; parameters `[]`; canonical quiet NaN on write, signed zero retained. |
| Str | `utf8` | BYTE_ARRAY with STRING logical annotation; parameters `[]`. |
| Blob | `blob` | Unannotated BYTE_ARRAY; parameters `[]`. |
| Uuid / UUID-backed ID | `uuid` | FIXED_LEN_BYTE_ARRAY(16), UUID annotation; network-order bytes; parameters `[opaque_type_id_or_null]`. |
| Date | `date` | INT32 DATE, signed days from 1970-01-01; parameters `[]`. |
| Instant | `instant_ns` | INT64 TIMESTAMP(NANOS, adjustedToUTC=true), when every value fits signed 64-bit nanoseconds; parameters `[]`. |
| Duration | `duration_ns` | Unannotated INT64 nanoseconds when every value fits; parameters `[]`. |
| Int | `int64` | Signed INT64 when every value fits; parameters `[]`. |
| Int | `ovb` | BYTE_ARRAY containing the exact OVB-1 integer for each value; parameters `[1]`. |
| Decimal | `decimal` | Standard DECIMAL, parameters `[precision, scale, storage_width]` as specified below. |
| Decimal / out-of-range time | `ovb` | BYTE_ARRAY with the complete tagged OVB-1 value; parameters `[1]`. |
| Money / Quantity | `numeric` | Underlying exact/numeric column, parameters `[numeric_mapping, nominal_currency_or_unit_id]`. |
| Option<T> | `option` | OPTIONAL group containing the T representation under `value`; parameters `[]`. Group absent means null; present with value means Some. |
| List<T> | `list` | Standard three-level LIST: group → repeated `list` → `element`; T mapping determines element structure. |
| Record / tuple | `record` / `tuple` | Group with each declared field/component mapping. Empty record/Unit uses required BOOLEAN `_unit = true`. |
| Enum | `enum` | Group with required FIXED_LEN_BYTE_ARRAY(16) `variant`, and at most one optional `v_<variant-id>` payload group. Payload-free chosen variant has no payload group. |
| Refined value | `refined` | Base representation with nominal type ID in parameters; decoding enforces its refinements. |
| Stored reference | `reference` | Group with required database/table UUIDs and complete canonical key-component mappings. |
| Other serialisable composite | `ovb` | BYTE_ARRAY of complete OVB-1 value when this fallback is explicitly chosen in the descriptor. |

The `ovb` fallback is permitted for any serialisable type, not only values outside an optimised numeric range. A reader that supports this profile must always support the fallback. It is never permissible to reinterpret a BYTE_ARRAY as an integer, decimal or enum without its descriptor.

For standard DECIMAL, precision is 1…38 and scale is 0…precision. The unscaled integer is exactly `value × 10^scale`; every value must produce an integer fitting the declared precision. Precision up to 9 uses INT32, up to 18 uses INT64, otherwise FIXED_LEN_BYTE_ARRAY of the smallest length n such that `10^precision−1 < 2^(8n−1)`. Fixed bytes are signed big-endian two's complement with sign extension to that exact width. Values that cannot share the declared precision/scale use OVB fallback; rounding is forbidden. Money's currency minor digits are not permission to round its stored amount.

Option is represented by an optional **group**, not merely by making the underlying scalar optional. This matters for `T??`: an absent outer group and a present group whose inner group is absent represent `null` and `Some(null)` respectively. Lists preserve null elements, empty lists and absent optional lists distinctly. A chosen empty enum payload is represented by its payload group with `_unit = true`; this differs from a payload-free variant.

**ORNA-STORAGE-FORMAT-001** Physical descriptors MUST fully specify every field's mapping. A reader MUST reject incompatible or incomplete descriptors rather than infer types from column names or sample values.

**ORNA-STORAGE-FORMAT-002** Logical row/schema identity MUST use canonical logical values. Parquet file hashes identify physical bytes; two valid encoders may produce different physical files with identical logical row hashes.

### Manifest fields

`manifest.orna` is canonical schema-directed Orna data with required fields `profile: Str`, `table: Uuid`, `schema: Digest`, `next_generation: Int` and `shards: [Shard]`. Profile is exactly `compact-storage-v1`; next_generation is positive. A Shard has `number: Int`, `min_key`, `max_key`, `entries: Int`, `file: Str`, and `hash: Digest`; key values use the table key schema. Numbers are nonnegative, bounds inclusive and entries in 1…256. An empty table has `shards: []`.

A shard file is the canonical record `{ entries: [...] }`. Each entry has exactly: `segment_id: Uuid`, `role: Str` (`data`, `replacement` or `deletion`), `generation: Int`, `schema_id: Digest`, `profile_version: Int` (1), `encoder_version: Str`, `relative_path: Str`, `git_object_id: sys.GitOid`, `sha256: Digest`, `min_key`, `max_key`, `min_event_time: Instant?`, `max_event_time: Instant?`, `row_count: Int`, `compressed_bytes: Int`, `columns: Blob`, `row_group_index: Bool`, and `bloom: Bool`. `columns` is the exact OVB physical-descriptor array. Optional times use `null`; one bound cannot be present without the other. Row count and bytes are positive, generation is positive and below next_generation, and paths are safe table-relative paths. Unknown fields require a different profile version.

A shard's file/hash must name its actual canonical bytes. Every entry's object ID, checksum, size, role, key bounds, schema and physical metadata are verified against the file before it is treated as authoritative. A partial clone may retain promised hashes without hydrating all bytes, but cannot mark an unverified absent blob as verified. Failure to hydrate is an availability error, not an empty table.

### Cross-reader obligation

A storage implementation must decode the golden OVB values and schema descriptors, write/read primitive and nested test files, and compare decoded logical rows with an independent Parquet implementation. Cross-reader tests cover nested Options, page checksums and floating-point statistics in addition to the canonical value vectors.

## Statistics and safe pruning

Primary-key min/max and row count are required at manifest, file and row-group levels. Ordered non-secret columns record min/max and null count when values have a normative total ordering. Bloom filters are optional. Statistics are never trusted as row data; malformed, missing or inconsistent statistics cause the reader to scan more, never return false negatives.

Float statistics use the Parquet floating-point statistics contract and Orna's `FLOAT-TOTAL-1` bit order:

- `FileMetaData.column_orders` declares `IEEE_754_TOTAL_ORDER` for every physical `DOUBLE` used for Orna `Float`;
- every emitted column-chunk `Statistics` object contains exact `nan_count`, including zero;
- every emitted page-level `ColumnIndex` contains `nan_counts` with exactly one exact count per indexed data page;
- min/max use the smallest/largest non-NaN values under `FLOAT-TOTAL-1`;
- when every non-null value in a scope is NaN, min/max use the smallest/largest NaN in that scope under `FLOAT-TOTAL-1`;
- `-0.0` and `+0.0` remain distinct in ordering and bounds;
- a missing count does not mean zero: it means NaN presence is unknown;
- a malformed count, count-list length mismatch, negative count or count inconsistent with available value/null totals invalidates the affected statistics scope;
- invalid or missing Float statistics are ignored for pruning, while the data remains readable by scanning;
- no Float predicate may prune a scope unless the bounds plus NaN information prove that no row in the scope can match under Orna comparison semantics.

Examples of the last rule:

- `value == NaN` matches nothing under ordinary Orna equality and therefore does not need bounds;
- an explicitly named `is_nan(value)` predicate may prune only when a present exact count is zero;
- ordinary ordered comparisons ignore NaN rows because every such comparison with NaN is false, so non-NaN bounds may be used when they are valid;
- total-order sorting and total-order-specific functions use the total-order bounds and the exact NaN counts.

Orna ordinary comparisons still follow `ORNA-FLOAT-001`; the total order is used for sorting and physical/statistical ordering, not to redefine `NaN` comparison or equality.


## Hybrid disjointness

Editable and compact key sets are disjoint in every visible snapshot. The invariant is checked at all of these boundaries:

1. programmatic insert or upsert placement;
2. direct editable row discovery and working-tree reconciliation;
3. opening, checking out or verifying a committed snapshot;
4. explicit row re-key;
5. editable/compact storage rewrite;
6. compact publication;
7. semantic merge before a candidate result becomes CWD;
8. repository integrity verification.

For one candidate editable key, the implementation first uses manifest/shard key bounds to reject impossible compact matches. When a range may contain the key, it performs an exact indexed point lookup or a sufficient physical scan. A range overlap is not proof that the exact key exists. Implementations must not decode every compact row solely to check disjointness when the required exact index is available.

If a duplicate is found, neither representation silently wins or overwrites the other. Insert, discovery, checkout, re-key, publication and verification return a typed conflict/error. Semantic merge records a typed conflict and obeys the configured conflict/resource ceiling; exceeding the ceiling leaves CWD unchanged.

During a recoverable storage rewrite, temporary physical shadows may exist, but one publication-generation barrier hides one side from readers and the two canonical row hashes must be identical. Queries acquire one generation and never observe both.

This is normative for repository format 1.

## Placement

A table's logical row map is the union of disjoint editable and compact key sets. The committed preference is `automatic`, `editable` or `compact`; it changes placement of future programmatic rows, never table type or query semantics.

`automatic` uses editable placement only while the table has no compact data, would remain at or below 10,000 rows, the pending publication is at or below 8 MiB of canonical row bodies and every path is valid. Otherwise the publication is compact. Existing editable rows are not silently converted.

Direct valid filesystem row creation is always an explicit editable insertion. Existing rows preserve placement on normal update. Explicit `sys.admin.rewrite_storage` is the only way to migrate existing rows between placements.

## Disjointness enforcement

One logical key may have one authoritative placement only. Orna checks exact editable/compact disjointness during:

1. programmatic insert or upsert placement;
2. direct row-file discovery and working-tree reconciliation;
3. snapshot open, checkout and repository verification;
4. explicit re-key;
5. storage rewrite;
6. compact publication;
7. semantic merge before the result becomes CWD.

Manifest and shard bounds are a pruning aid, not proof that a key exists. When a candidate key falls inside a compact range, Orna performs an exact point lookup through the compact key index or scans only the necessary physical range. It does not accept a duplicate because the host filesystem happens to hide one representation, and it does not decode the entire table when the exact index can answer the question.

A discovered duplicate is a typed storage conflict. No representation silently wins. A merge records the conflict; if the conflict budget is exceeded, CWD remains unchanged. During a recoverable rewrite, temporary physical shadows are allowed only behind the publication generation barrier and only when their canonical row hashes are identical.

## Re-key

```orna
Contact.rekey("alice-smith", "alice-jones")
```

1. Resolve the complete old and new keys under the activation snapshot.
2. Require old to exist and new not to exist.
3. Require an explicitly keyed table; implicit automatic integer IDs cannot be re-keyed.
4. Record one re-key intent in the activation transaction.
5. Allow the same activation to update every dependent stored reference.
6. At commit, validate all uniqueness, key encoding and referential constraints against the final state.
7. Commit all row/reference changes or none of them.
8. Project an editable row to the new path recoverably, or write compact deletion+insertion mutations as needed.
9. Report one semantic `rekey` change with old/new keys.

Default reference behavior is `restrict`. There is no automatic cascade in v1. A raw file rename without an Orna re-key intent is delete+insert.

