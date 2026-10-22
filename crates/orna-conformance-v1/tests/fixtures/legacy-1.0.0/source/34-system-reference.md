# 34. System API reference {#system-reference}

This reference defines system names, field types and callable interfaces. The [system model](#system) explains identity, snapshots, reflection and redaction; [administration](#administration) specifies state-changing preconditions. A machine-readable schema is available in [`api/sys.json`](api/sys.json).

Signatures use qualified callable names and type parameters such as `T`. Source declaration syntax is defined by the [grammar](#grammar). Optional types use `?`; their absence value is `null`. All relation rows are read-only.

Field types and declared invariants apply together. Source, host-path and payload fields also follow availability and redaction rules. `null` represents defined absence; unavailable mandatory data produces the specified failure.

## Namespace index

| Group | Use |
|---|---|
| `sys.database`, `sys.current`, `sys.rt`, `sys.repl` | Current attachment, activation, runtime and optional REPL observations. |
| `sys.catalog` | Declarations, types, source and dependencies. |
| `sys.history`, `sys.git` | Snapshots, semantic changes and Git objects. |
| `sys.storage`, `sys.build` | Physical placement and build/test metadata. |
| Root functions | Typed resolution, inspection, invocation and execution control. |
| `sys.admin` | Explicit state transitions; never row setters. |

## Singleton views

| Name | Type | Availability |
|---|---|---|
| `sys.database` | `sys.DatabaseView` | always while a database is attached |
| `sys.current` | `sys.CurrentContext` | every activation |
| `sys.rt` | `sys.RuntimeView` | every command/runtime owner |
| `sys.repl` | `sys.ReplView` | REPL only; otherwise sys.context.repl_unavailable |

## Opaque identifiers

An opaque identifier cannot be implicitly substituted for a string, UUID or another identifier. Its domain and canonical encoding remain part of the named type.

### `sys.DatabaseId` {#api-type-sys-databaseid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.FileId` {#api-type-sys-fileid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.DefinitionId` {#api-type-sys-definitionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.ObjectId` {#api-type-sys-objectid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.RevisionId` {#api-type-sys-revisionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SnapshotId` {#api-type-sys-snapshotid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.RuntimeId` {#api-type-sys-runtimeid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.TransactionId` {#api-type-sys-transactionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.InvocationId` {#api-type-sys-invocationid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.RunId` {#api-type-sys-runid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.QueryId` {#api-type-sys-queryid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SessionId` {#api-type-sys-sessionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.ClientId` {#api-type-sys-clientid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.TraceId` {#api-type-sys-traceid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SpanId` {#api-type-sys-spanid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.CheckpointVersion` {#api-type-sys-checkpointversion}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SegmentId` {#api-type-sys-segmentid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.BuildId` {#api-type-sys-buildid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.GitOid` {#api-type-sys-gitoid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.FailureVersion` {#api-type-sys-failureversion}

Nominal identity value; see [identity and encoding](#formats).

### `sys.ConsumerIdentity` {#api-type-sys-consumeridentity}

Nominal identity value; see [identity and encoding](#formats).

## Typed row-reference aliases

Each alias is a pinned `sys.RowRef<T>` for the named canonical relation. It is not an implicit conversion from the row value. Obtain it with `.reference`.

| Alias | Target |
|---|---|
| `sys.DatabaseRef` | `sys.Database` |
| `sys.NamespaceRef` | `sys.Namespace` |
| `sys.ModuleRef` | `sys.Module` |
| `sys.ImportRef` | `sys.Import` |
| `sys.FileRef` | `sys.File` |
| `sys.ObjectRef` | `sys.Object` |
| `sys.RevisionRef` | `sys.Revision` |
| `sys.DefinitionRef` | `sys.Definition` |
| `sys.TypeRef` | `sys.Type` |
| `sys.TypeParameterRef` | `sys.TypeParameter` |
| `sys.FieldRef` | `sys.Field` |
| `sys.VariantRef` | `sys.Variant` |
| `sys.ProtocolRef` | `sys.Protocol` |
| `sys.ProtocolMemberRef` | `sys.ProtocolMember` |
| `sys.ImplementationRef` | `sys.Implementation` |
| `sys.FunctionRef` | `sys.Function` |
| `sys.ParameterRef` | `sys.Parameter` |
| `sys.EffectRef` | `sys.Effect` |
| `sys.TableRef` | `sys.Table` |
| `sys.ColumnRef` | `sys.Column` |
| `sys.KeyRef` | `sys.Key` |
| `sys.AssertionRef` | `sys.Assertion` |
| `sys.ReferenceRef` | `sys.Reference` |
| `sys.PageRef` | `sys.Page` |
| `sys.DimensionRef` | `sys.Dimension` |
| `sys.UnitRef` | `sys.Unit` |
| `sys.CurrencyRef` | `sys.Currency` |
| `sys.SecretRequirementRef` | `sys.SecretRequirement` |
| `sys.ExtensionRef` | `sys.Extension` |
| `sys.DependencyRef` | `sys.Dependency` |
| `sys.DiagnosticRef` | `sys.Diagnostic` |
| `sys.SnapshotRef` | `sys.Snapshot` |
| `sys.SnapshotObjectRef` | `sys.SnapshotObject` |
| `sys.ChangeRef` | `sys.Change` |
| `sys.DiffEntryRef` | `sys.DiffEntry` |
| `sys.FileVersionRef` | `sys.FileVersion` |
| `sys.GitRepositoryRef` | `sys.GitRepository` |
| `sys.CommitRef` | `sys.Commit` |
| `sys.TreeEntryRef` | `sys.TreeEntry` |
| `sys.RefRef` | `sys.Ref` |
| `sys.BranchRef` | `sys.Branch` |
| `sys.TagRef` | `sys.Tag` |
| `sys.RemoteRef` | `sys.Remote` |
| `sys.StashRef` | `sys.Stash` |
| `sys.GitObjectRef` | `sys.GitObject` |
| `sys.WorktreeEntryRef` | `sys.WorktreeEntry` |
| `sys.QueryRef` | `sys.Query` |
| `sys.PlanRef` | `sys.Plan` |
| `sys.PlanNodeRef` | `sys.PlanNode` |
| `sys.InvocationRef` | `sys.Invocation` |
| `sys.InvocationArgumentRef` | `sys.InvocationArgument` |
| `sys.RunRef` | `sys.Run` |
| `sys.TransactionRef` | `sys.Transaction` |
| `sys.TraceRef` | `sys.Trace` |
| `sys.SpanRef` | `sys.Span` |
| `sys.TraceEventRef` | `sys.TraceEvent` |
| `sys.StreamRef` | `sys.Stream` |
| `sys.CheckpointRef` | `sys.Checkpoint` |
| `sys.CheckpointUpdateRef` | `sys.CheckpointUpdate` |
| `sys.FailureRef` | `sys.Failure` |
| `sys.SessionRef` | `sys.Session` |
| `sys.ClientRef` | `sys.Client` |
| `sys.LeaseRef` | `sys.Lease` |
| `sys.ListenerRef` | `sys.Listener` |
| `sys.StorageRef` | `sys.Storage` |
| `sys.SegmentRef` | `sys.Segment` |
| `sys.StatisticRef` | `sys.Statistic` |
| `sys.IndexRef` | `sys.Index` |
| `sys.MaterializationRef` | `sys.Materialization` |
| `sys.AllocatorRef` | `sys.Allocator` |
| `sys.HydrationRef` | `sys.Hydration` |
| `sys.CompactionRef` | `sys.Compaction` |
| `sys.MaintenanceJobRef` | `sys.MaintenanceJob` |
| `sys.StorageFileRef` | `sys.StorageFile` |
| `sys.SecretRef` | `sys.Secret` |
| `sys.SettingRef` | `sys.Setting` |
| `sys.BuildRef` | `sys.Build` |
| `sys.TestRef` | `sys.Test` |

## Closed enumerations

### `sys.RuntimeMode` {#api-type-sys-runtimemode}



Values: `repl`, `run`, `serve`, `command_owner`, `embedded`.

### `sys.ObjectKind` {#api-type-sys-objectkind}



Values: `namespace`, `module`, `table`, `column`, `key`, `assertion`, `function`, `parameter`, `type`, `field`, `variant`, `protocol`, `implementation`, `page`, `dimension`, `unit`, `currency`, `secret_requirement`, `extension`.

### `sys.Visibility` {#api-type-sys-visibility}



Values: `private`, `module`, `package`, `public`.

### `sys.Severity` {#api-type-sys-severity}



Values: `note`, `help`, `warning`, `error`, `fatal`.

### `sys.SnapshotKind` {#api-type-sys-snapshotkind}



Values: `commit`, `logical_cwd`, `synthetic_merge`, `build_input`.

### `sys.ChangeKind` {#api-type-sys-changekind}



Values: `add`, `modify`, `delete`, `rename`, `rekey`, `rewrite`.

### `sys.ChangeArea` {#api-type-sys-changearea}



Values: `cwd`, `staged`, `commit`, `merge`, `publication`, `storage`.

### `sys.RunStatus` {#api-type-sys-runstatus}



Values: `starting`, `running`, `completed`, `failed`, `cancelled`, `orphaned`.

### `sys.InvocationStatus` {#api-type-sys-invocationstatus}



Values: `queued`, `running`, `succeeded`, `failed`, `cancelled`, `orphaned`.

### `sys.TransactionStatus` {#api-type-sys-transactionstatus}



Values: `active`, `committing`, `committed`, `rolling_back`, `rolled_back`, `failed`.

### `sys.StreamStatus` {#api-type-sys-streamstatus}



Values: `starting`, `running`, `paused`, `backing_off`, `completed`, `failed`, `cancelled`, `orphaned`.

### `sys.FailureStatus` {#api-type-sys-failurestatus}



Values: `open`, `retrying`, `recovered`, `skipped`, `replaying`, `replayed`, `resolved`.

### `sys.LeaseStatus` {#api-type-sys-leasestatus}



Values: `acquiring`, `held`, `releasing`, `expired`, `lost`.

### `sys.BuildStatus` {#api-type-sys-buildstatus}



Values: `queued`, `running`, `passed`, `failed`, `cancelled`, `orphaned`.

### `sys.TestStatus` {#api-type-sys-teststatus}



Values: `not_run`, `running`, `passed`, `failed`, `skipped`.

### `sys.AssertionOwnerKind` {#api-type-sys-assertionownerkind}



Values: `executable`, `refined_type`, `table`, `module`.

### `sys.AssertionScope` {#api-type-sys-assertionscope}



Values: `activation`, `value_construction`, `transaction_candidate`, `snapshot_candidate`.

### `sys.ClientKind` {#api-type-sys-clientkind}



Values: `cli`, `repl`, `server`, `renderer`, `embedded`, `tool`.

### `sys.DependencyConfidence` {#api-type-sys-dependencyconfidence}



Values: `exact`, `conservative`, `possible`.

### `sys.DependencyKind` {#api-type-sys-dependencykind}



Values: `import`, `type_reference`, `call`, `table_read`, `table_write`, `assertion`, `implementation`, `page_entry`, `renderer_requirement`, `extension_import`, `storage_projection`.

### `sys.DiffScope` {#api-type-sys-diffscope}



Values: `all`, `semantic`, `source`, `rows`, `storage`.

### `sys.EffectKind` {#api-type-sys-effectkind}



Values: `table_read`, `table_write`, `network`, `filesystem`, `process`, `clock`, `randomness`, `secret_access`, `ui`, `git`, `storage_admin`, `runtime_admin`.

### `sys.ExtensionTrust` {#api-type-sys-extensiontrust}



Values: `sandboxed`, `trusted`.

### `sys.FileKind` {#api-type-sys-filekind}



Values: `module`, `row`, `manifest`, `package_manifest`, `storage_manifest`, `compact_segment`, `asset`, `secret_document`, `generated`, `other`.

### `sys.GitObjectKind` {#api-type-sys-gitobjectkind}



Values: `commit`, `tree`, `blob`, `tag`.

### `sys.HostAccess` {#api-type-sys-hostaccess}



Values: `filesystem_read`, `filesystem_write`, `network`, `process`, `environment`, `clock`, `device`.

### `sys.IndexKind` {#api-type-sys-indexkind}



Values: `primary`, `btree`, `hash`, `full_text`, `vector`, `provider`.

### `sys.InvokeTransaction` {#api-type-sys-invoketransaction}



Values: `inherit`, `separate`, `read_only`.

### `sys.ListenerKind` {#api-type-sys-listenerkind}



Values: `git_http`, `query_http`, `websocket`, `renderer`, `admin`, `custom`.

### `sys.MaintenanceKind` {#api-type-sys-maintenancekind}



Values: `flush`, `compact`, `verify`, `hydrate`, `prune`, `checkpoint_publish`, `storage_rewrite`.

### `sys.PlanNodeKind` {#api-type-sys-plannodekind}



Values: `scan`, `index_lookup`, `filter`, `project`, `join`, `aggregate`, `sort`, `limit`, `materialize`, `invoke`, `assertion_validate`, `checkpoint_update`, `external`.

### `sys.ProtocolMemberKind` {#api-type-sys-protocolmemberkind}



Values: `function`, `static_property`.

### `sys.ReferenceAction` {#api-type-sys-referenceaction}



Values: `restrict`, `cascade`, `set_none`.

### `sys.SettingScope` {#api-type-sys-settingscope}



Values: `runtime`, `database`, `repository`, `session`.

### `sys.SettingSource` {#api-type-sys-settingsource}



Values: `default`, `manifest`, `environment`, `command_line`, `local_config`.

### `sys.StatisticKind` {#api-type-sys-statistickind}



Values: `min`, `max`, `null_count`, `distinct_count`, `bloom`, `histogram`.

### `sys.StorageFileKind` {#api-type-sys-storagefilekind}



Values: `cwd_database`, `wal`, `lock`, `socket`, `segment_cache`, `temporary`, `identity`, `other`.

### `sys.StorageProfile` {#api-type-sys-storageprofile}



Values: `empty`, `editable`, `compact`, `hybrid`.

### `sys.StoragePreference` {#api-type-sys-storagepreference}



Values: `automatic`, `editable`, `compact`.

### `sys.StorageRewriteTarget` {#api-type-sys-storagerewritetarget}



Values: `editable`, `compact`.

### `sys.TypeKind` {#api-type-sys-typekind}



Values: `alias`, `nominal`, `refined`, `enum`, `protocol`, `table_row`, `generic_parameter`, `constructed`.

### `sys.VerificationStatus` {#api-type-sys-verificationstatus}



Values: `unknown`, `pending`, `valid`, `invalid`.

### `sys.VerifyScope` {#api-type-sys-verifyscope}



Values: `database`, `repository`, `storage`, `history`, `checkpoints`, `all`.

### `sys.ChangeTargetKind` {#api-type-sys-changetargetkind}



Values: `object`, `file`, `row`.

## Supporting value types

### `sys.RowRef<T>` {#api-type-sys-rowref-t}

Snapshot/runtime-pinned reference to one canonical system-relation row.

This type has no public structural fields; use the operations that explicitly accept it.

Invariant: preserves the row natural key and resolution context.

Invariant: does not grant authority.

Invariant: live references are runtime-generation-bound.

### `sys.Value` {#api-type-sys-value}

Explicit existential envelope for a value crossing a reflective system boundary.

| Field | Type |
|---|---|
| `type` | `sys.TypeRef` |
| `redacted` | `Bool` |
| `canonical_digest` | `Digest?` |

Invariant: is not a dynamic Any fallback.

Invariant: retains the exact originating static type.

Invariant: cannot be implicitly unboxed or converted.

### `sys.Argument` {#api-type-sys-argument}

One named typed reflective-call argument.

| Field | Type |
|---|---|
| `name` | `Str` |
| `value` | `sys.Value` |

### `sys.ArgumentMap` {#api-type-sys-argumentmap}

Immutable ordered collection of uniquely named reflective-call arguments.

| Field | Type |
|---|---|
| `entries` | `[sys.Argument]` |

Invariant: names are unique.

Invariant: canonical order is Unicode scalar-value order by name.

Invariant: record literals are boxed only under an ArgumentMap expected type.

### `sys.ValueMetadata<T>` {#api-type-sys-valuemetadata-t}

Safe type, protocol and codec metadata for a value without private-field disclosure.

| Field | Type |
|---|---|
| `static_type` | `sys.TypeRef` |
| `nominal_type` | `sys.TypeRef?` |
| `protocols` | `Relation<sys.Protocol>` |
| `codecs` | `[Str]` |
| `redacted` | `Bool` |

### `sys.InvocationHandle<T>` {#api-type-sys-invocationhandle-t}

Operational handle for one accepted invocation in one runtime generation.

| Field | Type |
|---|---|
| `invocation` | `sys.InvocationRef` |
| `runtime` | `sys.RuntimeId` |
| `result_type` | `sys.TypeRef` |
| `resumable` | `Bool` |

Invariant: resumable is false in 1.0.

Invariant: usable only by sys.await and sys.cancel.

Invariant: descriptive decoding never revives operational authority.

### `sys.InvocationResult<T>` {#api-type-sys-invocationresult-t}

Complete terminal result returned by sys.await.

| Field | Type |
|---|---|
| `invocation` | `sys.InvocationRef` |
| `status` | `sys.InvocationStatus` |
| `value` | `T?` |
| `failure` | `sys.Diagnostic?` |
| `started` | `Instant?` |
| `ended` | `Instant` |
| `duration` | `Duration?` |

Invariant: status is terminal.

Invariant: succeeded has value and no failure.

Invariant: failed has failure and no value.

Invariant: cancelled/orphaned have no value and may carry a diagnostic.

### `sys.ExpressionRef` {#api-type-sys-expressionref}

Snapshot-pinned reference to a resolved expression in a definition.

| Field | Type |
|---|---|
| `owner` | `sys.ObjectRef` |
| `definition` | `sys.DefinitionRef` |
| `span` | `sys.SourceSpan` |
| `semantic_hash` | `Digest` |

### `sys.SourceSpan` {#api-type-sys-sourcespan}

Half-open canonical UTF-8 source range with one-based human coordinates.

| Field | Type |
|---|---|
| `file` | `sys.FileRef` |
| `start_byte` | `Int` |
| `end_byte` | `Int` |
| `start_line` | `Int` |
| `start_column` | `Int` |
| `end_line` | `Int` |
| `end_column` | `Int` |

Invariant: 0 <= start_byte <= end_byte.

Invariant: line and column values are one-based.

### `sys.SourceMapEntry` {#api-type-sys-sourcemapentry}

Mapping from a generated span to an authored span and optional original name.

| Field | Type |
|---|---|
| `generated` | `sys.SourceSpan` |
| `original` | `sys.SourceSpan?` |
| `original_name` | `Str?` |

### `sys.DiagnosticLabel` {#api-type-sys-diagnosticlabel}

One source label attached to a structured diagnostic.

| Field | Type |
|---|---|
| `span` | `sys.SourceSpan` |
| `message` | `Str` |
| `primary` | `Bool` |

### `sys.SourceDocument` {#api-type-sys-sourcedocument}

Exact retained source plus source maps and explicit unavailability state.

| Field | Type |
|---|---|
| `file` | `sys.FileRef` |
| `snapshot` | `sys.SnapshotRef` |
| `text` | `Str?` |
| `exact_hash` | `Digest` |
| `encoding` | `Str` |
| `generated` | `Bool` |
| `maps` | `Relation<sys.SourceMapEntry>` |
| `unavailable_reason` | `Str?` |

Invariant: text and unavailable_reason are not both absent.

Invariant: text hashes to exact_hash after canonical decoding.

### `sys.Attribution` {#api-type-sys-attribution}

Source or semantic blame attribution for one exact target range.

| Field | Type |
|---|---|
| `file` | `sys.FileRef` |
| `span` | `sys.SourceSpan` |
| `snapshot` | `sys.SnapshotRef` |
| `commit` | `sys.CommitRef?` |
| `author` | `sys.PersonIdentity?` |
| `authored_at` | `Instant?` |
| `object` | `sys.ObjectRef?` |
| `semantic` | `Bool` |

### `sys.PersonIdentity` {#api-type-sys-personidentity}

Git-compatible authored identity without signing or authorization semantics.

| Field | Type |
|---|---|
| `name` | `Str` |
| `email` | `Str?` |

### `sys.ObjectDescription` {#api-type-sys-objectdescription}

Structured summary of one semantic object at its pinned revision.

| Field | Type |
|---|---|
| `object` | `sys.ObjectRef` |
| `revision` | `sys.RevisionRef` |
| `kind` | `sys.ObjectKind` |
| `qualified_name` | `Str` |
| `definition` | `sys.DefinitionRef?` |
| `docs` | `Str?` |
| `signature` | `Str?` |
| `metadata` | `sys.Value` |

### `sys.Explanation` {#api-type-sys-explanation}

Structured causal explanation of a diagnostic.

| Field | Type |
|---|---|
| `diagnostic` | `sys.Diagnostic` |
| `summary` | `Str` |
| `causes` | `[sys.Diagnostic]` |
| `suggestions` | `[Str]` |
| `related_objects` | `[sys.ObjectRef]` |
| `plan` | `sys.PlanRef?` |

### `sys.CheckpointPosition` {#api-type-sys-checkpointposition}

Provider-specific replay position with portable equality and canonical encoding.

| Field | Type |
|---|---|
| `format` | `Str` |
| `digest` | `Digest` |

Invariant: payload is not generically orderable.

Invariant: equality includes format and canonical payload.

Invariant: provider APIs own construction and comparison beyond equality.

### `sys.CompatibilityInfo` {#api-type-sys-compatibilityinfo}

Compatibility tuple embedded in builds and artifacts.

| Field | Type |
|---|---|
| `language_version` | `Str` |
| `sys_version` | `Str` |
| `canonical_orna_codec_version` | `Str` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |
| `presentation_protocol_version` | `Str` |
| `supported_profiles` | `[Str]` |

### `sys.RuntimeInfo` {#api-type-sys-runtimeinfo}

Exact language, sys, storage, presentation and implementation coordinates for the current runtime.

| Field | Type |
|---|---|
| `language_version` | `Str` |
| `sys_version` | `Str` |
| `canonical_orna_codec_version` | `Str` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |
| `presentation_protocol_version` | `Str` |
| `implementation_name` | `Str` |
| `implementation_version` | `Str` |
| `build_id` | `Str` |
| `supported_profiles` | `[Str]` |
| `runtime` | `sys.RuntimeId` |
| `mode` | `sys.RuntimeMode` |
| `read_only` | `Bool` |
| `std_snapshot` | `sys.SnapshotRef?` |
| `unicode_version` | `Str` |
| `timezone_database_version` | `Str?` |

### `sys.FlushResult` {#api-type-sys-flushresult}

Result of sealing durable pending rows without a logical data change.

| Field | Type |
|---|---|
| `tables` | `[sys.TableRef]` |
| `rows` | `Int` |
| `segments` | `[sys.SegmentRef]` |
| `bytes` | `Int` |
| `generation` | `Int` |
| `semantic_changes` | `Int` |

Invariant: semantic_changes is zero.

### `sys.CompactionResult` {#api-type-sys-compactionresult}

Result of an atomic physical segment compaction.

| Field | Type |
|---|---|
| `tables` | `[sys.TableRef]` |
| `input_segments` | `[sys.SegmentRef]` |
| `output_segments` | `[sys.SegmentRef]` |
| `rows` | `Int` |
| `bytes_before` | `Int` |
| `bytes_after` | `Int` |
| `generation` | `Int` |
| `semantic_changes` | `Int` |

Invariant: semantic_changes is zero.

### `sys.StorageRewriteResult` {#api-type-sys-storagerewriteresult}

Result of a generation-CAS rewrite between editable and compact physical representations.

| Field | Type |
|---|---|
| `table` | `sys.TableRef` |
| `from` | `sys.StorageProfile` |
| `to` | `sys.StorageProfile` |
| `rows` | `Int` |
| `previous_generation` | `Int` |
| `generation` | `Int` |
| `semantic_diff` | `Relation<sys.DiffEntry>` |

Invariant: semantic_diff is empty.

Invariant: generation is published only after complete verification.

### `sys.VerificationReport` {#api-type-sys-verificationreport}

Structured integrity-verification outcome.

| Field | Type |
|---|---|
| `scope` | `sys.VerifyScope` |
| `status` | `sys.VerificationStatus` |
| `started` | `Instant` |
| `ended` | `Instant` |
| `checked` | `sys.Value` |
| `diagnostics` | `Relation<sys.Diagnostic>` |

Invariant: invalid has at least one error/fatal diagnostic.

Invariant: valid has no error/fatal diagnostic.

### `sys.CancellationView` {#api-type-sys-cancellationview}

Immutable activation-local cancellation observation.

| Field | Type |
|---|---|
| `requested` | `Bool` |
| `reason` | `Str?` |
| `requested_at` | `Instant?` |

### `sys.PresentationOverride` {#api-type-sys-presentationoverride}

Session-local presentation selection for one type.

| Field | Type |
|---|---|
| `type` | `sys.TypeRef` |
| `renderer` | `Str?` |
| `mode` | `Str` |
| `formatter` | `sys.FunctionRef?` |

### `sys.Watch` {#api-type-sys-watch}

REPL watch expression and its most recent safe observation.

| Field | Type |
|---|---|
| `expression` | `sys.ExpressionRef` |
| `label` | `Str?` |
| `last_value` | `sys.Value?` |
| `last_diagnostic` | `sys.Diagnostic?` |
| `updated` | `Instant?` |

### `sys.DatabaseView` {#api-type-sys-databaseview}

Current attached database/worktree descriptor exposed by sys.database.

| Field | Type |
|---|---|
| `id` | `sys.DatabaseId` |
| `name` | `Str` |
| `root` | `Path` |
| `root_module` | `sys.ModuleRef` |
| `cwd` | `sys.SnapshotRef` |
| `head` | `sys.SnapshotRef?` |
| `branch` | `sys.BranchRef?` |
| `writable` | `Bool` |
| `attached_as` | `Str?` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |

### `sys.CurrentContext` {#api-type-sys-currentcontext}

Immutable context of the current activation exposed by sys.current.

| Field | Type |
|---|---|
| `snapshot` | `sys.SnapshotRef` |
| `transaction` | `sys.TransactionRef?` |
| `invocation` | `sys.InvocationRef?` |
| `run` | `sys.RunRef?` |
| `session` | `sys.SessionRef?` |
| `client` | `sys.ClientRef?` |
| `logical_cwd` | `Path` |
| `locale` | `Locale` |
| `timezone` | `TimeZone` |
| `trace` | `sys.TraceRef?` |
| `cancellation` | `sys.CancellationView` |
| `present_context` | `PresentContext` |

### `sys.RuntimeView` {#api-type-sys-runtimeview}

Current live runtime owner and live relation handles exposed by sys.rt.

| Field | Type |
|---|---|
| `id` | `sys.RuntimeId` |
| `mode` | `sys.RuntimeMode` |
| `started` | `Instant` |
| `owner_pid` | `Int?` |
| `owner_endpoint` | `Str?` |
| `read_only` | `Bool` |
| `runs` | `Relation<sys.Run>` |
| `streams` | `Relation<sys.Stream>` |
| `transactions` | `Relation<sys.Transaction>` |
| `invocations` | `Relation<sys.Invocation>` |
| `queries` | `Relation<sys.Query>` |
| `sessions` | `Relation<sys.Session>` |
| `clients` | `Relation<sys.Client>` |
| `traces` | `Relation<sys.Trace>` |
| `failures` | `Relation<sys.Failure>` |
| `diagnostics` | `Relation<sys.Diagnostic>` |
| `leases` | `Relation<sys.Lease>` |
| `listeners` | `Relation<sys.Listener>` |
| `plans` | `Relation<sys.Plan>` |
| `plan_nodes` | `Relation<sys.PlanNode>` |
| `invocation_arguments` | `Relation<sys.InvocationArgument>` |
| `spans` | `Relation<sys.Span>` |
| `events` | `Relation<sys.TraceEvent>` |
| `checkpoints` | `Relation<sys.Checkpoint>` |
| `checkpoint_updates` | `Relation<sys.CheckpointUpdate>` |

### `sys.ReplView` {#api-type-sys-replview}

Interactive session presentation and watch state exposed by sys.repl.

| Field | Type |
|---|---|
| `session` | `sys.SessionRef` |
| `renderer` | `Str` |
| `width` | `Int?` |
| `height` | `Int?` |
| `locale` | `Locale` |
| `timezone` | `TimeZone` |
| `presentation_overrides` | `Relation<sys.PresentationOverride>` |
| `watched_expressions` | `Relation<sys.Watch>` |

### `sys.ChangeTarget` {#api-type-sys-changetarget}

Discriminated identity of one changed semantic object, source file or table row.

| Field | Type |
|---|---|
| `kind` | `sys.ChangeTargetKind` |
| `object` | `sys.ObjectId?` |
| `file` | `sys.FileId?` |
| `table` | `sys.ObjectId?` |
| `row_key` | `sys.Value?` |

Invariant: object: only object is present; file: only file is present; row: only table and row_key are present.

Invariant: row_key retains a non-secret key type and canonical value.

Invariant: equality includes kind and all populated identity fields.

### `sys.CheckpointConflict` {#api-type-sys-checkpointconflict}

A three-way merge conflict between incomparable or divergent checkpoint positions.

| Field | Type |
|---|---|
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `base` | `sys.CheckpointPosition?` |
| `left` | `sys.CheckpointPosition?` |
| `right` | `sys.CheckpointPosition?` |
| `left_snapshot` | `sys.SnapshotRef` |
| `right_snapshot` | `sys.SnapshotRef` |
| `reason` | `Str` |

Invariant: reason is divergent_position, incompatible_format, or delete_update.

Invariant: opaque positions are never numerically ordered.

### `sys.RowConflict` {#api-type-sys-rowconflict}

A three-way merge conflict at one stable table identity and primary key.

| Field | Type |
|---|---|
| `table` | `sys.TableRef` |
| `key` | `sys.Value` |
| `base` | `sys.Value?` |
| `left` | `sys.Value?` |
| `right` | `sys.Value?` |
| `reason` | `Str` |

Invariant: absence means no row, not a row with null fields.

Invariant: reason is update_update, delete_update, key_collision, or assertion_failure.

### `sys.CheckoutPlan` {#api-type-sys-checkoutplan}

Read-only preview bound to the exact target and local worktree state.

| Field | Type |
|---|---|
| `token` | `Digest` |
| `target` | `sys.SnapshotRef` |
| `target_branch` | `Str?` |
| `base_head` | `sys.GitOid?` |
| `cwd` | `sys.SnapshotRef` |
| `index_digest` | `Digest` |
| `worktree_digest` | `Digest` |
| `pending_generation` | `Int` |
| `would_discard` | `[sys.ChangeTarget]` |
| `active_consumers` | `[sys.ConsumerIdentity]` |
| `conflicts` | `[sys.Diagnostic]` |

Invariant: token is a domain-separated digest of all plan inputs in canonical order.

Invariant: the plan performs no mutation.

Invariant: force is not permission to accept a stale token.

## Canonical system relations

### `sys.Database` {#api-type-sys-database}

Database/worktree attachments visible at a snapshot.

**Grouped handle:** `sys.catalog.databases`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.DatabaseRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DatabaseRef` |
| `id` | `sys.DatabaseId` |
| `name` | `Str` |
| `root` | `Path` |
| `root_module` | `sys.ModuleRef` |
| `cwd` | `sys.SnapshotRef` |
| `head` | `sys.SnapshotRef?` |
| `branch` | `sys.BranchRef?` |
| `writable` | `Bool` |
| `attached_as` | `Str?` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |

### `sys.Namespace` {#api-type-sys-namespace}

Resolved namespace tree.

**Grouped handle:** `sys.catalog.namespaces`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.NamespaceRef`.

| Field | Type |
|---|---|
| `reference` | `sys.NamespaceRef` |
| `object` | `sys.ObjectRef` |
| `database` | `sys.DatabaseRef` |
| `name` | `Str` |
| `parent` | `sys.NamespaceRef?` |
| `docs` | `Str?` |

### `sys.Module` {#api-type-sys-module}

Resolved source module.

**Grouped handle:** `sys.catalog.modules`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ModuleRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ModuleRef` |
| `object` | `sys.ObjectRef` |
| `namespace` | `sys.NamespaceRef` |
| `file` | `sys.FileRef` |
| `semantic_hash` | `Digest` |
| `reachable` | `Bool` |
| `diagnostics` | `Relation<sys.Diagnostic>` |

### `sys.Import` {#api-type-sys-import}

Resolved import edge.

**Grouped handle:** `sys.catalog.imports`. **Availability:** catalogue. **Natural key:** `module + position`. **Reference:** `sys.ImportRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ImportRef` |
| `module` | `sys.ModuleRef` |
| `position` | `Int` |
| `source` | `Str` |
| `target` | `sys.ObjectRef?` |
| `alias` | `Str?` |
| `visibility` | `sys.Visibility` |
| `span` | `sys.SourceSpan` |

### `sys.File` {#api-type-sys-file}

Repository file/source unit metadata.

**Grouped handle:** `sys.catalog.files`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.FileRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FileRef` |
| `id` | `sys.FileId` |
| `database` | `sys.DatabaseRef` |
| `path` | `Path` |
| `kind` | `sys.FileKind` |
| `git_object` | `sys.GitObjectRef?` |
| `exact_hash` | `Digest` |
| `size` | `Int` |
| `status` | `sys.ChangeKind?` |
| `text_available` | `Bool` |
| `generated` | `Bool` |

### `sys.Object` {#api-type-sys-object}

Stable semantic object identity at a snapshot.

**Grouped handle:** `sys.catalog.objects`. **Availability:** catalogue. **Natural key:** `id + snapshot`. **Reference:** `sys.ObjectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ObjectRef` |
| `id` | `sys.ObjectId` |
| `kind` | `sys.ObjectKind` |
| `qualified_name` | `Str` |
| `current_revision` | `sys.RevisionRef` |
| `definition` | `sys.DefinitionRef?` |
| `visibility` | `sys.Visibility` |
| `snapshot` | `sys.SnapshotRef` |

### `sys.Revision` {#api-type-sys-revision}

Immutable semantic revision of a stable object.

**Grouped handle:** `sys.catalog.revisions`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.RevisionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.RevisionRef` |
| `id` | `sys.RevisionId` |
| `object` | `sys.ObjectRef` |
| `semantic_hash` | `Digest` |
| `source_hash` | `Digest?` |
| `introduced_in` | `sys.SnapshotRef?` |
| `supersedes` | `sys.RevisionRef?` |
| `definition` | `sys.DefinitionRef?` |

### `sys.Definition` {#api-type-sys-definition}

Source definition and span of an object revision.

**Grouped handle:** `sys.catalog.definitions`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.DefinitionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DefinitionRef` |
| `id` | `sys.DefinitionId` |
| `object` | `sys.ObjectRef` |
| `module` | `sys.ModuleRef` |
| `file` | `sys.FileRef` |
| `span` | `sys.SourceSpan` |
| `docs` | `Str?` |

### `sys.Type` {#api-type-sys-type}

Resolved static type declaration or constructed type metadata.

**Grouped handle:** `sys.catalog.types`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.TypeRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TypeRef` |
| `object` | `sys.ObjectRef` |
| `kind` | `sys.TypeKind` |
| `transparent` | `Bool` |
| `base` | `sys.TypeRef?` |
| `parameters` | `Relation<sys.TypeParameter>` |
| `fields` | `Relation<sys.Field>` |
| `variants` | `Relation<sys.Variant>` |
| `assertions` | `Relation<sys.Assertion>` |

### `sys.TypeParameter` {#api-type-sys-typeparameter}

Generic parameter and protocol bounds.

**Grouped handle:** `sys.catalog.type_parameters`. **Availability:** catalogue. **Natural key:** `owner + position`. **Reference:** `sys.TypeParameterRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TypeParameterRef` |
| `owner` | `sys.ObjectRef` |
| `name` | `Str` |
| `position` | `Int` |
| `bounds` | `Relation<sys.Protocol>` |
| `inferred` | `Bool` |

### `sys.Field` {#api-type-sys-field}

Nominal type field metadata.

**Grouped handle:** `sys.catalog.fields`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.FieldRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FieldRef` |
| `object` | `sys.ObjectRef` |
| `owner` | `sys.TypeRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `visibility` | `sys.Visibility` |
| `docs` | `Str?` |

### `sys.Variant` {#api-type-sys-variant}

Enum/sum variant metadata.

**Grouped handle:** `sys.catalog.variants`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.VariantRef`.

| Field | Type |
|---|---|
| `reference` | `sys.VariantRef` |
| `object` | `sys.ObjectRef` |
| `owner` | `sys.TypeRef` |
| `name` | `Str` |
| `position` | `Int` |
| `payload_type` | `sys.TypeRef?` |
| `docs` | `Str?` |

### `sys.Protocol` {#api-type-sys-protocol}

Protocol declaration metadata.

**Grouped handle:** `sys.catalog.protocols`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ProtocolRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ProtocolRef` |
| `object` | `sys.ObjectRef` |
| `parameters` | `Relation<sys.TypeParameter>` |
| `members` | `Relation<sys.ProtocolMember>` |
| `docs` | `Str?` |

### `sys.ProtocolMember` {#api-type-sys-protocolmember}

Required protocol function or static property.

**Grouped handle:** `sys.catalog.protocol_members`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ProtocolMemberRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ProtocolMemberRef` |
| `object` | `sys.ObjectRef` |
| `protocol` | `sys.ProtocolRef` |
| `name` | `Str` |
| `kind` | `sys.ProtocolMemberKind` |
| `type` | `sys.TypeRef` |
| `position` | `Int` |

### `sys.Implementation` {#api-type-sys-implementation}

Nested protocol implementation owned by its target type.

**Grouped handle:** `sys.catalog.implementations`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ImplementationRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ImplementationRef` |
| `object` | `sys.ObjectRef` |
| `protocol` | `sys.ProtocolRef` |
| `target` | `sys.TypeRef` |
| `source_type` | `sys.TypeRef?` |
| `owner` | `sys.ObjectRef` |
| `members` | `Relation<sys.Function>` |

### `sys.Function` {#api-type-sys-function}

Resolved function signature, effects and dependency surface.

**Grouped handle:** `sys.catalog.functions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.FunctionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FunctionRef` |
| `object` | `sys.ObjectRef` |
| `definition` | `sys.DefinitionRef` |
| `parameters` | `Relation<sys.Parameter>` |
| `return_type` | `sys.TypeRef` |
| `failure_types` | `Relation<sys.Type>` |
| `effects` | `Relation<sys.Effect>` |
| `reads` | `Relation<sys.Table>` |
| `writes` | `Relation<sys.Table>` |
| `calls` | `Relation<sys.Function>` |
| `inferred_signature` | `Bool` |
| `docs` | `Str?` |

### `sys.Parameter` {#api-type-sys-parameter}

Function parameter metadata.

**Grouped handle:** `sys.catalog.parameters`. **Availability:** catalogue. **Natural key:** `function + position`. **Reference:** `sys.ParameterRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ParameterRef` |
| `function` | `sys.FunctionRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `optional` | `Bool` |
| `default_expression` | `sys.ExpressionRef?` |
| `explicit_type` | `Bool` |

### `sys.Effect` {#api-type-sys-effect}

Conservative static effect metadata.

**Grouped handle:** `sys.catalog.effects`. **Availability:** catalogue. **Natural key:** `function + kind + target`. **Reference:** `sys.EffectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.EffectRef` |
| `function` | `sys.FunctionRef` |
| `kind` | `sys.EffectKind` |
| `target` | `sys.ObjectRef?` |
| `conditional` | `Bool` |

### `sys.Table` {#api-type-sys-table}

Persistent relation declaration and storage projection.

**Grouped handle:** `sys.catalog.tables`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.TableRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TableRef` |
| `object` | `sys.ObjectRef` |
| `definition` | `sys.DefinitionRef` |
| `key` | `sys.KeyRef` |
| `columns` | `Relation<sys.Column>` |
| `assertions` | `Relation<sys.Assertion>` |
| `references` | `Relation<sys.Reference>` |
| `storage` | `sys.StorageRef` |
| `row_count` | `Int?` |
| `row_count_exact` | `Bool` |

### `sys.Column` {#api-type-sys-column}

Table column metadata.

**Grouped handle:** `sys.catalog.columns`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ColumnRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ColumnRef` |
| `object` | `sys.ObjectRef` |
| `table` | `sys.TableRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `optional` | `Bool` |
| `key_position` | `Int?` |
| `default_expression` | `sys.ExpressionRef?` |
| `computed_expression` | `sys.ExpressionRef?` |
| `docs` | `Str?` |

### `sys.Key` {#api-type-sys-key}

Primary-key definition and path/storage codec.

**Grouped handle:** `sys.catalog.keys`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.KeyRef`.

| Field | Type |
|---|---|
| `reference` | `sys.KeyRef` |
| `object` | `sys.ObjectRef` |
| `table` | `sys.TableRef` |
| `columns` | `Relation<sys.Column>` |
| `automatic` | `Bool` |
| `allocator` | `sys.AllocatorRef?` |
| `path_codec` | `Str` |

### `sys.Assertion` {#api-type-sys-assertion}

Always-enforced executable, refined, table or cross-table proposition.

**Grouped handle:** `sys.catalog.assertions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.AssertionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.AssertionRef` |
| `object` | `sys.ObjectRef` |
| `owner` | `sys.ObjectRef` |
| `owner_kind` | `sys.AssertionOwnerKind` |
| `subject_type` | `sys.TypeRef?` |
| `expression` | `sys.ExpressionRef` |
| `dependencies` | `Relation<sys.Dependency>` |
| `definition` | `sys.DefinitionRef` |
| `source_order` | `Int` |
| `scope` | `sys.AssertionScope` |
| `deterministic` | `Bool` |

### `sys.Reference` {#api-type-sys-reference}

Stored table-reference edge.

**Grouped handle:** `sys.catalog.references`. **Availability:** catalogue. **Natural key:** `from_column`. **Reference:** `sys.ReferenceRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ReferenceRef` |
| `from_table` | `sys.TableRef` |
| `from_column` | `sys.ColumnRef` |
| `to_table` | `sys.TableRef` |
| `to_key` | `sys.KeyRef` |
| `optional` | `Bool` |
| `on_delete` | `sys.ReferenceAction` |
| `on_rekey` | `sys.ReferenceAction` |

### `sys.Page` {#api-type-sys-page}

Function-derived page entry metadata.

**Grouped handle:** `sys.catalog.pages`. **Availability:** catalogue. **Natural key:** `function`. **Reference:** `sys.PageRef`.

| Field | Type |
|---|---|
| `reference` | `sys.PageRef` |
| `function` | `sys.FunctionRef` |
| `path` | `Str` |
| `dependencies` | `Relation<sys.Dependency>` |
| `definition` | `sys.DefinitionRef` |

### `sys.Dimension` {#api-type-sys-dimension}

Reduced physical dimension declaration.

**Grouped handle:** `sys.catalog.dimensions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.DimensionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DimensionRef` |
| `object` | `sys.ObjectRef` |
| `exponents` | `sys.Value` |
| `definition` | `sys.DefinitionRef` |

### `sys.Unit` {#api-type-sys-unit}

Unit scale/offset and dimension.

**Grouped handle:** `sys.catalog.units`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.UnitRef`.

| Field | Type |
|---|---|
| `reference` | `sys.UnitRef` |
| `object` | `sys.ObjectRef` |
| `dimension` | `sys.DimensionRef` |
| `scale` | `Decimal` |
| `offset` | `Decimal` |
| `affine` | `Bool` |
| `definition` | `sys.DefinitionRef` |

### `sys.Currency` {#api-type-sys-currency}

Nominal type implementing Currency.

**Grouped handle:** `sys.catalog.currencies`. **Availability:** catalogue. **Natural key:** `type`. **Reference:** `sys.CurrencyRef`.

| Field | Type |
|---|---|
| `reference` | `sys.CurrencyRef` |
| `type` | `sys.TypeRef` |
| `code` | `Str` |
| `minor_digits` | `Int` |
| `definition` | `sys.DefinitionRef` |

### `sys.SecretRequirement` {#api-type-sys-secretrequirement}

Declared secret dependency without plaintext.

**Grouped handle:** `sys.catalog.secret_requirements`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.SecretRequirementRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SecretRequirementRef` |
| `object` | `sys.ObjectRef` |
| `name` | `Str` |
| `required_by` | `sys.ObjectRef` |
| `optional` | `Bool` |
| `definition` | `sys.DefinitionRef` |

### `sys.Extension` {#api-type-sys-extension}

Extension component interface and coarse trust declaration.

**Grouped handle:** `sys.catalog.extensions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ExtensionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ExtensionRef` |
| `object` | `sys.ObjectRef` |
| `component_digest` | `Digest` |
| `interface` | `Str` |
| `declared_imports` | `[Str]` |
| `declared_exports` | `[Str]` |
| `host_access` | `[sys.HostAccess]` |
| `trust` | `sys.ExtensionTrust` |

### `sys.Dependency` {#api-type-sys-dependency}

Typed semantic dependency edge.

**Grouped handle:** `sys.catalog.dependencies`. **Availability:** catalogue. **Natural key:** `from + to + kind + span`. **Reference:** `sys.DependencyRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DependencyRef` |
| `from` | `sys.ObjectRef` |
| `to` | `sys.ObjectRef` |
| `kind` | `sys.DependencyKind` |
| `definition` | `sys.DefinitionRef?` |
| `span` | `sys.SourceSpan?` |
| `confidence` | `sys.DependencyConfidence` |
| `conditional` | `Bool` |

### `sys.Diagnostic` {#api-type-sys-diagnostic}

Structured diagnostic independent of renderer.

**Grouped handle:** `sys.rt.diagnostics`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.DiagnosticRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.DiagnosticRef` |
| `id` | `Str` |
| `severity` | `sys.Severity` |
| `code` | `Str` |
| `message` | `Str` |
| `object` | `sys.ObjectRef?` |
| `definition` | `sys.DefinitionRef?` |
| `primary_span` | `sys.SourceSpan?` |
| `labels` | `[sys.DiagnosticLabel]` |
| `causes` | `[sys.Diagnostic]` |
| `help` | `[Str]` |
| `data` | `sys.Value?` |
| `redacted` | `Bool` |
| `trace` | `sys.TraceRef?` |

### `sys.Snapshot` {#api-type-sys-snapshot}

Committed, logical-CWD or synthetic database snapshot.

**Grouped handle:** `sys.history.snapshots`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.SnapshotRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SnapshotRef` |
| `id` | `sys.SnapshotId` |
| `kind` | `sys.SnapshotKind` |
| `database` | `sys.DatabaseRef` |
| `git_commit` | `sys.CommitRef?` |
| `logical_generation` | `Int?` |
| `created` | `Instant` |
| `parents` | `Relation<sys.Snapshot>` |
| `complete` | `Bool` |
| `missing_objects` | `Relation<sys.GitObject>` |

### `sys.SnapshotObject` {#api-type-sys-snapshotobject}

Object revision membership in a snapshot.

**Grouped handle:** `sys.history.snapshot_objects`. **Availability:** catalogue. **Natural key:** `snapshot + object`. **Reference:** `sys.SnapshotObjectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SnapshotObjectRef` |
| `snapshot` | `sys.SnapshotRef` |
| `object` | `sys.ObjectRef` |
| `revision` | `sys.RevisionRef` |
| `reachable` | `Bool` |

### `sys.Change` {#api-type-sys-change}

Semantic/source/row change entry.

**Grouped handle:** `sys.history.changes`. **Availability:** catalogue. **Natural key:** `snapshot + area + target`. **Reference:** `sys.ChangeRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ChangeRef` |
| `snapshot` | `sys.SnapshotRef` |
| `target` | `sys.ChangeTarget` |
| `area` | `sys.ChangeArea` |
| `kind` | `sys.ChangeKind` |
| `object` | `sys.ObjectRef?` |
| `file` | `sys.FileRef?` |
| `table` | `sys.TableRef?` |
| `key` | `sys.Value?` |
| `before` | `sys.Value?` |
| `after` | `sys.Value?` |
| `old_name` | `Str?` |
| `new_name` | `Str?` |

Invariant: object/file/table/key projections agree exactly with target discriminator.

Invariant: snapshot identifies the compared result generation.

Invariant: one consolidated change per snapshot, area and typed target.

### `sys.DiffEntry` {#api-type-sys-diffentry}

Snapshot-to-snapshot semantic diff entry.

**Grouped handle:** `sys.history.diffs`. **Availability:** derived. **Natural key:** `from + to + change.area + change.target`. **Reference:** `sys.DiffEntryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DiffEntryRef` |
| `from` | `sys.SnapshotRef` |
| `to` | `sys.SnapshotRef` |
| `change` | `sys.Change` |
| `confidence` | `sys.DependencyConfidence` |
| `details` | `sys.Value?` |

### `sys.FileVersion` {#api-type-sys-fileversion}

One retained version of a repository file at a snapshot.

**Grouped handle:** `sys.history.file_versions`. **Availability:** derived. **Natural key:** `file + snapshot`. **Reference:** `sys.FileVersionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FileVersionRef` |
| `file` | `sys.FileRef` |
| `snapshot` | `sys.SnapshotRef` |
| `path` | `Path` |
| `kind` | `sys.FileKind` |
| `git_object` | `sys.GitObjectRef?` |
| `exact_hash` | `Digest` |
| `size` | `Int` |
| `change` | `sys.ChangeKind?` |
| `text_available` | `Bool` |
| `generated` | `Bool` |

### `sys.GitRepository` {#api-type-sys-gitrepository}

Underlying real Git repository metadata.

**Grouped handle:** `sys.git.repositories`. **Availability:** catalogue. **Natural key:** `database`. **Reference:** `sys.GitRepositoryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.GitRepositoryRef` |
| `database` | `sys.DatabaseRef` |
| `git_dir` | `Path` |
| `worktree` | `Path?` |
| `bare` | `Bool` |
| `object_format` | `Str` |
| `partial_clone` | `Bool` |

### `sys.Commit` {#api-type-sys-commit}

Git commit and corresponding Orna snapshot.

**Grouped handle:** `sys.git.commits`. **Availability:** catalogue. **Natural key:** `oid`. **Reference:** `sys.CommitRef`.

| Field | Type |
|---|---|
| `reference` | `sys.CommitRef` |
| `oid` | `sys.GitOid` |
| `snapshot` | `sys.SnapshotRef` |
| `parents` | `Relation<sys.Commit>` |
| `author` | `sys.PersonIdentity` |
| `committer` | `sys.PersonIdentity` |
| `message` | `Str` |
| `authored_at` | `Instant` |
| `committed_at` | `Instant` |
| `tree` | `sys.GitObjectRef` |

### `sys.TreeEntry` {#api-type-sys-treeentry}

Git tree entry.

**Grouped handle:** `sys.git.tree_entries`. **Availability:** catalogue. **Natural key:** `tree + path`. **Reference:** `sys.TreeEntryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TreeEntryRef` |
| `tree` | `sys.GitObjectRef` |
| `path` | `Path` |
| `mode` | `Int` |
| `object` | `sys.GitObjectRef` |
| `kind` | `sys.GitObjectKind` |

### `sys.Ref` {#api-type-sys-ref}

Git ref, including hidden Orna refs.

**Grouped handle:** `sys.git.refs`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.RefRef`.

| Field | Type |
|---|---|
| `reference` | `sys.RefRef` |
| `name` | `Str` |
| `target` | `sys.GitObjectRef` |
| `symbolic_target` | `Str?` |
| `hidden` | `Bool` |
| `remote` | `sys.RemoteRef?` |

### `sys.Branch` {#api-type-sys-branch}

Local/remote branch projection.

**Grouped handle:** `sys.git.branches`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.BranchRef`.

| Field | Type |
|---|---|
| `reference` | `sys.BranchRef` |
| `name` | `Str` |
| `commit` | `sys.CommitRef` |
| `current` | `Bool` |
| `upstream` | `sys.RefRef?` |
| `ahead` | `Int?` |
| `behind` | `Int?` |

### `sys.Tag` {#api-type-sys-tag}

Git tag projection.

**Grouped handle:** `sys.git.tags`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.TagRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TagRef` |
| `name` | `Str` |
| `object` | `sys.GitObjectRef` |
| `annotated` | `Bool` |
| `message` | `Str?` |
| `tagger` | `sys.PersonIdentity?` |

### `sys.Remote` {#api-type-sys-remote}

Git remote without plaintext credentials.

**Grouped handle:** `sys.git.remotes`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.RemoteRef`.

| Field | Type |
|---|---|
| `reference` | `sys.RemoteRef` |
| `name` | `Str` |
| `fetch_urls` | `[Str]` |
| `push_urls` | `[Str]` |
| `promisor` | `Bool` |
| `last_error` | `sys.Diagnostic?` |

### `sys.Stash` {#api-type-sys-stash}

Git stash projection.

**Grouped handle:** `sys.git.stashes`. **Availability:** catalogue. **Natural key:** `index`. **Reference:** `sys.StashRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StashRef` |
| `index` | `Int` |
| `commit` | `sys.CommitRef` |
| `message` | `Str` |
| `created` | `Instant?` |

### `sys.GitObject` {#api-type-sys-gitobject}

Git object availability and type.

**Grouped handle:** `sys.git.objects`. **Availability:** catalogue. **Natural key:** `oid`. **Reference:** `sys.GitObjectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.GitObjectRef` |
| `oid` | `sys.GitOid` |
| `kind` | `sys.GitObjectKind` |
| `size` | `Int?` |
| `available_locally` | `Bool` |
| `promisor_remote` | `sys.RemoteRef?` |

### `sys.WorktreeEntry` {#api-type-sys-worktreeentry}

Physical Git worktree status, distinct from logical CWD state.

**Grouped handle:** `sys.git.worktree`. **Availability:** local-durable. **Natural key:** `path`. **Reference:** `sys.WorktreeEntryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.WorktreeEntryRef` |
| `path` | `Path` |
| `index_status` | `sys.ChangeKind?` |
| `worktree_status` | `sys.ChangeKind?` |
| `ignored` | `Bool` |
| `untracked` | `Bool` |

### `sys.Query` {#api-type-sys-query}

Query execution observation.

**Grouped handle:** `sys.rt.queries`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.QueryRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.QueryRef` |
| `id` | `sys.QueryId` |
| `expression` | `Str?` |
| `function` | `sys.FunctionRef?` |
| `snapshot` | `sys.SnapshotRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.InvocationStatus` |
| `rows` | `Int?` |
| `bytes` | `Int?` |
| `duration` | `Duration?` |
| `plan` | `sys.PlanRef?` |
| `trace` | `sys.TraceRef?` |
| `failure` | `sys.Diagnostic?` |

### `sys.Plan` {#api-type-sys-plan}

Structured explain plan.

**Grouped handle:** `sys.rt.plans`. **Availability:** derived. **Natural key:** `id`. **Reference:** `sys.PlanRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.PlanRef` |
| `id` | `Str` |
| `snapshot` | `sys.SnapshotRef` |
| `root` | `sys.PlanNodeRef` |
| `estimated_cost` | `Decimal?` |
| `actual_available` | `Bool` |
| `warnings` | `[sys.Diagnostic]` |

### `sys.PlanNode` {#api-type-sys-plannode}

One structured plan operation.

**Grouped handle:** `sys.rt.plan_nodes`. **Availability:** derived. **Natural key:** `plan + position`. **Reference:** `sys.PlanNodeRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.PlanNodeRef` |
| `plan` | `sys.PlanRef` |
| `parent` | `sys.PlanNodeRef?` |
| `position` | `Int` |
| `kind` | `sys.PlanNodeKind` |
| `inputs` | `Relation<sys.PlanNode>` |
| `object` | `sys.ObjectRef?` |
| `estimated_rows` | `Int?` |
| `actual_rows` | `Int?` |
| `estimated_bytes` | `Int?` |
| `actual_bytes` | `Int?` |
| `predicate` | `sys.ExpressionRef?` |
| `details` | `sys.Value` |

### `sys.Invocation` {#api-type-sys-invocation}

Function activation/invocation observation.

**Grouped handle:** `sys.rt.invocations`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.InvocationRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.InvocationRef` |
| `id` | `sys.InvocationId` |
| `function` | `sys.FunctionRef` |
| `snapshot` | `sys.SnapshotRef` |
| `parent` | `sys.InvocationRef?` |
| `run` | `sys.RunRef?` |
| `owner_session` | `sys.SessionRef?` |
| `transaction` | `sys.TransactionRef?` |
| `arguments` | `Relation<sys.InvocationArgument>` |
| `started` | `Instant?` |
| `ended` | `Instant?` |
| `status` | `sys.InvocationStatus` |
| `result_type` | `sys.TypeRef?` |
| `failure` | `sys.Diagnostic?` |
| `trace` | `sys.TraceRef?` |
| `idempotency_key_hash` | `Digest?` |

Invariant: exactly one launch owner: parent invocation or owner_session, except a top-level command run.

Invariant: ordinary function calls do not detach ownership.

Invariant: terminal state is published only after all children terminate and resources are released.

### `sys.InvocationArgument` {#api-type-sys-invocationargument}

Redaction-safe invocation argument metadata.

**Grouped handle:** `sys.rt.invocation_arguments`. **Availability:** durable-observation. **Natural key:** `invocation + position`. **Reference:** `sys.InvocationArgumentRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.InvocationArgumentRef` |
| `invocation` | `sys.InvocationRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `value` | `sys.Value?` |
| `digest` | `Digest?` |
| `redacted` | `Bool` |

### `sys.Run` {#api-type-sys-run}

Top-level durable program-run observation.

**Grouped handle:** `sys.rt.runs`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.RunRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.RunRef` |
| `id` | `sys.RunId` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `function` | `sys.FunctionRef` |
| `source_identity` | `Str?` |
| `snapshot` | `sys.SnapshotRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status_at_snapshot` | `sys.RunStatus` |
| `observed_at` | `Instant` |
| `runtime_id` | `sys.RuntimeId?` |
| `live` | `Bool` |
| `invocation` | `sys.InvocationRef` |
| `checkpoint_count` | `Int` |
| `failure` | `sys.Diagnostic?` |

### `sys.Transaction` {#api-type-sys-transaction}

Orna transaction observation and atomic write/checkpoint set.

**Grouped handle:** `sys.rt.transactions`. **Availability:** local-durable. **Natural key:** `id`. **Reference:** `sys.TransactionRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.TransactionRef` |
| `id` | `sys.TransactionId` |
| `activation` | `sys.InvocationRef` |
| `snapshot` | `sys.SnapshotRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.TransactionStatus` |
| `writes` | `Relation<sys.Change>` |
| `checkpoint_updates` | `Relation<sys.CheckpointUpdate>` |
| `failure` | `sys.Diagnostic?` |
| `rolled_back` | `Bool` |

### `sys.Trace` {#api-type-sys-trace}

Bounded structured trace.

**Grouped handle:** `sys.rt.traces`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.TraceRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.TraceRef` |
| `id` | `sys.TraceId` |
| `root_invocation` | `sys.InvocationRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `sampled` | `Bool` |
| `spans` | `Relation<sys.Span>` |
| `detail_available` | `Bool` |

### `sys.Span` {#api-type-sys-span}

Nested trace span.

**Grouped handle:** `sys.rt.spans`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.SpanRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.SpanRef` |
| `id` | `sys.SpanId` |
| `trace` | `sys.TraceRef` |
| `parent` | `sys.SpanRef?` |
| `name` | `Str` |
| `definition` | `sys.DefinitionRef?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.InvocationStatus` |
| `attributes` | `sys.Value` |
| `events` | `Relation<sys.TraceEvent>` |

### `sys.TraceEvent` {#api-type-sys-traceevent}

Trace event with redaction-safe attributes.

**Grouped handle:** `sys.rt.events`. **Availability:** durable-observation. **Natural key:** `span + sequence`. **Reference:** `sys.TraceEventRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.TraceEventRef` |
| `span` | `sys.SpanRef` |
| `sequence` | `Int` |
| `time` | `Instant` |
| `name` | `Str` |
| `attributes` | `sys.Value` |
| `diagnostic` | `sys.Diagnostic?` |

### `sys.Stream` {#api-type-sys-stream}

Finite/unbounded stream execution observation.

**Grouped handle:** `sys.rt.streams`. **Availability:** durable-observation. **Natural key:** `run + source_identity + partition`. **Reference:** `sys.StreamRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.StreamRef` |
| `run` | `sys.RunRef` |
| `producer` | `sys.ObjectRef` |
| `consumer` | `sys.FunctionRef?` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `status_at_snapshot` | `sys.StreamStatus` |
| `items_seen` | `Int` |
| `items_committed` | `Int` |
| `items_failed` | `Int` |
| `checkpoint` | `sys.CheckpointRef?` |
| `last_item_at` | `Instant?` |
| `last_failure` | `sys.FailureRef?` |
| `last_diagnostic` | `sys.Diagnostic?` |
| `observed_at` | `Instant` |
| `live` | `Bool` |

### `sys.Checkpoint` {#api-type-sys-checkpoint}

Durable resumable consumer progress.

**Grouped handle:** `sys.rt.checkpoints`. **Availability:** local-durable. **Natural key:** `consumer_identity + source_identity + partition`. **Reference:** `sys.CheckpointRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.CheckpointRef` |
| `consumer` | `sys.FunctionRef` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `position` | `sys.CheckpointPosition` |
| `position_format` | `Str` |
| `version` | `sys.CheckpointVersion` |
| `replayable` | `Bool` |
| `updated` | `Instant` |
| `published_snapshot` | `sys.SnapshotRef?` |
| `last_transaction` | `sys.TransactionRef` |

### `sys.CheckpointUpdate` {#api-type-sys-checkpointupdate}

Atomic checkpoint compare-and-set update.

**Grouped handle:** `sys.rt.checkpoint_updates`. **Availability:** local-durable. **Natural key:** `transaction + checkpoint`. **Reference:** `sys.CheckpointUpdateRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.CheckpointUpdateRef` |
| `transaction` | `sys.TransactionRef` |
| `checkpoint` | `sys.CheckpointRef` |
| `expected_version` | `sys.CheckpointVersion` |
| `before` | `sys.CheckpointPosition?` |
| `after` | `sys.CheckpointPosition` |
| `committed` | `Bool` |

### `sys.Failure` {#api-type-sys-failure}

One durable blocked or preserved delivery; repeated attempts update this record.

**Grouped handle:** `sys.rt.failures`. **Availability:** local-durable. **Natural key:** `consumer_identity + source_identity + partition + position_format + position`. **Reference:** `sys.FailureRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.FailureRef` |
| `consumer` | `sys.FunctionRef` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `position_format` | `Str` |
| `position` | `sys.CheckpointPosition` |
| `position_digest` | `Digest` |
| `version` | `sys.FailureVersion` |
| `attempt_count` | `Int` |
| `last_attempt_at` | `Instant?` |
| `payload` | `Blob?` |
| `payload_reference` | `Str?` |
| `payload_digest` | `Digest?` |
| `error` | `sys.Diagnostic` |
| `created` | `Instant` |
| `status` | `sys.FailureStatus` |
| `status_changed` | `Instant` |
| `reason` | `Str?` |
| `replayed_at` | `Instant?` |
| `replacement_invocation` | `sys.InvocationRef?` |
| `checkpoint_before` | `sys.CheckpointPosition` |
| `checkpoint_after` | `sys.CheckpointPosition?` |
| `checkpoint_version_before` | `sys.CheckpointVersion` |

Invariant: attempt_count starts at one after the first failure and is monotonic.

Invariant: version changes on every committed state transition.

Invariant: position digest accelerates lookup but does not substitute for canonical position equality.

Invariant: retry failure updates this record without creating a successor record.

Invariant: only one retry or replay may hold this delivery lease.

Invariant: payload is redacted unless the explicit privileged payload-access boundary permits disclosure.

### `sys.Session` {#api-type-sys-session}

Interactive/page session.

**Grouped handle:** `sys.rt.sessions`. **Availability:** live. **Natural key:** `id`. **Reference:** `sys.SessionRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.SessionRef` |
| `id` | `sys.SessionId` |
| `started` | `Instant` |
| `last_seen` | `Instant` |
| `client` | `sys.ClientRef?` |
| `locale` | `Locale` |
| `timezone` | `TimeZone` |
| `renderer` | `Str?` |

### `sys.Client` {#api-type-sys-client}

Attached CLI/REPL/server/renderer client.

**Grouped handle:** `sys.rt.clients`. **Availability:** live. **Natural key:** `id`. **Reference:** `sys.ClientRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.ClientRef` |
| `id` | `sys.ClientId` |
| `kind` | `sys.ClientKind` |
| `connected` | `Instant` |
| `last_seen` | `Instant` |
| `protocol_version` | `Str?` |
| `remote` | `Str?` |
| `redacted` | `Bool` |

### `sys.Lease` {#api-type-sys-lease}

Local owner/consumer coordination lease.

**Grouped handle:** `sys.rt.leases`. **Availability:** local-durable. **Natural key:** `name`. **Reference:** `sys.LeaseRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.LeaseRef` |
| `name` | `Str` |
| `holder` | `sys.RuntimeId` |
| `status` | `sys.LeaseStatus` |
| `acquired` | `Instant` |
| `expires` | `Instant?` |
| `generation` | `Int` |

### `sys.Listener` {#api-type-sys-listener}

Optional network listener owned by the current runtime.

**Grouped handle:** `sys.rt.listeners`. **Availability:** live. **Natural key:** `id`. **Reference:** `sys.ListenerRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.ListenerRef` |
| `id` | `Str` |
| `kind` | `sys.ListenerKind` |
| `address` | `Str` |
| `started` | `Instant` |
| `clients` | `Int` |
| `tls` | `Bool` |
| `authenticated` | `Bool` |

### `sys.Storage` {#api-type-sys-storage}

Logical object's physical storage status.

**Grouped handle:** `sys.storage.objects`. **Availability:** local-durable. **Natural key:** `object`. **Reference:** `sys.StorageRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StorageRef` |
| `object` | `sys.ObjectRef` |
| `profile` | `sys.StorageProfile` |
| `preference` | `sys.StoragePreference` |
| `rows` | `Int?` |
| `rows_exact` | `Bool` |
| `bytes` | `Int?` |
| `pending_rows` | `Int` |
| `pending_bytes` | `Int` |
| `compression` | `Str?` |
| `logical_generation` | `Int` |
| `last_publication` | `Instant?` |
| `last_snapshot` | `sys.SnapshotRef?` |
| `location` | `Path?` |
| `publication_policy` | `sys.Value` |
| `verification` | `sys.VerificationStatus` |

### `sys.Segment` {#api-type-sys-segment}

Immutable compact table segment.

**Grouped handle:** `sys.storage.segments`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.SegmentRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SegmentRef` |
| `id` | `sys.SegmentId` |
| `table` | `sys.TableRef` |
| `git_object` | `sys.GitObjectRef` |
| `content_digest` | `Digest` |
| `schema_hash` | `Digest` |
| `min_key` | `sys.Value` |
| `max_key` | `sys.Value` |
| `rows` | `Int` |
| `bytes` | `Int` |
| `statistics` | `Relation<sys.Statistic>` |
| `available_locally` | `Bool` |
| `promisor_remote` | `sys.RemoteRef?` |
| `generation` | `Int` |

### `sys.Statistic` {#api-type-sys-statistic}

Segment statistics used for safe planning/pruning.

**Grouped handle:** `sys.storage.statistics`. **Availability:** catalogue. **Natural key:** `segment + column + kind`. **Reference:** `sys.StatisticRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StatisticRef` |
| `segment` | `sys.SegmentRef` |
| `column` | `sys.ColumnRef` |
| `kind` | `sys.StatisticKind` |
| `value` | `sys.Value?` |
| `exact` | `Bool` |
| `valid` | `Bool` |

### `sys.Index` {#api-type-sys-index}

Logical/physical index metadata.

**Grouped handle:** `sys.storage.indexes`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.IndexRef`.

| Field | Type |
|---|---|
| `reference` | `sys.IndexRef` |
| `object` | `sys.ObjectRef` |
| `table` | `sys.TableRef` |
| `columns` | `Relation<sys.Column>` |
| `kind` | `sys.IndexKind` |
| `unique` | `Bool` |
| `materialized` | `Bool` |
| `valid` | `Bool` |

### `sys.Materialization` {#api-type-sys-materialization}

Cached/materialized function/query result.

**Grouped handle:** `sys.storage.materializations`. **Availability:** local-durable. **Natural key:** `object + snapshot`. **Reference:** `sys.MaterializationRef`.

| Field | Type |
|---|---|
| `reference` | `sys.MaterializationRef` |
| `object` | `sys.ObjectRef` |
| `snapshot` | `sys.SnapshotRef` |
| `digest` | `Digest` |
| `rows` | `Int?` |
| `bytes` | `Int?` |
| `created` | `Instant` |
| `valid` | `Bool` |

### `sys.Allocator` {#api-type-sys-allocator}

Automatic-key allocator state.

**Grouped handle:** `sys.storage.allocators`. **Availability:** local-durable. **Natural key:** `table`. **Reference:** `sys.AllocatorRef`.

| Field | Type |
|---|---|
| `reference` | `sys.AllocatorRef` |
| `table` | `sys.TableRef` |
| `strategy` | `Str` |
| `state` | `sys.Value` |
| `generation` | `Int` |
| `updated` | `Instant` |

### `sys.Hydration` {#api-type-sys-hydration}

Lazy-object hydration progress.

**Grouped handle:** `sys.storage.hydrations`. **Availability:** live. **Natural key:** `object`. **Reference:** `sys.HydrationRef`.

| Field | Type |
|---|---|
| `reference` | `sys.HydrationRef` |
| `object` | `sys.GitObjectRef` |
| `remote` | `sys.RemoteRef?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `bytes` | `Int?` |
| `status` | `sys.BuildStatus` |
| `failure` | `sys.Diagnostic?` |

### `sys.Compaction` {#api-type-sys-compaction}

Atomic compact-segment rewrite.

**Grouped handle:** `sys.storage.compactions`. **Availability:** local-durable. **Natural key:** `id`. **Reference:** `sys.CompactionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.CompactionRef` |
| `id` | `Str` |
| `table` | `sys.TableRef` |
| `input_segments` | `Relation<sys.Segment>` |
| `output_segments` | `Relation<sys.Segment>` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.BuildStatus` |
| `transaction` | `sys.TransactionRef?` |
| `failure` | `sys.Diagnostic?` |

### `sys.MaintenanceJob` {#api-type-sys-maintenancejob}

Verification/prune/flush/maintenance operation.

**Grouped handle:** `sys.storage.maintenance`. **Availability:** local-durable. **Natural key:** `id`. **Reference:** `sys.MaintenanceJobRef`.

| Field | Type |
|---|---|
| `reference` | `sys.MaintenanceJobRef` |
| `id` | `Str` |
| `kind` | `sys.MaintenanceKind` |
| `object` | `sys.ObjectRef?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.BuildStatus` |
| `progress` | `Decimal?` |
| `failure` | `sys.Diagnostic?` |

### `sys.StorageFile` {#api-type-sys-storagefile}

Local runtime/storage file projection.

**Grouped handle:** `sys.storage.files`. **Availability:** local-durable. **Natural key:** `path`. **Reference:** `sys.StorageFileRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StorageFileRef` |
| `path` | `Path` |
| `kind` | `sys.StorageFileKind` |
| `bytes` | `Int` |
| `digest` | `Digest?` |
| `required` | `Bool` |
| `available` | `Bool` |
| `redacted` | `Bool` |

### `sys.Secret` {#api-type-sys-secret}

Secret availability metadata without plaintext.

**Grouped handle:** `sys.catalog.secrets`. **Availability:** local-durable. **Natural key:** `name`. **Reference:** `sys.SecretRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SecretRef` |
| `name` | `Str` |
| `available` | `Bool` |
| `provider` | `Str` |
| `encrypted_file` | `sys.FileRef?` |
| `recipients` | `[Str]` |
| `required_by` | `Relation<sys.Object>` |
| `last_error` | `sys.Diagnostic?` |

### `sys.Setting` {#api-type-sys-setting}

Effective configuration with explicit redaction.

**Grouped handle:** `sys.catalog.settings`. **Availability:** local-durable. **Natural key:** `name + scope`. **Reference:** `sys.SettingRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SettingRef` |
| `name` | `Str` |
| `value` | `sys.Value?` |
| `redacted` | `Bool` |
| `source` | `sys.SettingSource` |
| `scope` | `sys.SettingScope` |
| `effective_at` | `sys.SnapshotRef?` |

### `sys.Build` {#api-type-sys-build}

Reproducible build observation.

**Grouped handle:** `sys.build.builds`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.BuildRef`.

| Field | Type |
|---|---|
| `reference` | `sys.BuildRef` |
| `id` | `sys.BuildId` |
| `snapshot` | `sys.SnapshotRef` |
| `compiler` | `Str` |
| `compatibility` | `sys.CompatibilityInfo` |
| `package_lock_digest` | `Digest?` |
| `artifact_digest` | `Digest?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.BuildStatus` |
| `diagnostics` | `Relation<sys.Diagnostic>` |

### `sys.Test` {#api-type-sys-test}

Test execution observation.

**Grouped handle:** `sys.build.tests`. **Availability:** durable-observation. **Natural key:** `build + name`. **Reference:** `sys.TestRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TestRef` |
| `build` | `sys.BuildRef` |
| `name` | `Str` |
| `object` | `sys.ObjectRef?` |
| `status` | `sys.TestStatus` |
| `started` | `Instant?` |
| `ended` | `Instant?` |
| `duration` | `Duration?` |
| `diagnostic` | `sys.Diagnostic?` |

## Callable operations

Read operations do not mutate database state. Invocation/start operations follow [INVOKE-1](#system); admin operations follow [administrative transitions](#administration). Resolution and retained-data failures are ordinary typed failures, not missing/empty successful results.

### `sys.meta` {#api-call-sys-meta}

Return safe static/nominal/codec/protocol metadata for a value.

```text
fn sys.meta<T>(value: T): sys.ValueMetadata<T>
```

**Effect:** read.

### `sys.object` {#api-call-sys-object}

Look up a stable object at a snapshot.

```text
fn sys.object(id: sys.ObjectId, at: sys.SnapshotRef = sys.current.snapshot): sys.ObjectRef
```

**Effect:** read.

### `sys.resolve` {#api-call-sys-resolve}

Resolve a semantic name with ordinary visibility/import rules.

```text
fn sys.resolve(name: Str, kind: sys.ObjectKind? = null, at: sys.SnapshotRef = sys.current.snapshot, from: sys.ModuleRef? = null): sys.ObjectRef
```

**Effect:** read.

### `sys.resolve_function` {#api-call-sys-resolve-function}

Resolve a function reference.

```text
fn sys.resolve_function(name: Str, at: sys.SnapshotRef = sys.current.snapshot, from: sys.ModuleRef? = null): sys.FunctionRef
```

**Effect:** read.

### `sys.describe` {#api-call-sys-describe}

Return structured object description.

```text
fn sys.describe(object: sys.ObjectRef): sys.ObjectDescription
```

**Effect:** read.

### `sys.source(ObjectRef)` {#api-call-sys-source-objectref}

Return retained exact source/source maps for an object subject to redaction.

```text
fn sys.source(object: sys.ObjectRef): sys.SourceDocument
```

**Effect:** read.

### `sys.source(FileRef)` {#api-call-sys-source-fileref}

Return retained exact source/source maps for a file subject to redaction.

```text
fn sys.source(file: sys.FileRef): sys.SourceDocument
```

**Effect:** read.

### `sys.history(ObjectRef)` {#api-call-sys-history-objectref}

Return stable semantic-object revision history.

```text
fn sys.history(object: sys.ObjectRef): Relation<sys.Revision>
```

**Effect:** read.

### `sys.history(FileRef)` {#api-call-sys-history-fileref}

Return retained versions of one repository file.

```text
fn sys.history(file: sys.FileRef): Relation<sys.FileVersion>
```

**Effect:** read.

### `sys.blame(FileRef)` {#api-call-sys-blame-fileref}

Return source attribution.

```text
fn sys.blame(target: sys.FileRef): Relation<sys.Attribution>
```

**Effect:** read.

### `sys.blame(RowRef)` {#api-call-sys-blame-rowref}

Return semantic row/field attribution.

```text
fn sys.blame<T>(target: sys.RowRef<T>): Relation<sys.Attribution>
```

**Effect:** read.

### `sys.dependencies` {#api-call-sys-dependencies}

Traverse outgoing dependency edges.

```text
fn sys.dependencies(object: sys.ObjectRef, transitive: Bool = false, kinds: [sys.DependencyKind]? = null): Relation<sys.Dependency>
```

**Effect:** read.

### `sys.dependents` {#api-call-sys-dependents}

Traverse incoming dependency edges.

```text
fn sys.dependents(object: sys.ObjectRef, transitive: Bool = false, kinds: [sys.DependencyKind]? = null): Relation<sys.Dependency>
```

**Effect:** read.

### `sys.snapshot(SnapshotRef)` {#api-call-sys-snapshot-snapshotref}

Return an already resolved snapshot reference.

```text
fn sys.snapshot(reference: sys.SnapshotRef = sys.database.cwd): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(CommitRef)` {#api-call-sys-snapshot-commitref}

Resolve a commit snapshot.

```text
fn sys.snapshot(reference: sys.CommitRef): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(BranchRef)` {#api-call-sys-snapshot-branchref}

Resolve the branch target observed by the reference.

```text
fn sys.snapshot(reference: sys.BranchRef): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(TagRef)` {#api-call-sys-snapshot-tagref}

Resolve the peeled tag target.

```text
fn sys.snapshot(reference: sys.TagRef): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(GitOid)` {#api-call-sys-snapshot-gitoid}

Resolve an exact Git object identifier.

```text
fn sys.snapshot(reference: sys.GitOid): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(Str)` {#api-call-sys-snapshot-str}

Resolve a Git revision expression in the attached repository.

```text
fn sys.snapshot(reference: Str): sys.SnapshotRef
```

**Effect:** read.

### `sys.diff` {#api-call-sys-diff}

Compute semantic/source/row diff.

```text
fn sys.diff(from: sys.SnapshotRef, to: sys.SnapshotRef, scope: sys.DiffScope = sys.DiffScope.all): Relation<sys.DiffEntry>
```

**Effect:** read.

### `sys.changes` {#api-call-sys-changes}

Report changes represented by/relative to a state.

```text
fn sys.changes(snapshot: sys.SnapshotRef = sys.database.cwd, area: sys.ChangeArea? = null): Relation<sys.Change>
```

**Effect:** read.

### `sys.explain(Query)` {#api-call-sys-explain-query}

Return structured query plan.

```text
fn sys.explain<T>(query: Query<T>): sys.Plan
```

**Effect:** read.

### `sys.explain(FunctionRef)` {#api-call-sys-explain-functionref}

Return structured function/effect plan.

```text
fn sys.explain(function: sys.FunctionRef): sys.Plan
```

**Effect:** read.

### `sys.explain(Diagnostic)` {#api-call-sys-explain-diagnostic}

Return structured causal explanation.

```text
fn sys.explain(diagnostic: sys.Diagnostic): sys.Explanation
```

**Effect:** read.

### `sys.checkpoint` {#api-call-sys-checkpoint}

Read a checkpoint snapshot.

```text
fn sys.checkpoint(consumer_identity: sys.ConsumerIdentity, source_identity: Str, partition: Str? = null): sys.Checkpoint?
```

**Effect:** read.

### `sys.render_diagnostic` {#api-call-sys-render-diagnostic}

Render without altering structured diagnostic.

```text
fn sys.render_diagnostic(diagnostic: sys.Diagnostic, context: PresentContext = sys.current.present_context): PresentTree
```

**Effect:** read.

### `sys.rt.info` {#api-call-sys-rt-info}

Return exact language/sys/storage/protocol compatibility coordinates.

```text
fn sys.rt.info(): sys.RuntimeInfo
```

**Effect:** read.

### `sys.invoke(Value)` {#api-call-sys-invoke-value}

Reflectively invoke and return an explicitly erased value envelope.

```text
fn sys.invoke(function: sys.FunctionRef, arguments: sys.ArgumentMap, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit, idempotency_key: Str? = null): sys.Value
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.invoke<T>` {#api-call-sys-invoke-t}

Reflectively invoke after validating the declared result against an explicit type witness.

```text
fn sys.invoke<T>(function: sys.FunctionRef, arguments: sys.ArgumentMap, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit, idempotency_key: Str? = null): T
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.start(Value)` {#api-call-sys-start-value}

Start an awaitable invocation with an explicitly erased result.

```text
fn sys.start(function: sys.FunctionRef, arguments: sys.ArgumentMap, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate, idempotency_key: Str? = null): sys.InvocationHandle<sys.Value>
```

**Effect:** invoke.

Ownership: Current operation owns the child, except a direct REPL sys.start expression/binding is session-owned. Inherit transaction mode is rejected; separate/read_only are permitted.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.start<T>` {#api-call-sys-start-t}

Start an awaitable invocation after validating an explicit result type witness.

```text
fn sys.start<T>(function: sys.FunctionRef, arguments: sys.ArgumentMap, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate, idempotency_key: Str? = null): sys.InvocationHandle<T>
```

**Effect:** invoke.

Ownership: Current operation owns the child, except a direct REPL sys.start expression/binding is session-owned. Inherit transaction mode is rejected; separate/read_only are permitted.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.await` {#api-call-sys-await}

Wait for a terminal result; timeout does not cancel.

```text
fn sys.await<T>(invocation: sys.InvocationHandle<T>, timeout: Duration? = null): sys.InvocationResult<T>
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.cancel` {#api-call-sys-cancel}

Idempotently request invocation cancellation.

```text
fn sys.cancel<T>(invocation: sys.InvocationHandle<T>, reason: Str? = null): Bool
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.admin.commit` {#api-call-sys-admin-commit}

Commit the validated staged state, preserving unstaged CWD changes and unpublished tail.

```text
fn sys.admin.commit(message: Str, author: sys.PersonIdentity? = null): sys.CommitRef
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(SnapshotRef)` {#api-call-sys-admin-checkout-snapshotref}

Validate and select an already resolved snapshot.

```text
fn sys.admin.checkout(target: sys.SnapshotRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(CommitRef)` {#api-call-sys-admin-checkout-commitref}

Validate and select a commit snapshot.

```text
fn sys.admin.checkout(target: sys.CommitRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(BranchRef)` {#api-call-sys-admin-checkout-branchref}

Validate and select a branch target.

```text
fn sys.admin.checkout(target: sys.BranchRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(TagRef)` {#api-call-sys-admin-checkout-tagref}

Validate and select a peeled tag target.

```text
fn sys.admin.checkout(target: sys.TagRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(GitOid)` {#api-call-sys-admin-checkout-gitoid}

Validate and select an exact Git object.

```text
fn sys.admin.checkout(target: sys.GitOid, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(Str)` {#api-call-sys-admin-checkout-str}

Resolve, validate and select a Git revision expression.

```text
fn sys.admin.checkout(target: Str, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(SnapshotRef)` {#api-call-sys-admin-create-branch-snapshotref}

Create a branch at the supplied committed snapshot or current HEAD; do not switch or commit pending changes.

```text
fn sys.admin.create_branch(name: Str, at: sys.SnapshotRef? = null): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(CommitRef)` {#api-call-sys-admin-create-branch-commitref}

Create a Git branch at a commit.

```text
fn sys.admin.create_branch(name: Str, at: sys.CommitRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(BranchRef)` {#api-call-sys-admin-create-branch-branchref}

Create a Git branch at another branch target.

```text
fn sys.admin.create_branch(name: Str, at: sys.BranchRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(TagRef)` {#api-call-sys-admin-create-branch-tagref}

Create a Git branch at a peeled tag target.

```text
fn sys.admin.create_branch(name: Str, at: sys.TagRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(GitOid)` {#api-call-sys-admin-create-branch-gitoid}

Create a Git branch at an exact Git object.

```text
fn sys.admin.create_branch(name: Str, at: sys.GitOid): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(Str)` {#api-call-sys-admin-create-branch-str}

Resolve a Git revision expression and create a branch there.

```text
fn sys.admin.create_branch(name: Str, at: Str): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.flush` {#api-call-sys-admin-flush}

Durably seal pending rows without changing logical contents.

```text
fn sys.admin.flush(table: sys.TableRef? = null): sys.FlushResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.compact` {#api-call-sys-admin-compact}

Rewrite physical segments atomically.

```text
fn sys.admin.compact(table: sys.TableRef? = null): sys.CompactionResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.set_storage_preference` {#api-call-sys-admin-set-storage-preference}

Set future automatic placement preference without rewriting existing rows.

```text
fn sys.admin.set_storage_preference(table: sys.TableRef, preference: sys.StoragePreference): sys.Storage
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.rewrite_storage` {#api-call-sys-admin-rewrite-storage}

Atomically rewrite physical placement while preserving logical rows.

```text
fn sys.admin.rewrite_storage(table: sys.TableRef, to: sys.StorageRewriteTarget): sys.StorageRewriteResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.verify` {#api-call-sys-admin-verify}

Verify repository, metadata, storage and checkpoint invariants.

```text
fn sys.admin.verify(scope: sys.VerifyScope = sys.VerifyScope.database): sys.VerificationReport
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.cancel_run` {#api-call-sys-admin-cancel-run}

Cancel a durable program run.

```text
fn sys.admin.cancel_run(run: sys.RunRef, reason: Str? = null): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.pause_stream` {#api-call-sys-admin-pause-stream}

Pause at an item/batch transaction boundary.

```text
fn sys.admin.pause_stream(stream: sys.StreamRef, reason: Str? = null): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.resume_stream` {#api-call-sys-admin-resume-stream}

Resume a paused stream.

```text
fn sys.admin.resume_stream(stream: sys.StreamRef): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.reset_checkpoint` {#api-call-sys-admin-reset-checkpoint}

Compare-and-set checkpoint reset.

```text
fn sys.admin.reset_checkpoint(checkpoint: sys.CheckpointRef, expected_version: sys.CheckpointVersion, expected_position: sys.CheckpointPosition, to: sys.CheckpointPosition, reason: Str): sys.Checkpoint
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.retry_failure` {#api-call-sys-admin-retry-failure}

Lease and retry the same blocked delivery; keep identity stable across failed attempts.

```text
fn sys.admin.retry_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus = sys.FailureStatus.open): sys.InvocationHandle<sys.Value>
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.skip_failure` {#api-call-sys-admin-skip-failure}

Atomically skip a supported failed position and move progress.

```text
fn sys.admin.skip_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus, reason: Str): sys.Checkpoint
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.replay_failure` {#api-call-sys-admin-replay-failure}

Reprocess a preserved skipped delivery without rewinding the live checkpoint.

```text
fn sys.admin.replay_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus = sys.FailureStatus.skipped): sys.InvocationHandle<sys.Value>
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.resolve_failure` {#api-call-sys-admin-resolve-failure}

Resolve a preserved failure without marking its processing as successful.

```text
fn sys.admin.resolve_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus, reason: Str): sys.Failure
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.consumer_identity` {#api-call-sys-consumer-identity}

Derive durable consumer identity from database identity, stable function ObjectId and canonical typed arguments. Reject secret plaintext and unsupported argument values.

```text
fn sys.consumer_identity(function: sys.FunctionRef, arguments: sys.ArgumentMap): sys.ConsumerIdentity
```

**Effect:** read.

### `sys.admin.plan_checkout(SnapshotRef)` {#api-call-sys-admin-plan-checkout-snapshotref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.SnapshotRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(CommitRef)` {#api-call-sys-admin-plan-checkout-commitref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.CommitRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(BranchRef)` {#api-call-sys-admin-plan-checkout-branchref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.BranchRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(TagRef)` {#api-call-sys-admin-plan-checkout-tagref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.TagRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(GitOid)` {#api-call-sys-admin-plan-checkout-gitoid}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.GitOid): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(Str)` {#api-call-sys-admin-plan-checkout-str}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: Str): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

## Portable failure codes

A failure code is stable machine-readable classification. The message may be localised; the code must not be replaced by vendor-only wording. Unless a more specific transition is stated, failure before admission performs no requested mutation.

| Code | Condition |
|---|---|
| `sys.version.incompatible` | Required API/profile coordinate is unavailable. |
| `sys.context.repl_unavailable` | The operation requires a REPL context that is absent. |
| `sys.object.not_found` | No object matches the exact accessible selector. |
| `sys.object.ambiguous` | The selector matches more than one allowed object. |
| `sys.object.wrong_kind` | The resolved object is not the requested kind. |
| `sys.snapshot.not_found` | The snapshot selector cannot be resolved. |
| `sys.snapshot.incomplete` | Required snapshot objects are absent or unavailable. |
| `sys.source.unavailable` | Required source bytes or maps are not retained/available. |
| `sys.source.redacted` | The requested source content is protected. |
| `sys.handle.foreign_runtime` | A live handle belongs to another runtime generation. |
| `sys.handle.expired` | A same-runtime handle no longer names a live retained operation. |
| `sys.invoke.argument_missing` | A required argument has no supplied value or default. |
| `sys.invoke.argument_unknown` | An argument name is not declared by the target. |
| `sys.invoke.argument_type` | An argument has an incompatible exact type or invalid duplicate binding. |
| `sys.invoke.return_type` | The result witness disagrees with the declared result before effects. |
| `sys.invoke.effect_unavailable` | The selected context does not permit a target effect. |
| `sys.invoke.idempotency_mismatch` | The key is already bound to a different complete invocation identity. |
| `sys.invoke.await_timeout` | Waiting expired; the target was not cancelled by the wait. |
| `sys.invoke.not_callable` | The resolved object is not an invocable function. |
| `sys.invoke.revision_unavailable` | The pinned target revision cannot be loaded. |
| `sys.checkpoint.not_found` | A required checkpoint reference does not resolve. The optional lookup function instead returns null for known absence. |
| `sys.checkpoint.conflict` | The checkpoint version/position or active lease differs from its expected state. |
| `sys.checkpoint.not_replayable` | The provider cannot resume/reset to the requested position. |
| `sys.failure.state_conflict` | The requested failure transition is not allowed from this state. |
| `sys.failure.skip_unsupported` | The provider cannot safely advance past this delivery. |
| `sys.admin.read_only` | The attachment cannot perform the requested administrative mutation. |
| `sys.admin.busy` | An incompatible owner/lease or reentrant write activation prevents admission. |
| `sys.admin.assertion_failed` | The candidate state violates a required assertion. |
| `sys.admin.missing_object` | Required objects cannot be obtained before admission. |
| `sys.admin.verification_failed` | Verification could not complete; no clean result is asserted. |
| `sys.storage.corrupt` | Physical bytes, schema, manifest or digest contradict the profile. |
| `sys.storage.unavailable` | Required physical data cannot be obtained. |
| `sys.storage.unrepresentable_key` | An editable rewrite cannot encode a key/path or meet the declared bounds. |
| `sys.storage.rewrite_conflict` | Inputs changed, collide or became stale before rewrite admission. |
| `sys.redaction.denied` | A generic operation attempted to reveal protected content. |
| `sys.invoke.snapshot_mismatch` | An explicit snapshot conflicts with the function reference or inherited transaction pin. |
| `sys.invoke.transaction_mode` | The requested transaction mode is invalid, including asynchronous inheritance. |
| `sys.git.unborn_head` | No default committed HEAD exists. |
| `sys.git.uncommitted_snapshot` | A branch/checkout target does not identify a commit. |
| `sys.git.dirty_conflict` | Local staged, unstaged or pending changes would be overwritten. |
| `sys.git.stale_plan` | The checkout preview no longer matches current state. |
| `sys.failure.stale_version` | The failure changed after the caller observed its version. |
| `sys.snapshot.expired` | A CWD pin is no longer retained and cannot be rebound. |
| `sys.git.invalid_ref` | The proposed local branch name is invalid under the adopted Git ref rules. |
| `sys.git.branch_exists` | Branch creation expected nonexistence but the branch is already present. |
| `sys.git.nothing_to_commit` | No staged change is present for this nonempty-commit operation. |
