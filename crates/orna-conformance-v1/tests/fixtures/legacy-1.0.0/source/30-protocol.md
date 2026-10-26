# 30. Live protocol and session recovery {#protocol}

## Transport and value profile

The live protocol uses the WebSocket subprotocol `orna.present.v1` over RFC 6455. One binary WebSocket message contains one complete OVB-1 CBOR envelope. Fragmented WebSocket frames are reassembled before decoding. Text application messages are rejected; ping, pong and close retain their WebSocket meanings. TLS is required outside an explicitly trusted loopback transport.

The [canonical value profile](#formats) defines integer, float, decimal, option, nominal-value, reference and snapshot bytes. Protocol structure uses the untagged CBOR maps/arrays specified here; application values use their OVB-1 typed representations. A protocol decoder must not confuse structural null fields with a typed optional result.

**ORNA-PROTO-001** An implementation MUST decode and validate the complete message, required fields, canonical forms and configured limits before admitting its operation. It MUST reject duplicate map keys and noncanonical integer/length forms; it MUST NOT execute a partially decoded request.

## Session creation and ownership

A trusted client creates a session with `POST /orna/session`, content type `application/json`, using UTF-8 JSON without duplicate members. The request is exactly `{ "database": "<database-uuid>", "protocol": "orna.present.v1" }`. The database must already be exposed by the host; this request does not attach arbitrary filesystem paths. The endpoint checks its authentication/perimeter policy and Origin before creating execution state.

A successful response is HTTP 201 with `session`, `database` and `runtime` UUID strings, `resume_token` (unpadded Base64url of 32 unpredictable bytes), `websocket_path` (`/orna/live/<session-uuid>`), `lease_ms`, and `limits`. The response sets an HttpOnly, SameSite=Strict session cookie scoped to that WebSocket path; Secure is required on TLS. The cookie authenticates the WebSocket upgrade. The session UUID alone is not a capability. Native clients may retain and send the same cookie through their HTTP/WebSocket stack.

A client resumes with `POST /orna/session/<session-uuid>/resume` and exactly `{ "resume_token": "...", "protocol": "orna.present.v1" }`. A live, unexpired session returns HTTP 200 with the same session identity, current runtime identity, a rotated token/cookie and limits. Only one active WebSocket is attached to a session; a successful resume replaces the old attachment, not its owned tasks. A failed or expired resume returns HTTP 410 and never fabricates a continuation of the old runtime.

`DELETE /orna/session/<session-uuid>` with `Authorization: Bearer <resume_token>` explicitly ends the session. The token must be the currently retained, unexpired token for that exact session. The WebSocket-path cookie alone is not sent to this HTTP endpoint and is not its authentication mechanism. Deletion also checks Origin under the same trusted-client policy. It stops new requests, cancels session-owned work, joins cleanup, and returns HTTP 204 after orderly termination. Loss of the WebSocket starts the advertised finite reconnection lease. No new client request is admitted on a lost connection. Expiry terminates the owner and cancels its children. The default lease is 30,000 ms; a host may advertise a different positive bounded value no greater than 300,000 ms. Explicit close or local REPL death does not wait for this network grace period.

Creation/resume errors use JSON `{ "code": "...", "message": "..." }` and an appropriate status: 400 malformed request, 401/403 unauthenticated or prohibited origin, 404 unavailable database, 409 incompatible protocol/runtime, 410 expired session, 413 limits exceeded, 503 temporarily unavailable. No error includes plaintext credentials or host-private paths. Session HTTP JSON permits no unknown members in version 1; an extension requires explicit negotiation.

**ORNA-PROTO-002** Session resumption MUST preserve request reservations and terminal outcomes for that session while its lease remains valid. A new runtime generation MUST invalidate operational handles from the old generation. Durable request recovery may report old outcomes as data without reviving old handles or owners.

## Envelope and field rules

Every envelope contains exactly these required structural fields:

| Key | Type | Meaning |
|---|---|---|
| 0 | UInt | Protocol major version, exactly 1. |
| 1 | UInt16 | Message type from the registry below. |
| 2 | ByteString(16) or null | Request ID. Required and nonnull on every client operation. Server notifications may use null as specified below. |
| 3 | ByteString(16) or null | Watch ID. Required only for messages bound to an existing watch; otherwise null. |
| 4 | Map<UInt16, value> | Type-specific body. |

All maps use definite lengths and canonical key order. Unknown message types fail. Unknown envelope keys fail. In a body, an unknown key 0…32767 is optional extension data and is ignored after bounded decoding; an unknown key 32768…65535 is a mandatory extension and causes rejection. Known version-1 fields below are required unless explicitly marked optional. No extension may reinterpret a known key. Extensions are included in fingerprint bytes even if an endpoint ignores their semantics.

The request ID is a client-chosen 128-bit identifier, compared as bytes, scoped by session identity. Watch and action/resource handles are server-chosen 128-bit identifiers scoped by session/runtime. An ID is not source text, a user-visible name or an authority grant. Reusing the same request ID for different input fails with `wire.request_mismatch`.

## Complete message registry

| Type | Direction | Name | Envelope watch | Body fields |
|---|---|---|---|---|
| 0 | Client → host | subscribe | null | 0 resource handle bytes16; 1 PresentationContext. |
| 1 | Client → host | unsubscribe | existing ID | Empty map. Cancels and removes this watch; an already absent watch is an idempotent success. |
| 2 | Client → host | resync | existing ID | Empty map. Host replies with the latest full snapshot. |
| 3 | Client → host | event | existing ID | 0 page revision UInt; 1 action handle bytes16; 2 typed event value; 3 fingerprint bytes32. |
| 4 | Client → host | eval | null | 0 source Str; 1 DatabaseContext; 2 PresentationContext; 3 fingerprint bytes32. |
| 5 | Client → host | watch | null | 0 source Str; 1 DatabaseContext; 2 PresentationContext; optional 3 refresh floor as nonnegative Duration. |
| 6 | Client → host | cancel | null | 0 target kind (0 request, 1 watch); 1 target ID bytes16. The cancel request cannot target itself. |
| 7 | Client → host | request_status | null | 0 target request ID bytes16; 1 its expected fingerprint bytes32. Never executes the target request. |
| 16 | Host → client | snapshot | existing/new ID | 0 revision UInt; 1 complete PresentNode; 2 exact pinned Snapshot. |
| 17 | Host → client | delta | existing ID | 0 base revision UInt; 1 new revision UInt; 2 ordered operations; 3 exact pinned Snapshot. Request ID is null. |
| 18 | Host → client | result | null | 0 status (0 success, 1 ordinary failure, 2 cancellation, 3 terminal outcome retained without rich value); 1 typed value or structural null; 2 fingerprint bytes32; 3 Diagnostic or null. |
| 19 | Host → client | diagnostic | existing ID or null | 0 Diagnostic; optional 1 recoverable Bool. Request ID identifies a rejected request, or null for a watch/connection diagnostic. |
| 20 | Host → client | request_status_result | null | 0 target ID bytes16; 1 state (0 unknown, 1 reserved, 2 running, 3 terminal, 4 orphaned/uncertain); 2 fingerprint bytes32 or null; 3 retained result-body map or null. Request ID identifies the status request. |

An initial subscribe/watch or explicit resync receives a snapshot with its originating request ID. Subsequent automatic snapshots use a null request ID. Snapshot revisions start at 0 and increase; every delta has `new > base`. Resetting an existing watch revision to 0 without issuing a new watch ID is forbidden. A result for success carries the complete typed value, including Unit or a tagged Option; failure/cancellation carry structural null in field 1. A cancellation diagnostic describes termination but is not an ordinary catchable failure inside the target.

A completed control operation returns typed Unit in a success result. Cancelling another request acknowledges the cancellation request separately; the target eventually has its own terminal result. A terminal/unknown target cannot be turned into a different request by cancellation. `unsubscribe` and watch cancellation remove resources idempotently.

### Shared structures

**DatabaseContext** is map `{0: database_uuid, 1: snapshot_or_null}`. Null selects the current CWD at admission. A supplied snapshot is exact and permits only read-only evaluation unless it denotes the admitted writable CWD generation. The host pins the admitted context once; retrying an identical reserved request does not resolve its null selector again against a newer CWD.

**PresentationContext** is map `{0: locale, 1: timezone_or_null, 2: width_or_null, 3: theme, 4: supported_kinds}`. Locale is BCP 47 text accepted by the configured presentation package; timezone is an IANA identifier; width is a positive integer in character columns for a terminal or CSS pixels for a browser, with renderer mode declared by `theme` prefix `terminal/`, `web/` or `native/`. Theme is a presentation hint, not executable code. `supported_kinds` is an array of kind identifiers. Unsupported locale/theme falls back to the host's declared default and is reported in snapshot metadata; it does not alter stored values or canonical hashes.

**Diagnostic** is tag 60011 around map `{0: code, 1: severity, 2: message, 3: spans, 4: notes, 5: causes, 6: redacted}`. Code and message are text; severity is 0 note, 1 help, 2 warning, 3 error, 4 fatal; notes are safe strings; causes are nested diagnostics subject to depth limits; redacted is Bool. Each span is `[snapshot, file_path, start_byte, end_byte]`, with a repository-relative UTF-8 path or explicit redacted marker, and half-open byte offsets. A missing span is represented by an empty spans array, not a fabricated file. Optional key 7 carries a stable diagnostic UUID when retained.

These are transport structures, not user-declarable records whose exact field names must be guessed from JSON. Their corresponding typed system metadata preserves the same source and causal semantics.

## Present nodes and patches

A Present node is tag 60012 around `[kind, stable_key_or_null, properties, children]`. Kind is a stable text name or object UUID. Properties map stable text/field UUID keys to typed values. Children are an ordered array of Present nodes. Stable keys use one of `[0, field_name_or_id]`, `[1, table_uuid, complete_primary_key]` or `[3, explicit_typed_key]`. Unkeyed children have null keys and use position. Sibling stable keys must be unique.

A renderer may choose another visual layout, but it must preserve the logical content. An unknown kind is rendered as an inspectable structural node with its safe properties and children; it is not dropped. An action property is an opaque session-bound action handle with a declared input type, not JavaScript or Orna source to execute.

A patch operation is exactly one of:

```text
[0, path, value]             add
[1, path]                    remove
[2, path, value]             replace
[3, from_path, to_path]      move
```

Path components are `[0, field_name_or_id]` for a property, `[1, table_uuid, primary_key]` for a relation child, `[2, index]` for a positional child, and `[3, explicit_key]` for a keyed child. A property component is terminal unless its value is itself a Present node. Patches do not walk arbitrary private fields inside application values; replace that whole property instead.

`add` requires an absent property/key or a child insertion index in 0…length. `remove` requires an existing nonroot target. `replace` requires an existing target; the empty path replaces the complete root. `move` removes its source first and resolves the destination in that resulting tree, cannot move a node inside itself, and preserves its stable identity. Duplicate sibling keys, missing paths and invalid types invalidate the entire patch.

**ORNA-PROTO-003** The client MUST apply the ordered operations to a temporary tree and publish them atomically only if its current revision equals the declared base and every operation succeeds. Otherwise it MUST discard the patch and request resynchronisation. It MUST NOT leave a partially patched visible tree.

Fine-grained deltas are optional optimisations. Root replacement is always available. A host may coalesce visual updates for a slow client into its latest complete snapshot; this does not drop database rows or move source checkpoints.

## Actions, evaluation and request identity

A resource handle names a page/watchable value produced by the session's evaluation or configured entry function. An action handle is tied to the issuing watch and page revision. A stale action, wrong watch, incompatible typed input or unknown handle fails before any action execution; the host may send a new snapshot so the user can act on current state.

An eval source is exactly one REPL input. A watch source is exactly one expression whose resolved call graph is read-only and externally effect-free; its clock dependency is permitted and scheduled explicitly. Ordinary text in a page or event is never executable source.

For an event/eval, the fingerprint is SHA-256 of ASCII `orna.request.v1`, a zero byte, and canonical OVB encoding of `[session_id, message_type, envelope_watch, body_without_fingerprint]`. Identifiers in this structural tuple are byte strings. The host recomputes and compares it. Every supported optional extension field participates in these bytes. Non-event/eval requests obtain a fingerprint by the same rule using their entire body; clients need not send the redundant fingerprint field.

### Algorithm REQUEST-1

1. Decode, authenticate, enforce session ownership, validate the message and compute its fingerprint.
2. In a local durable reservation transaction, compare `(session, request_id)`. A different fingerprint fails. An existing terminal record returns the recorded outcome. An active record identifies that same operation; it does not schedule another copy.
3. Pin the requested database context and record admission with the safe operation identity. Resolve and type-check the source/action before its first user effect. Failure records one terminal diagnostic without writes.
4. Execute one activation with the reservation lease. Orna-controlled writes, the terminal claim and the compact terminal outcome are committed together on success. Failure/cancellation rolls back the user transaction and records terminal failure/cancellation separately. A read-only result also becomes a retained terminal outcome.
5. After a crash, a reserved operation without a terminal outcome is not automatically replayed. If it was proven to have performed only Orna-controlled transactional effects, recovery may establish rollback and mark it orphaned; if external effects may have occurred, report uncertainty. Neither case manufactures success or silently repeats external effects.
6. Send the terminal result if connected. A lost response does not undo a committed outcome. `request_status` reads the record without executing the request. Retention may prune rich output but keeps the identity/fingerprint and terminal disposition for the advertised reservation lifetime.

**ORNA-PROTO-004** A durable admission reservation and a terminal transactional claim are distinct records/states. The terminal claim and successful Orna writes MUST share one transaction. A reservation without a terminal claim MUST NOT be treated as permission to execute a potentially effectful request again.

## Limits, connection errors and reconnection

The `limits` JSON object contains `max_message_bytes`, `max_depth`, `max_nodes`, `max_collection_items`, `max_outgoing_bytes` and `request_retention_ms`, all positive integers. The minimum accepted profile is 16,777,216 encoded bytes, 64 nesting levels and 100,000 Present nodes. The host must advertise a bounded outgoing queue and a reservation-retention duration at least as long as the session lease. Compression, when negotiated, has separate decoded-size enforcement and cannot evade these bounds.

Malformed CBOR, an invalid envelope/version, unsupported mandatory extensions or invalid message direction closes the connection with code 1002 after a safe diagnostic where possible. Unsupported text application messages use 1003; excessive size uses 1009. A well-formed request with a stale handle, wrong fingerprint, invalid program or forbidden effect receives a correlated diagnostic without closing unrelated watches. Portable protocol codes are `wire.invalid_message`, `wire.unsupported`, `wire.limit`, `wire.request_mismatch`, `wire.stale_action`, `wire.unknown_handle`, `wire.read_only`, `wire.session_expired`, `wire.snapshot_expired` and `wire.outcome_unknown`.

On reconnect the host sends complete snapshots for resumed watches; delta history need not survive. A newly created session has new operational handles. A client must request retained status or inspect current state after uncertain delivery, not automatically resend a mutating action under a new request ID.

The message registry is also available as `profiles/live-messages.json`. The accompanying structural tests validate fields and bounds; independent client/server execution and malformed-frame fuzzing remain separate server-profile conformance obligations.

