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

## Identity, evolution, and catalogue hash

Record types share the existing qualified type namespace with object,
primitive value, enum, and type-binding definitions. A collision by exact
qualified name or `TypeId` is invalid.

Semantic matching can preserve a record `TypeId` across a type rename and a
field `FieldId` across a field rename. Rename-only evolution is the complete
initial evolution surface. Adding, dropping, reordering, or changing the type
of a field remains deferred. The canonical `ALTER TYPE ADD/DROP FIELD`
proposal uses the object-field grammar and does not apply implicitly to record
values.

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

## Codec and protocol boundary

Runtime record values cannot use `ORV1` or `ORV2`. Both accepted codecs remain
closed and continue to reject them. Before record codec implementation, this
ADR must be amended with the exact version-3 byte contract, including:

* the `ORV3` marker and one closed record tag;
* the outer record `TypeId` and field count;
* declaration-order entries containing `FieldId`, encoded length, and one
  complete version-3 field value;
* the retained total value-size bound; and
* the zero nested-record depth of this slice.

The corresponding frame amendment must define protocol 3.0 and `ORF3` before
a socket can carry record values. Protocols 1.0 and 2.0 remain byte-exact.
Catalogue metadata, runtime construction, codec bytes, and frame negotiation
are separate commits and authorities.

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
* type names collide with every other member of the shared type namespace;
* duplicate field names are rejected before candidate allocation;
* semantic comparison preserves exact type and field identities only for the
  accepted rename cases;
* catalogue hashes change for every semantic field fact and preserve all
  existing version-1 and record-free version-2 golden bytes;
* apply, recovery, and verification preserve exact identities, names, order,
  types, origins, mutability, and persistence;
* runtime construction requires the active nominal definition and exact
  complete field set; and
* versions 1 and 2 reject record runtime values before any version-3 support
  is accepted.

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
   Then add the lossless constructor to that real host in a separate two-file
   syntax commit. A standalone fragment parser is not accepted.
6. Register protected record-definition storage, then add apply and recovery
   in separate migration, source, and focused-test commits.
7. Add checked runtime record values and compiler construction in separate
   commits.
8. Amend this ADR with exact version-3 codec bytes before changing the codec.
9. Amend this ADR with exact protocol-3 frames before changing raw frames.
10. Add canonical by-value storage and one SERVER result proof only after the
    codec and recovery paths are green.
11. Index this ADR and mark the record checklist row complete only after all
    required proof passes.

Each implementation commit changes one to three files, uses a signed
conventional commit, and keeps the repository buildable.

This decision does not broaden ADR 0015's closed Boolean-literal CLIENT body
(`RETURN TRUE` or `RETURN FALSE`). The current relational SQL expression
parser is not a general Orna expression host. Until the required host
amendment, no accepted source position constructs a record value.

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
nullable fields, structural evolution, general value subtyping, automatic
conversion, presenters, `sys.invoke`, or a physical PostgreSQL composite.

## Precedence

The canonical DDL and EBNF mark general value-type syntax as a current
proposal and explicitly leave its exact syntax open. For named record values,
this decision accepts the narrow declaration, constructor, identity,
evolution, and proof rules above. It does not change canonical object fields,
primitive value types, enum types, or opaque transient values.

This decision extends work ADRs 0016, 0025, and 0029. It keeps their stable
standard identities, codec-version closure, enum identity, and all existing
canonical bytes unchanged.
