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

## Registered runtime boundary

A later implementation step introduces one immutable codec registry assembled
from checked-in standard-library code. Each entry binds one exact contract
string to one validator and canonical encoder/decoder pair. Registration fails
on duplicate contracts, an unrecognised standard type, a primitive contract,
or a contract/type mismatch. The registry is built before a connection uses an
opaque value and cannot be extended by database content.

An accepted runtime opaque value contains its nominal `TypeId` and one bounded
canonical payload. Construction and decode require the same admitted
`ActiveDatabaseRevision` and codec registry. They verify that the active type
is opaque, immutable, transient, has the registered contract, and that its
codec accepts the complete payload. Equality is nominal `TypeId` plus canonical
payload bytes.

Opaque payloads are transient. They cannot enter durable object storage,
defaults, indexes, catalogue expressions, mutation arguments, audit payloads,
or state storage. The first executable host is a CLIENT result whose exact
standard codec is registered. SERVER production and consumption remain closed
until a separate decision defines ownership and process boundaries.

## Protocol version

`ORV1`, `ORV2`, `ORV3`, `ORF1`, `ORF2`, and `ORF3` remain closed. Before the
first opaque runtime value is implemented, this decision must be amended with
exact `ORV4` and `ORF4` bytes, payload and nesting limits, registry selection,
and retained earlier-version proof. No implementation may append an opaque tag
to an existing marker.

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
version closure, and CLIENT-only execution.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
compiler, canonical-hash, recovery, and live PostgreSQL gates remain required.

## Implementation sequence

1. Parse the privileged opaque declaration without changing ordinary source.
2. Model and hash opaque catalogue definitions while keeping all slots closed.
3. Resolve and prepare the exact accepted standard definition and evidence.
4. Store, recover, and tamper-check the definition and contract.
5. Amend this decision with the registered runtime interface and exact ORV4 and
   ORF4 bytes.
6. Add checked opaque runtime values, the immutable registry, protocol codecs,
   and the first CLIENT result host.
7. Complete the product checklist only after the live registered-codec path is
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
