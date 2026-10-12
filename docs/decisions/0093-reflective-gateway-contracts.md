# ADR 0093: Gateway and Protocol Contract Boundaries

**Status:** Accepted scope; generic reflective gateways deferred

## Decision

For OrnaDB 1.0.0, accept only the endpoint, exposure, service, security,
conversion, redaction, and lifecycle contracts already defined by the frozen
Orna 1.0.0 reference:

* `orna serve` exposes the clone's Git transport, page/query endpoints, and
  presentation WebSocket. It binds to loopback by default. Binding beyond
  loopback is explicit and reports the exposed interfaces. Remote deployment
  relies on the trusted SSH, Tailscale, firewall, or authenticated
  reverse-proxy/TLS perimeter described by the reference.
* The interoperable live service is exactly `orna.present.v1`: one typed
  session-creation, resume, and deletion API and the registered WebSocket
  message set in `source/30-protocol.md`. Its protocol version, canonical
  OVB-1 values, request/session identities, errors, size limits, and session
  lifecycle are the ones specified there.
* Authentication remains the 1.0 trusted-host boundary. Local commands trust
  the invoking OS user; the protocol checks its configured perimeter policy
  and Origin. Orna 1.0 does not define application principals, roles, grants,
  or a separate multi-user authorisation service.
* Reflective invocation remains typed `sys.invoke<T>`/`sys.start<T>` over
  resolved function identities and canonical typed arguments. `sys.Value` is
  not an implicit conversion route, result witnesses are checked before
  effects, and no conversion chain is inferred.
* Protocol errors and diagnostics retain the reference's safe fields and
  redaction rules; they do not disclose plaintext credentials or private host
  paths. Session deletion, cancellation, resumption, attachment replacement,
  leases, and expiry follow the protocol's stated ownership and cleanup rules.

This decision does not add a general-purpose reflective gateway or a new
service abstraction. Generic function exposure, service discovery, JSON-RPC,
MCP, custom authentication/authorization, arbitrary conversion, and alternate
gateway session lifecycles remain deferred. Examples and proposal documents do
not reserve endpoint names, protocol identities, error codes, or compatibility
behavior for those surfaces. A later acceptance requires an authoritative spec
decision that defines their identities, errors, ownership, security, lifecycle,
redaction, and conformance proof.

This is a contract crosswalk and scope decision, not an implementation claim.
The installed product may still fail closed where the 1.0.0 endpoint or
protocol behavior is not implemented. Missing implementation is a conformance
gap; it does not authorize a different contract.

## Context

The issue proposed separate contracts for reflective gateways, protocol
gateways, and wire protocol, but the named `spec/docs/19-reflective-gateways.md`,
`spec/api/protocol-gateways.md`, and `spec/docs/27-wire-protocol.md` files are
not present in this repository or its spec checkout. Their open status cannot
replace the frozen 1.0.0 source. The reference already gives a narrow, complete
contract for `orna serve` and `orna.present.v1`; it does not define a generic
gateway that exposes arbitrary reflected functions.

Earlier work ADRs record implementation boundaries and deferrals. They do not
weaken the canonical 1.0.0 requirements. In particular, ADR 0092's deferral of
remote session implementation remains an implementation status; it does not
defer the normative `orna.present.v1` contract. ADR 0086 and the earlier
resource/UI decisions continue to defer generic reflective gateway expansion.

## Authority and traceability

The following frozen reference sections control this decision:

* `source/28-serving.md`: `ORNA-SERVE-001` through `ORNA-SERVE-009`, including
  listener exposure and deployment trust.
* `source/30-protocol.md`: `orna.present.v1` transport, session creation,
  resumption, deletion, authentication boundary, typed envelopes, errors,
  limits, and lifecycle.
* `source/26-security.md`: `ORNA-TRUST-001` through `ORNA-TRUST-003` and the
  trusted-host and external-perimeter model.
* `source/15-system.md`: `INVOKE-1`, `ORNA-SYS-077`, `ORNA-SYS-130`, and
  `ORNA-SYS-132` for typed reflection and conversion boundaries.
* `source/29-formats.md`: the canonical OVB-1 typed value profile used by the
  wire contract.

**Precedence:** the frozen OrnaDB 1.0.0 reference is authoritative. This work
ADR records which existing contract the implementation must follow and what
remains outside that contract; it does not add or modify normative language.

## Work-item disposition

Beads task `ornadb-1787784779221-37-0c3a890a` (GitHub #37) requested a
JSON-RPC gateway adapter. Resolve that request as deferred for OrnaDB 1.0.0
under this accepted decision. The controlling requirements are
`ORNA-SERVE-001` through `ORNA-SERVE-009` and `ORNA-PROTO-001` through
`ORNA-PROTO-004`; they specify `orna serve` and `orna.present.v1`, not a
JSON-RPC gateway. This disposition adds no gateway endpoint, identity,
authentication, conversion, error, or lifecycle behavior. Reconsider the
adapter only after a separately accepted contract defines that behavior.

## Work-item disposition: authenticated transport and artifact exchange

Beads task `ornadb-1787784775745-13-5e17c033` (GitHub #13) requested authenticated remote transport and artifact exchange. The accepted OrnaDB 1.0 contract already defines the interoperable authenticated live-session boundary: `orna.present.v1`, session creation/resumption/deletion, cookie-authenticated WebSocket upgrades, and the TLS/perimeter constraints in `source/30-protocol.md`, `source/26-security.md`, and `source/28-serving.md`. The existing live host and client implement those specified paths. `source/24-git-history.md` defines remote repository exchange through ordinary Git operations and synchronization of required `refs/orna/*` references; the repository transport implements that behavior.

The frozen reference does not define a separate artifact upload/download API or wire format. `ORNA-SYS-072` requires source-snapshot and compatibility metadata in build artifacts; it does not define remote artifact exchange. The relevant normative search covered `Orna-1.0.0.md`, `source/15-system.md`, `source/24-git-history.md`, `source/26-security.md`, `source/28-serving.md`, `source/30-protocol.md`, `api/`, and the requirement records under `tests/`. The only artifact-transfer behavior defined for remotes is the ordinary Git path in `ORNA-REMOTE-001` through `ORNA-REMOTE-005`.

Resolve this 1.0 work item by retaining the specified authenticated live and Git transport behavior and deferring any separate artifact-exchange service. Do not add endpoints, credential formats, artifact identities, or transfer semantics without an accepted normative contract. A later version may define those details in a separately accepted specification.
