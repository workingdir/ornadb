# ADR 0021: PostgreSQL Persists the Security Decision Snapshot

**Status:** Accepted

## Decision

The private PostgreSQL kernel persists the records required to reconstruct the
security decision core from ADR 0020. Migration 0009 adds exactly three
protected `_orna_kernel` tables:

```text
security_principals
security_role_memberships
security_execute_grants
```

`security_principals` stores the stable `PrincipalId`, kind, and status.
`security_role_memberships` stores the directed member-to-role edges.
`security_execute_grants` stores one grantee and stable `FunctionId` per direct
grant.

All identities are exact sixteen-byte values. PostgreSQL enforces unique rows,
known principal foreign keys, the closed kind and status spellings, and no
direct self-membership. The schema and tables grant no authority to `PUBLIC`.
No trigger, stored procedure, procedural language, credential column, or
PostgreSQL role is introduced.

The stable `FunctionId` in a grant is intentionally not foreign-keyed to one
catalogue revision. Grants survive semantic revision changes to the same
function identity. Recovery validates every grant against the functions in the
current active catalogue before it constructs a `SecuritySnapshot` bound to
that active `RevisionPair`.

PostgreSQL constraints are storage defence, not the complete security
authority. Recovery must pass every row through `SecuritySnapshot::new`, which
rejects a non-role target, indirect role cycle, dangling function, and every
other cross-row invariant from ADR 0020.

## Kernel interface

`PostgresKernel::recover_security_snapshot` opens one read-only, repeatable-read
transaction, requires the current migration set, locks no application row, and
loads:

1. the active source and catalogue revision identities;
2. the active catalogue's complete function identity set;
3. principals ordered by identity;
4. memberships ordered by member then role; and
5. grants ordered by grantee then function.

It decodes closed spellings to core enums, constructs the snapshot, commits,
and closes the PostgreSQL session before returning. Missing, malformed,
duplicated, dangling, or cyclic state fails without repair.

The first mutation interface replaces the complete security snapshot in one
serializable transaction. It requires the caller's revision pair to equal the
locked active pair, validates the candidate in core before writing, deletes
dependent rows before principals, inserts in deterministic identity order,
and re-recovers the result before commit. A later administration layer may
offer smaller protected operations; it must not bypass this validation seam.

## Required proof

The migration proof must show exact tables, columns, constraints, indexes,
ACLs, checksum, no procedural-language dependency, empty-state bootstrap, and
upgrade without changing application revision bytes.

Recovery and replacement tests must show:

* an empty snapshot survives reconnect;
* USER, ROLE, SERVICE, nested membership, disabled state, and grants recover
  exactly;
* a recovered direct or selected-role grant allows only its pinned target;
* revocation and disablement survive reconnect and deny an old session;
* stale revision replacement rolls back without changing stored rows; and
* durable role-kind, cycle, unknown-function, and malformed-byte tampering fail
  closed without repair.

Normal format, strict Clippy, rustdoc, diff, similarity, and live PostgreSQL
gates remain required.

## Deferred surface

This record does not accept principal names or attributes, credentials,
authentication providers, persisted sessions, audit events, delegation,
policies, object privileges, grant options, `SECURITY DEFINER`, capabilities,
public administration functions, protocol framing, `sys.invoke`, or a CLI.

Each requires a later accepted decision and fail-closed implementation.

## Precedence

This implements only durable storage and recovery for the decision core in ADR
0020. It advances milestone 3 in `spec/docs/38-implementation-roadmap.md`
without claiming that the canonical security catalogue or authenticated
invocation path is complete.
