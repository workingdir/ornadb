# Work ADR 0090: Local Principal and Session Authority

**Status:** Accepted

## Context

The canonical security model makes principals, authentication, authorization,
sessions, credentials, providers, delegation, and audit first-class concepts.
Spec ADR 0014 keeps the metadata visible to the database while reserving
authentication, session identity, credential verification, and privilege
enforcement for the trusted kernel. Spec ADR 0007 separately requires durable
`USER` state to be keyed by the authenticated principal.

The implemented local slice is narrower than that complete model. It is an
authority boundary around an authenticated local invocation, not an acceptance
of every object named by the canonical model. The relevant implementation
surfaces are:

 - `crates/orna-core/src/security.rs`: `PrincipalId`-backed
   `SecuritySnapshot`, `AuthenticatedSession`, `AuthenticatedSessionBinding`,
   `authorise_execute`, `authorise_privilege`, and the closed audit decision
   types;
 - `crates/orna-postgres/src/kernel/security.rs`: snapshot recovery,
   `authenticate_local_peer`, sealed `sys.invoke`, authenticated raw/resource
   dispatch, parent-ownership checks, and protected audit persistence;
 - `crates/orna-postgres/src/kernel/security_admin.rs`: typed session
   identity functions, privilege resolution, and protected security-admin
   mutations;
 - `crates/orna-server/src/local_auth.rs`: `authenticate_local_stream`,
   which obtains the peer UID from `SO_PEERCRED` and supplies it to the kernel;
 - `crates/orna-server/src/raw_socket.rs`: the fixed local authenticated
   socket and its ordered invocation/resource dispatch; and
 - `crates/orna-server/src/invoke.rs` and
   `crates/orna-client/src/lib.rs`: installed invocation/resource binding
   and the caller-owned `ClientStateStore` session and state boundary.

This ADR records the accepted local authority contract represented by those
surfaces. It does not widen the implementation to the remaining canonical
security or production CLIENT VM proposals.

## Decision

Accept the following v1 local authority boundary:

1. A `PrincipalId` is an exact 16-byte identity. The local catalogue admits
   active or disabled `USER`, `ROLE`, and `SERVICE` principals. `EXTERNAL` is
   not in this boundary.
2. A validated `SecuritySnapshot` is the single in-memory decision authority.
   It owns the exact active `RevisionPair`, the identity-ordered function
   target universe, principals, role memberships, direct `EXECUTE` grants,
   privilege-class grants, and protected local peer mappings recovered by the
   PostgreSQL kernel.
3. A local `AuthenticatedSession` is produced from trusted peer authentication
   and carries the authenticated principal, explicitly selected reachable
   active roles, and an opaque per-authentication
   `AuthenticatedSessionBinding`. Its effective principal is the authenticated
   principal for this slice.
4. `sys.invoke`, raw calls, resources, actions, and CLIENT state derive
   authority from that authenticated session and the active snapshot. Request
   payloads cannot choose a principal, role, credential, grant, effective
   principal, or revision authority.
5. Durable `USER` state is server-keyed by the authenticated principal and the
   state identity `(root function, root profile, function, function-instance
   key, state slot)`. Parent invocation and resource/action call-site
   identities are correlation and lineage metadata, not authority.
6. Security and invocation decisions use protected, append-only, redacted
   audit evidence. Any invalid, stale, mismatched, unauthenticated, or
   persistence-failure path fails closed and does not return success.

This boundary is sufficient authority input for a later CLIENT VM contract:
the later contract may consume an authenticated and authorised invocation
context. This ADR does **not** accept a CLIENT VM implementation, host-token
scheme, artifact-provenance scheme, sandbox mediation, or any other VM runtime
surface.

## Principal and grant rules

### Identities and catalogue closure

`SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants`
rejects duplicate or zero identities and validates the complete catalogue
snapshot before it can authorise anything. Principal status is checked at
session binding and role selection. A `ROLE` cannot authenticate as the session
principal. `LocalPeerCredential` is only a protected numeric-UID-to-principal
mapping; it is not a password, token, or secret store.

Role membership is directed from member to containing role. The snapshot
rejects duplicate, dangling, non-role, self-referential, and cyclic edges.
`SecuritySnapshot::reachable_roles` computes roles reachable from the
session principal, and `bind_authenticated_session` accepts only active,
reachable roles with no duplicates. Reachability alone does not grant
authority: a role must be selected in the trusted session context.

The target universe is closed. `SecurityFunctionTarget` represents the
identity-ordered union of the active application functions and the exact
verified standard functions pinned by that application revision. PostgreSQL
recovery uses `security_function_targets`,
`load_invocation_target_authorities`, and `require_complete_function_set` to
preserve that universe. `authorise_execute` rejects an unknown function, class
mismatch, unpinned standard target, or any target whose `RevisionPair` differs
from the snapshot's exact active pair before execution.

### EXECUTE and privilege resolution

A direct `ExecuteGrant` for the active session principal takes precedence. If
there is no direct grant, an `EXECUTE` grant held by one of the selected active
roles may authorise the target; the lowest canonical role identity is retained
when more than one selected role matches. An active but unselected or
unreachable role grants nothing. Durable `PrivilegeGrant::Execute` grants are
accepted when class-wide or scoped to the target function. The resulting
`AuthorisedInvocation` records the session principal, effective principal,
selected roles, authorising principal, target, pinned revision, and security
context digest.

The closed privilege classes are decided by `authorise_privilege`.
`crates/orna-postgres/src/kernel/security_admin.rs::privilege_classes_for_session`
resolves the authenticated principal together with its selected active roles.
Security-admin mutations must hold the class-wide `SecurityAdmin` privilege;
they do not bypass the same snapshot validation or audit boundary.

`crates/orna-postgres/src/kernel/security_admin.rs::session_principal`, `effective_principal`, and
`active_roles` expose only the typed facts bound to the session. In this slice,
`effective_principal` equals `session_principal`; there is no definer or policy
transition that could change it.

## Trusted authentication and session binding
`crates/orna-server/src/local_auth.rs::authenticate_local_stream` reads the UID attached by Linux to the
already-connected Unix stream through `SO_PEERCRED`, then calls
`PostgresKernel::authenticate_local_peer`. No request byte, environment value,
command argument, PostgreSQL role name, or caller-provided UID selects an Orna
principal.

The recovered protected mapping is one-to-one by UID and principal and must
select an active `USER` or `SERVICE`. An unknown UID, missing or malformed
mapping, disabled principal, role principal, invalid durable state, socket
failure, or database failure rejects authentication without creating a usable
session. `SecuritySnapshot::authenticate_local_peer` starts with no active
roles. The core `bind_authenticated_session` seam can validate a trusted
selected reachable role set, but the current local transport has no
role-selection operation or role-selection UX; no client frame supplies that
set.

Each successful binding gets a fresh `AuthenticatedSessionBinding`. Clones of
one session retain the opaque binding, while separately authenticated sessions
receive distinct bindings. The binding carries no principal identity and is
not a principal-selection mechanism. `ClientStateStore::bind_authenticated_session`
retains the first binding and rejects a different one before changing its
caller-owned USER cache.

## `sys.invoke`, raw, resource, and state authority

### Sealed invocation and raw calls

The installed host in `invoke.rs` builds a typed retained request from the
resolved target and arguments, then authenticates the local stream and calls
`PostgresKernel::dispatch_sealed_sys_invoke`. The kernel first admits the
protected `sys.invoke` entry and only then decodes retained request content.
Target resolution, argument binding, target class, active revision, security
snapshot, and `EXECUTE` authorisation are recovered or checked by the kernel.
The request has no principal, credentials, grants, or revision authority.

`dispatch_authenticated_raw_call` and its argument-bearing variants take an
`AuthenticatedSession`; they do not take a caller-supplied principal. The raw
surface remains a low-level recovery/compatibility path behind the same
session and target checks, not an alternate authority source.

### Resources and actions

`dispatch_authenticated_server_resource` validates the resource lineage and
state context, checks the authenticated parent through
`resource_parent_invocation_is_owned`, and recovers the target and security
state before reservation or execution. It generates the nested invocation
identity in the kernel. A missing parent, a parent owned by another session,
zero/invalid lineage identity, stale revision, invalid target, malformed
arguments, or unsupported target is denied without target execution.

The installed `InstalledClientResourceExecutor` and the CLIENT action trigger
carry checked target/argument and inherited lineage context, not authority.
`parent_invocation_id` and `call_site_id` correlate the enclosing invocation,
resource lifecycle, and audit records. They cannot select a principal, role,
grant, target revision, or permission. This is the distinction accepted by
work ADR 0089. Its low-level direct constructors remain compatibility/test
seams, not hostile external-plugin APIs.

### USER state
`crates/orna-postgres/src/kernel/state.rs::load_user_state_in_transaction` queries
`_orna_kernel.user_state_cells` using the principal from the revalidated
`AuthenticatedSession`, together with the requested root function and profile.
`PostgresKernel::load_user_state` and `write_user_state` revalidate the session
against the active snapshot before querying or writing. Writes validate the
active CLIENT root, declared slot types, canonical values, duplicate keys, and
optimistic revisions before the atomic write and audit commit.
`ClientStateKey` and the `ClientStateStore` in `crates/orna-client/src/lib.rs` retain the
non-principal part of the key: root function, root state profile, function,
function-instance key, and state slot. The store never stores or chooses a
`PrincipalId`; its opaque binding only enforces session affinity. Resource and
action call-site/parent IDs are correlation only and cannot redirect USER
state to another principal. A normal client cannot request another principal's
state by changing a profile, instance key, call-site ID, parent ID, or action
argument.

## Audit and failure ordering

Authentication appends one protected authentication decision in the kernel
transaction before returning either a session or a typed denial. Sealed
invocation appends the protected `EXECUTE` evidence and linked invocation
record before target execution; denied or unresolved requests receive only
closed denial evidence. Resource acceptance, nested invocation, and terminal
outcomes retain the same protected audit relationship. USER state load/write
operations append their closed operation evidence only after the relevant
session, active revision, root, type, and value checks pass.

Audit records contain only closed identities, target/revision evidence, outcome,
and closed denial or operation details. They do not contain UIDs, credentials,
secrets, request principals, role lists supplied by a request, grants,
arguments, USER state values, results, source text, or arbitrary error text.

The ordering rules are:

1. authenticate from the kernel peer credential and recover current protected
   state;
2. validate the active revision, session, target, lineage, and typed inputs;
3. append the corresponding protected decision evidence;
4. commit the decision (and, where applicable, the protected state mutation);
5. only then execute, publish, or return the successful result.

A denial is a typed closed result where the operation can safely record one. An
invalid protected row, audit insert failure, transaction commit failure, or
PostgreSQL session shutdown failure is a kernel failure, not an allow. No
failure path fabricates authority or converts an unavailable audit decision
into success.

## Explicit deferrals

The following are outside this accepted boundary and require separate ADRs
before implementation or acceptance:

- credential, authentication-provider, and secret enrollment or verification;
- durable session rows, session termination, and revocation;
- role-selection UX or a role-selection operation on the local transport;
- `SECURITY DEFINER`, policy evaluation, and effective-principal transitions;
- delegation and impersonation;
- `EXTERNAL` or other federated principals;
- remote or external gateway authentication; and
- production CLIENT VM host tokens, artifact provenance/signatures, and
  sandbox mediation.

The existing local peer mapping is therefore not credential enrollment, the
opaque binding is not a durable session, and the current effective-principal
equality is not a definer implementation.

## Alternatives rejected

### Trust principal or role fields in requests

Rejected. It would allow request data to choose authority and would violate the
canonical invocation rule that credentials and principal IDs come from the
authenticated connection. All local entry points instead receive an
`AuthenticatedSession`, while the server derives target and grant decisions
from its recovered snapshot.

### Treat reachable roles as implicitly active

Rejected. Reachability is catalogue topology, not a session selection. The
current local authentication path starts with no active roles, and only a
trusted binding can select reachable active roles. Adding a role-selection
operation is a separate contract.

### Treat parent or call-site IDs as authority

Rejected. These identities are needed for nested invocation correlation,
lineage validation, resource lifecycle, and audit linkage. ADR 0089 establishes
that they cannot change principal, grants, target, revision, or state ownership.

### Add passwords, providers, or durable sessions to this slice

Rejected. Those features require protected enrollment and verification,
secret-handling rules, lifecycle/revocation semantics, and separate proof.
Adding them here would claim capabilities absent from the local implementation.

### Make direct in-process constructors a hostile plugin boundary

Rejected for this decision. The direct resource constructors remain documented
low-level compatibility/test seams. An external plugin API would need a
separate authenticated binding and admission contract; the existing seams do
not prove or claim that boundary.

### Accept production CLIENT VM authority now

Rejected. The local session and invocation context is a prerequisite authority
input for a later VM contract, but host tokens, artifact signatures/provenance,
and sandbox mediation are not implemented or accepted here.

## Evidence and proof boundaries

The accepted claim is grounded in the implementation paths above and the
following existing proof seams:

 - `orna-core/src/security.rs` tests for direct grants, selected reachable role
   grants, disabled/unknown principals and roles, exact revision/target checks,
   duplicate/dangling/cyclic catalogue rejection, deterministic evidence, and
   opaque session bindings;
 - `orna-postgres/src/kernel/security.rs` recovery, local-peer authentication,
   sealed entry ordering, resource lineage/parent ownership, nested invocation
   identity generation, and protected audit paths;
 - `orna-server/src/local_auth.rs` peer-credential extraction from the actual
   connected Unix stream;
 - `orna-client/src/lib.rs` tests for session mismatch, context/key validation,
   deterministic USER-state changes, and aligned optimistic write results; and
 - the focused resource-lineage and installed-resource proof paths recorded by
   work ADR 0089, including
   `resource_lineage_validation_rejects_zero_parent_before_other_request_validation`
   and
   `installed_resource_socket_delivers_values_and_enforces_windows_and_grants`.

These paths prove the narrow local authority and fail-closed ordering. They do
not prove credential enrollment or verification, durable session lifecycle,
role selection, definer/policy transitions, delegation, federated identity,
remote gateway authentication, hostile external-plugin binding, or production
CLIENT VM host-token, artifact-signature, provenance, or sandbox guarantees.
No additional test or live database run is claimed by this ADR.

## Compatibility and non-goals

This decision is compatible with work ADRs 0020-0024, 0070, 0077-0079, and
0089. It composes the in-memory decision core, durable snapshot recovery,
local peer authentication, protected audit, authenticated invocation/resource
transport, and principal-keyed USER state without changing their wire bytes,
public request authority, low-level compatibility seams, or protected audit
shape.

It does not change code, tests, migrations, the canonical external
specification, Beads, or the PostgreSQL private-kernel boundary. It does not
add a public security catalogue API, a credential store, a session browser, a
role selector, a remote listener, a gateway identity mode, or a VM runtime. It
also does not make resource/action correlation metadata an authority token or
turn the local opaque session binding into a reusable credential.

## Precedence

For this accepted local slice, this ADR is the work-series contract for the
principal/session authority boundary. Spec ADRs 0007 and 0014 and
`spec/docs/35-security.md`, `spec/docs/13-invocation-system.md`, and
`spec/docs/16-state-model.md` remain canonical outside the explicitly bounded
surface. Work ADRs 0020-0024 remain authoritative for the decision core,
durable snapshot, CLIENT authorization gate, local peer authentication, and
protected audit. Work ADRs 0070 and 0077-0079 remain authoritative for USER
state, resource transport, and action values; work ADR 0089 remains authoritative
for resource lineage correlation. Any deferred capability listed here needs a
later accepted ADR before it can broaden this boundary.
