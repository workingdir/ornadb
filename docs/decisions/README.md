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
* **work ADR 0060:**
  [CLIENT Capability Requirements and the Local Sandbox](0060-client-capability-requirements.md)
* **work ADR 0061:**
  [Durable USER State Service](0061-durable-user-state-service.md)
* **work ADR 0058:**
  [`orna.std/3` Standard Output Value Types](0058-orna-std-3-output-value-types.md)
* **work ADR 0059:**
  [Offline LSP and Editor Tooling for `.orna` Source](0059-offline-lsp-editor-tooling.md)
* **work ADR 0066:**
  [`orna source diff` — Semantic Source Changes Without Apply](0066-semantic-source-diff.md)
* **work ADR 0067:**
  [`std.csv.encode` — the Sealed CSV Output Presenter](0067-csv-output-presenter.md)
* **work ADR 0068:**
  [CLIENT Expression Bodies and RUNTIME CONTRACT Clauses](0068-client-expression-bodies.md)
