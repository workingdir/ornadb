# ADR 0072: Sealed System Identity Calls Use the `sys.invoke` Route

**Status:** Accepted

## Decision

The first sealed system-call execution slice makes the two scalar security
identity functions callable through the existing `sys.invoke` route:

```text
sys.security.session_principal()
RETURNS REF sys.security.principal

sys.security.effective_principal()
RETURNS REF sys.security.principal
```

These functions remain sealed registry entries. They do not become application
catalogue functions, standard-library functions, PostgreSQL catalogue rows, or
fabricated `FunctionDefinition` values. Migration 0029 adds one private
`invocation_target_authorities` audit anchor per admitted system identity and
application catalogue revision. These rows do not define or execute functions;
they only satisfy the existing invocation-audit target foreign key. The
protected invocation decision resolves the `SystemFunctionDefinition` directly
from the exact sealed registry identity or qualified name, authorises the
target with the existing system-function rule, requires zero arguments, and
then dispatches the identity-specific kernel method.

The result uses the existing typed reference runtime value with
`SYS_SECURITY_PRINCIPAL_TYPE_ID` as its target type and the returned
`PrincipalId` as its object identity. `effective_principal` continues to equal
the authenticated session principal until a later accepted definer or policy
model changes that rule.

`sys.security.active_roles()` remains outside this execution slice. Its
accepted signature returns `SET OF REF sys.security.principal`, but the current
ORV5 constructed-value contract does not admit `SET` descriptors or values.
The route must therefore deny this target rather than return a `LIST` with a
different type. A later decision may admit the required set value and wire the
function without changing the scalar identity contract here.

All other sealed system functions remain outside this route. In particular,
security administration, inspection, state, catalogue health, and the root
`sys.invoke` entry retain their existing execution boundaries.

## Protected boundary

The existing ordering remains unchanged:

1. authenticate the local session and enter the exact sealed `sys.invoke`
   function;
2. decode the retained request against the active verified standard context;
3. resolve the target privately by exact identity or qualified name;
4. make the target system-function decision without an application `EXECUTE`
   grant;
5. bind the closed zero-argument signature; and
6. execute and append the existing security and invocation audit evidence.

A system target uses the active application `RevisionPair` in the audit target
for compatibility with the existing durable audit schema. The authority row
uses the sealed function identity again as its opaque revision anchor. It does
not acquire an application function revision or a standard executable revision.

## Rationale

The registry and direct kernel methods already define the identity facts. The
missing link is target resolution and execution at the sealed invocation
boundary. Reusing application definitions would make a sealed function appear
in the catalogue and would weaken the identity authority. A private system
variant preserves the registry boundary while allowing the existing protected
route, audit lifecycle, and event stream to remain unchanged.

Returning a list for `active_roles` would be type-incorrect. ORV5 explicitly
keeps `SET` and `STREAM` closed, so this slice records the boundary instead of
inventing a second collection representation.

## Required proof

Tests must prove that authenticated sealed invocation by both qualified name
and stable identity can execute `session_principal` and
`effective_principal`, that both return the expected typed principal reference,
that each invocation emits the normal completed event sequence, and that the
security and invocation audit rows retain the active application revision pair.
Tests must also prove that `active_roles` remains denied until a set-valued
runtime transport is accepted.

## Implementation order

1. Extend the private core invocation target model with a sealed scalar system
   identity variant and system-function authorisation.
2. Add PostgreSQL dispatch for the two scalar identity functions through the
   existing sealed route.
3. Add the focused sealed invocation proof and the explicit `active_roles`
   denial proof.

Each implementation commit changes one to three files and keeps the workspace
buildable.

## Deferred surface

Set-valued runtime construction and ORV5 transport, `active_roles` execution,
all other `sys.security` calls, system-function presentation changes, remote
transport, and new source syntax remain outside this ADR.

## Precedence

This decision narrows the current proposal in `spec/api/security.md` without
changing the canonical specification. Work ADRs 0042, 0053, 0054, and 0065
remain authoritative for registry identity, carrier transport, protected
invocation ordering, and the security-admin model.
