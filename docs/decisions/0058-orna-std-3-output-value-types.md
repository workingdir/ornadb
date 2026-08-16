# ADR 0058: `orna.std/3` Standard Output Value Types

**Status:** Accepted

## Decision

`orna.std/3` is the append-only child standard snapshot of `orna.std/2`
(work ADR 0055). It registers the two output value types accepted by work
ADR 0057 — `std.terminal.Document` and `std.io.ByteStream` — as
standard-library `VALUE OPAQUE IMMUTABLE TRANSIENT` types with stable
manifest identities, one new retained source unit (`std/output.orna`), and
codec registrations in `orna_standard::registered_opaque_codecs`.

`orna.std/1` and `orna.std/2` keep their identities, retained source bytes,
digest contracts, verifiers, and goldens byte-for-byte. The new snapshot is
purely additive: it reuses the V2 type catalogue and the one `std.invoke.echo`
executable unchanged, and appends the new schemas, value types, bindings, and
source unit.

## Append-only snapshot facts

All identities are sixteen exact bytes in network order. They are not UUIDs.
The `...NN` suffix means fifteen zero bytes then the single byte `NN`.

| Fact | Exact value |
| --- | --- |
| Standard version | `orna.std/3` |
| Language version | `orna.language/1` |
| `StandardLibraryRevisionId` | `...03` |
| Standard `CatalogueRevisionId` | `...03` |
| Standard `SourceBundleId` | `...03` |
| Standard `SourceRevisionId` | `...03` |
| Source-revision parent | V2 source revision `...02` |
| `std/types.orna` `SourceUnitId` | `...02` |
| `std/invoke.orna` `SourceUnitId` | `...03` |
| `std/output.orna` `SourceUnitId` | `...04` |
| `std.terminal` `SchemaId` | `...04` |
| `std.io` `SchemaId` | `...05` |
| `std.terminal.Document` `TypeId` | `...15` |
| `std.io.ByteStream` `TypeId` | `...16` |

The type identities continue the reserved standard sequence after
`std.types.opaque_token` (`...14`). They do not collide with the sealed
`sys.invoke` carrier types (`...f0`, `...f1`, `...f2`, work ADR 0053).

The ordered source bundle is exact:

| Ordinal | Source unit | Unit identity | Required content |
| --- | --- | --- | --- |
| `0` | `std/types.orna` | `...02` | the retained `orna.std/1` type declarations, byte-for-byte |
| `1` | `std/invoke.orna` | `...03` | the exact `std.invoke.echo` declaration from `orna.std/2`, byte-for-byte |
| `2` | `std/output.orna` | `...04` | the exact declarations below |

The order is part of the `orna.std/3` source-bundle digest. A reordered,
missing, additional, duplicate, renamed, or byte-modified unit is not
`orna.std/3`.

## `std/output.orna`

`stdlib/std/output.orna` contains exactly this source after its final
newline:

```sql
CREATE SCHEMA std.terminal;
CREATE SCHEMA std.io;

CREATE TYPE std.terminal.Document AS VALUE OPAQUE
    KERNEL CONTRACT 'orna.std.value.terminal-document@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.terminal.Document AS std.Document;

CREATE TYPE std.io.ByteStream AS VALUE OPAQUE
    KERNEL CONTRACT 'orna.std.value.byte-stream@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.io.ByteStream AS std.ByteStream;
```

The unit declares exactly two schemas, two opaque value types, and two
qualified type exports, and nothing else. The semantic names in the
`orna.std/3` catalogue are `std.terminal.document` and `std.io.bytestream`
(lower case, matching the existing standard manifest convention); the source
spellings are case-insensitive. The two qualified bindings are `std.document`
(target `...15`) and `std.bytestream` (target `...16`), following the
`std.opaque_token` binding pattern. Both binding identities are derived with
the work ADR 0016 formula (`SHA-256` over `ornadb.id/type-binding/v1\0`, the
kind byte `01`, and the normalised name payload):

| Binding | `TypeBindingId` |
| --- | --- |
| `std.document` | `7c 56 94 ea c9 66 7c 31 a9 18 fb 3e e2 83 8a 93` |
| `std.bytestream` | `51 b4 58 49 c0 d8 41 bc d9 e6 00 c5 30 cd 38 ea` |

## Canonical payloads and codec registrations

`std.terminal.Document` has the canonical payload

```text
ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8 bytes>
```

whose exact byte layout is the ASCII magic `ORNA-TERMINAL-DOCUMENT/1 `
(includes the separating space), a big-endian `u32` body length, then exactly
that many UTF-8 bytes and no trailing bytes. The body is plain text with
`\n` line separators and a final newline and carries no control codes; the
codec rejects a body that is not valid UTF-8.

`std.io.ByteStream` has the canonical payload

```text
ORNA-BYTE-STREAM/1 <media-type-len:u32 be> <media-type> <len:u32 be> <bytes>
```

whose exact byte layout is the ASCII magic `ORNA-BYTE-STREAM/1 `, a
big-endian `u32` media-type length, the non-empty media-type bytes, a
big-endian `u32` body length, then exactly that many bytes and no trailing
bytes.

Both codecs are registered in `orna_standard::registered_opaque_codecs`
bound to the exact verified `orna.std/3` snapshot, alongside the retained
`std.types.opaque_token` fixed-length codec. The `OpaqueCodecRegistration`
model in `orna-core` gains two framed constructors for these payloads:
`length_prefixed_utf8` (magic, length prefix, UTF-8 body) and
`media_type_framed` (magic, media-type length, media type, length, body).

## Digest contract

`orna.std/3` reuses `StandardLibraryDigestVersion::Version2` and its exact
digest domain (`ornadb.hash/standard-library/v2\0`). The V3 digest therefore
encodes the version number `2` followed by the new revision identities, the
new source revision, and the complete V3 catalogue, executable, reference,
and origin facts in the ADR 0055 order. The digest contract in `orna-core` is
append-only; a new `orna.std/3`-specific digest domain is a later
standard-upgrade decision and is not required to register the value types.
All seven V3 digest goldens (three unit content digests, the bundle,
source-revision, standard, and shared artifact/semantic digests) are computed
by the canonical encoders from the retained source and canonical records;
tests recompute every value. The V1 and V2 goldens do not change.

## Upgrade path

This decision registers the value types and their codecs. It does not install
`orna.std/3` into a database: work ADR 0055 defers upgrades after `orna.std/2`
to a later standard-upgrade decision, and the compiler-backed install
pipeline is V1-to-V2 shaped. `prepare_standard_upgrade_v2_to_v3` is the
fail-closed seam that retains and verifies the V3 snapshot and then reports
that the V2-to-V3 install is not yet supported by this build.

## Precedence

Work ADR 0055 remains authoritative for `orna.std/2` immutability and the
upgrade authority rule. Work ADR 0057 remains authoritative for the value
types, canonical payloads, and codec registration requirement. This decision
adds only the `orna.std/3` vehicle that carries those types.
