# ADR 0039: Canonical Collection Values Use ORV5 and ORF5

**Status:** Accepted

## Decision

Protocol version 5 adds the first canonical wire representation for the checked
`OPTION`, `LIST`, and `MAP` runtime values admitted by work ADR 0036. It does
not define another value model. The codec consumes and returns
`orna_core::value::RuntimeValue` and reconstructs collection values only through
their checked public constructors.

`ORV5` is a strict superset of `ORV4`. `ORV1` through `ORV4` and `ORF1`
through `ORF4` remain closed to constructed values and retain every accepted
byte and rejection rule. Version 5 does not change catalogue hashes, durable
storage, compiler positions, function signatures, executors, record-field
positions, `SET`, or `STREAM`.

## Public codec seam

The public value-codec seam adds only:

```rust
pub fn encode_constructed_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError>;

pub fn decode_constructed_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError>;
```

These are complete version-5 operations, not collection-only helpers. They
retain every version-4 scalar, enum, record, reference, null, and registered
opaque value. The active revision and registry are the same immutable
authorities used by ORV4. A top-level registered opaque value remains accepted.
An opaque collection leaf remains rejected because ADR 0036 does not admit an
opaque collection descriptor.

Recursive parsing, descriptor validation, size arithmetic, node accounting,
map-order verification, and version selection remain behind this seam. There
is no public unchecked wire value, descriptor decoder, partial collection, or
second collection representation.

## ORV5 envelope

Every ORV5 value retains the existing 25-byte envelope:

```text
offset  size  field
0       4     ASCII `ORV5`
4       1     value tag
5       16    TypeId bytes, or the constructed-value zero sentinel
21      4     unsigned payload length, big-endian
25      n     payload
```

Tags `0x00` through `0x0c` retain their ORV4 meaning and exact bytes after the
marker. Tag `0x0d` is one constructed value. It requires all sixteen bytes in
the envelope identity field to be zero. Those zero bytes are a sentinel and
are not a `TypeId`.

One generic constructed tag is sufficient because the canonical descriptor
already identifies `OPTION`, `LIST`, or `MAP`. The wire does not duplicate that
constructor in a second outer tag.

The constructed payload begins with:

```text
2 bytes  unsigned descriptor length, big-endian
n bytes  one complete canonical descriptor
...      constructor content
```

The descriptor length is non-zero. It covers only the descriptor, not the
two-byte length or constructor content. A descriptor must consume that complete
bounded region with no trailing byte.

If the declared descriptor region exceeds the remaining outer payload,
`TruncatedConstructedDescriptor` reports that declared length and the complete
number of bytes available after the length prefix. Inside an otherwise
available bounded region, `TruncatedConstructedDescriptorNode` reports the
zero-based start offset of the incomplete node, the minimum bytes required for
that known node, and the bytes remaining from that offset. A missing node tag
requires one byte; a `Named` or `Reference` tag requires seventeen bytes. A
constructor with a missing child reports the child's start offset and a
one-byte minimum. These syntax failures do not become `TypeDescriptorError`;
that source remains limited to the descriptor depth and node bounds it owns.

## Canonical descriptor bytes

The descriptor is prefix encoded in preorder:

```text
0x00  Named       then 16 TypeId bytes
0x01  Reference   then 16 TypeId bytes
0x02  List        then one child descriptor
0x03  Map         then key descriptor, then value descriptor
0x04  Option      then one child descriptor
```

There is no descriptor tag for `SET` or `STREAM`. An unrecognised tag fails;
it is not reserved as an extension point inside ORV5. A later descriptor family
requires a later codec version.

The descriptor retains ADR 0036's maximum constructor depth of 32 and maximum
node count of 256. Its exact maximum accepted encoded length is 2,304 bytes:
128 seventeen-byte leaves, 127 binary constructor bytes, and one unary
constructor byte. The unsigned 16-bit descriptor length is therefore
sufficient. Descriptor construction and validation use the active application
catalogue and its pinned verified standard snapshot. Every ADR 0036 ambiguity,
missing identity, wrong category, unsupported map key, and closed opaque leaf
remains exact.

The descriptor root for tag `0x0d` must be `Option`, `List`, or `Map`.
`Named` and `Reference` roots use their retained legacy ORV4 tags. A nested
value's runtime type must equal the descriptor at that exact position.

## Constructor content

An `OPTION` content is:

```text
1 byte   presence: exactly 0 or 1
if 1:
4 bytes  complete child-value length, big-endian
n bytes  one complete ORV5 child value
```

Presence zero has no child bytes. Presence one has exactly one complete child.

A `LIST` content is:

```text
4 bytes  element count, big-endian
repeat count times in list order:
4 bytes  complete element-value length, big-endian
n bytes  one complete ORV5 element value
```

A `MAP` content is:

```text
4 bytes  entry count, big-endian
repeat count times in canonical key order:
4 bytes  complete key-value length, big-endian
n bytes  one complete ORV5 key value
4 bytes  complete mapped-value length, big-endian
n bytes  one complete ORV5 mapped value
```

All child lengths include the complete 25-byte child envelope. A child marker
must be `ORV5`. A record uses the existing record payload and field-entry shape,
but every complete nested field value uses `ORV5`. Nested immutable records
therefore remain recursive without a second record encoding.

LIST retains caller order and duplicates. MAP entries on the wire must already
use ADR 0036's canonical key order. Decode constructs the checked map and then
requires the returned canonical entry sequence to equal the wire sequence. It
does not silently sort a non-canonical wire map. Duplicate keys return the
existing checked collection error with their retained original wire indexes.

## Bounds and validation order

The outer payload limit remains 16 MiB. The descriptor, constructor metadata,
every child-length prefix, and every complete nested envelope count towards
that limit. There is no independent collection-entry limit. The authoritative
runtime bound remains 65,536 value nodes. Every runtime value counts one node;
record fields and OPTION, LIST, and MAP descendants count exactly as in ADR
0036. Counts and lengths use checked arithmetic before allocation or slicing.

Decode validates in this order and returns no partial value:

1. Validate the complete outer header, exact marker, declared 16 MiB bound,
   truncation, and trailing bytes.
2. Validate the value tag and, for `0x0d`, the all-zero identity sentinel.
3. Validate the descriptor length, syntax, complete consumption, constructor
   depth, and descriptor node count.
4. Require an admitted `Option`, `List`, or `Map` root and preflight the whole
   descriptor against the active revision. Cross-catalogue ambiguity precedes
   category rejection.
5. Structurally parse the complete value tree in wire order and count runtime
   nodes. MAP visits each key before its value. The first node that would make
   the count 65,537 returns the node-limit error before any semantic
   materialisation or any later sibling is visited.
6. Materialise leaves and records in wire order through the existing ORV5
   scalar, enum, record, reference, and registered-opaque decoders.
7. Construct each collection through `RuntimeValue::option`,
   `RuntimeValue::list`, or `RuntimeValue::map`. After every required child has
   materialised, preserve the checked core null, mismatch, path, revalidation,
   and duplicate precedence.
8. Require a MAP's returned canonical sequence to equal its input sequence.

Structural errors that make step 5 impossible are returned where encountered:
an invalid presence byte, truncated count, truncated entry header, impossible
child length, wrong child marker, or bytes outside a bounded child. At every
bounded descriptor, child, and outer region, truncation precedes trailing-byte
rejection.

Step 4 uses the matching public constructor with a canonical `None`, empty
LIST, or empty MAP body and discards that checked value. This exercises ADR
0036's one descriptor-classification authority without copying its catalogue
rules into the protocol crate. It does not expose or retain a placeholder
value.

The implementation uses a private two-phase parse. Its first phase retains
only checked byte spans and structural facts and performs descriptor and global
node preflight. Its second phase materialises through existing public checked
constructors. A direct recursive semantic decoder is not acceptable because it
could report an inactive early child before the authoritative parent node
limit. The private parse representation is not an interface and cannot escape
the codec.

Encoding revalidates the complete value against the supplied active revision
and registry before it emits bytes. It uses the descriptor owned by the checked
constructed value, complete nested ORV5 envelopes, and the map's already
canonical stored sequence. It never erases a descriptor to make an earlier
codec accept a value.

## Error contract

`ValueCodecError` remains public, non-exhaustive, implements `Error`, and
derives `Clone`, `Debug`, `Eq`, and `PartialEq`. Version 5 adds:

| Variant | Payload | Display |
| --- | --- | --- |
| `ConstructedTypeIdentityNotZero` | `identity: TypeId` | `constructed runtime value identity must be zero` |
| `TruncatedConstructedHeader` | `actual: usize` | `constructed runtime value header is truncated` |
| `EmptyConstructedDescriptor` | none | `constructed runtime value descriptor is empty` |
| `TruncatedConstructedDescriptor` | `declared: usize, available: usize` | `constructed runtime value descriptor is truncated` |
| `TruncatedConstructedDescriptorNode` | `offset: usize, required: usize, available: usize` | `constructed runtime value descriptor node is truncated` |
| `TrailingConstructedDescriptor` | `remaining: usize` | `constructed runtime value descriptor has trailing bytes` |
| `UnknownConstructedDescriptorTag` | `tag: u8` | `constructed runtime value descriptor tag is unknown` |
| `InvalidConstructedDescriptor` | `source: TypeDescriptorError` | `constructed runtime value descriptor is invalid` |
| `UnsupportedConstructedDescriptor` | `descriptor: TypeDescriptor` | `constructed runtime value descriptor is not accepted` |
| `InvalidOptionPresence` | `value: u8` | `constructed OPTION presence is invalid` |
| `TruncatedCollectionEntry` | `path: Vec<CollectionValuePathSegment>` | `constructed runtime value entry is truncated` |
| `ConstructedChild` | `path: Vec<CollectionValuePathSegment>, source: Box<ValueCodecError>` | `constructed runtime value child is invalid` |
| `NonCanonicalMapOrder` | `index: usize` | `constructed MAP entries are not in canonical key order` |
| `CollectionValue` | `source: CollectionValueError` | `constructed runtime value is invalid` |

`InvalidConstructedDescriptor`, `ConstructedChild`, and `CollectionValue`
expose their typed source. The other new variants have no source. The path
vector uses the public ADR 0036 path segments and is rooted at the outer
constructed value. Existing generic envelope errors remain authoritative for
the outer envelope. Existing scalar, enum, record, opaque, payload, and
active-revision errors retain their exact payloads, displays, and order.

`TruncatedCollectionEntry` names the first incomplete expected path.
Once one complete child region has been isolated, `ConstructedChild` retains
its exact nested marker, truncation, trailing, type, or semantic codec source.
`NonCanonicalMapOrder.index` is the first wire entry whose checked canonical
position differs from the entry at the same zero-based index in the checked
map returned by `RuntimeValue::map`. A nested codec or materialisation failure,
including an undeclared enum label or inactive record, uses
`ConstructedChild`. `CollectionValue` reports checked-constructor failures only
after every required child materialises. Encoding starts from existing runtime
values and may therefore return `CollectionValue` during complete active
revalidation.

## ORF5 frame boundary

An ORF5 frame with no embedded value differs from ORF4 only in the four-byte
frame marker. Frame tags, directions, flags, stream rules, payload layouts,
event sequence, channel windows, cancellation, maximum live streams, argument
counts, byte credit, and the 16 MiB plus 64-byte frame limit do not change. In
a value-bearing frame, the frame marker changes to ORF5 and every embedded
complete value marker changes to ORV5; all other accepted bytes remain exact.

Step 8 does not admit `RuntimeValue::Constructed` in `CALL_ARGUMENT` or in an
`Event::Value`. Those positions return:

```rust
FrameCodecError::ConstructedValueNotAccepted {
    descriptor: TypeDescriptor,
}
```

Its display is `constructed runtime values are not accepted by protocol 5 frames`
and it has no source. Encoding checks the typed value before it writes bytes.
Decoding first validates and materialises the isolated complete ORV5 value
through the value codec. A malformed value retains the existing
`FrameCodecError::Value { source }`; a valid constructed value then returns
`ConstructedValueNotAccepted` with its exact descriptor before the frame is
returned. Either failure changes no connection state, consumes no window
credit, and returns no partial frame. Later accepted sealed
`sys.invoke.Request` and `sys.invoke.Event` positions selectively open
constructed frame values; Step 8 does not make arbitrary application
parameters or results collection-capable.

The exact version-5 client hello is:

```text
ORNA 01 00 00 05 00 00 00 00
```

The exact server ACK changes only the message byte from `01` to `81`.
The adapter authenticates the session, recovers one complete active revision,
and constructs its matching immutable opaque registry before it writes the
ACK. Protocol major 5 accepts minor zero only. Invalid flags, reserved bytes,
minor versions, or unsupported majors close without an ACK under the existing
local-socket policy.

Negotiation and transport exposure are separate implementation commits from
the in-process codec. Defining ORF5 does not imply that a live adapter already
offers it.

## Required proof

Public-behaviour tests must prove:

* independent exact goldens and round trips for empty and non-empty OPTION,
  LIST, and MAP values and nested combinations;
* descriptor bytes bind every leaf identity, constructor, child position, and
  exact descriptor length;
* descriptor depth 32 and node count 256 pass, while the next value fails with
  the exact typed descriptor error;
* value node 65,536 passes and node 65,537 fails before a later stale or
  malformed sibling is semantically visited;
* OPTION presence, counts, child lengths, nested markers, descriptor tags,
  sentinels, truncation, trailing bytes, and checked-arithmetic boundaries
  fail with their exact public errors;
* every admitted leaf family, nested immutable records, and a top-level opaque
  value round trip against the same active revision and registry;
* opaque collection leaves, `SET`, `STREAM`, missing identities, wrong
  categories, and cross-catalogue collisions remain closed;
* canonical MAP input round trips, input permutations encode identically, a
  non-canonical wire permutation fails instead of being sorted, and duplicate
  indexes remain the original wire indexes;
* stale active definitions, labels, records, object targets, standard pins,
  opaque contracts, and registries fail without returning a value;
* every accepted ORV4 value retains exact bytes after changing the marker of
  every complete outer or nested envelope to ORV5;
* ORV1 through ORV5 reject every other marker in both directions and all
  earlier golden bytes remain unchanged;
* arbitrary bounded ORV5 bytes and descriptor bytes never panic or allocate
  from an unchecked declared count;
* ORF5 retains exact ORF4 non-value and legacy-value frame bytes after marker
  substitution, while both constructed value-bearing directions return the
  exact closed-position error without state or credit change;
* exact protocol-5 hello and ACK succeed only after active-revision and
  registry readiness, and versions 1 through 4 remain exact; and
* the installed raw path does not accept a constructed application argument or
  emit a constructed application result before the sealed invocation carriers
  land.

Tests use the public codec, frame, connection, and socket interfaces. They do
not inspect source constants, reproduce the recursive parser, or compute their
expected bytes with the implementation under test.

## Implementation sequence

1. `docs(protocol): define ORV5 and ORF5 constructed values` changes this ADR
   and the work-ADR index only.
2. `feat(protocol): encode and decode ORV5 values` changes
   `crates/orna-protocol/src/lib.rs` only. Public tracer tests land one
   behaviour at a time with their production path.
3. `test(protocol): exhaust ORV5 malformed inputs` changes
   `crates/orna-protocol/src/lib.rs` only and completes compatibility,
   precedence, boundary, and arbitrary-input evidence without production
   changes.
4. `feat(protocol): delegate ORF5 frames` changes
   `crates/orna-protocol/src/lib.rs` and `crates/orna-protocol/src/frame.rs`.
   It retains constructed frame closure.
5. `feat(server): negotiate protocol 5` changes
   `crates/orna-server/src/raw_socket.rs` only.
6. `test(server): prove protocol 5 socket closure` changes
   `crates/orna-server/tests/standard_database.rs` only and exercises the real
   local socket against the running private database.

Each commit changes one to three files, uses a signed Conventional Commit, and
keeps the repository buildable. Normal format, strict Clippy, rustdoc, diff,
similarity, workspace, protocol, socket, system, and live PostgreSQL gates
remain required.

## Deferred surface

This decision does not admit collection types in application function
signatures, source expressions, constructors, record fields, SERVER result
rows, PostgreSQL storage, catalogue hashes, physical plans, executors, audit
payloads, state storage, defaults, presenters, `SET`, or `STREAM`. It does not
define the sealed `sys.invoke.Request`, `sys.invoke.Event`, invocation function,
general collection arguments/results, remote transport, TLS, or presentation.

## Precedence

This decision implements Step 8 of work ADR 0036. For this narrow version-5
codec and frame scope, it extends work ADRs 0025, 0026, 0029, 0031, and 0034.
Those earlier byte contracts remain authoritative for their markers and
retained tags. Work ADR 0036 remains authoritative for descriptor structure,
collection construction, admissible leaves, canonical MAP order, validation
precedence, and limits. The canonical specification remains authoritative
outside this accepted implementation boundary.
