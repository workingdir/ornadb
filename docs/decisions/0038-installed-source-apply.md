# ADR 0038: Installed Source Apply Activates One Complete File

**Status:** Accepted

## Decision

The first installed source mutation command is exactly:

```text
orna source apply <file.orna>
```

It treats one regular UTF-8 file as the complete application source. It checks
that source against the current active application catalogue and the accepted
standard library, prepares one candidate with the recovered active
`RevisionPair` as its expected base, and applies that candidate to the running
default embedded database.

The command is a live database operation. It must run as the installed `orna`
operating-system service account and attach through the fixed private Unix
socket selected by `inspect_ready_embedded_host`. It accepts no database URL,
instance name, environment selector, standard input, directory, glob, second
file, option, or remote PostgreSQL connection. It does not start, repair,
upgrade, or replace the installed service.

The path token, regular-file check, exact byte read, UTF-8 decoding, logical
source path, and diagnostic rendering are the same as work ADR 0018. Source
apply does not change the offline operation or empty-catalogue semantics of
`orna source check <file.orna>`.

## Active-base and transaction contract

Source apply performs these operations in order:

1. validate the exact command and path shape;
2. read and decode the exact source file;
3. verify and retain the ready embedded host;
4. recover one complete active database revision;
5. verify that its pinned standard-library context equals the accepted
   retained standard library, then reconstruct its checked standard source;
6. call `check_standard_application` with the one-unit source bundle, the
   active application catalogue, and that checked standard library;
7. stop on any diagnostic without calling preparation or PostgreSQL apply;
8. call `prepare_standard_application` with the recovered active
   `RevisionPair` as the expected base;
9. construct the bounded success document before database mutation;
10. call `PostgresKernel::apply_source_apply`; and
11. write the success document only after apply has committed, recovered the
    exact candidate, and closed its PostgreSQL session successfully.

The recovered pair in step 4 is the expected base required by work ADR 0003.
It is not an optional command argument. `PostgresKernel::apply_source_apply`
locks the active pair and compares it with the prepared expected base before it
installs physical changes. A concurrent winner therefore makes the other
candidate stale. A stale candidate cannot be silently rechecked or rebased in
the same command.

The existing kernel apply transaction remains the only activation seam. The
`apply_source_apply` entry point uses that transaction for physical PostgreSQL
changes, exact source storage, lossless syntax evidence, semantic catalogue
records, executable artefacts, revision status changes, active-pair
replacement, post-apply recovery, and one protected `source_apply` audit event.
The event records the fixed catalogue-health service principal, the candidate
source and catalogue revisions, and `source_apply:committed`. The event is
appended before commit. An audit append or recovery failure rolls back the
candidate activation with the rest of the transaction. Host code does not
install physical tables, persist catalogue rows, or change the active pair
itself.

## Success output

A successful command writes one compact JSON document and one final line feed
to standard output. Standard error is empty. The object keys and their order
are exact:

```json
{"source_revision":"source-revision:<canonical-id>","catalogue_revision":"catalogue-revision:<canonical-id>","functions":[{"qualified_name":["schema","function"],"function_id":"function:<canonical-id>"}]}
```

`source_revision` and `catalogue_revision` are the committed active pair
returned by kernel apply. `functions` contains every application function in
the committed catalogue. It does not contain the sealed catalogue-health
intrinsic or a standard-library function.

Each `qualified_name` is the exact resolved name-part array. An array, rather
than a dot-joined string, preserves quoted identifiers that contain dots or
other punctuation. Each `function_id` is the corresponding stable canonical
`FunctionId`. Entries are ordered first by exact qualified name parts and then
by `FunctionId`. JSON strings use the normal JSON escaping rules. There is no
indentation or other insignificant whitespace.

The output is discovery evidence, not identity authority. The database
catalogue remains authoritative. The command emits no success byte before the
commit is confirmed. If writing standard output fails after that confirmation,
the command reports failure but does not claim or attempt to roll back the
committed database transaction.

## Failure contract

Usage errors write the global usage text and exit with status `2`. Source read,
UTF-8, standard-library, and compiler-diagnostic failures retain work ADR
0018's exact lines and exit with status `1`.

A stale expected base writes this exact line, with canonical identities in the
shown order, and exits with status `1`:

```text
orna: source apply expected <source-revision> <catalogue-revision> but active is <source-revision> <catalogue-revision>
```

An invalid service identity, incomplete package, absent instance, invalid
instance, invalid embedded engine, attach failure, preparation failure, apply
failure, recovery mismatch, PostgreSQL shutdown failure, or output failure
writes one closed source-apply error line and exits with status `1`. It does
not expose PostgreSQL SQL, a socket path, an internal filesystem path, source
bytes, a private error chain, or a partly constructed success document. The
implementation decision for the host adapter must fix each exact closed line
before that adapter is merged.

## Explicit fixed-service grant

Source apply grants no authority. The installed recovery service principal
from work ADR 0035 continues to receive no application grant during bootstrap,
source checking, preparation, apply, restart, or function discovery.

The first narrow administration command is exactly:

```text
orna security grant-execute <canonical-function-id>
```

It must run as the installed `orna` operating-system service account against
the same fixed ready embedded instance. The command has one fixed grantee:
`CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID`. It accepts no principal, role, name,
revision, database, option, or environment-selected authority. The function
argument must be one canonical active application `FunctionId`; there is no
function-name lookup or prefix match.

The kernel performs the grant in one serializable transaction. It locks and
recovers the active pair and complete security snapshot, requires the exact
installed recovery principal and local-peer mapping, requires the function in
the active application catalogue, adds only
`ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, function)`, rebuilds
the complete `SecuritySnapshot`, and commits. An existing identical grant is
an idempotent success. Every other principal, membership, credential, grant,
and active revision fact remains byte-for-byte unchanged.

Success writes nothing to either standard stream and exits `0`. Failure writes
one closed diagnostic, exits `1`, and changes no security row. This command
does not add a wildcard grant, owner privilege, PostgreSQL privilege, role
membership, grant option, implicit source authority, or general security
administration surface.

## Parameter-free raw `INSERT`

The existing command remains the invocation surface:

```text
orna raw-call <canonical-function-id>
```

This decision extends its authenticated raw SERVER dispatch by one exact
mutation shape. In addition to the accepted CLIENT and SERVER `SELECT` targets,
the target can be a SERVER `INSERT` that:

* has no parameters;
* has one checked single-row `INSERT ... VALUES (...)` body;
* returns exactly one non-null `REF` column for the inserted object; and
* uses only the mutation expressions, value types, transaction mode, security
  mode, and volatility already accepted by the compiler and PostgreSQL SERVER
  INSERT implementation.

The raw adapter still sends no arguments and makes no domain decision. The
kernel recovers the active and security snapshots in one write transaction,
authorises the exact function for the authenticated local session, appends one
protected `EXECUTE` audit decision, validates the raw INSERT shape, and runs
the existing active-revision mutation plan through an internal authorised
entry. The raw path must not call the current unauthorised public
`execute_server_insert` operation or open a second transaction.

For an allowed valid target, the inserted row and allowed audit commit in the
same outer transaction. The returned object reference is emitted through the
existing canonical value envelope and frame state machine only after commit.
For an allowed target-shape or execution rejection, the kernel rolls back the
mutation savepoint, retains the allowed audit under work ADR 0032's existing
target-unavailable rule, and emits no value. A denied grant appends the normal
denied audit and never reaches target validation. Operational, commit-outcome,
driver, and shutdown failures remain fail closed and never fabricate a public
success.

This is not general mutation dispatch. Raw UPDATE and DELETE, function
arguments, multiple result columns, multiple inserted rows, SQL supplied by a
caller, and automatic grant creation remain closed.

## First installed product fixture

The first product end-to-end fixture uses source equivalent to this accepted
shape:

```sql
CREATE SCHEMA product_test;

CREATE TYPE product_test.probe AS OBJECT (
    stored BOOLEAN NOT NULL
);

CREATE SERVER FUNCTION product_test.create_probe()
RETURNS ROWS (created REF product_test.probe)
SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE
AS INSERT INTO product_test.probe AS made (stored)
VALUES (TRUE) RETURNING REF(made);

CREATE SERVER FUNCTION product_test.read_probes()
RETURNS ROWS (stored BOOLEAN)
SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
AS SELECT probe.stored FROM product_test.probe probe;
```

The installed test must use only the packaged `orna` executable and public
commands. It applies the file, parses the exact success JSON to obtain both
function identities, explicitly grants both identities, calls
`create_probe`, calls `read_probes`, restarts the installed service, calls
`read_probes` again, and compares the exact canonical output bytes. Direct
PostgreSQL SQL can inspect protected physical facts in a separate system-level
substrate smoke test, but it is not a substitute for this Orna product path.
All test fixtures, scenarios, runners, and substrate checks live below the
system-test crate. The `packaging` tree owns only the final `.deb` or `.rpm`
construction inputs and contains no test fixture, scenario, runner, or image.

## Required proof

Tests must prove:

* the installed command applies the exact one-file fixture and reports only the
  committed pair and sorted qualified-name-to-`FunctionId` mappings;
* schema and object declarations create active semantic definitions and the
  required private physical relation through the production apply transaction;
* no application grant exists after apply;
* raw calls to both functions fail before the two explicit grants and succeed
  after them;
* parameter-free raw INSERT creates one object through its compiled Orna plan,
  and raw SELECT returns its exact stored Boolean value;
* service restart preserves the active source, catalogue identities, function
  identities, explicit grants, physical row, and exact SELECT result;
* compiler failure, preparation failure, a forced failure after physical DDL,
  and post-apply recovery failure leave the complete active pair, protected
  catalogue, revision records, physical relation inventory, and data unchanged;
* the successful apply produces exactly one protected `SourceApply` event
  with the fixed `CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID`, the committed
  candidate `RevisionPair`, and `source_apply:committed`, and the recovered
  audit history returns that exact evidence;
* a failure after physical changes and before commit leaves no changed active
  pair, candidate residue, or `SourceApply` audit event, and recovery rejects a
  tampered audit row whose source and catalogue IDs do not form a real retained
  revision pair;
* two candidates prepared from one base cannot both activate: one winner
  commits and one stale loser returns the exact expected/active identities with
  no partial state;
* repeated fixed-service grant is idempotent while another function, principal,
  role, UID mapping, or wildcard is never granted; and
* failed or denied raw INSERT emits no value and leaves no inserted object,
  while its audit and transaction outcome follow the exact rules above.

The wider product suite must then add source-driven UPDATE and DELETE,
required and nullable fields, unique references, all reference delete actions,
constraint rollback, restart after each mutation, stale applies, apply and
invoke concurrency, and crash or shutdown failure boundaries. Those cases use
the same installed product seam. Parameterised mutation cases require a later
accepted raw argument contract and cannot bypass that work with direct SQL.

Normal format, strict Clippy, rustdoc, diff, similarity, workspace, focused
live PostgreSQL, raw-socket, installed-package, restart, and concurrency gates
remain required.

## Implementation sequence

Each row is one signed Conventional Commit, changes only the listed one to
three files, and keeps the repository buildable.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define installed source apply` | `docs/decisions/0038-installed-source-apply.md`; `docs/decisions/README.md` | Accept and index this complete command, output, grant, raw INSERT, atomicity, and product-proof contract. |
| `feat(server): apply one installed source file` | `crates/orna-server/src/lib.rs`; `crates/orna-server/src/source_apply.rs` | Own the ready-host, active-standard, check, prepare, apply, stale mapping, and deterministic result document behind one host interface. |
| `feat(cli): expose installed source apply` | `crates/orna-server/src/main.rs`; `crates/orna-server/tests/source_apply.rs` | Add the exact command and closed CLI output without changing offline source check. |
| `feat(postgres): grant fixed-service execution` | `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/tests/catalogue_health_identity.rs` | Add the idempotent one-function fixed-service grant transaction and prove no other authority changes. |
| `feat(cli): grant fixed-service execution` | `crates/orna-server/src/main.rs`; `crates/orna-server/src/security_admin.rs`; `crates/orna-server/tests/security_admin.rs` | Add the exact installed administration command and its closed host diagnostics. |
| `feat(postgres): dispatch parameter-free server insert` | `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/src/kernel/server_mutation_execution.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Add the internal authorised same-transaction raw INSERT path, savepoint outcome, and audit proof. |
| `test(system): exercise installed Orna data path` | `crates/orna-system-tests/tests/installed_product.rs`; `crates/orna-system-tests/fixtures/product_test.orna`; `.github/workflows/debian-package.yml` | Apply, discover, grant, insert, read, restart, and read again through the exact packaged executable. |
| `fix(postgres): enforce source-apply audit recovery` | `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/migrations/0038_source_apply_principal.sql`; `crates/orna-postgres/tests/apply.rs` | Bind the protected event to the fixed service principal, require its historical revision pair, and prove rollback and tamper rejection in the Compose-gated suite. |

## Deferred surface

This decision does not accept multi-file or directory apply, imports, source
export, watch mode, automatic rebase, an expected-base option, a database
selector, remote administration, a general principal or role command,
automatic or source-declared grants, function-name invocation, arguments, raw
UPDATE or DELETE, general mutation dispatch, arbitrary result shapes, or
direct PostgreSQL as product behaviour.

## Precedence

This decision makes the illustrative one-file `orna source apply` command in
`spec/docs/06-bootstrapping-recovery.md` concrete and narrows the directory and
database-selector proposals in `spec/api/cli.md` and
`spec/docs/37-modules-distribution.md` for the first installed workflow.

It preserves work ADR 0003's authoritative active source, expected-base
rejection, complete transactional activation, and durable recovery rules. It
preserves work ADR 0018 as a separate offline empty-application check. It
extends work ADR 0032 only for the exact authenticated parameter-free SERVER
INSERT shape above. It preserves work ADR 0035's closed health-function access
and no-application-grant rule by requiring the separate exact grant command.

For the first installed source apply, fixed-service application grant, and
parameter-free raw INSERT workflow, this accepted record has precedence.
