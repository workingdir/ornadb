# ADR 0035: Catalogue Health Is One Mandatory System Function

**Status:** Accepted

## Decision

The first Ring-1 system function is the sealed kernel intrinsic
`sys.catalog.health`. It has the stable identity
`FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])`.
It takes no arguments and returns exactly one non-null Boolean value.

The identity is reserved. An application catalogue that contains it is
invalid and cannot become active or recover as active.

`sys.catalog.health` is not an application function and is not part of the
`std` catalogue. Its implementation is pinned by the signed Orna
distribution. The active database `RevisionPair` still pins the catalogue and
security state inspected by one call. No PostgreSQL function, SQL source,
application artefact, standard-library source, environment value, or
configuration file can replace the intrinsic.

The health result is `TRUE` only after one transaction has verified:

* the current kernel migration set;
* the complete active application revision;
* its accepted standard-library context; and
* the complete protected security snapshot for the same revision pair.

A verification, transaction, audit, commit, driver, or shutdown failure is an
operational failure. The function does not return `FALSE` for an invalid or
partly recovered database.

## Protected system access

Every active authenticated `USER` or `SERVICE` session may execute this one
function without a stored `EXECUTE` grant. This is a closed system access rule,
not a wildcard or public grant. It applies only to the exact health identity.
Application functions retain the explicit direct-or-selected-role grant rule
from ADR 0020.

The security decision module owns the rule. It first revalidates the trusted
session and exact revision pair. A valid system decision records the session
principal as the effective and authorising principal. An invalid session or
revision produces the existing typed denial. Any other function still requires
an explicit grant or returns the existing unknown/missing-grant denial.

The kernel appends the normal protected `EXECUTE` audit record before it runs
the intrinsic. The intrinsic cannot be called through a private health-check
shortcut, and `orna-server` cannot fabricate an authorised result.

## Installed recovery identity

The first installed instance reserves one stable active `SERVICE` principal
`PrincipalId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])`
for the `orna` operating-system service account and maps that account's exact
kernel-supplied UID through the protected local-peer table. Instance setup
creates the mapping transactionally after standard-library installation. An
existing different principal, UID mapping, kind, status, or duplicate record
fails closed; setup does not replace it.

This identity exists only so the service operator has one recovery principal
before a later security-administration surface exists. It grants no
application function. A later administration decision may add user mappings,
but it cannot remove or reinterpret this recovery identity while this contract
is active.

## Raw recovery command

The command accepts either closed spelling:

```text
orna raw-call sys.catalog.health
orna raw-call <canonical-health-function-id>
```

The name is one exact ASCII literal mapped locally to the fixed identity. It
does not introduce a general function-name resolver, catalogue lookup,
case-folding, search path, alias, or prefix match. Every other non-canonical
identifier remains invalid command input.

The command retains ADR 0033's fixed socket, protocol 1, empty arguments,
framing, flow control, cancellation, diagnostics, and status codes. Success
writes exactly one canonical `ORV1` Boolean `TRUE` envelope to standard output
and nothing to standard error.

## Required proof

Tests must prove:

* the reserved name and canonical identity select the same exact raw-call
  bytes and every similar name fails before socket access;
* the service UID maps to one exact active service principal and conflicting
  or repeated setup fails without repair;
* the protected decision allows only the exact health identity for a valid
  authenticated session without a stored grant;
* invalid, disabled, role, stale, and forged sessions fail through the existing
  denial and audit boundary;
* ordinary application functions still require their exact stored grant;
* one transaction verifies migrations, active revision, standard context, and
  security state before it emits `TRUE`;
* every allowed and denied call appends exactly one protected execute audit;
* every operational failure returns `INTERNAL_FAILURE`, emits no value, and
  retains its private typed source;
* the canonical-identity and reserved-name commands produce byte-identical
  standard output; and
* an installed clean instance executes the command as the `orna` service
  account, returns one exact Boolean envelope, and leaves no client or server
  task behind.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
live PostgreSQL, raw-socket, and clean-package gates remain required.

## Implementation sequence

1. Accept and index this system-function boundary.
2. Add the fixed system identity and the closed authenticated-system decision
   to the core security module.
3. Add transactional, idempotent installation of the reserved service
   principal and local-peer mapping.
4. Route the exact health identity through the existing protected raw kernel
   transaction and return one Boolean value.
5. Accept the exact reserved CLI name as an alias for the fixed identity.
6. Prove the installed service-account command on the clean package fixture.

Each implementation commit changes one to three files, uses a signed
conventional commit, and keeps the repository buildable.

## Deferred surface

This decision does not define a general system-function catalogue, inspectable
intrinsic source, general name resolution, user enrolment, role selection,
security administration, additional system functions, arguments, protocol
selection, `sys.invoke`, presenters, JSON, or public audit inspection.

## Precedence

This decision implements the first mandatory Ring-1 recovery function from
`spec/docs/06-bootstrapping-recovery.md`. It extends ADRs 0020, 0021, 0023,
0024, 0032, and 0033 only for the exact reserved identity and service recovery
principal described above. Every application-function, transport, and raw-call
rule remains unchanged.
