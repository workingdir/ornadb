# System API surface conformance audit

Issue: `ornadb-khj7` (GitHub #2098). Audit base after rebase: `origin/main` at `ea5d9d4a`; the protected `/home/pbox/dev/ornadb/work` was not used. Authority: frozen `/home/pbox/dev/ornadb/reference/Orna-1.0.0/api/sys.json` and `source/34-system-reference.md`.

## Normative anchors

- **ORNA-SYS-002** (`Orna-1.0.0.md:2835`): provide the complete portable `sys` surface applicable to a claimed conformance class.
- **ORNA-SYS-001** (`Orna-1.0.0.md:2833`): use typed structural references where possible.
- **ORNA-SYS-010** (`Orna-1.0.0.md:2843`): portable names are case-sensitive.
- **ORNA-SYS-016** (`Orna-1.0.0.md:2903`): opaque identifiers preserve identity, have deterministic canonical encoding, and are not implicitly interchangeable.
- **ORNA-SYS-111–114** (`Orna-1.0.0.md:2919–2925`): relation rows expose typed read-only references; references preserve natural keys and context, do not coerce to/from rows, and do not bypass visibility/redaction/trust/preconditions.
- **ORNA-SYS-099–101** (`Orna-1.0.0.md:3005–3009`): redact at the system-value boundary, preserve safe type/identity and an explicit marker, and refuse generic reveal-mode encoding.
- `source/34-system-reference.md:521–531` defines `sys.ValueMetadata<T>` and its five fields. `:2462–2470` defines `sys.meta<T>(value)` and its read effect.
- **ORNA-GENERIC-001** (`source/06-expressions.md:161`) governs explicit public generic parameters.
- **ORNA-PUB-004** (`source/22-publication.md:17`) requires effective policy and pending/published state through `sys.Storage` and runtime maintenance metadata.

## Audit result

The frozen portable API declares 4 singletons, 21 opaque identifiers, 78 reference aliases, 34 value types, 44 enums, 78 relations, 66 functions and 46 failure codes. The full symbol inventory is listed below. Existing semantic inventory tests validate declaration loading and consistency; that evidence is not reported as execution of each system binding.

The `sys-v1` `system_function_descriptor` registry has 33 entries: `sys.explain(Diagnostic)` and 32 administrative operations. It is a descriptor table, not a global executable binding registry. Existing source fixtures prove semantic admission for several system calls, and `orna-sys-v1/tests/explain_diagnostic.rs` exercises the `sys.explain(Diagnostic)` host behavior. No per-entry end-to-end Orna runtime execution proof was found for the full 66-function surface. The per-entry function table records descriptor presence and the remaining binding-proof obligation without claiming an absent descriptor proves a missing implementation in another owner.

### `sys.meta` gap (confirmed)

The frozen API and repository API both declare `fn sys.meta<T>(value: T): sys.ValueMetadata<T>` with `effect: read`. The fixture `crates/orna-conformance-v1/tests/fixtures/sys-api-audit-meta.orna` is accepted by semantic analysis and yields a green test, but `orna_sys_v1::system_function_descriptor("sys.meta")` returns `None`; a repository search found semantic inference/typechecking only, with no executable `sys.meta` binding or result-producing implementation. Existing Beads issues #1986 and #1987 have the same open title/description, “Implement sys.meta value metadata intrinsic”; neither has an open PR. A clean, stale worktree `herdr-domains/gov5-sys-meta-1378-20260928` remains registered, 114 commits behind current `origin/main` after rebase. This audit did not edit its files.

The normative shape is defined, and **ORNA-SYS-099–101** bound redaction: it must happen at the system-value boundary, preserve safe type/identity metadata plus an explicit marker, and generic codecs cannot reveal protected values. The inspected contract does not specify the complete metadata production algorithm for `static_type`, `nominal_type`, `protocols` and `codecs`, including their exact source and ordering. The marker requirement does not by itself supply those mappings. Record this as a contract/implementation gap; this slice invents no metadata values or fallback semantics.

### Frozen API versus repository API

The frozen API hash is `318b6d54f51d44e8117ffc91520dfd2dc2722cc023fdad51bd4dba601dd9abcb`; repository `api/sys.json` at this base hashes to `378f11e726a882e1596c0f1aade346f6cf082aa741bfc6f9ecd8a0d47a1f59c5`.

The repository schema has 35 value types versus 34 in the frozen API. Its extra `sys.PublicationPolicy` and publication additions to `sys.Storage`/`sys.MaintenanceJob` differ from frozen `source/34-system-reference.md:2203–2226` and `:2352–2368`: the repository API adds `published_rows`, `published_bytes`, and maintenance publication fields, and makes `publication_policy` typed. The frozen `sys.Storage.publication_policy` is `sys.Value`, and its maintenance relation has no publication counters. These newer repository fields appear related to **ORNA-PUB-004**, but the frozen sys API/reference does not define their complete typed shape. This is recorded as cross-artifact contract drift; this slice does not edit the frozen reference, root API file, or existing tests.

## Evidence and bounded proof

- `crates/orna-semantic-v1/src/system_api.rs` loads the checked-in API as its semantic descriptor source; `system_api::tests::complete_checked_in_inventory_is_loaded_exactly` verifies the pinned semantic inventory.
- `crates/orna-conformance-v1/tests/sys_api_contract.rs` checks API counts, selected schema shapes, admin effect labels and selected invocation admission. Most checks consume JSON metadata rather than executing Orna source.
- `crates/orna-conformance-v1/tests/semantic_runtime_adapter.rs` has real `.orna` fixtures for `sys.meta`, `sys.resolve`, snapshots, view members and invoke/start/await/cancel admission. These establish semantic-stage behavior only.
- `crates/orna-sys-v1/tests/explain_diagnostic.rs` exercises the diagnostic explanation operation; no broad fixture-backed source dispatch matrix was found.

The new `sys_api_surface_audit_khj7` test reads both API artifacts, analyzes the checked-in `sys.meta` fixture, and pins the `sys.meta` descriptor gap plus the frozen/repository value-type count difference. It makes no production claim.

Captured focused CLI output:

```text
$ ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-conformance-v1 --test sys_api_surface_audit_khj7
running 1 test
test sys_meta_contract_is_accepted_semantically_but_lacks_sys_v1_descriptor ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
CARGO_TEST_EXIT=0
```

Existing contract test capture:

```text
$ ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-conformance-v1 --test sys_api_contract
running 9 tests
8 passed; 1 failed: portable_sys_api_has_exact_declared_counts_and_surface
assertion `left == right` failed: value_types count
left: 35
right: 34
CARGO_TEST_EXIT=101
```

The failure is exactly the frozen/repository count divergence above. It is preserved as evidence; no existing test was edited.

## Per-entry inventory

Status key: `schema/inventory only` means the name is present in the frozen descriptor and the semantic inventory, but this audit did not locate per-entry runtime source-fixture execution. For functions, registry presence is separately recorded; it is descriptor evidence only. These rows remain coverage gaps under the accepted per-entry rule, not assertions that every untested entry is unimplemented.

### Singleton views (4)

All four have schema/inventory declarations. Selected schema checks exist in `sys_api_contract.rs`; per-view source runtime reads remain unproven.
- `sys.database` — schema/inventory only; no per-view source runtime proof located.
- `sys.current` — schema/inventory only; no per-view source runtime proof located.
- `sys.rt` — schema/inventory only; no per-view source runtime proof located.
- `sys.repl` — schema/inventory only; no per-view source runtime proof located.

### Opaque identifiers (21)

All 21 identifiers are declared; deterministic encoding and per-domain non-coercion evidence was not mapped entry-by-entry in the available source fixture suites. ORNA-SYS-016 applies to this family.
- `sys.DatabaseId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.FileId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.DefinitionId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.ObjectId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.RevisionId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.SnapshotId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.RuntimeId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.TransactionId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.InvocationId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.RunId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.QueryId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.SessionId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.ClientId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.TraceId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.SpanId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.CheckpointVersion` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.SegmentId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.BuildId` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.GitOid` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.FailureVersion` — schema/inventory only; per-identifier runtime codec proof not located.
- `sys.ConsumerIdentity` — schema/inventory only; per-identifier runtime codec proof not located.

### Typed row-reference aliases (78)

All aliases are present in the semantic inventory. ORNA-SYS-001 and ORNA-SYS-111–114 constrain reference typing, natural-key/context preservation and redaction/visibility checks; per-alias source dereference fixtures were not found.
- `sys.DatabaseRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.NamespaceRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ModuleRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ImportRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.FileRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ObjectRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.RevisionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.DefinitionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TypeRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TypeParameterRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.FieldRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.VariantRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ProtocolRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ProtocolMemberRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ImplementationRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.FunctionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ParameterRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.EffectRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TableRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ColumnRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.KeyRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.AssertionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ReferenceRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.PageRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.DimensionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.UnitRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.CurrencyRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SecretRequirementRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ExtensionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.DependencyRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.DiagnosticRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SnapshotRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SnapshotObjectRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ChangeRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.DiffEntryRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.FileVersionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.GitRepositoryRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.CommitRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TreeEntryRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.RefRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.BranchRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TagRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.RemoteRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.StashRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.GitObjectRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.WorktreeEntryRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.QueryRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.PlanRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.PlanNodeRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.InvocationRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.InvocationArgumentRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.RunRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TransactionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TraceRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SpanRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TraceEventRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.StreamRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.CheckpointRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.CheckpointUpdateRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.FailureRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SessionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ClientRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.LeaseRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.ListenerRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.StorageRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SegmentRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.StatisticRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.IndexRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.MaterializationRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.AllocatorRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.HydrationRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.CompactionRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.MaintenanceJobRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.StorageFileRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SecretRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.SettingRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.BuildRef` — schema/inventory only; per-alias runtime resolution proof not located.
- `sys.TestRef` — schema/inventory only; per-alias runtime resolution proof not located.

### Closed enumerations (44)

All 44 names/values are loaded by the semantic inventory. Per-enumeration decoding and runtime output fixtures were not mapped in this audit.
- `sys.AssertionOwnerKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.AssertionScope` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.BuildStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ChangeArea` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ChangeKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ChangeTargetKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ClientKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.DependencyConfidence` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.DependencyKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.DiffScope` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.EffectKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ExtensionTrust` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.FailureStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.FileKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.GitObjectKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.HostAccess` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.IndexKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.InvocationStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.InvokeTransaction` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.LeaseStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ListenerKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.MaintenanceKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ObjectKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.PlanNodeKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ProtocolMemberKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.ReferenceAction` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.RunStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.RuntimeMode` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.SettingScope` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.SettingSource` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.Severity` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.SnapshotKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.StatisticKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.StorageFileKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.StoragePreference` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.StorageProfile` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.StorageRewriteTarget` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.StreamStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.TestStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.TransactionStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.TypeKind` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.VerificationStatus` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.VerifyScope` — schema/inventory only; per-enum runtime fixture proof not located.
- `sys.Visibility` — schema/inventory only; per-enum runtime fixture proof not located.

### Value types (34)

All 34 frozen names/field schemas are loaded by the semantic inventory. Selected families (runtime context, client/lease/listener, source spans, compatibility) have schema checks in `sys_api_contract.rs` / `sys_api_compatibility.rs`; type-specific runtime construction and redaction behavior remains unproven for the listed entries.
- `sys.RowRef<T>` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.Value` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.Argument` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.ArgumentMap` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.ValueMetadata<T>` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.InvocationHandle<T>` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.InvocationResult<T>` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.ExpressionRef` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.SourceSpan` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.SourceMapEntry` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.DiagnosticLabel` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.SourceDocument` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.Attribution` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.PersonIdentity` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.ObjectDescription` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.Explanation` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.CheckpointPosition` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.CompatibilityInfo` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.RuntimeInfo` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.FlushResult` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.CompactionResult` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.StorageRewriteResult` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.VerificationReport` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.CancellationView` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.PresentationOverride` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.Watch` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.DatabaseView` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.CurrentContext` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.RuntimeView` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.ReplView` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.ChangeTarget` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.CheckpointConflict` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.RowConflict` — schema/inventory only; per-type runtime construction fixture not located.
- `sys.CheckoutPlan` — schema/inventory only; per-type runtime construction fixture not located.

### Canonical relations (78)

All 78 are present in the semantic inventory; selected `sys.Session`, context views and runtime information receive schema checks. Per-relation source query/result fixtures, historical pinning and availability/redaction evidence remain unproven entry-by-entry.
- `sys.Database` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Namespace` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Module` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Import` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.File` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Object` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Revision` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Definition` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Type` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.TypeParameter` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Field` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Variant` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Protocol` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.ProtocolMember` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Implementation` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Function` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Parameter` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Effect` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Table` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Column` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Key` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Assertion` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Reference` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Page` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Dimension` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Unit` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Currency` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.SecretRequirement` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Extension` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Dependency` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Diagnostic` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Snapshot` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.SnapshotObject` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Change` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.DiffEntry` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.FileVersion` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.GitRepository` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Commit` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.TreeEntry` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Ref` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Branch` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Tag` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Remote` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Stash` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.GitObject` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.WorktreeEntry` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Query` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Plan` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.PlanNode` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Invocation` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.InvocationArgument` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Run` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Transaction` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Trace` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Span` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.TraceEvent` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Stream` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Checkpoint` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.CheckpointUpdate` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Failure` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Session` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Client` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Lease` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Listener` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Storage` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Segment` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Statistic` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Index` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Materialization` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Allocator` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Hydration` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Compaction` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.MaintenanceJob` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.StorageFile` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Secret` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Setting` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Build` — schema/inventory only; per-relation runtime query fixture not located.
- `sys.Test` — schema/inventory only; per-relation runtime query fixture not located.

### Callable functions (66)

The registry classification below is `orna-sys-v1::system_function_descriptor`, not a claim that other runtime owners cannot implement an entry. Non-registry source-call execution must be attributed to its owning host before the function can be marked conformant. `sys.invoke`, `sys.start`, `sys.await` and `sys.cancel` have supervisor/admission tests and semantic source fixtures, but not a complete source-runtime binding proof in this audit.

| API function entry | Registry / evidence status |
|---|---|
| `sys.meta` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.object` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.resolve` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.resolve_function` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.describe` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.source(ObjectRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.source(FileRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.history(ObjectRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.history(FileRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.blame(FileRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.blame(RowRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.dependencies` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.dependents` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.snapshot(SnapshotRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.snapshot(CommitRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.snapshot(BranchRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.snapshot(TagRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.snapshot(GitOid)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.snapshot(Str)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.diff` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.changes` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.explain(Query)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.explain(FunctionRef)` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.explain(Diagnostic)` | descriptor present; direct host behavior tests exist, but no Orna source runtime fixture found |
| `sys.checkpoint` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.render_diagnostic` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.rt.info` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.invoke(Value)` | not in this registry; supervisor/admission tests exist, but source-runtime binding proof is incomplete |
| `sys.invoke<T>` | not in this registry; supervisor/admission tests exist, but source-runtime binding proof is incomplete |
| `sys.start(Value)` | not in this registry; supervisor/admission tests exist, but source-runtime binding proof is incomplete |
| `sys.start<T>` | not in this registry; supervisor/admission tests exist, but source-runtime binding proof is incomplete |
| `sys.await` | not in this registry; supervisor/admission tests exist, but source-runtime binding proof is incomplete |
| `sys.cancel` | not in this registry; supervisor/admission tests exist, but source-runtime binding proof is incomplete |
| `sys.admin.commit` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.checkout(SnapshotRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.checkout(CommitRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.checkout(BranchRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.checkout(TagRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.checkout(GitOid)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.checkout(Str)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.create_branch(SnapshotRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.create_branch(CommitRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.create_branch(BranchRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.create_branch(TagRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.create_branch(GitOid)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.create_branch(Str)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.flush` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.compact` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.set_storage_preference` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.rewrite_storage` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.verify` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.cancel_run` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.pause_stream` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.resume_stream` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.reset_checkpoint` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.retry_failure` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.skip_failure` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.replay_failure` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.resolve_failure` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.consumer_identity` | not in this registry; executable owner/binding and source fixture proof not established |
| `sys.admin.plan_checkout(SnapshotRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.plan_checkout(CommitRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.plan_checkout(BranchRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.plan_checkout(TagRef)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.plan_checkout(GitOid)` | descriptor present; operation-specific Orna source execution proof not located |
| `sys.admin.plan_checkout(Str)` | descriptor present; operation-specific Orna source execution proof not located |

### Failure-code vocabulary (46)

All 46 code strings are declared in the frozen API and semantic inventory. Per-code runtime production/mapping/serialization fixture proof was not mapped for the full vocabulary; ORNA-SYS-002 requires any claimed applicable surface to be complete.
- `sys.version.incompatible` — schema/inventory only; per-code source runtime proof not located.
- `sys.context.repl_unavailable` — schema/inventory only; per-code source runtime proof not located.
- `sys.object.not_found` — schema/inventory only; per-code source runtime proof not located.
- `sys.object.ambiguous` — schema/inventory only; per-code source runtime proof not located.
- `sys.object.wrong_kind` — schema/inventory only; per-code source runtime proof not located.
- `sys.snapshot.not_found` — schema/inventory only; per-code source runtime proof not located.
- `sys.snapshot.incomplete` — schema/inventory only; per-code source runtime proof not located.
- `sys.source.unavailable` — schema/inventory only; per-code source runtime proof not located.
- `sys.source.redacted` — schema/inventory only; per-code source runtime proof not located.
- `sys.handle.foreign_runtime` — schema/inventory only; per-code source runtime proof not located.
- `sys.handle.expired` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.argument_missing` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.argument_unknown` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.argument_type` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.return_type` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.effect_unavailable` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.idempotency_mismatch` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.await_timeout` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.not_callable` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.revision_unavailable` — schema/inventory only; per-code source runtime proof not located.
- `sys.checkpoint.not_found` — schema/inventory only; per-code source runtime proof not located.
- `sys.checkpoint.conflict` — schema/inventory only; per-code source runtime proof not located.
- `sys.checkpoint.not_replayable` — schema/inventory only; per-code source runtime proof not located.
- `sys.failure.state_conflict` — schema/inventory only; per-code source runtime proof not located.
- `sys.failure.skip_unsupported` — schema/inventory only; per-code source runtime proof not located.
- `sys.admin.read_only` — schema/inventory only; per-code source runtime proof not located.
- `sys.admin.busy` — schema/inventory only; per-code source runtime proof not located.
- `sys.admin.assertion_failed` — schema/inventory only; per-code source runtime proof not located.
- `sys.admin.missing_object` — schema/inventory only; per-code source runtime proof not located.
- `sys.admin.verification_failed` — schema/inventory only; per-code source runtime proof not located.
- `sys.storage.corrupt` — schema/inventory only; per-code source runtime proof not located.
- `sys.storage.unavailable` — schema/inventory only; per-code source runtime proof not located.
- `sys.storage.unrepresentable_key` — schema/inventory only; per-code source runtime proof not located.
- `sys.storage.rewrite_conflict` — schema/inventory only; per-code source runtime proof not located.
- `sys.redaction.denied` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.snapshot_mismatch` — schema/inventory only; per-code source runtime proof not located.
- `sys.invoke.transaction_mode` — schema/inventory only; per-code source runtime proof not located.
- `sys.git.unborn_head` — schema/inventory only; per-code source runtime proof not located.
- `sys.git.uncommitted_snapshot` — schema/inventory only; per-code source runtime proof not located.
- `sys.git.dirty_conflict` — schema/inventory only; per-code source runtime proof not located.
- `sys.git.stale_plan` — schema/inventory only; per-code source runtime proof not located.
- `sys.failure.stale_version` — schema/inventory only; per-code source runtime proof not located.
- `sys.snapshot.expired` — schema/inventory only; per-code source runtime proof not located.
- `sys.git.invalid_ref` — schema/inventory only; per-code source runtime proof not located.
- `sys.git.branch_exists` — schema/inventory only; per-code source runtime proof not located.
- `sys.git.nothing_to_commit` — schema/inventory only; per-code source runtime proof not located.
