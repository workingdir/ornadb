# ADR 0065: The Security Admin Functions — sys.security Principal, Role, and Privilege Surface

**Status:** Accepted

## Decision

The Ring-2 security administration surface becomes real: session identity
functions (`sys.security.session_principal`, `effective_principal`,
`active_roles`), protected SERVER admin functions (`create_principal`,
`disable_principal`, `create_role`, `grant_role`, `revoke_role`,
`grant_privilege`, `revoke_privilege`), the two checks (`can_execute`,
`has_privilege`), a durable privilege-grant model, and an `orna security`
CLI family. This slice implements the spec `api/security.md` contract as
kernel model methods behind sealed registry identities, mirroring ADR 0064.

`create_delegation` and `terminate_session` are deferred: there is no
delegation model and no durable session registry today. Secret-channel
credential enrolment is also deferred (the CLI gains a secret-input flag in a
later slice). None of this slice needs `orna-syntax` or CLIENT bodies.

## Background

The security model (`crates/orna-core/src/security.rs`) already has
principals (kind/status), role memberships, `ExecuteGrant` (the only grant
type), local peer credentials, `AuthenticatedSession`, `authorise_execute`,
and a closed audit kind set. The durable tables (`security_principals`,
`security_role_memberships`, `security_execute_grants`,
`security_local_peer_credentials`) back a CLI-installed grant path
(`orna security grant-execute`). There is no privilege-class concept beyond
EXECUTE and the in-memory INSPECT ladder, no session table, and no
delegation model. The sealed `sys.*` registry (`crates/orna-core/src/system.rs`)
has exactly 12 entries; sealed dispatch never resolves system identities as
callable targets.

## Surface

```text
sys.security.session_principal() RETURNS REF sys.security.principal
sys.security.effective_principal() RETURNS REF sys.security.principal
sys.security.active_roles() RETURNS SET OF REF sys.security.principal

sys.security.create_principal / disable_principal
sys.security.create_role / grant_role / revoke_role
sys.security.grant_privilege / revoke_privilege

sys.security.can_execute(p_principal, p_function) RETURNS BOOLEAN
sys.security.has_privilege(p_principal, p_privilege, p_object) RETURNS BOOLEAN
```

`orna security user create|disable`, `role create|grant|revoke`,
`grants list|grant|revoke`, `check can-execute|has-privilege` render JSON
lines or canonical identity hex, mirroring the `orna inspect` and `orna state`
render paths.

## Sealed registration

Twelve new sealed `sys.security.*` entries (`...40` through `...4b`),
three new `SystemFunctionKind` variants (`SecurityIdentity`,
`SecurityAdmin`, `SecurityCheck`), a `SystemSecuritySignature` type mirroring
`SystemInspectSignature`, the `sys.security.principal` carrier type
(`...f7`, representation contract `orna.sys.security.principal@1`), and the
registry-length test updated 12 to 24. Execution is CLI-installed kernel
model methods, exactly like ADR 0064: sealed `sys.invoke` routing of system
identities is deferred because the sealed target resolution never resolves
system functions.

The original allocation placed the block at `...0d` through `...1a`. That
range collides with the retained standard library's FunctionId space
(`std.invoke.echo` at `...10`, `std.json.encode` at `...11`,
`std.terminal.present_table` at `...12`); `security_function_targets` would
then silently filter a standard function as a sealed system function and no
complete-active-function-set proof could install it. The block therefore
lives in the documented `...40` through `...4b` range, disjoint from the
standard library, and a registry test pins the disjointness.

## Privilege model

New closed `PrivilegeClass` set (`Execute`, `Inspect(InspectPrivilege)`,
`SecurityAdmin`) and a durable `PrivilegeGrant { grantee, class, object }`
row set in migration 0028. `authorise_privilege` on `SecuritySnapshot`
decides the closed ladder/classifier check; denial reasons follow the
existing `security_admin:%` / `inspect:%` pattern. `SecurityAuditKind`
gains `SecurityAdmin` with an allowed/denied decision shape recording the
operation kind, principal, and target identity — never argument payloads,
mirroring the capability audit redaction.

## Enforcement gap closed

The existing system-function authorisation admits any sealed identity to any
session. The admin kernel methods add a `SecurityAdmin`-privilege gate (or
the CLI host checks the service identity like the fixed-service grant path)
before any mutation, and audit every mutation through the closed
`SecurityAdmin` kind.

## Consequences

- `crates/orna-core/src/system.rs` registry extension, `security.rs`
  privilege/decision model, migration 0028
  (`security_privilege_grants` + audit kind extension),
  `crates/orna-postgres/src/kernel/security_admin.rs`, the extended
  `orna security` CLI, and a live proof mirroring `inspect_live.rs`.
- Deferred (documented, not invented): `create_delegation`,
  `terminate_session`, secret-channel enrolment, sealed `sys.invoke`
  routing of system identities, and the `sys.security` grammar
  statements (the syntax owner wires those).
- No owned-path changes; no new dependencies; `Cargo.lock` untouched.