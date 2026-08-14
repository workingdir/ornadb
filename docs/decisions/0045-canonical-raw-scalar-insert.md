# ADR 0045: Canonical Raw Calls Bind Remaining ORV1 Scalar INSERT Arguments

**Status:** Accepted

## Decision

This decision admits the remaining five non-null ORV1 scalar values as the
complete argument set for an already accepted single-row SERVER `INSERT`:

* `RuntimeValue::Integer` for exact standard `INTEGER`;
* `RuntimeValue::BigInt` for exact standard `BIGINT`;
* `RuntimeValue::Float` for exact standard `FLOAT`;
* `RuntimeValue::Text` for exact standard `CHARACTER LARGE OBJECT`; and
* `RuntimeValue::Bytes` for exact standard `BINARY LARGE OBJECT`.

The existing `RuntimeValue::Boolean` INSERT from work ADR 0040 and
`RuntimeValue::Reference` INSERT from work ADR 0043 remain accepted. This
decision does not narrow or reinterpret either path.

The installed command does not change:

```text
orna raw-call <canonical-function-id> <canonical-parameter-id>
```

Standard input remains exactly one complete bounded `ORV1` envelope followed
by end of file. Work ADR 0040 remains the authority for command parsing,
pre-connect input validation, statuses, `SIGINT`, the fixed local socket,
frame ordering, `ParameterId` binding, and source-apply parameter discovery.
Work ADR 0025 remains the authority for each accepted scalar's exact ORV1
identity, payload, canonical float rule, and size bound.

The client and raw adapter do not parse a source literal or convert a value.
They decode one ORV1 envelope and retain its checked `RuntimeValue`. This is
not a general invocation system, a new INSERT command, a SQL endpoint, or an
implicit conversion boundary.

## Exact scalar INSERT target

An authorised argument-bearing target is available through this decision only
when the active function:

* is a SERVER function with the accepted security, transaction, and volatility
  modes from work ADR 0005;
* uses one immutable accepted `orna.server-mutation-plan` INSERT artifact;
* declares exactly one required non-null parameter;
* declares that parameter as the exact active standard type for the supplied
  `Integer`, `BigInt`, `Float`, `Text`, or `Bytes` value;
* uses that parameter as its complete runtime argument set; and
* has at least one INSERT assignment whose complete expression is the direct
  parameter read with the same function owner and `ParameterId`.

The supplied `ParameterId` must equal the sole active parameter identity. The
active parameter type must resolve through the pinned standard-library
revision. A source alias can resolve to the same semantic standard type, but
the alias spelling has no runtime authority. Parameter name, declaration
ordinal, payload similarity, and a PostgreSQL-compatible storage type cannot
authorise binding.

The normal active INSERT validator remains the authority for the function
revision, artifact, definition-reference evidence, target, assignments,
parameter use, argument type, result declaration, and generated object
identity. The function may use source literals or contextual `NULL` for other
accepted INSERT assignments because they require no additional runtime
argument. It may not declare a second parameter, receive a second argument,
bind by position or name, nest the parameter inside a record constructor or
another expression, or coerce the supplied value.

On success, the call returns one complete canonical ORV1 reference to the new
object. The target `TypeId` is the INSERT target. The `ObjectId` is a new
nonzero identity from the normal executor. This decision adds no scalar result
form.

## Exact value preservation

The accepted scalar crosses the raw socket, kernel argument boundary, and
PostgreSQL bind boundary without text rendering or source-literal conversion:

* `Integer` preserves the exact signed 32-bit value.
* `BigInt` preserves the exact signed 64-bit value.
* `Float` preserves the exact canonical finite IEEE-754 binary64 value and bit
  pattern. ORV1 has already normalised negative zero to positive zero and
  rejected non-finite values.
* `Text` preserves the exact UTF-8 byte sequence, including empty text,
  non-ASCII text, combining sequences, whitespace, and line endings. It does
  not normalise Unicode, trim, apply a collation, or add a terminator.
* `Bytes` preserves every byte and the exact byte length, including empty data
  and embedded zero bytes.

One exception is explicit. A `Text` value that contains U+0000 is not an
available raw INSERT target. The raw adapter admits the checked Text shape to
kernel dispatch. The kernel authorises the exact `FunctionId` first. After an
allowed audit decision, target validation rejects U+0000 inside the existing
raw INSERT savepoint, rolls back that savepoint, retains the allowed audit
decision when the outer transaction commits, and returns
`RawCallTargetUnavailable`. The public result is `TARGET_UNAVAILABLE`.

This rejection occurs before the PostgreSQL driver binds the text. It is not a
database error or `INTERNAL_FAILURE`. A denied call containing U+0000 remains
`EXECUTE_DENIED`; it does not disclose the text restriction or any target
fact.

## Closed argument and operation surface

The raw adapter admits zero arguments or exactly one non-null Boolean,
Integer, BigInt, Float, Text, Bytes, or Reference value to kernel dispatch.
This decision opens only Integer, BigInt, Float, Text, and Bytes for SERVER
INSERT. Boolean and Reference retain their existing accepted INSERT scope.
Reference-selected UPDATE and DELETE retain work ADR 0041.

These shapes remain closed:

* every typed `NULL`, including a scalar or reference null;
* enum, record, opaque, and constructed collection values;
* Decimal, UUID, Date, Time, Timestamp, Duration, Void, and every other
  unavailable or later scalar;
* more than one argument or more than one declared parameter;
* a missing required argument or a nullable or defaulted parameter;
* CLIENT, health, or SELECT calls with any argument;
* Integer, BigInt, Float, Text, or Bytes arguments for UPDATE or DELETE; and
* a scalar value whose exact type differs from the sole active parameter.

Record values retain the unauthorised transactional preflight from work ADR
0031. Other closed nonempty shapes retain the raw-adapter closure from work ADR
0027. They do not open the authenticated PostgreSQL dispatch transaction or
append an execute audit decision. No closed value gains a fallback parser,
cast, or alternate error category.

## Authorisation, audit, savepoint, and commit order

For an admitted scalar, the kernel recovers one active revision and one
security snapshot in the same transaction. It authorises the exact
`FunctionId` before it checks the function domain, signature, artifact,
`ParameterId`, parameter type, parameter use, scalar payload, or INSERT
target.

A denied call appends one denied audit decision and returns `EXECUTE_DENIED`.
It does not disclose whether the function, parameter, standard type, value
restriction, artifact, target relation, or field exists.

An allowed scalar-bearing health, CLIENT, SELECT, UPDATE, DELETE, unsupported
domain, wrong `ParameterId`, wrong scalar type, extra parameter, unused sole
parameter, invalid artifact, invalid plan, or other unsupported target returns
`PostgresKernelError::RawCallTargetUnavailable { function, rule }`. The raw
adapter maps it to the existing public `TARGET_UNAVAILABLE` failure.

Only a superficial active SERVER INSERT artifact candidate opens the existing
raw INSERT savepoint. The normal INSERT validator then decides whether the
scalar is the exact complete argument set. Target validation failure,
including Text U+0000, rolls back that savepoint. The outer transaction commits
the one allowed audit decision and no object row.

Database failure, recovery failure, audit failure, savepoint failure,
execution failure after complete target validation, outer commit failure,
driver failure, shutdown failure, or unknown commit outcome returns
`INTERNAL_FAILURE`. None may fabricate a value or a clean completion.
Successful object creation and its allowed audit decision commit together. If
the outer transaction cannot commit, neither persists.

The implementation delegates to the existing transaction-scoped active INSERT
executor. It does not call the unauthorised public mutation entry, open another
database session, start another transaction, or perform a separate lookup.
Public diagnostics and audit records contain no argument value.

## Source replay and restart

Source apply output does not change. The existing optional `parameters` array
contains the sole parameter name and canonical `ParameterId`. Its position and
spelling are discovery evidence only.

Exact source replay preserves sorted function discovery, parameter identity,
function identity, and the explicit fixed-service grant. A successful replay
requires no regrant. Existing inserted rows and their exact scalar values
remain active when the complete source is semantically unchanged. Spelling or
type similarity does not preserve an identity outside an accepted semantic
identity transition.

Service restart preserves the active source revision, parameter and function
identities, grant, stored values, and the same argument closure. A caller can
use the original discovered identities after restart without a private
database lookup.

## Required proof

Focused adapter and PostgreSQL proof must establish:

* the adapter admits exactly one Integer, BigInt, Float, Text, or Bytes value
  with its exact `ParameterId`, while Boolean and Reference remain unchanged;
* denial and denied audit occur before any parameter, type, U+0000, or INSERT
  target fact is inspected;
* each new scalar reaches the existing active INSERT executor by exact
  `ParameterId` and exact active standard type;
* successful calls preserve integer limits, bigint limits, representative
  finite float bit patterns, exact UTF-8 text, and exact arbitrary bytes;
* a wrong or unknown `ParameterId`, wrong scalar type, extra declared
  parameter, unused sole parameter, and unsupported operation return the
  generic target-unavailable source after authorisation;
* Text U+0000 returns `TARGET_UNAVAILABLE` after authorisation, creates no row,
  retains one allowed audit decision, and never reaches the driver bind;
* each target rejection rolls back the INSERT savepoint and retains one
  allowed audit decision;
* operational failures remain internal failures; and
* typed NULL, enum, record, opaque, collection, unavailable scalar, and
  multiple-argument shapes retain their exact prior closure and audit order.

The installed proof uses only the exact built package, checked-in source,
public `/usr/bin/orna` commands, the installed service account, and the public
raw socket. It does not use PostgreSQL or a private API. It must:

* apply one source that has one single-parameter INSERT and a public reader for
  each of Integer, BigInt, Float, Text, and Bytes;
* discover the exact sorted function and parameter identities;
* prove each INSERT is `EXECUTE_DENIED` before its explicit grant;
* insert boundary and representative canonical values, then require the public
  readers to return the exact values and bytes;
* prove Text U+0000 is `TARGET_UNAVAILABLE` and creates no row;
* prove the existing Boolean and Reference INSERT journeys remain unchanged;
* replay the exact source without regrant and prove complete discovery,
  authority, rows, and values are unchanged; and
* restart the installed service, use the original identities and grants for
  one further call of each new scalar type, and prove all stored values remain
  exact.

DeepSeek owns every test line, test helper, fixture, and test-only change.
Production implementation, architecture, and difficult debugging remain owned
by the host GPT-5.6 model. Installed-package, replay, restart, security-audit,
strict Clippy, rustdoc, format, diff, similarity, workspace, and live
PostgreSQL gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and keeps the repository buildable and green. One RED behaviour tracer
precedes the smallest production change that makes it green. ORV1 bytes remain
unchanged.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define raw scalar inserts` | `docs/decisions/0045-canonical-raw-scalar-insert.md`; `docs/decisions/README.md` | Accept and index the exact scalar, identity, preservation, security, closure, and proof contract. |
| `feat(postgres): bind raw scalar inserts` | `crates/orna-postgres/src/kernel/server_mutation_execution.rs`; `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Admit the five new scalar shapes, require one exact active parameter and causal INSERT use, reject Text U+0000 as an authorised target failure before driver binding, and preserve audit and savepoint classification. DeepSeek owns every test line. |
| `feat(server): dispatch raw scalar inserts` | `crates/orna-server/src/raw_client_dispatch.rs`; `crates/orna-server/tests/standard_database.rs` | Admit one checked new scalar to authenticated kernel dispatch, preserve Boolean, Reference, record preflight, and all other closures, and prove public redaction. DeepSeek owns every test line. |
| `test(system): exercise installed scalar inserts` | `crates/orna-system-tests/fixtures/product_test_scalar_insert.orna`; `crates/orna-system-tests/tests/installed_product.rs` | DeepSeek-owned public denial, grant, exact value, U+0000, replay, retained authority, and restart proof for all five new scalars. |

## Deferred surface

This decision does not accept typed NULL, enum, record, opaque, collection,
Decimal, UUID, date-time, duration, Void, or another later value as a raw
INSERT argument. It does not accept multiple runtime arguments, multiple
declared parameters, nullable parameters, parameter defaults, implicit casts,
coercions, string or numeric literal parsing, caller-selected object
identities, arbitrary SQL, upsert, conflict clauses, or a new public failure
category.

It does not add scalar arguments to CLIENT, health, SELECT, UPDATE, or DELETE.
It does not change ORV1, ORF1, source apply, result encoding, remote endpoints,
`sys.invoke`, or the later sealed general invocation system.

## Precedence

This decision supersedes only the conflicting Integer, BigInt, Float, Text,
Bytes, and non-Boolean-or-Reference INSERT closures in work ADRs 0027, 0032,
0033, 0038, 0040, and 0043 for the exact one-argument SERVER INSERT scope
above. It preserves their command, protocol, transport, fixed-socket,
authentication, cancellation, resource, result, security, audit, transaction,
and error-redaction behaviour outside that scope.

It preserves work ADR 0005 as the language, artifact, parameter, execution,
and result authority; work ADR 0025 as the canonical ORV1 scalar codec
authority; work ADR 0031 as the record preflight authority; work ADR 0040 as
the Boolean INSERT authority; work ADR 0041 as the Reference UPDATE and DELETE
authority; and work ADR 0043 as the Reference INSERT authority. It changes no
canonical specification file and does not advance ORV2 through ORV5 or
`sys.invoke`.
