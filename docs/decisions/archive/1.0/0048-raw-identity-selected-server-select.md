# ADR 0048: Raw Reference Calls Select One Projected Object Row

**Status:** Accepted

## Decision

One canonical Reference argument may enter the identity-selected SERVER
`SELECT` accepted by work ADR 0009 through the existing authenticated raw-call
boundary. This exposes a real parameterised read without adding a second SQL,
protocol, or invocation model.

The installed command does not change:

```text
orna raw-call <canonical-function-id> <canonical-parameter-id>
```

Standard input remains exactly one complete bounded `ORV1` Reference envelope
followed by end of file. Work ADRs 0040 and 0043 remain authoritative for
command parsing, pre-connect input validation, `ParameterId` discovery,
statuses, `SIGINT`, and Reference envelope bytes.

The selected function must be an active `SERVER`, `SECURITY INVOKER`,
`TRANSACTION READ ONLY`, `VOLATILITY STABLE` function with one immutable
`orna.server-plan` version-2 artifact. It must:

* declare exactly one required, non-null parameter;
* declare that parameter as the exact `REF` of the object scanned by the plan;
* bind the supplied value by the parameter's stable `ParameterId`;
* contain exactly `WHERE REF(source_alias) = selector_parameter`;
* contain no ordering term; and
* return one or more declared `ROWS` columns from the existing protocol-1
  scalar, typed-null, and Reference result subset.

The active function signature, selector owner, selector parameter, Reference
target `TypeId`, scanned object, plan, and durable definition-reference
evidence must agree exactly. No cast, coercion, default, name binding, ordinal
binding, nullable argument, or caller-selected SQL is accepted.

The normal identity-selected compiler, artifact, active-revision validator,
argument validator, typed PostgreSQL bind, result decoder, and cardinality
check remain authoritative. Raw dispatch does not create a parallel query
plan or reinterpret PostgreSQL behaviour.

## Result adaptation

The query returns zero or one row, as work ADR 0009 requires:

* an absent object returns `CALL_COMPLETED` with no value event; and
* one object returns one `RESULT_VALUES` event action for each projected cell,
  in declared projection order, followed by `CALL_COMPLETED`.

The raw result contains no row wrapper, column name, column count, table
metadata, or PostgreSQL value. Flattening is unambiguous only because the
accepted query can return no more than one row. Multi-row, multi-column results
remain closed.

Every projected value must belong to the existing ORV1 result subset:
Boolean, Integer, BigInt, Float, Text, Bytes, a compatible typed NULL, or an
object Reference. Existing value validation and standard-value-to-ORV1 null
normalisation remain exact. The executor's row, cell, variable-payload, and
16-MiB logical result limits remain authoritative.

The raw adapter already turns the owned ordered result vector into one value
event action per element. It does not gain a query-specific code path or encode
bytes itself. Existing frame sequencing, window credit, cancellation, drain,
and retained-resource rules therefore remain unchanged.

## Protected routing and transaction order

The server admits the outer zero-or-one supported argument shape before it
opens PostgreSQL. For an admitted Reference, the kernel recovers one active
revision and matching security snapshot, constructs the exact pinned
`InvocationTarget`, then authorises the requested `FunctionId` before it
inspects the domain, signature, parameter, Reference target, artifact, plan,
or selected object.

A denied call appends and commits one denied `EXECUTE` audit decision and
returns `EXECUTE_DENIED`. It leaks no target, parameter, type, plan, object, or
row fact.

After an allowed decision and its audit append, target selection uses this
order:

1. an accepted raw INSERT candidate;
2. an accepted Reference UPDATE or DELETE candidate;
3. an identity-selected SERVER SELECT version-2 candidate;
4. the retained parameter-free raw target path; and
5. unavailable target.

This order preserves the already accepted mutation routes and prevents an
artifact from being probed through multiple execution families.

Only a superficial active SERVER `orna.server-plan` version-2 candidate opens
the existing raw SELECT savepoint. Within that savepoint, the kernel validates
the complete raw target, executes the existing authorised identity-selected
entry with the supplied argument, and adapts the zero-or-one row result.

Signature, artifact, plan, evidence, argument, result-shape, cardinality, or
other pure target rejection rolls back the savepoint. The outer transaction
commits the allowed audit and returns
`PostgresKernelError::RawCallTargetUnavailable { function, rule }`, which the
raw adapter redacts to `TARGET_UNAVAILABLE`.

Database, migration, recovery, audit, savepoint, row decode, active invariant,
commit, driver, shutdown, or unknown-outcome failure remains an
`INTERNAL_FAILURE`. None may become target-unavailable, emit a partial value,
or be hidden by cancellation. A successful read and its allowed audit commit
together. The read never changes object data or the active revision.

## Source replay and restart

Source apply output does not change. The existing optional `parameters` array
continues to expose the selector's source name and canonical `ParameterId`.
The name is discovery evidence only; calls bind by stable identity.

Exact source replay preserves the complete sorted function discovery,
selector identity, function identity, explicit grant, artifact, and stored
objects. No regrant is required. Service restart preserves the same active
revision, identities, grant, and object data so the original Reference and
discovered identities remain valid.

## Required proof

Focused PostgreSQL and direct-boundary proof must establish:

* denial and denied audit precede every domain, signature, parameter, type,
  plan, object, and result fact;
* an exact active Reference and `ParameterId` select only their object;
* an absent `ObjectId` of the correct object type completes with no value;
* one selected row with several projections becomes the exact ordered value
  sequence, including a Reference projection and compatible typed NULL;
* wrong or unknown `ParameterId`, wrong Reference target, an extra declared
  parameter, a missing required argument, wrong signature, wrong artifact,
  wrong plan, invalid evidence, and unsupported result type remain exact
  target failures after authorisation;
* the direct cardinality boundary classifies more than one returned row as a
  pure target failure, while a legal live version-2 identity predicate cannot
  produce that state through the private unique object identity;
* INSERT and Reference UPDATE/DELETE retain routing precedence;
* target failure rolls back the SELECT savepoint and retains one allowed audit;
* operational failures remain internal; and
* concurrent source or security change cannot split decision, target, plan,
  execution, and result across snapshots.

Focused server proof must establish the public `CALL_ACCEPTED`, ordered value
events, empty completion, `TARGET_UNAVAILABLE`, `EXECUTE_DENIED`, flow-control,
cancellation, and private-source behaviour without adapter-side catalogue
inspection.

The installed proof uses only the built package, checked-in source, public
`/usr/bin/orna` commands, the installed service account, and the public raw
socket. It must:

* apply one object with Text and Boolean fields, one scalar-argument creator,
  and one identity-selected reader returning the object Reference, Text, and
  Boolean fields;
* discover the exact function and selector identities;
* prove the reader is denied before its explicit grant;
* create two objects and retain their distinct canonical References;
* require each Reference to return only its own exact three projected values;
* require a same-type absent Reference to complete without output;
* replay the exact source without regrant and preserve identities, authority,
  References, and rows; and
* restart, then repeat the reads with the original identities and grants.

Every test line, fixture, and test-only helper remains owned by the approved
test implementation session. Production implementation, architecture, and
difficult debugging remain owned by the host GPT-5.6 model. Normal format,
strict Clippy, rustdoc, diff, similarity, workspace, live PostgreSQL, socket,
and installed-package gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and leaves the repository buildable and green. A focused RED behaviour
tracer precedes the smallest production change that makes it green.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(sql): define raw identity-selected reads` | `docs/decisions/0048-raw-identity-selected-server-select.md`; `docs/decisions/README.md` | Accept and index the exact identity, query, result, routing, security, closure, and proof contract. |
| `feat(postgres): dispatch raw identity-selected reads` | `crates/orna-postgres/src/kernel/server_execution.rs`; `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/tests/server_execution.rs` | Select the existing version-2 query, validate its raw result boundary, execute it inside the protected SELECT savepoint, flatten only its zero-or-one row, and prove live authority and result order. Test logic remains test-session owned. |
| `test(server): prove projected raw object reads` | `crates/orna-server/tests/standard_database.rs` | Prove the authenticated raw adapter, redaction, ordered event actions, empty completion, mutation precedence, and retained protocol behaviour. |
| `test(system): exercise installed identity-selected reads` | `crates/orna-system-tests/fixtures/product_test_identity_select.orna`; `crates/orna-system-tests/tests/installed_product.rs` | Prove public create, identity read, absent object, denial, grant, replay, retained authority, and restart behaviour. |

## Deferred surface

This decision does not accept multi-row multi-column results, more than one
argument, a scalar selector, a general parameter expression, another identity
operand order, a nullable/defaulted selector, `ORDER BY`, joins, aggregates,
grouping, windows, subqueries, common table expressions, row locking, mutation,
arbitrary SQL, row/column metadata, ORV5/ORF5, `sys.invoke`, presenters, or the
ordinary invocation CLI.

It does not widen application function signatures or protocol value types.
It changes no compiler, artifact, catalogue, storage, codec, frame, socket,
source-apply, or CLI bytes.

## Precedence

For this exact Reference-bearing version-2 SERVER SELECT, this decision
supersedes the conflicting argument and multi-column closures in work ADRs
0032, 0033, 0040, 0041, 0043, and 0045. It preserves those decisions' command,
transport, authentication, audit, cancellation, resource, error-redaction,
and all unrelated target rules.

Work ADR 0009 remains authoritative for language, compiler, artifact,
definition-reference, argument, execution, cardinality, and result semantics.
This decision remains a milestone-4 raw recovery capability. It does not
advance or weaken the constructed-value and `sys.invoke` sequence in work ADRs
0036, 0039, or 0042.
