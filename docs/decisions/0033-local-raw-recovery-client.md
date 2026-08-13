# ADR 0033: Local Raw Recovery Calls Use Stable Function Identities

**Status:** Accepted

## Decision

The first production client for the raw protocol is a local recovery command:

```text
orna raw-call <canonical-function-id>
```

The argument is exactly one canonical `FunctionId`, including its
`function:` prefix. The command accepts no function name, argument, option,
configuration, environment selector, SQL, or presenter. It connects only to
`/run/orna/default/orna.sock` and makes one parameter-free protocol-1 raw
call. This completes the local client side of milestone 4 without defining
`sys.invoke` policy.

The canonical examples use `orna raw-call sys.catalog.health`, but function
name resolution and the mandatory `sys.catalog.health` definition do not yet
exist. This decision does not silently invent either one. A later accepted
decision may add name resolution while retaining canonical function identity
as the fail-closed recovery form.

## Fixed local authority

The command has no instance flag or socket override. It uses the first-release
instance and fixed public socket selected by ADR 0028. Before connecting, it
requires `/run/orna/default` to be a directory and `orna.sock` to be one Unix
socket with no extra hard link. It does not inspect the private PostgreSQL
socket, configuration, readiness file, environment, current directory, home
directory, or `PATH`.

The server authenticates the connected operating-system peer. The command
does not send a principal, credential, role, or database authority. A silent
close during negotiation is an authentication or connection failure, not an
invitation to use another endpoint.

## One closed call

The client sends the exact protocol-1 hello. After the exact acknowledgement,
it opens stream 1 with the supplied `FunctionId`, grants the maximum accepted
`RESULT_VALUES` window, and declares the empty argument list complete. It
sends no other output-channel credit. Credit is an accounting bound and does
not allocate a matching client buffer. The client reads and writes at most one
bounded frame at a time.

The server must first return `CALL_ACCEPTED` for stream 1. It may then return
zero or more non-empty `RESULT_VALUES` event batches with contiguous sequence
numbers beginning at 1, followed by exactly one of:

* `CALL_COMPLETED`;
* `CALL_FAILED` with one closed public failure; or
* `CALL_CANCELLED` after client cancellation.

Every accepted event must be `Event::Value` and use protocol-1 `ORV1` bytes.
Any other channel, event kind, stream, sequence, order, marker, duplicate
terminal frame, frame after a terminal frame, malformed frame, oversized
frame, premature end of file, or unknown future value is a protocol failure.
The reusable protocol crate, not the CLI adapter, owns this client response
state validation.

The client does not recover a catalogue or active revision. ADR 0032 already
limits raw CLIENT and SERVER results to the protocol-1 value subset. Protocol
2 and 3 remain available to catalogue-aware clients but are not selected by
this recovery command.

## Output and cancellation

For each validated value event, the command writes the complete
self-delimiting `ORV1` value envelope to standard output in event sequence.
It writes no acknowledgement, acceptance, stream metadata, completion marker,
human label, JSON, separator, or trailing newline. Zero values produce empty
standard output. The 25-byte value header carries the marker, tag, `TypeId`,
and payload length, so concatenated envelopes remain lossless and parseable.

Human diagnostics go only to standard error. A call failure names only the
closed public `CallFailure`; connection and protocol diagnostics disclose no
principal, function-existence fact, catalogue, revision, SQL, PostgreSQL
message, filesystem fallback, or server-private source.

`SIGINT` after connection sends one `CALL_CANCEL` for stream 1. The client then
drains the bounded connection until `CALL_CANCELLED` or an already committed
terminal result. A second interruption closes the connection and returns the
cancelled status. Cancellation never changes an already emitted value or
hides a protocol failure. A standard-output write failure attempts the same
single cancellation and bounded drain before returning an internal failure.

## Exit status

The command uses this closed status set:

```text
0  CALL_COMPLETED
1  CALL_FAILED
2  invalid command or FunctionId
3  socket, connection, negotiation, or peer-authentication failure
6  CALL_CANCELLED or second interruption
7  frame, state, input/output, or other internal protocol failure
```

The server's closed `CallFailure` does not change status 1 into an additional
public exit-code vocabulary. The diagnostic retains its exact failure name.
No partial success returns status 0.

## Required proof

Tests must prove:

* only one exact canonical `FunctionId` command shape is accepted;
* the fixed socket is the sole endpoint and hostile environment, current
  directory, stdin, and extra command tokens have no authority;
* hello, acknowledgement, stream, empty arguments, and result credit have
  exact bytes and order;
* zero, one, and multiple values preserve complete `ORV1` envelopes and event
  order on stdout with empty stderr and status 0;
* every public call failure returns status 1 with no value fabrication or
  private diagnostic;
* fragmented reads and short writes preserve the same result;
* wrong stream, channel, event, sequence, marker, order, terminal frame,
  length, EOF, and future value fail closed through the reusable client state;
* flow control never creates an unbounded client allocation;
* `SIGINT`, a peer close, and a stdout failure follow the exact cancellation,
  drain, and exit rules; and
* an actual installed local server authenticates the operating-system peer,
  executes one permitted parameter-free raw call, denies a revoked call, and
  leaves no client or server connection task behind.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
protocol, and live local-socket gates remain required.

## Implementation sequence

1. Accept and index this local recovery boundary.
2. Add a protocol-owned raw-call client response state that validates accepted
   events and one terminal outcome without changing server-side bytes or
   state.
3. Add a deep fixed-socket client module and the narrow CLI adapter with unit,
   hostile-environment, fragmented-I/O, cancellation, and live server proof.

Each implementation commit changes one to three files, uses a signed
conventional commit, and keeps the repository buildable.

## Deferred surface

This decision does not define a function-name resolver,
`sys.catalog.health`, `sys.invoke`, arguments, defaults, enum or record raw
results, protocol selection, remote transport, TLS, presenters, JSON, terminal
rendering, configuration, instance selection, tracing, deadlines, or public
audit inspection.

## Precedence

This decision consumes ADRs 0025, 0026, 0028, and 0032. It narrows the open
recovery-command examples in the canonical CLI and bootstrapping proposals to
one implementable identity-based form. It does not change the locked concept
that ordinary root invocation later enters through `sys.invoke`.
