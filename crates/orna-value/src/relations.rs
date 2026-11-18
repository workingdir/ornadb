//! The canonical `sys` relations and the identity of references to their rows.
//!
//! One [`RelationSpec`] per relation of `api/sys.json`: its name, its reference alias, the
//! physical relation identity that a reference carries as `table_id`, and the declared
//! natural key with the representation each key field has inside a reference (see
//! [`crate::relation_reference`]).
//!
//! The physical identities are versioned implementation compatibility constants: the
//! specification names the relations but assigns no sixteen-byte identity. The eleven
//! relations that had a reference constructor before this registry keep the identity and the
//! natural-key encoding of that constructor, so a stored reference reads back unchanged.
//! `crates/orna-sys/tests/relation_reference_registry.rs` proves the registry names exactly the
//! relations, aliases and key fields of the generated catalogue.

use crate::contract::{
    SYS_CHECKPOINT_TABLE_ID, SYS_FAILURE_TABLE_ID, SYS_FUNCTION_TABLE_ID,
    SYS_INVOCATION_ARGUMENT_TABLE_ID, SYS_INVOCATION_TABLE_ID, SYS_MATERIALIZATION_TABLE_ID,
    SYS_OBJECT_TABLE_ID, SYS_RUN_TABLE_ID, SYS_SNAPSHOT_TABLE_ID, SYS_STREAM_TABLE_ID,
    SYS_TYPE_TABLE_ID,
};
use crate::relation_reference::{KeyField, KeyPartKind::*, RelationSpec};

/// The physical identity written as canonical lowercase UUID text.
///
/// Evaluated at compile time, so a malformed constant fails the build.
const fn uuid(text: &str) -> [u8; 16] {
    let text = text.as_bytes();
    assert!(text.len() == 36, "a UUID is 36 characters");
    let mut bytes = [0u8; 16];
    let mut position = 0;
    let mut index = 0;
    while index < 16 {
        if position == 8 || position == 13 || position == 18 || position == 23 {
            assert!(
                text[position] == b'-',
                "a UUID has hyphens at 8, 13, 18 and 23"
            );
            position += 1;
        }
        bytes[index] = (nibble(text[position]) << 4) | nibble(text[position + 1]);
        position += 2;
        index += 1;
    }
    bytes
}

const fn nibble(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        _ => panic!("a UUID is lowercase hexadecimal"),
    }
}

macro_rules! relation {
    ($constant:ident, $name:literal, $table_id:expr, [$(($field:literal, $ty:literal, $kind:expr)),+ $(,)?]) => {
        #[doc = concat!("The `", $name, "` relation.")]
        pub const $constant: RelationSpec = RelationSpec {
            name: $name,
            reference_type: concat!($name, "Ref"),
            table_id: $table_id,
            key: &[$(KeyField { name: $field, ty: $ty, kind: $kind }),+],
        };
    };
}

relation!(
    DATABASE,
    "sys.Database",
    uuid("ba2bcfdd-c007-4b89-ba09-44f60c8efff5"),
    [("id", "sys.DatabaseId", Uuid)]
);
relation!(
    NAMESPACE,
    "sys.Namespace",
    uuid("d03b28fe-8f66-412b-970e-6ed0bd796a56"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    MODULE,
    "sys.Module",
    uuid("37accff2-3c5d-4ed0-83b8-9f7c771f8c7a"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    IMPORT,
    "sys.Import",
    uuid("596cd5f6-19f1-4f26-963c-70110ed6e88d"),
    [
        ("module", "sys.ModuleRef", Reference("sys.Module")),
        ("position", "Int", Int)
    ]
);
relation!(
    FILE,
    "sys.File",
    uuid("6b6eb386-cee2-450d-803a-e7576fa18a9e"),
    [("id", "sys.FileId", Uuid)]
);
relation!(
    OBJECT,
    "sys.Object",
    SYS_OBJECT_TABLE_ID,
    [
        ("id", "sys.ObjectId", Uuid),
        ("snapshot", "sys.SnapshotRef", Pin)
    ]
);
relation!(
    REVISION,
    "sys.Revision",
    uuid("a44adc77-fa58-4fc8-b398-6ca5ee2a63dd"),
    [("id", "sys.RevisionId", Digest)]
);
relation!(
    DEFINITION,
    "sys.Definition",
    uuid("80cf64d9-1811-4d31-a798-42a9c9dfc8e8"),
    [("id", "sys.DefinitionId", Uuid)]
);
relation!(
    TYPE,
    "sys.Type",
    SYS_TYPE_TABLE_ID,
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    TYPE_PARAMETER,
    "sys.TypeParameter",
    uuid("6649f00c-002c-4220-be7e-b7bfefc9bb49"),
    [
        ("owner", "sys.ObjectRef", Reference("sys.Object")),
        ("position", "Int", Int)
    ]
);
relation!(
    FIELD,
    "sys.Field",
    uuid("05aad3a8-8bca-450f-838f-17a4aa841080"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    VARIANT,
    "sys.Variant",
    uuid("f608a108-d1f1-433f-850a-ab5a7579560a"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    PROTOCOL,
    "sys.Protocol",
    uuid("72d1571d-be02-4e8f-927a-455e8c5bd3dd"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    PROTOCOL_MEMBER,
    "sys.ProtocolMember",
    uuid("4c1dc606-cafd-492b-9bb8-8667a18eb645"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    IMPLEMENTATION,
    "sys.Implementation",
    uuid("d2b2beac-536c-4439-8dbd-026dd31c2c1e"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    FUNCTION,
    "sys.Function",
    SYS_FUNCTION_TABLE_ID,
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    PARAMETER,
    "sys.Parameter",
    uuid("da00593b-0597-47da-b479-e7c71ba4cf3f"),
    [
        ("function", "sys.FunctionRef", Reference("sys.Function")),
        ("position", "Int", Int)
    ]
);
relation!(
    EFFECT,
    "sys.Effect",
    uuid("34bb9599-f504-49c5-8317-5aa8b94b2f3f"),
    [
        ("function", "sys.FunctionRef", Reference("sys.Function")),
        ("kind", "sys.EffectKind", Enum("sys.EffectKind")),
        ("target", "sys.ObjectRef?", OptionalReference("sys.Object"))
    ]
);
relation!(
    TABLE,
    "sys.Table",
    uuid("3ef4b8a0-cd58-49f6-a081-dbc6948fa81e"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    COLUMN,
    "sys.Column",
    uuid("3837fe7b-ec2d-4630-8dbd-7d2b18a2542e"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    KEY,
    "sys.Key",
    uuid("5c2e65d2-1cd7-43c0-8be3-32ef9d247d60"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    ASSERTION,
    "sys.Assertion",
    uuid("5bb3aac6-2a6b-4af5-a5c5-b0c5fabfa322"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    REFERENCE,
    "sys.Reference",
    uuid("ecc35140-ad21-4a0d-9d94-ce15c9581ac0"),
    [("from_column", "sys.ColumnRef", Reference("sys.Column"))]
);
relation!(
    PAGE,
    "sys.Page",
    uuid("5eed3b62-03c7-4314-ae12-cff7ac12a462"),
    [("function", "sys.FunctionRef", Reference("sys.Function"))]
);
relation!(
    DIMENSION,
    "sys.Dimension",
    uuid("11a1d943-e8f0-4d11-b8f7-483f1661d135"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    UNIT,
    "sys.Unit",
    uuid("e49fc8c9-e61b-42fd-abfe-1bfc51bd3bc4"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    CURRENCY,
    "sys.Currency",
    uuid("0c1b0c36-8109-45de-a582-0581606ab642"),
    [("type", "sys.TypeRef", Reference("sys.Type"))]
);
relation!(
    SECRET_REQUIREMENT,
    "sys.SecretRequirement",
    uuid("b7ec5ab4-ad8f-4c66-b3ee-1fdacc736822"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    EXTENSION,
    "sys.Extension",
    uuid("326ee38e-a34c-46a6-a5b8-bc9c7d308c48"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    DEPENDENCY,
    "sys.Dependency",
    uuid("7f6eff0b-3d07-4f92-9a47-5229f2384448"),
    [
        ("from", "sys.ObjectRef", Reference("sys.Object")),
        ("to", "sys.ObjectRef", Reference("sys.Object")),
        ("kind", "sys.DependencyKind", Enum("sys.DependencyKind")),
        ("span", "sys.SourceSpan?", OptionalCanonical)
    ]
);
relation!(
    DIAGNOSTIC,
    "sys.Diagnostic",
    uuid("85c1a337-b823-4b06-a8e5-5a95893c3e3c"),
    [("id", "Str", Text)]
);
relation!(
    SNAPSHOT,
    "sys.Snapshot",
    SYS_SNAPSHOT_TABLE_ID,
    [("id", "sys.SnapshotId", PinId)]
);
relation!(
    SNAPSHOT_OBJECT,
    "sys.SnapshotObject",
    uuid("f7fd5ec0-aeac-480e-8910-6a588a56a76e"),
    [
        ("snapshot", "sys.SnapshotRef", Reference("sys.Snapshot")),
        ("object", "sys.ObjectRef", Reference("sys.Object"))
    ]
);
relation!(
    CHANGE,
    "sys.Change",
    uuid("3fff68b6-537f-4269-848b-715b5de79007"),
    [
        ("snapshot", "sys.SnapshotRef", Reference("sys.Snapshot")),
        ("area", "sys.ChangeArea", Enum("sys.ChangeArea")),
        ("target", "sys.ChangeTarget", Canonical)
    ]
);
relation!(
    DIFF_ENTRY,
    "sys.DiffEntry",
    uuid("8cd76205-c86c-4b0f-8f4d-6499113b88e4"),
    [
        ("from", "sys.SnapshotRef", Reference("sys.Snapshot")),
        ("to", "sys.SnapshotRef", Reference("sys.Snapshot")),
        ("change.area", "sys.ChangeArea", Enum("sys.ChangeArea")),
        ("change.target", "sys.ChangeTarget", Canonical)
    ]
);
relation!(
    FILE_VERSION,
    "sys.FileVersion",
    uuid("51138dce-0ed4-424f-a9e9-8c40484cd606"),
    [
        ("file", "sys.FileRef", Reference("sys.File")),
        ("snapshot", "sys.SnapshotRef", Reference("sys.Snapshot"))
    ]
);
relation!(
    GIT_REPOSITORY,
    "sys.GitRepository",
    uuid("0d5c6dd3-0f07-485f-a769-97eed9afe6c9"),
    [("database", "sys.DatabaseRef", Reference("sys.Database"))]
);
relation!(
    COMMIT,
    "sys.Commit",
    uuid("9d7105d7-8b2d-4e98-b993-63c0a7a9b509"),
    [("oid", "sys.GitOid", GitOid)]
);
relation!(
    TREE_ENTRY,
    "sys.TreeEntry",
    uuid("3ef11bdf-c2ba-4c1d-b1b1-74d25a8716f7"),
    [
        ("tree", "sys.GitObjectRef", Reference("sys.GitObject")),
        ("path", "Path", Path)
    ]
);
relation!(
    REF,
    "sys.Ref",
    uuid("e97f380c-4aa9-43e8-b707-b6a71aec33f3"),
    [("name", "Str", Text)]
);
relation!(
    BRANCH,
    "sys.Branch",
    uuid("1f13000e-27d5-45ae-97cf-8dbcf3d4c732"),
    [("name", "Str", Text)]
);
relation!(
    TAG,
    "sys.Tag",
    uuid("eddb8d2e-0c41-449f-b382-b74c61a318b1"),
    [("name", "Str", Text)]
);
relation!(
    REMOTE,
    "sys.Remote",
    uuid("7865fa9d-4c92-4de6-8504-c0b79133eda4"),
    [("name", "Str", Text)]
);
relation!(
    STASH,
    "sys.Stash",
    uuid("d7281ea1-f4ef-41e0-95c9-61385d9bb192"),
    [("index", "Int", Int)]
);
relation!(
    GIT_OBJECT,
    "sys.GitObject",
    uuid("55180bfe-acc5-48de-8c72-f74713ecdb9b"),
    [("oid", "sys.GitOid", GitOid)]
);
relation!(
    WORKTREE_ENTRY,
    "sys.WorktreeEntry",
    uuid("6badcdb9-cda4-4ceb-97ed-10fd94eb6bd9"),
    [("path", "Path", Path)]
);
relation!(
    QUERY,
    "sys.Query",
    uuid("0eb771e9-1980-43b5-9e19-61bcfe470b61"),
    [("id", "sys.QueryId", Uuid)]
);
relation!(
    PLAN,
    "sys.Plan",
    uuid("ce3e6f79-bd6e-4253-8c98-46989864f6ac"),
    [("id", "Str", Text)]
);
relation!(
    PLAN_NODE,
    "sys.PlanNode",
    uuid("c652b40d-37f7-45d1-a906-3a88a8168c65"),
    [
        ("plan", "sys.PlanRef", Reference("sys.Plan")),
        ("position", "Int", Int)
    ]
);
relation!(
    INVOCATION,
    "sys.Invocation",
    SYS_INVOCATION_TABLE_ID,
    [("id", "sys.InvocationId", Uuid)]
);
relation!(
    INVOCATION_ARGUMENT,
    "sys.InvocationArgument",
    SYS_INVOCATION_ARGUMENT_TABLE_ID,
    [
        (
            "invocation",
            "sys.InvocationRef",
            Reference("sys.Invocation")
        ),
        ("position", "Int", Int)
    ]
);
relation!(
    RUN,
    "sys.Run",
    SYS_RUN_TABLE_ID,
    [("id", "sys.RunId", Uuid)]
);
relation!(
    TRANSACTION,
    "sys.Transaction",
    uuid("9e6f8386-274e-4ea3-8123-47924da46a17"),
    [("id", "sys.TransactionId", Uuid)]
);
relation!(
    TRACE,
    "sys.Trace",
    uuid("4025657d-8b6b-40b2-8157-bb5e2968e8a7"),
    [("id", "sys.TraceId", Uuid)]
);
relation!(
    SPAN,
    "sys.Span",
    uuid("6f3d6f8d-18e6-430d-b79c-bc6cdab67baa"),
    [("id", "sys.SpanId", Uuid)]
);
relation!(
    TRACE_EVENT,
    "sys.TraceEvent",
    uuid("9f6d1669-5b3f-44c5-ac39-723bf6eab977"),
    [
        ("span", "sys.SpanRef", Reference("sys.Span")),
        ("sequence", "Int", Int)
    ]
);
relation!(
    STREAM,
    "sys.Stream",
    SYS_STREAM_TABLE_ID,
    [
        ("run", "sys.RunRef", Reference("sys.Run")),
        ("source_identity", "Str", Text),
        ("partition", "Str?", OptionalText)
    ]
);
relation!(
    CHECKPOINT,
    "sys.Checkpoint",
    SYS_CHECKPOINT_TABLE_ID,
    [
        ("consumer_identity", "sys.ConsumerIdentity", Text),
        ("source_identity", "Str", Text),
        ("partition", "Str?", OptionalText)
    ]
);
relation!(
    CHECKPOINT_UPDATE,
    "sys.CheckpointUpdate",
    uuid("b29bc8cf-d08d-4eba-af6e-385a676786fe"),
    [
        (
            "transaction",
            "sys.TransactionRef",
            Reference("sys.Transaction")
        ),
        (
            "checkpoint",
            "sys.CheckpointRef",
            Reference("sys.Checkpoint")
        )
    ]
);
relation!(
    FAILURE,
    "sys.Failure",
    SYS_FAILURE_TABLE_ID,
    [
        ("consumer_identity", "sys.ConsumerIdentity", Text),
        ("source_identity", "Str", Text),
        ("partition", "Str?", OptionalText),
        ("position_format", "Str", Text),
        ("position", "sys.CheckpointPosition", Text)
    ]
);
relation!(
    SESSION,
    "sys.Session",
    uuid("455a846a-db91-4a99-8f76-eaedf1e7607f"),
    [("id", "sys.SessionId", Uuid)]
);
relation!(
    CLIENT,
    "sys.Client",
    uuid("ecd6aa7d-8125-4f8b-8332-1ddda1290048"),
    [("id", "sys.ClientId", Uuid)]
);
relation!(
    LEASE,
    "sys.Lease",
    uuid("53c3227c-0a01-43c5-a5c7-80786ad23b60"),
    [("name", "Str", Text)]
);
relation!(
    LISTENER,
    "sys.Listener",
    uuid("5cbe8aae-3ae2-4a44-bceb-42752b6e084e"),
    [("id", "Str", Text)]
);
relation!(
    STORAGE,
    "sys.Storage",
    uuid("c23a13f5-0c40-4f70-a7c6-ede178d70569"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    SEGMENT,
    "sys.Segment",
    uuid("ae2b0fd2-f1d9-4ea1-a8cc-47da7dc70333"),
    [("id", "sys.SegmentId", Uuid)]
);
relation!(
    STATISTIC,
    "sys.Statistic",
    uuid("2a0c97b9-708e-4946-8b04-24a7af450934"),
    [
        ("segment", "sys.SegmentRef", Reference("sys.Segment")),
        ("column", "sys.ColumnRef", Reference("sys.Column")),
        ("kind", "sys.StatisticKind", Enum("sys.StatisticKind"))
    ]
);
relation!(
    INDEX,
    "sys.Index",
    uuid("a83dab44-313a-4cce-9d72-5e242f54a64e"),
    [("object", "sys.ObjectRef", Reference("sys.Object"))]
);
relation!(
    MATERIALIZATION,
    "sys.Materialization",
    SYS_MATERIALIZATION_TABLE_ID,
    [
        ("object", "sys.ObjectRef", Digest),
        ("snapshot", "sys.SnapshotRef", Digest)
    ]
);
relation!(
    ALLOCATOR,
    "sys.Allocator",
    uuid("9d4f69d0-51e4-45ee-ad69-6af27b318ae0"),
    [("table", "sys.TableRef", Reference("sys.Table"))]
);
relation!(
    HYDRATION,
    "sys.Hydration",
    uuid("f232a047-6fc5-4052-a6bb-c96316d719b8"),
    [("object", "sys.GitObjectRef", Reference("sys.GitObject"))]
);
relation!(
    COMPACTION,
    "sys.Compaction",
    uuid("39e02e4e-ed93-49ce-8423-f4638582b858"),
    [("id", "Str", Text)]
);
relation!(
    MAINTENANCE_JOB,
    "sys.MaintenanceJob",
    uuid("929fb5ac-198f-4b21-b04c-d3704e8f0425"),
    [("id", "Str", Text)]
);
relation!(
    STORAGE_FILE,
    "sys.StorageFile",
    uuid("ac35afed-5362-4edb-a82c-eda51e925580"),
    [("path", "Path", Path)]
);
relation!(
    SECRET,
    "sys.Secret",
    uuid("59e54e50-5e19-47fa-a18d-bb9276f58c98"),
    [("name", "Str", Text)]
);
relation!(
    SETTING,
    "sys.Setting",
    uuid("f3a2c600-544b-4c3a-ac8e-2c7d5e6742bf"),
    [
        ("name", "Str", Text),
        ("scope", "sys.SettingScope", Enum("sys.SettingScope"))
    ]
);
relation!(
    BUILD,
    "sys.Build",
    uuid("70299d77-3c9a-44d5-83e8-924be2fd87fd"),
    [("id", "sys.BuildId", Uuid)]
);
relation!(
    TEST,
    "sys.Test",
    uuid("e10af795-1fbd-44fe-89ae-0a88b8b2581a"),
    [
        ("build", "sys.BuildRef", Reference("sys.Build")),
        ("name", "Str", Text)
    ]
);

/// Every canonical relation, in `api/sys.json` order.
pub static ALL: [&RelationSpec; 78] = [
    &DATABASE,
    &NAMESPACE,
    &MODULE,
    &IMPORT,
    &FILE,
    &OBJECT,
    &REVISION,
    &DEFINITION,
    &TYPE,
    &TYPE_PARAMETER,
    &FIELD,
    &VARIANT,
    &PROTOCOL,
    &PROTOCOL_MEMBER,
    &IMPLEMENTATION,
    &FUNCTION,
    &PARAMETER,
    &EFFECT,
    &TABLE,
    &COLUMN,
    &KEY,
    &ASSERTION,
    &REFERENCE,
    &PAGE,
    &DIMENSION,
    &UNIT,
    &CURRENCY,
    &SECRET_REQUIREMENT,
    &EXTENSION,
    &DEPENDENCY,
    &DIAGNOSTIC,
    &SNAPSHOT,
    &SNAPSHOT_OBJECT,
    &CHANGE,
    &DIFF_ENTRY,
    &FILE_VERSION,
    &GIT_REPOSITORY,
    &COMMIT,
    &TREE_ENTRY,
    &REF,
    &BRANCH,
    &TAG,
    &REMOTE,
    &STASH,
    &GIT_OBJECT,
    &WORKTREE_ENTRY,
    &QUERY,
    &PLAN,
    &PLAN_NODE,
    &INVOCATION,
    &INVOCATION_ARGUMENT,
    &RUN,
    &TRANSACTION,
    &TRACE,
    &SPAN,
    &TRACE_EVENT,
    &STREAM,
    &CHECKPOINT,
    &CHECKPOINT_UPDATE,
    &FAILURE,
    &SESSION,
    &CLIENT,
    &LEASE,
    &LISTENER,
    &STORAGE,
    &SEGMENT,
    &STATISTIC,
    &INDEX,
    &MATERIALIZATION,
    &ALLOCATOR,
    &HYDRATION,
    &COMPACTION,
    &MAINTENANCE_JOB,
    &STORAGE_FILE,
    &SECRET,
    &SETTING,
    &BUILD,
    &TEST,
];

/// The relation `api/sys.json` names `name` (`sys.Branch`).
#[must_use]
pub fn by_name(name: &str) -> Option<&'static RelationSpec> {
    ALL.iter().copied().find(|relation| relation.name == name)
}

/// The relation whose references carry the physical identity `table_id`.
#[must_use]
pub fn by_table_id(table_id: [u8; 16]) -> Option<&'static RelationSpec> {
    ALL.iter()
        .copied()
        .find(|relation| relation.table_id == table_id)
}
