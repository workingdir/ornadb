# ADR 0073: SET Values Use ORV6 Transport

**Status:** Accepted

## Decision

OrnaDB adds a bounded SET runtime value and a version-six canonical value
transport. This slice enables the existing sealed
`sys.security.active_roles()` identity, whose return type is
`SET OF REF sys.security.principal`.

ORV5 remains byte-for-byte stable. ORV6 uses the marker `ORV6` and the same
25-byte value envelope. Existing scalar, reference, record, opaque, OPTION,
LIST, and MAP values continue to use ORV5 when they contain no SET value.
A value that contains a SET uses ORV6. The decoder accepts both versions.

The ORV6 descriptor adds one tag to the descriptor algebra:

```text
0x05  Set  then one child descriptor
```

The constructed value tag remains `0x0d`. A SET payload is:

```text
4 bytes  element count, big-endian
repeat count times in canonical element order:
4 bytes  complete element-value length, big-endian
n bytes  one complete ORV6 element value
```

SET elements use the existing canonical map-key ordering rules. The first
transport slice admits only `Named` and `Reference` element descriptors. The
runtime constructor validates every element, sorts the retained values into
canonical order, and rejects duplicate elements before publication. Nested SET,
LIST, MAP, OPTION, and STREAM element descriptors remain outside this slice.

The existing `RuntimeValue` and `ConstructedValueKind` models gain a checked
SET variant. SET values retain one descriptor, an ordered canonical element
sequence, and the existing runtime node count. They use the same active
catalogue and verified-standard validation authority as LIST and MAP values.

The sealed invocation route admits `SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID`
after the value transport is available. The kernel returns one checked SET of
reference values from the authenticated session's sorted active roles. The
protected decision, authority rows, audit evidence, invocation events, and
presentation path retain the existing sealed identity rules. The two scalar
identity functions remain unchanged.

## Rationale

ADR 0039 deliberately closed ORV5 to SET and STREAM. Adding a new marker keeps
all accepted ORV5 bytes and rejection rules unchanged while providing an explicit
extension point for the already-registered SET identity. Reusing the existing
canonical ordering and checked constructors avoids a second equality or type
authority.

The first slice limits SET elements to the descriptor classes that already have
stable ordering for MAP keys. This gives `active_roles` a deterministic wire
form without inventing ordering for arbitrary constructed values. STREAM remains
closed because it needs a lifecycle, backpressure, and cancellation contract,
not only a value encoding.

## Required proof

Tests must prove that ORV5 behaviour remains unchanged, ORV6 SET descriptors and
values round-trip canonically, unordered input becomes one canonical sequence,
duplicate and unsupported elements fail closed, and nested SET values obey the
existing size and node bounds.

The PostgreSQL proof must authenticate a session with active roles, invoke
`sys.security.active_roles` through the sealed `sys.invoke` path by qualified
name and stable identity, decode the returned typed SET of principal references,
verify canonical role ordering, verify the completed event sequence, and retain
normal security and invocation audit evidence. The existing denial proof must be
replaced by this execution proof. `STREAM` and all other non-admitted system
functions remain denied.

## Implementation order

1. Add the checked SET model and canonical element ordering in `orna-core`.
2. Add ORV6 descriptor and value encoding in `orna-protocol` while preserving
   ORV5.
3. Admit SET values in the invocation carrier validation and sealed active-role
   dispatch.
4. Extend the focused PostgreSQL proof and run the live kernel gate.

Each implementation commit changes one to three files and keeps the workspace
buildable.

## Deferred surface

STREAM values, arbitrary constructed SET elements, CLIENT expression calls to
sealed system functions, runtime resource transport, and presentation-specific
SET renderers remain outside this ADR.

## Precedence

This decision supersedes the SET exclusion in work ADR 0039 only for the ORV6
transport and the admitted `active_roles` return value. ADR 0039 remains
authoritative for ORV5, and STREAM remains excluded. Work ADRs 0042, 0053,
0054, and 0072 remain authoritative for sealed registry identity, carrier
framing, invocation ordering, and scalar security identities. The canonical
specification remains authoritative outside this accepted implementation scope.
