# Work ADR 0089: Resource Lineage Authority

**Status:** Accepted

## Context

The accepted resource contract distinguishes invocation authority from lineage
metadata. Spec ADR 0017 requires a resource to run as a nested invocation under
the authenticated root: the server derives the session principal, effective
roles, and security context, while the request carries parent and call-site
identities for correlation. Work ADRs 0077, 0078, and 0079 carry that boundary
into the accepted language, transport, and action slices.

Beads issue `ornadb-el1.2.1.34` records this as a contract clarification,
not an accepted P1 security bug. The implementation has two kinds of
construction path. Compiled CLIENT plans
and the installed authenticated host are execution paths. The local
`ClientResourceInvocationContext` and `ClientResourceRequest` APIs also expose
low-level lifecycle seams used by compatibility callers and focused tests.
Those seams can accept caller-provided non-zero correlation metadata in an
in-process caller. That observation is not evidence of a privilege
escalation, cross-principal execution, or a transport defect.

## Decision

Compiled evaluator plans and installed authenticated execution are the trusted
source of resource principal, profile, and instance lineage. They retain the
checked target and call-site identity, inherit the current trusted parent and
state context, and do not let source expressions provide a principal, role,
`run_as`, grant, credential, or unchecked result type.

`parent_invocation_id` and `call_site_id` are non-zero correlation identities,
not authority. They identify the enclosing invocation and checked operation for
transport, audit, and lifecycle correlation; neither selects a principal,
role, grant, target revision, or execution permission. The server recovers
those authorities from the authenticated session, active catalogue, security
snapshot, and sealed `sys.invoke` path.

## Trusted evaluator and action rules

The evaluator entry point
`crates/orna-client/src/lib.rs::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation`
seeds the enclosing invocation and state context. Its resource evaluation
branch in `evaluate_function_with_fuel` uses `lineage.current` with the
compiled operation's `call_site_id` when it builds a
`ClientResourceInvocationContext`. The checked plan, active revision, target,
argument identities, and local capability requirements remain the evaluator's
inputs to validation.

Actions do not trust call-site metadata carried in a transient value. The
`trigger_client_action_with_lineage` path allocates a fresh `CallSiteId` and
uses the current lineage and inherited state profile and instance key when it
builds the request. Every trigger also gets a fresh request identity and
resource generation. This fresh call-site rule is intentionally retained.

Installed execution continues to bind the typed context to the authenticated
resource path. `crates/orna-server/src/invoke.rs::InstalledClientResourceExecutor::execute`
converts the validated `ClientResourceRequest` context into an
`ORNA-RESOURCE/1` `ResourceRequest`; the PostgreSQL kernel then performs the
authenticated target and security checks before creating nested invocation
evidence.

## Direct-constructor boundary

`ClientResourceInvocationContext::new`, the public
`ClientResource::begin_request*` methods, and the internal
`ClientResourceRequest::new` constructor are low-level compatibility and test
seams. They are not hostile external-plugin APIs and must not be treated as
an authority boundary. Their accepted caller metadata can represent
in-process correlation or instance metadata, but it does not grant execution
authority. No code change is required for this decision.

If OrnaDB introduces an external plugin API, that API requires a separate
accepted authenticated binding contract before it can construct or submit
resource lineage. That future contract is outside this ADR; this decision does
not turn the existing low-level seams into a plugin surface.

## Invariants kept

This decision preserves the existing validation and proofs:

* `crates/orna-client/src/lib.rs::ClientResourceRequest::new` validates
  the active revision, target, result type, arguments, digest, non-zero
  invocation identities, and NUL-free context text before accepting a request.
* `crates/orna-protocol/src/frame.rs::encode_resource_request` and
  `decode_resource_request`, using `require_resource_invocation_id` and
  `require_resource_call_site_id`, enforce the canonical `ORNA-RESOURCE/1`
  shape and non-zero `parent_invocation_id` and `call_site_id` values.
* `crates/orna-postgres/src/kernel/security.rs::validate_resource_lineage`
  rejects zero request, parent, or call-site identities before state mutation.
  `resource_parent_invocation_is_owned` binds the parent to the authenticated
  session before request reservation or target dispatch.
* Installed dispatch recovers the active revision and security snapshot, checks
  the target through the authenticated `sys.invoke` path, and generates the
  nested invocation identity in the kernel rather than accepting one from the
  request.
* Resource cache identity, generation checks, typed result checks, cancellation,
  redaction, and audit invariants remain those accepted by ADRs 0071, 0074,
  0077, and 0078. Action freshness remains the rule accepted by ADR 0079.

The correlation distinction does not weaken any of these checks and does not
add a second principal or grant authority.

## Alternatives rejected

### Treat direct constructors as hostile plugin APIs

Rejected for this slice. That interpretation would require an authenticated
lineage-binding contract, plugin identity, and admission rules that are not
part of the existing runtime or resource surface. It would expand the accepted
security and runtime boundary rather than clarify the current one.

### Treat parent or call-site identities as authority

Rejected. The accepted wire and database contracts use these values for
non-zero correlation and lineage checks. Principal, role, grant, target, and
revision authority comes from authenticated execution and recovered catalogue
and security state.

### Change the current constructors or add a compatibility wrapper

Rejected. The trusted evaluator and installed paths already provide the desired
lineage, and the existing direct seams are intentionally used by compatibility
callers and tests. A constructor hardening change would expand this decision
without an identified privilege or cross-principal failure.

## Proof and evidence

Evidence for this decision is the accepted contract in spec ADR 0017 and work
ADRs 0071, 0074, and 0077-0079, plus the implementation paths cited above.
The focused evidence includes
`crates/orna-client/src/lib.rs::resource_request_rejects_zero_lineage_before_loading`,
`crates/orna-client/src/lib.rs::action_trigger_does_not_forward_forged_call_site_metadata`,
and
`crates/orna-client/src/lib.rs::action_trigger_after_terminal_completion_allocates_fresh_request_identity`.
The PostgreSQL validation evidence includes
`crates/orna-postgres/src/kernel/security.rs::resource_lineage_validation_rejects_zero_parent_before_other_request_validation`,
and the installed resource proof is
`crates/orna-server/tests/standard_database.rs::installed_resource_socket_delivers_values_and_enforces_windows_and_grants`.
The installed PostgreSQL proof is environment-gated (`#[ignore]`) where its
ADR says so; this record claims the existing focused and installed proof paths,
not an additional local live run.

The evidence does not prove that arbitrary in-process callers cannot spoof
correlation or instance metadata through low-level constructors. It proves the
narrower accepted claim: those constructors are not the hostile plugin
boundary, while compiled evaluator plans and installed authenticated execution
retain trusted principal/profile/instance lineage. No privilege escalation or
cross-principal bug is claimed.

## Explicit non-goals

This ADR does not change code, tests, protocol bytes, database schema, audit
shape, resource lifecycle, cache identity, action scheduling, or authorization.
It does not add an external plugin API, authenticated plugin binding, a new
principal or grant mechanism, or a general lineage capability. Hostile external
plugins remain a future contract and require their own accepted decision.
