# ADR 0079: CLIENT Action Values and `std.action.call`

**Status:** Accepted

## Decision

Implement the first executable action surface as the transient standard-library
value type `std.action.Action` in an append-only `orna.std/6` snapshot.

The source constructor is:

```sql
std.action.call(
    target => tasks.rebuild,
    arguments => std.call.args(p_owner => owner)
)
```

This constructor creates a typed action value. It does not execute the target.
The action value is submitted only when the CLIENT program explicitly triggers
it through the runtime action boundary.

The first implementation accepts one call node. The forms
`std.action.sequence(...)` and `std.action.parallel(...)` remain reserved for a
later scheduler contract. The implementation must reject those forms rather
than encode an unrecognised action node.

## Scope and precedence

This ADR is authoritative for the accepted executable v1 action boundary. Any
broader conceptual action language in the canonical resources/actions/streams
specification or in ADR 0077 is bounded by this decision: `std.action.call`
only is accepted, while `std.action.sequence(...)` and
`std.action.parallel(...)` are reserved and rejected until a later scheduler
contract.

## Catalogue and source snapshot

V6 retains every V5 source unit and adds `std/action.orna`:

```sql
CREATE SCHEMA std.action;

CREATE TYPE std.action.Action AS VALUE
    OPAQUE
    KERNEL CONTRACT 'orna.std.value.action@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.action.Action AS std.Action;
```

The V6 identities are:

| Item | Identity |
| --- | --- |
| Standard revision | `...06` |
| Catalogue revision | `...06` |
| Source bundle | `...06` |
| Source revision | `...06` |
| `std/action.orna` source unit | `...07` |
| `std.action` schema | `...09` |
| `std.action.Action` type | `...20` |

The value representation contract is `orna.std.value.action@1`. Its canonical
payload begins with `ORNA-ACTION/1 ` and uses the existing bounded,
length-prefixed opaque-value framing rules.

## Checked action contract

`std.action.call` is checked as a CLIENT expression. The checked node retains:

- target domain: `CLIENT` or `SERVER`;
- target `FunctionId`;
- target `RevisionPair` containing the pinned source and catalogue revisions;
- target result `TypeId`;
- call-site identity;
- canonical argument pairs sorted by declared `ParameterId`;
- the checked argument expression for each target parameter.

The target must be a declared function. The compiler rejects an unknown target,
an action target with an unsupported domain, an argument count mismatch, an
unknown or repeated parameter, an argument type mismatch, and a target whose
revision or result type does not match the checked catalogue.

An action value is parameterised by the target result type in the checked
artifact. It does not add a generic runtime catalogue type. The target result
is used to validate the pinned call and the executor completion; the action
completion is an outcome, not a CLIENT expression value.

The action artifact contains no principal, role, capability grant, transport
handle, or `run_as` value. The runtime derives those values from the
authenticated invocation context when it triggers the action.

## Artifact and value encoding

The client plan gains a new version for action nodes. Existing plan versions
remain decodable and unchanged. The action node encodes the fields above and
uses the canonical ORV3 argument frames for evaluated argument values.

The opaque action payload uses a separate `ORNA-ACTION/1` frame. It contains a
canonical call descriptor with the target domain, target identity, pinned
revision pair, call-site identity, target result type, and sorted argument
pairs. Decoders reject truncation, trailing bytes, invalid identities, invalid
revision pairs, unsorted or repeated parameters, invalid argument frames, and
unknown action tags.

The payload is immutable and transient. It cannot be persisted as a catalogue
object or used to smuggle execution authority across invocations.

## Trigger and completion

The runtime exposes one action trigger boundary. Triggering an action:

1. allocates a fresh invocation identity and action generation;
2. validates the action type, payload, target identity, pinned revisions, and
   argument digest;
3. derives the current principal, grants, and authenticated invocation context;
4. dispatches a `CLIENT` target through the local CLIENT evaluator or submits a
   `SERVER` target through the existing authenticated resource transport;
5. maps the target result to a structured action outcome without returning the
   target result as a CLIENT value.

The action boundary does not use a result cache. A trigger always creates a new
request identity. Cancellation, shutdown, stale generations, and executor
failures produce terminal outcomes and cannot update a later action generation.

The transport request contains the target and canonical arguments only. It does
not contain caller-supplied authority. Server authorisation remains inside the
sealed `sys.invoke` path.

The runtime action outcome has these terminal forms:

- `Completed`;
- `Failed` with a redacted public error;
- `Cancelled`.

An executor must not report both a terminal outcome and a later completion for
the same action generation.

## Required proof

The implementation requires focused tests for:

1. V6 identity, retained V5 content, source origins, and digest goldens;
2. parser acceptance of `std.action.call` and rejection of reserved sequence and
   parallel forms;
3. compiler target-domain, revision, result-type, parameter, and argument checks;
4. action-plan encoding and decoding, including malformed and non-canonical
   payloads;
5. action-value construction with canonical ORV3 arguments;
6. local CLIENT triggering and authenticated SERVER triggering;
7. fresh request identities, no cache reuse, cancellation, shutdown, stale
   completion, and redacted failure outcomes;
8. an installed-server proof showing that a SERVER action remains behind the
   authenticated `sys.invoke` boundary.

## Deferred surface

This ADR does not define sequence or parallel scheduling, automatic retries,
stream actions, graphical event bindings, action persistence, a new transport
protocol, or reflective gateway exposure metadata. Those surfaces require a
separate accepted contract.

## Precedence

This ADR implements the action portion of spec ADR 0017 and the action language
contract in work ADR 0077. Work ADRs 0060, 0068, 0071, 0073, 0074, 0075, 0076,
0077, and 0078 remain authoritative for CLIENT capabilities, expressions,
resource identity, value transport, runtime conformance, target language, and
server transport. The sealed `sys.invoke` boundary remains authoritative for
server execution and security.
