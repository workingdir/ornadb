# ADR 0028: The Local Raw Socket Negotiates Before Protected Dispatch

**Status:** Accepted

## Decision

The first public Orna transport is one Unix-domain listener at
`/run/orna/default/orna.sock`. It is created by `orna server run` only after
the private PostgreSQL kernel and accepted standard library are ready. The
runtime directory has mode `0711`: local users can traverse an exact known
socket path but cannot list the private ready, PostgreSQL, or support paths.
Those child paths retain their existing `0600`/`0700` contracts. The public
socket has mode `0666`, is owned by the Orna service identity, and is removed
before the postmaster stops. Socket mode grants only permission to attempt a
connection; `SO_PEERCRED` and the protected credential mapping remain the sole
authentication authority. While holding the exclusive instance lock, startup
may remove one stale socket only after `lstat` proves exact socket type, service
owner and group, mode `0666`, and link count one; it then synchronises the
runtime directory before bind. A link, non-socket, hostile owner, wrong mode,
bind failure, listener failure, or removal failure is a server failure.

The listener accepts at most 64 simultaneous connections. A 65th connection
is closed without reading. Each connection retains the ADR 0026 limit
of 64 live calls. The listener also owns one shared 256 MiB budget for declared
client-frame payload bytes that are being read or retained by calls. A worker
must reserve the complete declared payload before it allocates or reads that
payload. If the reservation is unavailable, it closes the connection without
reading the payload or changing `ProtocolConnection` state. Temporary frames
release their reservation after application. Argument reservations remain
owned by their stream and then by its accepted dispatch until protected kernel
work finishes, including during connection drain. No environment value,
request byte, client-supplied path, PostgreSQL role, or socket filename selects
a principal or database.

The listener owns 64 kernel-operation permits across all connections.
Authentication must reserve one before it opens PostgreSQL and hold it until
its protected decision commits and its session closes. Before a worker applies
a decoded `CALL_ARGUMENTS_COMPLETE`, it must reserve one permit. If none is
available, authentication closes without an ACK, or dispatch closes the
connection without applying the frame or producing a response. The
adapter-owned dispatch task retains the permit and the stream's retained-byte
reservation alongside `RawClientDispatch` until `finish` returns, even if the
connection closes. This bounds all simultaneous PostgreSQL-opening transport
work to 64.

## Version and authentication handshake

One connection begins with one exact twelve-byte client hello:

```text
offset  size  value
0       4     ASCII `ORNA`
4       1     `0x01`, client HELLO
5       1     flags, exactly zero
6       2     protocol major, unsigned big-endian, exactly 1
8       2     protocol minor, unsigned big-endian, exactly 0
10      2     reserved, exactly zero
```

The adapter reads no more than these bytes before it validates the hello. A
short read, extra interpretation, wrong magic, tag, flags, version, or
reserved value closes the connection without a response.

After a valid hello, the adapter authenticates the actual connected stream
through ADR 0023 `SO_PEERCRED`. Authentication must append and commit its
protected ADR 0024 decision and close its PostgreSQL session. Failure closes
the connection without a response or reusable credential.

Only successful authentication produces the exact twelve-byte server ACK. It
has the same bytes except offset 4 is `0x81`. The ACK means version 1.0 was
selected and an empty-role `AuthenticatedSession` is bound to the connection.
There is no request principal, role list, token, cookie, or session identity on
the wire.

The hello must complete within five seconds. After the ACK, the current
bounded Boolean transport closes a connection that supplies no complete next
frame for thirty seconds. These limits are transport resource bounds, not
function deadlines. Later streaming work must replace the post-ACK idle bound
before it admits legitimately idle long-lived calls.

## Framed connection driver

After the ACK, every byte is an exact ADR 0026 ORF1 client frame. The adapter
reads the fixed 18-byte header first, rejects a declared payload above the
public protocol maximum before allocation, then reads exactly that payload.
EOF between frames closes normally. EOF inside a frame, timeout, I/O failure,
codec error, state error, server-action error other than insufficient credit,
or an unexpected dispatch-task failure closes the connection.

One `ProtocolConnection` owns all state for the stream. All decoded client
frames and all server actions pass through it serially. Immediate PONG and
pre-dispatch cancellation frames are applied and written directly. One
complete dispatch action creates `RawClientDispatch` with the connection's
trusted session, applies and writes its fresh `CALL_ACCEPTED`, and schedules
`finish` without accepting another identity input.

At most 64 dispatch futures are live across the listener because each future
holds one shared kernel-operation permit; concurrent authentication reduces
the permits available for dispatch. They may complete in any order. The
connection driver serialises
their actions through the same state machine. It retains at most the one
returned value action per completed Boolean call while that stream lacks
`RESULT_VALUES` credit. A later `WINDOW_UPDATE` retries the unchanged action.
Terminal actions require no credit.

If the peer closes, violates the protocol, a write fails, or the server begins
shutdown, the adapter stops reading but continues every accepted `finish`
future to completion. It then discards actions that cannot be delivered and
closes the stream. It never aborts or drops protected kernel work.

## Cancellation

Cancellation in `RECEIVING_ARGUMENTS` stays the immediate ADR 0026 outcome.
Cancellation after dispatch records one pending cancellation for that stream.
It does not abort, detach, or drop the dispatch future.

When the future finishes, an operational kernel failure is applied as
`CALL_FAILED(INTERNAL_FAILURE)` even when cancellation is pending. Otherwise
the driver discards any unsent value, completion, expected denial, or evaluator
failure and applies one `CALL_CANCELLED`. A cancellation received before the
spawned `finish` future is first polled still leaves that future scheduled and
must produce the required durable audit or operational failure before the
stream closes.

## Listener lifecycle

The Orna supervisor forks the linked postmaster before it creates the listener
or any connection thread. Later PostgreSQL role forks occur inside that
separate postmaster process. Connection workers may therefore use threads and
current-thread asynchronous runtimes without violating the Orna supervisor's
linked-engine single-threaded fork boundary.

The server readiness record and systemd `READY=1` notification are published
only after the listener is bound and its mode, owner, and socket type are
reverified. Foreground supervision checks both the postmaster and listener.
Shutdown stops acceptance, asks every connection to drain protected work,
joins every worker, removes and synchronises the public socket, and only then
stops PostgreSQL. A listener or worker infrastructure failure removes
readiness and triggers the same ordered shutdown.

## Interface and failures

`orna-server` owns a deep local-transport module. Its public start operation
accepts only the fixed runtime directory and a cloned `PostgresKernel`; it
does not accept a socket path, UID, principal, roles, protocol limits, or frame
handler. The returned server handle exposes health and ordered stop operations.

Authentication and call outcomes remain typed private sources for trusted
logs. No error text crosses the socket. Version, authentication, I/O, codec,
state, adapter, and infrastructure failures close the connection. An accepted
call uses only the four closed ADR 0026 failure values.

## Required proof

Tests must prove:

* exact HELLO and ACK bytes, and silent close for every malformed or unsupported
  hello;
* no ACK for an unmapped, disabled, role, or otherwise rejected peer, with the
  exact protected authentication audit and no session leak;
* the socket path, type, mode, owner, absence-before-bind, cleanup, fixed path,
  connection limit, and listener health contract;
* the systemd unit and production host use runtime-root mode `0711`, while
  every private ready, PostgreSQL, and support child retains its required
  `0600` or `0700` mode;
* fragmented headers and payloads are reassembled, while oversized declarations,
  truncation, wrong-direction frames, malformed values, and state violations
  close without unbounded allocation or a text diagnostic;
* the shared 256 MiB payload budget and 64-operation limit are exact across
  connections, include authentication and dispatch, reject admission before
  payload allocation and protocol mutation, follow retained arguments into
  accepted dispatch work, and return every reservation on all success, denial,
  cancellation, protocol, I/O, and shutdown paths;
* PING/PONG, raw-call assembly, monotonic streams, window credit, accepted
  identity, value sequence, completion, closed denials, and independent
  interleaved streams use the exact ADR 0026 bytes;
* the actual Unix peer establishes the session and cannot supply a principal or
  role in the hello or raw call;
* cancellation before a dispatch future's first poll still completes the
  kernel decision and audit, clean outcomes become `CALL_CANCELLED`, and an
  audit, transaction, driver, or shutdown failure remains `INTERNAL_FAILURE`;
* peer EOF, protocol failure, write failure, listener shutdown, and process
  shutdown drain all accepted kernel work and leak no PostgreSQL session,
  socket, worker, or retained stream; and
* the production foreground server reports ready only after the public socket
  is verified and removes it before the linked postmaster stops.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
live PostgreSQL, and production lifecycle gates remain required.

## Implementation sequence

1. Accept this exact local transport and lifecycle boundary.
2. Expose the protocol payload maximum and add the deep socket driver with
   direct non-live framing, limit, flow-control, and cancellation tests.
3. Add focused live authentication, dispatch, audit, failure, and connection
   draining tests.
4. Integrate the fixed listener into foreground embedded-server readiness and
   shutdown; change the systemd runtime-directory mode to `0711`; preserve the
   private child modes; then prove the production lifecycle.

Each commit changes one to three files and keeps the repository buildable.

## Deferred surface

This decision does not define TCP, TLS, passwords, external authentication,
role selection, delegation, durable sessions, minor-version features,
capability offers, `sys.invoke`, general argument binding, SERVER dispatch,
artefact transfer, item windows, compression, deadlines, or long-lived
streaming calls.

## Precedence

This composes ADRs 0023, 0024, 0026, and 0027 for the first authenticated local
transport. It implements the local-socket and version-negotiation foundation
of milestone 4 for the current parameter-free Boolean CLIENT subset. It does
not mark TCP/TLS, `sys.invoke`, general invocation, richer values, or general
event streaming complete.

For the public socket to be reachable, this decision supersedes only ADR
0019's `0700` mode for `/run/orna/default` with `0711`. Every private child
path keeps its accepted `0600` or `0700` mode and remains absent from directory
listings by unprivileged users.
