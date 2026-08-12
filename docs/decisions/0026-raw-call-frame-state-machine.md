# ADR 0026: Raw Calls Use a Bounded Framed State Machine

**Status:** Accepted

## Decision

The first protocol state machine carries raw function calls after an adapter
has established an authenticated session. It is transport-independent and
lives in `orna-protocol`. Tests drive the same typed machine that a later Unix
socket adapter will use.

The protocol does not reuse PostgreSQL messages. A client frame cannot select
a session principal, effective principal, active role, credential, delegation,
or database-owner override. A principal reference remains valid as an ordinary
typed function argument and receives normal function-level authorisation. The
authenticated adapter supplies `AuthenticatedSession` when it dispatches a
complete call. The state machine does not authorise or execute a function.

Version 1 multiplexes calls by a non-zero unsigned 64-bit stream number. The
stream number is connection-local correlation data, not an `InvocationId` and
not a durable Orna identity. Each `CALL_RAW_START` must use a stream number
larger than every stream number that the client previously started on that
connection. This single high-water mark rejects reuse without retaining every
completed stream. No new call can start after stream `u64::MAX`. Stream zero is
reserved for connection control.

A connection retains at most 64 live call streams. One call retains at most
256 arguments and at most 16 MiB plus 64 bytes across all complete
`CALL_ARGUMENT` payloads. A frame that would exceed one of these limits is a
protocol violation and changes no state. A terminal transition removes the
live stream record but does not decrease the stream-number high-water mark.

## Frame envelope

Every frame has this exact byte sequence:

```text
offset  size  field
0       4     ASCII `ORF1`
4       1     frame tag
5       1     flags, exactly zero in version 1
6       8     stream number, unsigned big-endian
14      4     unsigned payload length, big-endian
18      n     payload
```

The maximum payload is 16 MiB plus 64 bytes. This admits one maximum-size
canonical value plus its call or event metadata. The decoder checks the
declared size before reading or allocating a payload. It rejects a bad marker,
unknown tag, non-zero flags, invalid stream number, oversized payload,
truncation, trailing bytes, and a tag-specific payload error.

Tags are direction-specific and closed:

```text
client to server
0x01  CALL_RAW_START
0x02  CALL_ARGUMENT
0x03  CALL_ARGUMENTS_COMPLETE
0x04  WINDOW_UPDATE
0x05  CALL_CANCEL
0x06  PING

server to client
0x81  CALL_ACCEPTED
0x82  EVENT_BATCH
0x83  CALL_COMPLETED
0x84  CALL_FAILED
0x85  CALL_CANCELLED
0x86  PONG
```

`PING` and `PONG` use stream zero and carry one opaque eight-byte token. All
other frames require a non-zero stream. A decoder for one direction rejects
every tag from the other direction.

## Client payloads

`CALL_RAW_START` contains exactly one sixteen-byte `FunctionId`.
`CALL_ARGUMENT` contains one sixteen-byte `ParameterId` followed by one
complete ADR 0025 value. A stream cannot contain the same `ParameterId` twice.
The dispatch action orders arguments by ascending `ParameterId` bytes, so
client frame order does not become invocation authority.
`CALL_ARGUMENTS_COMPLETE` and `CALL_CANCEL` have empty payloads.

`WINDOW_UPDATE` contains one channel byte followed by a non-zero unsigned
64-bit credit increase. Addition is checked. A window cannot exceed 1 GiB.
The closed channels are:

```text
0x01  RESULT_VALUES
0x02  RESULT_BYTES
0x03  DIAGNOSTIC
0x04  PROGRESS
0x05  TRACE
0x06  CLIENT_CONTROL
```

## Server payloads

`CALL_ACCEPTED` contains exactly one server-generated sixteen-byte
`InvocationId`. It is the first server frame for an accepted stream. A
dispatch can instead fail before acceptance and produce `CALL_FAILED` as its
first and terminal server frame.

`EVENT_BATCH` contains one channel byte, a non-zero unsigned 16-bit event
count, then that many entries:

```text
8 bytes  event sequence, unsigned big-endian
1 byte   event kind
4 bytes  content length, unsigned big-endian
n bytes  content
```

Sequence starts at one and is contiguous across all channels on a stream. A
batch contains one channel only. Version 1 admits these event/channel pairs:

```text
0x01  canonical value     RESULT_VALUES
0x02  byte chunk          RESULT_BYTES
0x03  diagnostic failure  DIAGNOSTIC
```

A canonical-value event contains exactly one ADR 0025 value. A byte chunk is
uninterpreted and non-empty. A diagnostic failure has this closed payload:

```text
1 byte   failure category
2 bytes  failure code, unsigned big-endian
1 byte   retry policy
```

Version 1 admits only these complete failure values:

```text
category  code    retry  meaning
0x01      0x0001  0x00   EXECUTE_DENIED
0x02      0x0001  0x00   TARGET_UNAVAILABLE
0x03      0x0001  0x00   CLIENT_EVALUATION_FAILED
0xff      0x0001  0x00   INTERNAL_FAILURE
```

Category `0x01` is `AUTHORIZATION`, `0x02` is `TARGET`, `0x03` is
`CLIENT_EVALUATOR`, and `0xff` is `INTERNAL`. Retry policy `0x00` is `NEVER`.
Every other category, code, retry value, or combination is invalid. All
`ExecuteDenial` variants map to the same `EXECUTE_DENIED` value. This prevents
function, grant, role, session, and revision discovery through the public
protocol. The protocol carries no arbitrary error text, PostgreSQL error,
source text, or unredacted security detail. The server adapter owns the later
closed mapping from typed kernel errors to these four values. A later protocol
version must define any retryable failure before it can use a new retry value.

`CALL_COMPLETED` and `CALL_CANCELLED` have empty payloads. `CALL_FAILED`
contains exactly the same structured diagnostic-failure payload. A terminal
frame is not subject to a byte window, so a peer cannot deadlock cleanup by
withholding credit.

## State machine

One stream follows this exact state sequence:

```text
ABSENT
  -- CALL_RAW_START --> RECEIVING_ARGUMENTS

RECEIVING_ARGUMENTS
  -- CALL_ARGUMENT* ----------> RECEIVING_ARGUMENTS
  -- WINDOW_UPDATE -----------> RECEIVING_ARGUMENTS
  -- CALL_ARGUMENTS_COMPLETE + dispatch --> DISPATCHING
  -- CALL_CANCEL / CALL_CANCELLED --> CANCELLED

DISPATCHING
  -- WINDOW_UPDATE -----------> DISPATCHING
  -- CALL_ACCEPTED -----------> RUNNING
  -- CALL_FAILED -------------> FAILED
  -- CALL_CANCEL + cancel ----> DISPATCH_CANCELLING

DISPATCH_CANCELLING
  -- WINDOW_UPDATE -----------> DISPATCH_CANCELLING
  -- CALL_CANCELLED ----------> CANCELLED
  -- CALL_FAILED -------------> FAILED

RUNNING
  -- WINDOW_UPDATE -----------> RUNNING
  -- EVENT_BATCH -------------> RUNNING
  -- CALL_CANCEL + cancel ----> RUNNING_CANCELLING
  -- CALL_COMPLETED ----------> COMPLETED
  -- CALL_FAILED -------------> FAILED

RUNNING_CANCELLING
  -- WINDOW_UPDATE -----------> RUNNING_CANCELLING
  -- EVENT_BATCH -------------> RUNNING_CANCELLING
  -- CALL_CANCELLED ----------> CANCELLED
  -- CALL_COMPLETED ----------> COMPLETED
  -- CALL_FAILED -------------> FAILED
```

`COMPLETED`, `FAILED`, and `CANCELLED` are terminal. The server adapter receives
one cancellation action when either `DISPATCHING` or `RUNNING` moves to
its corresponding cancelling state. The action identifies the stream and
carries the `InvocationId` only after acceptance. The adapter must serialise
its actions and client input through this state machine. If cancellation wins
before acceptance, a later acceptance is an adapter error and changes no state.
Repeated cancellation, a frame in the wrong state, a server event before
acceptance, an event after a terminal frame, a duplicate argument, a
non-contiguous event sequence, or reuse of a stream number is a protocol
violation.

The arrows labelled with client frame names are client inputs. `CALL_ACCEPTED`,
`EVENT_BATCH`, and terminal frames are server actions. A cancel while arguments
are still being received does not call the server adapter. It produces one
unwindowed `CALL_CANCELLED` frame. A dispatch action contains the complete raw
call. The state moves from `DISPATCHING` to `RUNNING` only when the adapter
supplies an `InvocationId` and applies `CALL_ACCEPTED`.

`PING` is a connection-control input in every connection state. It produces
one `PONG` with the same token and does not read or change call state. `PONG`
is never valid as client input.

Before the server emits an `EVENT_BATCH`, the selected channel must have
credit for the complete frame payload. The state machine subtracts that exact
payload length. It never emits a partial batch and never permits a negative
window. Every channel starts with zero credit. Windows are independent for
each stream and channel. Control and terminal frames do not consume window
credit.

The first event sequence is one. The machine checks the last sequence plus the
batch count before it emits a batch. A batch can contain event `u64::MAX`, but
the stream cannot emit another event after that. Sequence exhaustion and
arithmetic overflow are server-action errors and change no state or credit.

## Interface and failure boundary

The module exposes closed client and server frame types, exact encode/decode
operations, and one connection state machine. Receiving a valid client frame
returns zero or one typed action: dispatch one complete call, cancel one
dispatched call, emit one immediate cancelled outcome, or answer one ping.
Applying a server action returns the one server frame that may be encoded and
sent.

Frame decoding failures and state violations are typed protocol errors. A
transport adapter closes the connection after such an error. Function denial,
execution failure, and cancellation are call outcomes and use server frames;
they are not protocol violations.

## Required proof

Tests must prove:

* exact golden bytes and round trips for every client and server frame;
* direction, flag, stream-zero, payload-size, fixed-length, truncation, and
  trailing-byte rejection;
* canonical value errors remain typed frame errors with their source;
* 64 interleaved streams retain independent arguments, state, event sequence,
  and channel windows, while a 65th live stream fails without state change;
* stream-number monotonicity and exhaustion reject reuse with one bounded
  high-water mark;
* the argument-count and retained-byte limits admit their exact maxima and
  reject one unit more without state change;
* argument duplication, stream reuse, and every invalid transition fail
  closed without changing prior state;
* dispatch contains the exact function and unique typed arguments but no
  caller-selected authentication or authorisation context outside those
  ordinary typed arguments;
* event batches cannot exceed credit and consume the exact selected window;
* credit addition and frame-size arithmetic cannot overflow;
* cancellation before dispatch is immediate, while cancellation during
  dispatch or execution produces one adapter action and accepts exactly one
  terminal outcome;
* event sequence `u64::MAX` is valid once and makes another event fail closed;
* a terminal frame needs no window and forbids every later call frame; and
* property tests over arbitrary bytes and arbitrary typed frame sequences never
  panic or produce an invalid state.

Normal format, strict Clippy, rustdoc, diff, similarity, and workspace test
gates remain required.

## Implementation sequence

1. Accept this exact raw-call framing and state model.
2. Add frame types, exact codecs, and the connection state machine to
   `orna-protocol` with direct and property tests.
3. Define the protected kernel raw-call dispatcher that binds the adapter's
   authenticated session, authorises the pinned function, and maps typed
   results or failures to server actions.
4. Add a Unix-socket adapter with version negotiation and local-peer session
   authentication.

Each commit changes one to three files and keeps the repository buildable.

## Deferred surface

This decision does not define `sys.invoke`, presenter events, progress, trace,
CLIENT control, artefact transfer, table batching, TCP, TLS, deadlines,
delegation, active-role selection, compression, unknown-field skipping, or
minor-version feature negotiation. The reserved channels cannot carry events
until a later decision assigns exact bytes. Stream windows are byte windows;
item windows remain deferred.

## Precedence

This decision implements the first streaming, flow-control, cancellation, raw
call, and structured-failure foundation of milestone 4. It narrows the larger
frame vocabulary in `spec/docs/27-wire-protocol.md` to the smallest complete
authenticated raw-call state machine that the current runtime and security
model can consume. It does not mark the local socket, TCP/TLS, `sys.invoke`, or
general event checklist rows complete.
