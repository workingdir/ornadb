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
implemented catalogue and executor slices. Each accepted flat type has one
exact conversion to the new descriptor. New constructed-type work uses the
descriptor and does not add more flat variants to `ResolvedType`.

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
ResolvedType::Scalar(scalar)  -> Named(accepted standard TypeId for scalar)
```

The scalar conversion requires one verified standard snapshot and its exact
accepted scalar binding. It cannot use a hard-coded compatibility scalar as
catalogue identity authority. A missing, mismatched, or non-primitive binding
is a typed failure. The conversion is not part of the first structure-only
commit.

There is no reverse conversion for a constructed descriptor. A later caller
may project a `Named` or `Reference` leaf to the legacy model only when the
same active catalogue proves the exact classification. Code must not erase a
constructor to make an old API accept it.

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
