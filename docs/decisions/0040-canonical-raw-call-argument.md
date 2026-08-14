# ADR 0040: Canonical Raw Calls Bind One Boolean INSERT Argument

**Status:** Accepted

## Decision

This decision accepts one bounded argument form for the installed local raw
client. It extends the existing zero-argument command without changing it:

```text
orna raw-call <canonical-function-id>
orna raw-call <canonical-function-id> <canonical-parameter-id>
```

The first form remains byte-for-byte unchanged. It reads no standard input and
continues to accept the `sys.catalog.health` name exception only in that
zero-argument form. The second form accepts one unique canonical `FunctionId`
and one unique canonical `ParameterId`, including their `function:` and
`parameter:` prefixes. It accepts no function name, parameter name, ordinal,
option, or second parameter argument.

Invalid command tokens, including invalid FunctionId or ParameterId tokens,
use the existing global usage diagnostic and exit status `2`.

## Canonical input validation

The one-parameter form reads standard input before it connects to the fixed
local socket. Standard input must contain exactly one complete canonical
`ORV1` envelope followed by end of file. The envelope length must be at most
`MAX_FRAME_PAYLOAD_LENGTH - 16`, because the `ParameterId` owns the other
sixteen bytes of the `CALL_ARGUMENT` frame payload.

The client reads sequentially with this fixed bound. It does not use
`read_to_end`. It reads and fully decodes the one complete envelope, then uses
one-byte end-of-file probe. It finishes this validation before it opens a
socket or emits a protocol frame.

Empty input, a bad marker, a malformed or oversized envelope, trailing input,
or other incomplete input is canonical input validation failure. It writes no
standard output, writes this exact single standard-error diagnostic, and exits
with status `7`:

```text
orna: raw-call argument input is invalid
```

During this pre-connect read, `SIGINT` returns the existing status `6`. It
writes no frame and opens no socket. Canonical input validation does not use a
file, environment variable, current directory, database selector, principal,
credential, or other authority to obtain or interpret the value.

`ORV1` is the only argument type parser. The generic transport can carry any
complete `ORV1` value. This executor slice opens only one Boolean
`FunctionArgument` for one authorised SERVER `INSERT`. It does not add a
Boolean literal parser.

## Protocol and parameter binding

This decision does not change `ORF1`, `ORV1`, frame bounds, or protocol frame
encoding. Work ADR 0026 remains the owner of those bytes and bounds. The raw
client uses the existing `ClientFrame::CallArgument` form after the existing
hello, acknowledgement, stream start, and result-window update:

1. send one `CALL_ARGUMENT` that contains the sixteen-byte `ParameterId` and
   one complete `ORV1` envelope;
2. send `CALL_ARGUMENTS_COMPLETE`; and
3. retain the existing completion and failure handling.

The client sends one `CALL_ARGUMENT` only. The server canonicalises binding by
`ParameterId`; client frame order is not binding authority. A successful
INSERT returns its existing complete `ORV1` reference value without a new
result form.

## Source discovery and replay

Source apply discovery adds an optional `parameters` key only for a function
that has one or more declared parameters. The key follows `function_id`. Its
value is an array in declaration-ordinal order. Each entry has these keys in
this order:

```json
{"name":"p_stored","parameter_id":"parameter:<canonical-id>"}
```

For a parameterised function, the complete discovery entry is:

```json
{"qualified_name":["schema","function"],"function_id":"function:<canonical-id>","parameters":[{"name":"p_stored","parameter_id":"parameter:<canonical-id>"}]}
```

Missing `parameters` means that the function has no parameters. The array is
discovery evidence only. It never authorises positional binding.

An exact source replay can advance the source and catalogue revisions. It must
preserve sorted function discovery and each declared parameter's name and
`ParameterId` entry. A future accepted semantic parameter rename can preserve
its `ParameterId`. A plain spelling replacement is not inferred as that rename.
It is a delete-and-add transition with a new identity, or preparation fails
closed. Name, ordinal, and type similarity have no identity authority.

## Authorisation and execution order

The server uses the authenticated local peer. Zero arguments and exactly one
Boolean protocol argument enter kernel dispatch. The kernel recovers one active
revision and one security snapshot in the same transaction, then authorises
the exact `FunctionId` before it checks function domain, signature, parameter,
plan, or value. A denied call appends one denied audit decision and returns
`EXECUTE_DENIED`. It does not disclose an argument, parameter, target shape,
or function fact.

One Boolean argument with a wrong or unknown `ParameterId`, an unsupported
target, or a zero-argument call to an INSERT that requires a parameter is
authorised and audited before it returns `TARGET_UNAVAILABLE`. The kernel
represents this outcome as:

```rust
PostgresKernelError::RawCallTargetUnavailable { function, rule }
```

This is the generic authorised raw-call closure. It does not create a
`RawServerTargetUnavailable` variant. The raw adapter maps it to the existing
`TARGET_UNAVAILABLE` public call failure.

For an allowed SERVER INSERT candidate, the server appends one allowed audit
decision and opens one savepoint. It passes the Boolean argument only to the
existing validated SERVER INSERT executor. Target or argument rejection rolls
back that savepoint, retains the allowed audit decision, and returns
`TARGET_UNAVAILABLE`. Operational recovery, audit, savepoint, execution,
commit, driver, shutdown, or canonical-value failures return
`INTERNAL_FAILURE`. Public diagnostics and audit records do not contain
argument facts.

The record-argument preflight in work ADR 0031 remains an unauthorised
transactional preflight. It appends no audit decision. Every other nonempty
argument shape remains in work ADR 0027: it does not open PostgreSQL, append an
audit decision, or disclose a target, and returns `TARGET_UNAVAILABLE`.

This decision does not open these calls:

* CLIENT, health, or SELECT calls with an argument;
* a non-Boolean argument value;
* a parameter-free call to an INSERT that requires an argument;
* SERVER UPDATE or DELETE; or
* a call with more than one argument.

Only the exact allowed SERVER INSERT candidate opens a savepoint. A denied
call remains `EXECUTE_DENIED` and does not reach target validation. This
decision changes no protocol file and no fixture file.

## Required proof

The installed product proof extends the existing unavailable-insert journey.
It uses only the packaged `orna` executable, public commands, a fixture file,
and the installed service. It does not use PostgreSQL or a private API.

The proof must:

* prove invalid, empty, malformed, oversized, and trailing standard input
  returns the exact status-`7` diagnostic before any socket connection;
* prove `SIGINT` during the pre-connect input read returns status `6` without a
  socket connection or frame;
* prove the argument command emits the existing one `CALL_ARGUMENT` frame
  with the exact ParameterId and ORV1 bytes, while zero-argument frames and
  values remain byte-for-byte unchanged;
* apply the parameterised INSERT fixture and discover its canonical parameter
  identity from the source-apply result;
* prove an argument-bearing call is `EXECUTE_DENIED` before its explicit
  grant;
* prove the parameter-free call is `TARGET_UNAVAILABLE` and creates no row;
* send complete ORV1 Boolean TRUE and FALSE input and prove that each stored
  value is returned by the public reader;
* replay the exact fixture and prove sorted function discovery, declared
  parameter entries, and explicit grants remain stable; and
* restart the installed service, make one further argument-bearing call, and
  prove the stored result and durable authority remain correct.

Focused server proof must show that a Boolean argument with a wrong or unknown
ParameterId, and every unsupported Boolean target, is authorised and audited
before the generic unavailable error. It must show that record values retain
work ADR 0031 preflight with no audit, and all other nonempty shapes retain
work ADR 0027 with no PostgreSQL operation or audit. It must show one allowed
audit and retained audit after a SERVER INSERT savepoint rollback. It must not
use PostgreSQL as a substitute for the installed product proof.

Focused protocol, server, PostgreSQL, raw-socket, installed-package, replay,
restart, security-audit, strict Clippy, rustdoc, format, diff, similarity, and
workspace gates remain required. Every test line and fixture logic is owned by
the approved DeepSeek test session.

## Implementation sequence

Each row is one signed Conventional Commit. Each commit changes one to three
files and keeps the repository buildable and green. ORV5 remains deferred.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define canonical raw call argument` | `docs/decisions/0040-canonical-raw-call-argument.md`; `docs/decisions/README.md` | Accept and index this bounded command, source discovery, security, and proof contract. |
| `test(cli): share exact usage evidence` | `crates/orna-server/tests/support/mod.rs`; `crates/orna-server/tests/backend_shell.rs`; `crates/orna-server/tests/source_check.rs` | DeepSeek-owned migration to one independent expected usage value. Bytes remain unchanged. |
| `test(cli): share remaining exact usage evidence` | `crates/orna-server/tests/source_apply.rs`; `crates/orna-server/tests/security_admin.rs` | DeepSeek-owned migration to the shared expected usage value. Bytes remain unchanged. |
| `test(system): parse optional source parameters` | `crates/orna-system-tests/tests/installed_product.rs` | DeepSeek-owned parser support for ordered optional parameters. The old discovery form remains exact. |
| `feat(source): discover function parameters` | `crates/orna-server/src/source_apply.rs` | Emit parameter discovery only for non-empty parameter lists. DeepSeek owns inline builder tests in this green slice. |
| `feat(postgres): close authorised raw argument targets` | `crates/orna-postgres/src/kernel.rs` | Add the generic raw target-unavailable error. DeepSeek owns inline error tests. |
| `feat(postgres): bind one boolean raw insert argument` | `crates/orna-postgres/src/kernel/server_mutation_execution.rs` | Add one-Boolean authorised raw INSERT sibling or wrapper. DeepSeek owns inline binding tests. |
| `feat(postgres): dispatch one raw argument` | `crates/orna-postgres/src/kernel/security.rs`; `crates/orna-postgres/tests/server_mutation_execution.rs` | Add argument dispatch. Old and barrier wrappers use empty arguments. DeepSeek extends the live PostgreSQL dispatch test to prove authorisation, audit retention, savepoint rollback, successful Boolean execution, and unsupported-target closure. Inline tests retain the pure shape and error-classification boundary. |
| `feat(server): dispatch one raw boolean argument` | `crates/orna-server/src/raw_client_dispatch.rs` | Admit exactly one Boolean to the kernel, preserve record and other closures, and map the generic error. DeepSeek owns inline tests. |
| `test(server): prove raw argument authority` | `crates/orna-server/tests/standard_database.rs` | DeepSeek-owned live authentication and audit proof. |
| `feat(server): read canonical raw call input` | `crates/orna-server/src/raw_call.rs`; `crates/orna-server/src/lib.rs` | Add bounded pre-connect input and the existing `CallArgument` frame. DeepSeek owns inline framing and input tests. |
| `feat(cli): expose canonical raw call argument` | `crates/orna-server/src/main.rs`; `crates/orna-server/tests/support/mod.rs` | Expose the optional ParameterId and update independent usage. DeepSeek owns changed main test lines. |
| `test(cli): prove raw call process boundary` | `crates/orna-server/tests/raw_call.rs` | DeepSeek-owned compiled-process status and no-socket proof. |
| `test(system): exercise installed raw insert argument` | `crates/orna-system-tests/tests/installed_product.rs` | DeepSeek-owned TRUE/FALSE success, replay, grants, and restart proof in the existing parameterised INSERT journey. |

## Deferred surface

This decision does not define `sys.invoke`, reflected conversion, defaults,
parameter names or ordinals as invocation inputs, ORV2 through ORV5 arguments,
remote endpoints, SQL input, grants beyond the existing fixed-service grant,
or public audit inspection. It also does not define arbitrary argument types,
multiple arguments, CLIENT arguments, SELECT arguments, UPDATE, DELETE, or a
general invocation system.

## Precedence

This decision supersedes only the conflicting parameter-free closures in work
ADRs 0027, 0032, 0033, and 0038 for the exact one-argument command,
source-apply discovery result, and authorised Boolean SERVER INSERT scope
defined here. It preserves their byte, transport, result, cancellation,
resource, fixed-socket, security, audit, and parameter-free behaviour outside
that scope.

It preserves work ADR 0031 record preflight and work ADR 0026 identity-based
argument binding. It preserves the locked ordinary invocation direction:
`sys.invoke` remains the later general invocation surface. It does not change
the canonical specification except for this stated implementation boundary.
