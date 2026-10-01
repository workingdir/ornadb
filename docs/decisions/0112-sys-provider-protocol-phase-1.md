# Decision 0112: Phase 1 typed `sys` provider protocol

## Status

Accepted for the first implementation slice of [ADR 0111](0111-sys-baked-module-abi.md).

## Decision

The `orna-sys-v1` build collects operation declarations and semantic-role claims
from `#[ornasys]` implementation methods. It emits two deterministic build
products from that same collection:

* `api/sys.json`, the frozen Orna 1.0 public artifact, with no shape or content
  change; and
* an internal typed provider registry with parsed signatures, effect sets,
  precondition declarations, failure vocabularies, ABI versions, and role
  contracts.

Semantic loading checks that every public function has exactly one provider
contract with the same label, typed signature, and effect. Runtime startup
links the required baked roles into a one-provider-per-role registry and fails
closed if a required role is missing or incompatible. Provider offers must
preserve the role major version and stay within its effect ceiling. The
registry exposes typed argument/result and failure boundaries; it does not
load or execute third-party code in this phase.

Implementation annotations use `role = "<qualified-role>@<major>.<minor>"`.
All overloads of one role must use one version and one effect contract. The
baked 1.0 roles use provider identity `orna.sys.v1`, are required, and cannot be
replaced. The registry API can represent additional roles and provider offers;
portable extension discovery and execution remain future work.

## 1.0 compatibility choices

The reference and public artifact give each operation a typed signature,
effect, and optional precondition prose, while failure codes live in one global
list. Phase 1 therefore keeps the existing precondition text as a per-operation
declaration. It associates global failure codes whose namespace matches the
operation label, then adds the shared ABI failures `sys.abi.precondition_failed`,
`sys.abi.unavailable`, and `sys.abi.provider_failed` to every operation. This
conservative projection preserves the published 1.0 document and gives each
typed contract an explicit failure vocabulary; a later schema revision can
replace prose preconditions and namespace association with structured data.

Role compatibility requires the same major version and accepts a provider
whose minor version is at least the role's declared minor version. Effect
compatibility is set inclusion: an implementation may use fewer effects than
the role ceiling, but cannot add effects. Since all current public operation
contracts declare one effect, generated role overloads must agree on that
effect.

WIT/Component Model loading, Wasm sandbox execution, host capability imports,
and native adapters remain design-only. Phase 1 adds no raw Turso or Git handle
to the provider boundary and does not change the frozen `api/sys.json` path.
