# Work ADR 0105: Studio Security and DBA Page Boundary

**Status:** Deferred for OrnaDB 1.0.0

## Decision

Defer a Studio security/DBA page component. This decision does not remove or
change the existing security administration implementation or its CLI surface.
It records that the accepted Orna 1.0.0 reference and current Studio runtime
boundary do not define a production Studio page, a CLIENT-to-administration
authority path, or the page's layout and interaction contract.

The frozen reference establishes these applicable limits:

* Local commands trust the invoking OS user; Orna v1 does not define
  principals, groups, grants, row ACLs, device roles, or an enterprise policy
  language (**ORNA-TRUST-001**, `source/26-security.md:5`).
* An administrative call is admitted only through a trusted local process or
  an authenticated endpoint explicitly permitting administration. An
  application evaluation endpoint does not gain administrative permission
  merely because it accepts Orna expressions (`source/16-administration.md:7`).
  Every administrative call is audited with safe arguments and its terminal
  outcome (**ORNA-ADMIN-001**, `source/16-administration.md:9`); an
  administration call cannot run reentrantly inside an activation with
  uncommitted writes (**ORNA-ADMIN-002**, `source/16-administration.md:11`).
* Secret values remain redacted from Inspect, Display, Present, diagnostics,
  traces, and `sys` (**ORNA-SECRET-002**, `source/27-secrets.md:9`). If a
  future page displays `sys.Secret`, that surface may expose metadata such as
  name, provider, and availability, but not the secret contents
  (**ORNA-SECRET-004**, `source/27-secrets.md:37`).

Work ADR 0065 records the repository's security-function implementation and
CLI. It does not define a Studio page. Its sealed `sys.invoke` routing note
defers system-function routing. Work ADR 0090 records the local authenticated
session authority boundary, not a production Studio VM or page. Work ADR 0103
defers the Studio-specific runtime, explorer layout, navigation, and
interaction model. Work ADRs 0086 and 0100 continue to govern Inspector
projections and Studio source tooling respectively; neither supplies the
missing page contract.

Do not infer administration authority from an ordinary CLIENT page or add
credential enrollment, delegation, policy editing, audit querying, or
secret-disclosure controls. Reconsider this page only after a versioned
contract defines its trusted host entry, permitted operations, safe argument
and result projections, failure/outcome display, and interaction behavior.

## Search evidence

The frozen corpus search below covered `Orna-1.0.0.md`, `source/`, `grammar/`,
`tests/`, `examples/`, and `api/` for an explicit Studio security/DBA page,
ordinary-CLIENT security surface, and the named `sys.security` administration
functions:

```sh
rg -n -i 'Studio|security.*page|dba page|ordinary CLIENT|CLIENT.*security|sys\.security\.(create_principal|disable_principal|create_role|grant_role|revoke_role|grant_privilege|revoke_privilege|can_execute|has_privilege)' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/Orna-1.0.0.md \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/grammar \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/tests \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/examples \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/api
```

It returned no matches (exit 1). The positive security and admission
requirements cited above were verified directly at their listed chapter and
line locations. A current-main filename search for Studio/security/DBA page
components and frontend component suffixes found only
`crates/orna-client/examples/studio_demo.rs`,
`docs/decisions/0100-studio-source-tooling-boundary.md`, and
`docs/decisions/0103-studio-runtime-inspector-reference-boundary.md`; there
is no production Studio application/component tree to extend. ADR 0103's
captured reference and filename searches independently record the same
Studio-runtime boundary.

## Precedence

The frozen Orna 1.0.0 reference is authoritative. This ADR adds no Studio
component, CLIENT authority, security policy, administration API, credential
flow, or audit-query contract.
