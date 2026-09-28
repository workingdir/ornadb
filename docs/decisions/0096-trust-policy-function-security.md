# Work ADR 0096: OrnaDB 1.0 Trust Policy and Function Security Boundary

**Status:** Accepted trust boundary; function security policies deferred

## Decision

For OrnaDB 1.0, accept the trusted-host and network-perimeter security model
defined by the frozen reference:

* Local commands trust the invoking OS user. Orna 1.0 defines no application
  principals, groups, grants, row ACLs, device roles, or enterprise policy
  language (**ORNA-TRUST-001**).
* `orna serve` binds to loopback by default. Remote deployment SHOULD use
  ordinary SSH, Tailscale, or a trusted authenticated reverse proxy/TLS
  boundary (**ORNA-TRUST-002**).
* Public anonymous mutation and arbitrary remote REPL execution are outside
  the trusted 1.0 profile (**ORNA-TRUST-003**).

Orna 1.0 therefore does not define function owners, policy evaluation, a
privilege transition for `SECURITY DEFINER`, fixed dependency or search-path
semantics for a definer, or a dynamic SQL rule. Those behaviors remain
deferred. The appearance of a `FunctionSecurity` field in parser or catalogue
implementation does not, by itself, establish source-defined execution
semantics.

This decision accepts no implicit default security mode and makes no claim
that `SECURITY DEFINER` or `SECURITY INVOKER` has normative 1.0 semantics. A
later version may define these only through an authoritative contract that
specifies owner identity, privilege evaluation, transition and restoration,
dependencies, name resolution, dynamic execution, errors, audit behavior, and
conformance proof.

## Context and authority

The frozen OrnaDB 1.0 reference chapter `source/26-security.md` contains the
trust boundary in **ORNA-TRUST-001** through **ORNA-TRUST-003**. Searches of the
frozen `source/`, `grammar/`, `examples/`, and `tests/` corpora found no
`SECURITY DEFINER`, `SECURITY INVOKER`, function-owner, or function-policy
contract. This absence does not supply default semantics; it leaves the
proposal-level implementation fields outside the accepted 1.0 surface.

Work ADR 0090's accepted local authenticated-session boundary remains as
recorded. This ADR does not extend that boundary into role-based or definer
authorization and does not change the normative trust requirements.
