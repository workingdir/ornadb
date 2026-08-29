//! Source-independent facts for the Orna standard library.

use std::{error::Error, fmt};

use orna_artifact::client_plan::{ClientExpressionNode, ExpressionClientPlan};
use orna_artifact::server_terminal_table;
use orna_compiler::{
    CheckedStandardLibrary, PrepareStandardUpgradeError, PreparedStandardUpgrade,
    StandardLibraryCheckError, check_standard_library_source, prepare_checked_standard_upgrade,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, SchemaId, SourceBundleId,
    SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeBindingId, TypeId,
    canonical_hash::{
        CanonicalHashError, artifact_payload_digest, calculate_standard_library_digest,
        function_declaration_digest, function_semantic_digest_with_version, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest, standard_library_digest,
        verify_standard_library_snapshot as verify_canonical_standard_library_snapshot,
        verify_standard_library_v2_snapshot as verify_canonical_standard_library_v2_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, FunctionDefinition, FunctionDomain,
        FunctionReturn, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ParameterDefinition, PreludeTypeName, PreludeTypeNameError, QualifiedSemanticName,
        SchemaDefinition, SemanticNameError, TypeBinding, TypeBindingError, TypeLookupName,
        ValueTypeDefinition, ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DeployableRevision,
        ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionInvariantError, Sha256Digest, SourceOrigin,
        StandardExecutable, StandardLibraryDigestVersion, StandardLibrarySnapshot,
        StoredSourceRevision, StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    types::{ResolvedType, StandardScalar},
    value::{
        INSPECT_CARRIER_CODEC_REGISTRATIONS, InspectCarrierCodecRegistration,
        OpaqueCodecRegistration, OpaqueCodecRegistry, OpaqueCodecRegistryError,
    },
};
use orna_syntax::{NamePart, PrimitiveValueTypePersistence, QualifiedName, TypeExportTarget};

pub use orna_compiler::StandardUpgradeIdentity;
pub use orna_compiler::{
    CheckedStandardUiConstructor, CheckedStandardUiWindow, STD_DATA_ROWS_TYPE_BINDING_ID,
    STD_DATA_ROWS_TYPE_ID, STD_DATA_SCHEMA_ID, STD_DATA_SOURCE_UNIT_ID, STD_INTEGER_TYPE_ID,
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID,
    STD_INVOKE_SOURCE_UNIT_ID, STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_SCHEMA_ID, STD_JSON_VALUE_TYPE_ID,
    STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID, STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
    STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID, STD_TYPES_SOURCE_UNIT_ID,
    STD_UI_BUTTON_ENABLED_PARAMETER_ID, STD_UI_BUTTON_FUNCTION_ID,
    STD_UI_BUTTON_FUNCTION_REVISION_ID, STD_UI_BUTTON_LABEL_PARAMETER_ID,
    STD_UI_BUTTON_RUNTIME_CONTRACT, STD_UI_COLUMN_CONTENT_PARAMETER_ID, STD_UI_COLUMN_FUNCTION_ID,
    STD_UI_COLUMN_FUNCTION_REVISION_ID, STD_UI_COLUMN_RUNTIME_CONTRACT,
    STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID, STD_UI_PANEL_CONTENT_PARAMETER_ID,
    STD_UI_PANEL_FUNCTION_ID, STD_UI_PANEL_FUNCTION_REVISION_ID, STD_UI_PANEL_RUNTIME_CONTRACT,
    STD_UI_ROW_CONTENT_PARAMETER_ID, STD_UI_ROW_FUNCTION_ID, STD_UI_ROW_FUNCTION_REVISION_ID,
    STD_UI_ROW_RUNTIME_CONTRACT, STD_UI_TABS_CONTENT_PARAMETER_ID, STD_UI_TABS_FUNCTION_ID,
    STD_UI_TABS_FUNCTION_REVISION_ID, STD_UI_TABS_RUNTIME_CONTRACT, STD_UI_TEXT_FUNCTION_ID,
    STD_UI_TEXT_FUNCTION_REVISION_ID, STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
    STD_UI_TEXT_INPUT_FUNCTION_ID, STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID,
    STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID, STD_UI_TEXT_INPUT_RUNTIME_CONTRACT,
    STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID, STD_UI_TEXT_PARAMETER_ID, STD_UI_TEXT_RUNTIME_CONTRACT,
    STD_UI_WINDOW_CONTENT_PARAMETER_ID, STD_UI_WINDOW_FUNCTION_ID,
    STD_UI_WINDOW_FUNCTION_REVISION_ID, STD_UI_WINDOW_REVISION_NUMBER,
    STD_UI_WINDOW_RUNTIME_CONTRACT, STD_UI_WINDOW_TITLE_PARAMETER_ID, STD_WINDOW_SOURCE_UNIT_ID,
    check_standard_terminal_present_table, check_standard_ui_constructor, check_standard_ui_window,
};
pub use orna_core::inspect::INSPECT_RENDER_CONTRACT;

/// The standard-library version represented by this manifest.
pub const STANDARD_LIBRARY_VERSION_IDENTITY: &str = "orna.std/1";

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

const RETAINED_STANDARD_SOURCE: &str = include_str!("../../../stdlib/std/types.orna");
const ACCEPTED_SOURCE_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x5d, 0x53, 0x60, 0x01, 0xab, 0xc7, 0x54, 0xcf, 0x2c, 0xde, 0x9f, 0xf4, 0xed, 0x50, 0xb2, 0x2d,
    0xe8, 0xbb, 0x70, 0x04, 0x0a, 0x69, 0x1b, 0xc2, 0xec, 0x50, 0xbd, 0x6c, 0x65, 0xe5, 0x25, 0xf4,
]);
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

const RETAINED_STANDARD_INVOKE_SOURCE: &str = include_str!("../../../stdlib/std/invoke.orna");

// The V2 digest goldens below are computed by the canonical encoders from the
// retained source and canonical records (never copied from a handwritten
// encoder). The digest-golden tests recompute every value from the retained
// units and compare against these constants, so any retained-source edit fails
// loudly at build time.
const ACCEPTED_V2_TYPES_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x5d, 0x53, 0x60, 0x01, 0xab, 0xc7, 0x54, 0xcf, 0x2c, 0xde, 0x9f, 0xf4, 0xed, 0x50, 0xb2, 0x2d,
    0xe8, 0xbb, 0x70, 0x04, 0x0a, 0x69, 0x1b, 0xc2, 0xec, 0x50, 0xbd, 0x6c, 0x65, 0xe5, 0x25, 0xf4,
]);
const ACCEPTED_V2_INVOKE_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xb1, 0x9b, 0x95, 0x6b, 0xf6, 0xb2, 0x68, 0x54, 0x93, 0xe2, 0x83, 0x4a, 0xbd, 0x60, 0x35, 0x3a,
    0xbf, 0x70, 0xb7, 0x45, 0xe4, 0x89, 0x4b, 0x9c, 0x66, 0xd2, 0xa7, 0x7e, 0x74, 0x3e, 0xdd, 0xc5,
]);
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
const ACCEPTED_V2_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x65, 0x2a, 0x53, 0x25, 0xc9, 0xd1, 0x1d, 0x33, 0x20, 0x6c, 0x35, 0x1c, 0x0c, 0x5e, 0x8c, 0x3a,
    0x82, 0x2a, 0x5b, 0x9b, 0x72, 0x22, 0x02, 0xb9, 0x3c, 0x25, 0x87, 0x05, 0x1f, 0x0f, 0x46, 0xc2,
]);
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

const RETAINED_STANDARD_OUTPUT_SOURCE: &str = include_str!("../../../stdlib/std/output.orna");

// The V3 digest goldens below are computed by the canonical encoders from the
// retained source and canonical records (never copied from a handwritten
// encoder). The digest-golden tests recompute every value from the retained
// units and compare against these constants, so any retained-source edit fails
// loudly at build time. The V3 artifact and semantic digests are the V2
// goldens because `orna.std/3` retains the exact V2 parameter-echo executable
// unchanged.
const ACCEPTED_V3_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_TYPES_CONTENT_DIGEST;
const ACCEPTED_V3_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_INVOKE_CONTENT_DIGEST;
const ACCEPTED_V3_OUTPUT_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x8f, 0x16, 0x21, 0x4d, 0x9c, 0x4d, 0xee, 0x06, 0x6f, 0x24, 0x7b, 0x24, 0x15, 0xe9, 0xaf, 0xa7,
    0x0f, 0xcf, 0x5f, 0xb2, 0x66, 0x47, 0x3b, 0xb0, 0xfd, 0x6d, 0x72, 0x87, 0x98, 0xa2, 0xaf, 0x35,
]);
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
const ACCEPTED_V3_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V2_ARTIFACT_DIGEST;
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

const RETAINED_STANDARD_UI_SOURCE: &str = include_str!("../../../stdlib/std/ui.orna");

// The V4 digest goldens below are computed by the canonical encoders from the
// retained source and canonical records (never copied from a handwritten
// encoder). The digest-golden tests recompute every value from the retained
// units and compare against these constants, so any retained-source edit fails
// loudly at build time. `orna.std/4` retains the exact V1-V3 types unit, the
// exact V2/V3 invoke unit, and the exact V3 output unit unchanged, so those
// content digests are the earlier goldens; the ui content, V4 bundle, V4
// source revision, V4 artifact, and V4 standard-library digests are computed
// by the canonical encoders.
const ACCEPTED_V4_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_TYPES_CONTENT_DIGEST;
const ACCEPTED_V4_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V2_INVOKE_CONTENT_DIGEST;
const ACCEPTED_V4_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V3_OUTPUT_CONTENT_DIGEST;
const ACCEPTED_V4_UI_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xe0, 0x86, 0x9a, 0xe3, 0xd4, 0x7e, 0xcb, 0xb6, 0x22, 0x30, 0x05, 0xd5, 0x56, 0x8c, 0x39, 0x0f,
    0xad, 0xe2, 0x75, 0x6d, 0x45, 0xde, 0xb9, 0xa1, 0x83, 0x02, 0xd5, 0xe8, 0x4c, 0x2e, 0x5f, 0xd1,
]);
const ACCEPTED_V4_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x3d, 0x26, 0x40, 0x9b, 0x61, 0xab, 0x0f, 0xd5, 0xb7, 0xb3, 0x14, 0xf4, 0x4d, 0x0b, 0xc6, 0x21,
    0xeb, 0xce, 0xa0, 0x76, 0x8d, 0xa6, 0x33, 0xc9, 0x4a, 0xd2, 0x96, 0x4c, 0x99, 0xde, 0xc6, 0xfe,
]);
const ACCEPTED_V4_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xab, 0x6d, 0xba, 0x9d, 0xfc, 0x42, 0x35, 0x39, 0xc8, 0xea, 0x90, 0x55, 0xf5, 0xbf, 0x40, 0x6f,
    0x45, 0xb0, 0xd3, 0x36, 0x2c, 0x06, 0x35, 0x7e, 0x34, 0x13, 0x23, 0x88, 0xff, 0x51, 0x41, 0xdd,
]);
const ACCEPTED_V4_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V3_ARTIFACT_DIGEST;
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

const RETAINED_STANDARD_JSON_SOURCE: &str = include_str!("../../../stdlib/std/json.orna");
const ACCEPTED_V5_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_TYPES_CONTENT_DIGEST;
const ACCEPTED_V5_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_INVOKE_CONTENT_DIGEST;
const ACCEPTED_V5_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_OUTPUT_CONTENT_DIGEST;
const ACCEPTED_V5_UI_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_UI_CONTENT_DIGEST;
const ACCEPTED_V5_JSON_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x4b, 0x12, 0x56, 0xf5, 0x9d, 0x01, 0xe9, 0xec, 0x65, 0x22, 0x85, 0xb1, 0x4f, 0xb8, 0xfc, 0xd5,
    0xde, 0xcf, 0x9b, 0x6d, 0xbf, 0xfb, 0xf7, 0x0d, 0xa8, 0x7a, 0xad, 0xeb, 0xb9, 0xa0, 0x18, 0xbe,
]);
const ACCEPTED_V5_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x87, 0xe2, 0xf5, 0x9d, 0x44, 0x47, 0x78, 0x9f, 0x0f, 0x9d, 0xc5, 0xa9, 0x64, 0xf9, 0xec, 0x20,
    0x1a, 0xfd, 0xdd, 0xe2, 0x8b, 0xe0, 0x7e, 0xd3, 0xd2, 0x37, 0x74, 0xc8, 0x33, 0xf5, 0x31, 0x15,
]);
const ACCEPTED_V5_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x91, 0x2f, 0xb3, 0xb6, 0x6c, 0x28, 0x35, 0xb3, 0x68, 0x68, 0x68, 0x76, 0x75, 0x5d, 0x7c, 0x78,
    0x9c, 0xc3, 0xf2, 0x5c, 0x26, 0x87, 0x27, 0xb0, 0x83, 0xd9, 0x6e, 0x70, 0x7a, 0x99, 0xbc, 0x51,
]);
const ACCEPTED_V5_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V4_ARTIFACT_DIGEST;
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
pub const STD_ACTION_SOURCE_LOGICAL_PATH: &str = "std/action.orna";
pub const STD_ACTION_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(7));
pub const STD_ACTION_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(9));
pub const STD_ACTION_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(20));
pub const STD_ACTION_CONTRACT: &str = "orna.std.value.action@1";
pub const ACTION_MAGIC: &str = "ORNA-ACTION/1 ";

const RETAINED_STANDARD_ACTION_SOURCE: &str = include_str!("../../../stdlib/std/action.orna");
const ACCEPTED_V6_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V5_TYPES_CONTENT_DIGEST;
const ACCEPTED_V6_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V5_INVOKE_CONTENT_DIGEST;
const ACCEPTED_V6_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V5_OUTPUT_CONTENT_DIGEST;
const ACCEPTED_V6_UI_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V5_UI_CONTENT_DIGEST;
const ACCEPTED_V6_JSON_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V5_JSON_CONTENT_DIGEST;
const ACCEPTED_V6_ACTION_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x67, 0x6c, 0xf6, 0x55, 0x53, 0x5c, 0x72, 0xad, 0x0a, 0x2d, 0x1c, 0x51, 0x00, 0x92, 0x31, 0x3c,
    0x2a, 0x9e, 0x1a, 0x8c, 0xe8, 0xaf, 0x04, 0x56, 0xfe, 0xca, 0x28, 0x95, 0xc7, 0x72, 0xef, 0x7a,
]);
const ACCEPTED_V6_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x0d, 0x3e, 0xe1, 0x9e, 0xf8, 0x51, 0xec, 0xbd, 0x9b, 0x8a, 0xe6, 0x6f, 0x49, 0x86, 0xf1, 0x70,
    0x71, 0xb6, 0x3e, 0x6b, 0xf8, 0xff, 0xb4, 0x40, 0xeb, 0x06, 0x88, 0xc3, 0x76, 0xb2, 0xf7, 0x40,
]);
const ACCEPTED_V6_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xba, 0xd0, 0x13, 0x10, 0x53, 0x02, 0xa0, 0x48, 0x04, 0x4e, 0xce, 0xa7, 0x35, 0x1f, 0xde, 0x97,
    0x72, 0x11, 0x9c, 0x3c, 0x95, 0x4c, 0x69, 0x70, 0xcd, 0x03, 0xf2, 0x0e, 0x0f, 0x9e, 0x87, 0xe2,
]);
const ACCEPTED_V6_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V5_ARTIFACT_DIGEST;
const ACCEPTED_V6_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V5_SEMANTIC_DIGEST;
const ACCEPTED_V6_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x79, 0x5c, 0xa1, 0xd8, 0xbb, 0x5b, 0x8a, 0x9c, 0x8e, 0x42, 0xc4, 0x7f, 0x79, 0x0a, 0xc5, 0x47,
    0x27, 0xd9, 0x3f, 0xc3, 0xe8, 0xd5, 0x3e, 0x05, 0xc1, 0x08, 0xbc, 0x4a, 0x53, 0x4f, 0xe0, 0xbd,
]);
/// The standard-library version represented by the V7 manifest (ADR 0019).
pub const STANDARD_LIBRARY_V7_VERSION_IDENTITY: &str = "orna.std/7";
pub const STANDARD_LIBRARY_V7_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(7));
pub const STANDARD_CATALOGUE_V7_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(7));
pub const STANDARD_SOURCE_V7_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(7));
pub const STANDARD_SOURCE_V7_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(7));
pub const STD_WINDOW_SOURCE_LOGICAL_PATH: &str = "std/window.orna";
pub const STD_UI_WINDOW_CONTRACT: &str = STD_UI_WINDOW_RUNTIME_CONTRACT;

const RETAINED_STANDARD_WINDOW_SOURCE: &str = include_str!("../../../stdlib/std/window.orna");
const ACCEPTED_V7_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V6_TYPES_CONTENT_DIGEST;
const ACCEPTED_V7_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V6_INVOKE_CONTENT_DIGEST;
const ACCEPTED_V7_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V6_OUTPUT_CONTENT_DIGEST;
const ACCEPTED_V7_UI_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V6_UI_CONTENT_DIGEST;
const ACCEPTED_V7_JSON_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V6_JSON_CONTENT_DIGEST;
const ACCEPTED_V7_ACTION_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V6_ACTION_CONTENT_DIGEST;
const ACCEPTED_V7_WINDOW_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xcd, 0x58, 0x17, 0xf9, 0x3c, 0x4f, 0x42, 0xb7, 0x27, 0xb1, 0xea, 0xb4, 0x82, 0x8c, 0x1c, 0xf0,
    0xb8, 0xc7, 0xa9, 0x69, 0x42, 0x98, 0x3a, 0xc0, 0x8f, 0xe1, 0x1b, 0xc0, 0xf4, 0x30, 0x78, 0x18,
]);
const ACCEPTED_V7_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x5d, 0x9b, 0x80, 0x91, 0x08, 0x2f, 0xc3, 0xf7, 0xf2, 0xde, 0x47, 0xbe, 0x02, 0xcf, 0x66, 0xa6,
    0xe2, 0x15, 0x35, 0xf8, 0xe7, 0x07, 0x91, 0x8c, 0xfc, 0x5a, 0x2b, 0xb1, 0x2c, 0x03, 0x47, 0x36,
]);
const ACCEPTED_V7_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x74, 0xd7, 0xb9, 0x6e, 0xe3, 0x62, 0x18, 0x8e, 0x35, 0x77, 0x72, 0x68, 0xf2, 0xc5, 0x30, 0x53,
    0xcf, 0x41, 0x5b, 0xcc, 0x5b, 0x7f, 0x36, 0xa9, 0xc4, 0x17, 0x4b, 0xf8, 0xaf, 0x14, 0xad, 0x68,
]);
const ACCEPTED_V7_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V6_ARTIFACT_DIGEST;
const ACCEPTED_V7_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V6_SEMANTIC_DIGEST;
const ACCEPTED_V7_WINDOW_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x34, 0xaf, 0xe2, 0x8b, 0x87, 0x01, 0xbe, 0x44, 0x1e, 0xb7, 0xe8, 0x71, 0x57, 0x95, 0x50, 0x82,
    0xbd, 0x31, 0xce, 0x8d, 0x13, 0xba, 0xdd, 0x2b, 0x70, 0xbf, 0xe6, 0x06, 0x46, 0x54, 0x75, 0x86,
]);
const ACCEPTED_V7_WINDOW_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xd3, 0xa1, 0x28, 0xa4, 0x25, 0x13, 0x1a, 0x15, 0xe6, 0xfd, 0xa3, 0xcf, 0x0f, 0x09, 0x00, 0x36,
    0x2a, 0x6e, 0xb9, 0xc5, 0x30, 0x34, 0x29, 0xa6, 0x57, 0xc1, 0xf2, 0x6b, 0x80, 0xbd, 0x84, 0x13,
]);
const ACCEPTED_V7_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x44, 0xc7, 0x01, 0x00, 0xd2, 0x40, 0xc8, 0xc5, 0x49, 0x40, 0xf4, 0xae, 0x29, 0x13, 0x32, 0x62,
    0x3b, 0x02, 0x36, 0xc2, 0x81, 0x83, 0x6c, 0x21, 0x7b, 0x43, 0x2f, 0xd6, 0xe5, 0x2e, 0x30, 0x8e,
]);
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

const RETAINED_STANDARD_DATA_SOURCE: &str = include_str!("../../../stdlib/std/data.orna");

const ACCEPTED_V8_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_TYPES_CONTENT_DIGEST;
const ACCEPTED_V8_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_INVOKE_CONTENT_DIGEST;
const ACCEPTED_V8_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_OUTPUT_CONTENT_DIGEST;
const ACCEPTED_V8_UI_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_UI_CONTENT_DIGEST;
const ACCEPTED_V8_JSON_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_JSON_CONTENT_DIGEST;
const ACCEPTED_V8_ACTION_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_ACTION_CONTENT_DIGEST;
const ACCEPTED_V8_WINDOW_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_WINDOW_CONTENT_DIGEST;
const ACCEPTED_V8_DATA_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xd6, 0x8b, 0x4b, 0xae, 0x00, 0xc7, 0xe4, 0xa8, 0xc5, 0x0a, 0x8f, 0x32, 0x35, 0x1b, 0x0c, 0x8c,
    0xea, 0x41, 0x08, 0x95, 0xa7, 0xda, 0x7c, 0x8c, 0x90, 0xa4, 0xff, 0x8a, 0xf3, 0x26, 0xda, 0xc3,
]);
const ACCEPTED_V8_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x1b, 0x64, 0x21, 0xb5, 0xbe, 0xc8, 0xaa, 0xe3, 0xdd, 0xc2, 0x08, 0x38, 0x7a, 0xb3, 0xe1, 0xee,
    0x8c, 0xe7, 0xc9, 0x2d, 0x92, 0x5c, 0x41, 0xa4, 0xc0, 0x30, 0x44, 0x57, 0xb7, 0xb8, 0xfd, 0xee,
]);
const ACCEPTED_V8_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x3f, 0x73, 0xa0, 0xaa, 0x79, 0xa9, 0x1b, 0x20, 0x76, 0x8a, 0xb4, 0xa0, 0x44, 0xf8, 0x7e, 0x84,
    0x60, 0xc9, 0x58, 0x85, 0xbe, 0x96, 0x72, 0x1c, 0xbe, 0x12, 0x97, 0x9b, 0xe2, 0x38, 0x4f, 0x59,
]);
const ACCEPTED_V8_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V7_ARTIFACT_DIGEST;
const ACCEPTED_V8_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V7_SEMANTIC_DIGEST;
const ACCEPTED_V8_TABLE_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x33, 0x38, 0x0f, 0x5d, 0x50, 0x5c, 0x31, 0x75, 0x7b, 0xc6, 0x8c, 0x66, 0xf2, 0x0a, 0x4f, 0x13,
    0x9e, 0x9d, 0x61, 0x30, 0xd4, 0x0a, 0xd8, 0xb3, 0x0d, 0x02, 0xc0, 0xfa, 0x44, 0x01, 0x03, 0x94,
]);
const ACCEPTED_V8_TABLE_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x83, 0xa5, 0x6f, 0x9c, 0x3e, 0xc1, 0x2b, 0xb5, 0xa4, 0x08, 0x7f, 0xed, 0x57, 0x09, 0xca, 0xc4,
    0x51, 0xf4, 0x9b, 0xd8, 0x86, 0xff, 0x76, 0xef, 0x9c, 0x75, 0x52, 0xc7, 0x85, 0xc5, 0x4b, 0x46,
]);
const ACCEPTED_V8_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xc8, 0xc1, 0xed, 0x9e, 0xe5, 0x51, 0xe4, 0x52, 0x66, 0xd5, 0x8f, 0x0e, 0xee, 0x38, 0x3d, 0xed,
    0xee, 0x6e, 0x77, 0xa3, 0xfe, 0x25, 0x70, 0xd1, 0x84, 0x39, 0xe8, 0x77, 0xea, 0x6b, 0xf0, 0x68,
]);

/// The standard-library version represented by the V9 manifest (Work ADR 0088).
pub const STANDARD_LIBRARY_V9_VERSION_IDENTITY: &str = "orna.std/9";
pub const STANDARD_LIBRARY_V9_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(9));
pub const STANDARD_CATALOGUE_V9_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(9));
pub const STANDARD_SOURCE_V9_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(9));
pub const STANDARD_SOURCE_V9_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(9));
pub const STD_UI_CONSTRUCTORS_SOURCE_LOGICAL_PATH: &str = "std/ui_constructors.orna";

const RETAINED_STANDARD_UI_CONSTRUCTORS_SOURCE: &str =
    include_str!("../../../stdlib/std/ui_constructors.orna");
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
const RETAINED_STANDARD_CLI_SOURCE: &str = include_str!("../../../stdlib/std/cli.orna");
const ACCEPTED_V10_CLI_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x1e, 0x99, 0xf3, 0x2f, 0xf7, 0xc2, 0xf0, 0x65, 0x4d, 0x67, 0x61, 0xa6, 0xa9, 0xce, 0xd8, 0x26,
    0x95, 0x6c, 0x09, 0x5d, 0xdf, 0xd1, 0x2e, 0xbb, 0xdc, 0xc1, 0x8e, 0xc4, 0xe9, 0xab, 0x19, 0xc4,
]);
const ACCEPTED_V10_CLI_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xb4, 0xa2, 0xd8, 0xcb, 0x75, 0x7e, 0x49, 0x17, 0xcc, 0x1e, 0xe5, 0x1e, 0x92, 0xc4, 0x5f, 0x0a,
    0x68, 0xd6, 0x90, 0xbd, 0x04, 0x68, 0xd2, 0x3c, 0x0e, 0xba, 0x50, 0x35, 0xfd, 0x44, 0x74, 0x47,
]);
const ACCEPTED_V10_CLI_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x20, 0xf8, 0xae, 0xd5, 0x7b, 0x6b, 0xa6, 0x57, 0xbd, 0x29, 0xc1, 0xe9, 0x94, 0x9b, 0x5a, 0xce,
    0x81, 0x31, 0x4d, 0xe7, 0xb0, 0xda, 0x06, 0x28, 0x59, 0xb9, 0x26, 0x34, 0xa6, 0x89, 0x91, 0xf3,
]);
const ACCEPTED_V10_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xb1, 0xf0, 0xdb, 0x01, 0xe1, 0xce, 0x87, 0xd2, 0x2a, 0x2c, 0x60, 0x9d, 0x93, 0xbf, 0xa7, 0x4a,
    0xa4, 0xd8, 0x52, 0xc3, 0x68, 0x8e, 0x35, 0xc2, 0xb1, 0x61, 0x37, 0xd2, 0x83, 0x3c, 0x0b, 0x1c,
]);
const ACCEPTED_V10_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xe9, 0x35, 0x47, 0x5a, 0x86, 0x4d, 0xaa, 0x61, 0x66, 0x01, 0x9a, 0xda, 0xf1, 0x59, 0x86, 0xc4,
    0xee, 0x43, 0x85, 0xcd, 0x8e, 0x1e, 0x64, 0x3e, 0x61, 0x3b, 0x55, 0x3a, 0xb0, 0x6e, 0x04, 0x5a,
]);
const ACCEPTED_V10_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x9b, 0x72, 0xf3, 0x38, 0x8b, 0x46, 0xe5, 0x28, 0x42, 0x5a, 0x84, 0x1c, 0x5b, 0x90, 0x61, 0x65,
    0xf7, 0x31, 0x61, 0xe9, 0x2c, 0x9f, 0x93, 0xe6, 0x01, 0x99, 0x81, 0x76, 0x10, 0x6d, 0xdb, 0xbd,
]);
const ACCEPTED_V9_TYPES_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V8_TYPES_CONTENT_DIGEST;
const ACCEPTED_V9_INVOKE_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V8_INVOKE_CONTENT_DIGEST;
const ACCEPTED_V9_OUTPUT_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V8_OUTPUT_CONTENT_DIGEST;
const ACCEPTED_V9_UI_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V4_UI_CONTENT_DIGEST;
const ACCEPTED_V9_JSON_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V5_JSON_CONTENT_DIGEST;
const ACCEPTED_V9_ACTION_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V6_ACTION_CONTENT_DIGEST;
const ACCEPTED_V9_WINDOW_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V7_WINDOW_CONTENT_DIGEST;
const ACCEPTED_V9_DATA_CONTENT_DIGEST: Sha256Digest = ACCEPTED_V8_DATA_CONTENT_DIGEST;
const ACCEPTED_V9_UI_CONSTRUCTORS_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xdd, 0x5d, 0xc7, 0x93, 0xb0, 0xf8, 0x61, 0x45, 0xdd, 0x3e, 0xd2, 0x05, 0x5a, 0x55, 0x89, 0x82,
    0xc3, 0x42, 0x52, 0xc9, 0xb7, 0xfb, 0x47, 0xe7, 0xb3, 0x72, 0xb8, 0x21, 0x81, 0x73, 0x56, 0x87,
]);
const ACCEPTED_V9_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V8_ARTIFACT_DIGEST;
const ACCEPTED_V9_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V8_SEMANTIC_DIGEST;
const ACCEPTED_V9_TABLE_ARTIFACT_DIGEST: Sha256Digest = ACCEPTED_V8_TABLE_ARTIFACT_DIGEST;
const ACCEPTED_V9_TABLE_SEMANTIC_DIGEST: Sha256Digest = ACCEPTED_V8_TABLE_SEMANTIC_DIGEST;
const ACCEPTED_V9_UI_TEXT_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xde, 0x76, 0xc9, 0xf8, 0x46, 0x9e, 0x49, 0xdf, 0x37, 0xee, 0x4e, 0x89, 0x8d, 0x56, 0x67, 0x40,
    0x8d, 0xb2, 0x7e, 0xba, 0xfe, 0x37, 0x2e, 0xe1, 0xdf, 0xab, 0x34, 0x41, 0x0c, 0xd0, 0x73, 0x51,
]);
const ACCEPTED_V9_UI_TEXT_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xfe, 0x59, 0x16, 0x6c, 0xba, 0xce, 0x28, 0xb8, 0x11, 0x9f, 0xbb, 0x27, 0x6f, 0xdb, 0xa0, 0x8c,
    0x1e, 0xcf, 0xc9, 0xb3, 0x92, 0xd5, 0x72, 0xbc, 0x82, 0xac, 0xbc, 0xea, 0xf7, 0x0c, 0x4f, 0x51,
]);
const ACCEPTED_V9_UI_BUTTON_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x37, 0x70, 0x17, 0x0a, 0xfd, 0xcb, 0x13, 0x74, 0x0c, 0x19, 0xde, 0xe8, 0x32, 0x7d, 0x87, 0xea,
    0x51, 0x01, 0x8b, 0x41, 0xfd, 0xae, 0x61, 0x18, 0xe3, 0x7b, 0x15, 0xbf, 0x2e, 0x17, 0x64, 0x5e,
]);
const ACCEPTED_V9_UI_BUTTON_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x70, 0x56, 0x57, 0x99, 0xc0, 0xed, 0x55, 0xcd, 0xdf, 0x59, 0xf2, 0x51, 0x0f, 0x78, 0xd9, 0x68,
    0x26, 0xd8, 0x9b, 0xe4, 0xb6, 0xaf, 0x24, 0x56, 0x22, 0x12, 0x7e, 0x3c, 0x1f, 0xb4, 0xf4, 0x3f,
]);
const ACCEPTED_V9_UI_PANEL_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xcf, 0xe4, 0x8f, 0x3d, 0x05, 0xc1, 0x28, 0x38, 0x28, 0x78, 0x40, 0x2f, 0xa2, 0x35, 0xf0, 0xcc,
    0x42, 0x97, 0x32, 0x6a, 0x89, 0xff, 0x1a, 0x36, 0x40, 0xbc, 0x10, 0xd2, 0xa9, 0x0d, 0x85, 0x09,
]);
const ACCEPTED_V9_UI_PANEL_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x09, 0x6a, 0xcf, 0x62, 0x31, 0x09, 0x5e, 0xca, 0x7b, 0x21, 0x50, 0x51, 0x03, 0x42, 0x0b, 0x55,
    0xda, 0xcd, 0xfd, 0xb6, 0x52, 0xde, 0x1a, 0x47, 0xcd, 0x06, 0x9a, 0x59, 0x6f, 0x54, 0xe8, 0x14,
]);
const ACCEPTED_V9_UI_ROW_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xfe, 0x9f, 0x16, 0xac, 0x45, 0xd0, 0x99, 0x23, 0x42, 0xf7, 0x8c, 0xf5, 0xb2, 0x7b, 0x69, 0xa9,
    0x17, 0x17, 0x72, 0x7c, 0xa2, 0x64, 0x4a, 0x16, 0x23, 0x34, 0xa1, 0x7a, 0x74, 0x7a, 0xd6, 0x9b,
]);
const ACCEPTED_V9_UI_ROW_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xf5, 0x8e, 0x0b, 0x07, 0x56, 0xa2, 0xa3, 0x10, 0x63, 0xcc, 0xb7, 0x32, 0xb4, 0xe1, 0x1d, 0x32,
    0xfa, 0x69, 0x74, 0x95, 0x12, 0xe4, 0xf7, 0x91, 0x97, 0x56, 0xbf, 0x00, 0x86, 0x7f, 0x58, 0x1d,
]);
const ACCEPTED_V9_UI_COLUMN_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x46, 0x04, 0x29, 0x19, 0xab, 0x2a, 0x30, 0xf9, 0x03, 0x18, 0xf4, 0x81, 0x6e, 0x13, 0x51, 0x42,
    0xe3, 0x4c, 0xe7, 0x61, 0x95, 0xf6, 0x69, 0x7a, 0xe3, 0xaa, 0xd4, 0x5f, 0xf7, 0x2f, 0xb6, 0x20,
]);
const ACCEPTED_V9_UI_COLUMN_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x2f, 0x66, 0x24, 0xec, 0x28, 0x19, 0x31, 0xcc, 0xe8, 0xd5, 0x81, 0x10, 0x28, 0x75, 0xfb, 0xe1,
    0xda, 0x67, 0x20, 0xe3, 0x88, 0x59, 0x68, 0xd3, 0x10, 0x43, 0x48, 0x9d, 0x97, 0xc6, 0xe0, 0x7b,
]);
const ACCEPTED_V9_UI_TEXT_INPUT_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xb4, 0xb8, 0xeb, 0x44, 0x1e, 0x04, 0x38, 0xac, 0x2e, 0xd7, 0x43, 0x14, 0x05, 0x23, 0x1a, 0x18,
    0xa4, 0x68, 0x0c, 0xed, 0x92, 0x3f, 0x47, 0xb3, 0xc2, 0x7d, 0xff, 0x03, 0xed, 0x08, 0x34, 0x87,
]);
const ACCEPTED_V9_UI_TEXT_INPUT_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xe0, 0x71, 0x71, 0x0e, 0x94, 0x94, 0x1c, 0x48, 0x22, 0x89, 0xa9, 0x3f, 0x2a, 0xa7, 0x78, 0x09,
    0xd6, 0xe5, 0xeb, 0x5b, 0xf6, 0xaa, 0xef, 0x0c, 0x90, 0xd0, 0x12, 0x25, 0x48, 0x3c, 0x4a, 0x70,
]);
const ACCEPTED_V9_UI_TABS_ARTIFACT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x12, 0x40, 0x90, 0xf3, 0x7c, 0x20, 0xd9, 0x3a, 0x92, 0xef, 0xf5, 0x91, 0x4e, 0x70, 0x17, 0x25,
    0x04, 0xbc, 0x2b, 0x34, 0xbd, 0xdc, 0xf0, 0xe4, 0x77, 0xe0, 0x6d, 0xfd, 0x8f, 0x0c, 0x48, 0x32,
]);
const ACCEPTED_V9_UI_TABS_SEMANTIC_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x37, 0xaa, 0x1a, 0x41, 0xce, 0x04, 0x6d, 0xd0, 0x18, 0x33, 0x41, 0x98, 0x9d, 0x39, 0x28, 0x19,
    0x8d, 0x0c, 0x01, 0x79, 0x70, 0xbf, 0xb2, 0x42, 0x8d, 0x5e, 0xa2, 0xc5, 0xbb, 0x4b, 0x21, 0x76,
]);
const ACCEPTED_V9_SOURCE_BUNDLE_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x9f, 0x0a, 0xd9, 0xbd, 0x40, 0xc3, 0x6c, 0x8f, 0x20, 0x43, 0x83, 0x04, 0xbd, 0x81, 0xee, 0xfe,
    0x50, 0xec, 0xe4, 0xfd, 0x03, 0x98, 0x62, 0x08, 0xf3, 0x76, 0x77, 0xc6, 0x21, 0x78, 0x48, 0x2e,
]);
const ACCEPTED_V9_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x1b, 0xde, 0x03, 0x54, 0xa9, 0x87, 0x22, 0x4f, 0xf8, 0x0c, 0x01, 0xab, 0xcc, 0xfd, 0xc7, 0x3f,
    0xc8, 0xf3, 0xec, 0x4d, 0xbb, 0x8a, 0xe6, 0x6b, 0x2d, 0x74, 0xdf, 0x2d, 0x69, 0x26, 0x23, 0xa5,
]);
const ACCEPTED_V9_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xe2, 0xf7, 0xb7, 0x73, 0x89, 0x77, 0x04, 0x69, 0xbf, 0xbe, 0x6d, 0x94, 0x84, 0x3c, 0x43, 0x1a,
    0x3d, 0xc4, 0x00, 0x8a, 0x50, 0x52, 0x15, 0xc6, 0xaf, 0xd5, 0x6d, 0x26, 0x07, 0xa5, 0xb7, 0x56,
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

/// The source-independent facts required to recognise the executable
/// `orna.std/2` standard library.
///
/// This value does not contain standard source, origins, hashes, a digest, or
/// authority to install or use a standard-library snapshot.
#[derive(Clone, Debug)]
pub struct StandardLibraryV2Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV2Manifest {
    /// Returns the standard-library version label.
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V2_VERSION_IDENTITY
    }

    /// Returns the standard-library revision identity.
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V2_REVISION_ID
    }

    /// Returns the associated language version label.
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    /// Returns the identity reserved for the later retained V2 source bundle.
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V2_BUNDLE_ID
    }

    /// Returns the identity reserved for the later retained V2 source revision.
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V2_REVISION_ID
    }

    /// Returns the identity of the retained `std/types.orna` unit in the V2 bundle.
    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }

    /// Returns the identity of the retained `std/invoke.orna` unit in the V2 bundle.
    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }

    /// Returns the logical path of the retained `std/types.orna` unit.
    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }

    /// Returns the logical path of the retained `std/invoke.orna` unit.
    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }

    /// Returns the validated source-independent V2 standard catalogue.
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the accepted source-independent executable standard manifest.
///
/// The V2 catalogue extends the V1 catalogue with the `std.invoke` schema and
/// the single `std.invoke.echo` function. It reuses the V1 schemas, value
/// types, and type bindings unchanged; it adds no objects, fields, opaque
/// types, codecs, or system functions.
pub fn standard_library_v2_manifest()
-> Result<StandardLibraryV2Manifest, StandardLibraryManifestError> {
    let version_one = standard_library_manifest()?;
    let mut schemas = version_one.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_INVOKE_SCHEMA_ID,
        semantic_name("std.invoke", ["std", "invoke"])?,
    ));
    let echo = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        semantic_name("std.invoke.echo", ["std", "invoke", "echo"])?,
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_INVOKE_ECHO_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V2_REVISION_ID,
        schemas,
        Vec::new(),
        version_one.catalogue().value_types().to_vec(),
        version_one.catalogue().type_bindings().to_vec(),
        vec![echo],
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;

    Ok(StandardLibraryV2Manifest { catalogue })
}

/// The source-independent facts required to recognise the output
/// `orna.std/3` standard library (work ADR 0058).
///
/// This value does not contain standard source, origins, hashes, a digest, or
/// authority to install or use a standard-library snapshot.
#[derive(Clone, Debug)]
pub struct StandardLibraryV3Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV3Manifest {
    /// Returns the standard-library version label.
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V3_VERSION_IDENTITY
    }

    /// Returns the standard-library revision identity.
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V3_REVISION_ID
    }

    /// Returns the associated language version label.
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    /// Returns the identity reserved for the later retained V3 source bundle.
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V3_BUNDLE_ID
    }

    /// Returns the identity reserved for the later retained V3 source revision.
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V3_REVISION_ID
    }

    /// Returns the identity of the retained `std/types.orna` unit in the V3 bundle.
    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }

    /// Returns the identity of the retained `std/invoke.orna` unit in the V3 bundle.
    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }

    /// Returns the identity of the retained `std/output.orna` unit in the V3 bundle.
    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }

    /// Returns the logical path of the retained `std/types.orna` unit.
    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }

    /// Returns the logical path of the retained `std/invoke.orna` unit.
    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }

    /// Returns the logical path of the retained `std/output.orna` unit.
    pub const fn output_source_logical_path(&self) -> &'static str {
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    }

    /// Returns the validated source-independent V3 standard catalogue.
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the accepted source-independent output standard manifest.
///
/// The V3 catalogue extends the V2 catalogue with the `std.terminal` and
/// `std.io` schemas and the two opaque output value types
/// `std.terminal.document` and `std.io.bytestream`. It reuses the V2 schemas,
/// value types, type bindings, and the single `std.invoke.echo` function
/// unchanged.
pub fn standard_library_v3_manifest()
-> Result<StandardLibraryV3Manifest, StandardLibraryManifestError> {
    let version_two = standard_library_v2_manifest()?;
    let mut schemas = version_two.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_TERMINAL_SCHEMA_ID,
        semantic_name("std.terminal", ["std", "terminal"])?,
    ));
    schemas.push(SchemaDefinition::new(
        STD_IO_SCHEMA_ID,
        semantic_name("std.io", ["std", "io"])?,
    ));
    let mut value_types = version_two.catalogue().value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_TERMINAL_DOCUMENT_TYPE_ID,
        semantic_name("std.terminal.document", ["std", "terminal", "document"])?,
        STD_TERMINAL_DOCUMENT_CONTRACT,
    ));
    value_types.push(ValueTypeDefinition::opaque(
        STD_IO_BYTE_STREAM_TYPE_ID,
        semantic_name("std.io.bytestream", ["std", "io", "bytestream"])?,
        STD_IO_BYTE_STREAM_CONTRACT,
    ));
    let mut type_bindings = version_two.catalogue().type_bindings().to_vec();
    let document_name = semantic_name("std.document", ["std", "document"])?;
    let document_lookup = TypeLookupName::qualified(document_name.clone());
    let document_binding = TypeBinding::qualified(document_name, STD_TERMINAL_DOCUMENT_TYPE_ID)
        .map_err(|source| StandardLibraryManifestError::TypeBinding {
            name: document_lookup,
            source,
        })?;
    type_bindings.push(document_binding);
    let bytestream_name = semantic_name("std.bytestream", ["std", "bytestream"])?;
    let bytestream_lookup = TypeLookupName::qualified(bytestream_name.clone());
    let bytestream_binding = TypeBinding::qualified(bytestream_name, STD_IO_BYTE_STREAM_TYPE_ID)
        .map_err(|source| StandardLibraryManifestError::TypeBinding {
            name: bytestream_lookup,
            source,
        })?;
    type_bindings.push(bytestream_binding);
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V3_REVISION_ID,
        schemas,
        Vec::new(),
        value_types,
        type_bindings,
        version_two.catalogue().functions().to_vec(),
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;

    Ok(StandardLibraryV3Manifest { catalogue })
}

/// The source-independent facts required to recognise the UI
/// `orna.std/4` standard library (work ADR 0062).
///
/// This value does not contain standard source, origins, hashes, a digest, or
/// authority to install or use a standard-library snapshot.
#[derive(Clone, Debug)]
pub struct StandardLibraryV4Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV4Manifest {
    /// Returns the standard-library version label.
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V4_VERSION_IDENTITY
    }

    /// Returns the standard-library revision identity.
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V4_REVISION_ID
    }

    /// Returns the associated language version label.
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    /// Returns the identity reserved for the later retained V4 source bundle.
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V4_BUNDLE_ID
    }

    /// Returns the identity reserved for the later retained V4 source revision.
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V4_REVISION_ID
    }

    /// Returns the identity of the retained `std/types.orna` unit in the V4 bundle.
    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }

    /// Returns the identity of the retained `std/invoke.orna` unit in the V4 bundle.
    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }

    /// Returns the identity of the retained `std/output.orna` unit in the V4 bundle.
    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }

    /// Returns the identity of the retained `std/ui.orna` unit in the V4 bundle.
    pub const fn ui_source_unit(&self) -> SourceUnitId {
        STD_UI_SOURCE_UNIT_ID
    }

    /// Returns the logical path of the retained `std/types.orna` unit.
    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }

    /// Returns the logical path of the retained `std/invoke.orna` unit.
    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }

    /// Returns the logical path of the retained `std/output.orna` unit.
    pub const fn output_source_logical_path(&self) -> &'static str {
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    }

    /// Returns the logical path of the retained `std/ui.orna` unit.
    pub const fn ui_source_logical_path(&self) -> &'static str {
        STD_UI_SOURCE_LOGICAL_PATH
    }

    /// Returns the validated source-independent V4 standard catalogue.
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the accepted source-independent UI standard manifest.
///
/// The V4 catalogue extends the V3 catalogue with the `std.ui` schema, the
/// opaque UI value type `std.ui.ui` (contract `orna.std.value.ui@1`), and the
/// single `std.ui` type binding targeting `std.ui.UI`. It reuses the V3
/// schemas, value types, type bindings, and the single `std.invoke.echo`
/// function unchanged (work ADR 0062).
pub fn standard_library_v4_manifest()
-> Result<StandardLibraryV4Manifest, StandardLibraryManifestError> {
    let version_three = standard_library_v3_manifest()?;
    let mut schemas = version_three.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_UI_SCHEMA_ID,
        semantic_name("std.ui", ["std", "ui"])?,
    ));
    let mut value_types = version_three.catalogue().value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_UI_TYPE_ID,
        semantic_name("std.ui.ui", ["std", "ui", "ui"])?,
        STD_UI_CONTRACT,
    ));
    let mut type_bindings = version_three.catalogue().type_bindings().to_vec();
    let ui_name = semantic_name("std.ui", ["std", "ui"])?;
    let ui_lookup = TypeLookupName::qualified(ui_name.clone());
    let ui_binding = TypeBinding::qualified(ui_name, STD_UI_TYPE_ID).map_err(|source| {
        StandardLibraryManifestError::TypeBinding {
            name: ui_lookup,
            source,
        }
    })?;
    type_bindings.push(ui_binding);
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V4_REVISION_ID,
        schemas,
        Vec::new(),
        value_types,
        type_bindings,
        version_three.catalogue().functions().to_vec(),
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;

    Ok(StandardLibraryV4Manifest { catalogue })
}

/// The source-independent facts required to recognise the JSON `orna.std/5`
/// standard library (ADR 0075).
#[derive(Clone, Debug)]
pub struct StandardLibraryV5Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV5Manifest {
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V5_VERSION_IDENTITY
    }
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V5_REVISION_ID
    }
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V5_BUNDLE_ID
    }
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V5_REVISION_ID
    }
    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }
    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }
    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }
    pub const fn ui_source_unit(&self) -> SourceUnitId {
        STD_UI_SOURCE_UNIT_ID
    }
    pub const fn json_source_unit(&self) -> SourceUnitId {
        STD_JSON_SOURCE_UNIT_ID
    }
    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }
    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }
    pub const fn output_source_logical_path(&self) -> &'static str {
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    }
    pub const fn ui_source_logical_path(&self) -> &'static str {
        STD_UI_SOURCE_LOGICAL_PATH
    }
    pub const fn json_source_logical_path(&self) -> &'static str {
        STD_JSON_SOURCE_LOGICAL_PATH
    }
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the append-only V5 catalogue over V4.
pub fn standard_library_v5_manifest()
-> Result<StandardLibraryV5Manifest, StandardLibraryManifestError> {
    let version_four = standard_library_v4_manifest()?;
    let mut schemas = version_four.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_JSON_SCHEMA_ID,
        semantic_name("std.json", ["std", "json"])?,
    ));
    let mut value_types = version_four.catalogue().value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_JSON_VALUE_TYPE_ID,
        semantic_name("std.json.value", ["std", "json", "value"])?,
        STD_JSON_CONTRACT,
    ));
    let mut type_bindings = version_four.catalogue().type_bindings().to_vec();
    let json_name = semantic_name("std.jsonvalue", ["std", "jsonvalue"])?;
    let json_lookup = TypeLookupName::qualified(json_name.clone());
    let json_binding =
        TypeBinding::qualified(json_name, STD_JSON_VALUE_TYPE_ID).map_err(|source| {
            StandardLibraryManifestError::TypeBinding {
                name: json_lookup,
                source,
            }
        })?;
    type_bindings.push(json_binding);
    let json_encode = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        semantic_name("std.json.encode", ["std", "json", "encode"])?,
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_JSON_ENCODE_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::value(STD_JSON_VALUE_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::value(STD_IO_BYTE_STREAM_TYPE_ID)),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let mut functions = version_four.catalogue().functions().to_vec();
    functions.push(json_encode);
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V5_REVISION_ID,
        schemas,
        Vec::new(),
        value_types,
        type_bindings,
        functions,
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;
    Ok(StandardLibraryV5Manifest { catalogue })
}

/// The source-independent facts required to recognise the `orna.std/6`
/// standard library (ADR 0079).
#[derive(Clone, Debug)]
pub struct StandardLibraryV6Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV6Manifest {
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V6_VERSION_IDENTITY
    }
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V6_REVISION_ID
    }
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V6_BUNDLE_ID
    }
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V6_REVISION_ID
    }
    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }
    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }
    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }
    pub const fn ui_source_unit(&self) -> SourceUnitId {
        STD_UI_SOURCE_UNIT_ID
    }
    pub const fn json_source_unit(&self) -> SourceUnitId {
        STD_JSON_SOURCE_UNIT_ID
    }
    pub const fn action_source_unit(&self) -> SourceUnitId {
        STD_ACTION_SOURCE_UNIT_ID
    }
    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }
    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }
    pub const fn output_source_logical_path(&self) -> &'static str {
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    }
    pub const fn ui_source_logical_path(&self) -> &'static str {
        STD_UI_SOURCE_LOGICAL_PATH
    }
    pub const fn json_source_logical_path(&self) -> &'static str {
        STD_JSON_SOURCE_LOGICAL_PATH
    }
    pub const fn action_source_logical_path(&self) -> &'static str {
        STD_ACTION_SOURCE_LOGICAL_PATH
    }
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the append-only V6 catalogue over V5.
pub fn standard_library_v6_manifest()
-> Result<StandardLibraryV6Manifest, StandardLibraryManifestError> {
    let version_five = standard_library_v5_manifest()?;
    let mut schemas = version_five.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_ACTION_SCHEMA_ID,
        semantic_name("std.action", ["std", "action"])?,
    ));
    let mut value_types = version_five.catalogue().value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_ACTION_TYPE_ID,
        semantic_name("std.action.action", ["std", "action", "action"])?,
        STD_ACTION_CONTRACT,
    ));
    let mut type_bindings = version_five.catalogue().type_bindings().to_vec();
    let action_name = semantic_name("std.action", ["std", "action"])?;
    let action_lookup = TypeLookupName::qualified(action_name.clone());
    let action_binding =
        TypeBinding::qualified(action_name, STD_ACTION_TYPE_ID).map_err(|source| {
            StandardLibraryManifestError::TypeBinding {
                name: action_lookup,
                source,
            }
        })?;
    type_bindings.push(action_binding);
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V6_REVISION_ID,
        schemas,
        Vec::new(),
        value_types,
        type_bindings,
        version_five.catalogue().functions().to_vec(),
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;
    Ok(StandardLibraryV6Manifest { catalogue })
}
/// The source-independent facts required to recognise the `orna.std/7`
/// standard library (ADR 0019).
#[derive(Clone, Debug)]
pub struct StandardLibraryV7Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV7Manifest {
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V7_VERSION_IDENTITY
    }
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V7_REVISION_ID
    }
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V7_BUNDLE_ID
    }
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V7_REVISION_ID
    }
    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }
    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }
    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }
    pub const fn ui_source_unit(&self) -> SourceUnitId {
        STD_UI_SOURCE_UNIT_ID
    }
    pub const fn json_source_unit(&self) -> SourceUnitId {
        STD_JSON_SOURCE_UNIT_ID
    }
    pub const fn action_source_unit(&self) -> SourceUnitId {
        STD_ACTION_SOURCE_UNIT_ID
    }
    pub const fn window_source_unit(&self) -> SourceUnitId {
        STD_WINDOW_SOURCE_UNIT_ID
    }
    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }
    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }
    pub const fn output_source_logical_path(&self) -> &'static str {
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    }
    pub const fn ui_source_logical_path(&self) -> &'static str {
        STD_UI_SOURCE_LOGICAL_PATH
    }
    pub const fn json_source_logical_path(&self) -> &'static str {
        STD_JSON_SOURCE_LOGICAL_PATH
    }
    pub const fn action_source_logical_path(&self) -> &'static str {
        STD_ACTION_SOURCE_LOGICAL_PATH
    }
    pub const fn window_source_logical_path(&self) -> &'static str {
        STD_WINDOW_SOURCE_LOGICAL_PATH
    }
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the append-only V7 catalogue over V6.
pub fn standard_library_v7_manifest()
-> Result<StandardLibraryV7Manifest, StandardLibraryManifestError> {
    let version_six = standard_library_v6_manifest()?;
    let mut functions = version_six.catalogue().functions().to_vec();
    functions.push(FunctionDefinition::new(
        STD_UI_WINDOW_FUNCTION_ID,
        semantic_name("std.ui.window", ["std", "ui", "window"])?,
        FunctionDomain::Client,
        vec![
            ParameterDefinition::new(
                STD_UI_WINDOW_TITLE_PARAMETER_ID,
                "title",
                0,
                ResolvedType::value(CHARACTER_LARGE_OBJECT_TYPE_ID),
                None,
            ),
            ParameterDefinition::new(
                STD_UI_WINDOW_CONTENT_PARAMETER_ID,
                "content",
                1,
                ResolvedType::value(STD_UI_TYPE_ID),
                None,
            ),
        ],
        FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
        STD_UI_WINDOW_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    ));
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V7_REVISION_ID,
        version_six.catalogue().schemas().to_vec(),
        Vec::new(),
        version_six.catalogue().value_types().to_vec(),
        version_six.catalogue().type_bindings().to_vec(),
        functions,
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;
    Ok(StandardLibraryV7Manifest { catalogue })
}
/// The source-independent facts required to recognise the `orna.std/8`
/// standard library (Work ADR 0087).
#[derive(Clone, Debug)]
pub struct StandardLibraryV8Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV8Manifest {
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V8_VERSION_IDENTITY
    }

    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V8_REVISION_ID
    }

    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V8_BUNDLE_ID
    }

    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V8_REVISION_ID
    }

    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }

    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }

    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }

    pub const fn ui_source_unit(&self) -> SourceUnitId {
        STD_UI_SOURCE_UNIT_ID
    }

    pub const fn json_source_unit(&self) -> SourceUnitId {
        STD_JSON_SOURCE_UNIT_ID
    }

    pub const fn action_source_unit(&self) -> SourceUnitId {
        STD_ACTION_SOURCE_UNIT_ID
    }

    pub const fn window_source_unit(&self) -> SourceUnitId {
        STD_WINDOW_SOURCE_UNIT_ID
    }

    pub const fn data_source_unit(&self) -> SourceUnitId {
        STD_DATA_SOURCE_UNIT_ID
    }

    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }

    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }

    pub const fn output_source_logical_path(&self) -> &'static str {
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    }

    pub const fn ui_source_logical_path(&self) -> &'static str {
        STD_UI_SOURCE_LOGICAL_PATH
    }

    pub const fn json_source_logical_path(&self) -> &'static str {
        STD_JSON_SOURCE_LOGICAL_PATH
    }

    pub const fn action_source_logical_path(&self) -> &'static str {
        STD_ACTION_SOURCE_LOGICAL_PATH
    }

    pub const fn window_source_logical_path(&self) -> &'static str {
        STD_WINDOW_SOURCE_LOGICAL_PATH
    }

    pub const fn data_source_logical_path(&self) -> &'static str {
        STD_DATA_SOURCE_LOGICAL_PATH
    }

    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the append-only V8 catalogue over V7.
pub fn standard_library_v8_manifest()
-> Result<StandardLibraryV8Manifest, StandardLibraryManifestError> {
    let version_seven = standard_library_v7_manifest()?;
    let mut schemas = version_seven.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_DATA_SCHEMA_ID,
        semantic_name("std.data", ["std", "data"])?,
    ));
    let mut value_types = version_seven.catalogue().value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_DATA_ROWS_TYPE_ID,
        semantic_name(STD_DATA_ROWS_SEMANTIC_NAME, ["std", "data", "rows"])?,
        STD_DATA_ROWS_CONTRACT,
    ));
    let mut type_bindings = version_seven.catalogue().type_bindings().to_vec();
    let rows_binding_name = semantic_name(STD_DATA_ROWS_EXPORT_NAME, ["std", "rows"])?;
    let rows_binding_lookup = TypeLookupName::qualified(rows_binding_name.clone());
    let rows_binding =
        TypeBinding::qualified(rows_binding_name, STD_DATA_ROWS_TYPE_ID).map_err(|source| {
            StandardLibraryManifestError::TypeBinding {
                name: rows_binding_lookup.clone(),
                source,
            }
        })?;
    if rows_binding.id() != STD_DATA_ROWS_TYPE_BINDING_ID {
        return Err(StandardLibraryManifestError::TypeBindingIdentityMismatch {
            name: rows_binding_lookup,
            expected: STD_DATA_ROWS_TYPE_BINDING_ID,
            actual: rows_binding.id(),
        });
    }
    type_bindings.push(rows_binding);
    let mut functions = version_seven.catalogue().functions().to_vec();
    functions.push(FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        semantic_name(
            "std.terminal.present_table",
            ["std", "terminal", "present_table"],
        )?,
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            "p_rows",
            0,
            ResolvedType::value(STD_DATA_ROWS_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::value(STD_TERMINAL_DOCUMENT_TYPE_ID)),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    ));
    functions.sort_by_key(|function| function.id());
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V8_REVISION_ID,
        schemas,
        Vec::new(),
        value_types,
        type_bindings,
        functions,
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;
    Ok(StandardLibraryV8Manifest { catalogue })
}

/// The source-independent facts required to recognise the `orna.std/9`
/// standard library (Work ADR 0088).
#[derive(Clone, Debug)]
pub struct StandardLibraryV9Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV9Manifest {
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V9_VERSION_IDENTITY
    }

    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V9_REVISION_ID
    }

    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V9_BUNDLE_ID
    }

    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V9_REVISION_ID
    }

    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }

    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }

    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }

    pub const fn ui_source_unit(&self) -> SourceUnitId {
        STD_UI_SOURCE_UNIT_ID
    }

    pub const fn json_source_unit(&self) -> SourceUnitId {
        STD_JSON_SOURCE_UNIT_ID
    }

    pub const fn action_source_unit(&self) -> SourceUnitId {
        STD_ACTION_SOURCE_UNIT_ID
    }

    pub const fn window_source_unit(&self) -> SourceUnitId {
        STD_WINDOW_SOURCE_UNIT_ID
    }

    pub const fn data_source_unit(&self) -> SourceUnitId {
        STD_DATA_SOURCE_UNIT_ID
    }

    pub const fn ui_constructors_source_unit(&self) -> SourceUnitId {
        STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID
    }

    pub const fn types_source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }

    pub const fn invoke_source_logical_path(&self) -> &'static str {
        STD_INVOKE_SOURCE_LOGICAL_PATH
    }

    pub const fn output_source_logical_path(&self) -> &'static str {
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    }

    pub const fn ui_source_logical_path(&self) -> &'static str {
        STD_UI_SOURCE_LOGICAL_PATH
    }

    pub const fn json_source_logical_path(&self) -> &'static str {
        STD_JSON_SOURCE_LOGICAL_PATH
    }

    pub const fn action_source_logical_path(&self) -> &'static str {
        STD_ACTION_SOURCE_LOGICAL_PATH
    }

    pub const fn window_source_logical_path(&self) -> &'static str {
        STD_WINDOW_SOURCE_LOGICAL_PATH
    }

    pub const fn data_source_logical_path(&self) -> &'static str {
        STD_DATA_SOURCE_LOGICAL_PATH
    }

    pub const fn ui_constructors_source_logical_path(&self) -> &'static str {
        STD_UI_CONSTRUCTORS_SOURCE_LOGICAL_PATH
    }
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the append-only V9 catalogue over V8.
pub fn standard_library_v9_manifest()
-> Result<StandardLibraryV9Manifest, StandardLibraryManifestError> {
    let version_eight = standard_library_v8_manifest()?;
    let mut functions = version_eight.catalogue().functions().to_vec();
    functions.extend([
        FunctionDefinition::new(
            STD_UI_TEXT_FUNCTION_ID,
            semantic_name("std.ui.text", ["std", "ui", "text"])?,
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                STD_UI_TEXT_PARAMETER_ID,
                "text",
                0,
                ResolvedType::value(CHARACTER_LARGE_OBJECT_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
            STD_UI_TEXT_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        ),
        FunctionDefinition::new(
            STD_UI_BUTTON_FUNCTION_ID,
            semantic_name("std.ui.button", ["std", "ui", "button"])?,
            FunctionDomain::Client,
            vec![
                ParameterDefinition::new(
                    STD_UI_BUTTON_LABEL_PARAMETER_ID,
                    "label",
                    0,
                    ResolvedType::value(CHARACTER_LARGE_OBJECT_TYPE_ID),
                    None,
                ),
                ParameterDefinition::new(
                    STD_UI_BUTTON_ENABLED_PARAMETER_ID,
                    "enabled",
                    1,
                    ResolvedType::value(BOOLEAN_TYPE_ID),
                    None,
                ),
            ],
            FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
            STD_UI_BUTTON_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        ),
        FunctionDefinition::new(
            STD_UI_PANEL_FUNCTION_ID,
            semantic_name("std.ui.panel", ["std", "ui", "panel"])?,
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                STD_UI_PANEL_CONTENT_PARAMETER_ID,
                "content",
                0,
                ResolvedType::value(STD_UI_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
            STD_UI_PANEL_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        ),
        FunctionDefinition::new(
            STD_UI_ROW_FUNCTION_ID,
            semantic_name("std.ui.row", ["std", "ui", "row"])?,
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                STD_UI_ROW_CONTENT_PARAMETER_ID,
                "content",
                0,
                ResolvedType::value(STD_UI_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
            STD_UI_ROW_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        ),
        FunctionDefinition::new(
            STD_UI_COLUMN_FUNCTION_ID,
            semantic_name("std.ui.column", ["std", "ui", "column"])?,
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                STD_UI_COLUMN_CONTENT_PARAMETER_ID,
                "content",
                0,
                ResolvedType::value(STD_UI_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
            STD_UI_COLUMN_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        ),
        FunctionDefinition::new(
            STD_UI_TEXT_INPUT_FUNCTION_ID,
            semantic_name("std.ui.text_input", ["std", "ui", "text_input"])?,
            FunctionDomain::Client,
            vec![
                ParameterDefinition::new(
                    STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
                    "text",
                    0,
                    ResolvedType::value(CHARACTER_LARGE_OBJECT_TYPE_ID),
                    None,
                ),
                ParameterDefinition::new(
                    STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
                    "placeholder",
                    1,
                    ResolvedType::value(CHARACTER_LARGE_OBJECT_TYPE_ID),
                    None,
                ),
                ParameterDefinition::new(
                    STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
                    "enabled",
                    2,
                    ResolvedType::value(BOOLEAN_TYPE_ID),
                    None,
                ),
            ],
            FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
            STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        ),
        FunctionDefinition::new(
            STD_UI_TABS_FUNCTION_ID,
            semantic_name("std.ui.tabs", ["std", "ui", "tabs"])?,
            FunctionDomain::Client,
            vec![ParameterDefinition::new(
                STD_UI_TABS_CONTENT_PARAMETER_ID,
                "content",
                0,
                ResolvedType::value(STD_UI_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
            STD_UI_TABS_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        ),
    ]);
    functions.sort_by_key(|function| function.id());
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V9_REVISION_ID,
        version_eight.catalogue().schemas().to_vec(),
        Vec::new(),
        version_eight.catalogue().value_types().to_vec(),
        version_eight.catalogue().type_bindings().to_vec(),
        functions,
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;
    Ok(StandardLibraryV9Manifest { catalogue })
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

/// An error returned when the compiled standard facts cannot form a valid manifest.
#[non_exhaustive]
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
            | Self::Unavailable => None,
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

/// Prepares the append-only `orna.std/1` to `orna.std/2` standard upgrade.
///
/// This is the only path that selects `orna.std/2`. It fails closed when the
/// active revision already pins any standard snapshot (which includes V2),
/// when the active revision is not the empty expected base, or when the
/// immutable V1 snapshot cannot be retained and verified first. It retains
/// and verifies V1 before it prepares V2 so a fresh database can persist V1
/// as retained historical standard state; it never modifies V1 semantics.
pub fn prepare_standard_upgrade_v1_to_v2(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    if let Some(installed) = active.catalogue_hash_context().standard() {
        return Err(StandardUpgradeError::Prepare {
            source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: installed.revision(),
            },
        });
    }

    let version_one = retained_standard_library_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_snapshot(version_one)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;

    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v2_snapshot,
        verify_standard_library_v2_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

/// Prepares the append-only `orna.std/2` to `orna.std/3` standard upgrade
/// (work ADR 0059).
///
/// This is the only path that selects `orna.std/3`. It fails closed unless
/// the active revision pins exactly `orna.std/2` (an absent parent or a wrong
/// installed revision). It retains and verifies the
/// immutable `orna.std/2` parent snapshot before it prepares V3: V3 is the
/// append-only child, so the parent must be present and coherent; the
/// PostgreSQL apply path persists the parent alongside the child in the same
/// activation transaction. It then retains and verifies V3, checks the V3
/// snapshot with the compiler's V3 branch, and prepares the companion
/// application revision through the shared `prepare_checked_standard_upgrade`
/// machinery, exactly as the V1-to-V2 path does.
pub fn prepare_standard_upgrade_v2_to_v3(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V2_REVISION_ID)?;

    let version_two = retained_standard_library_v2_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v2_snapshot(version_two)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;

    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v3_snapshot,
        verify_standard_library_v3_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

/// Prepares the append-only `orna.std/3` to `orna.std/4` standard upgrade
/// (work ADR 0062).
///
/// This is the only path that selects `orna.std/4`. It fails closed unless
/// the active revision pins exactly `orna.std/3` (an absent parent or a wrong
/// installed revision). It retains and verifies the
/// immutable `orna.std/3` parent snapshot before it prepares V4: V4 is the
/// append-only child, so the parent must be present and coherent; the
/// PostgreSQL apply path persists the parent alongside the child in the same
/// activation transaction. It then retains and verifies V4, checks the V4
/// snapshot with the compiler's V4 branch, and prepares the companion
/// application revision through the shared `prepare_checked_standard_upgrade`
/// machinery, exactly as the V2-to-V3 path does.
pub fn prepare_standard_upgrade_v3_to_v4(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V3_REVISION_ID)?;

    let version_three = retained_standard_library_v3_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v3_snapshot(version_three)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;

    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v4_snapshot,
        verify_standard_library_v4_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

/// Prepares the append-only `orna.std/4` to `orna.std/5` standard upgrade
/// (ADR 0075). It fails closed unless `orna.std/4` is the installed parent;
/// the retained V4 parent is verified before the V5 child.
pub fn prepare_standard_upgrade_v4_to_v5(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V4_REVISION_ID)?;

    let version_four = retained_standard_library_v4_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v4_snapshot(version_four)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v5_snapshot,
        verify_standard_library_v5_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

/// Prepares the append-only `orna.std/5` to `orna.std/6` standard upgrade
/// (ADR 0079). It fails closed unless `orna.std/5` is the installed parent;
/// the retained V5 parent is verified before the V6 child.
pub fn prepare_standard_upgrade_v5_to_v6(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V5_REVISION_ID)?;

    let version_five = retained_standard_library_v5_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v5_snapshot(version_five)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v6_snapshot,
        verify_standard_library_v6_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}
/// Prepares the append-only `orna.std/6` to `orna.std/7` standard upgrade
/// (ADR 0019). It fails closed unless `orna.std/6` is the installed parent;
/// the retained V6 parent is verified before the V7 child.
pub fn prepare_standard_upgrade_v6_to_v7(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V6_REVISION_ID)?;

    let version_six = retained_standard_library_v6_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v6_snapshot(version_six)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v7_snapshot,
        verify_standard_library_v7_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}
/// Prepares the append-only `orna.std/7` to `orna.std/8` standard upgrade
/// (Work ADR 0087). It fails closed unless `orna.std/7` is the installed
/// parent; the retained V7 parent is verified before the V8 child.
pub fn prepare_standard_upgrade_v7_to_v8(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V7_REVISION_ID)?;

    let version_seven = retained_standard_library_v7_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v7_snapshot(version_seven)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v8_snapshot,
        verify_standard_library_v8_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

/// Prepares the append-only `orna.std/8` to `orna.std/9` standard upgrade
/// (Work ADR 0088). It fails closed unless `orna.std/8` is the installed
/// parent; the retained V8 Rows snapshot is verified before the V9 child.
pub fn prepare_standard_upgrade_v8_to_v9(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V8_REVISION_ID)?;

    let version_eight = retained_standard_library_v8_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v8_snapshot(version_eight)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v9_snapshot,
        verify_standard_library_v9_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

/// Prepares the append-only `orna.std/9` to `orna.std/10` standard upgrade.
pub fn prepare_standard_upgrade_v9_to_v10(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    require_standard_upgrade_parent(active, STANDARD_LIBRARY_V9_REVISION_ID)?;
    let version_nine = retained_standard_library_v9_snapshot()
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    verify_standard_library_v9_snapshot(version_nine)
        .map_err(|source| StandardUpgradeError::StandardLibrary { source })?;
    prepare_standard_upgrade_with(
        active,
        retained_standard_library_v10_snapshot,
        verify_standard_library_v10_snapshot,
        check_standard_library_source,
        prepare_checked_standard_upgrade,
    )
}

fn require_standard_upgrade_parent(
    active: &ActiveDatabaseRevision,
    expected: StandardLibraryRevisionId,
) -> Result<(), StandardUpgradeError> {
    match active.catalogue_hash_context().standard() {
        Some(installed) if installed.revision() == expected => Ok(()),
        Some(installed) => Err(StandardUpgradeError::Prepare {
            source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: installed.revision(),
            },
        }),
        None => Err(StandardUpgradeError::StandardLibrary {
            source: StandardLibraryError::Unavailable,
        }),
    }
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
    retained_standard_library_snapshot_from_source(RETAINED_STANDARD_SOURCE)
}

/// Verifies a retained standard snapshot and returns the authority capability.
///
/// The wrapper first checks the reserved catalogue identity, then the accepted
/// standard digest, and only then invokes the core canonical verifier.
pub fn verify_standard_library_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_REVISION_ID,
            actual: actual_catalogue,
        });
    }

    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }

    verify_canonical_standard_library_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
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
    retained_standard_library_v2_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
    )
}

/// Verifies a retained executable standard snapshot and returns the authority capability.
///
/// The wrapper first checks the reserved V2 catalogue identity, then the
/// accepted V2 standard digest, and only then invokes the core canonical V2
/// verifier.
pub fn verify_standard_library_v2_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V2_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V2_REVISION_ID,
            actual: actual_catalogue,
        });
    }

    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V2_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V2_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }

    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
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
    retained_standard_library_v3_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
        RETAINED_STANDARD_OUTPUT_SOURCE,
    )
}

/// Verifies a retained output standard snapshot and returns the authority
/// capability.
///
/// The wrapper first checks the reserved V3 catalogue identity, then the
/// accepted V3 standard digest, and only then invokes the core canonical V2
/// verifier. `orna.std/3` reuses the V2 digest contract (work ADR 0058); the
/// V3 catalogue, revision, source, and goldens are all new.
pub fn verify_standard_library_v3_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V3_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V3_REVISION_ID,
            actual: actual_catalogue,
        });
    }

    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V3_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V3_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }

    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
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
    retained_standard_library_v4_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
        RETAINED_STANDARD_OUTPUT_SOURCE,
        RETAINED_STANDARD_UI_SOURCE,
    )
}

/// Verifies a retained UI standard snapshot and returns the authority
/// capability.
///
/// The wrapper first checks the reserved V4 catalogue identity, then the
/// accepted V4 standard digest, and only then invokes the core canonical V2
/// verifier. `orna.std/4` reuses the V2 digest contract (work ADR 0062); the
/// V4 catalogue, revision, source, and goldens are all new.
pub fn verify_standard_library_v4_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V4_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V4_REVISION_ID,
            actual: actual_catalogue,
        });
    }

    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V4_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V4_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }

    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
}

/// Retains the canonical V5 JSON standard source as an unverified snapshot.
pub fn retained_standard_library_v5_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    retained_standard_library_v5_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
        RETAINED_STANDARD_OUTPUT_SOURCE,
        RETAINED_STANDARD_UI_SOURCE,
        RETAINED_STANDARD_JSON_SOURCE,
    )
}

/// Verifies a retained V5 JSON standard snapshot and returns authority.
pub fn verify_standard_library_v5_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V5_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V5_REVISION_ID,
            actual: actual_catalogue,
        });
    }
    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V5_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V5_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
}

fn retained_standard_library_v4_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v4_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();

    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    let output_origins = reconcile_retained_output_source(output_source, catalogue)?;
    origins.extend(output_origins.iter().cloned());
    let ui_origins = reconcile_retained_ui_source(ui_source, catalogue)?;
    origins.extend(ui_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V4_TYPES_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if invoke_content_hash != ACCEPTED_V4_INVOKE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if output_content_hash != ACCEPTED_V4_OUTPUT_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if ui_content_hash != ACCEPTED_V4_UI_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types_source,
        types_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke_source,
        invoke_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let output_unit = StoredSourceUnit::new(
        STD_OUTPUT_SOURCE_UNIT_ID,
        2,
        STD_OUTPUT_SOURCE_LOGICAL_PATH,
        output_source,
        output_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let ui_unit = StoredSourceUnit::new(
        STD_UI_SOURCE_UNIT_ID,
        3,
        STD_UI_SOURCE_LOGICAL_PATH,
        ui_source,
        ui_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![types_unit, invoke_unit, output_unit, ui_unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V4_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V4_BUNDLE_ID,
        Some(STANDARD_SOURCE_V3_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V4_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V4_BUNDLE_ID,
        STANDARD_SOURCE_V4_REVISION_ID,
        Some(STANDARD_SOURCE_V3_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    // `orna.std/4` retains the exact V2 parameter-echo executable unchanged;
    // its artifact and semantic digests are the V3 goldens, pinned here as the
    // V4 goldens so the retained path fails closed on any drift.
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V4_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V4_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V4_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable],
        origins,
        ACCEPTED_V4_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}

fn retained_standard_library_v5_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v5_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();
    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V5_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V5_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V5_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V5_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V5_JSON_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V5_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V5_BUNDLE_ID,
        Some(STANDARD_SOURCE_V4_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V5_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V5_BUNDLE_ID,
        STANDARD_SOURCE_V5_REVISION_ID,
        Some(STANDARD_SOURCE_V4_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V5_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V5_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V5_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable, json_executable],
        origins,
        ACCEPTED_V5_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V5_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V5_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}

/// Retains the canonical V6 action standard source as an unverified snapshot.
pub fn retained_standard_library_v6_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    retained_standard_library_v6_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
        RETAINED_STANDARD_OUTPUT_SOURCE,
        RETAINED_STANDARD_UI_SOURCE,
        RETAINED_STANDARD_JSON_SOURCE,
        RETAINED_STANDARD_ACTION_SOURCE,
    )
}

/// Verifies a retained V6 action standard snapshot and returns authority.
pub fn verify_standard_library_v6_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V6_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V6_REVISION_ID,
            actual: actual_catalogue,
        });
    }
    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V6_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V6_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
}
/// Retains the canonical V7 window standard source as an unverified snapshot.
pub fn retained_standard_library_v7_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    retained_standard_library_v7_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
        RETAINED_STANDARD_OUTPUT_SOURCE,
        RETAINED_STANDARD_UI_SOURCE,
        RETAINED_STANDARD_JSON_SOURCE,
        RETAINED_STANDARD_ACTION_SOURCE,
        RETAINED_STANDARD_WINDOW_SOURCE,
    )
}

/// Verifies a retained V7 window standard snapshot and returns authority.
pub fn verify_standard_library_v7_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V7_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V7_REVISION_ID,
            actual: actual_catalogue,
        });
    }
    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V7_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V7_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
}
/// Retains the canonical V8 Rows standard source as an unverified snapshot.
pub fn retained_standard_library_v8_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    retained_standard_library_v8_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
        RETAINED_STANDARD_OUTPUT_SOURCE,
        RETAINED_STANDARD_UI_SOURCE,
        RETAINED_STANDARD_JSON_SOURCE,
        RETAINED_STANDARD_ACTION_SOURCE,
        RETAINED_STANDARD_WINDOW_SOURCE,
        RETAINED_STANDARD_DATA_SOURCE,
    )
}

/// Verifies a retained V8 Rows standard snapshot and returns authority.
pub fn verify_standard_library_v8_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V8_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V8_REVISION_ID,
            actual: actual_catalogue,
        });
    }
    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V8_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V8_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
}

/// Retains the canonical V9 structural UI constructor source as an
/// unverified snapshot.
pub fn retained_standard_library_v9_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    retained_standard_library_v9_snapshot_from_source(
        RETAINED_STANDARD_SOURCE,
        RETAINED_STANDARD_INVOKE_SOURCE,
        RETAINED_STANDARD_OUTPUT_SOURCE,
        RETAINED_STANDARD_UI_SOURCE,
        RETAINED_STANDARD_JSON_SOURCE,
        RETAINED_STANDARD_ACTION_SOURCE,
        RETAINED_STANDARD_WINDOW_SOURCE,
        RETAINED_STANDARD_DATA_SOURCE,
        RETAINED_STANDARD_UI_CONSTRUCTORS_SOURCE,
    )
}

/// Verifies a retained V9 structural UI constructor snapshot and returns
/// authority.
pub fn verify_standard_library_v9_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    let actual_catalogue = snapshot.catalogue().revision();
    if actual_catalogue != STANDARD_CATALOGUE_V9_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V9_REVISION_ID,
            actual: actual_catalogue,
        });
    }
    let actual_digest = snapshot.digest();
    if actual_digest != ACCEPTED_V9_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V9_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
}

/// Source-independent facts required to recognise `orna.std/10`.
#[derive(Clone, Debug)]
pub struct StandardLibraryV10Manifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryV10Manifest {
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_V10_VERSION_IDENTITY
    }

    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_V10_REVISION_ID
    }

    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_V10_BUNDLE_ID
    }

    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_V10_REVISION_ID
    }

    pub const fn types_source_unit(&self) -> SourceUnitId {
        STD_TYPES_SOURCE_UNIT_ID
    }

    pub const fn invoke_source_unit(&self) -> SourceUnitId {
        STD_INVOKE_SOURCE_UNIT_ID
    }

    pub const fn output_source_unit(&self) -> SourceUnitId {
        STD_OUTPUT_SOURCE_UNIT_ID
    }

    pub const fn ui_source_unit(&self) -> SourceUnitId {
        STD_UI_SOURCE_UNIT_ID
    }

    pub const fn json_source_unit(&self) -> SourceUnitId {
        STD_JSON_SOURCE_UNIT_ID
    }

    pub const fn action_source_unit(&self) -> SourceUnitId {
        STD_ACTION_SOURCE_UNIT_ID
    }

    pub const fn window_source_unit(&self) -> SourceUnitId {
        STD_WINDOW_SOURCE_UNIT_ID
    }

    pub const fn data_source_unit(&self) -> SourceUnitId {
        STD_DATA_SOURCE_UNIT_ID
    }

    pub const fn ui_constructors_source_unit(&self) -> SourceUnitId {
        STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID
    }

    pub const fn cli_source_unit(&self) -> SourceUnitId {
        STD_CLI_SOURCE_UNIT_ID
    }

    pub const fn cli_source_logical_path(&self) -> &'static str {
        STD_CLI_SOURCE_LOGICAL_PATH
    }

    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds the append-only V10 catalogue over V9.
pub fn standard_library_v10_manifest()
-> Result<StandardLibraryV10Manifest, StandardLibraryManifestError> {
    let version_nine = standard_library_v9_manifest()?;
    let mut schemas = version_nine.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_CLI_SCHEMA_ID,
        semantic_name("std.cli", ["std", "cli"])?,
    ));
    let mut functions = version_nine.catalogue().functions().to_vec();
    functions.push(FunctionDefinition::new(
        STD_CLI_REPL_FUNCTION_ID,
        semantic_name("std.cli.repl", ["std", "cli", "repl"])?,
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID)),
        STD_CLI_REPL_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Volatile,
    ));
    functions.sort_by_key(|function| function.id());
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        STANDARD_CATALOGUE_V10_REVISION_ID,
        schemas,
        version_nine.catalogue().object_types().to_vec(),
        version_nine.catalogue().value_types().to_vec(),
        version_nine.catalogue().type_bindings().to_vec(),
        functions,
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;
    Ok(StandardLibraryV10Manifest { catalogue })
}

/// Retains the source-authored V10 CLI session unit.
pub fn retained_standard_library_v10_snapshot()
-> Result<StandardLibrarySnapshot, StandardLibraryError> {
    retained_standard_library_v10_snapshot_from_source(RETAINED_STANDARD_CLI_SOURCE)
}

/// Verifies the retained source-authored V10 CLI session snapshot.
pub fn verify_standard_library_v10_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    if snapshot.catalogue().revision() != STANDARD_CATALOGUE_V10_REVISION_ID {
        return Err(StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_V10_REVISION_ID,
            actual: snapshot.catalogue().revision(),
        });
    }
    if snapshot.source().bundle() != STANDARD_SOURCE_V10_BUNDLE_ID
        || snapshot.source().id() != STANDARD_SOURCE_V10_REVISION_ID
        || snapshot.source().parent() != Some(STANDARD_SOURCE_V9_REVISION_ID)
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    if snapshot.digest() != ACCEPTED_V10_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V10_STANDARD_LIBRARY_DIGEST,
            actual: snapshot.digest(),
        });
    }
    verify_canonical_standard_library_v2_snapshot(snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })
}

fn retained_standard_library_v10_snapshot_from_source(
    cli_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let parent = retained_standard_library_v9_snapshot()?;
    let manifest = standard_library_v10_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let cli_content_hash = source_unit_content_digest(cli_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if cli_content_hash != ACCEPTED_V10_CLI_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let cli_unit = StoredSourceUnit::new(
        STD_CLI_SOURCE_UNIT_ID,
        parent.source().units().len() as u32,
        STD_CLI_SOURCE_LOGICAL_PATH,
        cli_source,
        cli_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let parsed = orna_syntax::parse(cli_source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != cli_source
        || parsed.schemas().len() != 1
        || parsed.client_functions().len() != 1
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_CLI_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let cli_origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(STD_CLI_SCHEMA_ID),
            origin(&parsed.schemas()[0].span)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(STD_CLI_REPL_FUNCTION_ID),
            origin(&parsed.client_functions()[0].span)?,
        ),
    ];
    let checked = orna_compiler::check_standard_cli_repl(
        &parsed.client_functions()[0],
        manifest.catalogue(),
        &cli_origins,
        &cli_unit,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        checked.revision_number(),
        checked.declaration_origin(),
        checked.declaration_content_hash(),
        checked.semantic_hash(),
        checked.language_version(),
        checked.artifact().clone(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(checked.semantic_hash_version());
    let executable = StandardExecutable::new(
        checked.function_id(),
        revision,
        checked.references().to_vec(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    if checked.artifact().content_hash() != ACCEPTED_V10_CLI_ARTIFACT_DIGEST
        || checked.semantic_hash() != ACCEPTED_V10_CLI_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let mut units = parent.source().units().to_vec();
    units.push(cli_unit);
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V10_BUNDLE_ID,
        Some(STANDARD_SOURCE_V9_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V10_SOURCE_BUNDLE_DIGEST
        || revision_hash != ACCEPTED_V10_SOURCE_REVISION_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let source = StoredSourceRevision::new(
        STANDARD_SOURCE_V10_BUNDLE_ID,
        STANDARD_SOURCE_V10_REVISION_ID,
        Some(STANDARD_SOURCE_V9_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let mut origins = parent.origins().to_vec();
    origins.extend(cli_origins);
    let mut executables = parent.executables().to_vec();
    executables.push(executable);
    let provisional = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V10_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        source,
        LANGUAGE_VERSION_IDENTITY,
        manifest.catalogue().clone(),
        executables,
        origins,
        Sha256Digest::from_bytes([0; 32]),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let digest = calculate_standard_library_digest(&provisional)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if digest != ACCEPTED_V10_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V10_STANDARD_LIBRARY_DIGEST,
            actual: digest,
        });
    }
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V10_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        provisional.source().clone(),
        LANGUAGE_VERSION_IDENTITY,
        provisional.catalogue().clone(),
        provisional.executables().to_vec(),
        provisional.origins().to_vec(),
        digest,
    )
    .map_err(|source| StandardLibraryError::Revision { source })
}

fn retained_standard_library_v6_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v6_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();
    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    let action_origins = reconcile_retained_action_source(action_source, catalogue)?;
    origins.extend(action_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V6_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V6_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V6_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V6_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V6_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V6_ACTION_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V6_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V6_BUNDLE_ID,
        Some(STANDARD_SOURCE_V5_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V6_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V6_BUNDLE_ID,
        STANDARD_SOURCE_V6_REVISION_ID,
        Some(STANDARD_SOURCE_V5_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V6_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V6_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V6_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable, json_executable],
        origins,
        ACCEPTED_V6_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V6_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V6_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}

fn retained_standard_library_v7_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
    window_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v7_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();
    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    origins.extend(reconcile_retained_action_source(action_source, catalogue)?);
    let window_origins = reconcile_retained_window_source(window_source, catalogue)?;
    origins.extend(window_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let window_content_hash = source_unit_content_digest(window_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V7_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V7_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V7_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V7_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V7_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V7_ACTION_CONTENT_DIGEST
        || window_content_hash != ACCEPTED_V7_WINDOW_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
        StoredSourceUnit::new(
            STD_WINDOW_SOURCE_UNIT_ID,
            6,
            STD_WINDOW_SOURCE_LOGICAL_PATH,
            window_source,
            window_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V7_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V7_BUNDLE_ID,
        Some(STANDARD_SOURCE_V6_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V7_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V7_BUNDLE_ID,
        STANDARD_SOURCE_V7_REVISION_ID,
        Some(STANDARD_SOURCE_V6_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V7_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V7_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let window_executable = retained_window_executable(window_source, catalogue, &window_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V7_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable, json_executable, window_executable],
        origins,
        ACCEPTED_V7_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V7_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V7_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}
#[allow(clippy::too_many_arguments)]
fn retained_standard_library_v8_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
    window_source: &str,
    data_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v8_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();
    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    origins.extend(reconcile_retained_action_source(action_source, catalogue)?);
    let window_origins = reconcile_retained_window_source(window_source, catalogue)?;
    origins.extend(window_origins.iter().cloned());
    let data_origins = reconcile_retained_data_source(data_source, catalogue)?;
    origins.extend(data_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let window_content_hash = source_unit_content_digest(window_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let data_content_hash = source_unit_content_digest(data_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V8_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V8_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V8_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V8_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V8_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V8_ACTION_CONTENT_DIGEST
        || window_content_hash != ACCEPTED_V8_WINDOW_CONTENT_DIGEST
        || data_content_hash != ACCEPTED_V8_DATA_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
        StoredSourceUnit::new(
            STD_WINDOW_SOURCE_UNIT_ID,
            6,
            STD_WINDOW_SOURCE_LOGICAL_PATH,
            window_source,
            window_content_hash,
        ),
        StoredSourceUnit::new(
            STD_DATA_SOURCE_UNIT_ID,
            7,
            STD_DATA_SOURCE_LOGICAL_PATH,
            data_source,
            data_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V8_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V8_BUNDLE_ID,
        Some(STANDARD_SOURCE_V7_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V8_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V8_BUNDLE_ID,
        STANDARD_SOURCE_V8_REVISION_ID,
        Some(STANDARD_SOURCE_V7_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V8_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V8_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let window_executable = retained_window_executable(window_source, catalogue, &window_origins)?;
    let table_executable =
        retained_terminal_table_executable(data_source, catalogue, &data_origins)?;
    if table_executable.revision().artifact().content_hash() != ACCEPTED_V8_TABLE_ARTIFACT_DIGEST
        || table_executable.revision().semantic_hash() != ACCEPTED_V8_TABLE_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V8_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![
            executable,
            json_executable,
            table_executable,
            window_executable,
        ],
        origins,
        ACCEPTED_V8_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V8_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V8_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}

#[allow(clippy::too_many_arguments)]
fn retained_standard_library_v9_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
    window_source: &str,
    data_source: &str,
    constructors_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v9_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();
    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    origins.extend(reconcile_retained_action_source(action_source, catalogue)?);
    let window_origins = reconcile_retained_window_source(window_source, catalogue)?;
    origins.extend(window_origins.iter().cloned());
    let data_origins = reconcile_retained_data_source(data_source, catalogue)?;
    origins.extend(data_origins.iter().cloned());
    let constructor_origins =
        reconcile_retained_ui_constructors_source(constructors_source, catalogue)?;
    origins.extend(constructor_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let window_content_hash = source_unit_content_digest(window_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let data_content_hash = source_unit_content_digest(data_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let constructors_content_hash = source_unit_content_digest(constructors_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V9_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V9_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V9_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V9_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V9_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V9_ACTION_CONTENT_DIGEST
        || window_content_hash != ACCEPTED_V9_WINDOW_CONTENT_DIGEST
        || data_content_hash != ACCEPTED_V9_DATA_CONTENT_DIGEST
        || constructors_content_hash != ACCEPTED_V9_UI_CONSTRUCTORS_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
        StoredSourceUnit::new(
            STD_WINDOW_SOURCE_UNIT_ID,
            6,
            STD_WINDOW_SOURCE_LOGICAL_PATH,
            window_source,
            window_content_hash,
        ),
        StoredSourceUnit::new(
            STD_DATA_SOURCE_UNIT_ID,
            7,
            STD_DATA_SOURCE_LOGICAL_PATH,
            data_source,
            data_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID,
            8,
            STD_UI_CONSTRUCTORS_SOURCE_LOGICAL_PATH,
            constructors_source,
            constructors_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V9_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V9_BUNDLE_ID,
        Some(STANDARD_SOURCE_V8_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V9_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V9_BUNDLE_ID,
        STANDARD_SOURCE_V9_REVISION_ID,
        Some(STANDARD_SOURCE_V8_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V9_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V9_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let window_executable = retained_window_executable(window_source, catalogue, &window_origins)?;
    let table_executable =
        retained_terminal_table_executable(data_source, catalogue, &data_origins)?;
    if table_executable.revision().artifact().content_hash() != ACCEPTED_V9_TABLE_ARTIFACT_DIGEST
        || table_executable.revision().semantic_hash() != ACCEPTED_V9_TABLE_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let constructor_executables =
        retained_ui_constructor_executables(constructors_source, catalogue, &constructor_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V9_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        [
            vec![
                executable,
                json_executable,
                table_executable,
                window_executable,
            ],
            constructor_executables,
        ]
        .concat(),
        origins,
        ACCEPTED_V9_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V9_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V9_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}

fn reconcile_retained_data_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
        || parsed.server_functions().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let schema = catalogue
        .schema_by_id(STD_DATA_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let rows_definition = catalogue
        .type_definition_by_id(STD_DATA_ROWS_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let rows_binding = catalogue
        .type_binding_by_id(STD_DATA_ROWS_TYPE_BINDING_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let declaration_schema = &parsed.schemas()[0];
    let declaration_type = &parsed.opaque_value_types()[0];
    let declaration_export = &parsed.type_exports()[0];
    let [declaration_function] = parsed.server_functions() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    if !matches_qualified_name(&declaration_schema.name, schema.name())
        || !matches_qualified_name(&declaration_type.name, rows_definition.name())
        || decode_sql_string_literal(&declaration_type.kernel_contract.text).as_deref()
            != Some(rows_definition.representation_contract())
        || rows_definition.mutability() != ValueTypeMutability::Immutable
        || rows_definition.persistence() != ValueTypePersistence::Transient
        || !matches_qualified_export(
            declaration_export,
            rows_definition.name(),
            STD_DATA_ROWS_TYPE_ID,
            rows_binding,
        )
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    if !matches!(
        &declaration_function.name.parts[..],
        [first, second, third]
            if is_unquoted(first)
                && is_unquoted(second)
                && is_unquoted(third)
                && first.text.eq_ignore_ascii_case("std")
                && second.text.eq_ignore_ascii_case("terminal")
                && third.text.eq_ignore_ascii_case("present_table")
    ) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let parameter = declaration_function
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_DATA_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let mut origins = vec![
        origin(
            &declaration_schema.span,
            DefinitionIdentity::Schema(STD_DATA_SCHEMA_ID),
        )?,
        origin(
            &declaration_type.span,
            DefinitionIdentity::ValueType(STD_DATA_ROWS_TYPE_ID),
        )?,
        origin(
            &declaration_export.span,
            DefinitionIdentity::TypeBinding(rows_binding.id()),
        )?,
        origin(
            &declaration_function.span,
            DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID),
        )?,
        origin(
            &parameter.span,
            DefinitionIdentity::Parameter {
                owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            },
        )?,
    ];
    origins.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    orna_compiler::check_standard_terminal_present_table(
        declaration_function,
        catalogue,
        &origins,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    Ok(origins)
}

fn reconcile_retained_window_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.schemas().is_empty()
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || parsed.client_functions().len() != 1
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let [function] = parsed.client_functions() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_WINDOW_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let [title, content] = function.parameters.as_slice() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    let mut origins = vec![
        origin(
            &function.span,
            DefinitionIdentity::Function(STD_UI_WINDOW_FUNCTION_ID),
        )?,
        origin(
            &title.span,
            DefinitionIdentity::Parameter {
                owner: STD_UI_WINDOW_FUNCTION_ID,
                parameter: STD_UI_WINDOW_TITLE_PARAMETER_ID,
            },
        )?,
        origin(
            &content.span,
            DefinitionIdentity::Parameter {
                owner: STD_UI_WINDOW_FUNCTION_ID,
                parameter: STD_UI_WINDOW_CONTENT_PARAMETER_ID,
            },
        )?,
    ];
    origins.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    orna_compiler::check_standard_ui_window(function, catalogue, &origins)
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    Ok(origins)
}

fn reconcile_retained_ui_constructors_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.schemas().is_empty()
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
        || parsed.client_functions().len() != 7
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let expected_names = [
        "text",
        "button",
        "panel",
        "row",
        "column",
        "text_input",
        "tabs",
    ];
    let expected_functions = [
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
    ];
    let expected_parameters: [&[orna_core::ParameterId]; 7] = [
        &[STD_UI_TEXT_PARAMETER_ID],
        &[
            STD_UI_BUTTON_LABEL_PARAMETER_ID,
            STD_UI_BUTTON_ENABLED_PARAMETER_ID,
        ],
        &[STD_UI_PANEL_CONTENT_PARAMETER_ID],
        &[STD_UI_ROW_CONTENT_PARAMETER_ID],
        &[STD_UI_COLUMN_CONTENT_PARAMETER_ID],
        &[
            STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
            STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
            STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
        ],
        &[STD_UI_TABS_CONTENT_PARAMETER_ID],
    ];
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let mut origins = Vec::new();
    let mut function_origins = Vec::with_capacity(7);
    for (index, function) in parsed.client_functions().iter().enumerate() {
        let [first, second, third] = function.name.parts.as_slice() else {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        };
        if !is_unquoted(first)
            || !is_unquoted(second)
            || !is_unquoted(third)
            || !first.text.eq_ignore_ascii_case("std")
            || !second.text.eq_ignore_ascii_case("ui")
            || !third.text.eq_ignore_ascii_case(expected_names[index])
        {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        if function.parameters.len() != expected_parameters[index].len() {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        let function_origin = origin(
            &function.span,
            DefinitionIdentity::Function(expected_functions[index]),
        )?;
        let mut group = vec![function_origin.clone()];
        for (parameter, parameter_id) in function.parameters.iter().zip(expected_parameters[index])
        {
            group.push(origin(
                &parameter.span,
                DefinitionIdentity::Parameter {
                    owner: expected_functions[index],
                    parameter: *parameter_id,
                },
            )?);
        }
        origins.extend(group.iter().cloned());
        function_origins.push(group);
    }
    origins.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    for (function, group) in parsed
        .client_functions()
        .iter()
        .zip(function_origins.iter())
    {
        orna_compiler::check_standard_ui_constructor(function, catalogue, group)
            .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    }
    Ok(origins)
}

fn reconcile_retained_action_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let action_schema = catalogue
        .schema_by_id(STD_ACTION_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, action_schema.name()) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let action_definition = catalogue
        .type_definition_by_id(STD_ACTION_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let declaration = &parsed.opaque_value_types()[0];
    if !matches_qualified_name(&declaration.name, action_definition.name())
        || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
            != Some(action_definition.representation_contract())
        || action_definition.persistence() != ValueTypePersistence::Transient
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let action_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            semantic_name("std.action", ["std", "action"])
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?,
        ))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_export(
        &parsed.type_exports()[0],
        action_definition.name(),
        STD_ACTION_TYPE_ID,
        action_binding,
    ) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_ACTION_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let mut declarations = vec![
        (
            parsed.schemas()[0].span.clone(),
            DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
        ),
        (
            parsed.opaque_value_types()[0].span.clone(),
            DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
        ),
        (
            parsed.type_exports()[0].span.clone(),
            DefinitionIdentity::TypeBinding(action_binding.id()),
        ),
    ];
    declarations.sort_by_key(|(span, _)| span.start);
    let expected_identities = [
        DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
        DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
        DefinitionIdentity::TypeBinding(action_binding.id()),
    ];
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities.iter().copied())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    declarations
        .into_iter()
        .map(|(span, identity)| Ok(DefinitionOrigin::new(identity, origin(&span)?)))
        .collect()
}

fn reconcile_retained_json_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || parsed.server_functions().len() != 1
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let schema = catalogue
        .schema_by_id(STD_JSON_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let value = catalogue
        .type_definition_by_id(STD_JSON_VALUE_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            semantic_name("std.jsonvalue", ["std", "jsonvalue"])
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?,
        ))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, schema.name())
        || !matches_qualified_name(&parsed.opaque_value_types()[0].name, value.name())
        || decode_sql_string_literal(&parsed.opaque_value_types()[0].kernel_contract.text)
            .as_deref()
            != Some(value.representation_contract())
        || value.persistence() != ValueTypePersistence::Transient
        || !matches_qualified_export(
            &parsed.type_exports()[0],
            value.name(),
            STD_JSON_VALUE_TYPE_ID,
            binding,
        )
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let function = &parsed.server_functions()[0];
    if function
        .name
        .parts
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        != ["std", "json", "encode"]
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let function_definition = catalogue
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if function_definition.name()
        != &semantic_name("std.json.encode", ["std", "json", "encode"])
            .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let parameter = function
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_JSON_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let mut declarations = vec![
        origin(
            &parsed.schemas()[0].span,
            DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID),
        )?,
        origin(
            &parsed.opaque_value_types()[0].span,
            DefinitionIdentity::ValueType(STD_JSON_VALUE_TYPE_ID),
        )?,
        origin(
            &parsed.type_exports()[0].span,
            DefinitionIdentity::TypeBinding(binding.id()),
        )?,
        origin(
            &function.span,
            DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID),
        )?,
        origin(
            &parameter.span,
            DefinitionIdentity::Parameter {
                owner: STD_JSON_ENCODE_FUNCTION_ID,
                parameter: STD_JSON_ENCODE_PARAMETER_ID,
            },
        )?,
    ];
    declarations.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    orna_compiler::check_standard_json_encode(
        function,
        catalogue,
        &declarations,
        STD_JSON_VALUE_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    Ok(declarations)
}

/// Reconciles the retained `std/ui.orna` unit against the V4 catalogue.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.ui` schema declaration, the single opaque UI value type declaration
/// (`std.ui.UI` with the `orna.std.value.ui@1` kernel contract and the
/// `IMMUTABLE TRANSIENT` catalogue facts), and the single `std.ui` qualified
/// export. The complete origin set is exactly those three declarations at
/// their exact byte ranges in the retained unit.
fn reconcile_retained_ui_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let ui_schema = catalogue
        .schema_by_id(STD_UI_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, ui_schema.name()) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let ui_definition = catalogue
        .type_definition_by_id(STD_UI_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let declaration = &parsed.opaque_value_types()[0];
    if !matches_qualified_name(&declaration.name, ui_definition.name())
        || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
            != Some(ui_definition.representation_contract())
        || !matches!(ui_definition.mutability(), ValueTypeMutability::Immutable)
        || ui_definition.persistence() != ValueTypePersistence::Transient
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let ui_binding = catalogue
        .type_bindings()
        .get(33)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_export(
        &parsed.type_exports()[0],
        ui_definition.name(),
        STD_UI_TYPE_ID,
        ui_binding,
    ) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_UI_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };

    let expected_identities = [
        DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
        DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
        DefinitionIdentity::TypeBinding(ui_binding.id()),
    ];
    let mut declarations = vec![
        (
            parsed.schemas()[0].span.clone(),
            DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
        ),
        (
            parsed.opaque_value_types()[0].span.clone(),
            DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
        ),
        (
            parsed.type_exports()[0].span.clone(),
            DefinitionIdentity::TypeBinding(ui_binding.id()),
        ),
    ];
    declarations.sort_by_key(|(span, _)| span.start);
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities.iter().copied())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    declarations
        .into_iter()
        .map(|(span, identity)| Ok(DefinitionOrigin::new(identity, origin(&span)?)))
        .collect()
}

fn retained_standard_library_v3_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v3_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();

    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    let output_origins = reconcile_retained_output_source(output_source, catalogue)?;
    origins.extend(output_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V3_TYPES_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if invoke_content_hash != ACCEPTED_V3_INVOKE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if output_content_hash != ACCEPTED_V3_OUTPUT_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types_source,
        types_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke_source,
        invoke_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let output_unit = StoredSourceUnit::new(
        STD_OUTPUT_SOURCE_UNIT_ID,
        2,
        STD_OUTPUT_SOURCE_LOGICAL_PATH,
        output_source,
        output_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![types_unit, invoke_unit, output_unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V3_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V3_BUNDLE_ID,
        Some(STANDARD_SOURCE_V2_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V3_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V3_BUNDLE_ID,
        STANDARD_SOURCE_V3_REVISION_ID,
        Some(STANDARD_SOURCE_V2_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    // `orna.std/3` retains the exact V2 parameter-echo executable unchanged;
    // its artifact and semantic digests are the V2 goldens, pinned here as the
    // V3 goldens so the retained path fails closed on any drift.
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V3_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V3_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V3_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable],
        origins,
        ACCEPTED_V3_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}

/// Reconciles the retained `std/output.orna` unit against the V3 catalogue.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.terminal` and `std.io` schema declarations, the two opaque output
/// value type declarations, and their two qualified exports. The complete
/// origin set is exactly those six declarations at their exact byte ranges in
/// the retained unit.
fn reconcile_retained_output_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 2
        || parsed.opaque_value_types().len() != 2
        || parsed.type_exports().len() != 2
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let terminal_schema = catalogue
        .schema_by_id(STD_TERMINAL_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let io_schema = catalogue
        .schema_by_id(STD_IO_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, terminal_schema.name())
        || !matches_qualified_name(&parsed.schemas()[1].name, io_schema.name())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let document_definition = catalogue
        .type_definition_by_id(STD_TERMINAL_DOCUMENT_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let bytestream_definition = catalogue
        .type_definition_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let output_definitions = [document_definition, bytestream_definition];
    for (declaration, definition) in parsed.opaque_value_types().iter().zip(output_definitions) {
        if !matches_qualified_name(&declaration.name, definition.name())
            || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
                != Some(definition.representation_contract())
            || definition.persistence() != ValueTypePersistence::Transient
        {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    }

    let document_binding = catalogue
        .type_bindings()
        .get(31)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let bytestream_binding = catalogue
        .type_bindings()
        .get(32)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let output_bindings = [document_binding, bytestream_binding];
    for (export, (definition, binding)) in parsed
        .type_exports()
        .iter()
        .zip(output_definitions.iter().zip(output_bindings))
    {
        if !matches_qualified_export(export, definition.name(), definition.id(), binding) {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    }

    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_OUTPUT_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };

    let expected_identities = [
        DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
        DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
        DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
        DefinitionIdentity::TypeBinding(document_binding.id()),
        DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
        DefinitionIdentity::TypeBinding(bytestream_binding.id()),
    ];
    let mut declarations = vec![
        (
            parsed.schemas()[0].span.clone(),
            DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
        ),
        (
            parsed.schemas()[1].span.clone(),
            DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
        ),
        (
            parsed.opaque_value_types()[0].span.clone(),
            DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
        ),
        (
            parsed.type_exports()[0].span.clone(),
            DefinitionIdentity::TypeBinding(document_binding.id()),
        ),
        (
            parsed.opaque_value_types()[1].span.clone(),
            DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
        ),
        (
            parsed.type_exports()[1].span.clone(),
            DefinitionIdentity::TypeBinding(bytestream_binding.id()),
        ),
    ];
    declarations.sort_by_key(|(span, _)| span.start);
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities.iter().copied())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    declarations
        .into_iter()
        .map(|(span, identity)| Ok(DefinitionOrigin::new(identity, origin(&span)?)))
        .collect()
}

fn retained_standard_library_v2_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v2_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();

    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V2_TYPES_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if invoke_content_hash != ACCEPTED_V2_INVOKE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types_source,
        types_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke_source,
        invoke_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![types_unit, invoke_unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V2_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V2_BUNDLE_ID,
        Some(STANDARD_SOURCE_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V2_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V2_BUNDLE_ID,
        STANDARD_SOURCE_V2_REVISION_ID,
        Some(STANDARD_SOURCE_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V2_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable],
        origins,
        ACCEPTED_V2_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}

/// Builds the immutable opaque codec registry for the accepted standard snapshot.
///
/// The registration is compiled into this crate. The supplied snapshot can
/// validate it, but cannot add or select a codec. The accepted V1 and V2
/// snapshots bind only the fixed-length `std.types.opaque_token` codec; the
/// accepted `orna.std/3` snapshot additionally binds the two framed output
/// codecs for `std.terminal.Document` and `std.io.ByteStream` (work ADR
/// 0058); the accepted `orna.std/4` snapshot additionally binds the
/// `ORNA-UI/1 ` length-prefixed canonical UI-value codec for `std.ui.UI` (work ADR 0062);
/// the accepted V6 snapshot additionally binds the structurally checked
/// `ORNA-ACTION/1 ` action descriptor codec (ADR 0079).
pub fn registered_opaque_codecs(
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<OpaqueCodecRegistry, RegisteredOpaqueCodecsError> {
    let opaque_token = OpaqueCodecRegistration::fixed_length_identity(
        OPAQUE_TOKEN_TYPE_ID,
        semantic_name(
            "std.types.opaque_token",
            ["std", "types", OPAQUE_TOKEN_LOCAL_NAME],
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
        OPAQUE_TOKEN_CONTRACT,
        16,
    )
    .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;

    let registrations = if is_accepted_v10_standard(standard)
        || is_accepted_v9_standard(standard)
        || is_accepted_v8_standard(standard)
    {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let json = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_JSON_VALUE_TYPE_ID,
            semantic_name("std.json.value", ["std", "json", "value"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_JSON_CONTRACT,
            JSON_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let action = OpaqueCodecRegistration::length_prefixed_action(
            STD_ACTION_TYPE_ID,
            semantic_name("std.action.action", ["std", "action", "action"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_ACTION_CONTRACT,
            ACTION_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let rows = OpaqueCodecRegistration::rows(
            STD_DATA_ROWS_TYPE_ID,
            semantic_name(STD_DATA_ROWS_SEMANTIC_NAME, ["std", "data", "rows"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_DATA_ROWS_CONTRACT,
            "ORNA-ROWS/1 ",
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui, json, action, rows]
    } else if is_accepted_v7_standard(standard) || is_accepted_v6_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let json = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_JSON_VALUE_TYPE_ID,
            semantic_name("std.json.value", ["std", "json", "value"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_JSON_CONTRACT,
            JSON_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let action = OpaqueCodecRegistration::length_prefixed_action(
            STD_ACTION_TYPE_ID,
            semantic_name("std.action.action", ["std", "action", "action"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_ACTION_CONTRACT,
            ACTION_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui, json, action]
    } else if is_accepted_v5_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let json = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_JSON_VALUE_TYPE_ID,
            semantic_name("std.json.value", ["std", "json", "value"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_JSON_CONTRACT,
            JSON_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui, json]
    } else if is_accepted_v4_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui]
    } else if is_accepted_v3_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream]
    } else if is_accepted_v1_or_v2_standard(standard) {
        vec![opaque_token]
    } else {
        return Err(RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot);
    };

    OpaqueCodecRegistry::new(standard, registrations)
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })
}

/// Returns the deterministic checked-in contracts for the nine sealed
/// `sys.inspect` snapshot and projection carriers.
///
/// These system carriers are intentionally not added to a standard snapshot's
/// [`OpaqueCodecRegistry`]. They are recognised through their sealed TypeIds
/// and contracts, independently of application catalogue definitions.
pub fn registered_inspect_carrier_codecs() -> &'static [InspectCarrierCodecRegistration] {
    INSPECT_CARRIER_CODEC_REGISTRATIONS
}

/// Returns whether a TypeId is one of the fixed sealed Inspector carriers.
pub fn is_registered_inspect_carrier_type(opaque_type: TypeId) -> bool {
    registered_inspect_carrier_codecs()
        .iter()
        .any(|registration| registration.opaque_type() == opaque_type)
}

/// Returns whether one verified snapshot is exactly the accepted version-one
/// or version-two standard library (ADR 0055).
///
/// Version two retains the version-one types byte-for-byte and adds no new
/// opaque type or codec, so both accepted snapshots bind the same opaque-token
/// codec.
fn is_accepted_v1_or_v2_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    (standard.revision() == STANDARD_LIBRARY_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_REVISION_ID
        && standard.source().revision_hash() == ACCEPTED_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_STANDARD_LIBRARY_DIGEST)
        || (standard.revision() == STANDARD_LIBRARY_V2_REVISION_ID
            && standard.catalogue().revision() == STANDARD_CATALOGUE_V2_REVISION_ID
            && standard.source().bundle() == STANDARD_SOURCE_V2_BUNDLE_ID
            && standard.source().id() == STANDARD_SOURCE_V2_REVISION_ID
            && standard.source().revision_hash() == ACCEPTED_V2_SOURCE_REVISION_DIGEST
            && standard.digest() == ACCEPTED_V2_STANDARD_LIBRARY_DIGEST)
}

/// Returns whether one verified snapshot is exactly the accepted `orna.std/3`
/// standard library (work ADR 0058).
fn is_accepted_v3_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V3_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V3_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V3_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V3_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V2_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V3_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
}

/// Returns whether one verified snapshot is exactly the accepted `orna.std/4`
/// standard library (work ADR 0062).
fn is_accepted_v4_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V4_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V4_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V4_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V4_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V3_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V4_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v10_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V10_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V10_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V10_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V10_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V9_REVISION_ID)
        && standard.source().units().len() == 10
        && standard.source().units()[9].content_hash() == ACCEPTED_V10_CLI_CONTENT_DIGEST
        && standard.source().revision_hash() == ACCEPTED_V10_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V10_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v9_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V9_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V9_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V9_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V9_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V8_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V9_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V9_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v8_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V8_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V8_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V8_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V8_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V7_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V8_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V8_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v7_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V7_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V7_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V7_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V7_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V6_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V7_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V7_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v6_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V6_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V6_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V6_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V6_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V5_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V6_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V6_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v5_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V5_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V5_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V5_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V5_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V4_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V5_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V5_STANDARD_LIBRARY_DIGEST
}

/// An error from binding checked-in opaque codecs to a standard snapshot.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisteredOpaqueCodecsError {
    /// The supplied verified snapshot is not the hard-coded accepted standard.
    UnacceptedStandardSnapshot,
    /// A checked-in codec semantic name is invalid.
    Manifest {
        /// The standard manifest error.
        source: StandardLibraryManifestError,
    },
    /// The checked-in registry does not match the accepted definitions.
    Registry {
        /// The exact core registry validation error.
        source: OpaqueCodecRegistryError,
    },
}

impl fmt::Display for RegisteredOpaqueCodecsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnacceptedStandardSnapshot => {
                formatter.write_str("opaque codecs require the accepted standard snapshot")
            }
            Self::Manifest { source } => {
                write!(formatter, "opaque codec name is invalid: {source}")
            }
            Self::Registry { source } => {
                write!(formatter, "opaque codec registry is invalid: {source}")
            }
        }
    }
}

impl Error for RegisteredOpaqueCodecsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnacceptedStandardSnapshot => None,
            Self::Manifest { source } => Some(source),
            Self::Registry { source } => Some(source),
        }
    }
}

fn retained_standard_library_snapshot_from_source(
    source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let origins = reconcile_retained_source(source, &manifest)?;

    let content_hash = source_unit_content_digest(source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if content_hash != ACCEPTED_SOURCE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        source,
        content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(STANDARD_SOURCE_BUNDLE_ID, None, bundle_hash)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_BUNDLE_ID,
        STANDARD_SOURCE_REVISION_ID,
        None,
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let snapshot = StandardLibrarySnapshot::new(
        STANDARD_LIBRARY_REVISION_ID,
        StandardLibraryDigestVersion::Version1,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        manifest.catalogue().clone(),
        origins,
        ACCEPTED_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}

fn reconcile_retained_source(
    source: &str,
    manifest: &StandardLibraryManifest,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    reconcile_retained_source_with_unit(source, manifest, STANDARD_SOURCE_UNIT_ID)
}

/// Reconciles one retained `std/types.orna` source unit against the
/// source-independent type manifest.
///
/// The V1 snapshot retains the type declarations with the `orna.std/1`
/// source-unit identity `...01`. The V2 snapshot retains the exact same bytes
/// with the new durable unit identity `...02`; the declarations and their
/// byte ranges are identical, so this function differs only in the unit
/// identity attached to every origin.
fn reconcile_retained_source_with_unit(
    source: &str,
    manifest: &StandardLibraryManifest,
    unit_id: SourceUnitId,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let catalogue = manifest.catalogue();
    if parsed.schemas().len() != catalogue.schemas().len()
        || parsed.primitive_value_types().len() + parsed.opaque_value_types().len()
            != catalogue.value_types().len()
        || parsed.type_exports().len() != catalogue.type_bindings().len()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let mut declarations = Vec::with_capacity(45);
    for (declaration, schema) in parsed.schemas().iter().zip(catalogue.schemas()) {
        if !matches_qualified_name(&declaration.name, schema.name()) {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        declarations.push((
            declaration.span.clone(),
            DefinitionIdentity::Schema(schema.id()),
        ));
    }

    let mut binding_index = 0;
    for (declaration, definition) in parsed
        .primitive_value_types()
        .iter()
        .zip(catalogue.value_types())
    {
        if definition.kind() != ValueTypeKind::Primitive
            || !matches_qualified_name(&declaration.name, definition.name())
            || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
                != Some(definition.representation_contract())
            || source_persistence(declaration.persistence) != definition.persistence()
        {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        declarations.push((
            declaration.span.clone(),
            DefinitionIdentity::ValueType(definition.id()),
        ));

        let qualified = catalogue
            .type_bindings()
            .get(binding_index)
            .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
        let export = parsed
            .type_exports()
            .get(binding_index)
            .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
        if !matches_qualified_export(export, definition.name(), definition.id(), qualified) {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        declarations.push((
            export.span.clone(),
            DefinitionIdentity::TypeBinding(qualified.id()),
        ));
        binding_index += 1;

        while let Some(prelude) = catalogue.type_bindings().get(binding_index) {
            if prelude.kind() != orna_core::catalogue::TypeBindingKind::Prelude
                || prelude.target() != definition.id()
            {
                break;
            }
            let export = parsed
                .type_exports()
                .get(binding_index)
                .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
            if !matches_prelude_export(export, qualified, prelude) {
                return Err(StandardLibraryError::RetainedSourceMismatch);
            }
            declarations.push((
                export.span.clone(),
                DefinitionIdentity::TypeBinding(prelude.id()),
            ));
            binding_index += 1;
        }
    }

    let declaration = parsed
        .opaque_value_types()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let definition = catalogue
        .value_types()
        .get(VALUE_TYPE_FACTS.len())
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if parsed.opaque_value_types().len() != 1
        || definition.kind() != ValueTypeKind::Opaque
        || !matches_qualified_name(&declaration.name, definition.name())
        || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
            != Some(definition.representation_contract())
        || definition.persistence() != ValueTypePersistence::Transient
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    declarations.push((
        declaration.span.clone(),
        DefinitionIdentity::ValueType(definition.id()),
    ));
    let qualified = catalogue
        .type_bindings()
        .get(binding_index)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let export = parsed
        .type_exports()
        .get(binding_index)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_export(export, definition.name(), definition.id(), qualified) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    declarations.push((
        export.span.clone(),
        DefinitionIdentity::TypeBinding(qualified.id()),
    ));
    binding_index += 1;

    if binding_index != catalogue.type_bindings().len() || declarations.len() != 47 {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let expected_identities = declarations
        .iter()
        .map(|(_, identity)| *identity)
        .collect::<Vec<_>>();
    declarations.sort_by_key(|(span, _)| span.start);
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities)
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    declarations
        .into_iter()
        .map(|(span, identity)| {
            let start = u32::try_from(span.start)
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
            let end = u32::try_from(span.end)
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
            let source = SourceOrigin::new(unit_id, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?;
            Ok(DefinitionOrigin::new(identity, source))
        })
        .collect()
}

/// Reconciles the retained `std/invoke.orna` unit against the V2 catalogue.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `CREATE SCHEMA std.invoke;` declaration and the one `std.invoke.echo`
/// server function. The complete origin set is exactly the `std.invoke`
/// schema declaration, the function declaration, and the `p_value` parameter
/// declaration, each at its exact byte range in the retained unit. The closed
/// executable shape (parameter, result, security, transaction, volatility,
/// body, artifact, and references) is checked by the canonical compiler
/// checker in [`retained_v2_executable`].
fn reconcile_retained_invoke_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
        || !parsed.client_functions().is_empty()
        || parsed.schemas().len() != 1
        || parsed.server_functions().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let schema = &parsed.schemas()[0];
    let function = &parsed.server_functions()[0];
    let expected_schema = catalogue
        .schema_by_id(STD_INVOKE_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let expected_function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&schema.name, expected_schema.name())
        || !matches_qualified_name(&function.name, expected_function.name())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let parameter = function
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;

    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };

    Ok(vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
            origin(&schema.span)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
            origin(&function.span)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            },
            origin(&parameter.span)?,
        ),
    ])
}

/// Builds the retained V2 `StandardExecutable` from the retained invoke unit.
///
/// The canonical compiler checker validates the exact closed
/// `std.invoke.echo` source shape and returns the 44-byte
/// `orna.server-parameter-echo` artifact and the three ordered references at
/// their exact token ranges. The declaration-content digest and the
/// version-2 semantic digest are computed by the canonical encoders from the
/// retained declaration bytes and the checked function, artifact, and
/// references.
fn retained_v2_executable(
    invoke_source: &str,
    catalogue: &CatalogueSnapshot,
    invoke_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let parsed = orna_syntax::parse(invoke_source);
    let declaration = parsed
        .server_functions()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let checked = orna_compiler::check_standard_parameter_echo(
        declaration,
        catalogue,
        invoke_origins,
        INTEGER_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    if checked.artifact().content_hash() != ACCEPTED_V2_ARTIFACT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = invoke_origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &invoke_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        LANGUAGE_VERSION_IDENTITY,
        checked.artifact(),
        &[],
        checked.references(),
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if semantic_hash != ACCEPTED_V2_SEMANTIC_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        STD_INVOKE_ECHO_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        LANGUAGE_VERSION_IDENTITY,
        checked.artifact().clone(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);

    StandardExecutable::new(
        checked.function_id(),
        revision,
        checked.references().to_vec(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })
}

fn retained_json_executable(
    json_source: &str,
    catalogue: &CatalogueSnapshot,
    json_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let function = catalogue
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = json_origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &json_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let mut payload = Vec::with_capacity(44);
    payload.extend_from_slice(b"ORNAJE\0\0");
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&STD_JSON_ENCODE_PARAMETER_ID.to_bytes());
    payload.extend_from_slice(&STD_JSON_VALUE_TYPE_ID.to_bytes());
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-json-encode",
        1,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &[],
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let revision = FunctionRevisionRecord::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        1,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(STD_JSON_ENCODE_FUNCTION_ID, revision, Vec::new())
        .map_err(|source| StandardLibraryError::Revision { source })
}
fn retained_window_executable(
    window_source: &str,
    catalogue: &CatalogueSnapshot,
    window_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let parsed = orna_syntax::parse(window_source);
    let declaration = parsed
        .client_functions()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let checked = orna_compiler::check_standard_ui_window(declaration, catalogue, window_origins)
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let function = catalogue
        .function_by_id(STD_UI_WINDOW_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = window_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(STD_UI_WINDOW_FUNCTION_ID))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &window_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let source_origin = |span: &orna_syntax::SourceSpan| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_WINDOW_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let [title, content] = declaration.parameters.as_slice() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    let result = match &declaration.return_type {
        orna_syntax::FunctionReturnType::Single(result) => result,
        orna_syntax::FunctionReturnType::Rows { .. }
        | orna_syntax::FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    };
    let references = vec![
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            orna_core::revision::DefinitionReferenceTarget::ValueType(
                CHARACTER_LARGE_OBJECT_TYPE_ID,
            ),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(title.type_specification.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            orna_core::revision::DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(content.type_specification.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            orna_core::revision::DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(result.span())?,
        ),
    ];
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: STD_UI_WINDOW_CONTRACT.to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if artifact_hash != ACCEPTED_V7_WINDOW_ARTIFACT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if semantic_hash != ACCEPTED_V7_WINDOW_SEMANTIC_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        STD_UI_WINDOW_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryError::Revision { source })
}

fn retained_terminal_table_executable(
    data_source: &str,
    catalogue: &CatalogueSnapshot,
    data_origins: &[DefinitionOrigin],
) -> Result<StandardExecutable, StandardLibraryError> {
    let parsed = orna_syntax::parse(data_source);
    let declaration = parsed
        .server_functions()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let checked = orna_compiler::check_standard_terminal_present_table(
        declaration,
        catalogue,
        data_origins,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let function = catalogue
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let function_origin = data_origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let declaration_bytes = &data_source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let payload = server_terminal_table::TerminalTablePlan::new(
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?
    .encode()
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_terminal_table::FORMAT_IDENTITY,
        server_terminal_table::FORMAT_VERSION,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let source_origin = |span: &orna_syntax::SourceSpan| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_DATA_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let parameter = declaration
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let result = match &declaration.return_type {
        orna_syntax::FunctionReturnType::Single(result) => result,
        orna_syntax::FunctionReturnType::Rows { .. }
        | orna_syntax::FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    };
    let body = declaration
        .body
        .as_no_input_parameter_select()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let references = vec![
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            orna_core::revision::DefinitionReferenceTarget::ValueType(STD_DATA_ROWS_TYPE_ID),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            orna_core::revision::DefinitionReferenceTarget::ValueType(
                STD_TERMINAL_DOCUMENT_TYPE_ID,
            ),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(result.span())?,
        ),
        orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            orna_core::revision::DefinitionReferenceTarget::Parameter {
                owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            },
            orna_core::revision::DefinitionReferenceKind::ParameterRead,
            source_origin(&body.parameter.span)?,
        ),
    ];
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        u64::from(server_terminal_table::FORMAT_VERSION),
        function_origin,
        declaration_content_hash,
        semantic_hash,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryError::Revision { source })
}

fn retained_ui_constructor_executables(
    source: &str,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<Vec<StandardExecutable>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || parsed.client_functions().len() != 7
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let expected_functions = [
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
    ];
    let mut executables = Vec::with_capacity(expected_functions.len());
    for (index, declaration) in parsed.client_functions().iter().enumerate() {
        let function_id = expected_functions[index];
        let declaration_origins = origins
            .iter()
            .filter(|origin| match origin.identity() {
                DefinitionIdentity::Function(function) => function == function_id,
                DefinitionIdentity::Parameter { owner, .. } => owner == function_id,
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        let checked = orna_compiler::check_standard_ui_constructor(
            declaration,
            catalogue,
            &declaration_origins,
        )
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        executables.push(retained_ui_constructor_executable(
            source,
            catalogue,
            declaration,
            &declaration_origins,
            checked,
            index,
        )?);
    }
    Ok(executables)
}

fn retained_ui_constructor_executable(
    source: &str,
    catalogue: &CatalogueSnapshot,
    declaration: &orna_syntax::ClientFunctionDeclaration,
    origins: &[DefinitionOrigin],
    checked: CheckedStandardUiConstructor,
    index: usize,
) -> Result<StandardExecutable, StandardLibraryError> {
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(checked.function_id()))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .source();
    let source_origin = |span: &orna_syntax::SourceSpan| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let function = catalogue
        .function_by_id(checked.function_id())
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let mut references = Vec::with_capacity(declaration.parameters.len() + 1);
    for (ordinal, parameter) in declaration.parameters.iter().enumerate() {
        let target = function
            .parameters()
            .get(ordinal)
            .and_then(|parameter| parameter.resolved_type().value_type())
            .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
        references.push(orna_core::revision::DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            ordinal as u32,
            orna_core::revision::DefinitionReferenceTarget::ValueType(target),
            orna_core::revision::DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ));
    }
    let orna_syntax::FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    references.push(orna_core::revision::DefinitionReference::new(
        checked.function_id(),
        checked.revision_id(),
        declaration.parameters.len() as u32,
        orna_core::revision::DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
        orna_core::revision::DefinitionReferenceKind::NamedType,
        source_origin(result_type.span())?,
    ));
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: checked.runtime_contract().to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let expected_artifact_hashes = [
        ACCEPTED_V9_UI_TEXT_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_BUTTON_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_PANEL_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_ROW_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_COLUMN_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_TEXT_INPUT_ARTIFACT_DIGEST,
        ACCEPTED_V9_UI_TABS_ARTIFACT_DIGEST,
    ];
    if artifact_hash != expected_artifact_hashes[index] {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let expected_semantic_hashes = [
        ACCEPTED_V9_UI_TEXT_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_BUTTON_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_PANEL_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_ROW_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_COLUMN_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_TEXT_INPUT_SEMANTIC_DIGEST,
        ACCEPTED_V9_UI_TABS_SEMANTIC_DIGEST,
    ];
    if semantic_hash != expected_semantic_hashes[index] {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let declaration_bytes = &source.as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        1,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryError::Revision { source })
}

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

fn matches_qualified_name(source: &QualifiedName, expected: &QualifiedSemanticName) -> bool {
    source.parts.len() == expected.parts().len()
        && source
            .parts
            .iter()
            .zip(expected.parts())
            .all(|(part, expected)| is_unquoted(part) && part.text.eq_ignore_ascii_case(expected))
}

fn matches_prelude_words(source: &[NamePart], expected: &PreludeTypeName) -> bool {
    source.len() == expected.words().len()
        && source
            .iter()
            .zip(expected.words())
            .all(|(word, expected)| is_unquoted(word) && word.text.eq_ignore_ascii_case(expected))
}

fn is_unquoted(part: &NamePart) -> bool {
    !part.text.starts_with('"')
}

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

fn source_persistence(persistence: PrimitiveValueTypePersistence) -> ValueTypePersistence {
    match persistence {
        PrimitiveValueTypePersistence::Persistable => ValueTypePersistence::Persistable,
        PrimitiveValueTypePersistence::Transient => ValueTypePersistence::Transient,
    }
}

#[cfg(test)]
mod tests;
