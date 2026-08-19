# ADR 0078: CLIENT-to-SERVER Resource Transport and Scheduling

**Status:** Accepted

## Decision

Implement the asynchronous resource boundary from spec ADR 0017 with a
versioned authenticated frame family. The wire contract is
`ORNA-RESOURCE/1`. It carries typed canonical values and no JSON request or
result payloads.

The transport runs on the existing authenticated invocation connection. The
connection authenticates the session before it decodes or executes a resource
request. The server routes the request through the sealed `sys.invoke` path and
its existing security audit. The resource adapter owns request scheduling,
stream credits, cancellation, terminal ordering, and conversion to the local
`ClientResource` lifecycle.

## Request identity

Each request contains these fields in this order:

```text
protocol_version
stream_id: u64
request_id: InvocationId
parent_invocation_id: InvocationId
call_site_id: CallSiteId
target_function_id: FunctionId
target_revision: RevisionPair
generation: u64
resource_kind: SINGLE | STREAM
argument_count: u32
arguments: canonical ParameterId + typed value pairs
item_window: u64
byte_window: u64
```

`stream_id` is connection-local. `request_id` identifies the nested invocation
and is globally unique within the active invocation history. `parent_invocation_id`
and `call_site_id` provide correlation and instance context. `generation` is the
monotonic local resource generation.

The request does not contain a principal, role, `run_as` value, capability
list, credential, retry policy, unchecked result type, or client-supplied
invalidation token. The server derives the authenticated session context and
checks target revision and result type from its active catalogue.

Arguments are sorted by ascending `ParameterId`. Each parameter occurs once.
The value codec is the active canonical codec. A declared `SET` value uses the
accepted ORV6 transport form. The decoder rejects duplicate, missing, unknown,
non-canonical, truncated, over-sized, or trailing argument data.

`item_window` and `byte_window` are initial receive credits. Both must be
non-zero for a stream. A scalar request may use the fixed scalar limits. Credit
addition is checked for overflow and cannot exceed the configured connection
maximum.

## Server frames

The server emits one of these frames for an accepted request:

```text
RESOURCE_ACCEPTED {
    protocol_version
    stream_id
    request_id
    nested_invocation_id
    target_revision
    resource_kind
}

RESOURCE_VALUES {
    protocol_version
    stream_id
    request_id
    batch_sequence: u64
    item_count: u32
    byte_count: u32
    values: canonical typed values
}

RESOURCE_COMPLETED {
    protocol_version
    stream_id
    request_id
    final_batch_sequence: u64
    total_items: u64
}

RESOURCE_FAILED {
    protocol_version
    stream_id
    request_id
    failure: structured redacted CallFailure
}

RESOURCE_CANCELLED {
    protocol_version
    stream_id
    request_id
    reason: structured cancellation code
}
```

`RESOURCE_ACCEPTED` appears once and binds the client request to the nested
server invocation. A scalar request emits one non-empty `RESOURCE_VALUES`
frame followed by `RESOURCE_COMPLETED`. A stream emits zero or more non-empty
batches followed by one terminal frame. A batch sequence starts at zero and
increases by one. `total_items` includes all accepted batches.

A result frame must echo the request and active target revision. The client
rejects a mismatched request ID, stream ID, target revision, batch sequence,
value count, value byte count, or declared result type. It does not publish
partially decoded or mismatched values.

A `RESOURCE_FAILED` frame contains the existing structured failure form. It does
not contain raw arguments, credentials, principal data, capability grant
contents, or opaque value bytes. A `RESOURCE_CANCELLED` frame is terminal and
contains no result value.

## Client control frames

The client sends these control frames:

```text
RESOURCE_WINDOW_UPDATE {
    protocol_version
    stream_id
    request_id
    add_items: u64
    add_bytes: u64
}

RESOURCE_CANCEL {
    protocol_version
    stream_id
    request_id
    reason: structured cancellation code
}
```

`RESOURCE_WINDOW_UPDATE` is valid only for an accepted live stream. It adds
credit after checked overflow and configured maximum checks. The sender must
stop before it consumes either item or byte credit. Zero-credit windows are
valid after a stream starts and provide backpressure.

`RESOURCE_CANCEL` is idempotent. The client may send it once for a local
cancellation transition. Repeated controls are ignored after the request is
terminal. If the server commits a terminal result before it observes
cancellation, that result wins and the server emits no replacement terminal
frame. If the server observes cancellation first, it emits
`RESOURCE_CANCELLED` and emits no later value or failure.

The client drops every late frame for a terminal request. A dropped frame must
not change the resource cache, generation, model, state, or audit projection.

## Scheduling and transaction boundary

The resource adapter creates a nested `sys.invoke` request after it validates
frame structure, session state, target identity, revision, and typed arguments.
The SERVER function executes under the existing server transaction and
capability rules. The adapter never starts a second transaction for a batch and
never exposes a partially committed transaction result.

The client runtime remains responsive while a request is queued, running, or
waiting for stream credit. The server scheduler may run several independent
resources, but it must preserve per-request batch order and terminal ordering.
The transport does not promise global order between different stream IDs.

The adapter applies a bounded queue. It does not buffer an unbounded stream
when the client has no credit. When the connection closes, the adapter cancels
all active nested requests, releases reservations, and emits no frame on the
closed connection. Server shutdown applies the same terminal rule before it
releases the authenticated session.

## Failure and revision handling

The server denies a request before target execution when the protocol major,
frame shape, active session, target function, target domain, target revision,
argument set, or resource kind is invalid. The public failure is redacted but
has a stable code for the client runtime and audit system.

The server validates that a target declared as `STREAM<T>` is used only with a
stream resource and that a scalar target is used only with a scalar resource.
The client validates each returned value against the pinned declaration before
it stores or publishes the result.

A stale or future revision never executes. A stale generation never publishes.
A result from a valid older generation is discarded even if its target revision
is current. A malformed frame fails closed and terminates the affected request;
it does not alter other stream IDs on the connection unless the connection
itself fails validation.

The adapter does not retry a request after a transport, decode, server, or
cancellation failure. An explicit refresh creates a new request and generation.
A root deadline may cancel the request, but the client cannot request an
unbounded deadline.

## Audit mapping

The accepted request records a nested invocation start and the authorisation
decision through the existing security audit. The terminal result records one of
completed, failed, or cancelled and may include item and byte counts.

Audit identity fields include:

```text
request_id
parent_invocation_id
call_site_id
target FunctionId + RevisionPair
session principal
allow/deny decision
terminal outcome
```

Audit records exclude raw argument values, result values, credentials, grant
contents, USER state values, and opaque codec bytes. The server uses the
existing redaction policy for denied targets and failures. The client must not
create a second audit authority from transport frames.

## Versioning and canonical encoding

The protocol header contains `ORNA-RESOURCE` and major version `1`. The major
version must match exactly. A future minor version may append length-delimited
optional fields. Unknown required fields, duplicate fields, invalid enum values,
non-canonical integer encodings, oversized counts, and trailing bytes are
errors.

Frame fields use the repository's canonical integer widths and byte order. Typed
values use the active `orna-value` canonical encoding. Resource request and
result golden tests must compare exact bytes, not only decoded structures.

The decoder validates limits before allocation. It checks batch item count,
batch byte count, total item count, maximum frame size, maximum argument count,
maximum nesting depth, and stream credit. It must not allocate from an
attacker-controlled count before the limit check.

## Required implementation slices

Implement the transport in this order:

1. define resource frame structures and exact encode/decode helpers;
2. add deterministic golden tests, malformed-frame tests, and revision/type
   rejection tests;
3. add client control handling for window updates, cancellation, and terminal
   late-frame rejection;
4. connect accepted requests to the existing authenticated `sys.invoke` path and
   `ClientResource` state machine;
5. add server scheduling, bounded stream credits, nested invocation audit, and
   shutdown cancellation;
6. add an installed authenticated live proof for scalar results, a denied
   request, cancellation, and one bounded stream.

The implementation must preserve the current standalone client runtime fixture
and use the same runtime boundary for conformance tests. It must not add a
production native runtime, browser transport, or remote shared-library loader
under this ADR.

## Required proof

The proof is complete only when it shows:

- exact request and response frame round trips;
- rejection of wrong major, wrong revision, wrong domain, wrong result type,
  duplicate argument, non-canonical order, overflow, and trailing bytes;
- no server execution before authentication and authorisation;
- no raw arguments or result bytes in audit output;
- bounded stream delivery with credit exhaustion and resumed delivery;
- cancellation wins before completion and completion wins after commit;
- shutdown cancels active resources and drops late frames;
- an authenticated CLIENT resource receives a typed SERVER result through the
  installed invocation host.

## Precedence

This ADR implements spec ADR 0017 and work ADR 0077. Work ADRs 0020, 0054,
0060, 0068, 0071, 0073, 0074, and 0076 remain authoritative for authenticated
invocation, security decisions, CLIENT expressions, resource identity, value
transport, runtime lifecycle, and the headless runtime boundary.
