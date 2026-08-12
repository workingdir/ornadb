# ADR 0020: Authenticated Sessions Authorise Pinned Function Execution

**Status:** Accepted

## Decision

The first Orna security module is a deny-by-default decision core. It validates
one immutable view of principals, role memberships, function-specific
`EXECUTE` grants, known functions, and the active `RevisionPair`. It exposes
only two operations after construction:

1. bind a trusted, already-authenticated session principal and its selected
   active roles; and
2. authorise one known `FunctionId` at one exact `RevisionPair`.

An allowed decision records the session principal, effective principal,
selected active roles, authorising principal, function, and revision pair.
For this first slice, the effective principal is the session principal because
the executable CLIENT function fixed by ADR 0015 is `SECURITY INVOKER`.

An invocation request never supplies credentials, a session principal, an
effective principal, or active roles. A later authenticated connection adapter
will establish those values and bind the session before decoding or executing
an invocation. The security module accepts no credential material.

## Principal and grant model

This slice recognises three principal kinds:

```text
USER
ROLE
SERVICE
```

A principal is either active or disabled. Only an active `USER` or `SERVICE`
principal can be a session principal. An active `ROLE` can be selected only
when the session principal reaches it through the validated membership graph.

A membership is directed from a member principal to a containing role, as in
`GRANT developer TO bob`. Role membership may be nested. The snapshot rejects:

* duplicate principal identities;
* duplicate memberships or grants;
* a missing member, role, grantee, or function;
* a membership target that is not a role;
* self-membership; and
* every direct or indirect role-membership cycle.

An `EXECUTE` grant names one grantee `PrincipalId` and one `FunctionId`.
Authorisation succeeds only when the grantee is the active session principal
or one of the session's explicitly selected active roles. A reachable but
unselected role grants nothing. If several grants match, a direct grant takes
precedence; otherwise the lowest canonical role identity is recorded. This
keeps the decision independent of input ordering.

The snapshot owns the exact active `RevisionPair` and the closed set of known
functions for that pair. A request for an unknown function, a stale or future
revision pair, or a function without a matching grant returns a typed denial.
It never reaches CLIENT evaluation or a SERVER executor.

## Interface and seam

The interface lives in `orna_core::security`. Callers construct plain immutable
records, then use `SecuritySnapshot` as the single decision module. Tests and
future PostgreSQL recovery use the same public interface.

Invalid catalogue state and invalid session selection are construction errors.
An ordinary access refusal is an `ExecuteDecision::Denied` value, not a module
failure. The allowed value is the only value a later server-facing invocation
adapter may accept before calling `evaluate_client_function` or any SERVER
executor.

## Required proof

Public-interface tests must prove:

* a direct active-principal grant allows the exact function and revision;
* a selected reachable role grant allows execution;
* an unselected, unreachable, disabled, non-role, or unknown active role is
  rejected before a session is bound;
* an unknown, disabled, or role session principal is rejected;
* another function, another revision pair, and a missing grant deny execution;
* duplicate, dangling, self-referential, and cyclic catalogue records reject
  the complete snapshot; and
* record order does not change the decision or its evidence.

Normal format, strict Clippy, rustdoc, diff, and similarity gates remain
required.

## Deferred surface

This record does not accept:

* credentials, authentication providers, password or token handling, external
  principals, delegation, impersonation, or session persistence;
* PostgreSQL security migrations, recovery, mutation, audit storage, or
  protected administration functions;
* `SECURITY DEFINER`, ownership transitions, policies, object privileges,
  grant options, revocation cascades, or CLIENT capabilities;
* the canonical value codec, public socket protocol, raw call frames,
  cancellation, `sys.invoke`, or `orna invoke`; or
* direct access from a client-supplied principal or role identity to CLIENT or
  SERVER execution.

Each requires a later accepted decision and fail-closed implementation.

## Precedence

This implements only the in-memory decision core of milestone 3 in
`spec/docs/38-implementation-roadmap.md`. It follows the trust model in
`spec/docs/35-security.md` and the rule in `spec/docs/13-invocation-system.md`
that credentials and principal identities do not occur in invocation requests.

It preserves ADR 0015's statement that the Boolean CLIENT evaluator is a
post-authorisation boundary. It does not yet complete the canonical security
catalogue, authenticated session, audit, protocol, or invocation checklist
rows.
