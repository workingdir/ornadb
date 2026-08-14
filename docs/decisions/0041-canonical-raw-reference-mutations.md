# ADR 0041: Canonical Raw Calls Select UPDATE and DELETE by Reference

**Status:** Accepted

## Decision

This decision admits one canonical object reference as the selector for the
already accepted single-object SERVER `UPDATE` and `DELETE` forms. It does not
change the installed command:

```text
orna raw-call <canonical-function-id> <canonical-parameter-id>
```

Standard input remains exactly one complete bounded `ORV1` envelope followed
by end of file. Work ADR 0040 remains the authority for command parsing,
pre-connect input validation, statuses, `SIGINT`, the fixed local socket, frame
ordering, `ParameterId` binding, and source-apply parameter discovery.

The admitted raw argument shapes are now:

* no argument, as accepted before work ADR 0040;
* exactly one Boolean for the SERVER `INSERT` slice in work ADR 0040; or
* exactly one `RuntimeValue::Reference` for the UPDATE and DELETE targets in
  this decision.

The reference carries one nominal application object `TypeId` and one
`ObjectId`. The client does not resolve the object, inspect the target
function, or reinterpret the value. Every record argument retains the closed
transactional preflight in work ADR 0031. Every other non-empty argument shape
returns `TARGET_UNAVAILABLE` before PostgreSQL is opened, as in work ADR 0027.

This is one deeper raw-mutation module behind the existing raw-call interface.
It is not a new update command, delete command, SQL endpoint, positional
binding rule, name resolver, or general invocation system.

## Raw UPDATE target

An authorised reference-bearing target is available as a raw UPDATE only when
the active function:

* is a SERVER function with the accepted security, transaction, and volatility
  modes from work ADR 0007;
* uses one immutable `orna.server-mutation-plan` version-2 UPDATE artifact;
* declares exactly one required non-null parameter;
* uses that parameter as the plan's identity selector;
* declares that parameter as the exact `REF` of the updated object type; and
* uses only `TRUE`, `FALSE`, or a contextually typed `NULL` in every `SET`
  assignment, so the selector is the complete argument set.

The supplied `ParameterId` must equal the selector parameter and the supplied
reference `TypeId` must equal the UPDATE target. The normal active mutation
validator remains the authority for the function revision, artifact,
definition-reference evidence, target, assignments, selector, argument, and
return declaration. No raw-only SQL or alternate mutation plan is created.

If the selected object exists, the result is one complete canonical `ORV1`
reference that is byte-for-byte equal to the supplied selector value. If it
does not exist, the call succeeds with no value event and then completes. An
absent object is not an error and does not allocate a replacement identity.

## Raw DELETE target

An authorised reference-bearing target is available as a raw DELETE only when
the active function:

* is a SERVER function with the accepted security, transaction, and volatility
  modes from work ADR 0008;
* uses one immutable `orna.server-mutation-plan` version-3 DELETE artifact;
* declares exactly one required non-null parameter;
* uses that parameter as the plan's identity selector; and
* declares that parameter as the exact `REF` of the deleted object type.

The supplied `ParameterId` and reference `TypeId` must match that selector
exactly. The normal active delete validator remains the sole authority for the
function revision, artifact, definition-reference evidence, target, selector,
argument, return declaration, and declared reference policies.

If the selected object exists and is deleted, the result is one canonical
`ORV1` Boolean `TRUE`. If it does not exist, the call succeeds with no value
event and then completes. Repeating a confirmed successful DELETE with the
same reference is therefore an empty success. The deleted reference is never
returned as a live result.

## Authorisation, audit, and transaction order

The server admits the outer zero, Boolean, or Reference shape before it opens
PostgreSQL. For an admitted shape, the kernel recovers one active revision and
one security snapshot in one transaction, then authorises the exact
`FunctionId` before it checks the function domain, signature, plan,
`ParameterId`, reference `TypeId`, or selected `ObjectId`.

A denied call appends one denied audit decision and returns
`EXECUTE_DENIED`. It does not disclose whether the function, parameter,
object type, object identity, artifact, or row exists.

An allowed call appends one allowed audit decision. Reference-bearing health,
CLIENT, SELECT, INSERT, and unsupported-domain targets then return
`PostgresKernelError::RawCallTargetUnavailable { function, rule }` without
opening a savepoint. The raw adapter closes that typed source as
`TARGET_UNAVAILABLE`.

Only an active SERVER UPDATE or DELETE artifact candidate opens one savepoint
in the same outer transaction. Its signature, selector, constant-assignment,
`ParameterId`, reference `TypeId`, plan, or target validation failure rolls
back that savepoint, commits the allowed audit decision, and returns the same
generic raw-target error and public `TARGET_UNAVAILABLE`. The implementation
delegates successful validation and execution to the existing active UPDATE
or DELETE executor; it does not call the unauthorised public mutation entry,
open another database session, or start another transaction. The public error
contains no function, parameter, reference, plan, PostgreSQL, or row detail.

A typed data or integrity rejection from an otherwise valid mutation, such as
`DeleteRestricted`, remains its typed UPDATE or DELETE kernel source and maps
to `INTERNAL_FAILURE` in protocol version 1. Database, recovery, audit,
savepoint, outer commit, driver, shutdown, or unknown-outcome failures also
map to `INTERNAL_FAILURE`. None may fabricate a value or a clean completion.
This decision does not add a public constraint-failure category or change the
four closed work ADR 0026 failure bytes.

Successful mutation and allowed audit commit together. A candidate-validation
or data failure rolls back the mutation savepoint before the outer transaction
retains the allowed audit. If the outer transaction cannot commit, neither the
audit nor the mutation persists.

## Source replay and stable authority

Source apply output does not change. The existing optional `parameters` array
contains the UPDATE or DELETE selector name and canonical `ParameterId`.
Binding remains by `ParameterId`, never array position or spelling.

An exact source replay preserves the complete sorted function discovery,
selector identity, function identity, and explicit fixed-service grant. A
successful replay requires no regrant. A later accepted semantic rename may
preserve an identity only through its own explicit identity-transition
contract; spelling similarity never authorises reuse.

## Required proof

The first installed proof uses only the exact built package, checked-in source,
public `/usr/bin/orna` commands, the installed service account, and the public
raw socket. It does not inspect or mutate PostgreSQL directly.

The fixture contains one Boolean object plus parameter-free creators and
readers, one constant-assignment UPDATE whose only parameter is its object
selector, and one DELETE whose only parameter is its object selector. The
proof must:

* discover the exact sorted function and selector identities from source
  apply;
* prove reference-bearing UPDATE and DELETE are `EXECUTE_DENIED` before their
  explicit grants;
* create two distinct objects and retain their exact canonical reference
  envelopes;
* update only the first object from `TRUE` to `FALSE`, return its input
  reference byte-for-byte, and prove the second object remains `TRUE` through
  public readers without relying on row order;
* delete the updated object, return exact canonical `TRUE`, repeat the DELETE
  as an empty success, then call UPDATE with that deleted reference and prove
  another empty success while the other object remains;
* replay the exact source without regrant and prove the complete function and
  selector discovery is unchanged; and
* restart the installed service, use the original identities and retained
  grants to update and delete the surviving object, then prove the relation is
  empty.

Focused PostgreSQL proof must establish exact target selection, stable
argument binding, result conversion, authorisation-before-target precedence,
allowed-audit retention after savepoint rollback, successful row mutation,
absent-row completion, and rollback on typed target rejection. Focused server
proof must establish that exactly one Reference reaches the kernel, while
record preflight and all other argument closures remain unchanged.

Every test line and fixture remains owned by the approved DeepSeek test
session. Production implementation, architecture, and difficult debugging
remain owned by the host GPT-5.6 model. Installed-package, replay, restart,
security-audit, strict Clippy, rustdoc, format, diff, similarity, workspace,
and live PostgreSQL gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and keeps the repository buildable and green. One RED behavior tracer is
added immediately before the smallest production change that makes it green.
ORV5 remains deferred.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define raw reference mutations` | `docs/decisions/0041-canonical-raw-reference-mutations.md`; `docs/decisions/README.md` | Accept and index the exact Reference, UPDATE, DELETE, security, result, and proof contract. |
| `feat(postgres): dispatch raw reference mutations` | `crates/orna-postgres/src/kernel/server_mutation_execution.rs`; `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Add transaction-scoped authorised UPDATE and DELETE helpers, admit Reference shape, dispatch each mutation candidate in one savepoint, and prove live authorisation, audit, rows, absent objects, and rollback. DeepSeek owns every test line. |
| `feat(server): dispatch one raw reference argument` | `crates/orna-server/src/raw_client_dispatch.rs` | Forward exactly one Reference, retain record preflight and other closures, and preserve public error redaction. DeepSeek owns inline tests. |
| `test(server): prove raw reference mutation authority` | `crates/orna-server/tests/standard_database.rs` | DeepSeek-owned authenticated live adapter proof. |
| `test(system): exercise installed update and delete` | `crates/orna-system-tests/fixtures/product_test_reference_mutations.orna`; `crates/orna-system-tests/tests/installed_product.rs` | DeepSeek-owned public create, read, update, delete, replay, grant, and restart journey. |

## Deferred surface

This decision does not accept Reference arguments for INSERT, SELECT, CLIENT,
health, another function domain, or another mutation shape. It does not accept
more than one argument, parameter-valued UPDATE assignments, another selector,
general predicates, names or ordinals as bindings, object lookup outside the
selected mutation, arbitrary SQL, ORV2 through ORV5 arguments, remote
endpoints, `sys.invoke`, or a general invocation system.

The first installed proof cannot create a separate dependent reference row,
so it does not claim unique-reference, `NO ACTION`, `RESTRICT`, `SET NULL`, or
`CASCADE` behaviour through the installed product. The dependency-correct next
slice is one Reference argument for the already accepted single-row INSERT;
that later slice can exercise those constraints without ORV5.

## Precedence

This decision supersedes only the conflicting non-Boolean argument and raw
UPDATE/DELETE closures in work ADRs 0027, 0032, 0033, 0038, and 0040 for the
exact Reference mutation scope above. It preserves their command, protocol,
transport, fixed-socket, authentication, security, audit, cancellation,
resource, result, and error-redaction behaviour outside that scope.

It preserves work ADRs 0007 and 0008 as the language, artifact, execution, and
result authorities for UPDATE and DELETE. It changes no canonical
specification file and does not advance the constructed-type or `sys.invoke`
sequence in work ADRs 0036 and 0039.
