# ADR 0050: Canonical Raw Calls Update One Selected Object with One Value

**Status:** Accepted

## Decision

One installed raw call may update one selected object with one caller value.
The call uses the bounded two-argument command from work ADR 0049:

```text
orna raw-call <canonical-function-id> <canonical-parameter-id-1> <canonical-parameter-id-2>
```

One argument is the object Reference selector. The other argument is one
non-null Boolean, Integer, BigInt, Float, Text, Bytes, or Reference value.
The two command tokens and two complete ORV1 envelopes can use either order.
The active `ParameterId`, not command order, declaration order, value family,
or source name, identifies the selector and the value.

The command parser, standard-input reader, aggregate byte limit, ORV1 values,
ORF1 frames, fixed socket, protocol canonicalisation, statuses, and signal
boundary do not change. No CLI, protocol, or raw adapter production change is
required.

## Exact UPDATE target

After an allowed execute decision, a two-argument target is available as a raw
value UPDATE only when the active function:

* is a SERVER function with the accepted `SECURITY INVOKER`, transaction, and
  volatility modes from work ADR 0007;
* uses one immutable `orna.server-mutation-plan` version-2 UPDATE artifact;
* declares exactly two required, non-null parameters;
* uses one parameter as the identity selector and declares it as the exact
  `REF` of the updated object type;
* declares the other parameter as the exact active type of the supplied value;
* reads the value parameter in at least one `SET` assignment whose complete
  expression is the direct parameter read with the same function owner and
  `ParameterId`;
* does not read the selector parameter in a `SET` assignment; and
* uses only the direct value parameter, `TRUE`, `FALSE`, or a contextually
  typed `NULL` in all `SET` assignments.

The two supplied `ParameterId`s must be the complete declared parameter set.
The selector argument must be a Reference with the UPDATE target `TypeId`.
The value argument can itself be a Reference. When both arguments are
References, the selector in the immutable plan remains the only selector
authority.

The value parameter can be read by more than one assignment of its exact type.
Literal and contextual-NULL assignments can update other fields in the same
accepted statement. This decision does not accept casts, coercion, expression
nesting, record constructors, field reads, arithmetic, caller SQL, a second
value parameter, or another selector.

The existing active UPDATE validator remains authoritative for the function
revision, artifact, definition-reference evidence, target, assignments,
selector, complete argument set, exact types, required fields, result
declaration, payload limits, and prepared statement.

## Result and value preservation

If the selected object exists, the existing UPDATE executor stores the exact
supplied value and returns one canonical ORV1 Reference that is byte-for-byte
equal to the selector argument. If the object does not exist, the call emits
no value and completes successfully. It does not create a replacement object.

Integer limits, finite Float bits, UTF-8 bytes, arbitrary Bytes, Boolean, and
Reference target and object identities cross without source rendering or
conversion. Existing Reference constraints and delete policies remain
authoritative for a Reference-valued assignment.

Text containing U+0000 remains unavailable. The kernel checks it after
authorisation and complete target validation but before PostgreSQL binding.
The UPDATE savepoint rolls back, the outer transaction retains its allowed
audit, no field changes, and the public result is `TARGET_UNAVAILABLE`.
Public diagnostics and audit records contain neither argument value.

The protocol connection presents the two arguments in ascending `ParameterId`
order. Complete argument validation retains its existing order. The new
selector, direct-use, and Text checks also use that canonical order when more
than one supplied fact is invalid.

## Authorisation, routing, and transaction order

Closed outer value shapes remain rejected before PostgreSQL. For one admitted
pair, the kernel recovers one active revision and security snapshot in one
transaction, constructs the pinned `InvocationTarget`, and authorises the
`FunctionId` before it inspects the function domain, signature, parameters,
types, artifact, plan, selector, assignments, or target relation.

A denied call appends and commits one denied execute audit and returns
`EXECUTE_DENIED`. It discloses no target or argument fact.

After one allowed decision and audit append, routing retains this order:

1. an accepted raw INSERT artifact candidate;
2. an accepted version-2 UPDATE artifact candidate for one selector/value
   pair or the retained one-Reference constant UPDATE;
3. the retained one-Reference DELETE candidate;
4. the retained one-Reference identity-selected SELECT candidate;
5. the retained parameter-free raw target; and
6. unavailable target.

Only the UPDATE branch added here can accept a pair outside INSERT. A pair
aimed at DELETE, SELECT, CLIENT, health, another domain, or another artifact
returns `RawCallTargetUnavailable` after the allowed audit. A superficial
UPDATE candidate opens the existing reference-mutation savepoint. Signature,
identity, type, selector, direct-use, plan, evidence, result, or value
rejection rolls back that savepoint. The outer transaction then commits the
allowed audit and returns public `TARGET_UNAVAILABLE`.

Data and integrity rejection after complete target validation remains its
typed UPDATE source and maps to `INTERNAL_FAILURE` in protocol version 1.
Database, recovery, audit, savepoint, execution, outer commit, driver,
shutdown, and unknown-outcome failures also remain internal. No failure may
commit a partial field change, return the selector, or fabricate completion.
Successful mutation and allowed audit commit together.

## Replay and restart

Source apply already reports both parameters in declaration order. This is
discovery evidence only. Exact source replay preserves the complete sorted
function discovery, both parameter identities, function identity, artifact,
grant, and stored rows. It requires no regrant.

Service restart preserves the active revision, identities, grant, and updated
field values. The original `FunctionId` and both `ParameterId`s remain usable.
Swapping both command tokens and their input envelopes does not change the
selected object or assigned value.

## Closed surface

This decision does not admit:

* typed NULL, enum, record, opaque, or constructed value arguments;
* an unavailable standard value or another scalar family;
* duplicate, missing, nullable, defaulted, unused, indirect, or extra
  parameters;
* two caller values, two selectors, positional binding, or name binding;
* a parameter-valued DELETE, SELECT, CLIENT, health, or another function
  domain;
* a general predicate, expression, SQL, reflection, or invocation surface;
* ORV2 through ORV5, ORF2 through ORF5, remote transport, or `sys.invoke`.

Existing zero-argument, one-argument, pair INSERT, constant UPDATE, DELETE,
identity SELECT, cancellation, flow-control, error, and audit behaviour remains
unchanged.

## Required proof

Focused PostgreSQL proof must establish:

* denial and denied audit precede every UPDATE and argument fact;
* the selector and value bind by exact `ParameterId` in either supplied order;
* a scalar value and a Reference value each update only the selected object;
* wrong, duplicate, missing, extra, mistyped, unused, indirect, nullable,
  defaulted, selector-as-value, or unsupported assignment shapes remain
  target-unavailable after authorisation where the admitted shape can reach
  them;
* U+0000 creates no change and retains one allowed audit;
* an absent selector completes with no value;
* validation failure rolls back the UPDATE savepoint, while an operational
  failure remains internal; and
* retained pair INSERT, constant UPDATE, DELETE, identity SELECT, cancellation,
  and flow control remain unchanged.

Focused server proof must use the authenticated local socket. It must establish
accepted two-frame input, public denial and unavailability redaction, exact
selector result, value-event flow control, cancellation, and private typed
source without adapter-side catalogue inspection.

The installed proof uses only the built package, checked-in source, public
commands, installed service account, and local raw socket. It must discover
the function and both parameter identities, deny before grant, create two
objects with caller values, update only one selected object, read both objects
publicly, swap the selector/value token and envelope order, replay without
regrant, restart, and reuse the original identities and grant. It must include
one scalar value UPDATE and one Reference value UPDATE.

Every test line, helper, and fixture remains owned by the approved test
implementation session. Production implementation, architecture, and hard
debugging remain owned by the host GPT-5.6 model. Format, strict Clippy,
rustdoc, diff, similarity, workspace, live PostgreSQL, socket, installed
package, replay, restart, and security-audit gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and keeps the repository buildable and green. A focused RED behaviour
tracer precedes the smallest production change that makes it green.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define raw reference value update` | `docs/decisions/0050-canonical-raw-reference-value-update.md`; `docs/decisions/README.md` | Accept and index the exact selector, value, UPDATE, security, result, closure, and proof contract. |
| `test(postgres): trace raw reference value update` | `crates/orna-postgres/tests/server_mutation_execution.rs` | Add the focused live RED target, binding, row, absent-selector, rollback, and audit tracer. |
| `feat(postgres): bind raw reference value update` | `crates/orna-postgres/src/kernel/server_mutation_execution.rs`; `crates/orna-postgres/src/kernel/security.rs` | Admit the exact UPDATE pair, preserve savepoint and failure order, and delegate to the normal active executor. |
| `test(server): prove raw reference value update authority` | `crates/orna-server/tests/standard_database.rs` | Prove authenticated socket authority, values, results, flow control, cancellation, and redaction. |
| `test(system): exercise installed raw reference value update` | `crates/orna-system-tests/fixtures/product_test_reference_value_update.orna`; `crates/orna-system-tests/tests/installed_product.rs` | Prove public scalar and Reference updates, denial, grants, exact rows, order independence, replay, and restart. |

## Precedence

For the exact selector/value UPDATE pair, this decision supersedes the
conflicting parameter-valued UPDATE and two-argument UPDATE closures in work
ADRs 0027, 0033, 0041, 0045, 0048, and 0049. It preserves their command,
canonical value, framing, transport, security, audit, one-argument, unrelated
operation, cancellation, result, and error rules.

Work ADR 0007 remains the language, artifact, execution, and result authority
for single-object UPDATE. Work ADR 0026 remains the argument-retention and
protocol state authority. This decision changes no canonical specification
file and does not advance or weaken the constructed-value and `sys.invoke`
sequence in work ADRs 0036, 0039, or 0042.
