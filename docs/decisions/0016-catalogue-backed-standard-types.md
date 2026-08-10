# ADR 0016: Standard Scalars Are Catalogue-Backed `std` Value Types

**Status:** Accepted

## Decision

Every Orna standard scalar is a real value-type definition in the standard
library catalogue. A standard scalar is not defined only by a compiler enum,
parser branch, PostgreSQL type, or runtime value variant.

The standard library owns one canonical type definition and one stable
`TypeId` for each scalar. Other accepted names are bindings to that identity.
For Boolean, the public naming relationship is:

```text
std.types.boolean   canonical value-type definition
std.boolean         standard-library namespace binding
BOOLEAN             standard-prelude binding
BOOL                standard-prelude binding
```

Unquoted source remains case-insensitive, so source may write
`std.types.BOOLEAN` and `std.BOOLEAN`. The resolved semantic names are
normalised as shown above. A quoted unqualified spelling does not invoke the
standard prelude. A quoted part in a qualified name uses normal exact catalogue
lookup, so `std."boolean"` resolves while `std."BOOLEAN"` does not.

All four Boolean names resolve directly to the existing Boolean `TypeId` whose
bytes end in `01`. They do not form a runtime alias chain. A binding does not
receive another `TypeId`, and resolving a binding never changes assignment,
equality, hashing, storage, function-signature, or result identity.

In this decision, **catalogue-backed** means that the type has a durable,
hash-verified standard-library definition and source origin. It does not mean
that PostgreSQL creates one relation for each value type. PostgreSQL remains a
private storage implementation.

## Standard type names and identities

The initial standard library retains the existing thirteen reserved type
identities:

| Suffix | Canonical definition | `std` binding | Prelude spelling or spellings | Kernel contract |
| --- | --- | --- | --- | --- |
| `01` | `std.types.boolean` | `std.boolean` | `BOOLEAN`, `BOOL` | `orna.kernel.value.boolean@1` |
| `02` | `std.types.integer` | `std.integer` | `INTEGER`, `INT` | `orna.kernel.value.integer@1` |
| `03` | `std.types.bigint` | `std.bigint` | `BIGINT` | `orna.kernel.value.bigint@1` |
| `04` | `std.types.float` | `std.float` | `FLOAT` | `orna.kernel.value.float@1` |
| `05` | `std.types.decimal` | `std.decimal` | `DECIMAL` | `orna.kernel.value.decimal@1` |
| `06` | `std.types.character_large_object` | `std.character_large_object` | `CHARACTER LARGE OBJECT`, `TEXT` | `orna.kernel.value.character-large-object@1` |
| `07` | `std.types.binary_large_object` | `std.binary_large_object` | `BINARY LARGE OBJECT`, `BYTES` | `orna.kernel.value.binary-large-object@1` |
| `08` | `std.types.uuid` | `std.uuid` | `UUID` | `orna.kernel.value.uuid@1` |
| `09` | `std.types.date` | `std.date` | `DATE` | `orna.kernel.value.date@1` |
| `0a` | `std.types.time` | `std.time` | `TIME` | `orna.kernel.value.time@1` |
| `0b` | `std.types.timestamp` | `std.timestamp` | `TIMESTAMP` | `orna.kernel.value.timestamp@1` |
| `0c` | `std.types.duration` | `std.duration` | `DURATION` | `orna.kernel.value.duration@1` |
| `0d` | `std.types.void` | `std.void` | `VOID` | `orna.kernel.value.void@1` |

The complete sixteen-byte `TypeId` values remain the current values: fifteen
zero bytes followed by the suffix above. Existing databases, artefacts, and
values must not allocate replacements for them.

The first twelve definitions are `PERSISTABLE`. `std.types.void` is
`TRANSIENT`, has no runtime value or storage representation, and remains valid
only where an accepted function contract permits `VOID`. This record preserves
that identity but does not expand the current `VOID` surface.

The canonical `std.types.*` name identifies the definition. The `std.*`
binding provides the ordinary qualified standard-library name. Prelude
bindings provide the SQL-family public spelling. Diagnostic rendering may use
the established uppercase prelude spelling, but semantic comparison always
uses the `TypeId`.

The standard library stores every binding with a stable `TypeBindingId`, its
source origin, and the resolved target `TypeId`. A binding identity is not a
type identity. The standard library rejects a missing target, a target outside
the same verified standard revision, duplicate normalised names, cycles, and
a binding that tries to create a second `TypeId`. Although the standard source
is readable, callers do not traverse bindings during normal resolution.

## Type categories

Orna has two identity categories for user-visible types.

An **object type** has a stable `TypeId`, durable object instances with
`ObjectId` identities, fields, and reference participation. `REF object_type`
refers to an object identity.

A **value type** has a stable `TypeId`, but its values have no inherent
`ObjectId`. Values are passed, returned, compared, encoded, and, when the type
permits it, stored by value. Standard scalars, enums, immutable records, domain
types, and opaque runtime values all belong to this category.

A **type binding** gives another source name to an existing `TypeId`. It is not
a third type category. In particular, an alias is not a nominal newtype.

A future user-defined nominal value type receives a new `TypeId`, even when
its representation is based on a standard type. An explicitly declared alias
retains the target `TypeId`. This distinction prevents an email address
newtype, for example, from being silently interchangeable with text merely
because both use the same private storage representation.

Object and value definitions share one owner-qualified type namespace and one
`TypeId` identity class. A primary type name, alias, object definition, and
value definition cannot occupy the same normalised name in one catalogue
view.

## Standard-library source

The standard definitions are implemented by these exact canonical retained
standard-library source bytes. The retained bytes are not authority. Only a
`VerifiedStandardLibrarySnapshot` grants standard-library authority. The source
is UTF-8 with no BOM,
contains ASCII bytes and LF line endings only, contains no CR byte, and has
exactly one final LF byte. Its declarations are in the stated source order:
two schemas, then the thirteen types in manifest and `TypeId` order, followed
by each type's qualified export and its manifest-order prelude exports.

```sql
CREATE SCHEMA std;
CREATE SCHEMA std.types;

CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.boolean@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;

EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;
EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;

CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.integer@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.INTEGER AS std.INTEGER;

EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;
EXPORT TYPE std.INTEGER TO PRELUDE AS INT;

CREATE TYPE std.types.BIGINT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.bigint@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.BIGINT AS std.BIGINT;

EXPORT TYPE std.BIGINT TO PRELUDE AS BIGINT;

CREATE TYPE std.types.FLOAT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.float@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.FLOAT AS std.FLOAT;

EXPORT TYPE std.FLOAT TO PRELUDE AS FLOAT;

CREATE TYPE std.types.DECIMAL AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.decimal@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.DECIMAL AS std.DECIMAL;

EXPORT TYPE std.DECIMAL TO PRELUDE AS DECIMAL;

CREATE TYPE std.types.CHARACTER_LARGE_OBJECT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.character-large-object@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.CHARACTER_LARGE_OBJECT AS std.CHARACTER_LARGE_OBJECT;

EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS CHARACTER LARGE OBJECT;
EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS TEXT;

CREATE TYPE std.types.BINARY_LARGE_OBJECT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.binary-large-object@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.BINARY_LARGE_OBJECT AS std.BINARY_LARGE_OBJECT;

EXPORT TYPE std.BINARY_LARGE_OBJECT TO PRELUDE AS BINARY LARGE OBJECT;
EXPORT TYPE std.BINARY_LARGE_OBJECT TO PRELUDE AS BYTES;

CREATE TYPE std.types.UUID AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.uuid@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.UUID AS std.UUID;

EXPORT TYPE std.UUID TO PRELUDE AS UUID;

CREATE TYPE std.types.DATE AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.date@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.DATE AS std.DATE;

EXPORT TYPE std.DATE TO PRELUDE AS DATE;

CREATE TYPE std.types.TIME AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.time@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.TIME AS std.TIME;

EXPORT TYPE std.TIME TO PRELUDE AS TIME;

CREATE TYPE std.types.TIMESTAMP AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.timestamp@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.TIMESTAMP AS std.TIMESTAMP;

EXPORT TYPE std.TIMESTAMP TO PRELUDE AS TIMESTAMP;

CREATE TYPE std.types.DURATION AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.duration@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.DURATION AS std.DURATION;

EXPORT TYPE std.DURATION TO PRELUDE AS DURATION;

CREATE TYPE std.types.VOID AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.void@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.types.VOID AS std.VOID;

EXPORT TYPE std.VOID TO PRELUDE AS VOID;
```

The fenced source is exactly 3273 bytes and contains exactly 45 semicolons.
The accepted value
`e844ebda2f3de385a9f7f193021bc1abbd4863d61fe6af7bef43ed4e60f92fea`
is its canonical source-unit content digest, not a raw file SHA-256. Compute
that digest with the existing `ornadb.hash/source-unit-content/v1\0` domain
bytes, the big-endian `u32` byte length `3273`, and the exact source bytes.

The 45 source origins cover the complete declarations, including each final
semicolon, in this source order. The final LF at byte `3272` is outside every
declaration origin.

| Ordinal | Declaration | Span |
| --- | --- | --- |
| 0 | `CREATE SCHEMA std` | `0..18` |
| 1 | `CREATE SCHEMA std.types` | `19..43` |
| 2 | `CREATE TYPE std.types.BOOLEAN` | `45..174` |
| 3 | `EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN` | `176..221` |
| 4 | `EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN` | `223..269` |
| 5 | `EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL` | `270..313` |
| 6 | `CREATE TYPE std.types.INTEGER` | `315..444` |
| 7 | `EXPORT TYPE std.types.INTEGER AS std.INTEGER` | `446..491` |
| 8 | `EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER` | `493..539` |
| 9 | `EXPORT TYPE std.INTEGER TO PRELUDE AS INT` | `540..582` |
| 10 | `CREATE TYPE std.types.BIGINT` | `584..711` |
| 11 | `EXPORT TYPE std.types.BIGINT AS std.BIGINT` | `713..756` |
| 12 | `EXPORT TYPE std.BIGINT TO PRELUDE AS BIGINT` | `758..802` |
| 13 | `CREATE TYPE std.types.FLOAT` | `804..929` |
| 14 | `EXPORT TYPE std.types.FLOAT AS std.FLOAT` | `931..972` |
| 15 | `EXPORT TYPE std.FLOAT TO PRELUDE AS FLOAT` | `974..1016` |
| 16 | `CREATE TYPE std.types.DECIMAL` | `1018..1147` |
| 17 | `EXPORT TYPE std.types.DECIMAL AS std.DECIMAL` | `1149..1194` |
| 18 | `EXPORT TYPE std.DECIMAL TO PRELUDE AS DECIMAL` | `1196..1242` |
| 19 | `CREATE TYPE std.types.CHARACTER_LARGE_OBJECT` | `1244..1403` |
| 20 | `EXPORT TYPE std.types.CHARACTER_LARGE_OBJECT AS std.CHARACTER_LARGE_OBJECT` | `1405..1480` |
| 21 | `EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS CHARACTER LARGE OBJECT` | `1482..1558` |
| 22 | `EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS TEXT` | `1559..1617` |
| 23 | `CREATE TYPE std.types.BINARY_LARGE_OBJECT` | `1619..1772` |
| 24 | `EXPORT TYPE std.types.BINARY_LARGE_OBJECT AS std.BINARY_LARGE_OBJECT` | `1774..1843` |
| 25 | `EXPORT TYPE std.BINARY_LARGE_OBJECT TO PRELUDE AS BINARY LARGE OBJECT` | `1845..1915` |
| 26 | `EXPORT TYPE std.BINARY_LARGE_OBJECT TO PRELUDE AS BYTES` | `1916..1972` |
| 27 | `CREATE TYPE std.types.UUID` | `1974..2097` |
| 28 | `EXPORT TYPE std.types.UUID AS std.UUID` | `2099..2138` |
| 29 | `EXPORT TYPE std.UUID TO PRELUDE AS UUID` | `2140..2180` |
| 30 | `CREATE TYPE std.types.DATE` | `2182..2305` |
| 31 | `EXPORT TYPE std.types.DATE AS std.DATE` | `2307..2346` |
| 32 | `EXPORT TYPE std.DATE TO PRELUDE AS DATE` | `2348..2388` |
| 33 | `CREATE TYPE std.types.TIME` | `2390..2513` |
| 34 | `EXPORT TYPE std.types.TIME AS std.TIME` | `2515..2554` |
| 35 | `EXPORT TYPE std.TIME TO PRELUDE AS TIME` | `2556..2596` |
| 36 | `CREATE TYPE std.types.TIMESTAMP` | `2598..2731` |
| 37 | `EXPORT TYPE std.types.TIMESTAMP AS std.TIMESTAMP` | `2733..2782` |
| 38 | `EXPORT TYPE std.TIMESTAMP TO PRELUDE AS TIMESTAMP` | `2784..2834` |
| 39 | `CREATE TYPE std.types.DURATION` | `2836..2967` |
| 40 | `EXPORT TYPE std.types.DURATION AS std.DURATION` | `2969..3016` |
| 41 | `EXPORT TYPE std.DURATION TO PRELUDE AS DURATION` | `3018..3066` |
| 42 | `CREATE TYPE std.types.VOID` | `3068..3189` |
| 43 | `EXPORT TYPE std.types.VOID AS std.VOID` | `3191..3230` |
| 44 | `EXPORT TYPE std.VOID TO PRELUDE AS VOID` | `3232..3272` |

This decision accepts that closed declaration family for the standard
library. `AS VALUE PRIMITIVE` creates a primitive value type. `KERNEL CONTRACT`
identifies its irreducible representation and operations. `EXPORT TYPE ... AS`
creates a qualified binding. `EXPORT TYPE ... TO PRELUDE` creates an
unqualified public binding. Preparation flattens both export forms to the
primary definition's `TypeId`.

`KERNEL CONTRACT`, mutation of `std`, and both export forms are privileged
standard-library operations. Ordinary application source cannot declare a
kernel contract, create or replace a definition in `std`, export a qualified
or prelude binding, or change a reserved standard `TypeId`. The compiler
reports a human-readable `ORNA0303` diagnostic at the reserved declaration or
modifier before preparation or storage changes.

The standard source uses no PostgreSQL type name. A contract identifier is an
Orna kernel ABI name, not a backend name or a dynamically loaded host symbol.
Version 1 admits only the exact embedded allow-list and exact standard
declaration-to-contract mapping. A contract cannot be selected by untrusted
application source.

## Protected-source diagnostics

The parser retains the privileged declaration forms losslessly for standard
source and application diagnostics. Normal application checking rejects the
protected surface before identity allocation or preparation. It emits one
`ORNA0303` diagnostic at the complete offending name or modifier with this
exact public text:

| Attempt | Message |
| --- | --- |
| Declare or replace any name whose first semantic part is `std` | `the std namespace is owned by the standard library` |
| Use `KERNEL CONTRACT` outside verified standard source | `KERNEL CONTRACT is available only to the standard library` |
| Export a type to the prelude outside verified standard source | `only the standard library can export a type to the prelude` |
| Export a type as a qualified binding outside verified standard source | `qualified type exports are available only to the standard library` |

Normal syntax errors still take precedence over these semantic diagnostics.
After syntax succeeds, the compiler reports a reserved `std` owner first, a
kernel contract second, a qualified type export third, and a prelude export
fourth. The qualified-export diagnostic covers the complete qualified target
name after `AS`. The compiler does not emit later protection diagnostics for
the same declaration after the first one.
Any diagnostic rejects the complete candidate and no standard or application
catalogue state changes.

The compiler accepts no caller-provided standard `TypeId` or installable
standard-upgrade value. It accepts only a `CheckedStandardLibrary` and returns
an opaque prepared capability. A malformed private value therefore cannot
replace a reserved type identity through a public compiler path.

An existing version-1 application catalogue may predate the protected
namespace. Standard installation does not reinterpret or overwrite it. If an
application definition already has `std` as its first semantic name part,
`prepare_checked_standard_upgrade` returns
`PrepareStandardUpgradeError::NamespaceOccupied`, displayed as
`the application catalogue already uses the reserved std namespace`. If an
application identity collides with any reserved manifest identity, it returns
`PrepareStandardUpgradeError::ReservedIdentity`, displayed as
`the application state conflicts with a reserved standard library identity`.
The public `orna-standard` wrapper returns these compiler failures as its
transparent `StandardUpgradeError::Prepare` source. Both failures occur before
an upgrade value or durable write exists. This decision does not invent an
automatic rename for either conflict.

## Bootstrap and revision authority

The standard library has its own immutable, versioned revision, initially
`orna.std/1`. The database pins one exact `StandardLibraryRevisionId`. That
revision contains:

* the exact lossless standard source bundle;
* the fixed manifest that maps its declarations to reserved `TypeId` values;
* canonical value-type definitions;
* qualified and prelude type bindings;
* exact source origins and resolved binding targets;
* one canonical standard-library digest; and
* the exact association between `orna.language/1` and `orna.std/1`.

The initial bootstrap manifest is the smallest source-independent trusted
input needed to compile the source that defines the primitive types. It
contains the exact reserved IDs, 13 primary names, 13 allow-listed versioned
kernel contracts, and 30 direct binding facts: 13 qualified `std.*` bindings
plus 17 prelude bindings (`13 + 17 = 30`). It does not yet contain source
bytes, source origins, hashes, a standard digest, or a
`StandardLibrarySnapshot` or `VerifiedStandardLibrarySnapshot`. It stages
identity and contract expectations without providing a second set of public
type semantics or claiming that source-derived facts have already been
verified.

### Canonical standard identities

`orna.std/1` uses these exact sixteen-byte identities, written as fifteen zero
bytes followed by the listed byte:

| Identity class | Member | Final byte |
| --- | --- | --- |
| `StandardLibraryRevisionId` | `orna.std/1` | `01` |
| `CatalogueRevisionId` | standard catalogue revision | `01` |
| `SourceBundleId` | standard source bundle | `01` |
| `SourceRevisionId` | standard source revision | `01` |
| `SourceUnitId` | `std/types.orna` | `01` |
| `SchemaId` | `std` | `01` |
| `SchemaId` | `std.types` | `02` |

The identity classes are distinct Rust and durable types, so equal byte arrays
do not cross identity domains. Reuse of the listed `01` bytes across different
identity classes is intentional. Within one identity class, every reserved
manifest member must remain unique. Manifest construction rejects any
collision in the same identity class, including a derived `TypeBindingId`
collision. Collision checks never compare identities from different classes.
Installation and recovery compare application identities only with reserved
manifest identities of the same class and reject equality. In particular,
standard installation rejects an application catalogue whose
`CatalogueRevisionId` equals the reserved standard catalogue identity before
creating an upgrade. Recovery requires the stored standard catalogue to use
the reserved identity and rejects an application collision in the same
identity class or a crossed catalogue link instead of interpreting either
record through the other role.

The standard source revision has no parent. Its one source unit has ordinal
`0`, logical path `std/types.orna`, and the exact embedded UTF-8 content. Its
content, bundle, and revision hashes use the existing source hash contracts
without a special standard-library branch.

Retaining that source is the first stage that owns provenance and canonical
verification. It derives exactly forty-five source origins: two schemas,
thirteen value types, and thirty bindings (`2 + 13 + 30 = 45`). That stage
locks these exact accepted goldens:

| Fact | Digest |
| --- | --- |
| source-unit content | `e844ebda2f3de385a9f7f193021bc1abbd4863d61fe6af7bef43ed4e60f92fea` |
| source bundle | `f30293aa3c4068e2cb4e19b815ae5077338931562af0ee1cd444e9b0b4e08616` |
| source revision | `0f64a10ec8e620c0bddf402cc1d25c16aa847c48fb6a0af7367f8e76b283f01c` |
| standard library | `e53c41a35e1a092380188fd20d24b6322ae82c2d50dfb5dd053100b51c3b7e9c` |

### Retained standard-source interface

This stage adds these two public functions to `orna-standard`:

```rust
pub fn retained_standard_library_snapshot() -> Result<StandardLibrarySnapshot, StandardLibraryError>
pub fn verify_standard_library_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError>
```

`StandardLibraryError` is `#[non_exhaustive]` and derives `Clone`, `Debug`,
`Eq`, and `PartialEq`. Retained-source construction and verification construct
exactly these variants and fields:

```rust
Manifest { source: StandardLibraryManifestError }
RetainedSourceMismatch
Revision { source: RevisionInvariantError }
CanonicalHash { source: CanonicalHashError }
CatalogueIdentityMismatch {
    expected: CatalogueRevisionId,
    actual: CatalogueRevisionId,
}
AcceptedDigestMismatch {
    expected: Sha256Digest,
    actual: Sha256Digest,
}
```

Its public `Display` text is exact:

| Variant | Display |
| --- | --- |
| `Manifest` | `the standard library manifest is invalid: {source}` |
| `RetainedSourceMismatch` | `the retained standard library source does not match its manifest` |
| `Revision` | `the retained standard library revision is invalid: {source}` |
| `CanonicalHash` | `the standard library canonical hashes are invalid: {source}` |
| `CatalogueIdentityMismatch` | `the standard library catalogue identity does not match the reserved identity` |
| `AcceptedDigestMismatch` | `the standard library digest does not match the hard-coded accepted digest` |

Its `Error::source` is `Some(source)` only for `Manifest`, `Revision`, and
`CanonicalHash`. It is `None` for the other variants. These are the only six
`StandardLibraryError` variants implemented by the retained-source row.

The later `feat(std): orchestrate standard upgrades` row, in
`crates/orna-standard/src/lib.rs`, extends this non-exhaustive error with only
`Unavailable`. Its display is `the standard library is not installed`. It is
returned only at a service, orchestration, or database boundary when an
installed standard is absent. It is not returned by retained-source construction
or verification.

`StandardLibraryAlreadyInstalled`, `NamespaceOccupied`, and
`ReservedIdentity` are compiler-owned `PrepareStandardUpgradeError` variants.
The standard wrapper returns them through its transparent
`StandardUpgradeError::Prepare` source.
Neither the compiler checker nor `StandardApplicationCheckContext` has an
`Unavailable` error. Later standard-application preparation owns its distinct
`StandardLibraryUnavailable` error.

`retained_standard_library_snapshot` constructs the source unit, bundle, and
parentless source revision under the existing source hash contracts. It parses
the retained bytes directly with `orna_syntax`. It does not call the compiler.
It first maps manifest construction failure to `Manifest`. It maps a source
parse diagnostic; a count other than exactly two schemas, thirteen primitive
value types, thirteen qualified exports, or seventeen prelude exports; a quoted
identifier, name, qualified target, or prelude word; or any source fact that
does not match the manifest to `RetainedSourceMismatch`.

The direct parse must produce exactly two schemas, thirteen primitive value
types, thirteen qualified exports, and seventeen prelude exports. Each
identifier, name, qualified target, and prelude word must be unquoted and match
the source-independent manifest one-for-one. Each `kernel_contract` is an
exact quoted `SourceSlice`; its SQL string-literal content is decoded and
compared to the manifest contract text. Persistence is the parsed keyword enum.
Declaration order is positional and must match the manifest one-for-one. It
must then attach the 45 complete-declaration origins
listed above, assemble the snapshot, and apply the four retained-source
goldens. Source-revision invariant failure maps to `Revision`; canonical hash
failure maps to `CanonicalHash`. The function returns an unverified
`StandardLibrarySnapshot` only after all those checks succeed.

`verify_standard_library_snapshot` applies checks in this exact order:

1. compare the snapshot `CatalogueRevisionId` with the reserved manifest
   identity and return `CatalogueIdentityMismatch` on a difference;
2. compare the snapshot standard digest with the hard-coded accepted digest and
   return `AcceptedDigestMismatch` on a difference; and
3. call `orna_core::canonical_hash::verify_standard_library_snapshot`, mapping
   failure to `CanonicalHash`.

This ordering prevents a different but internally self-consistent standard
snapshot from becoming authority.

Each binding identity is derived rather than allocated. Compute SHA-256 over
the domain bytes `ornadb.id/type-binding/v1\0`, then one binding-kind byte
(`01` qualified, `02` prelude), then its normalised name payload. A qualified
name payload is
encoded as a big-endian `u32` part count followed by each UTF-8 part as a
big-endian `u32` byte length and its bytes. A prelude spelling uses the same
encoding over its lower-case keyword words, so `CHARACTER LARGE OBJECT` is the
three parts `character`, `large`, and `object`. The first sixteen digest bytes
form `TypeBindingId`. Bootstrap rejects a collision between two bindings.

### Trusted compiler standard-source checker

`orna-compiler` adds this exact public seam:

```rust
pub fn check_standard_library_source(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError>
```

The compiler accepts only the core-owned
`VerifiedStandardLibrarySnapshot` capability. It accepts no raw standard
snapshot, manifest, source bundle, boolean trust flag, or equivalent bypass.
`orna-compiler` has no dependency on `orna-standard`. The caller supplies the
verified core carrier after it has completed the applicable standard-library
authority checks.

`StandardLibraryCheckError` is public, `#[non_exhaustive]`, and derives
`Clone`, `Debug`, `Eq`, and `PartialEq`. It has exactly these variants and
fields:

```rust
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StandardLibraryCheckError {
    SourceUnitCount { actual: usize },
    Diagnostics { diagnostics: Vec<CompilerDiagnostic> },
    SourceMismatch,
}
```

Its public `Display` text is exact:

| Variant | Display |
| --- | --- |
| `SourceUnitCount` | `the verified standard library has {actual} source units, expected exactly one` |
| `Diagnostics` | `the verified standard library source has compiler diagnostics` |
| `SourceMismatch` | `the verified standard library source does not match its catalogue and origins` |

`{actual}` is the decimal `usize` value. `std::error::Error::source()` returns
`None` for every variant. No other error variant or implicit conversion is
accepted.

`CheckedStandardLibrary` owns a clone of the verified snapshot and the checked
families. It derives `Clone` and `Debug`. Its public read-only accessors are
`verified_snapshot() -> &VerifiedStandardLibrarySnapshot`,
`schemas() -> &[CheckedStandardSchema]`,
`value_types() -> &[CheckedStandardValueType]`, and
`type_bindings() -> &[CheckedStandardTypeBinding]`. The family accessors return
slices in source order; they do not sort by durable identity. Each checked
family model derives `Clone`, `Debug`, `Eq`, and `PartialEq`.

```rust
#[derive(Clone, Debug)]
pub struct CheckedStandardLibrary { /* private fields */ }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardSchema { /* private fields */ }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardValueType { /* private fields */ }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardTypeBinding { /* private fields */ }
```

| Checked family | Private fields | Read-only accessors |
| --- | --- | --- |
| `CheckedStandardSchema` | `id: SchemaId`, `name: QualifiedSemanticName`, `origin: SourceOrigin` | `id() -> SchemaId`, `name() -> &QualifiedSemanticName`, `origin() -> SourceOrigin` |
| `CheckedStandardValueType` | `id: TypeId`, `name: QualifiedSemanticName`, `kind: ValueTypeKind`, `mutability: ValueTypeMutability`, `persistence: ValueTypePersistence`, `representation_contract: String`, `origin: SourceOrigin` | `id() -> TypeId`, `name() -> &QualifiedSemanticName`, `kind() -> ValueTypeKind`, `mutability() -> ValueTypeMutability`, `persistence() -> ValueTypePersistence`, `representation_contract() -> &str`, `origin() -> SourceOrigin` |
| `CheckedStandardTypeBinding` | `id: TypeBindingId`, `kind: TypeBindingKind`, `name: TypeLookupName`, `target: TypeId`, `origin: SourceOrigin` | `id() -> TypeBindingId`, `kind() -> TypeBindingKind`, `name() -> &TypeLookupName`, `target() -> TypeId`, `origin() -> SourceOrigin` |

Each checked item owns its durable identity, checked catalogue fields, and
exact source origin. This model preserves the catalogue facts and source
origins without allocating identities, preparing a revision, or constructing a
type-use model. Identity, enum, origin, and target accessors return copied
values. Name, contract, snapshot, and family accessors return borrows.

The checker runs these gates in this exact order:

1. Require exactly one stored source unit. If the count differs, return
   `SourceUnitCount { actual }` before parsing or catalogue work.
2. Parse that unit's exact logical path and content exactly once.
3. If parsing reports diagnostics, return
   `Diagnostics { diagnostics }` with the existing exact
   `CompilerDiagnostic` vector before semantic reconciliation.
4. Require lossless parsed text, only schema, primitive-value-type, and
   type-export declaration categories, and the exact schema, primitive,
   qualified-export, and prelude-export family counts from the supplied
   verified catalogue.
5. Reconcile every unquoted source fact one-to-one with the verified
   catalogue. Reconcile schemas; primitive name, `Primitive`, `Immutable`,
   parsed persistence, and decoded contract; qualified-export primary source,
   binding name, kind, and direct target; and prelude-export source qualified
   binding, prelude name, kind, and the same direct target. Consume each
   catalogue fact exactly once. Durable identities come from those matched
   catalogue facts; source-visible facts do not encode them.
6. Key origins by `DefinitionIdentity`. Require the sole stored
   `SourceUnitId` and each exact complete-declaration start and end range for
   every fact. Consume every origin exactly once.
7. Construct `CheckedStandardLibrary` with the cloned verified capability and
   its source-ordered checked schema, value-type, and type-binding families.

Any failure in gates 4 through 6 returns `SourceMismatch`. The checker is a
separate trusted path and must never call ordinary `check_parsed`, directly or
indirectly. It does not call `prepare`, access a database, install a standard
library, resolve a type use, or convert to or from `StandardScalar`.

Ordinary application checking remains separate. Its `ORNA0303` protected-source
diagnostics, their syntax precedence and category order, and its existing
scalar compatibility adapter remain unchanged.

Core canonical verification can verify a self-consistent non-golden snapshot.
This checker therefore proves only source, catalogue, and origin agreement
with the supplied core-verified snapshot and accepts that case when those
facts agree. It does not enforce the accepted `orna.std/1` golden, duplicate
standard-digest verification, or produce directly installable state. Every
production caller must first use
`orna_standard::verify_standard_library_snapshot`. The later
`feat(std): orchestrate standard upgrades` row and its proof own that
integration ordering. This compiler row proves only the absence of an
`orna-standard` dependency and acceptance of a core-verified non-golden
agreement fixture.

### Standard-library digest version 1

The canonical standard digest is SHA-256 over this exact sequence:

1. domain bytes `ornadb.hash/standard-library/v1\0`;
2. big-endian `u32` value `1`;
3. the sixteen `StandardLibraryRevisionId` bytes;
4. the sixteen standard `SourceRevisionId` bytes;
5. the exact 32-byte source revision hash;
6. `orna.language/1` as a big-endian `u32` UTF-8 byte length and bytes;
7. schemas sorted by `SchemaId` bytes, prefixed by a big-endian `u32` count,
   each encoded as its sixteen ID bytes, qualified semantic name, and source
   origin;
8. value types sorted by `TypeId` bytes, prefixed by a big-endian `u32` count,
   each encoded as its sixteen ID bytes, qualified primary name, type-kind byte
   `01` for primitive, mutability byte `01` for immutable, persistence byte
   (`01` persistable, `02` transient), length-prefixed kernel-contract text,
   and source origin; and
9. bindings sorted by `TypeBindingId` bytes, prefixed by a big-endian `u32`
   count, each encoded as its sixteen ID bytes, binding-kind byte, the same
   untagged name payload used for identity derivation, sixteen target `TypeId`
   bytes, and source origin.

The reserved standard `CatalogueRevisionId` is deliberately excluded from
standard digest version 1. It is a manifest identity, not another encoding of
the standard contents. Standard assembly and recovery must compare it with the
exact reserved manifest identity before digest verification begins. The later
`orna_standard::verify_standard_library_snapshot` wrapper performs that
identity check and the hard-coded accepted digest comparison before invoking
the core canonical verifier, so a valid self-consistent digest cannot
legitimise a renamed, crossed, or different non-golden standard snapshot.

A qualified semantic name uses the existing canonical framing: big-endian
`u32` part count followed by length-prefixed UTF-8 parts. A source origin is
sixteen `SourceUnitId` bytes followed by big-endian `u32` start and end byte
offsets. Every sequence length and text length must fit `u32`; overflow is a
typed canonical-hash error. The digest contains no PostgreSQL name, host path,
filesystem metadata, map iteration order, or source spelling outside the exact
source revision hash and origins.

`PostgresKernel::bootstrap` remains the bare kernel operation from the
canonical bootstrap design. It installs private migrations and returns the
recoverable version-1 empty or existing active source/catalogue pair. A bare
database has no standard catalogue and cannot check or execute normal Orna
source. This preserves the explicit diagnostic and recovery environment
allowed by `spec/docs/06-bootstrapping-recovery.md`.

Version-1 recovery has no standard context. The later
`feat(postgres): decode standard revisions` row is the first recovery path that
decodes a raw standard catalogue. It must reject
`EMPTY_APPLICATION_CATALOGUE_REVISION_ID` in that raw standard catalogue with
`RevisionInvariantError::ReservedOfflineCheckCatalogueRevision { role:
ActiveOrRecoveredStandard, .. }`, its exact display, and no error source. The
failure returns no active revision and performs no repair or write.

The `orna-standard` crate owns normal standard installation. Its public
`prepare_standard_upgrade` operation embeds and checks the exact standard
source and manifest, reconstructs the active application source, and returns
one opaque `StandardUpgrade`. The PostgreSQL kernel accepts that type only
through `apply_standard_upgrade`, which verifies and commits the complete value
in one read-write repeatable-read transaction.

The compiler row owns only `crates/orna-compiler/src/prepare.rs` and
`crates/orna-compiler/src/lib.rs`. It defines and re-exports an opaque prepared
capability, not an installable standard upgrade:

```rust
#[derive(Clone, Debug)]
pub struct PreparedStandardUpgrade {
    standard: CheckedStandardLibrary,
    application: DeployableRevision,
}

impl PreparedStandardUpgrade {
    pub fn standard_library(&self) -> &CheckedStandardLibrary;
    pub fn application_revision(&self) -> &DeployableRevision;
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardUpgradeIdentity {
    StandardLibraryRevision(StandardLibraryRevisionId),
    CatalogueRevision(CatalogueRevisionId),
    SourceBundle(SourceBundleId),
    SourceRevision(SourceRevisionId),
    SourceUnit(SourceUnitId),
    Schema(SchemaId),
    Type(TypeId),
    TypeBinding(TypeBindingId),
}

pub fn prepare_checked_standard_upgrade(
    standard: &CheckedStandardLibrary,
    active: &ActiveDatabaseRevision,
) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError>;

#[non_exhaustive]
#[derive(Debug)]
pub enum PrepareStandardUpgradeError {
    StandardLibraryAlreadyInstalled { revision: StandardLibraryRevisionId },
    NamespaceOccupied { name: QualifiedSemanticName },
    ReservedIdentity { identity: StandardUpgradeIdentity },
    ActiveSourceDiagnostics { diagnostics: Vec<CompilerDiagnostic> },
    ActiveSourceMismatch,
    Catalogue { source: CatalogueSnapshotError },
    CanonicalHash { source: CanonicalHashError },
    Revision { source: RevisionInvariantError },
}
```

`PreparedStandardUpgrade` has private fields and no public constructor, owned
extraction, conversion, `Deref`, or `inner` interface. Its
`application_revision()` accessor is intentionally a borrow escape to normal
kernel input. It does not transfer ownership of the candidate or permit direct
standard installation. Its two accessors are the complete public interface.

`StandardUpgradeIdentity` is compiler-owned. It retains the exact conflicting
durable identity in `ReservedIdentity`; it is not a core upgrade model.
`StandardUpgradeIdentity::StandardLibraryRevision` is kernel-only. The
compiler does not return it from `ReservedIdentity`: its first gate rejects an
active standard library before the visible-identity gate starts.

`PrepareStandardUpgradeError` public `Display` text is exact:

| Variant | Display |
| --- | --- |
| `StandardLibraryAlreadyInstalled` | `standard library {revision} is already installed` |
| `NamespaceOccupied` | `the application catalogue already uses the reserved std namespace` |
| `ReservedIdentity` | `the application state conflicts with a reserved standard library identity` |
| `ActiveSourceDiagnostics` | `the active application source has compiler diagnostics` |
| `ActiveSourceMismatch` | `the active application source does not match the active catalogue` |
| `Catalogue` | `the standard upgrade catalogue is invalid: {source}` |
| `CanonicalHash` | `the standard upgrade canonical hashes are invalid: {source}` |
| `Revision` | `the standard upgrade revision is invalid: {source}` |

`Error::source()` is `Some(source)` only for `Catalogue`, `CanonicalHash`, and
`Revision`. It is `None` for every other variant. This compiler path has no
`Unavailable` error and accepts only `CheckedStandardLibrary`.

The active catalogue family order is schemas, object types, value types, type
bindings, then functions. Each family uses snapshot order. The compiler applies
these complete gates in this exact order:

1. reject an active revision that already contains a standard library with
   `StandardLibraryAlreadyInstalled { revision }`;
2. inspect visible active catalogue names in that explicit family and snapshot
   order and reject the first `std` namespace occupant with
   `NamespaceOccupied { name }`;
3. inspect visible active identities in this exact order and reject the first
   match with `ReservedIdentity { identity }`: active catalogue revision,
   source bundle, source revision, source units by ordinal, schemas in snapshot
   order, object `TypeId` values in snapshot order, value `TypeId` values in
   snapshot order, then type bindings in snapshot order;
4. parse the active application source and return its complete ordered vector
   through `ActiveSourceDiagnostics { diagnostics }`;
5. compare that lossless active source with the active catalogue and return
   `ActiveSourceMismatch` on a difference;
6. validate the candidate catalogue and map failure to `Catalogue`;
7. validate its canonical hashes and map failure to `CanonicalHash`; and
8. validate its revision invariants and map failure to `Revision`.

After gate 5 and before candidate catalogue, canonical-hash, or revision
construction, private retry allocation excludes reserved manifest IDs of the
same class only for the newly allocated companion application
`CatalogueRevisionId`, `SourceBundleId`, `SourceRevisionId`, and every copied
`SourceUnitId`. It reuses existing application `SchemaId` and `TypeId` values,
which active gate 3 already covers. It creates no `TypeBindingId`: that
identity is derived from its name, and neither current preparation slice
creates an application type binding. This retry mechanism is private and has
no public error. The active `ReservedIdentity` gate remains before allocation.

The compiler sees only the active revision. Database-wide reserved-identity
collisions in inactive source records belong to the atomic kernel special apply
path, before it writes anything.

The one-file `feat(std): orchestrate standard upgrades` row adds this public
opaque capability and wrapper in `crates/orna-standard/src/lib.rs`:

```rust
pub use orna_compiler::StandardUpgradeIdentity;

#[derive(Clone, Debug)]
pub struct StandardUpgrade {
    prepared: PreparedStandardUpgrade,
}

impl StandardUpgrade {
    pub fn checked_standard_library(&self) -> &CheckedStandardLibrary;
    pub fn verified_standard_snapshot(&self) -> &VerifiedStandardLibrarySnapshot;
    pub fn application_revision(&self) -> &DeployableRevision;
}

pub fn prepare_standard_upgrade(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError>;

#[non_exhaustive]
#[derive(Debug)]
pub enum StandardUpgradeError {
    StandardLibrary { source: StandardLibraryError },
    StandardSource { source: StandardLibraryCheckError },
    Prepare { source: PrepareStandardUpgradeError },
}
```

`StandardUpgrade` has a private field and no public constructor, owned
conversion, `Deref`, or `inner` interface. Only
`prepare_standard_upgrade` constructs it after the retained snapshot,
accepted-golden verification, standard-source check, and compiler preparation
succeed. Its `application_revision()` accessor intentionally retains the
borrow escape to normal kernel input. The permanent normal-apply guard rejects
that borrowed version-2 candidate from a version-1 active revision and locks
it to the same standard context from a version-2 active revision.

`orna-standard` re-exports the compiler-owned `StandardUpgradeIdentity` for
the atomic kernel error. This preserves the exact typed conflicting identity
without a direct `orna-kernel-postgres -> orna-compiler` dependency.

Every `StandardUpgradeError` variant has transparent `Display` text and returns
its contained error from `Error::source()`. The wrapper calls
`retained_standard_library_snapshot`,
`verify_standard_library_snapshot`, `check_standard_library_source`, then
`prepare_checked_standard_upgrade`, in that exact order. It does not call a
raw-standard or unnamed compiler preparation route.

### PostgreSQL standard-context transition guard

Before the standard-upgrade rows, `fix(postgres): guard standard context
transitions` changes only `crates/orna-kernel-postgres/src/apply.rs`,
`crates/orna-kernel-postgres/src/lib.rs`, and
`crates/orna-kernel-postgres/tests/apply.rs`. It defines this public
standard-context value with private fields:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StandardContextIdentity {
    standard_library_revision: StandardLibraryRevisionId,
    standard_catalogue_revision: CatalogueRevisionId,
    source_bundle: SourceBundleId,
    source_revision: SourceRevisionId,
    source_bundle_hash: Sha256Digest,
    source_revision_hash: Sha256Digest,
    standard_library_digest: Sha256Digest,
}

impl StandardContextIdentity {
    pub fn standard_library_revision(&self) -> StandardLibraryRevisionId;
    pub fn standard_catalogue_revision(&self) -> CatalogueRevisionId;
    pub fn source_bundle(&self) -> SourceBundleId;
    pub fn source_revision(&self) -> SourceRevisionId;
    pub fn source_bundle_hash(&self) -> Sha256Digest;
    pub fn source_revision_hash(&self) -> Sha256Digest;
    pub fn standard_library_digest(&self) -> Sha256Digest;
}
```

The guard row adds the first two `PostgresKernelError` variants. The later
atomic special-apply row adds `ReservedStandardIdentity`. All three have no
error source and their displays are static and exact:

```rust
StandardContextTransitionRequired {
    active: CatalogueHashVersion,
    candidate: CatalogueHashVersion,
}
StandardContextMismatch {
    active: StandardContextIdentity,
    candidate: StandardContextIdentity,
}
ReservedStandardIdentity {
    identity: StandardUpgradeIdentity,
}
```

| Variant | Display |
| --- | --- |
| `StandardContextTransitionRequired` | `the active and candidate catalogue hash versions require a standard context transition` |
| `StandardContextMismatch` | `the active and candidate standard contexts do not match` |
| `ReservedStandardIdentity` | `the database contains an identity reserved for the standard library` |

Normal apply performs this permanent guard after expected-base recovery and
before materialisation, planning, or writes. It rejects every version-1 to
version-2 or version-2 to version-1 catalogue-hash transition with
`StandardContextTransitionRequired`. For version-2 to version-2 normal apply,
it reconstructs the two `StandardContextIdentity` values and requires exact
equality, otherwise returning `StandardContextMismatch`. Version-1 to
version-1 normal apply keeps its existing path.

The normal-apply guard makes a borrowed version-2 application candidate
non-installable from a version-1 active revision and context-locked from a
version-2 active revision. The atomic `apply_standard_upgrade` accepts only
`&orna_standard::StandardUpgrade`. It executes these complete steps in order:

1. enter the trusted special-apply path and start its one atomic transaction;
2. lock and recover the active revision;
3. check the expected base;
4. complete the database-wide `ReservedStandardIdentity` gate;
5. materialise the candidate;
6. build the physical plan; and
7. perform writes.

`StandardUpgrade` has a private field and one constructor, so its prepared
association is unforgeable. Compiler construction already proves the
`DeployableRevision` core invariants. The kernel has no additional opaque
association or invariant gate.

The special gate uses identity-class order: standard-library revision,
catalogue revision, source bundle, source revision, source unit, schema, type,
then type binding. It includes `StandardLibraryRevision` first in this
kernel-only scan. For every class, it checks active-visible records first in
the explicit active family order. It then checks all remaining inactive records,
excluding those active IDs, by raw durable ID byte order within that class.
An active standard-library revision, if present, precedes inactive
standard-library revisions; inactive standard-library revisions use raw durable
ID byte order. Active source units use durable ordinal order; inactive source
units use raw durable ID byte order only. `ReservedStandardIdentity` has no
error source.

Replaying the same already-applied `StandardUpgrade` returns the existing
`ExpectedBaseMismatch` at step 3, before the reserved-identity scan or any
writes. Calling `prepare_standard_upgrade` again against an active version-2
revision returns `StandardLibraryAlreadyInstalled` at the compiler's first
gate. The dependency graph is `postgres -> standard -> compiler -> core`.

The companion application source revision retains byte-identical ordered
source-unit paths, contents, content hashes, and ordinals, has the previous
application source as its parent, and receives a new `SourceRevisionId`, new
`SourceBundleId`, and new `SourceUnitId` for every copied unit. It recomputes
the bundle and revision hashes under the existing source contracts. Current
application `DefinitionOrigin` rows are remapped through the old-to-new unit
map without changing byte offsets. New or migrated function revisions and
their references use the remapped origins. Active definition-reference rows
for reused immutable function revisions are also remapped to the corresponding
new units without changing ordinals, kinds, targets, or byte offsets; reference
origins are active-catalogue evidence and are not part of the immutable
function semantic hash. Only a reused immutable function revision's own
declaration origin retains its historical source unit. This preserves global
source identity uniqueness, active reference-origin validation, and the current
one-to-one source/catalogue pair invariant instead of inventing a catalogue
revision without source.
Standard installation is a semantic migration even when the application
source bytes do not change.

A normal fresh-database opener runs bare kernel bootstrap and then the standard
orchestrator before accepting application source. A deliberate recovery tool
may stop at the bare state. On every normal recovery, the kernel reconstructs
and verifies the stored standard revision before it trusts a version-2
application active revision. Missing, duplicated, renamed, crossed, altered,
or hash-inconsistent standard definitions or bindings make recovery fail
closed.

The standard revision is stored once per database. It is not copied into each
application source bundle. Application source remains the complete candidate
snapshot for application-owned schemas under work ADR 0003. Every version-2
active database revision pins the verified standard revision alongside one
application catalogue revision. The effective catalogue view contains both.
Normal service, orchestration, and database operations require that view. At
those boundaries only, the later standard-orchestration extension returns
`orna_standard::StandardLibraryError::Unavailable`, displayed as
`the standard library is not installed`, for a bare version-1 database rather
than falling back to hard-coded names. The compiler has no
unavailable-standard-library error: its standard-backed entry point cannot be
called without an already checked standard-library capability.

`orna.language/1` is paired with `orna.std/1`. Existing executable artefacts
therefore continue to pin the standard scalar semantics without adding the
complete standard source digest to each artefact. A future change to standard
type semantics, contract versions, or bindings requires an accepted standard
library upgrade and language-compatibility rule. Replacing `orna.std/1` in
place is forbidden.

Existing version-1 catalogue and function hashes remain valid historical
records and are never rewritten. The first catalogue-backed standard-type
apply creates a child revision under canonical hash contract version 2.
Version 2 covers the pinned `StandardLibraryRevisionId`, its exact digest,
resolved standard type uses, and all new value-type definition and binding
facts. Recovery reads and verifies version 1 history and version 2 active
state without reinterpreting one as the other.

Catalogue and function revision records retain explicit canonical-hash
versions. Existing records have version `1`. New or migrated records use
version `2` when they contain catalogue-backed value types or value-type
references.

Function semantic version 2 uses domain bytes
`ornadb.hash/function-semantic/v2\0` and otherwise keeps the current field
order. The existing scalar tag and discriminator retain their exact version-1
bytes when an initial compatibility projection represents a reserved standard
type. The required `ValueType` reference records that projection's catalogue
identity in every migrated function. Its resolved-type encoding also reserves
tag `04` followed by sixteen `TypeId` bytes for `Value(TypeId)` once the final
resolved-type migration emits that form. Its reference-target encoding adds tag `06`
followed by sixteen `TypeId` bytes for `ValueType(TypeId)`. Existing tags and
their bytes retain their version-1 meanings. `NamedType` retains reference-kind
tag `02` and may target `ObjectType` in either version or `ValueType` in version
2. The Boolean CLIENT return reference is ordinal `0`, kind `NamedType`, target
`ValueType` with the reserved Boolean `TypeId`, and source origin equal to the
complete written return type.

Canonical definition-identity tags are append-only. Version 2 assigns tag
`08` followed by sixteen `TypeId` bytes to
`DefinitionIdentity::ValueType` and tag `09` followed by sixteen
`TypeBindingId` bytes to `DefinitionIdentity::TypeBinding`. Existing tags
`01` through `07` retain their version-1 meanings. Canonical reference-target
tags are also append-only: `DefinitionReferenceTarget::ValueType` uses tag
`06` followed by sixteen `TypeId` bytes, while existing tags `01` through `05`
retain their meanings.

Catalogue version 1 keeps the current domain bytes
`ornadb.hash/catalogue/v1\0` and exact encoder. Catalogue version 2 uses domain
bytes `ornadb.hash/catalogue/v2\0`, then encodes:

1. big-endian `u32` value `2`;
2. the sixteen pinned `StandardLibraryRevisionId` bytes;
3. the exact 32-byte standard-library digest;
4. application schemas with the existing version-1 schema encoder;
5. application object types with the existing version-1 object encoder;
6. application value types using the standard value-type field encoding above,
   excluding the source origin;
7. application type bindings using the standard binding field encoding above,
   excluding the source origin;
8. functions with the existing function encoder except for version-2 resolved
   types;
9. expressions, current function revisions, definition origins, and definition
   references in their existing version-1 sequence order. Each current
   function-revision entry inserts its big-endian `u32`
   `semantic_hash_version` immediately after the 32 semantic-hash bytes and
   before the language-version text. Definition origins and references use the
   append-only value-type identity and target tags above.

Each sequence retains the existing big-endian `u32` count and canonical sort
rule. Value types sort by `TypeId`; bindings sort by `TypeBindingId`. The
standard digest already covers its ordered definitions and bindings, so the
catalogue encoder does not append a second copy. Version 2 validation computes
each function semantic digest using the version recorded by that immutable
function revision. It can therefore retain an unaffected version-1 function
revision while migrating another function to version 2. A decoder or verifier
must never select an encoding version from the bytes it is trying to validate;
it uses the typed durable version field and rejects an unsupported value.
Application value-type and binding origins appear only in the normal
definition-origin sequence in item 9. Their exclusion from items 6 and 7 avoids
hashing the same fact twice. The standard digest includes standard origins
because standard definitions and bindings are not copied into the application
definition-origin sequence.

The durable catalogue-revision row stores `canonical_hash_version` as a
positive integer column; existing rows are backfilled with `1`, and the first
standard-backed active row stores `2`. A
function-revision row stores the positive `semantic_hash_version`; existing
rows are backfilled with `1`. A standard-library revision row stores positive
`digest_version`, initially `1`. These columns select the encoders described
here. Only their values encoded inside canonical byte streams use the
big-endian `u32` framing stated above.

## Catalogue and semantic model

The public core catalogue gains one `TypeDefinition` family, a value-type
definition model, a type-binding model, and a standard-library snapshot.
Object-specific APIs remain checked projections over definitions whose kind is
object. A value-type definition retains at least:

* its stable `TypeId`;
* canonical owner-qualified name;
* value category and persistence facts;
* versioned representation contract;
* immutable standard source origin; and
* any semantic capabilities required by the accepted language surface.

A type binding retains its `TypeBindingId`, qualified or prelude name, binding
kind, exact target `TypeId`, and standard source origin. Primary definitions
and bindings share one collision-checked type-name view.

Durable identity enums add `DefinitionIdentity::ValueType(TypeId)` and
`DefinitionIdentity::TypeBinding(TypeBindingId)`. Reference targets add
`DefinitionReferenceTarget::ValueType(TypeId)`. Existing `ObjectType` variants
and their version-1 tags remain unchanged. This prevents a value definition
from being represented as an object merely to reuse a target variant.

The standard snapshot indexes primary names and bindings separately, while
both resolve to one `TypeId`. Lookups by identity return the primary
definition, not the spelling used by a caller.

The final resolved type model carries a `TypeId` for every value type and a
typed reference target for object identity:

```text
Value(TypeId)
Reference { target: TypeId }
```

The initial catalogue migration may retain `ResolvedType::Scalar` as an
internal compatibility projection for reserved standard identities while the
ordered `ValueType` evidence carries the durable catalogue dependency. That
projection must be derived from a previously resolved and verified `TypeId`;
it cannot resolve source or establish identity itself.

`StandardScalar` may remain temporarily as the representation code inside that
projection and inside existing version-1 codecs and physical lowering. It is not
the authority for source names, aliases, semantic identity, or catalogue
membership. The compiler must resolve standard spellings through the verified
standard bindings. Kernel representation selection must start from the
resolved `TypeId` and its verified definition before choosing an internal
primitive code.

The compatibility type therefore loses public source-resolution,
`canonical_name`, and `type_id` authority as soon as the standard catalogue is
introduced. Those operations move to the standard definition and binding
view. Matches over the compatibility kind remain permitted only inside exact
version-1 artefact adapters, runtime-value adapters, and backend adapters after
they have validated the originating type definition's contract. Compiler and
catalogue code may not construct semantics from the compatibility kind alone.

The migration may be staged in buildable commits, but no new public seam may
make `StandardScalar` the semantic source of truth. Once all consumers accept
the catalogue identity directly, the compatibility projection can become a
private kernel representation detail.

## Kernel representation contracts

There is an unavoidable trusted implementation beneath a primitive value.
For Boolean, `orna.kernel.value.boolean@1` fixes:

* the two non-null logical values `FALSE` and `TRUE`;
* literal construction and `RuntimeValue::Boolean` validation;
* every already accepted format-specific Boolean encoding, including the
  Boolean byte in `orna.client-plan` version 1;
* equality and any explicitly accepted operations;
* whether the value is persistable; and
* the backend adapter requirements.

The standard Orna declaration gives those semantics a durable public type
identity and name. It does not attempt to implement bit storage or host
arithmetic recursively in terms of itself.

PostgreSQL `BOOLEAN` is the private storage encoding selected by the
PostgreSQL adapter for the verified Boolean kernel contract. Another backend
may use another representation while preserving the same Orna contract.
PostgreSQL casts, collations, ordering, implicit conversions, null treatment,
and diagnostic behaviour are not inherited unless an Orna decision accepts
them explicitly.

The same rule applies to every other standard contract. A standard definition
can exist before every possible operation over that type is implemented. An
unsupported operation remains rejected by the Orna compiler or runtime rather
than falling through to PostgreSQL.

The exact version-1 compatibility projection is:

| Kernel contract suffix | Internal compatibility kind | Existing runtime value | PostgreSQL storage |
| --- | --- | --- | --- |
| `boolean@1` | `Boolean` | `RuntimeValue::Boolean` | `boolean` |
| `integer@1` | `Integer` | `RuntimeValue::Integer` | `integer` |
| `bigint@1` | `BigInt` | `RuntimeValue::BigInt` | `bigint` |
| `float@1` | `Float` | `RuntimeValue::Float` | `double precision` |
| `decimal@1` | `Decimal` | unavailable | `numeric` |
| `character-large-object@1` | `CharacterLargeObject` | `RuntimeValue::Text` | `text` |
| `binary-large-object@1` | `BinaryLargeObject` | `RuntimeValue::Bytes` | `bytea` |
| `uuid@1` | `Uuid` | unavailable | `uuid` |
| `date@1` | `Date` | unavailable | `date` |
| `time@1` | `Time` | unavailable | `time without time zone` |
| `timestamp@1` | `Timestamp` | unavailable | `timestamp with time zone` |
| `duration@1` | `Duration` | unavailable | `interval` |
| `void@1` | `Void` | unavailable | unavailable |

The table preserves the current implementation boundary; it does not accept a
new runtime value or operation. The PostgreSQL column names are adapter facts
and never enter the standard source or standard digest. Existing work ADR
allow-lists remain authoritative for equality, ordering, literals, mutation
arguments, projections, and results. A contract provides only the capabilities
those accepted decisions name; it does not inherit every operation offered by
its PostgreSQL storage type.

There is no general canonical value codec in this decision. Each existing
artefact retains its own exact accepted encoding, and the runtime values above
retain their current in-memory contract. A later protocol or custom-value
transport decision must define its own canonical wire encoding.

## Resolution

Type resolution proceeds in this order:

1. retain exact lossless source spelling and quotedness;
2. if the source is an accepted unquoted prelude spelling, resolve its direct
   standard binding;
3. otherwise resolve an exact owner-qualified primary name or qualified
   binding in the effective catalogue;
4. load the primary definition by the resulting `TypeId`; and
5. validate that the definition category is permitted at the use site.

An unquoted `BOOLEAN`, `BOOL`, `std.BOOLEAN`, or `std.types.BOOLEAN` therefore
selects the same definition. A quoted `"BOOLEAN"` is an ordinary exact
unqualified identifier and does not select the prelude type. Qualified quoted
parts use exact catalogue lookup. A qualified scalar name is no longer
rejected merely because standard scalars were formerly keyword-only.

`REF` accepts only an object-type definition. A standard or custom value type
used as a `REF` target reports the existing scalar/value-category diagnostic at
the exact target span. The active standard application checker retains the
resolved `TypeId` for a field, parameter, or declared return value and never
re-resolves its source name. Its later relational and mutation body-owner rows
retain expression and result uses. The later function-evidence row emits the
normal ordered `NamedType` definition reference to the canonical value-type
`TypeId` at the written type span. Prelude and qualified spellings therefore
produce the same reference target with different retained source origins.
`DefinitionReferenceKind::NamedType` consequently means a dependency on any
named object or value type. `ObjectReference`, `QueryObject`, `WriteObject`, and
`REF` remain restricted to object definitions.

SERVER signature evidence remains the prefix before the body evidence fixed by
work ADRs 0005 and 0007 through 0012. The later function-evidence row scans
accepted SERVER parameters in ordinal order and then accepted SERVER `ROWS`
columns in ordinal order. It also scans the accepted scalar CLIENT return.
SERVER scalar `Single` returns remain rejected and contribute no evidence.
Each direct value type then contributes `NamedType` at its written type; each
`REF` continues to contribute only its existing `ObjectReference` at the
written target. Repeated written type uses produce repeated ordered references.
The body sequences in those decisions retain their exact relative order after
this expanded prefix.

### Standard application checking

The standard-backed application seam accepts a checked standard library, not a
raw `VerifiedStandardLibrarySnapshot`. Its exact public capability is:

```rust
#[derive(Clone, Copy, Debug)]
pub struct StandardApplicationCheckContext<'a> {
    application: &'a CatalogueSnapshot,
    standard: &'a CheckedStandardLibrary,
}

impl<'a> StandardApplicationCheckContext<'a> {
    pub fn try_new(
        application: &'a CatalogueSnapshot,
        standard: &'a CheckedStandardLibrary,
    ) -> Result<Self, StandardApplicationContextError>;

    pub fn application_catalogue(&self) -> &'a CatalogueSnapshot;
    pub fn standard_library(&self) -> &'a CheckedStandardLibrary;
}

pub fn check_standard_application(
    bundle: &SourceBundle,
    context: &StandardApplicationCheckContext<'_>,
) -> StandardApplicationCheckReport;
```

`CheckedStandardLibrary` is unforgeable outside `orna-compiler` and has already
reconciled its source, catalogue, and origins. `try_new` trusts that checked
capability. It does not re-run reconciliation or validate checked facts against
their own snapshot catalogue. It uses the checked source-ordered facts and the
checked capability's owned snapshot catalogue for lookup. Those facts, rather
than a raw verified snapshot, are the required authority for application
checking. Accepted `orna.std/1` golden enforcement remains outside the
compiler.

`StandardApplicationContextError` is public, `#[non_exhaustive]`, and derives
`Clone`, `Debug`, `Eq`, and `PartialEq`. It has exactly these variants:

```rust
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StandardApplicationContextError {
    SchemaIdentityConflict { id: SchemaId },
    SchemaNameConflict { name: QualifiedSemanticName },
    TypeIdentityConflict { id: TypeId },
    TypeBindingIdentityConflict { id: TypeBindingId },
    UnsupportedCompatibilityContract { type_id: TypeId, contract: String },
    CompatibilityContractConflict { contract: String },
}
```

Its exact actionable `Display` text is:

| Variant | Display |
| --- | --- |
| `SchemaIdentityConflict` | `the application catalogue conflicts with standard schema identity {id}` |
| `SchemaNameConflict` | `the application catalogue conflicts with standard schema name {name}` |
| `TypeIdentityConflict` | `the application catalogue conflicts with standard type identity {id}` |
| `TypeBindingIdentityConflict` | `the application catalogue conflicts with standard type binding identity {id}` |
| `UnsupportedCompatibilityContract` | `the standard value type {type_id} uses unsupported compatibility contract {contract}` |
| `CompatibilityContractConflict` | `the standard library uses compatibility contract {contract} for more than one type` |

`std::error::Error::source()` returns `None` for every context-error variant.
`try_new` has no lookup-order winner. It completes each of these source-ordered
gates over every checked fact before it starts the next gate:

1. schema identities;
2. schema names;
3. type identities;
4. type-binding identities;
5. complete unsupported-contract mapping; and
6. complete duplicate-contract detection.

The first four gates compare the appropriate checked standard facts with the
application catalogue. The final two gates derive private compatibility from
the checked value-type facts. A valid application primary or qualified
`std.*` name collision requires the standard parent schema, so
`SchemaNameConflict` rejects it at gate 2. Every qualified or prelude
binding-name collision has the same core-derived `TypeBindingId`, so
`TypeBindingIdentityConflict` rejects it at gate 4. No later type-name winner
or error exists. A context cannot substitute a raw verified snapshot, manifest,
standard digest, or trust flag for `CheckedStandardLibrary`.

`StandardApplicationCheckReport` is a distinct public result. It derives
`Clone` and implements `Debug` manually. It owns a clone of the exact
`CheckedStandardLibrary`, a `ParseReport`, diagnostics, and, only when there
are no diagnostics, a distinct `CheckedStandardApplicationBundle`. Its exact
read-only accessors are:

```rust
#[derive(Clone)]
pub struct StandardApplicationCheckReport {
    standard_library: CheckedStandardLibrary,
    parse_report: ParseReport,
    diagnostics: Vec<CompilerDiagnostic>,
    checked_bundle: Option<CheckedStandardApplicationBundle>,
}

pub fn standard_library(&self) -> &CheckedStandardLibrary;
pub fn parse_report(&self) -> &ParseReport;
pub fn diagnostics(&self) -> &[CompilerDiagnostic];
pub fn checked_bundle(&self) -> Option<&CheckedStandardApplicationBundle>;
```

It has no conversion, `Deref`, borrow, `inner`, or extraction API to
`CheckReport` or `CheckedBundle`. A standard application report cannot be
prepared by the legacy preparation path.

`CheckedStandardApplicationBundle` derives `Clone`, `Eq`, and `PartialEq` and
implements `Debug` manually. It owns the following implementation boundary:

```rust
#[derive(Clone, Eq, PartialEq)]
pub struct CheckedStandardApplicationBundle {
    pub(super) inner: CheckedBundle,
    standard_catalogue_revision: CatalogueRevisionId,
    standard_library_revision: StandardLibraryRevisionId,
    standard_library_digest: Sha256Digest,
    uses: Vec<CheckedApplicationTypeUse>,
    standard_type_references: Vec<CheckedStandardTypeReference>,
    /* private lookup into uses */
}
```

The private state stores the standard catalogue revision, standard-library
revision, standard-library digest, one canonical
`Vec<CheckedApplicationTypeUse>` arena, an initially empty
`Vec<CheckedStandardTypeReference>` arena, and a private lookup into the type
use arena. The initial public rustdoc for the latter records that it is empty
until the later function-reference row populates it. Each declared or body type
use occurs in the type-use arena exactly once. Its exact public accessors are:

```rust
pub fn base_catalogue_revision(&self) -> CatalogueRevisionId;
pub fn standard_catalogue_revision(&self) -> CatalogueRevisionId;
pub fn standard_library_revision(&self) -> StandardLibraryRevisionId;
pub fn standard_library_digest(&self) -> Sha256Digest;
pub fn uses(&self) -> &[CheckedApplicationTypeUse];
pub fn value_type_uses(&self) -> impl Iterator<Item = &CheckedValueTypeUse> + '_;
/// Returns standard function-signature references. This arena is initially
/// empty; the later function-reference row populates it.
pub fn standard_type_references(&self) -> &[CheckedStandardTypeReference];
pub fn schemas(&self) -> &[CheckedSchema];
pub fn object_types(
    &self,
) -> impl ExactSizeIterator<Item = CheckedStandardApplicationObjectType<'_>> + '_;
pub fn server_functions(
    &self,
) -> impl ExactSizeIterator<Item = CheckedStandardApplicationServerFunction<'_>> + '_;
pub fn client_functions(
    &self,
) -> impl ExactSizeIterator<Item = CheckedStandardApplicationClientFunction<'_>> + '_;
```

The family iterators return scalar-free borrowed views over that one `inner`
state. They do not expose an existing checked object or function family,
`SemanticType`, or a parallel owned family copy. Each view has only private
references and indices, derives `Clone` and `Copy`, and implements `Debug`
manually:

```rust
#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationObjectType<'a> { /* private references and indices */ }

#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationField<'a> { /* private references and indices */ }

#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationServerFunction<'a> { /* private references and indices */ }

#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationClientFunction<'a> { /* private references and indices */ }

#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationParameter<'a> { /* private references and indices */ }

#[derive(Clone, Copy)]
pub struct CheckedStandardApplicationReturnColumn<'a> { /* private references and indices */ }

impl<'a> CheckedStandardApplicationObjectType<'a> {
    pub fn id(&self) -> CheckedTypeId;
    pub fn name(&self) -> &QualifiedSemanticName;
    pub fn fields(
        &self,
    ) -> impl ExactSizeIterator<Item = CheckedStandardApplicationField<'a>> + '_;
    pub fn location(&self) -> &SourceLocation;
}

impl<'a> CheckedStandardApplicationField<'a> {
    pub fn id(&self) -> CheckedFieldId;
    pub fn name(&self) -> &str;
    pub fn ordinal(&self) -> u32;
    pub fn resolved_type(&self) -> &CheckedApplicationTypeUse;
    pub fn nullable(&self) -> bool;
    pub fn unique(&self) -> bool;
    pub fn default(&self) -> Option<(&ConstantValue, &SourceLocation)>;
    pub fn on_delete(&self) -> Option<OnDeleteAction>;
    pub fn location(&self) -> &SourceLocation;
}

impl<'a> CheckedStandardApplicationServerFunction<'a> {
    pub fn id(&self) -> CheckedFunctionId;
    pub fn name(&self) -> &QualifiedSemanticName;
    pub fn parameters(
        &self,
    ) -> impl ExactSizeIterator<Item = CheckedStandardApplicationParameter<'a>> + '_;
    pub fn return_columns(
        &self,
    ) -> impl ExactSizeIterator<Item = CheckedStandardApplicationReturnColumn<'a>> + '_;
    pub fn security(&self) -> FunctionSecurity;
    pub fn transaction(&self) -> Option<FunctionTransaction>;
    pub fn volatility(&self) -> FunctionVolatility;
    pub fn location(&self) -> &SourceLocation;
    pub fn references(&self) -> &[CheckedDefinitionReference];
}

impl<'a> CheckedStandardApplicationClientFunction<'a> {
    pub fn id(&self) -> CheckedFunctionId;
    pub fn name(&self) -> &QualifiedSemanticName;
    pub fn domain(&self) -> FunctionDomain;
    pub fn parameters(
        &self,
    ) -> impl ExactSizeIterator<Item = CheckedStandardApplicationParameter<'a>> + '_;
    pub fn return_type(&self) -> &CheckedApplicationTypeUse;
    pub fn security(&self) -> FunctionSecurity;
    pub fn transaction(&self) -> Option<FunctionTransaction>;
    pub fn volatility(&self) -> FunctionVolatility;
    pub fn location(&self) -> &SourceLocation;
    pub fn references(&self) -> &[CheckedDefinitionReference];
}

impl<'a> CheckedStandardApplicationParameter<'a> {
    pub fn id(&self) -> CheckedParameterId;
    pub fn name(&self) -> &str;
    pub fn ordinal(&self) -> u32;
    pub fn resolved_type(&self) -> &CheckedApplicationTypeUse;
    pub fn location(&self) -> &SourceLocation;
}

impl<'a> CheckedStandardApplicationReturnColumn<'a> {
    pub fn name(&self) -> &str;
    pub fn ordinal(&self) -> u32;
    pub fn resolved_type(&self) -> &CheckedApplicationTypeUse;
    pub fn location(&self) -> &SourceLocation;
}
```

`references()` in those function views exposes application and object evidence
only until the later standard-reference row adds its separate arena. The
borrowed default value has no type-resolution surface; no existing checked
family leaks through a view.

The canonical type-use arena has this exact public model:

```rust
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CheckedTypeUseKind {
    Field {
        owner: CheckedTypeId,
        field: CheckedFieldId,
    },
    Parameter {
        owner: CheckedFunctionId,
        parameter: CheckedParameterId,
    },
    Return {
        owner: CheckedFunctionId,
        ordinal: u32,
    },
    Expression {
        owner: CheckedFunctionId,
        ordinal: u32,
    },
    Result {
        owner: CheckedFunctionId,
        ordinal: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedValueTypeUse {
    type_id: TypeId,
    kind: CheckedTypeUseKind,
    location: SourceLocation,
}

impl CheckedValueTypeUse {
    pub fn type_id(&self) -> TypeId;
    pub fn kind(&self) -> CheckedTypeUseKind;
    pub fn location(&self) -> &SourceLocation;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedObjectReferenceUse {
    target: CheckedTypeId,
    kind: CheckedTypeUseKind,
    location: SourceLocation,
}

impl CheckedObjectReferenceUse {
    pub fn target(&self) -> CheckedTypeId;
    pub fn kind(&self) -> CheckedTypeUseKind;
    pub fn location(&self) -> &SourceLocation;
}

#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckedApplicationTypeUse {
    Value(CheckedValueTypeUse),
    ObjectReference(CheckedObjectReferenceUse),
}

impl CheckedApplicationTypeUse {
    pub fn value(&self) -> Option<&CheckedValueTypeUse>;
    pub fn object_reference(&self) -> Option<&CheckedObjectReferenceUse>;
    pub fn kind(&self) -> CheckedTypeUseKind;
    pub fn location(&self) -> &SourceLocation;
}
```

The public model rustdoc describes both declared and body type uses. Each
direct field, SERVER parameter, accepted CLIENT scalar return, accepted SERVER
`ROWS` column, or `REF` target records its exact written target span in this
arena. Each body `Expression` or `Result` records its complete expression
span. `Field`, `Parameter`, and `Return` are emitted by the active checker row.
`Parameter` currently records SERVER parameters only. Both the legacy and
standard-backed paths retain the exact accepted diagnostic `this CLIENT
function cannot declare parameters yet`; a successful CLIENT view's
`parameters()` iterator is empty and remains a future-safe surface. `Return`
records an accepted CLIENT scalar return or each accepted SERVER `ROWS` column,
never a body result. A rejected SERVER scalar `Single` return produces no
declaration evidence. Body-owner rows emit `Expression` and `Result`; they
must not use `CheckedExpressionId`.

`Expression` records every accepted value-producing body expression. Its
ordinal is zero-based deterministic preorder within the owning function body.
`Result` records a final body projection or return value. Its ordinal is the
zero-based declared result order. It may share a source span with an
`Expression`; for coincident spans, `Expression` precedes `Result`. A CLIENT
Boolean body therefore adds `Expression { ordinal: 0 }` and
`Result { ordinal: 0 }` in its relational owner row.

The complete arena order is source-unit ordinal, source start, source end, and
then kind tag in this order: `Field`, `Parameter`, `Return`, `Expression`,
`Result`. Within one kind, the stored ordinal or durable checked identity order
breaks a tie. Family views borrow the corresponding arena use through the
private lookup; they never copy it.

Resolution retains exact lossless spelling and quotedness. It accepts the
unquoted standard type spelling set fixed by this decision: all thirteen
qualified primaries, all thirteen qualified bindings, and all seventeen prelude
bindings, including `BOOLEAN`, `BOOL`, `INTEGER`, `INT`, `TEXT`, `BYTES`,
their qualified aliases, and the remaining manifest spellings. The two schemas
are schema facts, not type spellings. Quoted names follow exact catalogue lookup
and do not acquire prelude meaning.
After `TypeId` lookup and a checked standard-value definition lookup, the
checker derives a compatibility scalar privately from that definition's exact,
unique supported contract. It never reverse-maps a `StandardScalar` to a
`TypeId`. An unsupported contract or duplicate supported contract fails
context construction with the exact context error above.

The exact private compatibility mapping is:

| Representation contract | `StandardScalar` |
| --- | --- |
| `orna.kernel.value.boolean@1` | `Boolean` |
| `orna.kernel.value.integer@1` | `Integer` |
| `orna.kernel.value.bigint@1` | `BigInt` |
| `orna.kernel.value.float@1` | `Float` |
| `orna.kernel.value.decimal@1` | `Decimal` |
| `orna.kernel.value.character-large-object@1` | `CharacterLargeObject` |
| `orna.kernel.value.binary-large-object@1` | `BinaryLargeObject` |
| `orna.kernel.value.uuid@1` | `Uuid` |
| `orna.kernel.value.date@1` | `Date` |
| `orna.kernel.value.time@1` | `Time` |
| `orna.kernel.value.timestamp@1` | `Timestamp` |
| `orna.kernel.value.duration@1` | `Duration` |
| `orna.kernel.value.void@1` | `Void` |

The unsupported-contract gate completes before the duplicate-contract gate.
Two distinct `TypeId` values using the same supported contract are rejected by
the latter. No `StandardScalar` to `TypeId` reverse lookup exists.

### Relational value provenance

The relational checker carries the resolved standard `TypeId` beside its private
compatibility scalar. When a relational or CLIENT Boolean expression needs the
intrinsic Boolean type, it looks up the exact
`orna.kernel.value.boolean@1` contract in the checked standard value-type
facts and retains that fact's `TypeId`. It never reverse-maps
`StandardScalar::Boolean`, or any other `StandardScalar`, to a `TypeId`.

The six `StandardApplicationCheckContext::try_new` gates remain unchanged. A
checked standard library that lacks the Boolean value type is not a new context
error. Instead, each Boolean-producing expression that requires the intrinsic
type reports `DiagnosticCode::DomainIncompatible` (`ORNA0303`) with this exact
message at the complete expression span:

```text
the checked standard library does not provide a Boolean value type
```

That diagnostic returns no checked standard-application bundle and no
`CheckedApplicationTypeUse`. It does not substitute a scalar-derived Boolean
identity, fall back to a spelling lookup, or continue body evidence collection.

Each accepted body expression produces exactly one `Expression` arena use. A
standard value expression produces `CheckedApplicationTypeUse::Value` with its
resolved durable `TypeId`. Every object-reference-valued expression produces
`CheckedApplicationTypeUse::ObjectReference` with its checked object target.
This includes `REF(...)` and the parameter read in an identity-selector
equality. A projection root has its additional `Result` use as specified below.
Every body use retains the complete expression span, not only an alias, member,
parameter, or `REF` child span.

Relational equality compares standard values by supplied `TypeId`, not by the
private compatibility scalar. Two operands with the same compatibility scalar
and different supplied `TypeId` values do not match. Two operands with the same
supplied `TypeId` do match. Object-reference equality remains comparison by the
`CheckedTypeId` target.

The public context cannot construct two value definitions that use one
supported compatibility contract with different `TypeId` values. Its duplicate
contract gate rejects that input. The relational row therefore owns private
relational-seam tests that supply those facts directly. They prove different
supplied IDs mismatch despite an equal compatibility scalar, equal supplied IDs
match, and mixed present and absent provenance returns the existing
`DiagnosticCode::TypeMismatch` (`ORNA0201`) `equality requires expressions
with compatible types` diagnostic.
They also prove that legacy scalar equality, its diagnostics, and its artefact
bytes remain exact.

A self-consistent core-verified non-golden checked standard library may use a
changed Boolean `TypeId`. The relational row tests that a Boolean literal and
an equality retain that supplied ID, and that CLIENT `Expression { ordinal: 0 }`
and `Result { ordinal: 0 }` uses retain it too. No relational path can
hard-code the accepted standard Boolean identity.

For ordinary SERVER traversal, visit expressions in declared projection order,
then the predicate, then `ORDER BY` terms. Within every expression, visit the
parent before its children. Allocate zero-based `Expression` ordinals in that
one traversal order. Each projection root also produces one `Result` use with
the zero-based projection index, including a projection whose root is a `REF`.
Predicates and ordering terms produce no `Result` use.

For an identity-selected SERVER query, visit all projections first. Then visit
the equality parent, its left `REF` child, and its right parameter child. Those
three selector uses receive consecutive `Expression` ordinals in that exact
parent-before-children order. The selector produces no `Result` use. Projection
roots retain their normal indexed `Result` uses.

A successful CLIENT Boolean literal body produces
`Expression { ordinal: 0 }` and `Result { ordinal: 0 }`, both with the complete
literal span and the intrinsically resolved Boolean `TypeId`.

### Mutation value provenance

Before mutation evidence is added, syntax retains these complete expression
spans without changing its closed mutation grammar:

* `InsertStatement::returning_ref_span: SourceSpan`,
* `UpdateStatement::selector_equality_span: SourceSpan`,
* `UpdateStatement::selector_ref_span: SourceSpan`,
* `UpdateStatement::returning_ref_span: SourceSpan`,
* `DeleteStatement::selector_equality_span: SourceSpan`, and
* `DeleteStatement::selector_ref_span: SourceSpan`.

`returning_ref_span` starts at `REF` and ends after its closing parenthesis.
`selector_equality_span` starts at selector `REF` and ends after the selector
parameter. `selector_ref_span` starts at the selector `REF` token and ends
after its closing parenthesis. Each includes intervening trivia, including a
comment before the closing parenthesis. Existing `MutationValue::span()`
continues to identify the complete right-hand-side parameter, Boolean literal,
or `NULL` literal. `DeleteStatement::returning_true` continues to identify the
complete returned `TRUE` literal. Parser tests use mixed-case keywords and
intervening trivia, retain lossless text, and compare every retained span to
the exact source substring.

The mutation checker carries a supplied standard value `TypeId` beside its
private compatibility scalar. It obtains the intrinsic Boolean `TypeId` only
from the checked `orna.kernel.value.boolean@1` value-type fact. It never
reverse-maps a `StandardScalar` to a `TypeId`. Mutation assignment
compatibility compares supplied `TypeId` values, not compatibility scalars.
Selector `REF` validation continues to compare the checked object
`CheckedTypeId` target.

A missing checked Boolean fact leaves the six context gates unchanged. Every
Boolean-producing mutation expression, including a Boolean right-hand side, a
selector equality, and `DELETE ... RETURNING TRUE`, reports
`DiagnosticCode::DomainIncompatible` (`ORNA0303`) with the exact existing
message `the checked standard library does not provide a Boolean value type`
at its complete expression span. The diagnostic returns no checked standard
application bundle and no type use. Missing-Boolean tests require exact ordered
diagnostic vectors. An `INSERT` with a Boolean literal right-hand side reports
one exact `ORNA0303` at the complete literal span. An `UPDATE` with Boolean
right-hand-side values and a selector equality reports those right-hand sides
in source order, then the complete selector-equality span. A `DELETE` with a
selector equality and `RETURNING TRUE` reports the complete selector-equality
span, then the complete returned-`TRUE` span. Each vector contains only the
stated exact `ORNA0303` diagnostics and returns no bundle or uses.

Mutation traversal allocates zero-based `Expression` ordinals in this exact
order, and visits each parent before its children:

* `INSERT` visits right-hand-side values in source order, then its
  `RETURNING REF(...)` expression.
* `UPDATE` visits right-hand-side values in source order, then selector
  equality, its `REF(...)` child, its parameter child, and its
  `RETURNING REF(...)` expression.
* `DELETE` visits selector equality, its `REF(...)` child, its parameter child,
  then its returned `TRUE` expression.

Assignments and selectors produce no `Result`. An `INSERT` or `UPDATE`
`RETURNING REF(...)` produces an `ObjectReference` `Expression` and an
`ObjectReference` `Result { ordinal: 0 }`. It is not a `Value` result. A
`DELETE ... RETURNING TRUE` produces a `Value` `Expression` and a `Value`
`Result { ordinal: 0 }`. Every mutation arena use keeps its complete expression
span. If `Expression` and `Result` share the same span, canonical sorting puts
`Expression` before `Result`.

The mutation row owns private assignment-compatibility seam tests because the
public context rejects equal supported contracts on different value `TypeId`
values. They prove that an equal compatibility scalar with a different supplied
`TypeId` cannot be assigned, equal supplied IDs can be assigned, and mixed
present and absent provenance fails compatibility. INSERT reports at the
parameter span `parameter {name} cannot be inserted into field {field} because
their types do not match`. UPDATE reports at the parameter span
`parameter {name} cannot be assigned to field {field} because their types do
not match`. Selector `REF` validation retains its exact parameter-span
diagnostic `selector parameter {name} must use REF {target}`. Missing Boolean
tests cover every stated Boolean-producing mutation expression. A
self-consistent core-verified non-golden standard with a changed Boolean
`TypeId` proves that every distinct mutation Boolean path retains that supplied
ID: INSERT and UPDATE Boolean right-hand sides, UPDATE and DELETE selector
equalities, and DELETE returned `TRUE`. Legacy mutation assignment
compatibility, diagnostics, and artefact bytes remain exact.

### Standard type-reference arena

The active `feat(compiler): resolve types through std` row defines this
compiler-owned model and adds an empty arena to every
`CheckedStandardApplicationBundle`. It remains separate from existing
application and object definition references. Its initial public rustdoc says
that `standard_type_references()` is empty until the later function-reference
row populates it:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStandardTypeReference {
    owner: CheckedFunctionId,
    ordinal: u32,
    target: TypeId,
    location: SourceLocation,
}

impl CheckedStandardTypeReference {
    pub fn owner(&self) -> CheckedFunctionId;
    pub fn ordinal(&self) -> u32;
    pub fn target(&self) -> TypeId;
    pub fn location(&self) -> &SourceLocation;
}
```

`CheckedStandardApplicationBundle` owns a source/function-order arena of these
references. `ordinal` is one flattened zero-based ordinal for each function
signature. For accepted SERVER functions, it covers parameters first in
declaration order and then `ROWS` columns in declaration order. For accepted
CLIENT functions, it covers the scalar return at ordinal `0`; CLIENT parameters
remain rejected. Rejected SERVER scalar `Single` returns do not enter this
arena. Every accepted direct signature slot, including a `REF` slot, increments
this ordinal. `REF` emits no standard type reference, so its slots intentionally
leave gaps in the emitted ordinal sequence.

The arena is initially empty. The later
`feat(compiler): reference standard function types` row derives its entries
from the already canonical declaration type uses and their retained
source-order metadata. It does not re-resolve type spelling, repeat catalogue
lookup, or infer a `TypeId` from a scalar. Each value declaration use yields a
repeated `NamedType` and `ValueType` standard-type reference, even if an
earlier use has the same `TypeId`. There is deliberately no `kind` field: this
arena records only standard value-type signature evidence. It does not add an
entry to `CheckedServerFunction::references()`,
`CheckedDefinitionReference`, or `ObjectReference`; those application and
object reference sequences remain unchanged and separate.

Its arena order is source-unit ordinal, function declaration start, then this
flattened signature ordinal. The implementation carries source-order metadata
through a shared resolver seam to obtain that order and must not derive it by
iterating the resolver's separate SERVER and CLIENT family passes. That later
row performs no preparation. The source-unit ordinal is the `SourceBundle`
insertion ordinal, not a logical-path sort. The required proof inserts a unit
whose logical path sorts after a later-inserted unit and asserts the vector
follows insertion ordinal rather than lexical path order.

### Standard application preparation

Only the later dedicated preparation row adds this public function:

```rust
pub fn prepare_standard_application(
    report: &StandardApplicationCheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
) -> Result<DeployableRevision, PrepareStandardApplicationError>;
```

`PrepareStandardApplicationError` is public, `#[non_exhaustive]`, and derives
`Debug` only. It has exactly these variants:

```rust
#[non_exhaustive]
#[derive(Debug)]
pub enum PrepareStandardApplicationError {
    CheckNotComplete { diagnostic_count: usize },
    ExpectedBaseMismatch { expected: RevisionPair, active: RevisionPair },
    CheckedBaseMismatch { checked: CatalogueRevisionId, active: CatalogueRevisionId },
    StandardLibraryUnavailable,
    StandardCatalogueMismatch { checked: CatalogueRevisionId, active: CatalogueRevisionId },
    StandardRevisionMismatch {
        checked: StandardLibraryRevisionId,
        active: StandardLibraryRevisionId,
    },
    StandardDigestMismatch { checked: Sha256Digest, active: Sha256Digest },
    DeclarationTypeEvidenceMismatch { kind: CheckedTypeUseKind },
    BodyTypeEvidenceMismatch { function: CheckedFunctionId },
    FunctionTypeReferenceMismatch { function: CheckedFunctionId },
    Prepare { source: PrepareError },
}
```

Its exact `Display` text is:

| Variant | Display |
| --- | --- |
| `CheckNotComplete` | `the standard application check has {diagnostic_count} diagnostics` |
| `ExpectedBaseMismatch` | `the expected application base does not match the active revision` |
| `CheckedBaseMismatch` | `the checked application base does not match the active revision` |
| `StandardLibraryUnavailable` | `the active database has no standard library` |
| `StandardCatalogueMismatch` | `the checked standard catalogue does not match the active standard catalogue` |
| `StandardRevisionMismatch` | `the checked standard library revision does not match the active standard library revision` |
| `StandardDigestMismatch` | `the checked standard library digest does not match the active standard library digest` |
| `DeclarationTypeEvidenceMismatch` | `the checked declaration type evidence does not match its {kind} type use` |
| `BodyTypeEvidenceMismatch` | `the checked body type evidence does not match function {function}` |
| `FunctionTypeReferenceMismatch` | `the checked function type references do not match function {function}` |
| `Prepare` | `the standard application could not be prepared: {source}` |

For `DeclarationTypeEvidenceMismatch`, `{kind}` is the lower-case variant tag
`field`, `parameter`, `return`, `expression`, or `result`, without an owner or
ordinal. `{function}` is the exact checked-function identity. `{source}` is
the exact `PrepareError` display text.

`Error::source()` returns `Some(source)` only for `Prepare`; it returns `None`
for all other variants. Preparation runs in this exact order: complete report;
expected base; checked application base; active standard presence; standard
catalogue; standard revision; standard digest; declaration arena one-to-one;
body arena one-to-one; function-reference arena one-to-one; shared semantic
preflight; then allocation. It never re-resolves source spelling.

Before it constructs the candidate catalogue, canonical hashes, or revision,
`prepare_standard_application` privately retries every random new application
`CatalogueRevisionId`, `SourceBundleId`, `SourceRevisionId`, copied
`SourceUnitId`, `SchemaId`, and `TypeId` when it equals a reserved standard ID
of the same class. This retry has no public error. It creates no
`TypeBindingId`: binding identity is derived from its name, and neither current
preparation slice creates an application type binding. A later accepted
application-binding row must run a post-derivation same-class
`ReservedIdentity` gate before candidate catalogue construction. The private
deterministic test allocator yields the relevant reserved ID and then a
non-reserved ID for every listed allocation, proving retry and that no
constructed candidate ID collides with a reserved ID.

Legacy `check` and legacy `prepare` remain frozen version-1 compatibility
paths. They retain ordinary `ORNA0303` and scalar behaviour unchanged. Their
eventual removal is an explicit later migration after all callers use this
distinct standard application path; the new report has no legacy conversion.

## Compatibility

This decision preserves:

* all thirteen existing reserved standard `TypeId` byte values;
* the public `BOOLEAN`, `BOOL`, `INTEGER`, `INT`, `TEXT`, and `BYTES` spellings
  and every other existing canonical standard spelling;
* quoted-name behaviour and lossless source retention;
* `RuntimeValue` value semantics and contextual errors;
* existing `orna.server-plan`, `orna.server-mutation-plan`, and
  `orna.client-plan` payload bytes, versions, decoders, and cross-version
  rejection;
* immutable version-1 function semantic hashes and application catalogue
  hashes as historical records;
* stable object, field, function, parameter, expression, and source identities;
  and
* private PostgreSQL relation, column, constraint, and index names.

Version-1 executable artefact encoders continue to encode a reserved standard
`TypeId` with the existing scalar tag and discriminator. Decoders map that tag
to the corresponding reserved `TypeId`. This is a compatibility encoding, not
a second semantic identity. Canonical hash contract version 2 separately
records catalogue-backed type dependencies. A custom value type needs a later
accepted artefact capability or version where the current closed formats do
not already carry a general `TypeId`.

Work ADR 0015's Boolean CLIENT function continues to accept `BOOLEAN` and
`BOOL`; it additionally accepts unquoted `std.BOOLEAN` and
`std.types.BOOLEAN`, plus qualified forms whose quoted parts exactly match the
normalised catalogue name. It returns the same typed Boolean runtime value and
produces the exact same 14-byte client plan. Its checked and durable return
validation changes from enum equality to the canonical Boolean `TypeId` once
the catalogue model is available.

The complete CLIENT definition-reference sequence is then exactly one item:
ordinal `0`, kind `NamedType`, target `ValueType` with the canonical Boolean
`TypeId`, and source origin at the written return type. The Boolean literal adds
no second reference. Preparation independently requires that exact sequence.
The local evaluator requires that exact reference and rejects a missing,
additional, reordered, wrong-kind, wrong-target, or wrong-revision reference as
`ClientExecutionRule::References`. Its client-plan bytes, diagnostics,
source-only revision reuse within hash contract version 2, evaluation result,
and security boundary otherwise remain unchanged.

Installing `orna.std/1` on an existing database creates a new active catalogue
revision rather than rewriting an old one. Each active function that uses a
standard value type receives a new immutable function revision containing the
new exact `NamedType` evidence, even when its executable artefact payload is
byte-identical. Functions without a standard type dependency retain their
current revision. The migration is atomic: either the standard revision, new
catalogue revision, all required function revisions, and active pointer commit
together, or the previous version-1 active revision remains authoritative.

## Required proof

### Trusted compiler checker proof matrix

| Boundary | Required cases | Required result |
| --- | --- | --- |
| Public interface and model | The exact public function signature; no raw snapshot, manifest, source bundle, or trust flag; compiler dependency graph; each stated derive; `verified_snapshot()` and every checked family accessor with its exact return type | `check_standard_library_source` accepts only `&VerifiedStandardLibrarySnapshot`. `orna-compiler` has no `orna-standard` dependency. The checked result owns a clone of the supplied capability and source-ordered schema, value-type, and type-binding fields with durable IDs taken from the matched catalogue facts and exact `SourceOrigin` values. Copy and borrow accessor behaviour is exact. |
| Error contract | Each `StandardLibraryCheckError` derive, variant, exact `Display`, and `Error::source()` | The error is public, non-exhaustive, derives `Clone`, `Debug`, `Eq`, and `PartialEq`, and exposes only `SourceUnitCount { actual }`, `Diagnostics { diagnostics }`, and `SourceMismatch`. Each display is exact and every error source is `None`. |
| Count and parse precedence | Zero, one, and multiple stored source units; malformed sole source unit; malformed source with otherwise mismatched catalogue data | A non-one unit count returns `SourceUnitCount` before parsing or catalogue work. One unit parses once. Syntax diagnostics return the unchanged ordered `CompilerDiagnostic` vector in `Diagnostics` before every reconciliation check. |
| Lossless shape | A test seam where parsed lossless text differs from the stored source text; object type, rename, SERVER function, CLIENT function, and unsupported declaration categories; each family count that differs from the supplied verified catalogue | Each case returns `SourceMismatch`. The accepted fixture proves two schemas, thirteen primitive value types, thirteen qualified exports, and seventeen prelude exports. A self-consistent non-golden snapshot can have different supported-family counts. |
| Schema facts | Missing, extra, duplicate, renamed, quoted, or crossed schema source fact; a catalogue schema identity with a missing or crossed origin | Each case returns `SourceMismatch`. Each verified schema fact is consumed once, with no source fact left over. |
| Primitive value-type facts | Missing, extra, duplicate, renamed, quoted, or crossed primitive; non-vacuous persistence and decoded-contract mismatches; a catalogue type identity with a missing or crossed origin; code review of current kind and mutability matches | Each case returns `SourceMismatch`. The implementation matches each current `ValueTypeKind` and `ValueTypeMutability` variant and has a fail-closed wildcard. Public core APIs cannot construct a future non-exhaustive variant, so executable hostile-variant proof is deferred until such a variant exists. Each source primitive matches one verified value-type fact and is consumed once. |
| Qualified binding facts | Missing, extra, duplicate, renamed, quoted, or crossed qualified export; wrong primary source, binding name, binding kind, or direct target; a catalogue binding identity with a missing or crossed origin | Each case returns `SourceMismatch`. Each verified qualified binding fact is consumed once. |
| Prelude binding facts | Missing, extra, duplicate, renamed, quoted, or crossed prelude export; wrong source qualified binding, prelude words, binding kind, or direct target; a catalogue binding identity with a missing or crossed origin | Each case returns `SourceMismatch`. Each verified prelude binding fact is consumed once and has the same direct target as its source qualified binding. |
| Origins | Missing, extra, duplicate, or crossed `DefinitionIdentity`; wrong source-unit ID; non-full declaration range; wrong start or end | Each case returns `SourceMismatch`. Every fact has one exact complete-declaration `SourceOrigin` in the sole stored unit, and every verified origin is consumed once. |
| Successful result | A core-verified snapshot whose source, catalogue, and origins agree | The result retains the supplied verified capability and all checked facts and origins in source order. It has no preparation, database, install, or type-use output. |
| Authority boundary | A core-verified self-consistent non-golden snapshot, including changed logical path, whitespace, comment, content, declaration order, durable identities, or supported-family counts when source, catalogue, and origins agree; a compiler dependency review | The compiler checker proves source, catalogue, and origin agreement only. It accepts the self-consistent non-golden case, has no `orna-standard` dependency, does not recheck the accepted digest, and does not create installable state. |
| Ordinary path compatibility | Protected application `std` owner, kernel contract, qualified export, and prelude export cases; syntax-error precedence; all established scalar spellings and rejected aliases | Ordinary checking preserves the exact `ORNA0303` diagnostics, spans, category order, scalar compatibility adapter, accepted aliases, and rejected aliases. The trusted checker never calls ordinary `check_parsed`. |
| Standard application capability | Exact `StandardApplicationCheckContext<'a>` derive, private fields, constructor, accessors, and accepted `CheckedStandardLibrary` capability; raw verified snapshot, manifest, digest, trust flag, and absent checked standard capability | Only the unforgeable, already reconciled `CheckedStandardLibrary` enters `try_new`. The context trusts its checked source-ordered facts and uses its owned snapshot catalogue for lookup without rerunning retained-source reconciliation. It neither accepts nor returns an unavailable-standard-library error, and it does not enforce the accepted `orna.std/1` golden. |
| Context gate order and errors | Schema-ID, schema-name, type-ID, binding-ID, unsupported-contract, and duplicate-contract conflicts; simultaneous conflicts in every adjacent pair; a valid primary or qualified `std.*` collision with its standard parent schema; qualified and prelude binding-name collisions with their same core-derived `TypeBindingId`; each `StandardApplicationContextError` derive, exact display, and absent error source | Each complete source-ordered fact class finishes before the next begins: schema IDs, schema names, type IDs, type-binding IDs, unsupported contracts, then duplicate contracts. No map or lookup insertion selects a winner. `SchemaNameConflict` at gate 2 structurally subsumes primary and qualified `std.*` name collisions. `TypeBindingIdentityConflict` at gate 4 structurally subsumes qualified and prelude binding-name collisions. No later type-name winner or error exists. Each public non-exhaustive context error has the exact typed field, `Display`, and `Error::source() == None`. |
| Standard application report separation | Success, diagnostics, and clone/debug cases; attempted `CheckReport` or `CheckedBundle` conversion, dereference, borrow, inner access, extraction, and legacy preparation | `StandardApplicationCheckReport` owns the exact checked standard-library clone, parse report, diagnostics, and optional distinct standard application bundle. Its four accessors are exact. It has no legacy report or bundle escape hatch and cannot enter legacy preparation. |
| Standard application bundle and views | Exact base and standard revision/digest accessors; schemas; all six borrowed view derives, accessors, source order, and manual `Debug`; the initially empty standard type-reference arena, accessor, and rustdoc; attempted existing object/function family, `SemanticType`, or parallel-copy exposure | `CheckedStandardApplicationBundle` owns its resolver-visible `pub(super) inner: CheckedBundle`, standard revision data, one canonical type-use arena, an initially empty `CheckedStandardTypeReference` arena, and its private type-use lookup. The initial `standard_type_references()` rustdoc records that later function evidence populates that empty arena. Schemas safely remain a slice. Object, field, SERVER, CLIENT, parameter, and return-column views borrow that one state and return a borrowed `CheckedApplicationTypeUse` for every resolved direct type. Successful current CLIENT views have an empty parameter iterator. |
| Canonical type-use arena | Field, SERVER parameter, CLIENT parameter rejection, accepted CLIENT scalar return, each accepted SERVER `ROWS` column, rejected SERVER scalar `Single` return, direct `REF`, repeated written type, expression, result, coincident expression/result spans, and all ordering ties; each `CheckedTypeUseKind`, `CheckedValueTypeUse`, `CheckedObjectReferenceUse`, and `CheckedApplicationTypeUse` derive and accessor | One arena owns every accepted use exactly once. The active row emits only `Field`, SERVER `Parameter`, accepted CLIENT scalar `Return`, and accepted SERVER `ROWS`-column `Return`; a rejected SERVER scalar `Single` return emits no use. Both checking paths retain the exact accepted CLIENT-parameter diagnostic, and direct `REF` target spans retain `ObjectReference` uses in the same arena. Later body rows emit `Expression` and `Result` with the stated deterministic ordinals and kind ordering, never `CheckedExpressionId`. Family views borrow the exact arena item. |
| Relational value provenance | Intrinsic Boolean lookup from the exact Boolean contract; a checked standard library without that type; ordinary SERVER traversal with projections, predicate, nested children, and `ORDER BY`; `REF` and standard-value projection roots; identity-selected projections and selector; CLIENT Boolean literal; owned private relational-seam equality fixtures with equal compatibility scalars and different or equal supplied `TypeId` values, mixed present and absent provenance, and `REF` targets; a self-consistent core-verified non-golden checked standard library with a changed Boolean `TypeId`; public type-use model rustdoc | Boolean provenance comes from the checked `orna.kernel.value.boolean@1` fact's `TypeId`, never from a `StandardScalar` reverse lookup. A missing Boolean fact returns `ORNA0303` with `the checked standard library does not provide a Boolean value type` at each complete Boolean-producing expression span, with no checked bundle or use. SERVER traversal is projection order, predicate, then ordering, with parent-before-children `Expression` uses and one indexed `Result` for every projection root, including `REF`. Identity-selected traversal adds only equality, left `REF`, and right parameter selector expressions after projections, with no selector result; the `REF` and parameter read are `ObjectReference` uses. CLIENT literal evidence is `Expression 0` then `Result 0`. Private relational-seam tests prove equal compatibility scalars with different supplied IDs mismatch, equal supplied IDs match, mixed provenance returns exact `DiagnosticCode::TypeMismatch` (`ORNA0201`) `equality requires expressions with compatible types`, and `REF` equality compares its `CheckedTypeId` target. The public context cannot construct the different-ID duplicate-contract fixture, so that test is private and owned by the relational row. Legacy scalar equality, diagnostics, and artefact bytes remain exact. The non-golden fixture proves relational literal, equality, and CLIENT `Expression`/`Result` uses retain its changed Boolean ID. Public rustdoc states that `Expression` and `Result` are body-capable kinds with complete expression locations. |
| Mutation expression spans and value provenance | Exact `returning_ref_span`, `selector_equality_span`, and `selector_ref_span` syntax fields; mixed-case and trivia lossless parser cases, including a comment before selector `REF` closing parenthesis; INSERT, UPDATE, and DELETE traversal; right-hand-side values; selector equality, `REF`, and parameter children; `RETURNING REF` and `RETURNING TRUE`; complete spans; coincident `Expression`/`Result` order; private assignment-compatibility seam; ordered multi-Boolean missing diagnostics; changed Boolean `TypeId`; legacy artefacts | The syntax row retains `InsertStatement::returning_ref_span`, `UpdateStatement::selector_equality_span`, `UpdateStatement::selector_ref_span`, `UpdateStatement::returning_ref_span`, `DeleteStatement::selector_equality_span`, and `DeleteStatement::selector_ref_span` as exact complete expression spans. The mutation row carries supplied value `TypeId` provenance beside its private scalar compatibility value. It traverses INSERT right-hand sides then returned `REF`; UPDATE right-hand sides, selector equality, `REF`, parameter, then returned `REF`; and DELETE selector equality, `REF`, parameter, then returned `TRUE`, with zero-based parent-before-children `Expression` ordinals. Assignments and selectors have no `Result`. Returned `REF` is `ObjectReference` `Expression` plus `Result 0`; returned `TRUE` is `Value` `Expression` plus `Result 0`; every body location is complete and an equal-span `Expression` precedes its `Result`. Private seams prove parameter-to-field supplied-`TypeId` compatibility and mixed provenance. At the parameter span, INSERT reports `parameter {name} cannot be inserted into field {field} because their types do not match` and UPDATE reports `parameter {name} cannot be assigned to field {field} because their types do not match`. Selector `CheckedTypeId` validation reports `selector parameter {name} must use REF {target}` at its parameter span. Missing-Boolean tests assert exact `ORNA0303` diagnostics, messages, and spans: one INSERT Boolean-literal right-hand side; UPDATE Boolean right-hand sides in source order then selector equality; and DELETE selector equality then returned `TRUE`, with no bundle or uses. A changed non-golden Boolean `TypeId` appears in every distinct mutation Boolean path. Legacy mutation diagnostics and artefact bytes remain exact. |
| Standard resolution and compatibility | Every accepted thirteen qualified primary, thirteen qualified binding, and seventeen prelude type spelling; the two schema facts; quoted counterparts; unknown aliases; non-golden supported catalogue facts; each supported, unsupported, and duplicate representation contract | Accepted unquoted type spellings resolve to the checked `TypeId` with lossless source retained; schemas are not type spellings. Quoted input follows exact catalogue lookup. Compatibility is derived privately only from a resolved checked definition and its exact unique supported contract; unsupported-contract checking completes before duplicate-contract checking, and there is no `StandardScalar` to `TypeId` reverse lookup. |
| Function type-reference evidence | The initial empty arena and public model; a multi-unit source with interleaved CLIENT and SERVER functions whose insertion order differs from logical-path lexical order; SERVER parameters and `ROWS` columns, CLIENT scalar returns, and rejected SERVER scalar `Single` returns; mixed value and `REF` signature slots; repeated aliases resolving to one `TypeId` at distinct locations; a non-golden supplied `TypeId`; exact owners, ordinals, targets, locations, and accessor; unchanged `CheckedServerFunction::references()` and object references; empty application; CLIENT-parameter rejection; attempted deduplication, re-resolution, kind field, or preparation | The function-evidence row derives each `CheckedStandardTypeReference` from canonical declaration uses and retained source-order metadata without type re-resolution. It emits one entry for each accepted value declaration use, including repeated aliases with the same `TypeId`, and no entry for `REF` or rejected SERVER scalar `Single` returns. The flattened ordinal counts every accepted signature slot, so `REF` leaves valid gaps. A source-order seam orders entries by source-unit insertion ordinal, function declaration start, then ordinal rather than logical-path sort or SERVER/CLIENT resolver-family pass. The arena is separate from `CheckedServerFunction::references()`, `CheckedDefinitionReference`, and `ObjectReference`, has no kind field, preserves exact existing application and object references, and performs no preparation. |
| Standard application preparation | Every `PrepareStandardApplicationError` derive, typed fields, exact display, and source; incomplete report; base, standard-presence, catalogue, revision, digest, declaration-arena, body-arena, and reference-arena mismatches; private deterministic allocation retry for every new application catalogue, source, schema, and type ID | `prepare_standard_application` accepts only the distinct report and validates in the stated complete order before shared semantic preflight or allocation. Its private deterministic allocator yields the relevant reserved ID then a non-reserved ID for every new application `CatalogueRevisionId`, `SourceBundleId`, `SourceRevisionId`, copied `SourceUnitId`, `SchemaId`, and `TypeId`, proving retry before candidate catalogue, hash, and revision construction with no candidate collision. It creates no `TypeBindingId`; a later binding row rejects a same-class reserved collision after derivation. It never re-resolves source spelling. Only `Prepare { source }` exposes an error source. |
| Prepared standard-upgrade capability | `PreparedStandardUpgrade` private fields, derives, exact accessors, and absent owned extraction, conversion, dereference, and inner interfaces; every compiler-owned `StandardUpgradeIdentity` payload; exact compiler signature, error, display, source, and gate precedence; installed-standard, namespace, every compiler-visible identity class and ordering position, diagnostics, source mismatch, companion source-ID retry, catalogue, canonical hash, and revision failures | `prepare_checked_standard_upgrade` accepts only `CheckedStandardLibrary` and returns only `PreparedStandardUpgrade` or `PrepareStandardUpgradeError`. It has no installable capability, but `application_revision()` intentionally borrows normal kernel input. The permanent normal-apply guard rejects that borrowed version-2 candidate from version 1 and context-locks it in version 2. The compiler rejects an installed standard, then active namespace and visible identity conflicts from `CatalogueRevision` through `TypeBinding`, then diagnostics and source mismatch. It privately retries only its new companion application `CatalogueRevisionId`, `SourceBundleId`, `SourceRevisionId`, and copied `SourceUnitId` before candidate construction. It reuses version-1 application `SchemaId` and `TypeId` values already covered by active gate 3, and creates no `TypeBindingId`. Each earlier gate wins against hostile data for every later gate. `ReservedIdentity` retains the exact conflicting durable identity; `StandardLibraryRevision` is kernel-only. |
| Opaque standard-upgrade capability | `StandardUpgrade` private field, derives, exact accessors, and absent owned conversion, dereference, and inner interfaces; exact wrapper signature, transparent error, and retain, verify, check, prepare order | Only `prepare_standard_upgrade` constructs the opaque `StandardUpgrade`. It owns a `PreparedStandardUpgrade`, exposes its checked standard library, verified snapshot, and borrowed application revision, and returns retained, checked-source, and compiler-preparation failures through transparent `StandardUpgradeError` variants. It adds only `StandardLibraryError::Unavailable`; no raw or unnamed compiler route exists. |
| Normal-apply and atomic standard guards | Every `StandardContextIdentity` field, accessor, derive, and error field; version-1/version-2 transitions; matching and mismatching version-2 contexts; `ReservedStandardIdentity` field, display, and source; exact trusted-path, transaction, recovery, expected-base, identity-gate, materialisation, physical-plan, and write ordering; replay and repeat-preparation precedence; active and inactive-record ordering | Normal apply performs the permanent context guard after expected-base recovery and before materialisation, planning, or writes. It rejects every version transition and requires exact version-2 context equality. Atomic special apply accepts only `&orna_standard::StandardUpgrade`, then follows the stated exact order. A replay returns `ExpectedBaseMismatch` before collision scanning or writes. It completes the typed `ReservedStandardIdentity` gate before materialisation and physical planning, with `StandardLibraryRevision` first and every active-visible record in explicit family order before inactive records by durable ID bytes. Compiler construction already proves deployable core invariants, so special apply has no opaque-association or invariant gate. A repeated prepare against active version 2 returns `StandardLibraryAlreadyInstalled`. |
| Standard-revision recovery | Complete raw version-2 fixtures; the raw standard catalogue set separately to `EMPTY_APPLICATION_CATALOGUE_REVISION_ID`; complete version-1 fixtures; a complete table snapshot before and after each rejected recovery | The decoder is the first recovery path with a standard context. It preserves complete version-2 decoding and all version-1 recovery behaviour. A raw standard sentinel returns exact `RevisionInvariantError::ReservedOfflineCheckCatalogueRevision { revision: EMPTY_APPLICATION_CATALOGUE_REVISION_ID, role: ActiveOrRecoveredStandard }`, exact display, and no error source. It returns no active revision and performs no repair or write; the complete table snapshot remains unchanged. |
| Legacy compatibility boundary | Existing `check`, `prepare`, `ORNA0303`, scalar spelling, and legacy preparation tests; attempted standard-report preparation | Version-1 legacy checking and preparation remain unchanged and frozen. The standard application seam is distinct, introduces no compiler `Unavailable` error, and is removed or merged only by an explicit later caller-migration row. |

The later `feat(std): orchestrate standard upgrades` proof, not this compiler
checker matrix, proves wrapper-before-check production ordering. It calls
`orna_standard::verify_standard_library_snapshot` before
`check_standard_library_source`.

Tests must prove:

* the source-independent manifest contains the exact reserved IDs for
  `orna.std/1`, the standard catalogue, the later source bundle, revision, and
  unit, both schemas, all 13 types, and all 30 bindings; it also contains the
  exact schema and type names, 13 contracts, and each binding kind, name,
  derived ID, and direct target;
* the source-independent manifest contains no source bytes, origins, hashes,
  standard digest, `StandardLibrarySnapshot`, or
  `VerifiedStandardLibrarySnapshot` capability and therefore cannot establish
  standard authority by itself;
* all 30 qualified and prelude bindings resolve directly to the expected
  identity, with no alias-created identity or runtime chain;
* the retained source has the literal 3273 UTF-8 ASCII/LF bytes stated above,
  no BOM or CR, exactly one final LF, exactly 45 semicolons, and all 45 exact
  complete-declaration origins including `0..18`, `19..43`, and `3232..3272`;
* retained standard source parsing directly through `orna_syntax`, without the
  compiler, matches every source-independent manifest fact one-for-one,
  produces exactly 45 origins, and rejects a quoted or reordered fact;
* the framed source-unit content digest, bundle digest, source-revision digest,
  and standard-library digest equal the four literal accepted goldens, and the
  content digest is not confused with a raw file SHA-256;
* retained-source `StandardLibraryError` has only the six stated implemented
  variants, their exact `Display` text, and `Error::source` result; and
* the later standard-orchestration extension adds only `Unavailable` with its
  stated service, orchestration, and database ownership; the compiler-owned
  `PrepareStandardUpgradeError` owns `StandardLibraryAlreadyInstalled`,
  `NamespaceOccupied`, and `ReservedIdentity`; compiler checking and standard
  application context construction expose no `Unavailable` variant;
* `verify_standard_library_snapshot` checks the exact reserved
  `CatalogueRevisionId`, then the hard-coded accepted digest, then the core
  canonical verifier; it yields `VerifiedStandardLibrarySnapshot` for the
  accepted snapshot and rejects a core-accepted, self-consistent non-golden
  snapshot at the accepted-digest gate;
* case-insensitive unquoted spellings resolve while quoted spellings remain
  exact;
* object and value primary names and bindings share one collision-checked type
  namespace;
* ordinary source cannot own `std`, declare a kernel contract, export a
  qualified or prelude binding, or replace a reserved standard identity;
* compiler preparation rejects an already installed standard, a pre-existing
  active application `std` owner in schema, object-type, value-type,
  type-binding, then function snapshot order, or each visible active reserved
  collision from `CatalogueRevision` through `TypeBinding` in the stated
  identity order without changing the active pair. Upgrade preparation retries
  only newly allocated companion catalogue and source IDs, while retaining the
  existing version-1 schema and type identities covered by active gate 3;
* `prepare_standard_application` uses an injected private deterministic
  allocator that yields the relevant reserved ID and then a non-reserved ID for
  every new application catalogue, source, schema, and type allocation. It
  retries before candidate catalogue, hash, and revision construction, and no
  candidate ID collides with a reserved ID. Neither current preparation slice
  creates a `TypeBindingId`; a later binding row rejects a reserved collision
  after derivation;
* atomic standard apply separately rejects database-wide collisions after all
  active-visible records and before inactive records in the stated ordering;
* bootstrap and recovery reject every missing, duplicate, crossed, renamed,
  contract-mismatched, source-mismatched, or hash-mismatched standard fact;
  complete raw version-2 recovery fixtures preserve every standard fact and
  complete version-1 recovery remains unchanged; the later standard-revision
  decoder inserts the all-zero sentinel as the raw standard catalogue and
  returns exact `RevisionInvariantError::ReservedOfflineCheckCatalogueRevision`
  with `ActiveOrRecoveredStandard`, exact display, and no source, without an
  active result, repair, or write, while the complete table snapshot remains
  unchanged; and
* compiler checking consults the standard catalogue bindings rather than a
  source-spelling match over a Rust enum;
* every standard application type use is one `CheckedApplicationTypeUse` in
  the canonical arena, with exact kind, target or `TypeId`, source/unit order,
  and borrowed view lookup; public model rustdoc distinguishes exact written
  target locations for declaration uses from complete expression locations for
  body-capable `Expression` and `Result` uses; the active row emits field,
  SERVER parameter, accepted CLIENT scalar return, accepted SERVER `ROWS`
  return, and `REF` uses only, while a rejected SERVER scalar `Single` return
  emits no use. Body rows add the exact expression/result ordinals and
  coincident-span order without `CheckedExpressionId`;
* relational Boolean provenance comes only from the checked
  `orna.kernel.value.boolean@1` contract fact and its `TypeId`, never a
  `StandardScalar` reverse lookup; a missing Boolean fact reports `ORNA0303`
  `the checked standard library does not provide a Boolean value type` at the
  complete Boolean-producing expression span and returns no checked bundle or
  use;
* relational SERVER traversal records projections in declaration order, then
  the predicate, then `ORDER BY`, with parent-before-children complete
  expression spans, `Value` or `ObjectReference` evidence as applicable, and
  an indexed `Result` for each projection root including `REF`; identity-selected
  traversal records projections followed by equality, left `REF`, and
  right parameter as its three selector expressions with no selector result;
  every object-reference-valued expression, including that `REF` and parameter
  read, is `ObjectReference`; and a CLIENT Boolean literal records
  `Expression 0` then `Result 0`;
* owned private relational-seam tests prove that equal compatibility scalars
  with different supplied `TypeId` values mismatch, equal supplied IDs match,
  mixed present and absent provenance returns exact
  `DiagnosticCode::TypeMismatch` (`ORNA0201`) `equality requires expressions
  with compatible types`, and `REF` equality remains by `CheckedTypeId` target.
  The public context rejects the different-ID,
  duplicate-contract fixture, so this relational seam remains private. Legacy
  scalar equality, diagnostics, and artefact bytes remain exact;
* a self-consistent core-verified non-golden checked standard with a changed
  Boolean `TypeId` proves relational literal and equality evidence and CLIENT
  `Expression 0`/`Result 0` uses retain that supplied ID, with no canonical
  Boolean ID hard-code;
* mutation syntax retains exact `InsertStatement::returning_ref_span`,
  `UpdateStatement::selector_equality_span`,
  `UpdateStatement::selector_ref_span`,
  `UpdateStatement::returning_ref_span`,
  `DeleteStatement::selector_equality_span`, and
  `DeleteStatement::selector_ref_span`; mixed-case mutation keywords and
  trivia, including a comment before selector `REF` closing parenthesis, prove
  lossless source and exact complete source substrings for every new span;
  existing compiler mutation fixtures are updated for the append-only syntax
  fields without changing mutation semantics or expectations. Existing
  right-hand-side and returned-`TRUE` spans remain exact;
* standard mutation traversal records INSERT right-hand sides then returned
  `REF`; UPDATE right-hand sides, selector equality, selector `REF`, selector
  parameter, then returned `REF`; and DELETE selector equality, selector
  `REF`, selector parameter, then returned `TRUE`. It proves zero-based
  parent-before-children `Expression` ordinals, complete spans,
  `Expression`-before-`Result` equal-span order, no assignment or selector
  result, an `ObjectReference` returned-`REF` `Expression` and `Result 0`, and
  a `Value` returned-`TRUE` `Expression` and `Result 0`;
* owned private mutation assignment-compatibility seams prove equal
  compatibility scalars with different supplied `TypeId` values cannot be
  assigned, equal IDs can be assigned, and mixed present and absent provenance
  fails compatibility. INSERT retains `parameter {name} cannot be inserted
  into field {field} because their types do not match` and UPDATE retains
  `parameter {name} cannot be assigned to field {field} because their types do
  not match`, both at the parameter span. Selector `REF` retains its exact
  `selector parameter {name} must use REF {target}` parameter-span diagnostic.
  Missing Boolean proof asserts one INSERT Boolean-literal right-hand-side
  `ORNA0303` with the exact message and complete literal span, no bundle, and
  no use. It also asserts exact ordered `ORNA0303` vectors: UPDATE Boolean
  right-hand sides in source order followed by selector equality, and DELETE
  selector equality followed by `RETURNING TRUE`. Every diagnostic has its
  complete expression span, and each vector returns no bundle or use. A
  self-consistent core-verified non-golden checked standard with a changed
  Boolean `TypeId` retains that ID for every distinct mutation Boolean path:
  INSERT and UPDATE Boolean right-hand sides, UPDATE and DELETE selector
  equalities, and DELETE returned `TRUE`. Legacy mutation assignment
  compatibility, diagnostics, and artefact bytes remain exact;
* the exact private thirteen-contract compatibility table maps only after
  `TypeId` and checked-definition lookup; unsupported contracts complete before
  duplicate contracts; and no `StandardScalar` to `TypeId` reverse lookup
  exists;
* standard application contexts trust the unforgeable checked standard
  capability, complete schema identity, schema name, type identity,
  type-binding identity, unsupported-contract, then duplicate-contract gates
  in the specified source-order sequence, and expose each typed conflict with
  exact display and no error source;
* all six borrowed standard application views have their exact derives,
  accessors, manual debug contract, and single-arena behaviour; and
* the initially empty `CheckedStandardTypeReference` arena and its exact public
  model and accessors are present before function evidence. The later
  function-reference row proves a multi-unit source with CLIENT and SERVER
  declarations interleaved by source order, with insertion order deliberately
  different from logical-path lexical order; SERVER parameters and `ROWS`
  columns, CLIENT scalar returns, and rejected SERVER scalar `Single` returns;
  value and `REF` signature slots with valid flattened-ordinal gaps for `REF`;
  repeated aliases of one `TypeId` at distinct locations; and a
  self-consistent non-golden supplied `TypeId`. It asserts every owner,
  ordinal, target, location, and arena accessor, derives the vector from
  canonical declaration uses without re-resolution or dedupe, and proves the
  vector follows source-unit insertion ordinal rather than lexical path order
  or resolver family passes. It proves no kind field exists,
  `CheckedServerFunction::references()` and object references remain unchanged,
  an empty application has an empty arena, accepted CLIENT parameters remain
  rejected, and no preparation occurs;
* function type-reference and standard-application preparation evidence have
  their separate exact models, error contracts, and precedence gates;
* `PreparedStandardUpgrade` and `StandardUpgrade` have their exact capability
  fields, derives, accessors, and absent owned extraction, conversion,
  dereference, and inner interfaces; `application_revision()` is the deliberate
  borrow escape to normal kernel input; compiler preparation uses every stated
  gate and error in order, retries only companion catalogue and source IDs
  before candidate construction without creating a `TypeBindingId`, while only
  standard orchestration can create the opaque installable capability; and
* normal apply rejects version-1/version-2 transitions and mismatched
  version-2 `StandardContextIdentity` values before materialisation, planning,
  or writes; atomic standard apply accepts only the opaque standard capability,
  returns existing `ExpectedBaseMismatch` for replay before collision scanning
  or writes, and returns exact source-free `ReservedStandardIdentity` for the
  first database-wide collision, including inactive standard-library revisions
  and source records, in the stated deterministic order before materialisation
  or physical planning. It has no runtime opaque-association or invariant gate:
  compiler construction already proves deployable core invariants.
* every existing standard spelling remains source-compatible and every
  previously rejected non-public alias remains rejected unless this decision
  names it;
* version-1 artefact goldens and immutable version-1 canonical hashes remain
  byte-identical, while version 2 hashes include the exact standard revision,
  value definitions, bindings, and type references;
* Boolean physical storage and execution are selected only after the verified
  type definition maps to the exact allow-listed kernel contract;
* hostile PostgreSQL names and `search_path` cannot influence type resolution
  or storage selection; and
* apply, recovery, restart, CLIENT evaluation, SERVER execution, tamper
  rejection, and complete connection cleanup pass with the standard revision
  physically present.

Normal formatting, workspace tests, strict Clippy, rustdoc, diff, similarity,
and live PostgreSQL gates remain required. Assertions must use exact identities,
typed errors, diagnostic text, paths, and spans rather than only checking that
an error exists.

## Initial implementation sequence

The implementation uses these buildable commits. Each row names its complete
file ownership and the compatibility state required after the commit. No row
owns more than three files. The initial `orna-standard` `Cargo.toml` may declare
`orna-core`, `orna-syntax`, and `orna-compiler` so the later retained-source and
standard-orchestration rows remain within their two-file caps.

| Commit | Files | Required state after the commit |
| --- | --- | --- |
| `docs(types): define catalogue-backed standard scalars` | `docs/decisions/0016-catalogue-backed-standard-types.md`, `docs/decisions/README.md` | Decision and index only. |
| `feat(core): model catalogue value types` | `crates/orna-core/src/catalogue/types.rs`, `crates/orna-core/src/catalogue.rs`, `crates/orna-core/src/lib.rs` | Definitions, bindings, IDs, direct lookup, and legacy object projections compile; existing constructors and hashes are unchanged. |
| `refactor(postgres): fail closed on future definition evidence` | `crates/orna-kernel-postgres/src/apply.rs`, `crates/orna-kernel-postgres/tests/bootstrap.rs`, `crates/orna-kernel-postgres/tests/recovery.rs` | PostgreSQL production and test adapters explicitly reject unknown definition identities or reference targets; the currently exhaustive enums and all version-1 behaviour remain unchanged. |
| `feat(core): version standard and catalogue hashes` | `crates/orna-core/src/canonical_hash.rs`, `crates/orna-core/src/revision.rs`, `crates/orna-core/src/lib.rs` | Definition identities and reference targets become non-exhaustive and gain the append-only value-type variants. The derived `StandardLibraryRevisionId`, exact version-1 preservation tests, and version-2 models and goldens compile, but no active caller emits version 2. |
| `feat(std): define the standard manifest` | `crates/orna-standard/Cargo.toml`, `crates/orna-standard/src/lib.rs`, `Cargo.lock` | The source-independent manifest exposes the exact reserved IDs, 13 primary names and contracts, and 30 direct binding facts: 13 qualified plus 17 prelude. It contains no source bytes, origins, hashes, standard digest, `StandardLibrarySnapshot`, or `VerifiedStandardLibrarySnapshot`. The crate manifest predeclares `orna-core`, `orna-syntax`, and `orna-compiler` so the later source and orchestration rows stay within their file caps; no database state changes. |
| `feat(syntax): parse primitive value types` | `crates/orna-syntax/src/lib.rs`, `crates/orna-syntax/src/parser.rs`, `crates/orna-compiler/src/resolver.rs` | The parser losslessly accepts the privileged primitive and export forms. Before identity allocation, ordinary application checking enforces the complete protected-source table across existing and new declaration forms, including every primitive value declaration and type export, with the exact ordered `ORNA0303` diagnostic text and spans defined above. It cannot silently ignore one. No trusted standard-checking path exists yet. |
| `feat(std): retain the standard source` | `stdlib/std/types.orna`, `crates/orna-standard/src/lib.rs` | The crate retains the exact 3273-byte source and its 45 complete-declaration origins. It parses directly with `orna_syntax`, checks every unquoted source fact against the manifest, and locks the literal framed content, bundle, revision, and standard digests. It exposes `retained_standard_library_snapshot` and `verify_standard_library_snapshot` with the stated `StandardLibraryError` contract. The verifier checks reserved catalogue identity, then accepted digest, then the core canonical verifier. Tests prove each error field, display text, source result, gate precedence, and rejection of a core-accepted self-consistent non-golden snapshot. |
| `feat(compiler): check standard type source` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/resolver/model.rs`, `crates/orna-compiler/src/lib.rs` | A dedicated `check_standard_library_source(&VerifiedStandardLibrarySnapshot) -> Result<CheckedStandardLibrary, StandardLibraryCheckError>` path accepts only the core-verified carrier, has no `orna-standard` dependency, and checks the one stored retained unit, diagnostics, lossless declaration families, one-to-one catalogue facts, and exact origins in the stated order. It returns `verified_snapshot()` plus source-ordered checked schema, value-type, and binding fields with durable IDs from matched catalogue facts and exact origins. It never calls ordinary `check_parsed`, `prepare`, database, installation, type-use resolution, or `StandardScalar` conversion. Ordinary application checking retains the exact `ORNA0303` protection introduced with the syntax forms, and ordinary scalar resolution retains its compatibility adapter. |
| `feat(compiler): resolve types through std` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/resolver/model.rs`, `crates/orna-compiler/src/lib.rs` | Add `StandardApplicationCheckContext`, `StandardApplicationContextError`, `check_standard_application`, and the distinct standard application report and bundle. The path requires `CheckedStandardLibrary`, separately checks two standard schema facts, preserves all thirteen qualified primaries, thirteen qualified bindings, and seventeen prelude type spellings, derives private compatibility only after checked `TypeId` and contract lookup, records declaration field, SERVER parameter, accepted CLIENT scalar return, accepted SERVER `ROWS` column, and direct `REF` uses, preserves the exact CLIENT-parameter rejection and SERVER scalar `Single` rejection, and defines the initially empty `CheckedStandardTypeReference` arena and corresponding accessor rustdoc. It does not call legacy `check` or `prepare`, accept raw verified authority, emit compiler `Unavailable`, or prepare a report. |
| `test(compiler): scope declaration type-use assertions` | `crates/orna-compiler/src/lib.rs` | Make the two existing body-bearing, declaration-focused standard-application tests select only `Field`, `Parameter`, and `Return` entries from the canonical `uses()` arena before asserting exact declaration counts, kinds, source order, locations, borrowed views, scalar-free `Debug` output, and the empty successful CLIENT parameter view. The assertions pass unchanged before and after body evidence is added and make no assertion about `Expression` or `Result`. The relational row owns exact body-arena proof in `resolver.rs` and private equality and legacy-artefact proof in `relational.rs`. No production behaviour or public API changes. |
| `feat(compiler): preserve relational value provenance` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/relational.rs`, `crates/orna-compiler/src/resolver/model.rs` | Carry supplied `TypeId` provenance beside the private scalar compatibility value. Resolve intrinsic Boolean only from the checked `orna.kernel.value.boolean@1` contract fact, never from a scalar reverse map. A missing Boolean fact reports the stated `ORNA0303` at the complete Boolean-producing expression and returns no checked bundle or use; the six context gates remain unchanged. For SERVER queries, traverse projections in declared order, then predicate, then `ORDER BY`, with parent-before-children `Expression` uses and one indexed `Result` for every projection root, including `REF`. For identity-selected queries, after projections record equality, left `REF`, and right parameter as three selector expressions without a selector result. A CLIENT Boolean literal records `Expression 0` then `Result 0`. Value expressions use `Value`. Every object-reference-valued expression, including `REF` and the identity-selector parameter read, uses `ObjectReference`. All body locations are complete expression spans. Equality is by supplied `TypeId`, while `REF` equality is by `CheckedTypeId` target; there is no `CheckedExpressionId` use. Update public model rustdoc so declaration uses identify written target locations and body-capable `Expression`/`Result` kinds identify complete expression locations. This row owns private relational-seam tests because the public context rejects equal-contract, different-ID facts: equal compatibility scalar plus different supplied IDs mismatches, equal IDs match, mixed present and absent provenance returns exact `DiagnosticCode::TypeMismatch` (`ORNA0201`) `equality requires expressions with compatible types`, and legacy scalar equality, diagnostics, and artefact bytes remain exact. A self-consistent core-verified non-golden checked standard with a changed Boolean `TypeId` proves relational literal/equality and CLIENT `Expression`/`Result` uses retain that supplied ID. Standard application preparation remains unavailable. |
| `feat(syntax): retain mutation expression spans` | `crates/orna-syntax/src/lib.rs`, `crates/orna-syntax/src/parser.rs`, `crates/orna-compiler/src/mutation.rs` | Add `InsertStatement::returning_ref_span`, `UpdateStatement::selector_equality_span`, `UpdateStatement::selector_ref_span`, `UpdateStatement::returning_ref_span`, `DeleteStatement::selector_equality_span`, and `DeleteStatement::selector_ref_span`, each a `SourceSpan`. Returned-`REF` and selector-`REF` spans start at `REF` and end after `)`. Selector-equality spans start at `REF` and end after the selector parameter. They retain every intervening trivia byte, including a comment before selector `REF` closing parenthesis. Existing `MutationValue::span()` and `DeleteStatement::returning_true` retain right-hand-side and returned-`TRUE` spans. Mixed-case and trivia parser tests prove exact lossless text and each literal source range. Existing compiler mutation test fixtures initialise the new fields with the exact synthetic ranges and preserve all previous expectations; no compiler production behaviour changes. |
| `feat(compiler): preserve mutation value provenance` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/mutation.rs`, `crates/orna-compiler/src/resolver/model.rs` | Carry supplied standard `TypeId` provenance beside the private scalar compatibility value for mutation fields, parameters, literals, nulls, assignments, selectors, and results. Resolve intrinsic Boolean only from the checked `orna.kernel.value.boolean@1` contract fact. Never reverse-map a scalar to `TypeId`. A missing Boolean reports the stated `ORNA0303` at every complete Boolean-producing mutation expression and returns no checked bundle or use. Tests require one INSERT Boolean-literal right-hand-side diagnostic with the exact message and complete literal span, no bundle, and no use; exact ordered multi-Boolean vectors then require UPDATE Boolean right-hand sides in source order, then selector equality, and DELETE selector equality, then returned `TRUE`. Traverse INSERT right-hand-side values then `RETURNING REF`; UPDATE right-hand-side values, selector equality, selector `REF`, selector parameter, then `RETURNING REF`; and DELETE selector equality, selector `REF`, selector parameter, then `RETURNING TRUE`. Allocate zero-based parent-before-children `Expression` ordinals. Assignments and selectors have no `Result`. INSERT and UPDATE returned `REF` are `ObjectReference` `Expression` plus `Result { ordinal: 0 }`, not value results. DELETE returned `TRUE` is `Value` `Expression` plus `Result { ordinal: 0 }`. All locations use exact complete spans and equal-span `Expression` precedes `Result`. Assignment compatibility is by supplied `TypeId`; selector `REF` validation is by `CheckedTypeId` target. Private mutation seams prove compatible and incompatible supplied IDs and mixed provenance. At the parameter span, INSERT reports `parameter {name} cannot be inserted into field {field} because their types do not match`, UPDATE reports `parameter {name} cannot be assigned to field {field} because their types do not match`, and selector validation reports `selector parameter {name} must use REF {target}`. A self-consistent core-verified non-golden checked standard with a changed Boolean `TypeId` retains that ID in every distinct mutation Boolean path: INSERT and UPDATE Boolean right-hand sides, UPDATE and DELETE selector equalities, and DELETE returned `TRUE`; legacy mutation assignment compatibility, diagnostics, and artefact bytes remain exact. The temporary prior relational-only evidence cannot prepare a standard application. |
| `test(compiler): release staged function-reference assertion` | `crates/orna-compiler/src/lib.rs` | Remove only the stale empty-standard-reference-arena assertion from the existing non-empty SERVER and CLIENT signature fixture. Preserve the valid empty-application arena assertion. This row makes no production or public API change. The later two-file function-reference row owns the exact non-empty vector proof in `resolver.rs` and the public model rustdoc in `resolver/model.rs`. |
| `feat(compiler): reference standard function types` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/resolver/model.rs` | Populate the existing compiler-owned `CheckedStandardTypeReference` arena from canonical declaration uses and retained source-order metadata, without re-resolution. Emit repeated `ValueType`/`NamedType` evidence for every accepted SERVER parameter and `ROWS` column, and each accepted CLIENT scalar return. Rejected SERVER scalar `Single` returns emit no evidence. Use one flattened zero-based signature ordinal: SERVER parameters first, then SERVER `ROWS` columns; CLIENT scalar return is ordinal `0`; every direct `REF` slot advances its function's ordinal but emits no standard reference, so gaps are valid. Preserve exact CLIENT-parameter rejection. Order the vector by source-unit insertion ordinal, function declaration start, then ordinal through the shared source-order seam, never logical-path sort or a SERVER/CLIENT resolver-family pass. The reference model has no kind field. It remains separate from `CheckedServerFunction::references()`, `CheckedDefinitionReference`, and `ObjectReference`; existing application and object references stay exact. `resolver.rs` owns a multi-unit interleaved CLIENT/SERVER proof whose insertion order deliberately differs from logical-path lexical order, plus mixed value/`REF` gaps, repeated-alias distinct locations, non-golden `TypeId`, exact owner/accessor, empty-application, CLIENT-rejection, one-to-one/no-dedupe, and no-preparation proof. `resolver/model.rs` owns rustdoc that changes the initial-empty arena contract to its populated signature-evidence contract. |
| `fix(postgres): guard standard context transitions` | `crates/orna-kernel-postgres/src/apply.rs`, `crates/orna-kernel-postgres/src/lib.rs`, `crates/orna-kernel-postgres/tests/apply.rs` | Define `StandardContextIdentity` and exact source-free `PostgresKernelError` transition and mismatch variants. After expected-base recovery, normal apply rejects every version-1/version-2 transition and requires equal version-2 contexts before materialisation, planning, or writes. The guard rejects a borrowed version-2 candidate from version 1 and context-locks it in version 2. |
| `feat(compiler): prepare standard applications` | `crates/orna-compiler/src/prepare.rs`, `crates/orna-compiler/src/resolver/model.rs`, `crates/orna-compiler/src/lib.rs` | Add `prepare_standard_application(&StandardApplicationCheckReport, RevisionPair, &ActiveDatabaseRevision) -> Result<DeployableRevision, PrepareStandardApplicationError>` with the exact report-completeness, base, active-standard, arena-evidence, semantic-preflight, then allocation order. Its private retry allocator excludes same-class reserved IDs for every new application catalogue, source, schema, and type ID before candidate construction, with deterministic reserved-then-non-reserved proof and no public retry error. It creates no `TypeBindingId`; a later binding row rejects a post-derivation reserved collision. It accepts no legacy `CheckReport`, preserves report separation, and owns all standard-application preparation rejection and durable evidence proof. |
| `refactor(types): remove scalar naming authority` | `crates/orna-core/src/types.rs`, `crates/orna-compiler/src/resolver.rs` | Only after callers migrate to the distinct standard application path, remove public `StandardScalar::from_source_spelling`, `canonical_name`, `type_id`, and `ScalarResolutionError`. Diagnostics render names from checked catalogue definitions or retained source, while exact representation matching remains internal. |
| `feat(client): prepare catalogue Boolean constants` | `crates/orna-compiler/src/prepare.rs`, `crates/orna-compiler/src/resolver/model.rs` | Work ADR 0015 preparation uses the canonical Boolean identity and exact one-reference sequence; no evaluator or database consumer exists yet. |
| `feat(client): evaluate catalogue Boolean constants` | `crates/orna-client/Cargo.toml`, `crates/orna-client/src/lib.rs`, `Cargo.lock` | The local evaluator verifies canonical hash version 1 or 2 as appropriate, requires the exact standard revision and one-reference sequence for version-2 CLIENT revisions, and retains the exact result and error contract. |
| `feat(compiler): prepare checked standard upgrades` | `crates/orna-compiler/src/prepare.rs`, `crates/orna-compiler/src/lib.rs` | Define and re-export private-field `PreparedStandardUpgrade` and compiler-owned payload-bearing `StandardUpgradeIdentity`, then implement `prepare_checked_standard_upgrade(&CheckedStandardLibrary, &ActiveDatabaseRevision) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError>`. It has the exact installed-standard, active-source, reserved-identity, and nested catalogue, canonical, and revision error contract. After the active source gates and before candidate construction, its private retry allocator excludes reserved same-class IDs only for new companion application catalogue and source IDs. It reuses version-1 application schema and type identities already covered by active gate 3, and creates no `TypeBindingId`. It produces no opaque installable capability; its application revision is a normal-input borrow guarded by the permanent PostgreSQL transition rule. |
| `feat(std): orchestrate standard upgrades` | `crates/orna-standard/src/lib.rs` | Define opaque private-field `StandardUpgrade`, re-export `StandardUpgradeIdentity`, and expose `prepare_standard_upgrade(&ActiveDatabaseRevision) -> Result<StandardUpgrade, StandardUpgradeError>`. It calls retained snapshot construction, accepted verification, standard-source checking, and the public `orna_compiler::prepare_checked_standard_upgrade` seam in that exact order. It adds only boundary-owned `StandardLibraryError::Unavailable` and maps retained, checked-source, and compiler-preparation failures through transparent `StandardUpgradeError` variants. Its proof owns wrapper-before-check ordering. The crate has no database authority. |
| `build(postgres): add standard-upgrade dependency` | `crates/orna-kernel-postgres/Cargo.toml`, `Cargo.lock` | Add the normal `orna-standard` dependency required only by atomic special apply. The dependency graph is `postgres -> standard -> compiler -> core`; no reverse dependency exists. |
| `feat(postgres): store standard catalogue types` | `crates/orna-kernel-postgres/migrations/0007_catalogue_types.sql`, `crates/orna-kernel-postgres/src/bootstrap.rs` | Bare bootstrap installs only schema support and still recovers all v1 databases exactly. |
| `feat(postgres): decode standard revisions` | `crates/orna-kernel-postgres/src/recovery.rs`, `crates/orna-kernel-postgres/src/recovery/functions.rs`, `crates/orna-kernel-postgres/tests/recovery.rs` | This is the first recovery path with a standard context. It verifies complete raw version-2 fixtures and still recovers version 1 exactly. Its live raw standard-catalogue sentinel test inserts `EMPTY_APPLICATION_CATALOGUE_REVISION_ID` and requires exact `RevisionInvariantError::ReservedOfflineCheckCatalogueRevision { revision: EMPTY_APPLICATION_CATALOGUE_REVISION_ID, role: ActiveOrRecoveredStandard }`, exact display, and no error source, active revision, repair, or write, with an unchanged complete table snapshot. No public production mutation can create version-2 active state. |
| `feat(storage): lower verified value contracts` | `crates/orna-core/src/physical.rs`, `crates/orna-kernel-postgres/src/physical.rs`, `crates/orna-kernel-postgres/src/physical/verify.rs` | Physical planning and verification start from a verified contract; generated SQL and existing physical identities remain exact. |
| `feat(server): execute verified value contracts` | `crates/orna-kernel-postgres/src/server_runtime.rs`, `crates/orna-kernel-postgres/src/server_execution.rs`, `crates/orna-kernel-postgres/src/server_mutation_execution.rs` | Runtime adapters start from the same contract and preserve every existing plan byte, bind, result, and error. |
| `feat(postgres): apply standard upgrades` | `crates/orna-kernel-postgres/src/apply.rs`, `crates/orna-kernel-postgres/src/lib.rs` | After compiler, recovery, storage, and execution consumers are ready, `apply_standard_upgrade` accepts only `&orna_standard::StandardUpgrade`. Its trusted transaction path locks and recovers, checks expected base, scans database-wide identities, materialises, plans physically, then writes. Compiler construction already proves deployable core invariants, so no opaque-association or invariant gate exists. A replay returns `ExpectedBaseMismatch` before scanning or writes. The identity scan returns `ReservedStandardIdentity { identity }` for the first active-visible or inactive collision, including an inactive standard-library revision, in the stated order. Normal apply cannot transition standard context. |
| `feat(server): open standard-backed databases` | `crates/orna-server/Cargo.toml`, `crates/orna-server/src/lib.rs`, `Cargo.lock` | The host opener composes bare bootstrap, exact standard preparation, atomic standard apply when required, and verified recovery. It does not return a normal application database handle until `orna.std/1` is active. |
| `test(postgres): prove the standard lifecycle` | `crates/orna-kernel-postgres/tests/apply.rs`, `crates/orna-kernel-postgres/tests/recovery.rs`, `justfile` | Fresh install, v1 upgrade, replay, restart, tamper rejection, and exact physical storage pass on PostgreSQL 18. |
| `test(postgres): preserve standard execution` | `crates/orna-kernel-postgres/tests/server_execution.rs`, `crates/orna-kernel-postgres/tests/server_mutation_execution.rs` | Existing SERVER and mutation behaviour is byte- and value-identical under the installed standard revision. |
| `test(client): recover and evaluate standard Boolean constants` | `crates/orna-client/src/lib.rs`, `crates/orna-kernel-postgres/tests/recovery.rs` | Apply, source-only replay, semantic change, restart, tamper rejection, and local evaluation prove the exact CLIENT version-2 context and leave no PostgreSQL session open. |

The later replacement of public `ResolvedType::Scalar` with
`ResolvedType::Value(TypeId)` is a separate compatibility migration. Before
adding the enum variant, consumers are converted in one-to-three-file commits
to catalogue-aware accessors so the workspace remains exhaustive and
buildable. Existing executable decoders remain dual-read. This decision does
not permit a single workspace-wide mechanical commit.

A temporary compatibility adapter is acceptable only while both its input and
output are exact-tested against the one canonical `TypeId`.

## Deferred surface

This decision does not yet accept:

* application-defined aliases or prelude exports;
* user-selected kernel contracts or definitions in the protected `std`
  namespace;
* exact public DDL for enums, structured values, domains, opaque values,
  algebraic values, collections, generics, constraints, constructors, casts,
  or subtyping;
* automatic assignment compatibility between nominal value types that share a
  representation;
* standard-library replacement, package distribution, online upgrade, or
  simultaneous use of two standard revisions;
* new scalar operations, implicit conversions, ordering, collation, or
  PostgreSQL operator inheritance;
* custom value-type storage, indexes, migration, wire transport, or CLIENT
  runtime materialisation; or
* changing an existing standard `TypeId`, primary name, binding, contract, or
  version-1 encoding.

Those features require accepted semantics and fail-closed implementation. The
catalogue model deliberately makes future custom value types possible without
claiming that their source, storage, or execution rules are already settled.

## Precedence

This record makes catalogue-backed standard types concrete from the generic
value-type direction in `spec/docs/10-ui-type.md`,
`spec/docs/12-object-relational-model.md`, `spec/docs/22-ddl-reference.md`,
`spec/docs/28-ebnf-ast.md`, spec ADR 0003, and spec ADR 0012.

It supersedes work ADR 0002's exclusive alias set only to add the qualified
`std.*` exports and canonical `std.types.*` definitions listed here. The only
unqualified compatibility aliases remain `BOOL`, `INT`, `TEXT`, and `BYTES`;
names such as `std.BOOL`, `std.INT`, `std.TEXT`, and `std.BYTES` are not added.
The same-`TypeId` rule and lossless public spellings remain unchanged.

It supersedes work ADR 0015 where that decision fixes
`ResolvedType::Scalar`, `StandardScalar`, `BOOLEAN|BOOL` as the complete return
spelling set, an empty definition-reference sequence, preparation of no
references, evaluator rejection of every reference, and proof of those empty
sequences. The replacement is the catalogue Boolean `TypeId`, the qualified
spellings and exact one-reference sequence above. The CLIENT body, artefact
bytes, diagnostics for genuinely non-Boolean returns, security, and evaluation
result contract remain unchanged.

It expands only the signature-reference prefix described by work ADRs 0005
and 0007 through 0012. Their body-evidence kinds and relative order remain
unchanged. Version-1 immutable function revisions and their historical
reference rows are not rewritten.

It narrows work ADR 0003's complete-source rule by defining the immutable,
separately revised standard-library overlay. Application source remains
complete and authoritative for application-owned definitions. The standard
source is complete and authoritative for the protected `std` definitions.

It narrows the bare-database option in
`spec/docs/06-bootstrapping-recovery.md`: bare kernel recovery remains
available, but normal Orna source checking and execution require successful
installation of the pinned standard revision.

For all other subjects, prior work ADRs and the canonical specification remain
unchanged.
