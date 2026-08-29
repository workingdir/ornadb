
use super::*;

const TARGET: TypeId = TypeId::from_bytes([1; 16]);
const OTHER_TARGET: TypeId = TypeId::from_bytes([2; 16]);
const FIELD_A: FieldId = FieldId::from_bytes([3; 16]);
const FIELD_B: FieldId = FieldId::from_bytes([4; 16]);
const FUNCTION_A: FunctionId = FunctionId::from_bytes([5; 16]);
const FUNCTION_B: FunctionId = FunctionId::from_bytes([6; 16]);
const PARAMETER_A: ParameterId = ParameterId::from_bytes([7; 16]);
const PARAMETER_B: ParameterId = ParameterId::from_bytes([8; 16]);
const REF_TARGET: TypeId = TypeId::from_bytes([9; 16]);
const RECORD_TYPE: TypeId = TypeId::from_bytes([10; 16]);
const RECORD_FIELD_A: FieldId = FieldId::from_bytes([11; 16]);
const RECORD_FIELD_B: FieldId = FieldId::from_bytes([12; 16]);
const ENUM_TYPE: TypeId = TypeId::from_bytes([13; 16]);

fn assignment(field: FieldId, expression: MutationExpression) -> FieldAssignment {
    FieldAssignment::new(TARGET, field, expression)
}

fn plan_with(
    expressions: impl IntoIterator<Item = (FieldId, MutationExpression)>,
) -> ServerMutationPlan {
    mutation_plan_with(None, expressions)
}

fn update_plan_with(
    expressions: impl IntoIterator<Item = (FieldId, MutationExpression)>,
) -> ServerMutationPlan {
    mutation_plan_with(
        Some(MutationSelector::new(FUNCTION_A, PARAMETER_A)),
        expressions,
    )
}

fn mutation_plan_with(
    selector: Option<MutationSelector>,
    expressions: impl IntoIterator<Item = (FieldId, MutationExpression)>,
) -> ServerMutationPlan {
    let assignments = expressions
        .into_iter()
        .map(|(field, expression)| assignment(field, expression));
    match selector {
        Some(selector) => {
            ServerMutationPlan::new_update(TARGET, selector, assignments, TARGET).unwrap()
        }
        None => ServerMutationPlan::new_insert(TARGET, assignments, TARGET).unwrap(),
    }
}

fn record_expression() -> MutationExpression {
    MutationExpression::record_constructor(
        RECORD_TYPE,
        [
            RecordFieldExpression::boolean_literal(RECORD_TYPE, RECORD_FIELD_A, true),
            RecordFieldExpression::parameter(
                RECORD_TYPE,
                RECORD_FIELD_B,
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::named(ENUM_TYPE),
            )
            .unwrap(),
        ],
    )
    .unwrap()
}

#[test]
fn round_trips_all_expression_kinds_and_supported_types() {
    let expressions = [
        (FIELD_A, MutationExpression::boolean_literal(true)),
        (
            FIELD_B,
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::scalar(StandardScalar::Integer),
            )
            .unwrap(),
        ),
        (
            FieldId::from_bytes([10; 16]),
            MutationExpression::typed_null(ResolvedType::reference(REF_TARGET)).unwrap(),
        ),
        (
            FieldId::from_bytes([11; 16]),
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_B,
                ResolvedType::scalar(StandardScalar::BigInt),
            )
            .unwrap(),
        ),
        (
            FieldId::from_bytes([12; 16]),
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::scalar(StandardScalar::Float),
            )
            .unwrap(),
        ),
        (
            FieldId::from_bytes([13; 16]),
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_B,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            )
            .unwrap(),
        ),
        (
            FieldId::from_bytes([14; 16]),
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            )
            .unwrap(),
        ),
        (
            FieldId::from_bytes([15; 16]),
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_B,
                ResolvedType::reference(REF_TARGET),
            )
            .unwrap(),
        ),
    ];
    let plan = plan_with(expressions);
    let encoded = plan.encode().unwrap();
    let decoded = ServerMutationPlan::decode(&encoded).unwrap();
    assert_eq!(decoded, plan);
    assert_eq!(decoded.encode(), Ok(encoded));
    assert_eq!(decoded.target(), TARGET);
    assert_eq!(decoded.returned_object(), TARGET);
    assert_eq!(decoded.assignments()[1].owner(), TARGET);
    assert_eq!(decoded.assignments()[1].field(), FIELD_B);
    assert_eq!(
        decoded.assignments()[1].expression().resolved_type(),
        ResolvedType::scalar(StandardScalar::Integer)
    );
    assert!(!decoded.assignments()[1].expression().nullable());
}

#[test]
fn round_trips_update_with_exact_selector_and_operation() {
    let plan = update_plan_with([
        (
            FIELD_A,
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_B,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            )
            .unwrap(),
        ),
        (FIELD_B, MutationExpression::boolean_literal(false)),
    ]);

    let encoded = plan.encode().unwrap();
    let decoded = ServerMutationPlan::decode(&encoded).unwrap();

    assert_eq!(decoded, plan);
    assert_eq!(decoded.encode(), Ok(encoded));
    assert_eq!(decoded.format_version(), UPDATE_FORMAT_VERSION);
    assert_eq!(
        decoded.operation(),
        &ServerMutationOperation::Update {
            selector: MutationSelector::new(FUNCTION_A, PARAMETER_A)
        }
    );
    assert_eq!(
        decoded.selector(),
        Some(MutationSelector::new(FUNCTION_A, PARAMETER_A))
    );
    assert_eq!(decoded.selector().unwrap().owner(), FUNCTION_A);
    assert_eq!(decoded.selector().unwrap().parameter(), PARAMETER_A);
}

#[test]
fn record_constructor_insert_has_exact_version_four_bytes_and_round_trips() {
    let plan = plan_with([(FIELD_A, record_expression())]);
    let encoded = plan.encode().unwrap();

    assert_eq!(plan.format_version(), RECORD_INSERT_FORMAT_VERSION);
    assert_eq!(
        encoded,
        vec![
            79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3, 3, 3,
            3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 2, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
            10, 10, 10, 0, 0, 0, 0, 2, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
            10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 2, 1, 1, 0, 1, 10,
            10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 12, 12, 12, 12, 12, 12, 12,
            12, 12, 12, 12, 12, 12, 12, 12, 12, 1, 2, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13,
            13, 13, 13, 13, 13, 0, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7,
            7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ]
    );

    let decoded = ServerMutationPlan::decode(&encoded).unwrap();
    assert_eq!(decoded, plan);
    assert_eq!(decoded.encode(), Ok(encoded));
    let MutationExpressionKind::RecordConstructor { fields } =
        decoded.assignments()[0].expression().kind()
    else {
        panic!("version-4 assignment must retain its record constructor");
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].owner(), RECORD_TYPE);
    assert_eq!(fields[0].field(), RECORD_FIELD_A);
    assert_eq!(fields[1].resolved_type(), ResolvedType::named(ENUM_TYPE));
    assert!(matches!(
        fields[1].kind(),
        RecordFieldExpressionKind::Parameter {
            owner: FUNCTION_A,
            parameter: PARAMETER_A,
        }
    ));
}

#[test]
fn record_constructor_versions_and_structure_fail_closed() {
    let valid = plan_with([(FIELD_A, record_expression())])
        .encode()
        .unwrap();

    let mut legacy_version = valid.clone();
    legacy_version[8..12].copy_from_slice(&INSERT_FORMAT_VERSION.to_be_bytes());
    assert_eq!(
        ServerMutationPlan::decode(&legacy_version),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "resolved type kind",
            tag: NAMED_TYPE_TAG,
        })
    );

    let mut wrong_operation = valid.clone();
    wrong_operation[12] = UPDATE_OPERATION_TAG;
    assert_eq!(
        ServerMutationPlan::decode(&wrong_operation),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "operation",
            tag: UPDATE_OPERATION_TAG,
        })
    );

    let mut noncanonical = plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
        .encode()
        .unwrap();
    noncanonical[8..12].copy_from_slice(&RECORD_INSERT_FORMAT_VERSION.to_be_bytes());
    assert_eq!(
        ServerMutationPlan::decode(&noncanonical),
        Err(ServerMutationPlanError::NonCanonicalFormatVersion {
            expected: INSERT_FORMAT_VERSION,
            actual: RECORD_INSERT_FORMAT_VERSION,
        })
    );

    let mut empty = valid.clone();
    empty[84..88].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        ServerMutationPlan::decode(&empty),
        Err(ServerMutationPlanError::EmptyRecordFields)
    );

    let mut over_count = valid.clone();
    over_count[84..88].copy_from_slice(&(MAX_RECORD_FIELDS + 1).to_be_bytes());
    assert_eq!(
        ServerMutationPlan::decode(&over_count),
        Err(ServerMutationPlanError::CollectionLimit {
            kind: "record fields",
            count: MAX_RECORD_FIELDS as usize + 1,
            maximum: MAX_RECORD_FIELDS,
        })
    );

    let mut wrong_owner = valid.clone();
    wrong_owner[88..104].copy_from_slice(&OTHER_TARGET.to_bytes());
    assert_eq!(
        ServerMutationPlan::decode(&wrong_owner),
        Err(ServerMutationPlanError::RecordFieldOwnerMismatch {
            position: 0,
            expected: RECORD_TYPE,
            actual: OTHER_TARGET,
        })
    );

    let mut duplicate = valid.clone();
    duplicate[141..157].copy_from_slice(&RECORD_FIELD_A.to_bytes());
    assert_eq!(
        ServerMutationPlan::decode(&duplicate),
        Err(ServerMutationPlanError::DuplicateRecordField {
            first: 0,
            duplicate: 1,
            owner: RECORD_TYPE,
            field: RECORD_FIELD_A,
        })
    );

    let mut child_kind = valid.clone();
    child_kind[120] = TYPED_NULL_EXPRESSION_TAG;
    assert_eq!(
        ServerMutationPlan::decode(&child_kind),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "record field expression kind",
            tag: TYPED_NULL_EXPRESSION_TAG,
        })
    );

    let mut child_nullable = valid.clone();
    child_nullable[123] = 1;
    assert_eq!(
        ServerMutationPlan::decode(&child_nullable),
        Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
            expression_kind: "record field",
            expected: false,
            actual: true,
        })
    );

    let mut child_boolean = valid.clone();
    child_boolean[124] = 2;
    assert_eq!(
        ServerMutationPlan::decode(&child_boolean),
        Err(ServerMutationPlanError::InvalidBoolean {
            context: "record literal",
            value: 2,
        })
    );

    let mut outer_nullable = valid.clone();
    outer_nullable[83] = 1;
    assert_eq!(
        ServerMutationPlan::decode(&outer_nullable),
        Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
            expression_kind: "record constructor",
            expected: false,
            actual: true,
        })
    );

    for prefix in 0..valid.len() {
        assert_eq!(
            ServerMutationPlan::decode(&valid[..prefix]),
            Err(ServerMutationPlanError::Truncated),
            "prefix {prefix}"
        );
    }
    let mut trailing = valid;
    trailing.push(0);
    assert_eq!(
        ServerMutationPlan::decode(&trailing),
        Err(ServerMutationPlanError::TrailingBytes)
    );
}

#[test]
fn record_constructor_builders_enforce_the_closed_child_and_plan_shapes() {
    assert_eq!(
        MutationExpression::record_constructor(RECORD_TYPE, Vec::new()),
        Err(ServerMutationPlanError::EmptyRecordFields)
    );
    assert_eq!(
        MutationExpression::record_constructor(
            RECORD_TYPE,
            [RecordFieldExpression::boolean_literal(
                OTHER_TARGET,
                RECORD_FIELD_A,
                true,
            )],
        ),
        Err(ServerMutationPlanError::RecordFieldOwnerMismatch {
            position: 0,
            expected: RECORD_TYPE,
            actual: OTHER_TARGET,
        })
    );
    assert_eq!(
        MutationExpression::record_constructor(
            RECORD_TYPE,
            [
                RecordFieldExpression::boolean_literal(RECORD_TYPE, RECORD_FIELD_A, true,),
                RecordFieldExpression::boolean_literal(RECORD_TYPE, RECORD_FIELD_A, false,),
            ],
        ),
        Err(ServerMutationPlanError::DuplicateRecordField {
            first: 0,
            duplicate: 1,
            owner: RECORD_TYPE,
            field: RECORD_FIELD_A,
        })
    );
    assert_eq!(
        RecordFieldExpression::parameter(
            RECORD_TYPE,
            RECORD_FIELD_A,
            FUNCTION_A,
            PARAMETER_A,
            ResolvedType::reference(REF_TARGET),
        ),
        Err(ServerMutationPlanError::UnsupportedValueType {
            resolved_type: ResolvedType::reference(REF_TARGET),
        })
    );

    let update = ServerMutationPlan::new_update(
        TARGET,
        MutationSelector::new(FUNCTION_A, PARAMETER_A),
        [assignment(FIELD_A, record_expression())],
        TARGET,
    );
    assert_eq!(
        update,
        Err(ServerMutationPlanError::RecordConstructorRequiresInsert)
    );

    let mixed_owners = MutationExpression::record_constructor(
        RECORD_TYPE,
        [
            RecordFieldExpression::parameter(
                RECORD_TYPE,
                RECORD_FIELD_A,
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::scalar(StandardScalar::Integer),
            )
            .unwrap(),
            RecordFieldExpression::parameter(
                RECORD_TYPE,
                RECORD_FIELD_B,
                FUNCTION_B,
                PARAMETER_B,
                ResolvedType::named(ENUM_TYPE),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(
        ServerMutationPlan::new_insert(TARGET, [assignment(FIELD_A, mixed_owners)], TARGET,),
        Err(ServerMutationPlanError::MixedParameterOwners {
            first: 0,
            assignment: 0,
            expected: FUNCTION_A,
            actual: FUNCTION_B,
        })
    );

    let too_many = (0..=MAX_RECORD_FIELDS).map(|index| {
        RecordFieldExpression::boolean_literal(
            RECORD_TYPE,
            FieldId::from_bytes([(index % 251) as u8; 16]),
            true,
        )
    });
    assert!(matches!(
        MutationExpression::record_constructor(RECORD_TYPE, too_many),
        Err(ServerMutationPlanError::CollectionLimit {
            kind: "record fields",
            count,
            maximum: MAX_RECORD_FIELDS,
        }) if count == MAX_RECORD_FIELDS as usize + 1
    ));
}

#[test]
fn round_trips_delete_with_exact_target_selector_and_version() {
    let plan = ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A));

    let encoded = plan.encode().unwrap();
    let decoded = ServerDeletePlan::decode(&encoded).unwrap();

    assert_eq!(decoded, plan);
    assert_eq!(decoded.encode(), Ok(encoded));
    assert_eq!(decoded.format_version(), DELETE_FORMAT_VERSION);
    assert_eq!(decoded.target(), TARGET);
    assert_eq!(decoded.selector().owner(), FUNCTION_A);
    assert_eq!(decoded.selector().parameter(), PARAMETER_A);
}

#[test]
fn encodes_minimal_boolean_golden_exactly() {
    let plan = plan_with([(FIELD_A, MutationExpression::boolean_literal(true))]);
    assert_eq!(
        plan.encode().unwrap(),
        vec![
            79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3, 3, 3,
            3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1,
        ]
    );
}

#[test]
fn encodes_minimal_update_golden_exactly() {
    let plan = update_plan_with([(FIELD_A, MutationExpression::boolean_literal(false))]);
    assert_eq!(
        plan.encode().unwrap(),
        vec![
            79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            7, 7, 7, 7, 7, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3,
            3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 1, 1, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1,
        ]
    );
}

#[test]
fn encodes_minimal_delete_golden_exactly() {
    let plan = ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A));
    assert_eq!(
        plan.encode().unwrap(),
        vec![
            79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 3, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            7, 7, 7, 7, 7,
        ]
    );
}

#[test]
fn encodes_rich_parameter_and_typed_null_golden_exactly() {
    let plan = plan_with([
        (
            FIELD_A,
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::reference(REF_TARGET),
            )
            .unwrap(),
        ),
        (
            FIELD_B,
            MutationExpression::typed_null(ResolvedType::scalar(
                StandardScalar::CharacterLargeObject,
            ))
            .unwrap(),
        ),
    ]);
    let encoded = plan.encode().unwrap();
    assert_eq!(&encoded[..8], b"ORNAMP\0\0");
    assert_eq!(&encoded[8..13], &[0, 0, 0, 1, 1]);
    assert_eq!(
        encoded,
        vec![
            79, 82, 78, 65, 77, 80, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 0, 0, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3, 3, 3,
            3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 3, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 0, 5,
            5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
            7, 7, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
            4, 4, 4, 4, 4, 3, 1, 6, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ]
    );
    assert!(
        encoded
            .windows(b"INSERT".len())
            .all(|window| window != b"INSERT")
    );
}

#[test]
fn private_tags_are_append_only() {
    assert_eq!(INSERT_OPERATION_TAG, 1);
    assert_eq!(UPDATE_OPERATION_TAG, 2);
    assert_eq!(DELETE_OPERATION_TAG, 3);
    assert_eq!(INSERT_FORMAT_VERSION, 1);
    assert_eq!(UPDATE_FORMAT_VERSION, 2);
    assert_eq!(DELETE_FORMAT_VERSION, 3);
    assert_eq!(RECORD_INSERT_FORMAT_VERSION, 4);
    assert_eq!(
        [
            PARAMETER_EXPRESSION_TAG,
            BOOLEAN_EXPRESSION_TAG,
            TYPED_NULL_EXPRESSION_TAG,
            RECORD_CONSTRUCTOR_EXPRESSION_TAG,
        ],
        [1, 2, 3, 4]
    );
    assert_eq!(
        [SCALAR_TYPE_TAG, NAMED_TYPE_TAG, REFERENCE_TYPE_TAG],
        [1, 2, 3]
    );
    assert_eq!(
        [
            BOOLEAN_SCALAR_TAG,
            INTEGER_SCALAR_TAG,
            BIGINT_SCALAR_TAG,
            FLOAT_SCALAR_TAG,
            CLOB_SCALAR_TAG,
            BLOB_SCALAR_TAG,
        ],
        [1, 2, 3, 4, 6, 7]
    );
}

#[test]
fn rejects_corruption_truncation_trailing_and_limits() {
    let valid = plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
        .encode()
        .unwrap();
    let mut magic = valid.clone();
    magic[0] = b'X';
    assert_eq!(
        ServerMutationPlan::decode(&magic),
        Err(ServerMutationPlanError::InvalidMagic)
    );
    let mut version = valid.clone();
    version[11] = 3;
    assert_eq!(
        ServerMutationPlan::decode(&version),
        Err(ServerMutationPlanError::UnsupportedVersion(3))
    );
    let mut operation = valid.clone();
    operation[12] = 99;
    assert_eq!(
        ServerMutationPlan::decode(&operation),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "operation",
            tag: 99
        })
    );
    let mut expression_tag = valid.clone();
    expression_tag[65] = 99;
    assert_eq!(
        ServerMutationPlan::decode(&expression_tag),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "mutation expression kind",
            tag: 99
        })
    );
    let mut type_tag = valid.clone();
    type_tag[66] = NAMED_TYPE_TAG;
    assert_eq!(
        ServerMutationPlan::decode(&type_tag),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "resolved type kind",
            tag: NAMED_TYPE_TAG
        })
    );
    let mut scalar_tag = valid.clone();
    scalar_tag[67] = 99;
    assert_eq!(
        ServerMutationPlan::decode(&scalar_tag),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "scalar type",
            tag: 99
        })
    );
    let mut nullability = valid.clone();
    nullability[68] = 2;
    assert_eq!(
        ServerMutationPlan::decode(&nullability),
        Err(ServerMutationPlanError::InvalidBoolean {
            context: "expression nullability",
            value: 2
        })
    );
    let mut boolean = valid.clone();
    boolean[69] = 2;
    assert_eq!(
        ServerMutationPlan::decode(&boolean),
        Err(ServerMutationPlanError::InvalidBoolean {
            context: "literal",
            value: 2
        })
    );
    for prefix in 0..valid.len() {
        assert_eq!(
            ServerMutationPlan::decode(&valid[..prefix]),
            Err(ServerMutationPlanError::Truncated)
        );
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    assert_eq!(
        ServerMutationPlan::decode(&trailing),
        Err(ServerMutationPlanError::TrailingBytes)
    );
    let oversized = vec![0; MAX_ARTIFACT_BYTES + 1];
    assert_eq!(
        ServerMutationPlan::decode(&oversized),
        Err(ServerMutationPlanError::ArtifactSizeLimit {
            size: MAX_ARTIFACT_BYTES + 1,
            maximum: MAX_ARTIFACT_BYTES,
        })
    );
    let mut over_count = valid.clone();
    over_count[29..33].copy_from_slice(&(MAX_ASSIGNMENTS + 1).to_be_bytes());
    assert_eq!(
        ServerMutationPlan::decode(&over_count),
        Err(ServerMutationPlanError::CollectionLimit {
            kind: "assignments",
            count: MAX_ASSIGNMENTS as usize + 1,
            maximum: MAX_ASSIGNMENTS,
        })
    );
    let mut empty = valid.clone();
    empty[29..33].copy_from_slice(&0_u32.to_be_bytes());
    empty.drain(33..70);
    assert_eq!(
        ServerMutationPlan::decode(&empty),
        Err(ServerMutationPlanError::EmptyAssignments)
    );

    let rich = plan_with([
        (
            FIELD_A,
            MutationExpression::parameter(
                FUNCTION_A,
                PARAMETER_A,
                ResolvedType::reference(REF_TARGET),
            )
            .unwrap(),
        ),
        (
            FIELD_B,
            MutationExpression::typed_null(ResolvedType::scalar(
                StandardScalar::CharacterLargeObject,
            ))
            .unwrap(),
        ),
    ])
    .encode()
    .unwrap();
    let mut nullable_parameter = rich.clone();
    nullable_parameter[83] = 1;
    assert_eq!(
        ServerMutationPlan::decode(&nullable_parameter),
        Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
            expression_kind: "parameter",
            expected: false,
            actual: true
        })
    );
    let mut nonnullable_null = rich;
    nonnullable_null[151] = 0;
    assert_eq!(
        ServerMutationPlan::decode(&nonnullable_null),
        Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
            expression_kind: "typed NULL",
            expected: true,
            actual: false
        })
    );
}

#[test]
fn rejects_update_version_operation_and_selector_corruption() {
    let valid = update_plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
        .encode()
        .unwrap();

    let mut insert_operation = valid.clone();
    insert_operation[12] = INSERT_OPERATION_TAG;
    assert_eq!(
        ServerMutationPlan::decode(&insert_operation),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "operation",
            tag: INSERT_OPERATION_TAG,
        })
    );

    let mut update_operation_in_v1 = valid.clone();
    update_operation_in_v1[11] = INSERT_FORMAT_VERSION as u8;
    assert_eq!(
        ServerMutationPlan::decode(&update_operation_in_v1),
        Err(ServerMutationPlanError::InvalidEnumTag {
            kind: "operation",
            tag: UPDATE_OPERATION_TAG,
        })
    );

    for prefix in 0..valid.len() {
        assert_eq!(
            ServerMutationPlan::decode(&valid[..prefix]),
            Err(ServerMutationPlanError::Truncated)
        );
    }
}

#[test]
fn rejects_delete_header_truncation_and_trailing_corruption() {
    let valid = ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A))
        .encode()
        .unwrap();

    let mut magic = valid.clone();
    magic[0] = b'X';
    assert_eq!(
        ServerDeletePlan::decode(&magic),
        Err(ServerMutationPlanError::InvalidMagic)
    );

    for version in [INSERT_FORMAT_VERSION, UPDATE_FORMAT_VERSION, 99] {
        let mut wrong_version = valid.clone();
        wrong_version[8..12].copy_from_slice(&version.to_be_bytes());
        assert_eq!(
            ServerDeletePlan::decode(&wrong_version),
            Err(ServerMutationPlanError::UnsupportedVersion(version))
        );
    }

    for operation in [INSERT_OPERATION_TAG, UPDATE_OPERATION_TAG, 99] {
        let mut wrong_operation = valid.clone();
        wrong_operation[12] = operation;
        assert_eq!(
            ServerDeletePlan::decode(&wrong_operation),
            Err(ServerMutationPlanError::InvalidEnumTag {
                kind: "operation",
                tag: operation,
            })
        );
    }

    assert_eq!(
        ServerMutationPlan::decode(&valid),
        Err(ServerMutationPlanError::UnsupportedVersion(
            DELETE_FORMAT_VERSION
        ))
    );
    for prefix in 0..valid.len() {
        assert_eq!(
            ServerDeletePlan::decode(&valid[..prefix]),
            Err(ServerMutationPlanError::Truncated)
        );
    }
    let mut trailing = valid;
    trailing.push(0);
    assert_eq!(
        ServerDeletePlan::decode(&trailing),
        Err(ServerMutationPlanError::TrailingBytes)
    );
    let oversized = vec![0; MAX_ARTIFACT_BYTES + 1];
    assert_eq!(
        ServerDeletePlan::decode(&oversized),
        Err(ServerMutationPlanError::ArtifactSizeLimit {
            size: MAX_ARTIFACT_BYTES + 1,
            maximum: MAX_ARTIFACT_BYTES,
        })
    );
}

#[test]
fn rejects_all_plan_and_expression_invariants() {
    assert_eq!(
        ServerMutationPlan::new_insert(TARGET, Vec::new(), TARGET),
        Err(ServerMutationPlanError::EmptyAssignments)
    );
    assert_eq!(
        ServerMutationPlan::new_update(
            TARGET,
            MutationSelector::new(FUNCTION_A, PARAMETER_A),
            Vec::new(),
            TARGET,
        ),
        Err(ServerMutationPlanError::EmptyAssignments)
    );
    let too_many = (0..=MAX_ASSIGNMENTS).map(|index| {
        assignment(
            FieldId::from_bytes([index as u8; 16]),
            MutationExpression::boolean_literal(true),
        )
    });
    assert!(matches!(
        ServerMutationPlan::new_insert(TARGET, too_many, TARGET),
        Err(ServerMutationPlanError::CollectionLimit { .. })
    ));
    assert!(matches!(
        ServerMutationPlan::new_insert(
            TARGET,
            [FieldAssignment::new(
                OTHER_TARGET,
                FIELD_A,
                MutationExpression::boolean_literal(true)
            )],
            TARGET,
        ),
        Err(ServerMutationPlanError::AssignmentOwnerMismatch { assignment: 0, .. })
    ));
    assert!(matches!(
        ServerMutationPlan::new_insert(
            TARGET,
            [
                assignment(FIELD_A, MutationExpression::boolean_literal(true)),
                assignment(FIELD_A, MutationExpression::boolean_literal(false)),
            ],
            TARGET,
        ),
        Err(ServerMutationPlanError::DuplicateFieldAssignment {
            first: 0,
            duplicate: 1,
            ..
        })
    ));
    assert_eq!(
        ServerMutationPlan::new_update(
            TARGET,
            MutationSelector::new(FUNCTION_A, PARAMETER_A),
            [assignment(
                FIELD_A,
                MutationExpression::parameter(
                    FUNCTION_B,
                    PARAMETER_B,
                    ResolvedType::scalar(StandardScalar::Integer),
                )
                .unwrap(),
            )],
            TARGET,
        ),
        Err(ServerMutationPlanError::SelectorParameterOwnerMismatch {
            assignment: 0,
            selector_owner: FUNCTION_A,
            assignment_owner: FUNCTION_B,
        })
    );
    assert!(matches!(
        ServerMutationPlan::new_insert(
            TARGET,
            [
                assignment(
                    FIELD_A,
                    MutationExpression::parameter(
                        FUNCTION_A,
                        PARAMETER_A,
                        ResolvedType::scalar(StandardScalar::Integer)
                    )
                    .unwrap()
                ),
                assignment(
                    FIELD_B,
                    MutationExpression::parameter(
                        FUNCTION_B,
                        PARAMETER_B,
                        ResolvedType::scalar(StandardScalar::Integer)
                    )
                    .unwrap()
                ),
            ],
            TARGET,
        ),
        Err(ServerMutationPlanError::MixedParameterOwners {
            first: 0,
            assignment: 1,
            ..
        })
    ));
    assert_eq!(
        ServerMutationPlan::new_insert(
            TARGET,
            [assignment(
                FIELD_A,
                MutationExpression::boolean_literal(true)
            )],
            OTHER_TARGET
        ),
        Err(ServerMutationPlanError::ReturnedObjectMismatch {
            target: TARGET,
            returned: OTHER_TARGET
        })
    );
    for resolved_type in [
        ResolvedType::scalar(StandardScalar::Decimal),
        ResolvedType::scalar(StandardScalar::Uuid),
        ResolvedType::scalar(StandardScalar::Date),
        ResolvedType::scalar(StandardScalar::Time),
        ResolvedType::scalar(StandardScalar::Timestamp),
        ResolvedType::scalar(StandardScalar::Duration),
        ResolvedType::scalar(StandardScalar::Void),
        ResolvedType::named(OTHER_TARGET),
    ] {
        assert!(matches!(
            MutationExpression::parameter(FUNCTION_A, PARAMETER_A, resolved_type),
            Err(ServerMutationPlanError::UnsupportedValueType { .. })
        ));
        assert!(matches!(
            MutationExpression::typed_null(resolved_type),
            Err(ServerMutationPlanError::UnsupportedValueType { .. })
        ));
    }
    assert!(matches!(
        MutationExpression::typed_null(ResolvedType::scalar(StandardScalar::Integer)),
        Ok(expression) if expression.nullable()
    ));
    let wrong_boolean_type = ServerMutationPlan {
        operation: ServerMutationOperation::Insert,
        target: TARGET,
        assignments: vec![assignment(
            FIELD_A,
            MutationExpression {
                kind: MutationExpressionKind::BooleanLiteral { value: true },
                resolved_type: ResolvedType::scalar(StandardScalar::Integer),
                nullable: false,
            },
        )],
        returned_object: TARGET,
    };
    assert_eq!(
        wrong_boolean_type.encode(),
        Err(ServerMutationPlanError::ExpressionTypeMismatch {
            expression_kind: "BOOLEAN literal",
            expected: ResolvedType::scalar(StandardScalar::Boolean),
            actual: ResolvedType::scalar(StandardScalar::Integer),
        })
    );
    let nullable_parameter = ServerMutationPlan {
        operation: ServerMutationOperation::Insert,
        target: TARGET,
        assignments: vec![assignment(
            FIELD_A,
            MutationExpression {
                kind: MutationExpressionKind::Parameter {
                    owner: FUNCTION_A,
                    parameter: PARAMETER_A,
                },
                resolved_type: ResolvedType::scalar(StandardScalar::Integer),
                nullable: true,
            },
        )],
        returned_object: TARGET,
    };
    assert_eq!(
        nullable_parameter.encode(),
        Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
            expression_kind: "parameter",
            expected: false,
            actual: true,
        })
    );
    let nullable_boolean = ServerMutationPlan {
        operation: ServerMutationOperation::Insert,
        target: TARGET,
        assignments: vec![assignment(
            FIELD_A,
            MutationExpression {
                kind: MutationExpressionKind::BooleanLiteral { value: true },
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: true,
            },
        )],
        returned_object: TARGET,
    };
    assert_eq!(
        nullable_boolean.encode(),
        Err(ServerMutationPlanError::ExpressionNullabilityMismatch {
            expression_kind: "BOOLEAN literal",
            expected: false,
            actual: true,
        })
    );
}

#[test]
fn no_source_or_backend_names_enter_encoded_bytes() {
    let encoded_plans = [
        plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
            .encode()
            .unwrap(),
        update_plan_with([(FIELD_A, MutationExpression::boolean_literal(true))])
            .encode()
            .unwrap(),
        ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION_A, PARAMETER_A))
            .encode()
            .unwrap(),
    ];
    for encoded in encoded_plans {
        for forbidden in [
            b"INSERT".as_slice(),
            b"UPDATE".as_slice(),
            b"DELETE".as_slice(),
            b"_orna".as_slice(),
            b"source".as_slice(),
            b"tasks".as_slice(),
            b"created".as_slice(),
            b"updated".as_slice(),
            b"deleted".as_slice(),
        ] {
            assert!(
                !encoded
                    .windows(forbidden.len())
                    .any(|window| window == forbidden)
            );
        }
    }
}
