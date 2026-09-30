# 29. Canonical values and schema descriptors {#formats}

This section defines the canonical binary representation used by compact-value fallbacks, semantic digests, request fingerprints and portable wire values. It does not replace the human-editable Orna text codec. Presentation strings, locale formatting and physical Parquet compression are not inputs to canonical logical identity.

## Binary value profile OVB-1

OVB-1 is an application profile of [CBOR](#ref-cbor). Every length and integer argument uses the shortest permitted head. Arrays, byte strings, text strings and maps have definite length. Maps have no duplicate keys and are ordered by unsigned lexicographic comparison of each key's complete deterministic encoding. Text must be well-formed UTF-8; ordinary string data is not normalised. Indefinite items, CBOR undefined, unsupported simple values and unregistered tags are errors.

Orna Float always uses a CBOR binary64 item, even when a shorter float could express the same number. This is an explicit application-level deterministic rule, not a claim to use CBOR's shortest-float representation. Negative zero retains its sign. All encoded NaNs use bits `0x7ff8000000000000`; infinities remain signed infinities.

Integers in the CBOR major-type 0/1 ranges use those forms. Larger positive integers use tag 2 with minimal unsigned big-endian magnitude bytes; larger negative integers use tag 3 with the minimal magnitude of `−1−n`. A bignum with a leading zero or a value representable directly in major type 0/1 is noncanonical. Zero is the single byte `00`, not an empty bignum. This rule completely defines arbitrary-size integer bytes without host endianness or signed-big-integer conventions.

**ORNA-FORMAT-001** Canonical encoders MUST produce the unique OVB-1 representation for supported values. Strict decoders MUST reject noncanonical aliases before using bytes as an identity or fingerprint.

### Type-preserving values

An untagged CBOR Bool, Int, Str or Blob represents that corresponding Orna primitive. Arrays represent lists. Bare CBOR null is reserved for absent metadata fields, not the complete representation of a nested Orna Option. All other portable values use the following registry. Integer codes are exact and not extensible without a negotiated profile version.

| Tag | Value | Payload and constraints |
|---:|---|---|
| 37 | UUID / ObjectId | Exactly 16 network-order bytes. The enclosing type descriptor distinguishes opaque identifier types. |
| 60000 | Decimal | `[coefficient, exponent10]`. Both are Int. Zero is `[0,0]`; nonzero coefficient has no trailing factor of 10. |
| 60001 | Date | Canonical `YYYY-MM-DD` text, validated Gregorian date. |
| 60002 | Instant | `[unix_seconds, nanosecond]`; floor seconds, nanosecond 0…999999999. No leap-second alias. |
| 60003 | LocalDateTime | Canonical local date/time text with exactly nine fractional digits and no offset. |
| 60004 | TimeZone | IANA identifier text; resolution uses the enclosing snapshot's recorded time-zone dataset. |
| 60005 | Duration | `[seconds, nanosecond]` with floor-normalised seconds and nonnegative nanosecond remainder. |
| 60006 | Quantity | `[number, unit_object_id]`; the type descriptor identifies dimension, scale and offset. |
| 60007 | Money | `[Decimal, currency_object_id]`; the Decimal is tag 60000, never an intermediate Float. |
| 60008 | Enum | `[type_object_id, variant_object_id, payload_record_or_null]`; variant and payload schema must agree. |
| 60009 | Record / nominal record / logical row | `[type_object_id_or_null, fields]`, where fields is an array of `[field_id_or_name, value]` in deterministic field-key order. A full row includes key fields. |
| 60010 | Pinned row reference | `[database_id, table_id, key, snapshot]`; snapshot uses the exact snapshot representation below. |
| 60011 | Diagnostic | The diagnostic map defined by the live protocol; safe fields only. |
| 60012 | Present node | `[kind, stable_key_or_null, properties, children]` as specified by the live protocol. |
| 60013 | Option | `[0]` for `null`, `[1, value]` for `Some(value)`. Nested Options remain distinct. |
| 60014 | Unit | Empty array `[]`; no other payload. |
| 60015 | Tuple | Ordered array of component values. Zero arity canonicalises to Unit/tag 60014. |
| 60016 | Error | Map `{0: code, 1: message, 2: causes, 3: safe_details}`; causes are ordered Error values, details are a text-keyed map. Cycles fail. |
| 60017 | TimeOfDay | Integer nanoseconds since local midnight in 0…86399999999999. |
| 60018 | ZonedDateTime | `[Instant, TimeZone, offset_seconds]`; offset must agree with the pinned zone dataset at that instant. |
| 60019 | Range | `[lower: Option, upper: Option, upper_inclusive: Bool]`; endpoint types must agree. Lower endpoint, when present, is inclusive. |
| 60020 | Refined nominal value | `[type_object_id, base_value]`; decoding validates the referenced refinement before returning the nominal value. |
| 60021 | Stored table reference | `[database_id, table_id, key]`; resolution uses the containing row's snapshot. Export as an independently pinned value uses tag 60010 instead. |
| 60022 | Live-handle description | `[original_type_id, runtime_id, natural_key, false]`; never reconstructs an operational handle. Typed decoding as an operational handle fails. |

Structural field keys are NFC field names. Nominal and row field keys are stable field ObjectIds. Encoders sort by the encoded field key, not by presentation order. Equal field keys are invalid. A record schema must be known from its type ID or from the explicit `as: T` decoding witness. Private field encoding is governed by the type's codec implementation and cannot be requested by circumventing visibility through a structural record.

A `sys.Value` carries both its exact originating type descriptor and the OVB-1 value. A tag or UUID alone does not permit a value of one opaque identifier type to masquerade as another. Secret plaintext, source closures, open streams, database transaction handles and other operational resources are not ordinary portable values. A live-handle description is data about a resource, not permission to revive it.

### Snapshot encoding

A snapshot is exactly one of:

```text
[0, database_uuid, runtime_uuid, generation_uint, snapshot_id_bytes]
[1, database_uuid, git_hash_algorithm, commit_oid_bytes]
```

For a CWD pin, `snapshot_id_bytes` is the 32-byte SHA-256 digest of ASCII `orna.snapshot.v1`, a zero byte and OVB-1 structural encoding of `[0, database_uuid, runtime_uuid, generation_uint]`. The digest does not include itself. A runtime generation must never identify two different logical states. `runtime_uuid` fences owner restarts. The mapping to the exact logical generation is retained for the pin's documented lifetime. For committed snapshots, `git_hash_algorithm` is `"sha1"` with a 20-byte object ID or `"sha256"` with a 32-byte object ID. An attached database uses its own database UUID in the same committed form, not a different ambiguous shorthand.

**ORNA-FORMAT-002** A CWD reference MUST identify its captured generation. A context-free `[0]` is invalid. A decoder unable to resolve a pin MUST report `sys.snapshot.expired` or `sys.snapshot.incomplete`; it MUST NOT rebind the reference to current CWD.

**ORNA-FORMAT-003** A referenced database, type, table and key schema MUST agree. Decoding data from another snapshot does not implicitly import its schema or replace the current execution context.

## Domain-separated digests

`SHA256(domain || 00 || payload)` means the ASCII domain bytes, one zero byte and the exact OVB-1 payload. Domains have no terminating zero of their own. The following strings are fixed:

| Identity | Domain | Payload |
|---|---|---|
| Stored logical row | `orna.row.v1` | `[database_id, table_id, canonical_key, canonical_stored_fields]`; computed fields excluded. |
| Storage schema | `orna.schema.v1` | Exact schema descriptor below. |
| Durable consumer | `orna.consumer.v1` | `[database_id, function_object_id, canonical_typed_bound_arguments]`; code revision excluded. |
| Argument identity | `orna.arguments.v1` | Arguments sorted by NFC name; each includes its exact type descriptor and value. |
| Request fingerprint | `orna.request.v1` | Operation kind plus the canonical body fields specified by the protocol; no request ID or credential. |
| Checkout plan | `orna.checkout.v1` | Exact typed plan excluding its token. |
| Preserved payload | `orna.payload.v1` | `[media_type, bytes]`; secret payload hashes remain protected metadata. |

Default arguments are bound once before a consumer or invocation identity is computed. A changing default such as a clock-derived configuration gives a different canonical argument identity; it is not silently omitted. Durable consumers should use explicit stable configuration. Function code revisions are excluded from consumer identity but included in reflective invocation idempotency identity.

Digests do not replace equality where a collision could change semantics. Natural-key lookup verifies exact typed key equality. A mismatch between declared content digest and bytes is corruption, not a choice of another decoding.

## Closed storage schema descriptor

The descriptor is an OVB-1 map with exactly these integer keys:

| Key | Type | Meaning |
|---:|---|---|
| 0 | UInt, value 1 | Descriptor version. |
| 1 | UUID | Table ObjectId. |
| 2 | Array of UUID | Complete primary-key field IDs, in key order. |
| 3 | Array of field descriptors | Fields ordered by unsigned field-ID bytes. |
| 4 | Array of nominal definitions | Definitions ordered by unsigned type-ID bytes. |

A field descriptor is `[field_id, name, type_node, role, fallback]`. `role` is 0 for key, 1 for stored field or 2 for computed field. `fallback` is `[0]` for no introduction fallback, `[1, value]` for the frozen field-introduction fallback or `[2, expression_digest]` for a computed selector. An insert-time default is code in the schema revision, not a mutable read-time fallback; it is not substituted for `[1,value]`. Optional physically missing fields obtain `null` under the schema-evolution rules even when fallback is `[0]`.

A nominal definition is `[type_id, kind, body]`. Kind 0 is a record with its field-descriptor array; 1 is an enum with variants `[variant_id, name, payload_fields]`; 2 is a refined value with `[base_type_node, assertion_semantic_digests]`; 3 is a unit with `[dimension_vector, scale_numerator, scale_denominator, offset_numerator, offset_denominator, affine]`; 4 is a currency with `[code, minor_digits]`. Vectors and arrays have the orders stated here; duplicate IDs are invalid.

Type nodes are closed positional arrays:

```text
[0, primitive_name]              primitive from the list below
[1, element_type]                List
[2, element_type]                Option
[3, [component_types...]]        Tuple (zero components is Unit)
[4, [[name, type]...]]            Structural record, names sorted
[5, nominal_type_id]             Definition in key 4
[6, database_id, table_id,
    [key_component_types...]]    Stored table reference
[7, numeric_type, unit_id]       Quantity
[8, currency_type_id]            Money
[9, endpoint_type]               Range
```

The primitive names are `Bool`, `Int`, `Float`, `Decimal`, `Str`, `Blob`, `Uuid`, `Date`, `Instant`, `LocalDateTime`, `TimeOfDay`, `Duration`, `TimeZone`, `ZonedDateTime` and `Unit`. A type outside this serialisable subset requires a versioned explicit codec representation or is rejected for stored fields. Transparent aliases lower to their target. Nominal references resolve through the descriptor graph; stored references can be cyclic across tables without embedding recursive row contents. A recursively embedded value type with no finite constructible representation is invalid.

Unknown keys, type codes, roles or definition kinds are rejected. A canonical descriptor is complete: every referenced nominal definition is present, every key field appears once, and a computed field is never a primary-key field. Its schema fingerprint is the domain-separated digest above. File-specific physical encodings are not inputs to this logical schema fingerprint.

## Canonical Orna text

The canonical text codec is schema-directed: `decode(input, as: T)` supplies the exact type and snapshot. It accepts only data constructors in the supplied type, never arbitrary function execution, imports or declarations. Row files are the same data subset with key and computed fields omitted. Loading a row does not execute user code other than deterministic declared construction/refinement validation.

Canonical output is UTF-8 without a byte-order mark, uses LF, two-space indentation for nonempty records, one space after `:`, comma-separated array/tuple elements, a trailing comma after each record field, and one final LF. Record field order is primary-key order followed by stored fields in stable field-ID order for table rows; structural records use NFC name order. Arrays and tuples retain value order. No comments or interpolation occur in canonical output.

Int uses minimal decimal digits with no `+`, leading zeros or separators. Decimal uses its normalised coefficient followed by `e`, a minimal signed decimal exponent (no `+`) and `.decimal`; zero is `0e0.decimal`. Finite Float uses exactly 17 significant decimal digits in scientific form with lowercase `e`, a minimal exponent and suffix `f`; signed zero is retained. Nonfinite Float uses `Float.nan`, `Float.infinity` or `-Float.infinity`. Strings use JSON-style quote/backslash/newline/carriage-return/tab escapes where shared by Orna; other control scalars and a literal opening brace use minimal lowercase `\u{...}`. Ordinary non-control Unicode scalars remain UTF-8.

Bool uses `true`/`false`, Option uses `null` or `Some(value)`, Unit uses `()`, and tuples use the comma distinction from the grammar. Uuid and opaque UUID-backed IDs use quoted lowercase canonical UUID text under their explicit type witness. Blob uses quoted padded standard Base64. Date/Instant use the validated literal forms; other time values use their OVB component structure rendered as data tuples under the type witness. Enums use the variant name resolved in the supplied type and an explicit record payload when needed. Nominal records use their declared field record; refined values use the base representation and are revalidated. Money and quantities use the exact numeric representation under their supplied type witness; no display symbol or locale is encoded.

Stored references use the target's schema-directed key representation under their declared database/table type. Independently pinned references include the snapshot descriptor and therefore cannot be confused with relative stored references. Generic standalone row encoding includes primary keys; a row-body encoding must be requested through the repository row writer. Unsupported values fail with a decode/encode diagnostic rather than losing type information or private data.

**ORNA-FORMAT-004** Canonical text decoding MUST be type-directed and non-executable. It MUST distinguish optional absence, scalar values, row references and nominal construction according to the supplied schema, and validate all refinements before returning a value.


## Portable opaque and contextual values

The following encodings complete the system-value boundary. UUIDs occurring in the structural slots of a snapshot, schema or row-reference encoding use the representation explicitly assigned to that slot. Encoding an opaque ID as an independent typed value additionally preserves its nominal kind.

| Tag | Value | Exact representation |
|---|---|---|
| 60023 | Portable opaque system identifier | `[qualified_type_name, representation]`, using the closed table below. |
| 60024 | Path | `[flavour, text]`; flavour is `repo`, `posix` or `windows`. Repository paths are safe relative slash-separated paths; other flavours are descriptive host paths subject to redaction, not executable access grants. |
| 60025 | Digest | `["sha256", bytes32]`. Display is lowercase 64-hex; no implicit string conversion. |
| 60026 | Explicit `sys.Value` box | `[closed_type_descriptor, value]`. The descriptor identifies the exact pinned originating type; the value must validate against it. |
| 60027 | Materialised finite Relation | `[ordered: Bool, rows]`. Rows are an array of complete typed values. An unordered relation is sorted by canonical encoded value for canonical encoding, preserving duplicate multiplicity; this does not add an observable source query order. |

The UUID-backed system identifier names are `sys.DatabaseId`, `sys.FileId`, `sys.DefinitionId`, `sys.ObjectId`, `sys.RuntimeId`, `sys.TransactionId`, `sys.InvocationId`, `sys.RunId`, `sys.QueryId`, `sys.SessionId`, `sys.ClientId`, `sys.TraceId`, `sys.SpanId`, `sys.SegmentId` and `sys.BuildId`. Their representation is a tag-37 UUID. `sys.RevisionId`, `sys.SnapshotId` and `sys.ConsumerIdentity` use 32 digest bytes. `sys.CheckpointVersion` and `sys.FailureVersion` use nonnegative arbitrary-precision integers that increase on transitions. `sys.GitOid` uses `["sha1", bytes20]` or `["sha256", bytes32]` and cannot be confused with a semantic revision digest.

`sys.RevisionId` is the semantic revision's SHA-256 identity. A `sys.SnapshotId` is the domain-separated digest using `orna.snapshot.v1`, zero byte and the OVB structural bytes of the committed descriptor, or the first four fields of a CWD descriptor. The CWD descriptor’s fifth field carries that result and is excluded from its own input. The snapshot descriptor—not that digest alone—retains repository/hash-algorithm and local-generation context. Consumer identity uses the consumer hash tuple specified earlier. Version integers never wrap or reset when progress is reset; a restored snapshot still receives a fresh local version when installed as current state.

Path and Digest are intrinsic nominal types used by the system and repository APIs. A path codec does not normalise an arbitrary host path into another filesystem's semantics. A repository-path operation validates the `repo` flavour before use. A typed `sys.Value` cannot make a protected path or secret revealable. Function closures, unbounded streams and executable process/session handles are not serialisable operational values.

A finite relation may be encoded only after its complete bounded observation succeeds. Exceeding the configured resource bound fails; the codec must not emit a truncated successful relation. A stored table field may not use a live Relation in place of a finite stored list; materialisation and the field's declared type are explicit.
