# ADR 0014: Host-Only Backend Shell

**Status:** Accepted

## Decision

The first operator command is exactly:

```text
orna server backend-shell
```

It accepts no flags, arguments, connection overrides, or command to run after
connection. Any additional command-line token is a usage error. The command is
available only as a local Unix process for a host operator. It is not an Orna
source form, function, script operation, or public-protocol operation.

The command attaches the installed `psql` program to the private PostgreSQL
backend selected by the server-host configuration. It does not implement
PostgreSQL compatibility for public clients. It is the raw, trusted operator
escape hatch accepted by ADR 0001.

## Host and terminal boundary

The command runs with the caller's existing Unix user, group, environment, and
process permissions. It does not call `sudo`, change user or group, request a
capability, use a privileged helper, or otherwise elevate access. The caller
must already possess the private server-host configuration in its process
environment and have permission to connect to PostgreSQL. Orna does not make
that configuration or its password secret from the caller; it only avoids
copying those values into arguments, diagnostics, debug output, or logs.

Before reading backend configuration, the command requires standard input,
standard output, and standard error all to be terminals. If any one is not a
terminal, it refuses to start `psql`. A Unix pseudo-terminal satisfies this
terminal test; the command does not try to infer whether a human or automation
created that terminal.

For ADR 0001, "not available through scripts" means that Orna source, Orna
functions, Orna scripts, and the public protocol cannot address the escape
hatch, and that the host command cannot run with redirected or piped standard
streams. No parser, catalogue definition, executable artefact, invocation
method, or protocol message represents this operation.

An interactive remote login may run the local process when all three standard
streams are terminals and the remote account already has the required host
permissions. This does not create a remote OrnaDB administration endpoint.

## Server-host configuration

`ORNA_SERVER_POSTGRES_URL` is the one required configuration input. It is a
private server-host value rather than a public client setting. The command
fails closed when the variable is absent, empty, or invalid.

The accepted URL shape is exactly:

```text
postgresql://<user>[:<password>]@<host>:<port>/<database>
```

The components use standard URL escaping. The host is one TCP hostname, IPv4
address, or bracketed IPv6 address. The port is an explicit decimal integer in
the range `1..=65535`. The user and database are explicit and non-empty. A
password may be present or absent.
A URL with another scheme, multiple hosts, a Unix socket, a service
definition, an omitted component, a query, or a fragment is invalid. The
command does not accept URL options that select another host, user, database,
credential source, startup command, or session behaviour.

Percent escapes in the user, password, and database must be complete pairs of
hexadecimal digits and must decode as UTF-8. A decoded user, password, or
database containing a NUL byte is invalid because it cannot be represented in
the Unix child environment. An empty decoded password is permitted when the
URL explicitly contains the password separator; the resulting `PGPASSWORD` is
present and empty.

The resolved host, port, user, and database are the complete connection target,
and the optional URL password is the only explicit credential supplied by
Orna. Transport is one TCP connection without TLS or GSS encryption, matching
the first PostgreSQL kernel's `NoTls` connection contract. The command does not
read `DATABASE_URL`, inherited `PG*` variables, the default PostgreSQL password
file, default PostgreSQL TLS material, Docker Compose configuration, files
found from the current working directory, or a built-in development target.
It does not guess a user, database, host, or credential. Command-line
connection overrides are not available.

The configured user is the server role used by this shell. The command neither
selects nor falls back to a separate administration role, operating-system
user, or PostgreSQL default role. What that configured role may do remains a
private deployment decision enforced by PostgreSQL.

This URL and its parsed, redacting value model are reserved as the first shared
PostgreSQL configuration boundary for a future server-host process. That
process must reuse the same parser, resolved connection facts, and non-TLS
transport rather than create a second interpretation of
`ORNA_SERVER_POSTGRES_URL`. This record does not otherwise define the server
process, its lifecycle, or its remaining configuration.

## Child process boundary

The command parses and validates the URL before constructing the child
environment. Formatting or reporting the parsed configuration never includes
the password or the original URL. An invalid value is reported without echoing
any part of it.

Before starting `psql`, the command removes `ORNA_SERVER_POSTGRES_URL` and
every inherited environment variable whose name begins with `PG` from the
child environment. It then supplies exactly these libpq variables from the
resolved URL:

* `PGHOST` with the single resolved TCP host;
* `PGPORT` with the resolved numeric port;
* `PGUSER` with the explicit configured user;
* `PGDATABASE` with the explicit configured database; and
* `PGPASSWORD` only when the URL contains a password.

The child also receives `PGPASSFILE=/dev/null`, `PGSSLMODE=disable`, and
`PGGSSENCMODE=disable`.
`PGPASSFILE` prevents libpq from consulting the caller's default password file;
`PGSSLMODE` prevents libpq from loading default TLS or client-certificate
material; and `PGGSSENCMODE` prevents GSS-encrypted transport. Together they
fix the same non-TLS transport as the server host. These are fixed connection
facts, not user-selectable overrides.

The URL and any password initially exist in the launching Orna process
environment and parsed memory because the caller supplied them there. The
replacement removes the URL variable and supplies the password to `psql` only
through `PGPASSWORD`; it is never placed in an argument, diagnostic, usage
text, debug output, or Orna log. This is a redaction and propagation boundary,
not protection from the host operator or operating-system process inspection.
When the URL has no password, `PGPASSWORD` is absent rather than empty. Any
password prompt or authentication diagnostic after replacement is owned by
`psql`; Orna does not collect or reinterpret it.

PostgreSQL still chooses the authentication exchange after `psql` connects.
The configured server may accept trust authentication, request the supplied or
interactively entered password, or request native GSS authentication using the
operator's existing host credentials. Orna neither selects nor claims to
isolate those native authentication mechanisms. They do not change the fixed
target or introduce another Orna configuration source. A future server-host
process must define which of those authentication exchanges it implements;
sharing this configuration boundary does not silently grant it all of
`psql`'s authentication capabilities.

The command resolves an installed executable named `psql` through the caller's
`PATH`. An absent or empty `PATH` fails. Empty and relative path entries are
ignored; only absolute entries are searched, in order, so executable discovery
never falls back to the current directory or a platform default path. It
invokes the resolved absolute executable directly, without a command shell,
with exactly one argument:

```text
--no-psqlrc
```

The command does not load a user or system `psqlrc` file. It supplies no
`--command`, `--file`, connection URI, database argument, variable assignment,
or other PostgreSQL option. It inherits the three terminal streams unchanged.

After validation and environment construction, the Unix process is replaced
with `psql`. There is no supervising Orna process. Normal exit codes, signal
termination, terminal job control, terminal size changes, and interactive
input therefore have the exact behaviour of the installed `psql` process.
Failure to replace the process remains a pre-launch Orna failure.

## Administration boundary

The command only attaches to the configured backend. It does not start or stop
PostgreSQL, Docker, the OrnaDB server, or another service. It does not
bootstrap storage, run or verify a migration, inspect the active revision,
issue a query, begin a transaction, acquire an administration lock, or write
backend data before process replacement.

Once attached, the operator has the raw PostgreSQL access granted to the
configured server role. `psql` may read or write private state at the
operator's direction. Orna does not parse, authorise, constrain, translate,
record, or repair those commands. PostgreSQL syntax and results used in this
shell remain private administration behaviour and do not become Orna language
or protocol semantics.

The shell may be opened while the OrnaDB server is running. It provides no
coordination with active reads or writes and does not make a repair safe.
Before changing private state, the operator must stop all other application
and operator writes and establish the maintenance conditions required for that
repair. This record does not define or pretend to enforce a maintenance mode.
A later guarded repair workflow requires its own accepted concurrency,
validation, rollback, and recovery rules.

Opening or closing the shell creates no Orna identity, definition reference,
source revision, catalogue revision, function revision, executable artefact,
or active-revision change. It emits no Orna audit record. Unix and PostgreSQL
process, connection, and statement auditing are deployment concerns outside
this command.

When the product command lands, the existing `just backend-shell` development
shortcut is removed in the same implementation slice. It must not remain as a
second Compose-specific configuration and credential path.

## Diagnostics and exit status

Command-line shape is checked before the terminal and configuration checks.
An incorrect command shape writes this exact text to standard error and exits
with status `2`:

```text
Usage: orna server backend-shell
```

Pre-launch failures write exactly one of these lines to standard error and
exit with status `1`:

```text
orna: backend-shell must be run in an interactive terminal
orna: backend-shell needs ORNA_SERVER_POSTGRES_URL
orna: ORNA_SERVER_POSTGRES_URL must use postgresql://user[:password]@host:port/database
orna: could not start psql from PATH
```

The terminal diagnostic has precedence over both configuration diagnostics.
For an interactive command, a missing or empty variable uses the `needs`
diagnostic. Any other parse, host-count, transport, user, database, port, or
unsupported-option failure uses the `must use` diagnostic. Failure to
find, execute, or replace the process uses the `could not start` diagnostic.
These messages do not append operating-system errors or configuration values.

After successful process replacement, Orna emits no further diagnostic and
does not translate `psql` output, exit status, or signal termination.

## Required proof matrix

| Boundary | Required cases | Required result |
| --- | --- | --- |
| Command shape | exact three-part command; missing part; extra flag; extra argument; attempted SQL command | Only the exact command continues. Every other shape prints the exact usage line and exits `2`. |
| Terminal | all three streams are terminals; each stream separately redirected; piped input and output | All three terminals are required. A failure prints only the exact terminal diagnostic, exits `1`, and does not read configuration or start a process. |
| Required configuration | variable absent, empty, valid without password, valid with password | Missing and empty values use the exact `needs` diagnostic. Both valid forms resolve one complete connection target. |
| URL validation | one TCP host; multiple hosts; Unix socket; missing user; missing database; port `0`, port above `65535`, non-decimal port; unsupported target or credential option; malformed escaping; invalid UTF-8; decoded NUL | Only the accepted single-host form reaches process construction. Every rejection uses the same redacted accepted-shape diagnostic. |
| No fallback | conflicting `DATABASE_URL`; hostile inherited `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`, `PGPASSWORD`, `PGSERVICE`, `PGPASSFILE`, and `PGOPTIONS`; Compose files; changed working directory | No alternate configuration changes the resolved target or explicit URL password. No default development connection is attempted. Native authentication selected by PostgreSQL remains outside this configuration claim. |
| Child environment | URL with absent, empty, and non-empty password; unrelated inherited environment | The child receives exact `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`, `PGPASSFILE=/dev/null`, `PGSSLMODE=disable`, and `PGGSSENCMODE=disable`; it receives `PGPASSWORD` only when the URL contains one. The URL variable and every other inherited `PG*` variable are absent. Unrelated variables, including `PATH`, remain inherited. |
| Ambient connection inputs | hostile `$HOME/.pgpass`; default TLS/client-certificate files; hostile inherited libpq variables | None changes the target, explicit password, or transport. Password-file lookup, TLS material, TLS, and GSS encryption are disabled by fixed child values. PostgreSQL-selected native authentication after connection remains `psql` behaviour. |
| Secret handling | successful launch; invalid URL; debug formatting; executable not found; executable denied | The original URL and password appear in no argument, Orna diagnostic, formatted value, or captured Orna output. Tests acknowledge that the launching process environment and replacement `psql` environment hold the caller-supplied secret. |
| Executable discovery | absent or empty `PATH`; empty, relative, nonexistent, non-executable, and absolute entries; changed working directory | Only the first executable `psql` in an absolute `PATH` entry is selected. No current-directory or platform-default fallback occurs. |
| Process invocation | fake `psql` first in an absolute `PATH`; paths and values containing shell metacharacters; user `psqlrc` present | The executable is invoked directly with only `--no-psqlrc`. No command shell, startup file, injected option, connection argument, or preliminary query runs. |
| Process result | fake `psql` exits `0`, exits non-zero, and terminates under `SIGHUP`, `SIGINT`, `SIGQUIT`, and `SIGTERM` | Successful replacement preserves the exact child exit and signal behaviour, with no Orna wrapper output. Process identity plus direct `exec` establishes native behaviour for other Unix signals without claiming that every signal terminates. |
| Attach-only behaviour | PostgreSQL and OrnaDB stopped; PostgreSQL available; OrnaDB serving; fresh and existing storage | The command starts no service and performs no bootstrap, migration, revision, lock, query, or write operation. Availability is determined only by `psql` after replacement. |
| Public boundary | crate dependency direction and the existing public syntax, checked-body, artefact, and execution enums | Only the host binary depends on the shell entry point, and no public-language or durable-execution representation contains it. When script or protocol dispatch is introduced, its first tests must prove that no request can name this operation. |
| Durable evidence | successful and failed launch against existing storage | No Orna identity, evidence row, revision, artefact, or audit record is created or changed by launch. |
| Development tooling | product command present in the workspace | The old `just backend-shell` Compose shortcut is removed so it cannot remain a second configuration or credential path. |

The normal workspace formatting, build, lint, unit-test, and live PostgreSQL
gates remain required. Process-boundary tests must use Unix terminals and a
fake `psql`; they must not depend on a developer's installed program or alter a
real backend.

## Deferred surface

This record does not accept:

* a flag, command argument, connection override, SQL argument, file input, or
  non-interactive mode;
* a public pgwire endpoint, PostgreSQL client compatibility, Orna source form,
  Orna function, Orna script operation, or public-protocol administration
  method;
* Windows or another non-Unix process and terminal contract;
* multiple hosts, failover, a Unix socket, PostgreSQL service files, passfiles,
  TLS, GSS encryption, client certificates, inherited libpq configuration, or
  another backend URL source;
* a separate shell role, privilege elevation, role switching, or a default
  administration account;
* starting or stopping services, Docker or Compose discovery, backend
  bootstrap, migrations, revision inspection, automatic repair, or an Orna
  query before attachment;
* a maintenance lock, write quiescence check, server-liveness check, guarded
  repair transaction, rollback policy, or validation of an operator's raw
  PostgreSQL changes;
* an Orna audit identity, audit record, source revision, catalogue revision,
  function revision, or artefact for host administration;
* packaging or installing `psql`, selecting its version, or changing its
  native interactive diagnostics and authentication prompts; or
* the future server process, protocol listener, service manager, broader
  server configuration, secret rotation, or deployment-specific Unix and
  PostgreSQL auditing.

Those surfaces require their own accepted security, lifecycle, concurrency,
configuration, and recovery rules rather than being inferred from this trusted
escape hatch.

## Precedence

This record makes the server-side backend shell accepted by ADR 0001 concrete.
It preserves ADR 0001's private-PostgreSQL and no-public-pgwire boundary and
ADR 0004's protected-schema and sole-escape-hatch boundary. It does not weaken
ADR 0003's source authority or ADR 0004's recovery validation for normal Orna
apply and restart paths; raw operator actions remain explicitly outside those
source and execution paths.

This record narrows the operator-access direction in
`spec/docs/35-security.md`, `spec/docs/36-storage-transactions.md`,
`spec/docs/38-implementation-roadmap.md`, and `spec/docs/41-open-questions.md`.
It does not make PostgreSQL behaviour part of the public OrnaDB contract.

For the host-only `orna server backend-shell` command, this accepted record has
precedence.
