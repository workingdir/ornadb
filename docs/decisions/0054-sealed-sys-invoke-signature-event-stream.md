# ADR 0054: `sys.invoke` Has One Sealed Request Stream

**Status:** Accepted

## Decision

The Ring-1 function registered by work ADR 0042 has this single stable logical
signature:

```text
sys.invoke(p_request sys.invoke.Request)
RETURNS STREAM<sys.invoke.Event>
```

It is a sealed system-function signature. It is not an application
`FunctionDefinition`, an application source declaration, a PostgreSQL row, or
a general extension of application `STREAM` return types.

The sole parameter has these fixed facts:

| Fact | Value |
| --- | --- |
| Name | `p_request` |
| `ParameterId` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 03` |
| Type | sealed `sys.invoke.Request` (`TypeId` ending `f1`) |
| Presence | required and non-null |

The sole result has type `STREAM<sys.invoke.Event>`, where Event is the sealed
carrier with the `TypeId` ending `f2`. There is no result column, result alias,
alternate return type, default parameter, overload, or variadic form. A
caller cannot select the signature by name, source search path, parameter name,
or a client-provided descriptor.

The system registry remains the only authority for the function identity,
name, signature, parameter identity, and result stream. The function and its
parameter do not enter an application catalogue or standard-library catalogue.
An implementation must expose immutable signature facts from that registry;
it must not fabricate an application definition to reuse an ordinary
application execution path.

## Selective ORF5 positions

ORF5 opens exactly two carrier positions. It does not open a general carrier,
constructed-value, or `STREAM` frame position.

| Direction | Frame position | Exact admitted value | Rejected values |
| --- | --- | --- | --- |
| Client to server | `CALL_ARGUMENT` for a raw call whose function is `sys.invoke` and whose parameter is `p_request` | one complete `sys.invoke.Request` ORV5 envelope | `Value`, `Event`, every ordinary runtime value, a second argument, and every other parameter identity |
| Server to client | `EVENT_BATCH` on an accepted `sys.invoke` stream in `RESULT_VALUES` | one complete `sys.invoke.Event` ORV5 envelope per event record | `Value`, `Request`, ordinary runtime values, and every non-`RESULT_VALUES` channel |

`CALL_RAW_START` must name the stable `sys.invoke` `FunctionId`. The stream
must contain exactly one `CALL_ARGUMENT` with `p_request`, followed by
`CALL_ARGUMENTS_COMPLETE`. Duplicate, missing, extra, or wrong-identity
arguments are a frame-state failure. A normal raw call remains governed by its
existing frame rules; it cannot carry any invocation carrier.

For the special request position, the frame decoder validates the complete ORF5
envelope and a bounded complete ORV5 Request envelope. It checks marker, tag,
carrier identity, declared lengths, and trailing bytes. It retains the request
envelope as private bounded bytes. It does not materialise Request payload
fields in the frame codec. The protected invocation dispatcher decodes that
envelope against its pinned active revision and matching opaque registry. This
is required so a structurally invalid Request becomes the redacted invocation
outcome below, rather than a catalogue or selector oracle at the frame layer.

The Event position fully decodes each Event carrier before it changes
connection or invocation lifecycle state. An invalid server Event is a
protocol failure. It does not produce a partial Event, consume credit, or
advance an event sequence.

The retained work ADR 0026 `EVENT_BATCH` record envelope remains exact. Every
record on a `sys.invoke` stream uses outer record kind `0x01`, canonical value,
and its content is exactly one complete ORV5 `sys.invoke.Event` envelope. Its
outer record sequence starts at one and is contiguous across the raw stream.
That outer sequence is distinct from the embedded Event sequence, which starts
at zero and is contiguous only for its one `InvocationId`.

ORF1 through ORF4 remain unchanged. In ORF5, ordinary `CALL_ARGUMENT` and
ordinary `EVENT_BATCH` positions remain closed to all three carriers and to
constructed application values. Existing frame sizes, stream counts, retained
argument bounds, byte windows, and stream-number rules remain authoritative.

## Stream lifecycle and cancellation

The raw-call stream and the Event-carrier stream have separate sequences.
The raw frame sequence remains the connection-local sequence defined by work
ADR 0026. Event `sequence` remains a per-invocation sequence. The server must
set it to zero for `InvocationStarted` and increase it by one for each later
Event. The `InvocationId` in every Event must equal the value in the preceding
`CALL_ACCEPTED` frame for that raw stream.

After a complete Request has passed structural decode, the dispatcher creates
one `InvocationId`, sends `CALL_ACCEPTED`, and queues `InvocationStarted` with
sequence zero. It does this before private target resolution. The start event
contains no visible principal in this slice. It is sent only when the client
grants sufficient `RESULT_VALUES` byte credit for its
complete `EVENT_BATCH`. Until it is sent, the dispatcher may complete private
resolution, prebind, security, and audit work, but it must not evaluate
defaults, execute the target, or retain an unbounded result stream.

After this accepted `sys.invoke` call, the allowed Event lifecycle is:

```text
CALL_ACCEPTED(invocation)
    -> InvocationStarted(sequence 0)
    -> (ValueBatch | Diagnostic)*
    -> InvocationCompleted | InvocationFailed | InvocationCancelled
    -> CALL_COMPLETED
```

There is exactly one terminal Event. No Event may occur before
`InvocationStarted`, after a terminal Event, or with a skipped, repeated, or
wrong-invocation sequence. `InvocationCompleted` is the only successful
terminal Event. `InvocationFailed` and `InvocationCancelled` are complete
invocation outcomes, not raw-frame failures.

After `InvocationStarted` is sent, the dispatcher may produce a later Event
only when sufficient `RESULT_VALUES` credit exists for its complete frame. It
must apply backpressure to the target result producer before it accumulates
Events in memory. A failure or cancellation discovered while the start Event
is queued is retained as one pending terminal outcome and is sent only after
the start Event. A peer disconnect before the start Event is sent is an
implicit pre-execution cancellation: the dispatcher completes only the private
resolution, security, and audit work that is already required, records the
required decision evidence, and does not evaluate defaults or execute the
target. A peer disconnect after the start Event is sent does not abort target
work. The dispatcher completes it to a durable terminal state and discards
each undeliverable Event at production time. It does not construct or retain
an unbounded Event list.

For an accepted `sys.invoke` stream, `CALL_FAILED` and `CALL_CANCELLED` are
not permitted. The server sends the terminal Event and then `CALL_COMPLETED`.
The terminal frame remains unwindowed. Event batches consume only the existing
`RESULT_VALUES` byte window. A server must not buffer an unbounded list of
Events while it waits for credit.

A `CALL_CANCEL` in `RECEIVING_ARGUMENTS` retains the existing immediate raw
`CALL_CANCELLED` outcome. A `CALL_CANCEL` in `DISPATCHING` retains the work ADR
0026 cancellation action with no InvocationId; it must finish the protected
pre-accept work and then produce the existing terminal raw outcome without
`CALL_ACCEPTED` or Event. A cancellation after acceptance reaches the
protected invocation operation with its `InvocationId`. The operation stops
starting new target work, applies the existing durable-decision rules,
discards unsent non-terminal Events, emits one `InvocationCancelled` Event
when sufficient `RESULT_VALUES` credit is available, then emits
`CALL_COMPLETED`. An operational failure or an audit failure wins over
cancellation and produces one redacted `InvocationFailed` Event. Connection
closure, peer disappearance, and a zero window use the preceding flow-control
and disconnected-peer rules. The server never retains an unbounded Event list.

## Protected invocation boundary

The dispatcher performs one root invocation with one pinned active database
revision, pinned verified standard snapshot, matching opaque registry, and
security snapshot. It never resolves a target in one snapshot and authorises,
binds, audits, or executes it in another.

The phases are ordered as follows:

1. Require the authenticated active `USER` or `SERVICE` session that work ADR
   0042 permits to enter the sealed system function. A `ROLE` cannot enter. A
   rejected system entry produces `CALL_FAILED(EXECUTE_DENIED)` before Request
   decode or `CALL_ACCEPTED`.
2. Decode the retained Request against the pinned revision and require its
   offered protocol major to equal the authenticated ORF5 connection major. A
   failure emits `CALL_FAILED(INTERNAL_FAILURE)` before `CALL_ACCEPTED`; it
   exposes no target, parameter, type, opaque-contract, value, or version
   fact.
3. Create the `InvocationId`, send `CALL_ACCEPTED`, and queue the empty-safe
   `InvocationStarted` Event for available credit. No target or binding fact
   has crossed the disclosure boundary.
4. Resolve the requested target privately and pin its identity, definition,
   signature, and executable revision. An unknown, ambiguous, stale, or
   unavailable target shares the same non-disclosing target-denied outcome.
5. Make the base target `EXECUTE` decision by stable target identity before
   exposing the signature or binding result. The gateway permission from work
   ADR 0042 is not a wildcard target grant.
6. Run private prebind against the pinned signature. It resolves supplied
   selectors and verifies typed values, but evaluates no default. An ID and a
   name that resolve to one parameter are a bind failure. A policy can receive
   only its declared prebound inputs. A missing, defaulted, or unbound policy
   input fails closed.
7. Evaluate policy, `SECURITY INVOKER` or `SECURITY DEFINER` transition, and
   required SERVER or CLIENT capabilities under trusted session state. Request
   fields, caller offers, observer context, and runtime trust cannot create a
   principal, role, delegation, grant, or capability.
8. Append and commit one protected invocation decision before any allowed
   binding fact, retained bind failure, default result, target fact, or Event
   crosses the disclosure boundary. A final denial discards the private
   resolution and prebind state. A bind failure that policy permits to remain
   visible is emitted only after this durable allowed decision.
9. After an allowed committed decision, evaluate defaults in the same pinned
   context, require complete binding, then execute the target. The start Event
   is not disclosure-bearing; only disclosure-safe Event bodies may follow.

The invocation audit record is a new protected kernel relation named
`_orna_kernel.invocation_audit_events`. It does not change the closed work ADR
0024 security-audit row. Each row has a kernel-generated sixteen-byte
`InvocationAuditEventId`, a generated database sequence and recording time,
its unique `InvocationId`, and one closed decision outcome: `ALLOWED` or
`DENIED`. It is append-only decision evidence, not an invocation lifecycle
log. Later completion, failure, cancellation, and delivery states do not alter
or append an invocation-audit row.

`target_function`, the pinned revision pair, and `security_audit_event` are
all present together or all absent. They are present for `ALLOWED` and
they are present for `DENIED` when resolution pinned a target; and they are
absent for an unresolved target. `security_audit_event` references the matching
protected allowed or denied `EXECUTE` decision for its target and revision. The
row contains audited session and effective-principal facts plus
the authorising evidence that protected inspection permits. It does not retain
a complete Request, argument values, idempotency key, observer context, opaque
bytes, unredacted bind failure, or arbitrary error text. A malformed Request
has no invocation audit record because no InvocationId or target decision
exists. A target denial has one durable denied decision. An allowed target has
one durable allowed decision before execution.

One owned PostgreSQL migration creates this exact shape, including its
identity, unique InvocationId, paired-nullability, foreign-key, and
closed-outcome constraints. Recovery first validates row shape and outcome,
then validates every required security-audit link, target identity, and pinned
revision. It fails closed; it does not repair, infer, or delete an invocation
decision. The relation grants no public write authority; a later protected
inspection decision owns its readable view and redaction policy.

Redaction occurs before Event construction. It is not a presenter function and
cannot be changed by trace policy, output requirements, caller context, or
client offers. `Display` and default `Debug` output for private request,
binding, security, and error state must not reveal values or selectors.

## Error boundary

The following boundaries are closed:

| Failure | Result |
| --- | --- |
| Invalid ORF5 envelope, stream state, wrong carrier position, wrong `p_request` identity, or invalid Event received from the server | protocol failure; close the connection without an invocation result |
| Rejected sealed system entry | `CALL_FAILED(EXECUTE_DENIED)` before Request decode or acceptance |
| Bounded Request envelope that fails Request decoding or protocol-major comparison | `CALL_FAILED(INTERNAL_FAILURE)` before acceptance; no target disclosure |
| Target resolution, base `EXECUTE`, policy, definer, or capability failure before allow | one redacted `InvocationFailed` Event after `InvocationStarted`, then `CALL_COMPLETED` |
| Binding failure that a final allowed decision permits to be disclosed | one redacted `InvocationFailed` Event only after the durable allowed decision, then `CALL_COMPLETED` |
| Audit, transaction, execution, or transport failure after acceptance | one redacted `InvocationFailed` terminal Event, then `CALL_COMPLETED` when delivery remains possible |
| Client cancellation after acceptance | one `InvocationCancelled` terminal Event, then `CALL_COMPLETED`, unless an operational failure wins |

Every `InvocationFailed` uses one of these exact redacted bodies. Details are
absent in every case and retryability is `no` unless stated otherwise:

| Cause class | Phase | Code | Message | Retryability |
| --- | --- | --- | --- | --- |
| Resolution, base `EXECUTE`, policy, definer, or capability denial | `authorise` | `INVOKE_DENIED` | `invocation was not permitted` | no |
| Bind failure after an allowed decision | `bind` | `INVOKE_BIND_FAILED` | `invocation arguments were not accepted` | no |
| Target execution failure | `target` | `INVOKE_TARGET_FAILED` | `invocation target failed` | unknown |
| Audit, transaction, or internal failure | `internal` | `INVOKE_INTERNAL_FAILURE` | `invocation could not complete` | unknown |
| Delivery failure while the peer remains connected | `transport` | `INVOKE_TRANSPORT_FAILURE` | `invocation delivery failed` | unknown |

`InvocationFailed.phase` uses the fixed carrier phases from work ADR 0053.
These five codes are the complete version-1 set for this signature. This
decision adds no causes, origin spans, failure maps, or JSON wire form. The
JSON schemas in `spec/spec/` remain diagnostic and Inspector-export material;
they are not ORF5 or ORV5 authority.

## Standard source and first dogfood path

`sys.invoke` remains Ring-1 and does not become an ordinary `.orna` function.
It is inspectable through its sealed registry facts and protected inspection
interfaces. The first target exercised through it must be normal installed
standard source, not a Rust-only test function or a raw direct dispatch.

The first source slice adds `stdlib/std/invoke.orna`. It creates
`std.invoke.echo`, a `SECURITY INVOKER` SERVER function with one non-null
`INTEGER` parameter named `p_value` and one typed INTEGER result. Its body has
no data access, default, presenter, CLIENT runtime, capability, state, or UI
behaviour. The standard-install path compiles and pins this unit in the normal
verified standard snapshot.

The first live proof performs this exact path:

```text
installed std.invoke.echo .orna source
  -> authenticated ORF5 raw call to sealed sys.invoke
  -> Request target by qualified name and argument by parameter name
  -> RESULT_VALUES WINDOW_UPDATE with credit for all three Event frames
  -> protected resolve, execute decision, prebind, audit, and execution
  -> InvocationStarted(0)
  -> ValueBatch(1) containing typed INTEGER echo result
  -> InvocationCompleted(2)
  -> CALL_COMPLETED
```

It proves that the public invocation route enters a normal `.orna` target.
It does not make `std.invoke.echo` a special function after installation.
The same proof must also invoke it by the pinned target and parameter IDs,
and prove that direct raw execution of that application target stays denied by
the ordinary general-invocation boundary.

## Required proof

Public behaviour and live PostgreSQL tests must prove:

* the sealed registry exposes exactly the function signature, `p_request`
  identity, and `STREAM<Event>` result, without an application definition;
* only the two stated ORF5 positions accept their exact carrier identities;
  all ordinary positions and every prior protocol major remain closed;
* wrong, missing, duplicate, and additional arguments fail without state or
  credit changes;
* frame decoding keeps a bounded Request envelope private until protected
  decode, while invalid outer frames remain protocol failures;
* accepted Event streams start at zero, use one InvocationId, remain
  contiguous, have one terminal Event, and use only `RESULT_VALUES` credit;
* cancellation, zero credit, connection closure, audit failure, and execution
  failure retain the stated durable-work and terminal-outcome rules;
* malformed requests, resolution failures, base denials, policy denials, and
  bind failures disclose no protected target, signature, selector, type,
  value, default, principal, capability, or audit fact;
* a final allowed or denied target decision is durable before its permitted
  outcome crosses the disclosure boundary, and all phases use one snapshot;
* Request and audit redaction prevents payload bytes from entering `Display`,
  default `Debug`, Event payloads, or ordinary audit storage; and
* the protected invocation-audit migration and recovery reject a malformed,
  unlinked, wrongly paired, wrong-outcome, or disclosure-bearing durable row
  without changing prior history;
* each redacted failure class uses its exact phase, code, message, absent
  details, and retryability without exposing the private cause; and
* the installed `stdlib/std/invoke.orna` function follows the complete live
  path above by name and by identity, while a direct raw call to it remains
  outside the general invocation boundary.

## Implementation sequence

1. `docs(invoke): define the sealed invocation stream` changes this ADR and
   the work-ADR index only.
2. `feat(core): register the sealed invocation signature` changes the sealed
   system registry, its public exports, and revision closure tests. It adds no
   application function or frame position.
3. `feat(protocol): open the sealed invocation frame positions` changes the
   frame state machine and protocol exports. It keeps a bounded Request
   envelope private and adds the Event lifecycle state with direct tests.
4. `feat(core): decide protected invocation access` changes the invocation
   and security modules. It implements one-snapshot resolution, prebind,
   target decision, and redaction without execution or durable storage.
5. `feat(postgres): persist protected invocation decisions` changes one
   migration, the private PostgreSQL audit module, and focused recovery tests.
   It adds the new protected relation without changing work ADR 0024 rows.
6. `feat(server): dispatch the first sealed invocation` changes the server
   dispatcher and its focused tests. It performs the durable decision before
   every disclosure-bearing Event and implements cancellation draining.
7. `feat(std): dogfood the first invocation target` changes
   `stdlib/std/invoke.orna` and the standard-source install manifest only.
8. `test(server): prove the first live invocation` changes the focused live
   system test and its source fixture only.

Each commit changes one to three files, uses a signed Conventional Commit, and
keeps the repository buildable. Format, strict Clippy, rustdoc, diff,
similarity, workspace, protocol, security, standard-install, and focused live
PostgreSQL gates remain required for their changed surfaces.

## Deferred surface

This decision does not define a general application `STREAM` type or source
syntax, general collection arguments or results, a Timestamp deadline,
presenter selection, output-byte or UI channels, progress, trace, artifact
transfer, CLIENT execution, runtime selection, Inspector storage, CLI
`orna invoke`, `--explain`, JSON-RPC, MCP, remote transport, TLS, or a general
planner. It also does not add the event kinds deferred by work ADR 0053.

Later decisions must add each payload, event kind, frame position, security
input, and user interface surface explicitly. They must not repurpose a
reserved channel or use untyped JSON, maps, or opaque bytes as a shortcut.

## Precedence

This decision implements Step 7 of work ADR 0053 and the first executable
boundary of spec ADR 0004. It narrows the `sys.invoke` signature in
`spec/api/sys-invoke.md` to this sealed first stream and provides the first
standard-source dogfood path required by the self-hosting rings.

Work ADR 0039 remains authoritative for ORV5 value bytes and bounds. Work ADR
0042 remains authoritative for the system function identity and entry access.
Work ADR 0053 remains authoritative for Request, Value, and Event carrier
bytes, carrier fields, event bodies, redaction rules, and Event codec scope.
Work ADR 0026 remains authoritative for the raw frame envelope, stream bounds,
and ordinary flow control except for the two selective ORF5 positions and the
`sys.invoke` lifecycle stated here. The canonical specification remains
authoritative outside this accepted implementation boundary.
