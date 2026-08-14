# ADR 0042: Mandatory System Functions Have One Sealed Registry

**Status:** Accepted

## Decision

Ring-1 functions that must exist before an application catalogue is available
have one sealed registry in `orna-core`. The registry is compiled into the
signed Orna distribution and is not reconstructed from application source,
the standard-library catalogue, PostgreSQL rows, environment values, or
configuration.

The first two entries are:

| Kind | Qualified name | Stable `FunctionId` |
|---|---|---|
| catalogue health | `sys.catalog.health` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01` |
| root invocation gateway | `sys.invoke` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02` |

These byte strings are the exact opaque identifier bytes in network order.
They are not UUIDs. The existing canonical `FunctionId` text form remains the
only public textual identity.

The registry exposes exact lookup by stable identity and resolved semantic
name. Name comparison uses the complete case-sensitive resolved parts. It
does not perform case folding, prefix matching, alias lookup, search-path
resolution, or application-catalogue fallback.

`sys.invoke` is the mandatory bootstrap target required by spec ADR 0004. Its
stable identity is therefore available before its sealed request and event
families are admitted. Allocating the identity does not admit a temporary
signature, byte blob, JSON request, application record, or direct target call.

## Application-catalogue boundary

An application catalogue cannot contain a function whose `FunctionId` or
exact resolved qualified name belongs to the sealed system registry. Revision
admission first checks reserved identities in registry order, then checks
reserved names in registry order, and returns the first collision. The
existing `ReservedSystemFunctionIdentity` error remains exact for an identity
collision. A name collision returns the new typed error:

```rust
RevisionInvariantError::ReservedSystemFunctionName {
    function: FunctionId,
}
```

Its display is:

```text
the reserved system function name cannot enter an application catalogue
```

The payload identifies the conflicting application function. Every identity
collision precedes every name collision. Registry order is catalogue health
followed by `sys.invoke`. Application function vector order does not affect
the selected error.

The `sys` schema itself is not reserved by this decision. Application names
such as `sys.probe` remain valid. Only the complete registered function names
and identities are closed.

## Trusted invocation access

The security snapshot owns one closed system-entry decision. Every valid
authenticated active `USER` or `SERVICE` session may enter either registered
system function without a stored application `EXECUTE` grant. A `ROLE` still
cannot authenticate. Invalid, disabled, forged, or stale session state returns
the existing denial.

This decision grants entry only to the exact registered system function. It
does not authorise the application target named inside a future
`sys.invoke.Request`. `sys.invoke` must resolve, pin, authorise, and audit that
target separately under the same trusted authenticated session. It cannot
turn its own gateway access into a wildcard application grant.

The existing catalogue-health implementation keeps its stronger verification
contract from work ADR 0035. Registering the identity does not merge health
execution with invocation planning.

## Public and durable shape

The sealed registry is a core read-only definition boundary. PostgreSQL does
not store duplicate system-function rows and application recovery does not add
the entries to `CatalogueSnapshot`. Application security snapshots continue
to contain the complete active application function set only.

Callers distinguish a system entry from an application definition before they
select execution behaviour. They do not fabricate a `FunctionDefinition` or
an application `FunctionRevisionId` for a sealed entry.

The first implementation exposes a non-exhaustive `SystemFunctionKind`, a
copyable `SystemFunctionDefinition`, the ordered registry slice, and exact
identity/name lookup functions. Existing catalogue-health constants remain
available as compatibility aliases to the registry facts during migration.

## Required proof

Public behavioural tests must prove:

* the ordered registry contains exactly the health and invocation entries with
  their accepted names and opaque identity bytes;
* identity and resolved-name lookup return the same exact definitions;
* similar names, prefixes, different case, unqualified names, and unknown
  identities do not resolve;
* application revision admission rejects each reserved identity and each exact
  reserved name through the exact typed payload and display;
* an application function carrying the invocation identity and health name is
  rejected for its identity, proving the global identity-before-name phase;
* identity and name selection each follow registry order under reversed
  application definition input;
* neighbouring `sys` names remain admissible;
* valid authenticated users and services may enter exactly both registered
  system functions without a stored grant;
* invalid, disabled, role, stale, unknown, and ordinary application targets
  retain their existing denial; and
* ordinary application execution still requires its exact direct or active
  role grant.

Tests exercise public registry, revision, and security interfaces. They do not
inspect source constants, compare source text, or duplicate the registry's
lookup algorithm.

## Implementation sequence

1. `docs(core): define the sealed system function registry` changes this ADR
   and the work-ADR index only.
2. `feat(core): register mandatory system function identities` changes
   `crates/orna-core/src/system.rs`, `crates/orna-core/src/lib.rs`, and
   `crates/orna-core/src/revision.rs`. Test logic remains in those owned module
   test sections.
3. `feat(core): authorise mandatory system entry` changes
   `crates/orna-core/src/security.rs` only and replaces the health-only
   decision with the closed registry-backed decision while retaining its
   compatibility method.
4. Begin the separately accepted sealed `sys.invoke` type-family decision only
   after the canonical constructed-value transport prerequisite is complete.

Every commit changes one to three files, uses a signed Conventional Commit,
and leaves the workspace buildable.

## Deferred surface

This decision does not define or admit the sealed `sys.invoke.Request`,
`sys.invoke.Value`, `sys.invoke.Event`, `STREAM<Event>`, ORV5/ORF5 frames,
constructed frame arguments, invocation target resolution, argument binding,
defaults, target security decisions, target audit records, execution,
presentation, `--explain`, `orna invoke`, remote transport, or TLS.

It does not resume or weaken work ADR 0039. In particular, no ordinary CLI is
added before one complete canonical typed request can cross `CALL_RAW`.

## Precedence

This decision implements the stable mandatory Ring-1 identity prerequisite
from spec ADR 0004 and `spec/docs/06-bootstrapping-recovery.md`. It extends work
ADR 0035 from one special identity to one sealed registry while retaining the
health function's exact access, audit, verification, and recovery behaviour.
It does not alter the ordered constructed-value position rules in work ADR
0036 or the accepted protocol contract in work ADR 0039.
