# ADR 0043: Canonical Raw Calls Bind One Reference INSERT Argument

**Status:** Accepted

## Decision

This decision admits one canonical object reference as the complete argument
set for an already accepted single-row SERVER `INSERT`. It does not change the
installed command:

```text
orna raw-call <canonical-function-id> <canonical-parameter-id>
```

Standard input remains exactly one complete bounded `ORV1` envelope followed
by end of file. Work ADR 0040 remains the authority for command parsing,
pre-connect input validation, statuses, `SIGINT`, the fixed local socket,
frame ordering, `ParameterId` binding, and source-apply parameter discovery.
Work ADR 0041 remains the authority for the same Reference envelope when it
selects an UPDATE or DELETE target.

The client decodes one `RuntimeValue::Reference` containing one nominal
application object `TypeId` and one `ObjectId`. It does not look up that
object, resolve a target function, inspect a field, or reinterpret the value.
The existing raw adapter and kernel argument boundary already admit exactly
one Reference. This decision changes only the authorised SERVER INSERT target
that may consume it.

This is not a new insert command, SQL endpoint, positional binding rule, name
resolver, object-creation protocol, or general invocation system.

## Raw Reference INSERT target

An authorised Reference-bearing target is available as a raw INSERT only when
the active function:

* is a SERVER function with the accepted security, transaction, and volatility
  modes from work ADR 0005;
* uses one immutable accepted `orna.server-mutation-plan` INSERT artifact;
* declares exactly one required non-null parameter;
* declares that parameter as an exact `REF` to one active application object
  type; and
* uses that parameter as its complete runtime argument set for the accepted
  single-row INSERT plan; and
* reads that parameter in at least one INSERT assignment expression, so the
  supplied reference is causally stored by the mutation.

The function may use source literals or contextual `NULL` for other accepted
INSERT values because they require no additional runtime argument. A sole but
unused Reference parameter is not an accepted raw target. The function may not
declare another parameter, receive another argument, bind by position or name,
coerce the reference, or accept a reference whose `TypeId` differs from the
declared parameter type.

The supplied `ParameterId` must equal the sole active parameter. The normal
active INSERT validator remains the authority for the function revision,
artifact, definition-reference evidence, target, values, parameter use,
argument type, result declaration, and generated object identity. No raw-only
SQL, reference lookup, or alternate mutation plan is created.

On success, the call returns one complete canonical `ORV1` reference to the
new object, exactly as the existing Boolean and parameter-free raw INSERT
paths do. The returned reference is the created object, not the supplied
reference. Its target `TypeId` is the INSERT target and its `ObjectId` is a new
nonzero identity allocated by the normal executor.

## Constraint and missing-target behaviour

The supplied reference is stored through the existing typed INSERT plan. The
active field's normal foreign key, nullability, uniqueness, and delete-action
contracts remain authoritative. The raw path performs no preflight `SELECT`
for reference existence or uniqueness.

A supplied reference whose target object does not exist is a data rejection,
not a target-shape rejection. A duplicate required unique reference produces
the existing typed `UniqueReferenceConflict`. Both failures roll back the
INSERT savepoint, retain the allowed audit decision when the outer transaction
commits, and map to protocol-1 `INTERNAL_FAILURE`. Neither failure creates a
row or exposes a PostgreSQL relation, constraint, field, object identity, or
diagnostic.

A wrong `ParameterId`, wrong reference `TypeId`, missing required argument,
extra declared parameter, unused sole Reference parameter, unsupported
function signature, invalid artifact, or invalid plan remains an authorised
raw-target rejection and maps to `TARGET_UNAVAILABLE`. Those failures also
create no row.

This decision adds no public constraint-failure category. It preserves the
four closed work ADR 0026 failure bytes.

## Authorisation, audit, and transaction order

The server admits the outer zero, Boolean, or Reference shape before it opens
PostgreSQL. For an admitted Reference, the kernel recovers one active revision
and one security snapshot in one transaction, then authorises the exact
`FunctionId` before it checks the function domain, signature, artifact,
`ParameterId`, reference `TypeId`, referenced `ObjectId`, or INSERT target.

A denied call appends one denied audit decision and returns
`EXECUTE_DENIED`. It does not disclose whether the function, parameter,
reference type, referenced object, INSERT plan, or target relation exists.

An allowed Reference-bearing health, CLIENT, SELECT, or unsupported-domain
target returns `PostgresKernelError::RawCallTargetUnavailable { function,
rule }` without opening a savepoint. UPDATE and DELETE continue through work
ADR 0041.

Only a superficial active SERVER INSERT artifact candidate opens the existing
raw INSERT savepoint in the same outer transaction. The normal INSERT
validator then decides whether the reference is the exact complete argument
set. Target validation failure rolls back that savepoint, commits the allowed
audit decision, and returns `TARGET_UNAVAILABLE`. A missing referenced object,
unique-reference conflict, other data rejection, database failure, recovery
failure, audit failure, savepoint failure, outer commit failure, driver
failure, shutdown failure, or unknown outcome returns `INTERNAL_FAILURE`.
None may fabricate a value or a clean completion.

Successful object creation and the allowed audit decision commit together. If
the outer transaction cannot commit, neither persists. The implementation
delegates to the existing transaction-scoped active INSERT executor; it does
not call the unauthorised public mutation entry, open another database session,
or start another transaction.

## Source replay and stable authority

Source apply output does not change. The existing optional `parameters` array
contains the one Reference parameter name and canonical `ParameterId`.
Binding remains by `ParameterId`, never array position or spelling.

Exact source replay preserves the complete sorted function discovery,
parameter identity, function identity, and explicit fixed-service grant. A
successful replay requires no regrant. Existing stored rows and references
remain active when the complete source is semantically unchanged. A later
accepted semantic rename can preserve an identity only through its own
identity-transition contract; spelling similarity never authorises reuse.

## Required proof

Focused PostgreSQL proof must establish:

* denial and denied audit before any INSERT-target fact is inspected;
* one allowed Reference reaches the existing active INSERT executor by exact
  `ParameterId` and nominal `TypeId`;
* a successful dependent row and its allowed audit commit together;
* wrong parameter and reference types return the generic target-unavailable
  source after authorisation, roll back the savepoint, and retain one allowed
  audit decision;
* a sole Reference parameter that no INSERT assignment reads follows that same
  target-unavailable path and creates no row;
* a missing referenced object fails internally, leaves no dependent row, and
  retains one allowed audit decision;
* a duplicate required unique reference returns the exact typed
  `UniqueReferenceConflict`, leaves exactly the first row, and retains one
  allowed audit decision for the failed call; and
* Boolean INSERT plus Reference-selected UPDATE and DELETE retain their exact
  existing behaviour and precedence.

The first installed proof uses only the exact built package, checked-in source,
public `/usr/bin/orna` commands, the installed service account, and the public
raw socket. It must:

* apply one source with an owner object, one dependent object containing an
  exact required unique owner reference, parameter-free owner creation, one
  Reference-argument dependent INSERT, and public one-column readers;
* discover the exact sorted function and parameter identities;
* prove the dependent INSERT is `EXECUTE_DENIED` before its explicit grant;
* create two distinct owners and retain their exact canonical references;
* insert one dependent row for the first owner and require a created reference
  whose target differs from the supplied owner target;
* retry with the same owner, require `INTERNAL_FAILURE`, and prove through the
  public reader that exactly one dependent row exists;
* insert the second owner successfully and prove the reader returns exactly
  the two supplied owner references without relying on row order;
* replay the exact source without regrant and prove complete function and
  parameter discovery is unchanged; and
* restart the installed service, prove both dependent rows remain, then make
  one further duplicate call through the original identities and retained
  grant and prove it still leaves exactly two rows.

A second installed proof exercises delete actions without private database
access. It creates roots and one-Reference dependants for the accepted omitted
`NO ACTION`, explicit `RESTRICT`, `SET NULL`, and `CASCADE` policies. Public
Reference readers and the accepted raw DELETE path must prove blocked deletes
roll back every effect, removing blocking dependants permits the root delete,
`SET NULL` retains a dependent with one typed null reference, and `CASCADE`
removes its dependent. Replay and restart must preserve the surviving rows,
identities, grants, and policy behaviour.

Every test line and fixture remains owned by the approved DeepSeek test
session. Production implementation, architecture, and difficult debugging
remain owned by the host GPT-5.6 model. Installed-package, replay, restart,
security-audit, strict Clippy, rustdoc, format, diff, similarity, workspace,
and live PostgreSQL gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and keeps the repository buildable and green. One RED behaviour tracer
is added immediately before the smallest production change that makes it
green. ORV5 remains deferred.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define raw reference inserts` | `docs/decisions/0043-canonical-raw-reference-insert.md`; `docs/decisions/README.md` | Accept and index the exact Reference INSERT, constraint, security, result, and proof contract. |
| `feat(postgres): bind one raw reference insert` | `crates/orna-postgres/src/kernel/server_mutation_execution.rs` | Admit exactly one Boolean or Reference argument at the existing transaction-scoped raw INSERT helper, require at least one assignment to read the sole Reference parameter, and delegate stable identity and type validation to the normal executor. DeepSeek owns inline tests. |
| `feat(postgres): dispatch one raw reference insert` | `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Route a superficial Reference-bearing INSERT candidate through the existing savepoint and prove live authorisation, audit, success, target rejection, missing-target rollback, and typed unique conflict. DeepSeek owns every test line. |
| `test(server): prove raw reference insert authority` | `crates/orna-server/tests/standard_database.rs` | DeepSeek-owned authenticated adapter proof using existing ORF1/ORV1 transport and public failure redaction. |
| `test(system): exercise installed unique references` | `crates/orna-system-tests/fixtures/product_test_unique_reference.orna`; `crates/orna-system-tests/tests/installed_product.rs` | DeepSeek-owned public dependent creation, duplicate rejection, replay, grant, and restart journey. |
| `test(system): exercise installed delete policies` | `crates/orna-system-tests/fixtures/product_test_delete_policies.orna`; `crates/orna-system-tests/tests/installed_product.rs` | DeepSeek-owned public NO ACTION, RESTRICT, SET NULL, CASCADE, rollback, replay, grant, and restart journey. |

## Deferred surface

This decision does not accept a second runtime argument, another non-Boolean
or non-Reference argument type, a Reference argument for SELECT or CLIENT,
more than one declared INSERT parameter, parameter defaults, nullable
parameters, caller-selected created-object identities, arbitrary SQL, upsert,
conflict clauses, implicit casts, a uniqueness preflight query, public
constraint diagnostics, ORV2 through ORV5 arguments, remote endpoints,
`sys.invoke`, or a general invocation system.

It does not change UPDATE or DELETE, accept general INSERT expressions, expose
private PostgreSQL names, or define automatic retry after an unknown commit
outcome. Multiple arguments and richer inputs remain dependent on the later
sealed invocation system.

## Precedence

This decision supersedes only the conflicting Reference INSERT and
non-Boolean INSERT closures in work ADRs 0027, 0032, 0033, 0038, 0040, and
0041 for the exact one-Reference SERVER INSERT scope above. It preserves their
command, protocol, transport, fixed-socket, authentication, security, audit,
cancellation, resource, result, and error-redaction behaviour outside that
scope.

It preserves work ADR 0005 as the language, artifact, execution, and result
authority; work ADR 0013 as the unique-reference authority; and work ADR 0008
as the delete-policy authority. It changes no canonical specification file and
does not advance the constructed-type or `sys.invoke` sequence in work ADRs
0036, 0039, and 0042.
