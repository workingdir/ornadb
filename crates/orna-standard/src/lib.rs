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
        CanonicalHashError, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest, standard_library_digest,
        verify_standard_library_snapshot as verify_canonical_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, PreludeTypeName, PreludeTypeNameError,
        QualifiedSemanticName, SchemaDefinition, SemanticNameError, TypeBinding, TypeBindingError,
        TypeLookupName, ValueTypeDefinition, ValueTypeKind, ValueTypeMutability,
        ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DeployableRevision,
        RevisionInvariantError, Sha256Digest, SourceOrigin, StandardLibraryDigestVersion,
        StandardLibrarySnapshot, StoredSourceRevision, StoredSourceUnit,
        VerifiedStandardLibrarySnapshot,
    },
};
use orna_syntax::{NamePart, PrimitiveValueTypePersistence, QualifiedName, TypeExportTarget};

pub use orna_compiler::StandardUpgradeIdentity;

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
}

impl fmt::Display for StandardUpgradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StandardLibrary { source } => source.fmt(formatter),
            Self::StandardSource { source } => source.fmt(formatter),
            Self::Prepare { source } => source.fmt(formatter),
        }
    }
}

impl Error for StandardUpgradeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::StandardLibrary { source } => Some(source),
            Self::StandardSource { source } => Some(source),
            Self::Prepare { source } => Some(source),
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
            let source = SourceOrigin::new(STANDARD_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?;
            Ok(DefinitionOrigin::new(identity, source))
        })
        .collect()
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
    use orna_core::{
        CatalogueRevisionId, SourceBundleId, SourceRevisionId, SourceUnitId,
        canonical_hash::{
            catalogue_digest, catalogue_digest_with_context, source_bundle_digest,
            source_revision_record_digest, source_unit_content_digest,
        },
        catalogue::CatalogueSnapshot,
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, RevisionPair, StoredSourceRevision, StoredSourceUnit,
        },
    };

    use super::{
        BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID,
        CHARACTER_LARGE_OBJECT_TYPE_ID, DATE_TYPE_ID, DECIMAL_TYPE_ID, DURATION_TYPE_ID,
        EXPECTED_TYPE_BINDING_IDS, FLOAT_TYPE_ID, INTEGER_TYPE_ID, LANGUAGE_VERSION_IDENTITY,
        OPAQUE_TOKEN_TYPE_ID, SOURCE_LOGICAL_PATH, STANDARD_CATALOGUE_REVISION_ID,
        STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_VERSION_IDENTITY, STANDARD_SOURCE_BUNDLE_ID,
        STANDARD_SOURCE_REVISION_ID, STANDARD_SOURCE_UNIT_ID, STANDARD_TYPE_IDS, STD_SCHEMA_ID,
        STD_TYPES_SCHEMA_ID, StandardLibraryError, StandardLibraryManifestError,
        StandardUpgradeError, TIME_TYPE_ID, TIMESTAMP_TYPE_ID, UUID_TYPE_ID, VOID_TYPE_ID,
        build_type_bindings, prepare_standard_upgrade, prepare_standard_upgrade_with,
        retained_standard_library_snapshot, retained_standard_library_snapshot_from_source,
        standard_library_manifest, verify_standard_library_snapshot,
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

        assert!(
            orna_core::canonical_hash::verify_standard_library_snapshot(non_golden.clone()).is_ok()
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
}
