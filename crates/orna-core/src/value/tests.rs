use super::*;

use crate::{
    CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        calculate_standard_library_digest_for_test, catalogue_digest_with_context,
        source_bundle_digest, source_revision_record_digest, source_unit_content_digest,
        verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, ObjectTypeDefinition, QualifiedSemanticName,
        RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
        ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, RevisionPair, Sha256Digest,
        SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
};

mod constructed;
mod opaque_codecs;
mod records_and_rows;

use constructed::{
    MAP_STD_ENUM, active_nested_record_revision_with_child_fields,
    active_nested_record_revision_with_seed,
};

const TARGET: TypeId = TypeId::from_bytes([0x41; 16]);
const OBJECT: ObjectId = ObjectId::from_bytes([0x42; 16]);
const ENUM_TYPE: TypeId = TypeId::from_bytes([0x43; 16]);
const RECORD_TYPE: TypeId = TypeId::from_bytes([0x47; 16]);
const STANDARD_BOOLEAN: TypeId = TypeId::from_bytes([0x48; 16]);
const OPAQUE_TYPE: TypeId = TypeId::from_bytes([0x49; 16]);
const OTHER_OPAQUE_TYPE: TypeId = TypeId::from_bytes([0x4a; 16]);
const ENABLED_FIELD: FieldId = FieldId::from_bytes([0x59; 16]);
const STAGE_FIELD: FieldId = FieldId::from_bytes([0x5a; 16]);
const OPAQUE_NAME: [&str; 3] = ["std", "types", "opaque_token"];
const OPAQUE_CONTRACT: &str = "orna.std.value.opaque-token@1";

fn active_record_revision() -> ActiveDatabaseRevision {
    active_record_revision_with_type(RECORD_TYPE)
}

fn active_record_revision_with_type(record_type: TypeId) -> ActiveDatabaseRevision {
    active_record_revision_with_opaque_contract(record_type, OPAQUE_CONTRACT)
}

fn active_record_revision_with_opaque_contract(
    record_type: TypeId,
    opaque_contract: &str,
) -> ActiveDatabaseRevision {
    let standard = verified_standard_with_value_types(vec![
        standard_boolean_definition(),
        opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, opaque_contract),
    ]);

    active_record_revision_with_standard(record_type, standard)
}

fn verified_standard_with_value_types(
    value_types: Vec<ValueTypeDefinition>,
) -> VerifiedStandardLibrarySnapshot {
    verified_standard_with_value_types_and_schemas(value_types, Vec::new())
}

fn verified_standard_with_value_types_and_schemas(
    value_types: Vec<ValueTypeDefinition>,
    extra_schemas: Vec<SchemaDefinition>,
) -> VerifiedStandardLibrarySnapshot {
    let standard_unit_content = "x".repeat(value_types.len() + extra_schemas.len() + 2);
    let standard_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x50; 16]),
        0,
        "std/types.orna",
        &standard_unit_content,
        source_unit_content_digest(&standard_unit_content).unwrap(),
    )
    .unwrap();
    let standard_bundle_hash = source_bundle_digest(std::slice::from_ref(&standard_unit)).unwrap();
    let standard_source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x51; 16]),
        SourceRevisionId::from_bytes([0x52; 16]),
        None,
        vec![standard_unit],
        standard_bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x51; 16]),
            None,
            standard_bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let standard_schema = SchemaId::from_bytes([0x53; 16]);
    let standard_types_schema = SchemaId::from_bytes([0x54; 16]);
    let mut schemas = vec![
        SchemaDefinition::new(
            standard_schema,
            QualifiedSemanticName::new(["std"]).unwrap(),
        ),
        SchemaDefinition::new(
            standard_types_schema,
            QualifiedSemanticName::new(["std", "types"]).unwrap(),
        ),
    ];
    schemas.extend(extra_schemas);
    let standard_catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x5b; 16]),
        schemas,
        vec![],
        value_types,
        vec![],
    )
    .unwrap();
    let mut standard_origins = standard_catalogue
        .schemas()
        .iter()
        .enumerate()
        .map(|(index, schema)| {
            let start = u32::try_from(index).unwrap();
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(schema.id()),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    standard_origins.extend(standard_catalogue.value_types().iter().enumerate().map(
        |(index, value_type)| {
            let start = u32::try_from(index + standard_catalogue.schemas().len()).unwrap();
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(value_type.id()),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1).unwrap(),
            )
        },
    ));
    let provisional_standard = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([0x55; 16]),
        StandardLibraryDigestVersion::Version1,
        standard_source.clone(),
        "orna.language/1",
        standard_catalogue.clone(),
        standard_origins.clone(),
        Sha256Digest::from_bytes([0x56; 32]),
    )
    .unwrap();
    let standard_digest =
        calculate_standard_library_digest_for_test(&provisional_standard).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            provisional_standard.revision(),
            provisional_standard.digest_version(),
            standard_source,
            provisional_standard.language_version(),
            standard_catalogue,
            standard_origins,
            standard_digest,
        )
        .unwrap(),
    )
    .unwrap()
}

fn active_record_revision_with_standard(
    record_type: TypeId,
    standard: VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let application_schema = SchemaId::from_bytes([0x57; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            application_schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            ENUM_TYPE,
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            ["lead", "qualified"],
        )],
        vec![RecordValueTypeDefinition::new(
            record_type,
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    ENABLED_FIELD,
                    "enabled",
                    0,
                    TypeDescriptor::named(STANDARD_BOOLEAN),
                )
                .unwrap(),
                RecordValueFieldDefinition::try_new_descriptor(
                    STAGE_FIELD,
                    "stage",
                    1,
                    TypeDescriptor::named(ENUM_TYPE),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let application_content = "abcde";
    let application_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x63; 16]),
        0,
        "app/types.orna",
        application_content,
        source_unit_content_digest(application_content).unwrap(),
    )
    .unwrap();
    let application_bundle_hash =
        source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
    let application_source_revision = SourceRevisionId::from_bytes([0x64; 16]);
    let application_source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x65; 16]),
        application_source_revision,
        None,
        vec![application_unit],
        application_bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x65; 16]),
            None,
            application_bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let source_unit = SourceUnitId::from_bytes([0x63; 16]);
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(application_schema),
            SourceOrigin::new(source_unit, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(ENUM_TYPE),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(record_type),
            SourceOrigin::new(source_unit, 2, 3).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: record_type,
                field: ENABLED_FIELD,
            },
            SourceOrigin::new(source_unit, 3, 4).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: record_type,
                field: STAGE_FIELD,
            },
            SourceOrigin::new(source_unit, 4, 5).unwrap(),
        ),
    ];
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(application_source_revision, catalogue_revision),
            application_source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
        ),
        context,
    )
    .unwrap()
}

fn standard_boolean_definition() -> ValueTypeDefinition {
    ValueTypeDefinition::primitive(
        STANDARD_BOOLEAN,
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    )
}

fn opaque_definition(
    opaque_type: TypeId,
    name: impl IntoIterator<Item = &'static str>,
    contract: &str,
) -> ValueTypeDefinition {
    ValueTypeDefinition::opaque(
        opaque_type,
        QualifiedSemanticName::new(name).unwrap(),
        contract,
    )
}

fn opaque_registration(
    opaque_type: TypeId,
    name: impl IntoIterator<Item = &'static str>,
    contract: &str,
) -> OpaqueCodecRegistration {
    OpaqueCodecRegistration::fixed_length_identity(
        opaque_type,
        QualifiedSemanticName::new(name).unwrap(),
        contract,
        16,
    )
    .unwrap()
}

fn enum_catalogue(labels: &[&str]) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x45; 16]),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            ENUM_TYPE,
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            labels.iter().copied(),
        )],
        vec![],
    )
    .unwrap()
}

fn standard_enum_catalogue(labels: &[&str]) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x96; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x97; 16]),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            MAP_STD_ENUM,
            QualifiedSemanticName::new(["std", "mode"]).unwrap(),
            labels.iter().copied(),
        )],
        vec![],
    )
    .unwrap()
}

fn column(name: &str, resolved_type: ResolvedType, nullable: bool) -> ResultColumn {
    ResultColumn::new(name, resolved_type, nullable).unwrap()
}
