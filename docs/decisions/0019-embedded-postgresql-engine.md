# ADR 0019: PostgreSQL Is Part of the Orna Executable

**Status:** Accepted

## Context

Work ADR 0017 interpreted a self-contained Orna distribution as one public
Orna executable plus a private tree of PostgreSQL executables and shared
objects. That is not the product boundary.

The production distribution has one executable. PostgreSQL code and the
PostgreSQL support assets that Orna needs are compiled into that executable.
Orna does not install, extract, discover, or launch a separate PostgreSQL
binary at runtime.

## Decision

The first production package installs exactly one product executable:

```text
/usr/bin/orna
```

The build statically links the required PostgreSQL 18.4 code into that ELF
file. It embeds the required immutable PostgreSQL support assets in the same
file. The supported target remains Debian 12 amd64 Linux.

Debian package-manager control scripts can remain in dpkg's private control
state and execute under dpkg, but they contain no PostgreSQL code and are not a
product command or runtime. The product payload and a running installation
contain no separate `postgres`,
`psql`, `initdb`, `pg_upgrade`, `pg_ctl`, `pg_resetwal`, PostgreSQL executable,
or PostgreSQL shared object. Orna does not extract such code to a regular file
or a Linux memory-backed file. It does not search `PATH` or another directory
for PostgreSQL code. It does not download PostgreSQL code or support assets
when it starts.

One executable does not require one operating-system process. PostgreSQL uses
a postmaster, backend processes, auxiliary processes, shared memory,
semaphores, signals, and process-level crash isolation. Orna preserves those
database semantics. Every production process that executes PostgreSQL code
must execute code from the same `/usr/bin/orna` ELF image. No process can
select or load a different PostgreSQL program.

Mutable instance state remains outside the executable. This includes the
database cluster, write-ahead log (WAL), generated configuration, locks,
readiness state, and other durable database files. These files are data, not a
PostgreSQL program distribution.

The signed Orna distribution is the only production distribution authority.
Unmodified upstream PostgreSQL source is checked out at
`third_party/postgresql` as a Git submodule of
`https://github.com/postgres/postgres.git`. The superproject gitlink pins the
exact `REL_18_4` commit
`f5cc81719e6da4cbdb1f797c48b693e91018153a`. A branch name, moving tag, fork,
or remote default branch is not source authority.

This first release has one PostgreSQL source gitlink. A future release that
embeds two PostgreSQL majors must pin one distinct, versioned upstream gitlink
per major instead of moving this one path between source and candidate trees.
The future upgrade amendment must name both paths, commits, overlay sets, and
patch series before a second engine is added.

All Orna-owned PostgreSQL integration is checked in beneath `postgresql/` in
this repository. New source files live in `postgresql/overlays/18.4/` at their
final PostgreSQL-relative paths. An overlay can only add an absent path; it
cannot replace an upstream file. Changes to existing upstream files use the
small ordered series beneath `postgresql/patches/18.4/`. A patch must contain
only the minimum existing-file edit that cannot be expressed as an added
source file. The Git index pins each overlay and patch path, mode, and byte
content. The zero-padded patch filenames define their order. Untracked input,
duplicate destinations, a patch that adds a file, and an overlay that replaces
a file fail the build. There is no separate source recipe.

A proof build fails if the submodule is absent, modified, has untracked files,
is at another commit, or does not match the pinned source-tree inventory.
`postgresql/Makefile` never initialises, fetches, updates, repairs, or modifies
the submodule. `make -C postgresql update-source POSTGRESQL_REF=<full-commit>`
is the only maintenance interface that may change the gitlink. It accepts only
a full commit identifier, fetches through the configured upstream submodule
remote, checks out that detached commit, and stops before building, staging, or
committing. The resulting gitlink change remains an ordinary reviewable
superproject change.

Credential-free workflow checkout is the one provisioning boundary. It may
initialise and fetch the exact superproject-pinned submodule and its required
history before the proof starts. Checkout persists no credentials. All native
source validation, configuration, compilation, and lifecycle work after that
boundary is network-disabled and fails rather than fetching missing state.

The destination embedded PostgreSQL build must be reproducible. Two isolated
builds must produce identical static link inputs and embedded support-data
inputs. `postgresql/Makefile` is the one Orna build interface. It verifies the
immutable upstream gitlink, copies its canonical tracked-file inventory into a
private source directory beneath the caller-owned `target` root, installs the
tracked added-file overlays, and applies each tracked patch in filename order
with `--batch --forward --fuzz=0`. It then configures and builds only in a
separate out-of-tree build directory. It never writes into the submodule,
never uses a host-global temporary directory, and contains no second
declarative build language.

The `Containerfile` prepares the pinned dependency and toolchain image before
either proof build. That image preparation may use only the accepted Debian
snapshot network sources and does not receive PostgreSQL source. Its resulting
image digest is fixed for both proof builds. Both PostgreSQL source builds and
all lifecycle probes then run from that same image with `--network=none` and
the submodule mounted read-only.

One generated embedded-engine manifest records and binds the declared
submodule URL and path, exact upstream commit, canonical upstream source-tree
inventory, ordered overlay and patch inventories, resulting prepared-source
inventory, C compiler and linker, static archives, symbol closure, path
remapping, PostgreSQL licence, and support data for downstream build stages.
It also binds the exact checked-in `Makefile`, `Containerfile`, support
generator, manifest generator, lifecycle probe, lifecycle verifier, prepared
builder image digest, and pinned Debian package closure. The manifest records
evidence after the build. It does not configure the build or select its own
inputs. All inventories use path and blob bytes, not filesystem times. The
submodule is an upstream input, not a separate runtime acceptance, publication,
or signing authority.

A later Orna distribution manifest must bind that embedded-engine manifest to
the exact Rust toolchain, `Cargo.lock`, Rust linker flags, Rust path remapping,
and final Orna ELF. The later package gate must prove identical final Orna ELF
bytes and bind those bytes to the signed Orna distribution. The embedded build
and final Orna build consume these authorities in that order. Neither manifest
is a separate PostgreSQL runtime acceptance or signing authority.

## Fixed product boundary

The following rules do not depend on the first link tracer:

* PostgreSQL remains Orna's private storage and transaction kernel.
* Orna exposes no public PostgreSQL TCP listener, pgwire endpoint, SQL
  interface, driver contract, or PostgreSQL compatibility promise.
* The Rust kernel can continue to use a protected private Unix socket as an
  internal transport.
* Production cannot depend on an installed PostgreSQL package, host
  PostgreSQL process, Docker, container runtime, first-start download, or
  external development database.
* PostgreSQL extension shared objects and user-selected native modules cannot
  be loaded. Required native code must be statically linked into Orna.
* The package cannot contain a second executable copy of Orna for PostgreSQL
  dispatch or package maintenance.

## Prototype questions

The one-executable rule is accepted. The first native link tracer must answer
these implementation questions before a later lifecycle decision selects an
answer:

* whether PostgreSQL roles enter through direct post-fork calls or private
  re-execution of the same Orna ELF;
* how the complete PostgreSQL `main.c` initialisation and role dispatcher are
  retained without a second C `main` symbol;
* how initialisation replaces sibling-`postgres` discovery and `popen` calls
  for bootstrap and single-user backends;
* whether non-executable support assets stay memory-resident or are
  materialised with verified private modes and digests;
* whether PL/pgSQL is a statically linked built-in or is removed from the
  fixed Orna cluster initialisation;
* how the trusted `orna server backend-shell` command supplies raw
  administration without a `psql` executable;
* whether the trusted backend shell is restricted, or the embedded engine
  rejects SQL `LOAD`, C-language functions, extension loading, and
  `COPY PROGRAM`, so that SQL cannot introduce another executable or shared
  object authority;
* how package maintenance enters the installed Orna executable without the
  duplicate `/usr/lib/orna/libexec/orna-package-maintenance` helper required by
  work ADR 0017, while retaining the accepted package transaction semantics;
* how one Orna executable guarantees process closure if its supervisor stops;
  and
* how a future release embeds and namespaces two PostgreSQL major versions for
  `orna server upgrade` without using external `pg_upgrade` executables.

The prototype cannot answer one of these questions by weakening the fixed
product boundary. A later accepted amendment must define the selected process,
resource, shell, upgrade, error, and proof contracts before production host or
package implementation starts.

## Initial proof

The first tracer is deliberately smaller than a database lifecycle. It must:

1. build the pinned PostgreSQL 18.4 Linux backend objects without producing a
   packaged PostgreSQL executable or shared object;
2. archive the required objects as one deterministic static link input;
3. replace the upstream C `main` symbol with one private entry shim that runs
   the complete PostgreSQL startup dispatcher;
4. link that archive into a deterministic native C entry probe;
5. enter the read-only `--describe-config` PostgreSQL path in a fresh process;
6. prove that the probe executable is not named `postgres` and that no
   PostgreSQL executable or shared object was opened or executed; and
7. build the same archive and probe twice and compare their exact bytes.

The submodule-backed reproduction of this proof starts only from the checked
out gitlink and the checked-in Orna Makefile, overlays, and sparse patch series.
It runs with network access disabled. Before `configure` runs, it proves the
exact clean upstream commit and inventory, copies that inventory beneath the
caller-owned target root, applies only the recorded overlays and patches, and
proves the exact prepared-source inventory. It uses a separate out-of-tree
build and never modifies the submodule source.

This tracer does not claim initialisation, support-asset completeness,
postmaster readiness, worker operation, private SQL, crash recovery, backend
shell, or major upgrade. Each later tracer must add one complete behaviour
without adding a PostgreSQL executable or shared object.

## Selected process model

The production target uses PostgreSQL's non-`EXEC_BACKEND` Linux process
model. Orna does not re-execute itself to select a PostgreSQL role.

`orna server run` remains a Rust supervisor. It completes command, identity,
package, instance, path, embedded-manifest, support-data, and configuration
checks while the process has exactly one operating-system thread. It prepares
every child argument, environment value, file descriptor, signal mask, and C
string before it forks. The child performs only the fixed post-fork setup and
calls the versioned private C entry:

```text
/usr/bin/orna server run
  Orna supervisor
    fork
      orna_postgres18_entry(...)
        PostmasterMain(...)
          fork PostgreSQL backends and auxiliary roles directly
```

The child does not call `execve`, `execveat`, a command shell, or another Rust
command dispatcher. It clears the inherited environment and supplies only the
fixed locale, time-zone, data-directory, support-root, socket, authentication,
configuration, and engine facts selected by the supervisor. It closes every
file descriptor except the exact inherited descriptors named by the lifecycle
capability. It sets `argv[0]` to `/usr/bin/orna` before entering the complete
renamed PostgreSQL `main.c` dispatcher.

Only the parent creates an asynchronous Rust runtime, and only after the
postmaster fork has succeeded. The parent must prove that `/proc/self/task`
contains exactly one task immediately before every engine or initialiser fork.
A missing procfs fact or a second task fails before `fork`.

PostgreSQL 18.4's non-`EXEC_BACKEND` postmaster then uses its normal
`fork_process()` path and calls the selected child role function directly.
Client backends, auxiliary processes, and background workers inherit the same
ELF mappings. Orna does not add a Rust dispatcher for those roles.

For every postmaster, backend, auxiliary, bootstrap, single-user, and upgrade
role, `/proc/<pid>/exe` must resolve to the same installed `/usr/bin/orna`
inode. Process-title text is not executable identity. A PostgreSQL role
activates one embedded major version and one role for its complete process
lifetime.

## Selected initialisation

The production ELF also contains a versioned private initialisation entry
derived from PostgreSQL 18.4 `initdb`. It retains upstream directory creation,
catalogue bootstrap, configuration generation, synchronisation, `template1`,
`template0`, and `postgres` creation. It is not a new public command and it
does not parse operator-supplied arguments.

For a new generation, the single-threaded supervisor forks one initialiser
child and calls the linked entry with the exact accepted initialisation facts:

```text
data directory     new empty mode-0700 generation data directory
superuser          orna_kernel
encoding           UTF8
locale provider    builtin
builtin locale     PG_UNICODE_FAST
data checksums     enabled
local auth         peer
host auth          reject
sync               enabled
instructions       disabled
```

The linked initialiser does not discover a sibling `postgres` program. Its
three upstream process boundaries are replaced as follows:

1. the configuration check forks a child and calls the linked backend entry
   with `--check`;
2. catalogue bootstrap creates a pipe, forks a child, calls the linked backend
   entry with `--boot`, and streams the embedded `postgres.bki` input; and
3. post-bootstrap setup creates a pipe, forks a child, calls the linked backend
   entry with `--single` against `template1`, and streams the retained setup
   SQL.

Each child has fixed descriptors and arguments, activates the executable-load
filter below, and exits through the PostgreSQL entry. The initialiser closes
the pipe, waits for the exact child, and treats a signal, non-zero status,
short write, unexpected output, or wait failure as initialisation failure. It
uses no `system`, `popen`, shell string, `PATH`, bindir, or executable path.

The linked initialiser replaces upstream `setup_collation()` with one fixed
catalogue assertion. It does not call `pg_import_system_collations`, run
`locale -a`, enumerate operating-system locales, or add an operating-system
collation. The assertion requires the exact seven pinned bootstrap rows named
`default`, `C`, `POSIX`, `ucs_basic`, `unicode`, `pg_c_utf8`, and
`pg_unicode_fast`, including their pinned OIDs, providers, encodings, locale
names, and version fields. Any difference is initialisation failure. The
selected database locale remains the built-in `PG_UNICODE_FAST`; host locale
installation and ordering cannot change the initial catalogue.

Initial database creation then follows the retained work ADR 0017 contract:
Orna starts the embedded postmaster on the bootstrap private Unix socket,
connects as the accepted peer, creates `orna` from `template0`, fast-stops and
waits for the postmaster, installs the exact normal authentication files, and
only then begins normal recovery.

### Embedded support data

PostgreSQL continues to use ordinary read-only paths for generated catalogue,
configuration, time-zone, and text-search inputs. A broad virtual filesystem
patch is not accepted. The build instead creates one deterministic support
bundle whose manifest enumerates every member by relative path, mode, length,
and SHA-256 digest.

The first closed inventory contains:

* `postgres.bki`, `pg_hba.conf.sample`, `pg_ident.conf.sample`, and
  `postgresql.conf.sample`;
* `snowball_create.sql`, `information_schema.sql`, `sql_features.txt`,
  `system_constraints.sql`, `system_functions.sql`, and `system_views.sql`;
* the compiled PostgreSQL time-zone tree and selected `timezonesets` files;
  and
* the selected `tsearch_data` files referenced by the retained built-in text
  search definitions.

The inventory contains no `extension` member, control file, procedural
language SQL, executable, archive, object, or shared object. The build fails
on an unlisted path, duplicate path, link, special file, executable mode, path
traversal, case collision, or changed bytes. The embedded-engine manifest
binds the support-bundle digest and the exact member inventory. The final Orna
distribution manifest binds the unchanged embedded-engine manifest and final
ELF.

Before initialisation or postmaster entry, Orna materialises this data-only
bundle beneath the operation-specific private runtime root:

```text
service and first start  /run/orna/default/embedded-postgresql/<embedded-engine-manifest-sha256>
offline upgrade          /run/orna-upgrade/default/embedded-postgresql/<embedded-engine-manifest-sha256>
```

The root and every directory are `orna:orna` mode `0700`. Every file is a
non-linked regular file, link count `1`, mode `0600`, and has the exact
manifest length and digest. Materialisation uses descriptor-relative
`openat2`, rejects links and unexpected existing members, writes through a
same-directory temporary file, calls file `fsync`, renames, and calls parent
directory `fsync`. A complete existing tree is reverified before reuse. An
incomplete, additional, or changed tree is removed only while the instance
lock is held, then rebuilt from bytes already in the current Orna ELF.

The Rust supervisor passes the verified absolute root to the C entry through
a private setter before PostgreSQL initialisation. Patched PostgreSQL path
resolution uses only that fixed root. It does not derive a share path from
`argv[0]`, `/usr/bin/orna`, `PATH`, configuration, or the environment.

One support root belongs to exactly one embedded-engine manifest. A
two-major upgrade materialises two independent roots:

```text
/run/orna-upgrade/default/embedded-postgresql/source/<source-manifest-sha256>
/run/orna-upgrade/default/embedded-postgresql/candidate/<candidate-manifest-sha256>
```

The source and candidate entries each receive only the root bound by their
own manifest. The supervisor rejects equal roots, crossed roots, a member from
the other inventory, or a digest verified against the other manifest before
forking a role.

This materialised tree is a cache of non-executable bytes whose sole authority
is the current ELF. It is not a PostgreSQL installation. No executable,
archive, object, or shared object can be materialised to this directory, a
different regular file, or a memory-backed file.

### No procedural or external code authority

The first embedded cluster does not install PL/pgSQL. The linked initialiser
omits upstream `load_plpgsql()`, the support bundle omits PL/pgSQL files, and
the package contains no PL/pgSQL shared object. Before this initialiser becomes
a production input, migrations `0002` and `0005` must express their current
checks and data steps as declarative SQL without `DO`. Development-only tests
against an external PostgreSQL service can continue to use PL/pgSQL, but no
production migration or clean-machine proof can depend on it.

One PostgreSQL-owned guard applies to every private connection, including the
Orna kernel and backend shell. The utility-tag part runs before privilege
checks, event triggers, support-file reads, catalogue writes, process
creation, or dynamic loading and rejects:

* `LOAD`;
* `COPY FROM PROGRAM` and `COPY TO PROGRAM`;
* every create, alter, update, contents, drop, rename, owner, comment, and
  security-label operation on an extension;
* `CREATE LANGUAGE`;
* anonymous `DO`; and
* `CREATE` or `CREATE OR REPLACE` of a function or procedure whose resolved
  language OID is `C` or `internal`.

The guard uses SQLSTATE `0A000` and these exact primary messages without a
detail or hint:

```text
Orna does not permit SQL LOAD
Orna does not permit COPY PROGRAM
Orna does not permit PostgreSQL extension management
Orna does not permit procedural language creation
Orna does not permit anonymous procedural blocks
Orna does not permit C or internal language function or procedure definitions
Orna does not permit PostgreSQL dynamic loading
```

The function and procedure part runs after PostgreSQL resolves the language
catalogue row but before it records or resolves an object-file name. The
single-user child used by the linked initialiser carries one process-local,
non-SQL initialisation capability that permits only the retained upstream
`system_functions.sql` internal definitions. The capability is set by the
typed initialiser entry, cannot be set by a GUC, role, environment value, or
SQL statement, and is absent from every postmaster backend. Shell-side text
filtering is not an authority and is not permitted.

Every linked PostgreSQL entry installs one inherited Linux seccomp filter
before entering PostgreSQL code. The filter denies `execve`, `execveat`,
`memfd_create`, executable `mmap`, executable `mprotect`, executable
`pkey_mprotect`, and executable `shmat`. The single-threaded supervisor first
loads and verifies the complete allowed base ELF and name-service mapping
closure, so normal engine work requires no later executable mapping. The
filter complements the typed SQL guard and unconditional failure in
`load_file()` and `load_external_function()`; it does not replace their exact
diagnostics. Both dynamic-loader functions fail with SQLSTATE `0A000` and the
last primary message above, without a detail or hint, before resolving or
opening a file name.

On Debian 12 amd64, the filter checks `__X32_SYSCALL_BIT` immediately after it
validates `AUDIT_ARCH_X86_64` and loads the syscall number. It returns `EPERM`
for every x32-numbered syscall before any native x86-64 syscall comparison.
Orna does not use the x32 ABI. An x32 syscall number cannot select an alias of
`execve`, `execveat`, `memfd_create`, or an executable mapping operation.

The support-root check and filter installation are compiled only into the
private backend entry object built with `ORNA_EMBEDDED_ENTRY`. The ordinary
PostgreSQL `main` object has no Orna runtime reference and retains its upstream
startup behaviour.

Preload lists, JIT, injection points, output plugins, archive libraries,
archive commands, restore commands, recovery-end commands, and SSL remain
disabled. `allow_alter_system=off` and an exact empty
`postgresql.auto.conf` remain required. A trusted operator can damage private
database state, but SQL cannot introduce or execute another program or shared
object.

## Selected service lifecycle

Work ADR 0017's service account, trusted paths, local-filesystem boundary,
package and instance locks, generation manifest, peer authentication,
readiness, activation commit, systemd notification, and fail-closed recovery
remain accepted with these substitutions:

* an embedded-engine identity replaces a private runtime-tree identity;
* a verified data-only support root replaces a private PostgreSQL installation
  root;
* the supervisor forks a linked entry instead of executing `postgres`;
* the linked initialiser replaces `initdb`; and
* no fixed PostgreSQL `PATH`, loader path, or executable handle exists.

`orna server run` verifies the signed Orna distribution and the embedded
engine manifest before it opens instance data. It materialises and verifies
support data before initialisation or postmaster entry. It reports ready only
after PostgreSQL readiness, kernel bootstrap, accepted-standard installation,
migrations, canonical recovery, durable activation commit, and ready-record
commit have all succeeded.

The ready record contains the server PID, postmaster PID, generation,
embedded-engine identity, final Orna ELF digest, and instance-manifest digest.
The instance manifest records the engine identity that last opened each
generation. A normal run fails closed when the current ELF cannot supply the
recorded engine or one accepted forward transition.

The systemd unit remains the production process-closure authority. It uses the
retained `Type=notify`, `KillMode=mixed`, `KillSignal=SIGINT`,
`TimeoutStopSec=90s`, `SendSIGKILL=yes`, and rate-limited restart settings. On
controlled stop, Orna removes readiness, rejects new host work, sends
`SIGINT` to the direct postmaster for fast shutdown, and waits up to 60
seconds. It then sends `SIGQUIT` and waits for PostgreSQL's immediate-shutdown
escalation. Any process remaining at the unit deadline is killed by systemd in
the service cgroup. The supervisor is a Linux child subreaper and reaps
orphaned descendants, but this does not replace cgroup containment.

An unexpected postmaster exit removes readiness and makes Orna exit non-zero.
There is no in-process restart. The next rate-limited systemd start repeats
embedded-manifest verification, support-data verification, PostgreSQL crash
recovery, migrations, standard verification, and active Orna recovery before
readiness.

## Package maintenance entry

The package installs no private helper or second Orna copy. Before public
command parsing, the installed `/usr/bin/orna` recognises a private
package-maintenance request only when the argument and environment facts below
hold:

```text
argument count            1, including argv[0]
ORNA_PACKAGE_MAINTENANCE  begin or complete
```

Dispatch reads the argument count first. It reads
`ORNA_PACKAGE_MAINTENANCE` only when the count is exactly `1`; every public
command, including source check, therefore reaches public parsing without an
environment read. A selected private request then checks that both the real
and effective user IDs are `0`. A non-root request reaches the exact root
diagnostic below and performs no package work.

Debian maintainer scripts first use the already-running trusted POSIX shell's
`unset` builtin to remove every loader-control variable supported by the
pinned Debian 12 glibc. They then use the absolute system `env` program to
discard the remaining inherited environment before the dynamic loader starts
Orna, supply only the private selector, and replace the script with the
absolute installed Orna binary. The exact shapes are:

```text
unset GLIBC_TUNABLES LD_AUDIT LD_DEBUG LD_DEBUG_OUTPUT LD_DYNAMIC_WEAK
unset LD_ASSUME_KERNEL LD_BIND_NOT LD_BIND_NOW LD_HWCAP_MASK LD_LIBRARY_PATH
unset LD_ORIGIN_PATH LD_PREFER_MAP_32BIT_EXEC LD_PRELOAD LD_PROFILE
unset LD_PROFILE_OUTPUT LD_SHOW_AUXV LD_TRACE_LOADED_OBJECTS
unset LD_TRACE_PRELINKING LD_USE_LOAD_BIAS LD_VERBOSE LD_WARN
exec /usr/bin/env -i ORNA_PACKAGE_MAINTENANCE=begin /usr/bin/orna

unset GLIBC_TUNABLES LD_AUDIT LD_DEBUG LD_DEBUG_OUTPUT LD_DYNAMIC_WEAK
unset LD_ASSUME_KERNEL LD_BIND_NOT LD_BIND_NOW LD_HWCAP_MASK LD_LIBRARY_PATH
unset LD_ORIGIN_PATH LD_PREFER_MAP_32BIT_EXEC LD_PRELOAD LD_PROFILE
unset LD_PROFILE_OUTPUT LD_SHOW_AUXV LD_TRACE_LOADED_OBJECTS
unset LD_TRACE_PRELINKING LD_USE_LOAD_BIAS LD_VERBOSE LD_WARN
exec /usr/bin/env -i ORNA_PACKAGE_MAINTENANCE=complete /usr/bin/orna
```

The entry verifies that this exact one-variable environment was supplied,
then clears it immediately and performs only the retained work ADR 0017
package-lock, exact-state, atomic-write, `fsync`, verification, removal, and
purge protocol. No loader-control, locale, path, home, or configuration
variable reaches the Orna process. The entry never opens instance data, calls
a PostgreSQL entry, or starts a child. `begin` is available in the installed
predecessor before unpack; `complete` is available in the newly unpacked
executable after unpack. Package work leaves the service stopped. `orna server
upgrade` owns every data transition.

An invalid maintenance value or any additional argument follows normal public
usage and exits `2`. A selected non-root request writes the first line below;
a selected operation that cannot reach its durable commit writes the second.
Both exit `1`:

```text
orna: package maintenance requires root
orna: package maintenance did not complete
```

Source checking cannot select this entry because it has public arguments.
Tests must also supply a hostile maintenance variable to every public command
shape and prove that only the exact root, zero-public-argument entry selects
package work.

## Embedded-engine upgrades

The public maintenance command remains exactly:

```text
orna server upgrade
```

It accepts no argument or flag, requires the service to be stopped, and
retains work ADR 0017's package lock, instance lock, durable transition,
copy-only generation, activation-commit, interruption, re-entry, and
no-automatic-repair rules.

### Same-major releases

A same-major Orna release embeds only the candidate PostgreSQL code. Its signed
distribution manifest binds the candidate embedded-engine manifest, the exact
accepted predecessor engine identities, the supported forward edges, and the
final Orna ELF.

When an instance records an accepted predecessor, upgrade retains its current
generation and:

1. verifies the stopped instance and predecessor identity;
2. inspects `pg_control` through a typed linked read-only entry;
3. durably records `same_major_candidate_may_open`;
4. forks the candidate postmaster entry from the current Orna ELF;
5. runs migrations, accepted-standard verification, and active recovery;
6. fast-stops and waits for the candidate;
7. records the candidate identity with `activation_committed=false`; and
8. leaves the next `server run` to commit activation before readiness.

After `same_major_candidate_may_open`, no older Orna release may open the
generation automatically. Re-entry is forward-only. A normal run that sees an
accepted predecessor without a durable transition writes this exact line and
exits `1`:

```text
orna: the default Orna instance needs an offline upgrade
```

### Future major releases

A release that supports a PostgreSQL major transition embeds exactly two
engine sets: the accepted source major and the candidate major. Its
distribution manifest binds both embedded-engine manifests and one exact
transition edge.

Each major and executable-role closure is first flattened into one
relocatable object. The build generates a rename map from every defined global
symbol, including bundled third-party definitions, and applies a
major-and-role prefix. Unresolved accepted base-system symbols remain
unchanged. The final link proves that defined-symbol sets are disjoint except
for one explicit bridge allow-list and exports one typed entry per role.

The pristine Rust upgrade supervisor remains single-threaded and is the only
process that forks upgrade roles. It activates exactly one role in each
freshly forked process. Pipes carry input, output, diagnostics, and status. No
role accepts a bindir, executable path, `PATH`, shell command, or program name.
The supervisor first forks the coordinator with the fixed request and response
pipes, then services that protocol while monitoring the coordinator and every
role child. It does not wait synchronously for the coordinator before serving
requests.

The candidate major's adapted `pg_upgrade` coordinator retains upstream check
and copy semantics but replaces every program launch with one typed linked
role request to that supervisor. The coordinator never forks a role. A fixed
binary pipe protocol carries a protocol version, request tag,
source-or-candidate engine tag, closed role tag, and big-endian invocation
ordinal. The supervisor owns all data-directory, support-root, descriptor, and
fixed-option facts, validates the next allowed request against the durable
transition phase, then forks the selected entry.

A `start-server` request forks the selected postmaster, waits through its
private readiness channel, and responds `ready` with a supervisor-issued
big-endian live-role handle while that postmaster continues to run. While one
server handle is live, the coordinator can issue ordered `run-role` requests
for the exact dump, restore, SQL, or vacuum clients allowed against that
engine. Each request names the handle, receives its own fresh child, and
responds only after that client exits. Offline control-data, reset-WAL, and
initialisation roles require no live handle. A `stop-server` request names the
handle and fixed fast or immediate shutdown mode; the supervisor signals the
postmaster, waits for its complete process closure, and responds `stopped`.
The state machine permits at most one live source or candidate postmaster and
requires it to stop before starting the other engine.

Every response echoes the protocol version and invocation ordinal and carries
only `ready` with a handle, `completed`, `stopped`, `exited`, `signalled`, or
`protocol-failure`. A coordinator exit, closed request pipe, or malformed
request while a server handle is live makes the supervisor fast-stop, then
immediate-stop and wait for that server before it fails the transition. An
unknown handle, duplicate, skipped, crossed-engine, wrong-server, or
out-of-phase request fails before another fork.

The embedded role set includes the source and candidate server entries plus
the exact control-data, read-only and mutating reset-WAL, initialisation, dump,
restore, SQL, and vacuum work required by that transition. Each child receives
the support root for its selected engine and activates that engine and role for
its complete lifetime. The coordinator fixes every option itself and exposes
no general reset-WAL repair entry. None is a file or public command.

The major transition retains work ADR 0017's sequence: create a new generation,
initialise with the candidate entry, run the equivalent of `pg_upgrade
--check`, run copy mode, reject link, clone, copy-file-range, required rebuild,
required reindex, and post-upgrade scripts, verify the candidate through Orna
recovery, then switch with `activation_committed=false`. Rollback is permitted
only before activation commit, and no manifest-named generation is deleted
during re-entry.

Upgrade uses these exact diagnostics after global usage. Each exits `1`:

```text
orna: server upgrade must run as the orna service account
orna: package maintenance is incomplete
orna: the default Orna instance is not installed
orna: the default Orna instance is invalid
orna: the default Orna instance is running
orna: this Orna executable cannot upgrade the installed PostgreSQL engine
orna: PostgreSQL upgrade did not complete
```

After global command-shape validation, checks occur in this exact order:

1. real, effective, primary, and supplementary service-account identity;
2. shared package lock and exact ready package state;
3. installed default-instance presence;
4. fixed paths, manifests, generation, engine identity, control data, and any
   durable transition record;
5. stopped service, absent live ready record, and acquirable instance lock;
6. completed/no-op state, accepted predecessor, embedded engine set, and exact
   supported forward edge; and
7. role protocol, role completion, migration, verification, synchronisation,
   and durable commit.

The first six checks map in order to the first six diagnostic lines above. A
completed or no-op transition at check 6 exits `0`. A predecessor outside the
signed forward edges, a missing required source engine, or a downgrade uses
the sixth line. A role exit, signal, malformed response, interrupted
transition, migration failure, candidate recovery failure, or durable-write
failure after the role boundary uses the final line. A malformed or
impossible pre-existing transition record is an invalid instance at check 4.
No later failure can replace an earlier diagnostic.

A no-op or completed upgrade exits `0`. `SIGINT` or `SIGTERM` requests fast
shutdown of an active role, waits for it, leaves the durable transition
re-enterable, and exits `1`.

## Complete lifecycle proof

The first production package gate must establish all of these facts on a
fresh network-disabled Debian 12 amd64 machine:

* the product payload installs one executable, `/usr/bin/orna`, and no
  PostgreSQL executable, Orna helper copy, archive, object, or shared object;
  dpkg control scripts contain no PostgreSQL code and cannot be invoked as a
  product command;
* final Orna ELF bytes and the embedded support bundle are reproduced twice
  and bound through the embedded-engine and distribution manifests;
* initialisation, bootstrap, normal service, private kernel work, backend
  shell, same-major upgrade, shutdown, and restart execute no PostgreSQL
  program or shared object and perform no runtime download;
* every PostgreSQL-role PID maps to the installed Orna inode, and exactly one
  thread exists before every direct linked-entry fork;
* every linked role inherits the executable-load filter, and process tracing
  observes no `execve`, `execveat`, executable memory-backed file, or later
  executable mapping; the proof invokes one harmless x32-numbered syscall
  after filter installation and requires `EPERM`, treating `ENOSYS`, success,
  a signal, or any other error as failure;
* support materialisation accepts only the manifest inventory and exact
  data-only modes and rejects missing, changed, extra, linked, executable, and
  raced members before engine entry;
* bootstrap creates checksum-enabled storage, the fixed templates, database,
  superuser, socket, authentication, exact seven-row bootstrap collation
  catalogue, and standard-backed Orna state without PL/pgSQL or host locale
  enumeration; a hostile or absent `locale` executable and different installed
  locales do not change the result;
* every forbidden SQL form above fails with exact SQLSTATE and text before
  file access, catalogue mutation, process creation, or event-trigger work,
  including nested, prepared, role-changed, and multi-statement attempts;
* readiness, controlled stop, immediate escalation, unexpected postmaster
  exit, cgroup cleanup, systemd restart, crash recovery, and no in-process
  restart preserve work ADR 0017's lifecycle contract;
* package maintenance selects only the exact zero-public-argument environment
  entry, rejects non-root selection before work, removes the complete pinned
  loader-variable set before a new ELF is loaded, starts Orna with only the
  selector despite hostile loader, locale, path, and home values, retains
  transaction exclusion, leaves the service stopped, and installs no helper;
* source check does not enter a PostgreSQL, package, support-data, instance,
  network, or process path; and
* same-major fault injection preserves every forward-only boundary. A later
  two-major package adds symbol-disjointness, separate source and candidate
  support roots, supervisor-brokered role requests, one-role-per-process,
  copy-only, pre-commit rollback, and post-commit rollback-rejection proof.

## PostgreSQL source integration

The immutable upstream checkout contains no Orna commits. The prepared source
is the upstream tree plus this exact Orna-owned input sequence. Git records the
input bytes and modes. The table documents their responsibilities, and the
zero-padded patch filenames define application order.

| Input | PostgreSQL-relative scope | Required result |
| --- | --- | --- |
| `postgresql/overlays/18.4/src/backend/main/orna_embedded.c` | new backend runtime source | Add the one-shot support root, initialisation capabilities, and executable-load filter, including unconditional x32-number rejection. |
| `postgresql/overlays/18.4/src/include/orna_embedded.h` | new private header | Declare the typed backend, initialiser, support-root, capability, and filter interfaces used by patched upstream files. |
| `postgresql/patches/18.4/0001-linked-backend-entry.patch` | backend Makefile and `main.c` | Compile the runtime overlay, add the private backend entry, and emit the deterministic one-member backend archive. |
| `postgresql/patches/18.4/0002-fixed-postmaster-paths.patch` | postmaster and path code | Remove executable-relative postmaster, share, and `pkglib` path authority. Use only the verified support root and fixed executable identity. |
| `postgresql/patches/18.4/0003-executable-code-guards.patch` | utility dispatch, function creation, dynamic loading | Reject executable SQL and dynamic loading before hooks, privilege, path, file, or catalogue work. |
| `postgresql/patches/18.4/0004-linked-check-and-bootstrap.patch` | backend child output and initdb | Replace external configuration-check and bootstrap processes with the hardened fresh linked-child runner, and suppress only expected initialisation-child output. |
| `postgresql/patches/18.4/0005-complete-initialiser.patch` | backend and initdb Makefiles, backend `main.c`, and initdb | Add fixed initialiser authority, deterministic bootstrap facts, three linked single-user phases, the typed initialiser entry, the namespaced initialiser archive, and the dual-archive target. |
| `postgresql/patches/18.4/0006-empty-automatic-configuration.patch` | initdb | Create `postgresql.auto.conf` as an exact empty file before the first linked configuration check. Keep it empty for the completed cluster so the check child emits no missing-file log and SQL cannot inherit an alternate configuration authority. |

A patch can change more than one upstream file only when those edits implement
one indivisible concern and cannot be moved into an added overlay file. Patch
regeneration must start from the exact upstream gitlink, apply the accepted
predecessors, and use stable semantic context. The proof applies every patch
serially with `--fuzz=0` and rejects offsets, fuzz, rejects, or an unexpected
prepared-source inventory.

## Initial implementation sequence

The tarball-and-patch builder remains the active proof path until the
submodule-backed build has produced byte-identical backend and initialiser archives,
support bundle and manifest, symbol evidence, licence, and deterministic entry
probe output. The new path must also complete its own linked initialiser and
postmaster lifecycle proof twice. The legacy builder is not claimed to provide
that later lifecycle evidence. The new top-level `postgresql/` build is
exercised but is not the selected proof path during this parallel gate. A committed parity gate
precedes the cut-over. The cut-over then selects one build path atomically and
deletes the legacy builder entry point in the same commit, with no automatic
fallback. The inert legacy recipe and patch files are removed only after the
new path is green. The obsolete external-runtime builder remains until the
linked initialisation and postmaster lifecycle proof is green on the new path.

Each row is one buildable, reviewable Conventional Commit. Each commit changes
only the exact one to three files listed in that row.

A change only to patch hunk context is a representation correction, not an
embedded-engine change, when `--fuzz=0` application produces a byte-identical
final source tree and every deterministic output remains byte-identical. Git
records the new patch bytes and the embedded manifest records their digest,
but the embedded identity does not advance. Any applied source or
deterministic-output byte change fails this exception and requires a new
identity.

### Completed prototype history

These completed rows record how the one-ELF boundary and native seams were
proved. Until the atomic native cut-over, the legacy builder remains the
temporary selected proof authority. After that cut-over, rows that name the
source tarball, copied source, or parent-repository patches are superseded
prototype history and are not current source or build authority.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(architecture): require one Orna executable` | `docs/decisions/0017-bundled-postgresql-runtime.md`; `docs/decisions/0019-embedded-postgresql-engine.md`; `docs/decisions/README.md` | Supersede only ADR 0017's external PostgreSQL program-distribution authority and accept the one-executable boundary. |
| `docs(plan): align the embedded PostgreSQL prototype` | `TODO.md` | Replace signed-runtime, private-`psql`, duplicate-helper, and executable-tree work with the accepted native entry probe and embedded-lifecycle sequence. |
| `build(postgres): define embedded PostgreSQL inputs` | `packaging/postgresql/embedded-build.toml`; `packaging/postgresql/embedded-postgresql-18.4.patch`; `packaging/postgresql/build-embedded.sh` | Pin the source, patch, Debian 12 amd64 non-`EXEC_BACKEND` build, compiler and linker inputs, static symbol closure, and first private entry shim. |
| `build(postgres): prove a linked PostgreSQL entry` | `.github/workflows/postgresql-embedded.yml`; `packaging/postgresql/build-embedded.sh`; `packaging/postgresql/embedded-build.toml` | Build the static archive and native C entry probe twice, run the exact initial proof, and publish only review evidence and static link inputs. |
| `docs(architecture): select the embedded lifecycle` | `docs/decisions/0019-embedded-postgresql-engine.md`; `docs/decisions/0014-host-only-backend-shell.md`; `docs/decisions/0018-offline-source-check.md` | Use tracer evidence to select the process, initialisation, support-asset, shell, upgrade, error, command, lifecycle, and complete proof contracts before production code. |
| `refactor(postgres): remove production PL/pgSQL` | `crates/orna-kernel-postgres/migrations/0002_revisions.sql`; `crates/orna-kernel-postgres/migrations/0005_owner_qualified_reference_targets.sql`; `crates/orna-kernel-postgres/tests/bootstrap.rs` | Replace both production `DO` blocks with declarative, fail-closed SQL and prove fresh and every retained migration path without a procedural language. |
| `chore(postgres): stage embedded patch series one` | `packaging/postgresql/embedded-postgresql-18.4/0001-linked-backend-entry-and-archive.patch`; `packaging/postgresql/embedded-postgresql-18.4/0002-embedded-runtime-capabilities-and-seccomp.patch`; `packaging/postgresql/embedded-postgresql-18.4/0003-fixed-postmaster-support-paths.patch` | Add the linked backend entry and flattened archive, embedded runtime capabilities and executable-load filter, and fixed postmaster, share, and `pkglib` paths. Each zero-context patch applies after its exact predecessor with `--fuzz=0`. The monolithic patch remains the sole active builder input. |
| `chore(postgres): stage embedded patch series two` | `packaging/postgresql/embedded-postgresql-18.4/0004-executable-sql-and-loader-guard.patch`; `packaging/postgresql/embedded-postgresql-18.4/0005-initdb-direct-check-and-bootstrap.patch`; `packaging/postgresql/embedded-postgresql-18.4/0006-fixed-bootstrap-collation-and-no-plpgsql.patch` | Add the executable-SQL and dynamic-loader guard, direct linked `--check` and `--boot` child runner with fixed UTC and initialisation-child output suppression, and exact seven-collation bootstrap with every PL/pgSQL load declaration, implementation, and call removed. Each zero-context patch applies after its exact predecessor with `--fuzz=0`. The monolithic patch remains the sole active builder input. |
| `chore(postgres): stage embedded patch series three` | `packaging/postgresql/embedded-postgresql-18.4/0007-initdb-fixed-support-paths.patch`; `packaging/postgresql/embedded-postgresql-18.4/0008-initdb-single-user-entry-and-archive.patch` | Add fixed initialiser support paths, direct linked `--single` setup, the typed initialiser ABI, public-to-private encoding bridges, and the flattened initialiser archive. Patch `0008` first activates the complete initialiser archive build. Both zero-context patches apply after their exact predecessors with `--fuzz=0`. The monolithic patch remains the sole active builder input. |
| `build(postgres): freeze embedded patch series` | `packaging/postgresql/embedded-build.toml`; `packaging/postgresql/build-embedded.sh`; `packaging/postgresql/embedded-postgresql-18.4.patch` | Delete the monolithic patch and select the staged series atomically. Make the ordered TOML patch list the sole path, order, and SHA-256 authority. Freeze each listed patch, verify its digest, dry-run and apply it after its exact predecessor with `--fuzz=0`, and bind the ordered list and frozen bytes in the deterministic manifest. Preserve the linked archives, symbols, support bundle, probe, licence, caller-owned output, and no-executable output. |
| `fix(postgres): remove argv0 service authority` | `packaging/postgresql/embedded-postgresql-18.4/0009-remove-argv0-locale-and-service-authority.patch`; `packaging/postgresql/embedded-build.toml` | Append one pinned patch to the ordered recipe list. Remove both linked calls to `set_pglocale_pgservice`, clear `PGSYSCONFDIR` and `PGLOCALEDIR` before each private entry, preserve the fixed backend locale and initialiser `locale=C`, and make no builder-code change. |

### Upstream submodule and Orna integration transition

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(architecture): select upstream PostgreSQL patch management` | `docs/decisions/0019-embedded-postgresql-engine.md` | Select the immutable official upstream gitlink, top-level Orna integration module, added-file overlays, sparse ordered patches, prepared-source build, and explicit source-update interface. |
| `docs(plan): sequence PostgreSQL patch-managed integration` | `TODO.md` | Replace the fork import and pointer-advance sequence with the upstream reset, overlay and patch staging, top-level build, parity gate, and legacy removal. |
| `build(postgres): pin upstream PostgreSQL 18.4` | `.gitmodules`; `third_party/postgresql` | Replace the temporary fork URL and patched tip with the official PostgreSQL remote and exact unmodified `REL_18_4` commit. |
| `build(postgres): add embedded PostgreSQL overlays` | `postgresql/overlays/18.4/src/backend/main/orna_embedded.c`; `postgresql/overlays/18.4/src/include/orna_embedded.h` | Add the complete reviewed Orna-owned source files outside the submodule, with no generated or duplicate source authority. |
| `build(postgres): add embedded patches one` | `postgresql/patches/18.4/0001-linked-backend-entry.patch`; `postgresql/patches/18.4/0002-fixed-postmaster-paths.patch`; `postgresql/patches/18.4/0003-executable-code-guards.patch` | Add the first three sparse existing-file patch concerns and prove serial zero-fuzz application after overlay installation. |
| `build(postgres): add embedded patches two` | `postgresql/patches/18.4/0004-linked-check-and-bootstrap.patch`; `postgresql/patches/18.4/0005-complete-initialiser.patch` | Add the final two sparse existing-file patch concerns and prove the complete prepared source matches the accepted prototype source. |
| `build(postgres): define the PostgreSQL engine build` | `postgresql/Makefile`; `postgresql/Containerfile` | Add the only build and maintenance interface. Use the gitlink as upstream identity, Git as overlay and patch byte authority, numbered filenames as patch order, and the Containerfile as the Debian package and toolchain authority. Prepare source only beneath caller-owned `target`; provide the explicit full-commit `update-source` target. |
| `build(postgres): generate embedded support data` | `postgresql/support_bundle.py`; `postgresql/Makefile` | Generate and verify the deterministic data-only support bundle from the prepared source and out-of-tree build. |
| `build(postgres): emit the embedded engine manifest` | `postgresql/engine_manifest.py`; `postgresql/Makefile` | Record the upstream gitlink and inventory, overlay and patch bytes and order, prepared-source inventory, every checked-in build and proof input, prepared image digest, package closure, archives, symbols, support data, and licence. The manifest records evidence and is not build configuration. |
| `fix(postgres): create empty automatic configuration` | `postgresql/patches/18.4/0006-empty-automatic-configuration.patch`; `TODO.md` | Add the smallest existing-file correction discovered by the live linked initialiser. Require the first configuration-check child to start without a missing-file diagnostic and retain exact empty `postgresql.auto.conf` bytes after initialisation. |
| `test(postgres): add the linked lifecycle probe` | `postgresql/lifecycle_probe.c`; `postgresql/Makefile` | Add the unpublished one-ELF initialiser, postmaster, raw-pgwire, hostile-authority, and controlled-shutdown tracer as ordinary reviewed C source and bind it as a build input. |
| `test(postgres): verify the embedded lifecycle` | `postgresql/verify_lifecycle.py`; `postgresql/Makefile` | Gate the exact cluster, support, process, filter, network, mapping, trace, output, and two-build reproducibility contracts and bind the verifier as a build input. |
| `build(postgres): prove patch-managed engine parity` | `.github/workflows/postgresql-embedded.yml`; `postgresql/Makefile` | While the legacy builder remains selected, run both paths and require byte-identical archives, support data, symbol evidence, licence, and deterministic entry-probe output; require the top-level path's full lifecycle proof twice. |
| `build(postgres): select the PostgreSQL engine build` | `.github/workflows/postgresql-embedded.yml`; `justfile`; `packaging/postgresql/build-embedded.sh` | Check out the exact upstream submodule with no persisted credentials, atomically select only the already-green `postgresql/Makefile`, delete the legacy builder entry point, and upload only the accepted evidence root. |
| `chore(postgres): remove legacy embedded recipe` | `packaging/postgresql/embedded-build.toml` | Remove the inert tarball recipe after the top-level path is the only callable proof. |
| `chore(postgres): remove legacy patches one` | `packaging/postgresql/embedded-postgresql-18.4/0001-linked-backend-entry-and-archive.patch`; `packaging/postgresql/embedded-postgresql-18.4/0002-embedded-runtime-capabilities-and-seccomp.patch`; `packaging/postgresql/embedded-postgresql-18.4/0003-fixed-postmaster-support-paths.patch` | Remove the first obsolete prototype patch group while retaining it in Git history. |
| `chore(postgres): remove legacy patches two` | `packaging/postgresql/embedded-postgresql-18.4/0004-executable-sql-and-loader-guard.patch`; `packaging/postgresql/embedded-postgresql-18.4/0005-initdb-direct-check-and-bootstrap.patch`; `packaging/postgresql/embedded-postgresql-18.4/0006-fixed-bootstrap-collation-and-no-plpgsql.patch` | Remove the second obsolete prototype patch group while retaining it in Git history. |
| `chore(postgres): remove legacy patches three` | `packaging/postgresql/embedded-postgresql-18.4/0007-initdb-fixed-support-paths.patch`; `packaging/postgresql/embedded-postgresql-18.4/0008-initdb-single-user-entry-and-archive.patch`; `packaging/postgresql/embedded-postgresql-18.4/0009-remove-argv0-locale-and-service-authority.patch` | Remove the final obsolete prototype patch group and empty legacy directory while retaining it in Git history. |
| `chore(postgres): retire external runtime work` | `.github/workflows/postgresql-runtime.yml`; `packaging/postgresql/build-runtime.sh`; `packaging/postgresql/runtime-build.toml` | Remove the obsolete executable-tree builder after the linked lifecycle proof is green, without adding a fallback or deleting review history from Git. |
| `build(workspace): reserve the embedded engine crate` | `Cargo.toml` | Exclude the not-yet-created engine crate path from the `crates/*` workspace glob so its three-file creation commit remains independently buildable and cannot create an unrecorded lockfile change. |
| `feat(postgres): expose the embedded engine boundary` | `crates/orna-postgres-engine/Cargo.toml`; `crates/orna-postgres-engine/build.rs`; `crates/orna-postgres-engine/src/lib.rs` | As an explicitly versioned standalone crate, invoke the checked-in deterministic builder into Cargo `OUT_DIR` only for its embedded feature, validate and link the resulting manifest, archive, support bundle, and licence, then expose typed initialiser, postmaster, control-data, support-materialisation, and process-filter capabilities without a raw C entry. A clean checkout has no dependency on ignored target output. |
| `build(workspace): select the embedded engine` | `Cargo.toml`; `crates/orna-server/Cargo.toml`; `Cargo.lock` | Remove the temporary exclusion, record the engine package in the workspace lock, and make a production server feature consume it. Promote `tokio`, `tokio-postgres`, and the required `nix` signal and terminal features to normal server dependencies in this same precursor while normal development can continue to use the explicit external test backend. |
| `feat(server): model the embedded instance host` | `crates/orna-server/src/embedded.rs`; `crates/orna-server/src/lib.rs`; `crates/orna-server/src/main.rs` | Add embedded identity, trusted path, support root, package lock, instance lock, manifest, generation, transition, ready-record, and one-thread fork capability types without starting PostgreSQL. |
| `test(server): prove embedded host invariants` | `crates/orna-server/tests/embedded_host.rs` | Prove identities, paths, owners, modes, filesystems, lock lifetimes, manifest durability, stale readiness, hostile environment, and exact pre-fork thread and descriptor gates. |
| `feat(server): materialise embedded support data` | `crates/orna-server/src/embedded.rs`; `crates/orna-server/src/lib.rs`; `crates/orna-server/tests/embedded_support.rs` | Materialise, synchronise, and reverify only the manifest data inventory beneath the private runtime root, with no code file or alternate authority. |
| `feat(server): initialise embedded PostgreSQL` | `crates/orna-server/src/embedded.rs`; `crates/orna-server/src/lib.rs`; `crates/orna-server/tests/embedded_initialise.rs` | Fork the linked initialiser, create checksum-enabled storage and the private database, install exact peer authentication, and prove no program execution or PL/pgSQL. |
| `feat(server): supervise embedded PostgreSQL` | `crates/orna-server/src/embedded.rs`; `crates/orna-server/src/lib.rs`; `crates/orna-server/src/main.rs` | Implement no-argument `server run`, linked postmaster startup, private readiness, kernel and standard recovery, activation commit, systemd notification, bounded shutdown, and no in-process restart. |
| `test(server): prove embedded process closure` | `crates/orna-server/tests/embedded_supervision.rs` | Prove one Orna inode for every PostgreSQL role, normal and immediate stop, postmaster failure, orphan and cgroup cleanup, restart recovery, seccomp inheritance, and no executable or shared-object load. |
| `feat(server): own package maintenance` | `crates/orna-server/src/package_maintenance.rs`; `crates/orna-server/src/main.rs`; `crates/orna-server/tests/package_maintenance.rs` | Implement argument-and-value selection followed by the exact root gate, clean-environment verification, shared public-command locks, state bytes, atomic persistence, first-install creation, and no-instance or PostgreSQL boundary without a helper executable. |
| `feat(server): replace psql with a native shell` | `crates/orna-server/src/backend_shell.rs`; `crates/orna-server/src/main.rs`; `crates/orna-server/tests/backend_shell.rs` | Replace URL, `PATH`, and process replacement with the ready-host private Unix socket, native simple-query terminal framing, cancellation, rendering, package-lock lifetime, and exact diagnostics from ADR 0014. |
| `test(postgres): reject executable SQL paths` | `crates/orna-server/tests/embedded_sql_guard.rs` | Through kernel and shell sessions, prove every forbidden utility and function form fails before file, catalogue, event-trigger, fork, or mapping effects and cannot be bypassed by prepared, nested, role-changed, or multi-statement SQL. |
| `feat(server): add embedded engine upgrade` | `crates/orna-server/src/embedded.rs`; `crates/orna-server/src/upgrade.rs`; `crates/orna-server/src/main.rs` | Implement no-argument stopped-service same-major transition, typed control inspection, forward-only durable re-entry, candidate recovery, and activation hand-off without an external tool. |
| `test(server): prove embedded upgrade transitions` | `crates/orna-server/tests/embedded_upgrade.rs` | Prove no-op, unsupported predecessor, every same-major interruption boundary, post-open forward-only behaviour, candidate recovery, exact diagnostics, and no external execution. |
| `build(debian): define the managed embedded service` | `packaging/debian/orna.sysusers`; `packaging/debian/orna.tmpfiles`; `packaging/debian/orna.service` | Create the locked account and runtime roots and apply notify, mixed-kill, timeout, cgroup, and rate-limited restart rules. |
| `build(debian): define the one-executable package` | `packaging/debian/control`; `packaging/debian/rules`; `packaging/debian/orna.install` | Reproduce and bind the final Orna ELF, install only `/usr/bin/orna` plus non-executable configuration, licence, manifest, service, and maintainer data, and reject every PostgreSQL executable, shared object, archive, object, or helper copy. |
| `build(debian): drive the package protocol` | `packaging/debian/preinst`; `packaging/debian/prerm`; `packaging/debian/postinst` | Encode retained dpkg ordering by clearing the pinned loader-variable set with shell builtins, replacing the script through absolute `/usr/bin/env -i`, and supplying only the private selector. Perform no data transition and leave the service stopped. |
| `test(debian): prove package transaction exclusion` | `packaging/debian/tests/package-install.sh` | Prove first install, update, downgrade rejection, repair, removal, purge, reinstall, reader conflicts, commit-boundary states, no helper, and fault-injected fail-closed re-entry. |
| `test(debian): prove one executable owns PostgreSQL` | `packaging/debian/clean-machine-test.sh`; `.github/workflows/debian-package.yml` | On an offline hostile Debian 12 machine, prove the complete lifecycle matrix, exact installed inventory, final ELF and manifest identity, every PostgreSQL `/proc` identity, native shell, restart recovery, and absence of a PostgreSQL executable or shared object. |

The obsolete external-runtime builder remains reviewable Git history until the
linked initialisation tracer is green. The explicit retirement row then
removes its live files. It cannot become a fallback or a production input.

## Retained work ADR 0017 contracts

This record does not discard work ADR 0017's unrelated accepted host and
instance rules. The following contracts remain accepted unless the later
embedded-lifecycle amendment changes them explicitly:

* the unprivileged `orna` service account and its ownership boundary;
* one default managed instance, instance locking, durable readiness, and
  fail-closed recovery;
* private Unix-socket peer authentication and no PostgreSQL TCP listener;
* foreground service ownership, signal handling, bounded shutdown, and
  process cleanup;
* transactional package state, update exclusion, removal, and purge rules;
* exact host command shapes, including `orna server upgrade`; and
* clean-machine installation and hostile-host proof.

Work ADR 0017's implementation sequence is not authoritative after its
deterministic external-runtime build row. The external acceptance, publication,
runtime-tree verification, private executable-tree, and signed-tree package
rows cannot proceed.

## Rejected alternatives

The separate signed PostgreSQL executable tree from work ADR 0017 is rejected
because it creates PostgreSQL binaries outside Orna.

An Orna-owned PostgreSQL fork as the selected submodule is rejected. It moves
Orna source authority into a second repository, makes review span two commit
histories, and hides added integration code from the main Orna tree.

A monolithic PostgreSQL patch is rejected. New Orna files remain ordinary
reviewed files beneath `postgresql/overlays/`, and the small ordered patch set
contains only changes that must edit existing upstream files.

Extracting an embedded PostgreSQL executable to disk or a Linux memory-backed
file is rejected because it changes packaging, not the runtime boundary.

A single-process PostgreSQL rewrite is rejected because it removes the
postmaster and worker isolation used by the selected engine.

Private self-re-execution of `/usr/bin/orna` for PostgreSQL roles is rejected
for the first Linux target. It would require the `EXEC_BACKEND` state-transfer
model, add a private command dispatcher, and provide no benefit over the
selected inherited non-`EXEC_BACKEND` fork path.

Installing stock PL/pgSQL under the name `$libdir/plpgsql` is rejected because
it restores the dynamic shared-object authority that this record removes. A
future statically linked procedural language requires its own built-in
registration, migration, security, and proof decision.

A PostgreSQL WebAssembly module is rejected for the first production engine.
It requires a new process, shared-memory, filesystem, signal, and durable-write
contract and does not preserve the selected native PostgreSQL model.

## Precedence

This record supersedes only these parts of work ADR 0017:

* the separate private PostgreSQL executable and shared-object tree;
* accepted-runtime records, PostgreSQL release keys, detached runtime
  signatures, protected runtime publication, and runtime-tree verification;
* executable discovery, absolute PostgreSQL program paths, private PostgreSQL
  `PATH`, and execution of `postgres`, `psql`, `initdb`, `pg_upgrade`, or
  another PostgreSQL binary;
* retention and ingestion of signed PostgreSQL executable trees; and
* the duplicate `/usr/lib/orna/libexec/orna-package-maintenance` executable and
  its byte-identity mechanism, while retaining its package transaction rules;
* implementation rows that require one of those rejected authorities.

The retained ADR 0017 contracts above continue to have precedence except where
the selected lifecycle in this record replaces them explicitly.

For work ADR 0014, this record retains only its command shape, trusted-terminal
requirement, host-identity boundary, raw-administration boundary, no-elevation
rule, and prohibition on writes before attachment. It supersedes installed
`psql` discovery, process replacement, URL and `PATH` authority, session
diagnostics, and exit behaviour. The amended ADR 0014 defines the native
session, diagnostic, and exit contracts selected here.

Work ADR 0018's exact command and global usage contracts remain accepted,
including `orna server upgrade`. Its implementation waits for the corrected
embedded server dispatcher, upgrade command, and backend shell rather than the
rejected executable-tree rows.

This record preserves work ADR 0001's private PostgreSQL kernel and
no-public-pgwire rule. It preserves work ADR 0004's protected schemas, stable
physical identities, transactional apply, and fail-closed recovery. It does
not change source, catalogue, standard-library, or active-revision authority.

Work ADR 0017's rejection of an in-process PostgreSQL fork does not reject
this record's static link. PostgreSQL engine roles may execute statically
linked code from the Orna ELF in separate operating-system processes. A
single-process PostgreSQL rewrite remains rejected.

## References

* [PostgreSQL 18 connection process model](https://www.postgresql.org/docs/18/connect-estab.html)
* [PostgreSQL 18 WAL configuration](https://www.postgresql.org/docs/18/wal-configuration.html)
* [PostgreSQL 18.4 server entry point](https://github.com/postgres/postgres/blob/REL_18_4/src/backend/main/main.c)
* [PostgreSQL 18.4 postmaster](https://github.com/postgres/postgres/blob/REL_18_4/src/backend/postmaster/postmaster.c)
* [PostgreSQL 18.4 process launcher](https://github.com/postgres/postgres/blob/REL_18_4/src/backend/postmaster/launch_backend.c)
* [PostgreSQL 18.4 initialisation source](https://github.com/postgres/postgres/blob/REL_18_4/src/bin/initdb/initdb.c)
* [PostgreSQL 18.4 utility dispatcher](https://github.com/postgres/postgres/blob/REL_18_4/src/backend/tcop/utility.c)
* [PostgreSQL 18.4 function creation](https://github.com/postgres/postgres/blob/REL_18_4/src/backend/commands/functioncmds.c)
* [PostgreSQL 18 protocol flow](https://www.postgresql.org/docs/18/protocol-flow.html)
* [PostgreSQL 18 `pg_upgrade`](https://www.postgresql.org/docs/18/pgupgrade.html)
* [PostgreSQL licence](https://www.postgresql.org/about/licence/)
