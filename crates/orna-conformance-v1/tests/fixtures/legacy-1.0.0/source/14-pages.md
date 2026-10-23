# 14. Pages and live views {#pages}

## Pages as ordinary values

```text
A page function returns a Page value.
The Page's view callback returns a Present tree.
An action callback performs one ordinary activation.
```

**ORNA-PAGE-001** Pages MUST be ordinary values returned by functions, not a separate component declaration grammar.

**ORNA-PAGE-002** Widgets and layouts MUST be ordinary values that can be composed by normal functions.

## Watch model

Every connected page root is a watch over a typed value expression.

```text
CWD/system change
-> dependency graph finds affected watches
-> re-evaluate affected expression/subtree
-> compare old/new presentation trees
-> emit contextual delta
```

**ORNA-LIVE-001** The same watch and delta mechanism MUST support tables, charts, text, maps, Git views, runtime views and custom widgets.

**ORNA-LIVE-002** If a fine-grained delta cannot be derived, the runtime MUST fall back to replacing the nearest stable presentation subtree. There MUST be no data type for which live refresh is impossible merely because a specialized delta is absent.

## Stable presentation identity

- record children are keyed by field name;
- relation rows are keyed by primary key;
- UI nodes use explicit keys when supplied and otherwise a stable call-site-derived key;
- unkeyed lists use position and may require subtree replacement.

**ORNA-LIVE-003** A renderer MUST apply deltas in sequence order.

**ORNA-LIVE-004** A client detecting a missing base revision MUST request or accept a complete resynchronization snapshot.

## Live transport and programmable clients

The required live transport profile is the WebSocket subprotocol `orna.present.v1` with deterministic CBOR messages. This is implementation terminology and is absent from normal user-facing output.

**ORNA-WIRE-001** Every watch begins with a complete Present-tree snapshot. Deltas are an optimization; replacement of the nearest stable subtree, including the root, is the universal correctness fallback.

**ORNA-WIRE-002** Each watch has a server-issued stable identifier and a monotonically increasing revision. A delta names its base revision and new revision. A client MUST apply the complete ordered patch atomically only when the base equals its current revision; otherwise it MUST discard the patch and obtain a complete snapshot.

**ORNA-WIRE-003** The patch vocabulary is exactly `add`, `remove`, `replace` and `move`. Paths are typed sequences of record fields, relation keys and list indexes. A root replacement uses the empty path.

**ORNA-WIRE-004** Record children use field identity, relation rows use table `ObjectId` plus primary key, explicitly keyed UI children use that key, and unkeyed lists use position. Missing stable identity affects efficiency only; replacement MUST remain correct for every value.

**ORNA-WIRE-005** Connection queues, decoded message size, collection size, nesting depth and decompressed bytes MUST be bounded. A slow client MAY skip intermediate visual states and receive the newest complete snapshot. Database rows are not lost by visual coalescing.

**ORNA-WIRE-006** Reconnection does not require retained delta history. The client resubscribes and receives a complete current snapshot.

### Page actions

Normal page controls use page-produced action handles. An event contains the watch, page revision, action handle, request identity and typed input; it never relies on source text copied from the rendered page.

**ORNA-WIRE-007** A stale or unknown action handle MUST NOT execute. The host returns the current snapshot or a user-readable stale-view result.

**ORNA-WIRE-008** One action is one normal activation transaction. Successful writes commit together and invalidate watches; an escaping error or cancellation rolls them back.

### Explicit remote Orna evaluation

A trusted programmable client, browser developer console, LLM or remote CLI MAY intentionally submit Orna source.

**ORNA-EVAL-001** Source is executable only inside an explicit `eval` or `watch` operation. Ordinary strings, fields, page events and protocol values MUST never be implicitly parsed as Orna.

**ORNA-EVAL-002** The host MUST parse, resolve, type-check and execute submitted source with the same language implementation, module graph, activation semantics, diagnostics and presentation system as the local REPL. A client-supplied AST, bytecode or query plan is never trusted as authoritative.

**ORNA-EVAL-003** `eval` accepts one REPL input and may read or mutate the served clone's CWD. It is one activation transaction.

**ORNA-EVAL-004** `watch` accepts an expression and remains live through the universal watch mechanism. A watch expression may read tables and `sys`, call deterministic read-only functions and use activation time. It MUST NOT mutate tables, reveal secrets, open connectors, perform network/filesystem/process operations or use randomness. The host derives this from the resolved call graph; no user effect annotation is required.

**ORNA-EVAL-005** A time-dependent watch such as `now() - 1.min` is permitted and records an explicit clock dependency so it is reevaluated at the required boundary.

**ORNA-EVAL-006** Remote REPL sessions use the same ephemeral-module model as the local REPL: imports, bindings and declared helper functions persist for the session, and wildcard imports follow ordinary language rules.

### Request recovery

**ORNA-EVAL-007** Every mutating remote action or `eval` carries a client-generated 128-bit request identity. The host binds that identity to a canonical fingerprint of the operation.

**ORNA-EVAL-008** A request's terminal claim, successful Orna-controlled writes and compact terminal outcome MUST commit together. Durable admission reservations are distinct; [REQUEST-1](#protocol) defines crash and uncertain-external-effect handling. A matching completed identity returns its recorded outcome rather than executing again.

**ORNA-EVAL-009** Reusing a request identity for different source, action or typed input MUST be rejected.

**ORNA-EVAL-010** The compact request identity/fingerprint/outcome record is durable database runtime metadata and remains reserved. Rich result presentation may be discarded according to documented retention; loss of the rich result MUST NOT permit re-execution under the same identity.

**ORNA-EVAL-011** The deduplication guarantee covers Orna-controlled database writes only. External effects cannot be made transactional. Clients MUST NOT automatically retry a mutating evaluation after uncertain delivery; they recover its recorded status or inspect current state.

### Wire messages and values

Client operations are `subscribe`, `unsubscribe`, `resync`, `event`, `eval`, `watch`, `cancel` and `request_status`. Host operations are `snapshot`, `delta`, `result`, `diagnostic` and `request_status_result`. WebSocket control frames provide connection close, ping and pong.

**ORNA-WIRE-009** The binary envelope, message fields, deterministic CBOR rules and exact encodings for `Decimal`, `Money`, UUID/ObjectId, dates, instants, duration, quantities, enums, typed records, references, diagnostics and Present nodes are defined in `profiles/live-protocol.md` and form part of repository/server conformance.

**ORNA-WIRE-010** Secret values are not wire-encodable. A redacted secret handle may be presented only as non-revealing metadata.

**ORNA-WIRE-011** WebSocket compression is optional and cannot change semantics. Normal same-origin checks and the trusted authenticated VPN/SSH/reverse-proxy boundary apply; core Orna does not add a grants database.

**ORNA-WIRE-012** Protocol failures are rendered normally as user concepts such as “the live view was refreshed” or “this operation may already have completed.” Raw CBOR, revision or framing details appear only in explicit developer inspection.

A production server-profile claim requires independent client/server interoperability, loss/reorder/reconnect simulation, malformed-input fuzzing, bounded slow-client tests and equality with the reference vectors. These are evidence obligations for an implementation, not unresolved semantics.

