# Architecture decision records

OrnaDB has two accepted ADR series:

* **spec ADR NNNN** is a canonical specification decision in the sibling
  `../spec/` checkout.
* **work ADR NNNN** is an implementation decision in this repository's
  `docs/decisions/` directory.

Use the qualified form when referring to a decision: `spec ADR 0001` or
`work ADR 0001`. Within a work decision file, an unqualified `ADR NNNN` means
the work series.

Work ADRs narrow implementation choices for the canonical specification. If a
work ADR conflicts with the canonical specification, its explicit `Precedence`
section governs the conflict for the scope stated in that section. This index
defines naming and traceability only. It does not change the existing
source-of-truth or authority rules.

## Status convention

The recent accepted slices below are implemented unless a deferred portion is
called out explicitly. Historical ADRs retain their original rationale and
closed boundaries; later ADRs supersede only the named deferred portion.

| Work ADRs | Implemented slice | Still deferred or superseded by |
|---|---|---|
| 0068 | CLIENT expression bodies and external runtime contracts | Resource language/transport and actions are implemented by 0077-0079; LOCAL/SESSION state, sandbox forms, tracing, and graphical runtime contracts remain deferred. |
| 0071, 0074 | Resource identity/lifecycle and the runtime executor seam | Resource language, transport, scheduling, and executable actions are implemented by 0077-0079; virtual models, retry/cache policy, and production runtime integration remain deferred. |
| 0072, 0073 | Scalar identity calls, ORV6 SET values, and sealed `active_roles` | Other sealed security calls, generic STREAM values, arbitrary SET elements, presentation-specific renderers, remote transport, and new source syntax remain deferred. |
| 0075 | V5 `std.json.Value` snapshot and recovery | GUI runtimes, resource inspection, and reflective gateways remain outside the decision. |
| 0076 | Test-only headless runtime conformance fixture | Production runtime ABI/loading remains deferred. ADR 0082 records a legacy/proposal Qt v1 boundary, not a current 1.0 acceptance; native runtime packaging and installed-runtime selection remain deferred, alongside browser, second-toolkit, and native platform expansion. |
| 0077, 0078, 0079 | Resource language/transport and `std.action.call` | Virtual models, replay/cursor/cleanup semantics, sequence/parallel actions, automatic retries, graphical bindings, and reflective gateways remain deferred. |
| 0080, 0081 | Headless ordinary Inspector v1 and generic `std.inspect.render@1` | Graphical/native runtimes, live trace streams, durable snapshots, source editing/apply, and reflective gateways remain deferred. |
| 0082 | Legacy/proposal Qt v1 runtime boundary | Current 1.0 does not accept a Qt provider, loader, session bridge, or installed-runtime selection; Qt visual smoke and screenshot evidence remain deferred; browser runtime, second toolkit/platform, production CLIENT VM, Studio database operations, and gateways remain deferred. |
| 0083 | Retained `std.ui.window` CLIENT entry point and host-owned adapter boundary | General UI JSON-to-ABI transport, models, Studio operations, `std.launch` metadata (bounded by 0094), and second runtimes remain separately gated. |
| 0084 | Programmable CLIENT plans and shared runtime hosts | Collection/range `FOR`, general algebraic values, second toolkit/browser deployment, `std.launch` metadata (bounded by 0094), gateways, and broader UI transport remain deferred. |
| 0085 | Legacy/proposal fixed Qt runtime-package selection boundary | Current 1.0 does not prescribe an installed package path or native-runtime selection policy; browser/second toolkit, arbitrary runtime paths, database-selected native code, and model contracts remain deferred. |
| 0086 | Deferred: populated Inspector projection rows are not required by frozen Orna 1.0.0 | Structural Inspect, snapshot coherence, unavailable-detail metadata, and redaction remain required; resource/UI/presenter row schemas and capture semantics remain deferred. |
| 0087 | Bounded `std.data.Rows` and retained table presentation | Materialised Rows, shape-preserving sealed presentation, and V8 retained table input are accepted; virtual models, Rows resources, lossless JSON, and extra presenters remain deferred. |
| 0088 | Structural UI constructors for source-authored CLIENT work | Seven V9 pure UI constructors are accepted after Rows V8; actions, models, Studio operations, metadata, and runtime expansion remain deferred. |
| 0089 | Trusted resource lineage authority: compiled evaluator and installed authenticated execution derive principal/profile/instance lineage; parent/call-site identities remain correlation-only, and direct constructors remain low-level compatibility/test seams. | Hostile external-plugin authenticated binding remains deferred; no runtime or security-surface expansion is accepted. |
| 0090 | Local principal and authenticated-session authority boundary for local invocation, resource, raw, and USER-state paths. | Credential/provider/secret enrollment, durable sessions, role selection, definer/effective-principal transitions, delegation, EXTERNAL principals, remote gateway auth, and production CLIENT VM trust features remain deferred. |
| 0092 | Legacy/proposal function-backed CLI-session boundary | Canonical 1.0 entry execution is `orna run` or `orna repl`; it defines no accepted `std.cli.repl` catalogue identity. Endpoint transport, persistent action loops, native session-frame wiring, remote TLS/auth, and production artifact trust remain deferred. |
| 0093 | 1.0.0 `orna serve` and `orna.present.v1` contract crosswalk | Generic reflective gateways, service discovery, JSON-RPC, MCP, custom auth, `std.protocol`/`std.service` identities and registrations, and alternate service lifecycles remain deferred. The reference specifies `orna.present.v1` under `ORNA-PROTO-001`–`ORNA-PROTO-004`; `ORNA-LIB-003` requires exact pinned module declarations rather than inferring an API from a namespace. |
| 0094 | Reference-defined `orna run` entry selection and ownership crosswalk | `std.launch` catalogue/metadata, application argument schemas, presenter/runtime selection through metadata, launch-specific authorization, and additional lifecycle behavior remain undefined and deferred. |
| 0095 | OrnaDB 1.0 principal, credential, and delegation scope | Built-in principal metadata, credential enrollment/providers, delegated sessions, and multi-user authorization remain deferred; `sys.Secret` exposes only its normative redacted metadata. |
| 0096 | OrnaDB 1.0 trust policy and function security boundary | Accepts the trusted-host and network-perimeter rules in ORNA-TRUST-001..003; function ownership, policy evaluation, and SECURITY DEFINER semantics remain undefined and deferred. |
| 0097 | Normative local REPL scope and conformance boundary | The function-backed session API from 0092 remains deferred; normative `ORNA-REPL-001` through `ORNA-REPL-006` remain in scope. |
| 0098 | Ring-1 catalogue/source/dependency/revision gateway deferral | Normative system API semantics remain; sealed Ring-1 registration is deferred pending accepted function identities and runtime authority. |
| 0099 | OrnaDB 1.0 CLIENT capability-grant configuration boundary | Local CLIENT grant loading and its configuration contract are not defined; the built-in grants database remains explicitly outside the 1.0 profile. |
| 0100 | Studio source tooling boundary | Orna 1.0 accepts semantic CLI diff and read-only retained source/revision APIs; Studio source editing/apply, revision browsing UI, and public revision activation remain deferred. |
| 0101 | Deferred: CLIENT STATE dogfood is not defined by frozen Orna 1.0.0 | ORNA-PAGE-001/002 define pages and widgets as ordinary values, not CLIENT state declarations, scope/default semantics, or StateClientPlan metadata. |
| 0102 | Qualified type editor highlighting deferral | Lowercase qualified-name capture semantics are unspecified, and the current main branch contains no editor/tree-sitter implementation to update. |
| 0103 | Studio runtime and Inspector reference boundary | Generic Orna inspection, presentation, redaction, and snapshot rules remain required; Studio-specific runtime and explorer contracts are deferred. |
| 0105 | Deferred: Studio security/DBA page reference and authority boundary | A production page, CLIENT-to-administration authority path, and its interaction contract are not defined by the frozen reference; existing security implementation and CLI are unchanged. |
| 0109 | Deferred: `orna-artifact` owns no PUB-1 immutable publication-object consumer | Keep executable plan codecs separate from compact segments, manifests, Git objects, and durability barriers; continue at the accepted runtime/storage/repository publication boundary. |
| 0110 | Source-document and object-description contracts as specified by Orna 1.0.0 | A separate bounded function-declaration metadata value and `sys.source.function` API remain gated on a canonical contract; this ADR does not add identities or runtime behavior. |
| 0111 | Design for `sys` as a baked typed module ABI with one provider protocol for built-ins and extensions. | Typed provider dispatch, semantic-role linkage, capability negotiation, Wasm loading, and adapters are phased follow-on slices; 1.0.0 `sys` semantics and `api/sys.json` remain frozen. |
| 0112 | Phase 1 typed `sys` provider protocol implementation and 1.0 compatibility choices. | Build-time typed registry and role linkage are consumed by semantic/runtime crates; Wasm/WIT loading remains deferred. |
| 0101 | OrnaDB 1.0 CLIENT VM trust boundary | Host-side remote source evaluation remains normative; production CLIENT bytecode VM, artifact trust, and CLIENT sandbox contracts are deferred. |

## Current work ADRs

* **work ADR 0001:** [PostgreSQL Is a Private Kernel](0001-private-postgresql-kernel.md)
* **work ADR 0002:** [Public Language Contract](0002-public-language-contract.md)
* **work ADR 0003:** [Active Source Revision Is Authoritative](0003-source-revision-authority.md)
* **work ADR 0004:** [Private PostgreSQL Layout Uses Stable Orna Identities](0004-private-postgresql-layout.md)
* **work ADR 0005:** [Single-Row SERVER INSERT](0005-single-row-server-insert.md)
* **work ADR 0006:** [Field Renames Are Replay-Safe Identity Transitions](0006-replay-safe-field-renames.md)
* **work ADR 0007:** [Single-Object SERVER UPDATE](0007-single-object-server-update.md)
* **work ADR 0008:** [Single-Object SERVER DELETE](0008-single-object-server-delete.md)
* **work ADR 0009:** [Identity-Selected SERVER SELECT](0009-identity-selected-server-select.md)
* **work ADR 0010:** [Parameter-Free SERVER SELECT DISTINCT](0010-parameter-free-server-select-distinct.md)
* **work ADR 0011:** [Direct Boolean SERVER SELECT Predicates](0011-direct-boolean-server-select-predicates.md)
* **work ADR 0012:** [Direct Boolean Predicates in SELECT DISTINCT](0012-direct-boolean-select-distinct-predicates.md)
* **work ADR 0013:** [Required Unique Reference Fields](0013-required-unique-reference-fields.md)
* **work ADR 0014:** [Host-Only Backend Shell](0014-host-only-backend-shell.md)
* **work ADR 0015:** [The First CLIENT Function Returns a Boolean Constant](0015-boolean-constant-client-functions.md)
* **work ADR 0016:** [Standard Scalars Are Catalogue-Backed `std` Value Types](0016-catalogue-backed-standard-types.md)
* **work ADR 0017 (partly superseded):** [Orna Ships and Owns Its PostgreSQL Runtime](0017-bundled-postgresql-runtime.md)
* **work ADR 0018:** [Source Check Is an Offline One-File Compiler Command](0018-offline-source-check.md)
* **work ADR 0019:** [PostgreSQL Is Part of the Orna Executable](0019-embedded-postgresql-engine.md)
* **work ADR 0020:**
  [Authenticated Sessions Authorise Pinned Function Execution](0020-authenticated-execute-decisions.md)
* **work ADR 0021:**
  [PostgreSQL Persists the Security Decision Snapshot](0021-durable-security-snapshot.md)
* **work ADR 0022:**
  [CLIENT Evaluation Requires an Authorised Invocation](0022-client-evaluation-requires-authorisation.md)
* **work ADR 0023:**
  [Local Sessions Authenticate with Unix Peer Credentials](0023-local-peer-authentication.md)
* **work ADR 0024:**
  [Security Decisions Append Protected Audit Records](0024-protected-security-audit.md)
* **work ADR 0025:**
  [Canonical Runtime Values Use One Binary Codec](0025-canonical-runtime-value-codec.md)
* **work ADR 0026:**
  [Raw Calls Use a Bounded Framed State Machine](0026-raw-call-frame-state-machine.md)
* **work ADR 0027:**
  [Raw CLIENT Dispatch Preserves the Protected Kernel Gate](0027-raw-client-dispatch.md)
* **work ADR 0028:**
  [The Local Raw Socket Negotiates Before Protected Dispatch](0028-authenticated-local-raw-socket.md)
* **work ADR 0029:**
  [Enum Types Are Ordered Catalogue Values](0029-catalogue-enum-value-types.md)
* **work ADR 0030:**
  [One Authenticated, Authorised, Audited SERVER SELECT](0030-authenticated-server-select.md)
* **work ADR 0031:**
  [Named Immutable Records Are Nominal Catalogue Values](0031-named-immutable-record-values.md)
* **work ADR 0032:**
  [Raw Calls Dispatch One-Column SERVER SELECTs](0032-raw-server-select-dispatch.md)
* **work ADR 0033:**
  [Local Raw Recovery Calls Use Stable Function Identities](0033-local-raw-recovery-client.md)
* **work ADR 0034:**
  [Opaque Values Require Registered Canonical Codecs](0034-opaque-values-require-registered-codecs.md)
* **work ADR 0035:**
  [Catalogue Health Is One Mandatory System Function](0035-mandatory-catalogue-health-function.md)
* **work ADR 0036:**
  [Constructed Types Use Canonical Recursive Descriptors](0036-canonical-constructed-type-descriptors.md)
* **work ADR 0037:**
  [PostgreSQL Uses One Private Rust Crate](0037-single-private-postgresql-crate.md)
* **work ADR 0038:**
  [Installed Source Apply Activates One Complete File](0038-installed-source-apply.md)
* **work ADR 0039:**
  [Canonical Collection Values Use ORV5 and ORF5](0039-canonical-collection-value-codec.md)
* **work ADR 0040:**
  [Canonical Raw Calls Bind One Boolean INSERT Argument](0040-canonical-raw-call-argument.md)
* **work ADR 0041:**
  [Canonical Raw Calls Select UPDATE and DELETE by Reference](0041-canonical-raw-reference-mutations.md)
* **work ADR 0042:**
  [Mandatory System Functions Have One Sealed Registry](0042-mandatory-system-function-registry.md)
* **work ADR 0043:**
  [Canonical Raw Calls Bind One Reference INSERT Argument](0043-canonical-raw-reference-insert.md)
* **work ADR 0044:**
  [Existing Objects Admit One Appended Nullable Boolean Field](0044-appended-nullable-boolean-fields.md)
* **work ADR 0045:**
  [Canonical Raw Calls Bind Remaining ORV1 Scalar INSERT Arguments](0045-canonical-raw-scalar-insert.md)
* **work ADR 0046:**
  [Existing Objects Admit One Appended Nullable Executable Scalar Field](0046-appended-nullable-scalar-fields.md)
* **work ADR 0047:**
  [The First 1.0 Release Uses One Authenticated Debian Authority](0047-first-one-zero-release.md)
* **work ADR 0048:**
  [Raw Reference Calls Select One Projected Object Row](0048-raw-identity-selected-server-select.md)
* **work ADR 0049:**
  [Canonical Raw Calls Bind One Bounded Argument Pair](0049-canonical-raw-argument-pairs.md)
* **work ADR 0050:**
  [Canonical Raw Calls Update One Selected Object with One Value](0050-canonical-raw-reference-value-update.md)
* **work ADR 0051:**
  [Text Fields Have Byte-Exact Uniqueness](0051-unique-text-fields.md)
* **work ADR 0052:**
  [Raw Calls Select One Object by Unique Text](0052-raw-unique-text-server-select.md)
* **work ADR 0053:**
  [Sealed `sys.invoke` Carriers Use Three ORV5 Codecs](0053-sealed-sys-invoke-carriers.md)
* **work ADR 0054:**
  [`sys.invoke` Has One Sealed Request Stream](0054-sealed-sys-invoke-signature-event-stream.md)
* **work ADR 0055:**
  [`orna.std/2` Is an Immutable Executable Source Snapshot](0055-standard-executable-source-units.md)
* **work ADR 0056:**
  [`orna invoke` Binds Typed Arguments Through the Sealed Route](0056-orna-invoke-cli.md)
* **work ADR 0057:**
  [Terminal Documents, JSON Output, and the TTY Runtime](0057-terminal-documents-json-output.md)
* **work ADR 0058:**
  [`orna.std/3` Standard Output Value Types](0058-orna-std-3-output-value-types.md)
* **work ADR 0059:**
  [Compiler-Backed `orna.std/3` Standard Upgrade](0059-compiler-backed-v3-standard-upgrade.md)
* **work ADR 0059 (duplicate historical number):**
  [Offline LSP and Editor Tooling for `.orna` Source](0059-offline-lsp-editor-tooling.md)
* **work ADR 0060:**
  [CLIENT Capability Requirements and the Local Sandbox](0060-client-capability-requirements.md)
* **work ADR 0061:**
  [Durable USER State Service](0061-durable-user-state-service.md)
* **work ADR 0062:**
  [`std.ui.UI` Standard-Library Value Type](0062-std-ui-value-type.md)
* **work ADR 0063:**
  [Automatic Runtime Selection](0063-automatic-runtime-selection.md)
* **work ADR 0064:**
  [`sys.inspect` Core](0064-sys-inspect-core.md)
* **work ADR 0065:**
  [Security Admin Functions](0065-security-admin-functions.md)
* **work ADR 0066:**
  [`orna source diff` - Semantic Source Changes Without Apply](0066-semantic-source-diff.md)
* **work ADR 0067:**
  [`std.csv.encode` - the Sealed CSV Output Presenter](0067-csv-output-presenter.md)
* **work ADR 0068:**
  [CLIENT Expression Bodies and RUNTIME CONTRACT Clauses](0068-client-expression-bodies.md)
* **work ADR 0069:**
  [CLIENT STATE Declarations and Function-Instance State](0069-client-state-declarations.md)
* **work ADR 0070:**
  [CLIENT USER State Lifecycle](0070-client-user-state-lifecycle.md)
* **work ADR 0071:**
  [CLIENT Resource Lifecycle](0071-client-resource-lifecycle.md)
* **work ADR 0072:**
  [Sealed System Identity Calls](0072-sealed-system-identity-calls.md)
* **work ADR 0073:**
  [SET Values Use ORV6 Transport](0073-set-valued-runtime-transport.md)
* **work ADR 0074:**
  [Runtime-Only CLIENT Resource Executor Seam](0074-client-resource-executor-seam.md)
* **work ADR 0075:**
  [`std.json.Value` Standard Value Snapshot](0075-std-json-value.md)
* **work ADR 0076:**
  [Headless Runtime ABI Conformance Boundary](0076-runtime-headless-conformance.md)
* **work ADR 0077:**
  [CLIENT-to-SERVER Resource Language Surface](0077-client-server-resource-language.md)
* **work ADR 0078:**
  [CLIENT-to-SERVER Resource Transport and Scheduling](0078-client-server-resource-transport.md)
* **work ADR 0079:**
  [CLIENT Action Values and `std.action.call`](0079-client-action-values.md)
* **work ADR 0080:**
  [Headless Ordinary CLIENT Inspector v1](0080-client-inspector.md)
* **work ADR 0081:**
  [Generic Standard Inspector Render Contract](0081-standard-inspector-render-contract.md)
* **work ADR 0082:**
  [First Production Qt Non-TTY Runtime Boundary](0082-production-qt-runtime.md)
* **work ADR 0083:**
  [Registered `std.ui.window` Client Function](0083-standard-ui-window.md)
* **work ADR 0084:**
  [Programmable CLIENT Plans and Shared Runtime Hosts](0084-client-control-flow.md)
* **work ADR 0085:**
  [Install and Select the Qt Runtime from a Fixed Package Path](0085-installed-qt-runtime-package.md)
* **work ADR 0086 (deferred):**
  [Populate Existing Inspector Projection Rows](0086-populated-inspector-projections.md)
* **work ADR 0087:**
  [Bounded `std.data.Rows` and Retained Table Presenter](0087-std-data-rows.md)
* **work ADR 0088:**
  [Structural UI Constructors for Source-Authored CLIENT Work](0088-structural-ui-constructors.md)
* **work ADR 0089:**
  [Resource Lineage Authority](0089-resource-lineage-authority.md)
* **work ADR 0090:**
  [Local Principal and Session Authority](0090-local-principal-session-authority.md)
* **work ADR 0091 (deferred):**
  [CLIENT VM Trust and Sandbox](0091-client-vm-trust-and-sandbox.md)
* **work ADR 0092:**
  [Function-Backed CLI Sessions](0092-function-backed-cli-sessions.md)
* **work ADR 0093 (deferred):**
  [Source Introspection Reference Gate](0093-source-introspection-reference-gate.md)
* **work ADR 0093:**
  [Gateway and Protocol Contract Boundaries](0093-reflective-gateway-contracts.md)
* **work ADR 0094:**
  [Reference-Defined Run Entry Contract](0094-std-launch-contract.md)
* **work ADR 0095:**
  [OrnaDB 1.0 Principal, Credential, and Delegation Scope](0095-principal-credential-delegation-scope.md)
* **work ADR 0096:**
  [OrnaDB 1.0 Trust Policy and Function Security Boundary](0096-trust-policy-function-security.md)
* **work ADR 0097:**
  [Normative REPL Boundary](0097-normative-repl-boundary.md)
* **work ADR 0098:**
  [Ring-1 Catalogue and Revision Gateway Deferral](0098-ring1-catalogue-revision-deferral.md)
* **work ADR 0099:**
  [OrnaDB 1.0 CLIENT Capability-Grant Configuration Boundary](0099-client-capability-grant-configuration.md)
* **work ADR 0100:**
  [Studio Source Tooling Boundary](0100-studio-source-tooling-boundary.md)
* **work ADR 0101 (deferred):**
  [Defer CLIENT STATE Dogfood as Orna 1.0.0 Conformance](0101-client-state-dogfood-reference-deferral.md)
* **work ADR 0102 (deferred):**
  [Qualified Type Editor Highlighting Deferral](0102-qualified-type-editor-highlighting-deferral.md)
* **work ADR 0103 (deferred):**
  [Studio Runtime and Inspector Reference Boundary](0103-studio-runtime-inspector-reference-boundary.md)
* **work ADR 0104 (deferred):**
  [Studio Catalogue Tree and Function Search Boundary](0104-studio-catalogue-tree-search-boundary.md)
* **work ADR 0105 (deferred):**
  [Studio Security and DBA Page Boundary](0105-studio-security-dba-page-boundary.md)
* **work ADR 0101:**
  [OrnaDB 1.0 CLIENT VM Trust Boundary](0101-client-vm-trust-boundary.md)
* **work ADR 0108 (deferred):**
  [Persistent Scalar and Record Backend Parity](0108-persistent-value-backend-parity-deferral.md)
* **work ADR 0109 (deferred):**
  [Publication Object Encoding in `orna-artifact`](0109-publication-artifact-completeness-boundary.md)
* **work ADR 0110 (deferred):**
  [Source Introspection Reference Gate](0110-source-introspection-reference-gate.md)
* **work ADR 0111:**
  [`sys` Baked Module ABI and Extensible Provider Protocol](0111-sys-baked-module-abi.md)
