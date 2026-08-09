# ADR 0015: The First CLIENT Function Returns a Boolean Constant

**Status:** Accepted

## Decision

The first executable CLIENT function is a closed, parameter-free Boolean
constant:

```sql
CREATE CLIENT FUNCTION examples.enabled()
RETURNS BOOLEAN
RETURN TRUE;
```

`BOOL` is the only alternate spelling of `BOOLEAN`. The body is exactly one
explicit `RETURN` followed by `TRUE` or `FALSE` and the declaration terminator.
The literal spelling is case-insensitive and remains lossless in source. The
resolved value is the normal Orna `BOOLEAN` scalar and execution returns the
existing typed `RuntimeValue::Boolean` value.

This narrow implementation proves CLIENT source, semantic identity, durable
revision storage, recovery, artefact verification, and local evaluation. It is
not a parser-only declaration and it is not described as a general CLIENT VM.

The function has:

* `FunctionDomain::Client`;
* no parameters;
* `FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean))`;
* `SECURITY INVOKER`;
* no transaction mode;
* `VOLATILITY IMMUTABLE`;
* no capability, state, call, resource, runtime-contract, or definition
  references; and
* one Boolean constant-return body.

No modifier is written in this source form. A later CLIENT slice must add
syntax and durable semantics before a modifier can be accepted.

## Why Boolean comes before UI

The canonical specification permits CLIENT functions to return non-UI values.
`work ADR 0002` fixes `RETURNS UI` as the public spelling for a CLIENT UI
function, but it does not require every CLIENT function to return UI.

`std.ui.UI` still depends on the unresolved generic value-type facility,
standard-prelude bootstrap, external CLIENT runtime contracts, contract and
call-site identities, a value codec, runtime negotiation, and a graphical
runtime. Inventing an empty UI value or treating UI as a core Boolean-like
special case would contradict work ADR 0002 and spec ADR 0012. This slice
therefore proves the CLIENT execution boundary with the already closed
`BOOLEAN` scalar and leaves `RETURNS UI` unavailable until its real typed
contract can be implemented.

## Syntax and diagnostics

`CREATE CLIENT FUNCTION` is a distinct lossless declaration. It is not parsed
as a SERVER function with a changed flag. The parse report retains CLIENT and
SERVER declarations separately, while both kinds share the one function
namespace and stable `FunctionId` identity class.

The parser retains the complete declaration, function name, empty parameter
list, return type, Boolean literal, literal span, and declaration span. It
recovers at the declaration terminator so one malformed CLIENT declaration
does not hide later declarations.

Syntax failures use `ORNA0001` and point at the token that prevents the closed
form from being parsed. Diagnostics use public language rather than parser or
artefact terminology. In particular:

```text
CLIENT functions use RETURN before their result value
CLIENT RETURN currently supports only TRUE or FALSE
expected ';' after CLIENT function body
```

Semantic checking occurs only after syntax succeeds. It uses the existing
diagnostic categories and this deterministic order:

1. namespace ownership and duplicate function names across both domains;
2. the zero-parameter requirement;
3. the single `BOOLEAN` return requirement; and
4. the Boolean literal/return agreement retained by the checked body.

A nonempty parameter list reports `ORNA0303` on the complete parameter list:

```text
this CLIENT function cannot declare parameters yet
```

Any return other than the canonical `BOOLEAN` scalar reports `ORNA0201` on
the written return type:

```text
this CLIENT function must return BOOLEAN
```

The deferred `RETURNS UI` spelling receives the same type diagnostic. The
compiler must not pretend that a standard-prelude `TypeId` exists before the
standard library is bootstrapped. Any diagnostic rejects the complete checked
bundle, as for SERVER declarations.

## Semantic identity and evidence

CLIENT and SERVER functions occupy the same owner-qualified function
namespace. A normalised duplicate name is rejected even when the two
declarations use different domains. A new CLIENT declaration receives the
normal provisional and durable `FunctionId`; an exact active declaration
reuses its stable identity.

This slice does not accept changing an existing function between SERVER and
CLIENT. A submitted declaration whose resolved active `FunctionId` belongs to
the other domain reports `ORNA0303` at the function name:

```text
this function is already declared as a SERVER function
this function is already declared as a CLIENT function
```

The same check applies in both directions before body checking and preparation.
Changing execution authority is not treated as an ordinary body revision.

The checked CLIENT body retains only the Boolean value and exact source
location. The return type is a standard scalar, the function has no
parameters, and the body names no definition. Consequently this slice emits:

* one `DefinitionOrigin::Function` at the complete declaration;
* no parameter, return-column, expression, or body definition origins; and
* no `DefinitionReference` rows.

Changing source formatting or literal case without changing the resolved
Boolean value reuses the immutable function revision. Changing `TRUE` to
`FALSE` changes the artefact payload and semantic hash, creates the next
function revision, and retains the `FunctionId`. Removing the declaration from
the complete candidate source removes the function from the active catalogue
under the existing source-authority rules.

## Artefact contract

The artefact format identity is exactly:

```text
orna.client-plan
```

Version 1 is a closed Boolean-return plan:

```text
magic[8] = ORNACP\0\0
version  = u32 big-endian 1
operation = u8 1          -- return Boolean
value     = u8 0|1
```

The complete canonical payload is therefore 14 bytes. The decoder rejects an
invalid magic value, another version, another operation tag, a non-Boolean
value byte, every truncated prefix, and trailing bytes. Construction and
decoding validate the same closed model. Source text, semantic names, source
locations, provisional identities, and backend names never enter the payload.

The public artefact model is `ClientPlan::return_boolean(bool)`, with
`format_version()`, `returned_boolean()`, `encode()`, and `decode()` methods.
Construction and encoding are infallible because the model cannot represent
another operation or value kind. `ClientPlanError` is public, non-exhaustive,
and has this complete version-1 contract:

| Variant | Display text |
| --- | --- |
| `InvalidMagic` | `invalid orna.client-plan artefact magic` |
| `UnsupportedVersion(u32)` | `unsupported orna.client-plan artefact version {version}` |
| `InvalidOperation(u8)` | `invalid client-plan operation tag {tag}` |
| `InvalidBoolean(u8)` | `invalid client-plan Boolean byte {value}` |
| `Truncated` | `truncated orna.client-plan artefact` |
| `TrailingBytes` | `trailing bytes after orna.client-plan artefact` |

The numeric fields are available through normal enum matching. Decoder errors
never retain the supplied byte slice.

The durable executable artefact has `ExecutableArtifactKind::Client`, format
`orna.client-plan`, version `1`, language `orna.language/1`, and the exact
payload digest already required of all executable artefacts. SERVER plan and
mutation artefact formats and bytes are unchanged, and no CLIENT decoder may
reinterpret a SERVER artefact.

Preparation independently revalidates the checked bundle before allocating or
persisting an artefact. It requires the CLIENT domain, exact modes, zero
parameters, one `BOOLEAN` return, the closed Boolean body, and an empty
reference sequence. It maps the `FunctionId`, constructs the durable
definition and CLIENT artefact, computes the existing canonical declaration,
semantic, and catalogue hashes, and applies the same immutable-revision reuse
rules as SERVER functions. Malformed private checked values fail closed before
artefact construction.

## Local evaluation boundary

The first CLIENT execution code lives in a small `orna-client` crate. Its
public entry point is exactly:

```rust
pub fn evaluate_client_function(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<ClientExecutionResult, ClientExecutionError>;
```

It performs no database, protocol, filesystem, process, environment, clock,
random, network, or runtime-library operation.

`ClientExecutionContext` contains the pinned `RevisionPair`, `FunctionId`, and
`FunctionRevisionId`, with read-only getters. `ClientExecutionResult` contains
that context and one `RuntimeValue`, again with read-only getters. Evaluation
of this slice always returns `RuntimeValue::Boolean`.

`ClientExecutionError` is public and non-exhaustive:

```rust
pub enum ClientExecutionError {
    InvalidActiveRevision {
        pair: RevisionPair,
        function: FunctionId,
        source: ClientActiveRevisionError,
    },
    FunctionNotFound {
        pair: RevisionPair,
        function: FunctionId,
    },
    InvalidFunction {
        context: ClientExecutionContext,
        rule: ClientExecutionRule,
    },
    InvalidArtifact {
        context: ClientExecutionContext,
        source: ClientPlanError,
    },
}
```

Every variant exposes read-only access to its recorded pair and function. The
last two expose their complete context. `InvalidActiveRevision` exposes its
`ClientActiveRevisionError` and says `the active revision cannot be trusted`.
`InvalidArtifact` exposes its `ClientPlanError` and says `the saved CLIENT
function cannot be evaluated`. Both nested errors are available through
`Error::source`; the other variants have no nested source. `FunctionNotFound`
says `the active revision does not contain this function`.

`ClientActiveRevisionError` is public and non-exhaustive. Its
`Canonical(CanonicalHashError)` variant preserves a failure to calculate the
complete canonical semantics and displays that source unchanged. Its
`CatalogueHashMismatch` variant says `active revision catalogue hash differs
from its canonical semantics`. It retains no hash bytes. Both implement
`Error`; only `Canonical` exposes a nested source.

`ClientExecutionRule` is also public and non-exhaustive. Its variants and
exact human-facing text are:

| Rule | Display text |
| --- | --- |
| `FunctionDomain` | `this function does not run on the client` |
| `Parameters` | `this CLIENT function requires unsupported parameters` |
| `ReturnType` | `this CLIENT function does not return BOOLEAN` |
| `Security` | `this CLIENT function has an unsupported security mode` |
| `Volatility` | `this CLIENT function is not an immutable constant` |
| `References` | `this CLIENT function depends on unsupported definitions` |
| `ArtifactFormat` | `the saved CLIENT function uses an unsupported artefact format` |
| `ArtifactVersion` | `the saved CLIENT function uses an unsupported artefact version` |
| `LanguageVersion` | `the saved CLIENT function uses an unsupported language version` |

`InvalidFunction` displays its rule text exactly. Typed rules rather than
internal validation strings make failures stable without exposing source,
payload, hash, or storage details.

Before returning a value it validates, in order:

1. the canonical catalogue digest of the complete active catalogue, current
   function revisions, expressions, origins, and references equals the active
   revision's recorded catalogue hash;
2. the function exists in the active catalogue;
3. its domain is CLIENT;
4. the function has no parameters, returns exactly one `BOOLEAN`, uses
   INVOKER/IMMUTABLE modes, and has no references;
5. the revision artefact format, version, and language label agree with the
   closed contract; and
6. the artefact decodes as the closed version-1 Boolean return plan.

The complete digest check already recomputes every executable payload hash, so
a mismatched target payload hash is an `InvalidActiveRevision::Canonical`
failure rather than a later `ClientExecutionRule`. `CatalogueSnapshot` already
rejects a transaction mode on every CLIENT
function. `ActiveDatabaseRevision` construction already proves that there is
exactly one current revision record for each active function, that it belongs
to the function and current revision named by the catalogue, and that its
artefact kind matches the function domain. The evaluator consumes those
validated facts; it does not expose duplicate, missing, crossed-revision,
transaction, or artefact-kind errors that safe public input cannot contain.
Core construction and PostgreSQL recovery retain the typed failures and tests
for those invariant classes.

Validation uses one pinned in-memory active revision throughout. Recomputing
the complete canonical catalogue digest prevents a caller from crossing a
loose artefact and signature inside a manually constructed public value. A
value produced by compiler preparation followed by kernel apply, or by
PostgreSQL recovery, has already passed the same semantic validation; the
evaluator repeats it because its public input type can also be constructed
directly. It returns a result
containing the active revision pair, `FunctionId`, `FunctionRevisionId`, and
typed Boolean value. Errors preserve all context available at their validation
stage without exposing source bytes, artefact bytes, hash material, or
internal storage names. `InvalidActiveRevision` and `FunctionNotFound` can
retain only the pair and requested `FunctionId`; every failure after function
resolution also retains the resolved `FunctionRevisionId` through
`ClientExecutionContext`.

The evaluator is the complete runtime for this one operation. Because its
instruction set contains only a Boolean literal return, verification itself
proves that database-provided code cannot access a host capability. This does
not settle bytecode versus WASM versus another representation for the later
general CLIENT VM.

This function is a low-level, post-authorisation, post-trust-policy artefact
evaluator. It is not an invocation, authentication, authorisation, capability,
or trust-policy API. Its direct caller is responsible for deciding that the
authenticated principal may execute the selected function and that the source
of the active revision is trusted. The canonical digest check proves internal
semantic consistency; it does not grant `EXECUTE`, authenticate a principal,
or establish publisher trust.

No CLI or public-protocol command is added in this slice. The exact
`ActiveDatabaseRevision` API is limited to the local compiler/apply/recovery
implementation proof; it is not the target shape promised to a future wire
protocol. A later accepted transport slice must define its own bounded pinned
target view and execution entry point after `sys.invoke` has completed
authentication, `EXECUTE`/policy/capability authorisation, revision pinning,
and configured artefact trust checks. That entry point must reuse the same
public `ClientPlan` decoder and closed semantic validation rather than create a
second artefact interpretation. This record does not claim that a transported
target view can be passed to `evaluate_client_function`.

## PostgreSQL apply and recovery

The existing private catalogue already stores CLIENT domains and
`ExecutableArtifactKind::Client` artefact kinds. Applying this function creates
no `_orna_data` relation, column, constraint, or index. It writes only the
normal source, catalogue, function-revision, origin, artefact, and
active-revision records.

Recovery reconstructs the exact CLIENT definition and artefact through the
same repeatable-read active snapshot and existing hash/invariant checks. A
fresh apply, source-only replay, semantic change, restart, and local evaluation
must all retain the exact function and revision relationships described above.
Tampered kind, format, version, language, payload, hash, current revision, or
reference rows fail before evaluation.

## Required proof matrix

| Boundary | Required proof |
| --- | --- |
| Syntax | Lossless `TRUE` and `FALSE`, quoted/unquoted names, exact spans, exact malformed diagnostics, and recovery to later schema/type/SERVER/CLIENT declarations. |
| Closed shape | Parameters, `ROWS`, non-Boolean returns, `UI`, `AS`, `IS BEGIN`, calls, identifiers, `NULL`, numbers, text, modifiers, and extra body tokens are rejected at the stated boundary. |
| Namespace | CLIENT/CLIENT and CLIENT/SERVER duplicates normalise identically and reject the whole checked bundle. |
| Domain continuity | Replacing an active SERVER declaration with CLIENT, or an active CLIENT declaration with SERVER, produces the exact domain diagnostic and no candidate. |
| Checked model | Exact `FunctionDomain::Client`, zero parameters, single Boolean return, modes, Boolean body value/location, stable `FunctionId`, and empty reference evidence. |
| Artefact | Exact 14-byte golden for both values; exhaustive corruption, every truncation prefix, trailing bytes, version and operation crossing, and no source/backend leakage. |
| Preparation | `ExecutableArtifactKind::Client`, format/version/language/hash, exact origin, empty evidence, source-only reuse, Boolean-change revision, identity reuse, and malformed checked-bundle rejection before allocation. |
| Compatibility | Existing SERVER source, checked bodies, artefact goldens/decoders, preparation, execution, and live tests remain byte- and behaviour-identical. |
| Evaluation | Exact contextual typed TRUE/FALSE result plus untrusted active semantic or payload hash, unknown function, SERVER function, wrong security/volatility/signature/evidence, corrupt artefact payload, wrong format/language, and unsupported version failures. |
| Invocation boundary | Public docs and tests identify this local evaluation as post-authorisation/post-trust; no CLI or protocol represents it. Future transport tests must prove `sys.invoke` checks before executing a bounded transported target view through its separately accepted entry point. |
| Validated input | Catalogue and active-revision constructors retain exact tests for CLIENT transaction, missing/duplicate/crossed/non-current revision records, and function-domain/artefact-kind mismatch; the evaluator accepts only the resulting valid active value. |
| Durability | Apply/recover/restart/evaluate, no physical data objects, source-only replay, semantic revision change, durable tamper rejection, active-pair pinning, and complete session cleanup on every live-test path. |

Normal format, strict Clippy, workspace test, rustdoc, diff, similarity, and
live PostgreSQL gates remain required. Tests must assert exact typed errors and
source spans rather than only checking that an error exists.

## Deferred surface

This record does not accept:

* `RETURNS UI`, `std.ui.UI`, value-type DDL, standard-prelude bootstrap, UI
  constructors, runtime contracts, or graphical runtimes;
* parameters, defaults, named arguments, capabilities, state, resources,
  actions, streams, calls, local variables, control flow, or procedural bodies;
* any return type or literal other than non-null `BOOLEAN`;
* external CLIENT functions, CLIENT-to-SERVER calls, `sys.invoke`, artefact
  transport, authentication, presentation, runtime selection, or a CLI invoke
  command;
* artefact signing, trust policy, cache policy, general instruction resource
  limits, or a choice of bytecode, WASM, or custom VM IR; or
* hot CLIENT instances, state reconciliation, cancellation, deadlines, or
  tracing.

Each requires a later accepted decision and a fail-closed implementation.

## Precedence

This record implements the non-UI CLIENT-function possibility described by
`spec/docs/22-ddl-reference.md` and the `CREATE CLIENT FUNCTION` domain fixed
by spec ADR 0011. It narrows the open CLIENT VM and artefact questions in
`spec/docs/41-open-questions.md` only for this literal operation.

It does not change work ADR 0002's `RETURNS UI` spelling or spec ADR 0012's
requirement that UI remain a standard-library value type. For this first
CLIENT slice, it supersedes the `AS expression` examples in
`spec/docs/22-ddl-reference.md`, `spec/docs/23-function-language.md`, and
`spec/docs/46-syntax-to-runtime-trace.md`; the accepted public body spelling
remains explicit `RETURN`.

For all other subjects, the prior work ADRs and canonical specification remain
unchanged.
