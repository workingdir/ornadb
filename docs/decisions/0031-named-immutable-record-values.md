# ADR 0031: Named Records Are Immutable Catalogue Values

**Status:** Accepted

## Decision

Orna accepts one initial named record value form:

```sql
CREATE TYPE example.point AS VALUE (
    x INT,
    y INT,
)
IMMUTABLE
PERSISTABLE;

example.point{x: 10, y: 20}
```

This is a nominal, by-value type. The type has a stable `TypeId`. Each field
has a stable `FieldId`, an exact semantic name, a zero-based declaration
ordinal, and one resolved field type. A record value has no `ObjectId`, cannot
be the target of `REF`, and exposes no field mutation operation.

The accepted declaration is narrower than the current proposal in the
canonical EBNF. It requires:

* one or more fields;
* a comma between fields;
* an optional trailing comma;
* exactly `IMMUTABLE PERSISTABLE`, once each and in that order; and
* one terminating semicolon.

The initial declaration accepts no value-field modifier. `DEFAULT`, `CHECK`,
and `DOCUMENTATION` remain deferred. It does not accept `NULL` or `NOT NULL`.
The canonical grammar includes those two modifiers only for object fields,
not value fields. This decision does not create a second nullability syntax.

The initial record fields accept only these executable, persistable value
types:

* `BOOLEAN` or `BOOL`;
* `INTEGER` or `INT`;
* `BIGINT`;
* `FLOAT`;
* `CHARACTER LARGE OBJECT` or `TEXT`;
* `BINARY LARGE OBJECT` or `BYTES`; and
* an active catalogue enum type.

Aliases resolve through the existing catalogue-backed standard type
identities. `VOID`, `DECIMAL`, `UUID`, `DATE`, `TIME`, `TIMESTAMP`, and
`DURATION` remain unavailable as record fields until the executable runtime
value and canonical codec support them. `REF` remains unavailable because its
database-scope wire semantics are still open. `LIST`, `SET`, `MAP`, `OPTION`,
the `?` form, `STREAM`, nested record values, opaque values, and union or
general `VALUE` fields remain deferred.

## Construction and runtime value

A record constructor names the nominal record type and supplies every field
exactly once:

```text
qualified_name "{" field_name ":" expression { "," ... } [ "," ] "}"
```

Constructor source order is not semantic. The compiler resolves each field
name to its stable `FieldId`, rejects unknown and duplicate names, requires
every declared field, and verifies the exact resolved field type. There is no
omission or default evaluation. `NULL` is invalid because the accepted record
field family has no nullable or optional form.

The checked runtime shape is equivalent to:

```text
RecordValue {
    record_type: TypeId,
    fields: [RuntimeValue in declaration order]
}
```

Only construction against one immutable active catalogue can create this
value. Construction stores fields in declaration order after it validates the
complete field set and each value. It does not retain caller ordering or
unchecked names. Equality is nominal: record `TypeId`, active field sequence,
and each field value must match. This decision does not define a general
language hash operation.

### First compiler host

The first source position that constructs a record is one value in the
single-row `VALUES` tuple of the accepted `SERVER INSERT` function body. The
constructor must target a non-null record-valued object field. It is not
accepted in `UPDATE`, `SELECT`, a function argument or default, a CLIENT body,
or a standalone expression parser.

Each constructor field accepts only one of these source expressions:

* a non-null declared parameter of the enclosing SERVER function whose exact
  resolved type equals the record field type; or
* `TRUE` or `FALSE` when the record field is the accepted Boolean type.

The parameter form accepts the six scalar record field types and an active
catalogue enum type. It does not accept a record-valued parameter. `NULL`,
field paths, function calls, operators, enum-label literals, other scalar
literals, nested record constructors, collections, and general expressions
remain unavailable in this host.

The compiler resolves the constructor name and each field name through the
same immutable candidate catalogue used for the enclosing INSERT. It records
the record type and field identities as definition references. It stores the
checked fields in declaration order. Source order cannot change the prepared
value. The prepared mutation plan retains the constructor's record `TypeId`,
each declaration-order `FieldId`, and each checked child expression. At
execution, the server evaluates the child expressions, constructs one checked
`RecordValue` against the transaction's active revision, encodes it as one
complete `ORV3` value, and binds those bytes to the target PostgreSQL `bytea`
column. It does not generate a PostgreSQL composite, JSON value, or SQL record
constructor.

Existing INSERT, UPDATE, and DELETE mutation artifact versions and bytes stay
closed. The first record-constructor INSERT uses
`orna.server-mutation-plan` version 4. Version 4 retains the existing header,
INSERT operation tag, target, assignment sequence, and returned-object bytes.
It is selected if and only if an INSERT contains at least one record
constructor. A record-free INSERT remains version 1. UPDATE remains version 2
and rejects record constructors. DELETE remains the separate version-3 plan.

Version 4 retains expression tags 1 through 3 and adds record-constructor tag
4. The record expression bytes are:

```text
offset  size  field
0       1     expression tag, exactly 4
1       1     resolved-type tag, exactly 2 for a named value type
2       16    record TypeId
18      1     nullable, exactly 0
19      4     field count, unsigned big-endian, 1 through 1024
23      ...   declaration-order field entries
```

Each field entry is:

```text
offset  size  field
0       16    owning record TypeId, equal to the outer record TypeId
16      16    record FieldId
32      ...   one child expression
```

A child expression reuses the existing complete expression encoding. It is
limited to parameter tag 1 or Boolean-literal tag 2 and is always non-null.
A parameter child uses one of the six accepted scalar type encodings or named
type tag 2 followed by an enum `TypeId`. A Boolean child uses the existing
Boolean scalar encoding. Typed NULL, reference types, and record-constructor
children are invalid. Field owner and `FieldId` pairs are unique, and every
parameter read in the complete plan has the same function owner. The active
catalogue later proves that a named child type is the exact declared enum,
that every owner-qualified field exists in declaration order, and that the
complete field set matches the record definition.

Version 4 permits the existing non-record expressions beside a record
constructor without changing their bytes. A version-4 artifact with no record
constructor is non-canonical and rejected. Version 1 and version 2 reject
version 4, expression tag 4, and named resolved-type tag 2. Version 3 remains
accepted only by the DELETE decoder. The existing 16 MiB artifact limit and
1,024-assignment limit apply; the record field count has its own 1,024 limit.
The artifact implementation must pin one exact mixed scalar-and-enum record
golden, round-trip version 4, reject every structural and version mismatch,
and retain every existing version-1, version-2, and version-3 golden byte.

## Identity, evolution, and catalogue hash

Record types share the existing qualified type namespace with object,
primitive value, enum, and type-binding definitions. A collision by exact
qualified name or `TypeId` is invalid.

Exact semantic-name matching preserves a record `TypeId` and each field
`FieldId` when the same complete definition is submitted against its active
catalogue. Record type renames and record field renames remain deferred.
Removing an existing name and adding another does not infer continuity.
Adding, dropping, reordering, or changing the type of a field also remains
deferred. The canonical `ALTER TYPE ADD/DROP FIELD` proposal uses the
object-field grammar and does not apply implicitly to record values.

Catalogue hash version 1 rejects record value definitions. Catalogue hash
version 2 accepts one conditional record-definition section immediately after
the existing optional enum section and before type bindings. The section is
omitted, including its count, when the catalogue has no records. Every
previously accepted record-free version-2 byte sequence and digest therefore
remains exact.

When present, the section uses the existing catalogue-hash `Encoder`
primitives. `sequence_len` is an unsigned 32-bit big-endian count. An identity
is its 16 raw bytes. `text` is a `sequence_len` followed by its UTF-8 bytes,
and `semantic_name` is a `sequence_len` followed by each part as `text`.
Records are sorted by raw `TypeId` bytes. The section encodes:

1. `sequence_len(record_count)`;
2. for each record, `type_id`, `semantic_name`, `u8(2)` for the record
   value-kind, `u8(1)` for immutable, and `u8(1)` for persistable;
3. `sequence_len(field_count)`; and
4. each field in increasing ordinal order as `field_id`, `text(field_name)`,
   `u32(ordinal)`, and the existing canonical `ResolvedType` encoding.

Field ordinals must be the exact contiguous range from zero to
`field_count - 1`. Accepted primitive fields encode as `ResolvedType::Value`
tag `4` followed by their standard `TypeId`. Accepted enum fields encode as
`ResolvedType::Named` tag `2` followed by their enum `TypeId`. Record
validation rejects the scalar tag `1`, reference tag `3`, and every other
resolved type in this initial family.

Changing a field name, order, or type changes the catalogue hash even when
the stable identities remain. PostgreSQL persists these definitions as
protected catalogue metadata. It does not create PostgreSQL composite types
or become record identity authority.

`PERSISTABLE` declares that an accepted durable position can store the whole
record by value. Actual object-field storage waits for the canonical record
codec. It must use canonical Orna value bytes rather than identity-bearing
PostgreSQL rows or composites.

## Codec boundary

Runtime record values cannot use `ORV1` or `ORV2`. Both accepted codecs remain
closed and continue to reject them. Canonical value codec version 3 retains
the ADR 0025 envelope, size limit, and exact tag and payload semantics for all
version-1 values. It also retains both ADR 0029 enum tags and their exact
payload semantics. It changes the marker to `ORV3` and adds this one tag:

```text
0x0b  named record value
```

The version-3 record envelope is:

```text
offset  size  field
0       4     ASCII `ORV3`
4       1     record tag `0x0b`
5       16    record TypeId bytes
21      4     unsigned record-payload length, big-endian
25      n     record payload
```

The record payload is:

```text
size  field
4     unsigned field count, big-endian
for each field in declaration order:
16    FieldId bytes
4     unsigned complete field-value length, big-endian
n     one complete ORV3 field value, including its 25-byte envelope
```

No field name, ordinal, catalogue hash, or revision identity occurs in the
bytes. The declaration order and stable field identities bind the payload to
the active nominal definition. The field count must equal the active
definition's field count. Each entry `FieldId` must equal the active field at
that ordinal. A duplicate, missing, unknown, or reordered field therefore
fails closed.

Each complete field value uses `ORV3`, not `ORV1` or `ORV2`. Its tag, stable
type identity, payload, and canonical numeric rules come from the active field
definition. An accepted standard scalar uses the tag and payload rules for its
pinned representation contract, while its envelope carries the exact
`ResolvedType::Value` identity from the verified standard snapshot. An enum
uses its exact active application or verified standard `TypeId` and label. The
field-value length includes its complete 25-byte envelope and must equal the
exact bytes consumed. A record tag is not valid in a field value. This
enforces zero nested-record depth. A null or reference value also fails because
the initial record field family accepts only the six non-null standard scalar
values and active enum values. Top-level version-3 scalar and application-enum
values retain their exact version-2 type identities and payload shapes.

Version-3 encoding and decoding both require one immutable
`ActiveDatabaseRevision`. Encoding revalidates the record `TypeId`, active
field sequence, and every field value. A record created against a different
active revision cannot cross this boundary unless it remains valid against
the supplied revision. Decoding validates the envelope and each entry before
it constructs the value through the checked active-revision constructor. It
never creates an unchecked or partial record.

The outer payload length remains limited to 16 MiB. The four-byte field count,
every entry header, and every complete field value are part of that limit. A
field-value length must be at least 25 and cannot exceed the bytes that remain
in the outer payload. Its nested payload length plus 25 must equal the declared
field-value length. Before allocation, the decoder checks that the field count
matches the active definition and that the payload can contain at least that
many 45-byte entries. All length addition, subtraction, and conversion uses
checked arithmetic. Truncation, trailing bytes, an oversized payload, an
impossible count or length, and any inner or outer canonical-value error fail
closed.

`ORV3` accepts only the closed tags `0x00` through `0x0b`. `ORV1` accepts only
its existing version-1 tags. `ORV2` accepts only its existing version-2 tags.
Each decoder accepts only its exact marker. All version-1 and version-2 bytes
and rejection rules remain exact.

## Protocol 3.0 frame boundary

Protocol 3.0 retains the exact twelve-byte ADR 0028 hello and ACK shapes. The
client hello is:

```text
offset  size  value
0       4     ASCII `ORNA`
4       1     client HELLO `0x01`
5       1     flags, exactly zero
6       2     protocol major, unsigned big-endian, exactly `3`
8       2     protocol minor, unsigned big-endian, exactly `0`
10      2     reserved, exactly zero
```

The server ACK has the same bytes except offset 4 is `0x81`. A protocol-3 ACK
means authentication succeeded and one immutable `ActiveDatabaseRevision` is
bound to the connection's value codec. The client cannot supply or select that
revision. Authentication and complete active-revision recovery both succeed
before the adapter writes the ACK. Recovery failure closes the connection
without an ACK.

After the ACK, protocol 3.0 uses the exact ADR 0026 frame envelope with marker
`ORF3`:

```text
offset  size  field
0       4     ASCII `ORF3`
4       1     ADR 0026 frame tag
5       1     flags, exactly zero
6       8     stream number, unsigned big-endian
14      4     unsigned payload length, big-endian
18      n     payload
```

Every client and server frame tag, direction, flag, stream rule, field order,
fixed payload, event shape, failure value, channel, size limit, state
transition, flow-control rule, cancellation rule, and connection resource
bound remains exact from ADR 0026. `CALL_ARGUMENT` contains its existing
16-byte `ParameterId` followed by one complete `ORV3` value. A canonical-value
event contains one complete `ORV3` value. No other payload changes.

The active-revision frame encoder and decoder use the same immutable
`ActiveDatabaseRevision` as the ORV3 codec. They revalidate record and enum
values through that revision. The connection-local revision is decoding and
encoding context only. Protected dispatch still recovers and pins its
transactional active revision, and it rejects an argument that is stale
against that revision before execution.

Protocol 1.0 accepts only `ORF1` frames containing `ORV1`. Protocol 2.0 accepts
only `ORF2` frames containing `ORV2`. Protocol 3.0 accepts only `ORF3` frames
containing `ORV3`. Each frame decoder rejects the other two markers, and each
value decoder rejects the other two value markers. The local adapter accepts
only the exact three hello values for major versions 1, 2, and 3 with minor
version zero. Protocols 1.0 and 2.0 remain byte-exact.

Catalogue metadata, runtime construction, codec bytes, frame bytes, and socket
negotiation remain separate commits and authorities.

## Required proof

Tests must prove:

* the accepted declaration shape, comments, exact spans, field order, and
  optional trailing comma;
* exact diagnostics for an empty field list, missing comma or close, trailing
  tokens, missing or reordered modifiers, every deferred field modifier, and
  unsupported field type;
* the accepted constructor and its exact field-name and expression spans;
* constructors reject missing, duplicate, unknown, null, and wrong-type
  fields, while caller field order cannot change the checked value;
* the constructor is accepted only in the single-row SERVER INSERT host, and
  every deferred source position and child expression fails closed;
* record-constructor INSERT artifacts preserve declaration-order record and
  field identities, while every earlier mutation artifact version and byte
  sequence remains exact;
* type names collide with every other member of the shared type namespace;
* duplicate field names are rejected before candidate allocation;
* exact-name replay preserves type and field identities, while type or field
  spelling changes are rejected without inferring continuity;
* catalogue hashes change for every semantic field fact and preserve all
  existing version-1 and record-free version-2 golden bytes;
* apply, recovery, and verification preserve exact identities, names, order,
  types, origins, mutability, and persistence;
* runtime construction requires the active nominal definition and exact
  complete field set;
* versions 1 and 2 reject record runtime values and `ORV3` bytes;
* version 3 preserves exact version-2 scalar and enum shapes under the `ORV3`
  marker;
* exact version-3 golden bytes and a round trip preserve a record `TypeId`,
  declaration-order `FieldId` values, and complete scalar and enum fields;
* version-3 encoding rejects a record that is stale or incompatible with the
  supplied active revision;
* version-3 decoding rejects a wrong record type, field count, field identity,
  field order, field type, enum label, marker, tag, nested record, null field,
  reference field, length, truncation, trailing bytes, and oversized payload;
  and
* decoding arbitrary version-3 bytes never panics or returns a partial value;
* the exact version-1, version-2, and version-3 hello and ACK values select only
  their matching frame codecs, while every other major version and every
  non-zero flag, minor, or reserved value fails closed;
* exact `ORF3` golden client argument and server value-event frames carry a
  mixed scalar-and-enum record without changing any non-value frame bytes;
* every protocol-3 client and server frame round-trips through the
  active-revision interface;
* `ORF1`, `ORF2`, and `ORF3` reject each other's markers and embedded value
  markers;
* protocol-3 frame encoding and decoding reject a value that is stale against
  the connection's active revision; and
* the authenticated adapter recovers the complete active revision before its
  protocol-3 ACK and still revalidates arguments against the transactional
  revision at protected dispatch.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, and focused
live PostgreSQL gates remain required.

## Implementation sequence

1. Accept this named immutable record boundary.
2. Add the lossless record declaration to `orna-syntax` in two files.
3. Add semantic record definitions, shared namespace checks, and conditional
   version-2 catalogue hashing in at most three `orna-core` files.
4. Resolve record definitions, then prepare their stable identities, in
   separate compiler commits of at most three files.
5. Before accepting record construction in source, amend this ADR to name the
   first compiler-supported expression host and its closed expression subset.
   The accepted host is the single-row SERVER INSERT value expression defined
   above. Add its lossless constructor in a separate two-file syntax commit. A
   standalone fragment parser is not accepted.
6. Register protected record-definition storage, then add apply and recovery
   in separate migration, source, and focused-test commits.
7. Add checked runtime record values and compiler construction in separate
   commits.
8. Add the exact version-3 codec defined by this ADR without changing the
   version-1 or version-2 interfaces or bytes.
9. Add the exact protocol-3 frame codec defined by this ADR without changing
   version-1 or version-2 frame interfaces, bytes, or state transitions. Add
   socket negotiation in a later adapter commit.
10. Add canonical by-value storage and one SERVER result proof only after the
    codec and recovery paths are green.
11. Add the record-constructor mutation artifact, compiler preparation, and
    active-revision execution in separate commits. The live proof must insert
    canonical record bytes through the accepted SERVER INSERT source and read
    the same nominal value through the accepted SERVER result path.
12. Index this ADR and mark the record checklist row complete only after all
    required proof passes.

Each implementation commit changes one to three files, uses a signed
conventional commit, and keeps the repository buildable.

This decision does not broaden ADR 0015's closed Boolean-literal CLIENT body
(`RETURN TRUE` or `RETURN FALSE`). The current relational SQL expression
parser is not a general Orna expression host. Only the SERVER INSERT position
and child expressions specified above construct a record value.

## `sys.invoke` boundary

This record is a prerequisite for, not an implementation of,
`sys.invoke.Request`. The canonical request also needs maps, optional values,
nested records, stable reference or identity choices, and event types. This
ADR defers every one of those shapes.

The version-2 JSON request schema describes a diagnostic JSON representation.
It is not Orna record-value, codec, or wire authority. It also uses names that
differ from the current invocation prose, including `trace` versus
`trace_policy` and `parent_invocation_id` versus `parent_invocation`. A later
decision must reconcile those sources before defining the exact logical
request type.

## Deferred surface

This decision does not define opaque values, collections, optionals, unions,
nested records, references, defaults, checks, documentation modifiers,
nullable fields, type or record-field renames, structural evolution, general
value subtyping, automatic conversion, presenters, `sys.invoke`, or a physical
PostgreSQL composite.

## Precedence

The canonical DDL and EBNF mark general value-type syntax as a current
proposal and explicitly leave its exact syntax open. For named record values,
this decision accepts the narrow declaration, constructor, identity,
evolution, and proof rules above. It does not change canonical object fields,
primitive value types, enum types, or opaque transient values.

This decision extends work ADRs 0016, 0025, and 0029. It keeps their stable
standard identities, codec-version closure, enum identity, and all existing
canonical bytes unchanged.

ADR 0006 remains limited to object fields. Its `ALTER TYPE ... RENAME FIELD`
transition does not apply to record value fields. A later decision must define
record rename syntax and identity evidence before either record type or record
field renames are accepted.
