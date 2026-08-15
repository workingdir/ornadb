# ADR 0049: Canonical Raw Calls Bind One Bounded Argument Pair

**Status:** Accepted

## Decision

One installed raw call may carry exactly two non-null ORV1 arguments. This is
the first public path that can initialise two fields of one object from real
caller values instead of combining one argument with source constants.

The installed command gains one form while the existing zero- and one-argument
forms remain unchanged:

```text
orna raw-call <canonical-function-id> <canonical-parameter-id-1> <canonical-parameter-id-2>
```

The two parameter identities must be distinct canonical `ParameterId` tokens.
Each token is paired with one complete ORV1 envelope from standard input in the
same command-token order. The first envelope belongs to parameter 1 and the
second belongs to parameter 2. Parameter identity, not command position,
declaration ordinal, or source name, remains the binding authority.

The client sends two existing `CALL_ARGUMENT` frames in command-token order,
then the existing `CALL_ARGUMENTS_COMPLETE` frame. The protocol connection
retains the arguments by stable identity and presents the completed `RawCall`
in ascending `ParameterId` order. No ORV1 or ORF1 byte, tag, frame, marker, or
connection transition changes.

## Canonical pair input

The two-parameter command reads and validates all input before checking or
connecting to the fixed local socket. Standard input contains exactly two
self-delimiting complete ORV1 envelopes followed by end of file. There is no
separator, count, JSON wrapper, source literal, or alternate parser.

For each value, the client reads the fixed ORV1 header, checks its declared
payload length, reads that exact payload, and decodes the complete envelope.
It then performs the same process for the second value and uses a one-byte EOF
probe. It does not use `read_to_end`.

The retained protocol argument budget remains authoritative. If the encoded
value lengths are `first` and `second`, validation requires:

```text
16 + first + 16 + second <= MAX_FRAME_PAYLOAD_LENGTH
```

The two sixteen-byte terms are the stable ParameterIds retained with the
values. This aggregate rule is stricter than merely accepting two individually
bounded frames and prevents the local client from constructing a call the
protocol connection must reject for retained argument bytes.

Empty input, one value only, malformed input, an oversized individual value,
an aggregate overflow, or bytes after the second value return the existing
status-7 diagnostic before any socket connection:

```text
orna: raw-call argument input is invalid
```

An invalid FunctionId or ParameterId token, duplicate parameter tokens, a
missing token, or a third parameter token is a command-shape failure. It uses
the existing usage diagnostic and status 2 and reads no standard input.
`SIGINT` while either value or the EOF probe is being read returns status 6,
writes no frame, and opens no socket.

## Exact two-parameter INSERT target

The raw server adapter admits zero arguments, one argument under the retained
decisions, or exactly two values where each is Boolean, Integer, BigInt, Float,
Text, Bytes, or Reference. A two-value call enters the protected kernel path
only when the two stable ParameterIds are distinct.

After an allowed execute decision, a two-argument target is available only
when the active function:

* is a SERVER function with the accepted `SECURITY INVOKER`, transaction, and
  volatility modes for one-row INSERT;
* has one accepted immutable `orna.server-mutation-plan` INSERT artifact;
* declares exactly two required, non-null parameters;
* has exactly the supplied two ParameterIds as its complete parameter set;
* declares each parameter as the exact active type of its supplied value;
* uses each parameter in at least one INSERT assignment whose complete
  expression is the direct parameter read with the same function owner and
  ParameterId; and
* returns the accepted single object Reference result.

The two parameters may have the same type, and any accepted scalar/Reference
combination is allowed. Their identities must still differ. A parameter can be
read by more than one assignment, but both parameters must be causally used.
Other accepted assignments may remain source literals or contextual NULL and
require no additional runtime input.

The existing active mutation validator remains authoritative for function,
revision, artifact, definition-reference evidence, target, assignments,
argument completeness, exact types, required fields, result declaration,
payload bounds, and generated ObjectId. This decision adds no positional
binding, cast, coercion, defaulted parameter, nullable parameter, expression
nesting, record constructor, or caller-selected SQL.

On success, the existing executor stores both exact values in one row and
returns one canonical ORV1 Reference to that object. Both parameter bindings,
the row, and the allowed audit decision commit together.

## Value preservation and rejection

Each argument retains the value rules already accepted for its one-argument
path. Signed integer limits, canonical finite float bits, exact UTF-8 bytes,
arbitrary Bytes payloads, Boolean value, and Reference target/ObjectId cross
without source rendering or conversion.

Text containing U+0000 remains unavailable. If either argument contains
U+0000, validation occurs after authorisation and complete target validation
but before PostgreSQL binding. The INSERT savepoint rolls back, the outer
transaction retains its allowed audit, no row is created, and the public
result is `TARGET_UNAVAILABLE`. Public diagnostics and audit records contain
neither argument value.

The protocol connection canonicalises public pair arguments by ascending
ParameterId. When more than one supplied value is independently invalid, the
normal complete argument and plan validators retain their existing order; the
pair-specific causal-use and Text checks inspect that canonical argument
order. No declaration-name or PostgreSQL-column order becomes error authority.

## Protected routing and failure order

Closed outer value shapes are rejected before PostgreSQL exactly as today.
For one admitted pair, the kernel recovers one active revision and security
snapshot in one transaction, constructs the pinned InvocationTarget, and
authorises the FunctionId before inspecting its domain, signature, parameters,
types, artifact, plan, assignments, or target relation.

A denied call appends and commits one denied execute audit and returns
`EXECUTE_DENIED`. It does not disclose whether either parameter, type, field,
plan, or function exists.

After one allowed decision and audit append, routing retains this order:

1. an accepted raw INSERT artifact candidate;
2. the one-Reference UPDATE or DELETE candidates;
3. the one-Reference identity-selected SELECT candidate;
4. the parameter-free raw target; and
5. unavailable target.

Only the first branch can accept two arguments. An allowed pair aimed at
health, CLIENT, SELECT, UPDATE, DELETE, another domain, or an unsupported
SERVER artifact returns `RawCallTargetUnavailable`. A superficial INSERT
candidate opens the existing INSERT savepoint. Complete signature, identity,
type, use, plan, evidence, result, or value rejection rolls back only that
savepoint and retains the allowed audit when the outer transaction commits.

Database, recovery, audit, savepoint, execution after complete target
validation, outer commit, driver, shutdown, or unknown-outcome failure remains
an `INTERNAL_FAILURE`. No failure may commit a partial row, emit one argument,
or fabricate a clean completion.

## Closed surface

This decision does not admit:

* one typed NULL or any enum, record, opaque, or constructed collection;
* an unavailable standard value or another scalar family;
* duplicate ParameterIds, missing values, or more than two arguments;
* a function with fewer or more than two parameters;
* a nullable or defaulted parameter;
* a supplied parameter that is unused or appears only inside another
  expression;
* two-argument CLIENT, health, SELECT, UPDATE, or DELETE;
* a general SQL, expression, reflection, name-binding, or invocation surface;
* ORV2 through ORV5, ORF2 through ORF5, remote transport, or `sys.invoke`.

Record values retain their unauthorised transactional preflight. Every other
closed outer shape returns the existing public `TARGET_UNAVAILABLE` without a
PostgreSQL operation or audit. Existing zero- and one-argument calls retain
their exact authority, routing, bytes, errors, and replay behaviour.

## Replay and restart

Source apply output already reports all declared parameters in declaration
order. That order is discovery evidence only. Exact source replay preserves
the complete sorted function discovery, both parameter identities, function
identity, artifact, explicit grant, and stored rows. It requires no regrant.

Service restart preserves the active revision, identities, grant, and exact
stored pair values. The original FunctionId and ParameterIds remain usable.
Changing the two parameter tokens and input envelopes together may change
client transmission order without changing semantic binding or the stored
row.

## Required proof

Focused CLI and socket proof must establish:

* exactly two distinct canonical parameter tokens pair with exactly two
  complete ORV1 envelopes in token order;
* duplicate, missing, extra, malformed, trailing, individual-limit, and
  aggregate-limit cases fail before a socket connection;
* SIGINT during either envelope or the EOF probe opens no socket;
* the client emits two unchanged `CALL_ARGUMENT` frames and one completion
  frame, while zero- and one-argument bytes remain unchanged; and
* swapping both parameter/value pairs produces the same canonical server call.

Focused server and PostgreSQL proof must establish:

* the adapter admits exactly two supported values and retains both identities
  and values without conversion;
* denial and denied audit precede every target and argument fact;
* two exact active parameters bind by identity and store both exact values in
  one row;
* same-typed parameters are not confused by declaration or frame order;
* wrong, duplicate, missing, extra, mistyped, unused, indirect, nullable, or
  defaulted parameters remain target-unavailable after authorisation where the
  admitted public shape can reach them;
* either Text argument containing U+0000 creates no row and retains one
  allowed audit;
* target rejection rolls back the INSERT savepoint and operational failures
  remain internal; and
* one-argument INSERT, Reference mutation, identity SELECT, parameter-free
  SELECT, cancellation, and flow-control behaviour remain unchanged.

The installed proof uses only the built package, checked-in source, public
commands, installed service account, and local raw socket. It must discover
one two-parameter creator, deny it before grant, create rows with two distinct
caller values, read both stored fields publicly, swap parameter/value command
order, replay without regrant, restart, and reuse the original identities and
grant. It must include at least one scalar pair and one scalar/Reference pair,
so the accepted family is not falsely proved by one homogeneous shape.

Every test line, helper, and fixture remains owned by the approved test
implementation session. Production implementation, architecture, and hard
debugging remain owned by the host GPT-5.6 model. Format, strict Clippy,
rustdoc, diff, similarity, workspace, live PostgreSQL, socket, installed
package, replay, restart, and security-audit gates remain required.

## Implementation sequence

Each row is one signed Conventional Commit, changes one to three files, and
leaves the repository buildable and green. A focused RED behaviour tracer
precedes its smallest production change.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define canonical raw argument pairs` | `docs/decisions/0049-canonical-raw-argument-pairs.md`; `docs/decisions/README.md` | Accept and index the exact command, input, binding, INSERT, security, closure, and proof contract. |
| `feat(postgres): bind raw argument pairs` | `crates/orna-postgres/src/kernel/server_mutation_execution.rs`; `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Admit one protected two-value shape, require two exact directly used parameters, preserve routing/savepoint/error order, and prove live storage and audit. Test logic remains test-session owned. |
| `feat(server): dispatch raw argument pairs` | `crates/orna-server/src/raw_client_dispatch.rs` | Pass two supported values to the protected kernel without conversion and keep all other shapes closed. Inline test logic remains test-session owned. |
| `test(server): prove raw argument-pair authority` | `crates/orna-server/tests/standard_database.rs` | Prove authenticated socket frames, denial, redaction, exact storage, retained paths, flow control, and cancellation. |
| `feat(cli): read canonical raw argument pairs` | `crates/orna-server/src/raw_call.rs`; `crates/orna-server/src/lib.rs`; `crates/orna-server/src/main.rs` | Add the exact two-token/two-envelope command, aggregate pre-connect bound, unchanged frames, and signal boundary. Inline test logic remains test-session owned. |
| `test(cli): prove canonical raw argument-pair input` | `crates/orna-server/tests/raw_call.rs` | Prove compiled-process token, input, status, no-socket, and retained zero/one-argument behaviour. |
| `test(system): exercise installed raw argument pairs` | `crates/orna-system-tests/fixtures/product_test_argument_pairs.orna`; `crates/orna-system-tests/tests/installed_product.rs` | Prove public scalar and scalar/Reference pairs, discovery, denial, grants, exact rows, pair-order independence, replay, and restart. |

## Precedence

For the exact two-argument form, this decision supersedes the conflicting
multiple-argument closures in work ADRs 0027, 0033, 0040, 0041, 0043, 0045,
and 0048. It preserves their canonical values, frames, fixed socket,
authentication, audit, cancellation, result, one-argument, and unrelated
operation rules.

Work ADRs 0005 and 0026 remain authoritative for single-row INSERT semantics,
identity-based argument retention, aggregate argument bytes, and canonical
completed RawCall order. This remains a bounded milestone-4 recovery
capability. It does not advance or weaken the constructed-value and
`sys.invoke` sequence in work ADRs 0036, 0039, or 0042.
