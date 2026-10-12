# Work ADR 0099: OrnaDB 1.0 CLIENT Capability-Grant Configuration Boundary

**Status:** Local grant configuration deferred for OrnaDB 1.0

## Decision

OrnaDB 1.0 does not define a built-in CLIENT capability-grant database or a
local configuration contract for loading grants. The trusted-host boundary
and the absence of an Orna grants database are explicit: local commands trust
the invoking OS user and Orna v1 defines no principals or grants
(**ORNA-TRUST-001**); core serving defines no principals, groups, grants,
per-device roles, or row-level permissions (**ORNA-SERVE-005**); core Orna
does not add a grants database (**ORNA-WIRE-011**); and `sys.admin` must not be
represented as a built-in enterprise grants or role engine
(**ORNA-SYS-088**).

Accordingly, this ADR defers CLIENT capability-grant loading from local
configuration. It does not define or infer a configuration path, file format,
schema, owner, precedence, reload behavior, grant scope, or CLI. The
`LocalCapabilityGrant` model and configuration assumptions in work ADR 0060
remain a work-level design; they are not accepted OrnaDB 1.0 requirements and
must not be presented as 1.0 conformance behavior.

## Search evidence

The frozen reference was searched across `source/`, `grammar/`, `tests/`,
`examples/`, and `api/` for `capability`, `capability grant`, CLIENT grant,
local configuration, and grant-configuration terms. It defines no CLIENT
capability declaration or local grant-loading contract. The only capability
match in normative source is `source/30-protocol.md`, where the session UUID
is explicitly not a capability; it supplies no CLIENT grant semantics.

Reference test scenario `SEC-001` says that starting the trusted server does
not create an Orna grants database and maps that check to the normative
loopback, explicit-exposure, and trust requirements. The authoritative
requirements controlling this boundary are **ORNA-TRUST-001**,
**ORNA-SERVE-005**, **ORNA-WIRE-011**, and **ORNA-SYS-088**. The frozen
reference remains authoritative over work ADRs.
