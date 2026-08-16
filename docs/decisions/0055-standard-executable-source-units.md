# ADR 0055: `orna.std/2` Is an Immutable Executable Source Snapshot

**Status:** Accepted

## Decision

`orna.std/2` is the first immutable, verified standard-library snapshot that
contains executable normal Orna source. It has two ordered source units. It
does not modify `orna.std/1`.

`orna.std/1` remains a valid version-1, one-unit, type-only snapshot. Its
identities, source bytes, digest contract, verifier, persistence records, and
recovery behaviour remain unchanged. Code that selects
`StandardLibraryDigestVersion::Version1` continues to reject functions. A
version-1 database does not infer, upgrade, or substitute `orna.std/2`.

The existing singular V1 standard exports remain V1 exports:
`STANDARD_LIBRARY_VERSION_IDENTITY`, `STANDARD_LIBRARY_REVISION_ID`,
`STANDARD_CATALOGUE_REVISION_ID`, `STANDARD_SOURCE_BUNDLE_ID`,
`STANDARD_SOURCE_REVISION_ID`, `STANDARD_SOURCE_UNIT_ID`,
`standard_library_manifest`, `retained_standard_library_snapshot`,
`verify_standard_library_snapshot`, and `prepare_standard_upgrade`. They do
not silently change meaning. V2 adds explicitly named V2 constants,
`standard_library_v2_manifest`, `retained_standard_library_v2_snapshot`,
`verify_standard_library_v2_snapshot`, and
`prepare_standard_upgrade_v1_to_v2`. Bootstrap selects V2 only through that
last explicit path after it has retained and verified V1. No public default
changes from V1 to V2 in this decision.

`orna.std/2` has a new immutable standard-library revision, a new standard
catalogue revision, a new source bundle, a new source revision, and new
source-unit identities. Its source revision is the append-only child of the
`orna.std/1` source revision. A source-unit identity is globally durable.
Therefore the retained `std/types.orna` bytes receive a new unit identity in
this new snapshot; they do not reuse the `orna.std/1` source-unit identity.

All identities in this decision are sixteen exact bytes in network order. They
are not UUIDs.

| Fact | Exact value |
| --- | --- |
| Standard version | `orna.std/2` |
| Language version | `orna.language/1` |
| `StandardLibraryRevisionId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02` |
| Standard `CatalogueRevisionId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02` |
| Standard `SourceBundleId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02` |
| Standard `SourceRevisionId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02` |
| `std/types.orna` `SourceUnitId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02` |
| `std/invoke.orna` `SourceUnitId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 03` |
| `std.invoke` `SchemaId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 03` |
| `std.invoke.echo` `FunctionId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 10` |
| `std.invoke.echo.p_value` `ParameterId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 10` |
| `std.invoke.echo` `FunctionRevisionId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 10` |

The ordered source bundle is exact:

| Ordinal | Source unit | Unit identity | Required content |
| --- | --- | --- | --- |
| `0` | `std/types.orna` | `...02` | the retained `orna.std/1` type declarations, byte-for-byte |
| `1` | `std/invoke.orna` | `...03` | the exact `std.invoke.echo` declaration below |

The order is part of the V2 source-bundle digest. A reordered, missing,
additional, duplicate, renamed, or byte-modified unit is not `orna.std/2`.
Source-unit content digests use the existing source-content contract. The
bundle digest consumes each complete retained unit in ascending ordinal order.
The source-revision digest consumes the V2 bundle identity, the exact V1
standard source-revision identity `...01` as its parent, and the bundle
digest. The V2 standard digest consumes its version number,
standard revision identity, source revision identity and digest, language
version, complete standard catalogue, complete standard executable sequence,
and complete origin sequence in their stated canonical orders.

The implementation stores and tests each resulting SHA-256 digest as an exact
compiled golden: both unit content digests, the bundle digest, source-revision
digest, standard digest, the executable artifact digest, and the executable
semantic digest. The implementation must calculate those values from the
retained source and canonical records. It must not copy a digest from this ADR
into a second handwritten encoder.

## Executable standard snapshot

`StandardLibrarySnapshot` gains a V2-only ordered `StandardExecutable`
sequence. Version 1 has an empty sequence. `StandardExecutable` does not copy
the catalogue function or the snapshot origins. It links the one canonical
catalogue definition to its executable facts. It contains exactly:

1. the standard `FunctionId` of one `FunctionDefinition` in the snapshot
   catalogue;
2. its immutable `FunctionRevisionRecord`, including declaration origin,
   declaration-content digest, semantic digest version, semantic digest,
   language version, and complete versioned executable artifact;
3. its complete ordered `DefinitionReference` sequence; and
4. no other payload, executable bytes, origin, source text, or mutable state.

The snapshot origin sequence contains the complete `DefinitionOrigin` set for
the function declaration and its parameter declaration. The function
declaration origin covers its single result declaration; a single result has
no separate result-column identity.

The function catalogue entry and its `FunctionRevisionRecord` must agree on
function identity and current revision. The artifact kind must match the
function domain. Every reference must name that exact function revision and
must have contiguous zero-based ordinals. Every origin must belong to one of
the two retained V2 source units. No V2 executable may use a Rust dispatch by
function name or `FunctionId`, a PostgreSQL procedure, a migration string, or
an unchecked artifact blob. The normal executor may dispatch only by checked
artifact kind, format, and version, then validate the artifact against the
pinned function signature.

The V2 standard catalogue contains the V1 standard schemas, value types, and
type bindings, the new `std.invoke` schema, and exactly one function:
`std.invoke.echo`. It contains no
objects, fields, application state, CLIENT function, presenter, gateway,
runtime contract, capability, new opaque type or codec, or new system function.
The source checker reconciles every stored declaration, identity, resolved
signature, artifact, reference, and origin against both retained units. It
does not trust a source file because its path looks standard.

The V2 standard function identity and exact name are reserved across the
database. Application admission rejects `FunctionId ...10` and the exact name
`std.invoke.echo` when the V2 snapshot is pinned. Normal candidate allocation
retries the fixed function identity when that snapshot is pinned. The V2
install path scans active and inactive application catalogue and function
revision records before it writes any standard row, and fails on either a
function ID or name collision. Recovery performs the same disjointness check.
The fixed `FunctionRevisionId ...10` is also globally reserved across all
application current and historical revision rows. `ParameterId ...10` is not
globally reserved: parameter identity is scoped by its owning function, and
the reserved standard function identity makes its parameter unambiguous.

The V2 standard digest has a new `StandardLibraryDigestVersion::Version2`.
Its exact byte contract uses the new domain
`ornadb.hash/standard-library/v2\0`; it does not reuse the V1 domain. Its
encoder writes this exact order:

1. `u32` big-endian digest version `2`;
2. raw standard-library revision ID;
3. raw source-revision ID;
4. raw source-revision SHA-256 digest;
5. length-prefixed UTF-8 language version;
6. `encode_catalogue_schemas` in ascending schema-ID order;
7. `encode_value_types` without origins in ascending type-ID order;
8. `encode_enum_types` without origins in ascending type-ID order;
9. `encode_type_bindings` without origins in ascending binding-ID order;
10. `encode_catalogue_functions` in ascending function-ID order;
11. `encode_current_function_revisions_with_contract` with the version-2
    semantic-hash contract in ascending function-ID order;
12. `encode_definition_references` in its existing complete reference sort
    order; and
13. `encode_definition_origins` in its existing complete
    definition-identity sort order.

All existing helper encodings retain their exact tags, length prefixes,
big-endian integers, resolved-type encoding, source-origin encoding, artifact
descriptor encoding, and validation. V2 adds no alternative encoder. The
function sequence and the `StandardExecutable` sequence are one sequence: the
catalogue function at index `i` and the executable revision at index `i` must
have the same function identity. Digest V1 remains byte-for-byte unchanged.

The one V2 executable has revision number `1` and
`FunctionSemanticHashVersion::Version2`. Its reference sequence has exactly
these entries in this order, with each origin set to the exact UTF-8 range of
the stated token in the retained `std/invoke.orna` source:

| Ordinal | Kind | Target | Origin token |
| --- | --- | --- | --- |
| `0` | `NamedType` | `ValueType(INTEGER_TYPE_ID)` | parameter type `INTEGER` |
| `1` | `NamedType` | `ValueType(INTEGER_TYPE_ID)` | result type `INTEGER` |
| `2` | `ParameterRead` | `Parameter { owner: STD_INVOKE_ECHO_FUNCTION_ID, parameter: STD_INVOKE_ECHO_PARAMETER_ID }` | body identifier `p_value` |

The function declaration origin covers the `CREATE SERVER FUNCTION` through
the semicolon. The parameter origin covers `p_value INTEGER`. The new schema
origin covers `CREATE SCHEMA std.invoke;`. These identities, revision facts,
and origins are complete. No other reference, expression artifact, or origin
belongs to the V2 executable.

## First normal executable

`stdlib/std/invoke.orna` contains exactly this source after its final newline:

```sql
CREATE SCHEMA std.invoke;

CREATE SERVER FUNCTION std.invoke.echo(
    p_value INTEGER
)
RETURNS INTEGER
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT p_value;
```

It has one required, non-null `INTEGER` parameter, `p_value`, and a single
`INTEGER` result. It has no default, data access, predicate, order, join,
object reference, state, capability, client artifact, presenter, or runtime
behaviour. It is a normal `SERVER` function. The compiler gives it the fixed
manifest identities above. It never obtains an identity from random candidate
allocation.

The source grammar admits the no-`FROM` `SELECT p_value` body in this exact
form. It is one projection of one parameter identifier and no other clause.
The parser adds one closed `NoInputParameterSelect` server-body AST variant;
it contains only that identifier and its source range. It is disjoint from
`SelectQuery`, which continues to require an object source. The parser does
not resolve it, infer a type, or execute it. This completes an omitted
`orna.language/1` SQL subset; it does not change the language-version label.
The V2 standard checker is the only checker that accepts the AST variant. It
requires the exact source above and rejects every other no-`FROM` SELECT before
artifact construction.

The function compiles to one closed server parameter-echo artifact. Its
artifact `format` field is exactly `orna.server-parameter-echo`; its numeric
`version` field is exactly `1`. The canonical payload is exactly 44 bytes:

```text
bytes 0..8    ASCII `ORNAPE\0\0`
bytes 8..12   u32 big-endian format version `1`
bytes 12..28  raw `ParameterId`
bytes 28..44  raw resolved INTEGER `TypeId`
```

The decoder requires the exact magic, version, and length, consumes all 44
bytes, and rejects wrong magic, version, parameter identity, type identity,
truncation, excess bytes, or invalid signature before execution. It contains
no SQL text, expression tree, function name, default, object identifier,
predicate, cast, value literal, or general parameter-selection feature.

The PostgreSQL executor verifies this artifact against the pinned standard
function signature and returns the already bound typed integer. It does not
match `std.invoke.echo` by Rust name or `FunctionId`. A non-standard function
may use this artifact only after a later accepted decision extends the source
and compiler rules. This decision opens it only for the fixed V2 standard
signature.

This narrow artifact is a prerequisite for source dogfooding. It is not a
shortcut to generic SQL expression evaluation or a hard-coded Rust echo.

## Standard target authority

The root invocation operation pins one active application revision and its
verified standard snapshot before it resolves a target. A standard target is
valid only when it occurs in that exact verified snapshot. The resolved target
contains its stable `FunctionId`, definition, function revision, source
origin, artifact, and target class: `Application` or `VerifiedStandard`.

An `EXECUTE` grant may name either an application function in the pinned
application catalogue or a function in the exact verified standard snapshot
pinned by that application revision. The security snapshot is the canonical,
identity-ordered union of those two function sets. A missing, duplicated, or
unverified standard target fails recovery and invocation closed. A standard
upgrade that removes a granted `FunctionId` also fails recovery; it does not
drop, translate, or keep an unknown grant.

`sys.invoke` performs the same protected sequence for both target classes:
private resolution, base `EXECUTE`, prebind, policy and security mode checks,
durable decision, default evaluation, then execution. The ordinary raw
dispatcher remains closed to every standard target. A direct raw request for
`std.invoke.echo` records the normal denied `EXECUTE` evidence and executes
nothing, even when its grant exists for `sys.invoke`.

The application `RevisionPair` in a protected invocation audit record remains
the durable standard pin. For a standard target, recovery uses that historical
application catalogue revision to load its exact verified standard revision,
then requires the audited `FunctionId` and its executable revision to occur
there exactly once. The row does not add a second standard-origin or artifact
column. This preserves one causal audit relation while preventing an audit row
from pointing to a current, different, or unverified standard snapshot.

The one append-only V2 migration adds
`_orna_kernel.invocation_target_authorities`. One row is owned by one
historical application catalogue revision and one target `FunctionId`. It
stores the closed target class (`application` or `standard`), the pinned
executable `FunctionRevisionId`, and a null-or-exact
`StandardLibraryRevisionId`. Its primary key is
`(catalogue_revision_id, function_id)`. An application row has a null standard
revision and resolves only in that application catalogue. A standard row has
the exact standard revision from that application catalogue hash context and
resolves only in that standard snapshot. Apply writes this relation from
verified catalogue facts; no public writer can add a row.

Before the migration adds the replacement invocation-audit foreign key, it
backfills one `application` authority row for every historical application
catalogue function. Each row uses the function's stored current revision and a
null standard revision. It validates the complete backfill, including every
existing invocation-audit target pair, before it creates the foreign key. A
missing, duplicate, or revision-mismatched application function aborts the
migration. No old invocation-audit row is dropped, rewritten, or made
unrecoverable.

The existing application-only invocation-audit target foreign key is replaced
with a foreign key from `(catalogue_revision_id, function_id)` to this common
target-authority relation. This preserves the target foreign-key requirement
in work ADR 0054 without asking PostgreSQL to reference a union of two
relations. The existing `security_audit_event` composite evidence link remains
unchanged. Recovery becomes the fail-closed authority for the two allowed
target classes: it validates the authority row, historical application
catalogue, exact pinned standard snapshot where required, executable revision,
and exact linked protected `EXECUTE` evidence. It rejects absent, ambiguous,
application-and-standard duplicate, mismatched revision, mismatched artifact,
unlinked, or disclosure-bearing rows. It never repairs an audit row or writes
a replacement during recovery.

## Persistence and recovery

One standard-install transaction persists the complete V2 snapshot before an
application revision can pin it:

1. the V2 standard revision header and digest version;
2. both source units, their ordinals, paths, content, and content digests;
3. the V2 standard catalogue, including `std.invoke.echo` and its signature;
4. the `StandardExecutable`, its function revision, server artifact, origins,
   and ordered references; and
5. the normal application catalogue revision whose hash context pins that
   verified standard revision.

The V2 install is an append-only standard upgrade from the immutable V1
snapshot. It never mutates a V1 standard, source, catalogue, or application
revision. Historical application revisions keep their V1 pin. The new active
application revision is a complete companion revision. It creates one new
application `SourceBundleId`, one new application `SourceRevisionId` whose
parent is the prior active application source revision, and one new globally
unique `SourceUnitId` for every copied active application source unit. It
copies each unit in ordinal, logical-path, content, and content-digest order.
It remaps every active catalogue `DefinitionOrigin` and active
`DefinitionReference` source origin through that exact old-unit to new-unit
map, creates one new application `CatalogueRevisionId`, and recalculates the
catalogue digest in the V2 verified-standard context before it activates the
new complete `RevisionPair`. It reuses an immutable application function
revision only when the normal exact semantic/artifact reuse rules accept it;
the original function-revision declaration origin remains immutable history.

A fresh database may persist V1 as retained historical standard state and then
install V2 with this companion application revision in its first activation
transaction. It may not claim V2 while its V1 parent is absent. A later
standard-upgrade decision owns upgrades after V2; this decision defines only
the V1-to-V2 transition.

Every relation remains private to `_orna_kernel`. No migration executes the
source, creates an equivalent PostgreSQL function, or trusts a stored artifact
without recovery verification.

The V2 migration owns a separate standard-revision-keyed durable family. It
does not reuse application-catalogue foreign keys:

* `_orna_kernel.standard_catalogue_functions` owns one standard function and
  its resolved signature, result, execution properties, declaration origin,
  and current function revision under `(standard_library_revision_id,
  function_id)`;
* `_orna_kernel.standard_catalogue_function_parameters` owns its ordered
  parameter records and origins under the same standard revision and function;
* `_orna_kernel.standard_function_revisions` owns immutable revision records
  under `(standard_library_revision_id, function_revision_id)`;
* `_orna_kernel.standard_function_artifacts` owns the complete server artifact
  under the same standard revision and function revision; and
* `_orna_kernel.standard_definition_references` owns the contiguous ordered
  reference sequence under the same standard revision and function revision.

The standard schema, value-type, enum, and binding relations retain their
existing V1 ownership. The V2 rows add their declaration and parameter origins
to the existing standard-origin model. Every new relation has a foreign key to
the selected standard revision, exact 16-byte identity checks, closed domain,
security, transaction, volatility, artifact-kind, and artifact-format checks,
and no public grants. A V1 standard revision has no row in any new executable
relation. Generic application `catalogue_functions`, `function_revisions`,
`function_artifacts`, and `definition_references` remain application-only.

Recovery reconstructs both retained units in ordinal order, validates each
identity and digest, reconstructs the standard schema, function, parameter,
return, revision, artifact, references, and origins, verifies the V2 standard
digest, and only then issues the verified-standard capability used by
catalogue, security, codec, and invocation code. It rejects a V2 snapshot with
the V1 digest version, a V1 snapshot with an executable, an invalid V2 parent,
any unsupported version, or any source/catalogue/artifact/reference/origin
disagreement. Existing V1 recovery remains independent and unchanged.

## Required implementation order

1. `docs(std): define executable standard snapshots` changes this ADR and the
   work-ADR index only.
2. `feat(core): model executable standard snapshots` changes the standard
   snapshot, canonical V2 digest, and public exports. It preserves V1 bytes
   and rejects a malformed executable sequence before it returns a snapshot.
3. `feat(artifact): encode standard parameter echo` adds only the closed
   `orna.server-parameter-echo` version-1 artifact and its decoder tests.
4. `feat(compiler): check standard parameter echo` changes the SQL parser and
   compiler to accept only the exact required non-null `INTEGER -> INTEGER`
   source shape. It rejects every data-access, default, type, result, domain,
   security, transaction, volatility, and expression variation.
5. `feat(postgres): execute standard parameter echo` decodes and executes the
   closed artifact after normal typed binding. It proves that no Rust
   function-name special case exists.
6. `feat(compiler): reconcile executable standard source` changes the
   standard source checker and manifest reconciliation for the ordered V2
   source bundle, fixed identities, executable contents, origins, and
   artifact/reference evidence.
7. `feat(postgres): persist executable standard snapshots` adds one
   append-only migration for all V2 standard relations and
   `invocation_target_authorities`, backfills historical application targets,
   replaces the audit target foreign key, and applies and recovers all V2
   executable facts fail closed.
8. `feat(core): pin standard invocation targets` models the two target
   classes, immutable standard executable pin, and redacted protected
   resolution boundary.
9. `feat(postgres): recover standard invocation targets` changes the
   security-target union and protected invocation-audit recovery atomically.
10. `feat(std): retain the first executable standard source` adds
    `stdlib/std/invoke.orna` and the V2 standard manifest and digest goldens.
11. `test(server): prove standard invocation dogfooding` proves the accepted
    live PostgreSQL route through the normal installed source.

Each commit changes one to three files, has a signed Conventional Commit, and
keeps the workspace buildable. The affected commit gates include format,
strict Clippy, rustdoc, diff, similarity, focused unit tests, standard-source
verification, standard install and reopen, recovery tamper tests, and the
focused live PostgreSQL proof.

The live proof installs `orna.std/2`, grants the caller `EXECUTE` on
`std.invoke.echo`, invokes it through `sys.invoke` by qualified name and
parameter name, repeats it by the fixed function and parameter identities,
and receives `InvocationStarted(0)`, `ValueBatch(1)` with the typed integer,
`InvocationCompleted(2)`, and `CALL_COMPLETED`. It also proves that a direct
raw call to the same standard target returns `EXECUTE_DENIED`, records one
denied decision, and executes no artifact.

The same live proof reads the allowed protected `security_audit_event` and
`invocation_audit_event`. It proves that both link to the exact historical
application `RevisionPair` whose catalogue hash context pins `orna.std/2`,
and that recovery resolves the standard target and executable revision only
through that pair. Restart/reopen succeeds with the valid rows. Separate
tamper fixtures for an absent standard target, wrong standard executable
revision, unlinked security evidence, mismatched application revision pair,
and an extra disclosure-bearing audit column all fail recovery without writing
or changing prior history.

## Deferred surface

This decision does not define standard-library upgrade selection, a package
registry, module distribution, a generic multi-file source-apply interface,
general function support in `orna.std/1`, arbitrary parameter expressions,
generic projection plans, presenter selection, CLI rendering, CLIENT artifacts,
runtime loading, UI, Inspector views, JSON-RPC, MCP, or remote transport.

## Precedence

This decision implements the first standard-source dogfood path required by
work ADR 0054. Work ADR 0054 remains authoritative for `sys.invoke`, carrier
positions, protected ordering, lifecycle, redaction, and Event bytes. Work
ADR 0053 remains authoritative for carrier values. The existing source,
catalogue, artifact, security, PostgreSQL, and canonical-hash work decisions
remain authoritative outside the V2 executable-standard boundary stated here.
The canonical specification remains authoritative outside this accepted
implementation scope.
