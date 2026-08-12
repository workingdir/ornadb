# ADR 0023: Local Sessions Authenticate with Unix Peer Credentials

**Status:** Accepted

## Decision

The first trusted authentication mechanism is the operating system credential
attached to an accepted Unix-domain stream. The server reads the peer UID from
the connected socket. No request byte, environment value, command argument, or
PostgreSQL role name may select an Orna principal.

The protected security catalogue contains one-to-one mappings from a numeric
Linux UID to an active `USER` or `SERVICE` principal. A UID and a principal may
each occur at most once. A missing mapping, unknown principal, disabled
principal, or role principal fails authentication without creating a session.

The trusted kernel recovers the mapping, principal, and role-membership state
in one repeatable-read transaction. It derives every reachable active role in
canonical identity order and returns an `AuthenticatedSession`. The local
client cannot submit a principal or active-role list. Explicit role selection
is deferred until a protected session-control operation exists.

This is authentication for Orna's public local transport. It is distinct from
the fixed peer mapping used internally between the Orna server and its embedded
PostgreSQL machinery.

## Protected storage

Migration 10 adds `_orna_kernel.security_local_peer_credentials` with:

* an unsigned 32-bit UID represented by a checked PostgreSQL integer;
* one unique principal identity referencing `security_principals`;
* no name, password, token, secret, environment value, or PostgreSQL identity;
  and
* no privileges for `PUBLIC`.

The complete security replacement and recovery operations include these
records. Replacement remains serializable and recovery remains fail closed.

## Trusted socket adapter

The server adapter accepts an already-connected Unix stream, reads Linux
`SO_PEERCRED`, and passes only its kernel-supplied UID to the PostgreSQL kernel.
The adapter does not accept a UID parameter beside the stream. A socket error,
unsupported peer credential, missing mapping, invalid durable state, or failed
database shutdown fails authentication.

The adapter establishes identity only. It does not decode calls, expose a
listener, authenticate TCP, issue a reusable token, or persist a session row.

## Required proof

Tests must prove:

* duplicate UIDs and duplicate mapped principals are rejected;
* unknown, disabled, and role principals cannot authenticate;
* nested reachable active roles are derived in canonical order;
* disabled roles are omitted and cannot grant authority;
* replacement and restart recovery preserve the exact mapping;
* an unmapped UID fails without falling back to an Orna or PostgreSQL identity;
* the Unix adapter uses the actual peer UID from `SO_PEERCRED`; and
* callers cannot supply a principal or role list at the adapter boundary.

Every success and failure path must close its PostgreSQL session. Normal
format, strict Clippy, rustdoc, diff, similarity, and live PostgreSQL gates
remain required.

## Deferred surface

This record does not define passwords, external providers, credential secret
storage, enrolment commands, durable session records, audit events, delegation,
role-selection frames, protocol negotiation, TCP/TLS, raw calls, or
`sys.invoke`. Those require later accepted decisions.

## Precedence

This implements the first local authentication mechanism required by milestone
3 and the local-transport security rule in the canonical wire design. It does
not complete milestone 3 session storage or milestone 4 transport.
