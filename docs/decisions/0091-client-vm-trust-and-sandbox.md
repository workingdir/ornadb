# Work ADR 0091: CLIENT VM Trust and Sandbox

**Status:** Proposed

## Context

The canonical architecture places `CLIENT` execution in the local `orna` client
VM, while `SERVER` execution remains beside the data. A remote database may
publish CLIENT artifacts, but connecting to that database must not grant those
artifacts ambient filesystem, process, credential, or network authority. The
canonical status record still lists the CLIENT VM bytecode and sandbox
implementation as open. This ADR proposes the next implementation lane without
pretending that the open trust work is already implemented.

The current work repository already has a useful evaluator boundary. The
`orna-client` crate exposes the `evaluate_client_function*` family, including
state, capability-grant, executor, and parent-invocation variants. Its
`ClientExecutionContext` carries the active `RevisionPair`, `FunctionId`,
`FunctionRevisionId`, and invocation lineage. The evaluator is intentionally
closed: it does not itself perform database, protocol, filesystem, process,
environment, clock, random, network, or runtime-library operations. Resource
work is handed to the caller-owned `ClientResourceExecutor` seam, and local
capability requirements are checked against the client-owned
`LocalCapabilityGrantSet`.

The existing `validate_client_artifact_integrity` check is narrower than the
boundary proposed here. It checks that an `ExecutableArtifact` is a CLIENT
artifact and that its canonical payload digest matches its stored content hash.
The implementation documents that this proves payload integrity only; it does
not authenticate provenance, verify a signature, apply sandbox policy, or prove
host capabilities. The current `ClientArtifactIntegrityError` and
`ClientExecutionError` paths therefore remain evidence for the lower-level
checks, not evidence that a production trust envelope or sandbox broker exists.

Work ADR 0060 accepted the declarative CLIENT capability vocabulary and the
local grant gate, while explicitly deferring filesystem, network, and secret
mediation. Work ADRs 0071, 0074, and 0078 establish resource identity,
executor ownership, authenticated `ORNA-RESOURCE/1` transport, cancellation,
terminal ordering, and redacted audit rules. Work ADR 0084 establishes the
shared evaluator/runtime-host direction. Work ADR 0089 distinguishes trusted
evaluator lineage from correlation-only parent and call-site identities. Work
ADR 0090 accepts only local `SO_PEERCRED` authentication, authenticated session
and revision authority, and local grants; it explicitly defers production
CLIENT VM host tokens, artifact provenance/signatures, and sandbox mediation.
This proposed ADR composes those decisions and does not silently widen them.

The existing server `SharedInvokeBroker` is a resource transport and scheduling
broker governed by the resource decisions. The capability broker proposed here
is a separate host-owned boundary in `orna-client::vm`; this ADR does not rename,
replace, or reinterpret the existing resource broker.

## Canonical sources

The following repository documents are the source of the terms and constraints
used by this ADR:

| Source | Contract used here |
| --- | --- |
| [Current decisions and open questions](../../../spec/docs/02-status-decisions.md) | `CLIENT` runs in a sandboxed VM inside `orna`; CLIENT VM bytecode and sandbox implementation remain open. |
| [Source, compiler pipeline, AST, and semantic IR](../../../spec/docs/25-source-compiler-ir.md) | Stable IDs, source/revision snapshots, CLIENT artifact checks, required capabilities/contracts, content hashes, and language/VM version checks. |
| [Wire protocol and canonical typed values](../../../spec/docs/27-wire-protocol.md) | CLIENT artifact metadata, `FunctionId`/`FunctionRevisionId`, `RevisionPair`, canonical payloads, validation-before-execution, content-hash caching, bounded channels, and cancellation. |
| [Principals, authentication, authorization, capabilities, and auditing](../../../spec/docs/35-security.md) | No ambient local authority for remote CLIENT code, local capability policy, trusted-kernel enforcement, and protected redacted audit. |
| [System architecture](../../../spec/docs/04-system-architecture.md) | Process topology, trust boundaries, local runtime selection, failure isolation, and the rule that a database cannot force an arbitrary native library. |
| [SERVER and CLIENT functions](../../../spec/docs/05-execution-locations.md) | CLIENT responsibilities, one execution domain per function, asynchronous CLIENT-to-SERVER resources, and external runtime contracts. |
| [`sys.invoke` universal invocation system](../../../spec/docs/13-invocation-system.md) | Resolve/bind/authorise/execute ordering, authenticated request context, nested root invocation and trace, client artifact loading, cancellation, and event outcomes. |
| [Testing and conformance strategy](../../../spec/docs/39-testing.md) | Hostile CLIENT artifact capability tests, invocation success/failure/cancel/deadline coverage, redaction, audit completeness, and runtime conformance. |

Related accepted decisions are linked explicitly:

- [Spec ADR 0004: Root invocation is planned by `sys.invoke`](../../../spec/adrs/0004-sys-invoke.md)
- [Spec ADR 0006: Runtimes are explicitly installed client libraries](../../../spec/adrs/0006-runtimes-are-installed-libraries.md)
- [Spec ADR 0009: Preserve source and a resolved semantic graph](../../../spec/adrs/0009-source-and-semantic-graph.md)
- [Spec ADR 0014: Principals are first-class catalog data with kernel enforcement](../../../spec/adrs/0014-principals-are-first-class-and-dogfooded.md)
- [Spec ADR 0020: A programmable CLIENT core](../../../spec/adrs/0020-client-control-flow.md)
- [Work ADR 0060: CLIENT capability requirements and the local sandbox](0060-client-capability-requirements.md)
- [Work ADR 0071: CLIENT resource lifecycle](0071-client-resource-lifecycle.md)
- [Work ADR 0074: Runtime-only CLIENT resource executor seam](0074-client-resource-executor-seam.md)
- [Work ADR 0078: CLIENT-to-SERVER resource transport and scheduling](0078-client-server-resource-transport.md)
- [Work ADR 0082: The first production non-TTY runtime is Qt](0082-production-qt-runtime.md)
- [Work ADR 0084: Programmable CLIENT plans and shared runtime hosts](0084-client-control-flow.md)
- [Work ADR 0089: Resource lineage authority](0089-resource-lineage-authority.md)
- [Work ADR 0090: Local principal and session authority](0090-local-principal-session-authority.md)

The implementation references in this ADR describe existing seams or proposed
seams only. In particular, a link to
[`crates/orna-client/src/lib.rs`](../../crates/orna-client/src/lib.rs) does not
mean that the `orna-client::vm` module, the capability broker, or signed
attestation already exists.

## Decision

Adopt one additive CLIENT VM lane with three ordered trust layers:

1. **Structural artifact admission** is the next implementation slice. It is a
   pure, fail-closed preflight boundary around the current evaluator and binds
   an immutable CLIENT artifact to the exact active function and revision.
2. **Host-owned in-process capability mediation** is the selected sandbox
   boundary after structural admission. The host owns capability operations and
   issues opaque, non-serialisable, invocation-scoped leases; evaluator code and
   runtime libraries never receive ambient OS authority.
3. **Signed identity-bound attestation** is a selected extension, not a current
   implementation claim. It may become an additional admission policy for
   artifacts published by a remote database, but its cryptographic profile is a
   separate implementation gate described below.

This is an additive boundary in `orna-client::vm`. It may introduce proposed
host/admission types such as `ClientVmAdmission`, `VerifiedClientArtifact`,
`ClientVmHost`, `ClientCapabilityBroker`, and `CapabilityLease`, but it must
retain the existing `evaluate_client_function*` evaluator façade and its
state/executor/parent-invocation seams. The new boundary wraps and preflights
those APIs; it does not replace the evaluator, change CLIENT language meaning,
or create a second evaluator with different recursion, fuel, state, resource,
or action semantics.
The current `evaluate_client_function*` functions remain a
compatibility/test façade until Stage 1 integration. Their caller-supplied
declarations and grants, internal decode order, and existing error/state
behaviour continue to follow the accepted evaluator contracts. They are not
the production VM admission boundary and must not be used to claim lease,
attestation, or broker enforcement. Stage 1 adds a private evaluator seam that
consumes one admitted plan after the new checks, while the existing public
functions retain their current behaviour until production dispatch is migrated.

The private Stage 1 seam must not decode an admitted plan a second time or
silently replace its tuple. It may adapt the immutable admitted plan into the
existing private execution machinery, but the old façade and the new VM path
must share the same recursive, fuel, state, resource, action, and Inspector
semantics.

### Structural artifact admission tuple

Every artifact accepted by the new VM path has one canonical artifact identity
tuple:

```text
(
    FunctionId,
    FunctionRevisionId,
    RevisionPair,
    kind,
    format,
    outer_version,
    inner_version?,
    language,
    digest,
    declared_capabilities/contracts,
    artifact_declared_limits,
)
```

The tuple is separate from the mutable host admission context:

```text
HostAdmissionContext {
    grant_policy_epoch_or_digest,
    runtime_offer_identity,
    host_limit_ceiling,
    cancellation_and_deadline_epoch,
}
```

The signed or cached artifact identity never includes a host-specific ceiling,
deadline, cancellation epoch, or mutable grant set. The VM combines the
artifact-declared limits with the current host ceilings and uses the more
restrictive value.
The host derives `runtime_offer_identity` from an immutable snapshot of the
selected runtime descriptor, ABI version, contract offers, and ordered
properties. The snapshot has a canonical encoding and digest owned by the
host; mutable provider descriptors are never used as policy witnesses. Runtime
replacement or shutdown advances the host policy epoch and invalidates affected
admissions and leases.

The fields mean:

| Tuple field | Required fact and authority |
| --- | --- |
| `FunctionId` | The stable target identity resolved by `sys.invoke` and the active catalogue. It is not selected by an artifact payload or runtime callback. |
| `FunctionRevisionId` | The exact immutable `FunctionRevisionRecord` selected for the target. A different function revision is a different artifact identity even when payload bytes happen to match. |
| `RevisionPair` | The exact source/catalogue pair pinned by the authorised root invocation. A stale or future pair is rejected; the pair is not supplied by an untrusted artifact request. |
| `kind` | Exactly `ExecutableArtifactKind::Client`. A SERVER artifact or domain mismatch fails closed. |
| `format` and `outer_version` | The exact `ExecutableArtifact` format and outer plan version. Unknown, unsupported, or shape-incompatible values fail before evaluator execution. |
| `inner_version?` | The effective inner plan version for the version-five capability envelope. Version five therefore compares both its outer envelope version and its inner plan version; other plans use no inner version. |
| `language` | The function revision language version matched to the locally supported CLIENT language/VM version. A database cannot raise or substitute the supported version. |
| `digest` | The canonical payload digest, currently represented by `content_hash` and recomputed with `artifact_payload_digest`. A mismatch is a denial. |
| `declared_capabilities/contracts` | Requirements are derived from checked revision evidence and the decoded plan. A version-five envelope carries its requirements in the envelope; its canonical tuple encoding sorts requirement name and argument-source pairs and rejects duplicates. The new path admits legacy plans only when their checked operation graph has no capability or runtime-contract requirements; legacy plans with caller-supplied declarations remain on the compatibility façade until a production admission manifest is defined. Caller declarations cannot add authority to the new path. Runtime contracts are the exact closed references retained by the checked plan and active revision. |
| `artifact_declared_limits` | Limits encoded by the plan format and any future artifact-declared maxima: encoded size, decoded nodes/bodies/arguments, nesting, and declared operation budgets. Host ceilings, deadlines, and cancellation are policy context, not artifact identity. |

For current plan versions, the artifact-declared limit class is the fixed
versioned decoder/evaluator ceiling; the payload does not supply an arbitrary
limit field. A future plan may carry declared maxima only through an explicit
versioned codec. Local host ceilings remain a separate policy witness.
Tuple normalisation is distinct from the encoded plan. It does not rewrite
existing version-five declaration order or change plan bytes. The normalised
comparison form uses bounded UTF-8 name and argument-source values, preserves
the distinction between literal and parameter requirements, and rejects
duplicate capability names or contract identities according to the plan
contract.

Capability-operation closure is exact for operations represented by the current
plan model. Every decoded capability declaration, external runtime contract,
resource target, action target, or Inspector operation must be reconciled with
the checked revision evidence and current host policy before lease issuance.
The current plan model has no direct filesystem, network, or secret operation
node; Stage 2 must define the operation-to-capability mapping before adding
those backends. An undeclared operation, an over-declared requirement, a
duplicate, a dynamically widened scope, or a nested operation absent from the
parent's attenuated context fails closed. The structural slice must reconcile
the decoded operation graph with checked evidence rather than trust the fact
that a compiler normally produced the bytes.

The tuple is an admission identity, not a user-visible bearer token. Its
artifact fields are derived from trusted target/revision/catalogue state plus
canonical artifact bytes. The tuple and any retained decoded plan are
immutable after admission. A cache hit must reconstruct or revalidate the
tuple and the current `HostAdmissionContext`; a digest hit alone is never an
authorisation decision.

Admission has two distinct phases. First, the host performs bounded framing and
decoding with fixed byte/count/depth limits. This may inspect untrusted bytes,
but it must not construct an evaluator, acquire a descriptor, issue a lease,
or perform a host operation. Second, semantic admission checks the decoded
plan's references, exact function shape, capability-operation closure, runtime
contracts, active revision, and host policy. Only this second phase produces
the immutable admission value. Error precedence is bounded framing/size,
target and revision identity, digest, format/version/language, decoded shape
and references, capability/contract closure, host policy, then cancellation
and deadline. Any failure leaves evaluator, lease, cache, state, resource,
runtime, and host state unchanged.

The trusted inputs to the VM boundary are the authenticated
`AuthenticatedSession`/`AuthorisedInvocation`, the active
`ActiveDatabaseRevision`, the locally configured `LocalCapabilityGrantSet`,
and the client-owned runtime/host limits. Request bytes, database plan fields,
`PrincipalId` values, role lists, grants, credentials, parent/call-site
correlation IDs, and runtime callbacks are not authority inputs merely because
they are present in a value or frame.
The host allocates one non-zero root `InvocationId` and a collision-free child
identity sequence when it creates a VM. The root identity, authenticated
session binding, active revision pair, security-context digest, and root
security epoch are immutable for that VM lifetime. The host also owns a
mutable policy epoch that changes when grants, runtime offers, revisions,
sessions, or host ceilings change; each admission and lease captures the
current value and must revalidate it before use and at the effect fence.
A local child receives a fresh identity from the allocator; a nested
`ORNA-RESOURCE/1` request keeps its separate request/stream identity and
generation. These identities are correlation and lifecycle evidence, not caller
authority, and are never reused after release.

The uniqueness domain is one VM root and its lifetime for child identities. A
host-wide root registry reserves each non-zero root identity before VM
construction and retains a closed-root tombstone until process shutdown.
Concurrent root creation and release use the same linearizable registry
operation; a live or closed identity is never overwritten or reused. Root
zero, root-source exhaustion, child overflow, and duplicate allocation are
closed failures. Child allocation reserves its identity before returning it,
and release records it as closed. The VM root identity, local child identity,
and resource request/stream identity therefore have separate owners and
lifetimes.
Stage 1 keeps this registry in memory and uses it only for deterministic
control-plane tests. Before Stage 2 can recover an effect or audit terminal
across process restart, the authenticated host/kernel boundary registers a
non-reused `HostIncarnationId` and monotonically increasing fence epoch for one
host instance. The kernel-owned operation ledger durably records the
incarnation, fence epoch, and operation identity.
`HostIncarnationId` is a kernel-generated opaque 16-byte value and `fence_epoch`
is a persisted non-zero `u64`; neither is accepted from artifact bytes or an
ordinary request field. A registration transaction returns the pair only after
binding it to the authenticated host instance. Registering a new pair atomically
changes the previous pair for that instance from `Registered` to `Fenced` and
marks the new pair `Registered`. Retirement then changes it to `Retired` in the
same ledger. Recovery examines abandoned operations only after the fence
transition commits, and every old-host request fails the ledger epoch check
before idempotency or backend-start handling.

Registration has explicit liveness states: `Registered`, `Fenced`, and
`Retired`. A new registration for the same host instance atomically fences the
previous live incarnation before the recovery worker examines its intents.
Every VM, audit, lease, and backend-ledger request carries the registered
incarnation and fence epoch; the kernel rejects requests from a fenced or
retired incarnation, including a paused old host that resumes after recovery.
The recovery worker owns terminal-only closure of abandoned operations and
cannot create a lease, change principal authority, or start a new effect.

A Stage 2 operation identity is exactly the tuple
`(HostIncarnationId, fence epoch, root InvocationId, operation sequence,
DecisionId)`. The ledger rejects zero, stale, or unregistered identities and
duplicates with different artifact tuple digests, operation arguments, resolved
scope, runtime offer, host limits, security context, capability, operation, or
session binding. A new host receives a new incarnation and must start a new
root invocation after recovery; it cannot claim another host's operation by
reusing an invocation or session binding.

### Admitted plan input and decode boundary

Stage 1 introduces a private, non-forgeable admitted-plan input containing the
immutable artifact tuple, the decoded plan variant, and the host admission
context. Only the new verifier constructs it. A bounded byte reader owns the
first framing and format dispatch; the selected plan decoder runs once for the
admitted invocation, including the version-five envelope and its inner plan.
The private evaluator receives that decoded input and never re-reads the same
payload.

An independently resolved child function has its own target, tuple, bounded
decode, and admission. That is not a second decode of the parent's plan. The
existing public `evaluate_client_function*` façade remains compatibility/test
behaviour with its current decode path until production dispatch is migrated;
it is not a source of the new VM admission value or its host-effect guarantees.


### Signed identity-bound attestation extension

After structural admission is implemented, a future trust policy may require an
artifact attestation with a canonical shape like:

```text
Attestation {
    envelope_format/version,
    subject: the artifact identity tuple,
    signer_key_id,
    algorithm,
    signature,
}
```

The signed subject contains the artifact identity tuple: `FunctionId`,
`FunctionRevisionId`, `RevisionPair`, `kind`, `format`, outer and effective
inner plan versions, language, digest, declared capabilities/contracts, and
artifact-declared limits. It does not contain a host-specific ceiling, grant
policy epoch, runtime offer, deadline, or cancellation epoch. Signing only the
payload, display name, or digest is insufficient because it would permit the
signed bytes to be rebound to another function, revision, capability set,
contract set, or artifact limit.

The attestation is identity-bound to the immutable artifact and its target
revision. It does not replace `AuthenticatedSession`, `AuthorisedInvocation`,
local `SO_PEERCRED`, the current grant decision, or host policy. It cannot
choose a principal, role, grant, effective principal, revision, or host limit.

The signature, signer identity, and policy label are metadata about the artifact
trust envelope, not a database-supplied host token. Verification is a separate
fail-closed policy step after the artifact tuple is reconstructed and before VM
admission. No cryptographic verification is claimed by this ADR or by creating
this file.

The following are an explicit next implementation gate for this extension and
must not be guessed or hidden in the structural slice:

- the allowed signature algorithm(s), canonical signed-byte encoding, and
  envelope format/version;
- the trust roots and keyring ownership, including how a signer key is selected
  for an OrnaDB/catalogue/artifact authority;
- key creation, rotation, overlap, and rollback rules;
- revocation representation, propagation, cache invalidation, and behaviour
  when the keyring is unavailable;
- expiry, replay, and offline-cache policy;
- stable denial/audit codes and redaction for invalid signatures or key state;
- migration and compatibility rules for artifacts signed under a later
  envelope version.

Until that gate is accepted, payload digest and structural identity checks are
the only proposed artifact trust checks. A local session attestation without a
signature is deliberately not a substitute for this extension.

### Host-owned in-process capability broker

The `orna-client::vm` host owns a `ClientCapabilityBroker`. The evaluator asks
the host for a checked operation; it does not open files, connect sockets,
resolve secrets, spawn processes, load arbitrary libraries, or turn capability
names into ambient authority. The broker receives trusted invocation context
from the host, not an authority-bearing request field.

A successful broker admission returns an opaque `CapabilityLease`. A lease is:

- bound to the issuing `InvocationId`, its root invocation and lineage, the
  admitted `FunctionId`/`FunctionRevisionId`/`RevisionPair`, canonical artifact
  tuple digest, and the relevant security-context digest and grant-policy epoch;
- bound to one closed capability name, resolved scope, operation kind, runtime
  offer identity, and host limit/cancellation epoch;
- owned by the host and invalidated on cancellation, deadline, invocation
  completion, host shutdown, session loss, revision invalidation, or explicit
  release; and
- non-serialisable and non-authoritative outside the broker. It has no canonical
  value codec, `RuntimeValue` representation, wire-frame form, durable row,
  cache entry, string bearer-token form, or runtime-library export.

The lease may be moved between host calls as an implementation detail, but code
must not be able to clone it into a durable or transport value. A callee cannot
manufacture one from a capability name, path, host, secret ID, request ID,
`parent_invocation_id`, `call_site_id`, or an opaque byte string. A lease from
one invocation, root, function revision, security context, or operation kind is
rejected for another.

Mediation is descriptor/handle-relative. Stage 2 cannot use an implementation
that only checks a lexical path and then opens it through an ambient working
directory. A filesystem backend must resolve each path component beneath a
broker-owned root using an equivalent no-follow, race-safe mechanism, reject
scope escape and host changes, and retain the verified handle for later
operations. It must define the behaviour for symlink, rename, mount, and
permission races before that backend is accepted.

A path supplied by CLIENT code is an operation argument subject to the
already-admitted path scope; it is not an authority root. Network operations
remain tied to a broker-owned connection or handle and an admitted host scope;
DNS or endpoint changes cannot silently retarget an existing lease. Secret use
remains a broker policy operation; secret IDs and secret bytes never become
audit or lease authority. No backend may fall back to ambient environment,
working-directory state, or unrestricted global handles.
Scope attenuation is structural. A path scope is the ordered, lexically
normalised absolute component list already defined by the local capability
model; a child scope is narrower only when the parent components are an exact
prefix of the child components. Host and secret scopes require exact equality,
and operation kinds may not widen. The lease stores the normalised scope and a
scope digest; changing the operation argument, scope, or kind requires fresh
admission. Lexical normalisation never resolves symlinks. The Stage 2 backend
must apply the descriptor/handle race rules above before it treats a path scope
as an OS authority.

The next capability implementation is limited to the closed vocabulary already
accepted by work ADR 0060: `std.fs.read(path-scope)`,
`std.fs.write(path-scope)`, `std.net.connect(host-scope)`, and
`std.secret.use(secret-id)`. `std.process.spawn`, clipboard, listen, and other
canonical possibilities remain outside this ADR unless a later work decision
accepts them. A database cannot add a capability by changing a request frame or
runtime offer.

### Deny-before-side-effect ordering

For every root or nested CLIENT operation, the host follows this ordering:

1. authenticate and bind the `AuthenticatedSession` and
   `AuthorisedInvocation` supplied by the trusted local/server path;
2. perform bounded framing and plan decoding without constructing an evaluator,
   acquiring a descriptor, issuing a lease, or performing a host operation;
3. resolve the target and exact root-pinned `RevisionPair`, reconstruct the
   artifact tuple, and verify digest, format/version, language, limits, decoded
   references, function shape, capability-operation closure, and runtime
   contracts;
4. validate the current grant-policy epoch/digest, runtime offer policy,
   invocation lineage, cancellation/deadline state, operation kind, and any
   descriptor/handle provenance;
5. For a production host operation, send one typed, redacted capability
   admission or operation decision to the kernel-owned protected audit path and
   wait for its commit acknowledgement. Capability decisions always require
   this audit; missing or failed persistence denies the operation. Stage 1
   deterministic adapters record the same ordering locally and perform no
   production host effect;
6. In production, only after the acknowledgement succeeds, issue or use a
   lease and cross the host effect linearization point. Stage 1 completes
   without a lease or host effect; and
7. publish a typed result/event or terminal outcome only after the operation's
   own checks and cancellation ordering succeed.

No rejected artifact or operation may open a descriptor, connect a socket,
resolve a secret, create a process, load an arbitrary runtime library, reserve
a server resource, write USER state, or publish a result. Read operations are
also denied before descriptor acquisition; "read-only" does not mean
"unchecked." This is deny-before-side-effect, not merely deny-before-return.
Post-admission resource or runtime failures retain the existing evaluator's
typed lifecycle transitions; this rule does not claim that every later
evaluation error leaves resource state unchanged.

### Nested calls and lineage

Nested CLIENT calls remain ordinary local typed calls under one rooted
invocation graph and trace. Each child is evaluated against the root's
immutable active revision and security snapshot or policy epoch. A child that
is resolved after activation, revocation, or session change is stale and is
rejected or causes the root to restart according to the later lifecycle
contract.

Before child admission or lease issuance, the trusted path must:

1. resolve the child target and exact function revision in the root-pinned
   catalogue;
2. run the kernel-owned `EXECUTE` and security-policy decision for that child
   under the inherited authenticated session;
3. derive the child capability context as an attenuation of the parent's
   admitted requirements, grant scopes, operation kinds, and host ceilings; and
4. allocate a child invocation identity from the host-owned root allocator.

A child cannot consult a wider global grant set to obtain a capability absent
from its parent context. It cannot widen a scope, operation kind, limit, or
runtime contract, and it cannot mint a lease. Parent and child invocation
identities are host-owned lifecycle identities; `parent_invocation_id` and
`call_site_id` remain lineage/correlation fields and are never authority
tokens.

A parent lease is never passed through a `RuntimeValue`, function argument,
resource request, action value, or runtime callback. A child operation receives
a fresh lease only after the child EXECUTE decision, tuple admission, grant
attenuation, and host audit decision succeed. Local recursive-call identities
are distinct from nested `ORNA-RESOURCE/1` request identities, but both remain
under the root's pinned revision and security epoch. Root cancellation and
session loss revoke all child leases.

Recursive CLIENT calls continue to use the current evaluator's depth and fuel
limits. The VM boundary must not turn recursion or nested resource work into an
unbounded host-operation queue.

### Cancellation, deadlines, and teardown

Cancellation is invocation-scoped and idempotent. The host owns a monotonic
cancellation epoch for each root. Cancelling a root prevents new child
admissions, advances the epoch, and begins revocation of all leases; the
change propagates to nested calls, CLIENT resources, runtime views where
appropriate, and pending broker operations. The current evaluator remains
cooperative for pure computation because its existing public API has no
cancellation argument. The new VM host path checks cancellation at evaluator
checkpoints and before each broker operation; it does not claim pre-emption of
an old façade call.

Each lease has host-owned atomic states:

```text
Active         -> InUse -> EffectInFlight -> EffectStarted -> Committed -> Released
Active         -> Revoking -> Revoked -> Released
InUse          -> Revoking -> Revoked -> Released
EffectInFlight -> Revoking -> Revoked -> Released
EffectStarted  -> Revoked -> Released
EffectStarted  -> Committed -> Released
EffectStarted  -> EffectUnknown -> Released
```

`begin_effect` may move `Active` to `InUse` only when the invocation, policy
epoch, operation, scope, runtime offer, and cancellation epoch still match.
The kernel-owned audit transaction must already have reserved the operation
identity and lease intent; the host does not reserve them in a second store. It
must receive the durable commit acknowledgement before entering the effect
sequence. The single-writer broker commits the durable `effect_in_flight`
marker; `EffectInFlight` is exposed in memory only after that commit.
The broker then runs one ledger-controlled start
transaction that rechecks the registered host incarnation, fence epoch, tuple
fingerprint, policy epoch, and lease state. It atomically consumes a one-shot
start grant and appends `effect_started`, with a durable acknowledgement. Only
the broker may start the backend call after that transaction commits.

The Stage 2 VM never performs a filesystem, network, or secret syscall itself.
It submits the operation to the same trusted, single-writer host-effect broker.
The broker owns the descriptor/connection/secret handle and is the sole writer
through the kernel-owned durable operation ledger; it does not maintain a
second operation store. A registration fence invalidates every unconsumed start
grant before recovery examines its intents. A fenced host's start request or
grant-consumption attempt is rejected before the backend call. Registration
fencing, audit reservation, effect markers, grant consumption, and recovery use
the same ledger owner, so a recovery worker cannot terminalise a started
operation while an old host still has a valid start grant.

The effect fence is the committed `effect_started` marker and the broker's
single-writer start decision. A cancellation or policy revocation linearised
before that fence wins, moves the lease through `Revoking` to `Revoked`, and
prevents the backend call. A cancellation or policy revocation linearised after
the fence cannot revoke the in-flight operation; it records a pending
revocation. The broker retains ownership across the backend call and outcome
transition, so no competing host request can report a revoked lease while the
call is in flight.
Any cancellation or policy revocation observed after `effect_started` is stored
as an idempotent pending-revocation marker in the operation ledger. It cannot
change the terminal classification or authorise a retry. The single-writer
broker clears the marker only when it appends the terminal record.

The backend call is not assumed to be atomically coupled to in-memory state.
Each Stage 2 backend must provide a reservation/commit protocol or an
operation-idempotency and outcome-classification protocol. After the call, the
host moves the lease to `Revoked` only when the backend proves that no effect
occurred, to `Committed` when the effect occurred, or to `EffectUnknown` when
a crash or backend error leaves the effect uncertain. `EffectUnknown` is
terminal and cannot be retried automatically. A process restart recovers a
durable `effect_in_flight` intent without an `effect_started` marker as
no-effect `Revoked`; it recovers an `effect_started` marker as terminal
`EffectUnknown`. A backend reconciliation, if available, appends a separate
non-authorising resolution record and never mutates that terminal state. If a
backend cannot provide this ledger, recovery, and no-retry rule, the operation
is unavailable and fails closed before entering `EffectInFlight`.

`release` is an idempotent host operation. In Stage 2, release of an `Active`
lease first appends its one no-effect terminal (`aborted_before_lease` or
`aborted_before_effect`) and moves the lease through `Revoking` to `Revoked`,
then to `Released`, in the same ledger-owned operation. Stage 1 may release an
ephemeral in-memory lease directly because it has no audit record or host
effect.

`InUse`, `Revoking`, `EffectInFlight`, and `EffectStarted` must first resolve
through their state machine and durable operation ledger. A `Released` lease
rejects all use, cannot be reacquired, and is never reused for another
invocation. A late completion, callback, or lease use observes `Revoking`,
`Revoked`, `EffectUnknown`, or `Released` and cannot perform an effect,
recreate a lease, or mutate terminal state, resource cache, USER state, or
audit projection.

Invocation completion, runtime shutdown, connection close, session loss,
deadline, and host failure revoke or release each lease exactly once and cancel
pending work. Cleanup failure is a closed host failure, not permission to reuse
a stale lease.

### Audit and redaction

The broker is not a second security authority. The kernel owns the protected
audit record and the Stage 2 audit exchange described below. Every capability
admission and operation decision is audit-required. Stage 1 uses only an
in-memory recorder and has no production audit call or host effect.

Audit identity may include target and revision identities, artifact tuple digest,
root/current invocation identities, lineage correlation, closed capability
name, operation kind, policy epoch, allow/deny decision, and terminal outcome.
It must not include raw source or artifact bytes, payloads or signatures,
function arguments, result bytes, USER state values, credentials, secret values,
grant contents, path/host/secret scope values, opaque lease contents, arbitrary
environment text, or unbounded provider errors. A future attestation gate may
record a non-secret signer key identity and stable verification outcome, but it
must not turn signatures or keyring material into runtime-visible values.

Broker logs and runtime diagnostics apply the same redaction and do not become
an alternate audit stream that can grant authority.

### Protected audit contract prerequisite

The current capability audit API and stored schema do not carry the complete
VM decision or terminal record described above. Stage 1 therefore performs no
production host effect and uses only the deterministic in-memory recorder.
Before Stage 2 can issue a production lease, a separate additive kernel-to-VM
audit contract must define the bounded request and acknowledgement, transaction
and commit ownership, decision identity, idempotency, pre-effect versus
post-effect record cardinality, schema migration, disconnect handling, and
stable error mapping. That contract may extend the protected audit schema; this
ADR does not silently change it.

The Stage 2 audit transaction owns one operation reservation. It appends the
pre-effect decision, reserves the operation identity and lease intent, and
returns an acknowledgement only after that transaction commits. The host does
not reserve the same operation identity in a second store. The exchange uses a
non-zero host-owned `DecisionId` and an operation identity unique across
restarts:

```text
CapabilityAuditExchange {
    decision_id,
    host_incarnation_id,
    fence_epoch,
    operation_id,
    operation_sequence,
    artifact_tuple_digest,
    operation_arguments_digest,
    resolved_scope_digest,
    runtime_offer_identity,
    host_limit_ceiling,
    cancellation_epoch,
    phase: pre_effect | effect_in_flight | effect_started | terminal_pending | post_effect,
    authenticated_session_binding,
    security_context_digest,
    target_and_revision,
    root_and_current_invocation,
    lineage,
    capability,
    operation_kind,
    policy_epoch,
    decision_or_terminal_outcome,
}
```

The host allocator issues `DecisionId` and `operation_sequence` under the root
lock. The kernel ledger keys idempotency by the complete operation identity
`(HostIncarnationId, fence epoch, root InvocationId, operation sequence,
DecisionId)` and stores the complete fingerprint alongside it. The fingerprint
includes the current invocation, artifact tuple digest, operation-arguments
digest, resolved-scope digest, runtime-offer identity, host-limit ceiling,
cancellation epoch, security context, session binding, capability, operation
kind, and policy epoch.
`operation_sequence` and `DecisionId` are non-zero fixed-width `u64` values
starting at one for each root. The host allocator increments both with checked
arithmetic while holding the root lock; overflow, zero, or reuse is a closed
failure. Stage 2 persists the allocation in the same kernel-owned ledger
transaction as the operation reservation, so restart recovery cannot allocate
the same pair. Stage 1 uses the equivalent in-memory rule for tests.

Only a request with the same complete identity and identical fingerprint
returns the recorded state. A reused identity with any changed field is
rejected. A different operation identity is a new operation even when its
fingerprint matches another operation. Host-incarnation fencing runs before
idempotency lookup, so a stale host cannot replay an old request as a match.

The kernel appends exactly one immutable `pre_effect` decision for each
admission. A denied decision has no lease and no terminal effect record. An
allowed decision requires exactly one linked `post_effect` terminal record,
even when lease issuance fails, no effect starts, or recovery classifies the
result as `EffectUnknown`. If an operation reaches `EffectInFlight`, the
single-writer broker appends one durable `effect_in_flight` intent and waits for
its commit acknowledgement. If it reaches the effect fence, the same broker
transaction validates the live host fence, appends one `effect_started` marker,
and waits for its commit acknowledgement before the backend call; both records
are followed by the same terminal record.

A crash before lease issuance produces `aborted_before_lease`. A crash after
`effect_in_flight` but before `effect_started` produces no-effect `Revoked`.
A crash after `effect_started` produces terminal `EffectUnknown`; recovery does
not mutate that terminal state after the fact. A backend reconciliation, if
available, appends a separate non-authorising resolution record linked to the
same operation; it never rewrites or reuses the terminal record. Recovery is
kernel-owned and terminal-only; it appends the missing terminal record before
any retry, rejects stale host-incarnation replay, and never starts a new
effect. Repeated pre-effect, in-flight, or post-effect requests return the
recorded state without issuing another lease or replaying an operation. The
acknowledgement means that the owning transaction committed the corresponding
record. No audit record grants authority by itself.
If a terminal append fails, the single ledger records `terminal_pending` with
the already classified outcome and retains the complete operation identity.
The recovery worker retries only the terminal append by that identity. It never
reissues the backend call, allocates a new lease, or treats a pending terminal
as permission to retry. A successful retry changes the ledger to
`post_effect`; repeated retries return the same terminal record.

### Cache and replay

The immutable artifact cache is content-addressed by the canonical digest as
required by the wire contract, but a cache hit is only a candidate artifact.
Every use revalidates the complete artifact identity tuple against the current
root-pinned `RevisionPair`, target revision, format/version, language,
capabilities, contracts, and artifact-declared limits. It also rechecks the
current `HostAdmissionContext`, including the local grant-policy epoch/digest,
runtime-offer policy, host ceilings, and cancellation/deadline epoch. A digest
collision or a matching payload attached to a different function or revision
is not an admission.

Grant-policy or runtime-offer changes invalidate matching admitted-artifact
and capability-derived result entries, or force per-invocation re-admission
before use. No cache entry stores the grant set, policy decision, lease,
session authority, host ceiling, attestation outcome, open descriptor/handle,
cancellation state, runtime session, or host-operation result. If a later
attestation gate is enabled, key revocation or rotation must invalidate or
recheck any attestation cache before admission.

Decoded plans may be reused only as immutable data after these checks.
Existing resource/result cache identity remains governed by work ADRs 0071 and
0078: target `FunctionId` plus pinned revision, principal/security context,
canonical typed argument digest, and catalogue/data invalidation evidence. A
resource result or terminal outcome does not replay a broker side effect, and
failed, cancelled, stale, or late work cannot populate a cache. No automatic
retry is introduced here; an explicit new invocation obtains fresh admission
and fresh leases.

### Runtime separation

The VM host and capability broker stay in `orna-client`; `orna-runtime-*`
implementations remain installed, locally selected runtime libraries. A runtime
receives validated typed values and external runtime-contract operations such as
`std.ui.window@1`. It does not receive artifact authority, an authenticated
session, `PrincipalId`, `LocalCapabilityGrantSet`, raw descriptors/handles,
leases, database protocol access, SQL access, or USER-state authority. A
runtime callback returns an owned event snapshot to the host; the host decides
whether it is a local state transition, resource/action request, or diagnostic.

Host-operation dataflow is explicit. Pure expression, state, and control-flow
evaluation performs no host operation. CLIENT resources and actions continue to
use their existing `ClientResourceExecutor` and `ORNA-RESOURCE/1` boundaries;
when their implementation needs a local capability, the VM host performs a
broker admission first and passes only the resulting operation-scoped lease to
the host implementation. The broker owns no resource transport, stream credit,
or result protocol, and the resource executor cannot bypass broker policy.
The lease-bearing path is a private `ClientVmHost` adapter. It is the only
adapter allowed to receive a `CapabilityLease`; the public
`ClientResourceExecutor::execute` compatibility method remains unchanged and
is not a production local-capability implementation. For a future
capability-backed resource or action, the VM performs operation closure,
policy/audit admission, and lease issuance before reserving capability-backed
resource state. Ordinary SERVER-resource requests retain the existing resource
generation, transport, and cancellation semantics. A compatibility executor
cannot bypass the private broker adapter.
The private adapter exposes only a lease-bearing operation method whose input
contains the opaque lease, complete operation identity, normalized scope
digest, bounded operation arguments, and cancellation epoch. It returns a
bounded `EffectOutcome` and never returns the lease, raw handle, secret, or
provider error to CLIENT code. It performs the ledger fence and backend call
through the single-writer broker. This method is private to the VM host and is
not added to `ClientResourceExecutor`.

Inspector and external runtime-contract calls use typed requests and provider
offers. They do not receive leases or local authority. If a future provider
operation needs a filesystem, network, or secret capability, it enters the
same broker path and carries the resource generation or contract request
identity only as correlation. Lease lifetime never extends a resource request,
stream credit window, runtime session, or action value.

The database cannot choose an arbitrary native library or make a runtime a
security bypass. Runtime offer/ABI/contract checks remain separate from CLIENT
artifact admission, as required by work ADRs 0082 and 0084. Runtime failure
cancels or detaches the local invocation and must not corrupt durable server
state. The existing runtime ABI and `ORNA-RESOURCE/1` bytes are unchanged by
the next structural slice.

### Runtime offer witness

The selected runtime offer is represented by a private immutable host snapshot,
not by a mutable `RuntimeDescriptor` or an untrusted request. The snapshot
includes ABI major/minor, runtime name/version/build identity, platform, thread
model, feature flags, and the complete sink and contract offers. The v1
canonical field order is:

```text
abi_major:u32, abi_minor:u32,
runtime_name:text, runtime_version:text, build_id:text, platform:text,
thread_model:i32, features:u64,
sinks:count<sink>,
contracts:count<contract>,
```

Each sink encodes `type_name:text`, `media_types:count<text>`,
`supports_streaming:u8`, and `preference_rank:i32`. Each contract encodes
`name:text`, `major:u32`, `minor:u32`, and `features:count<text>`. Identity
text is non-empty, contains no NUL, and uses the exact UTF-8 byte sequence with
a four-byte big-endian length and no normalisation. Media-type and feature
text is also NUL-free; empty entries and duplicate entries are rejected.
`u32` and `u64` use fixed-width big-endian encoding, `i32` uses its
two's-complement four-byte big-endian representation, and `u8` uses one byte.
Every count is a four-byte big-endian `u32` with maxima `sinks <= 1`,
`contracts <= 8`, `media_types <= 16`, and `features <= 16`; each text value
is at most 4096 bytes and the complete canonical witness is at most 16 MiB.
Media-type lists and feature lists are sorted lexicographically. Sink offers
are sorted by type name, media types, streaming flag, and rank; contracts are
sorted by name, major, minor, and features. `supports_streaming` must be `0` or
`1`, and feature bits must be within the accepted mask for the ABI/runtime.
Exact duplicate sinks or contracts, unknown fields, and unsupported ABI or
thread values reject the snapshot.
For the current Qt v1 provider, `thread_model` is the fixed
`ORNA_THREAD_MODEL_CALLER_PUMPS` value `3`, and `features` must equal the
accepted `RUNTIME_FEATURE_MULTIPLE_WINDOWS` bit `1 << 0`. A future runtime or
ABI version must declare its own accepted thread values and feature mask before
its witness can be used.

The host computes SHA-256 over the ASCII
`orna.runtime-offer.witness/1` domain separator followed by these canonical
bytes. Descriptor limits are not present in the current ABI snapshot; host
limit ceilings remain in `HostAdmissionContext`. A future descriptor limit
field requires a versioned witness update. The digest is a host policy witness,
not a wire identity. Runtime replacement or shutdown creates a new witness,
advances the mutable host policy epoch, and invalidates affected admissions and
leases.

### Explicit process-isolation limitation

The selected broker is an **in-process** authority and mediation boundary. It
is appropriate for the current closed, memory-safe evaluator and for host code
that obeys the broker API. It is **not process isolation for memory-unsafe
code**. A memory-corruption bug, unsafe native extension, JIT, arbitrary
untrusted runtime, or malicious FFI code in the same process could read or
alter broker state, process memory, descriptors, or secrets; opaque leases do
not defend against that class of failure.

The installed Qt runtime is an explicitly trusted native provider under work
ADR 0082 and is outside this untrusted-code trigger. This ADR makes no claim
that Qt is an OS sandbox; the existing ABI/loader trust and package boundary
remain authoritative. Introducing memory-unsafe or native code from a remote
artifact, untrusted plugin, JIT, FFI, or code with access to VM authority
requires a separate process-isolation and platform-policy decision with IPC
framing, lifecycle, handle transfer, cancellation, audit, crash containment,
and deployment proof. The in-process broker cannot be relabelled as that
isolation.

## Staged acceptance boundary

This ADR is Proposed. It records a staged implementation target; no stage is
claimed to be implemented merely because the record exists.

### Stage 1: next implementation slice — additive admission and broker seam

The next implementation slice accepts:

- an additive `orna-client::vm` boundary that preserves every existing
  `evaluate_client_function*` API and evaluator semantic;
- a typed, immutable structural admission value containing the complete
  artifact tuple, with bounded parsing before evaluator construction and
  semantic preflight before any host side effect;
- cache identity/revalidation rules that keep content-addressed artifact reuse
  separate from authorisation, capability grants, leases, and results;
- an invocation context and host-owned broker state machine with immutable root
  revision/security identity, current grant-policy epoch, child
  `EXECUTE`/attenuation checks, ephemeral in-memory lease states, cancellation,
  and deny-before-side-effect ordering tests; Stage 1 exposes no production
  lease, kernel audit call, or host effect; and
- focused conformance tests using deterministic host adapters so the broker
  control plane can be proved without pretending to have cryptographic or OS
  isolation.

Stage 1 does not require a signature verifier, keyring, production host token,
subprocess, or new wire/runtime ABI. It must not advertise a lease as a
serialisable capability or make the existing local session binding reusable as
a host credential.

### Stage 2: concrete host-operation mediation

After Stage 1's preflight and lease-state proofs, and after the separate audit
contract is accepted, the follow-on slice may implement the closed vocabulary
backends (`std.fs.read`, `std.fs.write`, `std.net.connect`, and `std.secret.use`)
through descriptor/handle-relative host operations. Each backend needs its own
side-effect, scope, cancellation, cleanup, redaction, and audit proofs.
No unsupported capability is silently mapped to an ambient operation.

### Stage 3: signed artifact trust gate

A later gate may add the identity-bound attestation extension. It must first
settle algorithm, canonical envelope bytes, keyring/trust roots, rotation,
revocation, cache invalidation, replay/expiry, migration, and audit/error
semantics. Until that gate is accepted and implemented, the repository must not
claim cryptographic verification, trusted artifact provenance, or a production
host-token scheme.

### Stage 4: process-isolation gate

If the VM or plugin surface admits untrusted memory-unsafe code, JIT, FFI,
native extensions, or a remote native runtime, a separate ADR must choose and
prove OS/process isolation. The installed trusted Qt provider is governed by
work ADR 0082 and is not an untrusted plugin boundary. The in-process broker
cannot be relabelled as process isolation.

## Alternatives rejected

### Unsigned session attestation

Rejected as the artifact trust boundary. An authenticated session proves the
local/server authority represented by `AuthenticatedSession` and
`AuthorisedInvocation`; it does not prove that artifact bytes were published by
a trusted artifact authority or that those bytes remain bound to the exact
function, function revision, `RevisionPair`, capabilities, contracts, and
limits. An unsigned assertion attached to a session would also be replayable or
forwardable as data and would blur work ADR 0090's explicit boundary around
local session authority, host tokens, and provenance. The structural tuple is
still required, and the future extension must be signed and identity-bound.

### Subprocess or OS isolation for the first slice

Rejected for the next implementation slice, not as a claim that it is
unnecessary forever. A process boundary would require a separate cross-platform
IPC/value protocol, artifact transfer rules, descriptor/handle transfer,
crash/cleanup lifecycle, cancellation race semantics, audit propagation,
backpressure, runtime integration, and deployment policy. The canonical
`CLIENT` evaluator currently has a closed in-process semantic core and no
accepted untrusted evaluator or plugin native-code boundary.
Choosing a subprocess now would couple unresolved OS policy to the structural
artifact contract and make the first proof much larger. It remains the required
direction to evaluate before admitting untrusted memory-unsafe code.

### Replace the current evaluator

Rejected. Replacing the evaluator would discard the existing
`evaluate_client_function*` façade, `ClientExecutionContext`, explicit
`ClientStateStore`, `ClientResourceExecutor`, capability grant gate, recursive
fuel/depth checks, resource/action lifecycle, and focused proof seams. It would
mix trust-boundary migration with a language/runtime rewrite and risk changing
accepted CLIENT semantics. An additive `orna-client::vm` boundary can preflight
and host the current evaluator while preserving those contracts.

## Implementation sequence

The implementation should proceed in this order; these are future changes, not
claims about the current work tree:

1. Add the private/publicly documented `orna-client::vm` boundary and host
   context without removing or changing existing evaluator entry points. The
   host context owns a non-zero root identity, collision-free child allocator,
   immutable root revision/security witness, mutable policy epoch, host limits,
   and cancellation state.
2. Define canonical construction and comparison for the structural admission
   tuple. Reuse `ExecutableArtifact`, `FunctionRevisionRecord`, `RevisionPair`,
   `artifact_payload_digest`, existing CLIENT format/language checks, the v5
   outer/inner versions, and existing capability/contract validation rather
   than creating parallel identity or value codecs.
3. Implement bounded framing and parsing before evaluator construction, then
   create one immutable admitted-plan input for semantic validation and
   execution. The private evaluator seam consumes that input without a second
   decode; old façade calls remain separate compatibility behaviour.
   Reject every tuple mismatch, malformed/oversized payload, unsupported format
   or language, duplicate/unknown capability or contract, unsupported limit,
   stale revision, and invalid reference without host effects.
4. Add the content-addressed verified-artifact cache entry and full-tuple
   revalidation. Keep decoded plans immutable; never cache leases, grants,
   session authority, attestation outcomes without revocation policy, or
   side-effect results as artifact authority.
5. Add the host-owned broker control plane and opaque lease lifecycle. Prove
   invocation/root/revision/security-context binding, child authorization and
   attenuation, non-serialisability, cancellation, deadline, shutdown, release,
   and deny-before-side-effect with deterministic adapters. Stage 1 adapters
   have no production host effect.
6. Accept and implement the separate kernel-to-VM audit request/acknowledgement
   contract before production capability effects. Add a private VM-host adapter
   that passes an operation-scoped lease to host implementations while
   preserving the public `ClientResourceExecutor` compatibility façade and
   `ORNA-RESOURCE/1` transport semantics.
7. Add concrete closed-vocabulary operation adapters only after the control
   plane and audit contract are proven. Use descriptor/handle-relative
   mediation, bounded operations, protected redacted audit, and explicit
   terminal/cancellation ordering.
8. Integrate normal `sys.invoke` CLIENT dispatch with the new preflight/host
   boundary while retaining the old evaluator façade for compatibility and
   focused tests. Verify that nested resource/action calls inherit trusted
   lineage but never receive a serialisable lease or caller-selected authority.
9. Open the separate signed-attestation gate. Specify and test canonical
   envelope bytes, algorithm/keyring/trust-root policy, rotation, revocation,
   cache invalidation, expiry/replay, migration, and redacted audit/error
   outcomes before accepting cryptographic verification.
10. Open a separate process-isolation ADR before any untrusted memory-unsafe VM,
    JIT/native extension, or untrusted native runtime is admitted.

## Acceptance tests

The following tests are required before the corresponding future slice can be
marked accepted. They are test requirements, not reports that these tests
already pass.

### Structural admission and API compatibility

1. A valid `ExecutableArtifactKind::Client` with the exact target
   `FunctionId`, `FunctionRevisionId`, `RevisionPair`, format, outer and
   effective inner versions, language, digest, closed capabilities/contracts,
   and artifact-declared limits produces one immutable verified admission value
   when the current host policy also permits it.
2. Each individual tuple or host-context mismatch—wrong function, function
   revision, root-pinned `RevisionPair`, kind, format, outer/inner version,
   language, digest, capability or contract set, artifact limit, grant-policy
   epoch, runtime offer, or host ceiling—fails closed after bounded parsing but
   before evaluator or host effects, leaving resource, cache, and host state
   unchanged.
3. Truncated, trailing, malformed, oversized, unknown-version, unsupported
   instruction, duplicate-capability, duplicate-contract, and invalid-reference
   inputs fail before attacker-controlled counts can cause unbounded allocation.
4. Existing `evaluate_client_function*` façade calls retain their signatures and
   accepted evaluator behaviour. The additive host path reaches the same
   evaluator semantics for capability-free legacy Boolean and Opaque plans,
   and for versioned plans whose requirements are carried by the admission
   manifest, including expression, state, procedural/control-flow, resource,
   action, and Inspector plans rather than routing to a replacement evaluator.
5. An instrumented evaluator proves that no database, protocol, filesystem,
   process, environment, network, or runtime-library operation occurs inside
   the evaluator; all host effects go through the broker or existing executor
   seam.

Stage 1 also tests one bounded decode of the selected outer and inner plan,
payload non-reread, immutable retention, and independent child admission and
decode. It must distinguish the V5 envelope decode from the independently
admitted child decode.

Stage 1 runtime-witness tests cover the golden SHA-256 bytes, reordered
collection invariance, duplicate/unknown/empty/NUL/unsupported-field rejection,
`supports_streaming` values outside `0`/`1`, feature-bit policy, count and
aggregate byte limits, and mutation after snapshot creation.

Stage 1 also tests concurrent root reservation and release, root collision,
root zero/exhaustion, concurrent child allocation, child overflow, zero
rejection, release recording, and no identity reuse. These tests exercise the
in-memory allocator and do not claim a production invocation service.

### Broker, leases, and side effects

Tests 6-11 are Stage 2 broker tests. Stage 1 may exercise the same state
machine with an in-memory recorder, but it must not issue a production lease,
contact the kernel audit path, or perform a host effect.
The Stage 1 negative proof injects production-effect, kernel-audit, and
lease-export hooks and asserts that none is reachable from the ephemeral
admission/state-machine path.

Stage 2 adapter tests prove that the private lease-bearing `ClientVmHost`
path is the only path that can reach a capability-backed host operation. The
public compatibility executor receives no lease and cannot reserve
capability-backed state before broker admission; ordinary SERVER-resource
requests remain on their existing path.

Stage 2 ledger tests cover the complete identity matrix: an identical operation
identity and fingerprint returns the recorded state; reuse with any changed
fingerprint is rejected; and a distinct operation identity with the same
fingerprint starts a new operation. They also cover concurrent
`DecisionId`/`operation_sequence` allocation, overflow, restart non-reuse,
host-incarnation fencing, and stale-host rejection.

6. A permitted capability operation receives an opaque, invocation-scoped lease
   with the declared atomic state transitions; attempts to encode it as a
   `RuntimeValue`, canonical value, wire frame, durable/cache entry, function
   argument, log field, or runtime callback fail at the type/API boundary.
7. A lease from another invocation, root lineage, function, function revision,
   `RevisionPair`, security context, capability, scope, operation kind, or
   cancellation epoch is rejected without host state mutation.
8. Descriptor/handle-relative tests reject path traversal/scope escape, foreign
   or retired handles, symlink/rename/mount/permission races, host changes,
   invalid secret IDs, unsupported operations, and operation-limit violations
   before opening, writing, connecting, or resolving anything.
   `std.process.spawn` remains rejected in the closed Stage 2 vocabulary.
9. For a Stage 2 production operation, a deny-before-side-effect test records
   admission, mandatory protected audit acknowledgement, lease reservation,
   durable `effect_in_flight` intent, live fence validation, `effect_started`
   marker, the broker backend call, and the terminal outcome. It proves no
   effect when cancellation or denial wins before `effect_started`, and reports
   `Revoked` for a backend-proven no-effect result, `Committed` for a proven
   effect, or `EffectUnknown` after a crash or uncertain backend result.
10. Nested calls receive fresh child admission and, where permitted, fresh
    child leases only after child `EXECUTE`/policy authorization and grant
    attenuation. A parent lease cannot be passed, cloned, serialised, or used
    to widen the child grant; parent/call-site IDs remain correlation only.
11. Root and child cancellation is idempotent, revokes leases through the
    declared state machine, prevents new effects, cancels pending
    resource/runtime work, rejects late completions, and preserves the
    committed-terminal-wins race rule. Cancellation and policy revocation
    during `EffectInFlight` wait for outcome classification; concurrent host
    registration fences the old incarnation before recovery, and an old host
    cannot submit a terminal or effect request after retirement. Restart
    recovery distinguishes `effect_in_flight` without `effect_started` as
    no-effect `Revoked`, and `effect_started` as terminal `EffectUnknown`.
    Backend reconciliation is a separate non-authorising record. A forced
    terminal-write failure is recovered idempotently without a duplicate
    terminal or repeated effect. Shutdown, disconnect, and deadline cleanup
    release every lease exactly once.
The Stage 2 audit test asserts that a denied `pre_effect` has no terminal
record, an allowed lease-issuance failure produces one
`aborted_before_lease` terminal, and every allowed operation has exactly one
linked `post_effect` terminal. A forced terminal-write failure leaves the
operation ledger pending; recovery retries the same terminal append by complete
identity and never repeats the backend effect.

### Audit, cache, replay, and runtime separation

12. Protected audit evidence records target/revision/invocation identity,
    capability decision, operation kind, and terminal outcome with stable
    redacted codes, while excluding source, artifact/signature bytes, arguments,
    results, USER state, credentials, grant contents, capability scopes,
    path/host/secret values, lease contents, and arbitrary errors.
13. A content-addressed artifact cache hit re-runs full tuple admission and
    current host-context checks. A stale/future revision, changed
    capability/contract set, changed artifact limit, grant-policy or runtime
    offer change, revoked future attestation, cancelled operation, failed
    result, or late completion cannot be replayed as authority or populate a
    result cache.
14. A runtime receives only validated typed values and contract operations; it
    cannot obtain a lease, principal, grant, raw descriptor/handle, database
    connection, or USER-state authority. A trusted installed Qt runtime remains
    governed by ADR 0082; an untrusted native runtime requires the separate
    isolation gate.
15. The next structural/broker slice leaves `ORNA-RESOURCE/1`, the existing
    runtime ABI, and current evaluator semantics unchanged.

### Future attestation and isolation gates

16. Only after its explicit gate is accepted, a signed-attestation test proves
    that a valid signature binds the complete artifact tuple and that wrong
    tuple, algorithm, signer key, keyring state, rotation state, revocation
    state, expiry, replay, and envelope version fail closed with redacted audit
    codes. Until then, no test or documentation may claim cryptographic
    verification.
17. Before any untrusted memory-unsafe or native code is admitted, a separate
    process/OS isolation test proves IPC framing, handle transfer, cancellation,
    crash containment, audit propagation, and cleanup. The installed trusted Qt
    provider remains governed by ADR 0082. The in-process broker test itself
    must not be reported as process-isolation proof.

## Explicitly blocked or out of scope

This Proposed ADR does not accept or implement:

- cryptographic signature verification, artifact provenance, a keyring, key
  rotation, revocation, expiry, or production host tokens;
- OS/process/container isolation, seccomp policy, JIT/native/FFI execution, or
  a hostile memory-unsafe plugin boundary;
- a replacement evaluator, new CLIENT language semantics, a second value codec,
  or a second source of runtime/UI semantics;
- capability vocabulary expansion beyond the closed work ADR 0060 entries,
  including process spawning, listening, or clipboard access;
- arbitrary runtime-library loading or database-selected native binaries;
- principal/role/effective-principal transitions, delegation, impersonation,
  `EXTERNAL` identity, remote gateway authentication, or credential enrollment;
- a serialisable capability token, durable lease, caller-selected grant, or
  authority-bearing parent/call-site identity; or
- changes to `sys.invoke`, `ORNA-RESOURCE/1`, the runtime ABI, USER-state
  authority, or existing resource/action semantics; Stage 1 also does not
  change the protected audit schema. A separate audit contract must explicitly
  extend that schema before Stage 2 production effects;

In particular, this record does not claim that the current work tree verifies
signatures, authenticates artifact provenance, mediates filesystem/network/
secret side effects, or isolates a memory-unsafe process. Those claims remain
blocked until the staged gates and acceptance tests above are implemented and
accepted.

## Consequences

The first proof is small and reversible: the current evaluator remains the
semantic core, while a single additive host boundary makes artifact identity,
local limits, capability mediation, and lease lifetime explicit. Content-hash
caching remains useful without turning a cache hit into authority. The broker
also gives nested calls, cancellation, audit, and runtime separation one place
to enforce rather than scattering host checks through evaluator nodes.

The cost is an additional admission value and host-owned lifecycle that every
normal CLIENT invocation must respect. Concrete filesystem/network/secret
backends require careful platform-specific descriptor/handle work and cannot
be inferred from the capability declaration alone. Signed provenance requires a
separate key-management decision, and memory-unsafe code requires a separate
process boundary. Those costs are intentional: the current local session
authority is not silently converted into a production artifact credential or
OS sandbox.

## Precedence

Until this ADR is accepted, the canonical specification and accepted work ADRs
remain authoritative; this Proposed record is a design target and cannot claim
an implementation override. If accepted, it narrows the open CLIENT VM and
sandbox proposal only for the staged surface named here.

The canonical sources above remain authoritative for language meaning, artifact
and revision identity, `sys.invoke`, capability concepts, runtime contracts,
protocol framing, and testing requirements. Work ADR 0060 remains authoritative
for the closed local capability vocabulary and declarative grant gate. Work ADRs
0071, 0074, and 0078 remain authoritative for resource identity, executor
ownership, `ORNA-RESOURCE/1`, cancellation, terminal ordering, and resource
cache evidence. Work ADR 0082 remains authoritative for installed runtime
trust, ABI, and local runtime selection. Work ADR 0084 remains authoritative
for the shared evaluator/runtime-host direction. Work ADR 0089 remains
authoritative for lineage versus correlation. Work ADR 0090 remains
authoritative for local `SO_PEERCRED`, authenticated session/revision/grant
authority, and its explicit deferrals.

A future signed-attestation or process-isolation decision must name the exact
portion it supersedes or extends. No future implementation may treat this ADR's
opaque lease model as a credential, cryptographic proof, or OS isolation
without those separate gates.
