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

The standard definitions are implemented by real Orna standard-library
source. The initial privileged declaration shape is:

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
```

This decision accepts that closed declaration family for the standard
library. `AS VALUE PRIMITIVE` creates a primitive value type. `KERNEL CONTRACT`
identifies its irreducible representation and operations. `EXPORT TYPE ... AS`
creates a qualified binding. `EXPORT TYPE ... TO PRELUDE` creates an
unqualified public binding. Preparation flattens both export forms to the
primary definition's `TypeId`.

`KERNEL CONTRACT`, mutation of `std`, and prelude exports are privileged
standard-library operations. Ordinary application source cannot declare a
kernel contract, create or replace a definition in `std`, export a prelude
name, or change a reserved standard `TypeId`. The compiler reports a
human-readable `ORNA0303` diagnostic at the reserved declaration or modifier
before preparation or storage changes.

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

Normal syntax errors still take precedence over these semantic diagnostics.
After syntax succeeds, the compiler reports a reserved `std` owner first, a
kernel contract second, and a prelude export third. It does not emit later
protection diagnostics for the same declaration after the first one.
Any diagnostic rejects the complete candidate and no standard or application
catalogue state changes.

A malformed private checked or prepared value that supplies or replaces a
reserved `TypeId` has no source diagnostic. It fails through the typed
`StandardLibraryError::ReservedTypeIdentity` rule, whose public display is
`the standard library has an unexpected type identity`, before
identity allocation or durable writes.

An existing version-1 application catalogue may predate the protected
namespace. Standard installation does not reinterpret or overwrite it. If an
application definition already has `std` as its first semantic name part,
`prepare_standard_upgrade` returns
`StandardLibraryError::NamespaceOccupied`, displayed as
`the application catalogue already uses the reserved std namespace`. If an
application identity collides with any reserved manifest identity, it returns
`StandardLibraryError::ReservedIdentity`, displayed as
`the application catalogue conflicts with a reserved standard library identity`.
Both failures occur before an upgrade value or durable write exists. This
decision does not invent an automatic rename for either conflict.

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

The bootstrap manifest is the smallest trusted input needed to compile the
source that defines the primitive types. It contains only reserved identities,
the expected primary names, the allow-listed versioned kernel contracts, and
the expected source and semantic digests. It does not provide a second set of
public type semantics.

### Canonical standard identities

`orna.std/1` uses these exact sixteen-byte identities, written as fifteen zero
bytes followed by the listed byte:

| Identity class | Member | Final byte |
| --- | --- | --- |
| `StandardLibraryRevisionId` | `orna.std/1` | `01` |
| `SourceBundleId` | standard source bundle | `01` |
| `SourceRevisionId` | standard source revision | `01` |
| `SourceUnitId` | `std/types.orna` | `01` |
| `SchemaId` | `std` | `01` |
| `SchemaId` | `std.types` | `02` |

The identity classes are distinct Rust and durable types, so equal byte arrays
do not cross identity domains.

The standard source revision has no parent. Its one source unit has ordinal
`0`, logical path `std/types.orna`, and the exact embedded UTF-8 content. Its
content, bundle, and revision hashes use the existing source hash contracts
without a special standard-library branch.

Each binding identity is derived rather than allocated. Compute SHA-256 over
the domain bytes `ornadb.id/type-binding/v1\0`, then one binding-kind byte
(`01` qualified, `02` prelude), then its normalised name payload. A qualified
name payload is
encoded as a big-endian `u32` part count followed by each UTF-8 part as a
big-endian `u32` byte length and its bytes. A prelude spelling uses the same
encoding over its lower-case keyword words, so `CHARACTER LARGE OBJECT` is the
three parts `character`, `large`, and `object`. The first sixteen digest bytes
form `TypeBindingId`. Bootstrap rejects a collision between two bindings.

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

The new `orna-standard` orchestration crate owns normal standard installation.
Its public `prepare_standard_upgrade(active)` operation embeds and checks the
exact standard source and manifest, reconstructs the active application source,
and returns one closed `StandardUpgrade`. That value contains the standard
revision, one companion application `SourceRevision`, the version-2 catalogue,
and every new or reused function revision and reference needed by the
transition. The PostgreSQL kernel accepts it only through
`apply_standard_upgrade`, which verifies and commits the entire value in one
read-write repeatable-read transaction.

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
Normal checking, preparation, apply, SERVER execution, and CLIENT evaluation
require that view. They return a typed unavailable-standard-library error for
a bare version-1 database rather than falling back to hard-coded names.
The shared rule is `StandardLibraryError::Unavailable`, displayed as
`the standard library is not installed`; each public boundary retains its
normal operation context and nests that source where it already exposes one.

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
the exact target span. A field, parameter, return, expression, or result value
retains the resolved `TypeId`; later stages do not re-resolve its source name.
A function signature or body use of a standard value type emits the normal
ordered `NamedType` definition reference to the canonical value-type `TypeId`
at the written type span. Prelude and qualified spellings therefore produce
the same reference target with different retained source origins.
`DefinitionReferenceKind::NamedType` consequently means a dependency on any
named object or value type. `ObjectReference`, `QueryObject`, `WriteObject`, and
`REF` remain restricted to object definitions.

SERVER signature evidence remains the prefix before the body evidence fixed by
work ADRs 0005 and 0007 through 0012. That prefix scans parameters in ordinal
order and then a single return or `ROWS` columns in ordinal order. Each direct
value type contributes `NamedType` at its written type; each `REF` continues to
contribute only its existing `ObjectReference` at the written target. Repeated
written type uses produce repeated ordered references. The body sequences in
those decisions retain their exact relative order after this expanded prefix.

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

Tests must prove:

* the embedded `orna.std/1` source and manifest produce all thirteen exact
  primary definitions with the existing reserved `TypeId` values;
* every qualified and prelude binding resolves directly to the expected
  identity, with no alias-created identity or runtime chain;
* case-insensitive unquoted spellings resolve while quoted spellings remain
  exact;
* object and value primary names and bindings share one collision-checked type
  namespace;
* ordinary source cannot own `std`, declare a kernel contract, export a
  prelude name, or replace a reserved standard identity;
* standard installation rejects a pre-existing application `std` owner or
  reserved identity collision without changing the active pair;
* bootstrap and recovery reject every missing, duplicate, crossed, renamed,
  contract-mismatched, source-mismatched, or hash-mismatched standard fact;
* compiler checking consults the standard catalogue bindings rather than a
  source-spelling match over a Rust enum;
* each accepted type use retains the exact resolved `TypeId`, source path, and
  span, produces the exact type-reference evidence required by its definition,
  and a value type cannot be used as a `REF` target;
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
file ownership and the compatibility state required after the commit.

| Commit | Files | Required state after the commit |
| --- | --- | --- |
| `docs(types): define catalogue-backed standard scalars` | `docs/decisions/0016-catalogue-backed-standard-types.md`, `docs/decisions/README.md` | Decision and index only. |
| `feat(core): model catalogue value types` | `crates/orna-core/src/catalogue/types.rs`, `crates/orna-core/src/catalogue.rs`, `crates/orna-core/src/lib.rs` | Definitions, bindings, IDs, direct lookup, and legacy object projections compile; existing constructors and hashes are unchanged. |
| `feat(core): version standard and catalogue hashes` | `crates/orna-core/src/canonical_hash.rs`, `crates/orna-core/src/revision.rs` | Exact version-1 hash tests remain; version-2 models and goldens compile but no active caller emits them. |
| `feat(std): define the standard manifest` | `crates/orna-standard/Cargo.toml`, `crates/orna-standard/src/lib.rs`, `Cargo.lock` | The exact IDs, contracts, names, bindings, and digest are available in memory; no database state changes. |
| `feat(syntax): parse primitive value types` | `crates/orna-syntax/src/lib.rs`, `crates/orna-syntax/src/parser.rs` | Lossless privileged declarations parse and recover; existing application semantics still fail closed. |
| `feat(std): retain the standard source` | `stdlib/std/types.orna`, `crates/orna-standard/src/lib.rs` | The manifest verifies the exact parsed source, spans, IDs, and digest in memory. |
| `feat(compiler): check standard type source` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/resolver/model.rs`, `crates/orna-compiler/src/lib.rs` | Trusted standard checking and exact application protection diagnostics work; ordinary scalar resolution still uses its compatibility adapter. |
| `feat(compiler): resolve types through std` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/resolver/model.rs` | Public and qualified scalar names resolve through an explicitly supplied verified standard snapshot to `TypeId`; compilation without that snapshot returns `StandardLibraryError::Unavailable`. No database can install the snapshot yet. |
| `refactor(types): remove scalar naming authority` | `crates/orna-core/src/types.rs`, `crates/orna-compiler/src/resolver.rs` | Public `StandardScalar::from_source_spelling`, `canonical_name`, `type_id`, and `ScalarResolutionError` are removed. Diagnostics render names from verified catalogue definitions or retained source, while exact representation matching remains internal. |
| `feat(compiler): reference standard function types` | `crates/orna-compiler/src/resolver.rs`, `crates/orna-compiler/src/resolver/model.rs`, `crates/orna-compiler/src/prepare.rs` | Standard types in signatures emit exact `ValueType`/`NamedType` evidence and affected functions receive semantic-hash v2 revisions. A checked compatibility projection is derived only after binding resolution. |
| `feat(client): prepare catalogue Boolean constants` | `crates/orna-compiler/src/prepare.rs`, `crates/orna-compiler/src/resolver/model.rs` | Work ADR 0015 preparation uses the canonical Boolean identity and exact one-reference sequence; no evaluator or database consumer exists yet. |
| `feat(client): evaluate catalogue Boolean constants` | `crates/orna-client/Cargo.toml`, `crates/orna-client/src/lib.rs`, `Cargo.lock` | The local evaluator verifies canonical hash version 1 or 2 as appropriate, requires the exact standard revision and one-reference sequence for version-2 CLIENT revisions, and retains the exact result and error contract. |
| `feat(compiler): prepare standard revisions` | `crates/orna-compiler/src/prepare.rs`, `crates/orna-core/src/revision.rs` | A closed `StandardUpgrade` is produced with the companion source and v2 catalogue; no kernel consumer exists yet. |
| `feat(std): orchestrate standard upgrades` | `crates/orna-standard/src/lib.rs`, `crates/orna-compiler/src/prepare.rs` | `prepare_standard_upgrade` rechecks exact active source and returns the complete closed upgrade; the crate has no database authority. |
| `feat(postgres): store standard catalogue types` | `crates/orna-kernel-postgres/migrations/0007_catalogue_types.sql`, `crates/orna-kernel-postgres/src/bootstrap.rs` | Bare bootstrap installs only schema support and still recovers all v1 databases exactly. |
| `feat(postgres): decode standard revisions` | `crates/orna-kernel-postgres/src/recovery.rs`, `crates/orna-kernel-postgres/src/recovery/functions.rs` | Recovery verifies complete raw v2 fixtures and still recovers v1 exactly, but no public production mutation can create v2 active state. |
| `feat(storage): lower verified value contracts` | `crates/orna-core/src/physical.rs`, `crates/orna-kernel-postgres/src/physical.rs`, `crates/orna-kernel-postgres/src/physical/verify.rs` | Physical planning and verification start from a verified contract; generated SQL and existing physical identities remain exact. |
| `feat(server): execute verified value contracts` | `crates/orna-kernel-postgres/src/server_runtime.rs`, `crates/orna-kernel-postgres/src/server_execution.rs`, `crates/orna-kernel-postgres/src/server_mutation_execution.rs` | Runtime adapters start from the same contract and preserve every existing plan byte, bind, result, and error. |
| `feat(postgres): apply standard upgrades` | `crates/orna-kernel-postgres/src/apply.rs`, `crates/orna-kernel-postgres/src/lib.rs` | After compiler, recovery, storage, and execution consumers are ready, one explicit API verifies and atomically applies `StandardUpgrade`; normal apply rejects a bare database. |
| `feat(server): open standard-backed databases` | `crates/orna-server/Cargo.toml`, `crates/orna-server/src/lib.rs` | The host opener composes bare bootstrap, exact standard preparation, atomic standard apply when required, and verified recovery. It does not return a normal application database handle until `orna.std/1` is active. |
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
