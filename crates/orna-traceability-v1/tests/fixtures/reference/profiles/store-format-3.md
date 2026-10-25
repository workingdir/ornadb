# Format 3: OGS-1 / ORP-1 / OGB-2 / ROV-3 / SOV-3

This is the one new-format write contract. Unknown node kinds/versions/tags
fail before use. Models illustrate the limited byte/partition cases they implement; the prose
below defines full typed integration and limits. A mismatch is a failing
reference test requiring correction, not permission to change engine semantics.

## 1. Header and native edges

`database.orna` uses the ordinary row parser restricted to one literal record
with exactly `repository_format` and `database_id`; no imports/calls/defaults.
It is UTF-8, at most 64 KiB, canonical UUIDv4, no duplicate/unknown keys. The
canonical writer emits repository_format first then database_id and LF text.
`.orna/store` is the native Git tree root; it is never a textual OID symlink.

Each nonterminal tree contains `100644 data` and, iff dependencies exist,
`040000 refs`. No other children. `data` is canonical CBOR under the common
structural rules: definite lengths, shortest integer heads, UTF-8, no duplicate
map keys, maps ordered by lexicographic encoded-key bytes, registered tags
only. Floats use the existing OVB deterministic binary64 rule.

`refs` entries are lowercase native OID names: mode 040000 for a tree,
100644 for a raw blob. Native OID width follows repository Git object format.
Exact expected child roles are derived from the node's typed payload, not
by treating arbitrary byte strings as OIDs. All expected children occur once
per distinct OID; extra/unreferenced children fail. OID strings alone do not
retain content. Native object hash validation precedes node interpretation.
No child may masquerade as a gitlink, symlink or executable mode.

## 2. Node tuples

Every `data` starts `[1, kind, ...]`. All lengths/counts are checked unsigned
integers no larger than 2^63−1 unless a tighter bound is stated. Native OIDs
are byte strings of 20 or 32 bytes, not textual SHA names.

| Kind | Tuple after `[1, kind]` | Native children |
|---|---|---|
| 0 Store root | `[relation_map_oid]` | map root |
| 1 Ordered leaf | `[domain, entries]` | dependencies in entry values/keys |
| 2 Ordered branch | `[domain, height, entries]` | child maps plus any out-of-line fence keys |
| 3 Value overflow | `[encoded_length, semantic_digest, byte_root, dependency_root_or_null]` | byte root and dependency root |
| 4 Byte index | `[height, total_length, entries]` | data chunks at height 0; byte indices above |
| 5 Blob descriptor | `[length, raw_sha256, byte_root_or_null]` | byte root if nonempty |
| 6 Schema | `[encoded_length, schema_digest, byte_root]` | encoded complete schema bytes |
| 7 Dependency index | `[height, child_oids]` | value/Blob descriptor trees at 0; dependency indices above |

`domain` is a canonical tuple naming the map purpose and stable schema/scope
identity. Distinct purposes cannot reuse a node merely because their bytes
happen to decode similarly. Leaf entries are `[key, value]` in **logical**
key order. Branch entries are `[inclusive_max_key, child_oid, row_count]`;
child ranges are nonoverlapping/exhaustive in that order; counts are exact
local counts, not guesses. All descendants share the domain and height rule.
No tombstones in published roots; deletions remove the key. Empty table: one
empty leaf. Root with one child collapses; intermediate final-tail unary
nodes are allowed, not silently removed at a different height.

The store-root relation map has domain `["relations", database_uuid]`. Its
keys are stable relation UUIDs. Values are
`[schema_oid, primary_map_oid, secondary_map_or_null, row_count]`.
Each table primary domain is `["rows", table_uuid, schema_digest]`; row key
uses its complete schema-typed key and value is stored non-key/non-computed
fields in stable field-ID order. System historical relations use identical
maps but their schema/admission authority remains sealed.

Each secondary map is described by source expression/schema, ordering and
complete primary-key tiebreaker, is updated in the same transaction, and uses
ORP-1 too. No persisted index is trusted against a different row root.

Byte-index entry: `[span_length, target_oid, chunk_sha256_or_null]`.
For height 0 the target is a raw Git blob and SHA-256 is exactly 32 bytes;
otherwise the target is another byte-index tree and the hash slot is null.
Span lengths sum to total_length. No zero span in a nonempty index. Offsets
are calculated as node-local prefix sums, never global positions encoded
through the whole graph. Blob descriptor's raw_sha256 is exactly 32 bytes;
empty descriptor has length 0, SHA256(empty) and null byte root.

## 3. Exact page boundary algorithm

ORP-1 partitions ordered entries incrementally, without sorting by hashes.
Each boundary token is the canonical **logical key** representation (SOV-3
for that key, no storage OIDs). At height `h`, calculate
`SHA256(ASCII("orna.orp.boundary.v1") || 00 || uint32be(h) || token)`.
An anchor is a digest whose low six bits are zero.

Accumulate entries. Before an entry would exceed 64 KiB encoded node data,
256 entries or 256 distinct native refs, emit the current nonempty group.
After adding an entry, emit on an anchor if there are at least 16 entries,
or at exactly 256 entries. Emit final nonempty tail even if below 16.
Run the same algorithm for successive branch levels using each child's last
logical key as token until one root remains. Reject height above 64.
Domain/header/ref metadata count toward the byte bound. An entry too large
for a node must first use the overflow encoding below, never oversize a page.
The final physical root is deterministic for the same profile/domain/state.

The anchor avoids dependence on preceding entry positions; a prefix insertion
can share later pages. Forced size/cap cuts and adversarial keys can still
cause more changes. There is no unconditional logarithmic rewrite guarantee.
Writers may rebuild incrementally but must match the canonical complete build.
Indexes/row pages are bounded fetch units; cold query benchmarks count native
Git tree transfer too. This is a pinned prolly-style algorithm, not a claim
that it reproduces Dolt's on-disk format.

## 4. Binary chunk and byte-index partition

For every byte x, `gear[x]` is the big-endian first eight bytes of
`SHA256(b"orna.gear.v1\0" + bytes([x]))`. Start accumulator 0. For each byte:
`a = ((a << 1) + gear[x]) mod 2^64`. Cut after at least 65,536 bytes when
`a & 262143 == 0`, or at 1,048,576 bytes. Reset length and accumulator after
cut. Emit the final nonempty tail. Empty input emits no payload chunk.

Byte-index grouping uses the ORP-1 limits and anchor construction with domain
`orna.ogb.index.v2` followed by NUL, level uint32be and the child's stable
content/layout token. At height 0 token is CBOR `[span_length, chunk_sha256]`;
at higher levels token is the child index OID. Node-relative span lengths keep
an unchanged subtree independent of its absolute position in a recording.
Node kinds/header sizes count toward the 64 KiB bound. Maximum refs 256 and
height 64. Full-object SHA identity is calculated while streaming, not from
the chunk tree. Git OIDs verify native representation; SHA values verify raw
content commitments. Changing MIME never changes these chunk/index bytes.

## 5. Value codecs and overflow

OVB-1 byte meanings remain frozen for legacy contexts. **OVB-2** inherits its
non-Blob encodings and introduces tag **60110**:
`[raw_bytes, canonical_media_type, suffix_hint_or_null]`.
Bare CBOR byte strings do not mean annotated Blob in OVB-2; identifier bytes
remain valid within their explicitly typed payloads. A full portable encoding
really includes all bytes and must stream or reject the sink's limit. Encoding
an annotated Blob through an OVB-1 interface is an explicit error, never an
implicit metadata loss. Raw download is not structured value serialization.

**ROV-3** uses the same typed values except each Blob is tag **60111**:
`[length, raw_sha256, media_type, suffix_or_null, descriptor_oid]`.
The descriptor is a native child reference. Small and large Blobs use the same
reference representation. No Base64 media lives in a repository row.

**SOV-3** substitutes tag **60112**:
`[length, raw_sha256, media_type, suffix_or_null]`.
It excludes physical references but binds annotations. It is a semantic
commitment encoding, not a payload export or independent availability proof.
Domain digests use ASCII domain, NUL, canonical SOV-3 payload:
`orna.row.v3`, `orna.schema.v3`, `orna.arguments.v3`, `orna.consumer.v3`.
Row payload is `[database_uuid, table_uuid, typed_key, stored_fields]`;
schema uses the full closed storage descriptor with format coordinate 3;
arguments include exact type descriptors and once-bound defaults; consumer
uses existing natural identity inputs with code revision still excluded.
Legacy wire `orna.request.v1` and legacy consumer keys keep old meanings.
Migration records identity correspondence before any consumer resumes.

Any logical value/row entry exceeding 8 KiB or 32 direct Blob references is
encoded through tag **60113** `[kind3_tree_oid]` in ROV-3. Its full ROV-3
encoding is stored through the byte-index mechanism; kind3 stores its length
and SOV-3 semantic digest. Dependencies appearing inside those bytes must also
be exposed through a bounded kind7 dependency tree; no hidden references.
The canonical kind7 inventory is sorted by native OID bytes, deduplicated,
and partitioned by the same bounded key-anchor rule using OID as token.
Nested large subvalues are lowered bottom-up. Expanding SOV-3 conceptually
eliminates overflow references; incremental hashing can stream structure
without loading Blob payloads. Avoid cycles among stored value nodes.

Canonical key encoding is bounded to 1 MiB and row metadata/value encoding to
1 GiB by this profile; payload bytes do not count toward those limits. Larger
admission fails explicitly before mutation. Keys are not silently truncated.
Large text is a Str with overflow, not a new Clob primitive. Out-of-line keys
compare by their logical values after bounded decoding, never by the hash.

## 6. Schema and dependent interpretation

Schema nodes store the existing closed descriptor (table ID, key field IDs,
field definitions and nominal definitions) under the new semantic profile.
No physical object ID enters semantic type/row identity. Source/code revision
pins remain part of execution environment. Native graph verification does
not execute arbitrary schema/source expressions; it checks recorded shape and
integrity. Full engine admission executes applicable assertion semantics in
its explicitly selected context; independent extraction need not do that.

## 7. Retention and unsupported states

No content service outside Git is required. Git owns native object fanout and
packing. Graph roots are journalled/protected before any successful dependent
acknowledgement. Format limits are protocol safety bounds; operating quotas
can be stricter only when reported as quotas, never as a success with omissions.
An untrusted descriptor can lie about total content hash until all bytes are
verified; keep partial and full verification distinct. Equal SHA alone never
overrules known unequal bytes. Snapshot cloning is not authorization filtering.
