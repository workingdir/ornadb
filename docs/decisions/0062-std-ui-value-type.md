# ADR 0062: `std.ui.UI` Value Type and Runtime Contracts

**Status:** Accepted

## Decision

`orna.std/4` is the append-only child standard snapshot of `orna.std/3`
(work ADR 0058). It registers the standard-library UI value type
`std.ui.UI` as a `VALUE OPAQUE IMMUTABLE TRANSIENT` type with stable
manifest identities, one new retained source unit (`std/ui.orna`), and a
codec registration in `orna_standard::registered_opaque_codecs`.

`orna.std/1`, `orna.std/2`, and `orna.std/3` keep their identities,
retained source bytes, digest contracts, verifiers, and goldens
byte-for-byte. The new snapshot is purely additive: it reuses the V3
catalogue, the `std.invoke.echo` executable, and the two output value
types unchanged, and appends the new schema, value type, binding, and
source unit.

`std.ui.UI` is a transient immutable value, never a durable object. The
core remains UI-ignorant; it sees only a standard `TypeId`, an opaque
value type, and the value codec (spec ADR 0012, spec docs/10-ui-type.md).
`CREATE EXTERNAL CLIENT FUNCTION` declarations with `RUNTIME CONTRACT`
clauses are a separate later decision (work ADR 0063); this decision only
registers the type the external functions return.

## Append-only snapshot facts

All identities are sixteen exact bytes in network order. They are not
UUIDs. The `...NN` suffix means fifteen zero bytes then the single byte
`NN`. Every value below is read from the pinned manifest definitions, not
guessed.

| Fact | Exact value |
| --- | --- |
| Standard version | `orna.std/4` |
| Language version | `orna.language/1` |
| `StandardLibraryRevisionId` | `...04` |
| Standard `CatalogueRevisionId` | `...04` |
| Standard `SourceBundleId` | `...04` |
| Standard `SourceRevisionId` | `...04` |
| Source-revision parent | V3 source revision `...03` |
| `std/types.orna` `SourceUnitId` | `...02` |
| `std/invoke.orna` `SourceUnitId` | `...03` |
| `std/output.orna` `SourceUnitId` | `...04` |
| `std/ui.orna` `SourceUnitId` | `...05` |
| `std.terminal` `SchemaId` | `...04` |
| `std.io` `SchemaId` | `...05` |
| `std.io` `SchemaId` | `...05` |
| `std.json` `SchemaId` | `...06` |
| `std.data` `SchemaId` | `...07` |
| `std.ui` `SchemaId` | `...08` |
| `std.terminal.Document` `TypeId` | `...15` |
| `std.io.ByteStream` `TypeId` | `...16` |
| `std.json.Value` `TypeId` | `...17` |
| `std.data.Rows` `TypeId` | `...18` |
| `std.ui.UI` `TypeId` | `...19` |
| `std.ui.UI` kernel contract | `orna.std.value.ui@1` |

The identity values above are the V1-V3 manifest facts plus one new
`...05` source unit, one new `...08` schema, and the next free reserved
type identity `...19`. The reserved sequence after
`std.types.opaque_token` (`...14`) is `Document` `...15`, `ByteStream`
`...16`; work ADR 0057 already claims `...17` (`std.json.Value`) and
`...18` (`std.data.Rows`) in the compiler resolver model, so `...19` is
the next unattributed byte. `...19` is not claimed by any other
identity kind or crate; the resolver model verifies the invariant
during the V4 reconcile tests.

The ordered source bundle is exact:

| Ordinal | Source unit | Unit identity | Required content |
| --- | --- | --- | --- |
| `0` | `std/types.orna` | `...02` | the retained `orna.std/1` type declarations, byte-for-byte |
| `1` | `std/invoke.orna` | `...03` | the exact `std.invoke.echo` declaration from `orna.std/2`, byte-for-byte |
| `2` | `std/output.orna` | `...04` | the exact V3 output declarations, byte-for-byte |
| `3` | `std/ui.orna` | `...05` | the exact declarations below |

The order is part of the `orna.std/4` source-bundle digest. A reordered,
missing, additional, duplicate, renamed, or byte-modified unit is not
`orna.std/4`.

## `std/ui.orna`

`stdlib/std/ui.orna` contains exactly this source after its final
newline:

```sql
CREATE SCHEMA std.ui;

CREATE TYPE std.ui.UI AS VALUE
    OPAQUE
    KERNEL CONTRACT 'orna.std.value.ui@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.ui.UI AS std.UI;
```

The unit declares exactly one schema, one opaque value type, and one
qualified export, and nothing else. The semantic name in the `orna.std/4`
catalogue is `std.ui.ui` (lower case, matching the existing standard
manifest convention); the source spelling is case-insensitive. The
qualified binding is `std.UI` (target `...19`), following the
`std.Document` / `std.ByteStream` export pattern.

The type carries the `KERNEL CONTRACT 'orna.std.value.ui@1'` exactly
like the existing opaque value types. The compiler's standard source
check (`check_standard_library_source`) requires the declared kernel
contract to equal the catalogue `representation_contract` for every
opaque type (verified at orna-compiler resolver.rs:760-781, applied to
Document/ByteStream), and `registered_opaque_codecs` binds each opaque
type through that same contract. A `KERNEL CONTRACT` is therefore
required for the type to be installed with an executable representation.

## Canonical payload and codec registration

This slice registers a codec because `OpaqueCodecRegistration` and the
standard manifest require a closed payload contract to register the type
at the Rust boundary. The byte layout below is a provisional
implementation decision, not a spec mandate: the spec deliberately leaves
the UI transport open ("settle after the first Qt/TTY prototype"). The
provisional frame is:

```text
ORNA-UI/1 <len:u32 be> <utf-8 bytes>
```

whose exact byte layout is the ASCII magic `ORNA-UI/1 ` (includes the
separating space), a big-endian `u32` body length, then exactly that many
UTF-8 bytes and no trailing bytes. The body is the canonical JSON of the
UI value (spec `std-ui-value-v1.schema.json` diagnostic framing): one
object that is exactly one of `empty`, `fragment`, or `node`, with the
closed field shapes in that schema. The schema is stable for this
snapshot.

The registered codec reuses the existing
`OpaqueCodecRegistration::length_prefixed_utf8` constructor (verified at
orna-core value.rs:1023) with magic `ORNA-UI/1 `. No new codec API is
introduced in this slice; if a future ABI replaces the representation,
the new codec is a separate decision with a new contract version. This
provisional codec is versioned by the magic (`/1`), so a later compact
typed codec can coexist under a new magic.

The V4 registration set in `registered_opaque_codecs` is the V3 set plus
the UI registration. The V3 set (opaque token, terminal document, byte
stream) is unchanged.

## Digest contract

`orna.std/4` reuses `StandardLibraryDigestVersion::Version2` and its exact
digest domain, exactly as `orna.std/3` does. The digest encodes the
version number followed by the new revision identities, the new source
revision, and the complete V4 catalogue, executable, reference, and
origin facts in the ADR 0055 order. The V4 digest goldens are computed by
the canonical encoders; tests recompute every value. The V1-V3 goldens do
not change.

## Upgrade path

`prepare_standard_upgrade_v3_to_v4` is the accepted seam from an
installed `orna.std/3` to the retained V4 snapshot, mirroring
`prepare_standard_upgrade_v2_to_v3` (orna-standard v0:1301). The
compiler-backed install pipeline admits the append-only standard child
edge (work ADRs 0059, 0061 step 4), so an installed V3 database upgrades
to V4 through the same apply path when the server release activates it.

## Open decisions (not invented here)

- The transport ABI (`spec/orna_runtime_abi_v1.h`) defines the native
  runtime boundary and `OrnaValueRefV1`; the spec explicitly leaves the
  canonical encoding open ("settle after first Qt/TTY prototype"). This
  slice registers the canonical JSON frame above because the existing
  codec constructors and the standard manifest require a closed payload
  to register; the runtime ABI slice may replace the frame with a compact
  typed codec under a new contract version.
- `CREATE EXTERNAL CLIENT FUNCTION std.ui.window (...) RETURNS std.ui.UI
  RUNTIME CONTRACT 'std.ui.window@1'` (spec ui-runtime philosophy) is
  accepted as the later contract surface. The current parser has no
  RUNTIME CONTRACT clause and CLIENT bodies accept only a Boolean
  literal; those prerequisites (syntax, compiler, and the client
  expression path) are tracked as the next steps, not this slice.

## Precedence

Work ADR 0055 remains authoritative for `orna.std/2` immutability and the
upgrade authority rule. Work ADR 0058 remains authoritative for the V3
output value types and codec registration. Work ADR 0059 installs V3 via
the compiler-backed pipeline; this decision adds only the V4 vehicle that
carries `std.ui.UI`. Spec ADR 0012 and spec docs/10-ui.md remain
authoritative for the UI-is-a-standard-value-type model.