use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, InvocationId, ObjectId, ParameterId, PrincipalId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{
        calculate_standard_library_digest, catalogue_digest_with_context, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
        verify_standard_library_snapshot as verify_core_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        RecordValueTypeDefinition, SchemaDefinition, ValueTypeDefinition, ValueTypeMutability,
        ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, RevisionPair, Sha256Digest,
        SourceOrigin, StandardLibrarySnapshot, StoredSourceRevision, StoredSourceUnit,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor},
    value::{EnumValue, OpaqueValue, RecordValue, ResultRowsError, RuntimeFloat, RuntimeValue},
};
use orna_standard::{
    BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID, CHARACTER_LARGE_OBJECT_TYPE_ID,
    DATE_TYPE_ID, DECIMAL_TYPE_ID, DURATION_TYPE_ID, FLOAT_TYPE_ID, INTEGER_TYPE_ID,
    OPAQUE_TOKEN_TYPE_ID, STANDARD_TYPE_IDS, TIME_TYPE_ID, TIMESTAMP_TYPE_ID, UUID_TYPE_ID,
    VOID_TYPE_ID, registered_opaque_codecs, retained_standard_library_snapshot,
    verify_standard_library_snapshot,
};
use proptest::prelude::*;

use super::*;
mod carriers;
mod constructed;

const ENUM_TYPE: TypeId = TypeId::from_bytes([0x43; 16]);

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

fn active_record_revision() -> ActiveDatabaseRevision {
    active_record_revision_with_second_type(TypeDescriptor::named(ENUM_TYPE))
}

fn active_revision_without_standard() -> ActiveDatabaseRevision {
    let schema = SchemaId::from_bytes([0x75; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x76; 16]);
    let catalogue = CatalogueSnapshot::new(
        catalogue_revision,
        vec![SchemaDefinition::new(
            schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        Vec::new(),
    )
    .unwrap();
    let source_unit_id = SourceUnitId::from_bytes([0x77; 16]);
    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "app/schema.orna",
        "a",
        source_unit_content_digest("a").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source_revision = SourceRevisionId::from_bytes([0x78; 16]);
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x79; 16]),
        source_revision,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x79; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let origins = vec![DefinitionOrigin::new(
        DefinitionIdentity::Schema(schema),
        SourceOrigin::new(source_unit_id, 0, 1).unwrap(),
    )];
    let context = CatalogueHashContext::version_one();
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source_revision, catalogue_revision),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn alternate_verified_standard() -> orna_core::revision::VerifiedStandardLibrarySnapshot {
    let accepted = retained_standard_library_snapshot().unwrap();
    let alternate = orna_core::revision::StandardLibrarySnapshot::new(
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
    .unwrap();
    verify_core_standard_library_snapshot(alternate).unwrap()
}

const STANDARD_ENUM_TYPE: TypeId = TypeId::from_bytes([0x90; 16]);

fn verified_standard_with_enum() -> orna_core::revision::VerifiedStandardLibrarySnapshot {
    let accepted = retained_standard_library_snapshot().unwrap();
    let accepted_catalogue = accepted.catalogue();
    let catalogue = CatalogueSnapshot::new_with_enum_types(
        accepted_catalogue.revision(),
        accepted_catalogue.schemas().to_vec(),
        accepted_catalogue.object_types().to_vec(),
        accepted_catalogue.value_types().to_vec(),
        vec![EnumTypeDefinition::new(
            STANDARD_ENUM_TYPE,
            QualifiedSemanticName::new(["std", "mode"]).unwrap(),
            ["safe", "unsafe"],
        )],
        accepted_catalogue.type_bindings().to_vec(),
    )
    .unwrap();
    let mut origins = accepted.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ValueType(STANDARD_ENUM_TYPE),
        SourceOrigin::new(accepted.source().units()[0].id(), 0, 1).unwrap(),
    ));
    let provisional = StandardLibrarySnapshot::new(
        accepted.revision(),
        accepted.digest_version(),
        accepted.source().clone(),
        accepted.language_version(),
        catalogue.clone(),
        origins.clone(),
        Sha256Digest::from_bytes([0x98; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&provisional).unwrap();
    verify_core_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            provisional.source().clone(),
            provisional.language_version(),
            catalogue,
            origins,
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

fn active_revision_with_standard_named_collision() -> ActiveDatabaseRevision {
    let standard =
        verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap()).unwrap();
    let schema = SchemaId::from_bytes([0x7a; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x7b; 16]);
    let catalogue = CatalogueSnapshot::new_with_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        Vec::new(),
        vec![ValueTypeDefinition::primitive(
            OPAQUE_TOKEN_TYPE_ID,
            QualifiedSemanticName::new(["crm", "collision"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.crm.value.collision@1",
        )],
        Vec::new(),
    )
    .unwrap();
    let source_unit_id = SourceUnitId::from_bytes([0x7c; 16]);
    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "app/collision.orna",
        "ab",
        source_unit_content_digest("ab").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source_revision = SourceRevisionId::from_bytes([0x7d; 16]);
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x7e; 16]),
        source_revision,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x7e; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema),
            SourceOrigin::new(source_unit_id, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(OPAQUE_TOKEN_TYPE_ID),
            SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
        ),
    ];
    let context = CatalogueHashContext::version_two(standard);
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source_revision, catalogue_revision),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn active_record_revision_with_second_type(
    second_field_type: TypeDescriptor,
) -> ActiveDatabaseRevision {
    active_record_revision_with_types(TypeDescriptor::named(BOOLEAN_TYPE_ID), second_field_type)
}

fn active_record_revision_with_types(
    first_field_type: TypeDescriptor,
    second_field_type: TypeDescriptor,
) -> ActiveDatabaseRevision {
    let standard =
        verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap()).unwrap();
    active_record_revision_with_types_and_standard(first_field_type, second_field_type, standard)
}

fn active_record_revision_with_types_and_standard(
    first_field_type: TypeDescriptor,
    second_field_type: TypeDescriptor,
    standard: orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let record_type = TypeId::from_bytes([0x47; 16]);
    let record_field = FieldId::from_bytes([0x48; 16]);
    let second_record_field = FieldId::from_bytes([0x4e; 16]);
    let schema = SchemaId::from_bytes([0x49; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x4a; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            schema,
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
            QualifiedSemanticName::new(["crm", "flag"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    record_field,
                    "enabled",
                    0,
                    first_field_type,
                )
                .unwrap(),
                RecordValueFieldDefinition::try_new_descriptor(
                    second_record_field,
                    "verified",
                    1,
                    second_field_type,
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap();
    let source_unit_id = SourceUnitId::from_bytes([0x4b; 16]);
    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "app/types.orna",
        "ab",
        source_unit_content_digest("ab").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source_revision = SourceRevisionId::from_bytes([0x4c; 16]);
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x4d; 16]),
        source_revision,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x4d; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema),
            SourceOrigin::new(source_unit_id, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(record_type),
            SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(ENUM_TYPE),
            SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: record_type,
                field: record_field,
            },
            SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: record_type,
                field: second_record_field,
            },
            SourceOrigin::new(source_unit_id, 1, 2).unwrap(),
        ),
    ];
    let context = CatalogueHashContext::version_two(standard);
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source_revision, catalogue_revision),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
        ),
        context,
    )
    .unwrap()
}

fn active_nested_record_revision() -> ActiveDatabaseRevision {
    active_nested_record_revision_with_fields(vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "value",
            0,
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap(),
    ])
}

fn active_nested_record_revision_with_fields(
    inner_fields: Vec<RecordValueFieldDefinition>,
) -> ActiveDatabaseRevision {
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let outer_field = FieldId::from_bytes([0x3b; 16]);
    let inner_field_ids = inner_fields
        .iter()
        .map(|field| field.id())
        .collect::<Vec<_>>();
    let schema = SchemaId::from_bytes([0x49; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x4a; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![],
        vec![
            RecordValueTypeDefinition::new(
                inner_type,
                QualifiedSemanticName::new(["crm", "inner"]).unwrap(),
                inner_fields,
            ),
            RecordValueTypeDefinition::new(
                outer_type,
                QualifiedSemanticName::new(["crm", "outer"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        outer_field,
                        "payload",
                        0,
                        TypeDescriptor::named(inner_type),
                    )
                    .unwrap(),
                ],
            ),
        ],
        vec![],
    )
    .unwrap();
    let source_unit_id = SourceUnitId::from_bytes([0x4b; 16]);
    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "app/types.orna",
        "abcdef",
        source_unit_content_digest("abcdef").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source_revision = SourceRevisionId::from_bytes([0x4c; 16]);
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x4d; 16]),
        source_revision,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x4d; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let mut identities = vec![
        DefinitionIdentity::Schema(schema),
        DefinitionIdentity::ValueType(inner_type),
    ];
    identities.extend(
        inner_field_ids
            .iter()
            .map(|&field| DefinitionIdentity::Field {
                owner: inner_type,
                field,
            }),
    );
    identities.push(DefinitionIdentity::ValueType(outer_type));
    identities.push(DefinitionIdentity::Field {
        owner: outer_type,
        field: outer_field,
    });
    let origins = identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source_unit_id, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let standard =
        verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap()).unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source_revision, catalogue_revision),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
        ),
        context,
    )
    .unwrap()
}

#[test]
fn public_frame_payload_limit_matches_the_wire_contract() {
    assert_eq!(MAX_FRAME_PAYLOAD_LENGTH, 16 * 1024 * 1024 + 64);
}

#[test]
fn boolean_has_exact_golden_bytes_and_round_trips() {
    let mut expected = b"ORV1".to_vec();
    expected.push(0x02);
    expected.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.push(1);

    assert_eq!(
        encode_value(&RuntimeValue::Boolean(true)),
        Ok(expected.clone())
    );
    assert_eq!(decode_value(&expected), Ok(RuntimeValue::Boolean(true)));
}

#[test]
fn catalogue_codec_has_exact_enum_bytes_and_preserves_version_one_closure() {
    let catalogue = enum_catalogue(&["lead", "owner's"]);
    let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "owner's").unwrap());
    let mut expected = b"ORV2".to_vec();
    expected.push(0x0a);
    expected.extend_from_slice(&ENUM_TYPE.to_bytes());
    expected.extend_from_slice(&7_u32.to_be_bytes());
    expected.extend_from_slice(b"owner's");

    assert_eq!(
        encode_catalogue_value(&catalogue, &value),
        Ok(expected.clone())
    );
    assert_eq!(
        decode_catalogue_value(&catalogue, &expected),
        Ok(value.clone())
    );
    assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
    assert_eq!(decode_value(&expected), Err(ValueCodecError::InvalidMarker));
}

#[test]
fn version_one_and_two_codecs_reject_record_runtime_values() {
    let active = active_record_revision();
    let record_type = active.catalogue().record_value_types()[0].id();
    let value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record_type,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );

    assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
    assert_eq!(
        encode_catalogue_value(active.catalogue(), &value),
        Err(ValueCodecError::UnsupportedValue)
    );
}

#[test]
fn active_codec_has_exact_record_bytes_and_round_trips() {
    let active = active_record_revision();
    let record = &active.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let mut field_value = b"ORV3".to_vec();
    field_value.push(0x02);
    field_value.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    field_value.extend_from_slice(&1_u32.to_be_bytes());
    field_value.push(1);
    let mut payload = 2_u32.to_be_bytes().to_vec();
    payload.extend_from_slice(&record.fields()[0].id().to_bytes());
    payload.extend_from_slice(&26_u32.to_be_bytes());
    payload.extend_from_slice(&field_value);
    let mut second_field_value = b"ORV3".to_vec();
    second_field_value.push(0x0a);
    second_field_value.extend_from_slice(&ENUM_TYPE.to_bytes());
    second_field_value.extend_from_slice(&4_u32.to_be_bytes());
    second_field_value.extend_from_slice(b"lead");
    payload.extend_from_slice(&record.fields()[1].id().to_bytes());
    payload.extend_from_slice(&29_u32.to_be_bytes());
    payload.extend_from_slice(&second_field_value);
    let mut expected = b"ORV3".to_vec();
    expected.push(0x0b);
    expected.extend_from_slice(&record.id().to_bytes());
    expected.extend_from_slice(&99_u32.to_be_bytes());
    expected.extend_from_slice(&payload);

    assert_eq!(encode_active_value(&active, &value), Ok(expected.clone()));
    assert_eq!(decode_active_value(&active, &expected), Ok(value));
}

#[test]
fn tracer_bullet_active_codec_encodes_nested_immutable_records() {
    let active = active_nested_record_revision();
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let inner_field = FieldId::from_bytes([0x3a; 16]);
    let outer_field = FieldId::from_bytes([0x3b; 16]);
    let inner = RecordValue::new(
        &active,
        inner_type,
        [(String::from("value"), RuntimeValue::Boolean(true))],
    )
    .expect("the inner record must construct");
    let outer = RecordValue::new(
        &active,
        outer_type,
        [(String::from("payload"), RuntimeValue::Record(inner))],
    )
    .expect("the outer record must construct");
    let value = RuntimeValue::Record(outer);

    let mut boolean_envelope = b"ORV3".to_vec();
    boolean_envelope.push(0x02);
    boolean_envelope.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    boolean_envelope.extend_from_slice(&1_u32.to_be_bytes());
    boolean_envelope.push(1);

    let mut inner_payload = 1_u32.to_be_bytes().to_vec();
    inner_payload.extend_from_slice(&inner_field.to_bytes());
    inner_payload.extend_from_slice(&(boolean_envelope.len() as u32).to_be_bytes());
    inner_payload.extend_from_slice(&boolean_envelope);
    let mut inner_envelope = b"ORV3".to_vec();
    inner_envelope.push(0x0b);
    inner_envelope.extend_from_slice(&inner_type.to_bytes());
    inner_envelope.extend_from_slice(&(inner_payload.len() as u32).to_be_bytes());
    inner_envelope.extend_from_slice(&inner_payload);

    let mut outer_payload = 1_u32.to_be_bytes().to_vec();
    outer_payload.extend_from_slice(&outer_field.to_bytes());
    outer_payload.extend_from_slice(&(inner_envelope.len() as u32).to_be_bytes());
    outer_payload.extend_from_slice(&inner_envelope);
    let mut expected = b"ORV3".to_vec();
    expected.push(0x0b);
    expected.extend_from_slice(&outer_type.to_bytes());
    expected.extend_from_slice(&(outer_payload.len() as u32).to_be_bytes());
    expected.extend_from_slice(&outer_payload);

    assert_eq!(
        encode_active_value(&active, &value),
        Ok(expected.clone()),
        "the active codec must encode a nested immutable record"
    );
    assert_eq!(
        decode_active_value(&active, &expected),
        Ok(value),
        "the active codec must round-trip a nested immutable record"
    );
}

#[test]
fn tracer_bullet_active_codec_rejects_stale_inner_field_identity() {
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let field_a = RecordValueFieldDefinition::try_new_descriptor(
        FieldId::from_bytes([0x3a; 16]),
        "a",
        0,
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    let field_b = RecordValueFieldDefinition::try_new_descriptor(
        FieldId::from_bytes([0x3c; 16]),
        "b",
        1,
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    let old = active_nested_record_revision_with_fields(vec![field_a, field_b]);
    let current = active_nested_record_revision_with_fields(vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3c; 16]),
            "b",
            0,
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap(),
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "a",
            1,
            TypeDescriptor::named(BOOLEAN_TYPE_ID),
        )
        .unwrap(),
    ]);
    let old_child = RecordValue::new(
        &old,
        inner_type,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(false)),
        ],
    )
    .expect("the child must construct under the old revision");

    let value = RuntimeValue::Record(old_child);
    assert_eq!(
        encode_active_value(&current, &value),
        Err(ValueCodecError::RecordValueNotActive {
            record_type: inner_type,
        }),
        "the encoder must reject a stale inner field identity"
    );
    let standard = current.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    assert_eq!(
        encode_registered_value(&current, &registry, &value),
        Err(ValueCodecError::RecordValueNotActive {
            record_type: inner_type,
        }),
        "the registered encoder must reject a stale inner field identity"
    );
}

#[test]
fn stale_replaced_field_identity_fails_both_encoders() {
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let field_a = RecordValueFieldDefinition::try_new_descriptor(
        FieldId::from_bytes([0x3a; 16]),
        "a",
        0,
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    let replaced_a = RecordValueFieldDefinition::try_new_descriptor(
        FieldId::from_bytes([0x3d; 16]),
        "a",
        0,
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    let old = active_nested_record_revision_with_fields(vec![field_a]);
    let current = active_nested_record_revision_with_fields(vec![replaced_a]);
    let old_child = RecordValue::new(
        &old,
        inner_type,
        [(String::from("a"), RuntimeValue::Boolean(true))],
    )
    .expect("the child must construct under the old revision");
    let value = RuntimeValue::Record(old_child);
    assert_eq!(
        encode_active_value(&current, &value.clone()),
        Err(ValueCodecError::RecordValueNotActive {
            record_type: inner_type,
        })
    );
    let standard = current.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    assert_eq!(
        encode_registered_value(&current, &registry, &value),
        Err(ValueCodecError::RecordValueNotActive {
            record_type: inner_type,
        })
    );
}

fn nested_record_value(active: &ActiveDatabaseRevision) -> RuntimeValue {
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let inner = RecordValue::new(
        active,
        inner_type,
        [(String::from("value"), RuntimeValue::Boolean(true))],
    )
    .unwrap();
    let outer = RecordValue::new(
        active,
        outer_type,
        [(String::from("payload"), RuntimeValue::Record(inner))],
    )
    .unwrap();
    RuntimeValue::Record(outer)
}

fn nested_envelope(marker: &[u8; 4], tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
    let mut bytes = marker.to_vec();
    bytes.push(tag);
    bytes.extend_from_slice(&type_id.to_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn nested_record_payload(fields: &[(FieldId, Vec<u8>)]) -> Vec<u8> {
    let mut payload = (fields.len() as u32).to_be_bytes().to_vec();
    for (field, encoded) in fields {
        payload.extend_from_slice(&field.to_bytes());
        payload.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        payload.extend_from_slice(encoded);
    }
    payload
}

fn assemble_nested_envelope(
    marker: &[u8; 4],
    inner_tag: u8,
    inner_type: TypeId,
    inner_field: FieldId,
    inner_count: u32,
    inner_field_length: u32,
    inner_extra_payload: &[u8],
) -> Vec<u8> {
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let outer_field = FieldId::from_bytes([0x3b; 16]);
    let boolean = nested_envelope(marker, 0x02, BOOLEAN_TYPE_ID, &[1]);
    let mut inner_payload = inner_count.to_be_bytes().to_vec();
    inner_payload.extend_from_slice(&inner_field.to_bytes());
    inner_payload.extend_from_slice(&inner_field_length.to_be_bytes());
    inner_payload.extend_from_slice(&boolean);
    inner_payload.extend_from_slice(inner_extra_payload);
    let inner = nested_envelope(marker, inner_tag, inner_type, &inner_payload);
    let outer_payload = nested_record_payload(&[(outer_field, inner)]);
    nested_envelope(marker, 0x0b, outer_type, &outer_payload)
}

#[test]
fn registered_codec_has_exact_nested_record_bytes_and_round_trips() {
    let active = active_nested_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let value = nested_record_value(&active);
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let inner_field = FieldId::from_bytes([0x3a; 16]);
    let expected = assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);

    assert_eq!(
        encode_registered_value(&active, &registry, &value),
        Ok(expected.clone())
    );
    assert_eq!(
        decode_registered_value(&active, &registry, &expected),
        Ok(value.clone())
    );
    assert_eq!(&expected[0..4], b"ORV4", "the outer marker must be ORV4");

    assert_eq!(
        encode_value(&value),
        Err(ValueCodecError::UnsupportedValue),
        "version one must stay closed for nested records"
    );
    assert_eq!(
        encode_catalogue_value(active.catalogue(), &value),
        Err(ValueCodecError::UnsupportedValue),
        "version two must stay closed for nested records"
    );
    assert_eq!(decode_value(&expected), Err(ValueCodecError::InvalidMarker));
    assert_eq!(
        decode_catalogue_value(active.catalogue(), &expected),
        Err(ValueCodecError::InvalidMarker)
    );
}

#[test]
fn nested_codec_rejects_inner_marker_crossing() {
    let active = active_nested_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let value = nested_record_value(&active);
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let inner_field = FieldId::from_bytes([0x3a; 16]);
    let inner_offset = 25 + 4 + 16 + 4;

    let active_bytes = assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 26, &[]);
    assert_eq!(
        encode_active_value(&active, &value),
        Ok(active_bytes.clone())
    );
    let mut wrong_active = active_bytes.clone();
    wrong_active[inner_offset..inner_offset + 4].copy_from_slice(b"ORV4");
    assert_eq!(
        decode_active_value(&active, &wrong_active),
        Err(ValueCodecError::InvalidMarker),
        "an ORV4 inner envelope must be rejected by the ORV3 decoder"
    );

    let registered_bytes =
        assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);
    let mut wrong_registered = registered_bytes.clone();
    wrong_registered[inner_offset..inner_offset + 4].copy_from_slice(b"ORV3");
    assert_eq!(
        decode_registered_value(&active, &registry, &wrong_registered),
        Err(ValueCodecError::InvalidMarker),
        "an ORV3 inner envelope must be rejected by the ORV4 decoder"
    );
}

#[test]
fn nested_codec_rejects_wrong_inner_tag_and_type() {
    let active = active_nested_record_revision();
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let inner_field = FieldId::from_bytes([0x3a; 16]);
    for inner_tag in [0x0a, 0x02] {
        let corrupted =
            assemble_nested_envelope(b"ORV3", inner_tag, inner_type, inner_field, 1, 26, &[]);
        assert_eq!(
            decode_active_value(&active, &corrupted),
            Err(ValueCodecError::WrongRecordFieldType {
                ordinal: 0,
                expected: TypeDescriptor::named(inner_type),
                tag: inner_tag,
                actual: inner_type,
            })
        );
    }
    let replaced_type = TypeId::from_bytes([0x99; 16]);
    let corrupted = assemble_nested_envelope(b"ORV3", 0x0b, replaced_type, inner_field, 1, 26, &[]);
    assert_eq!(
        decode_active_value(&active, &corrupted),
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal: 0,
            expected: TypeDescriptor::named(inner_type),
            tag: 0x0b,
            actual: replaced_type,
        })
    );
}

#[test]
fn nested_codec_rejects_inner_structure_corruption() {
    let active = active_nested_record_revision();
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let inner_field = FieldId::from_bytes([0x3a; 16]);

    let wrong_count = assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 2, 26, &[]);
    assert_eq!(
        decode_active_value(&active, &wrong_count),
        Err(ValueCodecError::WrongRecordFieldCount {
            expected: 1,
            actual: 2,
        })
    );

    let wrong_field = FieldId::from_bytes([0x99; 16]);
    let wrong_identity =
        assemble_nested_envelope(b"ORV3", 0x0b, inner_type, wrong_field, 1, 26, &[]);
    assert_eq!(
        decode_active_value(&active, &wrong_identity),
        Err(ValueCodecError::WrongRecordFieldIdentity {
            ordinal: 0,
            expected: inner_field,
            actual: wrong_field,
        })
    );

    let wrong_length = assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 10, &[]);
    assert_eq!(
        decode_active_value(&active, &wrong_length),
        Err(ValueCodecError::InvalidRecordFieldLength {
            ordinal: 0,
            declared: 10,
            remaining: 26,
        })
    );

    let trailing =
        assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 26, &[0xaa, 0xbb]);
    assert_eq!(
        decode_active_value(&active, &trailing),
        Err(ValueCodecError::TrailingBytes {
            declared: 50,
            actual: 52,
        })
    );
}

#[test]
fn nested_codec_checks_inner_payload_limit_before_truncation() {
    let active = active_nested_record_revision();
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let outer_field = FieldId::from_bytes([0x3b; 16]);
    let mut inner_header = b"ORV3".to_vec();
    inner_header.push(0x0b);
    inner_header.extend_from_slice(&inner_type.to_bytes());
    inner_header.extend_from_slice(&((PAYLOAD_LIMIT as u32) + 1).to_be_bytes());
    assert_eq!(
        inner_header.len(),
        25,
        "inner envelope must carry no payload"
    );

    let mut outer_payload = 1_u32.to_be_bytes().to_vec();
    outer_payload.extend_from_slice(&outer_field.to_bytes());
    outer_payload.extend_from_slice(&(inner_header.len() as u32).to_be_bytes());
    outer_payload.extend_from_slice(&inner_header);
    let mut expected = b"ORV3".to_vec();
    expected.push(0x0b);
    expected.extend_from_slice(&outer_type.to_bytes());
    expected.extend_from_slice(&(outer_payload.len() as u32).to_be_bytes());
    expected.extend_from_slice(&outer_payload);

    assert_eq!(
        decode_active_value(&active, &expected),
        Err(ValueCodecError::PayloadTooLarge {
            actual: PAYLOAD_LIMIT + 1,
            maximum: PAYLOAD_LIMIT,
        }),
        "the declared inner payload limit must be checked before truncation"
    );
}

#[test]
fn nested_record_values_delegate_unchanged_through_frames() {
    let active = active_nested_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let value = nested_record_value(&active);
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let inner_field = FieldId::from_bytes([0x3a; 16]);
    let parameter = ParameterId::from_bytes([0x5f; 16]);
    let active_envelope =
        assemble_nested_envelope(b"ORV3", 0x0b, inner_type, inner_field, 1, 26, &[]);
    let registered_envelope =
        assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);
    assert_eq!(active_envelope.len(), 124);
    assert_eq!(registered_envelope.len(), 124);

    let argument = ClientFrame::CallArgument {
        stream: 7,
        parameter,
        value: value.clone(),
    };
    let mut expected_argument = b"ORF3\x02\0".to_vec();
    expected_argument.extend_from_slice(&7_u64.to_be_bytes());
    expected_argument.extend_from_slice(&140_u32.to_be_bytes());
    expected_argument.extend_from_slice(&parameter.to_bytes());
    expected_argument.extend_from_slice(&active_envelope);
    assert_eq!(
        encode_active_client_frame(&active, &argument),
        Ok(expected_argument.clone())
    );
    assert_eq!(
        decode_active_client_frame(&active, &expected_argument),
        Ok(argument)
    );
    assert_eq!(
        decode_client_frame(&expected_argument),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_catalogue_client_frame(active.catalogue(), &expected_argument),
        Err(FrameCodecError::InvalidMarker)
    );

    let event_batch = ServerFrame::EventBatch {
        stream: 7,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(value.clone()),
        }],
    };
    let mut expected_batch = b"ORF3\x82\0".to_vec();
    expected_batch.extend_from_slice(&7_u64.to_be_bytes());
    expected_batch.extend_from_slice(&140_u32.to_be_bytes());
    expected_batch.push(0x01);
    expected_batch.extend_from_slice(&1_u16.to_be_bytes());
    expected_batch.extend_from_slice(&1_u64.to_be_bytes());
    expected_batch.push(0x01);
    expected_batch.extend_from_slice(&124_u32.to_be_bytes());
    expected_batch.extend_from_slice(&active_envelope);
    assert_eq!(
        encode_active_server_frame(&active, &event_batch),
        Ok(expected_batch.clone())
    );
    assert_eq!(
        decode_active_server_frame(&active, &expected_batch),
        Ok(event_batch)
    );

    let registered_batch = ServerFrame::EventBatch {
        stream: 8,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(value),
        }],
    };
    let mut expected_registered = b"ORF4\x82\0".to_vec();
    expected_registered.extend_from_slice(&8_u64.to_be_bytes());
    expected_registered.extend_from_slice(&140_u32.to_be_bytes());
    expected_registered.push(0x01);
    expected_registered.extend_from_slice(&1_u16.to_be_bytes());
    expected_registered.extend_from_slice(&1_u64.to_be_bytes());
    expected_registered.push(0x01);
    expected_registered.extend_from_slice(&124_u32.to_be_bytes());
    expected_registered.extend_from_slice(&registered_envelope);
    assert_eq!(
        encode_registered_server_frame(&active, &registry, &registered_batch),
        Ok(expected_registered.clone())
    );
    assert_eq!(
        decode_registered_server_frame(&active, &registry, &expected_registered),
        Ok(registered_batch)
    );
    assert_eq!(
        decode_active_server_frame(&active, &expected_registered),
        Err(FrameCodecError::InvalidMarker),
        "the ORV4 frame must be rejected by the active frame decoder"
    );
}

#[test]
fn registered_codec_has_exact_opaque_bytes_and_preserves_earlier_closure() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let payload = [0x71; 16];
    let value = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload).unwrap(),
    );
    let mut expected = b"ORV4".to_vec();
    expected.push(0x0c);
    expected.extend_from_slice(&OPAQUE_TOKEN_TYPE_ID.to_bytes());
    expected.extend_from_slice(&16_u32.to_be_bytes());
    expected.extend_from_slice(&payload);

    assert_eq!(
        encode_registered_value(&active, &registry, &value),
        Ok(expected.clone())
    );
    assert_eq!(
        decode_registered_value(&active, &registry, &expected),
        Ok(value.clone())
    );
    assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
    assert_eq!(
        encode_catalogue_value(active.catalogue(), &value),
        Err(ValueCodecError::UnsupportedValue)
    );
    assert_eq!(
        encode_active_value(&active, &value),
        Err(ValueCodecError::UnsupportedValue)
    );
    assert_eq!(decode_value(&expected), Err(ValueCodecError::InvalidMarker));
    assert_eq!(
        decode_catalogue_value(active.catalogue(), &expected),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_active_value(&active, &expected),
        Err(ValueCodecError::InvalidMarker)
    );

    let mut wrong_type = expected.clone();
    wrong_type[5..21].fill(0x72);
    assert_eq!(
        decode_registered_value(&active, &registry, &wrong_type),
        Err(ValueCodecError::OpaqueValue {
            source: OpaqueValueError::UnregisteredType {
                opaque_type: TypeId::from_bytes([0x72; 16]),
            },
        })
    );
    let mut wrong_length = expected;
    wrong_length[21..25].copy_from_slice(&15_u32.to_be_bytes());
    wrong_length.pop();
    assert_eq!(
        decode_registered_value(&active, &registry, &wrong_length),
        Err(ValueCodecError::OpaqueValue {
            source: OpaqueValueError::WrongPayloadLength {
                opaque_type: OPAQUE_TOKEN_TYPE_ID,
                expected: 16,
                actual: 15,
            },
        })
    );
}

#[test]
fn registered_codec_retains_version_three_shapes_under_its_marker() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let record = &active.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let active_bytes = encode_active_value(&active, &value).unwrap();
    let registered_bytes = encode_registered_value(&active, &registry, &value).unwrap();
    assert_eq!(&registered_bytes[..4], b"ORV4");
    assert_eq!(&registered_bytes[4..49], &active_bytes[4..49]);
    assert_eq!(&registered_bytes[49..53], b"ORV4");
    assert_eq!(&registered_bytes[53..95], &active_bytes[53..95]);
    assert_eq!(&registered_bytes[95..99], b"ORV4");
    assert_eq!(&registered_bytes[99..], &active_bytes[99..]);
    assert_eq!(
        decode_registered_value(&active, &registry, &registered_bytes),
        Ok(value)
    );
    assert_eq!(
        decode_active_value(&active, &registered_bytes),
        Err(ValueCodecError::InvalidMarker)
    );
}

#[test]
fn active_client_frame_has_exact_record_bytes_and_round_trips() {
    let active = active_record_revision();
    let record = &active.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let parameter = ParameterId::from_bytes([0x5f; 16]);
    let frame = ClientFrame::CallArgument {
        stream: 7,
        parameter,
        value: value.clone(),
    };
    let mut record_value = b"ORV3".to_vec();
    record_value.push(0x0b);
    record_value.extend_from_slice(&record.id().to_bytes());
    record_value.extend_from_slice(&99_u32.to_be_bytes());
    record_value.extend_from_slice(&2_u32.to_be_bytes());
    record_value.extend_from_slice(&record.fields()[0].id().to_bytes());
    record_value.extend_from_slice(&26_u32.to_be_bytes());
    record_value.extend_from_slice(b"ORV3");
    record_value.push(0x02);
    record_value.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    record_value.extend_from_slice(&1_u32.to_be_bytes());
    record_value.push(1);
    record_value.extend_from_slice(&record.fields()[1].id().to_bytes());
    record_value.extend_from_slice(&29_u32.to_be_bytes());
    record_value.extend_from_slice(b"ORV3");
    record_value.push(0x0a);
    record_value.extend_from_slice(&ENUM_TYPE.to_bytes());
    record_value.extend_from_slice(&4_u32.to_be_bytes());
    record_value.extend_from_slice(b"lead");
    let mut expected = b"ORF3\x02\0".to_vec();
    expected.extend_from_slice(&7_u64.to_be_bytes());
    expected.extend_from_slice(&140_u32.to_be_bytes());
    expected.extend_from_slice(&parameter.to_bytes());
    expected.extend_from_slice(&record_value);

    assert_eq!(
        encode_active_client_frame(&active, &frame),
        Ok(expected.clone())
    );
    assert_eq!(decode_active_client_frame(&active, &expected), Ok(frame));
    assert_eq!(
        decode_client_frame(&expected),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_catalogue_client_frame(active.catalogue(), &expected),
        Err(FrameCodecError::InvalidMarker)
    );
    let mut wrong_value_marker = expected.clone();
    wrong_value_marker[34..38].copy_from_slice(b"ORV2");
    assert_eq!(
        decode_active_client_frame(&active, &wrong_value_marker),
        Err(FrameCodecError::Value {
            source: ValueCodecError::InvalidMarker,
        })
    );

    let server = ServerFrame::EventBatch {
        stream: 7,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(value),
        }],
    };
    let mut expected_server = b"ORF3\x82\0".to_vec();
    expected_server.extend_from_slice(&7_u64.to_be_bytes());
    expected_server.extend_from_slice(&140_u32.to_be_bytes());
    expected_server.push(0x01);
    expected_server.extend_from_slice(&1_u16.to_be_bytes());
    expected_server.extend_from_slice(&1_u64.to_be_bytes());
    expected_server.push(0x01);
    expected_server.extend_from_slice(&124_u32.to_be_bytes());
    expected_server.extend_from_slice(&record_value);
    assert_eq!(
        encode_active_server_frame(&active, &server),
        Ok(expected_server.clone())
    );
    assert_eq!(
        decode_active_server_frame(&active, &expected_server),
        Ok(server)
    );

    for marker in [b"ORF1", b"ORF2"] {
        let mut wrong_version = expected.clone();
        wrong_version[..4].copy_from_slice(marker);
        assert_eq!(
            decode_active_client_frame(&active, &wrong_version),
            Err(FrameCodecError::InvalidMarker)
        );
    }
}

#[test]
fn registered_frame_carries_opaque_results_but_rejects_opaque_arguments() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let payload = [0x73; 16];
    let value = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload).unwrap(),
    );
    let mut encoded_value = b"ORV4".to_vec();
    encoded_value.push(0x0c);
    encoded_value.extend_from_slice(&OPAQUE_TOKEN_TYPE_ID.to_bytes());
    encoded_value.extend_from_slice(&16_u32.to_be_bytes());
    encoded_value.extend_from_slice(&payload);

    let server = ServerFrame::EventBatch {
        stream: 8,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(value.clone()),
        }],
    };
    let mut expected_server = b"ORF4\x82\0".to_vec();
    expected_server.extend_from_slice(&8_u64.to_be_bytes());
    expected_server.extend_from_slice(&57_u32.to_be_bytes());
    expected_server.push(0x01);
    expected_server.extend_from_slice(&1_u16.to_be_bytes());
    expected_server.extend_from_slice(&1_u64.to_be_bytes());
    expected_server.push(0x01);
    expected_server.extend_from_slice(&41_u32.to_be_bytes());
    expected_server.extend_from_slice(&encoded_value);
    assert_eq!(
        encode_registered_server_frame(&active, &registry, &server),
        Ok(expected_server.clone())
    );
    assert_eq!(
        decode_registered_server_frame(&active, &registry, &expected_server),
        Ok(server)
    );
    assert_eq!(
        decode_active_server_frame(&active, &expected_server),
        Err(FrameCodecError::InvalidMarker)
    );

    let parameter = ParameterId::from_bytes([0x74; 16]);
    let argument = ClientFrame::CallArgument {
        stream: 8,
        parameter,
        value: value.clone(),
    };
    assert_eq!(
        encode_registered_client_frame(&active, &registry, &argument),
        Err(FrameCodecError::OpaqueArgumentNotAccepted {
            opaque_type: OPAQUE_TOKEN_TYPE_ID,
        })
    );
    let mut encoded_argument = b"ORF4\x02\0".to_vec();
    encoded_argument.extend_from_slice(&8_u64.to_be_bytes());
    encoded_argument.extend_from_slice(&57_u32.to_be_bytes());
    encoded_argument.extend_from_slice(&parameter.to_bytes());
    encoded_argument.extend_from_slice(&encoded_value);
    assert_eq!(
        decode_registered_client_frame(&active, &registry, &encoded_argument),
        Err(FrameCodecError::OpaqueArgumentNotAccepted {
            opaque_type: OPAQUE_TOKEN_TYPE_ID,
        })
    );

    let mut connection = ProtocolConnection::default();
    let function = FunctionId::from_bytes([0x75; 16]);
    connection
        .receive_registered(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 8,
                function,
            },
        )
        .unwrap();
    assert_eq!(
        connection.receive_registered(&active, &registry, argument),
        Err(ConnectionError::InvalidFrame {
            source: FrameCodecError::OpaqueArgumentNotAccepted {
                opaque_type: OPAQUE_TOKEN_TYPE_ID,
            },
        })
    );
    assert_eq!(connection.live_streams(), 1);
    connection
        .receive_registered(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 8,
                channel: Channel::ResultValues,
                credit: 57,
            },
        )
        .unwrap();
    assert_eq!(
        connection
            .receive_registered(
                &active,
                &registry,
                ClientFrame::CallArgumentsComplete { stream: 8 },
            )
            .unwrap(),
        Some(ClientAction::Dispatch {
            stream: 8,
            call: RawCall {
                function,
                arguments: vec![],
            },
        })
    );
    let invocation = InvocationId::from_bytes([0x76; 16]);
    connection
        .apply_registered(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 8,
                invocation,
            },
        )
        .unwrap();
    assert_eq!(
        connection
            .apply_registered(
                &active,
                &registry,
                ServerAction::Events {
                    stream: 8,
                    events: vec![Event::Value(value.clone())],
                },
            )
            .unwrap(),
        ServerFrame::EventBatch {
            stream: 8,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value),
            }],
        }
    );
}

#[test]
fn active_frame_codec_round_trips_every_non_value_frame_shape() {
    let active = active_record_revision();
    let function = FunctionId::from_bytes([0x60; 16]);
    let invocation = InvocationId::from_bytes([0x61; 16]);
    let token = [1, 2, 3, 4, 5, 6, 7, 8];
    let client_frames = [
        ClientFrame::CallRawStart {
            stream: 1,
            function,
        },
        ClientFrame::CallArgumentsComplete { stream: 1 },
        ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: 4096,
        },
        ClientFrame::CallCancel { stream: 1 },
        ClientFrame::Ping { token },
    ];
    for frame in client_frames {
        let encoded = encode_active_client_frame(&active, &frame).unwrap();
        let version_one = encode_client_frame(&frame).unwrap();
        assert_eq!(&encoded[..4], b"ORF3");
        assert_eq!(&encoded[4..], &version_one[4..]);
        assert_eq!(decode_active_client_frame(&active, &encoded), Ok(frame));
    }

    let server_frames = [
        ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        },
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultBytes,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Bytes(vec![1, 2, 3]),
            }],
        },
        ServerFrame::CallCompleted { stream: 1 },
        ServerFrame::CallFailed {
            stream: 1,
            failure: CallFailure::ExecuteDenied,
        },
        ServerFrame::CallCancelled { stream: 1 },
        ServerFrame::Pong { token },
    ];
    for frame in server_frames {
        let encoded = encode_active_server_frame(&active, &frame).unwrap();
        let version_one = encode_server_frame(&frame).unwrap();
        assert_eq!(&encoded[..4], b"ORF3");
        assert_eq!(&encoded[4..], &version_one[4..]);
        assert_eq!(decode_active_server_frame(&active, &encoded), Ok(frame));
    }
}

#[test]
fn active_frame_codec_rejects_a_record_from_an_incompatible_revision() {
    let original = active_record_revision();
    let record = &original.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &original,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(original.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let frame = ClientFrame::CallArgument {
        stream: 1,
        parameter: ParameterId::from_bytes([0x62; 16]),
        value,
    };
    let encoded = encode_active_client_frame(&original, &frame).unwrap();
    let changed = active_record_revision_with_second_type(TypeDescriptor::named(BIGINT_TYPE_ID));

    assert_eq!(
        encode_active_client_frame(&changed, &frame),
        Err(FrameCodecError::Value {
            source: ValueCodecError::RecordValueNotActive {
                record_type: record.id(),
            },
        })
    );
    assert!(matches!(
        decode_active_client_frame(&changed, &encoded),
        Err(FrameCodecError::Value {
            source: ValueCodecError::WrongRecordFieldType { ordinal: 1, .. },
        })
    ));
}

#[test]
fn active_connection_carries_record_arguments_and_results() {
    let active = active_record_revision();
    let record = &active.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let function = FunctionId::from_bytes([0x63; 16]);
    let parameter = ParameterId::from_bytes([0x64; 16]);
    let invocation = InvocationId::from_bytes([0x65; 16]);
    let mut connection = ProtocolConnection::new();
    connection
        .receive_active(
            &active,
            ClientFrame::CallRawStart {
                stream: 1,
                function,
            },
        )
        .unwrap();
    connection
        .receive_active(
            &active,
            ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value: value.clone(),
            },
        )
        .unwrap();
    connection
        .receive_active(
            &active,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 4096,
            },
        )
        .unwrap();
    assert_eq!(
        connection
            .receive_active(&active, ClientFrame::CallArgumentsComplete { stream: 1 })
            .unwrap(),
        Some(ClientAction::Dispatch {
            stream: 1,
            call: RawCall {
                function,
                arguments: vec![CallArgument {
                    parameter,
                    value: value.clone(),
                }],
            },
        })
    );
    connection
        .apply_active(
            &active,
            ServerAction::Accepted {
                stream: 1,
                invocation,
            },
        )
        .unwrap();
    let result = connection
        .apply_active(
            &active,
            ServerAction::Events {
                stream: 1,
                events: vec![Event::Value(value.clone())],
            },
        )
        .unwrap();
    assert_eq!(
        result,
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value),
            }],
        }
    );
}

#[test]
fn active_codec_preserves_earlier_shapes_and_marker_closure() {
    let active = active_record_revision();
    let boolean = RuntimeValue::Boolean(true);
    let mut expected_boolean = b"ORV3".to_vec();
    expected_boolean.push(0x02);
    expected_boolean.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    expected_boolean.extend_from_slice(&1_u32.to_be_bytes());
    expected_boolean.push(1);
    let enum_value =
        RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap());
    let mut expected_enum = b"ORV3".to_vec();
    expected_enum.push(0x0a);
    expected_enum.extend_from_slice(&ENUM_TYPE.to_bytes());
    expected_enum.extend_from_slice(&9_u32.to_be_bytes());
    expected_enum.extend_from_slice(b"qualified");

    assert_eq!(
        encode_active_value(&active, &boolean),
        Ok(expected_boolean.clone())
    );
    assert_eq!(decode_active_value(&active, &expected_boolean), Ok(boolean));
    assert_eq!(
        encode_active_value(&active, &enum_value),
        Ok(expected_enum.clone())
    );
    assert_eq!(decode_active_value(&active, &expected_enum), Ok(enum_value));
    assert_eq!(
        decode_value(&expected_boolean),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_catalogue_value(active.catalogue(), &expected_boolean),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_active_value(&active, &encoded_value(0x02, BOOLEAN_TYPE_ID, &[1])),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_active_value(
            &active,
            &encoded_catalogue_value(0x02, BOOLEAN_TYPE_ID, &[1])
        ),
        Err(ValueCodecError::InvalidMarker)
    );
}

#[test]
fn active_codec_round_trips_verified_standard_enums_with_application_precedence() {
    let active = active_record_revision_with_types_and_standard(
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
        TypeDescriptor::named(ENUM_TYPE),
        verified_standard_with_enum(),
    );
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("the fixture pins a verified standard library");
    let standard_value = RuntimeValue::Enum(
        EnumValue::new(standard.catalogue(), STANDARD_ENUM_TYPE, "safe")
            .expect("the verified standard enum declares safe"),
    );
    let encoded_standard = encode_active_value(&active, &standard_value).unwrap();
    assert_eq!(
        decode_active_value(&active, &encoded_standard),
        Ok(standard_value)
    );

    let standard_null = RuntimeValue::null(ResolvedType::named(STANDARD_ENUM_TYPE)).unwrap();
    let encoded_standard_null = encode_active_value(&active, &standard_null).unwrap();
    assert_eq!(
        decode_active_value(&active, &encoded_standard_null),
        Ok(standard_null)
    );

    let application_value = RuntimeValue::Enum(
        EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified")
            .expect("the application enum declares qualified"),
    );
    let encoded_application = encode_active_value(&active, &application_value).unwrap();
    assert_eq!(
        decode_active_value(&active, &encoded_application),
        Ok(application_value)
    );

    let mut unknown_type = encoded_value(0x0a, TypeId::from_bytes([0x99; 16]), b"safe");
    unknown_type[..4].copy_from_slice(b"ORV3");
    assert_eq!(
        decode_active_value(&active, &unknown_type),
        Err(ValueCodecError::InactiveEnumType {
            enum_type: TypeId::from_bytes([0x99; 16]),
        })
    );
    let mut undeclared_label = encoded_value(0x0a, STANDARD_ENUM_TYPE, b"retired");
    undeclared_label[..4].copy_from_slice(b"ORV3");
    assert_eq!(
        decode_active_value(&active, &undeclared_label),
        Err(ValueCodecError::UndeclaredEnumLabel {
            enum_type: STANDARD_ENUM_TYPE,
            label: String::from("retired"),
        })
    );
}

#[test]
fn active_codec_rejects_record_structure_and_value_corruption() {
    let active = active_record_revision();
    let record = &active.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let encoded = encode_active_value(&active, &value).unwrap();

    let mut wrong_count = encoded.clone();
    wrong_count[25..29].copy_from_slice(&1_u32.to_be_bytes());
    assert_eq!(
        decode_active_value(&active, &wrong_count),
        Err(ValueCodecError::WrongRecordFieldCount {
            expected: 2,
            actual: 1,
        })
    );

    let mut wrong_identity = encoded.clone();
    wrong_identity[29..45].fill(0xff);
    assert_eq!(
        decode_active_value(&active, &wrong_identity),
        Err(ValueCodecError::WrongRecordFieldIdentity {
            ordinal: 0,
            expected: record.fields()[0].id(),
            actual: FieldId::from_bytes([0xff; 16]),
        })
    );

    let mut unknown_record = encoded.clone();
    unknown_record[5..21].fill(0xfe);
    assert_eq!(
        decode_active_value(&active, &unknown_record),
        Err(ValueCodecError::InactiveRecordType {
            record_type: TypeId::from_bytes([0xfe; 16]),
        })
    );

    let mut unknown_tag = encoded.clone();
    unknown_tag[4] = 0x0c;
    assert_eq!(
        decode_active_value(&active, &unknown_tag),
        Err(ValueCodecError::UnknownTag { tag: 0x0c })
    );

    for declared in [24_u32, u32::MAX] {
        let mut wrong_length = encoded.clone();
        wrong_length[45..49].copy_from_slice(&declared.to_be_bytes());
        assert_eq!(
            decode_active_value(&active, &wrong_length),
            Err(ValueCodecError::InvalidRecordFieldLength {
                ordinal: 0,
                declared: declared as usize,
                remaining: 75,
            })
        );
    }

    let mut record_tag_in_scalar_field = encoded.clone();
    record_tag_in_scalar_field[53] = 0x0b;
    assert_eq!(
        decode_active_value(&active, &record_tag_in_scalar_field),
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal: 0,
            expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
            tag: 0x0b,
            actual: BOOLEAN_TYPE_ID,
        })
    );

    let mut wrong_field_type = encoded.clone();
    wrong_field_type[53] = 0x06;
    wrong_field_type[54..70].copy_from_slice(&CHARACTER_LARGE_OBJECT_TYPE_ID.to_bytes());
    assert_eq!(
        decode_active_value(&active, &wrong_field_type),
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal: 0,
            expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
            tag: 0x06,
            actual: CHARACTER_LARGE_OBJECT_TYPE_ID,
        })
    );

    let mut stale_enum = encoded.clone();
    stale_enum[120..124].copy_from_slice(b"lost");
    assert_eq!(
        decode_active_value(&active, &stale_enum),
        Err(ValueCodecError::UndeclaredEnumLabel {
            enum_type: ENUM_TYPE,
            label: String::from("lost"),
        })
    );

    let mut wrong_inner_marker = encoded.clone();
    wrong_inner_marker[49..53].copy_from_slice(b"ORV2");
    assert_eq!(
        decode_active_value(&active, &wrong_inner_marker),
        Err(ValueCodecError::InvalidMarker)
    );

    let mut null_field = encoded.clone();
    null_field[45..49].copy_from_slice(&25_u32.to_be_bytes());
    null_field[53] = 0x00;
    null_field[70..74].copy_from_slice(&0_u32.to_be_bytes());
    null_field.remove(74);
    null_field[21..25].copy_from_slice(&98_u32.to_be_bytes());
    assert_eq!(
        decode_active_value(&active, &null_field),
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal: 0,
            expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
            tag: 0x00,
            actual: BOOLEAN_TYPE_ID,
        })
    );

    let mut reference_field = encoded.clone();
    let reference_type = TypeId::from_bytes([0x51; 16]);
    reference_field[45..49].copy_from_slice(&41_u32.to_be_bytes());
    reference_field[53] = 0x08;
    reference_field[54..70].copy_from_slice(&reference_type.to_bytes());
    reference_field[70..74].copy_from_slice(&16_u32.to_be_bytes());
    reference_field.splice(74..75, [0x52; 16]);
    reference_field[21..25].copy_from_slice(&114_u32.to_be_bytes());
    assert_eq!(
        decode_active_value(&active, &reference_field),
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal: 0,
            expected: TypeDescriptor::named(BOOLEAN_TYPE_ID),
            tag: 0x08,
            actual: reference_type,
        })
    );

    let mut truncated = encoded.clone();
    truncated.pop();
    assert_eq!(
        decode_active_value(&active, &truncated),
        Err(ValueCodecError::TruncatedPayload {
            declared: 99,
            actual: 98,
        })
    );

    let mut trailing = encoded.clone();
    trailing[21..25].copy_from_slice(&100_u32.to_be_bytes());
    trailing.push(0);
    assert_eq!(
        decode_active_value(&active, &trailing),
        Err(ValueCodecError::TrailingBytes {
            declared: 99,
            actual: 100,
        })
    );

    let mut oversized = encoded;
    oversized[21..25].copy_from_slice(&((PAYLOAD_LIMIT as u32) + 1).to_be_bytes());
    assert_eq!(
        decode_active_value(&active, &oversized),
        Err(ValueCodecError::PayloadTooLarge {
            actual: PAYLOAD_LIMIT + 1,
            maximum: PAYLOAD_LIMIT,
        })
    );
}

#[test]
fn active_codec_rejects_a_field_length_that_consumes_the_next_entry() {
    let active = active_record_revision_with_types(
        TypeDescriptor::named(BINARY_LARGE_OBJECT_TYPE_ID),
        TypeDescriptor::named(ENUM_TYPE),
    );
    let record = &active.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Bytes(vec![1])),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let mut encoded = encode_active_value(&active, &value).unwrap();
    encoded[45..49].copy_from_slice(&75_u32.to_be_bytes());
    encoded[70..74].copy_from_slice(&50_u32.to_be_bytes());

    assert_eq!(
        decode_active_value(&active, &encoded),
        Err(ValueCodecError::TruncatedRecordFieldHeader {
            ordinal: 1,
            actual: 0,
        })
    );
}

#[test]
fn active_codec_rejects_a_record_from_an_incompatible_active_revision() {
    let original = active_record_revision();
    let record = &original.catalogue().record_value_types()[0];
    let value = RuntimeValue::Record(
        RecordValue::new(
            &original,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(original.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let changed = active_record_revision_with_second_type(TypeDescriptor::named(BIGINT_TYPE_ID));

    assert_eq!(
        encode_active_value(&changed, &value),
        Err(ValueCodecError::RecordValueNotActive {
            record_type: record.id(),
        })
    );
}

#[test]
fn catalogue_codec_round_trips_enum_null_and_legacy_values_as_version_two() {
    let catalogue = enum_catalogue(&["lead"]);
    let null = RuntimeValue::null(ResolvedType::named(ENUM_TYPE)).unwrap();
    let expected_null = encoded_catalogue_value(0x09, ENUM_TYPE, &[]);
    assert_eq!(
        encode_catalogue_value(&catalogue, &null),
        Ok(expected_null.clone())
    );
    assert_eq!(decode_catalogue_value(&catalogue, &expected_null), Ok(null));

    let boolean = RuntimeValue::Boolean(true);
    let expected_boolean = encoded_catalogue_value(0x02, BOOLEAN_TYPE_ID, &[1]);
    assert_eq!(
        encode_catalogue_value(&catalogue, &boolean),
        Ok(expected_boolean.clone())
    );
    assert_eq!(
        decode_catalogue_value(&catalogue, &expected_boolean),
        Ok(boolean)
    );
}

#[test]
fn catalogue_codec_rejects_stale_unknown_and_mismatched_enum_labels() {
    let original = enum_catalogue(&["lead", "qualified"]);
    let active = enum_catalogue(&["lead", "customer"]);
    let stale = RuntimeValue::Enum(EnumValue::new(&original, ENUM_TYPE, "qualified").unwrap());
    assert_eq!(
        encode_catalogue_value(&active, &stale),
        Err(ValueCodecError::UndeclaredEnumLabel {
            enum_type: ENUM_TYPE,
            label: String::from("qualified"),
        })
    );

    let unknown = TypeId::from_bytes([0x46; 16]);
    assert_eq!(
        decode_catalogue_value(&active, &encoded_catalogue_value(0x0a, unknown, b"lead")),
        Err(ValueCodecError::InactiveEnumType { enum_type: unknown })
    );
    assert_eq!(
        decode_catalogue_value(
            &active,
            &encoded_catalogue_value(0x0a, ENUM_TYPE, b"qualified")
        ),
        Err(ValueCodecError::UndeclaredEnumLabel {
            enum_type: ENUM_TYPE,
            label: String::from("qualified"),
        })
    );
    assert_eq!(
        decode_catalogue_value(&active, &encoded_catalogue_value(0x0a, ENUM_TYPE, &[0xff])),
        Err(ValueCodecError::InvalidUtf8)
    );
    assert_eq!(
        decode_catalogue_value(&active, &encoded_catalogue_value(0x09, ENUM_TYPE, b"lead")),
        Err(ValueCodecError::WrongPayloadLength {
            tag: 0x09,
            expected: 0,
            actual: 4,
        })
    );
}

#[test]
fn signed_integers_have_exact_big_endian_bytes_and_round_trip() {
    let mut integer = b"ORV1".to_vec();
    integer.push(0x03);
    integer.extend_from_slice(&INTEGER_TYPE_ID.to_bytes());
    integer.extend_from_slice(&4_u32.to_be_bytes());
    integer.extend_from_slice(&(-2_i32).to_be_bytes());
    assert_eq!(
        encode_value(&RuntimeValue::Integer(-2)),
        Ok(integer.clone())
    );
    assert_eq!(decode_value(&integer), Ok(RuntimeValue::Integer(-2)));

    let mut bigint = b"ORV1".to_vec();
    bigint.push(0x04);
    bigint.extend_from_slice(&BIGINT_TYPE_ID.to_bytes());
    bigint.extend_from_slice(&8_u32.to_be_bytes());
    bigint.extend_from_slice(&(-3_i64).to_be_bytes());
    assert_eq!(encode_value(&RuntimeValue::BigInt(-3)), Ok(bigint.clone()));
    assert_eq!(decode_value(&bigint), Ok(RuntimeValue::BigInt(-3)));
}

#[test]
fn float_has_exact_bytes_and_normalises_negative_zero() {
    let mut expected = b"ORV1".to_vec();
    expected.push(0x05);
    expected.extend_from_slice(&FLOAT_TYPE_ID.to_bytes());
    expected.extend_from_slice(&8_u32.to_be_bytes());
    expected.extend_from_slice(&1.5_f64.to_bits().to_be_bytes());
    let value = RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap());
    assert_eq!(encode_value(&value), Ok(expected.clone()));
    assert_eq!(decode_value(&expected), Ok(value));

    let positive = RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap());
    let negative = RuntimeValue::Float(RuntimeFloat::new(-0.0).unwrap());
    assert_eq!(encode_value(&negative), encode_value(&positive));
}

#[test]
fn text_and_bytes_preserve_payloads_and_enforce_the_shared_limit() {
    let mut text = b"ORV1".to_vec();
    text.push(0x06);
    text.extend_from_slice(&CHARACTER_LARGE_OBJECT_TYPE_ID.to_bytes());
    text.extend_from_slice(&2_u32.to_be_bytes());
    text.extend_from_slice("é".as_bytes());
    assert_eq!(
        encode_value(&RuntimeValue::Text("é".into())),
        Ok(text.clone())
    );
    assert_eq!(decode_value(&text), Ok(RuntimeValue::Text("é".into())));

    let mut bytes = b"ORV1".to_vec();
    bytes.push(0x07);
    bytes.extend_from_slice(&BINARY_LARGE_OBJECT_TYPE_ID.to_bytes());
    bytes.extend_from_slice(&3_u32.to_be_bytes());
    bytes.extend_from_slice(&[0, 0xff, 1]);
    assert_eq!(
        encode_value(&RuntimeValue::Bytes(vec![0, 0xff, 1])),
        Ok(bytes.clone())
    );
    assert_eq!(
        decode_value(&bytes),
        Ok(RuntimeValue::Bytes(vec![0, 0xff, 1]))
    );

    let oversized = vec![b'x'; 16 * 1024 * 1024 + 1];
    assert_eq!(
        encode_value(&RuntimeValue::Bytes(oversized.clone())),
        Err(ValueCodecError::PayloadTooLarge {
            actual: oversized.len(),
            maximum: 16 * 1024 * 1024,
        })
    );
    assert_eq!(
        encode_value(&RuntimeValue::Text(
            String::from_utf8(oversized).expect("ASCII fixture is UTF-8")
        )),
        Err(ValueCodecError::PayloadTooLarge {
            actual: 16 * 1024 * 1024 + 1,
            maximum: 16 * 1024 * 1024,
        })
    );
}

#[test]
fn typed_nulls_and_references_retain_exact_type_and_object_identity() {
    let boolean_null = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean))
        .expect("BOOLEAN null is supported");
    let mut expected_null = b"ORV1".to_vec();
    expected_null.push(0x00);
    expected_null.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    expected_null.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(encode_value(&boolean_null), Ok(expected_null.clone()));
    assert_eq!(decode_value(&expected_null), Ok(boolean_null));

    let target = TypeId::from_bytes([0x41; 16]);
    let object = ObjectId::from_bytes([0x42; 16]);
    let reference_null =
        RuntimeValue::null(ResolvedType::reference(target)).expect("reference null is supported");
    let mut expected_reference_null = b"ORV1".to_vec();
    expected_reference_null.push(0x01);
    expected_reference_null.extend_from_slice(&target.to_bytes());
    expected_reference_null.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        encode_value(&reference_null),
        Ok(expected_reference_null.clone())
    );
    assert_eq!(decode_value(&expected_reference_null), Ok(reference_null));

    let reference = RuntimeValue::Reference { target, object };
    let mut expected_reference = b"ORV1".to_vec();
    expected_reference.push(0x08);
    expected_reference.extend_from_slice(&target.to_bytes());
    expected_reference.extend_from_slice(&16_u32.to_be_bytes());
    expected_reference.extend_from_slice(&object.to_bytes());
    assert_eq!(encode_value(&reference), Ok(expected_reference.clone()));
    assert_eq!(decode_value(&expected_reference), Ok(reference));
}

#[test]
fn runtime_and_codec_accept_exactly_the_six_supported_standard_scalar_families() {
    let supported = [
        (StandardScalar::Boolean, BOOLEAN_TYPE_ID),
        (StandardScalar::Integer, INTEGER_TYPE_ID),
        (StandardScalar::BigInt, BIGINT_TYPE_ID),
        (StandardScalar::Float, FLOAT_TYPE_ID),
        (
            StandardScalar::CharacterLargeObject,
            CHARACTER_LARGE_OBJECT_TYPE_ID,
        ),
        (
            StandardScalar::BinaryLargeObject,
            BINARY_LARGE_OBJECT_TYPE_ID,
        ),
    ];
    for (scalar, type_id) in supported {
        let value = RuntimeValue::null(ResolvedType::scalar(scalar)).unwrap();
        let encoded = encode_value(&value).unwrap();
        assert_eq!(encoded, encoded_value(0x00, type_id, &[]));
        assert_eq!(decode_value(&encoded), Ok(value));
    }

    // Keep this list explicit: deriving it by subtracting supported IDs would let
    // a newly admitted scalar silently widen the contract without changing this proof.
    let unsupported = [
        (StandardScalar::Decimal, DECIMAL_TYPE_ID),
        (StandardScalar::Uuid, UUID_TYPE_ID),
        (StandardScalar::Date, DATE_TYPE_ID),
        (StandardScalar::Time, TIME_TYPE_ID),
        (StandardScalar::Timestamp, TIMESTAMP_TYPE_ID),
        (StandardScalar::Duration, DURATION_TYPE_ID),
        (StandardScalar::Void, VOID_TYPE_ID),
    ];
    for (scalar, unsupported) in unsupported {
        let resolved_type = ResolvedType::scalar(scalar);
        assert_eq!(
            RuntimeValue::null(resolved_type),
            Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
        );
        let mut encoded = b"ORV1".to_vec();
        encoded.push(0x00);
        encoded.extend_from_slice(&unsupported.to_bytes());
        encoded.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            decode_value(&encoded),
            Err(ValueCodecError::WrongType {
                tag: 0x00,
                actual: unsupported,
            })
        );
    }
}

#[test]
fn references_reject_every_stable_standard_scalar_identity() {
    for target in STANDARD_TYPE_IDS {
        let reference = RuntimeValue::Reference {
            target,
            object: ObjectId::from_bytes([0x42; 16]),
        };
        assert_eq!(
            encode_value(&reference),
            Err(ValueCodecError::StandardTypeAsReference { target })
        );
        let null = RuntimeValue::null(ResolvedType::reference(target)).unwrap();
        assert_eq!(
            encode_value(&null),
            Err(ValueCodecError::StandardTypeAsReference { target })
        );

        for tag in [0x01, 0x08] {
            let payload = if tag == 0x08 {
                &[0x42; 16][..]
            } else {
                &[][..]
            };
            let mut encoded = b"ORV1".to_vec();
            encoded.push(tag);
            encoded.extend_from_slice(&target.to_bytes());
            encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            encoded.extend_from_slice(payload);
            assert_eq!(
                decode_value(&encoded),
                Err(ValueCodecError::StandardTypeAsReference { target })
            );
        }
    }
}

#[test]
fn scalar_tags_accept_only_their_matching_supported_identity() {
    for (tag, expected, payload) in [
        (0x02, BOOLEAN_TYPE_ID, vec![1]),
        (0x03, INTEGER_TYPE_ID, vec![0; 4]),
        (0x04, BIGINT_TYPE_ID, vec![0; 8]),
        (0x05, FLOAT_TYPE_ID, vec![0; 8]),
        (0x06, CHARACTER_LARGE_OBJECT_TYPE_ID, vec![]),
        (0x07, BINARY_LARGE_OBJECT_TYPE_ID, vec![]),
    ] {
        for actual in STANDARD_TYPE_IDS {
            let encoded = encoded_value(tag, actual, &payload);
            if actual == expected {
                assert!(decode_value(&encoded).is_ok());
            } else {
                assert_eq!(
                    decode_value(&encoded),
                    Err(ValueCodecError::WrongType { tag, actual })
                );
            }
        }
    }
}

#[test]
fn malformed_envelopes_and_payloads_fail_closed() {
    for length in 0..25 {
        assert_eq!(
            decode_value(&vec![0; length]),
            Err(ValueCodecError::TruncatedHeader { actual: length })
        );
    }

    let mut bad_marker = encoded_value(0x02, BOOLEAN_TYPE_ID, &[1]);
    bad_marker[0] = b'X';
    assert_eq!(
        decode_value(&bad_marker),
        Err(ValueCodecError::InvalidMarker)
    );

    assert_eq!(
        decode_value(&encoded_value(0xff, BOOLEAN_TYPE_ID, &[])),
        Err(ValueCodecError::UnknownTag { tag: 0xff })
    );

    let mut truncated = encoded_value(0x02, BOOLEAN_TYPE_ID, &[1]);
    truncated[21..25].copy_from_slice(&2_u32.to_be_bytes());
    assert_eq!(
        decode_value(&truncated),
        Err(ValueCodecError::TruncatedPayload {
            declared: 2,
            actual: 1,
        })
    );

    let mut trailing = encoded_value(0x02, BOOLEAN_TYPE_ID, &[1]);
    trailing[21..25].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        decode_value(&trailing),
        Err(ValueCodecError::TrailingBytes {
            declared: 0,
            actual: 1,
        })
    );

    let mut oversized = encoded_value(0x07, BINARY_LARGE_OBJECT_TYPE_ID, &[]);
    oversized[21..25].copy_from_slice(&(16_u32 * 1024 * 1024 + 1).to_be_bytes());
    assert_eq!(
        decode_value(&oversized),
        Err(ValueCodecError::PayloadTooLarge {
            actual: 16 * 1024 * 1024 + 1,
            maximum: 16 * 1024 * 1024,
        })
    );

    assert_eq!(
        decode_value(&encoded_value(
            0x06,
            CHARACTER_LARGE_OBJECT_TYPE_ID,
            &[0xff]
        )),
        Err(ValueCodecError::InvalidUtf8)
    );
    assert_eq!(
        decode_value(&encoded_value(0x03, INTEGER_TYPE_ID, &[0; 3])),
        Err(ValueCodecError::WrongPayloadLength {
            tag: 0x03,
            expected: 4,
            actual: 3,
        })
    );
    assert_eq!(
        decode_value(&encoded_value(0x00, BOOLEAN_TYPE_ID, &[0])),
        Err(ValueCodecError::WrongPayloadLength {
            tag: 0x00,
            expected: 0,
            actual: 1,
        })
    );
}

#[test]
fn boolean_and_float_payloads_require_canonical_values() {
    for value in 2..=u8::MAX {
        assert_eq!(
            decode_value(&encoded_value(0x02, BOOLEAN_TYPE_ID, &[value])),
            Err(ValueCodecError::InvalidBoolean { value })
        );
    }

    for bits in [
        (-0.0_f64).to_bits(),
        f64::NAN.to_bits(),
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
    ] {
        assert_eq!(
            decode_value(&encoded_value(0x05, FLOAT_TYPE_ID, &bits.to_be_bytes())),
            Err(ValueCodecError::NonCanonicalFloat)
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_bytes_never_panic(
        bytes in prop::collection::vec(any::<u8>(), 0..=65_536),
    ) {
        let _ = decode_value(&bytes);
    }

    #[test]
    fn arbitrary_version_one_envelopes_never_panic(
        tag in any::<u8>(),
        type_bytes in any::<[u8; 16]>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let mut encoded = b"ORV1".to_vec();
        encoded.push(tag);
        encoded.extend_from_slice(&type_bytes);
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_value(&encoded);
    }

    #[test]
    fn arbitrary_version_two_envelopes_never_panic(
        tag in any::<u8>(),
        type_bytes in any::<[u8; 16]>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let catalogue = enum_catalogue(&["lead", "qualified"]);
        let mut encoded = b"ORV2".to_vec();
        encoded.push(tag);
        encoded.extend_from_slice(&type_bytes);
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_catalogue_value(&catalogue, &encoded);
    }

    #[test]
    fn arbitrary_version_three_envelopes_never_panic(
        tag in any::<u8>(),
        type_bytes in any::<[u8; 16]>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let mut encoded = b"ORV3".to_vec();
        encoded.push(tag);
        encoded.extend_from_slice(&type_bytes);
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_active_value(&active, &encoded);
    }

    #[test]
    fn arbitrary_version_three_frame_envelopes_never_panic(
        tag in any::<u8>(),
        flags in any::<u8>(),
        stream in any::<u64>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let mut encoded = b"ORF3".to_vec();
        encoded.push(tag);
        encoded.push(flags);
        encoded.extend_from_slice(&stream.to_be_bytes());
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_active_client_frame(&active, &encoded);
        let _ = decode_active_server_frame(&active, &encoded);
    }

    #[test]
    fn arbitrary_version_four_envelopes_never_panic(
        tag in any::<u8>(),
        type_bytes in any::<[u8; 16]>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let registry = registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        ).unwrap();
        let mut encoded = b"ORV4".to_vec();
        encoded.push(tag);
        encoded.extend_from_slice(&type_bytes);
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_registered_value(&active, &registry, &encoded);
    }

    #[test]
    fn arbitrary_version_five_constructed_bytes_never_panic(
        descriptor in prop::collection::vec(any::<u8>(), 0..=4_096),
        body in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let registry = registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        ).unwrap();
        let mut payload = (descriptor.len() as u16).to_be_bytes().to_vec();
        payload.extend_from_slice(&descriptor);
        payload.extend_from_slice(&body);
        let _ = decode_constructed_value(&active, &registry, &orv5_constructed(payload));
    }

    #[test]
    fn arbitrary_version_five_untrusted_envelopes_never_panic(
        tag in any::<u8>(),
        type_bytes in any::<[u8; 16]>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let registry = registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        ).unwrap();
        let mut encoded = b"ORV5".to_vec();
        encoded.push(tag);
        encoded.extend_from_slice(&type_bytes);
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_constructed_value(&active, &registry, &encoded);
    }

    #[test]
    fn arbitrary_bounded_invocation_carrier_payloads_never_panic(
        carrier_index in 0_usize..3,
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let registry = registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        ).unwrap();
        let carrier = [
            SYS_INVOKE_VALUE_TYPE_ID,
            SYS_INVOKE_REQUEST_TYPE_ID,
            SYS_INVOKE_EVENT_TYPE_ID,
        ][carrier_index];
        let _ = decode_constructed_value(
            &active,
            &registry,
            &raw_carrier(carrier, &payload),
        );
    }

    #[test]
    fn arbitrary_version_four_frame_envelopes_never_panic(
        tag in any::<u8>(),
        flags in any::<u8>(),
        stream in any::<u64>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let registry = registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        ).unwrap();
        let mut encoded = b"ORF4".to_vec();
        encoded.push(tag);
        encoded.push(flags);
        encoded.extend_from_slice(&stream.to_be_bytes());
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_registered_client_frame(&active, &registry, &encoded);
        let _ = decode_registered_server_frame(&active, &registry, &encoded);
    }

    #[test]
    fn arbitrary_version_five_frame_envelopes_never_panic(
        tag in any::<u8>(),
        flags in any::<u8>(),
        stream in any::<u64>(),
        declared in any::<u32>(),
        payload in prop::collection::vec(any::<u8>(), 0..=4_096),
    ) {
        let active = active_record_revision();
        let registry = registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        ).unwrap();
        let mut encoded = b"ORF5".to_vec();
        encoded.push(tag);
        encoded.push(flags);
        encoded.extend_from_slice(&stream.to_be_bytes());
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_constructed_client_frame(&active, &registry, &encoded);
        let _ = decode_constructed_server_frame(&active, &registry, &encoded);
    }
}

fn constructed_collection_values(active: &ActiveDatabaseRevision) -> Vec<RuntimeValue> {
    let option = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let list = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let map = TypeDescriptor::map(
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    vec![
        RuntimeValue::option(active, option, Some(RuntimeValue::Boolean(true))).unwrap(),
        RuntimeValue::list(active, list, vec![RuntimeValue::Boolean(true)]).unwrap(),
        RuntimeValue::map(
            active,
            map,
            vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(true))],
        )
        .unwrap(),
    ]
}

fn minimal_invocation_request(
    sinks: Vec<InvocationSinkOffer>,
    runtimes: Vec<InvocationRuntimeOffer>,
) -> InvokeRequest {
    InvokeRequest::new(InvokeRequestInput {
        target: InvocationTarget::function_id(FunctionId::from_bytes([0x11; 16])),
        arguments: Vec::new(),
        caller_context: InvocationCallerContext::new(
            InvocationCallerKind::Browser,
            false,
            false,
            None,
            None,
            "en-GB",
            "UTC",
            None,
        )
        .unwrap(),
        client_offer: InvocationClientOffer::new(
            5, "en-GB", "UTC", sinks, runtimes, 1_024, 0, None, None,
        )
        .unwrap(),
        output_requirement: None,
        state_profile: None,
        trace_policy: InvocationTracePolicy::Off,
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })
    .unwrap()
}

fn carrier_test_values() -> [RuntimeValue; 3] {
    [
        RuntimeValue::InvokeValue(InvokeValue::new(RuntimeValue::Integer(7)).unwrap()),
        RuntimeValue::InvokeRequest(minimal_invocation_request(Vec::new(), Vec::new())),
        RuntimeValue::InvokeEvent(
            InvokeEvent::new(
                InvocationId::from_bytes([0x22; 16]),
                u64::MAX,
                InvocationEventBody::Completed {
                    duration_nanoseconds: 99,
                },
            )
            .unwrap(),
        ),
    ]
}

fn assert_carrier_source(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    carrier: TypeId,
    payload: &[u8],
    expected: InvocationCarrierCodecError,
) {
    assert_eq!(
        decode_constructed_value(active, registry, &raw_carrier(carrier, payload)),
        Err(ValueCodecError::InvocationCarrier {
            carrier,
            source: expected,
        })
    );
}

fn raw_invoke_value_carrier(inner: &[u8]) -> Vec<u8> {
    raw_carrier(SYS_INVOKE_VALUE_TYPE_ID, &raw_invoke_value_payload(inner))
}

fn raw_invoke_value_payload(inner: &[u8]) -> Vec<u8> {
    let mut payload = vec![1];
    payload.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    payload.extend_from_slice(inner);
    payload
}

fn raw_request_carrier(arguments: &[(ParameterId, Vec<u8>)]) -> Vec<u8> {
    raw_carrier(SYS_INVOKE_REQUEST_TYPE_ID, &raw_request_payload(arguments))
}

fn raw_request_payload(arguments: &[(ParameterId, Vec<u8>)]) -> Vec<u8> {
    let mut payload = vec![1, 0];
    payload.extend_from_slice(&[0x11; 16]);
    payload.extend_from_slice(&(arguments.len() as u32).to_be_bytes());
    for (parameter, value) in arguments {
        payload.push(0);
        payload.extend_from_slice(&parameter.to_bytes());
        payload.extend_from_slice(&(value.len() as u32).to_be_bytes());
        payload.extend_from_slice(value);
    }
    payload.extend_from_slice(&[3, 0, 0, 0]);
    payload.extend_from_slice(&5_u32.to_be_bytes());
    payload.extend_from_slice(b"en-GB");
    payload.extend_from_slice(&3_u32.to_be_bytes());
    payload.extend_from_slice(b"UTC");
    payload.push(0);
    payload.extend_from_slice(&5_u16.to_be_bytes());
    payload.extend_from_slice(&5_u32.to_be_bytes());
    payload.extend_from_slice(b"en-GB");
    payload.extend_from_slice(&3_u32.to_be_bytes());
    payload.extend_from_slice(b"UTC");
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    payload.extend_from_slice(&1_024_u32.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(&[0; 9]);
    payload
}

fn raw_carrier(carrier: TypeId, payload: &[u8]) -> Vec<u8> {
    let mut encoded = b"ORV5".to_vec();
    encoded.push(0x0c);
    encoded.extend_from_slice(&carrier.to_bytes());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(payload);
    encoded
}

fn orv5_constructed(payload: Vec<u8>) -> Vec<u8> {
    let mut encoded = b"ORV5".to_vec();
    encoded.push(0x0d);
    encoded.extend_from_slice(&[0; 16]);
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(&payload);
    encoded
}

fn orv6_constructed(payload: Vec<u8>) -> Vec<u8> {
    let mut encoded = b"ORV6".to_vec();
    encoded.push(0x0d);
    encoded.extend_from_slice(&[0; 16]);
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(&payload);
    encoded
}

fn orf5_frame(tag: u8, stream: u64, payload: &[u8]) -> Vec<u8> {
    let mut encoded = b"ORF5".to_vec();
    encoded.push(tag);
    encoded.push(0);
    encoded.extend_from_slice(&stream.to_be_bytes());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(payload);
    encoded
}

fn orv5_descriptor_payload(descriptor: &[u8], body: &[u8]) -> Vec<u8> {
    let mut payload = (descriptor.len() as u16).to_be_bytes().to_vec();
    payload.extend_from_slice(descriptor);
    payload.extend_from_slice(body);
    payload
}

fn orv5_named_descriptor(type_id: TypeId) -> Vec<u8> {
    let mut descriptor = vec![0x00];
    descriptor.extend_from_slice(&type_id.to_bytes());
    descriptor
}

fn assert_orv4_to_orv5_flat_marker_substitution(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: RuntimeValue,
) {
    let version_four = encode_registered_value(active, registry, &value).unwrap();
    let mut expected = version_four;
    assert_eq!(&expected[..4], b"ORV4");
    expected[..4].copy_from_slice(b"ORV5");
    if matches!(
        &value,
        RuntimeValue::Text(text) if text.as_bytes().windows(4).any(|window| window == b"ORV4")
    ) || matches!(
        &value,
        RuntimeValue::Bytes(bytes) if bytes.windows(4).any(|window| window == b"ORV4")
    ) {
        assert!(expected.windows(4).any(|window| window == b"ORV4"));
    }
    assert_eq!(
        encode_constructed_value(active, registry, &value),
        Ok(expected.clone())
    );
    assert_eq!(
        decode_constructed_value(active, registry, &expected),
        Ok(value)
    );
}

fn orv5_integer(value: i32) -> Vec<u8> {
    let mut encoded = b"ORV5".to_vec();
    encoded.push(0x03);
    encoded.extend_from_slice(&INTEGER_TYPE_ID.to_bytes());
    encoded.extend_from_slice(&4_u32.to_be_bytes());
    encoded.extend_from_slice(&value.to_be_bytes());
    encoded
}

fn orv5_boolean(value: bool) -> Vec<u8> {
    let mut encoded = b"ORV5".to_vec();
    encoded.push(0x02);
    encoded.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    encoded.extend_from_slice(&1_u32.to_be_bytes());
    encoded.push(u8::from(value));
    encoded
}

fn orv5_boolean_list_prefix(count: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&18_u16.to_be_bytes());
    payload.extend_from_slice(&[0x02, 0x00]);
    payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    payload.extend_from_slice(&count.to_be_bytes());
    payload
}

fn orv5_map_entry(key: Vec<u8>, value: Vec<u8>) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&(key.len() as u32).to_be_bytes());
    entry.extend_from_slice(&key);
    entry.extend_from_slice(&(value.len() as u32).to_be_bytes());
    entry.extend_from_slice(&value);
    entry
}

fn encoded_value(tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
    let mut encoded = b"ORV1".to_vec();
    encoded.push(tag);
    encoded.extend_from_slice(&type_id.to_bytes());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(payload);
    encoded
}

fn encoded_catalogue_value(tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
    let mut encoded = encoded_value(tag, type_id, payload);
    encoded[..4].copy_from_slice(b"ORV2");
    encoded
}
