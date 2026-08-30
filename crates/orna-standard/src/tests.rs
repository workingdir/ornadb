use std::{cell::Cell, error::Error as _};

use orna_core::catalogue::{
    CatalogueSnapshot, CatalogueSnapshotError, PreludeTypeName, PreludeTypeNameError,
    QualifiedSemanticName, SemanticNameError, TypeBindingError, TypeBindingKind, TypeLookupName,
    ValueTypeDefinition, ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
};
use orna_core::revision::DefinitionIdentity;
use orna_core::system::{
    SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT, SYS_INSPECT_CALLS_TYPE_ID,
    SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT, SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
    SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT,
    SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID, SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT,
    SYS_INSPECT_RESOURCES_TYPE_ID, SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT,
    SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID, SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT,
    SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID, SYS_INSPECT_SNAPSHOT_REPRESENTATION_CONTRACT,
    SYS_INSPECT_SNAPSHOT_TYPE_ID, SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT,
    SYS_INSPECT_STATE_CELLS_TYPE_ID, SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT,
    SYS_INSPECT_UI_NODES_TYPE_ID,
};
use orna_core::{
    canonical_hash::{
        artifact_payload_digest, calculate_standard_library_digest, catalogue_digest,
        catalogue_digest_with_context, function_semantic_digest_with_version, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest, standard_library_digest,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionPair, StandardExecutable,
        StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit,
    },
    value::{
        OpaqueValue, OpaqueValueError, MAX_OPAQUE_CODEC_ACTION_ARGUMENTS, MAX_RUNTIME_VALUE_NODES,
    },
    CatalogueRevisionId, SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
};

use super::{
    build_type_bindings, is_registered_inspect_carrier_type, prepare_standard_upgrade,
    prepare_standard_upgrade_v1_to_v2, prepare_standard_upgrade_v2_to_v3,
    prepare_standard_upgrade_with, registered_inspect_carrier_codecs, registered_opaque_codecs,
    retained_standard_library_snapshot, retained_standard_library_snapshot_from_source,
    retained_standard_library_v2_snapshot, retained_standard_library_v2_snapshot_from_source,
    retained_standard_library_v3_snapshot, retained_standard_library_v4_snapshot,
    standard_library_manifest, standard_library_v2_manifest, standard_library_v3_manifest,
    standard_library_v4_manifest, verify_standard_library_snapshot,
    verify_standard_library_v2_snapshot, verify_standard_library_v3_snapshot,
    verify_standard_library_v4_snapshot, StandardLibraryError, StandardLibraryManifestError,
    StandardUpgradeError, ACTION_MAGIC, BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID,
    BOOLEAN_TYPE_ID, BYTE_STREAM_MAGIC, CHARACTER_LARGE_OBJECT_TYPE_ID, DATE_TYPE_ID,
    DECIMAL_TYPE_ID, DURATION_TYPE_ID, EXPECTED_TYPE_BINDING_IDS, FLOAT_TYPE_ID, INTEGER_TYPE_ID,
    JSON_MAGIC, LANGUAGE_VERSION_IDENTITY, OPAQUE_TOKEN_TYPE_ID, RETAINED_STANDARD_INVOKE_SOURCE,
    RETAINED_STANDARD_OUTPUT_SOURCE, RETAINED_STANDARD_SOURCE, RETAINED_STANDARD_UI_SOURCE,
    SOURCE_LOGICAL_PATH, STANDARD_CATALOGUE_REVISION_ID, STANDARD_CATALOGUE_V2_REVISION_ID,
    STANDARD_CATALOGUE_V3_REVISION_ID, STANDARD_CATALOGUE_V4_REVISION_ID,
    STANDARD_CATALOGUE_V5_REVISION_ID, STANDARD_CATALOGUE_V6_REVISION_ID,
    STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_V2_REVISION_ID,
    STANDARD_LIBRARY_V2_VERSION_IDENTITY, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_LIBRARY_V3_VERSION_IDENTITY, STANDARD_LIBRARY_V4_REVISION_ID,
    STANDARD_LIBRARY_V4_VERSION_IDENTITY, STANDARD_LIBRARY_V5_REVISION_ID,
    STANDARD_LIBRARY_V5_VERSION_IDENTITY, STANDARD_LIBRARY_V6_REVISION_ID,
    STANDARD_LIBRARY_V6_VERSION_IDENTITY, STANDARD_LIBRARY_VERSION_IDENTITY,
    STANDARD_SOURCE_BUNDLE_ID, STANDARD_SOURCE_REVISION_ID, STANDARD_SOURCE_UNIT_ID,
    STANDARD_SOURCE_V2_BUNDLE_ID, STANDARD_SOURCE_V2_REVISION_ID, STANDARD_SOURCE_V3_BUNDLE_ID,
    STANDARD_SOURCE_V3_REVISION_ID, STANDARD_SOURCE_V4_BUNDLE_ID, STANDARD_SOURCE_V4_REVISION_ID,
    STANDARD_SOURCE_V5_BUNDLE_ID, STANDARD_SOURCE_V5_REVISION_ID, STANDARD_SOURCE_V6_BUNDLE_ID,
    STANDARD_SOURCE_V6_REVISION_ID, STANDARD_TYPE_IDS, STD_ACTION_CONTRACT, STD_ACTION_SCHEMA_ID,
    STD_ACTION_SOURCE_LOGICAL_PATH, STD_ACTION_SOURCE_UNIT_ID, STD_ACTION_TYPE_ID,
    STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID,
    STD_INVOKE_SOURCE_LOGICAL_PATH, STD_INVOKE_SOURCE_UNIT_ID, STD_IO_BYTE_STREAM_CONTRACT,
    STD_IO_BYTE_STREAM_TYPE_ID, STD_IO_SCHEMA_ID, STD_JSON_CONTRACT, STD_JSON_ENCODE_FUNCTION_ID,
    STD_JSON_SCHEMA_ID, STD_JSON_SOURCE_LOGICAL_PATH, STD_JSON_SOURCE_UNIT_ID,
    STD_JSON_VALUE_TYPE_ID, STD_OUTPUT_SOURCE_LOGICAL_PATH, STD_OUTPUT_SOURCE_UNIT_ID,
    STD_SCHEMA_ID, STD_TERMINAL_DOCUMENT_CONTRACT, STD_TERMINAL_DOCUMENT_TYPE_ID,
    STD_TERMINAL_SCHEMA_ID, STD_TYPES_SCHEMA_ID, STD_TYPES_SOURCE_UNIT_ID, STD_UI_CONTRACT,
    STD_UI_SCHEMA_ID, STD_UI_SOURCE_LOGICAL_PATH, STD_UI_SOURCE_UNIT_ID, STD_UI_TYPE_ID,
    TERMINAL_DOCUMENT_MAGIC, TIMESTAMP_TYPE_ID, TIME_TYPE_ID, UI_MAGIC, UUID_TYPE_ID, VOID_TYPE_ID,
};

const EXPECTED_RETAINED_STANDARD_SOURCE: &str = r#"CREATE SCHEMA std;
CREATE SCHEMA std.types;

CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.boolean@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;

EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;
EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;

CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.integer@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.INTEGER AS std.INTEGER;

EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;
EXPORT TYPE std.INTEGER TO PRELUDE AS INT;

CREATE TYPE std.types.BIGINT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.bigint@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.BIGINT AS std.BIGINT;

EXPORT TYPE std.BIGINT TO PRELUDE AS BIGINT;

CREATE TYPE std.types.FLOAT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.float@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.FLOAT AS std.FLOAT;

EXPORT TYPE std.FLOAT TO PRELUDE AS FLOAT;

CREATE TYPE std.types.DECIMAL AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.decimal@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.DECIMAL AS std.DECIMAL;

EXPORT TYPE std.DECIMAL TO PRELUDE AS DECIMAL;

CREATE TYPE std.types.CHARACTER_LARGE_OBJECT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.character-large-object@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.CHARACTER_LARGE_OBJECT AS std.CHARACTER_LARGE_OBJECT;

EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS CHARACTER LARGE OBJECT;
EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS TEXT;

CREATE TYPE std.types.BINARY_LARGE_OBJECT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.binary-large-object@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.BINARY_LARGE_OBJECT AS std.BINARY_LARGE_OBJECT;

EXPORT TYPE std.BINARY_LARGE_OBJECT TO PRELUDE AS BINARY LARGE OBJECT;
EXPORT TYPE std.BINARY_LARGE_OBJECT TO PRELUDE AS BYTES;

CREATE TYPE std.types.UUID AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.uuid@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.UUID AS std.UUID;

EXPORT TYPE std.UUID TO PRELUDE AS UUID;

CREATE TYPE std.types.DATE AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.date@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.DATE AS std.DATE;

EXPORT TYPE std.DATE TO PRELUDE AS DATE;

CREATE TYPE std.types.TIME AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.time@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.TIME AS std.TIME;

EXPORT TYPE std.TIME TO PRELUDE AS TIME;

CREATE TYPE std.types.TIMESTAMP AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.timestamp@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.TIMESTAMP AS std.TIMESTAMP;

EXPORT TYPE std.TIMESTAMP TO PRELUDE AS TIMESTAMP;

CREATE TYPE std.types.DURATION AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.duration@1'
    IMMUTABLE
    PERSISTABLE;

EXPORT TYPE std.types.DURATION AS std.DURATION;

EXPORT TYPE std.DURATION TO PRELUDE AS DURATION;

CREATE TYPE std.types.VOID AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.void@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.types.VOID AS std.VOID;

EXPORT TYPE std.VOID TO PRELUDE AS VOID;

CREATE TYPE std.types.OPAQUE_TOKEN AS VALUE OPAQUE
    KERNEL CONTRACT 'orna.std.value.opaque-token@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.types.OPAQUE_TOKEN AS std.OPAQUE_TOKEN;
"#;

fn empty_active_revision() -> ActiveDatabaseRevision {
    let source_bundle = SourceBundleId::from_bytes([0x81; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x82; 16]);
    let bundle_hash = source_bundle_digest(&[]).expect("the empty source bundle is valid");
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash)
            .expect("the empty source revision is valid"),
    )
    .expect("the empty stored source revision is valid");
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x83; 16]),
        Vec::new(),
        Vec::new(),
    )
    .expect("the empty catalogue is valid");
    let pair = RevisionPair::new(source.id(), catalogue.revision());

    ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue.clone(),
        catalogue_digest(&catalogue, &[], &[], &[], &[])
            .expect("the empty catalogue digest is valid"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("the empty active revision is valid")
}

fn empty_version_two_active_revision(
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x84; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("").expect("the empty source-unit digest is valid"),
    )
    .expect("the empty source unit is valid");
    let source_bundle = SourceBundleId::from_bytes([0x85; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit))
        .expect("the active source bundle is valid");
    let source = StoredSourceRevision::new(
        source_bundle,
        SourceRevisionId::from_bytes([0x86; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash)
            .expect("the active source revision is valid"),
    )
    .expect("the active stored source revision is valid");
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x87; 16]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("the active catalogue is valid");
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])
        .expect("the active catalogue digest is valid");

    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .expect("the version-two active revision is valid")
}

mod v1;
mod v2_v4;
mod v5_v10;
mod v11;

use v1::EXPECTED_RETAINED_INVOKE_SOURCE;
use v2_v4::EXPECTED_RETAINED_ACTION_SOURCE;
