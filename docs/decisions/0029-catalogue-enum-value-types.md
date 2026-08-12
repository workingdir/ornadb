# ADR 0029: Enum Types Are Ordered Catalogue Values

**Status:** Accepted

## Decision

Orna source accepts enum value types with this exact declaration shape:

```sql
CREATE TYPE crm.stage AS ENUM (
    'lead',
    'qualified',
    'customer'
);
```

An enum is an immutable, persistable value type. It is not an object type, a
primitive kernel-contract type, or an opaque runtime value. Its stable type
identity is a `TypeId`. One runtime enum value contains that `TypeId` and one
declared label. The label does not receive a separate durable identity.

The declaration contains at least one label. Each label is one ordinary Orna
string literal. Two consecutive apostrophes decode to one apostrophe. Label
comparison is byte-exact after this decoding. Duplicate decoded labels are
invalid, including duplicates that use different source escaping.

Declaration order is semantic. It defines the enum's total order and is part
of the catalogue hash. Reordering labels changes the catalogue even when the
label set is unchanged. A later source revision may add, remove, rename, or
reorder labels through the normal candidate-revision process. The type keeps
its `TypeId` when the existing semantic type identity is retained, but the new
label sequence belongs only to the new catalogue revision.

The initial enum slice does not add implicit conversion to or from text. A
typed enum value can cross the canonical value codec only with its exact
`TypeId` and label. A stale, unknown, or mismatched label fails closed.

## Canonical codec boundary

Enum values use canonical value codec version 2. Version 1 remains closed and
continues to reject enum values and the `ORV2` marker. Version 2 retains the
ADR 0025 envelope, size limit, and exact tag and payload semantics for every
version-1 value, but uses the marker `ORV2`. It adds these tags:

```text
0x09  null enum value
0x0a  enum label
```

Both tags carry the enum `TypeId` in the envelope. A null enum has an empty
payload. A non-null enum payload is the exact UTF-8 label bytes. No ordinal,
name, display value, or revision identifier occurs on the wire.

Version-2 encoding and decoding both require the active catalogue snapshot.
The supplied `TypeId` must resolve to an active enum definition. A non-null
label must occur exactly in that definition. Encoding revalidates an existing
runtime value, so a value created under an older catalogue cannot cross the
boundary after its label is removed or changed. Decoding never constructs an
unchecked enum value.

The version-1 raw-call frames remain unchanged until their authenticated
execution path supplies the active catalogue to a version-2 frame boundary.
An unauthenticated or catalogue-free decoder cannot accept enum bytes.

## Syntax boundary

The lossless parser records:

* the qualified type name;
* each exact label literal and source span, in declaration order; and
* the complete declaration span.

The parser accepts comments and whitespace wherever the existing token rules
permit trivia. It requires parentheses, commas between labels, and one final
semicolon. It rejects an empty list, a trailing comma, a non-string label, a
missing comma, and an unterminated declaration. Malformed source remains in
the concrete syntax tree and does not create a partial enum declaration.

The compiler, not the parser, decodes labels and rejects semantic duplicates.
This keeps the parser lossless and keeps semantic identity in one layer.

## Catalogue and storage boundary

The semantic catalogue stores enum types separately from object types and
primitive value types. Enum names share the same qualified type namespace, so
one catalogue cannot contain an enum, object, value type, or binding with an
ambiguous qualified name.

The PostgreSQL kernel persists the ordered labels as protected catalogue data
bound to one catalogue revision and `TypeId`. It does not create a PostgreSQL
enum type. Apply, recovery, canonical hashing, and verification must reproduce
the exact order and label bytes. PostgreSQL remains storage machinery and does
not become enum identity authority.

## Required proof

Tests must prove:

* the accepted source shape, exact spans, label order, comments, and escaped
  apostrophes;
* exact diagnostics for an empty list, trailing comma, non-string label,
  missing comma, and missing close or semicolon;
* duplicate decoded labels fail without allocating or persisting a candidate;
* enum names conflict with every other declaration in the shared type
  namespace;
* canonical hashes change when a label is added, removed, renamed, or
  reordered;
* prepare, apply, recovery, and verification preserve the exact `TypeId`,
  revision, label bytes, and order; and
* version-2 golden bytes and typed nulls retain the exact enum `TypeId`;
* version 1 rejects version-2 bytes and enum runtime values; and
* version-2 encoding and decoding reject a label whose type or active
  declaration does not match.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, and focused
live PostgreSQL gates remain required.

## Implementation sequence

1. Accept this enum identity, syntax, catalogue, and storage boundary.
2. Add the lossless enum declaration and parser with direct syntax tests in
   `crates/orna-syntax/src/lib.rs` and `crates/orna-syntax/src/parser.rs`.
3. Add the semantic enum definition, shared type-namespace checks, and
   canonical hash input in at most three `orna-core` files.
4. Resolve, compare, allocate, and prepare enum definitions in at most three
   `orna-compiler` files.
5. Add one registered migration and the smallest kernel apply and recovery
   changes, split into buildable commits of at most three files.
6. Extend the canonical value codec and the first execution path only after
   active enum catalogue recovery is green.

Each commit changes one to three files and keeps the repository buildable.

## Deferred surface

This decision does not define enum-specific source casts, aliases, label
renames, localisation, display names, presenter policy, general record values,
opaque values, `sys.invoke`, or physical PostgreSQL enum types.

## Precedence

This implements the enum value-type shape in the canonical DDL and type-kind
specification. It extends ADR 0016's catalogue-backed type foundation without
changing the accepted standard primitive identities or version-one hash bytes.
