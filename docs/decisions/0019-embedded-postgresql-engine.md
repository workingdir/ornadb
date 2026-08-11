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

The first production package installs exactly one executable:

```text
/usr/bin/orna
```

The build statically links the required PostgreSQL 18.4 code into that ELF
file. It embeds the required immutable PostgreSQL support assets in the same
file. The supported target remains Debian 12 amd64 Linux.

The package and a running installation contain no separate `postgres`,
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
The build must pin the official PostgreSQL source archive and digest, every
Orna source patch, the complete build inputs, and the PostgreSQL licence. A
separate PostgreSQL accepted-runtime record, release key, detached runtime
signature, publisher, tree verifier, or signed-runtime ingestion path is not
permitted.

The embedded PostgreSQL build must be reproducible. Two isolated builds must
produce identical static link inputs and embedded support-asset inputs. One
generated embedded-engine manifest is the authority for the official
PostgreSQL source and digest, Orna source patches, C compiler and linker,
static archive, symbol closure, path remapping, and support assets.

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

This tracer does not claim initialisation, support-asset completeness,
postmaster readiness, worker operation, private SQL, crash recovery, backend
shell, or major upgrade. Each later tracer must add one complete behaviour
without adding a PostgreSQL executable or shared object.

## Initial implementation sequence

Each row is one buildable, reviewable Conventional Commit. Each commit changes
only the exact one to three files listed in that row.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(architecture): require one Orna executable` | `docs/decisions/0017-bundled-postgresql-runtime.md`; `docs/decisions/0019-embedded-postgresql-engine.md`; `docs/decisions/README.md` | Supersede only ADR 0017's external PostgreSQL program-distribution authority and accept the one-executable boundary. |
| `docs(plan): align the embedded PostgreSQL prototype` | `TODO.md` | Replace signed-runtime, private-`psql`, duplicate-helper, and executable-tree work with the accepted native entry probe and embedded-lifecycle sequence. |
| `build(postgres): define embedded PostgreSQL inputs` | `packaging/postgresql/embedded-build.toml`; `packaging/postgresql/embedded-postgresql-18.4.patch`; `packaging/postgresql/build-embedded.sh` | Pin the source, patch, Debian 12 amd64 non-`EXEC_BACKEND` build, compiler and linker inputs, static symbol closure, and first private entry shim. |
| `build(postgres): prove a linked PostgreSQL entry` | `.github/workflows/postgresql-embedded.yml`; `packaging/postgresql/build-embedded.sh`; `packaging/postgresql/embedded-build.toml` | Build the static archive and native C entry probe twice, run the exact initial proof, and publish only review evidence and static link inputs. |
| `docs(architecture): select the embedded lifecycle` | `docs/decisions/0019-embedded-postgresql-engine.md`; `docs/decisions/0014-host-only-backend-shell.md`; `docs/decisions/0018-offline-source-check.md` | Use tracer evidence to select the process, initialisation, support-asset, shell, upgrade, error, command, lifecycle, and complete proof contracts before production code. |

The obsolete external-runtime builder remains reviewable history until the
linked-entry tracer is green. The lifecycle decision then gives its exact
retirement row. It cannot become a fallback or a production input.

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

Extracting an embedded PostgreSQL executable to disk or a Linux memory-backed
file is rejected because it changes packaging, not the runtime boundary.

A single-process PostgreSQL rewrite is rejected because it removes the
postmaster and worker isolation used by the selected engine.

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

The retained ADR 0017 contracts above continue to have precedence for the
embedded lifecycle until the later accepted amendment replaces them.

For work ADR 0014, this record retains only its command shape, trusted-terminal
requirement, host-identity boundary, raw-administration boundary, no-elevation
rule, and prohibition on writes before attachment. It supersedes installed
`psql` discovery, process replacement, URL and `PATH` authority, session
diagnostics, and exit behaviour. The later lifecycle amendment must define the
replacement session, diagnostic, and exit contracts.

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
* [PostgreSQL 18.4 initialisation source](https://github.com/postgres/postgres/blob/REL_18_4/src/bin/initdb/initdb.c)
* [PostgreSQL licence](https://www.postgresql.org/about/licence/)
