
use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, FunctionRevisionId, ParameterId, SchemaId,
    SourceUnitId, TypeId,
    catalogue::{
        EnumTypeDefinition, FunctionDomain, FunctionReturnColumnDefinition, FunctionSecurity,
        FunctionVolatility, ParameterDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        RecordValueTypeDefinition, SchemaDefinition,
    },
    revision::{CatalogueHashContext, DefinitionReference, SourceOrigin},
    types::TypeDescriptor,
};

use super::*;

#[test]
fn resolved_runtime_type_classifies_legacy_shapes_and_postgres_types() {
    let context = CatalogueHashContext::version_one();
    let scalar_cases = [
        (
            StandardScalar::Boolean,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Boolean),
            Some(Type::BOOL),
        ),
        (
            StandardScalar::Integer,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Integer),
            Some(Type::INT4),
        ),
        (
            StandardScalar::BigInt,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::BigInt),
            Some(Type::INT8),
        ),
        (
            StandardScalar::Float,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Float),
            Some(Type::FLOAT8),
        ),
        (
            StandardScalar::Decimal,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Decimal),
            None,
        ),
        (
            StandardScalar::CharacterLargeObject,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::CharacterLargeObject),
            Some(Type::TEXT),
        ),
        (
            StandardScalar::BinaryLargeObject,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::BinaryLargeObject),
            Some(Type::BYTEA),
        ),
        (
            StandardScalar::Uuid,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Uuid),
            None,
        ),
        (
            StandardScalar::Date,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Date),
            None,
        ),
        (
            StandardScalar::Time,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Time),
            None,
        ),
        (
            StandardScalar::Timestamp,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Timestamp),
            None,
        ),
        (
            StandardScalar::Duration,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Duration),
            None,
        ),
        (
            StandardScalar::Void,
            ResolvedRuntimeType::LegacyScalar(StandardScalar::Void),
            None,
        ),
    ];
    assert_eq!(scalar_cases.len(), StandardScalar::ALL.len());
    for (scalar, runtime, postgres) in scalar_cases {
        let resolved = ResolvedType::scalar(scalar);
        assert_eq!(resolve_runtime_type(&context, resolved), runtime);
        assert_eq!(postgres_type(runtime), postgres);
    }

    let named = ResolvedType::named(TypeId::from_bytes([0x51; 16]));
    assert_eq!(
        resolve_runtime_type(&context, named),
        ResolvedRuntimeType::Unsupported
    );
    assert_eq!(postgres_type(resolve_runtime_type(&context, named)), None);

    let target = TypeId::from_bytes([0x52; 16]);
    let reference = ResolvedType::reference(target);
    assert_eq!(
        resolve_runtime_type(&context, reference),
        ResolvedRuntimeType::Reference(target)
    );
    assert_eq!(
        postgres_type(resolve_runtime_type(&context, reference)),
        Some(Type::BYTEA)
    );
}

#[test]
fn active_catalogue_classifies_declared_named_runtime_types() {
    let enum_type = TypeId::from_bytes([0x53; 16]);
    let record_type = TypeId::from_bytes([0x57; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes([0x54; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x55; 16]),
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        Vec::new(),
        Vec::new(),
        vec![EnumTypeDefinition::new(
            enum_type,
            QualifiedSemanticName::new(["app", "stage"]).unwrap(),
            ["lead"],
        )],
        vec![RecordValueTypeDefinition::new(
            record_type,
            QualifiedSemanticName::new(["app", "flag"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes([0x58; 16]),
                    "stage",
                    0,
                    TypeDescriptor::named(enum_type),
                )
                .expect("record field"),
            ],
        )],
        Vec::new(),
    )
    .unwrap();
    let context = CatalogueHashContext::version_one();

    assert_eq!(
        resolve_catalogue_runtime_type(&catalogue, &context, ResolvedType::named(enum_type)),
        ResolvedRuntimeType::CatalogueEnum(enum_type)
    );
    assert_eq!(
        postgres_type(ResolvedRuntimeType::CatalogueEnum(enum_type)),
        Some(Type::TEXT)
    );
    assert_eq!(
        resolve_catalogue_runtime_type(&catalogue, &context, ResolvedType::named(record_type),),
        ResolvedRuntimeType::Record(record_type)
    );
    assert_eq!(
        postgres_type(ResolvedRuntimeType::Record(record_type)),
        Some(Type::BYTEA)
    );
    assert_eq!(
        resolve_catalogue_runtime_type(
            &catalogue,
            &context,
            ResolvedType::named(TypeId::from_bytes([0x56; 16])),
        ),
        ResolvedRuntimeType::Unsupported
    );
}

#[test]
fn postgres_types_cover_the_exact_runtime_subset() {
    let context = CatalogueHashContext::version_one();
    let supported = [
        (ResolvedType::scalar(StandardScalar::Boolean), Type::BOOL),
        (ResolvedType::scalar(StandardScalar::Integer), Type::INT4),
        (ResolvedType::scalar(StandardScalar::BigInt), Type::INT8),
        (ResolvedType::scalar(StandardScalar::Float), Type::FLOAT8),
        (
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            Type::TEXT,
        ),
        (
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            Type::BYTEA,
        ),
        (
            ResolvedType::reference(TypeId::from_bytes([0x55; 16])),
            Type::BYTEA,
        ),
    ];
    for (resolved_type, expected) in supported {
        assert_eq!(
            postgres_type(resolve_runtime_type(&context, resolved_type)),
            Some(expected)
        );
    }
    for scalar in [
        StandardScalar::Decimal,
        StandardScalar::Uuid,
        StandardScalar::Date,
        StandardScalar::Time,
        StandardScalar::Timestamp,
        StandardScalar::Duration,
        StandardScalar::Void,
    ] {
        assert_eq!(
            postgres_type(resolve_runtime_type(&context, ResolvedType::scalar(scalar))),
            None
        );
    }
    assert_eq!(
        postgres_type(resolve_runtime_type(
            &context,
            ResolvedType::named(TypeId::from_bytes([0x56; 16]))
        )),
        None
    );
}

#[test]
fn retained_version_two_value_contracts_match_legacy_runtime_capabilities() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()
            .expect("retained standard-library snapshot"),
    )
    .expect("verified standard-library snapshot");
    let context = CatalogueHashContext::version_two(standard);
    let cases = [
        (
            "orna.kernel.value.boolean@1",
            StandardScalar::Boolean,
            Some(Type::BOOL),
        ),
        (
            "orna.kernel.value.integer@1",
            StandardScalar::Integer,
            Some(Type::INT4),
        ),
        (
            "orna.kernel.value.bigint@1",
            StandardScalar::BigInt,
            Some(Type::INT8),
        ),
        (
            "orna.kernel.value.float@1",
            StandardScalar::Float,
            Some(Type::FLOAT8),
        ),
        (
            "orna.kernel.value.character-large-object@1",
            StandardScalar::CharacterLargeObject,
            Some(Type::TEXT),
        ),
        (
            "orna.kernel.value.binary-large-object@1",
            StandardScalar::BinaryLargeObject,
            Some(Type::BYTEA),
        ),
        ("orna.kernel.value.decimal@1", StandardScalar::Decimal, None),
        ("orna.kernel.value.uuid@1", StandardScalar::Uuid, None),
        ("orna.kernel.value.date@1", StandardScalar::Date, None),
        ("orna.kernel.value.time@1", StandardScalar::Time, None),
        (
            "orna.kernel.value.timestamp@1",
            StandardScalar::Timestamp,
            None,
        ),
        (
            "orna.kernel.value.duration@1",
            StandardScalar::Duration,
            None,
        ),
        ("orna.kernel.value.void@1", StandardScalar::Void, None),
    ];
    assert_eq!(cases.len(), StandardScalar::ALL.len());
    for (contract, expected_compatibility, expected_postgres) in cases {
        let value_type = context
            .standard()
            .expect("version-two standard")
            .catalogue()
            .value_types()
            .iter()
            .find(|definition| definition.representation_contract() == contract)
            .expect("retained value type")
            .id();
        let runtime = resolve_runtime_type(&context, ResolvedType::value(value_type));
        assert_eq!(
            runtime,
            ResolvedRuntimeType::VerifiedValue {
                value_type,
                compatibility: expected_compatibility,
            },
            "{contract}"
        );
        assert!(runtime_types_match(
            &context,
            ResolvedType::scalar(expected_compatibility),
            ResolvedType::value(value_type),
        ));
        assert_eq!(postgres_type(runtime), expected_postgres, "{contract}");
    }
}

#[test]
fn values_require_the_selected_pinned_standard_identity() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()
            .expect("retained standard-library snapshot"),
    )
    .expect("verified standard-library snapshot");
    let integer = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.representation_contract() == "orna.kernel.value.integer@1")
        .expect("retained integer value type")
        .id();
    let missing = TypeId::from_bytes([0x5a; 16]);

    assert_eq!(
        resolve_runtime_type(
            &CatalogueHashContext::version_one(),
            ResolvedType::value(integer)
        ),
        ResolvedRuntimeType::Unsupported
    );
    assert_eq!(
        resolve_runtime_type(
            &CatalogueHashContext::version_two(standard),
            ResolvedType::value(missing)
        ),
        ResolvedRuntimeType::Unsupported
    );
    assert!(!runtime_types_match(
        &CatalogueHashContext::version_two(
            orna_standard::verify_standard_library_snapshot(
                orna_standard::retained_standard_library_snapshot()
                    .expect("retained standard-library snapshot"),
            )
            .expect("verified standard-library snapshot"),
        ),
        ResolvedType::scalar(StandardScalar::Integer),
        ResolvedType::value(missing),
    ));
}

#[test]
fn reference_replay_puts_signature_references_before_body_evidence() {
    let parameter_value = TypeId::from_bytes([0x60; 16]);
    let parameter_target = TypeId::from_bytes([0x61; 16]);
    let result_target = TypeId::from_bytes([0x62; 16]);
    let body_target = TypeId::from_bytes([0x63; 16]);
    let result_value = TypeId::from_bytes([0x68; 16]);
    let enum_type = TypeId::from_bytes([0x6a; 16]);
    let function = FunctionDefinition::new(
        FunctionId::from_bytes([0x64; 16]),
        QualifiedSemanticName::new(["test", "function"]).unwrap(),
        FunctionDomain::Server,
        vec![
            ParameterDefinition::new(
                ParameterId::from_bytes([0x65; 16]),
                "ignored_scalar",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                None,
            ),
            ParameterDefinition::new(
                ParameterId::from_bytes([0x69; 16]),
                "value",
                1,
                ResolvedType::value(parameter_value),
                None,
            ),
            ParameterDefinition::new(
                ParameterId::from_bytes([0x66; 16]),
                "reference",
                2,
                ResolvedType::reference(parameter_target),
                None,
            ),
        ],
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new("value", 0, ResolvedType::value(result_value)),
            FunctionReturnColumnDefinition::new(
                "reference",
                1,
                ResolvedType::reference(result_target),
            ),
            FunctionReturnColumnDefinition::new(
                "ignored_scalar",
                2,
                ResolvedType::scalar(StandardScalar::Integer),
            ),
            FunctionReturnColumnDefinition::new("enum_value", 3, ResolvedType::named(enum_type)),
        ]),
        FunctionRevisionId::from_bytes([0x67; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );
    let body = [ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(body_target),
    )];

    assert_eq!(
        expected_function_references(&function, &body),
        vec![
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(parameter_value),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(parameter_target),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(result_value),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(result_target),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(enum_type),
            ),
            body[0],
        ]
    );
    let stream_function = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        FunctionReturn::Stream(ResolvedType::reference(result_target)),
        function.current_revision(),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );
    assert_eq!(
        expected_function_references(&stream_function, &body),
        vec![
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(parameter_value),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(parameter_target),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(result_target),
            ),
            body[0],
        ],
    );
}

#[test]
fn reference_replay_rejects_missing_extra_and_reordered_records() {
    let function = FunctionId::from_bytes([0x71; 16]);
    let revision = FunctionRevisionId::from_bytes([0x72; 16]);
    let first = DefinitionReferenceTarget::ObjectType(TypeId::from_bytes([0x73; 16]));
    let second = DefinitionReferenceTarget::Field {
        owner: TypeId::from_bytes([0x74; 16]),
        field: FieldId::from_bytes([0x75; 16]),
    };
    let expected = [
        ExpectedDefinitionReference::new(DefinitionReferenceKind::QueryObject, first),
        ExpectedDefinitionReference::new(DefinitionReferenceKind::QueryField, second),
    ];
    let source = SourceOrigin::new(SourceUnitId::from_bytes([0x76; 16]), 0, 0).unwrap();
    let records = [
        DefinitionReference::new(
            function,
            revision,
            0,
            first,
            DefinitionReferenceKind::QueryObject,
            source,
        ),
        DefinitionReference::new(
            function,
            revision,
            1,
            second,
            DefinitionReferenceKind::QueryField,
            source,
        ),
    ];
    assert!(validate_reference_sequence(&[&records[0], &records[1]], &expected).is_ok());
    assert_eq!(
        validate_reference_sequence(&[&records[0]], &expected),
        Err(ReferenceReplayMismatch::Count)
    );
    assert_eq!(
        validate_reference_sequence(&[&records[0], &records[1], &records[1]], &expected),
        Err(ReferenceReplayMismatch::Count)
    );
    assert_eq!(
        validate_reference_sequence(&[&records[1], &records[0]], &expected),
        Err(ReferenceReplayMismatch::Sequence)
    );
}
