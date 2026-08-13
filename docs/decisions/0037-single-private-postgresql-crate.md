# ADR 0037: PostgreSQL Uses One Private Rust Crate

**Status:** Accepted

## Context

Orna currently divides its private PostgreSQL implementation between two Rust
packages:

* `orna-postgres-engine` owns the linked C entries, static archives, embedded
  support data, and PostgreSQL control-file access; and
* `orna-kernel-postgres` owns the private Unix-socket client, migrations,
  storage, recovery, security, and SERVER execution.

This division does not describe two product capabilities or two deployment
units. The Orna server is the only normal consumer of both packages. It starts
PostgreSQL code from the Orna executable and then uses a private Unix socket to
communicate with the resulting PostgreSQL processes. PostgreSQL remains part
of Orna even though the process boundary and the socket remain necessary for
PostgreSQL isolation and supervision.

The separate package interface makes callers know which half of one private
implementation owns each operation. It also makes the workspace appear to
have a remote PostgreSQL adapter and a separate embedded engine. Neither is a
supported product seam.

## Decision

Orna uses one non-published Rust package named `orna-postgres` for all private
PostgreSQL implementation code. The package contains two internal modules:

```text
orna-postgres
  engine  linked entries, archives, support data, and control data
  kernel  private SQL transport, storage, recovery, security, and execution
```

The internal module split preserves the process-safety seam. Engine operations
that enter linked PostgreSQL remain available only through typed unsafe
process functions. Production kernel operations remain asynchronous Rust
operations over the verified private Unix socket. Development tests can use
their existing explicit external PostgreSQL configuration. The modules do not
become separate packages or public product interfaces.

`orna-server` is the only normal workspace consumer of `orna-postgres`. It
enables the `embedded` feature for the production linked engine. The
`test-hooks` feature is enabled only by the PostgreSQL package tests and the
server's focused test dependency.

The package retains the current static archive names, link order, support
archive, support manifest, engine manifest, PostgreSQL licence, build output
override, C symbols, process rules, migrations, SQL bytes, and test hooks. The
merge changes Rust package ownership only. It does not change a durable byte,
database fact, PostgreSQL argument, process, socket, authentication rule, or
public Orna command.

The private Unix socket remains the kernel transport. Orna does not replace it
with an in-process SQL call. PostgreSQL uses separate postmaster, backend, and
auxiliary processes from the same Orna executable. The socket is local process
communication inside one owned product, not a remote PostgreSQL dependency.

## Migration

The repository keeps every migration commit buildable and limits each commit
to one to three files. A short compatibility package can therefore forward the
two old package interfaces while callers move to `orna-postgres`. During the
physical move, an old package module can use an explicit Rust module path to a
source file at its final new path. Migration includes can similarly point to
the final migration path. This permits each source, migration, and test file to
move in a separate small commit without breaking either package. The temporary
package and path references are migration scaffolding only.

Each package ownership change uses an acyclic dependency inversion. The new
package first stages the final source and build module while it still forwards
the old package. It then removes that old dependency and compiles the staged
implementation directly in one manifest, library-root, and lockfile commit.
Only after that commit can the old package depend on and forward the new
package. An old package must never depend on `orna-postgres` while
`orna-postgres` still depends on it.

The migration order is:

1. create `orna-postgres` as a private compatibility package;
2. move the server to the new package in small source groups;
3. stage the linked-engine build and Rust entry module in the new package;
4. invert engine ownership, then make the old engine package a wrapper;
5. stage kernel source files behind explicit temporary module paths;
6. invert kernel ownership, then make the old kernel package a wrapper;
7. move each migration with its two compile-time include consumers;
8. move each integration test to the new package;
9. delete both old package manifests and all forwarding dependencies; and
10. require the final workspace, focused PostgreSQL, embedded lifecycle,
   package, rustdoc, strict Clippy, and similarity checks to pass.

The compatibility phase can contain three workspace packages, but only while
the new package forwards unchanged interfaces and the old packages remain the
implementation. The completed migration has one PostgreSQL package and no
forwarding package, alias package, duplicate source authority, or compatibility
feature.

When implementation ownership changes, the old engine package becomes a thin
wrapper whose `embedded` feature forwards to `orna-postgres/embedded`; it does
not retain a build script or emit native link instructions. The old kernel
package similarly forwards `test-hooks` to `orna-postgres/test-hooks`. This
prevents duplicate C symbols and preserves the existing focused test gates.
Shared kernel test support moves only after its old-package consumers have
moved or received an explicit temporary path.

## Consequences

Callers learn one PostgreSQL interface. PostgreSQL-specific implementation,
build, storage, and execution changes stay local to one package. The package
still has a strong internal process seam because Rust module privacy and typed
unsafe functions preserve it without a Cargo package boundary.

The package has more implementation files and dependencies. This is accepted
because those files already change together as one private PostgreSQL system.
Tests must continue to target the engine and kernel interfaces separately.

## Superseded implementation detail

This decision supersedes only work ADR 0019's requirement that the embedded
engine boundary use an explicitly versioned standalone
`orna-postgres-engine` crate and the related workspace-selection row. It does
not supersede the linked engine interface, one-executable boundary, source
integration, deterministic build, lifecycle, private socket, support-data,
security, or proof contracts in that decision.

Historical and future plan references that name `orna-postgres-engine` or
`orna-kernel-postgres` in work ADRs 0016, 0017, 0018, 0019, and 0030 transfer
their required behaviour to `orna-postgres`. They remain traceability records
and do not require the old package split after this migration.

## Precedence

This decision changes an internal Rust package layout only. The canonical
specification does not define Rust package boundaries. Work ADRs 0001, 0004,
0017, and 0019 remain authoritative for PostgreSQL privacy, physical storage,
process ownership, and distribution.
