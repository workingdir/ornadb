# ADR 0014: Host-Only Backend Shell

**Status:** Accepted

## Decision

The operator command remains exactly:

```text
orna server backend-shell
```

It accepts no flag, argument, connection override, SQL argument, or input
file. Any other command shape uses the exact global usage accepted by ADR
0018 and exits `2` before another check.

The command is a local, interactive, host-only escape hatch into the private
PostgreSQL kernel. It is not an Orna source form, function, script operation,
artefact, or public-protocol operation. It does not create a PostgreSQL
compatibility promise.

The production command is an Orna-native terminal client. It does not locate,
load, extract, or execute `psql` or another PostgreSQL program. It connects to
the already-running embedded engine through the private Unix socket and uses
PostgreSQL's simple-query protocol. PostgreSQL code continues to run only from
the installed `/usr/bin/orna` ELF.

## Host and terminal boundary

Standard input, standard output, and standard error must all be terminals. A
Unix pseudo-terminal satisfies this check. If one stream is not a terminal,
the command fails before account, package, instance, environment, or socket
work.

The command runs with the caller's existing Unix identity and never calls
`sudo`, changes user or group, requests a capability, or invokes a privileged
helper. It requires the exact locked `orna:orna` service-account contract
retained by ADR 0017. Account lookup or a real, effective, primary, or
supplementary identity mismatch fails before package or instance access.

An interactive remote login can invoke this local command when the remote
account already has the service identity and all streams are terminals. This
does not create a remote administration endpoint.

The shell is absent from Orna source, functions, scripts, artefacts, and the
public protocol. No parser, catalogue definition, executable artefact,
invocation method, or public message can represent it.

## Ready-host attachment

After the terminal and identity checks, the shell takes and retains the shared
package lock and requires exact ready package state. It then verifies the
fixed default configuration and an installed default instance. It accepts
only a valid instance manifest, current generation, embedded-engine identity,
final Orna ELF identity, ready record, live server PID, held instance lock,
postmaster PID, and private socket.

The shell uses `F_GETLK` on the existing instance lock. The returned write-lock
holder must equal the ready record's live server PID. The shell does not take
the instance write lock. It holds its one package-lock descriptor for the
complete session so package maintenance cannot replace the Orna ELF or engine
while the shell is attached.

It connects only to this fixed target:

```text
socket directory  /run/orna/default/postgres
socket suffix     .s.PGSQL.5432
database          orna
role              orna_kernel
authentication    Unix peer map from orna to orna_kernel
transport         local Unix socket, no TLS or GSS
```

No environment or configuration value can change that target. The shell does
not read `ORNA_SERVER_POSTGRES_URL`, `DATABASE_URL`, `PG*`, `HOME`, `PATH`, a
service file, password file, TLS material, GSS material, Compose file, current
directory, or user startup file. It stores and sends no password.

Before connection, the command does not start or stop a service, initialise a
cluster, materialise support data, run a migration, inspect the active Orna
revision through SQL, issue a query, begin a transaction, or open a filesystem
path for writing. It only validates existing package, instance, process, and
socket facts.

## Native terminal session

The client sends PostgreSQL simple Query messages over the fixed private
connection. It does not call a backend entry directly. A direct backend cannot
join the running postmaster's shared-memory, process, signal, and connection
state, and single-user mode cannot attach to a live cluster.

The line protocol is exact:

* `orna=> ` is the prompt when the local query buffer is empty;
* `orna-> ` is the prompt when the buffer is non-empty;
* ordinary UTF-8 terminal lines are appended to the buffer with one line feed;
* a line containing only `\g` sends the complete buffer as one simple Query
  message and clears the buffer after PostgreSQL returns `ReadyForQuery`;
* `\g` with an empty buffer does nothing and prints the empty prompt again;
* a line containing only `\q` discards any unsent buffer, sends Terminate, and
  exits `0`; and
* no other backslash command exists in the normal query prompt.

There is no shell escape, `\!`, `\copy`, include, editor, pager, history file,
variable substitution, connection command, role shortcut, startup SQL, or
client-side SQL parser. A backslash sequence other than the two control lines
is ordinary SQL input.

`Ctrl-C` clears an unsent buffer and returns to the empty prompt. During an
active query it sends a PostgreSQL CancelRequest to the same fixed endpoint,
waits for the query error and `ReadyForQuery`, then returns to the empty
prompt. A failed cancellation is a session failure. `SIGHUP`, `SIGQUIT`, and
`SIGTERM` retain normal signal termination. Connection loss lets PostgreSQL
clean up the backend through normal disconnect handling.

End-of-file with an empty buffer sends Terminate and exits `0`. End-of-file
with a non-empty buffer discards it, writes the exact session-failure line,
and exits `1` without sending the incomplete SQL.

PostgreSQL statement errors render and the session continues after
`ReadyForQuery`. They do not set the eventual clean exit status. The renderer
accepts only text-format fields. A `RowDescription` that selects binary format
for any field causes the client to send Terminate and fail the session before
it renders a `DataRow`. The renderer preserves column names, text values, the
distinction between NULL and text, notices, SQLSTATE, primary message, detail,
hint, command tag, and transaction status. This rendering is private operator
presentation, not an Orna language or public protocol format.

The renderer is terminal-safe. It writes printable UTF-8 unchanged and escapes
backslash, tab, carriage return, line feed, escape, DEL, and every other
control byte. NULL uses a distinct `<NULL>` token which cannot be confused
with escaped text. Column names, row values, notices, errors, and COPY output
all use the same escaping authority, so database bytes cannot inject terminal
control sequences.

For `COPY FROM STDIN`, the client enters a `copy=> ` prompt, sends each
ordinary line as CopyData with one line feed, and treats a line containing
only `\.` as CopyDone. This is the one additional control line and exists only
at the copy prompt. `\q`, `\g`, and every other line are ordinary CopyData.
`Ctrl-C` sends CopyFail and drains responses through `ReadyForQuery` before
returning to the empty normal prompt. End-of-file sends CopyFail with the fixed
reason `Orna COPY input ended before \.`, drains through `ReadyForQuery`,
writes the session-failure line, and exits `1`. For text `COPY TO STDOUT`, the
client writes terminal-safe escaped CopyData and drains through
`ReadyForQuery`. A binary COPY response, COPY BOTH response, terminal write
failure, or protocol violation cancels where possible and is a session
failure. `COPY PROGRAM` never reaches a COPY subprotocol because the embedded
backend rejects it.

## Administration and executable boundary

The session has the raw PostgreSQL authority of `orna_kernel`. A trusted
operator can read or damage private state, including protected Orna relations.
Orna does not translate an accepted SQL statement into an Orna operation,
record an Orna audit identity, or repair the operator's changes. Normal
recovery remains fail-closed on the next start.

Raw administration does not grant executable authority. ADR 0019's
PostgreSQL-owned guard applies equally to the shell and Orna's kernel sessions.
It rejects program execution, external and internal function definitions,
procedural languages, anonymous procedural blocks, extension management, and
dynamic loading before their side effects. The inherited engine process
filter independently prevents another executable or executable mapping.

The shell does not filter SQL text. Client-side filtering would be bypassable
through prepared, nested, or multi-statement SQL and would create a second
policy authority.

The command can run while the server is ready. It provides no write-quiescence
or repair-safety claim. Before a destructive repair, the operator must
establish the maintenance conditions required for that repair. A guarded
repair workflow needs its own accepted concurrency, validation, rollback, and
audit contract.

Opening and closing the shell creates no Orna identity, definition reference,
source revision, catalogue revision, function revision, executable artefact,
or active-revision change. SQL entered by the operator can change private
PostgreSQL state, but shell attachment itself does not.

## Diagnostics and exit status

After global command-shape validation, checks occur in this exact order:

1. all three terminal streams;
2. service-account identity;
3. shared package lock and exact ready state;
4. installed instance presence;
5. instance paths, manifest, ready record, live PID, instance lock, and socket;
6. running embedded-engine and final Orna ELF identity; and
7. private Unix-socket connection.

Failures write exactly one of these lines to standard error and exit `1`:

```text
orna: backend-shell must be run in an interactive terminal
orna: backend-shell must run as the orna service account
orna: package maintenance is incomplete
orna: the default Orna instance is not installed
orna: the default Orna instance is invalid
orna: the embedded PostgreSQL engine is not valid
orna: could not attach the backend shell
orna: backend-shell session failed
```

An absent fixed configuration or absent default state root uses the `not
installed` line. An unsafe or inconsistent configuration, path, manifest,
generation, ready record, PID, lock, or socket uses the `instance is invalid`
line. An unsupported embedded identity, changed final ELF identity, or
instance-to-engine mismatch uses the engine line. Connection or peer
authentication failure uses the attach line. Connection loss, protocol
failure, terminal I/O failure, incomplete EOF, unsupported COPY mode, binary
result field, or failed cancellation after attachment uses the session line.

The lines do not include an operating-system error, path, PID, SQL text,
environment value, or PostgreSQL credential. PostgreSQL notices and statement
errors after successful attachment use the private row renderer instead.

Normal `\q` and empty-buffer EOF exit `0`. Global usage exits `2`. A
pre-attachment or session failure exits `1`. Statement errors followed by a
later clean quit do not change exit `0`. Normal signal termination is not
translated to an Orna exit code or diagnostic.

## Required proof matrix

| Boundary | Required cases | Required result |
| --- | --- | --- |
| Command | exact command; missing part; extra flag; SQL argument; hostile package-maintenance variable | Only the exact public shape continues. Other public shapes use the exact global usage and exit `2`. The private package entry cannot intercept this command. |
| Terminal | all terminal; each stream redirected separately; piped input and output | All three terminals are required before account, package, instance, environment, socket, or write work. |
| Identity and package | exact service account; wrong real, effective, primary, and supplementary identities; ready, missing, incomplete, and writer-locked package state | Only the accepted identity and shared ready package state reach instance inspection. The package descriptor remains held for the session. |
| Ready host | absent instance; unsafe path; changed manifest; stale ready record; dead or wrong PID; unlocked or replaced lock; wrong engine or ELF; missing or hostile socket | Only the exact live host reaches connection, and every failure uses the correct precedence line without writing state. |
| Fixed target | hostile URL, `PG*`, `HOME`, `PATH`, password, service, TLS, GSS, Compose, current-directory, and startup-file inputs | The client connects only to the fixed private socket, database, and peer role and reads none of the hostile inputs. |
| Framing | empty and multi-line buffers; empty `\g`; `\q` with buffered text; other backslash lines; empty and buffered EOF | Buffering, dispatch, discard, and exit follow the exact terminal protocol without local SQL parsing. |
| Results | zero, one, and many rows; multiple statements; NULL and text `NULL`; tabs, line feeds, and non-ASCII text; notices; command tags; transactions; text and binary cursor results | Private rendering preserves every named text fact and waits for `ReadyForQuery`. A binary field fails before any row bytes reach the terminal. |
| Errors | syntax, permission, constraint, and failed-transaction errors followed by valid SQL | Each PostgreSQL error renders with its fields, the connection stays synchronised, and a later clean quit exits `0`. |
| Cancellation | unsent buffer; active query; cancellation race; cancellation failure | Unsent text clears locally. Active work uses CancelRequest and drains. Failure uses the session line and exit `1`. |
| COPY | text FROM STDIN completed with `\.`; `\q` and other backslash data; cancellation; EOF; text TO STDOUT; binary; COPY BOTH; PROGRAM in both directions | Text copy follows the exact subprotocol. EOF fails and cannot commit partial input. Unsupported modes fail the session. PROGRAM is rejected by the backend before process or COPY effects. |
| Executable closure | every ADR 0019 forbidden SQL form through direct, prepared, nested, role-changed, and multi-statement input | The PostgreSQL-owned guard, not the shell, rejects every path with exact SQLSTATE and message before side effects. |
| Attach only | stopped server; fresh and existing storage; successful and failed attachment | The shell starts no service and performs no bootstrap, migration, revision work, support materialisation, query, transaction, or filesystem write before connection. |
| Public boundary | source, checked-body, artefact, execution, script, and protocol representations | No public representation can name the shell, its SQL, or a control line. |
| Durable evidence | successful and failed attachment with complete instance snapshots | Attachment alone changes no Orna durable fact or audit record. |

Normal workspace formatting, build, lint, unit-test, live embedded-engine, and
clean-package gates remain required. Pseudo-terminal tests must use the native
Orna client and an embedded ready host. They cannot use a fake or installed
`psql`.

## Deferred surface

This record does not accept:

* a flag, command argument, connection override, SQL argument, input file, or
  non-interactive mode;
* public pgwire, PostgreSQL client compatibility, Orna source, function,
  script, or public-protocol administration;
* another instance, database, role, socket, TCP, TLS, GSS, password, or
  configuration source;
* psql startup files, general psql meta-commands, pager, editor, history, shell
  escape, or client-side variable language;
* privilege elevation, role switching by the host command, or another
  executable;
* automatic service start, bootstrap, migration, support materialisation,
  recovery, or repair before attachment;
* a write-quiescence gate, guarded repair transaction, rollback policy, or
  Orna audit record; or
* Windows or another non-Unix terminal and process contract.

## Precedence

This record keeps ADR 0001's private PostgreSQL and no-public-pgwire boundary
and ADR 0004's sole raw escape hatch. ADR 0019 supplies the embedded engine,
executable-load, process, package, and upgrade authority.

This amendment supersedes this record's earlier installed-`psql`, URL,
`PATH`, child environment, process replacement, native psql, and exit
contracts. It retains the exact command shape, terminal requirement, local
host identity, raw administration, no elevation, no public representation,
and no write before attachment.

For `orna server backend-shell`, this complete amended record has precedence.

## References

* [PostgreSQL 18 message flow](https://www.postgresql.org/docs/18/protocol-flow.html)
* [PostgreSQL 18 message formats](https://www.postgresql.org/docs/18/protocol-message-formats.html)
* [PostgreSQL 18 cancellation](https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-FLOW-CANCELING-REQUESTS)
* [PostgreSQL 18 COPY protocol](https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-COPY)
