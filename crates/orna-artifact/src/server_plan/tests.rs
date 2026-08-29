
use super::*;

const TASK: TypeId = TypeId::from_bytes([1; 16]);
const PERSON: TypeId = TypeId::from_bytes([2; 16]);
const TITLE: FieldId = FieldId::from_bytes([3; 16]);
const ASSIGNEE: FieldId = FieldId::from_bytes([4; 16]);
const NAME: FieldId = FieldId::from_bytes([5; 16]);
const FUNCTION: FunctionId = FunctionId::from_bytes([6; 16]);
const PARAMETER: ParameterId = ParameterId::from_bytes([7; 16]);

fn value_type(resolved_type: ResolvedType, nullable: bool) -> ValueType {
    ValueType {
        resolved_type,
        nullable,
    }
}

fn object_reference() -> Expression {
    Expression {
        kind: ExpressionKind::ObjectReference { input: 0 },
        value_type: value_type(ResolvedType::reference(TASK), false),
    }
}

fn title_path() -> Expression {
    Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![FieldStep {
                owner: TASK,
                field: TITLE,
            }],
        },
        value_type: value_type(
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        ),
    }
}

fn assigned_name_path() -> Expression {
    Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![
                FieldStep {
                    owner: TASK,
                    field: ASSIGNEE,
                },
                FieldStep {
                    owner: PERSON,
                    field: NAME,
                },
            ],
        },
        value_type: value_type(ResolvedType::named(PERSON), true),
    }
}

fn boolean(value: bool) -> Expression {
    Expression {
        kind: ExpressionKind::BooleanLiteral { value },
        value_type: value_type(ResolvedType::scalar(StandardScalar::Boolean), false),
    }
}

fn equality(left: Expression, right: Expression) -> Expression {
    Expression {
        value_type: value_type(
            ResolvedType::scalar(StandardScalar::Boolean),
            left.value_type.nullable || right.value_type.nullable,
        ),
        kind: ExpressionKind::Equality {
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

fn full_boolean_tree(depth: u32) -> Expression {
    if depth == 0 {
        boolean(true)
    } else {
        equality(full_boolean_tree(depth - 1), full_boolean_tree(depth - 1))
    }
}

fn plan() -> ServerPlan {
    ServerPlan {
        scan: Scan {
            input: 0,
            object_type: TASK,
        },
        projections: vec![
            object_reference(),
            title_path(),
            assigned_name_path(),
            equality(boolean(true), boolean(false)),
        ],
        selection: Some(equality(boolean(true), boolean(false))),
        ordering: vec![Ordering {
            expression: title_path(),
            direction: SortDirection::Descending,
            null_order: NullOrder::Unspecified,
        }],
    }
}

fn identity_selected_plan() -> IdentitySelectedServerPlan {
    IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: TASK,
        },
        [boolean(true)],
        IdentitySelector::new(FUNCTION, PARAMETER),
    )
    .unwrap()
}

fn distinct_plan(selection: Option<Expression>) -> DistinctServerPlan {
    DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: TASK,
        },
        [boolean(true)],
        selection,
    )
    .unwrap()
}

fn unique_text_selected_plan() -> UniqueTextSelectedServerPlan {
    UniqueTextSelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: TASK,
        },
        [boolean(true)],
        SelectBindValue::Text {
            scan_object_type: TASK,
            field_owner: TASK,
            field: TITLE,
            parameter_owner: FUNCTION,
            parameter: PARAMETER,
            resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            field_nullable: true,
            parameter_required_non_null: true,
        },
    )
    .unwrap()
}

#[test]
fn unique_text_selected_plan_has_a_sealed_v4_wire_format() {
    let plan = unique_text_selected_plan();
    let encoded = plan.encode().unwrap();

    assert_eq!(plan.format_version(), UNIQUE_TEXT_SELECTED_FORMAT_VERSION);
    assert_eq!(
        UniqueTextSelectedServerPlan::decode(&encoded),
        Ok(plan.clone())
    );
    assert_eq!(
        UniqueTextSelectedServerPlan::decode(&encoded)
            .unwrap()
            .encode(),
        Ok(encoded)
    );
}

#[test]
fn unique_text_selected_plan_rejects_hostile_selector_shapes() {
    let plan = unique_text_selected_plan();
    let encoded = plan.encode().unwrap();

    let missing_selection = {
        let mut bytes = encoded.clone();
        bytes[45] = 0;
        bytes
    };
    assert!(UniqueTextSelectedServerPlan::decode(&missing_selection).is_err());

    let wrong_owner = {
        let mut bytes = encoded.clone();
        bytes[63] = 2;
        bytes
    };
    assert!(UniqueTextSelectedServerPlan::decode(&wrong_owner).is_err());

    let malformed_selector = {
        let mut bytes = encoded.clone();
        bytes[46] = 2;
        bytes
    };
    assert!(UniqueTextSelectedServerPlan::decode(&malformed_selector).is_err());

    let nullable_parameter = {
        let mut bytes = encoded.clone();
        bytes[130] = 0;
        bytes
    };
    assert!(UniqueTextSelectedServerPlan::decode(&nullable_parameter).is_err());

    let ordering = {
        let mut bytes = encoded.clone();
        bytes[134] = 1;
        bytes
    };
    assert!(UniqueTextSelectedServerPlan::decode(&ordering).is_err());

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        UniqueTextSelectedServerPlan::decode(&trailing),
        Err(ServerPlanError::TrailingBytes)
    );

    assert!(matches!(
        UniqueTextSelectedServerPlan::new(
            Scan {
                input: 1,
                object_type: TASK,
            },
            [boolean(true)],
            *plan.selector(),
        ),
        Err(ServerPlanError::InvalidInputSlot(1))
    ));
    assert!(matches!(
        UniqueTextSelectedServerPlan::new(
            plan.scan(),
            plan.projections().iter().cloned(),
            SelectBindValue::Text {
                scan_object_type: TypeId::from_bytes([0x95; 16]),
                field_owner: TASK,
                field: TITLE,
                parameter_owner: FUNCTION,
                parameter: PARAMETER,
                resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                field_nullable: true,
                parameter_required_non_null: true,
            },
        ),
        Err(ServerPlanError::InvalidModel(
            "a unique-Text selector scan object must match the plan scan"
        ))
    ));

    assert!(matches!(
        UniqueTextSelectedServerPlan::new(
            plan.scan(),
            plan.projections().iter().cloned(),
            SelectBindValue::Text {
                scan_object_type: TASK,
                field_owner: TASK,
                field: TITLE,
                parameter_owner: FUNCTION,
                parameter: PARAMETER,
                resolved_type: ResolvedType::scalar(StandardScalar::Integer),
                field_nullable: true,
                parameter_required_non_null: true,
            },
        ),
        Err(ServerPlanError::InvalidModel(
            "a unique-Text selector must use resolved TEXT type"
        ))
    ));
}

#[test]
fn unique_text_selected_plan_preserves_standard_value_text_identity() {
    let plan = UniqueTextSelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: TASK,
        },
        [boolean(true)],
        SelectBindValue::Text {
            scan_object_type: TASK,
            field_owner: TASK,
            field: TITLE,
            parameter_owner: FUNCTION,
            parameter: PARAMETER,
            resolved_type: ResolvedType::value(TypeId::from_bytes([0x94; 16])),
            field_nullable: false,
            parameter_required_non_null: true,
        },
    )
    .unwrap();

    assert_eq!(
        UniqueTextSelectedServerPlan::decode(&plan.encode().unwrap())
            .unwrap()
            .selector(),
        plan.selector()
    );
}

fn typed_field_path(resolved_type: ResolvedType, nullable: bool) -> Expression {
    Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![FieldStep {
                owner: TASK,
                field: TITLE,
            }],
        },
        value_type: value_type(resolved_type, nullable),
    }
}

#[test]
fn distinct_plan_has_a_closed_v3_wire_format() {
    let plan = distinct_plan(None);
    let encoded = plan.encode().unwrap();

    assert_eq!(plan.format_version(), DISTINCT_FORMAT_VERSION);
    assert_eq!(plan.scan().object_type, TASK);
    assert_eq!(plan.projections(), [boolean(true)]);
    assert_eq!(plan.selection(), None);
    assert_eq!(encoded.len(), 50);
    assert_eq!(
        encoded,
        vec![
            79, 82, 78, 65, 83, 80, 0, 0, 0, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 3, 1, 1, 0, 1, 0, 0, 0, 0, 0,
        ]
    );
}

#[test]
fn distinct_plan_round_trips_optional_selection_canonically() {
    for plan in [
        distinct_plan(None),
        distinct_plan(Some(boolean(false))),
        distinct_plan(Some(typed_field_path(
            ResolvedType::scalar(StandardScalar::Boolean),
            true,
        ))),
    ] {
        let encoded = plan.encode().unwrap();

        assert_eq!(DistinctServerPlan::decode(&encoded), Ok(plan.clone()));
        assert_eq!(
            DistinctServerPlan::decode(&encoded).unwrap().encode(),
            Ok(encoded)
        );
    }
}

#[test]
fn distinct_projection_domain_is_exhaustive_and_ignores_nullability() {
    for scalar in StandardScalar::ALL {
        let expected = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        );
        for nullable in [false, true] {
            let resolved_type = ResolvedType::scalar(scalar);
            let result = DistinctServerPlan::new(
                Scan {
                    input: 0,
                    object_type: TASK,
                },
                [typed_field_path(resolved_type, nullable)],
                None,
            );
            if expected {
                assert!(result.is_ok(), "unexpected rejection for {scalar:?}");
            } else {
                assert_eq!(
                    result,
                    Err(ServerPlanError::UnsupportedDistinctProjectionType { resolved_type }),
                    "unexpected acceptance for {scalar:?}",
                );
            }
        }
    }

    for nullable in [false, true] {
        assert!(
            DistinctServerPlan::new(
                Scan {
                    input: 0,
                    object_type: TASK,
                },
                [typed_field_path(ResolvedType::reference(PERSON), nullable,)],
                None,
            )
            .is_ok()
        );

        let resolved_type = ResolvedType::named(PERSON);
        assert_eq!(
            DistinctServerPlan::new(
                Scan {
                    input: 0,
                    object_type: TASK,
                },
                [typed_field_path(resolved_type, nullable)],
                None,
            ),
            Err(ServerPlanError::UnsupportedDistinctProjectionType { resolved_type })
        );
    }

    let mut unsupported_payload = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: TASK,
        },
        [typed_field_path(
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )],
        None,
    )
    .unwrap()
    .encode()
    .unwrap();
    unsupported_payload[42] = 6;
    assert_eq!(
        DistinctServerPlan::decode(&unsupported_payload),
        Err(ServerPlanError::UnsupportedDistinctProjectionType {
            resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        })
    );
}

#[test]
fn distinct_plan_rejects_private_parameter_tags_in_every_expression_position() {
    let mut projection = distinct_plan(None).encode().unwrap();
    projection[40] = 5;
    assert_eq!(
        DistinctServerPlan::decode(&projection),
        Err(ServerPlanError::InvalidEnumTag {
            kind: "expression kind",
            tag: 5,
        })
    );

    let mut selection = distinct_plan(Some(boolean(true))).encode().unwrap();
    selection[46] = 5;
    assert_eq!(
        DistinctServerPlan::decode(&selection),
        Err(ServerPlanError::InvalidEnumTag {
            kind: "expression kind",
            tag: 5,
        })
    );

    let nested_plan = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: TASK,
        },
        [equality(boolean(true), boolean(false))],
        None,
    )
    .unwrap();
    let mut nested = nested_plan.encode().unwrap();
    nested[44] = 5;
    assert_eq!(
        DistinctServerPlan::decode(&nested),
        Err(ServerPlanError::InvalidEnumTag {
            kind: "expression kind",
            tag: 5,
        })
    );
}

#[test]
fn distinct_plan_rejects_ordering_with_a_typed_error() {
    let mut encoded = distinct_plan(None).encode().unwrap();
    let ordering_offset = encoded.len() - 4;
    encoded[ordering_offset..].copy_from_slice(&1_u32.to_be_bytes());

    assert_eq!(
        DistinctServerPlan::decode(&encoded),
        Err(ServerPlanError::DistinctOrderingNotAllowed { count: 1 })
    );
}

#[test]
fn distinct_plan_enforces_the_exact_shared_expression_node_budget() {
    let scan = Scan {
        input: 0,
        object_type: TASK,
    };
    let maximum_projection = full_boolean_tree(12);
    let maximum =
        DistinctServerPlan::new(scan, [maximum_projection.clone()], Some(boolean(true))).unwrap();
    let encoded = maximum.encode().unwrap();
    assert_eq!(DistinctServerPlan::decode(&encoded), Ok(maximum));

    let oversized_selection = equality(boolean(true), boolean(false));
    assert_eq!(
        DistinctServerPlan::new(
            scan,
            [maximum_projection.clone()],
            Some(oversized_selection.clone()),
        ),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );

    let unchecked = DistinctServerPlan {
        scan,
        projections: vec![maximum_projection.clone()],
        selection: Some(oversized_selection.clone()),
    };
    assert_eq!(
        unchecked.encode(),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );

    let mut writer =
        encode_plan_prefix(DISTINCT_FORMAT_VERSION, scan, &[maximum_projection]).unwrap();
    encode_optional_selection(&mut writer, Some(&oversized_selection)).unwrap();
    encode_count(&mut writer, "ordering", 0, MAX_ORDERING).unwrap();
    assert_eq!(
        DistinctServerPlan::decode(&writer.finish()),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );
}

#[test]
fn distinct_plan_rejects_corruption_and_shared_limits() {
    assert_eq!(
        DistinctServerPlan::new(
            Scan {
                input: 1,
                object_type: TASK,
            },
            [boolean(true)],
            None,
        ),
        Err(ServerPlanError::InvalidInputSlot(1))
    );
    assert_eq!(
        DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            [],
            None,
        ),
        Err(ServerPlanError::InvalidModel(
            "a server plan must contain at least one projection"
        ))
    );
    assert_eq!(
        DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            std::iter::repeat_with(|| boolean(true)).take(MAX_PROJECTIONS as usize + 1),
            None,
        ),
        Err(ServerPlanError::CollectionLimit {
            kind: "projections",
            count: MAX_PROJECTIONS + 1,
            maximum: MAX_PROJECTIONS,
        })
    );
    assert_eq!(
        DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            [boolean(true)],
            Some(typed_field_path(
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )),
        ),
        Err(ServerPlanError::InvalidModel(
            "a selection must have resolved BOOLEAN type"
        ))
    );

    let empty_path = Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: Vec::new(),
        },
        value_type: value_type(ResolvedType::scalar(StandardScalar::Integer), false),
    };
    assert_eq!(
        DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            [empty_path],
            None,
        ),
        Err(ServerPlanError::EmptyFieldPath)
    );

    let oversized_path = Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![
                FieldStep {
                    owner: TASK,
                    field: TITLE,
                };
                MAX_FIELD_PATH_STEPS as usize + 1
            ],
        },
        value_type: value_type(ResolvedType::scalar(StandardScalar::Integer), false),
    };
    assert_eq!(
        DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            [oversized_path],
            None,
        ),
        Err(ServerPlanError::CollectionLimit {
            kind: "field path steps",
            count: MAX_FIELD_PATH_STEPS + 1,
            maximum: MAX_FIELD_PATH_STEPS,
        })
    );

    let mut deep = boolean(true);
    for _ in 0..MAX_EXPRESSION_DEPTH {
        deep = equality(deep, boolean(false));
    }
    assert_eq!(
        DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            [deep],
            None,
        ),
        Err(ServerPlanError::RecursionLimitExceeded)
    );

    let encoded = distinct_plan(None).encode().unwrap();
    let mut magic = encoded.clone();
    magic[0] = b'X';
    assert_eq!(
        DistinctServerPlan::decode(&magic),
        Err(ServerPlanError::InvalidMagic)
    );
    let mut input_count = encoded.clone();
    input_count[12..16].copy_from_slice(&2_u32.to_be_bytes());
    assert_eq!(
        DistinctServerPlan::decode(&input_count),
        Err(ServerPlanError::UnexpectedInputCount(2))
    );
    let mut input_slot = encoded.clone();
    input_slot[16..20].copy_from_slice(&1_u32.to_be_bytes());
    assert_eq!(
        DistinctServerPlan::decode(&input_slot),
        Err(ServerPlanError::InvalidInputSlot(1))
    );
    let mut projection_count = encoded.clone();
    projection_count[36..40].copy_from_slice(&(MAX_PROJECTIONS + 1).to_be_bytes());
    assert_eq!(
        DistinctServerPlan::decode(&projection_count),
        Err(ServerPlanError::CollectionLimit {
            kind: "projections",
            count: MAX_PROJECTIONS + 1,
            maximum: MAX_PROJECTIONS,
        })
    );
    assert_eq!(
        DistinctServerPlan::decode(&encoded[..encoded.len() - 1]),
        Err(ServerPlanError::Truncated)
    );
    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        DistinctServerPlan::decode(&trailing),
        Err(ServerPlanError::TrailingBytes)
    );
    assert_eq!(
        DistinctServerPlan::decode(&vec![0; MAX_ARTIFACT_BYTES + 1]),
        Err(ServerPlanError::ArtifactSizeLimit {
            size: MAX_ARTIFACT_BYTES + 1,
            maximum: MAX_ARTIFACT_BYTES,
        })
    );
}

#[test]
fn all_server_plan_versions_decode_only_their_own_model() {
    let version_one = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: TASK,
        },
        projections: vec![boolean(true)],
        selection: None,
        ordering: Vec::new(),
    }
    .encode()
    .unwrap();
    let version_two = identity_selected_plan().encode().unwrap();
    let version_three = distinct_plan(None).encode().unwrap();
    let version_four = unique_text_selected_plan().encode().unwrap();

    assert!(ServerPlan::decode(&version_one).is_ok());
    assert_eq!(
        ServerPlan::decode(&version_two),
        Err(ServerPlanError::UnsupportedVersion(
            IDENTITY_SELECTED_FORMAT_VERSION
        ))
    );
    assert_eq!(
        ServerPlan::decode(&version_three),
        Err(ServerPlanError::UnsupportedVersion(DISTINCT_FORMAT_VERSION))
    );
    assert_eq!(
        ServerPlan::decode(&version_four),
        Err(ServerPlanError::UnsupportedVersion(
            UNIQUE_TEXT_SELECTED_FORMAT_VERSION
        ))
    );

    assert_eq!(
        IdentitySelectedServerPlan::decode(&version_one),
        Err(ServerPlanError::UnsupportedVersion(FORMAT_VERSION))
    );
    assert!(IdentitySelectedServerPlan::decode(&version_two).is_ok());
    assert_eq!(
        IdentitySelectedServerPlan::decode(&version_three),
        Err(ServerPlanError::UnsupportedVersion(DISTINCT_FORMAT_VERSION))
    );
    assert_eq!(
        IdentitySelectedServerPlan::decode(&version_four),
        Err(ServerPlanError::UnsupportedVersion(
            UNIQUE_TEXT_SELECTED_FORMAT_VERSION
        ))
    );

    assert_eq!(
        DistinctServerPlan::decode(&version_one),
        Err(ServerPlanError::UnsupportedVersion(FORMAT_VERSION))
    );
    assert_eq!(
        DistinctServerPlan::decode(&version_two),
        Err(ServerPlanError::UnsupportedVersion(
            IDENTITY_SELECTED_FORMAT_VERSION
        ))
    );
    assert!(DistinctServerPlan::decode(&version_three).is_ok());

    assert_eq!(
        UniqueTextSelectedServerPlan::decode(&version_one),
        Err(ServerPlanError::UnsupportedVersion(FORMAT_VERSION))
    );
    assert_eq!(
        UniqueTextSelectedServerPlan::decode(&version_two),
        Err(ServerPlanError::UnsupportedVersion(
            IDENTITY_SELECTED_FORMAT_VERSION
        ))
    );
    assert_eq!(
        UniqueTextSelectedServerPlan::decode(&version_three),
        Err(ServerPlanError::UnsupportedVersion(DISTINCT_FORMAT_VERSION))
    );
    assert!(UniqueTextSelectedServerPlan::decode(&version_four).is_ok());
}

#[test]
fn identity_selected_plan_has_a_closed_v2_wire_format() {
    let plan = identity_selected_plan();

    assert_eq!(plan.format_version(), IDENTITY_SELECTED_FORMAT_VERSION);
    assert_eq!(plan.scan().object_type, TASK);
    assert_eq!(plan.projections(), [boolean(true)]);
    assert_eq!(plan.selector().owner(), FUNCTION);
    assert_eq!(plan.selector().parameter(), PARAMETER);
    assert_eq!(
            plan.encode().unwrap(),
            b"\x4f\x52\x4e\x41\x53\x50\x00\x00\x00\x00\x00\x02\x00\x00\x00\x01\x00\x00\x00\x00\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x00\x00\x00\x01\x03\x01\x01\x00\x01\x01\x04\x01\x01\x00\x01\x03\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x00\x00\x00\x00\x00\x05\x03\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x01\x00\x06\x06\x06\x06\x06\x06\x06\x06\x06\x06\x06\x06\x06\x06\x06\x06\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x07\x00\x00\x00\x00"
                .to_vec()
        );
}

#[test]
fn identity_selected_plan_round_trips_and_versions_are_closed() {
    let identity_selected_plan = identity_selected_plan();
    let encoded = identity_selected_plan.encode().unwrap();
    let version_one = plan().encode().unwrap();

    assert_eq!(
        IdentitySelectedServerPlan::decode(&encoded),
        Ok(identity_selected_plan)
    );
    assert_eq!(
        IdentitySelectedServerPlan::decode(&encoded)
            .unwrap()
            .encode(),
        Ok(encoded.clone())
    );
    assert_eq!(
        ServerPlan::decode(&encoded),
        Err(ServerPlanError::UnsupportedVersion(
            IDENTITY_SELECTED_FORMAT_VERSION
        ))
    );
    assert_eq!(
        IdentitySelectedServerPlan::decode(&version_one),
        Err(ServerPlanError::UnsupportedVersion(FORMAT_VERSION))
    );
}

#[test]
fn identity_selected_plan_rejects_corruption_and_noncanonical_selection() {
    let encoded = identity_selected_plan().encode().unwrap();

    let mut magic = encoded.clone();
    magic[0] = b'X';
    assert_eq!(
        IdentitySelectedServerPlan::decode(&magic),
        Err(ServerPlanError::InvalidMagic)
    );

    assert_eq!(
        IdentitySelectedServerPlan::decode(&encoded[..encoded.len() - 1]),
        Err(ServerPlanError::Truncated)
    );

    let mut missing_selection = encoded.clone();
    missing_selection[45] = 0;
    assert_eq!(
        IdentitySelectedServerPlan::decode(&missing_selection),
        Err(ServerPlanError::InvalidModel(
            "an identity-selected server plan must contain its fixed selection"
        ))
    );

    let mut wrong_selection = encoded.clone();
    wrong_selection[46] = 3;
    assert_eq!(
        IdentitySelectedServerPlan::decode(&wrong_selection),
        Err(ServerPlanError::InvalidModel(
            "an identity-selected server plan must use its fixed equality selection"
        ))
    );

    let mut ordering = encoded;
    let ordering_offset = ordering.len() - 4;
    ordering[ordering_offset..].copy_from_slice(&1_u32.to_be_bytes());
    assert_eq!(
        IdentitySelectedServerPlan::decode(&ordering),
        Err(ServerPlanError::InvalidModel(
            "an identity-selected server plan must not contain ordering terms"
        ))
    );
}

#[test]
fn identity_selected_plan_enforces_projection_and_expression_budgets() {
    assert_eq!(
        IdentitySelectedServerPlan::new(
            Scan {
                input: 1,
                object_type: TASK,
            },
            [boolean(true)],
            IdentitySelector::new(FUNCTION, PARAMETER),
        ),
        Err(ServerPlanError::InvalidInputSlot(1))
    );

    assert_eq!(
        IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            [],
            IdentitySelector::new(FUNCTION, PARAMETER),
        ),
        Err(ServerPlanError::InvalidModel(
            "a server plan must contain at least one projection"
        ))
    );

    assert_eq!(
        IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            std::iter::repeat_with(|| boolean(true)).take(MAX_PROJECTIONS as usize + 1),
            IdentitySelector::new(FUNCTION, PARAMETER),
        ),
        Err(ServerPlanError::CollectionLimit {
            kind: "projections",
            count: MAX_PROJECTIONS + 1,
            maximum: MAX_PROJECTIONS,
        })
    );

    assert_eq!(
        IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: TASK,
            },
            [full_boolean_tree(13)],
            IdentitySelector::new(FUNCTION, PARAMETER),
        ),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );

    let scan = Scan {
        input: 0,
        object_type: TASK,
    };
    let selector = IdentitySelector::new(FUNCTION, PARAMETER);
    let projections = vec![full_boolean_tree(12)];
    assert_eq!(
        IdentitySelectedServerPlan::new(scan, projections.clone(), selector),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );

    let unchecked = IdentitySelectedServerPlan {
        scan,
        projections: projections.clone(),
        selector,
    };
    assert_eq!(
        unchecked.encode(),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );

    let mut writer =
        encode_plan_prefix(IDENTITY_SELECTED_FORMAT_VERSION, scan, &projections).unwrap();
    writer.boolean(true);
    encode_identity_selection(&mut writer, scan, selector).unwrap();
    encode_count(&mut writer, "ordering", 0, MAX_ORDERING).unwrap();
    assert_eq!(
        IdentitySelectedServerPlan::decode(&writer.finish()),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );

    let mut oversized_projection_count = identity_selected_plan().encode().unwrap();
    oversized_projection_count[36..40].copy_from_slice(&(MAX_PROJECTIONS + 1).to_be_bytes());
    assert_eq!(
        IdentitySelectedServerPlan::decode(&oversized_projection_count),
        Err(ServerPlanError::CollectionLimit {
            kind: "projections",
            count: MAX_PROJECTIONS + 1,
            maximum: MAX_PROJECTIONS,
        })
    );

    assert_eq!(
        IdentitySelectedServerPlan::decode(&vec![0; MAX_ARTIFACT_BYTES + 1]),
        Err(ServerPlanError::ArtifactSizeLimit {
            size: MAX_ARTIFACT_BYTES + 1,
            maximum: MAX_ARTIFACT_BYTES,
        })
    );
}

#[test]
fn round_trips_each_current_expression_kind_and_resolved_type_shape() {
    let plan = plan();
    let encoded = plan.encode().unwrap();

    assert_eq!(ServerPlan::decode(&encoded), Ok(plan));
    assert_eq!(ServerPlan::decode(&encoded).unwrap().encode(), Ok(encoded));
}

#[test]
fn resolved_type_wire_tags_remain_legacy_and_closed() {
    for (index, scalar) in StandardScalar::ALL.into_iter().enumerate() {
        let resolved_type = ResolvedType::scalar(scalar);
        let mut writer = Writer::new();

        assert_eq!(encode_resolved_type(&mut writer, resolved_type), Ok(()));
        let encoded = writer.finish();
        assert_eq!(encoded, vec![1, index as u8 + 1]);

        let mut reader = Reader::new(&encoded);
        assert_eq!(decode_resolved_type(&mut reader), Ok(resolved_type));
        assert_eq!(reader.require_finished(), Ok(()));
    }

    for (resolved_type, type_tag, type_id_bytes) in [
        (ResolvedType::named(PERSON), 2, [2; 16]),
        (ResolvedType::reference(TASK), 3, [1; 16]),
    ] {
        let mut writer = Writer::new();
        assert_eq!(encode_resolved_type(&mut writer, resolved_type), Ok(()));

        let mut expected = vec![type_tag];
        expected.extend_from_slice(&type_id_bytes);
        let encoded = writer.finish();
        assert_eq!(encoded, expected);

        let mut reader = Reader::new(&encoded);
        assert_eq!(decode_resolved_type(&mut reader), Ok(resolved_type));
        assert_eq!(reader.require_finished(), Ok(()));
    }

    for tag in [0, 4] {
        assert_eq!(
            decode_resolved_type(&mut Reader::new(&[tag])),
            Err(ServerPlanError::InvalidEnumTag {
                kind: "resolved type",
                tag,
            })
        );
    }
}

#[test]
fn encodes_a_deterministic_header_and_bytes() {
    let minimal = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: TASK,
        },
        projections: vec![boolean(true)],
        selection: None,
        ordering: Vec::new(),
    };
    let encoded = minimal.encode().unwrap();

    assert_eq!(&encoded[..8], &MAGIC);
    assert_eq!(&encoded[8..12], &FORMAT_VERSION.to_be_bytes());
    assert_eq!(&encoded[12..16], &1_u32.to_be_bytes());
    assert_eq!(
        encoded,
        vec![
            79, 82, 78, 65, 83, 80, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 3, 1, 1, 0, 1, 0, 0, 0, 0, 0,
        ]
    );
}

#[test]
fn displays_version_neutral_server_plan_errors() {
    let cases = [
        (
            ServerPlanError::InvalidMagic,
            "invalid orna.server-plan artifact magic",
        ),
        (
            ServerPlanError::UnsupportedVersion(9),
            "unsupported orna.server-plan artifact version 9",
        ),
        (
            ServerPlanError::UnexpectedInputCount(2),
            "server plan requires one input, found 2",
        ),
        (
            ServerPlanError::InvalidInputSlot(1),
            "server plan requires input slot zero, found 1",
        ),
        (
            ServerPlanError::InvalidEnumTag {
                kind: "kind",
                tag: 7,
            },
            "invalid kind tag 7",
        ),
        (
            ServerPlanError::InvalidBoolean {
                context: "context",
                value: 2,
            },
            "invalid context boolean byte 2",
        ),
        (
            ServerPlanError::CollectionLimit {
                kind: "items",
                count: 3,
                maximum: 2,
            },
            "items count 3 exceeds server-plan limit 2",
        ),
        (
            ServerPlanError::ArtifactSizeLimit {
                size: 3,
                maximum: 2,
            },
            "server plan artifact size 3 exceeds server-plan limit 2",
        ),
        (
            ServerPlanError::EmptyFieldPath,
            "field path must contain at least one step",
        ),
        (
            ServerPlanError::RecursionLimitExceeded,
            "server plan expression nesting exceeds server-plan limit",
        ),
        (
            ServerPlanError::ExpressionNodeLimitExceeded,
            "server plan expression count exceeds server-plan limit",
        ),
        (
            ServerPlanError::UnsupportedDistinctProjectionType {
                resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            },
            "SELECT DISTINCT projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values",
        ),
        (
            ServerPlanError::DistinctOrderingNotAllowed { count: 1 },
            "SELECT DISTINCT queries do not allow ORDER BY; remove the ORDER BY clause",
        ),
        (
            ServerPlanError::Truncated,
            "truncated orna.server-plan artifact",
        ),
        (
            ServerPlanError::TrailingBytes,
            "trailing bytes after orna.server-plan artifact",
        ),
        (
            ServerPlanError::InvalidModel("reason"),
            "invalid server plan model: reason",
        ),
    ];

    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn rejects_corrupt_header_enum_boolean_and_trailing_data() {
    let valid = plan().encode().unwrap();

    let mut magic = valid.clone();
    magic[0] = b'X';
    assert_eq!(
        ServerPlan::decode(&magic),
        Err(ServerPlanError::InvalidMagic)
    );

    let mut version = valid.clone();
    version[11] = 2;
    assert_eq!(
        ServerPlan::decode(&version),
        Err(ServerPlanError::UnsupportedVersion(2))
    );

    let mut input_count = valid.clone();
    input_count[12..16].copy_from_slice(&2_u32.to_be_bytes());
    assert_eq!(
        ServerPlan::decode(&input_count),
        Err(ServerPlanError::UnexpectedInputCount(2))
    );

    let mut expression_tag = valid.clone();
    expression_tag[40] = 99;
    assert_eq!(
        ServerPlan::decode(&expression_tag),
        Err(ServerPlanError::InvalidEnumTag {
            kind: "expression kind",
            tag: 99,
        })
    );

    let mut nullable = valid.clone();
    nullable[58] = 2;
    assert_eq!(
        ServerPlan::decode(&nullable),
        Err(ServerPlanError::InvalidBoolean {
            context: "expression nullability",
            value: 2,
        })
    );

    let mut trailing = valid;
    trailing.push(0);
    assert_eq!(
        ServerPlan::decode(&trailing),
        Err(ServerPlanError::TrailingBytes)
    );
}

#[test]
fn rejects_truncation_oversized_counts_and_deep_nesting() {
    let valid = plan().encode().unwrap();
    assert_eq!(
        ServerPlan::decode(&valid[..valid.len() - 1]),
        Err(ServerPlanError::Truncated)
    );

    let mut projection_count = valid.clone();
    projection_count[36..40].copy_from_slice(&(MAX_PROJECTIONS + 1).to_be_bytes());
    assert_eq!(
        ServerPlan::decode(&projection_count),
        Err(ServerPlanError::CollectionLimit {
            kind: "projections",
            count: MAX_PROJECTIONS + 1,
            maximum: MAX_PROJECTIONS,
        })
    );

    let mut expression = boolean(true);
    for _ in 0..MAX_EXPRESSION_DEPTH {
        expression = equality(expression, boolean(false));
    }
    let deep = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: TASK,
        },
        projections: vec![expression],
        selection: None,
        ordering: Vec::new(),
    };
    assert_eq!(deep.encode(), Err(ServerPlanError::RecursionLimitExceeded));

    let too_many_nodes = ServerPlan {
        scan: plan().scan,
        projections: vec![full_boolean_tree(13)],
        selection: None,
        ordering: Vec::new(),
    };
    assert_eq!(
        too_many_nodes.encode(),
        Err(ServerPlanError::ExpressionNodeLimitExceeded)
    );

    let oversized_artifact = vec![0; MAX_ARTIFACT_BYTES + 1];
    assert_eq!(
        ServerPlan::decode(&oversized_artifact),
        Err(ServerPlanError::ArtifactSizeLimit {
            size: MAX_ARTIFACT_BYTES + 1,
            maximum: MAX_ARTIFACT_BYTES,
        })
    );
}

#[test]
fn rejects_invalid_checked_plan_invariants() {
    let mut invalid_slot = plan();
    invalid_slot.scan.input = 1;
    assert_eq!(
        invalid_slot.encode(),
        Err(ServerPlanError::InvalidInputSlot(1))
    );

    let empty_projections = ServerPlan {
        scan: plan().scan,
        projections: Vec::new(),
        selection: None,
        ordering: Vec::new(),
    };
    assert_eq!(
        empty_projections.encode(),
        Err(ServerPlanError::InvalidModel(
            "a server plan must contain at least one projection"
        ))
    );

    let mut invalid_literal = plan();
    invalid_literal.projections[3].value_type.nullable = true;
    assert_eq!(
        invalid_literal.encode(),
        Err(ServerPlanError::InvalidModel(
            "equality must have BOOLEAN type and SQL nullability"
        ))
    );

    let empty_path = ServerPlan {
        scan: plan().scan,
        projections: vec![Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: Vec::new(),
            },
            value_type: value_type(ResolvedType::scalar(StandardScalar::Integer), false),
        }],
        selection: None,
        ordering: Vec::new(),
    };
    assert_eq!(empty_path.encode(), Err(ServerPlanError::EmptyFieldPath));

    let wrong_path_owner = ServerPlan {
        scan: plan().scan,
        projections: vec![Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![FieldStep {
                    owner: PERSON,
                    field: NAME,
                }],
            },
            value_type: value_type(ResolvedType::scalar(StandardScalar::Integer), false),
        }],
        selection: None,
        ordering: Vec::new(),
    };
    assert_eq!(
        wrong_path_owner.encode(),
        Err(ServerPlanError::InvalidModel(
            "a field path must start at the scanned object type"
        ))
    );
}
