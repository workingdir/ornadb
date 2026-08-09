//! Source-independent facts for the Orna standard library.

use std::{error::Error, fmt};

use orna_core::{
    CatalogueRevisionId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    StandardLibraryRevisionId, TypeBindingId, TypeId,
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, PreludeTypeName, PreludeTypeNameError,
        QualifiedSemanticName, SchemaDefinition, SemanticNameError, TypeBinding, TypeBindingError,
        TypeLookupName, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    },
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

/// All initial standard type identities in manifest order.
pub const STANDARD_TYPE_IDS: [TypeId; 13] = [
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
];

const fn reserved_id(final_byte: u8) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[15] = final_byte;
    bytes
}

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

// This order is part of the accepted manifest: each value type's qualified
// binding comes first, followed by that type's prelude bindings.
const EXPECTED_TYPE_BINDING_IDS: [[u8; 16]; 30] = [
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
    let mut value_types = Vec::with_capacity(VALUE_TYPE_FACTS.len());
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

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use orna_core::catalogue::{
        CatalogueSnapshotError, PreludeTypeName, PreludeTypeNameError, QualifiedSemanticName,
        SemanticNameError, TypeBindingError, TypeBindingKind, TypeLookupName, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    };

    use super::{
        BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID,
        CHARACTER_LARGE_OBJECT_TYPE_ID, DATE_TYPE_ID, DECIMAL_TYPE_ID, DURATION_TYPE_ID,
        EXPECTED_TYPE_BINDING_IDS, FLOAT_TYPE_ID, INTEGER_TYPE_ID, LANGUAGE_VERSION_IDENTITY,
        SOURCE_LOGICAL_PATH, STANDARD_CATALOGUE_REVISION_ID, STANDARD_LIBRARY_REVISION_ID,
        STANDARD_LIBRARY_VERSION_IDENTITY, STANDARD_SOURCE_BUNDLE_ID, STANDARD_SOURCE_REVISION_ID,
        STANDARD_SOURCE_UNIT_ID, STANDARD_TYPE_IDS, STD_SCHEMA_ID, STD_TYPES_SCHEMA_ID,
        StandardLibraryManifestError, TIME_TYPE_ID, TIMESTAMP_TYPE_ID, UUID_TYPE_ID, VOID_TYPE_ID,
        build_type_bindings, standard_library_manifest,
    };

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
    fn manifest_contains_the_exact_standard_value_type_facts() {
        let manifest = standard_library_manifest().expect("the accepted manifest must be valid");
        let catalogue = manifest.catalogue();
        let expected = [
            (
                BOOLEAN_TYPE_ID,
                "std.types.boolean",
                "orna.kernel.value.boolean@1",
                ValueTypePersistence::Persistable,
            ),
            (
                INTEGER_TYPE_ID,
                "std.types.integer",
                "orna.kernel.value.integer@1",
                ValueTypePersistence::Persistable,
            ),
            (
                BIGINT_TYPE_ID,
                "std.types.bigint",
                "orna.kernel.value.bigint@1",
                ValueTypePersistence::Persistable,
            ),
            (
                FLOAT_TYPE_ID,
                "std.types.float",
                "orna.kernel.value.float@1",
                ValueTypePersistence::Persistable,
            ),
            (
                DECIMAL_TYPE_ID,
                "std.types.decimal",
                "orna.kernel.value.decimal@1",
                ValueTypePersistence::Persistable,
            ),
            (
                CHARACTER_LARGE_OBJECT_TYPE_ID,
                "std.types.character_large_object",
                "orna.kernel.value.character-large-object@1",
                ValueTypePersistence::Persistable,
            ),
            (
                BINARY_LARGE_OBJECT_TYPE_ID,
                "std.types.binary_large_object",
                "orna.kernel.value.binary-large-object@1",
                ValueTypePersistence::Persistable,
            ),
            (
                UUID_TYPE_ID,
                "std.types.uuid",
                "orna.kernel.value.uuid@1",
                ValueTypePersistence::Persistable,
            ),
            (
                DATE_TYPE_ID,
                "std.types.date",
                "orna.kernel.value.date@1",
                ValueTypePersistence::Persistable,
            ),
            (
                TIME_TYPE_ID,
                "std.types.time",
                "orna.kernel.value.time@1",
                ValueTypePersistence::Persistable,
            ),
            (
                TIMESTAMP_TYPE_ID,
                "std.types.timestamp",
                "orna.kernel.value.timestamp@1",
                ValueTypePersistence::Persistable,
            ),
            (
                DURATION_TYPE_ID,
                "std.types.duration",
                "orna.kernel.value.duration@1",
                ValueTypePersistence::Persistable,
            ),
            (
                VOID_TYPE_ID,
                "std.types.void",
                "orna.kernel.value.void@1",
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
            ]
        );
        assert_eq!(catalogue.schemas()[0].name().to_string(), "std");
        assert_eq!(catalogue.schemas()[1].name().to_string(), "std.types");
        assert!(catalogue.object_types().is_empty());
        assert!(catalogue.functions().is_empty());
        assert_eq!(catalogue.value_types().len(), expected.len());
        for (definition, (id, name, contract, persistence)) in
            catalogue.value_types().iter().zip(expected)
        {
            assert_eq!(definition.id(), id);
            assert_eq!(definition.name().to_string(), name);
            assert_eq!(definition.kind(), ValueTypeKind::Primitive);
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
            13
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
        let shorter = build_type_bindings(&EXPECTED_TYPE_BINDING_IDS[..29]).unwrap_err();
        assert_eq!(
            shorter,
            StandardLibraryManifestError::TypeBindingCountMismatch {
                expected: 29,
                actual: 30,
            }
        );
        assert_eq!(
            shorter.to_string(),
            "the standard library manifest has 30 type bindings, expected 29"
        );
        assert!(shorter.source().is_none());

        let mut longer = EXPECTED_TYPE_BINDING_IDS.to_vec();
        longer.push([0; 16]);
        let longer = build_type_bindings(&longer).unwrap_err();
        assert_eq!(
            longer,
            StandardLibraryManifestError::TypeBindingCountMismatch {
                expected: 31,
                actual: 30,
            }
        );
        assert_eq!(
            longer.to_string(),
            "the standard library manifest has 30 type bindings, expected 31"
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
