//! Record value graph and revision admission tests.

use super::*;
fn record_graph_standard() -> CatalogueSnapshot {
    CatalogueSnapshot::new(CatalogueRevisionId::from_bytes(id::<150>()), vec![], vec![]).unwrap()
}

fn record_graph_standard_with_boolean() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<151>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<152>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![ValueTypeDefinition::primitive(
            TypeId::from_bytes(id::<71>()),
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )],
        vec![],
    )
    .unwrap()
}

fn record_graph_schema() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["crm"]).unwrap(),
    )
}

fn record_graph_type(
    record_byte: u8,
    name: &str,
    fields: Vec<(u8, u32, TypeId)>,
) -> RecordValueTypeDefinition {
    let fields = fields
        .into_iter()
        .map(|(field_byte, ordinal, target)| {
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([field_byte; 16]),
                format!("edge_{ordinal}"),
                ordinal,
                TypeDescriptor::named(target),
            )
            .unwrap()
        })
        .collect();
    RecordValueTypeDefinition::new(
        TypeId::from_bytes([record_byte; 16]),
        QualifiedSemanticName::new(["crm", name]).unwrap(),
        fields,
    )
}

fn record_graph_catalogue(records: Vec<RecordValueTypeDefinition>) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<152>()),
        vec![record_graph_schema()],
        vec![],
        vec![],
        vec![],
        records,
        vec![],
    )
    .unwrap()
}

fn record_graph_type_with_id(
    id: [u8; 16],
    name: &str,
    fields: Vec<([u8; 16], u32, TypeId)>,
) -> RecordValueTypeDefinition {
    let fields = fields
        .into_iter()
        .map(|(field_id, ordinal, target)| {
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes(field_id),
                format!("edge_{ordinal}"),
                ordinal,
                TypeDescriptor::named(target),
            )
            .unwrap()
        })
        .collect();
    RecordValueTypeDefinition::new(
        TypeId::from_bytes(id),
        QualifiedSemanticName::new(["crm", name]).unwrap(),
        fields,
    )
}

/// A deterministic record identity for one chain index beyond one byte.
fn index_type_id(index: u32) -> [u8; 16] {
    let value = index + 0x1000;
    let mut bytes = [0; 16];
    bytes[0] = (value >> 8) as u8;
    bytes[1] = value as u8;
    bytes
}

/// A deterministic field identity for one chain index beyond one byte.
fn index_field_id(index: u32) -> [u8; 16] {
    let mut bytes = index_type_id(index);
    bytes[15] = 0x01;
    bytes
}

fn record_graph_origins_for(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let source_unit = SourceUnitId::from_bytes(id::<3>());
    let mut identities = vec![DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>()))];
    for record in catalogue.record_value_types() {
        identities.push(DefinitionIdentity::ValueType(record.id()));
        for field in record.fields() {
            identities.push(DefinitionIdentity::Field {
                owner: record.id(),
                field: field.id(),
            });
        }
    }
    identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect()
}

fn validate_record_graph(
    records: Vec<RecordValueTypeDefinition>,
) -> Result<(), RecordValueFieldDescriptorValidationError> {
    validate_record_value_field_descriptors(
        &record_graph_catalogue(records),
        &record_graph_standard_with_boolean(),
    )
}

#[test]
fn record_value_field_application_record_provenance_and_active_projection() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes(id::<201>()))],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<71>()))],
    );
    let catalogue = record_graph_catalogue(vec![record_a.clone(), record_b.clone()]);

    assert_eq!(
        classify_record_value_field_descriptor(
            &catalogue,
            &record_graph_standard(),
            &TypeDescriptor::named(TypeId::from_bytes(id::<200>())),
        ),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<200>())
        ))
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &catalogue,
            &record_graph_standard(),
            &TypeDescriptor::named(TypeId::from_bytes(id::<201>())),
        ),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<201>())
        ))
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &catalogue,
            &record_graph_standard(),
            &TypeDescriptor::reference(TypeId::from_bytes(id::<200>())),
        ),
        Err(RecordValueFieldDescriptorClassificationError::Unsupported)
    );

    let active = active_for_flat_type_conversion(catalogue, standard_context());
    assert_eq!(
        active.record_value_field_descriptor_runtime_type(&TypeDescriptor::named(
            TypeId::from_bytes(id::<200>())
        )),
        Some(ResolvedType::named(TypeId::from_bytes(id::<200>())))
    );
    assert_eq!(
        active.record_value_field_descriptor_runtime_type(&TypeDescriptor::named(
            TypeId::from_bytes(id::<71>())
        )),
        Some(ResolvedType::scalar(StandardScalar::Boolean))
    );
    assert_eq!(
        active.record_value_field_descriptor_runtime_type(&TypeDescriptor::reference(
            TypeId::from_bytes(id::<200>())
        )),
        None
    );
    assert_eq!(validate_record_graph(vec![record_a, record_b]), Ok(()));
}

#[test]
fn record_value_field_self_cycle_is_rejected_exactly() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes(id::<200>()))],
    );
    assert_eq!(
        validate_record_graph(vec![record_a]).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<200>()),
            field: FieldId::from_bytes(id::<210>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_three_cycle_closes_at_the_exact_back_edge() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes(id::<201>()))],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<202>()))],
    );
    let record_c = record_graph_type(
        202,
        "record_c",
        vec![(212, 0, TypeId::from_bytes(id::<200>()))],
    );
    assert_eq!(
        validate_record_graph(vec![record_a, record_b, record_c]).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<202>()),
            field: FieldId::from_bytes(id::<212>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_cycle_selection_is_deterministic_across_orders() {
    // The A-B-C-A cycle must report the same closing edge when the record
    // input order is reversed.
    let forward = vec![
        record_graph_type(
            200,
            "record_a",
            vec![(210, 0, TypeId::from_bytes(id::<201>()))],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<202>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<200>()))],
        ),
    ];
    let reversed = forward.iter().rev().cloned().collect::<Vec<_>>();
    let expected = RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
        record_value_type: TypeId::from_bytes(id::<202>()),
        field: FieldId::from_bytes(id::<212>()),
        nested_record_value_type: TypeId::from_bytes(id::<200>()),
    };
    assert_eq!(validate_record_graph(forward).unwrap_err(), expected);
    assert_eq!(validate_record_graph(reversed).unwrap_err(), expected);

    // The closing edge is selected by ordinal, not by input field order.
    let first_field_first = vec![
        record_graph_type(
            200,
            "record_a",
            vec![
                (210, 0, TypeId::from_bytes(id::<201>())),
                (214, 1, TypeId::from_bytes(id::<202>())),
            ],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<200>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<71>()))],
        ),
    ];
    assert_eq!(
        validate_record_graph(first_field_first).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<201>()),
            field: FieldId::from_bytes(id::<211>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
    let second_field_first = vec![
        record_graph_type(
            200,
            "record_a",
            vec![
                (214, 0, TypeId::from_bytes(id::<201>())),
                (210, 1, TypeId::from_bytes(id::<202>())),
            ],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<200>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<71>()))],
        ),
    ];
    assert_eq!(
        validate_record_graph(second_field_first).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<201>()),
            field: FieldId::from_bytes(id::<211>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_diamond_dag_is_accepted() {
    let records = vec![
        record_graph_type(
            200,
            "record_a",
            vec![
                (210, 0, TypeId::from_bytes(id::<201>())),
                (214, 1, TypeId::from_bytes(id::<202>())),
            ],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<203>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<203>()))],
        ),
        record_graph_type(
            203,
            "record_d",
            vec![(213, 0, TypeId::from_bytes(id::<71>()))],
        ),
    ];
    assert_eq!(validate_record_graph(records), Ok(()));
}

#[test]
fn record_value_field_classification_errors_precede_cycles() {
    // The application catalogue also defines a record at the standard
    // boolean identity, so record_a's first field is ambiguous, while the
    // record_a -> record_b -> record_a cycle exists. Classification runs
    // before cycle detection, so the ambiguous field must win.
    let colliding = record_graph_type(
        71,
        "record_71",
        vec![(215, 0, TypeId::from_bytes(id::<201>()))],
    );
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![
            (210, 0, TypeId::from_bytes(id::<71>())),
            (214, 1, TypeId::from_bytes(id::<201>())),
        ],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<200>()))],
    );
    let catalogue = record_graph_catalogue(vec![colliding, record_a, record_b]);
    let error =
        validate_record_value_field_descriptors(&catalogue, &record_graph_standard_with_boolean())
            .unwrap_err();
    assert_eq!(
        error,
        RecordValueFieldDescriptorValidationError::Ambiguous {
            record_value_type: TypeId::from_bytes(id::<200>()),
            field: FieldId::from_bytes(id::<210>()),
            type_id: TypeId::from_bytes(id::<71>()),
        }
    );
}

#[test]
fn record_value_field_cycles_precede_depth() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![
            (210, 0, TypeId::from_bytes(id::<201>())),
            (214, 1, TypeId::from_bytes(id::<202>())),
        ],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<200>()))],
    );
    let chain = (0..40)
        .map(|index| {
            let record_byte = 202 + index as u8;
            let next_byte = if index == 39 {
                71
            } else {
                202 + index as u8 + 1
            };
            record_graph_type(
                record_byte,
                &format!("chain_{index}"),
                vec![(250, 0, TypeId::from_bytes([next_byte; 16]))],
            )
        })
        .collect::<Vec<_>>();
    let mut records = vec![record_a, record_b];
    records.extend(chain);
    assert_eq!(
        validate_record_graph(records).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<201>()),
            field: FieldId::from_bytes(id::<211>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_nesting_accepts_32_edges_and_rejects_33_exactly() {
    let accepted = (0..33)
        .map(|index| {
            let next_byte = if index == 32 {
                71
            } else {
                200 + index as u8 + 1
            };
            record_graph_type(
                200 + index as u8,
                &format!("chain_{index}"),
                vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(validate_record_graph(accepted), Ok(()));

    let too_deep = (0..34)
        .map(|index| {
            let next_byte = if index == 33 {
                71
            } else {
                200 + index as u8 + 1
            };
            record_graph_type(
                200 + index as u8,
                &format!("chain_{index}"),
                vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validate_record_graph(too_deep).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes(id::<232>()),
            field: FieldId::from_bytes(id::<242>()),
            nested_record_value_type: TypeId::from_bytes(id::<233>()),
            maximum: 32,
            actual: 33,
        }
    );
}

#[test]
fn record_value_field_long_acyclic_chain_returns_the_exact_depth_error_without_crashing() {
    let chain = (0..4096)
        .map(|index| {
            let next = if index == 4095 {
                TypeId::from_bytes([71; 16])
            } else {
                TypeId::from_bytes(index_type_id(index + 1))
            };
            record_graph_type_with_id(
                index_type_id(index),
                &format!("chain_{index}"),
                vec![(index_field_id(index), 0, next)],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validate_record_graph(chain).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes(index_type_id(32)),
            field: FieldId::from_bytes(index_field_id(32)),
            nested_record_value_type: TypeId::from_bytes(index_type_id(33)),
            maximum: 32,
            actual: 33,
        }
    );
}

#[test]
fn record_value_field_shared_suffix_memoisation_still_fails_on_a_later_deep_root() {
    let shallow_root = record_graph_type(
        2,
        "shallow_root",
        vec![(212, 0, TypeId::from_bytes([3; 16]))],
    );
    let suffix_leaf = record_graph_type(
        3,
        "suffix_leaf",
        vec![(213, 0, TypeId::from_bytes([71; 16]))],
    );
    let deep_root = record_graph_type(4, "deep_root", vec![(214, 0, TypeId::from_bytes([5; 16]))]);
    let deep_chain = (0..32)
        .map(|index| {
            let next = if index == 31 {
                TypeId::from_bytes([3; 16])
            } else {
                TypeId::from_bytes([5 + index as u8 + 1; 16])
            };
            record_graph_type(
                5 + index as u8,
                &format!("deep_{index}"),
                vec![(215 + index as u8, 0, next)],
            )
        })
        .collect::<Vec<_>>();
    let mut records = vec![shallow_root, suffix_leaf, deep_root];
    records.extend(deep_chain);
    assert_eq!(
        validate_record_graph(records).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes([36; 16]),
            field: FieldId::from_bytes([246; 16]),
            nested_record_value_type: TypeId::from_bytes([3; 16]),
            maximum: 32,
            actual: 33,
        }
    );
}

fn deployable_with(
    catalogue: CatalogueSnapshot,
    context: CatalogueHashContext,
) -> Result<DeployableRevision, RevisionInvariantError> {
    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<78>()),
        CatalogueRevisionId::from_bytes(id::<79>()),
    );
    DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue.clone(),
            digest::<7>(),
            DeployableRevisionContent::new(
                record_graph_origins_for(&catalogue),
                vec![],
                vec![],
                vec![],
            )
            .with_current_function_revisions(vec![]),
        ),
        context,
    )
}

#[test]
fn deployable_classification_returns_application_record_for_nested_targets() {
    let catalogue = record_graph_catalogue(vec![
        record_graph_type(
            200,
            "outer",
            vec![(210, 0, TypeId::from_bytes(id::<201>()))],
        ),
        record_graph_type(201, "inner", vec![(211, 0, TypeId::from_bytes(id::<71>()))]),
    ]);
    let deployable =
        deployable_with(catalogue, standard_context()).expect("nested record catalogue must admit");
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<200>()
        ))),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<200>())
        ))
    );
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<201>()
        ))),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<201>())
        ))
    );
}

#[test]
fn revision_admission_maps_cycles_and_depth_to_exact_errors() {
    let cyclic = record_graph_catalogue(vec![record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes([200; 16]))],
    )]);
    let expected_cycle = RevisionInvariantError::RecursiveRecordValueField {
        record_value_type: TypeId::from_bytes([200; 16]),
        field: FieldId::from_bytes([210; 16]),
        nested_record_value_type: TypeId::from_bytes([200; 16]),
    };
    let deployable_error = deployable_with(cyclic.clone(), standard_context())
        .expect_err("a self cycle must fail deployable admission");
    assert_eq!(deployable_error, expected_cycle);
    assert_eq!(
        deployable_error.to_string(),
        "record value fields must not form a recursive cycle"
    );

    let source = source(None);
    let pair = RevisionPair::new(source.id(), cyclic.revision());
    let active_origins = record_graph_origins_for(&cyclic);
    let active_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            source,
            cyclic,
            digest::<7>(),
            ActiveRevisionContent::new(vec![], vec![], active_origins, vec![]),
        ),
        standard_context(),
    )
    .expect_err("a self cycle must fail active admission");
    assert_eq!(active_error, expected_cycle);

    let too_deep = record_graph_catalogue(
        (0..34)
            .map(|index| {
                let next_byte = if index == 33 {
                    71
                } else {
                    200 + index as u8 + 1
                };
                record_graph_type(
                    200 + index as u8,
                    &format!("chain_{index}"),
                    vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
                )
            })
            .collect(),
    );
    let expected_depth = RevisionInvariantError::RecordValueNestingTooDeep {
        record_value_type: TypeId::from_bytes([232; 16]),
        field: FieldId::from_bytes([242; 16]),
        nested_record_value_type: TypeId::from_bytes([233; 16]),
        maximum: 32,
        actual: 33,
    };
    let deployable_depth = deployable_with(too_deep.clone(), standard_context())
        .expect_err("a depth-33 chain must fail deployable admission");
    assert_eq!(deployable_depth, expected_depth);
    assert_eq!(
        deployable_depth.to_string(),
        "record value nesting exceeds the maximum depth"
    );
}

#[test]
fn application_record_colliding_with_a_standard_enum_is_ambiguous() {
    let candidate = record_graph_catalogue(vec![
        record_graph_type(
            75,
            "colliding",
            vec![(210, 0, TypeId::from_bytes([200; 16]))],
        ),
        record_graph_type(200, "leaf", vec![(211, 0, TypeId::from_bytes([71; 16]))]),
    ]);
    let deployable = deployable_with(candidate, flat_type_standard_context())
        .expect("colliding record catalogue must admit");
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            [75; 16]
        ))),
        Err(RecordValueFieldDescriptorError::Ambiguous {
            type_id: TypeId::from_bytes([75; 16]),
        })
    );

    let with_field = record_graph_catalogue(vec![
        record_graph_type(
            75,
            "colliding",
            vec![(211, 0, TypeId::from_bytes([71; 16]))],
        ),
        record_graph_type(200, "user", vec![(210, 0, TypeId::from_bytes([75; 16]))]),
    ]);
    let expected = RevisionInvariantError::AmbiguousRecordValueFieldType {
        record_value_type: TypeId::from_bytes([200; 16]),
        field: FieldId::from_bytes([210; 16]),
        type_id: TypeId::from_bytes([75; 16]),
    };
    let error = deployable_with(with_field, flat_type_standard_context())
        .expect_err("an ambiguous record field must fail admission");
    assert_eq!(error, expected);
    assert_eq!(
        error.to_string(),
        "record field type is present in both application and standard catalogues"
    );
}

fn catalogue_with_record_value_slot() -> CatalogueSnapshot {
    let record_value_type = TypeId::from_bytes(id::<76>());
    CatalogueSnapshot::new_with_functions_and_record_value_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<80>()),
            QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
            vec![FieldDefinition::new(
                FieldId::from_bytes(id::<81>()),
                "status",
                0,
                ResolvedType::named(record_value_type),
                false,
                false,
                None,
                None,
            )],
        )],
        vec![],
        vec![],
        vec![RecordValueTypeDefinition::new(
            record_value_type,
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<77>()),
                    "active",
                    0,
                    TypeDescriptor::named(TypeId::from_bytes(id::<71>())),
                )
                .unwrap(),
            ],
        )],
        vec![],
        vec![FunctionDefinition::new(
            FunctionId::from_bytes(id::<82>()),
            QualifiedSemanticName::new(["crm", "read_status"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "status",
                0,
                ResolvedType::named(record_value_type),
            )]),
            FunctionRevisionId::from_bytes(id::<83>()),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap()
}

fn record_value_type_origins() -> Vec<DefinitionOrigin> {
    let source = SourceUnitId::from_bytes(id::<3>());
    vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
            SourceOrigin::new(source, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<76>())),
            SourceOrigin::new(source, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<76>()),
                field: FieldId::from_bytes(id::<77>()),
            },
            SourceOrigin::new(source, 2, 3).unwrap(),
        ),
    ]
}

#[test]
fn record_value_types_require_version_two_at_every_revision_admission_boundary() {
    let record_value_type = TypeId::from_bytes(id::<76>());
    let expected = RevisionInvariantError::RecordValueTypeRequiresCatalogueHashVersionTwo {
        record_value_type,
    };

    assert_eq!(
        validate_catalogue_hash_context_coherence(
            &CatalogueHashContext::version_one(),
            &record_value_type_catalogue(),
            &[],
            &[],
            &[],
        ),
        Err(expected.clone())
    );
    assert!(
        validate_catalogue_hash_context_coherence(
            &standard_context(),
            &record_value_type_catalogue(),
            &[],
            &[],
            &[],
        )
        .is_ok()
    );
    let version_two_source = source(None);
    let version_two_catalogue = record_value_type_catalogue();
    let version_two_pair =
        RevisionPair::new(version_two_source.id(), version_two_catalogue.revision());
    assert!(
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                version_two_pair,
                version_two_source,
                version_two_catalogue,
                digest::<7>(),
                ActiveRevisionContent::new(vec![], vec![], record_value_type_origins(), vec![]),
            ),
            standard_context(),
        )
        .is_ok()
    );

    let active_source = source(None);
    let catalogue = record_value_type_catalogue();
    let pair = RevisionPair::new(active_source.id(), catalogue.revision());
    assert_eq!(
        ActiveDatabaseRevision::new(
            pair,
            active_source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err(),
        expected.clone()
    );

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<78>()),
        CatalogueRevisionId::from_bytes(id::<79>()),
    );
    let deployable = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            record_value_type_catalogue(),
            digest::<7>(),
            DeployableRevisionContent::new(record_value_type_origins(), vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        standard_context(),
    )
    .unwrap();
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<71>()
        ),)),
        Ok(RecordValueFieldDescriptorClass::StandardPrimitive(
            TypeId::from_bytes(id::<71>()),
        ))
    );
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::reference(
            TypeId::from_bytes(id::<80>()),
        )),
        Err(RecordValueFieldDescriptorError::Unsupported)
    );
    let version_one = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        empty_catalogue(),
        digest::<7>(),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    assert_eq!(
        version_one.record_value_field_descriptor_class(&TypeDescriptor::named(
            TypeId::from_bytes(id::<71>()),
        )),
        Err(RecordValueFieldDescriptorError::StandardLibraryUnavailable)
    );
    let collision = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            enum_type_catalogue(),
            digest::<7>(),
            DeployableRevisionContent::new(value_type_origins(), vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        standard_context(),
    )
    .unwrap();
    assert_eq!(
        collision.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<71>()
        ),)),
        Err(RecordValueFieldDescriptorError::Ambiguous {
            type_id: TypeId::from_bytes(id::<71>()),
        })
    );
    let standard_enum = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            empty_catalogue(),
            digest::<7>(),
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        flat_type_standard_context(),
    )
    .unwrap();
    assert_eq!(
        standard_enum.record_value_field_descriptor_class(&TypeDescriptor::named(
            TypeId::from_bytes(id::<75>()),
        )),
        Ok(RecordValueFieldDescriptorClass::StandardEnum(
            TypeId::from_bytes(id::<75>()),
        ))
    );
    assert_eq!(
        DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            record_value_type_catalogue(),
            digest::<7>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err(),
        expected.clone()
    );

    let standard_revision = StandardLibraryRevisionId::from_bytes(id::<74>());
    assert_eq!(
        StandardLibrarySnapshot::new(
            standard_revision,
            StandardLibraryDigestVersion::Version1,
            source(None),
            "orna.language/1",
            record_value_type_catalogue(),
            vec![],
            digest::<75>(),
        )
        .unwrap_err(),
        RevisionInvariantError::UnsupportedStandardLibraryDefinition {
            revision: standard_revision,
        }
    );
    assert_eq!(
        expected.to_string(),
        "record value types require catalogue hash version 2"
    );
    let identities = expected_definition_identities(&record_value_type_catalogue(), &[]);
    assert!(identities.contains(&DefinitionIdentity::ValueType(record_value_type)));
    assert!(identities.contains(&DefinitionIdentity::Field {
        owner: record_value_type,
        field: FieldId::from_bytes(id::<77>()),
    }));
}

#[test]
fn record_value_types_can_enter_object_and_rows_slots() {
    assert!(
        validate_resolved_type_slots(&standard_context(), &catalogue_with_record_value_slot(),)
            .is_ok()
    );
}

#[test]
fn record_value_field_type_policy_is_closed_and_uses_pinned_standard_primitives() {
    let accepted_contracts = [
        ("orna.kernel.value.boolean@1", StandardScalar::Boolean),
        ("orna.kernel.value.integer@1", StandardScalar::Integer),
        ("orna.kernel.value.bigint@1", StandardScalar::BigInt),
        ("orna.kernel.value.float@1", StandardScalar::Float),
        (
            "orna.kernel.value.character-large-object@1",
            StandardScalar::CharacterLargeObject,
        ),
        (
            "orna.kernel.value.binary-large-object@1",
            StandardScalar::BinaryLargeObject,
        ),
    ];
    let accepted_values = accepted_contracts
        .iter()
        .enumerate()
        .map(|(index, (contract, _))| {
            ValueTypeDefinition::primitive(
                TypeId::from_bytes([index as u8 + 1; 16]),
                QualifiedSemanticName::new(["std", "types", *contract]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                *contract,
            )
        })
        .collect::<Vec<_>>();
    let standard = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<90>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<91>()),
            QualifiedSemanticName::new(["std", "types"]).unwrap(),
        )],
        vec![],
        accepted_values,
        vec![],
    )
    .unwrap();
    let application = empty_catalogue();
    for (index, (_, scalar)) in accepted_contracts.iter().enumerate() {
        let resolved_type = ResolvedType::value(TypeId::from_bytes([index as u8 + 1; 16]));
        assert!(record_value_field_runtime_type(&application, &standard, resolved_type).is_some());
        assert_eq!(
            record_value_field_runtime_type(&application, &standard, resolved_type),
            Some(ResolvedType::scalar(*scalar))
        );
    }

    for contract in [
        "orna.kernel.value.decimal@1",
        "orna.kernel.value.uuid@1",
        "orna.kernel.value.date@1",
        "orna.kernel.value.time@1",
        "orna.kernel.value.timestamp@1",
        "orna.kernel.value.duration@1",
        "orna.kernel.value.void@1",
        "orna.kernel.value.custom@1",
    ] {
        assert!(
            accepted_record_scalar(&ValueTypeDefinition::primitive(
                TypeId::from_bytes(id::<92>()),
                QualifiedSemanticName::new(["std", "types", "excluded"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                contract,
            ))
            .is_none()
        );
    }

    let application_primitive = TypeId::from_bytes(id::<93>());
    let application_enum = TypeId::from_bytes(id::<94>());
    let standard_enum = TypeId::from_bytes(id::<95>());
    let application = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<96>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<97>()),
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        vec![],
        vec![ValueTypeDefinition::primitive(
            application_primitive,
            QualifiedSemanticName::new(["app", "flag"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )],
        vec![EnumTypeDefinition::new(
            application_enum,
            QualifiedSemanticName::new(["app", "phase"]).unwrap(),
            ["new"],
        )],
        vec![],
    )
    .unwrap();
    let standard = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<98>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<99>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            standard_enum,
            QualifiedSemanticName::new(["std", "phase"]).unwrap(),
            ["new"],
        )],
        vec![],
    )
    .unwrap();
    assert!(
        record_value_field_runtime_type(
            &application,
            &standard,
            ResolvedType::value(application_primitive),
        )
        .is_none()
    );
    assert!(
        record_value_field_runtime_type(
            &application,
            &standard,
            ResolvedType::named(application_enum),
        )
        .is_some()
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &application,
            &standard,
            &TypeDescriptor::named(application_enum),
        ),
        Ok(RecordValueFieldDescriptorClass::ApplicationEnum(
            application_enum,
        ))
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &application,
            &standard,
            &TypeDescriptor::named(standard_enum),
        ),
        Ok(RecordValueFieldDescriptorClass::StandardEnum(standard_enum))
    );
    let colliding_standard = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<103>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<104>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            application_enum,
            QualifiedSemanticName::new(["std", "phase"]).unwrap(),
            ["new"],
        )],
        vec![],
    )
    .unwrap();
    assert_eq!(
        classify_record_value_field_descriptor(
            &application,
            &colliding_standard,
            &TypeDescriptor::named(application_enum),
        ),
        Err(RecordValueFieldDescriptorClassificationError::Ambiguous {
            type_id: application_enum,
        })
    );
    assert!(
        record_value_field_runtime_type(
            &application,
            &standard,
            ResolvedType::named(standard_enum),
        )
        .is_some()
    );
    for unsupported in [
        ResolvedType::value(TypeId::from_bytes(id::<100>())),
        ResolvedType::named(TypeId::from_bytes(id::<101>())),
        ResolvedType::scalar(StandardScalar::Boolean),
        ResolvedType::reference(TypeId::from_bytes(id::<102>())),
    ] {
        assert!(record_value_field_runtime_type(&application, &standard, unsupported,).is_none());
    }

    let collision = TypeId::from_bytes(id::<71>());
    let collision_catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<96>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<97>()),
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            collision,
            QualifiedSemanticName::new(["app", "collision"]).unwrap(),
            ["value"],
        )],
        vec![RecordValueTypeDefinition::new(
            TypeId::from_bytes(id::<98>()),
            QualifiedSemanticName::new(["app", "record"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<99>()),
                    "value",
                    0,
                    TypeDescriptor::named(collision),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap();
    assert_eq!(
        validate_record_value_field_types(&standard_context(), &collision_catalogue),
        Err(RevisionInvariantError::AmbiguousRecordValueFieldType {
            record_value_type: TypeId::from_bytes(id::<98>()),
            field: FieldId::from_bytes(id::<99>()),
            type_id: collision,
        })
    );
    let error = RevisionInvariantError::AmbiguousRecordValueFieldType {
        record_value_type: TypeId::from_bytes(id::<98>()),
        field: FieldId::from_bytes(id::<99>()),
        type_id: collision,
    };
    assert_eq!(
        error.to_string(),
        "record field type is present in both application and standard catalogues"
    );
    assert!(std::error::Error::source(&error).is_none());

    let cases = [
        (
            RecordValueFieldDescriptorError::StandardLibraryUnavailable,
            "deployable revision has no pinned standard library for record field classification",
        ),
        (
            RecordValueFieldDescriptorError::Unsupported,
            "record field descriptor is not supported by the deployable revision",
        ),
        (
            RecordValueFieldDescriptorError::Ambiguous { type_id: collision },
            "record field type is present in both application and standard catalogues",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
        assert!(std::error::Error::source(&error).is_none());
    }
}
