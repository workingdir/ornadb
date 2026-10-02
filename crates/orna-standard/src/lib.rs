//! Source-independent facts for the Orna standard library.

use std::{error::Error, fmt};

use orna_compiler::{
    CheckedStandardLibrary, PrepareStandardUpgradeError, PreparedStandardUpgrade,
    StandardLibraryCheckError, check_standard_library_source, prepare_checked_standard_upgrade,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, SchemaId, SourceBundleId,
    SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeBindingId, TypeId,
    canonical_hash::CanonicalHashError,
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, PreludeTypeName, PreludeTypeNameError,
        QualifiedSemanticName, SchemaDefinition, SemanticNameError, TypeBinding, TypeBindingError,
        TypeLookupName, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, DeployableRevision, RevisionInvariantError, Sha256Digest,
        StandardLibrarySnapshot, VerifiedStandardLibrarySnapshot,
    },
    value::{
        INSPECT_CARRIER_CODEC_REGISTRATIONS, InspectCarrierCodecRegistration,
        OpaqueCodecRegistration, OpaqueCodecRegistry, OpaqueCodecRegistryError,
    },
};
#[cfg(test)]
use orna_core::{
    canonical_hash::{
        artifact_payload_digest, calculate_standard_library_digest, function_declaration_digest,
        function_semantic_digest_with_version, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest, standard_library_digest,
        verify_standard_library_snapshot as verify_canonical_standard_library_snapshot,
        verify_standard_library_v2_snapshot as verify_canonical_standard_library_v2_snapshot,
    },
    catalogue::{
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity, FunctionTransaction,
        FunctionVolatility, ParameterDefinition, ValueTypeKind,
    },
    revision::{
        DefinitionIdentity, DefinitionOrigin, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, FunctionSemanticHashVersion, SourceOrigin, StandardExecutable,
        StandardLibraryDigestVersion, StoredSourceRevision, StoredSourceUnit,
    },
    types::{ResolvedType, StandardScalar},
};
use orna_semantic_v1::{
    Catalogue as StandardCatalogueV1, StandardCatalogueError, StandardDependencyProfile,
};
#[cfg(test)]
use orna_syntax::{NamePart, PrimitiveValueTypePersistence, QualifiedName, TypeExportTarget};

mod codecs;

pub use codecs::{
    RegisteredOpaqueCodecsError, is_registered_inspect_carrier_type,
    registered_inspect_carrier_codecs, registered_opaque_codecs,
};

pub use orna_compiler::StandardUpgradeIdentity;
pub use orna_compiler::{
    STD_DATA_ROWS_TYPE_BINDING_ID, STD_DATA_ROWS_TYPE_ID, STD_DATA_SCHEMA_ID,
    STD_DATA_SOURCE_UNIT_ID, STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID,
    STD_INVOKE_ECHO_FUNCTION_REVISION_ID, STD_INVOKE_ECHO_PARAMETER_ID,
    STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID, STD_INVOKE_SOURCE_UNIT_ID,
    STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_SCHEMA_ID, STD_JSON_VALUE_TYPE_ID,
    STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID, STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
    STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID, STD_TYPES_SOURCE_UNIT_ID,
    STD_UI_BUTTON_ENABLED_PARAMETER_ID, STD_UI_BUTTON_FUNCTION_ID,
    STD_UI_BUTTON_FUNCTION_REVISION_ID, STD_UI_BUTTON_LABEL_PARAMETER_ID,
    STD_UI_BUTTON_RUNTIME_CONTRACT, STD_UI_COLUMN_CONTENT_PARAMETER_ID, STD_UI_COLUMN_FUNCTION_ID,
    STD_UI_COLUMN_FUNCTION_REVISION_ID, STD_UI_COLUMN_RUNTIME_CONTRACT,
    STD_UI_PANEL_CONTENT_PARAMETER_ID, STD_UI_PANEL_FUNCTION_ID, STD_UI_PANEL_FUNCTION_REVISION_ID,
    STD_UI_PANEL_RUNTIME_CONTRACT, STD_UI_ROW_CONTENT_PARAMETER_ID, STD_UI_ROW_FUNCTION_ID,
    STD_UI_ROW_FUNCTION_REVISION_ID, STD_UI_ROW_RUNTIME_CONTRACT, STD_UI_TABS_CONTENT_PARAMETER_ID,
    STD_UI_TABS_FUNCTION_ID, STD_UI_TABS_FUNCTION_REVISION_ID, STD_UI_TABS_RUNTIME_CONTRACT,
    STD_UI_TEXT_FUNCTION_ID, STD_UI_TEXT_FUNCTION_REVISION_ID,
    STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID, STD_UI_TEXT_INPUT_FUNCTION_ID,
    STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID, STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
    STD_UI_TEXT_INPUT_RUNTIME_CONTRACT, STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
    STD_UI_TEXT_PARAMETER_ID, STD_UI_TEXT_RUNTIME_CONTRACT, STD_UI_WINDOW_CONTENT_PARAMETER_ID,
    STD_UI_WINDOW_FUNCTION_ID, STD_UI_WINDOW_FUNCTION_REVISION_ID, STD_UI_WINDOW_REVISION_NUMBER,
    STD_UI_WINDOW_RUNTIME_CONTRACT, STD_UI_WINDOW_TITLE_PARAMETER_ID,
    check_standard_terminal_present_table,
};
pub use orna_core::inspect::INSPECT_RENDER_CONTRACT;

/// The standard-library version represented by this manifest.
pub const STANDARD_LIBRARY_VERSION_IDENTITY: &str = "orna.std/1";

/// Logical source path of the pinned Orna 1.0.0 reference math module.
pub const REFERENCE_STANDARD_MATH_PATH_V1: &str = "std/math.orna";
pub const REFERENCE_STANDARD_COLLECTION_PATH_V1: &str = "std/collection.orna";
pub const REFERENCE_STANDARD_QUERY_PATH_V1: &str = "std/query.orna";
pub const REFERENCE_STANDARD_TEXT_PATH_V1: &str = "std/text.orna";
pub const REFERENCE_STANDARD_BITS_PATH_V1: &str = "std/bits.orna";
pub const REFERENCE_STANDARD_STATS_PATH_V1: &str = "std/stats.orna";
pub const REFERENCE_STANDARD_TIME_PATH_V1: &str = "std/time.orna";
pub const REFERENCE_STANDARD_TIME_COMPACT_PATH_V1: &str = "std/time/duration/compact.orna";
pub const REFERENCE_STANDARD_TIME_CLOCK_PATH_V1: &str = "std/time/duration/clock.orna";
pub const REFERENCE_STANDARD_TIME_WORDS_PATH_V1: &str = "std/time/duration/words.orna";
pub const REFERENCE_STANDARD_TIME_ISO_PATH_V1: &str = "std/time/duration/iso.orna";
pub const REFERENCE_STANDARD_OPTION_PATH_V1: &str = "std/option.orna";
pub const REFERENCE_STANDARD_RESULT_PATH_V1: &str = "std/result.orna";
pub const REFERENCE_STANDARD_LIST_PATH_V1: &str = "std/list.orna";
pub const REFERENCE_STANDARD_MAP_PATH_V1: &str = "std/map.orna";
pub const REFERENCE_STANDARD_SET_PATH_V1: &str = "std/set.orna";

const REFERENCE_STANDARD_MATH_SOURCE_V1: &str = include_str!("../../../stdlib/std/math.orna");
const REFERENCE_STANDARD_COLLECTION_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/collection.orna");
const REFERENCE_STANDARD_QUERY_SOURCE_V1: &str = include_str!("../../../stdlib/std/query.orna");
const REFERENCE_STANDARD_TEXT_SOURCE_V1: &str = include_str!("../../../stdlib/std/text.orna");
const REFERENCE_STANDARD_BITS_SOURCE_V1: &str = include_str!("../../../stdlib/std/bits.orna");
const REFERENCE_STANDARD_STATS_SOURCE_V1: &str = include_str!("../../../stdlib/std/stats.orna");
const REFERENCE_STANDARD_TIME_SOURCE_V1: &str = include_str!("../../../stdlib/std/time.orna");
const REFERENCE_STANDARD_TIME_COMPACT_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/compact.orna");
const REFERENCE_STANDARD_TIME_CLOCK_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/clock.orna");
const REFERENCE_STANDARD_TIME_WORDS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/words.orna");
const REFERENCE_STANDARD_TIME_ISO_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/iso.orna");
const REFERENCE_STANDARD_OPTION_SOURCE_V1: &str = include_str!("../../../stdlib/std/option.orna");
const REFERENCE_STANDARD_RESULT_SOURCE_V1: &str = include_str!("../../../stdlib/std/result.orna");
const REFERENCE_STANDARD_LIST_SOURCE_V1: &str = include_str!("../../../stdlib/std/list.orna");
const REFERENCE_STANDARD_MAP_SOURCE_V1: &str = include_str!("../../../stdlib/std/map.orna");
const REFERENCE_STANDARD_SET_SOURCE_V1: &str = include_str!("../../../stdlib/std/set.orna");

/// Source units for the Orna 1.0.0 reference standard dependency.
///
/// This is the current source-backed standard boundary. The retained `orna.std/1`–
/// `orna.std/11` APIs below model older, explicitly versioned snapshots.
#[must_use]
pub fn reference_standard_sources_v1() -> [(String, String); 16] {
    [
        (
            REFERENCE_STANDARD_MATH_PATH_V1.into(),
            REFERENCE_STANDARD_MATH_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_COLLECTION_PATH_V1.into(),
            REFERENCE_STANDARD_COLLECTION_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_QUERY_PATH_V1.into(),
            REFERENCE_STANDARD_QUERY_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TEXT_PATH_V1.into(),
            REFERENCE_STANDARD_TEXT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_BITS_PATH_V1.into(),
            REFERENCE_STANDARD_BITS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_STATS_PATH_V1.into(),
            REFERENCE_STANDARD_STATS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_COMPACT_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_COMPACT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_CLOCK_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_CLOCK_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_WORDS_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_WORDS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_ISO_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_ISO_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_OPTION_PATH_V1.into(),
            REFERENCE_STANDARD_OPTION_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_RESULT_PATH_V1.into(),
            REFERENCE_STANDARD_RESULT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_LIST_PATH_V1.into(),
            REFERENCE_STANDARD_LIST_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_MAP_PATH_V1.into(),
            REFERENCE_STANDARD_MAP_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_SET_PATH_V1.into(),
            REFERENCE_STANDARD_SET_SOURCE_V1.into(),
        ),
    ]
}

/// Profile that pins the exact 1.0.0 reference-standard source bytes.
#[must_use]
pub fn reference_standard_profile_v1() -> StandardDependencyProfile {
    StandardDependencyProfile::from_sources(
        "orna.std/v1-reference-library",
        reference_standard_sources_v1(),
    )
    .expect("the bundled Orna 1.0.0 standard module path is valid")
}

/// Builds the semantic catalogue from the pinned Orna 1.0.0 source, without
/// routing through the pre-1.0 SQL parser/compiler used by retained snapshots.
pub fn reference_standard_catalogue_v1() -> Result<StandardCatalogueV1, StandardCatalogueError> {
    StandardCatalogueV1::from_standard_sources(
        &reference_standard_profile_v1(),
        reference_standard_sources_v1(),
    )
}

/// The language version associated with this standard-library version.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";

/// The logical path reserved for the retained standard-library source.
pub const SOURCE_LOGICAL_PATH: &str = "std/types.orna";

/// The stable identity of `orna.std/1`.
pub const STANDARD_LIBRARY_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(1));

/// The stable identity of the standard catalogue revision.
pub const STANDARD_CATALOGUE_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(1));

/// The stable identity reserved for the standard source bundle.
pub const STANDARD_SOURCE_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(1));

/// The stable identity reserved for the standard source revision.
pub const STANDARD_SOURCE_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(1));

/// The stable identity reserved for `std/types.orna`.
pub const STANDARD_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(1));

/// The stable identity of the `std` schema.
pub const STD_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(1));

/// The stable identity of the `std.types` schema.
pub const STD_TYPES_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(2));

/// The stable identity of `std.types.boolean`.
pub const BOOLEAN_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(1));

/// The stable identity of `std.types.integer`.
pub const INTEGER_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(2));

/// The stable identity of `std.types.bigint`.
pub const BIGINT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(3));

/// The stable identity of `std.types.float`.
pub const FLOAT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(4));

/// The stable identity of `std.types.decimal`.
pub const DECIMAL_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(5));

/// The stable identity of `std.types.character_large_object`.
pub const CHARACTER_LARGE_OBJECT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(6));

/// The stable identity of `std.types.binary_large_object`.
pub const BINARY_LARGE_OBJECT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(7));

/// The stable identity of `std.types.uuid`.
pub const UUID_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(8));

/// The stable identity of `std.types.date`.
pub const DATE_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(9));

/// The stable identity of `std.types.time`.
pub const TIME_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(10));

/// The stable identity of `std.types.timestamp`.
pub const TIMESTAMP_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(11));

/// The stable identity of `std.types.duration`.
pub const DURATION_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(12));

/// The stable identity of `std.types.void`.
pub const VOID_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(13));

/// The stable identity of `std.types.opaque_token`.
pub const OPAQUE_TOKEN_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(14));

/// All initial standard type identities in manifest order.
pub const STANDARD_TYPE_IDS: [TypeId; 14] = [
    BOOLEAN_TYPE_ID,
    INTEGER_TYPE_ID,
    BIGINT_TYPE_ID,
    FLOAT_TYPE_ID,
    DECIMAL_TYPE_ID,
    CHARACTER_LARGE_OBJECT_TYPE_ID,
    BINARY_LARGE_OBJECT_TYPE_ID,
    UUID_TYPE_ID,
    DATE_TYPE_ID,
    TIME_TYPE_ID,
    TIMESTAMP_TYPE_ID,
    DURATION_TYPE_ID,
    VOID_TYPE_ID,
    OPAQUE_TOKEN_TYPE_ID,
];

/// The standard-library version represented by the V2 manifest.
pub const STANDARD_LIBRARY_V2_VERSION_IDENTITY: &str = "orna.std/2";

/// The stable identity of `orna.std/2`.
pub const STANDARD_LIBRARY_V2_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(2));

/// The stable identity of the V2 standard catalogue revision.
pub const STANDARD_CATALOGUE_V2_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(2));

/// The stable identity reserved for the V2 standard source bundle.
pub const STANDARD_SOURCE_V2_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(2));

/// The stable identity reserved for the V2 standard source revision.
pub const STANDARD_SOURCE_V2_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(2));

/// The logical path reserved for the retained V2 invoke source unit.
pub const STD_INVOKE_SOURCE_LOGICAL_PATH: &str = "std/invoke.orna";

const fn reserved_id(final_byte: u8) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[15] = final_byte;
    bytes
}

#[cfg(test)]
const ACCEPTED_SOURCE_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x5d, 0x53, 0x60, 0x01, 0xab, 0xc7, 0x54, 0xcf, 0x2c, 0xde, 0x9f, 0xf4, 0xed, 0x50, 0xb2, 0x2d,
    0xe8, 0xbb, 0x70, 0x04, 0x0a, 0x69, 0x1b, 0xc2, 0xec, 0x50, 0xbd, 0x6c, 0x65, 0xe5, 0x25, 0xf4,
]);
#[cfg(test)]
const ACCEPTED_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xd8, 0x0e, 0x8f, 0x73, 0x88, 0x78, 0x2d, 0x73, 0x0e, 0x4d, 0x6c, 0x5a, 0x6f, 0xcd, 0x4a, 0x56,
    0x42, 0xa4, 0x81, 0xcb, 0x65, 0x6d, 0x6e, 0x5f, 0xca, 0x35, 0x9a, 0x69, 0xf3, 0x72, 0x63, 0xeb,
]);
const ACCEPTED_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x40, 0x0e, 0xb4, 0x35, 0x5d, 0xa2, 0x8f, 0x41, 0xf4, 0xd4, 0xae, 0x8c, 0x06, 0x21, 0x24, 0x89,
    0xbe, 0x60, 0xf6, 0xd8, 0x7c, 0x6d, 0x8e, 0xf3, 0x0c, 0x29, 0x1c, 0xc8, 0x3b, 0x2c, 0xfb, 0x6b,
]);
const ACCEPTED_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56, 0xd4, 0x89, 0xad,
    0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70, 0x5b, 0x30, 0x23, 0x5b, 0x08, 0x1d,
]);

// The V2 digest goldens below are computed by the canonical encoders from the
// retained source and canonical records (never copied from a handwritten
// encoder). The digest-golden tests recompute every value from the retained
// units and compare against these constants, so any retained-source edit fails
// loudly at build time.
#[cfg(test)]
const ACCEPTED_V2_TYPES_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x5d, 0x53, 0x60, 0x01, 0xab, 0xc7, 0x54, 0xcf, 0x2c, 0xde, 0x9f, 0xf4, 0xed, 0x50, 0xb2, 0x2d,
    0xe8, 0xbb, 0x70, 0x04, 0x0a, 0x69, 0x1b, 0xc2, 0xec, 0x50, 0xbd, 0x6c, 0x65, 0xe5, 0x25, 0xf4,
]);
#[cfg(test)]
const ACCEPTED_V2_INVOKE_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xb1, 0x9b, 0x95, 0x6b, 0xf6, 0xb2, 0x68, 0x54, 0x93, 0xe2, 0x83, 0x4a, 0xbd, 0x60, 0x35, 0x3a,
    0xbf, 0x70, 0xb7, 0x45, 0xe4, 0x89, 0x4b, 0x9c, 0x66, 0xd2, 0xa7, 0x7e, 0x74, 0x3e, 0xdd, 0xc5,
]);
#[cfg(test)]
const ACCEPTED_V2_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xc5, 0xd5, 0xc6, 0x73, 0x22, 0xae, 0xb5, 0x8b, 0xfd, 0xe0, 0x7a, 0xb1, 0x02, 0x8d, 0x45, 0x7d,
    0x34, 0x1d, 0xd8, 0x5e, 0x25, 0x31, 0xe0, 0xf6, 0xa4, 0x2d, 0x89, 0xa8, 0xb9, 0x8e, 0x9d, 0x22,
]);
const ACCEPTED_V2_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x75, 0x5f, 0x9e, 0xfd, 0xb3, 0x39, 0xe7, 0x36, 0x9d, 0xa8, 0x75, 0x89, 0x42, 0x7e, 0x1c, 0x4a,
    0x0e, 0xae, 0x18, 0xbe, 0xe4, 0x53, 0x2b, 0x8e, 0x7d, 0x46, 0xbc, 0x9c, 0x79, 0x9e, 0x57, 0x89,
]);
const ACCEPTED_V2_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xb3, 0xb0, 0xf9, 0xb7, 0xed, 0x69, 0x1a, 0xaf, 0x03, 0x57, 0x9b, 0x20, 0x1c, 0xf3, 0xda, 0xc1,
    0xb7, 0x25, 0xba, 0xdf, 0x90, 0xb6, 0x91, 0x1a, 0x98, 0x23, 0xa3, 0x24, 0x91, 0x06, 0x73, 0xce,
]);
#[cfg(test)]
const ACCEPTED_V2_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x65, 0x2a, 0x53, 0x25, 0xc9, 0xd1, 0x1d, 0x33, 0x20, 0x6c, 0x35, 0x1c, 0x0c, 0x5e, 0x8c, 0x3a,
    0x82, 0x2a, 0x5b, 0x9b, 0x72, 0x22, 0x02, 0xb9, 0x3c, 0x25, 0x87, 0x05, 0x1f, 0x0f, 0x46, 0xc2,
]);
#[cfg(test)]
const ACCEPTED_V2_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x9e, 0xf8, 0x60, 0x0b, 0x7f, 0x63, 0xd2, 0xab, 0x4e, 0x43, 0xee, 0xaa, 0xfd, 0x23, 0xb9, 0x8a,
    0x82, 0x49, 0x07, 0xd4, 0x25, 0xb4, 0x62, 0x0c, 0x27, 0x35, 0x13, 0x75, 0x74, 0xff, 0x9b, 0x8d,
]);

/// The standard-library version represented by the V3 manifest (work ADR 0058).
pub const STANDARD_LIBRARY_V3_VERSION_IDENTITY: &str = "orna.std/3";

/// The stable identity of `orna.std/3`.
pub const STANDARD_LIBRARY_V3_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(3));

/// The stable identity of the V3 standard catalogue revision.
pub const STANDARD_CATALOGUE_V3_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(3));

/// The stable identity reserved for the V3 standard source bundle.
pub const STANDARD_SOURCE_V3_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(3));

/// The stable identity reserved for the V3 standard source revision.
pub const STANDARD_SOURCE_V3_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(3));

/// The logical path reserved for the retained V3 output source unit.
pub const STD_OUTPUT_SOURCE_LOGICAL_PATH: &str = "std/output.orna";

/// The stable identity of the retained `std/output.orna` unit in the V3 bundle.
pub const STD_OUTPUT_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(4));

/// The stable identity of the `std.terminal` schema.
pub const STD_TERMINAL_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(4));

/// The stable identity of the `std.io` schema.
pub const STD_IO_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(5));

/// The stable identity of `std.terminal.Document`.
pub const STD_TERMINAL_DOCUMENT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(15));

/// The stable identity of `std.io.ByteStream`.
pub const STD_IO_BYTE_STREAM_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(16));

/// The kernel representation contract of `std.terminal.Document`.
pub const STD_TERMINAL_DOCUMENT_CONTRACT: &str = "orna.std.value.terminal-document@1";

/// The kernel representation contract of `std.io.ByteStream`.
pub const STD_IO_BYTE_STREAM_CONTRACT: &str = "orna.std.value.byte-stream@1";

/// The canonical ASCII magic prefix of a `std.terminal.Document` payload.
///
/// The canonical payload is exactly `ORNA-TERMINAL-DOCUMENT/1 ` followed by a
/// big-endian `u32` body length and the body bytes.
pub const TERMINAL_DOCUMENT_MAGIC: &str = "ORNA-TERMINAL-DOCUMENT/1 ";

/// The canonical ASCII magic prefix of a `std.io.ByteStream` payload.
///
/// The canonical payload is exactly `ORNA-BYTE-STREAM/1 ` followed by a
/// big-endian `u32` media-type length, the media type, a big-endian `u32`
/// body length, and the body bytes.
pub const BYTE_STREAM_MAGIC: &str = "ORNA-BYTE-STREAM/1 ";

// The V3 digest goldens below are computed by the canonical encoders from the
// retained source and canonical records (never copied from a handwritten
// encoder). The digest-golden tests recompute every value from the retained
// units and compare against these constants, so any retained-source edit fails
// loudly at build time. The V3 artifact and semantic digests are the V2
// goldens because `orna.std/3` retains the exact V2 parameter-echo executable
// unchanged.
#[cfg(test)]
const ACCEPTED_V3_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_TYPES_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V3_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_INVOKE_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V3_OUTPUT_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x8f, 0x16, 0x21, 0x4d, 0x9c, 0x4d, 0xee, 0x06, 0x6f, 0x24, 0x7b, 0x24, 0x15, 0xe9, 0xaf, 0xa7,
    0x0f, 0xcf, 0x5f, 0xb2, 0x66, 0x47, 0x3b, 0xb0, 0xfd, 0x6d, 0x72, 0x87, 0x98, 0xa2, 0xaf, 0x35,
]);
#[cfg(test)]
const ACCEPTED_V3_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x28, 0x69, 0x41, 0x4f, 0x3b, 0xbc, 0xb9, 0x14, 0x60, 0x5b, 0xf4, 0x79, 0x4d, 0x2d, 0x4d, 0xd3,
    0xe4, 0x3f, 0x43, 0xc9, 0x72, 0xc7, 0x50, 0x53, 0xc7, 0xeb, 0xc3, 0xdf, 0xb9, 0x19, 0xb1, 0x5f,
]);
const ACCEPTED_V3_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x60, 0xb7, 0xba, 0xdc, 0x70, 0x1c, 0xf6, 0x2c, 0x2c, 0xd2, 0x83, 0xd3, 0xae, 0x5e, 0x5b, 0xc5,
    0x01, 0xc4, 0xff, 0x8f, 0x7b, 0x1d, 0x75, 0x7e, 0xa1, 0xdc, 0x0d, 0xf6, 0x48, 0xa2, 0x29, 0x44,
]);
const ACCEPTED_V3_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x9e, 0xf4, 0xcb, 0x13, 0xb7, 0x5e, 0xaf, 0x81, 0x40, 0x51, 0xd0, 0x37, 0x47, 0x9c, 0x34, 0x5c,
    0x0e, 0x3b, 0x1d, 0x4e, 0xe0, 0x70, 0x32, 0x3e, 0x36, 0x31, 0x59, 0xe2, 0x79, 0x2c, 0x7d, 0xcd,
]);
#[cfg(test)]
const ACCEPTED_V3_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V2_ARTIFACT_DIGEST;
#[cfg(test)]
const ACCEPTED_V3_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V2_SEMANTIC_DIGEST;

/// The standard-library version represented by the V4 manifest (work ADR 0062).
pub const STANDARD_LIBRARY_V4_VERSION_IDENTITY: &str = "orna.std/4";

/// The stable identity of `orna.std/4`.
pub const STANDARD_LIBRARY_V4_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(4));

/// The stable identity of the V4 standard catalogue revision.
pub const STANDARD_CATALOGUE_V4_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(4));

/// The stable identity reserved for the V4 standard source bundle.
pub const STANDARD_SOURCE_V4_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(4));

/// The stable identity reserved for the V4 standard source revision.
pub const STANDARD_SOURCE_V4_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(4));

/// The logical path reserved for the retained V4 ui source unit.
pub const STD_UI_SOURCE_LOGICAL_PATH: &str = "std/ui.orna";

/// The stable identity of the retained `std/ui.orna` unit in the V4 bundle.
pub const STD_UI_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(5));

/// The stable identity of the `std.ui` schema.
pub const STD_UI_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(8));

/// The stable identity of `std.ui.UI`.
pub const STD_UI_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(19));

/// The kernel representation contract of `std.ui.UI`.
pub const STD_UI_CONTRACT: &str = "orna.std.value.ui@1";

/// The canonical ASCII magic prefix of a `std.ui.UI` payload.
///
/// The canonical payload is exactly `ORNA-UI/1 ` followed by a big-endian
/// `u32` body length and the body bytes (work ADR 0062 provisional frame).
pub const UI_MAGIC: &str = "ORNA-UI/1 ";

// The V4 digest goldens below are computed by the canonical encoders from the
// retained source and canonical records (never copied from a handwritten
// encoder). The digest-golden tests recompute every value from the retained
// units and compare against these constants, so any retained-source edit fails
// loudly at build time. `orna.std/4` retains the exact V1-V3 types unit, the
// exact V2/V3 invoke unit, and the exact V3 output unit unchanged, so those
// content digests are the earlier goldens; the ui content, V4 bundle, V4
// source revision, V4 artifact, and V4 standard-library digests are computed
// by the canonical encoders.
#[cfg(test)]
const ACCEPTED_V4_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_TYPES_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V4_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_INVOKE_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V4_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V3_OUTPUT_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V4_UI_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xe0, 0x86, 0x9a, 0xe3, 0xd4, 0x7e, 0xcb, 0xb6, 0x22, 0x30, 0x05, 0xd5, 0x56, 0x8c, 0x39, 0x0f,
    0xad, 0xe2, 0x75, 0x6d, 0x45, 0xde, 0xb9, 0xa1, 0x83, 0x02, 0xd5, 0xe8, 0x4c, 0x2e, 0x5f, 0xd1,
]);
#[cfg(test)]
const ACCEPTED_V4_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x3d, 0x26, 0x40, 0x9b, 0x61, 0xab, 0x0f, 0xd5, 0xb7, 0xb3, 0x14, 0xf4, 0x4d, 0x0b, 0xc6, 0x21,
    0xeb, 0xce, 0xa0, 0x76, 0x8d, 0xa6, 0x33, 0xc9, 0x4a, 0xd2, 0x96, 0x4c, 0x99, 0xde, 0xc6, 0xfe,
]);
const ACCEPTED_V4_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xab, 0x6d, 0xba, 0x9d, 0xfc, 0x42, 0x35, 0x39, 0xc8, 0xea, 0x90, 0x55, 0xf5, 0xbf, 0x40, 0x6f,
    0x45, 0xb0, 0xd3, 0x36, 0x2c, 0x06, 0x35, 0x7e, 0x34, 0x13, 0x23, 0x88, 0xff, 0x51, 0x41, 0xdd,
]);
#[cfg(test)]
const ACCEPTED_V4_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V3_ARTIFACT_DIGEST;
#[cfg(test)]
const ACCEPTED_V4_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V3_SEMANTIC_DIGEST;
const ACCEPTED_V4_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xdc, 0xff, 0xa5, 0x23, 0x16, 0x43, 0xf4, 0x73, 0x29, 0xd3, 0x00, 0x34, 0x1f, 0xba, 0xa2, 0x4f,
    0x5a, 0xbf, 0xa6, 0xbc, 0xed, 0x77, 0x56, 0x5c, 0xd3, 0x82, 0x74, 0xce, 0xa4, 0x83, 0x8f, 0xb9,
]);

/// The standard-library version represented by the V5 manifest (ADR 0075).
pub const STANDARD_LIBRARY_V5_VERSION_IDENTITY: &str = "orna.std/5";
pub const STANDARD_LIBRARY_V5_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(5));
pub const STANDARD_CATALOGUE_V5_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(5));
pub const STANDARD_SOURCE_V5_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(5));
pub const STANDARD_SOURCE_V5_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(5));
pub const STD_JSON_SOURCE_LOGICAL_PATH: &str = "std/json.orna";
pub const STD_JSON_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(6));
pub const STD_JSON_CONTRACT: &str = "orna.std.value.json@1";
pub const JSON_MAGIC: &str = "ORNA-JSON-VALUE/1 ";

#[cfg(test)]
const ACCEPTED_V5_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_TYPES_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V5_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_INVOKE_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V5_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_OUTPUT_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V5_UI_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_UI_CONTENT_DIGEST;
#[cfg(test)]
const ACCEPTED_V5_JSON_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x4b, 0x12, 0x56, 0xf5, 0x9d, 0x01, 0xe9, 0xec, 0x65, 0x22, 0x85, 0xb1, 0x4f, 0xb8, 0xfc, 0xd5,
    0xde, 0xcf, 0x9b, 0x6d, 0xbf, 0xfb, 0xf7, 0x0d, 0xa8, 0x7a, 0xad, 0xeb, 0xb9, 0xa0, 0x18, 0xbe,
]);
#[cfg(test)]
const ACCEPTED_V5_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x87, 0xe2, 0xf5, 0x9d, 0x44, 0x47, 0x78, 0x9f, 0x0f, 0x9d, 0xc5, 0xa9, 0x64, 0xf9, 0xec, 0x20,
    0x1a, 0xfd, 0xdd, 0xe2, 0x8b, 0xe0, 0x7e, 0xd3, 0xd2, 0x37, 0x74, 0xc8, 0x33, 0xf5, 0x31, 0x15,
]);
const ACCEPTED_V5_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x91, 0x2f, 0xb3, 0xb6, 0x6c, 0x28, 0x35, 0xb3, 0x68, 0x68, 0x68, 0x76, 0x75, 0x5d, 0x7c, 0x78,
    0x9c, 0xc3, 0xf2, 0x5c, 0x26, 0x87, 0x27, 0xb0, 0x83, 0xd9, 0x6e, 0x70, 0x7a, 0x99, 0xbc, 0x51,
]);
#[cfg(test)]
const ACCEPTED_V5_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V4_ARTIFACT_DIGEST;
#[cfg(test)]
const ACCEPTED_V5_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V4_SEMANTIC_DIGEST;
const ACCEPTED_V5_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x22, 0x60, 0x9b, 0xe8, 0xc6, 0x6a, 0xce, 0x4a, 0xbe, 0x37, 0x6b, 0x2d, 0xfa, 0x82, 0x07, 0xd1,
    0x9f, 0xe0, 0xae, 0xa9, 0x49, 0xde, 0x0b, 0xbc, 0xc8, 0xcf, 0x97, 0xbf, 0xec, 0xf8, 0xef, 0xed,
]);

/// The standard-library version represented by the V6 manifest (ADR 0079).
pub const STANDARD_LIBRARY_V6_VERSION_IDENTITY: &str = "orna.std/6";
pub const STANDARD_LIBRARY_V6_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(6));
pub const STANDARD_CATALOGUE_V6_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(6));
pub const STANDARD_SOURCE_V6_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(6));
pub const STANDARD_SOURCE_V6_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(6));
pub const ACTION_MAGIC: &str = "ORNA-ACTION/1 ";

/// The standard-library version represented by the V7 manifest (ADR 0019).
pub const STANDARD_LIBRARY_V7_VERSION_IDENTITY: &str = "orna.std/7";
pub const STANDARD_LIBRARY_V7_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(7));
pub const STANDARD_CATALOGUE_V7_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(7));

/// The standard-library version represented by the V8 manifest (Work ADR 0087).
pub const STANDARD_LIBRARY_V8_VERSION_IDENTITY: &str = "orna.std/8";
pub const STANDARD_LIBRARY_V8_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(8));
pub const STANDARD_CATALOGUE_V8_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(8));
pub const STANDARD_SOURCE_V8_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(8));
pub const STANDARD_SOURCE_V8_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(8));
pub const STD_DATA_SOURCE_LOGICAL_PATH: &str = "std/data.orna";
pub const STD_DATA_ROWS_CONTRACT: &str = "orna.std.value.rows@1";
pub const STD_DATA_ROWS_SEMANTIC_NAME: &str = "std.data.rows";
pub const STD_DATA_ROWS_EXPORT_NAME: &str = "std.Rows";

/// The standard-library version represented by the V9 manifest (Work ADR 0088).
pub const STANDARD_LIBRARY_V9_VERSION_IDENTITY: &str = "orna.std/9";
pub const STANDARD_LIBRARY_V9_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(9));
pub const STANDARD_CATALOGUE_V9_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(9));
pub const STANDARD_SOURCE_V9_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(9));
pub const STANDARD_SOURCE_V9_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(9));

/// The standard-library version represented by the V10 manifest.
pub const STANDARD_LIBRARY_V10_VERSION_IDENTITY: &str = "orna.std/10";
pub const STANDARD_LIBRARY_V10_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(10));
pub const STANDARD_CATALOGUE_V10_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(10));
pub const STANDARD_SOURCE_V10_BUNDLE_ID: SourceBundleId =
    SourceBundleId::from_bytes(reserved_id(10));
pub const STANDARD_SOURCE_V10_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(10));
pub const STD_CLI_SOURCE_LOGICAL_PATH: &str = "std/cli.orna";
pub const STD_CLI_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(11));
pub const STD_CLI_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(10));
pub const STD_CLI_REPL_FUNCTION_ID: FunctionId = FunctionId::from_bytes(reserved_id(0x1C));
pub const STD_CLI_REPL_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes(reserved_id(0x1C));
pub const STD_CLI_REPL_REVISION_NUMBER: u64 = 1;
const ACCEPTED_V10_CLI_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x1e, 0x99, 0xf3, 0x2f, 0xf7, 0xc2, 0xf0, 0x65, 0x4d, 0x67, 0x61, 0xa6, 0xa9, 0xce, 0xd8, 0x26,
    0x95, 0x6c, 0x09, 0x5d, 0xdf, 0xd1, 0x2e, 0xbb, 0xdc, 0xc1, 0x8e, 0xc4, 0xe9, 0xab, 0x19, 0xc4,
]);
const ACCEPTED_V10_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xe9, 0x35, 0x47, 0x5a, 0x86, 0x4d, 0xaa, 0x61, 0x66, 0x01, 0x9a, 0xda, 0xf1, 0x59, 0x86, 0xc4,
    0xee, 0x43, 0x85, 0xcd, 0x8e, 0x1e, 0x64, 0x3e, 0x61, 0x3b, 0x55, 0x3a, 0xb0, 0x6e, 0x04, 0x5a,
]);
const ACCEPTED_V10_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x9b, 0x72, 0xf3, 0x38, 0x8b, 0x46, 0xe5, 0x28, 0x42, 0x5a, 0x84, 0x1c, 0x5b, 0x90, 0x61, 0x65,
    0xf7, 0x31, 0x61, 0xe9, 0x2c, 0x9f, 0x93, 0xe6, 0x01, 0x99, 0x81, 0x76, 0x10, 0x6d, 0xdb, 0xbd,
]);
#[derive(Clone, Copy)]
struct ValueTypeFact {
    id: TypeId,
    local_name: &'static str,
    representation_contract: &'static str,
    persistence: ValueTypePersistence,
    prelude_names: &'static [&'static [&'static str]],
}

const BOOLEAN_PRELUDE_NAMES: &[&[&str]] = &[&["BOOLEAN"], &["BOOL"]];
const INTEGER_PRELUDE_NAMES: &[&[&str]] = &[&["INTEGER"], &["INT"]];
const BIGINT_PRELUDE_NAMES: &[&[&str]] = &[&["BIGINT"]];
const FLOAT_PRELUDE_NAMES: &[&[&str]] = &[&["FLOAT"]];
const DECIMAL_PRELUDE_NAMES: &[&[&str]] = &[&["DECIMAL"]];
const CHARACTER_LARGE_OBJECT_PRELUDE_NAMES: &[&[&str]] =
    &[&["CHARACTER", "LARGE", "OBJECT"], &["TEXT"]];
const BINARY_LARGE_OBJECT_PRELUDE_NAMES: &[&[&str]] = &[&["BINARY", "LARGE", "OBJECT"], &["BYTES"]];
const UUID_PRELUDE_NAMES: &[&[&str]] = &[&["UUID"]];
const DATE_PRELUDE_NAMES: &[&[&str]] = &[&["DATE"]];
const TIME_PRELUDE_NAMES: &[&[&str]] = &[&["TIME"]];
const TIMESTAMP_PRELUDE_NAMES: &[&[&str]] = &[&["TIMESTAMP"]];
const DURATION_PRELUDE_NAMES: &[&[&str]] = &[&["DURATION"]];
const VOID_PRELUDE_NAMES: &[&[&str]] = &[&["VOID"]];

const VALUE_TYPE_FACTS: [ValueTypeFact; 13] = [
    ValueTypeFact {
        id: BOOLEAN_TYPE_ID,
        local_name: "boolean",
        representation_contract: "orna.kernel.value.boolean@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: BOOLEAN_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: INTEGER_TYPE_ID,
        local_name: "integer",
        representation_contract: "orna.kernel.value.integer@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: INTEGER_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: BIGINT_TYPE_ID,
        local_name: "bigint",
        representation_contract: "orna.kernel.value.bigint@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: BIGINT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: FLOAT_TYPE_ID,
        local_name: "float",
        representation_contract: "orna.kernel.value.float@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: FLOAT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: DECIMAL_TYPE_ID,
        local_name: "decimal",
        representation_contract: "orna.kernel.value.decimal@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: DECIMAL_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: CHARACTER_LARGE_OBJECT_TYPE_ID,
        local_name: "character_large_object",
        representation_contract: "orna.kernel.value.character-large-object@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: CHARACTER_LARGE_OBJECT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: BINARY_LARGE_OBJECT_TYPE_ID,
        local_name: "binary_large_object",
        representation_contract: "orna.kernel.value.binary-large-object@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: BINARY_LARGE_OBJECT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: UUID_TYPE_ID,
        local_name: "uuid",
        representation_contract: "orna.kernel.value.uuid@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: UUID_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: DATE_TYPE_ID,
        local_name: "date",
        representation_contract: "orna.kernel.value.date@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: DATE_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: TIME_TYPE_ID,
        local_name: "time",
        representation_contract: "orna.kernel.value.time@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: TIME_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: TIMESTAMP_TYPE_ID,
        local_name: "timestamp",
        representation_contract: "orna.kernel.value.timestamp@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: TIMESTAMP_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: DURATION_TYPE_ID,
        local_name: "duration",
        representation_contract: "orna.kernel.value.duration@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: DURATION_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: VOID_TYPE_ID,
        local_name: "void",
        representation_contract: "orna.kernel.value.void@1",
        persistence: ValueTypePersistence::Transient,
        prelude_names: VOID_PRELUDE_NAMES,
    },
];

const OPAQUE_TOKEN_LOCAL_NAME: &str = "opaque_token";
const OPAQUE_TOKEN_CONTRACT: &str = "orna.std.value.opaque-token@1";

// This order is part of the accepted manifest: each value type's qualified
// binding comes first, followed by that type's prelude bindings.
const EXPECTED_TYPE_BINDING_IDS: [[u8; 16]; 31] = [
    [
        0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd, 0x4d,
        0x31,
    ],
    [
        0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8, 0x15,
        0xaa,
    ],
    [
        0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11, 0x0c,
        0xad,
    ],
    [
        0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf, 0x25,
        0x93,
    ],
    [
        0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18, 0x57,
        0x78,
    ],
    [
        0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c, 0x58,
        0x89,
    ],
    [
        0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b, 0x0d,
        0x1e,
    ],
    [
        0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa, 0x5e,
        0x88,
    ],
    [
        0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e, 0xfc,
        0xb5,
    ],
    [
        0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97, 0xb5,
        0xe6,
    ],
    [
        0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b, 0x45,
        0xf9,
    ],
    [
        0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10, 0x6e,
        0xd5,
    ],
    [
        0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2, 0xd8,
        0x70,
    ],
    [
        0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6, 0x89,
        0x1a,
    ],
    [
        0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0, 0xdb,
        0xc2,
    ],
    [
        0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d, 0x81,
        0x34,
    ],
    [
        0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33, 0xaf,
        0xbf,
    ],
    [
        0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6, 0x9d,
        0xb7,
    ],
    [
        0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31, 0x66,
        0x00,
    ],
    [
        0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c, 0x79,
        0x7f,
    ],
    [
        0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71, 0xf7,
        0xac,
    ],
    [
        0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f, 0x93,
        0x56,
    ],
    [
        0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0, 0x34,
        0x8d,
    ],
    [
        0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2, 0x33,
        0xd4,
    ],
    [
        0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc, 0xff,
        0x04,
    ],
    [
        0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c, 0x8a,
        0xa1,
    ],
    [
        0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62, 0x14,
        0x9a,
    ],
    [
        0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d, 0xdc,
        0x37,
    ],
    [
        0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf, 0x94,
        0x0f,
    ],
    [
        0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01, 0x73,
        0xfb,
    ],
    [
        0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87, 0x63, 0x46,
        0xae,
    ],
];

/// The source-independent facts required to recognise the initial standard library.
///
/// This value does not contain standard source, origins, hashes, a digest, or
/// authority to install or use a standard-library snapshot.
#[derive(Clone, Debug)]
pub struct StandardLibraryManifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryManifest {
    /// Returns the standard-library version label.
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_VERSION_IDENTITY
    }

    /// Returns the standard-library revision identity.
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_REVISION_ID
    }

    /// Returns the associated language version label.
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    /// Returns the identity reserved for the later retained source bundle.
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_BUNDLE_ID
    }

    /// Returns the identity reserved for the later retained source revision.
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_REVISION_ID
    }

    /// Returns the identity reserved for the later retained source unit.
    pub const fn source_unit(&self) -> SourceUnitId {
        STANDARD_SOURCE_UNIT_ID
    }

    /// Returns the logical path reserved for the later retained source unit.
    pub const fn source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }

    /// Returns the validated source-independent standard catalogue.
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the accepted source-independent standard manifest.
///
/// This is a `Result` boundary because the core catalogue validates the
/// manifest facts. A failure means that the compiled manifest and the core
/// catalogue contract do not agree.
pub fn standard_library_manifest() -> Result<StandardLibraryManifest, StandardLibraryManifestError>
{
    let schemas = vec![
        SchemaDefinition::new(STD_SCHEMA_ID, semantic_name("std", ["std"])?),
        SchemaDefinition::new(
            STD_TYPES_SCHEMA_ID,
            semantic_name("std.types", ["std", "types"])?,
        ),
    ];
    let mut value_types = Vec::with_capacity(VALUE_TYPE_FACTS.len() + 1);
    for fact in VALUE_TYPE_FACTS {
        value_types.push(ValueTypeDefinition::primitive(
            fact.id,
            semantic_name(
                format!("std.types.{}", fact.local_name),
                ["std", "types", fact.local_name],
            )?,
            ValueTypeMutability::Immutable,
            fact.persistence,
            fact.representation_contract,
        ));
    }
    value_types.push(ValueTypeDefinition::opaque(
        OPAQUE_TOKEN_TYPE_ID,
        semantic_name(
            "std.types.opaque_token",
            ["std", "types", OPAQUE_TOKEN_LOCAL_NAME],
        )?,
        OPAQUE_TOKEN_CONTRACT,
    ));
    let type_bindings = build_type_bindings(&EXPECTED_TYPE_BINDING_IDS)?;
    let catalogue = CatalogueSnapshot::new_with_types(
        STANDARD_CATALOGUE_REVISION_ID,
        schemas,
        Vec::new(),
        value_types,
        type_bindings,
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;

    Ok(StandardLibraryManifest { catalogue })
}

fn build_type_bindings(
    expected_ids: &[[u8; 16]],
) -> Result<Vec<TypeBinding>, StandardLibraryManifestError> {
    let mut bindings = Vec::with_capacity(EXPECTED_TYPE_BINDING_IDS.len());
    for fact in VALUE_TYPE_FACTS {
        let qualified_name =
            semantic_name(format!("std.{}", fact.local_name), ["std", fact.local_name])?;
        let qualified_lookup = TypeLookupName::qualified(qualified_name.clone());
        let binding = TypeBinding::qualified(qualified_name, fact.id).map_err(|source| {
            StandardLibraryManifestError::TypeBinding {
                name: qualified_lookup,
                source,
            }
        })?;
        bindings.push(binding);

        for words in fact.prelude_names {
            let prelude_name = PreludeTypeName::new(words.iter().copied()).map_err(|source| {
                StandardLibraryManifestError::PreludeName {
                    name: words.join(" "),
                    source,
                }
            })?;
            let prelude_lookup = TypeLookupName::prelude(prelude_name.clone());
            let binding = TypeBinding::prelude(prelude_name, fact.id).map_err(|source| {
                StandardLibraryManifestError::TypeBinding {
                    name: prelude_lookup,
                    source,
                }
            })?;
            bindings.push(binding);
        }
    }

    let qualified_name = semantic_name("std.opaque_token", ["std", OPAQUE_TOKEN_LOCAL_NAME])?;
    let qualified_lookup = TypeLookupName::qualified(qualified_name.clone());
    let binding =
        TypeBinding::qualified(qualified_name, OPAQUE_TOKEN_TYPE_ID).map_err(|source| {
            StandardLibraryManifestError::TypeBinding {
                name: qualified_lookup,
                source,
            }
        })?;
    bindings.push(binding);

    validate_binding_identities(&bindings, expected_ids)?;
    Ok(bindings)
}

fn validate_binding_identities(
    bindings: &[TypeBinding],
    expected_ids: &[[u8; 16]],
) -> Result<(), StandardLibraryManifestError> {
    if bindings.len() != expected_ids.len() {
        return Err(StandardLibraryManifestError::TypeBindingCountMismatch {
            expected: expected_ids.len(),
            actual: bindings.len(),
        });
    }

    for (binding, expected_bytes) in bindings.iter().zip(expected_ids) {
        let expected = TypeBindingId::from_bytes(*expected_bytes);
        let actual = binding.id();
        if actual != expected {
            return Err(StandardLibraryManifestError::TypeBindingIdentityMismatch {
                name: binding.name().clone(),
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn semantic_name<const N: usize>(
    name: impl Into<String>,
    parts: [&'static str; N],
) -> Result<QualifiedSemanticName, StandardLibraryManifestError> {
    QualifiedSemanticName::new(parts).map_err(|source| StandardLibraryManifestError::SemanticName {
        name: name.into(),
        source,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StandardLibraryManifestError {
    /// One accepted qualified name is not valid under the core name contract.
    SemanticName {
        /// The accepted manifest spelling.
        name: String,
        /// The core validation error.
        source: SemanticNameError,
    },
    /// One accepted standard-prelude spelling is not a valid keyword name.
    PreludeName {
        /// The accepted keyword spelling.
        name: String,
        /// The core validation error.
        source: PreludeTypeNameError,
    },
    /// One accepted binding cannot be constructed under the core binding contract.
    TypeBinding {
        /// The accepted binding name.
        name: TypeLookupName,
        /// The core validation error.
        source: TypeBindingError,
    },
    /// A derived binding identity does not match the accepted manifest identity.
    TypeBindingIdentityMismatch {
        /// The accepted binding name.
        name: TypeLookupName,
        /// The hard-coded accepted identity.
        expected: TypeBindingId,
        /// The identity derived by the core binding contract.
        actual: TypeBindingId,
    },
    /// The compiled binding facts and identity table have different lengths.
    TypeBindingCountMismatch {
        /// The number of hard-coded accepted identities.
        expected: usize,
        /// The number of binding facts.
        actual: usize,
    },
    /// The accepted facts cannot form a coherent catalogue snapshot.
    Catalogue {
        /// The core catalogue validation error.
        source: CatalogueSnapshotError,
    },
}

impl fmt::Display for StandardLibraryManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SemanticName { name, source } => {
                write!(
                    formatter,
                    "the standard library manifest contains an invalid semantic name {name}: {source}"
                )
            }
            Self::PreludeName { name, source } => {
                write!(
                    formatter,
                    "the standard library manifest contains an invalid prelude name {name}: {source}"
                )
            }
            Self::TypeBinding { name, source } => {
                write!(
                    formatter,
                    "the standard library manifest contains an invalid type binding {name}: {source}"
                )
            }
            Self::TypeBindingIdentityMismatch {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "standard library type binding {name} has identity {actual}, expected {expected}"
            ),
            Self::TypeBindingCountMismatch { expected, actual } => write!(
                formatter,
                "the standard library manifest has {actual} type bindings, expected {expected}"
            ),
            Self::Catalogue { source } => write!(
                formatter,
                "the standard library manifest cannot form a catalogue: {source}"
            ),
        }
    }
}

impl Error for StandardLibraryManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SemanticName { source, .. } => Some(source),
            Self::PreludeName { source, .. } => Some(source),
            Self::TypeBinding { source, .. } => Some(source),
            Self::TypeBindingIdentityMismatch { .. } | Self::TypeBindingCountMismatch { .. } => {
                None
            }
            Self::Catalogue { source } => Some(source),
        }
    }
}

/// An error returned while retaining or verifying the standard-library source.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StandardLibraryError {
    /// The source-independent standard manifest is invalid.
    Manifest {
        /// The manifest construction error.
        source: StandardLibraryManifestError,
    },
    /// The retained source does not exactly match the source-independent manifest.
    RetainedSourceMismatch,
    /// A retained source revision violates a core revision invariant.
    Revision {
        /// The core revision invariant error.
        source: RevisionInvariantError,
    },
    /// A canonical source or standard-library hash cannot be verified.
    CanonicalHash {
        /// The canonical-hash error.
        source: CanonicalHashError,
    },
    /// A snapshot has a catalogue identity other than the reserved identity.
    CatalogueIdentityMismatch {
        /// The reserved standard catalogue identity.
        expected: CatalogueRevisionId,
        /// The catalogue identity retained by the snapshot.
        actual: CatalogueRevisionId,
    },
    /// A snapshot has a digest other than the accepted standard digest.
    AcceptedDigestMismatch {
        /// The hard-coded accepted digest.
        expected: Sha256Digest,
        /// The digest retained by the snapshot.
        actual: Sha256Digest,
    },
    /// The standard library is not installed at the service boundary.
    Unavailable,
    /// No retained, verified standard snapshot is registered for this revision.
    UnsupportedRevision {
        /// The requested standard-library revision.
        revision: StandardLibraryRevisionId,
    },
}

impl fmt::Display for StandardLibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest { source } => {
                write!(
                    formatter,
                    "the standard library manifest is invalid: {source}"
                )
            }
            Self::RetainedSourceMismatch => formatter
                .write_str("the retained standard library source does not match its manifest"),
            Self::Revision { source } => {
                write!(
                    formatter,
                    "the retained standard library revision is invalid: {source}"
                )
            }
            Self::CanonicalHash { source } => {
                write!(
                    formatter,
                    "the standard library canonical hashes are invalid: {source}"
                )
            }
            Self::CatalogueIdentityMismatch { .. } => formatter.write_str(
                "the standard library catalogue identity does not match the reserved identity",
            ),
            Self::AcceptedDigestMismatch { .. } => formatter.write_str(
                "the standard library digest does not match the hard-coded accepted digest",
            ),
            Self::Unavailable => formatter.write_str("the standard library is not installed"),
            Self::UnsupportedRevision { .. } => {
                formatter.write_str("the requested standard library revision is not retained")
            }
        }
    }
}

impl Error for StandardLibraryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest { source } => Some(source),
            Self::Revision { source } => Some(source),
            Self::CanonicalHash { source } => Some(source),
            Self::RetainedSourceMismatch
            | Self::CatalogueIdentityMismatch { .. }
            | Self::AcceptedDigestMismatch { .. }
            | Self::Unavailable
            | Self::UnsupportedRevision { .. } => None,
        }
    }
}

/// A standard-library upgrade prepared for atomic kernel application.
#[derive(Clone, Debug)]
pub struct StandardUpgrade {
    prepared: PreparedStandardUpgrade,
}

impl StandardUpgrade {
    /// Returns the checked standard library retained by this upgrade.
    pub fn checked_standard_library(&self) -> &CheckedStandardLibrary {
        self.prepared.standard_library()
    }

    /// Returns the verified standard snapshot retained by this upgrade.
    pub fn verified_standard_snapshot(&self) -> &VerifiedStandardLibrarySnapshot {
        self.checked_standard_library().verified_snapshot()
    }

    /// Returns the prepared application revision for normal kernel input.
    pub fn application_revision(&self) -> &DeployableRevision {
        self.prepared.application_revision()
    }
}

/// An error returned while preparing a standard-library upgrade.
#[non_exhaustive]
#[derive(Debug)]
pub enum StandardUpgradeError {
    /// Retained standard-library construction or verification failed.
    StandardLibrary {
        /// The standard-library error.
        source: StandardLibraryError,
    },
    /// Compiler standard-source verification failed.
    StandardSource {
        /// The compiler checker error.
        source: StandardLibraryCheckError,
    },
    /// Compiler preparation of the standard upgrade failed.
    Prepare {
        /// The compiler preparation error.
        source: PrepareStandardUpgradeError,
    },
    /// The standard upgrade is registered but its install pipeline is not yet
    /// accepted by this build.
    UnsupportedStandardUpgrade {
        /// The accepted base standard revision of the upgrade.
        from: StandardLibraryRevisionId,
        /// The registered target standard revision of the upgrade.
        to: StandardLibraryRevisionId,
    },
}

impl fmt::Display for StandardUpgradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StandardLibrary { source } => source.fmt(formatter),
            Self::StandardSource { source } => source.fmt(formatter),
            Self::Prepare { source } => source.fmt(formatter),
            Self::UnsupportedStandardUpgrade { from, to } => write!(
                formatter,
                "the {from} to {to} standard upgrade is not supported by this build"
            ),
        }
    }
}

impl Error for StandardUpgradeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::StandardLibrary { source } => Some(source),
            Self::StandardSource { source } => Some(source),
            Self::Prepare { source } => Some(source),
            Self::UnsupportedStandardUpgrade { .. } => None,
        }
    }
}

/// Prepares the accepted standard library for a later atomic kernel upgrade.
pub fn prepare_standard_upgrade(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    prepare_standard_upgrade_with(
        active,
        retained_standard_library_snapshot,
        verify_standard_library_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

fn prepare_standard_upgrade_with<Retain, Verify, Check, Prepare>(
    active: &ActiveDatabaseRevision,
    retain: Retain,
    verify: Verify,
    check: Check,
    prepare: Prepare,
) -> Result<StandardUpgrade, StandardUpgradeError>
where
    Retain: FnOnce() -> Result<StandardLibrarySnapshot, StandardLibraryError>,
    Verify: FnOnce(
        StandardLibrarySnapshot,
    ) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError>,
    Check: FnOnce(
        &VerifiedStandardLibrarySnapshot,
    ) -> Result<CheckedStandardLibrary, StandardLibraryCheckError>,
    Prepare: FnOnce(
        &CheckedStandardLibrary,
        &ActiveDatabaseRevision,
    ) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError>,
{
    let snapshot = retain().map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    let verified =
        verify(snapshot).map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    let checked =
        check(&verified).map_err(|source| StandardUpgradeError::StandardSource { source })?;
    let prepared =
        prepare(&checked, active).map_err(|source| StandardUpgradeError::Prepare { source })?;

    Ok(StandardUpgrade { prepared })
}

/// Retains the canonical standard source as an unverified snapshot.
///
/// This function parses the embedded source directly with `orna_syntax`,
/// reconciles every declaration with the source-independent manifest, and
/// verifies the accepted source and standard-library hash goldens. It does not
/// invoke the compiler and does not grant standard-library authority.
pub fn retained_standard_library_snapshot() -> Result<StandardLibrarySnapshot, StandardLibraryError>
{
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_REVISION_ID,
    })
}

/// Verifies a retained standard snapshot and returns the authority capability.
///
/// The wrapper first checks the reserved catalogue identity, then the accepted
/// standard digest, and only then invokes the core canonical verifier.
pub fn verify_standard_library_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_REVISION_ID,
    })
}

/// Retains the canonical executable standard source as an unverified snapshot.
///
/// This function parses both embedded units directly with `orna_syntax`,
/// reconciles every declaration with the source-independent V2 manifest, and
/// verifies the accepted source and standard-library hash goldens. It builds
/// the one retained `StandardExecutable` through the canonical compiler
/// checker and canonical digest encoders. It does not run the compiler
/// pipeline and does not grant standard-library authority.
pub fn retained_standard_library_v2_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V2_REVISION_ID,
    })
}

/// Verifies a retained executable standard snapshot and returns the authority capability.
///
/// The wrapper first checks the reserved V2 catalogue identity, then the
/// accepted V2 standard digest, and only then invokes the core canonical V2
/// verifier.
pub fn verify_standard_library_v2_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V2_REVISION_ID,
    })
}

/// Retains the canonical output standard source as an unverified snapshot.
///
/// This function parses all three embedded units directly with `orna_syntax`,
/// reconciles every declaration with the source-independent V3 manifest, and
/// verifies the accepted source and standard-library hash goldens. It retains
/// the V2 `std.invoke.echo` executable unchanged through the canonical
/// compiler checker and canonical digest encoders. It does not run the
/// compiler pipeline and does not grant standard-library authority.
pub fn retained_standard_library_v3_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V3_REVISION_ID,
    })
}

/// Verifies a retained output standard snapshot and returns the authority
/// capability.
///
/// The wrapper first checks the reserved V3 catalogue identity, then the
/// accepted V3 standard digest, and only then invokes the core canonical V2
/// verifier. `orna.std/3` reuses the V2 digest contract (work ADR 0058); the
/// V3 catalogue, revision, source, and goldens are all new.
pub fn verify_standard_library_v3_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V3_REVISION_ID,
    })
}

/// Retains the canonical UI standard source as an unverified snapshot.
///
/// This function parses all four embedded units directly with `orna_syntax`,
/// reconciles every declaration with the source-independent V4 manifest, and
/// verifies the accepted source and standard-library hash goldens. It retains
/// the V2 `std.invoke.echo` executable unchanged through the canonical
/// compiler checker and canonical digest encoders (work ADR 0062). It does
/// not run the compiler pipeline and does not grant standard-library
/// authority.
pub fn retained_standard_library_v4_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V4_REVISION_ID,
    })
}

/// Verifies a retained UI standard snapshot and returns the authority
/// capability.
///
/// The wrapper first checks the reserved V4 catalogue identity, then the
/// accepted V4 standard digest, and only then invokes the core canonical V2
/// verifier. `orna.std/4` reuses the V2 digest contract (work ADR 0062); the
/// V4 catalogue, revision, source, and goldens are all new.
pub fn verify_standard_library_v4_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V4_REVISION_ID,
    })
}

/// V5 predates the pinned Orna 1.0 profile and is no longer selectable.
pub fn retained_standard_library_v5_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V5_REVISION_ID,
    })
}

/// Rejects explicitly supplied V5 snapshots after source retirement.
pub fn verify_standard_library_v5_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V5_REVISION_ID,
    })
}

/// Retains the canonical V6 action standard source as an unverified snapshot.
pub fn retained_standard_library_v6_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V6_REVISION_ID,
    })
}

/// Verifies a retained V6 action standard snapshot and returns authority.
pub fn verify_standard_library_v6_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V6_REVISION_ID,
    })
}
/// Rejects caller-supplied historical V7 snapshots after source retirement.
pub fn verify_standard_library_v7_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V7_REVISION_ID,
    })
}

/// V8 Rows source is retired; requesting its historical snapshot fails closed.
pub fn retained_standard_library_v8_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V8_REVISION_ID,
    })
}

/// Rejects supplied V8 Rows snapshots after source retirement.
pub fn verify_standard_library_v8_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V8_REVISION_ID,
    })
}

/// V9 predates the pinned Orna 1.0 standard and is no longer retained.
pub fn retained_standard_library_v9_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V9_REVISION_ID,
    })
}

/// Historical V9 verification fails closed after its source bundle retired.
pub fn verify_standard_library_v9_snapshot(
    _snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision {
        revision: STANDARD_LIBRARY_V9_REVISION_ID,
    })
}

/// Selects and verifies one of the retained standard-library snapshots.
///
/// This is a pinned, fail-closed selection boundary. It does not install a
/// snapshot or mutate an active database revision; the kernel's standard
/// upgrade application remains the authority-granting operation.
pub fn select_verified_standard_library(
    revision: StandardLibraryRevisionId,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    Err(StandardLibraryError::UnsupportedRevision { revision })
}

#[cfg(test)]
fn matches_qualified_export(
    export: &orna_syntax::TypeExportDeclaration,
    expected_source: &QualifiedSemanticName,
    expected_target: TypeId,
    expected_binding: &TypeBinding,
) -> bool {
    if expected_binding.kind() != orna_core::catalogue::TypeBindingKind::Qualified
        || expected_binding.target() != expected_target
        || !matches_qualified_name(&export.source_type, expected_source)
    {
        return false;
    }
    let TypeLookupName::Qualified(expected_target) = expected_binding.name() else {
        return false;
    };
    matches!(
        &export.target,
        TypeExportTarget::Qualified { name } if matches_qualified_name(name, expected_target)
    )
}

#[cfg(test)]
fn matches_prelude_export(
    export: &orna_syntax::TypeExportDeclaration,
    qualified_binding: &TypeBinding,
    prelude_binding: &TypeBinding,
) -> bool {
    let TypeLookupName::Qualified(expected_source) = qualified_binding.name() else {
        return false;
    };
    let TypeLookupName::Prelude(expected_target) = prelude_binding.name() else {
        return false;
    };
    matches_qualified_name(&export.source_type, expected_source)
        && matches!(
            &export.target,
            TypeExportTarget::Prelude { words, .. } if matches_prelude_words(words, expected_target)
        )
}

#[cfg(test)]
fn matches_qualified_name(source: &QualifiedName, expected: &QualifiedSemanticName) -> bool {
    source.parts.len() == expected.parts().len()
        && source
            .parts
            .iter()
            .zip(expected.parts())
            .all(|(part, expected)| is_unquoted(part) && part.text.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
fn matches_prelude_words(source: &[NamePart], expected: &PreludeTypeName) -> bool {
    source.len() == expected.words().len()
        && source
            .iter()
            .zip(expected.words())
            .all(|(word, expected)| is_unquoted(word) && word.text.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
fn is_unquoted(part: &NamePart) -> bool {
    !part.text.starts_with('"')
}

#[cfg(test)]
fn decode_sql_string_literal(literal: &str) -> Option<String> {
    let content = literal.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut decoded = String::with_capacity(content.len());
    let mut characters = content.chars();
    while let Some(character) = characters.next() {
        if character != '\'' {
            decoded.push(character);
            continue;
        }
        if characters.next()? != '\'' {
            return None;
        }
        decoded.push('\'');
    }
    Some(decoded)
}

#[cfg(test)]
fn source_persistence(persistence: PrimitiveValueTypePersistence) -> ValueTypePersistence {
    match persistence {
        PrimitiveValueTypePersistence::Persistable => ValueTypePersistence::Persistable,
        PrimitiveValueTypePersistence::Transient => ValueTypePersistence::Transient,
    }
}

#[cfg(test)]
mod tests;
