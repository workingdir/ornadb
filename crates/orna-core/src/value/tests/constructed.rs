//! Constructed collection and nested-record value tests.

use super::*;
/// One active revision with an acyclic nested-record catalogue.
///
/// `outer.payload` declares `inner` as its Named application-record
/// field, and `inner.value` is a pinned-standard Boolean leaf.
fn active_nested_record_revision() -> ActiveDatabaseRevision {
    active_nested_record_revision_with_child_fields(vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "value",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
    ])
}

pub(super) fn active_nested_record_revision_with_child_fields(
    child_fields: Vec<RecordValueFieldDefinition>,
) -> ActiveDatabaseRevision {
    active_nested_record_revision_with_seed(child_fields, 0x58, 0x64)
}

pub(super) fn active_nested_record_revision_with_seed(
    child_fields: Vec<RecordValueFieldDefinition>,
    catalogue_revision_byte: u8,
    source_revision_byte: u8,
) -> ActiveDatabaseRevision {
    active_nested_record_revision_with_standard_and_seed(
        child_fields,
        verified_standard_with_value_types(vec![standard_boolean_definition()]),
        catalogue_revision_byte,
        source_revision_byte,
    )
}

fn active_nested_record_revision_with_standard_and_seed(
    child_fields: Vec<RecordValueFieldDefinition>,
    standard: VerifiedStandardLibrarySnapshot,
    catalogue_revision_byte: u8,
    source_revision_byte: u8,
) -> ActiveDatabaseRevision {
    let application_schema = SchemaId::from_bytes([0x57; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([catalogue_revision_byte; 16]);
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let outer_field = FieldId::from_bytes([0x3b; 16]);
    let child_field_ids = child_fields
        .iter()
        .map(|field| field.id())
        .collect::<Vec<_>>();
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            application_schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![],
        vec![
            RecordValueTypeDefinition::new(
                inner_type,
                QualifiedSemanticName::new(["crm", "inner"]).unwrap(),
                child_fields,
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
    let context = CatalogueHashContext::version_two(standard);
    let application_content = "abcdef";
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
    let application_source_revision = SourceRevisionId::from_bytes([source_revision_byte; 16]);
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
    let mut identities = vec![
        DefinitionIdentity::Schema(application_schema),
        DefinitionIdentity::ValueType(inner_type),
    ];
    identities.extend(
        child_field_ids
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
                SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect::<Vec<_>>();
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

/// One active revision with an acyclic chain of 33 application records.
///
/// Record `0x20 + index` holds one field pointing at the next record for
/// `index < 32`, and the leaf `0x40` holds a pinned-standard Boolean.
fn active_record_chain_revision() -> ActiveDatabaseRevision {
    let standard = verified_standard_with_value_types(vec![standard_boolean_definition()]);
    let application_schema = SchemaId::from_bytes([0x57; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
    let mut records = Vec::new();
    for index in 0..33_u8 {
        let record_type = TypeId::from_bytes([0x20 + index; 16]);
        let field_id = FieldId::from_bytes([0x80 + index; 16]);
        let target = if index == 32 {
            STANDARD_BOOLEAN
        } else {
            TypeId::from_bytes([0x20 + index + 1; 16])
        };
        records.push(RecordValueTypeDefinition::new(
            record_type,
            QualifiedSemanticName::new(["crm", &format!("chain_{index}")]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    field_id,
                    if index == 32 { "value" } else { "next" },
                    0,
                    TypeDescriptor::named(target),
                )
                .unwrap(),
            ],
        ));
    }
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            application_schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![],
        records,
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let application_content = "z".repeat(70);
    let application_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x63; 16]),
        0,
        "app/chain.orna",
        &application_content,
        source_unit_content_digest(&application_content).unwrap(),
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
    let mut identities = vec![DefinitionIdentity::Schema(application_schema)];
    for index in 0..33_u8 {
        let record_type = TypeId::from_bytes([0x20 + index; 16]);
        let field_id = FieldId::from_bytes([0x80 + index; 16]);
        identities.push(DefinitionIdentity::ValueType(record_type));
        identities.push(DefinitionIdentity::Field {
            owner: record_type,
            field: field_id,
        });
    }
    let origins = identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect::<Vec<_>>();
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

#[test]
fn nested_record_value_constructs_against_one_active_revision() {
    let active = active_nested_record_revision();
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let inner = RecordValue::new(
        &active,
        inner_type,
        [(String::from("value"), RuntimeValue::Boolean(true))],
    )
    .expect("the flat inner record must construct");
    assert_eq!(inner.record_type(), inner_type);
    assert_eq!(inner.fields(), &[RuntimeValue::Boolean(true)]);
    assert_eq!(
        RuntimeValue::Record(inner.clone()).runtime_type(),
        RuntimeType::Flat(ResolvedType::named(inner_type))
    );

    let outer = RecordValue::new(
        &active,
        outer_type,
        [(String::from("payload"), RuntimeValue::Record(inner.clone()))],
    )
    .expect("nested application record field must be admitted");
    assert_eq!(outer.record_type(), outer_type);
    let [RuntimeValue::Record(inner_value)] = outer.fields() else {
        panic!("outer payload must hold the inner record value");
    };
    assert_eq!(
        inner_value, &inner,
        "outer must store the equal inner record in declaration order"
    );
    assert_eq!(
        RuntimeValue::Record(outer).runtime_type(),
        RuntimeType::Flat(ResolvedType::named(outer_type))
    );
}

#[test]
fn runtime_type_preserves_every_flat_runtime_variant() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = OpaqueCodecRegistry::new(
        standard,
        [opaque_registration(
            OPAQUE_TYPE,
            OPAQUE_NAME,
            OPAQUE_CONTRACT,
        )],
    )
    .unwrap();
    let record = RecordValue::new(
        &active,
        RECORD_TYPE,
        [
            (String::from("enabled"), RuntimeValue::Boolean(true)),
            (
                String::from("stage"),
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap(),
                ),
            ),
        ],
    )
    .unwrap();
    let opaque = OpaqueValue::new(&active, &registry, OPAQUE_TYPE, [0; 16]).unwrap();

    let cases: [(RuntimeValue, ResolvedType); 11] = [
        (
            RuntimeValue::null(ResolvedType::reference(TARGET)).unwrap(),
            ResolvedType::reference(TARGET),
        ),
        (
            RuntimeValue::Boolean(true),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        (
            RuntimeValue::Integer(-7),
            ResolvedType::scalar(StandardScalar::Integer),
        ),
        (
            RuntimeValue::BigInt(8),
            ResolvedType::scalar(StandardScalar::BigInt),
        ),
        (
            RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
            ResolvedType::scalar(StandardScalar::Float),
        ),
        (
            RuntimeValue::Text("value".into()),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        ),
        (
            RuntimeValue::Bytes(vec![1, 2, 3]),
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
        ),
        (
            RuntimeValue::Reference {
                target: TARGET,
                object: OBJECT,
            },
            ResolvedType::reference(TARGET),
        ),
        (
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap()),
            ResolvedType::named(ENUM_TYPE),
        ),
        (
            RuntimeValue::Record(record),
            ResolvedType::named(RECORD_TYPE),
        ),
        (
            RuntimeValue::Opaque(opaque),
            ResolvedType::value(OPAQUE_TYPE),
        ),
    ];

    for (value, expected) in cases {
        assert_eq!(value.runtime_type(), RuntimeType::Flat(expected));
    }

    let query: for<'a> fn(&'a RuntimeValue) -> RuntimeType<'a> = RuntimeValue::runtime_type;
    assert_eq!(
        query(&RuntimeValue::Boolean(true)),
        RuntimeType::Flat(ResolvedType::scalar(StandardScalar::Boolean))
    );
}

#[test]
fn canonical_nested_list_and_option_values_expose_exact_public_views_and_stay_closed() {
    let active = active_record_revision();
    let option_descriptor =
        TypeDescriptor::option(TypeDescriptor::named(STANDARD_BOOLEAN)).unwrap();
    let list_descriptor = TypeDescriptor::list(option_descriptor.clone()).unwrap();

    let some = RuntimeValue::option(
        &active,
        option_descriptor.clone(),
        Some(RuntimeValue::Boolean(true)),
    )
    .unwrap();
    let none = RuntimeValue::option(&active, option_descriptor.clone(), None).unwrap();
    let list = RuntimeValue::list(
        &active,
        list_descriptor.clone(),
        vec![some.clone(), none.clone()],
    )
    .unwrap();

    assert_eq!(
        list.runtime_type(),
        RuntimeType::Constructed(&list_descriptor)
    );

    let RuntimeValue::Constructed(constructed) = &list else {
        panic!("list value must be a constructed value");
    };
    assert_eq!(constructed.descriptor(), &list_descriptor);
    let ConstructedValueKind::List(elements) = constructed.kind() else {
        panic!("list kind view must expose the elements");
    };
    assert_eq!(elements.len(), 2);
    assert_eq!(elements[0], some);
    assert_eq!(elements[1], none);

    let RuntimeValue::Constructed(some_value) = &elements[0] else {
        panic!("first element must be constructed");
    };
    assert_eq!(some_value.descriptor(), &option_descriptor);
    let ConstructedValueKind::Option(inner) = some_value.kind() else {
        panic!("first element kind view must be an option");
    };
    assert_eq!(inner, Some(&RuntimeValue::Boolean(true)));

    let RuntimeValue::Constructed(none_value) = &elements[1] else {
        panic!("second element must be constructed");
    };
    assert_eq!(none_value.descriptor(), &option_descriptor);
    let ConstructedValueKind::Option(inner) = none_value.kind() else {
        panic!("second element kind view must be an option");
    };
    assert_eq!(inner, None);

    let list_again = RuntimeValue::list(
        &active,
        list_descriptor.clone(),
        vec![some.clone(), none.clone()],
    )
    .unwrap();
    assert_eq!(list, list_again);

    let parameter = ParameterId::from_bytes([0x4c; 16]);
    let argument_error = FunctionArgument::new(parameter, list.clone()).unwrap_err();
    assert_eq!(
        argument_error,
        FunctionArgumentError::ConstructedValueNotAccepted {
            parameter,
            descriptor: list_descriptor.clone(),
        }
    );
    assert_eq!(
        argument_error.to_string(),
        "constructed function arguments are not accepted"
    );
    assert!(std::error::Error::source(&argument_error).is_none());

    let column = ResultColumn::new(
        "value",
        ResolvedType::scalar(StandardScalar::Boolean),
        false,
    )
    .unwrap();
    let rows_error =
        ResultRows::new(vec![column], vec![ResultRow::new(vec![list.clone()])]).unwrap_err();
    assert_eq!(
        rows_error,
        ResultRowsError::ConstructedValueNotAccepted {
            row: 0,
            column: 0,
            descriptor: list_descriptor.clone(),
        }
    );
    assert_eq!(
        rows_error.to_string(),
        "constructed SERVER result values are not accepted"
    );
    assert!(std::error::Error::source(&rows_error).is_none());

    let record_error = RecordValue::new(
        &active,
        RECORD_TYPE,
        [(String::from("enabled"), list.clone())],
    )
    .unwrap_err();
    assert_eq!(
        record_error,
        RecordValueError::ConstructedValueNotAccepted {
            record_type: RECORD_TYPE,
            field: ENABLED_FIELD,
            descriptor: list_descriptor.clone(),
        }
    );
    assert_eq!(
        record_error.to_string(),
        "constructed record field values are not accepted"
    );
    assert!(std::error::Error::source(&record_error).is_none());
}

#[test]
fn stale_record_with_removed_trailing_field_reports_that_field_path() {
    const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
    const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
    const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);

    let field_a = RecordValueFieldDefinition::try_new_descriptor(
        A_FIELD,
        "a",
        0,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let field_b = RecordValueFieldDefinition::try_new_descriptor(
        B_FIELD,
        "b",
        1,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let old_active =
        active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b]);
    let old_record = RecordValue::new(
        &old_active,
        INNER_TYPE,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(true)),
        ],
    )
    .expect("the old two-field inner record must construct");

    let current = active_nested_record_revision_with_child_fields(vec![field_a]);
    let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap();
    let error = RuntimeValue::list(
        &current,
        list_descriptor,
        vec![RuntimeValue::Record(old_record)],
    )
    .unwrap_err();

    let CollectionValueError::InactiveValue { path } = &error else {
        panic!("stale record list must fail as an inactive value: {error}");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::ListElement(0),
            CollectionValuePathSegment::RecordField(B_FIELD),
        ]
    );
    assert_eq!(error.to_string(), "collection value is not active");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn stale_enum_label_precedes_a_later_unknown_field() {
    let active = active_record_revision();
    let stale_catalogue = enum_catalogue(&["retired"]);
    let stale = RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
    let error = RecordValue::new(
        &active,
        RECORD_TYPE,
        [
            (String::from("stage"), stale),
            (String::from("missing"), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap_err();
    assert_eq!(
        error,
        RecordValueError::InactiveEnumLabel {
            record_type: RECORD_TYPE,
            field: STAGE_FIELD,
            enum_type: ENUM_TYPE,
            label: String::from("retired"),
        }
    );
    assert_eq!(error.to_string(), "record enum field label is not active");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn stale_enum_label_precedes_a_missing_required_field() {
    let active = active_record_revision();
    let stale_catalogue = enum_catalogue(&["retired"]);
    let stale = RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
    let error =
        RecordValue::new(&active, RECORD_TYPE, [(String::from("stage"), stale)]).unwrap_err();
    assert_eq!(
        error,
        RecordValueError::InactiveEnumLabel {
            record_type: RECORD_TYPE,
            field: STAGE_FIELD,
            enum_type: ENUM_TYPE,
            label: String::from("retired"),
        }
    );
    assert_eq!(error.to_string(), "record enum field label is not active");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn stale_nested_record_precedes_a_later_unknown_field() {
    const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
    const OUTER_TYPE: TypeId = TypeId::from_bytes([0x30; 16]);
    const OUTER_FIELD: FieldId = FieldId::from_bytes([0x3b; 16]);
    const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
    const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);
    let field_a = RecordValueFieldDefinition::try_new_descriptor(
        A_FIELD,
        "a",
        0,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let field_b = RecordValueFieldDefinition::try_new_descriptor(
        B_FIELD,
        "b",
        1,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let old = active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b]);
    let old_child = RecordValue::new(
        &old,
        INNER_TYPE,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(true)),
        ],
    )
    .expect("the old two-field child must construct");
    let current = active_nested_record_revision_with_child_fields(vec![field_a]);
    let error = RecordValue::new(
        &current,
        OUTER_TYPE,
        [
            (String::from("payload"), RuntimeValue::Record(old_child)),
            (String::from("missing"), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap_err();
    assert_eq!(
        error,
        RecordValueError::InactiveNestedRecord {
            record_type: OUTER_TYPE,
            field: OUTER_FIELD,
            nested_record_type: INNER_TYPE,
        }
    );
    assert_eq!(error.to_string(), "nested record field value is not active");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn empty_option_list_and_map_retain_exact_constructed_views_and_equal_contents() {
    let active = active_record_revision();
    let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
    let option_desc = TypeDescriptor::option(boolean.clone()).unwrap();
    let list_desc = TypeDescriptor::list(boolean.clone()).unwrap();
    let map_desc = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

    let option = RuntimeValue::option(&active, option_desc.clone(), None).unwrap();
    assert_eq!(
        option.runtime_type(),
        RuntimeType::Constructed(&option_desc)
    );
    let RuntimeValue::Constructed(option_value) = &option else {
        panic!("empty option must be a constructed value");
    };
    assert_eq!(option_value.descriptor(), &option_desc);
    let ConstructedValueKind::Option(inner) = option_value.kind() else {
        panic!("empty option must expose the option kind");
    };
    assert_eq!(inner, None);

    let list = RuntimeValue::list(&active, list_desc.clone(), vec![]).unwrap();
    assert_eq!(list.runtime_type(), RuntimeType::Constructed(&list_desc));
    let RuntimeValue::Constructed(list_value) = &list else {
        panic!("empty list must be a constructed value");
    };
    assert_eq!(list_value.descriptor(), &list_desc);
    let ConstructedValueKind::List(elements) = list_value.kind() else {
        panic!("empty list must expose the list kind");
    };
    assert_eq!(elements, &[]);

    let map = RuntimeValue::map(&active, map_desc.clone(), vec![]).unwrap();
    assert_eq!(map.runtime_type(), RuntimeType::Constructed(&map_desc));
    let RuntimeValue::Constructed(map_value) = &map else {
        panic!("empty map must be a constructed value");
    };
    assert_eq!(map_value.descriptor(), &map_desc);
    let ConstructedValueKind::Map(entries) = map_value.kind() else {
        panic!("empty map must expose the map kind");
    };
    assert_eq!(entries, &[]);

    let inner_list = RuntimeValue::list(
        &active,
        list_desc.clone(),
        vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
    )
    .unwrap();
    let nested_option = TypeDescriptor::option(list_desc.clone()).unwrap();
    let nested =
        RuntimeValue::option(&active, nested_option.clone(), Some(inner_list.clone())).unwrap();
    let RuntimeValue::Constructed(nested_value) = &nested else {
        panic!("nested option must be a constructed value");
    };
    let ConstructedValueKind::Option(Some(child)) = nested_value.kind() else {
        panic!("nested option must expose its some child");
    };
    assert_eq!(child, &inner_list);
    assert_eq!(
        nested,
        RuntimeValue::option(&active, nested_option, Some(inner_list.clone())).unwrap()
    );

    let map_entry = RuntimeValue::map(
        &active,
        map_desc.clone(),
        vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false))],
    )
    .unwrap();
    let RuntimeValue::Constructed(map_entry_value) = &map_entry else {
        panic!("map entry must be a constructed value");
    };
    let ConstructedValueKind::Map(entries) = map_entry_value.kind() else {
        panic!("map entry must expose the map kind");
    };
    assert_eq!(
        entries,
        &[(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false))]
    );
    assert_eq!(
        map_entry,
        RuntimeValue::map(
            &active,
            map_desc,
            vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false),)],
        )
        .unwrap()
    );

    let ordered = RuntimeValue::list(
        &active,
        list_desc.clone(),
        vec![
            RuntimeValue::Boolean(true),
            RuntimeValue::Boolean(false),
            RuntimeValue::Boolean(true),
        ],
    )
    .unwrap();
    let RuntimeValue::Constructed(ordered_value) = &ordered else {
        panic!("ordered list must be a constructed value");
    };
    let ConstructedValueKind::List(elements) = ordered_value.kind() else {
        panic!("ordered list must expose the list kind");
    };
    assert_eq!(
        elements,
        &[
            RuntimeValue::Boolean(true),
            RuntimeValue::Boolean(false),
            RuntimeValue::Boolean(true),
        ]
    );
    assert_eq!(
        ordered,
        RuntimeValue::list(
            &active,
            list_desc,
            vec![
                RuntimeValue::Boolean(true),
                RuntimeValue::Boolean(false),
                RuntimeValue::Boolean(true),
            ],
        )
        .unwrap()
    );
}

#[test]
fn set_values_are_canonically_ordered_and_unique() {
    let active = active_record_revision();
    let descriptor = TypeDescriptor::set(TypeDescriptor::named(STANDARD_BOOLEAN)).unwrap();
    let value = RuntimeValue::set(
        &active,
        descriptor.clone(),
        vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
    )
    .unwrap();
    let RuntimeValue::Constructed(constructed) = &value else {
        panic!("set construction must return a constructed value");
    };
    let ConstructedValueKind::Set(values) = constructed.kind() else {
        panic!("set construction must retain SET values");
    };
    assert_eq!(
        values,
        &[RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)]
    );
    assert_eq!(
        value,
        RuntimeValue::set(
            &active,
            descriptor.clone(),
            vec![RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)],
        )
        .unwrap()
    );
    assert_eq!(
        RuntimeValue::set(
            &active,
            descriptor,
            vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(true)],
        )
        .unwrap_err(),
        CollectionValueError::DuplicateSetElement {
            first: 0,
            duplicate: 1,
        }
    );
}

#[test]
fn constructed_constructors_reject_wrong_outer_descriptors_exactly() {
    let active = active_record_revision();
    let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
    let option_desc = TypeDescriptor::option(boolean.clone()).unwrap();
    let list_desc = TypeDescriptor::list(boolean.clone()).unwrap();
    let map_desc = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

    let wrong_option = RuntimeValue::option(&active, list_desc.clone(), None).unwrap_err();
    assert_eq!(
        wrong_option,
        CollectionValueError::WrongConstructor {
            expected: CollectionKind::Option,
            descriptor: list_desc.clone(),
        }
    );
    assert_eq!(
        wrong_option.to_string(),
        "collection descriptor has the wrong outer constructor"
    );
    assert!(std::error::Error::source(&wrong_option).is_none());

    let wrong_list = RuntimeValue::list(&active, option_desc.clone(), vec![]).unwrap_err();
    assert_eq!(
        wrong_list,
        CollectionValueError::WrongConstructor {
            expected: CollectionKind::List,
            descriptor: option_desc.clone(),
        }
    );
    assert_eq!(
        wrong_list.to_string(),
        "collection descriptor has the wrong outer constructor"
    );
    assert!(std::error::Error::source(&wrong_list).is_none());

    let wrong_map = RuntimeValue::map(&active, option_desc.clone(), vec![]).unwrap_err();
    assert_eq!(
        wrong_map,
        CollectionValueError::WrongConstructor {
            expected: CollectionKind::Map,
            descriptor: option_desc.clone(),
        }
    );
    assert_eq!(
        wrong_map.to_string(),
        "collection descriptor has the wrong outer constructor"
    );
    assert!(std::error::Error::source(&wrong_map).is_none());

    let set_desc = TypeDescriptor::set(boolean.clone()).unwrap();
    let list_of_set = TypeDescriptor::list(set_desc).unwrap();
    let wrong_before_unsupported =
        RuntimeValue::option(&active, list_of_set.clone(), None).unwrap_err();
    assert_eq!(
        wrong_before_unsupported,
        CollectionValueError::WrongConstructor {
            expected: CollectionKind::Option,
            descriptor: list_of_set.clone(),
        }
    );
    assert_eq!(
        wrong_before_unsupported.to_string(),
        "collection descriptor has the wrong outer constructor"
    );
    assert!(std::error::Error::source(&wrong_before_unsupported).is_none());

    let _ = map_desc;
}

#[test]
fn collection_descriptor_preorder_reports_exact_paths_for_unsupported_children() {
    let active = active_record_revision();
    let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
    let set_desc = TypeDescriptor::set(boolean.clone()).unwrap();
    let stream_desc = TypeDescriptor::stream(boolean.clone()).unwrap();

    let option_of_set = TypeDescriptor::option(set_desc.clone()).unwrap();
    let error = RuntimeValue::option(&active, option_of_set.clone(), None).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("option child must fail as an unsupported descriptor: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::OptionChild]);
    assert_eq!(descriptor, &set_desc);

    let list_of_set = TypeDescriptor::list(set_desc.clone()).unwrap();
    let error = RuntimeValue::list(&active, list_of_set.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("list child must fail as an unsupported descriptor: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(descriptor, &set_desc);

    let nested_list =
        TypeDescriptor::list(TypeDescriptor::list(stream_desc.clone()).unwrap()).unwrap();
    let error = RuntimeValue::list(&active, nested_list.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("nested list child must fail at the deepest path: {error}");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::ListChild,
            CollectionValuePathSegment::ListChild,
        ]
    );
    assert_eq!(descriptor, &stream_desc);

    let map_stream_key = TypeDescriptor::map(stream_desc.clone(), boolean.clone()).unwrap();
    let error = RuntimeValue::map(&active, map_stream_key.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("map stream key must fail before the value: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKeyChild]);
    assert_eq!(descriptor, &stream_desc);

    let map_set_value = TypeDescriptor::map(boolean.clone(), set_desc.clone()).unwrap();
    let error = RuntimeValue::map(&active, map_set_value.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("map set value must fail at the value child: {error}");
    };
    assert_eq!(
        path.segments(),
        &[CollectionValuePathSegment::MapValueChild]
    );
    assert_eq!(descriptor, &set_desc);

    let map_list_key = TypeDescriptor::map(
        TypeDescriptor::list(boolean.clone()).unwrap(),
        boolean.clone(),
    )
    .unwrap();
    let error = RuntimeValue::map(&active, map_list_key.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("constructed map key must fail at the key child without a deeper path: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKeyChild]);
    assert_eq!(descriptor, &TypeDescriptor::list(boolean.clone()).unwrap());

    let option_of_stream = TypeDescriptor::option(stream_desc.clone()).unwrap();
    let error = RuntimeValue::option(&active, option_of_stream.clone(), None).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("option stream child must fail at the option child path: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::OptionChild]);
    assert_eq!(descriptor, &stream_desc);
}

#[test]
fn collection_descriptor_rejects_missing_and_inactive_leaf_categories_exactly() {
    let active = active_record_revision();
    let missing = TypeDescriptor::named(TypeId::from_bytes([0x6c; 16]));

    let list_of_missing = TypeDescriptor::list(missing.clone()).unwrap();
    let error = RuntimeValue::list(&active, list_of_missing.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("missing named leaf must be unsupported: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(descriptor, &missing);

    let opaque = TypeDescriptor::named(OPAQUE_TYPE);
    let list_of_opaque = TypeDescriptor::list(opaque.clone()).unwrap();
    let error = RuntimeValue::list(&active, list_of_opaque.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("pinned opaque leaf must be unsupported: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(descriptor, &opaque);

    let reference = TypeDescriptor::reference(TARGET);
    let list_of_reference = TypeDescriptor::list(reference.clone()).unwrap();
    let error = RuntimeValue::list(&active, list_of_reference.clone(), vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("inactive reference leaf must be unsupported: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(descriptor, &reference);
}

#[test]
fn sealed_source_metadata_is_rejected_outside_permitted_collection_positions() {
    let active = active_record_revision();
    let source = TypeDescriptor::named(crate::system::SYS_SOURCE_FUNCTION_TYPE_ID);

    let map_value =
        TypeDescriptor::map(TypeDescriptor::named(STANDARD_BOOLEAN), source.clone()).unwrap();
    let error = RuntimeValue::map(&active, map_value, vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = error else {
        panic!("source metadata map value must be rejected");
    };
    assert_eq!(
        path.segments(),
        &[CollectionValuePathSegment::MapValueChild]
    );
    assert_eq!(descriptor, source.clone());

    let map_key =
        TypeDescriptor::map(source.clone(), TypeDescriptor::named(STANDARD_BOOLEAN)).unwrap();
    let error = RuntimeValue::map(&active, map_key, vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = error else {
        panic!("source metadata map key must be rejected");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKeyChild]);
    assert_eq!(descriptor, source);

    let nested_set = TypeDescriptor::list(TypeDescriptor::set(source).unwrap()).unwrap();
    let error = RuntimeValue::list(&active, nested_set, vec![]).unwrap_err();
    assert!(matches!(
        error,
        CollectionValueError::UnsupportedDescriptor { .. }
    ));
}

#[test]
fn ambiguous_application_standard_named_collision_precedes_category_rejection() {
    let active = active_record_revision_with_opaque_contract(
        TypeId::from_bytes([0x49; 16]),
        OPAQUE_CONTRACT,
    );
    let collision = TypeDescriptor::named(TypeId::from_bytes([0x49; 16]));
    let list_of_collision = TypeDescriptor::list(collision.clone()).unwrap();
    let error = RuntimeValue::list(&active, list_of_collision.clone(), vec![]).unwrap_err();
    let CollectionValueError::AmbiguousNamedType { path, type_id } = &error else {
        panic!("application-standard collision must be ambiguous: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(*type_id, TypeId::from_bytes([0x49; 16]));
    assert_eq!(
        error.to_string(),
        "collection descriptor type is present in both application and standard catalogues"
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn constructed_constructor_precedence_is_exact() {
    let active = active_record_revision();
    let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
    let set_desc = TypeDescriptor::set(boolean.clone()).unwrap();
    let list_of_set = TypeDescriptor::list(set_desc.clone()).unwrap();
    let list_boolean = TypeDescriptor::list(boolean.clone()).unwrap();
    let map_boolean = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

    let error = RuntimeValue::list(
        &active,
        list_of_set.clone(),
        vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES],
    )
    .unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, .. } = &error else {
        panic!("descriptor failure must precede node counting: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);

    let mut overflow = vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES];
    overflow.push(RuntimeValue::Text("x".into()));
    let error = RuntimeValue::list(&active, list_boolean.clone(), overflow).unwrap_err();
    assert_eq!(
        error,
        CollectionValueError::TooManyNodes {
            maximum: MAX_RUNTIME_VALUE_NODES,
        }
    );

    let error = RuntimeValue::list(
        &active,
        list_boolean.clone(),
        vec![
            RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
            RuntimeValue::Text("x".into()),
        ],
    )
    .unwrap_err();
    let CollectionValueError::NullValueNotAccepted { path } = &error else {
        panic!("null element must precede a later type mismatch: {error}");
    };
    assert_eq!(
        path.segments(),
        &[CollectionValuePathSegment::ListElement(0)]
    );

    const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
    const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
    const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);
    let field_a = RecordValueFieldDefinition::try_new_descriptor(
        A_FIELD,
        "a",
        0,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let field_b = RecordValueFieldDefinition::try_new_descriptor(
        B_FIELD,
        "b",
        1,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let old = active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b]);
    let stale_child = RecordValue::new(
        &old,
        INNER_TYPE,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(true)),
        ],
    )
    .expect("the old two-field child must construct");
    let current = active_nested_record_revision_with_child_fields(vec![field_a]);
    let list_inner = TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap();
    let error = RuntimeValue::list(
        &current,
        list_inner,
        vec![RuntimeValue::Integer(1), RuntimeValue::Record(stale_child)],
    )
    .unwrap_err();
    let CollectionValueError::ValueTypeMismatch { path } = &error else {
        panic!("type mismatch must precede an inactive element: {error}");
    };
    assert_eq!(
        path.segments(),
        &[CollectionValuePathSegment::ListElement(0)]
    );

    let error = RuntimeValue::map(
        &active,
        map_boolean.clone(),
        vec![(
            RuntimeValue::Text("k".into()),
            RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
        )],
    )
    .unwrap_err();
    let CollectionValueError::ValueTypeMismatch { path } = &error else {
        panic!("map key mismatch must precede the value failure: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKey(0)]);

    let error = RuntimeValue::map(
        &active,
        map_boolean.clone(),
        vec![
            (RuntimeValue::Boolean(true), RuntimeValue::Text("x".into())),
            (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
        ],
    )
    .unwrap_err();
    let CollectionValueError::ValueTypeMismatch { path } = &error else {
        panic!("value semantic failure must precede duplicate detection: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::MapValue(0)]);
}

const MAP_BOOLEAN: TypeId = TypeId::from_bytes([0x81; 16]);
const MAP_INTEGER: TypeId = TypeId::from_bytes([0x82; 16]);
const MAP_BIGINT: TypeId = TypeId::from_bytes([0x83; 16]);
const MAP_FLOAT: TypeId = TypeId::from_bytes([0x84; 16]);
const MAP_TEXT: TypeId = TypeId::from_bytes([0x85; 16]);
const MAP_BYTES: TypeId = TypeId::from_bytes([0x86; 16]);
pub(super) const MAP_STD_ENUM: TypeId = TypeId::from_bytes([0x87; 16]);
const MAP_APP_ENUM: TypeId = TypeId::from_bytes([0x88; 16]);
const MAP_OBJECT: TypeId = TypeId::from_bytes([0x89; 16]);
const MAP_FLAT: TypeId = TypeId::from_bytes([0x8a; 16]);
const MAP_INNER: TypeId = TypeId::from_bytes([0x8b; 16]);
const MAP_OUTER: TypeId = TypeId::from_bytes([0x8c; 16]);
const MAP_STD_ENUM_RECORD: TypeId = TypeId::from_bytes([0x8d; 16]);
const MAP_A_FIELD: FieldId = FieldId::from_bytes([0xa1; 16]);
const MAP_B_FIELD: FieldId = FieldId::from_bytes([0xa2; 16]);
const MAP_LEAF_FIELD: FieldId = FieldId::from_bytes([0xa3; 16]);
const MAP_FIRST_FIELD: FieldId = FieldId::from_bytes([0xa4; 16]);
const MAP_TAIL_FIELD: FieldId = FieldId::from_bytes([0xa5; 16]);
const MAP_STD_ENUM_ENABLED_FIELD: FieldId = FieldId::from_bytes([0xa6; 16]);
const MAP_STD_ENUM_FIELD: FieldId = FieldId::from_bytes([0xa7; 16]);

fn map_standard_primitive(type_id: TypeId, name: &str, contract: &str) -> ValueTypeDefinition {
    ValueTypeDefinition::primitive(
        type_id,
        QualifiedSemanticName::new(["std", name]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        contract,
    )
}

fn verified_map_standard() -> VerifiedStandardLibrarySnapshot {
    let value_types = vec![
        map_standard_primitive(MAP_BOOLEAN, "boolean", "orna.kernel.value.boolean@1"),
        map_standard_primitive(MAP_INTEGER, "integer", "orna.kernel.value.integer@1"),
        map_standard_primitive(MAP_BIGINT, "bigint", "orna.kernel.value.bigint@1"),
        map_standard_primitive(MAP_FLOAT, "float", "orna.kernel.value.float@1"),
        map_standard_primitive(
            MAP_TEXT,
            "character_large_object",
            "orna.kernel.value.character-large-object@1",
        ),
        map_standard_primitive(
            MAP_BYTES,
            "binary_large_object",
            "orna.kernel.value.binary-large-object@1",
        ),
    ];
    let enum_types = vec![EnumTypeDefinition::new(
        MAP_STD_ENUM,
        QualifiedSemanticName::new(["std", "mode"]).unwrap(),
        ["alpha", "beta"],
    )];
    let standard_unit_content = "x".repeat(value_types.len() + enum_types.len() + 2);
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
    let standard_catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes([0x5b; 16]),
        vec![
            SchemaDefinition::new(
                standard_schema,
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                standard_types_schema,
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ],
        vec![],
        value_types.clone(),
        enum_types.clone(),
        vec![],
        vec![],
    )
    .unwrap();
    let mut standard_origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(standard_schema),
            SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(standard_types_schema),
            SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), 1, 2).unwrap(),
        ),
    ];
    for (index, value_type) in standard_catalogue.value_types().iter().enumerate() {
        let start = u32::try_from(index + 2).unwrap();
        standard_origins.push(DefinitionOrigin::new(
            DefinitionIdentity::ValueType(value_type.id()),
            SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1).unwrap(),
        ));
    }
    for (index, enum_type) in standard_catalogue.enum_types().iter().enumerate() {
        let start = u32::try_from(value_types.len() + index + 2).unwrap();
        standard_origins.push(DefinitionOrigin::new(
            DefinitionIdentity::ValueType(enum_type.id()),
            SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), start, start + 1).unwrap(),
        ));
    }
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

fn active_map_revision() -> ActiveDatabaseRevision {
    let standard = verified_map_standard();
    let application_schema = SchemaId::from_bytes([0x91; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x92; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            application_schema,
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            MAP_OBJECT,
            QualifiedSemanticName::new(["app", "item"]).unwrap(),
            vec![],
        )],
        vec![],
        vec![EnumTypeDefinition::new(
            MAP_APP_ENUM,
            QualifiedSemanticName::new(["app", "stage"]).unwrap(),
            ["low", "high"],
        )],
        vec![
            RecordValueTypeDefinition::new(
                MAP_FLAT,
                QualifiedSemanticName::new(["app", "flat"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        MAP_A_FIELD,
                        "a",
                        0,
                        TypeDescriptor::named(MAP_BOOLEAN),
                    )
                    .unwrap(),
                    RecordValueFieldDefinition::try_new_descriptor(
                        MAP_B_FIELD,
                        "b",
                        1,
                        TypeDescriptor::named(MAP_INTEGER),
                    )
                    .unwrap(),
                ],
            ),
            RecordValueTypeDefinition::new(
                MAP_INNER,
                QualifiedSemanticName::new(["app", "inner"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        MAP_LEAF_FIELD,
                        "leaf",
                        0,
                        TypeDescriptor::named(MAP_BOOLEAN),
                    )
                    .unwrap(),
                ],
            ),
            RecordValueTypeDefinition::new(
                MAP_OUTER,
                QualifiedSemanticName::new(["app", "outer"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        MAP_FIRST_FIELD,
                        "first",
                        0,
                        TypeDescriptor::named(MAP_INNER),
                    )
                    .unwrap(),
                    RecordValueFieldDefinition::try_new_descriptor(
                        MAP_TAIL_FIELD,
                        "tail",
                        1,
                        TypeDescriptor::named(MAP_INTEGER),
                    )
                    .unwrap(),
                ],
            ),
            RecordValueTypeDefinition::new(
                MAP_STD_ENUM_RECORD,
                QualifiedSemanticName::new(["app", "standard_enum_record"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        MAP_STD_ENUM_ENABLED_FIELD,
                        "enabled",
                        0,
                        TypeDescriptor::named(MAP_BOOLEAN),
                    )
                    .unwrap(),
                    RecordValueFieldDefinition::try_new_descriptor(
                        MAP_STD_ENUM_FIELD,
                        "mode",
                        1,
                        TypeDescriptor::named(MAP_STD_ENUM),
                    )
                    .unwrap(),
                ],
            ),
        ],
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let application_content = "x".repeat(15);
    let application_content_digest = source_unit_content_digest(&application_content).unwrap();
    let application_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x93; 16]),
        0,
        "app/types.orna",
        application_content,
        application_content_digest,
    )
    .unwrap();
    let application_bundle_hash =
        source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
    let application_source_revision = SourceRevisionId::from_bytes([0x95; 16]);
    let application_source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x94; 16]),
        application_source_revision,
        None,
        vec![application_unit],
        application_bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x94; 16]),
            None,
            application_bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let source_unit = SourceUnitId::from_bytes([0x93; 16]);
    let mut origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(application_schema),
            SourceOrigin::new(source_unit, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(MAP_APP_ENUM),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(MAP_OBJECT),
            SourceOrigin::new(source_unit, 2, 3).unwrap(),
        ),
    ];
    for (index, record) in catalogue.record_value_types().iter().enumerate() {
        let record_start = u32::try_from(3 + index * 3).unwrap();
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::ValueType(record.id()),
            SourceOrigin::new(source_unit, record_start, record_start + 1).unwrap(),
        ));
        for (field_index, field) in record.fields().iter().enumerate() {
            let field_start = record_start + 1 + u32::try_from(field_index).unwrap();
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record.id(),
                    field: field.id(),
                },
                SourceOrigin::new(source_unit, field_start, field_start + 1).unwrap(),
            ));
        }
    }
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

fn canonical_map_entries(
    active: &ActiveDatabaseRevision,
    key_descriptor: TypeDescriptor,
    entries: Vec<(RuntimeValue, RuntimeValue)>,
) -> Vec<(RuntimeValue, RuntimeValue)> {
    let map_descriptor =
        TypeDescriptor::map(key_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap();
    let map = RuntimeValue::map(active, map_descriptor, entries).unwrap();
    let RuntimeValue::Constructed(value) = &map else {
        panic!("map value must be constructed");
    };
    let ConstructedValueKind::Map(entries) = value.kind() else {
        panic!("map value must expose the map kind");
    };
    entries.to_vec()
}

fn map_flat_record(active: &ActiveDatabaseRevision, a: bool, b: i32) -> RuntimeValue {
    RuntimeValue::Record(
        RecordValue::new(
            active,
            MAP_FLAT,
            [
                (String::from("a"), RuntimeValue::Boolean(a)),
                (String::from("b"), RuntimeValue::Integer(b)),
            ],
        )
        .unwrap(),
    )
}

fn map_outer_record(active: &ActiveDatabaseRevision, leaf: bool, tail: i32) -> RuntimeValue {
    let inner = RecordValue::new(
        active,
        MAP_INNER,
        [(String::from("leaf"), RuntimeValue::Boolean(leaf))],
    )
    .unwrap();
    RuntimeValue::Record(
        RecordValue::new(
            active,
            MAP_OUTER,
            [
                (String::from("first"), RuntimeValue::Record(inner)),
                (String::from("tail"), RuntimeValue::Integer(tail)),
            ],
        )
        .unwrap(),
    )
}

fn map_keys(keys: Vec<RuntimeValue>) -> Vec<(RuntimeValue, RuntimeValue)> {
    keys.into_iter()
        .map(|key| (key, RuntimeValue::Boolean(true)))
        .collect()
}

#[test]
fn map_canonical_order_holds_for_every_admitted_flat_key_family() {
    let active = active_map_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();

    let boolean_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_BOOLEAN),
        map_keys(vec![
            RuntimeValue::Boolean(true),
            RuntimeValue::Boolean(false),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        boolean_keys,
        vec![RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)]
    );

    let integer_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_INTEGER),
        map_keys(vec![
            RuntimeValue::Integer(2),
            RuntimeValue::Integer(0),
            RuntimeValue::Integer(-1),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        integer_keys,
        vec![
            RuntimeValue::Integer(-1),
            RuntimeValue::Integer(0),
            RuntimeValue::Integer(2),
        ]
    );

    let bigint_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_BIGINT),
        map_keys(vec![
            RuntimeValue::BigInt(0),
            RuntimeValue::BigInt(-5),
            RuntimeValue::BigInt(9),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        bigint_keys,
        vec![
            RuntimeValue::BigInt(-5),
            RuntimeValue::BigInt(0),
            RuntimeValue::BigInt(9),
        ]
    );

    let float_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_FLOAT),
        map_keys(vec![
            RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap()),
            RuntimeValue::Float(RuntimeFloat::new(-2.5).unwrap()),
            RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap()),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        float_keys,
        vec![
            RuntimeValue::Float(RuntimeFloat::new(-2.5).unwrap()),
            RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap()),
            RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap()),
        ]
    );

    let text_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_TEXT),
        map_keys(vec![
            RuntimeValue::Text("cherry".into()),
            RuntimeValue::Text("apple".into()),
            RuntimeValue::Text("banana".into()),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        text_keys,
        vec![
            RuntimeValue::Text("apple".into()),
            RuntimeValue::Text("banana".into()),
            RuntimeValue::Text("cherry".into()),
        ]
    );

    let bytes_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_BYTES),
        map_keys(vec![
            RuntimeValue::Bytes(vec![2]),
            RuntimeValue::Bytes(vec![1, 0]),
            RuntimeValue::Bytes(vec![1]),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        bytes_keys,
        vec![
            RuntimeValue::Bytes(vec![1]),
            RuntimeValue::Bytes(vec![1, 0]),
            RuntimeValue::Bytes(vec![2]),
        ]
    );

    let reference_keys = canonical_map_entries(
        &active,
        TypeDescriptor::reference(MAP_OBJECT),
        map_keys(vec![
            RuntimeValue::Reference {
                target: MAP_OBJECT,
                object: ObjectId::from_bytes([0x02; 16]),
            },
            RuntimeValue::Reference {
                target: MAP_OBJECT,
                object: ObjectId::from_bytes([0x01; 16]),
            },
            RuntimeValue::Reference {
                target: MAP_OBJECT,
                object: ObjectId::from_bytes([0x03; 16]),
            },
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        reference_keys,
        vec![
            RuntimeValue::Reference {
                target: MAP_OBJECT,
                object: ObjectId::from_bytes([0x01; 16]),
            },
            RuntimeValue::Reference {
                target: MAP_OBJECT,
                object: ObjectId::from_bytes([0x02; 16]),
            },
            RuntimeValue::Reference {
                target: MAP_OBJECT,
                object: ObjectId::from_bytes([0x03; 16]),
            },
        ]
    );

    let app_enum_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_APP_ENUM),
        map_keys(vec![
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), MAP_APP_ENUM, "low").unwrap()),
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), MAP_APP_ENUM, "high").unwrap()),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        app_enum_keys,
        vec![
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), MAP_APP_ENUM, "high").unwrap(),),
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), MAP_APP_ENUM, "low").unwrap(),),
        ]
    );

    let std_enum_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_STD_ENUM),
        map_keys(vec![
            RuntimeValue::Enum(EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "beta").unwrap()),
            RuntimeValue::Enum(
                EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "alpha").unwrap(),
            ),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        std_enum_keys,
        vec![
            RuntimeValue::Enum(
                EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "alpha").unwrap(),
            ),
            RuntimeValue::Enum(EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "beta").unwrap(),),
        ]
    );

    let flat_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_FLAT),
        map_keys(vec![
            map_flat_record(&active, true, 2),
            map_flat_record(&active, false, 1),
            map_flat_record(&active, true, 1),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        flat_keys,
        vec![
            map_flat_record(&active, false, 1),
            map_flat_record(&active, true, 1),
            map_flat_record(&active, true, 2),
        ]
    );

    let outer_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_OUTER),
        map_keys(vec![
            map_outer_record(&active, true, 1),
            map_outer_record(&active, false, 9),
            map_outer_record(&active, true, 0),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        outer_keys,
        vec![
            map_outer_record(&active, false, 9),
            map_outer_record(&active, true, 0),
            map_outer_record(&active, true, 1),
        ]
    );
}

#[test]
fn map_input_permutations_produce_equal_canonical_maps() {
    let active = active_map_revision();
    let integer_descriptor = TypeDescriptor::named(MAP_INTEGER);
    let text_descriptor = TypeDescriptor::named(MAP_TEXT);
    let flat_descriptor = TypeDescriptor::named(MAP_FLAT);

    let forward = RuntimeValue::map(
        &active,
        TypeDescriptor::map(
            integer_descriptor.clone(),
            TypeDescriptor::named(MAP_BOOLEAN),
        )
        .unwrap(),
        map_keys(vec![
            RuntimeValue::Integer(1),
            RuntimeValue::Integer(0),
            RuntimeValue::Integer(2),
        ]),
    )
    .unwrap();
    let reversed = RuntimeValue::map(
        &active,
        TypeDescriptor::map(integer_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
        map_keys(vec![
            RuntimeValue::Integer(2),
            RuntimeValue::Integer(1),
            RuntimeValue::Integer(0),
        ]),
    )
    .unwrap();
    assert_eq!(forward, reversed);
    let RuntimeValue::Constructed(forward_value) = &forward else {
        panic!("forward map must be constructed");
    };
    let RuntimeValue::Constructed(reversed_value) = &reversed else {
        panic!("reversed map must be constructed");
    };
    let ConstructedValueKind::Map(forward_entries) = forward_value.kind() else {
        panic!("forward map must expose the map kind");
    };
    let ConstructedValueKind::Map(reversed_entries) = reversed_value.kind() else {
        panic!("reversed map must expose the map kind");
    };
    assert_eq!(forward_entries, reversed_entries);

    let text_forward = RuntimeValue::map(
        &active,
        TypeDescriptor::map(text_descriptor.clone(), TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
        map_keys(vec![
            RuntimeValue::Text("b".into()),
            RuntimeValue::Text("a".into()),
            RuntimeValue::Text("c".into()),
        ]),
    )
    .unwrap();
    let text_scrambled = RuntimeValue::map(
        &active,
        TypeDescriptor::map(text_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
        map_keys(vec![
            RuntimeValue::Text("c".into()),
            RuntimeValue::Text("b".into()),
            RuntimeValue::Text("a".into()),
        ]),
    )
    .unwrap();
    assert_eq!(text_forward, text_scrambled);

    let flat_forward = RuntimeValue::map(
        &active,
        TypeDescriptor::map(flat_descriptor.clone(), TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
        map_keys(vec![
            map_flat_record(&active, false, 1),
            map_flat_record(&active, true, 2),
        ]),
    )
    .unwrap();
    let flat_reversed = RuntimeValue::map(
        &active,
        TypeDescriptor::map(flat_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
        map_keys(vec![
            map_flat_record(&active, true, 2),
            map_flat_record(&active, false, 1),
        ]),
    )
    .unwrap();
    assert_eq!(flat_forward, flat_reversed);
}

#[test]
fn map_duplicate_keys_report_exact_original_indices() {
    let active = active_map_revision();
    let integer_descriptor = TypeDescriptor::named(MAP_INTEGER);
    let float_descriptor = TypeDescriptor::named(MAP_FLOAT);

    let error = RuntimeValue::map(
        &active,
        TypeDescriptor::map(
            integer_descriptor.clone(),
            TypeDescriptor::named(MAP_BOOLEAN),
        )
        .unwrap(),
        map_keys(vec![RuntimeValue::Integer(5), RuntimeValue::Integer(5)]),
    )
    .unwrap_err();
    assert_eq!(
        error,
        CollectionValueError::DuplicateMapKey {
            first: 0,
            duplicate: 1,
        }
    );

    let three_error = RuntimeValue::map(
        &active,
        TypeDescriptor::map(
            integer_descriptor.clone(),
            TypeDescriptor::named(MAP_BOOLEAN),
        )
        .unwrap(),
        map_keys(vec![
            RuntimeValue::Integer(5),
            RuntimeValue::Integer(5),
            RuntimeValue::Integer(5),
        ]),
    )
    .unwrap_err();
    assert_eq!(
        three_error,
        CollectionValueError::DuplicateMapKey {
            first: 0,
            duplicate: 1,
        }
    );

    let canonical_first_error = RuntimeValue::map(
        &active,
        TypeDescriptor::map(
            integer_descriptor.clone(),
            TypeDescriptor::named(MAP_BOOLEAN),
        )
        .unwrap(),
        vec![
            (RuntimeValue::Integer(5), RuntimeValue::Boolean(true)),
            (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
            (RuntimeValue::Integer(5), RuntimeValue::Boolean(true)),
            (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap_err();
    assert_eq!(
        canonical_first_error,
        CollectionValueError::DuplicateMapKey {
            first: 1,
            duplicate: 3,
        }
    );

    let negative_zero_error = RuntimeValue::map(
        &active,
        TypeDescriptor::map(float_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
        map_keys(vec![
            RuntimeValue::Float(RuntimeFloat::new(-0.0).unwrap()),
            RuntimeValue::Float(RuntimeFloat::new(0.0).unwrap()),
        ]),
    )
    .unwrap_err();
    assert_eq!(
        negative_zero_error,
        CollectionValueError::DuplicateMapKey {
            first: 0,
            duplicate: 1,
        }
    );
    assert_eq!(
        negative_zero_error.to_string(),
        "map contains a duplicate key"
    );
    assert!(std::error::Error::source(&negative_zero_error).is_none());
}

#[test]
fn record_map_keys_order_lexicographically_in_declaration_order() {
    let active = active_map_revision();

    let flat_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_FLAT),
        map_keys(vec![
            map_flat_record(&active, true, 2),
            map_flat_record(&active, true, 1),
            map_flat_record(&active, false, 9),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        flat_keys,
        vec![
            map_flat_record(&active, false, 9),
            map_flat_record(&active, true, 1),
            map_flat_record(&active, true, 2),
        ]
    );

    let outer_keys = canonical_map_entries(
        &active,
        TypeDescriptor::named(MAP_OUTER),
        map_keys(vec![
            map_outer_record(&active, true, 1),
            map_outer_record(&active, false, 9),
            map_outer_record(&active, true, 0),
        ]),
    )
    .into_iter()
    .map(|(key, _)| key)
    .collect::<Vec<_>>();
    assert_eq!(
        outer_keys,
        vec![
            map_outer_record(&active, false, 9),
            map_outer_record(&active, true, 0),
            map_outer_record(&active, true, 1),
        ]
    );
}

#[test]
fn map_and_list_equality_distinguish_descriptors_values_and_order() {
    let active = active_map_revision();
    let boolean_descriptor = TypeDescriptor::named(MAP_BOOLEAN);
    let integer_descriptor = TypeDescriptor::named(MAP_INTEGER);
    let map_descriptor = TypeDescriptor::map(
        boolean_descriptor.clone(),
        TypeDescriptor::named(MAP_BOOLEAN),
    )
    .unwrap();

    let value_map = RuntimeValue::map(
        &active,
        map_descriptor.clone(),
        vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(false))],
    )
    .unwrap();
    let different_value = RuntimeValue::map(
        &active,
        map_descriptor.clone(),
        vec![(RuntimeValue::Boolean(true), RuntimeValue::Boolean(true))],
    )
    .unwrap();
    assert_ne!(value_map, different_value);

    let different_descriptor = RuntimeValue::map(
        &active,
        TypeDescriptor::map(integer_descriptor, TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
        vec![(RuntimeValue::Integer(1), RuntimeValue::Boolean(false))],
    )
    .unwrap();
    assert_ne!(value_map, different_descriptor);

    let list_descriptor = TypeDescriptor::list(boolean_descriptor).unwrap();
    let forward_list = RuntimeValue::list(
        &active,
        list_descriptor.clone(),
        vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
    )
    .unwrap();
    let reversed_list = RuntimeValue::list(
        &active,
        list_descriptor.clone(),
        vec![RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)],
    )
    .unwrap();
    assert_ne!(forward_list, reversed_list);

    let duplicate_list = RuntimeValue::list(
        &active,
        list_descriptor.clone(),
        vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(true)],
    )
    .unwrap();
    assert_ne!(
        forward_list,
        RuntimeValue::list(&active, list_descriptor, vec![RuntimeValue::Boolean(true)],).unwrap()
    );
    assert_ne!(forward_list, duplicate_list);
}
fn active_object_opaque_collision_revision() -> ActiveDatabaseRevision {
    let standard = verified_standard_with_value_types(vec![
        standard_boolean_definition(),
        opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
    ]);
    let application_schema = SchemaId::from_bytes([0x57; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            application_schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            OPAQUE_TYPE,
            QualifiedSemanticName::new(["crm", "item"]).unwrap(),
            vec![],
        )],
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let application_content = "ab";
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
            DefinitionIdentity::ObjectType(OPAQUE_TYPE),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
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

#[test]
fn enum_with_record_identity_is_rejected_as_a_field_type_mismatch() {
    const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
    const OUTER_TYPE: TypeId = TypeId::from_bytes([0x30; 16]);
    const OUTER_FIELD: FieldId = FieldId::from_bytes([0x3b; 16]);
    let current = active_nested_record_revision_with_child_fields(vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "value",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
    ]);
    let identity_catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x45; 16]),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            INNER_TYPE,
            QualifiedSemanticName::new(["crm", "inner"]).unwrap(),
            ["lead"],
        )],
        vec![],
    )
    .unwrap();
    let identity_enum =
        RuntimeValue::Enum(EnumValue::new(&identity_catalogue, INNER_TYPE, "lead").unwrap());
    let error = RecordValue::new(
        &current,
        OUTER_TYPE,
        [(String::from("payload"), identity_enum)],
    )
    .unwrap_err();
    assert_eq!(
        error,
        RecordValueError::FieldTypeMismatch {
            record_type: OUTER_TYPE,
            field: OUTER_FIELD,
            expected: ResolvedType::named(INNER_TYPE),
            actual: ResolvedType::named(INNER_TYPE),
        }
    );
    assert_eq!(error.to_string(), "record field value has a type mismatch");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn legacy_typed_null_precedes_type_mismatch_at_the_same_list_element() {
    let active = active_record_revision();
    let list_boolean = TypeDescriptor::list(TypeDescriptor::named(STANDARD_BOOLEAN)).unwrap();
    let error = RuntimeValue::list(
        &active,
        list_boolean,
        vec![
            RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject)).unwrap(),
            RuntimeValue::Boolean(false),
        ],
    )
    .unwrap_err();
    let CollectionValueError::NullValueNotAccepted { path } = &error else {
        panic!("typed null must precede the type mismatch: {error}");
    };
    assert_eq!(
        path.segments(),
        &[CollectionValuePathSegment::ListElement(0)]
    );
}

#[test]
fn enum_nominal_mismatch_precedes_label_inactivity_at_the_same_element() {
    let active = active_record_revision();
    let wrong_id_catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x45; 16]),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            TypeId::from_bytes([0x6d; 16]),
            QualifiedSemanticName::new(["crm", "other"]).unwrap(),
            ["retired"],
        )],
        vec![],
    )
    .unwrap();
    let wrong_enum = RuntimeValue::Enum(
        EnumValue::new(
            &wrong_id_catalogue,
            TypeId::from_bytes([0x6d; 16]),
            "retired",
        )
        .unwrap(),
    );
    let list_enum = TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap();
    let error = RuntimeValue::list(&active, list_enum, vec![wrong_enum]).unwrap_err();
    let CollectionValueError::ValueTypeMismatch { path } = &error else {
        panic!("nominal mismatch must precede label inactivity: {error}");
    };
    assert_eq!(
        path.segments(),
        &[CollectionValuePathSegment::ListElement(0)]
    );
}

#[test]
fn map_descriptor_reports_the_key_child_before_an_unsupported_value() {
    let active = active_record_revision();
    let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
    let set_key = TypeDescriptor::set(boolean.clone()).unwrap();
    let stream_value = TypeDescriptor::stream(boolean).unwrap();
    let map_descriptor = TypeDescriptor::map(set_key.clone(), stream_value).unwrap();
    let error = RuntimeValue::map(&active, map_descriptor, vec![]).unwrap_err();
    let CollectionValueError::UnsupportedDescriptor { path, descriptor } = &error else {
        panic!("unsupported map key must precede the unsupported value: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::MapKeyChild]);
    assert_eq!(descriptor, &set_key);
}

#[test]
fn object_opaque_shared_identity_is_ambiguous_before_unsupported() {
    let active = active_object_opaque_collision_revision();
    let collision = TypeDescriptor::named(OPAQUE_TYPE);
    let list = TypeDescriptor::list(collision.clone()).unwrap();
    let error = RuntimeValue::list(&active, list, vec![]).unwrap_err();
    let CollectionValueError::AmbiguousNamedType { path, type_id } = &error else {
        panic!("object/opaque identity collision must be ambiguous: {error}");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(*type_id, OPAQUE_TYPE);
    assert_eq!(
        error.to_string(),
        "collection descriptor type is present in both application and standard catalogues"
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn record_caller_input_order_reports_the_first_supplied_stale_enum_error() {
    let active = active_record_revision();
    let stale_catalogue = enum_catalogue(&["retired"]);
    let stale = RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
    let error = RecordValue::new(
        &active,
        RECORD_TYPE,
        [
            (String::from("stage"), stale),
            (String::from("enabled"), RuntimeValue::Integer(1)),
        ],
    )
    .unwrap_err();
    assert_eq!(
        error,
        RecordValueError::InactiveEnumLabel {
            record_type: RECORD_TYPE,
            field: STAGE_FIELD,
            enum_type: ENUM_TYPE,
            label: String::from("retired"),
        }
    );
    assert_eq!(error.to_string(), "record enum field label is not active");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn runtime_value_node_boundary_is_exact() {
    let active = active_record_revision();
    let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
    let option_boolean = TypeDescriptor::option(boolean.clone()).unwrap();
    let list_of_option = TypeDescriptor::list(option_boolean.clone()).unwrap();
    let list_boolean = TypeDescriptor::list(boolean.clone()).unwrap();
    let map_boolean = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

    let subtree = RuntimeValue::option(
        &active,
        option_boolean.clone(),
        Some(RuntimeValue::Boolean(true)),
    )
    .unwrap();
    let empty_subtree = RuntimeValue::option(&active, option_boolean.clone(), None).unwrap();
    let mut boundary_elements = vec![subtree.clone(); 32_767];
    boundary_elements.push(empty_subtree.clone());
    let at_boundary =
        RuntimeValue::list(&active, list_of_option.clone(), boundary_elements).unwrap();
    let RuntimeValue::Constructed(at_boundary_value) = &at_boundary else {
        panic!("boundary list must be constructed");
    };
    let ConstructedValueKind::List(elements) = at_boundary_value.kind() else {
        panic!("boundary list must expose the list kind");
    };
    assert_eq!(elements.len(), 32_768);
    let RuntimeValue::Constructed(last) = &elements[32_767] else {
        panic!("the final boundary element must be constructed");
    };
    let ConstructedValueKind::Option(None) = last.kind() else {
        panic!("the final boundary element must be the empty option");
    };

    let overflow = RuntimeValue::list(&active, list_of_option, vec![subtree; 32_768]).unwrap_err();
    assert_eq!(
        overflow,
        CollectionValueError::TooManyNodes {
            maximum: MAX_RUNTIME_VALUE_NODES,
        }
    );
    assert_eq!(overflow.to_string(), "runtime value has too many nodes");
    assert!(std::error::Error::source(&overflow).is_none());

    let list_lower_bound = RuntimeValue::list(&active, list_boolean.clone(), {
        let mut entries = vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES];
        entries.push(RuntimeValue::Text("x".into()));
        entries
    })
    .unwrap_err();
    assert_eq!(
        list_lower_bound,
        CollectionValueError::TooManyNodes {
            maximum: MAX_RUNTIME_VALUE_NODES,
        }
    );

    let map_lower_bound = RuntimeValue::map(&active, map_boolean, {
        let mut entries = vec![
            (RuntimeValue::Boolean(true), RuntimeValue::Boolean(true));
            MAX_RUNTIME_VALUE_NODES / 2
        ];
        entries.push((RuntimeValue::Text("x".into()), RuntimeValue::Boolean(true)));
        entries
    })
    .unwrap_err();
    assert_eq!(
        map_lower_bound,
        CollectionValueError::TooManyNodes {
            maximum: MAX_RUNTIME_VALUE_NODES,
        }
    );
}

#[test]
fn constructed_equality_ignores_the_construction_route() {
    let active = active_map_revision();
    let boolean = TypeDescriptor::named(MAP_BOOLEAN);
    let option_boolean = TypeDescriptor::option(boolean.clone()).unwrap();

    let first = RuntimeValue::option(
        &active,
        option_boolean.clone(),
        Some(RuntimeValue::Boolean(true)),
    )
    .unwrap();
    let second = RuntimeValue::option(
        &active,
        option_boolean.clone(),
        Some(RuntimeValue::Boolean(true)),
    )
    .unwrap();
    assert_eq!(first, second);

    let map_descriptor = TypeDescriptor::map(boolean.clone(), boolean).unwrap();
    let map_first = RuntimeValue::map(
        &active,
        map_descriptor.clone(),
        vec![
            (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
            (RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap();
    let map_second = RuntimeValue::map(
        &active,
        map_descriptor,
        vec![
            (RuntimeValue::Boolean(false), RuntimeValue::Boolean(true)),
            (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
        ],
    )
    .unwrap();
    assert_eq!(map_first, map_second);
}

#[test]
fn stale_and_identical_revision_semantics_are_exact() {
    const INNER_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);
    const A_FIELD: FieldId = FieldId::from_bytes([0x6a; 16]);
    const B_FIELD: FieldId = FieldId::from_bytes([0x6b; 16]);
    let field_a = RecordValueFieldDefinition::try_new_descriptor(
        A_FIELD,
        "a",
        0,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let field_b = RecordValueFieldDefinition::try_new_descriptor(
        B_FIELD,
        "b",
        1,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();

    let old = active_nested_record_revision_with_seed(vec![field_a.clone()], 0x58, 0x64);
    let current = active_nested_record_revision_with_seed(vec![field_a.clone()], 0x59, 0x65);
    let identical = RecordValue::new(
        &old,
        INNER_TYPE,
        [(String::from("a"), RuntimeValue::Boolean(true))],
    )
    .unwrap();

    let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap();
    assert!(
        RuntimeValue::list(
            &current,
            list_descriptor.clone(),
            vec![RuntimeValue::Record(identical.clone())],
        )
        .is_ok()
    );
    let option_descriptor = TypeDescriptor::option(TypeDescriptor::named(INNER_TYPE)).unwrap();
    assert!(
        RuntimeValue::option(
            &current,
            option_descriptor.clone(),
            Some(RuntimeValue::Record(identical.clone())),
        )
        .is_ok()
    );
    let map_descriptor = TypeDescriptor::map(
        TypeDescriptor::named(INNER_TYPE),
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    assert!(
        RuntimeValue::map(
            &current,
            map_descriptor.clone(),
            vec![(
                RuntimeValue::Record(identical.clone()),
                RuntimeValue::Boolean(true)
            )],
        )
        .is_ok()
    );

    let retired_active = active_record_revision();
    let retired_enum = RuntimeValue::Enum(
        EnumValue::new(&enum_catalogue(&["retired"]), ENUM_TYPE, "retired").unwrap(),
    );
    let enum_list = TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap();
    let error = RuntimeValue::list(&retired_active, enum_list, vec![retired_enum]).unwrap_err();
    let CollectionValueError::InactiveValue { path } = &error else {
        panic!("retired enum label must be inactive: {error}");
    };
    assert_eq!(
        path.segments(),
        &[CollectionValuePathSegment::ListElement(0)]
    );

    let old_two =
        active_nested_record_revision_with_child_fields(vec![field_a.clone(), field_b.clone()]);
    let removed = active_nested_record_revision_with_child_fields(vec![field_a.clone()]);
    let old_record = RecordValue::new(
        &old_two,
        INNER_TYPE,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap();
    let option_error = RuntimeValue::option(
        &removed,
        option_descriptor,
        Some(RuntimeValue::Record(old_record.clone())),
    )
    .unwrap_err();
    let CollectionValueError::InactiveValue { path } = &option_error else {
        panic!("removed trailing field must be inactive in an option: {option_error}");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::OptionChild,
            CollectionValuePathSegment::RecordField(B_FIELD),
        ]
    );
    let map_error = RuntimeValue::map(
        &removed,
        map_descriptor,
        vec![(
            RuntimeValue::Record(old_record.clone()),
            RuntimeValue::Boolean(true),
        )],
    )
    .unwrap_err();
    let CollectionValueError::InactiveValue { path } = &map_error else {
        panic!("removed trailing field must be inactive as a map key: {map_error}");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::MapKey(0),
            CollectionValuePathSegment::RecordField(B_FIELD),
        ]
    );
    let map_value_error = RuntimeValue::map(
        &removed,
        TypeDescriptor::map(
            TypeDescriptor::named(STANDARD_BOOLEAN),
            TypeDescriptor::named(INNER_TYPE),
        )
        .unwrap(),
        vec![(
            RuntimeValue::Boolean(true),
            RuntimeValue::Record(old_record.clone()),
        )],
    )
    .unwrap_err();
    let CollectionValueError::InactiveValue { path } = &map_value_error else {
        panic!("removed trailing field must be inactive as a map value: {map_value_error}");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::MapValue(0),
            CollectionValuePathSegment::RecordField(B_FIELD),
        ]
    );

    let swapped_id_a = RecordValueFieldDefinition::try_new_descriptor(
        B_FIELD,
        "a",
        0,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let swapped_id_b = RecordValueFieldDefinition::try_new_descriptor(
        A_FIELD,
        "b",
        1,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let reordered_current =
        active_nested_record_revision_with_child_fields(vec![swapped_id_a, swapped_id_b]);
    let reordered_error = RuntimeValue::list(
        &reordered_current,
        list_descriptor,
        vec![RuntimeValue::Record(old_record)],
    )
    .unwrap_err();
    let CollectionValueError::InactiveValue { path } = &reordered_error else {
        panic!("reordered field identities must be inactive: {reordered_error}");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::ListElement(0),
            CollectionValuePathSegment::RecordField(B_FIELD),
        ]
    );

    let integer_standard = verified_standard_with_value_types(vec![
        standard_boolean_definition(),
        map_standard_primitive(MAP_INTEGER, "integer", "orna.kernel.value.integer@1"),
    ]);
    let boolean_field = RecordValueFieldDefinition::try_new_descriptor(
        A_FIELD,
        "a",
        0,
        TypeDescriptor::named(STANDARD_BOOLEAN),
    )
    .unwrap();
    let integer_field = RecordValueFieldDefinition::try_new_descriptor(
        A_FIELD,
        "a",
        0,
        TypeDescriptor::named(MAP_INTEGER),
    )
    .unwrap();
    let boolean_active = active_nested_record_revision_with_standard_and_seed(
        vec![boolean_field],
        integer_standard.clone(),
        0x5a,
        0x66,
    );
    let integer_active = active_nested_record_revision_with_standard_and_seed(
        vec![integer_field],
        integer_standard,
        0x5b,
        0x67,
    );
    let boolean_record = RecordValue::new(
        &boolean_active,
        INNER_TYPE,
        [(String::from("a"), RuntimeValue::Boolean(true))],
    )
    .unwrap();
    let changed_error = RuntimeValue::list(
        &integer_active,
        TypeDescriptor::list(TypeDescriptor::named(INNER_TYPE)).unwrap(),
        vec![RuntimeValue::Record(boolean_record)],
    )
    .unwrap_err();
    let CollectionValueError::ValueTypeMismatch { path } = &changed_error else {
        panic!("changed record field type must be a type mismatch: {changed_error}");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::ListElement(0),
            CollectionValuePathSegment::RecordField(A_FIELD),
        ]
    );
    assert_eq!(
        changed_error.to_string(),
        "collection value has a type mismatch"
    );
    assert!(std::error::Error::source(&changed_error).is_none());
}

fn deep_record_chain_revision_error() -> crate::canonical_hash::CanonicalHashError {
    let standard = verified_standard_with_value_types(vec![standard_boolean_definition()]);
    let application_schema = SchemaId::from_bytes([0x57; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
    let mut records = Vec::new();
    for index in 0..34_u8 {
        let record_type = TypeId::from_bytes([0x20 + index; 16]);
        let field_id = FieldId::from_bytes([0x80 + index; 16]);
        let target = if index == 33 {
            STANDARD_BOOLEAN
        } else {
            TypeId::from_bytes([0x20 + index + 1; 16])
        };
        records.push(RecordValueTypeDefinition::new(
            record_type,
            QualifiedSemanticName::new(["crm", &format!("chain_{index}")]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    field_id,
                    if index == 33 { "value" } else { "next" },
                    0,
                    TypeDescriptor::named(target),
                )
                .unwrap(),
            ],
        ));
    }
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            application_schema,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![],
        records,
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let source_unit = SourceUnitId::from_bytes([0x63; 16]);
    let mut identities = vec![DefinitionIdentity::Schema(application_schema)];
    for index in 0..34_u8 {
        let record_type = TypeId::from_bytes([0x20 + index; 16]);
        let field_id = FieldId::from_bytes([0x80 + index; 16]);
        identities.push(DefinitionIdentity::ValueType(record_type));
        identities.push(DefinitionIdentity::Field {
            owner: record_type,
            field: field_id,
        });
    }
    let origins = identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap_err()
}

#[test]
fn descriptor_and_record_nesting_depth_boundaries_are_exact() {
    let active = active_record_revision();
    let mut descriptor = TypeDescriptor::named(STANDARD_BOOLEAN);
    let mut value = RuntimeValue::Boolean(true);
    for _ in 0..32 {
        descriptor = TypeDescriptor::option(descriptor).unwrap();
        value = RuntimeValue::option(&active, descriptor.clone(), Some(value)).unwrap();
    }
    assert_eq!(value.runtime_type(), RuntimeType::Constructed(&descriptor));
    let too_deep = TypeDescriptor::option(descriptor).unwrap_err();
    assert_eq!(
        too_deep,
        crate::types::TypeDescriptorError::TooDeep {
            maximum: 32,
            actual: 33,
        }
    );
    assert_eq!(too_deep.to_string(), "type descriptor is too deep");
    assert!(std::error::Error::source(&too_deep).is_none());

    let error = deep_record_chain_revision_error();
    match error {
        crate::canonical_hash::CanonicalHashError::RecordValueNestingTooDeep {
            record_value_type,
            field,
            nested_record_value_type,
            maximum,
            actual,
        } => {
            assert_eq!(record_value_type, TypeId::from_bytes([0x20 + 32; 16]));
            assert_eq!(field, FieldId::from_bytes([0x80 + 32; 16]));
            assert_eq!(
                nested_record_value_type,
                TypeId::from_bytes([0x20 + 33; 16])
            );
            assert_eq!(maximum, 32);
            assert_eq!(actual, 33);
        }
        other => panic!("deep record chain must fail as nesting too deep: {other:?}"),
    }
    assert_eq!(
        error.to_string(),
        "record value nesting exceeds the maximum depth"
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn collection_error_variants_preserve_exact_display_and_source() {
    let active = active_record_revision();
    let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
    let option_desc = TypeDescriptor::option(boolean.clone()).unwrap();
    let list_boolean = TypeDescriptor::list(boolean.clone()).unwrap();
    let list_enum = TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap();
    let map_boolean = TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap();

    let cases = [
        (
            RuntimeValue::list(&active, option_desc, vec![]).unwrap_err(),
            "collection descriptor has the wrong outer constructor",
        ),
        (
            RuntimeValue::list(
                &active,
                TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([0x6c; 16])))
                    .unwrap(),
                vec![],
            )
            .unwrap_err(),
            "collection descriptor is not supported",
        ),
        (
            RuntimeValue::list(
                &active,
                list_boolean.clone(),
                vec![RuntimeValue::Boolean(true); MAX_RUNTIME_VALUE_NODES],
            )
            .unwrap_err(),
            "runtime value has too many nodes",
        ),
        (
            RuntimeValue::list(
                &active,
                list_boolean.clone(),
                vec![RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap()],
            )
            .unwrap_err(),
            "collection values cannot contain legacy typed NULL",
        ),
        (
            RuntimeValue::list(&active, list_boolean, vec![RuntimeValue::Text("x".into())])
                .unwrap_err(),
            "collection value has a type mismatch",
        ),
        (
            RuntimeValue::list(
                &active,
                list_enum,
                vec![RuntimeValue::Enum(
                    EnumValue::new(&enum_catalogue(&["retired"]), ENUM_TYPE, "retired").unwrap(),
                )],
            )
            .unwrap_err(),
            "collection value is not active",
        ),
        (
            RuntimeValue::map(
                &active,
                map_boolean,
                vec![
                    (RuntimeValue::Boolean(true), RuntimeValue::Boolean(true)),
                    (RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)),
                ],
            )
            .unwrap_err(),
            "map contains a duplicate key",
        ),
    ];
    for (error, expected_display) in cases {
        assert_eq!(error.to_string(), expected_display);
        assert!(std::error::Error::source(&error).is_none());
    }

    let ambiguous = RuntimeValue::list(
        &active_record_revision_with_opaque_contract(
            TypeId::from_bytes([0x49; 16]),
            OPAQUE_CONTRACT,
        ),
        TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([0x49; 16]))).unwrap(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(
        ambiguous.to_string(),
        "collection descriptor type is present in both application and standard catalogues"
    );
    assert!(std::error::Error::source(&ambiguous).is_none());
}

proptest::proptest! {
    #[test]
    fn constructed_constructors_never_panic_on_bounded_public_input(
        descriptor_choice in 0usize..10,
        value_choice in 0usize..8,
        count in 0usize..6,
    ) {
        let active = active_record_revision();
        let boolean = TypeDescriptor::named(STANDARD_BOOLEAN);
        let descriptors = vec![
            TypeDescriptor::option(boolean.clone()).unwrap(),
            TypeDescriptor::list(boolean.clone()).unwrap(),
            TypeDescriptor::map(boolean.clone(), boolean.clone()).unwrap(),
            TypeDescriptor::option(TypeDescriptor::named(TypeId::from_bytes([0x6c; 16])))
                .unwrap(),
            TypeDescriptor::list(TypeDescriptor::named(ENUM_TYPE)).unwrap(),
            TypeDescriptor::set(boolean.clone()).unwrap(),
            TypeDescriptor::stream(boolean.clone()).unwrap(),
            TypeDescriptor::option(TypeDescriptor::list(boolean.clone()).unwrap()).unwrap(),
            TypeDescriptor::map(
                TypeDescriptor::named(ENUM_TYPE),
                TypeDescriptor::named(TypeId::from_bytes([0x6c; 16])),
            )
            .unwrap(),
            TypeDescriptor::list(
                TypeDescriptor::option(TypeDescriptor::named(ENUM_TYPE)).unwrap(),
            )
            .unwrap(),
        ];
        let values = [
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(1),
            RuntimeValue::Text("x".into()),
            RuntimeValue::Bytes(vec![1]),
            RuntimeValue::Enum(
                EnumValue::new(&enum_catalogue(&["lead"]), ENUM_TYPE, "lead").unwrap(),
            ),
            RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
            RuntimeValue::Reference {
                target: TypeId::from_bytes([0x41; 16]),
                object: ObjectId::from_bytes([0x42; 16]),
            },
            RuntimeValue::option(
                &active,
                TypeDescriptor::option(boolean).unwrap(),
                Some(RuntimeValue::Boolean(false)),
            )
            .unwrap(),
        ];
        let descriptor = descriptors[descriptor_choice].clone();
        let value = values[value_choice].clone();
        let _ = RuntimeValue::option(&active, descriptor.clone(), Some(value.clone()));
        let _ = RuntimeValue::list(
            &active,
            descriptor.clone(),
            vec![value.clone(); count],
        );
        let _ = RuntimeValue::map(
            &active,
            descriptor.clone(),
            (0..count)
                .map(|index| {
                    (
                        values[(value_choice + index) % values.len()].clone(),
                        value.clone(),
                    )
                })
                .collect(),
        );
    }
}

#[test]
fn admitted_leaf_values_construct_in_option_list_and_map() {
    let active = active_map_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let record = RecordValue::new(
        &active,
        MAP_FLAT,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Integer(1)),
        ],
    )
    .unwrap();
    let leaf_cases = [
        (
            TypeDescriptor::named(MAP_BOOLEAN),
            RuntimeValue::Boolean(true),
        ),
        (TypeDescriptor::named(MAP_INTEGER), RuntimeValue::Integer(1)),
        (TypeDescriptor::named(MAP_BIGINT), RuntimeValue::BigInt(2)),
        (
            TypeDescriptor::named(MAP_FLOAT),
            RuntimeValue::Float(RuntimeFloat::new(3.5).unwrap()),
        ),
        (
            TypeDescriptor::named(MAP_TEXT),
            RuntimeValue::Text("t".into()),
        ),
        (
            TypeDescriptor::named(MAP_BYTES),
            RuntimeValue::Bytes(vec![1, 2]),
        ),
        (
            TypeDescriptor::named(MAP_APP_ENUM),
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), MAP_APP_ENUM, "low").unwrap()),
        ),
        (
            TypeDescriptor::named(MAP_STD_ENUM),
            RuntimeValue::Enum(
                EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "alpha").unwrap(),
            ),
        ),
        (
            TypeDescriptor::named(MAP_FLAT),
            RuntimeValue::Record(record),
        ),
        (
            TypeDescriptor::reference(MAP_OBJECT),
            RuntimeValue::Reference {
                target: MAP_OBJECT,
                object: ObjectId::from_bytes([0x01; 16]),
            },
        ),
    ];
    for (descriptor, value) in leaf_cases {
        let option = RuntimeValue::option(
            &active,
            TypeDescriptor::option(descriptor.clone()).unwrap(),
            Some(value.clone()),
        )
        .expect("admitted leaf must construct an option");
        let RuntimeValue::Constructed(option_value) = &option else {
            panic!("admitted leaf option must be constructed");
        };
        let ConstructedValueKind::Option(Some(child)) = option_value.kind() else {
            panic!("admitted leaf option must expose its child");
        };
        assert_eq!(child, &value);

        let list = RuntimeValue::list(
            &active,
            TypeDescriptor::list(descriptor.clone()).unwrap(),
            vec![value.clone()],
        )
        .expect("admitted leaf must construct a list");
        let RuntimeValue::Constructed(list_value) = &list else {
            panic!("admitted leaf list must be constructed");
        };
        let ConstructedValueKind::List(elements) = list_value.kind() else {
            panic!("admitted leaf list must expose elements");
        };
        assert_eq!(elements, std::slice::from_ref(&value));

        let map = RuntimeValue::map(
            &active,
            TypeDescriptor::map(descriptor.clone(), TypeDescriptor::named(MAP_BOOLEAN)).unwrap(),
            vec![(value.clone(), RuntimeValue::Boolean(true))],
        )
        .expect("admitted leaf must construct a map key");
        let RuntimeValue::Constructed(map_value) = &map else {
            panic!("admitted leaf map must be constructed");
        };
        let ConstructedValueKind::Map(entries) = map_value.kind() else {
            panic!("admitted leaf map must expose entries");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, value);
    }
}

#[test]
fn record_value_accepts_a_verified_standard_enum_field_in_declaration_order() {
    let active = active_map_revision();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("the map fixture pins a verified standard library");
    let mode = EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "beta")
        .expect("the verified standard enum declares beta");

    let record = RecordValue::new(
        &active,
        MAP_STD_ENUM_RECORD,
        [
            (String::from("mode"), RuntimeValue::Enum(mode.clone())),
            (String::from("enabled"), RuntimeValue::Boolean(true)),
        ],
    )
    .expect("a verified standard enum is an admitted record field type");

    assert_eq!(record.record_type(), MAP_STD_ENUM_RECORD);
    assert_eq!(record.fields().len(), 2);
    assert_eq!(record.fields()[0], RuntimeValue::Boolean(true));
    assert_eq!(record.fields()[1], RuntimeValue::Enum(mode.clone()));
    let RuntimeValue::Enum(mode_value) = &record.fields()[1] else {
        panic!("the mode field must retain its enum value");
    };
    assert_eq!(mode_value.enum_type(), MAP_STD_ENUM);
    assert_eq!(mode_value.label(), "beta");
    assert_eq!(
        RuntimeValue::Record(record).runtime_type(),
        RuntimeType::Flat(ResolvedType::named(MAP_STD_ENUM_RECORD))
    );
}

#[test]
fn record_value_rejects_a_stale_verified_standard_enum_label() {
    let active = active_map_revision();
    let stale = RuntimeValue::Enum(
        EnumValue::new(
            &standard_enum_catalogue(&["retired"]),
            MAP_STD_ENUM,
            "retired",
        )
        .expect("the stale standard catalogue declares retired"),
    );

    let error = RecordValue::new(
        &active,
        MAP_STD_ENUM_RECORD,
        [
            (String::from("enabled"), RuntimeValue::Boolean(true)),
            (String::from("mode"), stale),
        ],
    )
    .expect_err("a stale standard enum label must not enter a current record");

    assert_eq!(
        error,
        RecordValueError::InactiveEnumLabel {
            record_type: MAP_STD_ENUM_RECORD,
            field: MAP_STD_ENUM_FIELD,
            enum_type: MAP_STD_ENUM,
            label: String::from("retired"),
        }
    );
}

#[test]
fn verified_standard_enum_value_rejects_an_undeclared_label() {
    let active = active_map_revision();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("the map fixture pins a verified standard library");

    assert_eq!(
        EnumValue::new(standard.catalogue(), MAP_STD_ENUM, "retired"),
        Err(EnumValueError::UndeclaredLabel {
            enum_type: MAP_STD_ENUM,
            label: String::from("retired"),
        })
    );
}

#[test]
fn stale_child_record_value_is_rejected_with_exact_inactive_nested_record_error() {
    let child_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let outer_field = FieldId::from_bytes([0x3b; 16]);
    let old = active_nested_record_revision();
    let current = active_nested_record_revision_with_child_fields(vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "value",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3c; 16]),
            "checked",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
    ]);
    let old_child = RecordValue::new(
        &old,
        child_type,
        [(String::from("value"), RuntimeValue::Boolean(true))],
    )
    .expect("the child must construct under the old revision");

    let error = RecordValue::new(
        &current,
        outer_type,
        [(String::from("payload"), RuntimeValue::Record(old_child))],
    )
    .expect_err("a stale child must be rejected by the current outer");
    assert_eq!(
        error,
        RecordValueError::InactiveNestedRecord {
            record_type: outer_type,
            field: outer_field,
            nested_record_type: child_type,
        }
    );
    assert_eq!(error.to_string(), "nested record field value is not active");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn nominal_mismatch_precedes_recursive_activity_checking() {
    let active = active_nested_record_revision();
    let child_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let outer_field = FieldId::from_bytes([0x3b; 16]);
    let inner = RecordValue::new(
        &active,
        child_type,
        [(String::from("value"), RuntimeValue::Boolean(true))],
    )
    .expect("the flat inner record must construct");
    let outer = RecordValue::new(
        &active,
        outer_type,
        [(String::from("payload"), RuntimeValue::Record(inner))],
    )
    .expect("a valid nested record must construct");

    let error = RecordValue::new(
        &active,
        outer_type,
        [(String::from("payload"), RuntimeValue::Record(outer))],
    )
    .expect_err("a nominal mismatch must be rejected");
    assert_eq!(
        error,
        RecordValueError::FieldTypeMismatch {
            record_type: outer_type,
            field: outer_field,
            expected: ResolvedType::named(child_type),
            actual: ResolvedType::named(outer_type),
        }
    );
}

#[test]
fn nested_record_value_carries_no_creation_provenance_identity() {
    let child_fields = vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "value",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
    ];
    let old = active_nested_record_revision();
    let fresh = active_nested_record_revision_with_seed(child_fields, 0x77, 0x78);
    let child_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let old_child = RecordValue::new(
        &old,
        child_type,
        [(String::from("value"), RuntimeValue::Boolean(true))],
    )
    .expect("the child must construct under the old revision");

    let outer = RecordValue::new(
        &fresh,
        outer_type,
        [(
            String::from("payload"),
            RuntimeValue::Record(old_child.clone()),
        )],
    )
    .expect("a semantically identical revision must accept the child");
    assert_eq!(outer.record_type(), outer_type);
    let [RuntimeValue::Record(inner_value)] = outer.fields() else {
        panic!("outer payload must hold the child record value");
    };
    assert_eq!(inner_value, &old_child);
    assert_eq!(
        RuntimeValue::Record(outer).runtime_type(),
        RuntimeType::Flat(ResolvedType::named(outer_type))
    );
}

#[test]
fn nested_record_value_chain_walks_32_edges_to_the_boolean_leaf() {
    let active = active_record_chain_revision();
    let root_type = TypeId::from_bytes([0x20; 16]);
    let leaf_type = TypeId::from_bytes([0x40; 16]);
    let leaf = RecordValue::new(
        &active,
        leaf_type,
        [(String::from("value"), RuntimeValue::Boolean(true))],
    )
    .expect("the leaf record must construct");
    let mut value = RuntimeValue::Record(leaf);
    for index in (0..32).rev() {
        let record_type = TypeId::from_bytes([0x20 + index; 16]);
        value = RuntimeValue::Record(
            RecordValue::new(&active, record_type, [(String::from("next"), value)])
                .expect("each parent record must construct"),
        );
    }
    let RuntimeValue::Record(root) = value else {
        panic!("the root must be a record value");
    };
    assert_eq!(root.record_type(), root_type);

    let mut current = &root;
    let mut edges = 0;
    loop {
        let [field] = current.fields() else {
            panic!("each chain record must hold exactly one field");
        };
        match field {
            RuntimeValue::Record(next) => {
                edges += 1;
                current = next;
            }
            RuntimeValue::Boolean(stored) => {
                assert!(*stored, "the leaf must hold Boolean true");
                break;
            }
            other => panic!("unexpected chain leaf value {other:?}"),
        }
    }
    assert_eq!(edges, 32, "the root must reach the leaf through 32 edges");
}

#[test]
fn reversed_child_declaration_order_is_inactive_in_the_current_revision() {
    let child_type = TypeId::from_bytes([0x31; 16]);
    let outer_type = TypeId::from_bytes([0x30; 16]);
    let old_fields = vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3c; 16]),
            "b",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
    ];
    let current_fields = vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3c; 16]),
            "b",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "a",
            1,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
    ];
    let old = active_nested_record_revision_with_child_fields(old_fields);
    let current = active_nested_record_revision_with_child_fields(current_fields);
    let old_child = RecordValue::new(
        &old,
        child_type,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(false)),
        ],
    )
    .expect("the child must construct under the old revision");

    let error = RecordValue::new(
        &current,
        outer_type,
        [(String::from("payload"), RuntimeValue::Record(old_child))],
    )
    .expect_err("reversed declaration order must be inactive in the current revision");
    assert_eq!(
        error,
        RecordValueError::InactiveNestedRecord {
            record_type: outer_type,
            field: FieldId::from_bytes([0x3b; 16]),
            nested_record_type: child_type,
        }
    );
}
