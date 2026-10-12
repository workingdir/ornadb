# ADR 0111: `sys` Is a Baked Module ABI with an Extensible Provider Protocol

**Status:** Accepted (design and phased implementation plan)

**Issue:** `ornadb-c66c2` ([GitHub #5386](https://github.com/workingdir/ornadb/issues/5386))
**Supersedes:** None. This decision records the ABI direction; it does not admit a new Orna 1.0.0 operation or extension.

## Decision

Treat `sys` as a typed, baked module ABI, not as a JSON catalogue whose entries
are disconnected from execution. A single method-attribute registry is the
source for the built-in operation descriptors. Semantic analysis and runtime
linkage consume that typed registry; the registry also emits the public,
deterministic `api/sys.json` artifact. Implementations, including built-in
implementations, use one provider protocol. A future third-party provider can
add an operation or explicitly replace a declared semantic role through that
same protocol.

This is an architectural plan, not approval to change the frozen 1.0.0
contract. The 1.0.0 `api/sys.json` path and contents, existing `sys` behavior,
and ORNA-EXT-001..003 trust and component rules remain fixed. The registry
does not rewrite that contract at load time. New extension operations are
separately versioned and cannot shadow a 1.0.0 operation or semantic role
unless a later language contract explicitly makes that role replaceable.

### Registry and baked-module boundary

PR #5359 is the registry foundation: `#[ornasys]` attributes on methods are
collected at build time, checked against the published schema and closed type
graph, and projected to canonical `api/sys.json`. Keep those annotated
declarations as the only authored inventory. Do not reintroduce parallel
descriptor constants or hand-maintained tables.

The target has three related views:

1. **Authored declarations:** attributes on the implementation-facing sys
   methods state each operation's public name, typed signature, effects,
   preconditions, failures, and capability imports.
2. **Baked typed registry:** build output contains stable typed descriptors
   and provider/role metadata. Semantic and runtime crates consume this view
   rather than independently interpreting JSON or maintaining their own
   operation lists.
3. **Published projection:** the same registry emits `api/sys.json` at its
   normative path using deterministic canonical serialization. JSON Schema
   and type-graph closure validation continue to guard that artifact. The
   semantic layer may embed the artifact for conformance and publication, but
   JSON is not a second dispatch registry.

The methods introduced by #5359 currently bind descriptors and do not by
themselves implement all the operations. Phase 2 below moves actual built-in
execution behind providers. Until each operation is migrated, its existing
execution path remains authoritative and behavior-preserving.

### Primitive and role contract

Every extensible operation has a closed, versioned descriptor with:

| Contract field | Required meaning |
|---|---|
| Operation identity | Qualified operation name plus ABI major/minor; semantic-role identity where it participates in a pluggable language role. Names are exact and case-sensitive. |
| Signature | Canonical typed parameters and result, including nullability, generic/type IDs, defaults, and ownership/borrowing rules. No untyped JSON argument bag or Rust layout crosses the ABI. |
| Effects | Declared effect set and whether the operation may fail. A provider may not perform effects wider than the descriptor permits. |
| Preconditions | Machine-checkable constraints over typed arguments, host state, and required capabilities. A provider must accept every input admitted by the published precondition; hidden, narrower preconditions are incompatible. |
| Failure vocabulary | Stable namespaced codes and typed payload shapes. Providers return only declared failures (plus the ABI's bounded trap/host-failure envelope); they do not leak arbitrary host exception strings as language failures. |
| Capabilities/imports | Typed host interfaces needed by the operation, marked required or optional, with the host grant checked on every linked call. |
| Provider contract | Provider identity, ABI compatibility range, operation/role versions, and compatibility claims for signature, effects, preconditions, and failures. |

Use a small, closed initial effect vocabulary aligned with semantic effect
analysis. A descriptor's declared effects are an upper bound: a replacement
provider must be effect-compatible and cannot silently widen it. An ABI-major
change is required for incompatible signature, role, precondition, or failure
changes; additive optional capabilities or failure codes require an explicit
minor-version compatibility rule. Until such a rule is implemented, exact
version matching fails closed.

Semantic roles are pluggable only when declared so. Exactly one provider is
selected per role and role version in a resolved module environment. Required
roles without a compatible provider fail during admission/linking; optional
roles may use a language-defined fallback. Selection is explicit in the
resolved environment, never “last plugin wins.” A provider implementing a
role must satisfy its type, effect, precondition, and failure contract.

### Resolution, capabilities, and fallback

Resolve statically known built-in operations from the baked registry during
semantic admission. For a pluggable role or extension operation, resolve a
typed operation reference against the active provider set and produce a
linkage record containing the operation ID, selected provider/version, and
validated contract. A dynamic call site may cache that record, as
`invokedynamic` caches a linked call-site target, but the cache key includes
the provider-set generation and capability grant generation. A changed
environment relinks before the next call. This linkage mechanism does not
change ordinary Orna name lookup or turn arbitrary strings into callable
operations.

Capabilities are opt-in and scoped. A provider sees only the typed host
imports declared by its component and granted by the host. An optional
capability is negotiated through `supports(capability, version)` and an
optional typed dispatch slot; the slot returns a declared
`FeatureUnavailable` result if called without a grant. The extension checks
`supports()` and chooses its specified fallback. `supports()` is discovery,
not authorization: the host checks grants again at invocation, so a stale
answer cannot preserve authority. Missing optional capabilities never create
ambient access. Required capability denial is an admission or invocation
failure with a stable typed code.

WIT imports are normally required for a component to instantiate. Therefore
the ABI must not model an optional operation by pretending a missing required
WIT import exists. Instead, optional features use a declared capability
query/dispatcher interface whose answer and operation result are typed. The
host wires only granted interfaces and imports; extensions have no route to
unlisted host functionality.

### Built-in sys and third-party providers

The baked system module is the first provider in this protocol, not a
privileged parallel dispatcher. Its built-in implementations register with
the same typed descriptors and are invoked through the same resolver,
compatibility checks, effect accounting, and typed result/failure boundary
as extension implementations. The small trusted bootstrap kernel may own
provider discovery, capability grants, typed value transport, and resource
budgets; it does not duplicate operation semantics. During migration, an
adapter may call a legacy implementation behind the provider boundary, then
be removed when that operation is converted.

Third-party portable extensions use the WebAssembly Component Model and WIT.
The component's declared imports/exports are its typed interface and its
capability request surface. The host decides which declared imports to grant.
Components receive bounded typed values and scoped host calls only: never raw
pointers, Turso handles, Git handles, or other repository/runtime internals.
Traps and resource exhaustion become bounded provider failures and cannot
corrupt host memory. Enforce coarse execution budgets and cancellation as
recommended by ORNA-EXT-004. Pinning the Git object identifies the portable code;
installing/running it remains an explicit trust decision under ORNA-EXT-002.
WIT imports do not require a second handwritten permissions list merely to
repeat the same interface (ORNA-EXT-001).

System integrations that are not suitable for an in-process component use
versioned out-of-process adapters and the same typed request/result, effect,
capability, and failure contract. Native extensions remain an explicitly
trusted, unsandboxed option; the loader must label that trust class and never
present native code as equivalent to a Wasm sandbox (ORNA-EXT-005).

### Prior art and adopted constraints

| Prior art | Constraint adopted for Orna |
|---|---|
| [LLVM intrinsics](https://www.llvm.org/docs/LangRef.html) | Intrinsics are typed, named semantic operations with documented behavior. Prefer an ordinary provider operation where possible; reserve special semantic roles for operations the compiler/runtime genuinely needs to recognize. |
| [Rust lang items](https://rustc-dev-guide.rust-lang.org/lang-items.html) | Model compiler-known semantic roles as explicit pluggable roles with one selected provider, a versioned contract, and effect compatibility. This is an Orna policy derived from the model, not a claim that Rust has this exact ABI rule. |
| [JVM `invokedynamic`](https://docs.oracle.com/javase/specs/jvms/se17/html/jvms-2.html) | Permit late linkage for declared dynamic operations and cache a validated target at a call site. Keep linkage typed and scoped to a provider-set generation. |
| [WIT](https://component-model.bytecodealliance.org/design/wit.html) and [Component Model worlds](https://component-model.bytecodealliance.org/design/worlds.html) | Describe an open, typed import/export boundary. Treat imports as capabilities; a component can access only interfaces the host wires. Keep shared-memory and raw-handle access outside the contract. |

## Phased ABI slice plan

Each phase is a separately reviewable slice with focused public-interface
proofs. Keep each phase behavior-preserving unless that phase explicitly adds
a versioned extension behavior.

| Phase | Slice | Proof and exit condition |
|---|---|---|
| 0 — registry base (complete) | PR #5359 collects method attributes, publishes the schema, validates type-graph closure, and emits deterministic `api/sys.json`. | Existing generation tests prove the artifact is unchanged byte-for-byte and the in-crate fixture resolves through the published signature. |
| 1 — typed baked descriptors | Extend the annotated declarations and build output with operation/role IDs, ABI versions, typed parameter/result descriptors, effects, preconditions, failures, and capability declarations. Generate one typed registry consumed by semantic and runtime crates; continue emitting the same 1.0 JSON projection. | Prove descriptor uniqueness, schema/type closure, effect/precondition/failure validation, deterministic registry ordering, and unchanged 1.0 `api/sys.json`. Invalid or incomplete annotations fail the build. |
| 2 — provider protocol and built-in sys | Define the typed provider/call context and result/failure boundary. Register built-in sys implementations through it. Migrate operations incrementally, with a legacy adapter at the seam; remove each old dispatch branch only when its provider passes parity proofs. | Fake and built-in providers run through the same resolver. Prove 1.0 operation results, diagnostics, and effects remain identical for migrated operations; denied/unavailable calls fail closed. |
| 3 — semantic roles and linkage | Add explicit role declarations, provider selection, compatibility checks, and cached call-site linkage. Keep the default role sealed unless its contract opts into replacement. | Prove one-provider-per-role, deterministic selection, required-role missing/version mismatch diagnostics, effect widening rejection, and relinking on provider/grant generation changes. |
| 4 — optional capability negotiation | Add typed `supports()` and optional dispatch slots with declared fallbacks. Enforce grants at call time, not only during discovery or instantiation. | Prove absent optional capability selects the documented fallback; unsupported required capability returns a stable code; stale `supports()` cannot bypass a revoked grant. |
| 5 — portable third-party components | Load pinned WIT-described Wasm components in a Wasm sandbox. Scope imports to grants; enforce execution budget/cancellation; map traps to bounded failures. A component may export ordinary operations or an explicitly replaceable role under the same provider ABI. | Prove import isolation, no raw handles, typed call/result roundtrip, trap/budget containment, pinned identity, and rejection of incompatible contracts. No 1.0 built-in behavior is replaceable by default. |
| 6 — integration adapters and trusted native path | Add separately versioned out-of-process adapters for system integrations. If native extensions are later admitted, label and gate them as trusted unsandboxed code. | Prove protocol-version negotiation, typed failure/effect behavior, adapter loss/cancellation, and trust classification. Native support remains outside the portable sandbox guarantee. |

## Pragmatic choices where the frozen reference is silent

* The registry is the source for typed descriptors; JSON is its stable public
  projection. Semantic/runtime crates must not each parse and reinterpret the
  artifact as independent inventories.
* Exact ABI-major compatibility and fail-closed behavior are the initial
  policy. Additive minor compatibility is admitted only after explicit rules
  and tests exist.
* Provider choice is fixed for one admitted environment and never depends on
  load order. One provider may fill a role; extension operations otherwise
  occupy their own namespaced identities.
* Optional capability negotiation uses typed `supports()` plus a typed
  optional call result because Component Model imports are required when
  declared. A query cannot grant authority.
* Built-in 1.0 operations are pinned to the built-in provider. Third-party
  components can be first-class providers for new operations and future
  explicitly replaceable roles without changing frozen 1.0 semantics.
* Phases 1–6 are plans, not capabilities enabled by this ADR. No extension
  loader, new source syntax, native ABI, or new 1.0 API is added here.

## Precedence and boundaries

This decision implements the extension architecture within ORNA-EXT-001..003
and the typed `sys` registry direction established by #5359. If an ABI detail
here conflicts with Orna 1.0.0, the frozen language and extension contract
prevail. Changes to operation semantics, `api/sys.json`, WIT rules, or trust
classes require their own versioned contract and conformance decision.

The initial implementation work is design-only. Focused follow-on proofs use
small public registry/provider interfaces and any `.orna` fixtures are stored
under the owning crate's `tests/fixtures/` directory and included with
`include_str!`.
