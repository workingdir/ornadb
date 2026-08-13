# ADR 0036: Constructed Types Use Canonical Recursive Descriptors

**Status:** Accepted

## Decision

Orna adds one backend-independent recursive type descriptor for the canonical
constructed types in the language grammar:

```text
LIST<T>
SET<T>
MAP<K, V>
OPTION<T>
STREAM<T>
```

This descriptor is a prerequisite for the mandatory `sys.invoke.Request` and
`sys.invoke.Event` values. It is not a second value type system. Existing
`ResolvedType` values are the closed, flat compatibility subset used by the
implemented catalogue and executor slices. Each catalogue-identified flat type
has one exact conversion to the new descriptor. The legacy
`ResolvedType::Scalar` form has no catalogue identity and must be migrated to
`ResolvedType::Value(TypeId)` before conversion. New constructed-type work uses
the descriptor and does not add more flat variants to `ResolvedType`.

This decision does not add `orna invoke` before its request can cross
`CALL_RAW` as one canonical typed value. The client must not resolve a target
name and call that target through the recovery path. That would bypass the
locked `sys.invoke` resolution, binding, authorisation, revision, event, and
presentation boundary.

## Descriptor algebra

The public core model is `TypeDescriptor` with this closed semantic shape:

```text
Named(TypeId)
Reference(TypeId)
List(TypeDescriptor)
Set(TypeDescriptor)
Map(TypeDescriptor, TypeDescriptor)
Option(TypeDescriptor)
Stream(TypeDescriptor)
```

`Named` carries one resolved by-value catalogue identity. Classification as a
primitive value, enum, record value, or registered opaque value remains an
active-catalogue operation. It does not carry an unchecked source name.
`Reference` carries one resolved durable object-type identity. The container
variants own their child descriptors.

The descriptor contains no source spelling, alias, nullability flag, database
name, runtime name, presenter name, PostgreSQL type, or physical-storage fact.
It has structural equality. Two descriptors are equal only when every variant
and resolved `TypeId` is equal in the same position.

The descriptor accepts at most 32 nested constructor levels and 256 total
nodes. A leaf has depth zero and one node. A constructor adds one level and
one node. `Map` counts both child subtrees. Construction checks both limits
before it returns a descriptor. No public unchecked constructor is available.

The first core commit models structure only. Active-catalogue classification,
position rules, canonical hash bytes, runtime values, and protocol bytes land
in later commits in this sequence. A structurally valid descriptor is not by
itself permission to use that type in a catalogue slot or execution position.

## Flat compatibility conversion

The existing flat model remains byte-for-byte stable while it is migrated.
Conversion has these exact rules:

```text
ResolvedType::Named(id)       -> Named(id)
ResolvedType::Value(id)       -> Named(id)
ResolvedType::Reference(id)   -> Reference(id)
ResolvedType::Scalar(scalar)  -> typed legacy-scalar failure
```

The conversion reads only the application catalogue and verified standard
snapshot pinned by the active database revision. It cannot use source spelling,
an unpinned standard snapshot, a representation contract, or a compatibility
scalar as catalogue identity authority. `Scalar(StandardScalar)` is the
version-1 compatibility form retained by ADR 0016; core must not reverse-map it
to a `TypeId`. A caller migrates that fact through the existing checked
standard-type evidence and supplies `Value(TypeId)` instead. The conversion is
not part of the first structure-only commit.

There is no reverse conversion for a constructed descriptor. A later caller
may project a `Named` or `Reference` leaf to the legacy model only when the
same active catalogue proves the exact classification. Code must not erase a
constructor to make an old API accept it.

### Active flat-leaf conversion

The only public flat conversion is:

```rust
pub fn type_descriptor_for(
    &self,
    resolved_type: ResolvedType,
) -> Result<TypeDescriptor, FlatTypeDescriptorError>;
```

It validates in variant order without returning a partial descriptor:

1. `Scalar(scalar)` returns `LegacyScalar { scalar }`. This check does not
   inspect a standard snapshot because a compatibility representation is not
   catalogue identity authority.
2. `Named(id)` checks the active application catalogue and, when present, its
   pinned standard catalogue before it classifies either result. If both contain
   `id`, it returns `AmbiguousNamedType { id }`. Otherwise it accepts only an
   enum or record value definition. An absent definition returns
   `UnknownNamedType { id }`, an object returns `NamedObjectType { id }`, and a
   primitive or opaque value definition returns `NamedValueType { id }` because
   that category must use `Value(id)`.
3. `Value(id)` first requires a pinned standard snapshot, otherwise returning
   `StandardLibraryUnavailable { value_type: id }`. It then requires `id` to
   resolve there to a primitive or opaque value definition, otherwise returning
   `UnknownStandardValueType { value_type: id }`.
4. `Reference { target }` requires `target` to resolve to an object definition
   in the active application catalogue, otherwise returning
   `ReferenceTargetNotObject { target }`. A standard-library object, value,
   enum, or record definition cannot become an application object reference.

`FlatTypeDescriptorError` is public, non-exhaustive, implements `Error`, and
derives `Clone`, `Debug`, `Eq`, and `PartialEq`. Its display messages are:

| Variant | Display |
| --- | --- |
| `LegacyScalar` | `legacy scalar type has no catalogue identity` |
| `AmbiguousNamedType` | `resolved named type is present in both application and standard catalogues` |
| `UnknownNamedType` | `resolved named type is absent from the active catalogue` |
| `NamedObjectType` | `resolved named type is an object and requires REF` |
| `NamedValueType` | `resolved named type is a value definition and requires a value identity` |
| `StandardLibraryUnavailable` | `the active database has no standard library for the resolved value type` |
| `UnknownStandardValueType` | `resolved value type is absent from the pinned standard library` |
| `ReferenceTargetNotObject` | `resolved reference target is not an active application object` |

The method does not expose a reverse conversion, change `ResolvedType`, admit a
descriptor in any catalogue or execution position, or change canonical bytes.
Its proof covers application enum and record leaves, pinned-standard enum
leaves, primitive and opaque `Value` leaves, application object references,
cross-catalogue identity collisions, every wrong-category and missing-definition
error, version-1 value and scalar failures, and unchanged legacy canonical
bytes.

### Record-field descriptor migration

Step 5 changes `RecordValueFieldDefinition` so its sole final type carrier is a
`TypeDescriptor`. It does not yet admit a constructed descriptor. The accepted
record-field family remains exactly the family implemented by ADR 0031:

* `Named(id)` for an application or pinned-standard enum; and
* `Named(id)` for a pinned-standard immutable, persistable primitive using one
  of the Boolean, integer, big integer, float, character-large-object, or
  binary-large-object kernel contracts.

An application record identity remains deferred to step 6. An application
primitive or opaque value, a pinned-standard opaque value, `Reference`,
`List`, `Set`, `Map`, `Option`, and `Stream` are rejected. `Stream` remains
permanently unavailable as a record field. Catalogue-local validation rejects
a non-`Named` descriptor and a locally disproven `Named` target before the
snapshot is returned. Canonical-hash and revision validation then resolve every
remaining `Named` identity through the active application catalogue and pinned
verified standard snapshot in record-type order and field ordinal order.
If the same identity is an accepted application enum and either a pinned
standard enum or accepted pinned standard primitive, validation returns
`AmbiguousRecordValueFieldType { record_value_type, field, type_id }` before it
selects a tag. Its display is
`record field type is present in both application and standard catalogues` and
it has no source.

Catalogue hash version 2 remains the only record-definition hash contract.
Step 5 allocates no recursive descriptor hash tag. For the accepted flat
descriptors, classification by the active hash context emits exactly the
existing bytes:

```text
enum Named(id)                -> resolved-type tag 2, then the 16-byte TypeId
standard primitive Named(id)  -> resolved-type tag 4, then the 16-byte TypeId
```

The conditional record section, record ordering, field identity, name, ordinal,
and every surrounding byte remain unchanged. A constructed descriptor fails
before canonical bytes are returned. A later step that admits `List`, `Map`,
`Option`, or another constructor must first amend this decision with its exact
recursive hash bytes and any required hash-version change.

Catalogue, canonical-hash, and revision errors replace their record-field
`ResolvedType` payload with the rejected `TypeDescriptor`; their existing
owner, field, ordering, and display contract remains unchanged. The proof
requires byte-identical existing record and record-free version-2 goldens,
exact tag-2 and tag-4 field bytes, rejection of every other leaf category and
all five constructors, cross-catalogue identity ambiguity before tag selection,
and no partial catalogue, revision, or digest.

The descriptor-native constructor is
`RecordValueFieldDefinition::try_new_descriptor`. It accepts only a flat
`Named` or `Reference` descriptor before catalogue classification. `List`,
`Set`, `Map`, `Option`, and `Stream` return the public, non-exhaustive
`RecordValueFieldConstructionError::ConstructedTypeNotAccepted { descriptor }`
before a field exists. That error displays
`constructed record field descriptors are not accepted` and has no source.
Catalogue validation remains responsible for rejecting `Reference` and an
unsupported or ambiguous `Named` identity.

The one public durable classification seam is
`DeployableRevision::record_value_field_descriptor_class(&TypeDescriptor) ->
Result<RecordValueFieldDescriptorClass, RecordValueFieldDescriptorError>`.
It reads only the deployable candidate catalogue and the verified standard
snapshot pinned in its catalogue-hash context. The public, non-exhaustive
`RecordValueFieldDescriptorClass` has `ApplicationEnum(TypeId)`,
`StandardEnum(TypeId)`, and `StandardPrimitive(TypeId)`. The public, non-exhaustive
`RecordValueFieldDescriptorError` has `StandardLibraryUnavailable`,
`Unsupported`, and `Ambiguous { type_id }`. Their displays are, respectively,
`deployable revision has no pinned standard library for record field classification`,
`record field descriptor is not supported by the deployable revision`, and
`record field type is present in both application and standard catalogues`.
The error derives `Clone`, `Debug`, `Eq`, and `PartialEq`, implements `Error`,
and has no source.
PostgreSQL apply uses only this seam to select the application-enum,
pinned-standard-enum, or pinned-primitive durable tuple. Both enum classes
retain catalogue-hash tag 2; the primitive class retains tag 4. Durable
standard enums carry the same exact standard-library revision pin as standard
primitives. Recovery reconstructs `Named(id)` only from those exact tuples and
the catalogue-wide standard-library pin; it does not recreate a second
classifier.

The one-to-three-file, always-green migration uses this ordered bridge:

1. Add a fallible flat compatibility constructor beside the existing
   constructor. It accepts `Named`, `Value`, and `Reference`; `Scalar` returns
   `LegacyScalar { scalar }` through a public, non-exhaustive
   `RecordValueFieldConstructionError` before a field exists. That error
   displays `legacy scalar cannot form a record field descriptor` and has no
   source.
2. Move existing producers to that fallible constructor in one-to-three-file
   commits. The stored fact remains the old `ResolvedType`, so every intermediate
   commit is behaviour- and byte-identical except that an already-invalid scalar
   fails earlier.
3. Move consumers to the descriptor view in one-to-three-file commits. During
   this interval one private dual carrier deterministically derives `Named` from
   `Named` or `Value` and `Reference` from `Reference`. No API accepts both facts,
   so disagreement is unrepresentable. Active hash-context classification later
   recovers the distinction between the old tag-2 and tag-4 leaves.
4. After every consumer uses the descriptor, remove every in-tree use of the
   infallible `new(ResolvedType)` and remove that constructor. In the same
   ownership flip, replace the stored `ResolvedType` and optional descriptor
   with one required descriptor, remove `resolved_type`, rewrite fallible
   `try_new` as the temporary flat `ResolvedType`-to-descriptor compatibility
   projection, and add the descriptor-native constructor. The latter rejects
   every constructor before a field exists while step 5 remains selected.
5. Move the remaining producers from `try_new` to the descriptor-native
   constructor in bounded commits, then remove `try_new`, the compatibility
   projection, and the `LegacyScalar` construction-error variant before step 6
   opens. The descriptor-native constructor and its
   `ConstructedTypeNotAccepted` error remain the sole construction boundary.

The bridge is not a durable format, public catalogue promise, second hash
authority, or permission to admit a descriptor-backed field in a catalogue
early. Its proof includes the scalar and five constructor construction errors
and the absence of a partial field.

## Source syntax

The parser accepts the existing canonical EBNF recursively:

```ebnf
type_spec
    = qualified_name
    | scalar_type
    | "REF" type_spec
    | "LIST" "<" type_spec ">"
    | "SET" "<" type_spec ">"
    | "MAP" "<" type_spec "," type_spec ">"
    | "STREAM" "<" type_spec ">"
    | "OPTION" "<" type_spec ">"
    | type_spec "?"
    ;
```

The syntax tree preserves the exact source bytes, spans, trivia, constructor
kind, child order, and postfix `?`. `T?` and `OPTION<T>` resolve to the same
semantic `Option` descriptor. Repeated postfix markers are nested options.
The parser does not rewrite one spelling to the other.

`REF` parses one recursive type specification because the canonical grammar
does so. Semantic checking later requires its complete target to resolve to
one named object type. The parser must not silently narrow the source grammar
to make the current executor easier to implement.

Malformed angle brackets, missing map keys or values, missing commas, missing
children, and a descriptor beyond the accepted depth recover to the next
complete declaration with one direct diagnostic. Parsing a constructor does
not admit it in an object field, value field, parameter, return, expression,
or durable slot.

## Position rules

Constructed descriptors enter execution one position at a time. Until a later
commit explicitly opens a position, every existing catalogue, compiler,
artifact, storage, executor, and protocol gate retains its current rejection.

The target position order for the `sys.invoke` carrier is:

1. nested immutable record-value fields;
2. `OPTION`, `LIST`, and `MAP` runtime values;
3. the sealed Ring-1 `sys.invoke.Request` record family;
4. the sealed Ring-1 `sys.invoke.Event` record family;
5. `STREAM<sys.invoke.Event>` as a function result shape; and
6. one raw call to the stable `sys.invoke` identity.

`SET` remains in the descriptor and source grammar but needs a separately
accepted canonical ordering and duplicate rule before it gains a runtime
value. `STREAM` is an execution result shape. It is never a materialised
`RuntimeValue`, record field, collection element, map key, map value, option
child, function argument, object field, or durable value.

The first `OPTION`, `LIST`, and `MAP` runtime boundary must define exact value
invariants and a new codec version before it is executable. `MAP` must define
canonical key ordering and duplicate detection. A later decision must also
define the sealed `sys.invoke` system catalogue identities, the open
`sys.invoke.Value` carrier, and exact request and event descriptors. This ADR
does not invent those bytes early.

## Required proof

The descriptor model must prove:

* every leaf and constructor retains its exact structural identity;
* nested descriptors at depth 32 and total size 256 are accepted;
* depth 33 and size 257 fail with distinct typed errors;
* `Map` counts and validates both branches without integer overflow;
* construction never returns a partial descriptor after a child failure;
* equality distinguishes every constructor, child position, and `TypeId`;
* the old flat type model and all existing canonical bytes remain unchanged;
  and
* arbitrary bounded construction input does not panic.

The syntax slice must additionally prove:

* exact lossless parsing and spans for every constructor;
* nested prefix and postfix forms preserve source order and trivia;
* `T?` and `OPTION<T>` have equal semantic structure after resolution;
* malformed and over-depth forms recover to a later valid declaration; and
* no existing semantic or execution position becomes accepted through parsing
  alone.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
syntax, core, canonical-hash, protocol, and live PostgreSQL gates remain
required as each later position opens.

## Implementation sequence

1. Accept this constructed-type prerequisite.
2. Add the bounded recursive core descriptor without changing `ResolvedType`.
3. Parse every canonical recursive type constructor losslessly.
4. Add active-catalogue leaf validation and exact flat conversion.
5. Migrate record field definitions and catalogue hash evidence to descriptors
   while retaining all earlier golden bytes when no constructor is present.
6. Admit nested immutable record values against one active revision.
7. Define and implement canonical `OPTION`, `LIST`, and `MAP` runtime values.
8. Amend the value and frame codec with their exact new version bytes.
9. Define and install the sealed Ring-1 invocation type family.
10. Carry one complete canonical request to the stable `sys.invoke` function.
11. Add `STREAM<sys.invoke.Event>` and the first canonical-result event path.
12. Add the ordinary `orna invoke` adapter only after the server path is live.

Each implementation commit changes one to three files, uses a signed
conventional commit, and keeps the repository buildable.

## Deferred surface

This decision does not accept collection storage, `SET` runtime values,
general streams, nested containers in application functions, defaults,
subtyping, unions, generic user definitions, presenter selection, output
aliases, runtime offers, remote transport, TLS, or `sys.invoke` request bytes.
Those surfaces remain open until their exact authority and proof land in the
ordered sequence above.

## Precedence

This decision implements the constructed-type prerequisite from
`spec/spec/orna.ebnf`, `spec/docs/12-object-relational-model.md`,
`spec/docs/13-invocation-system.md`, and `spec/docs/27-wire-protocol.md`. The
canonical sources leave nullability syntax and constructed-type implementation
details open. For the accepted descriptor, source syntax, limits, migration,
and position order above, this decision supplies those details.

It extends work ADRs 0025, 0031, 0032, 0034, and 0035. All earlier value,
record, protocol, raw recovery, system-health, and opaque-codec bytes remain
closed.
