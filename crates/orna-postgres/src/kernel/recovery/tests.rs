use std::collections::BTreeMap;

use orna_core::{
    CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        catalogue_digest, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        SchemaDefinition, ValueTypeKind, ValueTypePersistence,
    },
    revision::{
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        RevisionPair, SourceOrigin, StoredSourceUnit,
    },
    system::SYS_INSPECT_INVOCATION_TYPE_ID,
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};

use crate::{PostgresKernelError, decode::DurableRecord};

use super::{
    ACTIVE_RELATION, CATALOGUE_REVISION_RELATION, LegacyResolvedTypeTupleMember,
    RecordValueFieldTypeTuple, RecoveredCatalogueSemantics, RecoveredFunctionState,
    RecoveredRecordValueField, RecoveredRecordValueType, RecoveredRevisionHeader, RecoveredSchema,
    ResolvedTypeTuple, RevisionPairHistoryEntry, SOURCE_REVISION_RELATION,
    assemble_catalogue_semantics, assemble_revision, decode_catalogue_hash_version,
    decode_legacy_resolved_type_tuple, decode_legacy_resolved_type_tuple_kind,
    decode_record_value_field_descriptor, decode_resolved_type_tuple, decode_revision_pair_values,
    decode_standard_binding_target, recovered_standard_value_definition, validate_function_type,
    validate_revision_pair_listing, verify_recovered_standard_snapshot,
};

#[test]
fn recovered_standard_verifier_dispatches_all_retained_revisions_and_rejects_crossed_identity() {
    let retained = [
        orna_standard::retained_standard_library_snapshot().expect("retained V1 standard"),
        orna_standard::retained_standard_library_v2_snapshot().expect("retained V2 standard"),
        orna_standard::retained_standard_library_v3_snapshot().expect("retained V3 standard"),
        orna_standard::retained_standard_library_v4_snapshot().expect("retained V4 standard"),
        orna_standard::retained_standard_library_v5_snapshot().expect("retained V5 standard"),
        orna_standard::retained_standard_library_v6_snapshot().expect("retained V6 standard"),
        orna_standard::retained_standard_library_v7_snapshot().expect("retained V7 standard"),
        orna_standard::retained_standard_library_v8_snapshot().expect("retained V8 standard"),
        orna_standard::retained_standard_library_v9_snapshot().expect("retained V9 standard"),
        orna_standard::retained_standard_library_v10_snapshot().expect("retained V10 standard"),
        orna_standard::retained_standard_library_v11_snapshot().expect("retained V11 standard"),
    ];
    for snapshot in retained {
        let revision = snapshot.revision();
        let verified = verify_recovered_standard_snapshot(snapshot)
            .expect("each retained standard revision must use its matching verifier");
        assert_eq!(verified.revision(), revision);
    }

    let v3 = orna_standard::retained_standard_library_v3_snapshot().expect("retained V3 standard");
    let crossed = orna_core::revision::StandardLibrarySnapshot::new_with_executables(
        orna_standard::STANDARD_LIBRARY_V2_REVISION_ID,
        v3.digest_version(),
        v3.source().clone(),
        v3.language_version(),
        v3.catalogue().clone(),
        v3.executables().to_vec(),
        v3.origins().to_vec(),
        v3.digest(),
    )
    .expect("crossed identity keeps snapshot shape valid");
    assert!(matches!(
        verify_recovered_standard_snapshot(crossed),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.standard_library_revisions",
            rule: "standard library retained verifier rejected the recovered snapshot",
            ..
        })
    ));

    let v1 = orna_standard::retained_standard_library_snapshot().expect("retained V1 standard");
    let unknown = orna_core::revision::StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([0xee; 16]),
        v1.digest_version(),
        v1.source().clone(),
        v1.language_version(),
        v1.catalogue().clone(),
        v1.origins().to_vec(),
        v1.digest(),
    )
    .expect("unknown identity keeps snapshot shape valid");
    assert!(matches!(
        verify_recovered_standard_snapshot(unknown),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.standard_library_revisions",
            rule: "standard library revision identity is not an accepted retained revision",
            ..
        })
    ));
}

fn revision_pair_history_entry(
    source: u8,
    source_parent: Option<u8>,
    catalogue: u8,
    catalogue_parent: Option<u8>,
) -> RevisionPairHistoryEntry {
    revision_pair_history_entry_with_active(
        source,
        source_parent,
        catalogue,
        catalogue_parent,
        source,
        catalogue,
    )
}

fn revision_pair_history_entry_with_active(
    source: u8,
    source_parent: Option<u8>,
    catalogue: u8,
    catalogue_parent: Option<u8>,
    active_source: u8,
    active_catalogue: u8,
) -> RevisionPairHistoryEntry {
    let source_record = DurableRecord::new(SOURCE_REVISION_RELATION, "test");
    let catalogue_record = DurableRecord::new(CATALOGUE_REVISION_RELATION, "test");
    decode_revision_pair_values(
        vec![source; 16],
        source_parent.map(|id| vec![id; 16]),
        vec![catalogue; 16],
        catalogue_parent.map(|id| vec![id; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([active_source; 16]),
            CatalogueRevisionId::from_bytes([active_catalogue; 16]),
        ),
        &source_record,
        &catalogue_record,
    )
    .expect("valid revision pair test entry")
}

fn test_origin(identity: DefinitionIdentity, start: u32) -> DefinitionOrigin {
    DefinitionOrigin::new(
        identity,
        SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), start, start + 1)
            .expect("test source origin"),
    )
}

#[test]
fn revision_pair_history_decoder_requires_exact_identity_shapes() {
    let source_record = DurableRecord::new("_orna_kernel.source_revisions", "row=0");
    let catalogue_record = DurableRecord::new("_orna_kernel.catalogue_revisions", "row=0");
    let active = RevisionPair::new(
        SourceRevisionId::from_bytes([2; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );

    let entry = decode_revision_pair_values(
        vec![2; 16],
        Some(vec![1; 16]),
        vec![4; 16],
        Some(vec![3; 16]),
        active,
        &source_record,
        &catalogue_record,
    )
    .expect("valid revision pair row");
    assert!(entry.is_active());
    assert_eq!(
        entry.source_parent_revision_id(),
        Some(SourceRevisionId::from_bytes([1; 16]))
    );
    assert_eq!(
        entry.catalogue_parent_revision_id(),
        Some(CatalogueRevisionId::from_bytes([3; 16]))
    );

    assert!(
        decode_revision_pair_values(
            vec![2; 15],
            None,
            vec![4; 16],
            None,
            active,
            &source_record,
            &catalogue_record,
        )
        .is_err()
    );
    assert!(
        decode_revision_pair_values(
            vec![2; 16],
            Some(vec![3; 17]),
            vec![4; 16],
            None,
            active,
            &source_record,
            &catalogue_record,
        )
        .is_err()
    );
    assert!(
        decode_revision_pair_values(
            vec![2; 16],
            None,
            vec![4; 16],
            Some(vec![3; 16]),
            active,
            &source_record,
            &catalogue_record,
        )
        .is_err()
    );
    assert!(
        decode_revision_pair_values(
            vec![2; 16],
            Some(vec![3; 16]),
            vec![4; 16],
            None,
            active,
            &source_record,
            &catalogue_record,
        )
        .is_err()
    );
}

#[test]
fn revision_pair_listing_rejects_orphan_parent() {
    let entries = vec![revision_pair_history_entry(2, Some(1), 4, Some(3))];

    assert!(matches!(
        validate_revision_pair_listing(&entries),
        Err(PostgresKernelError::DurableInvariant {
            relation: CATALOGUE_REVISION_RELATION,
            rule: "each catalogue parent must exist and identify the corresponding source parent",
            ..
        })
    ));
}

#[test]
fn revision_pair_listing_rejects_duplicate_source_identity() {
    let entries = vec![
        revision_pair_history_entry(1, None, 2, None),
        revision_pair_history_entry_with_active(1, None, 4, None, 1, 4),
    ];

    assert!(matches!(
        validate_revision_pair_listing(&entries),
        Err(PostgresKernelError::DurableInvariant {
            relation: SOURCE_REVISION_RELATION,
            rule: "source revision identities must be unique",
            ..
        })
    ));
}

#[test]
fn revision_pair_listing_rejects_duplicate_catalogue_identity() {
    let entries = vec![
        revision_pair_history_entry(1, None, 2, None),
        revision_pair_history_entry_with_active(2, None, 2, None, 2, 2),
    ];

    assert!(matches!(
        validate_revision_pair_listing(&entries),
        Err(PostgresKernelError::DurableInvariant {
            relation: CATALOGUE_REVISION_RELATION,
            rule: "catalogue revision identities must be unique",
            ..
        })
    ));
}

#[test]
fn revision_pair_listing_rejects_cycles() {
    let entries = vec![
        revision_pair_history_entry(2, Some(1), 4, Some(3)),
        revision_pair_history_entry(1, Some(2), 3, Some(4)),
    ];

    assert!(matches!(
        validate_revision_pair_listing(&entries),
        Err(PostgresKernelError::DurableInvariant {
            relation: CATALOGUE_REVISION_RELATION,
            rule: "catalogue and source revision ancestry must terminate without repeated identities",
            ..
        })
    ));
}
#[test]
fn revision_pair_listing_rejects_mismatched_parent_source() {
    let entries = vec![
        revision_pair_history_entry(1, None, 3, None),
        revision_pair_history_entry_with_active(2, Some(9), 4, Some(3), 2, 4),
    ];

    assert!(matches!(
        validate_revision_pair_listing(&entries),
        Err(PostgresKernelError::DurableInvariant {
            relation: CATALOGUE_REVISION_RELATION,
            rule: "each catalogue parent must exist and identify the corresponding source parent",
            ..
        })
    ));
}

#[test]
fn revision_pair_listing_requires_exactly_one_active_pair() {
    let entries = vec![
        revision_pair_history_entry_with_active(1, None, 2, None, 9, 9),
        revision_pair_history_entry_with_active(2, Some(1), 4, Some(2), 9, 9),
    ];

    assert!(matches!(
        validate_revision_pair_listing(&entries),
        Err(PostgresKernelError::DurableInvariant {
            relation: ACTIVE_RELATION,
            rule: "exactly one listed revision pair must match the active marker",
            ..
        })
    ));
}

#[test]
fn revision_pair_listing_rejects_multiple_active_pairs() {
    let entries = vec![
        revision_pair_history_entry_with_active(1, None, 2, None, 1, 2),
        revision_pair_history_entry_with_active(2, Some(1), 4, Some(2), 2, 4),
    ];

    assert!(matches!(
        validate_revision_pair_listing(&entries),
        Err(PostgresKernelError::DurableInvariant {
            relation: ACTIVE_RELATION,
            rule: "exactly one listed revision pair must match the active marker",
            ..
        })
    ));
}

#[test]
fn recovers_only_the_closed_opaque_standard_definition_shape() {
    let record = DurableRecord::new(
        "_orna_kernel.standard_catalogue_value_types",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let id = TypeId::from_bytes([0xaa; 16]);
    let name = QualifiedSemanticName::new(["std", "example", "token"]).expect("opaque value name");
    let definition = recovered_standard_value_definition(
        &record,
        id,
        name.clone(),
        ValueTypeKind::Opaque,
        ValueTypePersistence::Transient,
        "std.example.token@1".to_owned(),
    )
    .expect("exact opaque definition");

    assert_eq!(definition.id(), id);
    assert_eq!(definition.name(), &name);
    assert_eq!(definition.kind(), ValueTypeKind::Opaque);
    assert_eq!(definition.persistence(), ValueTypePersistence::Transient);
    assert_eq!(definition.representation_contract(), "std.example.token@1");
    assert!(
        recovered_standard_value_definition(
            &record,
            id,
            name.clone(),
            ValueTypeKind::Opaque,
            ValueTypePersistence::Persistable,
            "std.example.token@1".to_owned(),
        )
        .is_err()
    );
    assert!(
        recovered_standard_value_definition(
            &record,
            id,
            name.clone(),
            ValueTypeKind::Opaque,
            ValueTypePersistence::Transient,
            String::new(),
        )
        .is_err()
    );
    assert!(
        recovered_standard_value_definition(
            &record,
            id,
            name.clone(),
            ValueTypeKind::Opaque,
            ValueTypePersistence::Transient,
            "x".repeat(129),
        )
        .is_err()
    );
    assert!(
        recovered_standard_value_definition(
            &record,
            id,
            name,
            ValueTypeKind::Opaque,
            ValueTypePersistence::Transient,
            "std.example.\ntoken@1".to_owned(),
        )
        .is_err()
    );
}

#[test]
fn assembles_record_value_definitions_fields_and_origins() {
    let catalogue = CatalogueRevisionId::from_bytes([0x92; 16]);
    let schema_id = SchemaId::from_bytes([0x93; 16]);
    let record_id = TypeId::from_bytes([0x94; 16]);
    let first_field = FieldId::from_bytes([0x95; 16]);
    let second_field = FieldId::from_bytes([0x96; 16]);
    let enum_id = TypeId::from_bytes([0x97; 16]);
    let schema_identity = DefinitionIdentity::Schema(schema_id);
    let record_identity = DefinitionIdentity::ValueType(record_id);
    let first_identity = DefinitionIdentity::Field {
        owner: record_id,
        field: first_field,
    };
    let second_identity = DefinitionIdentity::Field {
        owner: record_id,
        field: second_field,
    };
    let assembled = assemble_catalogue_semantics(
        catalogue,
        vec![RecoveredSchema {
            definition: SchemaDefinition::new(
                schema_id,
                QualifiedSemanticName::new(["app"]).expect("schema name"),
            ),
            origin: test_origin(schema_identity, 0),
        }],
        Vec::new(),
        vec![super::RecoveredEnumType {
            schema: schema_id,
            definition: EnumTypeDefinition::new(
                enum_id,
                QualifiedSemanticName::new(["app", "stage"]).expect("enum name"),
                ["open", "closed"],
            ),
            origin: test_origin(DefinitionIdentity::ValueType(enum_id), 1),
        }],
        vec![RecoveredRecordValueType {
            id: record_id,
            schema: schema_id,
            name: QualifiedSemanticName::new(["app", "status"]).expect("record name"),
            origin: test_origin(record_identity, 4),
        }],
        BTreeMap::new(),
        BTreeMap::from([(
            record_id,
            vec![
                RecoveredRecordValueField {
                    owner: record_id,
                    definition: RecordValueFieldDefinition::try_new_descriptor(
                        first_field,
                        "enabled",
                        0,
                        TypeDescriptor::named(TypeId::from_bytes([0x98; 16])),
                    )
                    .expect("record field"),
                    origin: test_origin(first_identity, 2),
                },
                RecoveredRecordValueField {
                    owner: record_id,
                    definition: RecordValueFieldDefinition::try_new_descriptor(
                        second_field,
                        "stage",
                        1,
                        TypeDescriptor::named(enum_id),
                    )
                    .expect("record field"),
                    origin: test_origin(second_identity, 3),
                },
            ],
        )]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("record catalogue semantics");

    let record = assembled
        .catalogue
        .record_value_type_by_id(record_id)
        .expect("recovered record");
    assert_eq!(record.fields().len(), 2);
    assert_eq!(record.fields()[0].id(), first_field);
    assert_eq!(record.fields()[1].id(), second_field);
    assert!(
        assembled
            .origins
            .iter()
            .any(|origin| origin.identity() == record_identity)
    );
    assert!(
        assembled
            .origins
            .iter()
            .any(|origin| origin.identity() == first_identity)
    );
    assert!(
        assembled
            .origins
            .iter()
            .any(|origin| origin.identity() == second_identity)
    );
}

#[test]
fn catalogue_hash_version_decoder_accepts_only_durable_versions() {
    let record = DurableRecord::new("_orna_kernel.catalogue_revisions", "test");

    assert_eq!(
        decode_catalogue_hash_version(1, &record).expect("version 1"),
        CatalogueHashVersion::Version1
    );
    assert_eq!(
        decode_catalogue_hash_version(2, &record).expect("version 2"),
        CatalogueHashVersion::Version2
    );
    assert!(decode_catalogue_hash_version(3, &record).is_err());
}

#[test]
fn function_links_accept_only_active_named_enums_and_object_references() {
    let enum_type = TypeId::from_bytes([0x81; 16]);
    let catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x82; 16]),
        vec![SchemaDefinition::new(
            orna_core::SchemaId::from_bytes([0x83; 16]),
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        Vec::new(),
        Vec::new(),
        vec![EnumTypeDefinition::new(
            enum_type,
            QualifiedSemanticName::new(["app", "stage"]).unwrap(),
            ["lead"],
        )],
        Vec::new(),
    )
    .unwrap();
    let record = DurableRecord::new(
        "_orna_kernel.catalogue_function_return_columns",
        "enum-link",
    );

    assert!(validate_function_type(&catalogue, ResolvedType::named(enum_type), &record).is_ok());
    assert!(
        validate_function_type(
            &catalogue,
            ResolvedType::named(TypeId::from_bytes([0x84; 16])),
            &record,
        )
        .is_err()
    );
    assert!(
        validate_function_type(&catalogue, ResolvedType::reference(enum_type), &record).is_err()
    );
    assert!(
        validate_function_type(
            &catalogue,
            ResolvedType::reference(SYS_INSPECT_INVOCATION_TYPE_ID),
            &record,
        )
        .is_ok()
    );
}

#[test]
fn legacy_resolved_type_tuple_decodes_a_scalar_field() {
    let record = DurableRecord::new("_orna_kernel.catalogue_fields", "test");
    let kind = decode_legacy_resolved_type_tuple_kind(
        Some("scalar"),
        &record,
        LegacyResolvedTypeTupleMember::Field,
    )
    .expect("scalar field kind");

    assert_eq!(
        decode_legacy_resolved_type_tuple(
            kind,
            Some("boolean"),
            None,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        )
        .expect("scalar field tuple"),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
}

#[test]
fn resolved_value_tuple_uses_the_recovered_standard_identity() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified retained standard snapshot");
    let value_type = standard
        .catalogue()
        .value_types()
        .first()
        .expect("retained standard value type")
        .id();
    let context = CatalogueHashContext::version_two(standard.clone());
    let record = DurableRecord::new("_orna_kernel.catalogue_fields", "value-tuple");

    for member in [
        LegacyResolvedTypeTupleMember::Field,
        LegacyResolvedTypeTupleMember::Parameter,
        LegacyResolvedTypeTupleMember::ReturnColumn,
        LegacyResolvedTypeTupleMember::SingleReturn,
        LegacyResolvedTypeTupleMember::StreamReturn,
    ] {
        let resolved_type = decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("value".to_owned()),
                scalar: None,
                target: None,
                value_type: Some(value_type),
                standard_library_revision: Some(standard.revision()),
                enum_type: None,
                record_type: None,
            },
            &context,
            &record,
            member,
        )
        .expect("value tuple");

        assert_eq!(resolved_type, ResolvedType::value(value_type));
    }
}

#[test]
fn resolved_enum_tuple_uses_only_the_application_enum_identity() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified retained standard snapshot");
    let context = CatalogueHashContext::version_two(standard);
    let enum_type = TypeId::from_bytes([0xa3; 16]);
    let record = DurableRecord::new("_orna_kernel.catalogue_fields", "enum-tuple");

    assert_eq!(
        decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("enum".to_owned()),
                scalar: None,
                target: None,
                value_type: None,
                standard_library_revision: None,
                enum_type: Some(enum_type),
                record_type: None,
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        )
        .expect("enum tuple"),
        ResolvedType::named(enum_type)
    );

    assert!(matches!(
        decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("enum".to_owned()),
                scalar: None,
                target: Some(enum_type),
                value_type: None,
                standard_library_revision: None,
                enum_type: Some(enum_type),
                record_type: None,
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_fields",
            record: failed_record,
            rule: "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple",
        }) if failed_record == "enum-tuple"
    ));
}

#[test]
fn standard_binding_target_tuple_is_exactly_value_or_enum() {
    let record = DurableRecord::new(
        "_orna_kernel.standard_catalogue_type_bindings",
        "binding-target",
    );
    let value = TypeId::from_bytes([0xb1; 16]);
    let enum_type = TypeId::from_bytes([0xb2; 16]);

    assert_eq!(
        decode_standard_binding_target("value", Some(value), None, &record)
            .expect("value binding target"),
        value,
    );
    assert_eq!(
        decode_standard_binding_target("enum", None, Some(enum_type), &record)
            .expect("enum binding target"),
        enum_type,
    );
    for (kind, value_target, enum_target) in [
        ("value", None, None),
        ("value", None, Some(enum_type)),
        ("value", Some(value), Some(enum_type)),
        ("enum", None, None),
        ("enum", Some(value), None),
        ("enum", Some(value), Some(enum_type)),
        ("unknown", Some(value), None),
    ] {
        assert!(matches!(
            decode_standard_binding_target(kind, value_target, enum_target, &record),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.standard_catalogue_type_bindings",
                record: failed_record,
                rule: "standard type binding target kind and identities must form one exact value or enum tuple",
            }) if failed_record == "binding-target"
        ));
    }
}

#[test]
fn standard_enum_record_tuple_checks_shape_pin_then_membership() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified retained standard snapshot");
    let context = CatalogueHashContext::version_two(standard.clone());
    let enum_type = TypeId::from_bytes([0xb3; 16]);
    let record = DurableRecord::new(
        "_orna_kernel.catalogue_record_value_fields",
        "standard-enum-tuple",
    );

    let malformed = decode_record_value_field_descriptor(
        RecordValueFieldTypeTuple {
            kind: Some("enum".to_owned()),
            value_type: None,
            value_standard_library_revision: None,
            application_enum_type: Some(enum_type),
            enum_standard_library_revision: Some(standard.revision()),
            standard_enum_type: Some(enum_type),
            application_record_type: None,
        },
        &context,
        &record,
    );
    assert!(matches!(
        malformed,
        Err(PostgresKernelError::DurableInvariant {
            rule: "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
            ..
        })
    ));

    for (revision, type_id) in [(Some(standard.revision()), None), (None, Some(enum_type))] {
        let partial = decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind: Some("enum".to_owned()),
                value_type: None,
                value_standard_library_revision: None,
                application_enum_type: None,
                enum_standard_library_revision: revision,
                standard_enum_type: type_id,
                application_record_type: None,
            },
            &context,
            &record,
        );
        assert!(matches!(
            partial,
            Err(PostgresKernelError::DurableInvariant {
                rule: "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
                ..
            })
        ));
    }

    let wrong_pin = decode_record_value_field_descriptor(
        RecordValueFieldTypeTuple {
            kind: Some("enum".to_owned()),
            value_type: None,
            value_standard_library_revision: None,
            application_enum_type: None,
            enum_standard_library_revision: Some(StandardLibraryRevisionId::from_bytes([0xb4; 16])),
            standard_enum_type: Some(enum_type),
            application_record_type: None,
        },
        &context,
        &record,
    );
    assert!(matches!(
        wrong_pin,
        Err(PostgresKernelError::DurableInvariant {
            rule: "record value field standard enum revision must equal the selected catalogue pin",
            ..
        })
    ));

    assert!(standard.catalogue().enum_type_by_id(enum_type).is_none());
    let missing = decode_record_value_field_descriptor(
        RecordValueFieldTypeTuple {
            kind: Some("enum".to_owned()),
            value_type: None,
            value_standard_library_revision: None,
            application_enum_type: None,
            enum_standard_library_revision: Some(standard.revision()),
            standard_enum_type: Some(enum_type),
            application_record_type: None,
        },
        &context,
        &record,
    );
    assert!(matches!(
        missing,
        Err(PostgresKernelError::DurableInvariant {
            rule: "record value field standard enum must identify one enum in the selected pinned standard library",
            ..
        })
    ));
}

#[test]
fn record_value_field_descriptor_rejects_partial_and_contaminated_record_tuples_exactly() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified retained standard snapshot");
    let context = CatalogueHashContext::version_two(standard);
    let record_type = TypeId::from_bytes([0xc1; 16]);
    let contamination = TypeId::from_bytes([0xc2; 16]);
    let record = DurableRecord::new("_orna_kernel.catalogue_record_value_fields", "record-tuple");
    let generic_rule = "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple";
    let widened_rule = "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple";

    assert_eq!(
        decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind: Some("record".to_owned()),
                value_type: None,
                value_standard_library_revision: None,
                application_enum_type: None,
                enum_standard_library_revision: None,
                standard_enum_type: None,
                application_record_type: Some(record_type),
            },
            &context,
            &record,
        )
        .expect("exact record tuple must decode"),
        TypeDescriptor::named(record_type),
    );

    for (kind, record_target) in [
        (Some("record".to_owned()), None),
        (None, Some(record_type)),
        (Some("value".to_owned()), Some(record_type)),
        (Some("enum".to_owned()), Some(record_type)),
    ] {
        let decoded = decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind,
                value_type: None,
                value_standard_library_revision: None,
                application_enum_type: None,
                enum_standard_library_revision: None,
                standard_enum_type: None,
                application_record_type: record_target,
            },
            &context,
            &record,
        );
        match decoded {
            Err(PostgresKernelError::DurableInvariant {
                relation,
                record,
                rule,
            }) => assert_eq!(
                (relation, record, rule),
                (
                    "_orna_kernel.catalogue_record_value_fields",
                    "record-tuple".to_owned(),
                    generic_rule,
                ),
                "unexpected partial record tuple error",
            ),
            other => panic!("unexpected partial record tuple result: {other:?}"),
        }
    }

    for (value_type, value_standard, app_enum) in [
        (Some(contamination), None, None),
        (
            None,
            Some(StandardLibraryRevisionId::from_bytes([0xc3; 16])),
            None,
        ),
        (None, None, Some(contamination)),
    ] {
        let decoded = decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind: Some("record".to_owned()),
                value_type,
                value_standard_library_revision: value_standard,
                application_enum_type: app_enum,
                enum_standard_library_revision: None,
                standard_enum_type: None,
                application_record_type: Some(record_type),
            },
            &context,
            &record,
        );
        match decoded {
            Err(PostgresKernelError::DurableInvariant {
                relation,
                record,
                rule,
            }) => assert_eq!(
                (relation, record, rule),
                (
                    "_orna_kernel.catalogue_record_value_fields",
                    "record-tuple".to_owned(),
                    generic_rule,
                ),
                "unexpected contaminated record tuple error",
            ),
            other => panic!("unexpected contaminated record tuple result: {other:?}"),
        }
    }

    for (enum_standard, std_enum) in [
        (
            Some(StandardLibraryRevisionId::from_bytes([0xc4; 16])),
            None,
        ),
        (None, Some(contamination)),
        (
            Some(StandardLibraryRevisionId::from_bytes([0xc4; 16])),
            Some(contamination),
        ),
    ] {
        let decoded = decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind: Some("record".to_owned()),
                value_type: None,
                value_standard_library_revision: None,
                application_enum_type: None,
                enum_standard_library_revision: enum_standard,
                standard_enum_type: std_enum,
                application_record_type: Some(record_type),
            },
            &context,
            &record,
        );
        match decoded {
            Err(PostgresKernelError::DurableInvariant {
                relation,
                record,
                rule,
            }) => assert_eq!(
                (relation, record, rule),
                (
                    "_orna_kernel.catalogue_record_value_fields",
                    "record-tuple".to_owned(),
                    widened_rule,
                ),
                "unexpected standard-enum provenance record tuple error",
            ),
            other => {
                panic!("unexpected standard-enum provenance record tuple result: {other:?}")
            }
        }
    }
}

#[test]
fn resolved_record_tuple_uses_only_the_application_record_identity() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified retained standard snapshot");
    let context = CatalogueHashContext::version_two(standard);
    let record_type = TypeId::from_bytes([0xa4; 16]);
    let record = DurableRecord::new("_orna_kernel.catalogue_fields", "record-tuple");

    assert_eq!(
        decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("record".to_owned()),
                scalar: None,
                target: None,
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: Some(record_type),
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        )
        .expect("record tuple"),
        ResolvedType::named(record_type),
    );
    assert!(
        decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("record".to_owned()),
                scalar: None,
                target: Some(record_type),
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: Some(record_type),
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        )
        .is_err()
    );
}

#[test]
fn resolved_value_tuple_checks_shape_then_pin_then_pinned_membership() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified retained standard snapshot");
    let value_type = standard
        .catalogue()
        .value_types()
        .first()
        .expect("retained standard value type")
        .id();
    let context = CatalogueHashContext::version_two(standard.clone());
    let record = DurableRecord::new("_orna_kernel.catalogue_fields", "value-tuple-order");

    let malformed = decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind: Some("value".to_owned()),
            scalar: Some("boolean".to_owned()),
            target: None,
            value_type: Some(value_type),
            standard_library_revision: None,
            enum_type: None,
            record_type: None,
        },
        &context,
        &record,
        LegacyResolvedTypeTupleMember::Field,
    );
    assert!(matches!(
        malformed,
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_fields",
            record: failed_record,
            rule: "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple",
        }) if failed_record == "value-tuple-order"
    ));

    let wrong_pin = StandardLibraryRevisionId::from_bytes([0xa4; 16]);
    assert_ne!(wrong_pin, standard.revision());
    let mismatched_pin = decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind: Some("value".to_owned()),
            scalar: None,
            target: None,
            value_type: Some(value_type),
            standard_library_revision: Some(wrong_pin),
            enum_type: None,
            record_type: None,
        },
        &context,
        &record,
        LegacyResolvedTypeTupleMember::Field,
    );
    assert!(matches!(
        mismatched_pin,
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_fields",
            record: failed_record,
            rule: "resolved value type standard library revision must equal the selected catalogue pin",
        }) if failed_record == "value-tuple-order"
    ));

    let missing_value_type = TypeId::from_bytes([0xa5; 16]);
    assert!(
        standard
            .catalogue()
            .value_type_by_id(missing_value_type)
            .is_none()
    );
    let missing_definition = decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind: Some("value".to_owned()),
            scalar: None,
            target: None,
            value_type: Some(missing_value_type),
            standard_library_revision: Some(standard.revision()),
            enum_type: None,
            record_type: None,
        },
        &context,
        &record,
        LegacyResolvedTypeTupleMember::Field,
    );
    assert!(matches!(
        missing_definition,
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_fields",
            record: failed_record,
            rule: "resolved value type must identify one value type in the selected pinned standard library",
        }) if failed_record == "value-tuple-order"
    ));
}

#[test]
fn version_two_legacy_resolved_type_tuples_keep_current_shapes() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified retained standard snapshot");
    let context = CatalogueHashContext::version_two(standard);
    let record = DurableRecord::new("_orna_kernel.catalogue_fields", "legacy-v2-tuple");
    let scalars = [
        ("boolean", StandardScalar::Boolean),
        ("integer", StandardScalar::Integer),
        ("bigint", StandardScalar::BigInt),
        ("float", StandardScalar::Float),
        ("decimal", StandardScalar::Decimal),
        (
            "character_large_object",
            StandardScalar::CharacterLargeObject,
        ),
        ("binary_large_object", StandardScalar::BinaryLargeObject),
        ("uuid", StandardScalar::Uuid),
        ("date", StandardScalar::Date),
        ("time", StandardScalar::Time),
        ("timestamp", StandardScalar::Timestamp),
        ("duration", StandardScalar::Duration),
        ("void", StandardScalar::Void),
    ];

    for (scalar, expected) in scalars {
        assert_eq!(
            decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("scalar".to_owned()),
                    scalar: Some(scalar.to_owned()),
                    target: None,
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: None,
                    record_type: None,
                },
                &context,
                &record,
                LegacyResolvedTypeTupleMember::Field,
            )
            .expect("transitional scalar tuple"),
            ResolvedType::scalar(expected)
        );
    }

    let target = TypeId::from_bytes([0xa6; 16]);
    assert_eq!(
        decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("named".to_owned()),
                scalar: None,
                target: Some(target),
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Parameter,
        )
        .expect("transitional named tuple"),
        ResolvedType::named(target)
    );
    assert_eq!(
        decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("reference".to_owned()),
                scalar: None,
                target: Some(target),
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        )
        .expect("transitional reference tuple"),
        ResolvedType::reference(target)
    );
}

#[test]
fn legacy_resolved_type_tuple_matrix_preserves_current_shapes_and_errors() {
    let record = DurableRecord::new("_orna_kernel.catalogue_fields", "tuple");
    let target = TypeId::from_bytes([0x91; 16]);
    let scalars = [
        ("boolean", StandardScalar::Boolean),
        ("integer", StandardScalar::Integer),
        ("bigint", StandardScalar::BigInt),
        ("float", StandardScalar::Float),
        ("decimal", StandardScalar::Decimal),
        (
            "character_large_object",
            StandardScalar::CharacterLargeObject,
        ),
        ("binary_large_object", StandardScalar::BinaryLargeObject),
        ("uuid", StandardScalar::Uuid),
        ("date", StandardScalar::Date),
        ("time", StandardScalar::Time),
        ("timestamp", StandardScalar::Timestamp),
        ("duration", StandardScalar::Duration),
        ("void", StandardScalar::Void),
    ];

    for member in [
        LegacyResolvedTypeTupleMember::Field,
        LegacyResolvedTypeTupleMember::Parameter,
        LegacyResolvedTypeTupleMember::ReturnColumn,
        LegacyResolvedTypeTupleMember::SingleReturn,
        LegacyResolvedTypeTupleMember::StreamReturn,
    ] {
        let scalar_kind = decode_legacy_resolved_type_tuple_kind(Some("scalar"), &record, member)
            .expect("scalar kind");
        for (name, scalar) in scalars {
            let decoded =
                decode_legacy_resolved_type_tuple(scalar_kind, Some(name), None, &record, member);
            if scalar == StandardScalar::Void
                && member != LegacyResolvedTypeTupleMember::Field
                && member != LegacyResolvedTypeTupleMember::SingleReturn
            {
                assert!(matches!(
                    decoded,
                    Err(PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.catalogue_fields",
                        record: failed_record,
                        rule: "void is valid only as a SINGLE function return, never as a parameter or ROWS column",
                    }) if failed_record == "tuple"
                ));
            } else {
                assert_eq!(
                    decoded.expect("current scalar tuple"),
                    ResolvedType::scalar(scalar)
                );
            }
        }

        let named_kind = decode_legacy_resolved_type_tuple_kind(Some("named"), &record, member)
            .expect("named kind");
        let named =
            decode_legacy_resolved_type_tuple(named_kind, None, Some(target), &record, member);
        if member == LegacyResolvedTypeTupleMember::Field {
            assert!(matches!(
                named,
                Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.catalogue_fields",
                    record: failed_record,
                    rule: "named field types are not supported by active recovery",
                }) if failed_record == "tuple"
            ));
        } else {
            assert_eq!(
                named.expect("current named tuple"),
                ResolvedType::named(target)
            );
        }

        let reference_kind =
            decode_legacy_resolved_type_tuple_kind(Some("reference"), &record, member)
                .expect("reference kind");
        assert_eq!(
            decode_legacy_resolved_type_tuple(reference_kind, None, Some(target), &record, member,)
                .expect("current reference tuple"),
            ResolvedType::reference(target)
        );
    }

    let parameter_scalar = decode_legacy_resolved_type_tuple_kind(
        Some("scalar"),
        &record,
        LegacyResolvedTypeTupleMember::Parameter,
    )
    .expect("parameter scalar kind");
    for (scalar, target) in [(None, None), (Some("boolean"), Some(target))] {
        assert!(matches!(
            decode_legacy_resolved_type_tuple(
                parameter_scalar,
                scalar,
                target,
                &record,
                LegacyResolvedTypeTupleMember::Parameter,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "parameter type columns must form one exact resolved type tuple",
            }) if failed_record == "tuple"
        ));
    }
    assert!(matches!(
        decode_legacy_resolved_type_tuple_kind(
            None,
            &record,
            LegacyResolvedTypeTupleMember::ReturnColumn,
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_fields",
            record: failed_record,
            rule: "return column type columns must form one exact resolved type tuple",
        }) if failed_record == "tuple"
    ));

    for kind_name in ["named", "reference"] {
        let kind = decode_legacy_resolved_type_tuple_kind(
            Some(kind_name),
            &record,
            LegacyResolvedTypeTupleMember::Parameter,
        )
        .expect("current parameter kind");
        for (scalar, target) in [(None, None), (Some("boolean"), Some(target))] {
            assert!(matches!(
                decode_legacy_resolved_type_tuple(
                    kind,
                    scalar,
                    target,
                    &record,
                    LegacyResolvedTypeTupleMember::Parameter,
                ),
                Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.catalogue_fields",
                    record: failed_record,
                    rule: "parameter type columns must form one exact resolved type tuple",
                }) if failed_record == "tuple"
            ));
        }
    }
    assert!(matches!(
        decode_legacy_resolved_type_tuple(
            parameter_scalar,
            Some("BOOLEAN"),
            None,
            &record,
            LegacyResolvedTypeTupleMember::Parameter,
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_fields",
            record: failed_record,
            rule: "resolved scalar type must be an exact standard scalar name",
        }) if failed_record == "tuple"
    ));
    assert!(matches!(
        decode_legacy_resolved_type_tuple_kind(
            Some("value"),
            &record,
            LegacyResolvedTypeTupleMember::Field,
        ),
        Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_fields",
            record: failed_record,
            rule: "field type kind must be scalar, named, or reference",
        }) if failed_record == "tuple"
    ));
}

#[test]
fn assembles_the_exact_empty_semantic_revision() {
    let bundle = SourceBundleId::from_bytes([1; 16]);
    let source = SourceRevisionId::from_bytes([2; 16]);
    let catalogue = CatalogueRevisionId::from_bytes([3; 16]);
    let bundle_hash = source_bundle_digest(&[]).expect("empty source bundle hash");
    let source_hash = source_revision_record_digest(bundle, None, bundle_hash)
        .expect("empty source revision hash");
    let empty_catalogue =
        CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");
    let catalogue_hash =
        catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).expect("empty catalogue hash");

    let recovered = assemble_revision(
        RecoveredRevisionHeader {
            bundle,
            source,
            source_parent: None,
            catalogue,
            bundle_hash,
            source_hash,
            catalogue_hash,
            catalogue_hash_version: CatalogueHashVersion::Version1,
            standard_library_revision: None,
        },
        Vec::new(),
        RecoveredCatalogueSemantics {
            catalogue: empty_catalogue,
            expressions: Vec::new(),
            origins: Vec::new(),
        },
        RecoveredFunctionState::empty(),
        orna_core::revision::CatalogueHashContext::version_one(),
    )
    .expect("exact empty revision");

    assert_eq!(recovered.pair().source(), source);
    assert_eq!(recovered.pair().catalogue(), catalogue);
    assert!(recovered.source().units().is_empty());
    assert!(recovered.catalogue().schemas().is_empty());
    assert!(recovered.catalogue().object_types().is_empty());
    assert!(recovered.catalogue().functions().is_empty());
    assert!(recovered.function_revisions().is_empty());
    assert!(recovered.historical_function_revisions().is_empty());
}

#[test]
fn rejects_an_empty_catalogue_with_a_different_digest() {
    let bundle = SourceBundleId::from_bytes([4; 16]);
    let source = SourceRevisionId::from_bytes([5; 16]);
    let catalogue = CatalogueRevisionId::from_bytes([6; 16]);
    let bundle_hash = source_bundle_digest(&[]).expect("empty source bundle hash");
    let source_hash = source_revision_record_digest(bundle, None, bundle_hash)
        .expect("empty source revision hash");
    let empty_catalogue =
        CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");

    assert!(
        assemble_revision(
            RecoveredRevisionHeader {
                bundle,
                source,
                source_parent: None,
                catalogue,
                bundle_hash,
                source_hash,
                catalogue_hash: bundle_hash,
                catalogue_hash_version: CatalogueHashVersion::Version1,
                standard_library_revision: None,
            },
            Vec::new(),
            RecoveredCatalogueSemantics {
                catalogue: empty_catalogue,
                expressions: Vec::new(),
                origins: Vec::new(),
            },
            RecoveredFunctionState::empty(),
            orna_core::revision::CatalogueHashContext::version_one(),
        )
        .is_err()
    );
}

#[test]
fn assembles_an_empty_semantic_revision_with_exact_source_content() {
    let bundle = SourceBundleId::from_bytes([7; 16]);
    let source = SourceRevisionId::from_bytes([8; 16]);
    let catalogue = CatalogueRevisionId::from_bytes([9; 16]);
    let content = "schema app";
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([10; 16]),
        0,
        "schema.orna",
        content,
        source_unit_content_digest(content).expect("source content hash"),
    )
    .expect("stored source unit");
    let units = vec![unit];
    let bundle_hash = source_bundle_digest(&units).expect("source bundle hash");
    let source_hash =
        source_revision_record_digest(bundle, None, bundle_hash).expect("source revision hash");
    let empty_catalogue =
        CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");
    let catalogue_hash =
        catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).expect("empty catalogue hash");

    let recovered = assemble_revision(
        RecoveredRevisionHeader {
            bundle,
            source,
            source_parent: None,
            catalogue,
            bundle_hash,
            source_hash,
            catalogue_hash,
            catalogue_hash_version: CatalogueHashVersion::Version1,
            standard_library_revision: None,
        },
        units,
        RecoveredCatalogueSemantics {
            catalogue: empty_catalogue,
            expressions: Vec::new(),
            origins: Vec::new(),
        },
        RecoveredFunctionState::empty(),
        orna_core::revision::CatalogueHashContext::version_one(),
    )
    .expect("empty semantic revision with source");

    assert_eq!(recovered.source().units().len(), 1);
    assert_eq!(recovered.source().units()[0].content(), content);
}
