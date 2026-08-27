# Architecture decision records

OrnaDB has two accepted ADR series. The number is unique only within its
series:

* **spec ADR NNNN** is a canonical specification decision in
  `spec/adrs/NNNN-*.md`.
* **work ADR NNNN** is an implementation decision in
  `work/docs/decisions/NNNN-*.md`.

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
| 0076 | Test-only headless runtime conformance fixture | Production runtime ABI/loading remains deferred except for the bounded Qt v1 provider accepted by 0082; browser, second-toolkit, and native platform expansion remain deferred. |
| 0077, 0078, 0079 | Resource language/transport and `std.action.call` | Virtual models, replay/cursor/cleanup semantics, sequence/parallel actions, automatic retries, graphical bindings, and reflective gateways remain deferred. |
| 0080, 0081 | Headless ordinary Inspector v1 and generic `std.inspect.render@1` | Graphical/native runtimes, live trace streams, durable snapshots, source editing/apply, and reflective gateways remain deferred. |
| 0082 | First production Qt non-TTY runtime boundary, provider, loader, and session bridge | Browser runtime, second toolkit/platform, production CLIENT VM, Studio database operations, and gateways remain separately gated. |
| 0083 | Retained `std.ui.window` CLIENT entry point and host-owned adapter boundary | General UI JSON-to-ABI transport, models, Studio operations, launch metadata, and second runtimes remain separately gated. |
| 0084 | Programmable CLIENT plans and shared runtime hosts | Collection/range `FOR`, general algebraic values, second toolkit/browser deployment, launch metadata, gateways, and broader UI transport remain deferred. |
| 0085 | Fixed installed Qt runtime package path and fail-closed production selection | Browser/second toolkit, arbitrary runtime paths, database-selected native code, and model contracts remain deferred. |
| 0086 | Bounded population of existing Inspector projection rows | Resource/UI/presenter identity enrichment is accepted by 0086; request lifecycle, full UI tree, models, and richer redaction remain deferred. |
| 0087 | Bounded `std.data.Rows` and retained table presentation | Materialised Rows, shape-preserving sealed presentation, and V8 retained table input are accepted; virtual models, Rows resources, lossless JSON, and extra presenters remain deferred. |
| 0088 | Structural UI constructors for source-authored CLIENT work | Seven V9 pure UI constructors are accepted after Rows V8; actions, models, Studio operations, metadata, and runtime expansion remain deferred. |
| 0089 | Trusted resource lineage authority: compiled evaluator and installed authenticated execution derive principal/profile/instance lineage; parent/call-site identities remain correlation-only, and direct constructors remain low-level compatibility/test seams. | Hostile external-plugin authenticated binding remains deferred; no runtime or security-surface expansion is accepted. |

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
* **work ADR 0086:**
  [Populate Existing Inspector Projection Rows](0086-populated-inspector-projections.md)
* **work ADR 0087:**
  [Bounded `std.data.Rows` and Retained Table Presenter](0087-std-data-rows.md)
* **work ADR 0088:**
  [Structural UI Constructors for Source-Authored CLIENT Work](0088-structural-ui-constructors.md)
* **work ADR 0089:**
  [Resource Lineage Authority](0089-resource-lineage-authority.md)
