//! Record values, function arguments, result rows, and Inspector carriers.

use super::*;
#[test]
fn record_values_validate_named_fields_and_store_declaration_order() {
    let active = active_record_revision();
    let stage =
        RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap());

    let record = RecordValue::new(
        &active,
        RECORD_TYPE,
        [
            (String::from("stage"), stage.clone()),
            (String::from("enabled"), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap();

    assert_eq!(record.record_type(), RECORD_TYPE);
    assert_eq!(
        record.fields(),
        &[RuntimeValue::Boolean(true), stage.clone()]
    );
    assert_eq!(
        RuntimeValue::Record(record).runtime_type(),
        RuntimeType::Flat(ResolvedType::named(RECORD_TYPE))
    );
}

#[test]
fn record_values_require_an_active_nominal_type_and_exact_field_names() {
    let active = active_record_revision();
    let unknown_type = TypeId::from_bytes([0x60; 16]);
    assert_eq!(
        RecordValue::new(&active, unknown_type, Vec::<(String, RuntimeValue)>::new(),),
        Err(RecordValueError::UnknownType {
            record_type: unknown_type,
        })
    );

    assert_eq!(
        RecordValue::new(
            &active,
            RECORD_TYPE,
            [(String::from("Enabled"), RuntimeValue::Boolean(true))],
        ),
        Err(RecordValueError::UnknownField {
            record_type: RECORD_TYPE,
            name: String::from("Enabled"),
        })
    );
}

#[test]
fn record_values_require_every_declared_field_exactly_once() {
    let active = active_record_revision();
    assert_eq!(
        RecordValue::new(
            &active,
            RECORD_TYPE,
            [(String::from("enabled"), RuntimeValue::Boolean(true))],
        ),
        Err(RecordValueError::MissingField {
            record_type: RECORD_TYPE,
            field: STAGE_FIELD,
        })
    );

    assert_eq!(
        RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (String::from("enabled"), RuntimeValue::Boolean(false)),
            ],
        ),
        Err(RecordValueError::DuplicateField {
            record_type: RECORD_TYPE,
            field: ENABLED_FIELD,
        })
    );
}

#[test]
fn record_values_reject_null_wrong_type_and_stale_enum_fields() {
    let active = active_record_revision();
    assert_eq!(
        RecordValue::new(
            &active,
            RECORD_TYPE,
            [(
                String::from("enabled"),
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
            )],
        ),
        Err(RecordValueError::NullField {
            record_type: RECORD_TYPE,
            field: ENABLED_FIELD,
        })
    );

    assert_eq!(
        RecordValue::new(
            &active,
            RECORD_TYPE,
            [(String::from("enabled"), RuntimeValue::Integer(1))],
        ),
        Err(RecordValueError::FieldTypeMismatch {
            record_type: RECORD_TYPE,
            field: ENABLED_FIELD,
            expected: ResolvedType::scalar(StandardScalar::Boolean),
            actual: ResolvedType::scalar(StandardScalar::Integer),
        })
    );

    let stale_catalogue = enum_catalogue(&["retired"]);
    let stale = RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
    assert_eq!(
        RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (String::from("stage"), stale),
            ],
        ),
        Err(RecordValueError::InactiveEnumLabel {
            record_type: RECORD_TYPE,
            field: STAGE_FIELD,
            enum_type: ENUM_TYPE,
            label: String::from("retired"),
        })
    );
}

#[test]
fn record_values_enter_server_results_but_not_the_argument_subset() {
    let active = active_record_revision();
    let record = RecordValue::new(
        &active,
        RECORD_TYPE,
        [
            (String::from("enabled"), RuntimeValue::Boolean(true)),
            (
                String::from("stage"),
                RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap()),
            ),
        ],
    )
    .unwrap();
    let parameter = ParameterId::from_bytes([0x61; 16]);
    assert_eq!(
        FunctionArgument::new(parameter, RuntimeValue::Record(record.clone())),
        Err(FunctionArgumentError::RecordValueNotAccepted {
            parameter,
            record_type: RECORD_TYPE,
        })
    );
    let expected = RuntimeValue::Record(record);
    let rows = ResultRows::new(
        [column("status", ResolvedType::named(RECORD_TYPE), false)],
        [ResultRow::new([expected.clone()])],
    )
    .unwrap();
    assert_eq!(rows.rows()[0].values(), &[expected]);
}

#[test]
fn record_value_equality_is_nominal_across_semantically_identical_revisions() {
    let active = active_record_revision();
    let fields = || {
        [
            (String::from("enabled"), RuntimeValue::Boolean(true)),
            (
                String::from("stage"),
                RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap()),
            ),
        ]
    };
    let record = RecordValue::new(&active, RECORD_TYPE, fields()).unwrap();
    assert_eq!(
        RecordValue::new(&active, RECORD_TYPE, fields()).unwrap(),
        record
    );

    let other_type = TypeId::from_bytes([0x62; 16]);
    let other_active = active_record_revision_with_type(other_type);
    let other = RecordValue::new(
        &other_active,
        other_type,
        [
            (String::from("enabled"), RuntimeValue::Boolean(true)),
            (
                String::from("stage"),
                RuntimeValue::Enum(
                    EnumValue::new(other_active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                ),
            ),
        ],
    )
    .unwrap();
    assert_ne!(record, other);

    // Equality must not bind a creation-revision identity: the same
    // nominal type and field sequence compare equal across revisions
    // with different source and catalogue revision IDs.
    let child_type = TypeId::from_bytes([0x31; 16]);
    let single_field = vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3a; 16]),
            "a",
            0,
            TypeDescriptor::named(STANDARD_BOOLEAN),
        )
        .unwrap(),
    ];
    let old = active_nested_record_revision_with_child_fields(single_field.clone());
    let fresh = active_nested_record_revision_with_seed(single_field, 0x77, 0x78);
    let left = RecordValue::new(
        &old,
        child_type,
        [(String::from("a"), RuntimeValue::Boolean(true))],
    )
    .unwrap();
    let right = RecordValue::new(
        &fresh,
        child_type,
        [(String::from("a"), RuntimeValue::Boolean(true))],
    )
    .unwrap();
    assert_eq!(
        left, right,
        "equality must ignore the creation-revision identity"
    );

    // A reversed field-ID declaration sequence with identical positional
    // Boolean values compares unequal.
    let ab_fields = vec![
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
    let ba_fields = vec![
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
    let ab = RecordValue::new(
        &active_nested_record_revision_with_child_fields(ab_fields.clone()),
        child_type,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(false)),
        ],
    )
    .unwrap();
    let ba = RecordValue::new(
        &active_nested_record_revision_with_child_fields(ba_fields),
        child_type,
        [
            (String::from("a"), RuntimeValue::Boolean(false)),
            (String::from("b"), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap();
    assert_ne!(
        ab, ba,
        "reversed field-ID declaration order must compare unequal"
    );

    // One replaced field ID with the same names, ordinals, and values
    // compares unequal.
    let replaced_fields = vec![
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([0x3d; 16]),
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
    let original = RecordValue::new(
        &active_nested_record_revision_with_child_fields(ab_fields),
        child_type,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(false)),
        ],
    )
    .unwrap();
    let replaced = RecordValue::new(
        &active_nested_record_revision_with_child_fields(replaced_fields),
        child_type,
        [
            (String::from("a"), RuntimeValue::Boolean(true)),
            (String::from("b"), RuntimeValue::Boolean(false)),
        ],
    )
    .unwrap();
    assert_ne!(
        original, replaced,
        "a replaced field ID must compare unequal"
    );
}

#[test]
fn accepts_every_current_non_null_runtime_value_as_a_function_argument() {
    let catalogue = enum_catalogue(&["lead", "qualified"]);
    let values = vec![
        RuntimeValue::Boolean(true),
        RuntimeValue::Integer(-7),
        RuntimeValue::BigInt(8),
        RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
        RuntimeValue::Text("value".into()),
        RuntimeValue::Bytes(vec![1, 2, 3]),
        RuntimeValue::Reference {
            target: TARGET,
            object: OBJECT,
        },
        RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap()),
    ];

    for (index, value) in values.into_iter().enumerate() {
        let parameter = ParameterId::from_bytes([index as u8; 16]);
        let argument = FunctionArgument::new(parameter, value.clone()).unwrap();
        assert_eq!(argument.parameter(), parameter);
        assert_eq!(argument.value(), &value);
    }
}

#[test]
fn enum_values_require_an_active_type_and_exact_declared_label() {
    let catalogue = enum_catalogue(&["lead", "owner's", "customer"]);
    let value = EnumValue::new(&catalogue, ENUM_TYPE, "owner's").unwrap();

    assert_eq!(value.enum_type(), ENUM_TYPE);
    assert_eq!(value.label(), "owner's");
    assert_eq!(
        RuntimeValue::Enum(value.clone()).runtime_type(),
        RuntimeType::Flat(ResolvedType::named(ENUM_TYPE))
    );
    assert_eq!(value, value.clone());

    let unknown = TypeId::from_bytes([0x46; 16]);
    let error = EnumValue::new(&catalogue, unknown, "lead").unwrap_err();
    assert_eq!(error, EnumValueError::UnknownType { enum_type: unknown });
    assert_eq!(error.to_string(), "enum type is not active");

    let error = EnumValue::new(&catalogue, ENUM_TYPE, "Lead").unwrap_err();
    assert_eq!(
        error,
        EnumValueError::UndeclaredLabel {
            enum_type: ENUM_TYPE,
            label: String::from("Lead"),
        }
    );
    assert_eq!(
        error.to_string(),
        "enum label is not declared by the active type"
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn result_rows_accept_exact_enum_values_and_typed_nulls() {
    let catalogue = enum_catalogue(&["lead", "qualified"]);
    let enum_type = ResolvedType::named(ENUM_TYPE);
    let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap());
    let rows = ResultRows::new(
        [
            column("stage", enum_type, false),
            column("previous_stage", enum_type, true),
        ],
        [ResultRow::new([
            value.clone(),
            RuntimeValue::null(enum_type).unwrap(),
        ])],
    )
    .unwrap();

    assert_eq!(rows.rows()[0].values()[0], value);
    assert!(rows.rows()[0].values()[1].is_null());
}

#[test]
fn rejects_typed_null_function_arguments_with_parameter_and_type() {
    let parameter = ParameterId::from_bytes([0x43; 16]);
    let resolved_type = ResolvedType::reference(TARGET);
    let value = RuntimeValue::null(resolved_type).unwrap();

    let error = FunctionArgument::new(parameter, value).unwrap_err();
    assert_eq!(
        error,
        FunctionArgumentError::NullValue {
            parameter,
            resolved_type,
        }
    );
    assert_eq!(error.to_string(), "function argument value cannot be NULL");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn function_argument_clone_and_equality_preserve_parameter_and_reference_identity() {
    let parameter = ParameterId::from_bytes([0x44; 16]);
    let value = RuntimeValue::Reference {
        target: TARGET,
        object: OBJECT,
    };
    let argument = FunctionArgument::new(parameter, value.clone()).unwrap();
    let clone = argument.clone();

    assert_eq!(clone, argument);
    assert_eq!(argument.parameter(), parameter);
    assert_eq!(argument.value(), &value);

    let other_parameter = ParameterId::from_bytes([0x45; 16]);
    let other = FunctionArgument::new(other_parameter, value).unwrap();
    assert_ne!(argument, other);
}

#[test]
fn accepts_every_initial_runtime_value_type_and_typed_null() {
    let rows = ResultRows::new(
        [
            column(
                "boolean",
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            ),
            column(
                "integer",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            ),
            column(
                "bigint",
                ResolvedType::scalar(StandardScalar::BigInt),
                false,
            ),
            column("float", ResolvedType::scalar(StandardScalar::Float), false),
            column(
                "text",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
            column(
                "optional_text",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            ),
            column(
                "bytes",
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                false,
            ),
            column("reference", ResolvedType::reference(TARGET), false),
        ],
        [ResultRow::new([
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(7),
            RuntimeValue::BigInt(8),
            RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
            RuntimeValue::Text("value".into()),
            RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject)).unwrap(),
            RuntimeValue::Bytes(vec![1, 2, 3]),
            RuntimeValue::Reference {
                target: TARGET,
                object: OBJECT,
            },
        ])],
    )
    .unwrap();

    assert_eq!(rows.columns().len(), 8);
    assert_eq!(rows.rows()[0].values().len(), 8);
    assert!(rows.rows()[0].values()[5].is_null());
}

#[test]
fn preserves_column_and_row_order() {
    let rows = ResultRows::new(
        [
            column(
                "second",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            ),
            column(
                "first",
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            ),
        ],
        [
            ResultRow::new([RuntimeValue::Integer(2), RuntimeValue::Boolean(false)]),
            ResultRow::new([RuntimeValue::Integer(1), RuntimeValue::Boolean(true)]),
        ],
    )
    .unwrap();

    assert_eq!(rows.columns()[0].name(), "second");
    assert_eq!(rows.columns()[1].name(), "first");
    assert_eq!(rows.rows()[0].values()[0], RuntimeValue::Integer(2));
    assert_eq!(rows.rows()[1].values()[1], RuntimeValue::Boolean(true));
}

#[test]
fn transfers_rows_and_values_in_order_without_cloning_payloads() {
    let bytes = vec![1_u8, 2, 3];
    let rows = ResultRows::new(
        [column(
            "payload",
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            false,
        )],
        [ResultRow::new([RuntimeValue::Bytes(bytes.clone())])],
    )
    .unwrap();

    let rows = rows.into_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows.into_iter().next().unwrap().into_values(),
        [RuntimeValue::Bytes(bytes),]
    );
}

#[test]
fn rejects_empty_duplicate_and_unsupported_columns() {
    assert_eq!(
        ResultColumn::new("", ResolvedType::scalar(StandardScalar::Boolean), false),
        Err(ResultRowsError::EmptyColumnName)
    );
    for resolved_type in [
        ResolvedType::scalar(StandardScalar::Decimal),
        ResolvedType::scalar(StandardScalar::Uuid),
        ResolvedType::scalar(StandardScalar::Date),
        ResolvedType::scalar(StandardScalar::Time),
        ResolvedType::scalar(StandardScalar::Timestamp),
        ResolvedType::scalar(StandardScalar::Duration),
        ResolvedType::scalar(StandardScalar::Void),
    ] {
        assert_eq!(
            ResultColumn::new("unsupported", resolved_type, false),
            Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
        );
        assert_eq!(
            RuntimeValue::null(resolved_type),
            Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
        );
    }
    assert_eq!(
        ResultRows::new(
            [
                column("same", ResolvedType::scalar(StandardScalar::Boolean), false),
                column("same", ResolvedType::scalar(StandardScalar::Integer), false),
            ],
            [],
        ),
        Err(ResultRowsError::DuplicateColumnName {
            first: 0,
            duplicate: 1,
            name: "same".into(),
        })
    );
}

#[test]
fn rejects_zero_columns_even_when_rows_have_zero_width() {
    assert_eq!(
        ResultRows::new(Vec::<ResultColumn>::new(), [ResultRow::new([])]),
        Err(ResultRowsError::EmptyColumns)
    );
}

#[test]
fn rejects_non_finite_floats_and_preserves_finite_equality() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            RuntimeFloat::new(value),
            Err(ResultRowsError::NonFiniteFloat)
        );
    }

    let finite = RuntimeFloat::new(2.5).unwrap();
    assert_eq!(finite, finite);
    assert_eq!(finite.value(), 2.5);
    assert_eq!(
        RuntimeFloat::new(0.0).unwrap(),
        RuntimeFloat::new(-0.0).unwrap()
    );
}

#[test]
fn null_values_expose_only_the_checked_type() {
    let value = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap();
    let RuntimeValue::Null(null) = value else {
        panic!("runtime null constructor must create a null value");
    };
    assert_eq!(
        null.resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
}

#[test]
fn rejects_width_nullability_and_type_mismatches() {
    let boolean = column(
        "boolean",
        ResolvedType::scalar(StandardScalar::Boolean),
        false,
    );
    assert_eq!(
        ResultRows::new([boolean.clone()], [ResultRow::new([])]),
        Err(ResultRowsError::RowWidthMismatch {
            row: 0,
            expected: 1,
            actual: 0,
        })
    );
    assert_eq!(
        ResultRows::new(
            [boolean.clone()],
            [ResultRow::new([RuntimeValue::null(ResolvedType::scalar(
                StandardScalar::Boolean
            ))
            .unwrap(),])],
        ),
        Err(ResultRowsError::NullInNonNullableColumn { row: 0, column: 0 })
    );
    assert_eq!(
        ResultRows::new([boolean], [ResultRow::new([RuntimeValue::Integer(1)])]),
        Err(ResultRowsError::ValueTypeMismatch {
            row: 0,
            column: 0,
            expected: ResolvedType::scalar(StandardScalar::Boolean),
            actual: ResolvedType::scalar(StandardScalar::Integer),
        })
    );
}

#[test]
fn rejects_references_with_the_wrong_target_type() {
    let expected = TypeId::from_bytes([0x51; 16]);
    let actual = TypeId::from_bytes([0x52; 16]);
    assert_eq!(
        ResultRows::new(
            [column(
                "reference",
                ResolvedType::reference(expected),
                false
            )],
            [ResultRow::new([RuntimeValue::Reference {
                target: actual,
                object: OBJECT,
            }])],
        ),
        Err(ResultRowsError::ValueTypeMismatch {
            row: 0,
            column: 0,
            expected: ResolvedType::reference(expected),
            actual: ResolvedType::reference(actual),
        })
    );
}

fn inspect_carrier_payload(active: &ActiveDatabaseRevision, tag: u8, rows: &[&[u8]]) -> Vec<u8> {
    let mut payload = b"ORNA-INSPECT/1 ".to_vec();
    payload.extend_from_slice(&1_u16.to_be_bytes());
    payload.push(tag);
    payload.extend_from_slice(&[0x11; 16]);
    payload.extend_from_slice(&active.pair().source().to_bytes());
    payload.extend_from_slice(&active.pair().catalogue().to_bytes());
    payload.extend_from_slice(&u32::try_from(rows.len()).unwrap().to_be_bytes());
    for row in rows {
        payload.extend_from_slice(&u32::try_from(row.len()).unwrap().to_be_bytes());
        payload.extend_from_slice(row);
    }
    payload
}

fn inspect_orv5_integer_row(value: i32) -> Vec<u8> {
    let mut row = b"ORV5".to_vec();
    row.push(0x03);
    row.extend_from_slice(&[0; 16]);
    row.extend_from_slice(&4_u32.to_be_bytes());
    row.extend_from_slice(&value.to_be_bytes());
    row
}

#[test]
fn inspect_opaque_constructor_accepts_all_nine_registered_carriers() {
    let active = active_record_revision();
    let carriers = [
        (1_u8, SYS_INSPECT_SNAPSHOT_TYPE_ID),
        (2_u8, SYS_INSPECT_INVOCATION_NODES_TYPE_ID),
        (3, SYS_INSPECT_CALLS_TYPE_ID),
        (4, SYS_INSPECT_RESOURCES_TYPE_ID),
        (5, SYS_INSPECT_STATE_CELLS_TYPE_ID),
        (6, SYS_INSPECT_UI_NODES_TYPE_ID),
        (7, SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID),
        (8, SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID),
        (9, SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID),
    ];

    for (tag, opaque_type) in carriers {
        let row = inspect_orv5_integer_row(1);
        let payload = inspect_carrier_payload(&active, tag, &[row.as_slice()]);
        let value = OpaqueValue::new_inspect_carrier(&active, opaque_type, &payload)
            .expect("the fixed Inspector carrier must construct");
        assert_eq!(value.opaque_type(), opaque_type);
        assert_eq!(value.canonical_payload(), payload);
    }
}

#[test]
fn inspect_opaque_constructor_accepts_snapshot_and_rejects_other_reserved_types() {
    let active = active_record_revision();
    let row = inspect_orv5_integer_row(1);
    let payload = inspect_carrier_payload(&active, 1, &[row.as_slice()]);
    let snapshot =
        OpaqueValue::new_inspect_carrier(&active, SYS_INSPECT_SNAPSHOT_TYPE_ID, &payload)
            .expect("the fixed snapshot carrier must construct");
    assert_eq!(snapshot.opaque_type(), SYS_INSPECT_SNAPSHOT_TYPE_ID);

    for opaque_type in [
        crate::system::SYS_INSPECT_INVOCATION_TYPE_ID,
        crate::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID,
        crate::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID,
    ] {
        assert_eq!(
            OpaqueValue::new_inspect_carrier(&active, opaque_type, &payload),
            Err(OpaqueValueError::UnregisteredType { opaque_type })
        );
    }
}

#[test]
fn inspect_opaque_constructor_rejects_malformed_trailing_and_mismatched_payloads() {
    let active = active_record_revision();
    let opaque_type = SYS_INSPECT_INVOCATION_NODES_TYPE_ID;
    let row = inspect_orv5_integer_row(1);
    let payload = inspect_carrier_payload(&active, 2, &[row.as_slice()]);

    let mut trailing = payload.clone();
    trailing.push(0);
    assert_eq!(
        OpaqueValue::new_inspect_carrier(&active, opaque_type, trailing),
        Err(OpaqueValueError::InvalidInspectCarrierEnvelope { opaque_type })
    );
    assert_eq!(
        OpaqueValue::new_inspect_carrier(&active, opaque_type, &payload[..payload.len() - 1]),
        Err(OpaqueValueError::InvalidInspectCarrierEnvelope { opaque_type })
    );

    let mut wrong_revision = payload.clone();
    let source_offset = b"ORNA-INSPECT/1 ".len() + 2 + 1 + 16;
    wrong_revision[source_offset] ^= 1;
    assert_eq!(
        OpaqueValue::new_inspect_carrier(&active, opaque_type, wrong_revision),
        Err(OpaqueValueError::InspectCarrierRevisionMismatch { opaque_type })
    );

    let unknown_type = TypeId::from_bytes([0xaa; 16]);
    assert_eq!(
        OpaqueValue::new_inspect_carrier(&active, unknown_type, payload),
        Err(OpaqueValueError::UnregisteredType {
            opaque_type: unknown_type,
        })
    );
}
#[test]
fn rows_opaque_registration_accepts_bounded_canonical_zero_row_frame() {
    const ROWS_TYPE: TypeId = TypeId::from_bytes([0x8a; 16]);
    let standard = verified_standard_with_value_types_and_schemas(
        vec![
            standard_boolean_definition(),
            opaque_definition(ROWS_TYPE, ["std", "data", "rows"], "orna.std.value.rows@1"),
        ],
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x8b; 16]),
            QualifiedSemanticName::new(["std", "data"]).unwrap(),
        )],
    );
    let active = active_record_revision_with_standard(RECORD_TYPE, standard);
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registration = OpaqueCodecRegistration::rows(
        ROWS_TYPE,
        QualifiedSemanticName::new(["std", "data", "rows"]).unwrap(),
        "orna.std.value.rows@1",
        "ORNA-ROWS/1 ",
    )
    .unwrap();
    let registry = OpaqueCodecRegistry::new(standard, [registration]).unwrap();
    let mut payload = b"ORNA-ROWS/1 ".to_vec();
    payload.extend_from_slice(&1_u16.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.push(b'x');
    payload.push(0x01);
    payload.extend_from_slice(&[0; 15]);
    payload.push(0x01);
    payload.push(0);
    payload.extend_from_slice(&0_u32.to_be_bytes());

    let value = OpaqueValue::new(&active, &registry, ROWS_TYPE, payload.clone())
        .expect("the bounded zero-row Rows frame must be structurally valid");
    assert_eq!(value.opaque_type(), ROWS_TYPE);
    assert_eq!(value.canonical_payload(), payload);

    let mut trailing = payload;
    trailing.push(0);
    assert_eq!(
        OpaqueValue::new(&active, &registry, ROWS_TYPE, trailing),
        Err(OpaqueValueError::InvalidRowsFrame {
            opaque_type: ROWS_TYPE,
        })
    );
}

#[test]
fn rows_opaque_registration_rejects_malformed_variable_orv5_cells() {
    const ROWS_TYPE: TypeId = TypeId::from_bytes([0x8a; 16]);
    let standard = verified_standard_with_value_types_and_schemas(
        vec![
            standard_boolean_definition(),
            opaque_definition(ROWS_TYPE, ["std", "data", "rows"], "orna.std.value.rows@1"),
        ],
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x8b; 16]),
            QualifiedSemanticName::new(["std", "data"]).unwrap(),
        )],
    );
    let active = active_record_revision_with_standard(RECORD_TYPE, standard);
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registration = OpaqueCodecRegistration::rows(
        ROWS_TYPE,
        QualifiedSemanticName::new(["std", "data", "rows"]).unwrap(),
        "orna.std.value.rows@1",
        "ORNA-ROWS/1 ",
    )
    .unwrap();
    let registry = OpaqueCodecRegistry::new(standard, [registration]).unwrap();

    let rows_frame = |cell: &[u8]| {
        let mut payload = b"ORNA-ROWS/1 ".to_vec();
        payload.extend_from_slice(&1_u16.to_be_bytes());
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(b"x");
        payload.push(0x01);
        payload.extend_from_slice(&[0; 15]);
        payload.push(0x01);
        payload.push(0);
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&u32::try_from(cell.len()).unwrap().to_be_bytes());
        payload.extend_from_slice(cell);
        payload
    };
    let orv5 = |tag: u8, type_id: [u8; 16], payload: &[u8]| {
        let mut cell = b"ORV5".to_vec();
        cell.push(tag);
        cell.extend_from_slice(&type_id);
        cell.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        cell.extend_from_slice(payload);
        cell
    };

    let scalar_type = |tag: u8| {
        let mut type_id = [0; 16];
        type_id[15] = tag;
        type_id
    };

    for cell in [
        orv5(0x06, [0x01; 16], &[0xff]),
        orv5(0x0a, [0x02; 16], &[0xff]),
        orv5(0x06, [0; 16], b"text"),
        orv5(0x06, scalar_type(0x07), b"text"),
        orv5(0x07, [0; 16], &[0xde, 0xad]),
        orv5(0x07, scalar_type(0x06), &[0xde, 0xad]),
        orv5(0x0c, [0x03; 16], &[0xde, 0xad]),
    ] {
        assert_eq!(
            OpaqueValue::new(&active, &registry, ROWS_TYPE, rows_frame(&cell)),
            Err(OpaqueValueError::InvalidRowsFrame {
                opaque_type: ROWS_TYPE,
            })
        );
    }

    let valid_text = orv5(0x06, scalar_type(0x06), b"text");
    let mut truncated = valid_text.clone();
    truncated.pop();
    assert_eq!(
        OpaqueValue::new(&active, &registry, ROWS_TYPE, rows_frame(&truncated)),
        Err(OpaqueValueError::InvalidRowsFrame {
            opaque_type: ROWS_TYPE,
        })
    );

    let mut declared_truncated = valid_text.clone();
    declared_truncated[21..25].copy_from_slice(&5_u32.to_be_bytes());
    assert_eq!(
        OpaqueValue::new(
            &active,
            &registry,
            ROWS_TYPE,
            rows_frame(&declared_truncated),
        ),
        Err(OpaqueValueError::InvalidRowsFrame {
            opaque_type: ROWS_TYPE,
        })
    );

    let mut declared_trailing = valid_text.clone();
    declared_trailing[21..25].copy_from_slice(&3_u32.to_be_bytes());
    assert_eq!(
        OpaqueValue::new(
            &active,
            &registry,
            ROWS_TYPE,
            rows_frame(&declared_trailing),
        ),
        Err(OpaqueValueError::InvalidRowsFrame {
            opaque_type: ROWS_TYPE,
        })
    );

    for cell in [
        valid_text,
        orv5(0x07, scalar_type(0x07), &[0xde, 0xad, 0xbe, 0xef]),
        orv5(0x0a, [0x05; 16], b"qualified"),
    ] {
        let payload = rows_frame(&cell);
        let value = OpaqueValue::new(&active, &registry, ROWS_TYPE, payload.clone())
            .expect("valid variable ORV5 cells are structurally accepted");
        assert_eq!(value.canonical_payload(), payload);
    }
}
