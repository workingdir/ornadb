# Work ADR 0095: OrnaDB 1.0 Principal, Credential, and Delegation Scope

**Status:** Accepted deferral for OrnaDB 1.0

## Decision

OrnaDB 1.0 does not accept a built-in application-principal catalog, credential
enrollment or provider-authentication model, multi-user authorization service,
or delegated/effective-principal sessions. Such behavior is deferred unless a
later version accepts an authoritative security contract for it. This decision
resolves the 1.0 scope of the principal, credential, and delegated-session
request; it does not authorize implementation of proposal-only behavior.

The 1.0 secret surface remains the narrower `sys.Secret` contract:

* A `SecretRef` has a stable displayable/serializable name, while its resolved
  value is not exposed (**ORNA-SECRET-003**).
* `sys.Secret` exposes non-sensitive metadata such as name, provider, and
  availability, but never secret contents (**ORNA-SECRET-004**).
* Secret values are not committed as plaintext by default and are redacted
  from Inspect, Display, Present, diagnostics, traces, and `sys`, except where
  an explicitly privileged operation requests disclosure
  (**ORNA-SECRET-001**, **ORNA-SECRET-002**).

That metadata does not identify an authenticated Orna principal, select an
authorization role, enroll or verify a credential, or grant authority to act
for another principal.

Orna 1.0 instead uses the trusted-host boundary. Local commands trust the
invoking OS user and do not define principals, groups, grants, row ACLs, device
roles, or enterprise policy (**ORNA-TRUST-001**). Core serving defines no
principals, groups, grants, per-device roles, or row-level permissions
(**ORNA-SERVE-005**). The `sys.admin` boundary relies on OS/process isolation,
Git/SSH credentials, and the authenticated network perimeter; it is not a
built-in grants or role engine (**ORNA-SYS-088**).

The `orna.present.v1` protocol's session resumption preserves request
reservations and terminal outcomes during its lease and invalidates old
operational handles on a new runtime generation (**ORNA-PROTO-002**). This is
protocol/session recovery, not principal delegation or privilege transfer.
This ADR leaves that normative protocol contract unchanged.

## Context and authority

This ADR records the acceptance resolution for contract-gate issue #20 and the
Orna 1.0 deferral of the implementation request in issue #52. Both concern a
proposed broader security model whose credential-provider, principal metadata,
and delegated-session behavior has no acceptance basis in the frozen OrnaDB
1.0 source. The reference's explicit boundaries control this decision:

* `source/26-security.md`: **ORNA-TRUST-001** through **ORNA-TRUST-003**;
* `source/28-serving.md`: **ORNA-SERVE-005**;
* `source/15-system.md`: **ORNA-SYS-088**;
* `source/27-secrets.md`: **ORNA-SECRET-001** through **ORNA-SECRET-004**;
* `source/30-protocol.md`: **ORNA-PROTO-002**.

The existing administration boundary in work ADR 0065 remains unchanged. The
accepted Orna 1.0 reference remains authoritative over work proposals. Any
later principal, credential-provider, or delegation feature needs a separate
accepted contract and versioned implementation task before it can be built.
