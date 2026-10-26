# Work ADR 1.0 archive status

Generated for W07-14 (ADR archive). Each row is one work ADR from `docs/decisions/`.
Archived rows (in this directory) are superseded by Orna-1.1.0 or have no remaining
code path. "Still constrains" rows stay in `docs/decisions/` and cite the live code path.
Evidence: `grep -rlF` of the ADR keyword in `reference/Orna-1.1.0` and `crates/`
(fixtures included in the count only when they are not under `crates/`).

| ADR | Title | Status | Superseded-by or code path |
|---|---|---|---|
| [0003](../../0003-source-revision-authority.md) | ADR 0003: Active Source Revision Is Authoritative | still constrains | source revision: `crates/orna-core/src/canonical_hash.rs` |
| [0006](../../0006-replay-safe-field-renames.md) | ADR 0006: Field Renames Are Replay-Safe Identity Transitions | still constrains | rename: `crates/orna-application-v1/src/lib.rs` |
| [0010](../../0010-parameter-free-server-select-distinct.md) | ADR 0010: Parameter-Free SERVER SELECT DISTINCT | still constrains | SELECT DISTINCT: `crates/orna-artifact/src/server_plan.rs` |
| [0012](../../0012-direct-boolean-select-distinct-predicates.md) | ADR 0012: Direct Boolean Predicates in SELECT DISTINCT | still constrains | SELECT DISTINCT: `crates/orna-artifact/src/server_plan.rs` |
| [0013](../../0013-required-unique-reference-fields.md) | ADR 0013: Required Unique Reference Fields | still constrains | REFERENCE: `crates/orna-artifact/src/server_mutation_plan.rs` |
| [0015](../../0015-boolean-constant-client-functions.md) | ADR 0015: The First CLIENT Function Returns a Boolean Constant | still constrains | CLIENT function: `crates/orna-artifact/src/client_plan.rs` |
| [0016](../../0016-catalogue-backed-standard-types.md) | ADR 0016: Standard Scalars Are Catalogue-Backed `std` Value Types | still constrains | catalogue: `crates/orna-application-v1/src/lib.rs` |
| [0018](../../0018-offline-source-check.md) | ADR 0018: Source Check Is an Offline One-File Compiler Command | still constrains | source check: `crates/orna-evaluator-v1/src/lib.rs` |
| [0020](../../0020-authenticated-execute-decisions.md) | ADR 0020: Authenticated Sessions Authorise Pinned Function Execution | still constrains | authenticated: `crates/orna-application-v1/src/lib.rs` |
| [0022](../../0022-client-evaluation-requires-authorisation.md) | ADR 0022: CLIENT Evaluation Requires an Authorised Invocation | still constrains | invocation: `crates/orna-application-v1/src/lib.rs` |
| [0023](../../0023-local-peer-authentication.md) | ADR 0023: Local Sessions Authenticate with Unix Peer Credentials | still constrains | peer credential: `crates/orna-core/src/security.rs` |
| [0024](../../0024-protected-security-audit.md) | ADR 0024: Security Decisions Append Protected Audit Records | still constrains | audit: `crates/orna-cli-v1/src/cli_args.rs` |
| [0025](../../0025-canonical-runtime-value-codec.md) | ADR 0025: Canonical Runtime Values Use One Binary Codec | still constrains | codec: `crates/orna-artifact/src/client_plan.rs` |
| [0026](../../0026-raw-call-frame-state-machine.md) | ADR 0026: Raw Calls Use a Bounded Framed State Machine | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0027](../../0027-raw-client-dispatch.md) | ADR 0027: Raw CLIENT Dispatch Preserves the Protected Kernel Gate | still constrains | raw: `crates/orna-application-v1/src/browser-profile-host.mjs` |
| [0028](../../0028-authenticated-local-raw-socket.md) | ADR 0028: The Local Raw Socket Negotiates Before Protected Dispatch | still constrains | socket: `crates/orna-cli-v1/src/cli_serve.rs` |
| [0029](../../0029-catalogue-enum-value-types.md) | ADR 0029: Enum Types Are Ordered Catalogue Values | still constrains | enum: `crates/orna-application-v1/src/lib.rs` |
| [0031](../../0031-named-immutable-record-values.md) | ADR 0031: Named Records Are Immutable Catalogue Values | still constrains | immutable record: `crates/orna-core/src/catalogue/types.rs` |
| [0033](../../0033-local-raw-recovery-client.md) | ADR 0033: Local Raw Recovery Calls Use Stable Function Identities | still constrains | recovery: `crates/orna-application-v1/src/lib.rs` |
| [0034](../../0034-opaque-values-require-registered-codecs.md) | ADR 0034: Opaque Values Require Registered Canonical Codecs | still constrains | opaque: `crates/orna-artifact/src/client_plan.rs` |
| [0035](../../0035-mandatory-catalogue-health-function.md) | ADR 0035: Catalogue Health Is One Mandatory System Function | still constrains | health: `crates/orna-core/src/canonical_hash/tests.rs` |
| [0036](../../0036-canonical-constructed-type-descriptors.md) | ADR 0036: Constructed Types Use Canonical Recursive Descriptors | still constrains | descriptor: `crates/orna-application-v1/src/lib.rs` |
| [0038](../../0038-installed-source-apply.md) | ADR 0038: Installed Source Apply Activates One Complete File | still constrains | source apply: `crates/orna-core/src/security.rs` |
| [0039](../../0039-canonical-collection-value-codec.md) | ADR 0039: Canonical Collection Values Use ORV5 and ORF5 | still constrains | ORV5: `crates/orna-client/src/execution/inspect.rs` |
| [0040](../../0040-canonical-raw-call-argument.md) | ADR 0040: Canonical Raw Calls Bind One Boolean INSERT Argument | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0041](../../0041-canonical-raw-reference-mutations.md) | ADR 0041: Canonical Raw Calls Select UPDATE and DELETE by Reference | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0042](../../0042-mandatory-system-function-registry.md) | ADR 0042: Mandatory System Functions Have One Sealed Registry | still constrains | registry: `crates/orna-cli-v1/src/cli_serve.rs` |
| [0043](../../0043-canonical-raw-reference-insert.md) | ADR 0043: Canonical Raw Calls Bind One Reference INSERT Argument | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0044](../../0044-appended-nullable-boolean-fields.md) | ADR 0044: Existing Objects Admit One Appended Nullable Boolean Field | still constrains | nullable: `crates/orna-artifact/src/server_mutation_plan.rs` |
| [0045](../../0045-canonical-raw-scalar-insert.md) | ADR 0045: Canonical Raw Calls Bind Remaining ORV1 Scalar INSERT Arguments | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0046](../../0046-appended-nullable-scalar-fields.md) | ADR 0046: Existing Objects Admit One Appended Nullable Executable Scalar Field | still constrains | nullable: `crates/orna-artifact/src/server_mutation_plan.rs` |
| [0049](../../0049-canonical-raw-argument-pairs.md) | ADR 0049: Canonical Raw Calls Bind One Bounded Argument Pair | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0050](../../0050-canonical-raw-reference-value-update.md) | ADR 0050: Canonical Raw Calls Update One Selected Object with One Value | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0051](../../0051-unique-text-fields.md) | ADR 0051: Text Fields Have Byte-Exact Uniqueness | still constrains | unique text: spec-only (Orna-1.1.0) |
| [0052](../../0052-raw-unique-text-server-select.md) | ADR 0052: Raw Calls Select One Object by Unique Text | still constrains | raw call: `crates/orna-protocol/src/frame.rs` |
| [0053](../../0053-sealed-sys-invoke-carriers.md) | ADR 0053: Sealed `sys.invoke` Carriers Use Three ORV5 Codecs | still constrains | sys.invoke: `crates/orna-application-v1/src/lib.rs` |
| [0054](../../0054-sealed-sys-invoke-signature-event-stream.md) | ADR 0054: `sys.invoke` Has One Sealed Request Stream | still constrains | sys.invoke: `crates/orna-application-v1/src/lib.rs` |
| [0055](../../0055-standard-executable-source-units.md) | ADR 0055: `orna.std/2` Is an Immutable Executable Source Snapshot | still constrains | orna.std/2: `crates/orna-standard/src/lib.rs` |
| [0056](../../0056-orna-invoke-cli.md) | ADR 0056: `orna invoke` Binds Typed Arguments Through the Sealed Route | still constrains | orna invoke: `crates/orna-core/src/invocation_binding.rs` |
| [0057](../../0057-terminal-documents-json-output.md) | ADR 0057: Terminal Documents, JSON Output, and the TTY Runtime | still constrains | TTY: `crates/orna-client/src/session.rs` |
| [0058](../../0058-orna-std-3-output-value-types.md) | ADR 0058: `orna.std/3` Standard Output Value Types | still constrains | orna.std/3: `crates/orna-standard/src/codecs.rs` |
| [0059](../../0059-compiler-backed-v3-standard-upgrade.md) | ADR 0059: Compiler-Backed `orna.std/3` Standard Upgrade | still constrains | orna.std/3: `crates/orna-standard/src/codecs.rs` |
| [0060](../../0060-client-capability-requirements.md) | ADR 0060: CLIENT Capability Requirements and the Local Sandbox | still constrains | capability: `crates/orna-application-v1/src/lib.rs` |
| [0061](../../0061-durable-user-state-service.md) | ADR 0061: Durable USER State Service | still constrains | USER: `crates/orna-artifact/src/client_plan.rs` |
| [0062](../../0062-std-ui-value-type.md) | ADR 0062: `std.ui.UI` Value Type and Runtime Contracts | still constrains | std.ui: `crates/orna-artifact/src/client_plan/tests.rs` |
| [0064](../../0064-sys-inspect-core.md) | ADR 0064: The Inspector Core — sys.inspect Snapshot, Projections, and Trace | still constrains | sys.inspect: `crates/orna-core/src/inspect.rs` |
| [0065](../../0065-security-admin-functions.md) | ADR 0065: The Security Admin Functions — sys.security Principal, Role, and Privilege Surface | still constrains | sys.security: `crates/orna-core/src/security.rs` |
| [0066](../../0066-semantic-source-diff.md) | ADR 0066: `orna source diff` — Semantic Source Changes Without Apply | still constrains | source diff: spec-only (Orna-1.1.0) |
| [0067](../../0067-csv-output-presenter.md) | ADR 0067: `std.csv.encode` — the Sealed CSV Output Presenter | still constrains | std.csv: `crates/orna-artifact/src/server_csv_encode.rs` |
| [0068](../../0068-client-expression-bodies.md) | ADR 0068: CLIENT Expression Bodies and RUNTIME CONTRACT Clauses | still constrains | RUNTIME CONTRACT: spec-only (Orna-1.1.0) |
| [0069](../../0069-client-state-declarations.md) | ADR 0069: CLIENT STATE Declarations and Function-Instance State | still constrains | STATE: `crates/orna-artifact/src/client_plan.rs` |
| [0070](../../0070-client-user-state-lifecycle.md) | ADR 0070: CLIENT USER State Lifecycle | still constrains | USER: `crates/orna-artifact/src/client_plan.rs` |
| [0071](../../0071-client-resource-lifecycle.md) | ADR 0071: CLIENT Resource Lifecycle | still constrains | resource: `crates/orna-application-v1/src/lib.rs` |
| [0072](../../0072-sealed-system-identity-calls.md) | ADR 0072: Sealed System Identity Calls Use the `sys.invoke` Route | still constrains | sys.invoke: `crates/orna-application-v1/src/lib.rs` |
| [0073](../../0073-set-valued-runtime-transport.md) | ADR 0073: SET Values Use ORV6 Transport | still constrains | ORV6: `crates/orna-protocol/src/frame/resource_connection.rs` |
| [0074](../../0074-client-resource-executor-seam.md) | ADR 0074: Runtime-Only CLIENT Resource Executor Seam | still constrains | executor: `crates/orna-application-v1/src/lib.rs` |
| [0075](../../0075-std-json-value.md) | ADR 0075: `std.json.Value` Standard Value Snapshot | still constrains | std.json: `crates/orna-artifact/src/server_json_encode.rs` |
| [0076](../../0076-runtime-headless-conformance.md) | ADR 0076: Headless Runtime ABI Conformance Boundary | still constrains | conformance: `crates/orna-cli-v1/Cargo.toml` |
| [0077](../../0077-client-server-resource-language.md) | ADR 0077: CLIENT-to-SERVER Resource Language Surface | still constrains | resource: `crates/orna-application-v1/src/lib.rs` |
| [0078](../../0078-client-server-resource-transport.md) | ADR 0078: CLIENT-to-SERVER Resource Transport and Scheduling | still constrains | transport: `crates/orna-cli-v1/README.md` |
| [0079](../../0079-client-action-values.md) | ADR 0079: CLIENT Action Values and `std.action.call` | still constrains | std.action: `crates/orna-artifact/src/client_plan/tests.rs` |
| [0080](../../0080-client-inspector.md) | ADR 0080: Headless Ordinary CLIENT Inspector v1 | still constrains | Inspector: `crates/orna-artifact/src/client_plan.rs` |
| [0081](../../0081-standard-inspector-render-contract.md) | ADR 0081: Generic Standard Inspector Render Contract | still constrains | std.inspect: `crates/orna-core/src/inspect.rs` |
| [0082](../../0082-production-qt-runtime.md) | ADR 0082: The First Production Non-TTY Runtime Is Qt | still constrains | Qt: `crates/orna-client/src/lib.rs` |
| [0083](../../0083-standard-ui-window.md) | Work ADR 0083: Register the v1 `std.ui.window` Client Function | still constrains | std.ui.window: `crates/orna-artifact/src/client_plan/tests.rs` |
| [0084](../../0084-client-control-flow.md) | Work ADR 0084: Programmable CLIENT Plans and Shared Runtime Hosts | still constrains | shared runtime: `crates/orna-client/src/studio_source_editor.rs` |
| [0085](../../0085-installed-qt-runtime-package.md) | Work ADR 0085: Install and Select the Qt Runtime from a Fixed Package Path | still constrains | Qt: `crates/orna-client/src/lib.rs` |
| [0086](../../0086-populated-inspector-projections.md) | ADR 0086: Populate the Existing Inspector Projection Rows (Deferred) | still constrains | Inspector projection: `crates/orna-artifact/src/client_plan.rs` |
| [0087](../../0087-std-data-rows.md) | Work ADR 0087: Bounded `std.data.Rows` and the Retained Table Presenter | still constrains | std.data: `crates/orna-artifact/src/client_plan/tests.rs` |
| [0088](../../0088-structural-ui-constructors.md) | Work ADR 0088: Structural UI Constructors for Source-Authored CLIENT Work | still constrains | std.ui: `crates/orna-artifact/src/client_plan/tests.rs` |
| [0089](../../0089-resource-lineage-authority.md) | Work ADR 0089: Resource Lineage Authority | still constrains | lineage: `crates/orna-cli-v1/src/fuse_mount.rs` |
| [0090](../../0090-local-principal-session-authority.md) | Work ADR 0090: Local Principal and Session Authority | still constrains | principal: `crates/orna-artifact/src/client_plan.rs` |
| [0091](../../0091-client-vm-trust-and-sandbox.md) | Work ADR 0091: CLIENT VM Trust and Sandbox (Deferred) | still constrains | VM: `crates/orna-client/src/execution.rs` |
| [0092](../../0092-function-backed-cli-sessions.md) | ADR 0092: Function-Backed CLI Sessions | still constrains | session: `crates/orna-application-v1/src/lib.rs` |
| [0093](../../0093-reflective-gateway-contracts.md) | ADR 0093: Gateway and Protocol Contract Boundaries | still constrains | gateway: `crates/orna-core/src/invocation.rs` |
| [0094](../../0094-std-launch-contract.md) | ADR 0094: Reference-Defined Run Entry Contract | still constrains | std.launch: spec-only (Orna-1.1.0) |
| [0095](../../0095-principal-credential-delegation-scope.md) | Work ADR 0095: OrnaDB 1.0 Principal, Credential, and Delegation Scope | still constrains | principal: `crates/orna-artifact/src/client_plan.rs` |
| [0096](../../0096-trust-policy-function-security.md) | Work ADR 0096: OrnaDB 1.0 Trust Policy and Function Security Boundary | still constrains | trust: `crates/orna-application-v1/src/lib.rs` |
| [0097](../../0097-normative-repl-boundary.md) | Work ADR 0097: Normative REPL Boundary | still constrains | REPL: `crates/orna-application-v1/src/lib.rs` |
| [0099](../../0099-client-capability-grant-configuration.md) | Work ADR 0099: OrnaDB 1.0 CLIENT Capability-Grant Configuration Boundary | still constrains | grant: `crates/orna-application-v1/src/lib.rs` |
| [0100](../../0100-studio-source-tooling-boundary.md) | Work ADR 0100: Studio Source Tooling Boundary | still constrains | Studio: `crates/orna-client/src/studio_source_editor.rs` |
| [0101](../../0101-client-state-dogfood-reference-deferral.md) | ADR 0101: Defer CLIENT STATE Dogfood as Orna 1.0.0 Conformance | still constrains | STATE: `crates/orna-artifact/src/client_plan.rs` |
| [0102](../../0102-qualified-type-editor-highlighting-deferral.md) | ADR 0102: Qualified Type Editor Highlighting Deferral | still constrains | highlight: `crates/orna-lsp/src/formatting.rs` |
| [0103](../../0103-studio-runtime-inspector-reference-boundary.md) | Work ADR 0103: Studio Runtime and Inspector Reference Boundary | still constrains | Studio: `crates/orna-client/src/studio_source_editor.rs` |
| [0104](../../0104-studio-catalogue-tree-search-boundary.md) | Work ADR 0104: Studio Catalogue Tree and Function Search Boundary | still constrains | Studio: `crates/orna-client/src/studio_source_editor.rs` |
| [0105](../../0105-studio-security-dba-page-boundary.md) | Work ADR 0105: Studio Security and DBA Page Boundary | still constrains | Studio: `crates/orna-client/src/studio_source_editor.rs` |
| [0106](../../0106-studio-source-reload-and-hot-revision-deferral.md) | Work ADR 0106: Studio Source Reload and Hot-Revision Deferral | still constrains | Studio: `crates/orna-client/src/studio_source_editor.rs` |
| [0107](../../0107-studio-sql-source-editor-boundary.md) | Work ADR 0107: Studio SQL and Source Editor Boundary | still constrains | Studio: `crates/orna-client/src/studio_source_editor.rs` |
| [0108](../../0108-persistent-value-backend-parity-deferral.md) | Work ADR 0108: Defer Persistent Scalar and Record Backend Parity | still constrains | Scalar: `crates/orna-artifact/src/client_plan.rs` |
| [0109](../../0109-publication-artifact-completeness-boundary.md) | Work ADR 0109: Defer Publication Object Encoding in `orna-artifact` | still constrains | publication: `crates/orna-application-v1/src/lib.rs` |
| [0110](../../0110-source-introspection-reference-gate.md) | Work ADR 0110: Source Introspection Reference Gate | still constrains | introspection: `crates/orna-core/src/source_metadata.rs` |
| [0111](../../0111-sys-baked-module-abi.md) | ADR 0111: `sys` Is a Baked Module ABI with an Extensible Provider Protocol | still constrains | sys: `crates/orna-application-v1/src/lib.rs` |
| [0112](../../0112-sys-provider-protocol-phase-1.md) | Decision 0112: Phase 1 typed `sys` provider protocol | still constrains | provider: `crates/orna-application-v1/src/lib.rs` |
| [0113](../../0113-reference-sys-bindings-addendum.md) | External reference record: sys binding architecture addendum | still constrains | addendum: `crates/orna-sys-v1/build_support.rs` |
| [0114](../../0114-reference-sys-bindings-placement-check.md) | External reference placement verification: sys binding addendum | still constrains | placement: `crates/orna-application-v1/src/lib.rs` |
| [0115](../../0115-single-source-editor-syntax-artifacts.md) | ADR 0115: Single-source editor syntax artifacts | still constrains | syntax: `crates/orna-application-v1/Cargo.toml` |
| [0116](../../0116-playground-in-db-application.md) | Work ADR 0116: Playground as a Database-Hosted Application | still constrains | playground: spec-only (Orna-1.1.0) |
| [0117](../../0117-retire-browser-playground.md) | Work ADR 0117: Retire the Browser Playground | still constrains | playground: spec-only (Orna-1.1.0) |
| [0118](../../0118-schema-expression-digests.md) | Work ADR 0118: Format-3 Schema Expression Digests | still constrains | digest: `crates/orna-application-v1/src/lib.rs` |
| [0119](../../0119-offline-lsp-editor-tooling.md) | 0119 — Offline LSP and Editor Tooling for `.orna` Source | still constrains | ?: `crates/orna-application-v1/src/lib.rs` |
| [0120](../../0120-client-vm-trust-boundary.md) | Work ADR 0120: OrnaDB 1.0 CLIENT VM Trust Boundary | still constrains | ?: `crates/orna-application-v1/src/lib.rs` |
| [0001](0001-private-postgresql-kernel.md) | ADR 0001: PostgreSQL Is a Private Kernel | archived (superseded) | PostgreSQL private kernel: 1.1.0 uses SQLite/Turso state.db; no PostgreSQL runtime in crates |
| [0002](0002-public-language-contract.md) | ADR 0002: Public Language Contract | archived (superseded) | Public language contract: no spec clause or code path remains |
| [0004](0004-private-postgresql-layout.md) | ADR 0004: Private PostgreSQL Layout Uses Stable Orna Identities | archived (superseded) | PostgreSQL layout: superseded by 1.1.0 SQLite/Turso state store; no code path |
| [0005](0005-single-row-server-insert.md) | ADR 0005: Single-Row SERVER INSERT | archived (superseded) | SERVER INSERT (PostgreSQL SERVER DML): absent from Orna-1.1.0 and crates |
| [0007](0007-single-object-server-update.md) | ADR 0007: Single-Object SERVER UPDATE | archived (superseded) | SERVER UPDATE: absent from Orna-1.1.0 and crates |
| [0008](0008-single-object-server-delete.md) | ADR 0008: Single-Object SERVER DELETE | archived (superseded) | SERVER DELETE: absent from Orna-1.1.0 and crates |
| [0009](0009-identity-selected-server-select.md) | ADR 0009: Identity-Selected SERVER SELECT | archived (superseded) | Implemented only in island crate orna-artifact (server_plan.rs, codec.rs); no product consumer; to be deleted with island step 7 |
| [0011](0011-direct-boolean-server-select-predicates.md) | ADR 0011: Direct Boolean SERVER SELECT Predicates | archived (superseded) | Implemented only in island crate orna-artifact (server_plan.rs, codec.rs); no product consumer; to be deleted with island step 7 |
| [0014](0014-host-only-backend-shell.md) | ADR 0014: Host-Only Backend Shell | archived (superseded) | Host-only backend shell: absent from Orna-1.1.0 and crates |
| [0017](0017-bundled-postgresql-runtime.md) | ADR 0017: Orna Ships and Owns Its PostgreSQL Runtime | archived (superseded) | Bundled PostgreSQL runtime: superseded by 1.1.0 SQLite/Turso store; no code path |
| [0019](0019-embedded-postgresql-engine.md) | ADR 0019: PostgreSQL Is Part of the Orna Executable | archived (superseded) | Embedded PostgreSQL engine: superseded by 1.1.0 SQLite/Turso store; no code path |
| [0021](0021-durable-security-snapshot.md) | ADR 0021: PostgreSQL Persists the Security Decision Snapshot | archived (superseded) | PostgreSQL security snapshot: superseded by 1.1.0 store; ADR 0024 audit and security.rs snapshot remain as separate decisions |
| [0030](0030-authenticated-server-select.md) | ADR 0030: One Authenticated, Authorised, Audited SERVER SELECT | archived (superseded) | Authenticated SERVER SELECT: absent from Orna-1.1.0 and crates |
| [0032](0032-raw-server-select-dispatch.md) | ADR 0032: Raw Calls Dispatch One-Column SERVER SELECTs | archived (superseded) | Raw SERVER SELECT dispatch: absent from Orna-1.1.0 and crates |
| [0037](0037-single-private-postgresql-crate.md) | ADR 0037: PostgreSQL Uses One Private Rust Crate | archived (superseded) | Single private PostgreSQL crate: superseded by 1.1.0 store; no code path |
| [0047](0047-first-one-zero-release.md) | ADR 0047: The First 1.0 Release Uses One Authenticated Debian Authority | archived (superseded) | 1.0.0 release-authority decision (Debian): 1.1.0 supersedes the release; no spec or code path |
| [0048](0048-raw-identity-selected-server-select.md) | ADR 0048: Raw Reference Calls Select One Projected Object Row | archived (superseded) | Raw identity-selected SERVER SELECT: absent from Orna-1.1.0 and crates |
| [0063](0063-automatic-runtime-selection.md) | ADR 0063: The Client Offers the Installed Runtime and Accepts a Runtime Override | archived (superseded) | Not implemented: wire types only in island crate orna-core (invocation.rs InvocationRuntimeOffer); no --runtime flag, no client population; to be deleted with island step 7 |
| [0098](0098-ring1-catalogue-revision-deferral.md) | ADR 0098: Ring-1 Catalogue and Revision Gateway Deferral | archived (superseded) | Ring-1 catalogue deferral: absent from Orna-1.1.0 and crates |
