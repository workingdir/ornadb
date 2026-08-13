# ADR 0034: Opaque Values Require Registered Canonical Codecs

**Status:** Accepted

## Decision

Orna accepts opaque value types as nominal, immutable, transient catalogue
definitions. An opaque value keeps its stable `TypeId`, but the kernel does not
interpret its payload. One accepted, versioned standard-library codec contract
is the only authority that can validate and construct values of that type.

The first exact declaration form is:

```sql
CREATE TYPE std.example.token AS VALUE OPAQUE
    KERNEL CONTRACT 'std.example.token@1'
    IMMUTABLE
    TRANSIENT;
```

`OPAQUE`, `KERNEL CONTRACT`, `IMMUTABLE`, and `TRANSIENT` are required exactly
once and in that order. The contract is a non-empty printable ASCII string of
at most 128 bytes. It has the same closed lexical rules as the accepted
primitive kernel contracts. `PERSISTABLE`, fields, labels, constructors,
defaults, checks, documentation, `SEALED`, and repeated modifiers are rejected.

This syntax is privileged standard-library source. Ordinary application source
cannot declare an opaque type or select a codec contract. The accepted standard
snapshot owns the type identity, semantic name, and contract. A database does
not load a codec named by source, an environment variable, a filesystem path,
or a shared library.

## Catalogue boundary

`ValueTypeKind` gains `Opaque`. An opaque definition reuses
`ValueTypeDefinition` and has:

* one nominal `TypeId`;
* one qualified semantic name;
* `ValueTypeMutability::Immutable`;
* `ValueTypePersistence::Transient`; and
* one versioned representation contract.

`ValueTypeDefinition::opaque` is the only normal constructor for this kind.
The existing primitive constructor remains primitive-only. Record and enum
definitions remain separate catalogue families.

Canonical catalogue hash version 2 conditionally encodes opaque definitions in
the existing value-type section. The `ValueTypeKind` byte is `2`; primitive
remains `1`. The existing identity, name, mutability, persistence, and contract
bytes are otherwise unchanged. Definitions remain sorted by `TypeId`. A
catalogue without an opaque definition retains identical version-2 bytes.
Version 1 rejects an opaque definition before slot validation.

The standard-library digest and source evidence bind the declaration and
contract. A contract change changes the standard catalogue digest and requires
a new accepted standard-library version. It never silently reinterprets an old
value.

## Closed first implementation boundary

The first implementation admits definitions only. It does not admit opaque
types in object fields, function parameters, function returns, expressions,
CLIENT artifacts, SERVER artifacts, result rows, protocol values, or physical
plans. Generic catalogue and revision validation must reject every such slot
with the owning definition identity and opaque `TypeId`.

This definition-only boundary is deliberate. Treating an arbitrary byte vector
as an opaque value would create a second, unregistered codec authority. The
product checklist remains open until the registered runtime and protocol path
below is complete.

## First registered contract

The first runtime contract is `orna.std.value.opaque-token@1`. The still
unpublished initial standard library adds `std.types.OPAQUE_TOKEN` with
`TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14])` and
exports it as `std.OPAQUE_TOKEN`. It has no prelude spelling. The type is
immutable and transient. Its value is exactly sixteen bytes. Every sixteen-byte
sequence is canonical; every other length is invalid. The codec does not assign
text, UUID, cryptographic, secret, or database semantics to those bytes.

The accepted `orna.std/1` source, manifest, digest, source revision, catalogue
revision, and PostgreSQL standard snapshot bind this definition before the
first publication. Their existing final-byte-`1` identities and root source
revision remain unchanged; their pinned source and digest goldens advance
together. No released database accepts the earlier development digest, so the
implementation does not create or persist an artificial predecessor revision.
After the first publication, any standard semantic change requires a new
standard-library revision and a normal append-only upgrade.

## Registered runtime boundary

One immutable codec registry is assembled from checked-in standard-library
code and one verified standard snapshot. Its first and only entry binds the
exact type name and contract above to the sixteen-byte validator and identity
canonicaliser. Registration rejects a duplicate name, contract, or `TypeId`;
an unrecognised opaque definition; a missing accepted definition; a primitive,
enum, or record definition; a contract or semantic-name mismatch; and a
standard snapshot other than the one used by the active revision. The registry
is built before a connection uses an opaque value and cannot be extended by
database content, source text, configuration, environment state, or a dynamic
library.

An accepted runtime opaque value contains its nominal `TypeId` and one bounded
canonical payload. Construction and decode require the same admitted
`ActiveDatabaseRevision` and codec registry. They verify that the active
revision pins the registry's verified standard snapshot, that the active type
is opaque, immutable, transient, has the registered name and contract, and that
its codec accepts the complete payload. Equality is nominal `TypeId` plus
canonical payload bytes.

Opaque payloads are transient. They cannot enter durable object storage,
defaults, indexes, catalogue expressions, mutation arguments, audit payloads,
or state storage. The first executable host is a parameter-free, immutable,
`INVOKER` CLIENT function with one non-null `std.OPAQUE_TOKEN` return. Core
catalogue validation admits an opaque type only in that exact return slot.
The compiler continues to reject opaque application syntax and does not create
this host. A direct checked version-2 CLIENT artefact and live protected-dispatch
fixture prove the boundary without inventing an opaque source literal.
SERVER production and consumption remain closed until a separate decision
defines ownership and process boundaries.

The version-2 `orna.client-plan` artefact retains the eight-byte
`ORNACP\0\0` magic. It then contains `u32(2)`, operation `u8(2)`, the sixteen
raw `TypeId` bytes, `u32(16)`, and the sixteen canonical token bytes. Version 1
continues to accept only its Boolean operation and exact fourteen bytes.
Version 2 accepts only the opaque operation and exact forty-nine bytes. The
CLIENT evaluator decodes the plan, constructs the value through the active
registry, and returns no caller-owned or unchecked byte buffer.

## Protocol version 4

`ORV1`, `ORV2`, `ORV3`, `ORF1`, `ORF2`, and `ORF3` remain closed. Version 4
uses `ORV4` and `ORF4`; no opaque tag is added to an earlier marker.

An `ORV4` value retains the exact 25-byte header from versions 1 through 3:

```text
offset  size  field
0       4     ASCII `ORV4`
4       1     value tag
5       16    TypeId bytes
21      4     unsigned payload length, big-endian
25      n     payload
```

Tags `0x00` through `0x0b` retain their version-3 meanings and bytes after the
marker. Tag `0x0c` is one non-null registered opaque value. It requires the
exact registered opaque `TypeId`; its payload is the complete canonical codec
payload. The first codec therefore requires length 16. There is no opaque-null
tag. Version-4 record field values use complete nested `ORV4` values, but the
existing record field policy still rejects opaque fields and nested records.
All codec versions treat the new standard opaque identity as a standard value
identity, so scalar and reference tags reject it. Versions 1 through 3 do not
gain an opaque representation.

The shared value payload limit remains 16 MiB. Codec validation occurs after
the generic length, truncation, and trailing-byte checks and before a runtime
value is returned. Encoding and decoding take both one complete
`ActiveDatabaseRevision` and its immutable registry. A mismatched active
revision, standard snapshot, type, contract, payload, or registry is a typed
failure. Arbitrary input cannot select or construct a codec.

`ORF4` retains every version-3 frame byte after the four-byte marker. Every
embedded value uses `ORV4`. Protocol negotiation selects frame version 4 only
when both peers select the exact version; catalogue or active-only version 2 or
3 contexts cannot decode it. The existing frame, argument, channel-window,
sequence, cancellation, and 16 MiB plus 64-byte frame limits do not change.
Version 4 carries the first opaque CLIENT result through the existing protected
raw dispatch and does not admit an opaque call argument.

## Required proof

The definition slice must prove:

* exact lossless syntax and recovery after every malformed modifier, contract,
  and terminating token;
* ordinary source cannot declare opaque types;
* standard checking binds one exact name, identity, contract, source span, and
  origin;
* primitive, record, enum, and opaque kinds cannot be confused;
* version-1 rejection and exact version-2 kind bytes;
* record-free and opaque-free version-2 golden bytes do not change;
* name, `TypeId`, contract, mutability, persistence, and kind mutations change
  or reject the expected catalogue hash;
* active and deployable revision validation admits a standalone definition but
  rejects every object and function slot that names it; and
* apply, recovery, and tamper tests preserve the exact kind and contract without
  creating a PostgreSQL type, column, executable, or codec lookup.

The runtime amendment must additionally prove exact codec registration,
canonical round trips, wrong-active-revision rejection, wrong-contract
rejection, bounded arbitrary-input decode, transient-slot rejection, protocol
version closure, and CLIENT-only execution. It must also prove:

* exact accepted `std/1` source, manifest, unchanged identities, new digest,
  durable rows, recovery, and rejection of the superseded development digest;
* every possible byte value round trips canonically in every one of the sixteen
  token positions, while every other payload length fails;
* the exact version-2 CLIENT artefact bytes and retained version-1 bytes;
* exact `ORV4` and `ORF4` goldens, all earlier-version goldens unchanged, and
  cross-version marker rejection in both directions;
* a registered opaque value cannot enter a parameter, object field, SERVER
  return, record field, mutation, result rows, or a typed null;
* one authorised CLIENT function returns the token through protocol 4, and a
  stale revision, stale registry, wrong contract, malformed artefact, revoked
  grant, or wrong payload length emits no value; and
* arbitrary registry definitions and arbitrary `ORV4` bytes never panic.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
compiler, canonical-hash, recovery, and live PostgreSQL gates remain required.

## Implementation sequence

1. Parse the privileged opaque declaration without changing ordinary source.
2. Model and hash opaque catalogue definitions while keeping all slots closed.
3. Resolve and prepare the exact accepted standard definition and evidence.
4. Store, recover, and tamper-check the definition and contract.
5. Amend this decision with the first registered contract, CLIENT artefact, and
   exact ORV4 and ORF4 bytes.
6. Install and recover the accepted initial-standard opaque definition.
7. Add checked opaque runtime values and the immutable registry.
8. Add the closed CLIENT artefact and evaluator host.
9. Add ORV4, ORF4, and exact protocol-4 negotiation.
10. Carry one authorised opaque CLIENT result through the live protected raw
    path.
11. Complete the product checklist only after the live registered-codec path is
    green.

Each implementation commit changes one to three files, uses a signed
conventional commit, and keeps the repository buildable.

## Deferred surface

This decision does not accept application-defined codecs, dynamic libraries,
filesystem codec discovery, persistent opaque values, opaque object fields,
SERVER opaque values, UI algebra, runtime contracts, nested containers,
presenters, state storage, `sys.invoke`, or `std.ui.UI` itself.

## Precedence

This decision narrows the open opaque-value details in the canonical EBNF,
DDL, value-codec, wire-protocol, and UI proposals. It preserves the locked
distinction between durable type identity and transient opaque values. The
canonical specification remains authoritative outside this narrow boundary.
