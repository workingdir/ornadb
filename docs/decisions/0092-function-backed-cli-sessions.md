# ADR 0092: Function-Backed CLI Sessions

**Status:** Accepted design direction

## Decision

The `orna` binary has two lifetimes:

* `orna --daemon` runs the embedded host in the foreground for systemd or a
  container supervisor; and
* every other invocation is a client session attached to one endpoint.

The no-command client form invokes the source-authored standard
`std.cli.repl` CLIENT function. `orna repl` is an explicit spelling of the
same target. The function owns the session behaviour in `.orna`, including
input handling and command evaluation; the host supplies only the language
execution and transport primitives that the source contract requires.

One-shot function calls use the same client path:

```text
orna --db orna://db.example.test/work invoke tasks.overdue
orna --db orna://db.example.test/work invoke studio.main
```

The client constructs one typed `sys.invoke` request, advertises its caller
facts and runtime offers, then consumes the bounded event stream. UI-returning
functions are rendered by a client-owned installed runtime. The database does
not send a library path or silently install code. Runtime actions and input
create child invocations under the root session and use the same authenticated
transport.

The endpoint forms are:

```text
PATH                         managed local database
orna+unix:///path/orna.sock  explicit local Orna socket
orna://local/default         managed local instance
orna://HOST[:PORT]/DATABASE  remote Orna protocol over TLS
```

Credentials are never URI fields. Remote auth belongs to the transport
handshake or an explicit protected input mechanism. `orna://local/<instance>`
is the only local authority form under the `orna` scheme. Other authorities
are remote. Unknown query parameters, fragments, credentials, and empty
DATABASE paths fail before a connection attempt.

The public help surface is intentionally small: the default session, `invoke`,
`inspect`, `source check|apply`, daemon mode, help, and version. Backend shell,
raw recovery, runtime metadata, security administration, and state repair remain
available behind explicit administrative/recovery paths but are not the normal
user workflow.

Daemon output is Orna-owned status and error output. The linked storage
engine's routine startup and shutdown messages do not reach the terminal.
They are captured in a bounded, private diagnostic log for failure analysis.

## Implementation boundary

The existing `ProtocolConnection`, constructed `CallInvokeRequest`, sealed
request carrier, event batches, resource transport, cancellation, and local
peer authentication are authoritative building blocks. The in-process
`SharedInvokeBroker` remains a test/local seam only.

The next implementation slices must:

1. extract a shared endpoint and client-session model from the CLI;
2. route local daemon calls through the authenticated local socket instead of
   opening an in-process broker from the command dispatcher;
3. add the versioned remote handshake and TLS transport before accepting
   remote invocation;
4. add a persistent runtime/session owner for terminal and graphical surfaces;
5. add source-level input and dynamic invocation primitives, then implement
   `std.cli.repl` as ordinary `.orna` code;
6. keep Qt and other genuinely host-only rendering operations behind explicit
   runtime boundaries;
7. keep raw-call as an explicit bounded recovery path.

No slice may claim remote execution, arbitrary CLIENT artifact execution, or
interactive action delivery until its transport, trust, and lifecycle tests
exist.

## Evidence

* `spec/docs/13-invocation-system.md` defines typed root invocation,
  presentation negotiation, UI/runtime offers, and cancellation.
* `spec/docs/27-wire-protocol.md` defines session authentication, bounded
  event channels, artifact verification, and the remote TLS target.
* GNU Emacs uses a persistent server and thin clients with local sockets,
  named sessions, and text/graphical frames:
  <https://www.gnu.org/software/emacs/manual/html_node/emacs/Emacs-Server.html>.
* SQLite and PostgreSQL show the useful interactive-shell conventions: one
  default session, script input, explicit output modes, and separate shell
  commands:
  <https://sqlite.org/cli.html#starting> and
  <https://www.postgresql.org/docs/current/app-psql.html>.
* Historical Java applet/WebStart documentation supports explicit code
  identity, manifest/digest binding, sandbox limits, and user-visible trust;
  it is not a deployment model for Orna:
  <https://docs.oracle.com/javase/tutorial/deployment/applet/security.html> and
  <https://docs.oracle.com/en/java/javase/11/migrate/index.html>.
