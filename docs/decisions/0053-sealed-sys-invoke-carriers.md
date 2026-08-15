# ADR 0053: Sealed `sys.invoke` Carriers Use Three ORV5 Codecs

**Status:** Accepted

## Decision

The first canonical `sys.invoke` boundary has three sealed Ring-1 value types:

| Type | Stable `TypeId` | Representation contract |
| --- | --- | --- |
| `sys.invoke.Value` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 f0` | `orna.sys.invoke.value@1` |
| `sys.invoke.Request` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 f1` | `orna.sys.invoke.request@1` |
| `sys.invoke.Event` | `00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 f2` | `orna.sys.invoke.event@1` |

These byte strings are exact opaque `TypeId` bytes in network order. They are
not UUIDs. The names are exact, case-sensitive semantic names. The contracts
are immutable for payload version 1. ORF5 negotiates only protocol major 5 and
does not negotiate carrier payload versions. Until a separate accepted
payload-version negotiation mechanism exists, another payload version requires
a new `TypeId` or a new protocol major. A decoder never guesses a payload
version or broadens one of these three contracts in place.

The carriers are sealed kernel values. They are not application record types,
standard-library declarations, generic opaque values, JSON objects, or byte
blobs accepted without validation. One checked codec owns each payload. The
three codecs use the existing ORV5 registered-opaque envelope tag `0x0c` and
the exact identity above. They do not add an ORV5 tag or descriptor tag.

This is a narrow carrier decision. It defines enough event data for the first
canonical result path. It does not define the open planner, CLIENT artifact,
runtime, UI, trace, gateway, or Inspector event bodies from the canonical
proposal. Those event families remain closed until their typed payloads have
separate accepted decisions.

## Sealed carrier authority

`orna-core` owns one ordered, read-only carrier registry in this order:

1. `sys.invoke.Value`;
2. `sys.invoke.Request`; and
3. `sys.invoke.Event`.

Each definition binds one exact name, `TypeId`, and representation contract.
The registry is compiled into the signed distribution. PostgreSQL does not
store duplicate definitions. Application recovery and standard-library
recovery do not add them to either catalogue snapshot.

An application or standard catalogue cannot contain one of the three exact
identities or names. Admission checks carrier identities in registry order,
then carrier names in registry order, before it checks other cross-catalogue
facts. Core validation rejects a collision; it does not own application or
standard identity allocation. The compiler preparation allocator retries each
exact reserved carrier `TypeId` before candidate construction. A collision
cannot make a carrier resolve as an application, standard, or ordinary opaque
value.

The existing standard opaque registry remains authoritative for ordinary
opaque values. Carrier lookup precedes it only for the three exact identities.
No source, environment value, database row, codec contract string, or shared
library can register a fourth carrier or replace one of these codecs.

## Checked core model

The three carriers are explicit `RuntimeValue` families. They do not reuse
`OpaqueValue`, because an ordinary opaque codec has different catalogue and
position rules. The public non-exhaustive runtime enum adds exact checked
variants equivalent to:

```rust
RuntimeValue::InvokeValue(InvokeValue)
RuntimeValue::InvokeRequest(InvokeRequest)
RuntimeValue::InvokeEvent(InvokeEvent)
```

`InvokeValue`, `InvokeRequest`, and `InvokeEvent` have private state. Public
construction accepts complete logical input and returns a checked carrier or a
typed error. It does not expose a partial builder, mutable field vector,
unchecked payload constructor, raw-byte constructor, or public field rewriter.
Accessors return only immutable borrowed facts or consume the complete checked
carrier.

`InvokeValue` owns one checked inner `RuntimeValue`. `InvokeRequest` owns one
checked target, canonical argument sequence, caller context, client offer, and
control fields. `InvokeEvent` owns one checked kind-specific body. The core
model contains no encoded length, byte offset, ORV marker, or cached wire
payload. Protocol code owns those facts.

Core checks client offers for structural well-formedness only. It checks
required fields, non-empty values, descriptor structure, `SET` and `STREAM`
closure, nested-carrier closure, aggregate nodes, and redaction-safe accessors.
It does not encode a descriptor or nested value to sort or deduplicate sinks,
runtimes, consumed types, media types, contracts, or features. The source model
may therefore retain structurally valid unordered or duplicate offer items
until protocol encode or decode. A checked core Request is not by itself proof
of canonical offer bytes.

Core construction counts the same aggregate carrier tree in logical payload
order. It accepts 65,536 nodes and returns
`InvocationCarrierConstructionError::TooManyNodes { maximum: 65_536 }` before
it returns a carrier at node 65,537. The error displays
`invocation carrier tree has too many nodes` and has no source. No caller can
construct an over-limit checked carrier and defer the failure to encoding.

The normal runtime-type view reports the exact sealed carrier `TypeId` without
projecting it into an application or standard catalogue. Ordinary descriptor,
record, collection, function argument, result, and storage validators do not
accept that view. There is one explicit carrier classification, not three
special name checks spread through callers.

## Common payload rules

All carrier payloads use these rules:

* unsigned integers use big-endian bytes;
* signed ranks use two's-complement big-endian bytes;
* a Boolean is exactly `0` or `1`;
* an optional value is one presence byte, exactly `0` or `1`, followed by its
  content only when present;
* text is a four-byte byte length followed by exact UTF-8 bytes;
* a list is a four-byte item count followed by its items;
* a descriptor is a two-byte length followed by one complete canonical ORV5
  descriptor from work ADR 0039;
* an embedded carrier is a four-byte length followed by one complete ORV5
  envelope; and
* all counts, lengths, and totals use checked arithmetic before allocation or
  slicing.

The outer ORV5 payload limit remains 16 MiB. Every carrier version byte,
length, count, descriptor, embedded envelope, and string counts towards that
limit. A decoder requires complete consumption and rejects trailing bytes.
An encoder validates the complete carrier before it emits any bytes.

`MAX_INVOCATION_CARRIER_NODES` is the public constant `65_536`. One complete
carrier tree has that aggregate runtime-node limit. The
outer `InvokeValue`, `InvokeRequest`, or `InvokeEvent` wrapper counts as one
node. Each embedded `InvokeValue` wrapper counts as one node. Every runtime
value inside an `InvokeValue` then counts exactly as work ADRs 0036 and 0039
define, including record fields and `OPTION`, `LIST`, and `MAP` descendants.
Request metadata and Event metadata that are not runtime values do not count
as nodes. The bound applies across all embedded values in one Request or Event;
separate values cannot each consume an independent 65,536-node allowance.

Decode uses two private phases and returns no partial carrier:

1. Validate the complete outer ORV5 envelope, carrier identity, payload
   version, fixed carrier structure, all bounded embedded-envelope spans,
   canonical order, duplicates, and complete payload consumption. This phase
   does not decode an embedded ORV5 value.
2. Start with the outer carrier wrapper. For an outer `InvokeValue`, preflight
   its one inner ORV5 value next. For a Request or Event, visit each embedded
   `sys.invoke.Value` envelope in carrier payload order, count its Value
   wrapper, then preflight its inner runtime value in normal ORV5 traversal
   order. The first node that would make the aggregate 65,537 returns the
   carrier-node limit error before semantic materialisation or any later
   embedded value is visited. Only after the complete aggregate preflight
   succeeds may the codec materialise inner values and construct the checked
   carrier.

Therefore an outer or fixed-carrier syntax, truncation, ordering, duplicate,
or trailing-byte error precedes the aggregate node limit. During aggregate
preflight, an earlier malformed embedded ORV5 value precedes the limit; the
limit precedes a stale, inactive, or malformed later embedded value. Encoding
counts and revalidates the complete checked carrier in the same payload order
before it emits the outer marker or payload.

The protocol codec is the sole client-offer canonicalisation authority. For
encoding, it obtains the complete provisional canonical ORV5 bytes for each
offer item, rejects duplicate byte keys with original source indexes, sorts by
the exact byte keys defined below, then emits one canonical sequence. Offer
items include sinks, media types, runtimes, consumed descriptors, contracts,
and features. For decoding, the codec requires each wire sequence to be
strictly increasing by its same keys and rejects the first non-canonical or
duplicate item. Core does not reproduce this provisional encoding or byte
comparison.

Each payload starts with version byte `0x01`. Every other version byte is
rejected. Unknown discriminants, set flag bits, invalid UTF-8, non-canonical
order, duplicates, truncation, overflow, and trailing bytes return typed
carrier codec errors. They do not return a partial carrier.

The existing complete ORV5 codec functions remain the only public byte seam.
They accept and return these `RuntimeValue` variants with the same active
revision and ordinary opaque registry arguments used for all nested values.
They do not add a public carrier-only decoder or expose the private payload
parser.

`ValueCodecError` adds one source-carrying variant:

```rust
InvocationCarrier {
    carrier: TypeId,
    source: InvocationCarrierCodecError,
}
```

Its display is `sealed invocation carrier is invalid`. The public,
non-exhaustive `InvocationCarrierCodecError` derives `Clone`, `Debug`, `Eq`,
and `PartialEq`, implements `Error`, and has these categories. Each `path`
payload below is an `InvocationCarrierPath`; indexes are `usize`:

| Variant | Payload | Display |
| --- | --- | --- |
| `UnsupportedVersion` | `actual: u8` | `invocation carrier version is not supported` |
| `Truncated` | `offset: usize, required: usize, available: usize` | `invocation carrier is truncated` |
| `Trailing` | `remaining: usize` | `invocation carrier has trailing bytes` |
| `UnknownDiscriminant` | `path, actual: u8` | `invocation carrier discriminant is unknown` |
| `InvalidBoolean` | `path, actual: u8` | `invocation carrier Boolean is invalid` |
| `InvalidText` | `path` | `invocation carrier text is invalid` |
| `InvalidSemanticName` | `path` | `invocation carrier semantic name is invalid` |
| `InvalidField` | `path` | `invocation carrier field is invalid` |
| `NonCanonicalOrder` | `path, index` | `invocation carrier items are not in canonical order` |
| `DuplicateItem` | `path, first, duplicate` | `invocation carrier contains a duplicate item` |
| `NestedCarrier` | `path, carrier: TypeId` | `invocation carrier cannot contain another carrier here` |
| `InnerValue` | `path, source: Box<ValueCodecError>` | `invocation carrier typed value is invalid` |
| `TooManyNodes` | `maximum: usize` | `invocation carrier tree has too many nodes` |
| `PayloadTooLarge` | `actual: usize, maximum: usize` | `invocation carrier payload is too large` |

`InvocationCarrierPath` is an immutable sequence of these public,
non-exhaustive `InvocationCarrierPathSegment` values:

```text
ValueInner

RequestTarget
RequestArguments
RequestCaller
RequestClientOffer
RequestOutputRequirement
RequestStateProfile
RequestTracePolicy
RequestDeadline
RequestIdempotencyKey
RequestParentInvocation
RequestObserverContext

Argument(index)              Selector                 Value
CallerKind                   CallerFlags              TerminalColumns
TerminalRows                 Locale                   Timezone
PreferencePolicy

ClientProtocol               ClientLocale             ClientTimezone
ClientSinks                  Sink(index)              Descriptor
MediaTypes                   MediaType(index)         Streaming
PreferenceRank               Limits

ClientRuntimes               Runtime(index)           RuntimeName
RuntimeVersion               ConsumedTypes            ConsumedType(index)
Contracts                    Contract(index)          ContractName
ContractVersion              Features                 Feature(index)
Trusted                      ClientMaximumFrameSize   ClientMaximumArtifactSize
ClientLimits                 ClientPreferences

OutputAlias                  OutputMediaType          OutputType
OutputStreaming

EventKind                    EventInvocation          EventSequence
EventBody                    VisiblePrincipal         Channel
Schema                       BatchValues              BatchValue(index)
Severity                     Code                     Message
Duration                     Phase                    Details
Retryability                 Reason
```

A path starts with its carrier field and adds indexed or nested segments in
wire order. For example, the third argument value is
`RequestArguments / Argument(2) / Value`; the first runtime contract feature
is `RequestClientOffer / ClientRuntimes / Runtime(i) / Contracts /
Contract(j) / Features / Feature(0)`. No free-form string is a path segment.

Only `InnerValue` exposes a nested source. Syntax, canonical-order, duplicate,
node-limit, and semantic-field failures have no source. The first error in the
precedence above wins. Complete outer-envelope marker, tag, identity, size,
truncation, and trailing errors still precede this carrier source under work
ADR 0039.

## `sys.invoke.Value` payload

`sys.invoke.Value` carries one canonical typed value without erasing its type:

```text
1 byte   carrier version, exactly 1
4 bytes  complete inner ORV5 envelope length
n bytes  one complete inner ORV5 value
```

The inner value is decoded with the same active application revision, pinned
verified standard snapshot, and matching ordinary opaque registry as the
request. It may use any scalar, enum, record, reference, registered ordinary
opaque, `OPTION`, `LIST`, or `MAP` value already accepted by ORV5. All normal
active-revision, descriptor, node, map-order, opaque-contract, and payload
checks remain exact.

The inner value cannot be `sys.invoke.Value`, `sys.invoke.Request`, or
`sys.invoke.Event`. This rule prevents recursive carrier nesting and prevents
a request or event from entering an argument as ordinary data. The inner ORV5
length can be at most 16 MiB minus the five carrier bytes. Earlier ORV markers,
an ORF frame, JSON, PostgreSQL bytes, and an unchecked descriptor are rejected.

The checked core value owns the decoded `RuntimeValue`. Callers can borrow that
value or consume the checked carrier. They cannot construct a carrier from
unvalidated bytes or mutate the retained value.

## `sys.invoke.Request` payload

The version-1 request has one fixed field order:

```text
1 byte   carrier version, exactly 1
...      target
...      arguments
...      caller context
...      client offer
...      optional output requirement
...      optional state profile
1 byte   trace policy
1 byte   deadline presence, exactly 0 in version 1
...      optional idempotency key
...      optional parent InvocationId
...      optional observer context
```

Credentials, session principal, effective principal, roles, delegation tokens,
and impersonation claims have no request field. They come only from the
authenticated session and protected server state.

### Target and arguments

The target is one discriminant followed by one value:

```text
0  then 16 exact FunctionId bytes
1  then one non-empty resolved qualified semantic name
```

An unqualified name, alias, search path, empty name, or invalid semantic name
is not canonical request data. Name resolution remains server work. The client
does not turn a name into a direct application raw call.

Arguments start with a four-byte count. Each entry contains one selector and
one embedded complete `sys.invoke.Value` ORV5 envelope. A selector is:

```text
0  then 16 exact ParameterId bytes
1  then one non-empty semantic parameter name
```

Entries are sorted first by selector discriminant and then by their exact ID
or UTF-8 name bytes. Duplicate selectors fail. An ID and a name that later
resolve to the same parameter are not codec duplicates; the binder rejects
that ambiguity before it evaluates a default or executes the target.

### Caller context

Caller context has this exact order:

```text
1 byte   caller kind
1 byte   flags
...      optional terminal columns as u32, non-zero
...      optional terminal rows as u32, non-zero
...      locale text
...      timezone text
...      optional preference-policy Value
```

Caller kind values are:

```text
0 CLI_TTY             5 JSON_RPC_GATEWAY
1 CLI_PIPE            6 MCP_GATEWAY
2 DESKTOP_LAUNCHER    7 SCHEDULER
3 BROWSER             8 TEST_RUNNER
4 CLIENT_FUNCTION     9 RECOVERY
```

Flag bit 0 is `interactive`; bit 1 is `stdout_is_tty`; bits 2 through 7 must
be zero. `CLI_TTY` requires both flags and non-zero columns and rows.
`CLI_PIPE` requires both flags clear. Other caller kinds may state the two
facts independently. Locale and timezone are exact non-empty policy inputs;
the carrier does not infer them from the server host.

The optional preference-policy value transports one already typed policy
value. This decision assigns no preference semantics to it.

### Client offer

The client offer has this exact order:

```text
2 bytes  offered protocol major, exactly 5 in version 1
...      locale text
...      timezone text
...      sink offers
...      runtime offers
4 bytes  maximum accepted frame size, at least 1,024
8 bytes  maximum accepted artifact size
...      optional client limits Value
...      optional client preferences Value
```

The protocol value records the already negotiated major. It cannot renegotiate
the connection. A mismatch with the authenticated ORF5 connection rejects the
call before target resolution.

A sink offer contains, in order:

```text
descriptor
list of non-empty media-type text values
1 byte streaming support Boolean
4 byte signed preference rank
optional limits Value
```

Sink offers are sorted by descriptor bytes, then media-type list bytes,
streaming flag, and rank. Exact duplicates fail. Media-type list order is
canonical UTF-8 byte order and duplicate media types fail.

A runtime offer contains, in order:

```text
non-empty runtime name text
non-empty runtime version text
list of consumed descriptors
list of runtime contracts
4 byte signed preference rank
1 byte trusted Boolean
optional limits Value
```

Consumed descriptors use canonical descriptor-byte order and contain no
duplicates. One runtime contract contains non-empty name and version text,
then a canonical UTF-8 byte-ordered list of distinct feature text values.
Contracts are sorted by name bytes, version bytes, then feature-list bytes;
exact duplicates fail. Runtime offers are sorted by name bytes, version bytes,
then their complete remaining canonical bytes; exact duplicates fail.

These are canonical wire rules. The protocol codec applies them from the exact
provisional ORV5 bytes. Encoding accepts any structurally valid source order,
emits one sorted order, and rejects duplicates. Decoding rejects a wire
permutation instead of sorting it. `NonCanonicalOrder` is therefore a decode
error for offer items; `DuplicateItem` applies to encode and decode. Core does
not decide either error.

`trusted` reports local installation policy. It does not grant a server or
CLIENT function a capability, and it does not let the server load a native
runtime.

### Output and control fields

An output requirement contains these fields in order:

```text
optional non-empty alias text
optional non-empty media-type text
optional type selector
1 byte streaming requirement
```

A type selector is either discriminant `0` plus one exact `TypeId`, or
discriminant `1` plus one resolved qualified semantic name. At least one of
alias, media type, or type selector must be present. Streaming values are `0`
unspecified, `1` required, `2` preferred, and `3` forbidden.

The state profile is optional non-empty text. Trace policy values are `0` off,
`1` basic, `2` normal, `3` verbose, and `4` profile.

The canonical proposal requires a typed `Timestamp` deadline, but the accepted
runtime and ORV5 subset has no Timestamp value. Request version 1 therefore
requires a zero deadline-presence byte. Opening this field requires one
canonical Timestamp value plus a separately negotiated carrier contract, new
`TypeId`, or new protocol major. Encoding a deadline as RFC 3339 text, Unix
seconds, JSON, or host time is not permitted.

The optional idempotency key is a four-byte length and exact bytes. An empty
present key is rejected. The optional parent invocation is sixteen exact
`InvocationId` bytes. The optional observer context is one embedded
`sys.invoke.Value`. It supplies typed observation policy only; it cannot supply
identity, privilege, delegation, or capability authority.

## `sys.invoke.Event` payload

Every version-1 event begins with:

```text
1 byte   carrier version, exactly 1
1 byte   event kind
16 bytes InvocationId
8 bytes  sequence, any u64 retained exactly
...      kind-specific body
```

Sequence is connection-stream local evidence for one `InvocationId`. The
carrier codec validates one Event in isolation and retains any `u64` sequence
exactly. It does not know the prior event, require sequence zero, require
`InvocationStarted` first, enforce contiguity, select one terminal event, or
reject an event after termination. The later `sys.invoke` stream-state decision
owns those lifecycle rules and their connection-state errors. Decoding one
structurally valid Event cannot mutate stream state.

Version 1 accepts only these kinds:

```text
0 InvocationStarted
1 ValueBatch
2 Diagnostic
3 InvocationCompleted
4 InvocationFailed
5 InvocationCancelled
```

`InvocationStarted` contains one optional sixteen-byte visible
`PrincipalId`. The server includes it only when the caller may inspect that
identity. Absence does not mean that the invocation lacks an authenticated
principal.

`ValueBatch` contains channel byte `0` for `RESULT_VALUES`, one optional schema
`sys.invoke.Value`, a four-byte count, and that many complete embedded
`sys.invoke.Value` envelopes. At least one result value is required. This is
the only version-1 result channel. Byte, UI, progress, trace, artifact, and
client-control output remain closed.

`Diagnostic` contains severity `0` info, `1` warning, or `2` error, followed
by a non-empty printable ASCII stable code and UTF-8 message. A diagnostic is
not result data and cannot change stdout bytes.

`InvocationCompleted` contains an unsigned 64-bit duration in nanoseconds.

`InvocationFailed` contains a phase, a non-empty printable ASCII stable code,
a UTF-8 redacted message, optional typed details as one `sys.invoke.Value`,
and retryability `0` unknown, `1` no, or `2` yes. Phase values are `0` resolve,
`1` bind, `2` authorise, `3` target, `4` present, `5` runtime, `6` transport,
and `7` internal. This initial failure body does not define origin spans,
causes, or security-classification values. Those remain closed rather than
being represented as an untyped map.

`InvocationCancelled` contains optional non-empty UTF-8 reason text.

The proposed `TargetResolved`, `ArgumentsBound`, `SecurityDecision`,
`RevisionPinned`, `ExecutionStarted`, CLIENT artifact, presenter, presentation
plan, runtime, byte, UI, progress, and trace families have no version-1 kind.
An encoder cannot emit them and a decoder rejects them. Their stable payloads
require a new negotiated carrier contract, `TypeId`, or protocol major. This
decision does not reserve discriminants for unspecified payloads.

## Security, redaction, and routing

Carrier decoding establishes structure, not authority. Invocation uses two
private phases with one explicit disclosure boundary.

### Phase 1: structural decode

1. Require an authenticated active `USER` or `SERVICE` session.
2. Decode one complete Request with the active revision and matching
   registries, including canonical nested values and the aggregate node bound.
3. Require the request protocol major to match the authenticated ORF5
   connection.
4. Retain the checked Request privately. Do not resolve a target, inspect a
   signature, evaluate a default, select a presenter, emit a target event, or
   append argument data to an audit record.

An external structural failure is one redacted invalid-request result. The
typed codec error remains available only to the protected diagnostic path. It
cannot disclose whether an embedded type, target selector, parameter selector,
or opaque contract exists.

### Phase 2: protected prebind and authorisation

1. Resolve and pin the target privately. Do not emit or release the resolved
   identity, name, domain, revision, signature, or failure.
2. Check the authenticated principal's base system-entry and target `EXECUTE`
   authority by stable target identity before signature inspection. A denial
   appends one denied audit decision, discards the private target, and emits
   only a redacted denial. An absent or ambiguous private resolution is handled
   through the same non-disclosing result and cannot be used as a target
   existence probe.
3. After the base check permits protected inspection, perform one
   non-disclosing prebind. It resolves supplied parameter selectors against the
   pinned signature, checks each already canonical typed value, and retains
   either a private binding or a private bind failure. It emits no
   `TargetResolved`, `ArgumentsBound`, diagnostic, trace, or Inspector value.
4. A value-dependent policy receives only its declared inputs from that
   private prebind. It cannot inspect caller offers, observer context, output
   policy, or an undeclared argument. Version 1 does not evaluate defaults to
   obtain policy input. If a required policy input is absent, defaulted, or
   cannot prebind, policy evaluation fails closed.
5. Evaluate role, policy, definer, and capability gates under trusted session
   context. A final denial wins over every retained target or bind failure,
   appends one denied audit decision, discards prebind state, and releases none
   of the target, selector, signature, type, default, policy-input, or value
   facts.
6. Only after a final allowed decision and its audit record are durable may
   the invocation release disclosure-safe target and binding facts. If no
   value-dependent policy needed a failed field, an allowed caller may then
   receive the redacted bind failure. Defaults are evaluated only after this
   allowed disclosure boundary, in the accepted security and domain context.
7. Execute only after complete binding and the durable allowed decision. Emit
   only disclosure-safe Events.

No public interface exposes the private resolved target, prebind, policy input,
or retained bind failure before step 6. A concurrent catalogue or security
change cannot split resolution, prebind, final decision, audit, default
evaluation, and execution across snapshots.

The sealed gateway access from work ADR 0042 does not grant target access.
Observer context, caller kind, runtime trust, output requirement, state profile,
trace policy, and nested Value data cannot select a principal or create a
grant. Gateways remain normal authenticated `SERVICE` or delegated sessions.

Denial and malformed-request responses do not reveal whether a target name,
parameter, revision, presenter, runtime, object, or capability exists beyond
what the caller may inspect. Security decisions, bound-argument summaries,
failure details, and diagnostics are redacted before Event construction.
Redaction is not a presenter action and cannot be disabled by trace policy.

Carrier `Display` output contains no payload. Default `Debug` output reports
the carrier kind and safe sizes only. It does not print argument values,
idempotency bytes, observer context, failure details, or embedded opaque bytes.
Audit storage never retains a complete Request or Value carrier by default.
An explicit protected inspection path must apply classification and redaction
before it stores or emits a value.

Only the later exact `sys.invoke` signature may admit Request as its sole raw
argument and Event as its result stream. Ordinary application parameters,
results, record fields, collections, defaults, PostgreSQL storage, CLIENT
artifacts, and arbitrary raw calls continue to reject all three carrier types.
This decision does not allocate the stable `p_request` `ParameterId`, define
`STREAM<sys.invoke.Event>`, or open an ORF5 frame position.

## Required proof

Public behaviour tests must prove:

* the ordered registry contains exactly the three accepted names, identities,
  and contracts, with exact identity and name lookup;
* application and standard revision admission rejects each carrier identity
  and exact name in the stated global order while neighbouring names remain
  valid;
* compiler application and standard preparation retries each exact reserved
  `TypeId` before candidate construction, with no allocation rule in core;
* each exact outer ORV5 `0x0c` envelope and version-1 payload has an independent
  golden and round trip;
* Value retains every admitted inner ORV5 family and active-revision check,
  while earlier markers, all three carrier identities, malformed inner values,
  stale definitions, and mismatched opaque registries fail;
* core Request construction retains structurally valid unordered and duplicate
  offer items, while it still rejects malformed fields, `SET`, `STREAM`, nested
  carriers, excess nodes, and unsafe disclosure;
* Request target alternatives, canonical argument order, caller invariants,
  output requirements, control fields, and every closed value return exact
  typed errors without core offer-byte encoding;
* protocol encoding gives every permutation of equal sinks, media types,
  runtimes, consumed descriptors, contracts, and features identical canonical
  bytes, rejects duplicate byte keys with original source indexes, and protocol
  decoding rejects the first non-canonical or duplicate wire item through the
  exact typed error;
* deadline presence and every unknown discriminant, version, flag, count,
  length, UTF-8 sequence, truncation, overflow, and trailing byte fail without
  a partial carrier or unchecked allocation;
* an aggregate carrier tree at 65,536 nodes succeeds and node 65,537 returns
  `TooManyNodes` before a later inactive or malformed inner value, while fixed
  carrier structural errors retain their earlier precedence;
* each Event body and any isolated `u64` sequence round trip without connection
  state change, an empty ValueBatch fails, every deferred event kind remains
  closed, and the carrier codec performs no lifecycle validation;
* payload and default debug output do not expose sensitive bytes;
* structural failure releases no selector or catalogue fact; base denial
  precedes signature inspection; and policy denial after non-disclosing
  prebind suppresses every retained target, selector, signature, default,
  bind-failure, policy-input, and value fact;
* an allowed decision becomes durable before a bind failure or default result
  crosses the disclosure boundary, and resolution through execution retains
  one catalogue and security snapshot;
* arbitrary bounded carrier bytes never panic;
* ORV1 through ORV4 and ORF1 through ORF4 retain all bytes and closures; and
* Step 5 extends the shared frame closed-position check to all three
  `RuntimeValue::Invoke*` variants, so ORF5 encoding and decoding reject them in
  every ordinary argument and result position without changing connection
  state or window credit. No ORF5 carrier position opens until the separately
  accepted `sys.invoke` signature does so.

Tests use public registry, checked carrier, codec, frame, security, and revision
interfaces. They do not inspect source constants, duplicate the codec, or
construct unchecked carrier state.

## Implementation sequence

1. `docs(invoke): define sealed invocation carriers` changes this ADR and the
   work-ADR index only.
2. `feat(core): register sealed invocation carrier identities` changes
   `crates/orna-core/src/system.rs`, `crates/orna-core/src/revision.rs`, and
   `crates/orna-core/src/lib.rs`. It adds the ordered definitions, lookup,
   revision collision proof, and no allocation behaviour.
3. `feat(compiler): reserve invocation carrier type identities` changes
   `crates/orna-compiler/src/prepare.rs` only. Application preparation and
   standard preparation retry each exact carrier `TypeId` before candidate
   construction. Inline tests drive all three collisions in registry order and
   prove that core remains allocation-free.
4. `feat(core): construct checked invocation carriers` changes
   `crates/orna-core/src/invocation.rs`, `crates/orna-core/src/value.rs`, and
   `crates/orna-core/src/lib.rs`. It adds private-state checked Value, Request,
   and Event models and exact safe accessors. It retains structurally valid
   source-order client offers and does not sort, deduplicate, or encode them.
   It does not add bytes or a frame position.
5. `feat(protocol): encode sealed invocation carriers` changes
   `crates/orna-protocol/src/lib.rs` and
   `crates/orna-protocol/src/frame.rs`. It adds the three ORV5 codec paths,
   aggregate node preflight, the sole exact-byte offer canonicalisation and
   duplicate authority, exact goldens, and round trips. The frame module
   extends its shared ordinary-position closure beyond `Constructed` to the
   three new carrier variants; it does not open an ORF5 carrier position.
6. `test(protocol): exhaust invocation carrier failures` changes
   `crates/orna-protocol/src/lib.rs` only. It completes malformed, precedence,
   bound, arbitrary-input, redaction, and compatibility proof without a
   production change.
7. Accept the separate stable `sys.invoke` signature and event-stream decision.
   That decision owns the `p_request` identity, `STREAM<Event>`, selective ORF5
   positions, protected prebind, target security, audit, lifecycle state,
   cancellation, and first live canonical result.

Each implementation commit changes one to three files, uses a signed
Conventional Commit, and keeps the repository buildable. Normal format, strict
Clippy, rustdoc, diff, similarity, workspace, protocol, frame, security, and
live PostgreSQL gates remain required as their owned surfaces change.

## Deferred surface

This decision does not implement `sys.invoke`, target resolution, argument
binding, defaults, target execution, presentation selection, `--explain`,
`orna invoke`, stdout/stderr adaptation, CLIENT execution, artifact transfer,
runtime selection, UI, progress, trace, Inspector storage, gateway exposure,
remote transport, TCP, TLS, or general application collection parameters.

It does not define a canonical Timestamp runtime value, full structured
Failure value, planner candidate or selected-plan body, capability body,
runtime-offer decision body, artifact metadata body, UI patch, byte channel,
or trace body. It does not use the diagnostic JSON schemas as wire authority.
Those schemas remain documentation and Inspector-export proposals.

## Precedence

This decision implements Step 9 of work ADR 0036 and the first carrier part of
spec ADR 0004. It resolves the `CURRENT PROPOSAL TYPES` in
`spec/api/sys-invoke.md` only for the exact three version-1 carrier contracts
and first event family above.

For these three identities and payloads, it extends work ADRs 0034, 0036, 0039,
and 0042. Work ADR 0039 remains authoritative for every retained ORV5 and ORF5
byte, descriptor, bound, error precedence, and closed ordinary frame position.
Work ADR 0042 remains authoritative for the stable `sys.invoke` function
identity and trusted system entry. The canonical specification remains
authoritative outside this accepted implementation boundary.
