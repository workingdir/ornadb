# Work ADR 0088: Structural UI Constructors for Source-Authored CLIENT Work

**Status:** Accepted

## Context

Work ADR 0062 established `std.ui.UI` as an immutable, transient opaque
standard-library value and registered its `ORNA-UI/1` codec. Work ADR 0083
registered `std.ui.window@1` as the first durable UI entry point. Work ADRs
0082, 0084, and 0085 then established the production Qt boundary, the shared
CLIENT host, and the installed-runtime selection boundary. Work ADR 0087 now
occupies `orna.std/8` with the bounded `std.data.Rows` value and retained table
presenter. The next available standard snapshot is therefore V9.

The Qt provider already understands a closed structural set of child contracts,
but source has no standard functions that construct those child nodes. A source
author cannot compose even a small static UI without hand-building a payload or
relying on a demo helper. This decision adds only the fixed source-callable
constructors needed for text, a disabled/enabled button, containers, and a text
input.

The canonical UI representation is the closed value shape in
`spec/spec/std_ui_value_v1.schema.json` (also retained as
`spec/spec/ui_ir_v1.schema.json`): `empty`, `fragment`, or `node`. A node has a
contract object, typed properties, named slot arrays, and action descriptors.
The current Qt adapter in `crates/orna-client/src/runtime_adapter.rs` accepts
these structural contracts and maps text and Boolean properties using its
existing `std.types.text` and `std.types.boolean` representations. Its
containers use a `content` slot. The canonical design keeps UI out of the core
language and treats a UI-producing function as an ordinary CLIENT function
returning `std.ui.UI`.

This missing slice is deliberately smaller than a complete UI language. These
constructors are deterministic value construction, not another runtime or a
hidden Studio service. `std.ui.window@1` remains the only accepted operation
that opens a runtime surface.

## Decision

### Append-only standard revision after Rows V8

Add `orna.std/9` as an append-only child of `orna.std/8`. V8 is the accepted
Rows snapshot from work ADR 0087; its source bytes, catalogue records, source
origins, executable records, codec facts, accepted digest, and identity remain
unchanged when V9 is built. V9 also retains every V1-V7 fact byte-for-byte. The
V9 snapshot uses the existing `StandardLibraryDigestVersion::Version2` domain;
this decision does not create a new digest format.

All Orna identities below are exact 16-byte values in network order. The full
hex form is shown so a suffix is not an allocation hint:

```text
...08 = 00000000000000000000000000000008
...09 = 00000000000000000000000000000009
...0A = 0000000000000000000000000000000A
...12 = 00000000000000000000000000000012
...15 = 00000000000000000000000000000015
...1A = 0000000000000000000000000000001A
```

The V9 revision facts are:

| Record | Exact identity or value |
| --- | --- |
| standard-library version | `orna.std/9` |
| `StandardLibraryRevisionId` | `00000000000000000000000000000009` (`...09`) |
| standard `CatalogueRevisionId` | `00000000000000000000000000000009` (`...09`) |
| standard `SourceBundleId` | `00000000000000000000000000000009` (`...09`) |
| standard `SourceRevisionId` | `00000000000000000000000000000009` (`...09`) |
| source-revision parent | V8 Rows source revision `00000000000000000000000000000008` (`...08`) |
| new source logical path | `std/ui_constructors.orna` |
| new source ordinal | `8` |
| new `SourceUnitId` | `0000000000000000000000000000000A` (`...0A`) |
| language version | `orna.language/1` |
| unchanged UI schema | `std.ui`, `00000000000000000000000000000008` (`...08`) |
| unchanged UI value type | `std.ui.UI`, `00000000000000000000000000000019` (`...19`) |
| unchanged UI value contract | `orna.std.value.ui@1` |
| unchanged UI frame magic | `ORNA-UI/1 ` |
| retained Rows V8 schema | `std.data`, `...07` |
| retained Rows V8 value type | `std.data.Rows`, `...12` |
| retained Rows V8 contract | `orna.std.value.rows@1` |
| retained Rows V8 frame magic | `ORNA-ROWS/1 ` |

The V9 source bundle is:

| Ordinal | Logical path | Source-unit identity | V9 treatment |
| ---: | --- | --- | --- |
| `0` | `std/types.orna` | `...02` | retain V1/V2 source bytes |
| `1` | `std/invoke.orna` | `...03` | retain V2 source bytes |
| `2` | `std/output.orna` | `...04` | retain V3 source bytes |
| `3` | `std/ui.orna` | `...05` | retain V4 source bytes |
| `4` | `std/json.orna` | `...06` | retain V5 source bytes |
| `5` | `std/action.orna` | `...07` | retain V6 source bytes |
| `6` | `std/window.orna` | `...08` | retain V7 source bytes |
| `7` | `std/data.orna` | `...09` | retain the V8 Rows source bytes from work ADR 0087 |
| `8` | `std/ui_constructors.orna` | `...0A` | append the seven declarations below |

The order, path, ordinal, identity, bytes, and final newline are all part of
the V9 source-bundle digest. A reordered, omitted, duplicated, renamed, or
modified retained unit is not `orna.std/9`.

The Rows allocation occupies `std.data` schema `...07`, `std.data.Rows` type
`...12`, and the retained `std.terminal.present_table` function/parameter
identities `...12`; it does not occupy any constructor identity from `...15`
through `...1F`. No constructor ID shift is required. The constructor IDs below
remain the exact shared decision values because they do not collide with the
Rows V8 allocation.

### Exact source unit and signatures

`stdlib/std/ui_constructors.orna` contains exactly the following declarations
and no schema, type, export, state, action, model, or other declaration:

```sql
CREATE EXTERNAL CLIENT FUNCTION std.ui.text (
    text TEXT
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.text@1';

CREATE EXTERNAL CLIENT FUNCTION std.ui.button (
    label TEXT,
    enabled BOOLEAN
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.button@1';

CREATE EXTERNAL CLIENT FUNCTION std.ui.panel (
    content std.ui.UI
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.panel@1';

CREATE EXTERNAL CLIENT FUNCTION std.ui.row (
    content std.ui.UI
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.row@1';

CREATE EXTERNAL CLIENT FUNCTION std.ui.column (
    content std.ui.UI
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.column@1';

CREATE EXTERNAL CLIENT FUNCTION std.ui.text_input (
    text TEXT,
    placeholder TEXT,
    enabled BOOLEAN
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.text_input@1';

CREATE EXTERNAL CLIENT FUNCTION std.ui.tabs (
    content std.ui.UI
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.tabs@1';
```

Every function is a `CLIENT` function with `SECURITY INVOKER`, no transaction
clause, and immutable volatility under the existing CLIENT function shape.
Every parameter is required, ordered, and has no default expression. The
runtime-contract literal and the external body identity are the same exact
contract identity. No capability requirement is declared.

The stable function, revision, and parameter identities are:

| Function | Contract | `FunctionId` | `FunctionRevisionId` | Ordered parameters (`ordinal`, name, `ParameterId`) | Revision number |
| --- | --- | --- | --- | --- | ---: |
| `std.ui.text` | `std.ui.text@1` | `...15` (`00000000000000000000000000000015`) | `...15` (`00000000000000000000000000000015`) | `0`, `text`, `...16` (`00000000000000000000000000000016`) | `1` |
| `std.ui.button` | `std.ui.button@1` | `...16` (`00000000000000000000000000000016`) | `...16` (`00000000000000000000000000000016`) | `0`, `label`, `...17` (`00000000000000000000000000000017`); `1`, `enabled`, `...18` (`00000000000000000000000000000018`) | `1` |
| `std.ui.panel` | `std.ui.panel@1` | `...17` (`00000000000000000000000000000017`) | `...17` (`00000000000000000000000000000017`) | `0`, `content`, `...19` (`00000000000000000000000000000019`) | `1` |
| `std.ui.row` | `std.ui.row@1` | `...18` (`00000000000000000000000000000018`) | `...18` (`00000000000000000000000000000018`) | `0`, `content`, `...1A` (`0000000000000000000000000000001A`) | `1` |
| `std.ui.column` | `std.ui.column@1` | `...19` (`00000000000000000000000000000019`) | `...19` (`00000000000000000000000000000019`) | `0`, `content`, `...1B` (`0000000000000000000000000000001B`) | `1` |
| `std.ui.text_input` | `std.ui.text_input@1` | `...1A` (`0000000000000000000000000000001A`) | `...1A` (`0000000000000000000000000000001A`) | `0`, `text`, `...1C` (`0000000000000000000000000000001C`); `1`, `placeholder`, `...1D` (`0000000000000000000000000000001D`); `2`, `enabled`, `...1E` (`0000000000000000000000000000001E`) | `1` |
| `std.ui.tabs` | `std.ui.tabs@1` | `...1B` (`0000000000000000000000000000001B`) | `...1B` (`0000000000000000000000000000001B`) | `0`, `content`, `...1F` (`0000000000000000000000000000001F`) | `1` |

The existing V7 `std.ui.window` function remains `FunctionId ...14`, with its
existing `title ...14` and `content ...15` parameters. The V8 Rows identities
remain those recorded by work ADR 0087. The V9 allocations above are distinct
from both earlier sets.

### Canonical `ORNA-UI/1` mapping

A constructor returns one `RuntimeValue::Opaque` whose opaque type is the
existing `std.ui.UI` type. Its canonical bytes remain exactly:

```text
ASCII `ORNA-UI/1 `
<big-endian u32 body length>
<exactly that many canonical UTF-8 JSON bytes>
```

There are no trailing bytes. The JSON body is a single canonical `node`
object from the existing schema. Its contract object always has equal `id` and
`name` fields and version string `"1.0"`; the `@1` suffix belongs to the
external runtime-contract identity, while `"1.0"` is the node's major/minor
contract version consumed by the current Qt adapter. Every generated node has
exactly the required `kind`, `contract`, `properties`, `slots`, and `actions`
fields. The optional `key`, `call_site_id`, `function_instance_id`, and
`source_origin` fields are omitted.

The exact semantic mapping is:

| Constructor | Node contract (`id = name`, version) | `properties` | `slots` | `actions` |
| --- | --- | --- | --- | --- |
| `std.ui.text(text)` | `std.ui.text`, `"1.0"` | `text: {"type":"std.types.text","value": text}` | `{}` | `{}` |
| `std.ui.button(label, enabled)` | `std.ui.button`, `"1.0"` | `label: {"type":"std.types.text","value": label}`; `enabled: {"type":"std.types.boolean","value": enabled}` | `{}` | `{}` |
| `std.ui.panel(content)` | `std.ui.panel`, `"1.0"` | `{}` | `content: [content]` | `{}` |
| `std.ui.row(content)` | `std.ui.row`, `"1.0"` | `{}` | `content: [content]` | `{}` |
| `std.ui.column(content)` | `std.ui.column`, `"1.0"` | `{}` | `content: [content]` | `{}` |
| `std.ui.text_input(text, placeholder, enabled)` | `std.ui.text_input`, `"1.0"` | `text: {"type":"std.types.text","value": text}`; `placeholder: {"type":"std.types.text","value": placeholder}`; `enabled: {"type":"std.types.boolean","value": enabled}` | `{}` | `{}` |
| `std.ui.tabs(content)` | `std.ui.tabs`, `"1.0"` | `{}` | `content: [content]` | `{}` |

For example, `std.ui.text('Ready')` produces the body:

```json
{"kind":"node","contract":{"id":"std.ui.text","name":"std.ui.text","version":"1.0"},"properties":{"text":{"type":"std.types.text","value":"Ready"}},"slots":{},"actions":{}}
```

`std.ui.button('Run', true)` produces the same closed node shape with the
`std.ui.button` contract and the two typed properties. A container places the
one supplied `std.ui.UI` value in the one `content` slot array; it does not
flatten it, turn it into a sibling list, or create a second slot. The content
value is decoded from its already-validated `ORNA-UI/1` frame and embedded as
its JSON value, not embedded as a string containing frame bytes.

The property type labels and value forms above are the existing Qt v1 surface
mapping. In the catalogue, source `TEXT` resolves to the retained
`std.types.character_large_object` value type (`TypeId ...06`) and source
`BOOLEAN` resolves to the retained `std.types.boolean` value type (`TypeId
...01`). The UI property labels remain `std.types.text` and
`std.types.boolean`, as required by `lower_property` in the current adapter;
they are not a new type or a second codec.

This constructor set intentionally has no source-level fragment constructor,
variadic child argument, or sibling-list constructor. A slot is always a
singleton array containing the one `content` argument. The existing UI schema
may continue to decode historical `empty` and `fragment` values, but this V9
source unit does not create them.

### Compiler and standard-library ownership

The compiler and standard library retain the existing source-to-catalogue
authority rules:

* `crates/orna-compiler/src/resolver/model.rs` receives the V9 revision and
  constructor identity constants and checked declaration facts. The IDs are
  constants, not runtime allocation results. The already accepted V8 Rows
  constants remain unchanged.
* `crates/orna-compiler/src/resolver.rs` adds an exact V9 source branch beside
  the V8 branch. It checks the nine-unit order, all retained source bytes, the
  new unit's seven declarations, contract literals, parameter order and names,
  defaults absent, `TEXT`/`BOOLEAN`/`std.ui.UI` type identities, `std.ui.UI`
  return type, invoker/immutable CLIENT shape, and complete source origins.
  Shape failures use the existing typed standard-check failures rather than a
  permissive generic external-contract path. The V8 Rows checker and its
  retained table function remain intact.
* Each constructor's durable executable uses the existing
  `orna.client-plan` expression representation for one
  `ExternalContract` identity (`std.ui.*@1`). There is no client-plan format
  bump and no new unbounded JSON-plan escape hatch. The executable's ordered
  definition references are the parameter type references followed by the
  return `std.ui.UI` reference; their source origins point into
  `std/ui_constructors.orna`.
* `crates/orna-standard/src/lib.rs` adds the V9 manifest over the verified V8
  Rows snapshot, the retained V9 source snapshot, canonical content/source/
  revision/standard digest checks, the seven executable records, and the V8-
  to-V9 upgrade seam. The V9 catalogue retains the V8 function and executable
  records in canonical ascending `FunctionId` order: `std.invoke.echo`,
  `std.json.encode`, `std.terminal.present_table`, and `std.ui.window`. It then
  appends the seven constructor functions in the declaration order shown
  above.
* `registered_opaque_codecs` retains every V8 Rows registration and continues
  to register the same `std.ui.UI` codec (`STD_UI_TYPE_ID`, `STD_UI_CONTRACT`,
  `UI_MAGIC`) for an accepted V9 snapshot. V9 does not add a second UI type, a
  second magic prefix, or a runtime-selected codec.

The source checker may share a table-driven exact-shape helper among the seven
constructors, but the accepted surface is closed: an extra parameter, default,
capability, property, contract version, or declaration is a V9 mismatch.

### Evaluator intrinsic boundary

The local CLIENT evaluator owns constructor evaluation. The ordinary checked
CLIENT call path still evaluates arguments in declaration order, consumes the
existing execution fuel, validates the active function and revision, and
requires the stable definition references. Once a constructor's external body
is reached, the evaluator selects an intrinsic only when both of these facts
hold:

1. the resolved active function is one of the seven V9 constructor
   `FunctionId`/`FunctionRevisionId` pairs above; and
2. its retained external identity is the exact matching `std.ui.*@1` string.

Matching a contract string alone is insufficient: an application-owned
external function must not acquire standard constructor semantics merely by
spelling the same contract.

The intrinsic then:

1. checks the ordered parameter identities and runtime value kinds;
2. obtains the verified standard snapshot from the active revision;
3. obtains the registered opaque codec set through
   `orna_standard::registered_opaque_codecs`;
4. builds the one closed node mapping above, revalidating an input `content`
   payload against the active UI codec before embedding it;
5. encodes the canonical `ORNA-UI/1` frame; and
6. constructs the result through `OpaqueValue::new(active, registry,
   STD_UI_TYPE_ID, payload)` and returns `RuntimeValue::Opaque`.

The final `OpaqueValue::new` call is required even though the builder created
the frame. It binds the result to the active standard snapshot and makes the
registered codec enforce the existing frame, canonical JSON, closed UI shape,
payload, and recursive-node rules. The intrinsic owns no runtime session and
performs no filesystem, database, network, process, environment, clock,
randomness, state, resource, or action operation.

The intrinsic path is not a generic runtime executor call. It must not create a
`ClientExternalContractRequest`, call `ClientResourceExecutor::external_contract`,
load a runtime, or create a `RuntimeUiBatch`. In particular, a constructor can
be evaluated with no runtime session because it only returns a typed immutable
value.

`std.ui.window@1` is the sole exception. Its existing external-contract path
continues to create the host-owned request; `QtRuntimeExecutor::execute_window`
passes the resulting UI frame to `show_window`, and only that operation creates
or shows a runtime surface. The Qt adapter continues to lower the generated
child nodes using its existing `parse_node`, `lower_property`, `content` slot,
and structural contract rules. The constructor functions do not become seven
new runtime entry points.

### Typed errors and redaction

The implementation preserves the current typed error families and their
fail-closed meaning:

* A malformed constructor call, wrong parameter identity, wrong argument kind,
  or wrong ordered argument count is an
  `ExpressionEvaluation` carrying `ClientExpressionError::InvalidCall` or
  `ClientExpressionError::TypeMismatch`, as appropriate. It is never silently
  coerced.
* A missing active standard snapshot, mismatched active snapshot, inactive UI
  registration, unregistered UI type, invalid frame length, wrong magic,
  invalid UTF-8, non-canonical JSON, or malformed UI node is an
  `InvalidOpaqueValue` carrying the existing typed
  `ClientOpaqueValueError::Registry` or `ClientOpaqueValueError::Value` and
  its `OpaqueValueError` source.
* A missing constructor function, stale active revision, invalid artefact, or
  invalid reference continues to use the evaluator's existing
  `FunctionNotFound`, `InvalidActiveRevision`, `InvalidArtifact`, or
  `InvalidFunction` variants.
* Constructor evaluation never reports the generic unavailable-runtime
  `ExternalContract` error merely because Qt is not installed. That error
  remains the failure for `std.ui.window@1` or another external contract when
  no host provider exists.

The public/displayed forms remain redacted. Constructor errors do not contain
text, labels, placeholders, Boolean values, nested UI bytes, credentials,
principal identities, grants, paths, environment values, or runtime-owned
payloads. The result value itself intentionally contains the requested UI text
and typed values; redaction applies to failures, diagnostics, logs, and
inspection/security evidence, not to a successful UI value. The runtime adapter
continues to return its stable redacted failure and the Qt provider continues
to reject malformed values before surface creation.

Root authorization remains the existing ADR 0020 gate. The seven functions are
known standard functions in the active catalogue and are evaluated only after
the enclosing invocation has an allowed, pinned function/revision decision.
The intrinsic adds no principal, grant, capability, or authority path.

### Bounds and canonical failure rules

No bound is raised or bypassed by V9. The constructor and evaluator retain:

* the existing client-plan artefact ceiling (`MAX_ARTIFACT_BYTES`, 16 MiB),
  expression depth (`MAX_EXPRESSION_DEPTH`, 64), expression-node count
  (`MAX_EXPRESSION_NODES`, 1024), and call-argument ceiling
  (`MAX_CALL_ARGUMENTS`, 64);
* the registered opaque codec payload ceiling
  (`MAX_OPAQUE_CODEC_PAYLOAD_LENGTH`, 16 MiB), the `u32` body length, exact
  frame end, UTF-8 check, canonical JSON re-encoding check, and no-trailing-
  bytes rule;
* the UI codec's recursive runtime-value bound
  (`MAX_RUNTIME_VALUE_NODES`, 65,536); and
* the existing Qt/client limits when `std.ui.window@1` later consumes the
  result: `CLIENT_MAX_RUNTIME_TEXT_BYTES` (4,096),
  `CLIENT_MAX_RUNTIME_VALUE_BYTES` (16 MiB),
  `CLIENT_MAX_RUNTIME_BATCH_OPERATIONS` (1,024), and the adapter's
  `MAX_UI_NODES` (4,096).

The builder must serialize through a bounded path or fail before retaining an
oversized result. It must not replace these limits with an unbounded temporary
JSON tree, an unchecked `u32` conversion, or a special case for constructor
payloads. Existing malformed-frame, duplicate-key, unsupported contract,
wrong property type, invalid slot, and child-ordinal failures remain failures
before any runtime surface changes. The V8 Rows payload, row/cell, and table
presenter bounds remain those defined by work ADR 0087.

### Compatibility and upgrade

The accepted upgrade for this constructor slice is only `orna.std/8` to
`orna.std/9`. The standard upgrade precondition is the exact installed V8 Rows
standard revision. A V7 or earlier installation first follows its existing
chain to V8 and then this V8-to-V9 edge; there is no direct V7-to-V9 shortcut
and no replacement of the V8 Rows snapshot. The server, source apply, recovery,
and offline LSP standard-selection tables must recognise V9 through the same
verified-snapshot path used by V8.

An installed V8 Rows database and V8 application remain valid and retain their
existing behaviour. They cannot resolve the V9 constructor identities until
the standard snapshot is upgraded. A V9 application that references a
constructor cannot be evaluated against V8: compiler reference checking and
active-catalogue validation fail closed rather than treating a missing
constructor as a runtime contract or selecting a fallback.

The existing client-plan versions remain mutually compatible and byte-stable;
constructor executable bodies use the current expression external-contract
representation. The V9 addition does not change `std.ui.window@1`, the Qt
runtime descriptor, the installed path, the ABI, the caller-pumps lifecycle,
or the `ORNA-UI/1` codec. V1-V8 bytes and accepted goldens remain unchanged;
only V9's newly computed source bundle, source revision, catalogue,
executable, and standard-library digests are new facts.

## Alternatives considered

### Send every constructor to the runtime executor

Rejected. Text, Boolean, and structural composition are pure construction of a
standard value. Requiring a runtime session for each child would make static
composition unavailable in headless/client-only evaluation, create seven
unnecessary executor operations, and blur the distinction between producing a
UI value and opening a surface. The runtime receives the final UI tree when
`std.ui.window@1` is invoked.

### Add a generic `std.ui.node` or raw JSON constructor

Rejected. It would bypass the closed compiler signature, typed property mapping,
contract allow-list, active-revision check, canonical UI codec, and bounded
failure path. It would also create a second UI semantics surface alongside the
existing runtime adapter.

### Add a core `UI` keyword or a separate UI DSL

Rejected by the canonical UI-is-a-standard-value model. These are ordinary
standard-library CLIENT functions returning `std.ui.UI`; core remains aware
only of the typed value and its registered codec.

### Keep using demo helpers or let applications hand-author opaque JSON

Rejected. Demo code in `crates/orna-client/examples/studio_demo.rs` is useful
runtime smoke evidence but is not a durable source API. Hand-authored payloads
would duplicate the contract mapping and would not provide stable standard
function identities or compiler reference evidence.

### Accept defaults, variadic children, fragments, keys, actions, or models now

Rejected for this bounded slice. Defaults change the exact source and parameter
contract; variadic children and fragments require sibling/cardinality and
reconciliation semantics; keys require identity policy; actions require the
accepted action/event execution boundary; and models require range,
completion, cancellation, and backpressure semantics. Each remains a separate
contract.

### Make constructors open runtime surfaces or perform Studio/database work

Rejected. Only `std.ui.window@1` opens a surface. Constructors must not read or
write PostgreSQL, execute SERVER work, mutate Studio state, load libraries, or
select a runtime. Native loading and package selection remain the local client
boundaries in work ADRs 0082 and 0085.

## Consequences

This decision makes the smallest useful source-authored static UI composition
available through ordinary CLIENT code:

```sql
CREATE CLIENT FUNCTION app.body()
RETURNS std.ui.UI
AS
    std.ui.column(
        content => std.ui.panel(
            content => std.ui.text('Ready')
        )
    );
```

A root CLIENT function may place that value under the existing
`std.ui.window(title TEXT, content std.ui.UI)` entry point. The nested result is
one validated immutable `std.ui.UI` value and can be consumed by the current
Qt structural lowering without a constructor-specific runtime call.

The cost is one new append-only standard snapshot and seven durable function
families after the V8 Rows snapshot. The compiler, standard verifier, client
evaluator, and standard selection consumers must all carry the V9 identity and
digest facts. The runtime package itself does not gain constructor entry points,
and the core value codec remains the single authority for UI payload validity.

This unblocks only the constructor and static source-authored UI slice. It does
not unblock full Studio.

## Deferred surface

The following are explicitly outside `orna.std/9`:

* **Actions and events:** action constructors, button click handlers,
  `std.action.Action` composition, state setters, event payloads, command
  routing, callback-to-SERVER dispatch, and navigation actions. The button's
  `enabled` property is static data; it is not an action binding.
* **Models and collections:** list/table/data-grid/tree contracts, virtual
  models, range/child requests, row completion, cancellation, streaming model
  updates, backpressure, collection/range `FOR`, and any variadic or sibling
  child API. The accepted V8 materialised Rows value and table presenter do not
  turn into a graphical model through this ADR.
* **Identity and dynamic UI metadata:** keys, call-site or function-instance
  metadata generated by these constructors, source-origin decoration, fragment
  constructors, sibling lists, reconciliation identity policy, and hot reload
  semantics beyond the existing evaluator/runtime contracts.
* **Input, focus, and accessibility:** focus operations, accessibility
  metadata, accessibility labels/roles, text-input change events, clipboard,
  drag/drop, native input routing, and platform-specific input semantics.
* **Studio:** a complete Orna Studio root and workspace, catalog tree, source
  editor, diagnostics view, result view, command palette, connections pane,
  dock layout, document lifecycle, Inspector UI, launch metadata, or a second
  Studio-specific UI API. The constructors do not make Studio itself an
  accepted application.
* **Studio database operations:** source check/apply, catalogue/schema
  browsing, SQL execution, SERVER queries/mutations, security administration,
  resources, durable documents, USER/SESSION state persistence, transaction
  controls, and any database-backed model or action. No constructor performs a
  PostgreSQL operation or receives database authority.
* **Runtime expansion:** runtime loading, runtime selection, package discovery,
  browser deployment, a second toolkit/platform, new ABI operations, native
  widget constructors, or any runtime surface other than the existing
  `std.ui.window@1` path.

These exclusions do not remove the existing accepted `std.ui.window@1`, Qt v1,
CLIENT control-flow, resource, action, Inspector, security, Rows, or package
contracts. They state only that this V9 constructor slice does not implement
those larger systems.

## Implementation artifacts

The implementation of this accepted contract is ordered as follows:

1. **Source and identity facts** — add `stdlib/std/ui_constructors.orna` with
   the exact bytes above; add V9 identity constants and checked constructor
   facts in `crates/orna-compiler/src/resolver/model.rs`; re-export the facts
   from `crates/orna-compiler/src/lib.rs` where the existing V8 facts are
   exposed. The V8 Rows constants and source owner remain unchanged.
2. **Standard catalogue and retained snapshot** — extend
   `crates/orna-standard/src/lib.rs` with the V9 catalogue append over the
   verified V8 Rows catalogue, source-unit order, retained source include,
   executable records, accepted digest facts, V8-to-V9 upgrade, and V9
   verification. Retain the V8 Rows codec and table presenter; do not rewrite
   V8 records.
3. **Compiler reconciliation** — extend
   `crates/orna-compiler/src/resolver.rs` with V9 source dispatch and exact
   constructor declaration/revision/reference reconciliation parallel to the
   existing `check_standard_ui_window` path. The checker must reject extra
   declarations, wrong IDs, wrong names, wrong type identities, defaults,
   capabilities, contracts, origins, and executable order, while preserving
   the V8 Rows checker.
4. **Plan compatibility** — keep
   `crates/orna-artifact/src/client_plan.rs`'s existing external-contract
   expression encoding and all existing decoders unchanged. The constructor
   executable records use that existing format; no generic JSON node plan is
   introduced.
5. **Intrinsic evaluation** — extend `crates/orna-client/src/lib.rs` with the
   closed FunctionId/revision-gated constructor intrinsic, bounded canonical
   node/frame builder, active-standard/codec admission, typed error mapping,
   and content-frame revalidation. The path must remain within the existing
   evaluator fuel and active-revision gate.
6. **Runtime separation** — preserve
   `crates/orna-client/src/runtime_adapter.rs`'s window-only external executor
   operation and existing structural child lowering. Add no constructor
   operation to `ClientResourceExecutor`, `RuntimeSession`, the loader, or the
   Qt ABI. The runtime's existing structural contract set remains the consumer
   of the generated child nodes.
7. **V9 consumers** — update the standard revision selectors in
   `crates/orna-lsp/src/analysis.rs`, `crates/orna-server/src/lib.rs`,
   `crates/orna-server/src/source_apply.rs`, and
   `crates/orna-postgres/src/kernel/recovery.rs` so V9 is verified and upgraded
   through the established authority path rather than selected by an
   unverified revision number. Preserve the V7-to-V8 Rows edge.
8. **Focused proof fixtures** — add focused compiler, standard, evaluator, and
   adapter coverage at the existing module test locations. The proof must use
   the exact identities and source bytes in this ADR; it must not replace the
   existing V1-V8 goldens or claim a full Studio/database integration.

## Ordered proof obligations

The boundary is complete only when the following proofs pass in this order:

1. **Historical immutability:** V1-V8 retained source units, catalogue facts,
   source origins, executable payloads, codec registrations, and accepted
   digest goldens remain byte-for-byte unchanged.
2. **V9 source bundle:** the nine units appear in the exact ordinals, paths, and
   identities above; `std/data.orna` is retained exactly from V8; the new
   source has the exact seven declarations and final newline; the V9 source
   revision has parent V8 and identity `...09`.
3. **Catalogue identity:** the V9 catalogue contains the unchanged `std.ui`
   schema and `std.ui.UI` type, the unchanged V8 Rows schema/type/presenter,
   and exactly the seven appended functions, with the exact
   function/revision/parameter IDs, ordinals, names, type identities, invoker
   security, immutable volatility, and no transaction.
4. **Compiler closure:** each retained constructor declaration reconciles only
   with its exact source origin, external body, runtime contract, parameter
   list, return type, and ordered durable references; all altered shapes fail
   closed with typed compiler errors.
5. **Executable determinism:** each constructor emits the existing bounded
   external-contract plan with its exact `std.ui.*@1` identity and stable
   artifact/semantic digest. The retained V8 executable records remain
   unchanged and V9 executable order is deterministic.
6. **Upgrade and recovery:** V8-to-V9 preparation rejects every non-V8 parent,
   preserves the active application revision and V8 Rows bytes, installs V9
   only through the existing verified compiler-backed path, and
   recovers/selects V9 only after standard digest and source verification.
7. **Codec admission:** an intrinsic succeeds only with the active verified V9
   standard and its registered `ORNA-UI/1` codec. Missing, mismatched, or
   inactive standard/codec facts produce the existing typed opaque-value
   errors. V8 Rows registration and validation remain intact.
8. **Leaf mappings:** each of `text`, `button`, and `text_input` produces the
   exact contract object, typed `std.types.text`/`std.types.boolean` property
   values, empty slot map, empty action map, and no optional key/metadata.
9. **Container mappings:** each of `panel`, `row`, `column`, and `tabs` places
   exactly one validated content value in `slots.content`, retains no sibling
   list or second slot, and leaves properties/actions empty.
10. **Frame validity and bounds:** generated frames round-trip through the
    existing UI codec and adapter; wrong magic, length, UTF-8, JSON
    canonicality, closed-shape fields, nested-node count, payload size, and
    runtime limits fail closed without partial output or surface mutation.
11. **Intrinsic/runtime separation:** a constructor evaluates successfully
    without an executor or runtime session and does not emit an external
    request; `std.ui.window@1` alone reaches the host executor and opens a
    surface. A missing Qt offer still affects window opening, not pure
    constructor evaluation.
12. **Nested window smoke:** a source-authored nested constructor tree can be
    passed to the existing window entry path, lowered by the existing Qt
    structural contract mapping, and returned/captured without changing the
    caller-pumps, handle, revision, shutdown, or redacted-failure rules.
13. **Security and redaction:** root authorisation remains required; no
    constructor grants authority or performs database/runtime loading; typed
    failures and diagnostics contain no raw constructor arguments, UI payload
    bytes, credentials, principals, grants, paths, or environment values.
14. **Compatibility:** V8 Rows callers, V7 window callers, old client-plan
    versions, the Qt ABI/descriptor, and the installed runtime path retain
    their prior behaviour while V9-only source resolves the seven new
    identities and no fallback aliases are accepted.

## Precedence and sources

This work ADR is an accepted implementation contract for the bounded constructor
slice. It does not replace the canonical UI value model, the accepted Rows
snapshot, or the existing runtime and security authorities:

* Spec ADR 0018 and work ADR 0082 remain authoritative for the Qt v1 ABI,
  caller-pumps runtime lifecycle, structural child contract set, ownership,
  limits, and redacted runtime failures.
* Spec ADR 0019 and work ADR 0083 remain authoritative for
  `std.ui.window@1`, its V7 identities, its window-only runtime operation, and
  the V1-V7 append-only history. This ADR supersedes only their deferred
  statement that no constructor beyond `window` is accepted.
* Spec ADR 0020 and work ADR 0084 remain authoritative for ordinary CLIENT
  calls, execution fuel, active revision checks, shared runtime hosts, and the
  rule that the database cannot select native code. This ADR adds the
  constructor intrinsic branch within that evaluator; it does not add a new
  CLIENT plan version.
* Spec ADR 0021 and work ADR 0085 remain authoritative for the separate
  `orna-runtime-qt` package, fixed installed path, pathless offers, and
  fail-closed runtime selection. Constructors do not load or select that
  package. This ADR supersedes only the deferred constructor portion.
* Spec ADR 0012 and work ADR 0062 remain authoritative for `std.ui.UI` as a
  standard-library immutable transient value, the active-standard codec gate,
  and the `ORNA-UI/1` frame. This ADR reuses that type and codec without a
  second representation.
* Work ADR 0087 remains authoritative for `orna.std/8`, `std/data.orna`,
  `std.data.Rows`, the `ORNA-ROWS/1` codec, the retained table presenter, and
  all V8 Rows bounds and failure rules. This ADR appends after that snapshot
  and does not reinterpret Rows as a UI model.
* `spec/spec/std_ui_value_v1.schema.json`, `spec/spec/ui_ir_v1.schema.json`,
  `spec/docs/10-ui-type.md`, `spec/docs/20-ui-dsl.md`,
  `spec/docs/14-presenters-surfaces-formats.md`,
  `spec/docs/15-runtime-architecture.md`, `spec/docs/32-studio.md`, and
  `spec/api/ui-runtime.md` define the canonical value, typed-property, slot,
  runtime, and Studio boundaries that this implementation narrows.
* The implementation facts are grounded in
  `crates/orna-compiler/src/resolver.rs` and `resolver/model.rs` (V7 exact
  standard checking and identities), `crates/orna-standard/src/lib.rs` (V7
  manifests, retained snapshots, upgrades, and codec registration),
  `crates/orna-client/src/lib.rs` (active CLIENT evaluation and opaque-value
  construction), `crates/orna-client/src/runtime_adapter.rs` (current Qt
  structural lowering), `crates/orna-client/src/runtime_loader.rs` (runtime
  limits and offers), and `crates/orna-artifact/src/client_plan.rs` (bounded
  external-contract plans). V8 Rows implementation facts are recorded in work
  ADR 0087 and must be consumed as a verified parent, not guessed from names.

The accepted scope is intentionally static and source-callable. It unblocks
only the constructor and static source-authored UI slice, not full Studio.
