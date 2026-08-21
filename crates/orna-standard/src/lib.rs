//! Source-independent facts for the Orna standard library.

use std::{error::Error, fmt};

use orna_compiler::{
    CheckedStandardLibrary, PrepareStandardUpgradeError, PreparedStandardUpgrade,
    StandardLibraryCheckError, check_standard_library_source, prepare_checked_standard_upgrade,
};
use orna_core::{
    CatalogueRevisionId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    StandardLibraryRevisionId, TypeBindingId, TypeId,
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
        InspectCarrierCodecRegistration, INSPECT_CARRIER_CODEC_REGISTRATIONS,
        OpaqueCodecRegistration, OpaqueCodecRegistry, OpaqueCodecRegistryError,
    },
};
use orna_syntax::{NamePart, PrimitiveValueTypePersistence, QualifiedName, TypeExportTarget};

pub use orna_compiler::StandardUpgradeIdentity;

pub use orna_compiler::{
    STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID,
    STD_INVOKE_SOURCE_UNIT_ID, STD_TYPES_SOURCE_UNIT_ID,
    STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_SCHEMA_ID, STD_JSON_VALUE_TYPE_ID,
};

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
    let json_binding = TypeBinding::qualified(json_name, STD_JSON_VALUE_TYPE_ID).map_err(|source| {
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
    let action_binding = TypeBinding::qualified(action_name, STD_ACTION_TYPE_ID).map_err(|source| {
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
/// This is the only path that selects `orna.std/3`. It fails closed when the
/// active revision pins any standard other than `orna.std/2` (an
/// already-installed V3 or a wrong V1 base). It retains and verifies the
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
    if let Some(installed) = active.catalogue_hash_context().standard()
        && installed.revision() != STANDARD_LIBRARY_V2_REVISION_ID
    {
        return Err(StandardUpgradeError::Prepare {
            source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: installed.revision(),
            },
        });
    }

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
/// This is the only path that selects `orna.std/4`. It fails closed when the
/// active revision pins any standard other than `orna.std/3` (an
/// already-installed V4 or a wrong V1/V2 base). It retains and verifies the
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
    if let Some(installed) = active.catalogue_hash_context().standard()
        && installed.revision() != STANDARD_LIBRARY_V3_REVISION_ID
    {
        return Err(StandardUpgradeError::Prepare {
            source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: installed.revision(),
            },
        });
    }

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
/// (ADR 0075). The retained V4 parent is verified before the V5 child.
pub fn prepare_standard_upgrade_v4_to_v5(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    if let Some(installed) = active.catalogue_hash_context().standard()
        && installed.revision() != STANDARD_LIBRARY_V4_REVISION_ID
    {
        return Err(StandardUpgradeError::Prepare {
            source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: installed.revision(),
            },
        });
    }
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
/// (ADR 0079). The retained V5 parent is verified before the V6 child.
pub fn prepare_standard_upgrade_v5_to_v6(
    active: &ActiveDatabaseRevision,
) -> Result<StandardUpgrade, StandardUpgradeError> {
    if let Some(installed) = active.catalogue_hash_context().standard()
        && installed.revision() != STANDARD_LIBRARY_V5_REVISION_ID
    {
        return Err(StandardUpgradeError::Prepare {
            source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: installed.revision(),
            },
        });
    }
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
        let start = u32::try_from(span.start)
            .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end = u32::try_from(span.end)
            .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
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
    if function.name.parts.iter().map(|part| part.text.as_str()).collect::<Vec<_>>()
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
    let parameter = function.parameters.first().ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start = u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_JSON_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let mut declarations = vec![
        origin(&parsed.schemas()[0].span, DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID))?,
        origin(&parsed.opaque_value_types()[0].span, DefinitionIdentity::ValueType(STD_JSON_VALUE_TYPE_ID))?,
        origin(&parsed.type_exports()[0].span, DefinitionIdentity::TypeBinding(binding.id()))?,
        origin(&function.span, DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID))?,
        origin(
            &parameter.span,
            DefinitionIdentity::Parameter {
                owner: STD_JSON_ENCODE_FUNCTION_ID,
                parameter: STD_JSON_ENCODE_PARAMETER_ID,
            },
        )?,
    ];
    declarations.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
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
/// `ORNA-UI/1 ` length-prefixed-canonical-JSON codec for `std.ui.UI` (work ADR 0062);
/// the accepted V6 snapshot additionally binds the bounded raw-byte action
/// codec (ADR 0079).
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

    let registrations = if is_accepted_v6_standard(standard) {
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
        let action = OpaqueCodecRegistration::length_prefixed_bytes(
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

/// Returns the deterministic checked-in contracts for the eight sealed
/// `sys.inspect` projection carriers. The snapshot uses a separate sealed
/// construction path because it has no projection tag.
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
        if !matches_qualified_name(&declaration.name, definition.name())
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
mod tests {
    use std::{cell::Cell, error::Error as _};

    use orna_core::catalogue::{
        CatalogueSnapshotError, PreludeTypeName, PreludeTypeNameError, QualifiedSemanticName,
        SemanticNameError, TypeBindingError, TypeBindingKind, TypeLookupName, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    };
    use orna_core::revision::DefinitionIdentity;
    use orna_core::system::{
        SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT, SYS_INSPECT_CALLS_TYPE_ID,
        SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT, SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
        SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT,
        SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID, SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT,
        SYS_INSPECT_RESOURCES_TYPE_ID, SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT,
        SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID, SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT,
        SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID, SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT,
        SYS_INSPECT_STATE_CELLS_TYPE_ID, SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT,
        SYS_INSPECT_UI_NODES_TYPE_ID,
    };
    use orna_core::{
        CatalogueRevisionId, SourceBundleId, SourceRevisionId, SourceUnitId, TypeId,
        canonical_hash::{
            artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
            function_semantic_digest_with_version, source_bundle_digest,
            source_revision_record_digest, source_unit_content_digest, standard_library_digest,
        },
        catalogue::CatalogueSnapshot,
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionReferenceKind, DefinitionReferenceTarget,
            ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord,
            FunctionSemanticHashVersion, RevisionPair, StandardExecutable,
            StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
            StoredSourceUnit,
        },
        value::{OpaqueValue, OpaqueValueError},
    };

    use super::{
        BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID, BYTE_STREAM_MAGIC,
        CHARACTER_LARGE_OBJECT_TYPE_ID, DATE_TYPE_ID, DECIMAL_TYPE_ID, DURATION_TYPE_ID,
        EXPECTED_TYPE_BINDING_IDS, FLOAT_TYPE_ID, INTEGER_TYPE_ID, LANGUAGE_VERSION_IDENTITY,
        OPAQUE_TOKEN_TYPE_ID, SOURCE_LOGICAL_PATH, STANDARD_CATALOGUE_REVISION_ID,
        STANDARD_CATALOGUE_V2_REVISION_ID, STANDARD_CATALOGUE_V3_REVISION_ID,
        STANDARD_CATALOGUE_V4_REVISION_ID, STANDARD_LIBRARY_REVISION_ID,
        STANDARD_LIBRARY_V2_REVISION_ID, STANDARD_LIBRARY_V2_VERSION_IDENTITY,
        STANDARD_LIBRARY_V3_REVISION_ID, STANDARD_LIBRARY_V3_VERSION_IDENTITY,
        STANDARD_LIBRARY_V4_REVISION_ID, STANDARD_LIBRARY_V4_VERSION_IDENTITY,
        STANDARD_LIBRARY_VERSION_IDENTITY, STANDARD_SOURCE_BUNDLE_ID,
        STANDARD_SOURCE_REVISION_ID, STANDARD_SOURCE_UNIT_ID, STANDARD_SOURCE_V2_BUNDLE_ID,
        STANDARD_SOURCE_V2_REVISION_ID, STANDARD_SOURCE_V3_BUNDLE_ID,
        STANDARD_SOURCE_V3_REVISION_ID, STANDARD_SOURCE_V4_BUNDLE_ID,
        STANDARD_SOURCE_V4_REVISION_ID, STANDARD_TYPE_IDS, STD_INTEGER_TYPE_ID,
        STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        STD_INVOKE_ECHO_PARAMETER_ID, STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID,
        STD_INVOKE_SOURCE_LOGICAL_PATH, STD_INVOKE_SOURCE_UNIT_ID, STD_IO_BYTE_STREAM_CONTRACT,
        STD_IO_BYTE_STREAM_TYPE_ID, STD_IO_SCHEMA_ID, STD_OUTPUT_SOURCE_LOGICAL_PATH,
        STD_OUTPUT_SOURCE_UNIT_ID, STD_SCHEMA_ID, STD_TERMINAL_DOCUMENT_CONTRACT,
        STD_TERMINAL_DOCUMENT_TYPE_ID, STD_TERMINAL_SCHEMA_ID, STD_TYPES_SCHEMA_ID,
        STD_TYPES_SOURCE_UNIT_ID, STD_UI_CONTRACT, STD_UI_SCHEMA_ID, STD_UI_SOURCE_LOGICAL_PATH,
        STD_UI_SOURCE_UNIT_ID, STD_UI_TYPE_ID, StandardLibraryError, StandardLibraryManifestError,
        StandardUpgradeError, TERMINAL_DOCUMENT_MAGIC, TIME_TYPE_ID, TIMESTAMP_TYPE_ID, UI_MAGIC,
        UUID_TYPE_ID, VOID_TYPE_ID, build_type_bindings, prepare_standard_upgrade,
        prepare_standard_upgrade_v1_to_v2, prepare_standard_upgrade_v2_to_v3,
        prepare_standard_upgrade_v4_to_v5, prepare_standard_upgrade_with,
        is_registered_inspect_carrier_type, registered_inspect_carrier_codecs,
        registered_opaque_codecs,
        retained_standard_library_snapshot, retained_standard_library_snapshot_from_source,
        retained_standard_library_v2_snapshot, retained_standard_library_v2_snapshot_from_source,
        retained_standard_library_v3_snapshot, retained_standard_library_v4_snapshot,
        retained_standard_library_v5_snapshot,
        standard_library_manifest, standard_library_v2_manifest, standard_library_v3_manifest,
        standard_library_v4_manifest, standard_library_v5_manifest,
        verify_standard_library_snapshot,
        verify_standard_library_v2_snapshot, verify_standard_library_v3_snapshot,
        verify_standard_library_v4_snapshot, verify_standard_library_v5_snapshot,
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
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])
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

    #[test]
    fn prepares_the_accepted_standard_upgrade_from_an_empty_active_revision() {
        let active = empty_active_revision();

        let upgrade = prepare_standard_upgrade(&active).expect("the standard upgrade prepares");

        assert_eq!(
            upgrade
                .checked_standard_library()
                .verified_snapshot()
                .revision(),
            STANDARD_LIBRARY_REVISION_ID
        );
        assert_eq!(
            upgrade.verified_standard_snapshot().revision(),
            STANDARD_LIBRARY_REVISION_ID
        );
        assert_eq!(
            upgrade.application_revision().expected_base(),
            active.pair()
        );
        assert_eq!(
            upgrade
                .application_revision()
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.revision()),
            Some(STANDARD_LIBRARY_REVISION_ID)
        );
    }

    #[test]
    fn standard_upgrade_stops_before_compiler_callbacks_when_accepted_verification_fails() {
        let accepted =
            retained_standard_library_snapshot().expect("the retained standard source is valid");
        let wrong_digest = orna_core::revision::StandardLibrarySnapshot::new(
            accepted.revision(),
            accepted.digest_version(),
            accepted.source().clone(),
            accepted.language_version(),
            accepted.catalogue().clone(),
            accepted.origins().to_vec(),
            orna_core::revision::Sha256Digest::from_bytes([0; 32]),
        )
        .expect("a different standard digest remains structurally valid");
        let active = empty_active_revision();
        let checker_calls = Cell::new(0);
        let preparation_calls = Cell::new(0);

        let error = prepare_standard_upgrade_with(
            &active,
            || Ok(wrong_digest),
            verify_standard_library_snapshot,
            |snapshot| {
                checker_calls.set(checker_calls.get() + 1);
                orna_compiler::check_standard_library_source(snapshot)
            },
            |standard, active| {
                preparation_calls.set(preparation_calls.get() + 1);
                orna_compiler::prepare_checked_standard_upgrade(standard, active)
            },
        )
        .expect_err("the accepted verifier rejects the different digest");

        assert!(matches!(
            error,
            StandardUpgradeError::StandardLibrary {
                source: StandardLibraryError::AcceptedDigestMismatch { expected, actual }
            } if expected == super::ACCEPTED_STANDARD_LIBRARY_DIGEST
                && actual == orna_core::revision::Sha256Digest::from_bytes([0; 32])
        ));
        assert_eq!(checker_calls.get(), 0);
        assert_eq!(preparation_calls.get(), 0);
    }

    #[test]
    fn standard_upgrade_maps_the_compiler_installed_gate_after_standard_acceptance() {
        let snapshot =
            retained_standard_library_snapshot().expect("the retained standard source is valid");
        let verified = verify_standard_library_snapshot(snapshot)
            .expect("the accepted standard source verifies");
        let active = empty_version_two_active_revision(&verified);

        let error =
            prepare_standard_upgrade(&active).expect_err("the standard is already installed");

        assert!(matches!(
            &error,
            StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if *revision == STANDARD_LIBRARY_REVISION_ID
        ));
        assert_eq!(
            error.to_string(),
            format!("standard library {STANDARD_LIBRARY_REVISION_ID} is already installed")
        );
        assert_eq!(
            error.source().map(ToString::to_string),
            Some(format!(
                "standard library {STANDARD_LIBRARY_REVISION_ID} is already installed"
            ))
        );
    }

    #[test]
    fn standard_upgrade_errors_are_transparent_and_preserve_their_fields() {
        let standard_library = StandardUpgradeError::StandardLibrary {
            source: StandardLibraryError::Unavailable,
        };
        let standard_source = StandardUpgradeError::StandardSource {
            source: orna_compiler::StandardLibraryCheckError::SourceUnitCount { actual: 9 },
        };
        let preparation = StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision: STANDARD_LIBRARY_REVISION_ID,
            },
        };

        assert!(matches!(
            standard_library,
            StandardUpgradeError::StandardLibrary {
                source: StandardLibraryError::Unavailable
            }
        ));
        assert!(matches!(
            standard_source,
            StandardUpgradeError::StandardSource {
                source: orna_compiler::StandardLibraryCheckError::SourceUnitCount { actual: 9 }
            }
        ));
        assert!(matches!(
            preparation,
            StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if revision == STANDARD_LIBRARY_REVISION_ID
        ));

        for (error, expected) in [
            (
                &standard_library,
                "the standard library is not installed".to_owned(),
            ),
            (
                &standard_source,
                "the verified standard library has 9 source units, expected exactly one".to_owned(),
            ),
            (
                &preparation,
                format!("standard library {STANDARD_LIBRARY_REVISION_ID} is already installed"),
            ),
        ] {
            assert_eq!(error.to_string(), expected);
            assert_eq!(error.source().map(ToString::to_string), Some(expected));
        }
    }

    #[test]
    fn manifest_exposes_the_reserved_staging_identities() {
        let manifest = standard_library_manifest().expect("the accepted manifest must be valid");
        let cloned = manifest.clone();

        assert_eq!(STANDARD_LIBRARY_VERSION_IDENTITY, "orna.std/1");
        assert_eq!(LANGUAGE_VERSION_IDENTITY, "orna.language/1");
        assert_eq!(SOURCE_LOGICAL_PATH, "std/types.orna");
        assert_eq!(
            manifest.standard_library_version(),
            STANDARD_LIBRARY_VERSION_IDENTITY
        );
        assert_eq!(
            manifest.standard_library_revision(),
            STANDARD_LIBRARY_REVISION_ID
        );
        assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_BUNDLE_ID);
        assert_eq!(manifest.source_revision(), STANDARD_SOURCE_REVISION_ID);
        assert_eq!(manifest.source_unit(), STANDARD_SOURCE_UNIT_ID);
        assert_eq!(manifest.source_logical_path(), SOURCE_LOGICAL_PATH);
        assert_eq!(
            manifest.catalogue().revision(),
            STANDARD_CATALOGUE_REVISION_ID
        );
        assert_eq!(manifest.catalogue().schemas().len(), 2);
        assert_eq!(manifest.catalogue().schemas()[0].id(), STD_SCHEMA_ID);
        assert_eq!(manifest.catalogue().schemas()[1].id(), STD_TYPES_SCHEMA_ID);
        assert_eq!(
            cloned.catalogue().revision(),
            STANDARD_CATALOGUE_REVISION_ID
        );
        assert_eq!(
            STANDARD_LIBRARY_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            STANDARD_CATALOGUE_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            STANDARD_SOURCE_BUNDLE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            STANDARD_SOURCE_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            STANDARD_SOURCE_UNIT_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            STD_SCHEMA_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            STD_TYPES_SCHEMA_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]
        );
    }

    #[test]
    fn retains_the_canonical_standard_source_as_an_unverified_snapshot() {
        let snapshot =
            retained_standard_library_snapshot().expect("the retained standard source is valid");

        assert_eq!(snapshot.revision(), STANDARD_LIBRARY_REVISION_ID);
        assert_eq!(
            snapshot.digest_version(),
            orna_core::revision::StandardLibraryDigestVersion::Version1
        );
        assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(
            snapshot.catalogue().revision(),
            STANDARD_CATALOGUE_REVISION_ID
        );
        assert_eq!(snapshot.source().id(), STANDARD_SOURCE_REVISION_ID);
        assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_BUNDLE_ID);
        assert_eq!(snapshot.source().parent(), None);
        assert_eq!(snapshot.source().units().len(), 1);
        assert_eq!(snapshot.source().units()[0].id(), STANDARD_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[0].ordinal(), 0);
        assert_eq!(
            snapshot.source().units()[0].logical_path(),
            SOURCE_LOGICAL_PATH
        );
    }

    #[test]
    fn retained_source_has_the_exact_literal_bytes_parse_and_hash_goldens() {
        let snapshot =
            retained_standard_library_snapshot().expect("the retained standard source is valid");
        let source = snapshot.source().units()[0].content();
        let parsed = orna_syntax::parse(source);

        assert_eq!(source, EXPECTED_RETAINED_STANDARD_SOURCE);
        assert_eq!(source.len(), 3463);
        assert!(source.is_ascii());
        assert!(!source.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
        assert!(!source.contains('\r'));
        assert!(source.ends_with('\n'));
        assert!(!source[..source.len() - 1].ends_with('\n'));
        assert_eq!(source.matches(';').count(), 47);
        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas().len(), 2);
        assert_eq!(parsed.primitive_value_types().len(), 13);
        assert_eq!(parsed.opaque_value_types().len(), 1);
        assert_eq!(parsed.type_exports().len(), 31);
        assert!(parsed.object_types().is_empty());
        assert!(parsed.field_renames().is_empty());
        assert!(parsed.server_functions().is_empty());
        assert!(parsed.client_functions().is_empty());
        assert_eq!(
            snapshot.source().units()[0].content_hash().to_bytes(),
            [
                0x5d, 0x53, 0x60, 0x01, 0xab, 0xc7, 0x54, 0xcf, 0x2c, 0xde, 0x9f, 0xf4, 0xed, 0x50,
                0xb2, 0x2d, 0xe8, 0xbb, 0x70, 0x04, 0x0a, 0x69, 0x1b, 0xc2, 0xec, 0x50, 0xbd, 0x6c,
                0x65, 0xe5, 0x25, 0xf4,
            ]
        );
        assert_eq!(
            snapshot.source().bundle_hash().to_bytes(),
            [
                0xd8, 0x0e, 0x8f, 0x73, 0x88, 0x78, 0x2d, 0x73, 0x0e, 0x4d, 0x6c, 0x5a, 0x6f, 0xcd,
                0x4a, 0x56, 0x42, 0xa4, 0x81, 0xcb, 0x65, 0x6d, 0x6e, 0x5f, 0xca, 0x35, 0x9a, 0x69,
                0xf3, 0x72, 0x63, 0xeb,
            ]
        );
        assert_eq!(
            snapshot.source().revision_hash().to_bytes(),
            [
                0x40, 0x0e, 0xb4, 0x35, 0x5d, 0xa2, 0x8f, 0x41, 0xf4, 0xd4, 0xae, 0x8c, 0x06, 0x21,
                0x24, 0x89, 0xbe, 0x60, 0xf6, 0xd8, 0x7c, 0x6d, 0x8e, 0xf3, 0x0c, 0x29, 0x1c, 0xc8,
                0x3b, 0x2c, 0xfb, 0x6b,
            ]
        );
        assert_eq!(
            snapshot.digest().to_bytes(),
            [
                0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56, 0xd4,
                0x89, 0xad, 0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70, 0x5b, 0x30,
                0x23, 0x5b, 0x08, 0x1d,
            ]
        );
    }

    #[test]
    fn retained_source_rejects_quoted_and_reordered_manifest_facts() {
        let quoted = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "std.types.BOOLEAN",
            "std.types.\"BOOLEAN\"",
            1,
        );
        let reordered = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;\nEXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;",
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;\nEXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
            1,
        );
        let changed_schema = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "CREATE SCHEMA std.types;",
            "CREATE SCHEMA std.other;",
            1,
        );
        let changed_contract = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "orna.kernel.value.boolean@1",
            "orna.kernel.value.boolean@2",
            1,
        );
        let changed_persistence =
            EXPECTED_RETAINED_STANDARD_SOURCE.replacen("    PERSISTABLE;", "    TRANSIENT;", 1);
        let changed_qualified_target = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;",
            "EXPORT TYPE std.types.BOOLEAN AS std.BOOL;",
            1,
        );
        let changed_prelude_source = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
            "EXPORT TYPE std.BOOL TO PRELUDE AS BOOLEAN;",
            1,
        );
        let changed_prelude_target = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;",
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
            1,
        );

        for source in [
            quoted,
            reordered,
            changed_schema,
            changed_contract,
            changed_persistence,
            changed_qualified_target,
            changed_prelude_source,
            changed_prelude_target,
        ] {
            assert!(matches!(
                retained_standard_library_snapshot_from_source(&source),
                Err(super::StandardLibraryError::RetainedSourceMismatch)
            ));
        }
    }

    #[test]
    fn retained_source_rejects_a_missing_complete_declaration() {
        let missing = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE\n    KERNEL CONTRACT 'orna.kernel.value.boolean@1'\n    IMMUTABLE\n    PERSISTABLE;\n\n",
            "",
            1,
        );

        assert!(matches!(
            retained_standard_library_snapshot_from_source(&missing),
            Err(super::StandardLibraryError::RetainedSourceMismatch)
        ));
    }

    #[test]
    fn retained_source_rejects_an_extra_declaration_or_category() {
        let extra_schema = format!("{EXPECTED_RETAINED_STANDARD_SOURCE}CREATE SCHEMA std.extra;\n");
        let extra_field_rename = format!(
            "{EXPECTED_RETAINED_STANDARD_SOURCE}ALTER TYPE std.types.BOOLEAN RENAME FIELD old TO new;\n"
        );

        for source in [extra_schema, extra_field_rename] {
            assert!(matches!(
                retained_standard_library_snapshot_from_source(&source),
                Err(super::StandardLibraryError::RetainedSourceMismatch)
            ));
        }
    }

    #[test]
    fn retained_source_rejects_a_valid_cross_type_export_association() {
        let crossed = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
            "EXPORT TYPE std.INTEGER TO PRELUDE AS BOOLEAN;",
            1,
        );
        let parsed = orna_syntax::parse(&crossed);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.type_exports().len(), 31);
        assert!(matches!(
            retained_standard_library_snapshot_from_source(&crossed),
            Err(super::StandardLibraryError::RetainedSourceMismatch)
        ));
    }

    #[test]
    fn retained_source_rejects_duplicate_schema_and_prelude_declarations() {
        let duplicate_schema = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "CREATE SCHEMA std.types;",
            "CREATE SCHEMA std;",
            1,
        );
        let duplicate_prelude = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;",
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
            1,
        );

        for source in [duplicate_schema, duplicate_prelude] {
            let parsed = orna_syntax::parse(&source);
            assert!(parsed.diagnostics().is_empty());
            assert!(matches!(
                retained_standard_library_snapshot_from_source(&source),
                Err(super::StandardLibraryError::RetainedSourceMismatch)
            ));
        }
    }

    #[test]
    fn retained_source_assigns_every_complete_declaration_its_exact_origin() {
        let snapshot =
            retained_standard_library_snapshot().expect("the retained standard source is valid");
        let source = snapshot.source().units()[0].content();
        let expected_spans = [
            (0, 18),
            (19, 43),
            (45, 174),
            (176, 221),
            (223, 269),
            (270, 313),
            (315, 444),
            (446, 491),
            (493, 539),
            (540, 582),
            (584, 711),
            (713, 756),
            (758, 802),
            (804, 929),
            (931, 972),
            (974, 1016),
            (1018, 1147),
            (1149, 1194),
            (1196, 1242),
            (1244, 1403),
            (1405, 1480),
            (1482, 1558),
            (1559, 1617),
            (1619, 1772),
            (1774, 1843),
            (1845, 1915),
            (1916, 1972),
            (1974, 2097),
            (2099, 2138),
            (2140, 2180),
            (2182, 2305),
            (2307, 2346),
            (2348, 2388),
            (2390, 2513),
            (2515, 2554),
            (2556, 2596),
            (2598, 2731),
            (2733, 2782),
            (2784, 2834),
            (2836, 2967),
            (2969, 3016),
            (3018, 3066),
            (3068, 3189),
            (3191, 3230),
            (3232, 3272),
            (3274, 3405),
            (3407, 3462),
        ];
        let expected_source_unit =
            orna_core::SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let expected_identities = [
            DefinitionIdentity::Schema(orna_core::SchemaId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            ])),
            DefinitionIdentity::Schema(orna_core::SchemaId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
                0x4d, 0x31,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
                0x15, 0xaa,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11,
                0x0c, 0xad,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf,
                0x25, 0x93,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18,
                0x57, 0x78,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c,
                0x58, 0x89,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b,
                0x0d, 0x1e,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa,
                0x5e, 0x88,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e,
                0xfc, 0xb5,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97,
                0xb5, 0xe6,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b,
                0x45, 0xf9,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10,
                0x6e, 0xd5,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2,
                0xd8, 0x70,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6,
                0x89, 0x1a,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0,
                0xdb, 0xc2,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d,
                0x81, 0x34,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33,
                0xaf, 0xbf,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6,
                0x9d, 0xb7,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31,
                0x66, 0x00,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c,
                0x79, 0x7f,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71,
                0xf7, 0xac,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f,
                0x93, 0x56,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0,
                0x34, 0x8d,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2,
                0x33, 0xd4,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc,
                0xff, 0x04,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c,
                0x8a, 0xa1,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 12,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62,
                0x14, 0x9a,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d,
                0xdc, 0x37,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 13,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf,
                0x94, 0x0f,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01,
                0x73, 0xfb,
            ])),
            DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14,
            ])),
            DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
                0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87, 0x63,
                0x46, 0xae,
            ])),
        ];

        assert_eq!(expected_spans.len(), 47);
        assert_eq!(expected_identities.len(), 47);
        assert_eq!(snapshot.origins().len(), 47);
        for ((origin, identity), (start, end)) in snapshot
            .origins()
            .iter()
            .zip(expected_identities)
            .zip(expected_spans)
        {
            assert_eq!(origin.identity(), identity);
            assert_eq!(origin.source().source_unit(), expected_source_unit);
            assert_eq!(origin.source().byte_start(), start);
            assert_eq!(origin.source().byte_end(), end);
            assert_eq!(
                &source[start as usize..end as usize],
                &EXPECTED_RETAINED_STANDARD_SOURCE[start as usize..end as usize]
            );
        }
        assert_eq!(snapshot.origins()[0].source().byte_start(), 0);
        assert_eq!(snapshot.origins()[1].source().byte_start(), 19);
        assert_eq!(snapshot.origins()[46].source().byte_end(), 3462);
        assert_eq!(&source[3462..], "\n");
    }

    #[test]
    fn standard_library_error_preserves_its_exact_public_contract() {
        let unavailable = StandardLibraryError::Unavailable;
        let manifest = super::StandardLibraryError::Manifest {
            source: StandardLibraryManifestError::TypeBindingCountMismatch {
                expected: 30,
                actual: 29,
            },
        };
        let retained = super::StandardLibraryError::RetainedSourceMismatch;
        let revision = super::StandardLibraryError::Revision {
            source: orna_core::revision::RevisionInvariantError::EmptyLogicalPath {
                source_unit: STANDARD_SOURCE_UNIT_ID,
            },
        };
        let canonical = super::StandardLibraryError::CanonicalHash {
            source: orna_core::canonical_hash::CanonicalHashError::SourceContentHashMismatch {
                source_unit: STANDARD_SOURCE_UNIT_ID,
            },
        };
        let catalogue = super::StandardLibraryError::CatalogueIdentityMismatch {
            expected: STANDARD_CATALOGUE_REVISION_ID,
            actual: orna_core::CatalogueRevisionId::from_bytes([2; 16]),
        };
        let digest = super::StandardLibraryError::AcceptedDigestMismatch {
            expected: orna_core::revision::Sha256Digest::from_bytes([3; 32]),
            actual: orna_core::revision::Sha256Digest::from_bytes([4; 32]),
        };

        assert_eq!(
            manifest.to_string(),
            "the standard library manifest is invalid: the standard library manifest has 29 type bindings, expected 30"
        );
        assert_eq!(
            unavailable.to_string(),
            "the standard library is not installed"
        );
        assert_eq!(
            retained.to_string(),
            "the retained standard library source does not match its manifest"
        );
        assert_eq!(
            revision.to_string(),
            "the retained standard library revision is invalid: stored source unit has an empty logical path"
        );
        assert_eq!(
            canonical.to_string(),
            "the standard library canonical hashes are invalid: stored source content hash differs from exact content"
        );
        assert_eq!(
            catalogue.to_string(),
            "the standard library catalogue identity does not match the reserved identity"
        );
        assert_eq!(
            digest.to_string(),
            "the standard library digest does not match the hard-coded accepted digest"
        );
        assert_eq!(
            manifest.source().map(ToString::to_string),
            Some("the standard library manifest has 29 type bindings, expected 30".to_owned())
        );
        assert!(unavailable.source().is_none());
        assert!(retained.source().is_none());
        assert_eq!(
            revision.source().map(ToString::to_string),
            Some("stored source unit has an empty logical path".to_owned())
        );
        assert_eq!(
            canonical.source().map(ToString::to_string),
            Some("stored source content hash differs from exact content".to_owned())
        );
        assert!(catalogue.source().is_none());
        assert!(digest.source().is_none());
    }

    #[test]
    fn verification_enforces_catalogue_digest_and_core_gates_in_order() {
        let accepted =
            retained_standard_library_snapshot().expect("the retained standard source is valid");
        let alternate_catalogue_id = orna_core::CatalogueRevisionId::from_bytes([2; 16]);
        let alternate_catalogue = orna_core::catalogue::CatalogueSnapshot::new_with_types(
            alternate_catalogue_id,
            accepted.catalogue().schemas().to_vec(),
            Vec::new(),
            accepted.catalogue().value_types().to_vec(),
            accepted.catalogue().type_bindings().to_vec(),
        )
        .expect("alternate catalogue remains structurally valid");
        let wrong_catalogue = orna_core::revision::StandardLibrarySnapshot::new(
            accepted.revision(),
            accepted.digest_version(),
            accepted.source().clone(),
            accepted.language_version(),
            alternate_catalogue,
            accepted.origins().to_vec(),
            orna_core::revision::Sha256Digest::from_bytes([0; 32]),
        )
        .expect("alternate snapshot remains structurally valid");
        let wrong_digest = orna_core::revision::StandardLibrarySnapshot::new(
            accepted.revision(),
            accepted.digest_version(),
            accepted.source().clone(),
            accepted.language_version(),
            accepted.catalogue().clone(),
            accepted.origins().to_vec(),
            orna_core::revision::Sha256Digest::from_bytes([0; 32]),
        )
        .expect("different digest does not affect structural validation");
        let invalid_source = orna_core::revision::StoredSourceRevision::new(
            accepted.source().bundle(),
            accepted.source().id(),
            accepted.source().parent(),
            accepted.source().units().to_vec(),
            orna_core::revision::Sha256Digest::from_bytes([0; 32]),
            accepted.source().revision_hash(),
        )
        .expect("incorrect source hash does not affect structural validation");
        let wrong_core = orna_core::revision::StandardLibrarySnapshot::new(
            accepted.revision(),
            accepted.digest_version(),
            invalid_source,
            accepted.language_version(),
            accepted.catalogue().clone(),
            accepted.origins().to_vec(),
            accepted.digest(),
        )
        .expect("incorrect source hash does not affect structural validation");

        assert!(matches!(
            verify_standard_library_snapshot(wrong_catalogue),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch { expected, actual })
                if expected == STANDARD_CATALOGUE_REVISION_ID && actual == alternate_catalogue_id
        ));
        assert!(matches!(
            verify_standard_library_snapshot(wrong_digest),
            Err(super::StandardLibraryError::AcceptedDigestMismatch { .. })
        ));
        assert!(matches!(
            verify_standard_library_snapshot(wrong_core),
            Err(super::StandardLibraryError::CanonicalHash { .. })
        ));
        let verified = verify_standard_library_snapshot(accepted)
            .expect("the accepted retained snapshot must grant authority");
        assert_eq!(verified.revision(), STANDARD_LIBRARY_REVISION_ID);
    }

    #[test]
    fn verification_rejects_a_core_accepted_self_consistent_non_golden_snapshot() {
        let accepted =
            retained_standard_library_snapshot().expect("the retained standard source is valid");
        let non_golden = orna_core::revision::StandardLibrarySnapshot::new(
            accepted.revision(),
            accepted.digest_version(),
            accepted.source().clone(),
            "orna.language/2",
            accepted.catalogue().clone(),
            accepted.origins().to_vec(),
            orna_core::revision::Sha256Digest::from_bytes([
                0x19, 0x65, 0xe6, 0xcb, 0xeb, 0x68, 0x77, 0xa6, 0xab, 0xea, 0x13, 0x14, 0xe9, 0x12,
                0xbe, 0xc5, 0xef, 0x12, 0xa9, 0x5b, 0xd3, 0x57, 0xdc, 0xee, 0xc9, 0xef, 0xb4, 0x54,
                0xf8, 0x4a, 0x98, 0xb2,
            ]),
        )
        .expect("the alternate standard snapshot is structurally valid");

        let core_verified =
            orna_core::canonical_hash::verify_standard_library_snapshot(non_golden.clone())
                .expect("the alternate standard is canonically self-consistent");
        assert_eq!(
            registered_opaque_codecs(&core_verified).unwrap_err(),
            super::RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot
        );
        assert!(matches!(
            verify_standard_library_snapshot(non_golden),
            Err(super::StandardLibraryError::AcceptedDigestMismatch { expected, actual })
                if expected == orna_core::revision::Sha256Digest::from_bytes([
                    0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56,
                    0xd4, 0x89, 0xad, 0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70,
                    0x5b, 0x30, 0x23, 0x5b, 0x08, 0x1d,
                ])
                && actual == orna_core::revision::Sha256Digest::from_bytes([
                    0x19, 0x65, 0xe6, 0xcb, 0xeb, 0x68, 0x77, 0xa6, 0xab, 0xea, 0x13, 0x14, 0xe9,
                    0x12, 0xbe, 0xc5, 0xef, 0x12, 0xa9, 0x5b, 0xd3, 0x57, 0xdc, 0xee, 0xc9, 0xef,
                    0xb4, 0x54, 0xf8, 0x4a, 0x98, 0xb2,
                ])
        ));
    }

    #[test]
    fn registered_opaque_codec_is_bound_to_the_accepted_active_standard() {
        let verified = verify_standard_library_snapshot(
            retained_standard_library_snapshot().expect("the retained standard source is valid"),
        )
        .expect("the accepted standard snapshot verifies");
        let registry = registered_opaque_codecs(&verified)
            .expect("the checked-in opaque codec matches the accepted standard");
        let active = empty_version_two_active_revision(&verified);
        let payload = [0xa5; 16];

        let value = OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload)
            .expect("the exact registered payload is accepted");

        assert_eq!(value.opaque_type(), OPAQUE_TOKEN_TYPE_ID);
        assert_eq!(value.canonical_payload(), payload);
    }

    #[test]
    fn manifest_contains_the_exact_standard_value_type_facts() {
        let manifest = standard_library_manifest().expect("the accepted manifest must be valid");
        let catalogue = manifest.catalogue();
        let expected = [
            (
                BOOLEAN_TYPE_ID,
                "std.types.boolean",
                "orna.kernel.value.boolean@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                INTEGER_TYPE_ID,
                "std.types.integer",
                "orna.kernel.value.integer@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                BIGINT_TYPE_ID,
                "std.types.bigint",
                "orna.kernel.value.bigint@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                FLOAT_TYPE_ID,
                "std.types.float",
                "orna.kernel.value.float@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                DECIMAL_TYPE_ID,
                "std.types.decimal",
                "orna.kernel.value.decimal@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                CHARACTER_LARGE_OBJECT_TYPE_ID,
                "std.types.character_large_object",
                "orna.kernel.value.character-large-object@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                BINARY_LARGE_OBJECT_TYPE_ID,
                "std.types.binary_large_object",
                "orna.kernel.value.binary-large-object@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                UUID_TYPE_ID,
                "std.types.uuid",
                "orna.kernel.value.uuid@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                DATE_TYPE_ID,
                "std.types.date",
                "orna.kernel.value.date@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                TIME_TYPE_ID,
                "std.types.time",
                "orna.kernel.value.time@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                TIMESTAMP_TYPE_ID,
                "std.types.timestamp",
                "orna.kernel.value.timestamp@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                DURATION_TYPE_ID,
                "std.types.duration",
                "orna.kernel.value.duration@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Persistable,
            ),
            (
                VOID_TYPE_ID,
                "std.types.void",
                "orna.kernel.value.void@1",
                ValueTypeKind::Primitive,
                ValueTypePersistence::Transient,
            ),
            (
                OPAQUE_TOKEN_TYPE_ID,
                "std.types.opaque_token",
                "orna.std.value.opaque-token@1",
                ValueTypeKind::Opaque,
                ValueTypePersistence::Transient,
            ),
        ];

        assert_eq!(STANDARD_TYPE_IDS, expected.map(|fact| fact.0));
        assert_eq!(
            STANDARD_TYPE_IDS.map(|id| id.to_bytes()),
            [
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 12],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 13],
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14],
            ]
        );
        assert_eq!(catalogue.schemas()[0].name().to_string(), "std");
        assert_eq!(catalogue.schemas()[1].name().to_string(), "std.types");
        assert!(catalogue.object_types().is_empty());
        assert!(catalogue.functions().is_empty());
        assert_eq!(catalogue.value_types().len(), expected.len());
        for (definition, (id, name, contract, kind, persistence)) in
            catalogue.value_types().iter().zip(expected)
        {
            assert_eq!(definition.id(), id);
            assert_eq!(definition.name().to_string(), name);
            assert_eq!(definition.kind(), kind);
            assert_eq!(definition.mutability(), ValueTypeMutability::Immutable);
            assert_eq!(definition.persistence(), persistence);
            assert_eq!(definition.representation_contract(), contract);
            let primary = TypeLookupName::qualified(definition.name().clone());
            assert_eq!(catalogue.type_id_by_name(&primary), Some(id));
        }
    }

    #[test]
    fn manifest_contains_the_exact_direct_binding_facts() {
        struct ExpectedBinding {
            kind: TypeBindingKind,
            name: &'static str,
            target: orna_core::TypeId,
            id: [u8; 16],
        }

        let expected = [
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.boolean",
                target: BOOLEAN_TYPE_ID,
                id: [
                    0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1,
                    0xdd, 0x4d, 0x31,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "boolean",
                target: BOOLEAN_TYPE_ID,
                id: [
                    0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e,
                    0xf8, 0x15, 0xaa,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "bool",
                target: BOOLEAN_TYPE_ID,
                id: [
                    0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61,
                    0x11, 0x0c, 0xad,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.integer",
                target: INTEGER_TYPE_ID,
                id: [
                    0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2,
                    0xdf, 0x25, 0x93,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "integer",
                target: INTEGER_TYPE_ID,
                id: [
                    0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf,
                    0x18, 0x57, 0x78,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "int",
                target: INTEGER_TYPE_ID,
                id: [
                    0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e,
                    0x7c, 0x58, 0x89,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.bigint",
                target: BIGINT_TYPE_ID,
                id: [
                    0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed,
                    0x5b, 0x0d, 0x1e,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "bigint",
                target: BIGINT_TYPE_ID,
                id: [
                    0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b,
                    0xaa, 0x5e, 0x88,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.float",
                target: FLOAT_TYPE_ID,
                id: [
                    0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71,
                    0x9e, 0xfc, 0xb5,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "float",
                target: FLOAT_TYPE_ID,
                id: [
                    0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87,
                    0x97, 0xb5, 0xe6,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.decimal",
                target: DECIMAL_TYPE_ID,
                id: [
                    0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8,
                    0x6b, 0x45, 0xf9,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "decimal",
                target: DECIMAL_TYPE_ID,
                id: [
                    0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3,
                    0x10, 0x6e, 0xd5,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.character_large_object",
                target: CHARACTER_LARGE_OBJECT_TYPE_ID,
                id: [
                    0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b,
                    0xc2, 0xd8, 0x70,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "character large object",
                target: CHARACTER_LARGE_OBJECT_TYPE_ID,
                id: [
                    0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3,
                    0xa6, 0x89, 0x1a,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "text",
                target: CHARACTER_LARGE_OBJECT_TYPE_ID,
                id: [
                    0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99,
                    0xf0, 0xdb, 0xc2,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.binary_large_object",
                target: BINARY_LARGE_OBJECT_TYPE_ID,
                id: [
                    0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c,
                    0x7d, 0x81, 0x34,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "binary large object",
                target: BINARY_LARGE_OBJECT_TYPE_ID,
                id: [
                    0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d,
                    0x33, 0xaf, 0xbf,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "bytes",
                target: BINARY_LARGE_OBJECT_TYPE_ID,
                id: [
                    0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51,
                    0xb6, 0x9d, 0xb7,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.uuid",
                target: UUID_TYPE_ID,
                id: [
                    0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f,
                    0x31, 0x66, 0x00,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "uuid",
                target: UUID_TYPE_ID,
                id: [
                    0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6,
                    0x7c, 0x79, 0x7f,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.date",
                target: DATE_TYPE_ID,
                id: [
                    0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1,
                    0x71, 0xf7, 0xac,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "date",
                target: DATE_TYPE_ID,
                id: [
                    0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e,
                    0x1f, 0x93, 0x56,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.time",
                target: TIME_TYPE_ID,
                id: [
                    0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8,
                    0xa0, 0x34, 0x8d,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "time",
                target: TIME_TYPE_ID,
                id: [
                    0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72,
                    0xd2, 0x33, 0xd4,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.timestamp",
                target: TIMESTAMP_TYPE_ID,
                id: [
                    0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30,
                    0xfc, 0xff, 0x04,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "timestamp",
                target: TIMESTAMP_TYPE_ID,
                id: [
                    0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97,
                    0x9c, 0x8a, 0xa1,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.duration",
                target: DURATION_TYPE_ID,
                id: [
                    0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb,
                    0x62, 0x14, 0x9a,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "duration",
                target: DURATION_TYPE_ID,
                id: [
                    0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80,
                    0x2d, 0xdc, 0x37,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.void",
                target: VOID_TYPE_ID,
                id: [
                    0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc,
                    0xbf, 0x94, 0x0f,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Prelude,
                name: "void",
                target: VOID_TYPE_ID,
                id: [
                    0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a,
                    0x01, 0x73, 0xfb,
                ],
            },
            ExpectedBinding {
                kind: TypeBindingKind::Qualified,
                name: "std.opaque_token",
                target: OPAQUE_TOKEN_TYPE_ID,
                id: [
                    0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87,
                    0x63, 0x46, 0xae,
                ],
            },
        ];
        let manifest = standard_library_manifest().expect("the accepted manifest must be valid");
        let catalogue = manifest.catalogue();

        assert_eq!(catalogue.type_bindings().len(), expected.len());
        assert_eq!(
            catalogue
                .type_bindings()
                .iter()
                .filter(|binding| binding.kind() == TypeBindingKind::Qualified)
                .count(),
            14
        );
        assert_eq!(
            catalogue
                .type_bindings()
                .iter()
                .filter(|binding| binding.kind() == TypeBindingKind::Prelude)
                .count(),
            17
        );
        for (binding, fact) in catalogue.type_bindings().iter().zip(expected) {
            assert_eq!(binding.kind(), fact.kind);
            assert_eq!(binding.name().to_string(), fact.name);
            assert_eq!(binding.target(), fact.target);
            assert_eq!(binding.id().to_bytes(), fact.id);
            assert_eq!(
                catalogue
                    .value_type_by_id(binding.target())
                    .map(|definition| definition.id()),
                Some(fact.target)
            );

            let lookup = if fact.kind == TypeBindingKind::Qualified {
                TypeLookupName::qualified(QualifiedSemanticName::new(fact.name.split('.')).unwrap())
            } else {
                assert_eq!(fact.kind, TypeBindingKind::Prelude);
                TypeLookupName::prelude(PreludeTypeName::new(fact.name.split(' ')).unwrap())
            };
            assert_eq!(catalogue.type_id_by_name(&lookup), Some(fact.target));
            assert_eq!(
                catalogue
                    .type_binding_by_name(&lookup)
                    .map(|item| item.id()),
                Some(binding.id())
            );
        }

        for absent in ["std.bool", "std.int", "std.text", "std.bytes"] {
            let lookup =
                TypeLookupName::qualified(QualifiedSemanticName::new(absent.split('.')).unwrap());
            assert_eq!(catalogue.type_id_by_name(&lookup), None);
        }
    }

    #[test]
    fn binding_identity_drift_is_a_typed_human_readable_error() {
        let mut changed_ids = EXPECTED_TYPE_BINDING_IDS;
        changed_ids[0] = [0; 16];

        let error = build_type_bindings(&changed_ids).unwrap_err();

        assert_eq!(
            error,
            StandardLibraryManifestError::TypeBindingIdentityMismatch {
                name: TypeLookupName::qualified(
                    QualifiedSemanticName::new(["std", "boolean"]).unwrap()
                ),
                expected: orna_core::TypeBindingId::from_bytes([0; 16]),
                actual: orna_core::TypeBindingId::from_bytes([
                    0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1,
                    0xdd, 0x4d, 0x31,
                ]),
            }
        );
        assert_eq!(
            error.to_string(),
            "standard library type binding std.boolean has identity type-binding:afrke7nfxydead3z2nef3qad64, expected type-binding:00000000000000000000000000"
        );
        assert!(error.source().is_none());
    }

    #[test]
    fn binding_identity_count_drift_fails_before_identity_comparison() {
        let shorter = build_type_bindings(&EXPECTED_TYPE_BINDING_IDS[..30]).unwrap_err();
        assert_eq!(
            shorter,
            StandardLibraryManifestError::TypeBindingCountMismatch {
                expected: 30,
                actual: 31,
            }
        );
        assert_eq!(
            shorter.to_string(),
            "the standard library manifest has 31 type bindings, expected 30"
        );
        assert!(shorter.source().is_none());

        let mut longer = EXPECTED_TYPE_BINDING_IDS.to_vec();
        longer.push([0; 16]);
        let longer = build_type_bindings(&longer).unwrap_err();
        assert_eq!(
            longer,
            StandardLibraryManifestError::TypeBindingCountMismatch {
                expected: 32,
                actual: 31,
            }
        );
        assert_eq!(
            longer.to_string(),
            "the standard library manifest has 31 type bindings, expected 32"
        );
        assert!(longer.source().is_none());
    }

    #[test]
    fn manifest_errors_preserve_exact_context_and_sources() {
        let semantic = StandardLibraryManifestError::SemanticName {
            name: "std.types.boolean".to_owned(),
            source: SemanticNameError::EmptyName,
        };
        assert_eq!(
            semantic.to_string(),
            "the standard library manifest contains an invalid semantic name std.types.boolean: a semantic name must contain at least one part"
        );
        assert_eq!(
            semantic.source().map(ToString::to_string),
            Some("a semantic name must contain at least one part".to_owned())
        );

        let prelude = StandardLibraryManifestError::PreludeName {
            name: "LARGE-OBJECT".to_owned(),
            source: PreludeTypeNameError::InvalidWord { index: 0 },
        };
        assert_eq!(
            prelude.to_string(),
            "the standard library manifest contains an invalid prelude name LARGE-OBJECT: prelude type name word 0 is not an unquoted SQL word"
        );
        assert_eq!(
            prelude.source().map(ToString::to_string),
            Some("prelude type name word 0 is not an unquoted SQL word".to_owned())
        );

        let unqualified = QualifiedSemanticName::new(["boolean"]).unwrap();
        let binding = StandardLibraryManifestError::TypeBinding {
            name: TypeLookupName::qualified(unqualified.clone()),
            source: TypeBindingError::QualifiedNameIsNotQualified { name: unqualified },
        };
        assert_eq!(
            binding.to_string(),
            "the standard library manifest contains an invalid type binding boolean: qualified type binding boolean has no schema namespace"
        );
        assert_eq!(
            binding.source().map(ToString::to_string),
            Some("qualified type binding boolean has no schema namespace".to_owned())
        );

        let count = StandardLibraryManifestError::TypeBindingCountMismatch {
            expected: 30,
            actual: 29,
        };
        assert_eq!(
            count.to_string(),
            "the standard library manifest has 29 type bindings, expected 30"
        );
        assert!(count.source().is_none());

        let catalogue = StandardLibraryManifestError::Catalogue {
            source: CatalogueSnapshotError::DuplicateSchemaId { id: STD_SCHEMA_ID },
        };
        assert_eq!(
            catalogue.to_string(),
            format!(
                "the standard library manifest cannot form a catalogue: duplicate schema identity {STD_SCHEMA_ID}"
            )
        );
        assert_eq!(
            catalogue.source().map(ToString::to_string),
            Some(format!("duplicate schema identity {STD_SCHEMA_ID}"))
        );
    }

    const EXPECTED_RETAINED_INVOKE_SOURCE: &str = r#"CREATE SCHEMA std.invoke;

CREATE SERVER FUNCTION std.invoke.echo(
    p_value INTEGER
)
RETURNS INTEGER
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT p_value;
"#;

    fn tampered_v2_snapshot(types: &str, invoke: &str) -> StandardLibrarySnapshot {
        // A structurally valid V2 snapshot whose unit content differs from the
        // retained source. The catalogue, origins, executable, and retained
        // digest are the accepted ones; only the source bytes and the
        // recomputed source hashes change, so the canonical digest encoder
        // must reject the resulting snapshot.
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");
        let types_unit = StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types,
            source_unit_content_digest(types).expect("the tampered types digest is valid"),
        )
        .expect("the tampered types unit is valid");
        let invoke_unit = StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke,
            source_unit_content_digest(invoke).expect("the tampered invoke digest is valid"),
        )
        .expect("the tampered invoke unit is valid");
        let units = vec![types_unit, invoke_unit];
        let bundle_hash =
            source_bundle_digest(&units).expect("the tampered bundle digest is valid");
        let source = StoredSourceRevision::new(
            STANDARD_SOURCE_V2_BUNDLE_ID,
            STANDARD_SOURCE_V2_REVISION_ID,
            Some(STANDARD_SOURCE_REVISION_ID),
            units,
            bundle_hash,
            source_revision_record_digest(
                STANDARD_SOURCE_V2_BUNDLE_ID,
                Some(STANDARD_SOURCE_REVISION_ID),
                bundle_hash,
            )
            .expect("the tampered source revision digest is valid"),
        )
        .expect("the tampered stored source revision is valid");
        StandardLibrarySnapshot::new_with_executables(
            STANDARD_LIBRARY_V2_REVISION_ID,
            StandardLibraryDigestVersion::Version2,
            source,
            LANGUAGE_VERSION_IDENTITY,
            snapshot.catalogue().clone(),
            snapshot.executables().to_vec(),
            snapshot.origins().to_vec(),
            snapshot.digest(),
        )
        .expect("the tampered V2 snapshot remains structurally valid")
    }

    #[test]
    fn manifest_v2_exposes_the_reserved_executable_standard_facts() {
        let manifest = standard_library_v2_manifest().expect("the accepted V2 manifest is valid");
        let cloned = manifest.clone();

        assert_eq!(STANDARD_LIBRARY_V2_VERSION_IDENTITY, "orna.std/2");
        assert_eq!(
            manifest.standard_library_version(),
            STANDARD_LIBRARY_V2_VERSION_IDENTITY
        );
        assert_eq!(
            manifest.standard_library_revision(),
            STANDARD_LIBRARY_V2_REVISION_ID
        );
        assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_V2_BUNDLE_ID);
        assert_eq!(manifest.source_revision(), STANDARD_SOURCE_V2_REVISION_ID);
        assert_eq!(manifest.types_source_unit(), STD_TYPES_SOURCE_UNIT_ID);
        assert_eq!(manifest.invoke_source_unit(), STD_INVOKE_SOURCE_UNIT_ID);
        assert_eq!(manifest.types_source_logical_path(), SOURCE_LOGICAL_PATH);
        assert_eq!(
            manifest.invoke_source_logical_path(),
            STD_INVOKE_SOURCE_LOGICAL_PATH
        );
        assert_eq!(
            manifest.catalogue().revision(),
            STANDARD_CATALOGUE_V2_REVISION_ID
        );
        assert_eq!(manifest.catalogue().schemas().len(), 3);
        assert_eq!(manifest.catalogue().schemas()[0].id(), STD_SCHEMA_ID);
        assert_eq!(manifest.catalogue().schemas()[1].id(), STD_TYPES_SCHEMA_ID);
        assert_eq!(manifest.catalogue().schemas()[2].id(), STD_INVOKE_SCHEMA_ID);
        assert_eq!(manifest.catalogue().value_types().len(), 14);
        assert_eq!(manifest.catalogue().type_bindings().len(), 31);
        assert_eq!(manifest.catalogue().functions().len(), 1);
        assert_eq!(
            manifest.catalogue().functions()[0].id(),
            STD_INVOKE_ECHO_FUNCTION_ID
        );
        assert_eq!(
            cloned.catalogue().revision(),
            STANDARD_CATALOGUE_V2_REVISION_ID
        );

        for (actual, expected) in [
            (
                STANDARD_LIBRARY_V2_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            ),
            (
                STANDARD_CATALOGUE_V2_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            ),
            (
                STANDARD_SOURCE_V2_BUNDLE_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            ),
            (
                STANDARD_SOURCE_V2_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            ),
            (
                STD_TYPES_SOURCE_UNIT_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            ),
            (
                STD_INVOKE_SOURCE_UNIT_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            ),
            (
                STD_INVOKE_SCHEMA_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            ),
            (
                STD_INVOKE_ECHO_FUNCTION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
            ),
            (
                STD_INVOKE_ECHO_PARAMETER_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
            ),
            (
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
            ),
            (
                INTEGER_TYPE_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            ),
        ] {
            assert_eq!(actual, expected);
        }
        assert_eq!(STD_INVOKE_ECHO_REVISION_NUMBER, 1);
        assert_eq!(INTEGER_TYPE_ID, STD_INTEGER_TYPE_ID);
    }

    #[test]
    fn retains_the_v2_executable_standard_snapshot() {
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");

        assert_eq!(snapshot.revision(), STANDARD_LIBRARY_V2_REVISION_ID);
        assert_eq!(
            snapshot.digest_version(),
            StandardLibraryDigestVersion::Version2
        );
        assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(
            snapshot.catalogue().revision(),
            STANDARD_CATALOGUE_V2_REVISION_ID
        );
        assert_eq!(snapshot.source().id(), STANDARD_SOURCE_V2_REVISION_ID);
        assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_V2_BUNDLE_ID);
        assert_eq!(
            snapshot.source().parent(),
            Some(STANDARD_SOURCE_REVISION_ID)
        );
        assert_eq!(snapshot.source().units().len(), 2);
        assert_eq!(snapshot.source().units()[0].id(), STD_TYPES_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[0].ordinal(), 0);
        assert_eq!(
            snapshot.source().units()[0].logical_path(),
            SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.source().units()[1].id(), STD_INVOKE_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[1].ordinal(), 1);
        assert_eq!(
            snapshot.source().units()[1].logical_path(),
            STD_INVOKE_SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.catalogue().schemas().len(), 3);
        assert_eq!(snapshot.catalogue().functions().len(), 1);
        assert_eq!(snapshot.origins().len(), 50);

        let [executable] = snapshot.executables() else {
            panic!("the V2 snapshot must retain exactly one executable");
        };
        assert_eq!(executable.function(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(
            executable.revision().id(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID
        );
        assert_eq!(
            executable.revision().revision_number(),
            STD_INVOKE_ECHO_REVISION_NUMBER
        );
        assert_eq!(
            executable.revision().semantic_hash_version(),
            FunctionSemanticHashVersion::Version2
        );
        assert_eq!(
            executable.revision().language_version(),
            LANGUAGE_VERSION_IDENTITY
        );
        assert_eq!(
            executable.revision().declaration_origin().source_unit(),
            STD_INVOKE_SOURCE_UNIT_ID
        );
        assert_eq!(executable.references().len(), 3);
        for (ordinal, reference) in executable.references().iter().enumerate() {
            assert_eq!(reference.ordinal(), ordinal as u32);
            assert_eq!(reference.source_function(), STD_INVOKE_ECHO_FUNCTION_ID);
            assert_eq!(
                reference.source_revision(),
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID
            );
            assert_eq!(
                reference.source_origin().source_unit(),
                STD_INVOKE_SOURCE_UNIT_ID
            );
        }
        assert_eq!(
            executable.references()[0].target(),
            DefinitionReferenceTarget::ValueType(INTEGER_TYPE_ID)
        );
        assert_eq!(
            executable.references()[0].kind(),
            DefinitionReferenceKind::NamedType
        );
        assert_eq!(
            executable.references()[1].target(),
            DefinitionReferenceTarget::ValueType(INTEGER_TYPE_ID)
        );
        assert_eq!(
            executable.references()[1].kind(),
            DefinitionReferenceKind::NamedType
        );
        assert_eq!(
            executable.references()[2].target(),
            DefinitionReferenceTarget::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            }
        );
        assert_eq!(
            executable.references()[2].kind(),
            DefinitionReferenceKind::ParameterRead
        );
    }

    #[test]
    fn v2_retained_invoke_source_has_the_exact_literal_bytes_and_parse() {
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();

        assert_eq!(invoke, EXPECTED_RETAINED_INVOKE_SOURCE);
        assert_eq!(invoke.len(), 185);
        assert!(invoke.is_ascii());
        assert!(!invoke.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
        assert!(!invoke.contains('\r'));
        assert!(invoke.ends_with('\n'));
        assert!(!invoke[..invoke.len() - 1].ends_with('\n'));
        assert_eq!(invoke.matches(';').count(), 2);
        assert_eq!(
            snapshot.source().units()[0].content(),
            super::RETAINED_STANDARD_SOURCE
        );
        assert_eq!(
            types,
            super::RETAINED_STANDARD_SOURCE,
            "the V2 types unit must retain the V1 bytes byte-for-byte"
        );

        let parsed = orna_syntax::parse(invoke);
        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), invoke);
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.server_functions().len(), 1);
        assert!(parsed.object_types().is_empty());
        assert!(parsed.field_renames().is_empty());
        assert!(parsed.primitive_value_types().is_empty());
        assert!(parsed.opaque_value_types().is_empty());
        assert!(parsed.type_exports().is_empty());
        assert!(parsed.client_functions().is_empty());
    }

    #[test]
    fn v2_invoke_origins_cover_the_exact_declaration_ranges() {
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");
        let invoke = snapshot.source().units()[1].content();
        let invoke_origins = snapshot
            .origins()
            .iter()
            .filter(|origin| origin.source().source_unit() == STD_INVOKE_SOURCE_UNIT_ID)
            .collect::<Vec<_>>();
        assert_eq!(invoke_origins.len(), 3);

        let schema_origin = invoke_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID))
            .expect("the schema origin is retained")
            .source();
        let function_origin = invoke_origins
            .iter()
            .find(|origin| {
                origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
            })
            .expect("the function origin is retained")
            .source();
        let parameter_origin = invoke_origins
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Parameter {
                        owner: STD_INVOKE_ECHO_FUNCTION_ID,
                        parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                    }
            })
            .expect("the parameter origin is retained")
            .source();

        let schema_end = "CREATE SCHEMA std.invoke;".len();
        let parameter_start = invoke.find("p_value").expect("the parameter is retained");
        let parameter_end = parameter_start + "p_value INTEGER".len();
        let function_start = invoke
            .find("CREATE SERVER FUNCTION")
            .expect("the function is retained");
        let function_end = invoke.rfind(';').expect("the declaration ends") + 1;

        assert_eq!(schema_origin.byte_start(), 0);
        assert_eq!(schema_origin.byte_end(), schema_end as u32);
        assert_eq!(&invoke[0..schema_end], "CREATE SCHEMA std.invoke;");
        assert_eq!(function_origin.byte_start(), function_start as u32);
        assert_eq!(function_origin.byte_end(), function_end as u32);
        assert_eq!(parameter_origin.byte_start(), parameter_start as u32);
        assert_eq!(parameter_origin.byte_end(), parameter_end as u32);
        assert_eq!(&invoke[parameter_start..parameter_end], "p_value INTEGER");

        let executable = &snapshot.executables()[0];
        let references = executable.references();
        let parameter_integer = invoke
            .find("INTEGER")
            .expect("the parameter type is retained");
        let result_integer = invoke
            .rfind("INTEGER")
            .expect("the result type is retained");
        let body_p_value = invoke
            .rfind("p_value")
            .expect("the body identifier is retained");
        assert_eq!(
            references[0].source_origin().byte_start(),
            parameter_integer as u32
        );
        assert_eq!(
            references[0].source_origin().byte_end(),
            parameter_integer as u32 + 7
        );
        assert_eq!(
            references[1].source_origin().byte_start(),
            result_integer as u32
        );
        assert_eq!(
            references[1].source_origin().byte_end(),
            result_integer as u32 + 7
        );
        assert_eq!(
            references[2].source_origin().byte_start(),
            body_p_value as u32
        );
        assert_eq!(
            references[2].source_origin().byte_end(),
            body_p_value as u32 + 7
        );
        assert_eq!(&invoke[parameter_integer..parameter_integer + 7], "INTEGER");
        assert_eq!(&invoke[result_integer..result_integer + 7], "INTEGER");
        assert_eq!(&invoke[body_p_value..body_p_value + 7], "p_value");
    }

    #[test]
    fn v2_digest_goldens_are_computed_from_the_retained_source() {
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();
        let units = snapshot.source().units().to_vec();
        let executable = &snapshot.executables()[0];
        let function = snapshot
            .catalogue()
            .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
            .expect("the echo function is retained");
        let artifact = executable.revision().artifact();
        let references = executable.references();

        assert_eq!(
            source_unit_content_digest(types).expect("the types content digest is valid"),
            super::ACCEPTED_V2_TYPES_CONTENT_DIGEST
        );
        assert_eq!(
            source_unit_content_digest(invoke).expect("the invoke content digest is valid"),
            super::ACCEPTED_V2_INVOKE_CONTENT_DIGEST
        );
        assert_eq!(
            source_bundle_digest(&units).expect("the bundle digest is valid"),
            super::ACCEPTED_V2_SOURCE_BUNDLE_DIGEST
        );
        assert_eq!(
            source_revision_record_digest(
                STANDARD_SOURCE_V2_BUNDLE_ID,
                Some(STANDARD_SOURCE_REVISION_ID),
                snapshot.source().bundle_hash(),
            )
            .expect("the source revision digest is valid"),
            super::ACCEPTED_V2_SOURCE_REVISION_DIGEST
        );
        assert_eq!(
            artifact_payload_digest(artifact.payload()).expect("the artifact digest is valid"),
            super::ACCEPTED_V2_ARTIFACT_DIGEST
        );
        assert_eq!(
            function_semantic_digest_with_version(
                FunctionSemanticHashVersion::Version2,
                function,
                LANGUAGE_VERSION_IDENTITY,
                artifact,
                &[],
                references,
            )
            .expect("the semantic digest is valid"),
            super::ACCEPTED_V2_SEMANTIC_DIGEST
        );
        assert_eq!(
            snapshot.digest(),
            super::ACCEPTED_V2_STANDARD_LIBRARY_DIGEST
        );
        assert_eq!(
            standard_library_digest(&snapshot).expect("the retained digest recomputes"),
            super::ACCEPTED_V2_STANDARD_LIBRARY_DIGEST
        );
    }

    #[test]
    fn v2_standard_digest_binds_every_retained_byte() {
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();

        let tampered_types = format!("{types} ");
        let tampered_invoke = format!("{invoke} ");
        for tampered in [
            tampered_v2_snapshot(&tampered_types, invoke),
            tampered_v2_snapshot(types, &tampered_invoke),
        ] {
            assert_eq!(
                tampered.digest(),
                super::ACCEPTED_V2_STANDARD_LIBRARY_DIGEST
            );
            assert!(matches!(
                standard_library_digest(&tampered),
                Err(
                    orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestMismatch { .. }
                )
            ));
            assert!(verify_standard_library_v2_snapshot(tampered).is_err());
        }
    }

    #[test]
    fn v2_snapshot_verifies_and_the_compiler_reconciles_the_bundle() {
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");
        let verified = verify_standard_library_v2_snapshot(snapshot)
            .expect("the retained V2 standard source verifies");
        assert_eq!(verified.revision(), STANDARD_LIBRARY_V2_REVISION_ID);
        assert_eq!(
            verified.digest_version(),
            StandardLibraryDigestVersion::Version2
        );

        let checked = orna_compiler::check_standard_library_source(&verified)
            .expect("the V2 standard source reconciles");
        assert_eq!(checked.schemas().len(), 2);
        assert_eq!(checked.value_types().len(), 14);
        assert_eq!(checked.type_bindings().len(), 31);
        let executable = checked
            .checked_executable()
            .expect("the V2 check retains the executable");
        assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(executable.parameter_id(), STD_INVOKE_ECHO_PARAMETER_ID);
        assert_eq!(
            executable.revision_id(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID
        );
        assert_eq!(
            executable.revision_number(),
            STD_INVOKE_ECHO_REVISION_NUMBER
        );
        assert_eq!(executable.references().len(), 3);
        assert_eq!(
            verified.executables()[0].references().len(),
            executable.references().len()
        );
    }

    #[test]
    fn v1_and_v2_snapshots_reject_each_others_verifiers() {
        let version_one =
            retained_standard_library_snapshot().expect("the retained V1 source is valid");
        let version_two =
            retained_standard_library_v2_snapshot().expect("the retained V2 source is valid");

        assert!(verify_standard_library_snapshot(version_one.clone()).is_ok());
        // The V2 wrapper rejects a V1 snapshot closed at the reserved
        // catalogue-identity gate before it reaches the canonical verifier.
        assert!(matches!(
            verify_standard_library_v2_snapshot(version_one.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V2_REVISION_ID
                && actual == STANDARD_CATALOGUE_REVISION_ID
        ));
        // The core V2 canonical verifier itself rejects the V1 digest version.
        assert!(matches!(
            orna_core::canonical_hash::verify_standard_library_v2_snapshot(version_one),
            Err(orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestVersionMismatch {
                expected: StandardLibraryDigestVersion::Version2,
                actual: StandardLibraryDigestVersion::Version1,
                ..
            })
        ));

        assert!(verify_standard_library_v2_snapshot(version_two.clone()).is_ok());
        assert!(matches!(
            verify_standard_library_snapshot(version_two.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_REVISION_ID
                && actual == STANDARD_CATALOGUE_V2_REVISION_ID
        ));
        assert!(matches!(
            orna_core::canonical_hash::verify_standard_library_snapshot(version_two),
            Err(orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestVersionMismatch {
                expected: StandardLibraryDigestVersion::Version1,
                actual: StandardLibraryDigestVersion::Version2,
                ..
            })
        ));
    }

    #[test]
    fn v2_artifact_is_the_exact_44_byte_parameter_echo() {
        let snapshot = retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid");
        let artifact = snapshot.executables()[0].revision().artifact();

        assert_eq!(artifact.kind(), ExecutableArtifactKind::Server);
        assert_eq!(artifact.format(), "orna.server-parameter-echo");
        assert_eq!(artifact.version(), 1);
        let payload = artifact.payload();
        assert_eq!(payload.len(), 44);
        assert_eq!(&payload[0..8], b"ORNAPE\0\0");
        assert_eq!(&payload[8..12], &1_u32.to_be_bytes());
        assert_eq!(&payload[12..28], STD_INVOKE_ECHO_PARAMETER_ID.to_bytes());
        assert_eq!(&payload[28..44], INTEGER_TYPE_ID.to_bytes());
        assert_eq!(
            artifact.content_hash(),
            artifact_payload_digest(payload).expect("the artifact digest is valid")
        );
        assert_eq!(
            snapshot.executables()[0].revision().semantic_hash_version(),
            FunctionSemanticHashVersion::Version2
        );
    }

    #[test]
    fn v2_retained_source_rejects_modified_invoke_bytes() {
        let modified =
            EXPECTED_RETAINED_INVOKE_SOURCE.replacen("VOLATILITY STABLE", "VOLATILITY VOLATILE", 1);
        assert!(matches!(
            retained_standard_library_v2_snapshot_from_source(
                super::RETAINED_STANDARD_SOURCE,
                &modified,
            ),
            Err(super::StandardLibraryError::RetainedSourceMismatch)
        ));

        let extra_schema = format!("{EXPECTED_RETAINED_INVOKE_SOURCE}CREATE SCHEMA std.extra;\n");
        assert!(matches!(
            retained_standard_library_v2_snapshot_from_source(
                super::RETAINED_STANDARD_SOURCE,
                &extra_schema,
            ),
            Err(super::StandardLibraryError::RetainedSourceMismatch)
        ));
    }

    #[test]
    fn prepares_the_v1_to_v2_standard_upgrade_from_an_empty_active_revision() {
        let active = empty_active_revision();

        let upgrade = prepare_standard_upgrade_v1_to_v2(&active)
            .expect("the V1-to-V2 standard upgrade prepares");

        assert_eq!(
            upgrade
                .checked_standard_library()
                .verified_snapshot()
                .revision(),
            STANDARD_LIBRARY_V2_REVISION_ID
        );
        assert_eq!(
            upgrade.verified_standard_snapshot().revision(),
            STANDARD_LIBRARY_V2_REVISION_ID
        );
        assert_eq!(
            upgrade.verified_standard_snapshot().source().parent(),
            Some(STANDARD_SOURCE_REVISION_ID),
            "V2 must be the append-only child of the retained V1 source revision"
        );
        let executable = upgrade
            .checked_standard_library()
            .checked_executable()
            .expect("the V2 upgrade retains the executable");
        assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(
            upgrade.application_revision().expected_base(),
            active.pair()
        );
        assert_eq!(
            upgrade
                .application_revision()
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.revision()),
            Some(STANDARD_LIBRARY_V2_REVISION_ID)
        );
        assert_eq!(
            upgrade
                .application_revision()
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.digest_version()),
            Some(StandardLibraryDigestVersion::Version2)
        );
    }

    #[test]
    fn v1_to_v2_standard_upgrade_fails_when_v2_is_already_installed() {
        let version_two = verify_standard_library_v2_snapshot(
            retained_standard_library_v2_snapshot()
                .expect("the retained V2 standard source is valid"),
        )
        .expect("the retained V2 standard source verifies");
        let active = empty_version_two_active_revision(&version_two);

        let error = prepare_standard_upgrade_v1_to_v2(&active)
            .expect_err("an installed V2 standard must close the upgrade");

        assert!(matches!(
            &error,
            StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if *revision == STANDARD_LIBRARY_V2_REVISION_ID
        ));
        assert_eq!(
            error.to_string(),
            format!("standard library {STANDARD_LIBRARY_V2_REVISION_ID} is already installed")
        );
    }

    #[test]
    fn v1_to_v2_standard_upgrade_fails_when_v1_is_pinned_or_the_base_is_not_expected() {
        let version_one = verify_standard_library_snapshot(
            retained_standard_library_snapshot().expect("the retained V1 source is valid"),
        )
        .expect("the retained V1 standard source verifies");
        let pinned = empty_version_two_active_revision(&version_one);

        let error = prepare_standard_upgrade_v1_to_v2(&pinned)
            .expect_err("a pinned V1 standard must close the upgrade");
        assert!(matches!(
            &error,
            StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if *revision == STANDARD_LIBRARY_REVISION_ID
        ));

        // A non-empty active revision with a reserved standard identity is not
        // the expected empty base and must fail closed.
        let occupied_source_unit = SourceUnitId::from_bytes([0x94; 16]);
        let occupied_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x92; 16]),
            vec![orna_core::catalogue::SchemaDefinition::new(
                STD_INVOKE_SCHEMA_ID,
                QualifiedSemanticName::new(["app"]).expect("the app schema name is valid"),
            )],
            Vec::new(),
        )
        .expect("the occupied catalogue is valid");
        let occupied_origin = orna_core::revision::DefinitionOrigin::new(
            DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
            orna_core::revision::SourceOrigin::new(occupied_source_unit, 0, 1)
                .expect("the occupied origin is valid"),
        );
        let occupied_unit = StoredSourceUnit::new(
            occupied_source_unit,
            0,
            "occupied.orna",
            " ",
            source_unit_content_digest(" ").expect("the occupied unit digest is valid"),
        )
        .expect("the occupied source unit is valid");
        let occupied = ActiveDatabaseRevision::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x91; 16]),
                CatalogueRevisionId::from_bytes([0x92; 16]),
            ),
            StoredSourceRevision::new(
                SourceBundleId::from_bytes([0x93; 16]),
                SourceRevisionId::from_bytes([0x91; 16]),
                None,
                vec![occupied_unit],
                source_bundle_digest(std::slice::from_ref(
                    &StoredSourceUnit::new(
                        occupied_source_unit,
                        0,
                        "occupied.orna",
                        " ",
                        source_unit_content_digest(" ").expect("the occupied unit digest is valid"),
                    )
                    .expect("the occupied source unit is valid"),
                ))
                .expect("the occupied source bundle digest is valid"),
                source_revision_record_digest(
                    SourceBundleId::from_bytes([0x93; 16]),
                    None,
                    source_bundle_digest(std::slice::from_ref(
                        &StoredSourceUnit::new(
                            occupied_source_unit,
                            0,
                            "occupied.orna",
                            " ",
                            source_unit_content_digest(" ")
                                .expect("the occupied unit digest is valid"),
                        )
                        .expect("the occupied source unit is valid"),
                    ))
                    .expect("the occupied source bundle digest is valid"),
                )
                .expect("the occupied source revision digest is valid"),
            )
            .expect("the occupied stored source revision is valid"),
            occupied_catalogue.clone(),
            catalogue_digest(
                &occupied_catalogue,
                &[],
                &[],
                std::slice::from_ref(&occupied_origin),
                &[],
            )
            .expect("the occupied catalogue digest is valid"),
            Vec::new(),
            Vec::new(),
            vec![occupied_origin],
            Vec::new(),
        )
        .expect("the occupied active revision is valid");

        let error = prepare_standard_upgrade_v1_to_v2(&occupied)
            .expect_err("a reserved identity must close the upgrade");
        assert!(matches!(
            &error,
            StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::ReservedIdentity { .. }
            }
        ));
    }

    const EXPECTED_RETAINED_OUTPUT_SOURCE: &str = r#"CREATE SCHEMA std.terminal;
CREATE SCHEMA std.io;

CREATE TYPE std.terminal.Document AS VALUE OPAQUE
    KERNEL CONTRACT 'orna.std.value.terminal-document@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.terminal.Document AS std.Document;

CREATE TYPE std.io.ByteStream AS VALUE OPAQUE
    KERNEL CONTRACT 'orna.std.value.byte-stream@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.io.ByteStream AS std.ByteStream;
"#;

    const EXPECTED_RETAINED_UI_SOURCE: &str = r#"CREATE SCHEMA std.ui;

CREATE TYPE std.ui.UI AS VALUE
    OPAQUE
    KERNEL CONTRACT 'orna.std.value.ui@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.ui.UI AS std.UI;
"#;

    fn tampered_v4_snapshot(
        types: &str,
        invoke: &str,
        output: &str,
        ui: &str,
    ) -> StandardLibrarySnapshot {
        // A structurally valid V4 snapshot whose unit content differs from the
        // retained source. The catalogue, origins, executable, and retained
        // digest are the accepted ones; only the source bytes and the
        // recomputed source hashes change, so the canonical digest encoder
        // must reject the resulting snapshot.
        let snapshot = retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid");
        let types_unit = StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types,
            source_unit_content_digest(types).expect("the tampered types digest is valid"),
        )
        .expect("the tampered types unit is valid");
        let invoke_unit = StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke,
            source_unit_content_digest(invoke).expect("the tampered invoke digest is valid"),
        )
        .expect("the tampered invoke unit is valid");
        let output_unit = StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output,
            source_unit_content_digest(output).expect("the tampered output digest is valid"),
        )
        .expect("the tampered output unit is valid");
        let ui_unit = StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui,
            source_unit_content_digest(ui).expect("the tampered ui digest is valid"),
        )
        .expect("the tampered ui unit is valid");
        let units = vec![types_unit, invoke_unit, output_unit, ui_unit];
        let bundle_hash =
            source_bundle_digest(&units).expect("the tampered bundle digest is valid");
        let source = StoredSourceRevision::new(
            STANDARD_SOURCE_V4_BUNDLE_ID,
            STANDARD_SOURCE_V4_REVISION_ID,
            Some(STANDARD_SOURCE_V3_REVISION_ID),
            units,
            bundle_hash,
            source_revision_record_digest(
                STANDARD_SOURCE_V4_BUNDLE_ID,
                Some(STANDARD_SOURCE_V3_REVISION_ID),
                bundle_hash,
            )
            .expect("the tampered source revision digest is valid"),
        )
        .expect("the tampered stored source revision is valid");
        StandardLibrarySnapshot::new_with_executables(
            STANDARD_LIBRARY_V4_REVISION_ID,
            StandardLibraryDigestVersion::Version2,
            source,
            LANGUAGE_VERSION_IDENTITY,
            snapshot.catalogue().clone(),
            snapshot.executables().to_vec(),
            snapshot.origins().to_vec(),
            snapshot.digest(),
        )
        .expect("the tampered V4 snapshot remains structurally valid")
    }

    fn tampered_v3_snapshot(types: &str, invoke: &str, output: &str) -> StandardLibrarySnapshot {
        // A structurally valid V3 snapshot whose unit content differs from the
        // retained source. The catalogue, origins, executable, and retained
        // digest are the accepted ones; only the source bytes and the
        // recomputed source hashes change, so the canonical digest encoder
        // must reject the resulting snapshot.
        let snapshot = retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid");
        let types_unit = StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types,
            source_unit_content_digest(types).expect("the tampered types digest is valid"),
        )
        .expect("the tampered types unit is valid");
        let invoke_unit = StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke,
            source_unit_content_digest(invoke).expect("the tampered invoke digest is valid"),
        )
        .expect("the tampered invoke unit is valid");
        let output_unit = StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output,
            source_unit_content_digest(output).expect("the tampered output digest is valid"),
        )
        .expect("the tampered output unit is valid");
        let units = vec![types_unit, invoke_unit, output_unit];
        let bundle_hash =
            source_bundle_digest(&units).expect("the tampered bundle digest is valid");
        let source = StoredSourceRevision::new(
            STANDARD_SOURCE_V3_BUNDLE_ID,
            STANDARD_SOURCE_V3_REVISION_ID,
            Some(STANDARD_SOURCE_V2_REVISION_ID),
            units,
            bundle_hash,
            source_revision_record_digest(
                STANDARD_SOURCE_V3_BUNDLE_ID,
                Some(STANDARD_SOURCE_V2_REVISION_ID),
                bundle_hash,
            )
            .expect("the tampered source revision digest is valid"),
        )
        .expect("the tampered stored source revision is valid");
        StandardLibrarySnapshot::new_with_executables(
            STANDARD_LIBRARY_V3_REVISION_ID,
            StandardLibraryDigestVersion::Version2,
            source,
            LANGUAGE_VERSION_IDENTITY,
            snapshot.catalogue().clone(),
            snapshot.executables().to_vec(),
            snapshot.origins().to_vec(),
            snapshot.digest(),
        )
        .expect("the tampered V3 snapshot remains structurally valid")
    }

    #[test]
    fn manifest_v3_exposes_the_reserved_output_standard_facts() {
        let manifest = standard_library_v3_manifest().expect("the accepted V3 manifest is valid");
        let cloned = manifest.clone();

        assert_eq!(STANDARD_LIBRARY_V3_VERSION_IDENTITY, "orna.std/3");
        assert_eq!(
            manifest.standard_library_version(),
            STANDARD_LIBRARY_V3_VERSION_IDENTITY
        );
        assert_eq!(
            manifest.standard_library_revision(),
            STANDARD_LIBRARY_V3_REVISION_ID
        );
        assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_V3_BUNDLE_ID);
        assert_eq!(manifest.source_revision(), STANDARD_SOURCE_V3_REVISION_ID);
        assert_eq!(manifest.types_source_unit(), STD_TYPES_SOURCE_UNIT_ID);
        assert_eq!(manifest.invoke_source_unit(), STD_INVOKE_SOURCE_UNIT_ID);
        assert_eq!(manifest.output_source_unit(), STD_OUTPUT_SOURCE_UNIT_ID);
        assert_eq!(manifest.types_source_logical_path(), SOURCE_LOGICAL_PATH);
        assert_eq!(
            manifest.invoke_source_logical_path(),
            STD_INVOKE_SOURCE_LOGICAL_PATH
        );
        assert_eq!(
            manifest.output_source_logical_path(),
            STD_OUTPUT_SOURCE_LOGICAL_PATH
        );
        assert_eq!(
            manifest.catalogue().revision(),
            STANDARD_CATALOGUE_V3_REVISION_ID
        );
        assert_eq!(manifest.catalogue().schemas().len(), 5);
        assert_eq!(
            manifest.catalogue().schemas()[3].id(),
            STD_TERMINAL_SCHEMA_ID
        );
        assert_eq!(manifest.catalogue().schemas()[4].id(), STD_IO_SCHEMA_ID);
        assert_eq!(manifest.catalogue().value_types().len(), 16);
        assert_eq!(manifest.catalogue().type_bindings().len(), 33);
        assert_eq!(manifest.catalogue().functions().len(), 1);
        assert_eq!(
            manifest.catalogue().functions()[0].id(),
            STD_INVOKE_ECHO_FUNCTION_ID
        );
        assert_eq!(
            cloned.catalogue().revision(),
            STANDARD_CATALOGUE_V3_REVISION_ID
        );

        let document = manifest
            .catalogue()
            .type_definition_by_id(STD_TERMINAL_DOCUMENT_TYPE_ID)
            .expect("the document type is retained")
            .as_opaque_value()
            .expect("the document type is opaque");
        assert_eq!(document.name().to_string(), "std.terminal.document");
        assert_eq!(
            document.representation_contract(),
            STD_TERMINAL_DOCUMENT_CONTRACT
        );
        let byte_stream = manifest
            .catalogue()
            .type_definition_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
            .expect("the byte-stream type is retained")
            .as_opaque_value()
            .expect("the byte-stream type is opaque");
        assert_eq!(byte_stream.name().to_string(), "std.io.bytestream");
        assert_eq!(
            byte_stream.representation_contract(),
            STD_IO_BYTE_STREAM_CONTRACT
        );

        for (actual, expected) in [
            (
                STANDARD_LIBRARY_V3_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            ),
            (
                STANDARD_CATALOGUE_V3_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            ),
            (
                STANDARD_SOURCE_V3_BUNDLE_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            ),
            (
                STANDARD_SOURCE_V3_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            ),
            (
                STD_OUTPUT_SOURCE_UNIT_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
            ),
            (
                STD_TERMINAL_SCHEMA_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
            ),
            (
                STD_IO_SCHEMA_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
            ),
            (
                STD_TERMINAL_DOCUMENT_TYPE_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0f],
            ),
            (
                STD_IO_BYTE_STREAM_TYPE_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
            ),
        ] {
            assert_eq!(actual, expected);
        }
        assert_eq!(
            STD_TERMINAL_DOCUMENT_CONTRACT,
            "orna.std.value.terminal-document@1"
        );
        assert_eq!(STD_IO_BYTE_STREAM_CONTRACT, "orna.std.value.byte-stream@1");
        assert_eq!(TERMINAL_DOCUMENT_MAGIC, "ORNA-TERMINAL-DOCUMENT/1 ");
        assert_eq!(BYTE_STREAM_MAGIC, "ORNA-BYTE-STREAM/1 ");
        assert_eq!(STD_OUTPUT_SOURCE_LOGICAL_PATH, "std/output.orna");
    }

    #[test]
    fn manifest_v4_exposes_the_reserved_ui_standard_facts() {
        let manifest = standard_library_v4_manifest().expect("the accepted V4 manifest is valid");
        let cloned = manifest.clone();

        assert_eq!(STANDARD_LIBRARY_V4_VERSION_IDENTITY, "orna.std/4");
        assert_eq!(
            manifest.standard_library_version(),
            STANDARD_LIBRARY_V4_VERSION_IDENTITY
        );
        assert_eq!(
            manifest.standard_library_revision(),
            STANDARD_LIBRARY_V4_REVISION_ID
        );
        assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_V4_BUNDLE_ID);
        assert_eq!(manifest.source_revision(), STANDARD_SOURCE_V4_REVISION_ID);
        assert_eq!(manifest.types_source_unit(), STD_TYPES_SOURCE_UNIT_ID);
        assert_eq!(manifest.invoke_source_unit(), STD_INVOKE_SOURCE_UNIT_ID);
        assert_eq!(manifest.output_source_unit(), STD_OUTPUT_SOURCE_UNIT_ID);
        assert_eq!(manifest.ui_source_unit(), STD_UI_SOURCE_UNIT_ID);
        assert_eq!(manifest.types_source_logical_path(), SOURCE_LOGICAL_PATH);
        assert_eq!(
            manifest.invoke_source_logical_path(),
            STD_INVOKE_SOURCE_LOGICAL_PATH
        );
        assert_eq!(
            manifest.output_source_logical_path(),
            STD_OUTPUT_SOURCE_LOGICAL_PATH
        );
        assert_eq!(
            manifest.ui_source_logical_path(),
            STD_UI_SOURCE_LOGICAL_PATH
        );
        assert_eq!(
            manifest.catalogue().revision(),
            STANDARD_CATALOGUE_V4_REVISION_ID
        );
        assert_eq!(manifest.catalogue().schemas().len(), 6);
        assert_eq!(manifest.catalogue().schemas()[5].id(), STD_UI_SCHEMA_ID);
        assert_eq!(manifest.catalogue().value_types().len(), 17);
        assert_eq!(manifest.catalogue().type_bindings().len(), 34);
        assert_eq!(manifest.catalogue().functions().len(), 1);
        assert_eq!(
            manifest.catalogue().functions()[0].id(),
            STD_INVOKE_ECHO_FUNCTION_ID
        );
        assert_eq!(
            cloned.catalogue().revision(),
            STANDARD_CATALOGUE_V4_REVISION_ID
        );

        let ui = manifest
            .catalogue()
            .type_definition_by_id(STD_UI_TYPE_ID)
            .expect("the ui type is retained")
            .as_opaque_value()
            .expect("the ui type is opaque");
        assert_eq!(ui.name().to_string(), "std.ui.ui");
        assert_eq!(ui.representation_contract(), STD_UI_CONTRACT);
        let ui_binding = manifest
            .catalogue()
            .type_bindings()
            .get(33)
            .expect("the ui binding is retained");
        assert_eq!(ui_binding.target(), STD_UI_TYPE_ID);
        assert_eq!(ui_binding.name().to_string(), "std.ui");

        for (actual, expected) in [
            (
                STANDARD_LIBRARY_V4_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
            ),
            (
                STANDARD_CATALOGUE_V4_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
            ),
            (
                STANDARD_SOURCE_V4_BUNDLE_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
            ),
            (
                STANDARD_SOURCE_V4_REVISION_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
            ),
            (
                STD_UI_SOURCE_UNIT_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
            ),
            (
                STD_UI_SCHEMA_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8],
            ),
            (
                STD_UI_TYPE_ID.to_bytes(),
                [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x13],
            ),
        ] {
            assert_eq!(actual, expected);
        }
        assert_eq!(STD_UI_CONTRACT, "orna.std.value.ui@1");
        assert_eq!(UI_MAGIC, "ORNA-UI/1 ");
        assert_eq!(STD_UI_SOURCE_LOGICAL_PATH, "std/ui.orna");
    }

    #[test]
    fn retains_the_v3_output_standard_snapshot() {
        let snapshot = retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid");

        assert_eq!(snapshot.revision(), STANDARD_LIBRARY_V3_REVISION_ID);
        assert_eq!(
            snapshot.digest_version(),
            StandardLibraryDigestVersion::Version2,
            "orna.std/3 reuses the V2 digest contract (work ADR 0058)"
        );
        assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(
            snapshot.catalogue().revision(),
            STANDARD_CATALOGUE_V3_REVISION_ID
        );
        assert_eq!(snapshot.source().id(), STANDARD_SOURCE_V3_REVISION_ID);
        assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_V3_BUNDLE_ID);
        assert_eq!(
            snapshot.source().parent(),
            Some(STANDARD_SOURCE_V2_REVISION_ID),
            "orna.std/3 must be the append-only child of orna.std/2"
        );
        assert_eq!(snapshot.source().units().len(), 3);
        assert_eq!(snapshot.source().units()[0].id(), STD_TYPES_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[0].ordinal(), 0);
        assert_eq!(
            snapshot.source().units()[0].logical_path(),
            SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.source().units()[1].id(), STD_INVOKE_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[1].ordinal(), 1);
        assert_eq!(
            snapshot.source().units()[1].logical_path(),
            STD_INVOKE_SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.source().units()[2].id(), STD_OUTPUT_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[2].ordinal(), 2);
        assert_eq!(
            snapshot.source().units()[2].logical_path(),
            STD_OUTPUT_SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.catalogue().schemas().len(), 5);
        assert_eq!(snapshot.catalogue().value_types().len(), 16);
        assert_eq!(snapshot.catalogue().type_bindings().len(), 33);
        assert_eq!(snapshot.catalogue().functions().len(), 1);
        assert_eq!(snapshot.origins().len(), 56);

        let [executable] = snapshot.executables() else {
            panic!("the V3 snapshot must retain exactly one executable");
        };
        assert_eq!(executable.function(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(
            executable.revision().id(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID
        );
        assert_eq!(
            executable.revision().revision_number(),
            STD_INVOKE_ECHO_REVISION_NUMBER
        );
    }

    #[test]
    fn v3_output_source_has_the_exact_literal_bytes_and_parse() {
        let snapshot = retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();
        let output = snapshot.source().units()[2].content();

        assert_eq!(output, EXPECTED_RETAINED_OUTPUT_SOURCE);
        assert!(output.is_ascii());
        assert!(!output.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
        assert!(!output.contains('\r'));
        assert!(output.ends_with('\n'));
        assert!(!output[..output.len() - 1].ends_with('\n'));
        assert_eq!(output.matches(';').count(), 6);
        assert_eq!(types, super::RETAINED_STANDARD_SOURCE);
        assert_eq!(invoke, super::RETAINED_STANDARD_INVOKE_SOURCE);

        let parsed = orna_syntax::parse(output);
        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), output);
        assert_eq!(parsed.schemas().len(), 2);
        assert_eq!(parsed.opaque_value_types().len(), 2);
        assert_eq!(parsed.type_exports().len(), 2);
        assert!(parsed.object_types().is_empty());
        assert!(parsed.field_renames().is_empty());
        assert!(parsed.primitive_value_types().is_empty());
        assert!(parsed.record_value_types().is_empty());
        assert!(parsed.enum_types().is_empty());
        assert!(parsed.server_functions().is_empty());
        assert!(parsed.client_functions().is_empty());
    }

    #[test]
    fn v3_output_origins_cover_the_exact_declaration_ranges() {
        let snapshot = retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid");
        let output = snapshot.source().units()[2].content();
        let output_origins = snapshot
            .origins()
            .iter()
            .filter(|origin| origin.source().source_unit() == STD_OUTPUT_SOURCE_UNIT_ID)
            .collect::<Vec<_>>();
        assert_eq!(output_origins.len(), 6);

        let terminal_schema_end = "CREATE SCHEMA std.terminal;".len();
        let io_schema_start = output
            .find("CREATE SCHEMA std.io;")
            .expect("the io schema is retained");
        let document_start = output
            .find("CREATE TYPE std.terminal.Document")
            .expect("the document type is retained");
        let document_end = output
            .find("TRANSIENT;")
            .expect("the document type is retained")
            + "TRANSIENT;".len();
        let document_binding_start = output
            .find("EXPORT TYPE std.terminal.Document AS std.Document;")
            .expect("the document binding is retained");
        let bytestream_start = output
            .find("CREATE TYPE std.io.ByteStream")
            .expect("the byte-stream type is retained");
        let bytestream_end = output
            .rfind("TRANSIENT;")
            .expect("the byte-stream type is retained")
            + "TRANSIENT;".len();

        let schema_origin = |id: orna_core::SchemaId| {
            output_origins
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::Schema(id))
                .expect("the schema origin is retained")
                .source()
        };
        let type_origin = |id: orna_core::TypeId| {
            output_origins
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::ValueType(id))
                .expect("the type origin is retained")
                .source()
        };
        let binding_origin = |id: orna_core::TypeBindingId| {
            output_origins
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::TypeBinding(id))
                .expect("the binding origin is retained")
                .source()
        };

        let terminal_schema = schema_origin(STD_TERMINAL_SCHEMA_ID);
        assert_eq!(terminal_schema.byte_start(), 0);
        assert_eq!(terminal_schema.byte_end(), terminal_schema_end as u32);
        let io_schema = schema_origin(STD_IO_SCHEMA_ID);
        assert_eq!(io_schema.byte_start(), io_schema_start as u32);
        assert_eq!(
            io_schema.byte_end(),
            (io_schema_start + "CREATE SCHEMA std.io;".len()) as u32
        );

        let document = type_origin(STD_TERMINAL_DOCUMENT_TYPE_ID);
        assert_eq!(document.byte_start(), document_start as u32);
        assert_eq!(document.byte_end(), document_end as u32);
        let document_binding = binding_origin(
            snapshot
                .catalogue()
                .type_bindings()
                .get(31)
                .expect("the document binding is retained")
                .id(),
        );
        assert_eq!(document_binding.byte_start(), document_binding_start as u32);
        assert_eq!(
            document_binding.byte_end(),
            (document_binding_start + "EXPORT TYPE std.terminal.Document AS std.Document;".len())
                as u32
        );

        let bytestream = type_origin(STD_IO_BYTE_STREAM_TYPE_ID);
        assert_eq!(bytestream.byte_start(), bytestream_start as u32);
        assert_eq!(bytestream.byte_end(), bytestream_end as u32);
        let bytestream_binding = binding_origin(
            snapshot
                .catalogue()
                .type_bindings()
                .get(32)
                .expect("the byte-stream binding is retained")
                .id(),
        );
        let bytestream_binding_start = output
            .find("EXPORT TYPE std.io.ByteStream AS std.ByteStream;")
            .expect("the byte-stream binding is retained");
        assert_eq!(
            bytestream_binding.byte_start(),
            bytestream_binding_start as u32
        );
        assert_eq!(
            bytestream_binding.byte_end(),
            (bytestream_binding_start + "EXPORT TYPE std.io.ByteStream AS std.ByteStream;".len())
                as u32
        );
    }

    #[test]
    fn v3_digest_goldens_are_computed_from_the_retained_source() {
        let snapshot = retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();
        let output = snapshot.source().units()[2].content();
        let units = snapshot.source().units().to_vec();
        let executable = &snapshot.executables()[0];
        let function = snapshot
            .catalogue()
            .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
            .expect("the echo function is retained");
        let artifact = executable.revision().artifact();
        let references = executable.references();

        assert_eq!(
            source_unit_content_digest(types).expect("the types content digest is valid"),
            super::ACCEPTED_V3_TYPES_CONTENT_DIGEST
        );
        assert_eq!(
            source_unit_content_digest(invoke).expect("the invoke content digest is valid"),
            super::ACCEPTED_V3_INVOKE_CONTENT_DIGEST
        );
        assert_eq!(
            source_unit_content_digest(output).expect("the output content digest is valid"),
            super::ACCEPTED_V3_OUTPUT_CONTENT_DIGEST
        );
        assert_eq!(
            source_bundle_digest(&units).expect("the bundle digest is valid"),
            super::ACCEPTED_V3_SOURCE_BUNDLE_DIGEST
        );
        assert_eq!(
            source_revision_record_digest(
                STANDARD_SOURCE_V3_BUNDLE_ID,
                Some(STANDARD_SOURCE_V2_REVISION_ID),
                snapshot.source().bundle_hash(),
            )
            .expect("the source revision digest is valid"),
            super::ACCEPTED_V3_SOURCE_REVISION_DIGEST
        );
        assert_eq!(
            artifact_payload_digest(artifact.payload()).expect("the artifact digest is valid"),
            super::ACCEPTED_V3_ARTIFACT_DIGEST,
            "orna.std/3 retains the exact V2 parameter-echo artifact"
        );
        assert_eq!(
            function_semantic_digest_with_version(
                FunctionSemanticHashVersion::Version2,
                function,
                LANGUAGE_VERSION_IDENTITY,
                artifact,
                &[],
                references,
            )
            .expect("the semantic digest is valid"),
            super::ACCEPTED_V3_SEMANTIC_DIGEST,
            "orna.std/3 retains the exact V2 semantic digest"
        );
        assert_eq!(
            snapshot.digest(),
            super::ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
        );
        assert_eq!(
            standard_library_digest(&snapshot).expect("the retained digest recomputes"),
            super::ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
        );
    }

    #[test]
    fn v3_standard_digest_binds_every_retained_byte() {
        let snapshot = retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();
        let output = snapshot.source().units()[2].content();

        let tampered_types = format!("{types} ");
        let tampered_invoke = format!("{invoke} ");
        let tampered_output = format!("{output} ");
        for tampered in [
            tampered_v3_snapshot(&tampered_types, invoke, output),
            tampered_v3_snapshot(types, &tampered_invoke, output),
            tampered_v3_snapshot(types, invoke, &tampered_output),
        ] {
            assert_eq!(
                tampered.digest(),
                super::ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
            );
            assert!(matches!(
                standard_library_digest(&tampered),
                Err(
                    orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestMismatch { .. }
                )
            ));
            assert!(verify_standard_library_v3_snapshot(tampered).is_err());
        }
    }

    #[test]
    fn v3_snapshot_verifies_and_rejects_and_is_rejected_by_the_other_verifiers() {
        let version_one =
            retained_standard_library_snapshot().expect("the retained V1 source is valid");
        let version_two =
            retained_standard_library_v2_snapshot().expect("the retained V2 source is valid");
        let version_three =
            retained_standard_library_v3_snapshot().expect("the retained V3 source is valid");

        assert!(verify_standard_library_v3_snapshot(version_three.clone()).is_ok());
        assert!(matches!(
            verify_standard_library_v3_snapshot(version_one.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V3_REVISION_ID
                && actual == STANDARD_CATALOGUE_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_v3_snapshot(version_two.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V3_REVISION_ID
                && actual == STANDARD_CATALOGUE_V2_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_snapshot(version_three.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_REVISION_ID
                && actual == STANDARD_CATALOGUE_V3_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_v2_snapshot(version_three.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V2_REVISION_ID
                && actual == STANDARD_CATALOGUE_V3_REVISION_ID
        ));
    }

    #[test]
    fn retains_the_v4_ui_standard_snapshot() {
        let snapshot = retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid");

        assert_eq!(snapshot.revision(), STANDARD_LIBRARY_V4_REVISION_ID);
        assert_eq!(
            snapshot.digest_version(),
            StandardLibraryDigestVersion::Version2,
            "orna.std/4 reuses the V2 digest contract (work ADR 0062)"
        );
        assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
        assert_eq!(
            snapshot.catalogue().revision(),
            STANDARD_CATALOGUE_V4_REVISION_ID
        );
        assert_eq!(snapshot.source().id(), STANDARD_SOURCE_V4_REVISION_ID);
        assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_V4_BUNDLE_ID);
        assert_eq!(
            snapshot.source().parent(),
            Some(STANDARD_SOURCE_V3_REVISION_ID),
            "orna.std/4 must be the append-only child of orna.std/3"
        );
        assert_eq!(snapshot.source().units().len(), 4);
        assert_eq!(snapshot.source().units()[0].id(), STD_TYPES_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[0].ordinal(), 0);
        assert_eq!(
            snapshot.source().units()[0].logical_path(),
            SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.source().units()[1].id(), STD_INVOKE_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[1].ordinal(), 1);
        assert_eq!(
            snapshot.source().units()[1].logical_path(),
            STD_INVOKE_SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.source().units()[2].id(), STD_OUTPUT_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[2].ordinal(), 2);
        assert_eq!(
            snapshot.source().units()[2].logical_path(),
            STD_OUTPUT_SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.source().units()[3].id(), STD_UI_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[3].ordinal(), 3);
        assert_eq!(
            snapshot.source().units()[3].logical_path(),
            STD_UI_SOURCE_LOGICAL_PATH
        );
        assert_eq!(snapshot.catalogue().schemas().len(), 6);
        assert_eq!(snapshot.catalogue().value_types().len(), 17);
        assert_eq!(snapshot.catalogue().type_bindings().len(), 34);
        assert_eq!(snapshot.catalogue().functions().len(), 1);
        assert_eq!(snapshot.origins().len(), 59);

        let [executable] = snapshot.executables() else {
            panic!("the V4 snapshot must retain exactly one executable");
        };
        assert_eq!(executable.function(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(
            executable.revision().id(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID
        );
        assert_eq!(
            executable.revision().revision_number(),
            STD_INVOKE_ECHO_REVISION_NUMBER
        );
    }

    #[test]
    fn v4_ui_source_has_the_exact_literal_bytes_and_parse() {
        let snapshot = retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();
        let output = snapshot.source().units()[2].content();
        let ui = snapshot.source().units()[3].content();

        assert_eq!(ui, EXPECTED_RETAINED_UI_SOURCE);
        assert!(ui.is_ascii());
        assert!(!ui.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
        assert!(!ui.contains('\r'));
        assert!(ui.ends_with('\n'));
        assert!(!ui[..ui.len() - 1].ends_with('\n'));
        assert_eq!(ui.matches(';').count(), 3);
        assert_eq!(types, super::RETAINED_STANDARD_SOURCE);
        assert_eq!(invoke, super::RETAINED_STANDARD_INVOKE_SOURCE);
        assert_eq!(output, super::RETAINED_STANDARD_OUTPUT_SOURCE);

        let parsed = orna_syntax::parse(ui);
        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), ui);
        assert_eq!(parsed.schemas().len(), 1);
        assert!(super::matches_qualified_name(
            &parsed.schemas()[0].name,
            &QualifiedSemanticName::new(["std", "ui"]).expect("the fixed schema name is valid")
        ));
        assert_eq!(parsed.opaque_value_types().len(), 1);
        assert_eq!(
            super::decode_sql_string_literal(&parsed.opaque_value_types()[0].kernel_contract.text)
                .as_deref(),
            Some(STD_UI_CONTRACT)
        );
        assert_eq!(parsed.type_exports().len(), 1);
        assert!(parsed.object_types().is_empty());
        assert!(parsed.field_renames().is_empty());
        assert!(parsed.primitive_value_types().is_empty());
        assert!(parsed.record_value_types().is_empty());
        assert!(parsed.enum_types().is_empty());
        assert!(parsed.server_functions().is_empty());
        assert!(parsed.client_functions().is_empty());
    }

    #[test]
    fn v4_ui_origins_cover_the_exact_declaration_ranges() {
        let snapshot = retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid");
        let ui = snapshot.source().units()[3].content();
        let ui_origins = snapshot
            .origins()
            .iter()
            .filter(|origin| origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID)
            .collect::<Vec<_>>();
        assert_eq!(ui_origins.len(), 3);

        let schema = ui_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID))
            .expect("the schema origin is retained")
            .source();
        assert_eq!(schema.byte_start(), 0);
        assert_eq!(schema.byte_end(), "CREATE SCHEMA std.ui;".len() as u32);

        let type_origin = ui_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(STD_UI_TYPE_ID))
            .expect("the type origin is retained")
            .source();
        let type_start = ui
            .find("CREATE TYPE std.ui.UI")
            .expect("the ui type is retained");
        let type_end = ui
            .find("TRANSIENT;")
            .expect("the ui type is retained")
            + "TRANSIENT;".len();
        assert_eq!(type_origin.byte_start(), type_start as u32);
        assert_eq!(type_origin.byte_end(), type_end as u32);

        let binding_origin = ui_origins
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::TypeBinding(
                        snapshot
                            .catalogue()
                            .type_bindings()
                            .get(33)
                            .expect("the ui binding is retained")
                            .id(),
                    )
            })
            .expect("the binding origin is retained")
            .source();
        let binding_start = ui
            .find("EXPORT TYPE std.ui.UI AS std.UI;")
            .expect("the ui binding is retained");
        assert_eq!(binding_origin.byte_start(), binding_start as u32);
        assert_eq!(
            binding_origin.byte_end(),
            (binding_start + "EXPORT TYPE std.ui.UI AS std.UI;".len()) as u32
        );
    }

    #[test]
    fn v4_digest_goldens_are_computed_from_the_retained_source() {
        let snapshot = retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();
        let output = snapshot.source().units()[2].content();
        let ui = snapshot.source().units()[3].content();
        let units = snapshot.source().units().to_vec();
        let executable = &snapshot.executables()[0];
        let function = snapshot
            .catalogue()
            .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
            .expect("the echo function is retained");
        let artifact = executable.revision().artifact();
        let references = executable.references();

        assert_eq!(
            source_unit_content_digest(types).expect("the types content digest is valid"),
            super::ACCEPTED_V4_TYPES_CONTENT_DIGEST
        );
        assert_eq!(
            source_unit_content_digest(invoke).expect("the invoke content digest is valid"),
            super::ACCEPTED_V4_INVOKE_CONTENT_DIGEST
        );
        assert_eq!(
            source_unit_content_digest(output).expect("the output content digest is valid"),
            super::ACCEPTED_V4_OUTPUT_CONTENT_DIGEST
        );
        assert_eq!(
            source_unit_content_digest(ui).expect("the ui content digest is valid"),
            super::ACCEPTED_V4_UI_CONTENT_DIGEST
        );
        assert_eq!(
            source_bundle_digest(&units).expect("the bundle digest is valid"),
            super::ACCEPTED_V4_SOURCE_BUNDLE_DIGEST
        );
        assert_eq!(
            source_revision_record_digest(
                STANDARD_SOURCE_V4_BUNDLE_ID,
                Some(STANDARD_SOURCE_V3_REVISION_ID),
                snapshot.source().bundle_hash(),
            )
            .expect("the source revision digest is valid"),
            super::ACCEPTED_V4_SOURCE_REVISION_DIGEST
        );
        assert_eq!(
            artifact_payload_digest(artifact.payload()).expect("the artifact digest is valid"),
            super::ACCEPTED_V4_ARTIFACT_DIGEST,
            "orna.std/4 retains the exact V2 parameter-echo artifact"
        );
        assert_eq!(
            function_semantic_digest_with_version(
                FunctionSemanticHashVersion::Version2,
                function,
                LANGUAGE_VERSION_IDENTITY,
                artifact,
                &[],
                references,
            )
            .expect("the semantic digest is valid"),
            super::ACCEPTED_V4_SEMANTIC_DIGEST,
            "orna.std/4 retains the exact V3 semantic digest"
        );
        assert_eq!(
            snapshot.digest(),
            super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
        );
        assert_eq!(
            standard_library_digest(&snapshot).expect("the retained digest recomputes"),
            super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
        );
    }

    #[test]
    fn v4_standard_digest_binds_every_retained_byte() {
        let snapshot = retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid");
        let types = snapshot.source().units()[0].content();
        let invoke = snapshot.source().units()[1].content();
        let output = snapshot.source().units()[2].content();
        let ui = snapshot.source().units()[3].content();

        let tampered_types = format!("{types} ");
        let tampered_invoke = format!("{invoke} ");
        let tampered_output = format!("{output} ");
        let tampered_ui = format!("{ui} ");
        for tampered in [
            tampered_v4_snapshot(&tampered_types, invoke, output, ui),
            tampered_v4_snapshot(types, &tampered_invoke, output, ui),
            tampered_v4_snapshot(types, invoke, &tampered_output, ui),
            tampered_v4_snapshot(types, invoke, output, &tampered_ui),
        ] {
            assert_eq!(
                tampered.digest(),
                super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
            );
            assert!(matches!(
                standard_library_digest(&tampered),
                Err(
                    orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestMismatch { .. }
                )
            ));
            assert!(verify_standard_library_v4_snapshot(tampered).is_err());
        }
    }

    #[test]
    fn v4_snapshot_verifies_and_rejects_and_is_rejected_by_the_other_verifiers() {
        let version_one =
            retained_standard_library_snapshot().expect("the retained V1 source is valid");
        let version_two =
            retained_standard_library_v2_snapshot().expect("the retained V2 source is valid");
        let version_three =
            retained_standard_library_v3_snapshot().expect("the retained V3 source is valid");
        let version_four =
            retained_standard_library_v4_snapshot().expect("the retained V4 source is valid");

        assert!(verify_standard_library_v4_snapshot(version_four.clone()).is_ok());
        assert!(matches!(
            verify_standard_library_v4_snapshot(version_one.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V4_REVISION_ID
                && actual == STANDARD_CATALOGUE_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_v4_snapshot(version_two.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V4_REVISION_ID
                && actual == STANDARD_CATALOGUE_V2_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_v4_snapshot(version_three.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V4_REVISION_ID
                && actual == STANDARD_CATALOGUE_V3_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_snapshot(version_four.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_REVISION_ID
                && actual == STANDARD_CATALOGUE_V4_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_v2_snapshot(version_four.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V2_REVISION_ID
                && actual == STANDARD_CATALOGUE_V4_REVISION_ID
        ));
        assert!(matches!(
            verify_standard_library_v3_snapshot(version_four.clone()),
            Err(super::StandardLibraryError::CatalogueIdentityMismatch {
                expected,
                actual
            }) if expected == STANDARD_CATALOGUE_V3_REVISION_ID
                && actual == STANDARD_CATALOGUE_V4_REVISION_ID
        ));
    }

#[test]
    fn v3_registered_opaque_codecs_construct_the_output_payloads() {
        let verified = verify_standard_library_v3_snapshot(
            retained_standard_library_v3_snapshot()
                .expect("the retained V3 standard source is valid"),
        )
        .expect("the retained V3 standard source verifies");
        let registry = registered_opaque_codecs(&verified).expect("the V3 opaque codecs register");
        let active = empty_version_two_active_revision(&verified);

        let mut document_payload = Vec::from(TERMINAL_DOCUMENT_MAGIC.as_bytes());
        document_payload.extend_from_slice(&5_u32.to_be_bytes());
        document_payload.extend_from_slice(b"hello");
        let document = OpaqueValue::new(
            &active,
            &registry,
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            &document_payload,
        )
        .expect("the terminal document payload constructs");
        assert_eq!(document.opaque_type(), STD_TERMINAL_DOCUMENT_TYPE_ID);
        assert_eq!(document.canonical_payload(), document_payload);

        let mut byte_stream_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        byte_stream_payload.extend_from_slice(&16_u32.to_be_bytes());
        byte_stream_payload.extend_from_slice(b"application/json");
        byte_stream_payload.extend_from_slice(&2_u32.to_be_bytes());
        byte_stream_payload.extend_from_slice(b"{}");
        let byte_stream = OpaqueValue::new(
            &active,
            &registry,
            STD_IO_BYTE_STREAM_TYPE_ID,
            &byte_stream_payload,
        )
        .expect("the byte-stream payload constructs");
        assert_eq!(byte_stream.opaque_type(), STD_IO_BYTE_STREAM_TYPE_ID);
        assert_eq!(byte_stream.canonical_payload(), byte_stream_payload);

        assert_eq!(
            OpaqueValue::new(
                &active,
                &registry,
                STD_TERMINAL_DOCUMENT_TYPE_ID,
                b"WRONG-DOCUMENT/1 \0\0\0\0",
            ),
            Err(OpaqueValueError::InvalidMagic {
                opaque_type: STD_TERMINAL_DOCUMENT_TYPE_ID,
            })
        );
        let mut empty_media_type = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        empty_media_type.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            OpaqueValue::new(
                &active,
                &registry,
                STD_IO_BYTE_STREAM_TYPE_ID,
                &empty_media_type,
            ),
            Err(OpaqueValueError::InvalidMediaType {
                opaque_type: STD_IO_BYTE_STREAM_TYPE_ID,
            })
        );

        // The V3 registry also retains the fixed-length opaque-token codec.
        let token = OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
            .expect("the opaque-token payload constructs");
        assert_eq!(token.opaque_type(), OPAQUE_TOKEN_TYPE_ID);

        // The V1 and V2 registries stay unchanged: only the opaque-token codec.
        let version_one = verify_standard_library_snapshot(
            retained_standard_library_snapshot().expect("the retained V1 source is valid"),
        )
        .expect("the retained V1 standard source verifies");
        let version_one_registry =
            registered_opaque_codecs(&version_one).expect("the V1 opaque codecs register");
        let version_two = verify_standard_library_v2_snapshot(
            retained_standard_library_v2_snapshot().expect("the retained V2 source is valid"),
        )
        .expect("the retained V2 standard source verifies");
        let version_two_registry =
            registered_opaque_codecs(&version_two).expect("the V2 opaque codecs register");
        let active_one = empty_version_two_active_revision(&version_one);
        let active_two = empty_version_two_active_revision(&version_two);
        for (active, registry) in [
            (&active_one, &version_one_registry),
            (&active_two, &version_two_registry),
        ] {
            assert_eq!(
                OpaqueValue::new(active, registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
                    .expect("the opaque-token payload constructs"),
                OpaqueValue::new(active, registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
                    .expect("the opaque-token payload constructs"),
            );
            assert_eq!(
                OpaqueValue::new(active, registry, STD_TERMINAL_DOCUMENT_TYPE_ID, [0; 16]),
                Err(OpaqueValueError::UnregisteredType {
                    opaque_type: STD_TERMINAL_DOCUMENT_TYPE_ID,
                })
            );
        }
    }

    #[test]
    fn v4_registered_opaque_codecs_construct_the_ui_payloads() {
        let verified = verify_standard_library_v4_snapshot(
            retained_standard_library_v4_snapshot()
                .expect("the retained V4 standard source is valid"),
        )
        .expect("the retained V4 standard source verifies");
        let registry = registered_opaque_codecs(&verified).expect("the V4 opaque codecs register");
        let active = empty_version_two_active_revision(&verified);

        let body = br#"{"kind":"empty"}"#;
        let mut ui_payload = Vec::from(UI_MAGIC.as_bytes());
        ui_payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        ui_payload.extend_from_slice(body);
        let ui = OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &ui_payload)
            .expect("the ui payload constructs");
        assert_eq!(ui.opaque_type(), STD_UI_TYPE_ID);
        assert_eq!(ui.canonical_payload(), ui_payload);

        let noncanonical_body = br#"{ "kind":"empty"}"#;
        let mut noncanonical_payload = Vec::from(UI_MAGIC.as_bytes());
        noncanonical_payload
            .extend_from_slice(&(noncanonical_body.len() as u32).to_be_bytes());
        noncanonical_payload.extend_from_slice(noncanonical_body);
        assert_eq!(
            OpaqueValue::new(
                &active,
                &registry,
                STD_UI_TYPE_ID,
                &noncanonical_payload,
            ),
            Err(OpaqueValueError::InvalidJsonBody {
                opaque_type: STD_UI_TYPE_ID,
            })
        );

        let malformed_body = br#"{"kind":}"#;
        let mut malformed_payload = Vec::from(UI_MAGIC.as_bytes());
        malformed_payload.extend_from_slice(&(malformed_body.len() as u32).to_be_bytes());
        malformed_payload.extend_from_slice(malformed_body);
        assert_eq!(
            OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &malformed_payload),
            Err(OpaqueValueError::InvalidJsonBody {
                opaque_type: STD_UI_TYPE_ID,
            })
        );

        let mut wrong_length_payload = ui_payload.clone();
        wrong_length_payload[UI_MAGIC.len()..UI_MAGIC.len() + 4]
            .copy_from_slice(&((body.len() as u32) - 1).to_be_bytes());
        assert_eq!(
            OpaqueValue::new(
                &active,
                &registry,
                STD_UI_TYPE_ID,
                &wrong_length_payload,
            ),
            Err(OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            })
        );

        // The core codec validates canonical JSON only; UI schema-shape
        // validation remains outside this registration's validator surface.
        let invalid_shape_body = br#"{"kind":"not-a-ui-kind"}"#;
        let mut invalid_shape_payload = Vec::from(UI_MAGIC.as_bytes());
        invalid_shape_payload
            .extend_from_slice(&(invalid_shape_body.len() as u32).to_be_bytes());
        invalid_shape_payload.extend_from_slice(invalid_shape_body);
        assert!(OpaqueValue::new(
            &active,
            &registry,
            STD_UI_TYPE_ID,
            &invalid_shape_payload,
        )
        .is_ok());

        // The V4 registry also binds the opaque-token, terminal-document, and
        // byte-stream codecs unchanged.
        let token = OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
            .expect("the opaque-token payload constructs");
        assert_eq!(token.opaque_type(), OPAQUE_TOKEN_TYPE_ID);
        let mut document_payload = Vec::from(TERMINAL_DOCUMENT_MAGIC.as_bytes());
        document_payload.extend_from_slice(&2_u32.to_be_bytes());
        document_payload.extend_from_slice(b"{}");
        let document = OpaqueValue::new(
            &active,
            &registry,
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            &document_payload,
        )
        .expect("the terminal document payload constructs");
        assert_eq!(document.opaque_type(), STD_TERMINAL_DOCUMENT_TYPE_ID);
        let mut byte_stream_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        byte_stream_payload.extend_from_slice(&16_u32.to_be_bytes());
        byte_stream_payload.extend_from_slice(b"application/json");
        byte_stream_payload.extend_from_slice(&0_u32.to_be_bytes());
        let byte_stream = OpaqueValue::new(
            &active,
            &registry,
            STD_IO_BYTE_STREAM_TYPE_ID,
            &byte_stream_payload,
        )
        .expect("the byte-stream payload constructs");
        assert_eq!(byte_stream.opaque_type(), STD_IO_BYTE_STREAM_TYPE_ID);

        assert_eq!(
            OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, b"WRONG-UI/1 \0\0\0\0"),
            Err(OpaqueValueError::InvalidMagic {
                opaque_type: STD_UI_TYPE_ID,
            })
        );
        // The V3 registry does not yet bind the ui codec.
        let version_three = verify_standard_library_v3_snapshot(
            retained_standard_library_v3_snapshot()
                .expect("the retained V3 standard source is valid"),
        )
        .expect("the retained V3 standard source verifies");
        let version_three_registry =
            registered_opaque_codecs(&version_three).expect("the V3 opaque codecs register");
        let active_three = empty_version_two_active_revision(&version_three);
        assert_eq!(
            OpaqueValue::new(&active_three, &version_three_registry, STD_UI_TYPE_ID, &ui_payload),
            Err(OpaqueValueError::UnregisteredType {
                opaque_type: STD_UI_TYPE_ID,
            })
        );
    }

    #[test]
    fn prepares_the_v2_to_v3_standard_upgrade_from_an_empty_active_revision() {
        let active = empty_active_revision();

        let upgrade = prepare_standard_upgrade_v2_to_v3(&active)
            .expect("the V2-to-V3 standard upgrade prepares");

        assert_eq!(
            upgrade
                .checked_standard_library()
                .verified_snapshot()
                .revision(),
            STANDARD_LIBRARY_V3_REVISION_ID
        );
        assert_eq!(
            upgrade.verified_standard_snapshot().revision(),
            STANDARD_LIBRARY_V3_REVISION_ID
        );
        assert_eq!(
            upgrade.verified_standard_snapshot().source().parent(),
            Some(STANDARD_SOURCE_V2_REVISION_ID),
            "V3 must be the append-only child of the retained V2 source revision"
        );
        let executable = upgrade
            .checked_standard_library()
            .checked_executable()
            .expect("the V3 upgrade retains the executable");
        assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(
            upgrade.application_revision().expected_base(),
            active.pair()
        );
        assert_eq!(
            upgrade
                .application_revision()
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.revision()),
            Some(STANDARD_LIBRARY_V3_REVISION_ID)
        );
        assert_eq!(
            upgrade
                .application_revision()
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.digest_version()),
            Some(StandardLibraryDigestVersion::Version2)
        );
    }

    #[test]
    fn prepare_standard_upgrade_v2_to_v3_fails_closed() {
        // A base that pins V1 is not the V2 parent and fails closed before the
        // V3 pipeline runs.
        let version_one = verify_standard_library_snapshot(
            retained_standard_library_snapshot().expect("the retained V1 source is valid"),
        )
        .expect("the retained V1 standard source verifies");
        let pinned_one = empty_version_two_active_revision(&version_one);
        let error =
            prepare_standard_upgrade_v2_to_v3(&pinned_one).expect_err("V1 is not the V2 base");
        assert!(matches!(
            &error,
            StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if *revision == STANDARD_LIBRARY_REVISION_ID
        ));
        assert_eq!(
            error.to_string(),
            format!("standard library {STANDARD_LIBRARY_REVISION_ID} is already installed")
        );

        // A base that already pins V3 cannot upgrade to the installed target.
        let version_three = verify_standard_library_v3_snapshot(
            retained_standard_library_v3_snapshot().expect("the retained V3 source is valid"),
        )
        .expect("the retained V3 standard source verifies");
        let pinned_three = empty_version_two_active_revision(&version_three);
        let error =
            prepare_standard_upgrade_v2_to_v3(&pinned_three).expect_err("V3 is not the V2 base");
        assert!(matches!(
            &error,
            StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if *revision == STANDARD_LIBRARY_V3_REVISION_ID
        ));
        assert_eq!(
            error.to_string(),
            format!("standard library {STANDARD_LIBRARY_V3_REVISION_ID} is already installed")
        );

        // A V2-pinned base is the exact append-only parent (work ADR 0059
        // upgrade pipeline): the shared machinery admits the source child and
        // prepares the V3 companion application revision on the V2 pair.
        let version_two = verify_standard_library_v2_snapshot(
            retained_standard_library_v2_snapshot().expect("the retained V2 source is valid"),
        )
        .expect("the retained V2 standard source verifies");
        let pinned_two = empty_version_two_active_revision(&version_two);
        let upgrade = prepare_standard_upgrade_v2_to_v3(&pinned_two)
            .expect("the installed V2 parent prepares the append-only V3 upgrade");
        assert_eq!(
            upgrade.verified_standard_snapshot().revision(),
            STANDARD_LIBRARY_V3_REVISION_ID
        );
        assert_eq!(
            upgrade.application_revision().expected_base(),
            pinned_two.pair()
        );
        assert_eq!(
            upgrade
                .application_revision()
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.revision()),
            Some(STANDARD_LIBRARY_V3_REVISION_ID)
        );
    }

    #[test]
    fn retains_and_verifies_v5_json_standard_snapshot() {
        let snapshot = super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 source is valid");
        assert_eq!(snapshot.source().units().len(), 5);
        let verified = super::verify_standard_library_v5_snapshot(snapshot)
            .expect("the retained V5 source verifies");
        assert!(super::registered_opaque_codecs(&verified).is_ok());
    }

    #[test]
    fn rejects_a_tampered_v5_json_source_byte_before_verification() {
        let mut json_source = super::RETAINED_STANDARD_JSON_SOURCE.to_owned();
        json_source.push('\n');
        let error = super::retained_standard_library_v5_snapshot_from_source(
            super::RETAINED_STANDARD_SOURCE,
            super::RETAINED_STANDARD_INVOKE_SOURCE,
            super::RETAINED_STANDARD_OUTPUT_SOURCE,
            super::RETAINED_STANDARD_UI_SOURCE,
            &json_source,
        )
        .expect_err("a changed V5 source byte must be rejected");
        assert!(matches!(
            error,
            super::StandardLibraryError::RetainedSourceMismatch
        ));
    }

    #[test]
    fn rejects_a_tampered_v5_json_executable_through_compiler_dispatch() {
        let snapshot = super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 source is valid");
        let json_index = snapshot
            .executables()
            .iter()
            .position(|executable| executable.function() == super::STD_JSON_ENCODE_FUNCTION_ID)
            .expect("the retained V5 snapshot contains the JSON executable");
        let original = &snapshot.executables()[json_index];
        let revision = original.revision();
        let mut payload = revision.artifact().payload().to_vec();
        payload.push(0);
        let content_hash =
            artifact_payload_digest(&payload).expect("the tampered payload can be hashed");
        let artifact = ExecutableArtifact::new(
            revision.artifact().kind(),
            revision.artifact().format(),
            revision.artifact().version(),
            payload,
            content_hash,
        )
        .expect("the tampered artifact remains structurally valid");
        let function = snapshot
            .catalogue()
            .function_by_id(super::STD_JSON_ENCODE_FUNCTION_ID)
            .expect("the retained V5 catalogue contains the JSON function");
        let semantic_hash = function_semantic_digest_with_version(
            revision.semantic_hash_version(),
            function,
            revision.language_version(),
            &artifact,
            &[],
            original.references(),
        )
        .expect("the tampered semantic hash can be calculated");
        let tampered_revision = FunctionRevisionRecord::new(
            revision.function(),
            revision.id(),
            revision.revision_number(),
            revision.declaration_origin(),
            revision.declaration_content_hash(),
            semantic_hash,
            revision.language_version(),
            artifact,
        )
        .expect("the tampered revision remains structurally valid")
        .with_semantic_hash_version(revision.semantic_hash_version());
        let tampered_executable = StandardExecutable::new(
            original.function(),
            tampered_revision,
            original.references().to_vec(),
        )
        .expect("the tampered executable remains structurally valid");
        let mut executables = snapshot.executables().to_vec();
        executables[json_index] = tampered_executable;
        let build_snapshot = |digest| {
            StandardLibrarySnapshot::new_with_executables(
                snapshot.revision(),
                snapshot.digest_version(),
                snapshot.source().clone(),
                snapshot.language_version(),
                snapshot.catalogue().clone(),
                executables.clone(),
                snapshot.origins().to_vec(),
                digest,
            )
            .expect("the tampered snapshot remains structurally valid")
        };
        let provisional = build_snapshot(snapshot.digest());
        let digest = orna_core::canonical_hash::calculate_standard_library_digest(&provisional)
            .expect("the tampered snapshot digest can be calculated");
        let tampered_snapshot = build_snapshot(digest);
        let verified = super::verify_canonical_standard_library_v2_snapshot(tampered_snapshot)
            .expect("the tampered snapshot verifies with its recalculated digest");
        let error = orna_compiler::check_standard_library_source(&verified)
            .expect_err("the V5 compiler path must reject the tampered executable");
        assert!(matches!(
            error,
            orna_compiler::StandardLibraryCheckError::ExecutableMismatch
        ));
    }

    #[test]
    fn prepares_the_v4_to_v5_standard_upgrade_from_an_empty_v4_active_revision() {
        let version_four = super::verify_standard_library_v4_snapshot(
            super::retained_standard_library_v4_snapshot()
                .expect("the retained V4 standard source is valid"),
        )
        .expect("the retained V4 standard source verifies");
        let version_five = super::verify_standard_library_v5_snapshot(
            super::retained_standard_library_v5_snapshot()
                .expect("the retained V5 standard source is valid"),
        )
        .expect("the retained V5 standard source verifies");
        orna_compiler::check_standard_library_source(&version_five)
            .unwrap_or_else(|error| panic!("the V5 source must check: {error:?}"));
        let active = empty_version_two_active_revision(&version_four);
        let upgrade = super::prepare_standard_upgrade_v4_to_v5(&active)
            .unwrap_or_else(|error| panic!("the V4-to-V5 upgrade must prepare: {error:?}"));
        assert_eq!(
            upgrade.verified_standard_snapshot().revision(),
            super::STANDARD_LIBRARY_V5_REVISION_ID
        );
        assert_eq!(upgrade.verified_standard_snapshot().executables().len(), 2);
        assert_eq!(
            upgrade
                .checked_standard_library()
                .checked_executable()
                .expect("the V5 upgrade retains the echo executable")
                .function_id(),
            super::STD_INVOKE_ECHO_FUNCTION_ID
        );
    }
    #[test]
    fn v5_to_v6_upgrade_rejects_a_non_v5_parent_before_child_work() {
        let v4 = super::verify_standard_library_v4_snapshot(
            super::retained_standard_library_v4_snapshot()
                .expect("the retained V4 source is valid"),
        )
        .expect("the retained V4 source verifies");
        let active = empty_version_two_active_revision(&v4);
        let error = super::prepare_standard_upgrade_v5_to_v6(&active)
            .expect_err("a V4 parent must not enter the V5-to-V6 path");
        assert!(matches!(
            error,
            super::StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision }
            } if revision == super::STANDARD_LIBRARY_V4_REVISION_ID
        ));
    }

    #[test]
    fn retains_and_verifies_v6_action_standard_snapshot() {
        let snapshot = super::retained_standard_library_v6_snapshot()
            .expect("the retained V6 action source is valid");
        assert_eq!(snapshot.source().units().len(), 6);
        assert_eq!(snapshot.source().parent(), Some(super::STANDARD_SOURCE_V5_REVISION_ID));
        assert_eq!(snapshot.source().units()[5].id(), super::STD_ACTION_SOURCE_UNIT_ID);
        assert_eq!(snapshot.source().units()[5].logical_path(), super::STD_ACTION_SOURCE_LOGICAL_PATH);
        assert_eq!(snapshot.executables().len(), 2);
        assert_eq!(
            snapshot
                .origins()
                .iter()
                .filter(|origin| origin.source().source_unit() == super::STD_ACTION_SOURCE_UNIT_ID)
                .count(),
            3
        );
        let verified = super::verify_standard_library_v6_snapshot(snapshot)
            .expect("the retained V6 action source verifies");
        assert_eq!(verified.revision(), super::STANDARD_LIBRARY_V6_REVISION_ID);
        assert!(super::registered_opaque_codecs(&verified).is_ok());
    }

    #[test]
    fn v6_action_codec_accepts_bounded_length_prefixed_bytes() {
        let verified = super::verify_standard_library_v6_snapshot(
            super::retained_standard_library_v6_snapshot()
                .expect("the retained V6 action source is valid"),
        )
        .expect("the retained V6 action source verifies");
        let registry = super::registered_opaque_codecs(&verified)
            .expect("the V6 opaque codecs register");
        let active = empty_version_two_active_revision(&verified);
        let mut payload = Vec::from(super::ACTION_MAGIC.as_bytes());
        payload.extend_from_slice(&3_u32.to_be_bytes());
        payload.extend_from_slice(&[0xa5, 0x00, 0xff]);
        let value = OpaqueValue::new(&active, &registry, super::STD_ACTION_TYPE_ID, &payload)
            .expect("the framed action payload is accepted");
        assert_eq!(value.canonical_payload(), payload.as_slice());
    }

    #[test]
    fn v6_manifest_appends_action_without_changing_v5_catalogue_content() {
        let v5 = super::standard_library_v5_manifest().expect("the V5 manifest is valid");
        let v6 = super::standard_library_v6_manifest().expect("the V6 manifest is valid");
        assert_eq!(v6.catalogue().schemas().len(), v5.catalogue().schemas().len() + 1);
        assert_eq!(
            v6.catalogue().value_types().len(),
            v5.catalogue().value_types().len() + 1
        );
        assert_eq!(
            v6.catalogue().type_bindings().len(),
            v5.catalogue().type_bindings().len() + 1
        );
        assert_eq!(v6.catalogue().functions(), v5.catalogue().functions());
        assert_eq!(v6.action_source_unit(), super::STD_ACTION_SOURCE_UNIT_ID);
        assert_eq!(v6.action_source_logical_path(), super::STD_ACTION_SOURCE_LOGICAL_PATH);
    }



    #[test]
    fn inspect_carrier_registry_is_fixed_and_deterministic() {
        let expected = [
            (
                SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
                SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT,
            ),
            (SYS_INSPECT_CALLS_TYPE_ID, SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT),
            (SYS_INSPECT_RESOURCES_TYPE_ID, SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT),
            (SYS_INSPECT_STATE_CELLS_TYPE_ID, SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT),
            (SYS_INSPECT_UI_NODES_TYPE_ID, SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT),
            (
                SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
                SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT,
            ),
            (
                SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
                SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT,
            ),
            (
                SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
                SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT,
            ),
        ];
        let registrations = registered_inspect_carrier_codecs();
        assert_eq!(registrations.len(), expected.len());
        for (registration, (opaque_type, contract)) in registrations.iter().zip(expected) {
            assert_eq!(registration.opaque_type(), opaque_type);
            assert_eq!(registration.representation_contract(), contract);
            assert!(is_registered_inspect_carrier_type(opaque_type));
        }
        assert!(!is_registered_inspect_carrier_type(TypeId::from_bytes([0xaa; 16])));
    }

}
