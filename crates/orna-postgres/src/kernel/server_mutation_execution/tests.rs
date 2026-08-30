use orna_artifact::server_mutation_plan::{
    FieldAssignment, MutationExpression, RecordFieldExpression,
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId,
    SourceUnitId,
    canonical_hash::{
        catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        EnumTypeDefinition, FieldDefinition, FunctionReturnColumnDefinition, ParameterDefinition,
        QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
        SchemaDefinition,
    },
    revision::{
        ActiveDatabaseRevisionInput, ActiveRevisionContent, CatalogueHashContext,
        DefinitionIdentity, DefinitionOrigin, RevisionPair, SourceOrigin, StoredSourceRevision,
        StoredSourceUnit,
    },
    types::{StandardScalar, TypeDescriptor},
    value::{RuntimeFloat, RuntimeValue},
};

use super::*;

const TARGET: TypeId = TypeId::from_bytes([0x10; 16]);
const OTHER: TypeId = TypeId::from_bytes([0x20; 16]);
const RECORD: TypeId = TypeId::from_bytes([0x70; 16]);
const MISSING: TypeId = TypeId::from_bytes([0x21; 16]);
const FUNCTION: FunctionId = FunctionId::from_bytes([0x30; 16]);
const OTHER_FUNCTION: FunctionId = FunctionId::from_bytes([0x31; 16]);
const REVISION: FunctionRevisionId = FunctionRevisionId::from_bytes([0x32; 16]);
const FIELD_TITLE: FieldId = FieldId::from_bytes([0x41; 16]);
const FIELD_ENABLED: FieldId = FieldId::from_bytes([0x42; 16]);
const FIELD_COUNT: FieldId = FieldId::from_bytes([0x43; 16]);
const FIELD_OWNER: FieldId = FieldId::from_bytes([0x44; 16]);
const FIELD_NOTE: FieldId = FieldId::from_bytes([0x45; 16]);
const PARAMETER_TITLE: ParameterId = ParameterId::from_bytes([0x51; 16]);
const PARAMETER_OWNER: ParameterId = ParameterId::from_bytes([0x52; 16]);
const PARAMETER_SELECTOR: ParameterId = ParameterId::from_bytes([0x53; 16]);
const OBJECT: ObjectId = ObjectId::from_bytes([0x61; 16]);
const SELECTED_OBJECT: ObjectId = ObjectId::from_bytes([0x62; 16]);

fn name(parts: &[&str]) -> QualifiedSemanticName {
    QualifiedSemanticName::new(parts.iter().copied()).unwrap()
}

fn field(
    id: FieldId,
    semantic_name: &str,
    ordinal: u32,
    resolved_type: ResolvedType,
    nullable: bool,
) -> FieldDefinition {
    FieldDefinition::new(
        id,
        semantic_name,
        ordinal,
        resolved_type,
        nullable,
        false,
        None,
        None,
    )
}

fn target_fields(reference_target: TypeId) -> Vec<FieldDefinition> {
    vec![
        field(
            FIELD_TITLE,
            "semantic_title",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
        ),
        field(
            FIELD_ENABLED,
            "semantic_enabled",
            1,
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        ),
        field(
            FIELD_COUNT,
            "semantic_count",
            2,
            ResolvedType::scalar(StandardScalar::Integer),
            true,
        ),
        field(
            FIELD_OWNER,
            "semantic_owner",
            3,
            ResolvedType::reference(reference_target),
            true,
        ),
        field(
            FIELD_NOTE,
            "semantic_note",
            4,
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            true,
        ),
    ]
}

fn object_types(fields: Vec<FieldDefinition>, include_other: bool) -> Vec<ObjectTypeDefinition> {
    let mut objects = vec![ObjectTypeDefinition::new(
        TARGET,
        name(&["test", "semantic_target"]),
        fields,
    )];
    if include_other {
        objects.push(ObjectTypeDefinition::new(
            OTHER,
            name(&["test", "semantic_other"]),
            Vec::new(),
        ));
    }
    objects
}

fn catalogue(
    fields: Vec<FieldDefinition>,
    include_other: bool,
    functions: Vec<FunctionDefinition>,
) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x01; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x02; 16]),
            name(&["test"]),
        )],
        object_types(fields, include_other),
        functions,
    )
    .unwrap()
}

fn parameters(reference_target: TypeId) -> Vec<ParameterDefinition> {
    vec![
        ParameterDefinition::new(
            PARAMETER_TITLE,
            "semantic_title_parameter",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        ),
        ParameterDefinition::new(
            PARAMETER_OWNER,
            "semantic_owner_parameter",
            1,
            ResolvedType::reference(reference_target),
            None,
        ),
    ]
}

fn function(
    domain: FunctionDomain,
    parameters: Vec<ParameterDefinition>,
    return_type: FunctionReturn,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
) -> FunctionDefinition {
    FunctionDefinition::new(
        FUNCTION,
        name(&["test", "semantic_insert"]),
        domain,
        parameters,
        return_type,
        REVISION,
        security,
        transaction,
        volatility,
    )
}

fn rows_reference(target: TypeId) -> FunctionReturn {
    FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
        "semantic_created",
        0,
        ResolvedType::reference(target),
    )])
}

fn valid_function() -> FunctionDefinition {
    function(
        FunctionDomain::Server,
        parameters(OTHER),
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

fn valid_plan() -> ServerMutationPlan {
    ServerMutationPlan::new_insert(
        TARGET,
        [
            FieldAssignment::new(
                TARGET,
                FIELD_TITLE,
                MutationExpression::parameter(
                    FUNCTION,
                    PARAMETER_TITLE,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )
                .unwrap(),
            ),
            FieldAssignment::new(
                TARGET,
                FIELD_ENABLED,
                MutationExpression::boolean_literal(true),
            ),
            FieldAssignment::new(
                TARGET,
                FIELD_COUNT,
                MutationExpression::typed_null(ResolvedType::scalar(StandardScalar::Integer))
                    .unwrap(),
            ),
            FieldAssignment::new(
                TARGET,
                FIELD_OWNER,
                MutationExpression::parameter(
                    FUNCTION,
                    PARAMETER_OWNER,
                    ResolvedType::reference(OTHER),
                )
                .unwrap(),
            ),
        ],
        TARGET,
    )
    .unwrap()
}

fn record_constructor_plan() -> ServerMutationPlan {
    ServerMutationPlan::new_insert(
        TARGET,
        [FieldAssignment::new(
            TARGET,
            FIELD_TITLE,
            MutationExpression::record_constructor(
                TARGET,
                [RecordFieldExpression::boolean_literal(
                    TARGET,
                    FIELD_ENABLED,
                    true,
                )],
            )
            .unwrap(),
        )],
        TARGET,
    )
    .unwrap()
}

fn valid_arguments() -> Vec<FunctionArgument> {
    vec![
        FunctionArgument::new(
            PARAMETER_OWNER,
            RuntimeValue::Reference {
                target: OTHER,
                object: OBJECT,
            },
        )
        .unwrap(),
        FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Text(String::from("title"))).unwrap(),
    ]
}

fn valid_update_function() -> FunctionDefinition {
    let mut declared_parameters = vec![ParameterDefinition::new(
        PARAMETER_SELECTOR,
        "semantic_selector_parameter",
        0,
        ResolvedType::reference(TARGET),
        None,
    )];
    declared_parameters.extend(parameters(OTHER).into_iter().enumerate().map(
        |(index, parameter)| {
            ParameterDefinition::new(
                parameter.id(),
                parameter.name(),
                u32::try_from(index + 1).unwrap(),
                parameter.resolved_type(),
                parameter.default_expression(),
            )
        },
    ));
    function(
        FunctionDomain::Server,
        declared_parameters,
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

fn valid_update_plan() -> ServerMutationPlan {
    ServerMutationPlan::new_update(
        TARGET,
        server_mutation_plan::MutationSelector::new(FUNCTION, PARAMETER_SELECTOR),
        [
            FieldAssignment::new(
                TARGET,
                FIELD_TITLE,
                MutationExpression::parameter(
                    FUNCTION,
                    PARAMETER_TITLE,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )
                .unwrap(),
            ),
            FieldAssignment::new(
                TARGET,
                FIELD_OWNER,
                MutationExpression::parameter(
                    FUNCTION,
                    PARAMETER_OWNER,
                    ResolvedType::reference(OTHER),
                )
                .unwrap(),
            ),
        ],
        TARGET,
    )
    .unwrap()
}

fn valid_update_arguments() -> Vec<FunctionArgument> {
    let mut arguments = valid_arguments();
    arguments.push(
        FunctionArgument::new(
            PARAMETER_SELECTOR,
            RuntimeValue::Reference {
                target: TARGET,
                object: SELECTED_OBJECT,
            },
        )
        .unwrap(),
    );
    arguments
}

fn rows_boolean() -> FunctionReturn {
    FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
        "semantic_deleted",
        0,
        ResolvedType::scalar(StandardScalar::Boolean),
    )])
}

fn valid_delete_function() -> FunctionDefinition {
    function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            PARAMETER_SELECTOR,
            "semantic_selector_parameter",
            0,
            ResolvedType::reference(TARGET),
            None,
        )],
        rows_boolean(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

fn valid_delete_plan() -> ServerDeletePlan {
    ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION, PARAMETER_SELECTOR))
}

fn valid_delete_arguments() -> Vec<FunctionArgument> {
    vec![
        FunctionArgument::new(
            PARAMETER_SELECTOR,
            RuntimeValue::Reference {
                target: TARGET,
                object: SELECTED_OBJECT,
            },
        )
        .unwrap(),
    ]
}

fn retained_standard_context() -> orna_core::revision::CatalogueHashContext {
    orna_core::revision::CatalogueHashContext::version_two(
        orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot().unwrap(),
        )
        .unwrap(),
    )
}

fn raw_pair_active() -> ActiveDatabaseRevision {
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x75; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    raw_pair_active_with_catalogue(catalogue)
}

fn raw_pair_active_with_catalogue(catalogue: CatalogueSnapshot) -> ActiveDatabaseRevision {
    let bundle = SourceBundleId::from_bytes([0x73; 16]);
    let source_unit = SourceUnitId::from_bytes([0x75; 16]);
    let content = "server mutation test source";
    let unit = StoredSourceUnit::new(
        source_unit,
        0,
        "mutation.orna",
        content,
        source_unit_content_digest(content).unwrap(),
    )
    .unwrap();
    let units = vec![unit];
    let bundle_hash = source_bundle_digest(&units).unwrap();
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x74; 16]),
        None,
        units,
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let (context, origins) = if catalogue.record_value_types().is_empty() {
        (CatalogueHashContext::version_one(), Vec::new())
    } else {
        let origin =
            SourceOrigin::new(source_unit, 0, u32::try_from(content.len()).unwrap()).unwrap();
        let mut origins = Vec::new();
        for schema in catalogue.schemas() {
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::Schema(schema.id()),
                origin,
            ));
        }
        for object in catalogue.object_types() {
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(object.id()),
                origin,
            ));
            origins.extend(object.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: object.id(),
                        field: field.id(),
                    },
                    origin,
                )
            }));
        }
        for record in catalogue.record_value_types() {
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ValueType(record.id()),
                origin,
            ));
            origins.extend(record.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: record.id(),
                        field: field.id(),
                    },
                    origin,
                )
            }));
        }
        (retained_standard_context(), origins)
    };
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn value_target_fields(reference_target: TypeId) -> Vec<FieldDefinition> {
    let mut fields = target_fields(reference_target);
    fields[0] = field(
        FIELD_TITLE,
        "semantic_title",
        0,
        ResolvedType::value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        false,
    );
    fields[1] = field(
        FIELD_ENABLED,
        "semantic_enabled",
        1,
        ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
        false,
    );
    fields
}

fn value_insert_function() -> FunctionDefinition {
    let mut declared_parameters = parameters(OTHER);
    declared_parameters[0] = ParameterDefinition::new(
        PARAMETER_TITLE,
        "semantic_title_parameter",
        0,
        ResolvedType::value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        None,
    );
    function(
        FunctionDomain::Server,
        declared_parameters,
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

fn value_insert_plan() -> ServerMutationPlan {
    ServerMutationPlan::new_insert(
        TARGET,
        [
            FieldAssignment::new(
                TARGET,
                FIELD_TITLE,
                MutationExpression::parameter(
                    FUNCTION,
                    PARAMETER_TITLE,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )
                .unwrap(),
            ),
            FieldAssignment::new(
                TARGET,
                FIELD_ENABLED,
                MutationExpression::boolean_literal(true),
            ),
            FieldAssignment::new(
                TARGET,
                FIELD_COUNT,
                MutationExpression::typed_null(ResolvedType::scalar(StandardScalar::Integer))
                    .unwrap(),
            ),
            FieldAssignment::new(
                TARGET,
                FIELD_OWNER,
                MutationExpression::parameter(
                    FUNCTION,
                    PARAMETER_OWNER,
                    ResolvedType::reference(OTHER),
                )
                .unwrap(),
            ),
        ],
        TARGET,
    )
    .unwrap()
}

fn value_delete_function() -> FunctionDefinition {
    function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            PARAMETER_SELECTOR,
            "semantic_selector_parameter",
            0,
            ResolvedType::reference(TARGET),
            None,
        )],
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "semantic_deleted",
            0,
            ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
        )]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

#[test]
fn verified_value_insert_preserves_legacy_bind_shapes_and_sql() {
    let legacy_catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let legacy_function = valid_function();
    let legacy_plan = valid_plan();
    validate_plan(&legacy_catalogue, &legacy_function, TARGET, &legacy_plan).unwrap();
    let legacy_arguments =
        validate_arguments(&legacy_catalogue, &legacy_function, &valid_arguments()).unwrap();
    let legacy_lowered = lower_insert(&legacy_plan, &legacy_arguments).unwrap();

    let context = retained_standard_context();
    let value_catalogue = catalogue(value_target_fields(OTHER), true, Vec::new());
    let value_function = value_insert_function();
    let value_plan = value_insert_plan();
    validate_function_signature_for_context(
        &context,
        &value_catalogue,
        &value_function,
        MutationExecutionKind::Insert,
    )
    .unwrap();
    validate_plan_for_context(
        &context,
        &value_catalogue,
        &value_function,
        TARGET,
        &value_plan,
        MutationExecutionKind::Insert,
    )
    .unwrap();
    let value_arguments = validate_arguments_with_context(
        &context,
        &value_catalogue,
        &value_function,
        &valid_arguments(),
    )
    .unwrap();
    let value_lowered = lower_insert_with_context(&context, &value_plan, &value_arguments).unwrap();

    assert_eq!(value_lowered.sql, legacy_lowered.sql);
    assert_eq!(value_lowered.bind_types, legacy_lowered.bind_types);
    assert_eq!(value_lowered.binds, legacy_lowered.binds);
}

#[test]
fn verified_value_update_preserves_bind_shapes_and_exact_selector_sql() {
    let legacy_catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let legacy_function = valid_update_function();
    let legacy_plan = valid_update_plan();
    validate_plan_for_operation(
        &legacy_catalogue,
        &legacy_function,
        TARGET,
        &legacy_plan,
        MutationExecutionKind::Update,
    )
    .unwrap();
    let legacy_arguments = validate_arguments(
        &legacy_catalogue,
        &legacy_function,
        &valid_update_arguments(),
    )
    .unwrap();
    let legacy_lowered = lower_update(&legacy_plan, &legacy_arguments).unwrap();

    let context = retained_standard_context();
    let value_catalogue = catalogue(value_target_fields(OTHER), true, Vec::new());
    let value_function = valid_update_function();
    let value_plan = valid_update_plan();
    validate_plan_for_context(
        &context,
        &value_catalogue,
        &value_function,
        TARGET,
        &value_plan,
        MutationExecutionKind::Update,
    )
    .unwrap();
    let value_arguments = validate_arguments_with_context(
        &context,
        &value_catalogue,
        &value_function,
        &valid_update_arguments(),
    )
    .unwrap();
    let value_lowered = lower_update_with_context(&context, &value_plan, &value_arguments).unwrap();

    assert_eq!(value_lowered.sql, legacy_lowered.sql);
    assert_eq!(value_lowered.bind_types, legacy_lowered.bind_types);
    assert_eq!(value_lowered.binds, legacy_lowered.binds);
}

#[test]
fn verified_value_delete_boolean_return_keeps_the_legacy_result_shape() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let legacy = validate_delete_function_signature(&catalogue, &valid_delete_function()).unwrap();
    let context = retained_standard_context();
    let value = validate_delete_function_signature_with_context(
        &context,
        &catalogue,
        &value_delete_function(),
    )
    .unwrap();

    assert_eq!(value, legacy);
    assert_eq!(
        value.resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
}

#[test]
fn verified_value_with_unsupported_contract_keeps_the_existing_signature_rule() {
    let mut declared_parameters = parameters(OTHER);
    declared_parameters[0] = ParameterDefinition::new(
        PARAMETER_TITLE,
        "semantic_title_parameter",
        0,
        ResolvedType::value(orna_standard::DECIMAL_TYPE_ID),
        None,
    );
    let function = function(
        FunctionDomain::Server,
        declared_parameters,
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let error = validate_function_signature_for_context(
        &retained_standard_context(),
        &catalogue,
        &function,
        MutationExecutionKind::Insert,
    )
    .unwrap_err();

    match expect_insert_error(error) {
        ServerInsertError::FunctionSignature { function, rule } => {
            assert_eq!(function, FUNCTION);
            assert_eq!(
                rule,
                "every INSERT SERVER function parameter must use a supported active type"
            );
        }
        other => panic!("unexpected mutation error: {other:?}"),
    }
}

fn raw_boolean_function() -> FunctionDefinition {
    function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            PARAMETER_TITLE,
            "raw_flag",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            None,
        )],
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

fn raw_text_parameter_function() -> FunctionDefinition {
    function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            PARAMETER_TITLE,
            "raw_title",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        )],
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

fn raw_zero_parameter_function() -> FunctionDefinition {
    function(
        FunctionDomain::Server,
        vec![],
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

fn raw_reference_function() -> FunctionDefinition {
    function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            PARAMETER_OWNER,
            "raw_owner",
            0,
            ResolvedType::reference(OTHER),
            None,
        )],
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    )
}

#[test]
fn raw_insert_argument_shape_accepts_supported_pairs_and_rejects_three_arguments() {
    let zero = raw_zero_parameter_function();
    validate_raw_server_insert_argument_shape(&zero, &[]).unwrap();

    let parameterised = raw_boolean_function();
    let error = expect_insert_error(
        validate_raw_server_insert_argument_shape(&parameterised, &[]).unwrap_err(),
    );
    match error {
        ServerInsertError::Argument { parameter, rule } => {
            assert_eq!(parameter, None);
            assert_eq!(rule, "raw SERVER INSERT calls must have zero parameters");
        }
        other => panic!("unexpected mutation error: {other:?}"),
    }

    let pair = [
        FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(true)).unwrap(),
        FunctionArgument::new(
            ParameterId::from_bytes([0x71; 16]),
            RuntimeValue::Boolean(false),
        )
        .unwrap(),
    ];
    validate_raw_server_insert_argument_shape(&parameterised, &pair)
        .expect("two supported arguments must pass the raw shape boundary");

    let three = [
        pair[0].clone(),
        pair[1].clone(),
        FunctionArgument::new(
            ParameterId::from_bytes([0x72; 16]),
            RuntimeValue::Boolean(true),
        )
        .unwrap(),
    ];
    let error = expect_insert_error(
        validate_raw_server_insert_argument_shape(&parameterised, &three).unwrap_err(),
    );
    match error {
        ServerInsertError::Argument { parameter, rule } => {
            assert_eq!(parameter, None);
            assert_eq!(
                rule,
                "raw SERVER INSERT calls accept at most two supported scalar or Reference arguments"
            );
        }
        other => panic!("unexpected mutation error: {other:?}"),
    }
}

#[test]
fn raw_insert_pair_validator_requires_distinct_declared_direct_reads() {
    let active = raw_pair_active();
    let duplicate = [
        FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(true)).unwrap(),
        FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(false)).unwrap(),
    ];
    let error = expect_insert_error(
        validate_raw_argument_pair_insert_parameter_use(
            &active,
            &raw_boolean_function(),
            &valid_plan(),
            &duplicate,
        )
        .unwrap_err(),
    );
    assert!(matches!(
        error,
        ServerInsertError::Argument {
            parameter: Some(PARAMETER_TITLE),
            rule: "raw SERVER INSERT argument pairs require two distinct parameter identities",
        }
    ));

    let pair = valid_arguments();
    let error = expect_insert_error(
        validate_raw_argument_pair_insert_parameter_use(
            &active,
            &raw_boolean_function(),
            &valid_plan(),
            &pair,
        )
        .unwrap_err(),
    );
    assert!(matches!(
        error,
        ServerInsertError::FunctionSignature {
            function: FUNCTION,
            rule: "raw SERVER INSERT argument pairs require exactly two parameters",
        }
    ));

    validate_raw_argument_pair_insert_parameter_use(
        &active,
        &valid_function(),
        &valid_plan(),
        &pair,
    )
    .expect("both declared parameters are direct INSERT reads");
}

#[test]
fn raw_insert_pair_failures_use_ascending_parameter_identity() {
    let active = raw_pair_active();
    let ignores_pair = ServerMutationPlan::new_insert(
        TARGET,
        [FieldAssignment::new(
            TARGET,
            FIELD_ENABLED,
            MutationExpression::boolean_literal(true),
        )],
        TARGET,
    )
    .unwrap();
    for arguments in [
        [
            FunctionArgument::new(PARAMETER_OWNER, RuntimeValue::Boolean(true)).unwrap(),
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(false)).unwrap(),
        ],
        [
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(false)).unwrap(),
            FunctionArgument::new(PARAMETER_OWNER, RuntimeValue::Boolean(true)).unwrap(),
        ],
    ] {
        let error = expect_insert_error(
            validate_raw_argument_pair_insert_parameter_use(
                &active,
                &valid_function(),
                &ignores_pair,
                &arguments,
            )
            .unwrap_err(),
        );
        assert!(matches!(
            error,
            ServerInsertError::Argument {
                parameter: Some(PARAMETER_TITLE),
                rule: "raw SERVER INSERT argument pairs must directly read both supplied parameters",
            }
        ));
    }

    for arguments in [
        [
            FunctionArgument::new(PARAMETER_OWNER, RuntimeValue::Text("\0owner".into())).unwrap(),
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Text("\0title".into())).unwrap(),
        ],
        [
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Text("\0title".into())).unwrap(),
            FunctionArgument::new(PARAMETER_OWNER, RuntimeValue::Text("\0owner".into())).unwrap(),
        ],
    ] {
        let error = expect_insert_error(validate_raw_text_insert_argument(&arguments).unwrap_err());
        assert!(matches!(
            error,
            ServerInsertError::Argument {
                parameter: Some(PARAMETER_TITLE),
                rule: "raw Text INSERT arguments cannot contain U+0000",
            }
        ));
    }
}

#[test]
fn raw_insert_boolean_singleton_keeps_its_direct_read_exception() {
    let active = raw_pair_active();
    let argument = FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(true)).unwrap();
    let ignores_boolean = ServerMutationPlan::new_insert(
        TARGET,
        [FieldAssignment::new(
            TARGET,
            FIELD_ENABLED,
            MutationExpression::boolean_literal(true),
        )],
        TARGET,
    )
    .unwrap();

    validate_raw_scalar_insert_parameter_use(
        &active,
        &raw_boolean_function(),
        &ignores_boolean,
        std::slice::from_ref(&argument),
    )
    .expect("the retained Boolean singleton path does not require a direct parameter read");
}

#[test]
fn raw_insert_boolean_argument_binds_through_the_existing_argument_validator() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = raw_boolean_function();
    for value in [true, false] {
        let argument =
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(value)).unwrap();
        validate_raw_server_insert_argument_shape(&function, std::slice::from_ref(&argument))
            .unwrap();
        let validated = validate_arguments(&catalogue, &function, &[argument]).unwrap();
        assert_eq!(validated[&PARAMETER_TITLE], BindValue::Boolean(value));
    }
}

#[test]
fn raw_insert_unknown_parameter_passes_shape_but_fails_argument_validation() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = raw_boolean_function();
    let unknown = ParameterId::from_bytes([0x72; 16]);
    let argument = FunctionArgument::new(unknown, RuntimeValue::Boolean(true)).unwrap();
    validate_raw_server_insert_argument_shape(&function, std::slice::from_ref(&argument)).unwrap();
    let error =
        expect_insert_error(validate_arguments(&catalogue, &function, &[argument]).unwrap_err());
    match error {
        ServerInsertError::Argument { parameter, rule } => {
            assert_eq!(parameter, Some(unknown));
            assert_eq!(
                rule,
                "an argument was supplied for a parameter that this function does not declare"
            );
        }
        other => panic!("unexpected mutation error: {other:?}"),
    }
}

#[test]
fn raw_insert_boolean_against_text_parameter_fails_argument_validation() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = raw_text_parameter_function();
    let argument = FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Boolean(true)).unwrap();
    validate_raw_server_insert_argument_shape(&function, std::slice::from_ref(&argument)).unwrap();
    let error =
        expect_insert_error(validate_arguments(&catalogue, &function, &[argument]).unwrap_err());
    match error {
        ServerInsertError::Argument { parameter, rule } => {
            assert_eq!(parameter, Some(PARAMETER_TITLE));
            assert_eq!(
                rule,
                "the argument type does not match the declared parameter type"
            );
        }
        other => panic!("unexpected mutation error: {other:?}"),
    }
}

#[test]
fn raw_insert_rejects_an_enum_single_argument_at_the_shape_boundary() {
    let function = raw_boolean_function();
    let enum_type = TypeId::from_bytes([0x73; 16]);
    let catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::new(),
        vec![SchemaDefinition::new(SchemaId::new(), name(&["app"]))],
        Vec::new(),
        Vec::new(),
        vec![EnumTypeDefinition::new(
            enum_type,
            name(&["app", "stage"]),
            ["lead"],
        )],
        Vec::new(),
    )
    .unwrap();
    let argument = FunctionArgument::new(
        PARAMETER_TITLE,
        RuntimeValue::Enum(EnumValue::new(&catalogue, enum_type, "lead").unwrap()),
    )
    .unwrap();
    let error = expect_insert_error(
        validate_raw_server_insert_argument_shape(&function, std::slice::from_ref(&argument))
            .unwrap_err(),
    );
    match error {
        ServerInsertError::Argument { parameter, rule } => {
            assert_eq!(parameter, Some(PARAMETER_TITLE));
            assert_eq!(
                rule,
                "raw SERVER INSERT calls accept only one supported scalar or Reference argument"
            );
        }
        other => panic!("unexpected mutation error: {other:?}"),
    }

    let pair = [
        FunctionArgument::new(PARAMETER_OWNER, RuntimeValue::Boolean(true)).unwrap(),
        argument,
    ];
    let error = expect_insert_error(
        validate_raw_server_insert_argument_shape(&function, &pair).unwrap_err(),
    );
    assert!(matches!(
        error,
        ServerInsertError::Argument {
            parameter: Some(PARAMETER_TITLE),
            rule: "raw SERVER INSERT argument pairs accept only supported scalar or Reference values",
        }
    ));
}

#[test]
fn raw_insert_reference_argument_is_accepted_at_the_shape_boundary_and_binds_object_bytes() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = raw_reference_function();
    let argument = FunctionArgument::new(
        PARAMETER_OWNER,
        RuntimeValue::Reference {
            target: OTHER,
            object: OBJECT,
        },
    )
    .unwrap();
    validate_raw_server_insert_argument_shape(&function, std::slice::from_ref(&argument)).unwrap();
    let validated = validate_arguments(&catalogue, &function, &[argument]).unwrap();
    assert_eq!(
        validated[&PARAMETER_OWNER],
        BindValue::Bytes(OBJECT.to_bytes().to_vec())
    );
}

#[test]
fn raw_reference_insert_parameter_use_requires_the_sole_reference_parameter_to_be_read() {
    let function = raw_reference_function();
    let argument = FunctionArgument::new(
        PARAMETER_OWNER,
        RuntimeValue::Reference {
            target: OTHER,
            object: OBJECT,
        },
    )
    .unwrap();
    let reads_reference = ServerMutationPlan::new_insert(
        TARGET,
        [FieldAssignment::new(
            TARGET,
            FIELD_OWNER,
            MutationExpression::parameter(
                FUNCTION,
                PARAMETER_OWNER,
                ResolvedType::reference(OTHER),
            )
            .unwrap(),
        )],
        TARGET,
    )
    .unwrap();
    validate_raw_reference_insert_parameter_use(
        &function,
        &reads_reference,
        std::slice::from_ref(&argument),
    )
    .expect("a plan that reads the sole Reference parameter must pass");

    let ignores_reference = ServerMutationPlan::new_insert(
        TARGET,
        [FieldAssignment::new(
            TARGET,
            FIELD_ENABLED,
            MutationExpression::boolean_literal(true),
        )],
        TARGET,
    )
    .unwrap();
    let error = expect_insert_error(
        validate_raw_reference_insert_parameter_use(&function, &ignores_reference, &[argument])
            .unwrap_err(),
    );
    match error {
        ServerInsertError::Argument { parameter, rule } => {
            assert_eq!(parameter, Some(PARAMETER_OWNER));
            assert_eq!(
                rule,
                "raw SERVER INSERT calls must read the sole Reference parameter"
            );
        }
        other => panic!("unexpected mutation error: {other:?}"),
    }
}

fn expect_insert_error(error: PostgresKernelError) -> ServerInsertError {
    let PostgresKernelError::ServerInsert(error) = error else {
        panic!("expected typed SERVER INSERT error");
    };
    error
}

fn expect_update_error(error: PostgresKernelError) -> ServerUpdateError {
    let PostgresKernelError::ServerUpdate(error) = error else {
        panic!("expected typed SERVER UPDATE error");
    };
    error
}

fn expect_delete_error(error: PostgresKernelError) -> ServerDeleteError {
    let PostgresKernelError::ServerDelete(error) = error else {
        panic!("expected typed SERVER DELETE error");
    };
    error
}

#[test]
fn context_and_result_expose_only_stable_execution_facts() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x71; 16]),
        CatalogueRevisionId::from_bytes([0x72; 16]),
    );
    let context = ServerInsertContext::new(pair, FUNCTION, REVISION);
    let result = ServerInsertResult::new(
        context,
        TARGET,
        OBJECT,
        ResultColumn::new("semantic_created", ResolvedType::reference(TARGET), false).unwrap(),
    )
    .unwrap();

    assert_eq!(context.pair(), pair);
    assert_eq!(context.function(), FUNCTION);
    assert_eq!(context.function_revision(), REVISION);
    assert_eq!(result.context(), context);
    assert_eq!(result.pair(), pair);
    assert_eq!(result.function(), FUNCTION);
    assert_eq!(result.function_revision(), REVISION);
    assert_eq!(result.target(), TARGET);
    assert_eq!(result.object(), OBJECT);
    assert_eq!(result.rows().columns().len(), 1);
    assert_eq!(result.rows().columns()[0].name(), "semantic_created");
    assert_eq!(
        result.rows().columns()[0].resolved_type(),
        ResolvedType::reference(TARGET),
    );
    assert!(!result.rows().columns()[0].nullable());
    assert_eq!(
        result.rows().rows()[0].values(),
        &[RuntimeValue::Reference {
            target: TARGET,
            object: OBJECT,
        }],
    );
}

#[test]
fn update_result_distinguishes_absent_and_matched_objects() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x71; 16]),
        CatalogueRevisionId::from_bytes([0x72; 16]),
    );
    let context = ServerUpdateContext::new(pair, FUNCTION, REVISION);
    let column =
        || ResultColumn::new("semantic_updated", ResolvedType::reference(TARGET), false).unwrap();
    let absent =
        ServerUpdateResult::new(context, TARGET, SELECTED_OBJECT, false, column()).unwrap();
    let matched =
        ServerUpdateResult::new(context, TARGET, SELECTED_OBJECT, true, column()).unwrap();

    assert_eq!(absent.context(), context);
    assert_eq!(absent.pair(), pair);
    assert_eq!(absent.function(), FUNCTION);
    assert_eq!(absent.function_revision(), REVISION);
    assert_eq!(absent.target(), TARGET);
    assert_eq!(absent.selector(), SELECTED_OBJECT);
    assert!(!absent.matched());
    assert!(absent.rows().rows().is_empty());
    assert_eq!(absent.rows().columns(), matched.rows().columns());
    assert!(matched.matched());
    assert_eq!(
        matched.rows().rows()[0].values(),
        &[RuntimeValue::Reference {
            target: TARGET,
            object: SELECTED_OBJECT,
        }],
    );
}

#[test]
fn delete_result_distinguishes_absent_and_deleted_objects() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x71; 16]),
        CatalogueRevisionId::from_bytes([0x72; 16]),
    );
    let context = ServerDeleteContext::new(pair, FUNCTION, REVISION);
    let column = || {
        ResultColumn::new(
            "semantic_deleted",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        )
        .unwrap()
    };
    let absent =
        ServerDeleteResult::new(context, TARGET, SELECTED_OBJECT, false, column()).unwrap();
    let deleted =
        ServerDeleteResult::new(context, TARGET, SELECTED_OBJECT, true, column()).unwrap();

    assert_eq!(absent.context(), context);
    assert_eq!(absent.pair(), pair);
    assert_eq!(absent.function(), FUNCTION);
    assert_eq!(absent.function_revision(), REVISION);
    assert_eq!(absent.target(), TARGET);
    assert_eq!(absent.selector(), SELECTED_OBJECT);
    assert!(!absent.matched());
    assert!(absent.rows().rows().is_empty());
    assert_eq!(absent.rows().columns(), deleted.rows().columns());
    assert_eq!(deleted.rows().columns()[0].name(), "semantic_deleted");
    assert_eq!(
        deleted.rows().columns()[0].resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean),
    );
    assert!(!deleted.rows().columns()[0].nullable());
    assert!(deleted.matched());
    assert_eq!(
        deleted.rows().rows()[0].values(),
        &[RuntimeValue::Boolean(true)],
    );
}

#[test]
fn delete_metadata_signature_plan_selector_and_references_are_exact() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = valid_delete_function();
    let plan = valid_delete_plan();

    validate_artifact_metadata_for_operation(
        FUNCTION,
        ExecutableArtifactKind::Server,
        server_mutation_plan::FORMAT_IDENTITY,
        server_mutation_plan::DELETE_FORMAT_VERSION,
        server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
        MutationExecutionKind::Delete,
    )
    .unwrap();
    for version in [
        server_mutation_plan::INSERT_FORMAT_VERSION,
        server_mutation_plan::UPDATE_FORMAT_VERSION,
    ] {
        assert!(
            validate_artifact_metadata_for_operation(
                FUNCTION,
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                version,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
                MutationExecutionKind::Delete,
            )
            .is_err()
        );
    }

    let column = validate_delete_function_signature(&catalogue, &function).unwrap();
    assert_eq!(column.name(), "semantic_deleted");
    assert_eq!(
        column.resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean),
    );
    assert!(!column.nullable());
    assert_eq!(
        validate_delete_plan(&catalogue, &function, &plan)
            .unwrap()
            .id(),
        TARGET
    );
    assert_eq!(
        plan.format_version(),
        server_mutation_plan::DELETE_FORMAT_VERSION
    );
    assert_eq!(plan.target(), TARGET);
    assert_eq!(
        plan.selector(),
        MutationSelector::new(FUNCTION, PARAMETER_SELECTOR),
    );
    assert_eq!(
        selector_argument_object(plan.target(), plan.selector(), &valid_delete_arguments())
            .unwrap(),
        SELECTED_OBJECT,
    );
    assert_eq!(
        expected_delete_body_references(&plan),
        [
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(TARGET),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(TARGET),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: FUNCTION,
                    parameter: PARAMETER_SELECTOR,
                },
            ),
        ],
    );
}

#[test]
fn delete_rejects_wrong_result_selector_owner_parameter_and_target() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let wrong_result = function(
        FunctionDomain::Server,
        valid_delete_function().parameters().to_vec(),
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    assert!(matches!(
        expect_insert_error(
            validate_delete_function_signature(&catalogue, &wrong_result).unwrap_err()
        ),
        ServerMutationError::FunctionSignature { .. },
    ));

    for selector in [
        MutationSelector::new(OTHER_FUNCTION, PARAMETER_SELECTOR),
        MutationSelector::new(FUNCTION, ParameterId::from_bytes([0x54; 16])),
    ] {
        assert!(
            validate_delete_plan(
                &catalogue,
                &valid_delete_function(),
                &ServerDeletePlan::new(TARGET, selector),
            )
            .is_err()
        );
    }

    let wrong_target_function = function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            PARAMETER_SELECTOR,
            "selector",
            0,
            ResolvedType::reference(OTHER),
            None,
        )],
        rows_boolean(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    assert!(
        validate_delete_plan(&catalogue, &wrong_target_function, &valid_delete_plan(),).is_err()
    );
}

#[test]
fn update_metadata_plan_selector_and_omitted_fields_are_exact() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = valid_update_function();
    let plan = valid_update_plan();

    validate_artifact_metadata_for_operation(
        FUNCTION,
        ExecutableArtifactKind::Server,
        server_mutation_plan::FORMAT_IDENTITY,
        server_mutation_plan::UPDATE_FORMAT_VERSION,
        server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
        MutationExecutionKind::Update,
    )
    .unwrap();
    assert!(
        validate_artifact_metadata_for_operation(
            FUNCTION,
            ExecutableArtifactKind::Server,
            server_mutation_plan::FORMAT_IDENTITY,
            server_mutation_plan::INSERT_FORMAT_VERSION,
            server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            MutationExecutionKind::Update,
        )
        .is_err()
    );
    let returned = validate_function_signature_for_operation(
        &catalogue,
        &function,
        MutationExecutionKind::Update,
    )
    .unwrap();
    let target = validate_plan_for_operation(
        &catalogue,
        &function,
        returned.target,
        &plan,
        MutationExecutionKind::Update,
    )
    .unwrap()
    .target;

    assert_eq!(target.id(), TARGET);
    assert_eq!(plan.format_version(), 2);
    assert_eq!(
        plan.selector(),
        Some(server_mutation_plan::MutationSelector::new(
            FUNCTION,
            PARAMETER_SELECTOR,
        )),
    );
    assert_eq!(plan.assignments().len(), 2);
    assert!(
        target
            .fields()
            .iter()
            .filter(|field| !field.nullable())
            .any(|field| !plan
                .assignments()
                .iter()
                .any(|assignment| assignment.field() == field.id()))
    );
    assert!(
        validate_plan_for_operation(
            &catalogue,
            &function,
            returned.target,
            &plan,
            MutationExecutionKind::Insert,
        )
        .is_err()
    );
}

#[test]
fn update_selector_requires_the_active_exact_target_reference_parameter() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let missing_parameter = ServerMutationPlan::new_update(
        TARGET,
        server_mutation_plan::MutationSelector::new(FUNCTION, ParameterId::from_bytes([0x54; 16])),
        [FieldAssignment::new(
            TARGET,
            FIELD_ENABLED,
            MutationExpression::boolean_literal(true),
        )],
        TARGET,
    )
    .unwrap();
    assert!(
        validate_plan_for_operation(
            &catalogue,
            &valid_update_function(),
            TARGET,
            &missing_parameter,
            MutationExecutionKind::Update,
        )
        .is_err()
    );

    for selector_type in [
        ResolvedType::scalar(StandardScalar::Integer),
        ResolvedType::reference(OTHER),
    ] {
        let function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                PARAMETER_SELECTOR,
                "selector",
                0,
                selector_type,
                None,
            )],
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let plan = ServerMutationPlan::new_update(
            TARGET,
            server_mutation_plan::MutationSelector::new(FUNCTION, PARAMETER_SELECTOR),
            [FieldAssignment::new(
                TARGET,
                FIELD_ENABLED,
                MutationExpression::boolean_literal(true),
            )],
            TARGET,
        )
        .unwrap();
        assert!(
            validate_plan_for_operation(
                &catalogue,
                &function,
                TARGET,
                &plan,
                MutationExecutionKind::Update,
            )
            .is_err()
        );
    }

    let wrong_owner = ServerMutationPlan::new_update(
        TARGET,
        server_mutation_plan::MutationSelector::new(OTHER_FUNCTION, PARAMETER_SELECTOR),
        [FieldAssignment::new(
            TARGET,
            FIELD_ENABLED,
            MutationExpression::boolean_literal(true),
        )],
        TARGET,
    )
    .unwrap();
    assert!(
        validate_plan_for_operation(
            &catalogue,
            &valid_update_function(),
            TARGET,
            &wrong_owner,
            MutationExecutionKind::Update,
        )
        .is_err()
    );
}

#[test]
fn signature_accepts_only_server_invoker_atomic_volatile_rows_ref() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let returned = validate_function_signature(&catalogue, &valid_function()).unwrap();
    assert_eq!(returned.target, TARGET);
    assert_eq!(returned.column.name(), "semantic_created");
    assert_eq!(
        returned.column.resolved_type(),
        ResolvedType::reference(TARGET),
    );
    assert!(!returned.column.nullable());

    let invalid = [
        function(
            FunctionDomain::Client,
            parameters(OTHER),
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Volatile,
        ),
        function(
            FunctionDomain::Server,
            parameters(OTHER),
            rows_reference(TARGET),
            FunctionSecurity::Definer,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        ),
        function(
            FunctionDomain::Server,
            parameters(OTHER),
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Volatile,
        ),
        function(
            FunctionDomain::Server,
            parameters(OTHER),
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Stable,
        ),
        function(
            FunctionDomain::Server,
            parameters(OTHER),
            FunctionReturn::Single(ResolvedType::reference(TARGET)),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        ),
        function(
            FunctionDomain::Server,
            parameters(OTHER),
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
            )]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        ),
    ];
    for function in invalid {
        assert!(matches!(
            expect_insert_error(validate_function_signature(&catalogue, &function).unwrap_err()),
            ServerInsertError::FunctionSignature { .. },
        ));
    }
}

#[test]
fn signature_rejects_defaults_unsupported_types_and_inactive_references() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let cases = [
        vec![ParameterDefinition::new(
            PARAMETER_TITLE,
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            Some(ExpressionId::from_bytes([0x73; 16])),
        )],
        vec![ParameterDefinition::new(
            PARAMETER_TITLE,
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Date),
            None,
        )],
        vec![ParameterDefinition::new(
            PARAMETER_TITLE,
            "value",
            0,
            ResolvedType::reference(MISSING),
            None,
        )],
    ];
    for parameters in cases {
        let candidate = function(
            FunctionDomain::Server,
            parameters,
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        assert!(matches!(
            expect_insert_error(validate_function_signature(&catalogue, &candidate).unwrap_err()),
            ServerInsertError::FunctionSignature { .. },
        ));
    }

    let missing_result = function(
        FunctionDomain::Server,
        Vec::new(),
        rows_reference(MISSING),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    assert!(validate_function_signature(&catalogue, &missing_result).is_err());
}

#[test]
fn artifact_metadata_accepts_only_insert_versions_one_and_four() {
    for version in [
        server_mutation_plan::INSERT_FORMAT_VERSION,
        server_mutation_plan::RECORD_INSERT_FORMAT_VERSION,
    ] {
        assert!(
            validate_artifact_metadata(
                FUNCTION,
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                version,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            )
            .is_ok()
        );
    }
    for (kind, format, version, language) in [
        (
            ExecutableArtifactKind::Client,
            server_mutation_plan::FORMAT_IDENTITY,
            1,
            server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
        ),
        (
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            1,
            server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
        ),
        (
            ExecutableArtifactKind::Server,
            server_mutation_plan::FORMAT_IDENTITY,
            2,
            server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
        ),
        (
            ExecutableArtifactKind::Server,
            server_mutation_plan::FORMAT_IDENTITY,
            1,
            "orna.language/2",
        ),
    ] {
        assert!(matches!(
            expect_insert_error(
                validate_artifact_metadata(FUNCTION, kind, format, version, language).unwrap_err()
            ),
            ServerInsertError::Artifact { .. },
        ));
    }
    assert!(matches!(
        ServerMutationPlan::decode(b"not a mutation plan"),
        Err(server_mutation_plan::ServerMutationPlanError::InvalidMagic),
    ));
}

#[test]
fn artifact_metadata_version_must_match_the_decoded_payload_version() {
    let version_one = valid_plan();
    let version_four = record_constructor_plan();

    assert!(validate_artifact_payload_version(FUNCTION, 1, &version_one).is_ok());
    assert!(validate_artifact_payload_version(FUNCTION, 4, &version_four).is_ok());
    for (metadata_version, plan) in [(1, &version_four), (4, &version_one)] {
        assert!(matches!(
            expect_insert_error(
                validate_artifact_payload_version(FUNCTION, metadata_version, plan).unwrap_err()
            ),
            ServerInsertError::Artifact {
                function: FUNCTION,
                rule: "the active artifact metadata version must match its mutation payload",
            }
        ));
    }
}

#[test]
fn plan_matches_the_active_catalogue_and_allows_omitted_nullable_fields() {
    let function = valid_function();
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let target = validate_plan(&catalogue, &function, TARGET, &valid_plan()).unwrap();

    assert_eq!(target.id(), TARGET);
    assert!(
        valid_plan()
            .assignments()
            .iter()
            .all(|assignment| assignment.field() != FIELD_NOTE)
    );
}

#[test]
fn plan_rejects_unknown_fields_type_mismatches_nullability_and_omissions() {
    let function = valid_function();
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let cases = [
        ServerMutationPlan::new_insert(
            TARGET,
            [FieldAssignment::new(
                TARGET,
                FieldId::from_bytes([0x7a; 16]),
                MutationExpression::boolean_literal(true),
            )],
            TARGET,
        )
        .unwrap(),
        ServerMutationPlan::new_insert(
            TARGET,
            [FieldAssignment::new(
                TARGET,
                FIELD_TITLE,
                MutationExpression::boolean_literal(true),
            )],
            TARGET,
        )
        .unwrap(),
        ServerMutationPlan::new_insert(
            TARGET,
            [FieldAssignment::new(
                TARGET,
                FIELD_TITLE,
                MutationExpression::typed_null(ResolvedType::scalar(
                    StandardScalar::CharacterLargeObject,
                ))
                .unwrap(),
            )],
            TARGET,
        )
        .unwrap(),
        ServerMutationPlan::new_insert(
            TARGET,
            [FieldAssignment::new(
                TARGET,
                FIELD_TITLE,
                MutationExpression::parameter(
                    OTHER_FUNCTION,
                    PARAMETER_TITLE,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                )
                .unwrap(),
            )],
            TARGET,
        )
        .unwrap(),
    ];
    for plan in cases {
        assert!(matches!(
            expect_insert_error(validate_plan(&catalogue, &function, TARGET, &plan).unwrap_err()),
            ServerInsertError::PlanInvariant { .. },
        ));
    }
}

#[test]
fn unique_constraints_admit_exact_text_and_required_reference_target_fields() {
    let mut version_one_fields = target_fields(OTHER);
    version_one_fields[0] = FieldDefinition::new(
        FIELD_TITLE,
        "semantic_title",
        0,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        true,
        true,
        None,
        None,
    );
    version_one_fields[3] = FieldDefinition::new(
        FIELD_OWNER,
        "semantic_owner",
        3,
        ResolvedType::reference(OTHER),
        false,
        true,
        None,
        None,
    );
    version_one_fields[4] = FieldDefinition::new(
        FIELD_NOTE,
        "semantic_note",
        4,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        false,
        true,
        None,
        None,
    );
    let version_one_catalogue = catalogue(version_one_fields, true, Vec::new());
    let version_one_target = version_one_catalogue.object_type_by_id(TARGET).unwrap();
    assert_eq!(
        UniqueConstraints::from_target(&CatalogueHashContext::version_one(), version_one_target)
            .unwrap()
            .fields,
        vec![
            UniqueConstraint::Text {
                owner: TARGET,
                field: FIELD_TITLE,
            },
            UniqueConstraint::Reference {
                owner: TARGET,
                field: FIELD_OWNER,
                referenced_type: OTHER,
            },
            UniqueConstraint::Text {
                owner: TARGET,
                field: FIELD_NOTE,
            },
        ]
    );

    let mut version_two_fields = value_target_fields(OTHER);
    version_two_fields[0] = FieldDefinition::new(
        FIELD_TITLE,
        "semantic_title",
        0,
        ResolvedType::value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        false,
        true,
        None,
        None,
    );
    version_two_fields[3] = FieldDefinition::new(
        FIELD_OWNER,
        "semantic_owner",
        3,
        ResolvedType::reference(OTHER),
        false,
        true,
        None,
        None,
    );
    version_two_fields[4] = FieldDefinition::new(
        FIELD_NOTE,
        "semantic_note",
        4,
        ResolvedType::value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        true,
        true,
        None,
        None,
    );
    let version_two_catalogue = catalogue(version_two_fields, true, Vec::new());
    let version_two_target = version_two_catalogue.object_type_by_id(TARGET).unwrap();
    assert_eq!(
        UniqueConstraints::from_target(&retained_standard_context(), version_two_target)
            .unwrap()
            .fields,
        vec![
            UniqueConstraint::Text {
                owner: TARGET,
                field: FIELD_TITLE,
            },
            UniqueConstraint::Reference {
                owner: TARGET,
                field: FIELD_OWNER,
                referenced_type: OTHER,
            },
            UniqueConstraint::Text {
                owner: TARGET,
                field: FIELD_NOTE,
            },
        ]
    );
}

#[test]
fn unique_text_constraints_close_non_exact_version_two_and_nullable_reference_shapes() {
    let version_two_context = retained_standard_context();
    let cases = [
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
        ResolvedType::value(TypeId::from_bytes([0x7b; 16])),
        ResolvedType::value(orna_standard::OPAQUE_TOKEN_TYPE_ID),
    ];
    for resolved_type in cases {
        let mut fields = value_target_fields(OTHER);
        fields[0] = FieldDefinition::new(
            FIELD_TITLE,
            "semantic_title",
            0,
            resolved_type,
            false,
            true,
            None,
            None,
        );
        let catalogue = catalogue(fields, true, Vec::new());
        assert!(
            UniqueConstraints::from_target(
                &version_two_context,
                catalogue.object_type_by_id(TARGET).unwrap(),
            )
            .is_err()
        );
    }

    let mut nullable_reference = target_fields(OTHER);
    nullable_reference[3] = FieldDefinition::new(
        FIELD_OWNER,
        "semantic_owner",
        3,
        ResolvedType::reference(OTHER),
        true,
        true,
        None,
        None,
    );
    let catalogue = catalogue(nullable_reference, true, Vec::new());
    assert!(
        UniqueConstraints::from_target(
            &CatalogueHashContext::version_one(),
            catalogue.object_type_by_id(TARGET).unwrap(),
        )
        .is_err()
    );
}

#[test]
fn plan_rejects_defaults_inactive_references_and_result_mismatch() {
    let function = valid_function();

    let mut default_fields = target_fields(OTHER);
    default_fields[4] = FieldDefinition::new(
        FIELD_NOTE,
        "semantic_note",
        4,
        ResolvedType::scalar(StandardScalar::BinaryLargeObject),
        true,
        false,
        Some(ExpressionId::from_bytes([0x74; 16])),
        None,
    );
    let default_catalogue = catalogue(default_fields, true, Vec::new());
    assert!(validate_plan(&default_catalogue, &function, TARGET, &valid_plan()).is_err());

    let inactive_catalogue = catalogue(target_fields(MISSING), false, Vec::new());
    assert!(validate_plan(&inactive_catalogue, &function, TARGET, &valid_plan()).is_err());
    assert!(
        validate_plan(
            &catalogue(target_fields(OTHER), true, Vec::new()),
            &function,
            OTHER,
            &valid_plan()
        )
        .is_err()
    );
}

#[test]
fn reference_replay_body_is_write_object_fields_parameter_reads_then_returned_ref() {
    let expected = expected_body_references(&valid_plan());
    assert_eq!(
        expected,
        vec![
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(TARGET),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: TARGET,
                    field: FIELD_TITLE,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: FUNCTION,
                    parameter: PARAMETER_TITLE,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: TARGET,
                    field: FIELD_ENABLED,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: TARGET,
                    field: FIELD_COUNT,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: TARGET,
                    field: FIELD_OWNER,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: FUNCTION,
                    parameter: PARAMETER_OWNER,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(TARGET),
            ),
        ],
    );
}

#[test]
fn update_reference_replay_includes_selector_before_returning() {
    assert_eq!(
        expected_body_references(&valid_update_plan()),
        vec![
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(TARGET),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: TARGET,
                    field: FIELD_TITLE,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: FUNCTION,
                    parameter: PARAMETER_TITLE,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: TARGET,
                    field: FIELD_OWNER,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: FUNCTION,
                    parameter: PARAMETER_OWNER,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(TARGET),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: FUNCTION,
                    parameter: PARAMETER_SELECTOR,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(TARGET),
            ),
        ],
    );
}

#[test]
fn arguments_are_unordered_exact_typed_and_reference_target_checked() {
    let function = valid_function();
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let validated = validate_arguments(&catalogue, &function, &valid_arguments()).unwrap();
    assert_eq!(validated.len(), 2);
    assert_eq!(validated[&PARAMETER_TITLE], BindValue::Text("title".into()));
    assert_eq!(
        validated[&PARAMETER_OWNER],
        BindValue::Bytes(OBJECT.to_bytes().to_vec()),
    );

    let duplicate = [
        valid_arguments()[1].clone(),
        valid_arguments()[1].clone(),
        valid_arguments()[0].clone(),
    ];
    assert!(validate_arguments(&catalogue, &function, &duplicate).is_err());
    assert!(validate_arguments(&catalogue, &function, &valid_arguments()[..1]).is_err());
    let unknown = [
        FunctionArgument::new(
            ParameterId::from_bytes([0x75; 16]),
            RuntimeValue::Integer(1),
        )
        .unwrap(),
        valid_arguments()[0].clone(),
        valid_arguments()[1].clone(),
    ];
    assert!(validate_arguments(&catalogue, &function, &unknown).is_err());
    let wrong_scalar = [
        FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Integer(1)).unwrap(),
        valid_arguments()[0].clone(),
    ];
    assert!(validate_arguments(&catalogue, &function, &wrong_scalar).is_err());
    let wrong_reference = [
        FunctionArgument::new(
            PARAMETER_OWNER,
            RuntimeValue::Reference {
                target: TARGET,
                object: OBJECT,
            },
        )
        .unwrap(),
        valid_arguments()[1].clone(),
    ];
    assert!(validate_arguments(&catalogue, &function, &wrong_reference).is_err());
}

#[test]
fn total_variable_argument_payload_is_bounded() {
    let function = valid_function();
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let oversized = [
        FunctionArgument::new(
            PARAMETER_TITLE,
            RuntimeValue::Text("x".repeat(VARIABLE_ARGUMENT_PAYLOAD_LIMIT + 1)),
        )
        .unwrap(),
        valid_arguments()[0].clone(),
    ];
    assert!(matches!(
        expect_insert_error(validate_arguments(&catalogue, &function, &oversized).unwrap_err()),
        ServerInsertError::ComplexityLimit {
            category: "total size of text and binary arguments",
            maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
        },
    ));
}

#[test]
fn aggregate_record_bind_payload_is_bounded_when_one_large_argument_is_reused() {
    let mut total = 0;
    let encoded_length = ACTIVE_VALUE_ENVELOPE_LENGTH + (VARIABLE_ARGUMENT_PAYLOAD_LIMIT / 2) + 1;

    account_record_bind_payload(&mut total, encoded_length).unwrap();
    assert!(matches!(
        expect_insert_error(account_record_bind_payload(&mut total, encoded_length).unwrap_err()),
        ServerInsertError::ComplexityLimit {
            category: "total size of canonical record payloads",
            maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
        },
    ));
}

#[test]
fn exact_argument_validation_accepts_more_parameters_than_the_assignment_limit() {
    let parameter_count = server_mutation_plan::MAX_ASSIGNMENTS as usize + 1;
    let integer_type = ResolvedType::scalar(StandardScalar::Integer);
    let parameters = (0..parameter_count)
        .map(|index| {
            ParameterDefinition::new(
                ParameterId::from_bytes((index as u128 + 1).to_be_bytes()),
                format!("parameter_{index}"),
                u32::try_from(index).unwrap(),
                integer_type,
                None,
            )
        })
        .collect();
    let function = function(
        FunctionDomain::Server,
        parameters,
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let arguments = (0..parameter_count)
        .map(|index| {
            FunctionArgument::new(
                ParameterId::from_bytes((index as u128 + 1).to_be_bytes()),
                RuntimeValue::Integer(i32::try_from(index).unwrap()),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    validate_function_signature(&catalogue, &function).unwrap();
    let validated = validate_arguments(&catalogue, &function, &arguments).unwrap();

    assert_eq!(validated.len(), parameter_count);
}

#[test]
fn lowering_uses_exact_stable_ids_typed_binds_and_an_unbound_typed_null() {
    let function = valid_function();
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let arguments = validate_arguments(&catalogue, &function, &valid_arguments()).unwrap();
    let lowered = lower_insert(&valid_plan(), &arguments).unwrap();

    assert_eq!(
        lowered.sql,
        "INSERT INTO _orna_data.t_10101010101010101010101010101010 (_orna_object_id, f_41414141414141414141414141414141, f_42424242424242424242424242424242, f_43434343434343434343434343434343, f_44444444444444444444444444444444) VALUES ($1, $2, $3, CAST(NULL AS int4), $4) RETURNING _orna_object_id AS c0",
    );
    assert_eq!(
        lowered.bind_types,
        vec![Type::BYTEA, Type::TEXT, Type::BOOL, Type::BYTEA],
    );
    assert_eq!(
        lowered.binds,
        vec![
            BindValue::Text(String::from("title")),
            BindValue::Boolean(true),
            BindValue::Bytes(OBJECT.to_bytes().to_vec()),
        ],
    );
    assert_eq!(lowered.sql.matches('$').count(), 4);
    for forbidden in [
        "semantic_target",
        "semantic_title",
        "semantic_insert",
        "semantic_created",
        "semantic_owner_parameter",
    ] {
        assert!(!lowered.sql.contains(forbidden));
    }
    assert!(!lowered.sql.contains("f_45454545454545454545454545454545"));
    assert!(lowered.sql.len() < SQL_LIMIT);
}

#[test]
fn record_constructor_binding_uses_declared_names_in_declaration_order() {
    let record = RecordValueTypeDefinition::new(
        RECORD,
        name(&["test", "semantic_record"]),
        vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FIELD_ENABLED,
                "enabled",
                0,
                TypeDescriptor::named(orna_standard::BOOLEAN_TYPE_ID),
            )
            .unwrap(),
            RecordValueFieldDefinition::try_new_descriptor(
                FIELD_COUNT,
                "count",
                1,
                TypeDescriptor::named(orna_standard::BOOLEAN_TYPE_ID),
            )
            .unwrap(),
        ],
    );
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes([0x76; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x02; 16]),
            name(&["test"]),
        )],
        vec![ObjectTypeDefinition::new(
            TARGET,
            name(&["test", "semantic_target"]),
            vec![field(
                FIELD_TITLE,
                "record_payload",
                0,
                ResolvedType::named(RECORD),
                false,
            )],
        )],
        Vec::new(),
        Vec::new(),
        vec![record],
        Vec::new(),
    )
    .unwrap();
    let active = raw_pair_active_with_catalogue(catalogue);
    let expression = MutationExpression::record_constructor(
        RECORD,
        [
            RecordFieldExpression::boolean_literal(RECORD, FIELD_ENABLED, true),
            RecordFieldExpression::boolean_literal(RECORD, FIELD_COUNT, false),
        ],
    )
    .unwrap();
    let plan = ServerMutationPlan::new_insert(
        TARGET,
        [FieldAssignment::new(TARGET, FIELD_TITLE, expression)],
        TARGET,
    )
    .unwrap();

    let lowered = lower_insert_with_active(&active, &plan, &BTreeMap::new()).unwrap();
    let expected = RuntimeValue::Record(
        RecordValue::new(
            &active,
            RECORD,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (String::from("count"), RuntimeValue::Boolean(false)),
            ],
        )
        .unwrap(),
    );
    let expected_bytes = encode_active_value(&active, &expected).unwrap();

    assert_eq!(
        lowered.sql,
        "INSERT INTO _orna_data.t_10101010101010101010101010101010 (_orna_object_id, f_41414141414141414141414141414141) VALUES ($1, $2) RETURNING _orna_object_id AS c0",
    );
    assert_eq!(lowered.bind_types, vec![Type::BYTEA, Type::BYTEA]);
    assert_eq!(lowered.binds, vec![BindValue::Bytes(expected_bytes)]);
}

#[test]
fn update_lowering_uses_stable_ids_typed_binds_and_exact_selector() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = valid_update_function();
    let plan = valid_update_plan();
    validate_plan_for_operation(
        &catalogue,
        &function,
        TARGET,
        &plan,
        MutationExecutionKind::Update,
    )
    .unwrap();
    let raw_arguments = valid_update_arguments();
    let arguments = validate_arguments(&catalogue, &function, &raw_arguments).unwrap();

    assert_eq!(
        selector_object(&plan, &raw_arguments).unwrap(),
        SELECTED_OBJECT,
    );
    let lowered = lower_update(&plan, &arguments).unwrap();
    assert_eq!(
        lowered.sql,
        "UPDATE _orna_data.t_10101010101010101010101010101010 SET f_41414141414141414141414141414141 = $1, f_44444444444444444444444444444444 = $2 WHERE _orna_object_id = $3 RETURNING _orna_object_id AS c0",
    );
    assert_eq!(
        lowered.bind_types,
        vec![Type::TEXT, Type::BYTEA, Type::BYTEA],
    );
    assert_eq!(
        lowered.binds,
        vec![
            BindValue::Text(String::from("title")),
            BindValue::Bytes(OBJECT.to_bytes().to_vec()),
            BindValue::Bytes(SELECTED_OBJECT.to_bytes().to_vec()),
        ],
    );
    for forbidden in [
        "semantic_target",
        "semantic_title",
        "semantic_insert",
        "semantic_updated",
        "semantic_selector_parameter",
    ] {
        assert!(!lowered.sql.contains(forbidden));
    }
}

#[test]
fn delete_lowering_uses_only_stable_ids_and_the_exact_bytea_selector() {
    let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
    let function = valid_delete_function();
    let plan = valid_delete_plan();
    validate_delete_plan(&catalogue, &function, &plan).unwrap();
    let raw_arguments = valid_delete_arguments();
    let arguments = validate_arguments(&catalogue, &function, &raw_arguments).unwrap();

    let lowered = lower_delete(&plan, &arguments).unwrap();

    assert_eq!(
        lowered.sql,
        "DELETE FROM _orna_data.t_10101010101010101010101010101010 WHERE _orna_object_id = $1 RETURNING _orna_object_id AS c0",
    );
    assert_eq!(lowered.bind_types, vec![Type::BYTEA]);
    assert_eq!(
        lowered.binds,
        vec![BindValue::Bytes(SELECTED_OBJECT.to_bytes().to_vec())],
    );
    for forbidden in [
        "semantic_target",
        "semantic_insert",
        "semantic_deleted",
        "semantic_selector_parameter",
    ] {
        assert!(!lowered.sql.contains(forbidden));
    }
    assert!(lowered.sql.len() < SQL_LIMIT);
}

#[test]
fn lowering_reuses_one_owned_bind_for_repeated_parameter_assignments() {
    let text_type = ResolvedType::scalar(StandardScalar::CharacterLargeObject);
    let function = function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            PARAMETER_TITLE,
            "semantic_title_parameter",
            0,
            text_type,
            None,
        )],
        rows_reference(TARGET),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let catalogue = catalogue(
        vec![
            field(FIELD_TITLE, "first", 0, text_type, false),
            field(FIELD_ENABLED, "second", 1, text_type, false),
            field(FIELD_COUNT, "third", 2, text_type, false),
        ],
        false,
        Vec::new(),
    );
    let parameter = || MutationExpression::parameter(FUNCTION, PARAMETER_TITLE, text_type).unwrap();
    let plan = ServerMutationPlan::new_insert(
        TARGET,
        [
            FieldAssignment::new(TARGET, FIELD_TITLE, parameter()),
            FieldAssignment::new(TARGET, FIELD_ENABLED, parameter()),
            FieldAssignment::new(TARGET, FIELD_COUNT, parameter()),
        ],
        TARGET,
    )
    .unwrap();
    validate_plan(&catalogue, &function, TARGET, &plan).unwrap();
    let arguments = validate_arguments(
        &catalogue,
        &function,
        &[FunctionArgument::new(
            PARAMETER_TITLE,
            RuntimeValue::Text(String::from("one owned payload")),
        )
        .unwrap()],
    )
    .unwrap();

    let lowered = lower_insert(&plan, &arguments).unwrap();

    assert_eq!(
        lowered.sql,
        "INSERT INTO _orna_data.t_10101010101010101010101010101010 (_orna_object_id, f_41414141414141414141414141414141, f_42424242424242424242424242424242, f_43434343434343434343434343434343) VALUES ($1, $2, $2, $2) RETURNING _orna_object_id AS c0",
    );
    assert_eq!(lowered.bind_types, vec![Type::BYTEA, Type::TEXT]);
    assert_eq!(
        lowered.binds,
        vec![BindValue::Text(String::from("one owned payload"))],
    );
}

#[test]
fn bind_ownership_covers_every_runtime_storage_type() {
    let values = [
        (RuntimeValue::Boolean(true), BindValue::Boolean(true)),
        (RuntimeValue::Integer(-1), BindValue::Integer(-1)),
        (RuntimeValue::BigInt(2), BindValue::BigInt(2)),
        (
            RuntimeValue::Float(RuntimeFloat::new(3.5).unwrap()),
            BindValue::Float(3.5),
        ),
        (
            RuntimeValue::Text(String::from("text")),
            BindValue::Text(String::from("text")),
        ),
        (
            RuntimeValue::Bytes(vec![1, 2]),
            BindValue::Bytes(vec![1, 2]),
        ),
        (
            RuntimeValue::Reference {
                target: OTHER,
                object: OBJECT,
            },
            BindValue::Bytes(OBJECT.to_bytes().to_vec()),
        ),
    ];
    for (value, expected) in values {
        assert_eq!(
            BindValue::from_runtime(&value, PARAMETER_TITLE).unwrap(),
            expected,
        );
    }
}

#[test]
fn public_errors_preserve_context_source_and_commit_state() {
    let context = ServerInsertContext::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x76; 16]),
            CatalogueRevisionId::from_bytes([0x77; 16]),
        ),
        FUNCTION,
        REVISION,
    );
    let not_committed = ServerInsertError::NotCommitted {
        context,
        source: Box::new(ServerInsertError::Argument {
            parameter: Some(PARAMETER_TITLE),
            rule: "the argument type does not match the declared parameter type",
        }),
    };
    assert_eq!(
        not_committed.commit_state(),
        ServerInsertCommitState::NotCommitted,
    );
    assert!(not_committed.source().is_some());
    assert_eq!(
        not_committed.to_string(),
        "the row was not added: a supplied function argument is invalid: the argument type does not match the declared parameter type",
    );

    let rejected = ServerInsertError::CommitRejected {
        context,
        target: TARGET,
        candidate: OBJECT,
        source: "port=invalid"
            .parse::<tokio_postgres::Config>()
            .unwrap_err(),
    };
    assert_eq!(
        rejected.to_string(),
        format!(
            "the database rejected the final save for object {}; no row was added",
            OBJECT.canonical(),
        ),
    );

    let unknown = ServerInsertError::CommitOutcomeUnknown {
        context,
        target: TARGET,
        candidate: OBJECT,
        source: "port=invalid"
            .parse::<tokio_postgres::Config>()
            .unwrap_err(),
    };
    assert_eq!(unknown.commit_state(), ServerInsertCommitState::Unknown);
    assert!(unknown.source().is_some());
    assert_eq!(
        unknown.to_string(),
        format!(
            "the connection failed while saving object {}; it is not known whether the row was added; do not retry automatically",
            OBJECT.canonical(),
        ),
    );

    let result = ServerInsertResult::new(
        context,
        TARGET,
        OBJECT,
        ResultColumn::new("created", ResolvedType::reference(TARGET), false).unwrap(),
    )
    .unwrap();
    let committed = ServerInsertError::CommittedButShutdownFailed {
        result: Box::new(result.clone()),
        source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
    };
    assert_eq!(committed.commit_state(), ServerInsertCommitState::Committed);
    let ServerInsertError::CommittedButShutdownFailed {
        result: retained, ..
    } = committed
    else {
        unreachable!();
    };
    assert_eq!(*retained, result);
    assert_eq!(
        ServerInsertError::CommittedButShutdownFailed {
            result: Box::new(result),
            source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
        }
        .to_string(),
        format!(
            "object {} was added, but the database connection did not close cleanly",
            OBJECT.canonical(),
        ),
    );
}

#[test]
fn unique_reference_conflict_preserves_typed_context_and_not_committed_outcomes() {
    let context = ServerInsertContext::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x76; 16]),
            CatalogueRevisionId::from_bytes([0x77; 16]),
        ),
        FUNCTION,
        REVISION,
    );
    let conflict = || ServerMutationError::UniqueReferenceConflict {
        owner: TARGET,
        field: FIELD_OWNER,
        referenced_type: OTHER,
        source: "port=invalid"
            .parse::<tokio_postgres::Config>()
            .unwrap_err(),
    };

    let error = conflict();
    let ServerMutationError::UniqueReferenceConflict {
        owner,
        field,
        referenced_type,
        ..
    } = &error
    else {
        unreachable!();
    };
    assert_eq!(
        (*owner, *field, *referenced_type),
        (TARGET, FIELD_OWNER, OTHER)
    );
    assert_eq!(
        error.to_string(),
        "this reference is already used by another object"
    );
    assert_eq!(
        error.commit_state(),
        ServerMutationCommitState::NotCommitted
    );
    assert!(error.source().is_some());

    let insert = expect_insert_error(not_committed(context, server_error(conflict())));
    let ServerMutationError::NotCommitted {
        context: insert_context,
        source: insert_source,
    } = insert
    else {
        panic!("expected contextual INSERT conflict");
    };
    assert_eq!(insert_context, context);
    assert!(matches!(
        insert_source.as_ref(),
        ServerMutationError::UniqueReferenceConflict {
            owner: TARGET,
            field: FIELD_OWNER,
            referenced_type: OTHER,
            ..
        }
    ));

    let update = expect_update_error(update_not_committed(context, server_error(conflict())));
    let ServerUpdateError::NotCommitted {
        context: update_context,
        source: update_source,
    } = update
    else {
        panic!("expected contextual UPDATE conflict");
    };
    assert_eq!(update_context, context);
    assert!(matches!(
        update_source.as_ref(),
        ServerMutationError::UniqueReferenceConflict {
            owner: TARGET,
            field: FIELD_OWNER,
            referenced_type: OTHER,
            ..
        }
    ));
}

#[test]
fn unique_text_conflict_preserves_typed_context_and_remains_operational_for_raw_insert() {
    let conflict = || ServerMutationError::UniqueTextConflict {
        owner: TARGET,
        field: FIELD_TITLE,
        source: "port=invalid"
            .parse::<tokio_postgres::Config>()
            .unwrap_err(),
    };
    let error = conflict();
    let ServerMutationError::UniqueTextConflict { owner, field, .. } = &error else {
        unreachable!();
    };
    assert_eq!((*owner, *field), (TARGET, FIELD_TITLE));
    assert_eq!(
        error.to_string(),
        "this text value is already used by another object"
    );
    assert_eq!(
        error.commit_state(),
        ServerMutationCommitState::NotCommitted
    );
    assert!(error.source().is_some());
    assert!(!raw_server_insert_target_is_unavailable(&error));

    let context = ServerInsertContext::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x76; 16]),
            CatalogueRevisionId::from_bytes([0x77; 16]),
        ),
        FUNCTION,
        REVISION,
    );
    let insert = expect_insert_error(not_committed(context, server_error(conflict())));
    assert!(matches!(
        insert,
        ServerMutationError::NotCommitted {
            source,
            ..
        } if matches!(source.as_ref(), ServerMutationError::UniqueTextConflict {
            owner: TARGET,
            field: FIELD_TITLE,
            ..
        })
    ));
}

#[test]
fn unique_constraint_classifier_requires_exact_active_constraint_evidence() {
    let expected_reference = UniqueConstraint::Reference {
        owner: TARGET,
        field: FIELD_OWNER,
        referenced_type: OTHER,
    };
    let expected_text = UniqueConstraint::Text {
        owner: TARGET,
        field: FIELD_TITLE,
    };
    let constraints = UniqueConstraints {
        fields: vec![expected_reference, expected_text],
    };
    let expected_reference_name = unique_constraint_name(FIELD_OWNER);
    let expected_text_name = unique_constraint_name(FIELD_TITLE);

    assert_eq!(
        unique_constraint(
            &constraints,
            Some(&SqlState::UNIQUE_VIOLATION),
            Some(&expected_reference_name),
        ),
        Some(expected_reference)
    );
    assert_eq!(
        unique_constraint(
            &constraints,
            Some(&SqlState::UNIQUE_VIOLATION),
            Some(&expected_text_name),
        ),
        Some(expected_text)
    );
    assert_eq!(
        unique_constraint(&constraints, Some(&SqlState::UNIQUE_VIOLATION), None),
        None
    );
    assert_eq!(
        unique_constraint(
            &constraints,
            Some(&SqlState::UNIQUE_VIOLATION),
            Some(&unique_constraint_name(FIELD_TITLE)),
        ),
        Some(expected_text)
    );
    assert_eq!(
        unique_constraint(
            &constraints,
            Some(&SqlState::UNIQUE_VIOLATION),
            Some("unrelated_unique_constraint"),
        ),
        None
    );
    assert_eq!(
        unique_constraint(
            &constraints,
            Some(&SqlState::FOREIGN_KEY_VIOLATION),
            Some(&expected_reference_name),
        ),
        None
    );
}

#[test]
fn update_errors_preserve_match_context_and_retry_state() {
    let context = ServerUpdateContext::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x76; 16]),
            CatalogueRevisionId::from_bytes([0x77; 16]),
        ),
        FUNCTION,
        REVISION,
    );
    let not_committed = ServerUpdateError::NotCommitted {
        context,
        source: Box::new(ServerMutationError::Argument {
            parameter: Some(PARAMETER_SELECTOR),
            rule: "the argument type does not match the declared parameter type",
        }),
    };
    assert_eq!(
        not_committed.commit_state(),
        ServerUpdateCommitState::NotCommitted,
    );
    assert_eq!(
        not_committed.to_string(),
        "the object was not updated: a supplied function argument is invalid: the argument type does not match the declared parameter type",
    );

    let unknown = ServerUpdateError::CommitOutcomeUnknown {
        context,
        target: TARGET,
        selector: SELECTED_OBJECT,
        matched: true,
        source: "port=invalid"
            .parse::<tokio_postgres::Config>()
            .unwrap_err(),
    };
    assert_eq!(unknown.commit_state(), ServerUpdateCommitState::Unknown);
    assert_eq!(
        unknown.to_string(),
        format!(
            "the connection failed while saving object {}; it is not known whether the update committed; do not retry automatically",
            SELECTED_OBJECT.canonical(),
        ),
    );
    let ServerUpdateError::CommitOutcomeUnknown {
        target,
        selector,
        matched,
        ..
    } = unknown
    else {
        unreachable!();
    };
    assert_eq!(target, TARGET);
    assert_eq!(selector, SELECTED_OBJECT);
    assert!(matched);

    let result = ServerUpdateResult::new(
        context,
        TARGET,
        SELECTED_OBJECT,
        false,
        ResultColumn::new("updated", ResolvedType::reference(TARGET), false).unwrap(),
    )
    .unwrap();
    let committed = ServerUpdateError::CommittedButShutdownFailed {
        result: Box::new(result.clone()),
        source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
    };
    assert_eq!(committed.commit_state(), ServerUpdateCommitState::Committed,);
    let ServerUpdateError::CommittedButShutdownFailed {
        result: retained, ..
    } = committed
    else {
        unreachable!();
    };
    assert_eq!(*retained, result);

    let wrapped = expect_update_error(update_not_committed(context, plan_invariant("test")));
    let ServerUpdateError::NotCommitted { source, .. } = wrapped else {
        panic!("expected a known-not-committed UPDATE failure");
    };
    assert!(matches!(
        *source,
        ServerMutationError::PlanInvariant { rule: "test" },
    ));
}

#[test]
fn delete_errors_preserve_selector_match_result_and_retry_state() {
    let context = ServerDeleteContext::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x76; 16]),
            CatalogueRevisionId::from_bytes([0x77; 16]),
        ),
        FUNCTION,
        REVISION,
    );
    let not_committed = ServerDeleteError::NotCommitted {
        context,
        source: Box::new(ServerMutationError::Argument {
            parameter: Some(PARAMETER_SELECTOR),
            rule: "the argument type does not match the declared parameter type",
        }),
    };
    assert_eq!(
        not_committed.commit_state(),
        ServerDeleteCommitState::NotCommitted,
    );
    assert_eq!(
        not_committed.to_string(),
        "the object was not deleted: a supplied function argument is invalid: the argument type does not match the declared parameter type",
    );

    let unknown = ServerDeleteError::CommitOutcomeUnknown {
        context,
        target: TARGET,
        selector: SELECTED_OBJECT,
        matched: true,
        source: "port=invalid"
            .parse::<tokio_postgres::Config>()
            .unwrap_err(),
    };
    assert_eq!(unknown.commit_state(), ServerDeleteCommitState::Unknown);
    assert_eq!(
        unknown.to_string(),
        format!(
            "the connection failed while deleting object {}; it is not known whether the delete committed; do not retry automatically",
            SELECTED_OBJECT.canonical(),
        ),
    );
    let ServerDeleteError::CommitOutcomeUnknown {
        target,
        selector,
        matched,
        ..
    } = unknown
    else {
        unreachable!();
    };
    assert_eq!(target, TARGET);
    assert_eq!(selector, SELECTED_OBJECT);
    assert!(matched);

    let result = ServerDeleteResult::new(
        context,
        TARGET,
        SELECTED_OBJECT,
        true,
        ResultColumn::new(
            "deleted",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let committed = ServerDeleteError::CommittedButShutdownFailed {
        result: Box::new(result.clone()),
        source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
    };
    assert_eq!(committed.commit_state(), ServerDeleteCommitState::Committed);
    let ServerDeleteError::CommittedButShutdownFailed {
        result: retained, ..
    } = committed
    else {
        unreachable!();
    };
    assert_eq!(*retained, result);

    let wrapped = expect_delete_error(delete_not_committed(context, plan_invariant("test")));
    let ServerDeleteError::NotCommitted { source, .. } = wrapped else {
        panic!("expected a known-not-committed DELETE failure");
    };
    assert!(matches!(
        *source,
        ServerMutationError::PlanInvariant { rule: "test" },
    ));
}

fn config_error() -> tokio_postgres::Error {
    "port=invalid"
        .parse::<tokio_postgres::Config>()
        .expect_err("invalid port must fail to parse")
}

#[test]
fn raw_reference_update_target_unavailability_pins_nested_mutation_failures() {
    let context = ServerUpdateContext::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x76; 16]),
            CatalogueRevisionId::from_bytes([0x77; 16]),
        ),
        FUNCTION,
        REVISION,
    );
    let unavailable = [
        ServerMutationError::FunctionSignature {
            function: FUNCTION,
            rule: "the raw reference UPDATE signature is unsupported",
        },
        ServerMutationError::Argument {
            parameter: Some(PARAMETER_SELECTOR),
            rule: "the raw reference UPDATE selector must match its sole active parameter and target",
        },
    ];
    for source in unavailable {
        assert!(
            raw_server_update_target_is_unavailable(&ServerUpdateError::NotCommitted {
                context,
                source: Box::new(source),
            }),
            "a nested UPDATE failure must close as an unavailable raw target",
        );
    }
    let internal = [
        ServerMutationError::CurrentRevision {
            function: FUNCTION,
            revision: REVISION,
        },
        ServerMutationError::ValueInvariant {
            rule: "the generated result contract is violated",
        },
    ];
    for source in internal {
        assert!(
            !raw_server_update_target_is_unavailable(&ServerUpdateError::NotCommitted {
                context,
                source: Box::new(source),
            }),
            "a nested mutation failure must stay internal",
        );
    }
    assert!(
        raw_server_update_target_is_unavailable(&ServerUpdateError::FunctionNotActive {
            pair: context.pair(),
            function: FUNCTION,
        }),
        "an inactive UPDATE function must close as unavailable",
    );
    for error in [
        ServerUpdateError::Unavailable {
            source: Box::new(PostgresKernelError::CatalogueInvariant("test")),
        },
        ServerUpdateError::CommitRejected {
            context,
            target: TARGET,
            selector: SELECTED_OBJECT,
            matched: true,
            source: config_error(),
        },
        ServerUpdateError::CommitOutcomeUnknown {
            context,
            target: TARGET,
            selector: SELECTED_OBJECT,
            matched: true,
            source: config_error(),
        },
    ] {
        assert!(
            !raw_server_update_target_is_unavailable(&error),
            "the UPDATE outcome {error:?} must stay internal",
        );
    }
}

#[test]
fn raw_reference_delete_target_unavailability_pins_nested_mutation_failures() {
    let context = ServerDeleteContext::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x76; 16]),
            CatalogueRevisionId::from_bytes([0x77; 16]),
        ),
        FUNCTION,
        REVISION,
    );
    let unavailable = [
        ServerMutationError::FunctionSignature {
            function: FUNCTION,
            rule: "the raw reference DELETE signature is unsupported",
        },
        ServerMutationError::Argument {
            parameter: Some(PARAMETER_SELECTOR),
            rule: "the raw reference DELETE selector must match its sole active parameter and target",
        },
    ];
    for source in unavailable {
        assert!(
            raw_server_delete_target_is_unavailable(&ServerDeleteError::NotCommitted {
                context,
                source: Box::new(source),
            }),
            "a nested DELETE failure must close as an unavailable raw target",
        );
    }
    let internal = [
        ServerMutationError::CurrentRevision {
            function: FUNCTION,
            revision: REVISION,
        },
        ServerMutationError::ValueInvariant {
            rule: "the generated result contract is violated",
        },
    ];
    for source in internal {
        assert!(
            !raw_server_delete_target_is_unavailable(&ServerDeleteError::NotCommitted {
                context,
                source: Box::new(source),
            }),
            "a nested mutation failure must stay internal",
        );
    }
    assert!(
        raw_server_delete_target_is_unavailable(&ServerDeleteError::FunctionNotActive {
            pair: context.pair(),
            function: FUNCTION,
        }),
        "an inactive DELETE function must close as unavailable",
    );
    for error in [
        ServerDeleteError::Unavailable {
            source: Box::new(PostgresKernelError::CatalogueInvariant("test")),
        },
        ServerDeleteError::DeleteRestricted {
            context,
            target: TARGET,
            selector: SELECTED_OBJECT,
            source: config_error(),
        },
        ServerDeleteError::CommitRejected {
            context,
            target: TARGET,
            selector: SELECTED_OBJECT,
            matched: true,
            source: config_error(),
        },
        ServerDeleteError::CommitOutcomeUnknown {
            context,
            target: TARGET,
            selector: SELECTED_OBJECT,
            matched: true,
            source: config_error(),
        },
    ] {
        assert!(
            !raw_server_delete_target_is_unavailable(&error),
            "the DELETE outcome {error:?} must stay internal",
        );
    }
}

#[test]
fn raw_reference_mutation_failure_classification_is_closed() {
    for failure in [
        ServerMutationError::FunctionNotActive {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x76; 16]),
                CatalogueRevisionId::from_bytes([0x77; 16]),
            ),
            function: FUNCTION,
        },
        ServerMutationError::FunctionSignature {
            function: FUNCTION,
            rule: "the raw reference mutation signature is unsupported",
        },
        ServerMutationError::Artifact {
            function: FUNCTION,
            rule: "the current revision lacks the mutation artifact",
        },
        ServerMutationError::PlanInvariant {
            rule: "the mutation plan disagrees with the active catalogue",
        },
        ServerMutationError::ReferenceEvidence {
            function: FUNCTION,
            rule: "the durable definition references do not prove the mutation body",
        },
        ServerMutationError::Argument {
            parameter: Some(PARAMETER_SELECTOR),
            rule: "the raw reference mutation selector must match its sole active parameter and target",
        },
        ServerMutationError::ComplexityLimit {
            category: "test",
            maximum: 1,
        },
    ] {
        assert!(
            raw_reference_mutation_failure_is_unavailable(&failure),
            "the mutation failure {failure:?} must close as unavailable",
        );
    }
    for failure in [
        ServerMutationError::Kernel {
            source: Box::new(PostgresKernelError::CatalogueInvariant("test")),
        },
        ServerMutationError::Database {
            source: config_error(),
        },
        ServerMutationError::CurrentRevision {
            function: FUNCTION,
            revision: REVISION,
        },
        ServerMutationError::PreparedResult {
            rule: "the prepared result shape differs",
        },
        ServerMutationError::ValueInvariant {
            rule: "the generated result contract is violated",
        },
    ] {
        assert!(
            !raw_reference_mutation_failure_is_unavailable(&failure),
            "the mutation failure {failure:?} must stay internal",
        );
    }
}

#[test]
fn delete_commit_classification_hides_constraint_timing() {
    assert_eq!(
        delete_commit_failure(Some(&SqlState::FOREIGN_KEY_VIOLATION)),
        DeleteCommitFailure::Restricted,
    );
    assert_eq!(
        delete_commit_failure(Some(&SqlState::RESTRICT_VIOLATION)),
        DeleteCommitFailure::Restricted,
    );
    assert_eq!(
        delete_commit_failure(Some(&SqlState::UNIQUE_VIOLATION)),
        DeleteCommitFailure::Rejected,
    );
    assert_eq!(delete_commit_failure(None), DeleteCommitFailure::Unknown,);
}

#[test]
fn saved_function_errors_hide_internal_rules_and_give_one_recovery_action() {
    assert_eq!(
        ServerInsertError::Artifact {
            function: FUNCTION,
            rule: "internal artifact detail",
        }
        .to_string(),
        "the saved function is unsupported; redeploy it or contact the database administrator",
    );
    assert_eq!(
        ServerInsertError::PlanDecode(ServerMutationPlan::decode(&[]).unwrap_err()).to_string(),
        "the saved function cannot be read; redeploy it or contact the database administrator",
    );
    assert_eq!(
        ServerInsertError::PlanInvariant {
            rule: "internal invariant detail",
        }
        .to_string(),
        "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
    );
    assert_eq!(
        ServerInsertError::ReferenceEvidence {
            function: FUNCTION,
            rule: "internal evidence detail",
        }
        .to_string(),
        "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
    );
    assert_eq!(
        ServerInsertError::PreparedResult {
            rule: "one BYTEA column named c0",
        }
        .to_string(),
        "the database prepared an unexpected result; redeploy the function or contact the database administrator",
    );
    assert_eq!(
        ServerInsertError::ValueInvariant {
            rule: "identity must contain 16 bytes",
        }
        .to_string(),
        "the database returned an unexpected object identity; contact the database administrator",
    );
    assert_eq!(
        ServerInsertError::ComplexityLimit {
            category: "total size of text and binary arguments",
            maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
        }
        .to_string(),
        format!(
            "the request is too large: total size of text and binary arguments limit is {VARIABLE_ARGUMENT_PAYLOAD_LIMIT}"
        ),
    );
    assert_eq!(
        ServerInsertError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }
        .to_string(),
        format!("the request is too large: saved function complexity limit is {SQL_LIMIT}"),
    );
}

#[test]
fn outer_kernel_error_keeps_the_public_server_insert_source() {
    let error = PostgresKernelError::ServerInsert(ServerInsertError::FunctionNotActive {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x78; 16]),
            CatalogueRevisionId::from_bytes([0x79; 16]),
        ),
        function: FUNCTION,
    });
    assert!(error.source().is_some());
    assert_eq!(
        error.to_string(),
        "row creation failed: the requested function is not active; no row was added",
    );
}
