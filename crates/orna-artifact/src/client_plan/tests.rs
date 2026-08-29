use super::*;

const TRUE_BYTES: [u8; ENCODED_LENGTH] = *b"ORNACP\0\0\0\0\0\x01\x01\x01";
const FALSE_BYTES: [u8; ENCODED_LENGTH] = *b"ORNACP\0\0\0\0\0\x01\x01\0";
const OPAQUE_TYPE: TypeId = TypeId::from_bytes([0x42; 16]);
const OPAQUE_PAYLOAD: [u8; OPAQUE_PAYLOAD_LENGTH] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];

#[test]
fn encodes_exact_golden_true_and_false_bytes() {
    assert_eq!(ClientPlan::return_boolean(true).encode(), TRUE_BYTES);
    assert_eq!(ClientPlan::return_boolean(false).encode(), FALSE_BYTES);
}

#[test]
fn round_trips_both_boolean_values() {
    for value in [false, true] {
        let plan = ClientPlan::return_boolean(value);
        let decoded = ClientPlan::decode(&plan.encode()).expect("golden plan must decode");
        assert_eq!(decoded, plan);
        assert_eq!(decoded.format_version(), FORMAT_VERSION);
        assert_eq!(decoded.returned_boolean(), value);
    }
}

#[test]
fn opaque_plan_has_exact_version_two_bytes_and_round_trips() {
    let plan = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD);
    let mut expected = b"ORNACP\0\0\0\0\0\x02\x02".to_vec();
    expected.extend_from_slice(&OPAQUE_TYPE.to_bytes());
    expected.extend_from_slice(&16_u32.to_be_bytes());
    expected.extend_from_slice(&OPAQUE_PAYLOAD);

    assert_eq!(expected.len(), 49);
    assert_eq!(plan.encode().expect("opaque plan encodes"), expected);
    assert_eq!(OpaqueClientPlan::decode(&expected), Ok(plan.clone()));
    assert_eq!(plan.format_version(), OPAQUE_FORMAT_VERSION);
    assert_eq!(plan.opaque_type(), OPAQUE_TYPE);
    assert_eq!(plan.canonical_payload(), &OPAQUE_PAYLOAD);
}

#[test]
fn opaque_plan_round_trips_variable_length_payload() {
    let payload = (0..32).collect::<Vec<u8>>();
    let plan = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, payload.clone());
    let encoded = plan.encode().expect("opaque plan encodes");

    assert_eq!(&encoded[29..33], &(payload.len() as u32).to_be_bytes());
    assert_eq!(OpaqueClientPlan::decode(&encoded), Ok(plan));
}

#[test]
fn opaque_plan_rejects_oversized_total_artifact_on_encode() {
    let payload = vec![0; MAX_ARTIFACT_BYTES - OPAQUE_FIXED_LENGTH + 1];
    let plan = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, payload);

    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::ArtifactSizeLimit {
            size: MAX_ARTIFACT_BYTES + 1,
            maximum: MAX_ARTIFACT_BYTES,
        })
    );
}

#[test]
fn opaque_plan_rejects_oversized_total_artifact_on_decode() {
    let mut bytes = vec![0; MAX_ARTIFACT_BYTES + 1];
    let version_start = MAGIC.len();
    let operation_start = version_start + size_of::<u32>();
    let type_start = operation_start + 1;
    let payload_length_start = type_start + 16;
    bytes[..MAGIC.len()].copy_from_slice(&MAGIC);
    bytes[version_start..operation_start].copy_from_slice(&OPAQUE_FORMAT_VERSION.to_be_bytes());
    bytes[operation_start] = RETURN_OPAQUE_OPERATION;
    bytes[type_start..payload_length_start].copy_from_slice(&OPAQUE_TYPE.to_bytes());
    bytes[payload_length_start..OPAQUE_FIXED_LENGTH]
        .copy_from_slice(&((MAX_ARTIFACT_BYTES - OPAQUE_FIXED_LENGTH + 1) as u32).to_be_bytes());

    assert_eq!(
        OpaqueClientPlan::decode(&bytes),
        Err(ClientPlanError::ArtifactSizeLimit {
            size: MAX_ARTIFACT_BYTES + 1,
            maximum: MAX_ARTIFACT_BYTES,
        })
    );
}

#[test]
fn opaque_plan_preserves_declared_payload_length_error_within_size_bound() {
    let mut bytes = vec![0; OPAQUE_FIXED_LENGTH];
    bytes[..MAGIC.len()].copy_from_slice(&MAGIC);
    bytes[MAGIC.len()..MAGIC.len() + size_of::<u32>()]
        .copy_from_slice(&OPAQUE_FORMAT_VERSION.to_be_bytes());
    bytes[MAGIC.len() + size_of::<u32>()] = RETURN_OPAQUE_OPERATION;
    bytes[OPAQUE_FIXED_LENGTH - size_of::<u32>()..]
        .copy_from_slice(&((MAX_ARTIFACT_BYTES as u32) + 1).to_be_bytes());

    assert_eq!(
        OpaqueClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidOpaquePayloadLength {
            actual: (MAX_ARTIFACT_BYTES as u32) + 1,
        })
    );
}

#[test]
fn client_plan_versions_remain_mutually_closed() {
    let opaque = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD)
        .encode()
        .expect("opaque plan encodes");
    assert_eq!(
        ClientPlan::decode(&opaque),
        Err(ClientPlanError::UnsupportedVersion(OPAQUE_FORMAT_VERSION))
    );
    assert_eq!(
        OpaqueClientPlan::decode(&TRUE_BYTES),
        Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
    );
    for length in 0..OPAQUE_ENCODED_LENGTH {
        assert_eq!(
            OpaqueClientPlan::decode(&opaque[..length]),
            Err(ClientPlanError::Truncated),
            "opaque prefix length {length} must be truncated"
        );
    }
}

#[test]
fn opaque_plan_rejects_operation_length_and_trailing_corruption() {
    let encoded = OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD)
        .encode()
        .expect("opaque plan encodes");

    let mut wrong_operation = encoded.clone();
    wrong_operation[12] = RETURN_BOOLEAN_OPERATION;
    assert_eq!(
        OpaqueClientPlan::decode(&wrong_operation),
        Err(ClientPlanError::InvalidOperation(RETURN_BOOLEAN_OPERATION))
    );
    let mut wrong_length = encoded.clone();
    wrong_length[29..33].copy_from_slice(&((MAX_ARTIFACT_BYTES as u32) + 1).to_be_bytes());
    assert_eq!(
        OpaqueClientPlan::decode(&wrong_length),
        Err(ClientPlanError::InvalidOpaquePayloadLength {
            actual: (MAX_ARTIFACT_BYTES as u32) + 1,
        })
    );
    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        OpaqueClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );
}

#[test]
fn rejects_every_truncated_prefix() {
    for length in 0..ENCODED_LENGTH {
        assert_eq!(
            ClientPlan::decode(&TRUE_BYTES[..length]),
            Err(ClientPlanError::Truncated),
            "prefix length {length} must be truncated"
        );
    }
}

#[test]
fn rejects_invalid_magic_version_operation_boolean_and_trailing_bytes() {
    let mut bytes = TRUE_BYTES;
    bytes[0] = b'X';
    assert_eq!(
        ClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidMagic)
    );

    let mut bytes = TRUE_BYTES;
    bytes[11] = 2;
    assert_eq!(
        ClientPlan::decode(&bytes),
        Err(ClientPlanError::UnsupportedVersion(2))
    );

    let mut bytes = TRUE_BYTES;
    bytes[12] = 2;
    assert_eq!(
        ClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidOperation(2))
    );

    let mut bytes = TRUE_BYTES;
    bytes[13] = 2;
    assert_eq!(
        ClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidBoolean(2))
    );

    let mut bytes = TRUE_BYTES.to_vec();
    bytes.push(0);
    assert_eq!(
        ClientPlan::decode(&bytes),
        Err(ClientPlanError::TrailingBytes)
    );
}

#[test]
fn displays_the_public_error_contract() {
    let duplicate_slot = StateSlotId::from_bytes([0x61; 16]);
    let duplicate_display = format!("duplicate client-plan state slot identity {duplicate_slot}");
    let cases = [
        (
            ClientPlanError::InvalidMagic,
            "invalid orna.client-plan artefact magic",
        ),
        (
            ClientPlanError::UnsupportedVersion(2),
            "unsupported orna.client-plan artefact version 2",
        ),
        (
            ClientPlanError::InvalidOperation(7),
            "invalid client-plan operation tag 7",
        ),
        (
            ClientPlanError::InvalidBoolean(3),
            "invalid client-plan Boolean byte 3",
        ),
        (
            ClientPlanError::InvalidOpaquePayloadLength { actual: 15 },
            "invalid client-plan opaque payload length 15",
        ),
        (
            ClientPlanError::Truncated,
            "truncated orna.client-plan artefact",
        ),
        (
            ClientPlanError::TrailingBytes,
            "trailing bytes after orna.client-plan artefact",
        ),
        (
            ClientPlanError::InvalidExpressionNode(9),
            "invalid client-plan expression node tag 9",
        ),
        (
            ClientPlanError::ExpressionDepthExceeded,
            "client-plan expression tree exceeds the depth cap",
        ),
        (
            ClientPlanError::ExpressionNodeCountExceeded,
            "client-plan expression tree exceeds the node-count cap",
        ),
        (
            ClientPlanError::ExpressionCollectionExceeded { limit: 64 },
            "client-plan expression collection exceeds the limit 64",
        ),
        (
            ClientPlanError::InvalidStateScope(9),
            "invalid client-plan state scope tag 9",
        ),
        (
            ClientPlanError::InvalidStateDefaultTag(7),
            "invalid client-plan state default tag 7",
        ),
        (
            ClientPlanError::InvalidStateSlotCount { actual: 0 },
            "invalid client-plan state slot count 0; a state plan requires at least one slot",
        ),
        (
            ClientPlanError::StateSlotLimitExceeded { limit: 64 },
            "client-plan state slot count exceeds the limit 64",
        ),
        (
            ClientPlanError::DuplicateStateSlotId(duplicate_slot),
            duplicate_display.as_str(),
        ),
        (
            ClientPlanError::InvalidCapabilityCount { actual: 0 },
            "invalid client-plan capability count 0; a capability plan requires at least one requirement",
        ),
        (
            ClientPlanError::CapabilityLimitExceeded { limit: 64 },
            "client-plan capability count exceeds the limit 64",
        ),
        (
            ClientPlanError::DuplicateCapabilityName("std.fs.read".to_owned()),
            "duplicate client-plan capability requirement std.fs.read",
        ),
        (
            ClientPlanError::EmptyCapabilityName,
            "client-plan capability name must not be empty",
        ),
        (
            ClientPlanError::EmptyCapabilityArgument,
            "client-plan capability argument must not be empty",
        ),
        (
            ClientPlanError::CapabilityNameTooLong {
                length: 300,
                limit: 256,
            },
            "client-plan capability name length 300 exceeds the limit 256",
        ),
        (
            ClientPlanError::CapabilityArgumentTooLong {
                length: 2000,
                limit: 1024,
            },
            "client-plan capability argument length 2000 exceeds the limit 1024",
        ),
        (
            ClientPlanError::InvalidCapabilityNameUtf8,
            "client-plan capability name is not valid UTF-8",
        ),
        (
            ClientPlanError::InvalidCapabilityArgumentUtf8,
            "client-plan capability argument is not valid UTF-8",
        ),
        (
            ClientPlanError::InvalidCapabilityArgumentTag(9),
            "invalid client-plan capability argument tag 9",
        ),
        (
            ClientPlanError::UnsupportedInnerVersion(6),
            "unsupported inner client-plan version 6",
        ),
        (
            ClientPlanError::ArtifactSizeLimit {
                size: 100,
                maximum: 50,
            },
            "orna.client-plan artefact size 100 exceeds the limit 50",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
    }
}

fn expression_plan() -> ExpressionClientPlan {
    let function = FunctionId::from_bytes([0x21; 16]);
    let parameter = ParameterId::from_bytes([0x31; 16]);
    let field = FieldId::from_bytes([0x41; 16]);
    ExpressionClientPlan::new(ClientExpressionNode::Call {
        function,
        arguments: vec![(
            parameter,
            ClientExpressionNode::Concat {
                left: Box::new(ClientExpressionNode::String {
                    value: "hello ".to_owned(),
                }),
                right: Box::new(ClientExpressionNode::FieldPath {
                    root: parameter,
                    fields: vec![field],
                }),
            },
        )],
    })
}

#[test]
fn expression_plan_round_trips_every_node_form() {
    let plans = [
        ExpressionClientPlan::new(ClientExpressionNode::Call {
            function: FunctionId::from_bytes([0x21; 16]),
            arguments: vec![(
                ParameterId::from_bytes([0x31; 16]),
                ClientExpressionNode::Boolean { value: true },
            )],
        }),
        ExpressionClientPlan::new(ClientExpressionNode::String {
            value: "a'b\"c".to_owned(),
        }),
        ExpressionClientPlan::new(ClientExpressionNode::Integer { value: -42 }),
        ExpressionClientPlan::new(ClientExpressionNode::Boolean { value: false }),
        ExpressionClientPlan::new(ClientExpressionNode::ParameterRead {
            parameter: ParameterId::from_bytes([0x32; 16]),
        }),
        ExpressionClientPlan::new(ClientExpressionNode::FieldPath {
            root: ParameterId::from_bytes([0x33; 16]),
            fields: vec![FieldId::from_bytes([0x43; 16])],
        }),
        expression_plan(),
        ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
            identity: "std.ui.window@1".to_owned(),
        }),
        ExpressionClientPlan::new(ClientExpressionNode::SourceIntrospection),
    ];
    for plan in plans {
        let bytes = plan.encode().expect("the plan encodes");
        let decoded = ExpressionClientPlan::decode(&bytes).expect("the plan decodes");
        assert_eq!(decoded, plan);
        assert_eq!(decoded.format_version(), EXPRESSION_FORMAT_VERSION);
    }
}

#[test]
fn external_contract_identity_accepts_valid_qualified_names_and_revisions() {
    for identity in [
        "std.ui.window@1",
        "app._internal9@42",
        "über.runtime.value@18446744073709551615",
    ] {
        let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
            identity: identity.to_owned(),
        });
        let encoded = plan.encode().expect("valid identity must encode");
        assert_eq!(ExpressionClientPlan::decode(&encoded), Ok(plan));
    }
}

#[test]
fn external_contract_identity_accepts_quoted_identifier_segments() {
    for identity in [
        "app.\"window\"@1",
        "\"window\"@2",
        "app.\"with\"\"quote\"@3",
        "app.\"display name\"@4",
    ] {
        let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
            identity: identity.to_owned(),
        });
        let encoded = plan.encode().expect("quoted identity must encode");
        assert_eq!(ExpressionClientPlan::decode(&encoded), Ok(plan));
    }
}

#[test]
fn external_contract_identity_rejects_malformed_values_at_encode_boundary() {
    for identity in [
        "",
        "@1",
        "std.ui",
        "std..ui@1",
        ".std.ui@1",
        "std.ui.@1",
        "std.ui@",
        "std.ui@0",
        "std.ui@-1",
        "std.ui@18446744073709551616",
        "std.ui@not-a-number",
        "std@ui@1",
        "std.ui@1@2",
        "std.ui-window@1",
        "1std.ui@1",
        "std.ui window@1",
        "app.\"\"@1",
        "app.\"unterminated@1",
        "app.\"bad\"suffix@1",
        "std.ui\u{7f}window@1",
        "std.ui\0window@1",
    ] {
        let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
            identity: identity.to_owned(),
        });
        assert_eq!(
            plan.encode(),
            Err(ClientPlanError::InvalidExpressionNode(
                NODE_EXTERNAL_CONTRACT
            )),
            "identity {identity:?} must be rejected"
        );
    }
}

#[test]
fn external_contract_identity_rejects_malformed_encoded_payloads() {
    for identity in [
        b"std..ui@1".as_slice(),
        b"std.ui@0".as_slice(),
        b"std.ui@-1".as_slice(),
    ] {
        let bytes = encoded_external_contract_bytes(identity);
        assert_eq!(
            ExpressionClientPlan::decode(&bytes),
            Err(ClientPlanError::InvalidExpressionNode(
                NODE_EXTERNAL_CONTRACT
            )),
            "encoded identity {identity:?} must be rejected"
        );
    }

    let bytes = encoded_external_contract_bytes(&[b's', b't', b'd', b'.', 0xff, b'@', b'1']);
    assert_eq!(
        ExpressionClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidExpressionNode(
            NODE_EXTERNAL_CONTRACT
        ))
    );
}

fn encoded_external_contract_bytes(identity: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&EXPRESSION_FORMAT_VERSION.to_be_bytes());
    bytes.push(RETURN_EXPRESSION_OPERATION);
    bytes.push(NODE_EXTERNAL_CONTRACT);
    bytes.extend_from_slice(&(identity.len() as u32).to_be_bytes());
    bytes.extend_from_slice(identity);
    bytes
}

fn encoded_nested_external_contract_bytes(root: u8) -> Vec<u8> {
    let identity = b"std.ui.window@1";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&EXPRESSION_FORMAT_VERSION.to_be_bytes());
    bytes.push(RETURN_EXPRESSION_OPERATION);
    bytes.push(root);
    match root {
        NODE_CALL => {
            bytes.extend_from_slice(&[0x21; 16]);
            bytes.extend_from_slice(&1_u32.to_be_bytes());
            bytes.extend_from_slice(&[0x31; 16]);
            bytes.push(NODE_EXTERNAL_CONTRACT);
            bytes.extend_from_slice(&(identity.len() as u32).to_be_bytes());
            bytes.extend_from_slice(identity);
        }
        NODE_CONCAT => {
            bytes.push(NODE_EXTERNAL_CONTRACT);
            bytes.extend_from_slice(&(identity.len() as u32).to_be_bytes());
            bytes.extend_from_slice(identity);
            bytes.push(NODE_BOOLEAN);
            bytes.push(1);
        }
        _ => unreachable!("test helper only supports call and concat roots"),
    }
    bytes
}

#[test]
fn nested_external_contracts_are_rejected_at_expression_encode_and_decode_boundaries() {
    let nested_call = ClientExpressionNode::Call {
        function: FunctionId::from_bytes([0x21; 16]),
        arguments: vec![(
            ParameterId::from_bytes([0x31; 16]),
            ClientExpressionNode::ExternalContract {
                identity: "std.ui.window@1".to_owned(),
            },
        )],
    };
    let nested_concat = ClientExpressionNode::Concat {
        left: Box::new(ClientExpressionNode::ExternalContract {
            identity: "std.ui.window@1".to_owned(),
        }),
        right: Box::new(ClientExpressionNode::Boolean { value: true }),
    };

    for (root, node) in [(NODE_CALL, nested_call), (NODE_CONCAT, nested_concat)] {
        assert_eq!(
            ExpressionClientPlan::new(node).encode(),
            Err(ClientPlanError::InvalidExpressionNode(
                NODE_EXTERNAL_CONTRACT
            ))
        );
        assert_eq!(
            ExpressionClientPlan::decode(&encoded_nested_external_contract_bytes(root)),
            Err(ClientPlanError::InvalidExpressionNode(
                NODE_EXTERNAL_CONTRACT
            ))
        );
    }
}

#[test]
fn state_plan_rejects_external_contract_root_at_encode_and_decode_boundaries() {
    let plan = StateClientPlan::new(
        ClientExpressionNode::ExternalContract {
            identity: "std.ui.window@1".to_owned(),
        },
        vec![StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::Local,
            StateDefault::Unset,
        )],
    );
    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::InvalidExpressionNode(
            NODE_EXTERNAL_CONTRACT
        ))
    );

    let identity = b"std.ui.window@1";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&STATE_FORMAT_VERSION.to_be_bytes());
    bytes.push(RETURN_STATE_OPERATION);
    bytes.push(NODE_EXTERNAL_CONTRACT);
    bytes.extend_from_slice(&(identity.len() as u32).to_be_bytes());
    bytes.extend_from_slice(identity);
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.extend_from_slice(&[0x11; 16]);
    bytes.extend_from_slice(&[0x12; 16]);
    bytes.push(STATE_SCOPE_LOCAL);
    bytes.push(STATE_DEFAULT_UNSET);
    assert_eq!(
        StateClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidExpressionNode(
            NODE_EXTERNAL_CONTRACT
        ))
    );
}

#[test]
fn inspector_plan_rejects_external_contract_root() {
    let identity = b"std.ui.window@1";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&INSPECT_FORMAT_VERSION.to_be_bytes());
    bytes.push(RETURN_INSPECT_OPERATION);
    bytes.push(NODE_EXTERNAL_CONTRACT);
    bytes.extend_from_slice(&(identity.len() as u32).to_be_bytes());
    bytes.extend_from_slice(identity);
    assert_eq!(
        ExpressionClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidExpressionNode(
            NODE_EXTERNAL_CONTRACT
        ))
    );
}

#[test]
fn external_contract_placement_reuses_expression_depth_limit() {
    let mut expression = ClientExpressionNode::Boolean { value: true };
    for _ in 0..=MAX_EXPRESSION_DEPTH {
        expression = ClientExpressionNode::Concat {
            left: Box::new(expression),
            right: Box::new(ClientExpressionNode::Boolean { value: true }),
        };
    }
    let plan = StateClientPlan::new(
        expression,
        vec![StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::Local,
            StateDefault::Unset,
        )],
    );

    assert_eq!(plan.encode(), Err(ClientPlanError::ExpressionDepthExceeded));
}

#[test]
fn expression_plan_has_the_exact_version_three_header() {
    let plan = ExpressionClientPlan::new(ClientExpressionNode::Boolean { value: true });
    let bytes = plan.encode().expect("the plan encodes");
    assert_eq!(&bytes[..8], &MAGIC);
    assert_eq!(&bytes[8..12], &EXPRESSION_FORMAT_VERSION.to_be_bytes());
    assert_eq!(bytes[12], RETURN_EXPRESSION_OPERATION);
    assert_eq!(bytes[13], NODE_BOOLEAN);
    assert_eq!(bytes[14], 1);
}

#[test]
fn expression_plan_versions_remain_mutually_closed() {
    let expression = expression_plan().encode().expect("the plan encodes");
    assert_eq!(
        ClientPlan::decode(&expression),
        Err(ClientPlanError::UnsupportedVersion(
            EXPRESSION_FORMAT_VERSION
        ))
    );
    assert_eq!(
        OpaqueClientPlan::decode(&expression),
        Err(ClientPlanError::UnsupportedVersion(
            EXPRESSION_FORMAT_VERSION
        ))
    );
    assert_eq!(
        ExpressionClientPlan::decode(&TRUE_BYTES),
        Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
    );
}

#[test]
fn expression_plan_rejects_magic_version_operation_and_trailing_corruption() {
    let encoded = expression_plan().encode().expect("the plan encodes");

    let mut wrong_magic = encoded.clone();
    wrong_magic[0] = b'X';
    assert_eq!(
        ExpressionClientPlan::decode(&wrong_magic),
        Err(ClientPlanError::InvalidMagic)
    );

    let mut wrong_version = encoded.clone();
    wrong_version[8..12].copy_from_slice(&2_u32.to_be_bytes());
    assert_eq!(
        ExpressionClientPlan::decode(&wrong_version),
        Err(ClientPlanError::UnsupportedVersion(2))
    );

    let mut wrong_operation = encoded.clone();
    wrong_operation[12] = RETURN_BOOLEAN_OPERATION;
    assert_eq!(
        ExpressionClientPlan::decode(&wrong_operation),
        Err(ClientPlanError::InvalidOperation(RETURN_BOOLEAN_OPERATION))
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        ExpressionClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );
}

#[test]
fn expression_plan_rejects_unknown_tags_and_exceeded_limits() {
    let plan = ExpressionClientPlan::new(ClientExpressionNode::Boolean { value: true });
    let mut encoded = plan.encode().expect("the plan encodes");
    encoded[13] = 9;
    assert_eq!(
        ExpressionClientPlan::decode(&encoded),
        Err(ClientPlanError::InvalidExpressionNode(9))
    );

    let mut boolean_byte = plan.encode().expect("the plan encodes");
    boolean_byte[14] = 2;
    assert_eq!(
        ExpressionClientPlan::decode(&boolean_byte),
        Err(ClientPlanError::InvalidExpressionNode(NODE_BOOLEAN))
    );

    let mut empty_field_path = ExpressionClientPlan::new(ClientExpressionNode::FieldPath {
        root: ParameterId::from_bytes([0x32; 16]),
        fields: vec![FieldId::from_bytes([0x42; 16])],
    })
    .encode()
    .expect("the field path encodes");
    empty_field_path[30..34].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        ExpressionClientPlan::decode(&empty_field_path),
        Err(ClientPlanError::InvalidExpressionNode(NODE_FIELD_PATH))
    );

    let deep = ExpressionClientPlan::new(deep_concat(MAX_EXPRESSION_DEPTH + 1));
    assert_eq!(deep.encode(), Err(ClientPlanError::ExpressionDepthExceeded));

    let call = ExpressionClientPlan::new(ClientExpressionNode::Call {
        function: FunctionId::from_bytes([0x21; 16]),
        arguments: (0..=MAX_CALL_ARGUMENTS)
            .map(|index| {
                (
                    ParameterId::from_bytes([index as u8; 16]),
                    ClientExpressionNode::Boolean { value: true },
                )
            })
            .collect(),
    });
    assert_eq!(
        call.encode(),
        Err(ClientPlanError::ExpressionCollectionExceeded {
            limit: MAX_CALL_ARGUMENTS,
        })
    );

    let wide = ExpressionClientPlan::new(ClientExpressionNode::Call {
        function: FunctionId::from_bytes([0x51; 16]),
        arguments: (0..MAX_CALL_ARGUMENTS)
            .map(|outer| {
                (
                    ParameterId::from_bytes([outer as u8; 16]),
                    ClientExpressionNode::Call {
                        function: FunctionId::from_bytes([0x52; 16]),
                        arguments: (0..MAX_CALL_ARGUMENTS)
                            .map(|inner| {
                                (
                                    ParameterId::from_bytes([inner as u8; 16]),
                                    ClientExpressionNode::Boolean { value: true },
                                )
                            })
                            .collect(),
                    },
                )
            })
            .collect(),
    });
    assert_eq!(
        wide.encode(),
        Err(ClientPlanError::ExpressionNodeCountExceeded)
    );
}

fn deep_concat(depth: usize) -> ClientExpressionNode {
    if depth == 0 {
        ClientExpressionNode::Boolean { value: true }
    } else {
        ClientExpressionNode::Concat {
            left: Box::new(deep_concat(depth - 1)),
            right: Box::new(ClientExpressionNode::Boolean { value: false }),
        }
    }
}

#[test]
fn expression_plan_rejects_every_truncated_prefix() {
    let encoded = expression_plan().encode().expect("the plan encodes");
    for length in 0..encoded.len() {
        assert_eq!(
            ExpressionClientPlan::decode(&encoded[..length]),
            Err(ClientPlanError::Truncated),
            "prefix length {length} must be truncated"
        );
    }
}

fn state_plan() -> StateClientPlan {
    let function = FunctionId::from_bytes([0x21; 16]);
    let parameter = ParameterId::from_bytes([0x31; 16]);
    let field = FieldId::from_bytes([0x41; 16]);
    StateClientPlan::new(
        ClientExpressionNode::Call {
            function,
            arguments: vec![(
                parameter,
                ClientExpressionNode::FieldPath {
                    root: parameter,
                    fields: vec![field],
                },
            )],
        },
        vec![
            StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x12; 16]),
                StateScope::Local,
                StateDefault::Unset,
            ),
            StateSlot::new(
                StateSlotId::from_bytes([0x21; 16]),
                TypeId::from_bytes([0x22; 16]),
                StateScope::Session,
                StateDefault::Null,
            ),
            StateSlot::new(
                StateSlotId::from_bytes([0x31; 16]),
                TypeId::from_bytes([0x32; 16]),
                StateScope::User,
                StateDefault::Expression(ClientExpressionNode::Concat {
                    left: Box::new(ClientExpressionNode::String {
                        value: "prefix".to_owned(),
                    }),
                    right: Box::new(ClientExpressionNode::ParameterRead { parameter }),
                }),
            ),
        ],
    )
}

fn expression_default_plan() -> StateClientPlan {
    let function = FunctionId::from_bytes([0x21; 16]);
    let parameter = ParameterId::from_bytes([0x31; 16]);
    let field = FieldId::from_bytes([0x41; 16]);
    let forms = [
        ClientExpressionNode::Call {
            function,
            arguments: vec![(parameter, ClientExpressionNode::Boolean { value: true })],
        },
        ClientExpressionNode::String {
            value: "a'b\"c".to_owned(),
        },
        ClientExpressionNode::Integer { value: -42 },
        ClientExpressionNode::Boolean { value: false },
        ClientExpressionNode::ParameterRead { parameter },
        ClientExpressionNode::FieldPath {
            root: parameter,
            fields: vec![field],
        },
        ClientExpressionNode::Concat {
            left: Box::new(ClientExpressionNode::String {
                value: "x".to_owned(),
            }),
            right: Box::new(ClientExpressionNode::Integer { value: 7 }),
        },
    ];
    let slots = forms
        .iter()
        .enumerate()
        .map(|(index, node)| {
            StateSlot::new(
                StateSlotId::from_bytes([index as u8; 16]),
                TypeId::from_bytes([0x40 + index as u8; 16]),
                match index % 3 {
                    0 => StateScope::Local,
                    1 => StateScope::Session,
                    _ => StateScope::User,
                },
                StateDefault::Expression(node.clone()),
            )
        })
        .collect();
    StateClientPlan::new(
        ClientExpressionNode::String {
            value: "ready".to_owned(),
        },
        slots,
    )
}

fn minimal_state_plan() -> StateClientPlan {
    StateClientPlan::new(
        ClientExpressionNode::Boolean { value: true },
        vec![StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::Local,
            StateDefault::Unset,
        )],
    )
}

#[test]
fn inspect_plan_round_trips_all_projections_and_uses_version_nine() {
    let projections = [
        InspectProjection::InvocationNodes,
        InspectProjection::Calls,
        InspectProjection::Resources,
        InspectProjection::StateCells,
        InspectProjection::UiNodes,
        InspectProjection::PresentationCandidates,
        InspectProjection::RuntimeBindings,
        InspectProjection::SecurityDecisions,
    ];
    for projection in projections {
        let plan = ExpressionClientPlan::new(ClientExpressionNode::Inspect {
            operation: InspectOperationNode::Projection {
                projection,
                snapshot: Box::new(ClientExpressionNode::Inspect {
                    operation: InspectOperationNode::Snapshot {
                        target: Box::new(ClientExpressionNode::ParameterRead {
                            parameter: ParameterId::from_bytes([0x31; 16]),
                        }),
                        options: None,
                    },
                }),
            },
        });
        assert_eq!(plan.format_version(), INSPECT_FORMAT_VERSION);
        let bytes = plan.encode().expect("inspect plan encodes");
        assert_eq!(&bytes[..8], &MAGIC);
        assert_eq!(&bytes[8..12], &INSPECT_FORMAT_VERSION.to_be_bytes());
        assert_eq!(bytes[12], RETURN_INSPECT_OPERATION);
        assert_eq!(bytes[13], NODE_INSPECT);
        assert_eq!(bytes[14], INSPECT_OPERATION_PROJECTION);
        assert_eq!(ExpressionClientPlan::decode(&bytes), Ok(plan));
    }
}

#[test]
fn inspect_plan_round_trips_structural_default_options() {
    let plan = ExpressionClientPlan::new(ClientExpressionNode::Inspect {
        operation: InspectOperationNode::snapshot(ClientExpressionNode::ParameterRead {
            parameter: ParameterId::from_bytes([0x32; 16]),
        }),
    });
    assert_eq!(
        ExpressionClientPlan::decode(&plan.encode().unwrap()),
        Ok(plan)
    );
}

#[test]
fn inspect_plan_rejects_unknown_projection_operation_trailing_and_old_version() {
    let plan = ExpressionClientPlan::new(ClientExpressionNode::Inspect {
        operation: InspectOperationNode::Projection {
            projection: InspectProjection::Calls,
            snapshot: Box::new(ClientExpressionNode::Boolean { value: true }),
        },
    });
    let encoded = plan.encode().expect("inspect plan encodes");

    let mut unknown_projection = encoded.clone();
    unknown_projection[15] = 0;
    assert_eq!(
        ExpressionClientPlan::decode(&unknown_projection),
        Err(ClientPlanError::InvalidInspectProjection(0))
    );
    unknown_projection[15] = 9;
    assert_eq!(
        ExpressionClientPlan::decode(&unknown_projection),
        Err(ClientPlanError::InvalidInspectProjection(9))
    );

    let mut unknown_operation = encoded.clone();
    unknown_operation[14] = 3;
    assert_eq!(
        ExpressionClientPlan::decode(&unknown_operation),
        Err(ClientPlanError::InvalidInspectOperation(3))
    );

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        ExpressionClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );
    assert_eq!(ExpressionClientPlan::decode(&encoded), Ok(plan));
    assert_eq!(
        ClientPlan::decode(&encoded),
        Err(ClientPlanError::UnsupportedVersion(INSPECT_FORMAT_VERSION))
    );

    let mut old_version = encoded;
    old_version[8..12].copy_from_slice(&EXPRESSION_FORMAT_VERSION.to_be_bytes());
    old_version[12] = RETURN_EXPRESSION_OPERATION;
    assert_eq!(
        ExpressionClientPlan::decode(&old_version),
        Err(ClientPlanError::InvalidExpressionNode(NODE_INSPECT))
    );
}

#[test]
fn inspect_version_nine_requires_an_inspect_node_and_rejects_truncation() {
    let ordinary = ExpressionClientPlan::new(ClientExpressionNode::Boolean { value: true });
    let mut noncanonical = ordinary.encode().expect("ordinary plan encodes");
    noncanonical[8..12].copy_from_slice(&INSPECT_FORMAT_VERSION.to_be_bytes());
    noncanonical[12] = RETURN_INSPECT_OPERATION;
    assert_eq!(
        ExpressionClientPlan::decode(&noncanonical),
        Err(ClientPlanError::InvalidInspectPlan)
    );

    let inspect = ExpressionClientPlan::new(ClientExpressionNode::Inspect {
        operation: InspectOperationNode::Snapshot {
            target: Box::new(ClientExpressionNode::Boolean { value: false }),
            options: None,
        },
    });
    let encoded = inspect.encode().expect("inspect plan encodes");
    for length in 0..encoded.len() {
        assert_eq!(
            ExpressionClientPlan::decode(&encoded[..length]),
            Err(ClientPlanError::Truncated),
            "prefix length {length} must be truncated"
        );
    }
}

#[test]
fn inspect_nodes_use_recursive_depth_limits() {
    let mut node = ClientExpressionNode::Boolean { value: true };
    for _ in 0..=MAX_EXPRESSION_DEPTH {
        node = ClientExpressionNode::Inspect {
            operation: InspectOperationNode::Snapshot {
                target: Box::new(node),
                options: None,
            },
        };
    }
    assert_eq!(
        ExpressionClientPlan::new(node).encode(),
        Err(ClientPlanError::ExpressionDepthExceeded)
    );
}

#[test]
fn state_plan_round_trips_every_scope_and_default_form() {
    let plans = [
        state_plan(),
        expression_default_plan(),
        minimal_state_plan(),
        StateClientPlan::new(
            ClientExpressionNode::Boolean { value: false },
            vec![StateSlot::new(
                StateSlotId::from_bytes([0x51; 16]),
                TypeId::from_bytes([0x52; 16]),
                StateScope::Session,
                StateDefault::Null,
            )],
        ),
    ];
    for plan in plans {
        let bytes = plan.encode().expect("the plan encodes");
        let decoded = StateClientPlan::decode(&bytes).expect("the plan decodes");
        assert_eq!(decoded, plan);
        assert_eq!(decoded.format_version(), STATE_FORMAT_VERSION);
        assert_eq!(decoded.slots().len(), plan.slots().len());
        assert_eq!(decoded.expression(), plan.expression());
    }
}

#[test]
fn state_plan_exposes_its_slot_accessors() {
    let plan = state_plan();
    let bytes = plan.encode().expect("the plan encodes");
    let decoded = StateClientPlan::decode(&bytes).expect("the plan decodes");
    let slots = decoded.slots();
    assert_eq!(slots.len(), 3);
    assert_eq!(
        slots[0].state_slot_id(),
        StateSlotId::from_bytes([0x11; 16])
    );
    assert_eq!(slots[0].type_id(), TypeId::from_bytes([0x12; 16]));
    assert_eq!(slots[0].scope(), StateScope::Local);
    assert_eq!(slots[0].default(), &StateDefault::Unset);
    assert_eq!(slots[1].scope(), StateScope::Session);
    assert_eq!(slots[1].default(), &StateDefault::Null);
    assert_eq!(slots[2].scope(), StateScope::User);
    assert!(matches!(
        slots[2].default(),
        StateDefault::Expression(ClientExpressionNode::Concat { .. })
    ));
}

#[test]
fn state_plan_has_the_exact_version_four_layout() {
    let plan = minimal_state_plan();
    let bytes = plan.encode().expect("the plan encodes");
    let mut expected = Vec::new();
    expected.extend_from_slice(&MAGIC);
    expected.extend_from_slice(&STATE_FORMAT_VERSION.to_be_bytes());
    expected.push(RETURN_STATE_OPERATION);
    expected.push(NODE_BOOLEAN);
    expected.push(1);
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.extend_from_slice(&[0x11; 16]);
    expected.extend_from_slice(&[0x12; 16]);
    expected.push(STATE_SCOPE_LOCAL);
    expected.push(STATE_DEFAULT_UNSET);
    assert_eq!(bytes, expected);
    assert_eq!(plan.format_version(), STATE_FORMAT_VERSION);
}

#[test]
fn state_plan_has_the_exact_expression_default_layout() {
    let plan = StateClientPlan::new(
        ClientExpressionNode::Boolean { value: false },
        vec![StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::User,
            StateDefault::Expression(ClientExpressionNode::String {
                value: "hi".to_owned(),
            }),
        )],
    );
    let bytes = plan.encode().expect("the plan encodes");
    let mut expected = Vec::new();
    expected.extend_from_slice(&MAGIC);
    expected.extend_from_slice(&STATE_FORMAT_VERSION.to_be_bytes());
    expected.push(RETURN_STATE_OPERATION);
    expected.push(NODE_BOOLEAN);
    expected.push(0);
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.extend_from_slice(&[0x11; 16]);
    expected.extend_from_slice(&[0x12; 16]);
    expected.push(STATE_SCOPE_USER);
    expected.push(STATE_DEFAULT_EXPRESSION);
    expected.push(NODE_STRING);
    expected.extend_from_slice(&2_u32.to_be_bytes());
    expected.extend_from_slice(b"hi");
    assert_eq!(bytes, expected);
}

#[test]
fn state_plan_versions_remain_mutually_closed() {
    let state = state_plan().encode().expect("the plan encodes");
    assert_eq!(
        ClientPlan::decode(&state),
        Err(ClientPlanError::UnsupportedVersion(STATE_FORMAT_VERSION))
    );
    assert_eq!(
        OpaqueClientPlan::decode(&state),
        Err(ClientPlanError::UnsupportedVersion(STATE_FORMAT_VERSION))
    );
    assert_eq!(
        ExpressionClientPlan::decode(&state),
        Err(ClientPlanError::UnsupportedVersion(STATE_FORMAT_VERSION))
    );
    assert_eq!(
        StateClientPlan::decode(&TRUE_BYTES),
        Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
    );
    let expression = expression_plan().encode().expect("the plan encodes");
    assert_eq!(
        StateClientPlan::decode(&expression),
        Err(ClientPlanError::UnsupportedVersion(
            EXPRESSION_FORMAT_VERSION
        ))
    );
}

#[test]
fn state_plan_rejects_magic_version_operation_and_trailing_corruption() {
    let encoded = state_plan().encode().expect("the plan encodes");

    let mut wrong_magic = encoded.clone();
    wrong_magic[0] = b'X';
    assert_eq!(
        StateClientPlan::decode(&wrong_magic),
        Err(ClientPlanError::InvalidMagic)
    );

    let mut wrong_version = encoded.clone();
    wrong_version[8..12].copy_from_slice(&2_u32.to_be_bytes());
    assert_eq!(
        StateClientPlan::decode(&wrong_version),
        Err(ClientPlanError::UnsupportedVersion(2))
    );

    let mut wrong_operation = encoded.clone();
    wrong_operation[12] = RETURN_EXPRESSION_OPERATION;
    assert_eq!(
        StateClientPlan::decode(&wrong_operation),
        Err(ClientPlanError::InvalidOperation(
            RETURN_EXPRESSION_OPERATION
        ))
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        StateClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );
}

#[test]
fn state_plan_rejects_unknown_scope_and_default_tags() {
    let plan = minimal_state_plan();
    let mut bytes = plan.encode().expect("the plan encodes");
    assert_eq!(bytes.len(), 53);
    bytes[51] = 4;
    assert_eq!(
        StateClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidStateScope(4))
    );

    let mut bytes = plan.encode().expect("the plan encodes");
    bytes[52] = 3;
    assert_eq!(
        StateClientPlan::decode(&bytes),
        Err(ClientPlanError::InvalidStateDefaultTag(3))
    );
}

#[test]
fn state_plan_rejects_zero_and_oversized_slot_counts() {
    let plan = minimal_state_plan();
    let mut zero_slots = plan.encode().expect("the plan encodes");
    zero_slots[15..19].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        StateClientPlan::decode(&zero_slots),
        Err(ClientPlanError::InvalidStateSlotCount { actual: 0 })
    );

    let mut oversized = plan.encode().expect("the plan encodes");
    oversized[15..19].copy_from_slice(&(MAX_STATE_SLOTS as u32 + 1).to_be_bytes());
    assert_eq!(
        StateClientPlan::decode(&oversized),
        Err(ClientPlanError::StateSlotLimitExceeded {
            limit: MAX_STATE_SLOTS,
        })
    );
}

#[test]
fn state_plan_rejects_duplicate_state_slot_identities() {
    let duplicated = StateClientPlan::new(
        ClientExpressionNode::Boolean { value: true },
        vec![
            StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x12; 16]),
                StateScope::Local,
                StateDefault::Unset,
            ),
            StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                TypeId::from_bytes([0x13; 16]),
                StateScope::Session,
                StateDefault::Null,
            ),
        ],
    );
    assert_eq!(
        duplicated.encode(),
        Err(ClientPlanError::DuplicateStateSlotId(
            StateSlotId::from_bytes([0x11; 16])
        ))
    );

    let mut crafted = minimal_state_plan().encode().expect("the plan encodes");
    crafted[15..19].copy_from_slice(&2_u32.to_be_bytes());
    crafted.extend_from_slice(&[0x11; 16]);
    crafted.extend_from_slice(&[0x13; 16]);
    crafted.push(STATE_SCOPE_SESSION);
    crafted.push(STATE_DEFAULT_NULL);
    assert_eq!(
        StateClientPlan::decode(&crafted),
        Err(ClientPlanError::DuplicateStateSlotId(
            StateSlotId::from_bytes([0x11; 16])
        ))
    );
}

#[test]
fn state_plan_encode_rejects_empty_and_oversized_slot_lists() {
    let empty = StateClientPlan::new(ClientExpressionNode::Boolean { value: true }, Vec::new());
    assert_eq!(
        empty.encode(),
        Err(ClientPlanError::InvalidStateSlotCount { actual: 0 })
    );

    let slot = StateSlot::new(
        StateSlotId::from_bytes([0x11; 16]),
        TypeId::from_bytes([0x12; 16]),
        StateScope::Local,
        StateDefault::Unset,
    );
    let oversized = StateClientPlan::new(
        ClientExpressionNode::Boolean { value: true },
        vec![slot; MAX_STATE_SLOTS + 1],
    );
    assert_eq!(
        oversized.encode(),
        Err(ClientPlanError::StateSlotLimitExceeded {
            limit: MAX_STATE_SLOTS,
        })
    );
}

#[test]
fn state_plan_rejects_malformed_return_and_default_trees() {
    let mut unknown_return = minimal_state_plan().encode().expect("the plan encodes");
    unknown_return[13] = 9;
    assert_eq!(
        StateClientPlan::decode(&unknown_return),
        Err(ClientPlanError::InvalidExpressionNode(9))
    );

    let expression_default = StateClientPlan::new(
        ClientExpressionNode::Boolean { value: true },
        vec![StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::Session,
            StateDefault::Expression(ClientExpressionNode::String {
                value: "ab".to_owned(),
            }),
        )],
    );
    let mut unknown_default = expression_default.encode().expect("the plan encodes");
    unknown_default[53] = 9;
    assert_eq!(
        StateClientPlan::decode(&unknown_default),
        Err(ClientPlanError::InvalidExpressionNode(9))
    );

    let mut truncated_default = expression_default.encode().expect("the plan encodes");
    truncated_default[54..58].copy_from_slice(&3_u32.to_be_bytes());
    assert_eq!(
        StateClientPlan::decode(&truncated_default),
        Err(ClientPlanError::Truncated)
    );

    let mut invalid_utf8 = expression_default.encode().expect("the plan encodes");
    invalid_utf8[54..58].copy_from_slice(&1_u32.to_be_bytes());
    invalid_utf8[58] = 0xff;
    assert_eq!(
        StateClientPlan::decode(&invalid_utf8),
        Err(ClientPlanError::InvalidExpressionNode(NODE_STRING))
    );
}

#[test]
fn state_plan_rejects_depth_and_collection_violations() {
    let deep = StateClientPlan::new(
        deep_concat(MAX_EXPRESSION_DEPTH + 1),
        vec![StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::Local,
            StateDefault::Unset,
        )],
    );
    assert_eq!(deep.encode(), Err(ClientPlanError::ExpressionDepthExceeded));

    let wide_default = StateClientPlan::new(
        ClientExpressionNode::Boolean { value: true },
        vec![StateSlot::new(
            StateSlotId::from_bytes([0x11; 16]),
            TypeId::from_bytes([0x12; 16]),
            StateScope::User,
            StateDefault::Expression(ClientExpressionNode::Call {
                function: FunctionId::from_bytes([0x51; 16]),
                arguments: (0..=MAX_CALL_ARGUMENTS)
                    .map(|index| {
                        (
                            ParameterId::from_bytes([index as u8; 16]),
                            ClientExpressionNode::Boolean { value: true },
                        )
                    })
                    .collect(),
            }),
        )],
    );
    assert_eq!(
        wide_default.encode(),
        Err(ClientPlanError::ExpressionCollectionExceeded {
            limit: MAX_CALL_ARGUMENTS,
        })
    );
}

#[test]
fn state_plan_rejects_every_truncated_prefix() {
    let encoded = minimal_state_plan().encode().expect("the plan encodes");
    for length in 0..encoded.len() {
        assert_eq!(
            StateClientPlan::decode(&encoded[..length]),
            Err(ClientPlanError::Truncated),
            "prefix length {length} must be truncated"
        );
    }
}

fn capability_plan() -> CapabilityClientPlan {
    let function = FunctionId::from_bytes([0x21; 16]);
    let parameter = ParameterId::from_bytes([0x31; 16]);
    let field = FieldId::from_bytes([0x41; 16]);
    CapabilityClientPlan::new(
        InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::Call {
            function,
            arguments: vec![(
                parameter,
                ClientExpressionNode::FieldPath {
                    root: parameter,
                    fields: vec![field],
                },
            )],
        })),
        vec![
            CapabilityRequirement::new(
                "std.fs.read",
                CapabilityArgumentSource::Text("/home/bob".to_owned()),
            ),
            CapabilityRequirement::new(
                "std.net.connect",
                CapabilityArgumentSource::Parameter("p_host".to_owned()),
            ),
        ],
    )
}

fn minimal_capability_plan() -> CapabilityClientPlan {
    CapabilityClientPlan::new(
        InnerClientPlan::Boolean(ClientPlan::return_boolean(true)),
        vec![CapabilityRequirement::new(
            "std.secret.use",
            CapabilityArgumentSource::Parameter("p_secret".to_owned()),
        )],
    )
}

#[test]
fn capability_plan_round_trips_every_inner_form_and_argument_source() {
    let inner_forms = [
        InnerClientPlan::Boolean(ClientPlan::return_boolean(false)),
        InnerClientPlan::Opaque(OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD)),
        InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::String {
            value: "hi".to_owned(),
        })),
        InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::Inspect {
            operation: InspectOperationNode::Snapshot {
                target: Box::new(ClientExpressionNode::Boolean { value: true }),
                options: None,
            },
        })),
        InnerClientPlan::State(minimal_state_plan()),
    ];
    for inner in inner_forms {
        let plan = CapabilityClientPlan::new(
            inner.clone(),
            vec![
                CapabilityRequirement::new(
                    "std.fs.read",
                    CapabilityArgumentSource::Text("/home/bob".to_owned()),
                ),
                CapabilityRequirement::new(
                    "std.fs.write",
                    CapabilityArgumentSource::Parameter("p_path".to_owned()),
                ),
            ],
        );
        let bytes = plan.encode().expect("the plan encodes");
        let decoded = CapabilityClientPlan::decode(&bytes).expect("the plan decodes");
        assert_eq!(decoded, plan);
        assert_eq!(decoded.format_version(), CAPABILITY_FORMAT_VERSION);
        assert_eq!(decoded.inner_plan_version(), inner.format_version());
        assert_eq!(decoded.inner_plan(), &inner);
        assert_eq!(decoded.requirements(), plan.requirements());
    }
}

#[test]
fn capability_plan_has_the_exact_version_five_layout() {
    let plan = minimal_capability_plan();
    let bytes = plan.encode().expect("the plan encodes");
    let mut expected = Vec::new();
    expected.extend_from_slice(&MAGIC);
    expected.extend_from_slice(&CAPABILITY_FORMAT_VERSION.to_be_bytes());
    expected.push(RETURN_CAPABILITY_OPERATION);
    expected.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    expected.extend_from_slice(&(TRUE_BYTES.len() as u32).to_be_bytes());
    expected.extend_from_slice(&TRUE_BYTES);
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.extend_from_slice(&(b"std.secret.use".len() as u32).to_be_bytes());
    expected.extend_from_slice(b"std.secret.use");
    expected.push(CAPABILITY_ARGUMENT_PARAMETER);
    expected.extend_from_slice(&(b"p_secret".len() as u32).to_be_bytes());
    expected.extend_from_slice(b"p_secret");
    assert_eq!(bytes, expected);
    assert_eq!(plan.format_version(), CAPABILITY_FORMAT_VERSION);
    assert_eq!(plan.inner_plan_version(), FORMAT_VERSION);
    assert_eq!(
        plan.inner_plan(),
        &InnerClientPlan::Boolean(ClientPlan::return_boolean(true))
    );
}

#[test]
fn capability_plan_versions_remain_mutually_closed() {
    let capability = capability_plan().encode().expect("the plan encodes");
    assert_eq!(
        ClientPlan::decode(&capability),
        Err(ClientPlanError::UnsupportedVersion(
            CAPABILITY_FORMAT_VERSION
        ))
    );
    assert_eq!(
        OpaqueClientPlan::decode(&capability),
        Err(ClientPlanError::UnsupportedVersion(
            CAPABILITY_FORMAT_VERSION
        ))
    );
    assert_eq!(
        ExpressionClientPlan::decode(&capability),
        Err(ClientPlanError::UnsupportedVersion(
            CAPABILITY_FORMAT_VERSION
        ))
    );
    assert_eq!(
        StateClientPlan::decode(&capability),
        Err(ClientPlanError::UnsupportedVersion(
            CAPABILITY_FORMAT_VERSION
        ))
    );
    let inner_artefacts = [
        (FORMAT_VERSION, ClientPlan::return_boolean(true).encode()),
        (
            OPAQUE_FORMAT_VERSION,
            OpaqueClientPlan::return_opaque(OPAQUE_TYPE, OPAQUE_PAYLOAD)
                .encode()
                .expect("opaque plan encodes"),
        ),
        (
            EXPRESSION_FORMAT_VERSION,
            expression_plan().encode().expect("the plan encodes"),
        ),
        (
            STATE_FORMAT_VERSION,
            minimal_state_plan().encode().expect("the plan encodes"),
        ),
    ];
    for (version, bytes) in inner_artefacts {
        assert_eq!(
            CapabilityClientPlan::decode(&bytes),
            Err(ClientPlanError::UnsupportedVersion(version))
        );
    }
}

#[test]
fn capability_plan_rejects_declared_inner_version_mismatch() {
    let mut expression_payload = CapabilityClientPlan::new(
        InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::Boolean {
            value: true,
        })),
        vec![CapabilityRequirement::new(
            "std.secret.use",
            CapabilityArgumentSource::Parameter("p_secret".to_owned()),
        )],
    )
    .encode()
    .expect("the plan encodes");
    expression_payload[13..17].copy_from_slice(&INSPECT_FORMAT_VERSION.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&expression_payload),
        Err(ClientPlanError::InnerVersionMismatch {
            declared: INSPECT_FORMAT_VERSION,
            actual: EXPRESSION_FORMAT_VERSION,
        })
    );

    let inspect = ExpressionClientPlan::new(ClientExpressionNode::Inspect {
        operation: InspectOperationNode::Snapshot {
            target: Box::new(ClientExpressionNode::Boolean { value: true }),
            options: None,
        },
    });
    let mut inspect_payload = CapabilityClientPlan::new(
        InnerClientPlan::Expression(inspect),
        vec![CapabilityRequirement::new(
            "std.secret.use",
            CapabilityArgumentSource::Parameter("p_secret".to_owned()),
        )],
    )
    .encode()
    .expect("the plan encodes");
    inspect_payload[13..17].copy_from_slice(&EXPRESSION_FORMAT_VERSION.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&inspect_payload),
        Err(ClientPlanError::InnerVersionMismatch {
            declared: EXPRESSION_FORMAT_VERSION,
            actual: INSPECT_FORMAT_VERSION,
        })
    );
}

#[test]
fn capability_plan_rejects_magic_version_operation_and_trailing_corruption() {
    let encoded = capability_plan().encode().expect("the plan encodes");

    let mut wrong_magic = encoded.clone();
    wrong_magic[0] = b'X';
    assert_eq!(
        CapabilityClientPlan::decode(&wrong_magic),
        Err(ClientPlanError::InvalidMagic)
    );

    let mut wrong_version = encoded.clone();
    wrong_version[8..12].copy_from_slice(&4_u32.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&wrong_version),
        Err(ClientPlanError::UnsupportedVersion(4))
    );

    let mut wrong_operation = encoded.clone();
    wrong_operation[12] = RETURN_STATE_OPERATION;
    assert_eq!(
        CapabilityClientPlan::decode(&wrong_operation),
        Err(ClientPlanError::InvalidOperation(RETURN_STATE_OPERATION))
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        CapabilityClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );
}

#[test]
fn capability_plan_rejects_invalid_argument_tags_and_utf8() {
    let count_offset = 8 + 4 + 1 + 4 + 4 + ENCODED_LENGTH;
    let name_length_offset = count_offset + 4;
    let tag_offset = name_length_offset + 4 + b"std.secret.use".len();
    let argument_length_offset = tag_offset + 1;

    let mut wrong_tag = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    wrong_tag[tag_offset] = 3;
    assert_eq!(
        CapabilityClientPlan::decode(&wrong_tag),
        Err(ClientPlanError::InvalidCapabilityArgumentTag(3))
    );

    let mut text_form = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    text_form[tag_offset] = CAPABILITY_ARGUMENT_TEXT;
    let decoded = CapabilityClientPlan::decode(&text_form).expect("the text form decodes");
    assert_eq!(
        decoded.requirements()[0].argument(),
        &CapabilityArgumentSource::Text("p_secret".to_owned())
    );

    let mut bad_name_utf8 = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    bad_name_utf8[name_length_offset..name_length_offset + 4].copy_from_slice(&1_u32.to_be_bytes());
    bad_name_utf8[name_length_offset + 4] = 0xff;
    assert_eq!(
        CapabilityClientPlan::decode(&bad_name_utf8),
        Err(ClientPlanError::InvalidCapabilityNameUtf8)
    );

    let mut bad_argument_utf8 = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    bad_argument_utf8[argument_length_offset..argument_length_offset + 4]
        .copy_from_slice(&1_u32.to_be_bytes());
    bad_argument_utf8[argument_length_offset + 4] = 0xff;
    assert_eq!(
        CapabilityClientPlan::decode(&bad_argument_utf8),
        Err(ClientPlanError::InvalidCapabilityArgumentUtf8)
    );
}

#[test]
fn capability_plan_rejects_zero_and_oversized_counts_and_lengths() {
    let count_offset = 8 + 4 + 1 + 4 + 4 + ENCODED_LENGTH;
    let name_length_offset = count_offset + 4;
    let tag_offset = name_length_offset + 4 + b"std.secret.use".len();
    let argument_length_offset = tag_offset + 1;

    let mut zero_count = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    zero_count[count_offset..count_offset + 4].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&zero_count),
        Err(ClientPlanError::InvalidCapabilityCount { actual: 0 })
    );

    let mut oversized_count = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    oversized_count[count_offset..count_offset + 4]
        .copy_from_slice(&(MAX_CAPABILITY_REQUIREMENTS as u32 + 1).to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&oversized_count),
        Err(ClientPlanError::CapabilityLimitExceeded {
            limit: MAX_CAPABILITY_REQUIREMENTS,
        })
    );

    let mut zero_name = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    zero_name[name_length_offset..name_length_offset + 4].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&zero_name),
        Err(ClientPlanError::EmptyCapabilityName)
    );

    let mut long_name = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    long_name[name_length_offset..name_length_offset + 4]
        .copy_from_slice(&(MAX_CAPABILITY_NAME_LENGTH as u32 + 1).to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&long_name),
        Err(ClientPlanError::CapabilityNameTooLong {
            length: MAX_CAPABILITY_NAME_LENGTH + 1,
            limit: MAX_CAPABILITY_NAME_LENGTH,
        })
    );

    let mut zero_argument = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    zero_argument[argument_length_offset..argument_length_offset + 4]
        .copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&zero_argument),
        Err(ClientPlanError::EmptyCapabilityArgument)
    );

    let mut long_argument = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    long_argument[argument_length_offset..argument_length_offset + 4]
        .copy_from_slice(&(MAX_CAPABILITY_ARGUMENT_LENGTH as u32 + 1).to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&long_argument),
        Err(ClientPlanError::CapabilityArgumentTooLong {
            length: MAX_CAPABILITY_ARGUMENT_LENGTH + 1,
            limit: MAX_CAPABILITY_ARGUMENT_LENGTH,
        })
    );

    let inner = InnerClientPlan::Boolean(ClientPlan::return_boolean(true));
    let empty = CapabilityClientPlan::new(inner.clone(), Vec::new());
    assert_eq!(
        empty.encode(),
        Err(ClientPlanError::InvalidCapabilityCount { actual: 0 })
    );

    let requirement = CapabilityRequirement::new(
        "std.fs.read",
        CapabilityArgumentSource::Text("/home/bob".to_owned()),
    );
    let oversized = CapabilityClientPlan::new(
        inner.clone(),
        vec![requirement; MAX_CAPABILITY_REQUIREMENTS + 1],
    );
    assert_eq!(
        oversized.encode(),
        Err(ClientPlanError::CapabilityLimitExceeded {
            limit: MAX_CAPABILITY_REQUIREMENTS,
        })
    );

    let empty_name = CapabilityClientPlan::new(
        inner.clone(),
        vec![CapabilityRequirement::new(
            "",
            CapabilityArgumentSource::Text("x".to_owned()),
        )],
    );
    assert_eq!(
        empty_name.encode(),
        Err(ClientPlanError::EmptyCapabilityName)
    );

    let long_name_plan = CapabilityClientPlan::new(
        inner.clone(),
        vec![CapabilityRequirement::new(
            "x".repeat(MAX_CAPABILITY_NAME_LENGTH + 1),
            CapabilityArgumentSource::Text("x".to_owned()),
        )],
    );
    assert_eq!(
        long_name_plan.encode(),
        Err(ClientPlanError::CapabilityNameTooLong {
            length: MAX_CAPABILITY_NAME_LENGTH + 1,
            limit: MAX_CAPABILITY_NAME_LENGTH,
        })
    );

    let empty_argument = CapabilityClientPlan::new(
        inner.clone(),
        vec![CapabilityRequirement::new(
            "std.fs.read",
            CapabilityArgumentSource::Text(String::new()),
        )],
    );
    assert_eq!(
        empty_argument.encode(),
        Err(ClientPlanError::EmptyCapabilityArgument)
    );

    let long_argument_plan = CapabilityClientPlan::new(
        inner,
        vec![CapabilityRequirement::new(
            "std.fs.read",
            CapabilityArgumentSource::Text("x".repeat(MAX_CAPABILITY_ARGUMENT_LENGTH + 1)),
        )],
    );
    assert_eq!(
        long_argument_plan.encode(),
        Err(ClientPlanError::CapabilityArgumentTooLong {
            length: MAX_CAPABILITY_ARGUMENT_LENGTH + 1,
            limit: MAX_CAPABILITY_ARGUMENT_LENGTH,
        })
    );
}

#[test]
fn capability_plan_rejects_duplicate_requirement_names() {
    let duplicated = CapabilityClientPlan::new(
        InnerClientPlan::Boolean(ClientPlan::return_boolean(true)),
        vec![
            CapabilityRequirement::new(
                "std.fs.read",
                CapabilityArgumentSource::Text("/home/bob".to_owned()),
            ),
            CapabilityRequirement::new(
                "std.fs.read",
                CapabilityArgumentSource::Parameter("p_path".to_owned()),
            ),
        ],
    );
    assert_eq!(
        duplicated.encode(),
        Err(ClientPlanError::DuplicateCapabilityName(
            "std.fs.read".to_owned()
        ))
    );

    let count_offset = 8 + 4 + 1 + 4 + 4 + ENCODED_LENGTH;
    let mut crafted = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    crafted[count_offset..count_offset + 4].copy_from_slice(&2_u32.to_be_bytes());
    crafted.extend_from_slice(&(b"std.secret.use".len() as u32).to_be_bytes());
    crafted.extend_from_slice(b"std.secret.use");
    crafted.push(CAPABILITY_ARGUMENT_TEXT);
    crafted.extend_from_slice(&(b"/home/bob".len() as u32).to_be_bytes());
    crafted.extend_from_slice(b"/home/bob");
    assert_eq!(
        CapabilityClientPlan::decode(&crafted),
        Err(ClientPlanError::DuplicateCapabilityName(
            "std.secret.use".to_owned()
        ))
    );
}

#[test]
fn capability_plan_rejects_unsupported_inner_versions_and_malformed_payloads() {
    let mut zero_inner = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    zero_inner[13..17].copy_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&zero_inner),
        Err(ClientPlanError::UnsupportedInnerVersion(0))
    );

    let mut inner_five = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    inner_five[13..17].copy_from_slice(&CAPABILITY_FORMAT_VERSION.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&inner_five),
        Err(ClientPlanError::UnsupportedInnerVersion(
            CAPABILITY_FORMAT_VERSION
        ))
    );

    let mut mismatched = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    mismatched[13..17].copy_from_slice(&OPAQUE_FORMAT_VERSION.to_be_bytes());
    // The inner payload is a version-1 artefact; the version-2 decoder
    // rejects it with the payload's own version.
    assert_eq!(
        CapabilityClientPlan::decode(&mismatched),
        Err(ClientPlanError::UnsupportedVersion(FORMAT_VERSION))
    );

    let mut corrupt_inner = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    corrupt_inner[8 + 4 + 1 + 4 + 4] = b'X';
    assert_eq!(
        CapabilityClientPlan::decode(&corrupt_inner),
        Err(ClientPlanError::InvalidMagic)
    );

    let mut oversized_inner = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    oversized_inner[17..21].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        CapabilityClientPlan::decode(&oversized_inner),
        Err(ClientPlanError::Truncated)
    );
}

#[test]
fn capability_plan_exposes_inner_plan_and_requirement_accessors() {
    let plan = capability_plan();
    let bytes = plan.encode().expect("the plan encodes");
    let decoded = CapabilityClientPlan::decode(&bytes).expect("the plan decodes");
    assert_eq!(decoded.format_version(), CAPABILITY_FORMAT_VERSION);
    assert_eq!(decoded.inner_plan_version(), EXPRESSION_FORMAT_VERSION);
    assert!(matches!(
        decoded.inner_plan(),
        InnerClientPlan::Expression(_)
    ));
    let requirements = decoded.requirements();
    assert_eq!(requirements.len(), 2);
    assert_eq!(requirements[0].name(), "std.fs.read");
    assert_eq!(
        requirements[0].argument(),
        &CapabilityArgumentSource::Text("/home/bob".to_owned())
    );
    assert_eq!(requirements[1].name(), "std.net.connect");
    assert_eq!(
        requirements[1].argument(),
        &CapabilityArgumentSource::Parameter("p_host".to_owned())
    );
}
#[test]
fn capability_plan_round_trips_resource_inner_plan() {
    let plan = CapabilityClientPlan::new(
        InnerClientPlan::Resource(resource_plan()),
        vec![CapabilityRequirement::new(
            "std.data.query",
            CapabilityArgumentSource::Parameter("p_owner".to_owned()),
        )],
    );
    let encoded = plan.encode().expect("resource capability plan encodes");
    let decoded = CapabilityClientPlan::decode(&encoded).expect("resource capability plan decodes");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.inner_plan_version(), RESOURCE_FORMAT_VERSION);
    assert!(matches!(decoded.inner_plan(), InnerClientPlan::Resource(_)));
}

#[test]
fn capability_plan_rejects_every_truncated_prefix() {
    let encoded = minimal_capability_plan()
        .encode()
        .expect("the plan encodes");
    for length in 0..encoded.len() {
        assert_eq!(
            CapabilityClientPlan::decode(&encoded[..length]),
            Err(ClientPlanError::Truncated),
            "prefix length {length} must be truncated"
        );
    }
}

#[test]
fn expression_plan_rejects_resource_nodes_outside_version_six() {
    let await_expression = ClientExpressionNode::Await {
        expression: Box::new(ClientExpressionNode::Boolean { value: true }),
    };
    assert_eq!(
        ExpressionClientPlan::new(await_expression).encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT))
    );

    let resource_expression = ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x21; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x22; 16]),
                CatalogueRevisionId::from_bytes([0x23; 16]),
            ),
            CallSiteId::from_bytes([0x24; 16]),
            Vec::new(),
            TypeId::from_bytes([0x25; 16]),
        ),
    };
    assert_eq!(
        ExpressionClientPlan::new(resource_expression).encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE))
    );
}
fn resource_plan() -> ResourceClientPlan {
    ResourceClientPlan::new(ClientExpressionNode::Await {
        expression: Box::new(ClientExpressionNode::Resource {
            operation: ResourceOperationNode::new(
                ResourceKind::Stream,
                FunctionId::from_bytes([0x21; 16]),
                RevisionPair::new(
                    SourceRevisionId::from_bytes([0x22; 16]),
                    CatalogueRevisionId::from_bytes([0x23; 16]),
                ),
                CallSiteId::from_bytes([0x24; 16]),
                vec![
                    (
                        ParameterId::from_bytes([0x31; 16]),
                        ClientExpressionNode::ParameterRead {
                            parameter: ParameterId::from_bytes([0x41; 16]),
                        },
                    ),
                    (
                        ParameterId::from_bytes([0x32; 16]),
                        ClientExpressionNode::Boolean { value: true },
                    ),
                ],
                TypeId::from_bytes([0x51; 16]),
            ),
        }),
    })
}

#[test]
fn resource_plan_round_trips_kind_revision_call_site_arguments_and_await() {
    let plan = resource_plan();
    let encoded = plan.encode().expect("resource plan encodes");
    assert_eq!(&encoded[..8], &MAGIC);
    assert_eq!(&encoded[8..12], &RESOURCE_FORMAT_VERSION.to_be_bytes());
    assert_eq!(encoded[12], RETURN_RESOURCE_OPERATION);
    assert_eq!(encoded[13], NODE_AWAIT);
    assert_eq!(encoded[14], NODE_RESOURCE);
    let decoded = ResourceClientPlan::decode(&encoded).expect("resource plan decodes");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.format_version(), RESOURCE_FORMAT_VERSION);
    let ClientExpressionNode::Await { expression } = decoded.expression() else {
        panic!("resource plan root must be await");
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(operation.kind(), ResourceKind::Stream);
    assert_eq!(operation.target(), FunctionId::from_bytes([0x21; 16]));
    assert_eq!(
        operation.target_revision().source(),
        SourceRevisionId::from_bytes([0x22; 16])
    );
    assert_eq!(
        operation.target_revision().catalogue(),
        CatalogueRevisionId::from_bytes([0x23; 16])
    );
    assert_eq!(operation.call_site_id(), CallSiteId::from_bytes([0x24; 16]));
    assert_eq!(operation.arguments().len(), 2);
    assert_eq!(
        operation.declared_result_type(),
        TypeId::from_bytes([0x51; 16])
    );
}

#[test]
fn resource_plan_round_trips_scalar_identity_and_argument_shape() {
    let target = FunctionId::from_bytes([0x61; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x62; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x63; 16]);
    let call_site = CallSiteId::from_bytes([0x64; 16]);
    let first_parameter = ParameterId::from_bytes([0x65; 16]);
    let second_parameter = ParameterId::from_bytes([0x66; 16]);
    let result_type = TypeId::from_bytes([0x67; 16]);
    let arguments = vec![
        (
            first_parameter,
            ClientExpressionNode::String {
                value: "owner".to_owned(),
            },
        ),
        (
            second_parameter,
            ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0x68; 16]),
            },
        ),
    ];
    let plan = ResourceClientPlan::new(ClientExpressionNode::Await {
        expression: Box::new(ClientExpressionNode::Resource {
            operation: ResourceOperationNode::new(
                ResourceKind::Scalar,
                target,
                RevisionPair::new(source_revision, catalogue_revision),
                call_site,
                arguments.clone(),
                result_type,
            ),
        }),
    });

    let decoded =
        ResourceClientPlan::decode(&plan.encode().expect("plan encodes")).expect("plan decodes");
    let ClientExpressionNode::Await { expression } = decoded.expression() else {
        panic!("resource plan root must be await");
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        panic!("await operand must be a resource");
    };
    assert_eq!(operation.kind(), ResourceKind::Scalar);
    assert_eq!(operation.target(), target);
    assert_eq!(operation.target_revision().source(), source_revision);
    assert_eq!(operation.target_revision().catalogue(), catalogue_revision);
    assert_eq!(operation.call_site_id(), call_site);
    assert_eq!(operation.arguments(), arguments.as_slice());
    assert_eq!(operation.declared_result_type(), result_type);
}

#[test]
fn resource_plan_encode_rejects_zero_identity_fields() {
    let make = |target, source, catalogue, call_site, result_type, parameter| {
        ResourceClientPlan::new(ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::Resource {
                operation: ResourceOperationNode::new(
                    ResourceKind::Scalar,
                    FunctionId::from_bytes(target),
                    RevisionPair::new(
                        SourceRevisionId::from_bytes(source),
                        CatalogueRevisionId::from_bytes(catalogue),
                    ),
                    CallSiteId::from_bytes(call_site),
                    vec![(
                        ParameterId::from_bytes(parameter),
                        ClientExpressionNode::Boolean { value: true },
                    )],
                    TypeId::from_bytes(result_type),
                ),
            }),
        })
    };
    let cases = [
        (
            [0; 16], [0x22; 16], [0x23; 16], [0x24; 16], [0x25; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0; 16], [0x23; 16], [0x24; 16], [0x25; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x22; 16], [0; 16], [0x24; 16], [0x25; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x22; 16], [0x23; 16], [0; 16], [0x25; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x22; 16], [0x23; 16], [0x24; 16], [0; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x22; 16], [0x23; 16], [0x24; 16], [0x25; 16], [0; 16],
        ),
    ];
    for (target, source, catalogue, call_site, result_type, parameter) in cases {
        assert_eq!(
            make(target, source, catalogue, call_site, result_type, parameter).encode(),
            Err(ClientPlanError::InvalidResourceIdentity),
        );
    }
}

#[test]
fn resource_plan_decode_rejects_zero_identity_fields() {
    let encoded = resource_plan().encode().expect("the resource plan encodes");
    let body_offset = 8 + 4 + 1;
    let identity_offsets = [3, 19, 35, 51, 122];
    for relative_offset in identity_offsets {
        let mut corrupted = encoded.clone();
        corrupted[body_offset + relative_offset..body_offset + relative_offset + 16].fill(0);
        assert_eq!(
            ResourceClientPlan::decode(&corrupted),
            Err(ClientPlanError::InvalidResourceIdentity),
            "identity field at offset {relative_offset} must be rejected"
        );
    }

    let first_parameter_offset = body_offset + 3 + (16 * 4) + 4;
    for parameter_offset in [first_parameter_offset, first_parameter_offset + 16 + 17] {
        let mut corrupted = encoded.clone();
        corrupted[parameter_offset..parameter_offset + 16].fill(0);
        assert_eq!(
            ResourceClientPlan::decode(&corrupted),
            Err(ClientPlanError::InvalidResourceIdentity),
            "argument identity at offset {parameter_offset} must be rejected"
        );
    }
}

#[test]
fn resource_plan_rejects_invalid_await_placement() {
    let mut await_non_resource = Vec::new();
    await_non_resource.extend_from_slice(&MAGIC);
    await_non_resource.extend_from_slice(&RESOURCE_FORMAT_VERSION.to_be_bytes());
    await_non_resource.push(RETURN_RESOURCE_OPERATION);
    await_non_resource.push(NODE_AWAIT);
    await_non_resource.push(NODE_BOOLEAN);
    await_non_resource.push(1);
    assert_eq!(
        ResourceClientPlan::decode(&await_non_resource),
        Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT))
    );

    let mut bare_resource = resource_plan().encode().expect("resource plan encodes");
    bare_resource.remove(13);
    assert_eq!(
        ResourceClientPlan::decode(&bare_resource),
        Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE))
    );

    let bare = ResourceClientPlan::new(ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x21; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x22; 16]),
                CatalogueRevisionId::from_bytes([0x23; 16]),
            ),
            CallSiteId::from_bytes([0x24; 16]),
            Vec::new(),
            TypeId::from_bytes([0x25; 16]),
        ),
    });
    assert_eq!(
        bare.encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE))
    );

    let nested_await = ResourceClientPlan::new(ClientExpressionNode::Call {
        function: FunctionId::from_bytes([0x61; 16]),
        arguments: vec![(
            ParameterId::from_bytes([0x62; 16]),
            resource_plan().expression().clone(),
        )],
    });
    assert_eq!(
        nested_await.encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT))
    );

    let encoded_resource = resource_plan().encode().expect("resource plan encodes");
    let mut nested_await_bytes = encoded_resource[..13].to_vec();
    nested_await_bytes.push(NODE_CALL);
    nested_await_bytes.extend_from_slice(&[0x61; 16]);
    nested_await_bytes.extend_from_slice(&1_u32.to_be_bytes());
    nested_await_bytes.extend_from_slice(&[0x62; 16]);
    nested_await_bytes.extend_from_slice(&encoded_resource[13..]);
    assert_eq!(
        ResourceClientPlan::decode(&nested_await_bytes),
        Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT))
    );
}

#[test]
fn resource_plan_rejects_noncanonical_and_duplicate_arguments() {
    let operation = |arguments| {
        ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x21; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x22; 16]),
                CatalogueRevisionId::from_bytes([0x23; 16]),
            ),
            CallSiteId::from_bytes([0x24; 16]),
            arguments,
            TypeId::from_bytes([0x51; 16]),
        )
    };
    let unsorted = ResourceClientPlan::new(ClientExpressionNode::Await {
        expression: Box::new(ClientExpressionNode::Resource {
            operation: operation(vec![
                (
                    ParameterId::from_bytes([0x32; 16]),
                    ClientExpressionNode::Boolean { value: true },
                ),
                (
                    ParameterId::from_bytes([0x31; 16]),
                    ClientExpressionNode::Boolean { value: false },
                ),
            ]),
        }),
    });
    assert_eq!(
        unsorted.encode(),
        Err(ClientPlanError::NonCanonicalResourceArgumentOrder)
    );
    let duplicate = ResourceClientPlan::new(ClientExpressionNode::Await {
        expression: Box::new(ClientExpressionNode::Resource {
            operation: operation(vec![
                (
                    ParameterId::from_bytes([0x31; 16]),
                    ClientExpressionNode::Boolean { value: true },
                ),
                (
                    ParameterId::from_bytes([0x31; 16]),
                    ClientExpressionNode::Boolean { value: false },
                ),
            ]),
        }),
    });
    assert_eq!(
        duplicate.encode(),
        Err(ClientPlanError::DuplicateResourceArgument(
            ParameterId::from_bytes([0x31; 16])
        ))
    );
}

#[test]
fn resource_plan_rejects_malformed_kind_and_limits() {
    let mut invalid_kind = resource_plan().encode().expect("resource plan encodes");
    invalid_kind[15] = 9;
    assert_eq!(
        ResourceClientPlan::decode(&invalid_kind),
        Err(ClientPlanError::InvalidResourceKind(9))
    );
    let second_parameter_offset = 13 + 1 + 1 + 1 + 16 + 16 + 16 + 16 + 4 + 16 + 17;
    let mut noncanonical = resource_plan().encode().expect("resource plan encodes");
    noncanonical[second_parameter_offset..second_parameter_offset + 16]
        .copy_from_slice(&[0x30; 16]);
    assert_eq!(
        ResourceClientPlan::decode(&noncanonical),
        Err(ClientPlanError::NonCanonicalResourceArgumentOrder)
    );
    let mut duplicate = resource_plan().encode().expect("resource plan encodes");
    duplicate[second_parameter_offset..second_parameter_offset + 16].copy_from_slice(&[0x31; 16]);
    assert_eq!(
        ResourceClientPlan::decode(&duplicate),
        Err(ClientPlanError::DuplicateResourceArgument(
            ParameterId::from_bytes([0x31; 16])
        ))
    );

    let empty = ResourceClientPlan::new(ClientExpressionNode::Boolean { value: true });
    assert_eq!(
        empty.encode(),
        Err(ClientPlanError::InvalidResourceOperationCount { actual: 0 })
    );

    let oversized = ResourceClientPlan::new(ClientExpressionNode::Await {
        expression: Box::new(ClientExpressionNode::Resource {
            operation: ResourceOperationNode::new(
                ResourceKind::Scalar,
                FunctionId::from_bytes([0x21; 16]),
                RevisionPair::new(
                    SourceRevisionId::from_bytes([0x22; 16]),
                    CatalogueRevisionId::from_bytes([0x23; 16]),
                ),
                CallSiteId::from_bytes([0x24; 16]),
                (0..=MAX_RESOURCE_ARGUMENTS)
                    .map(|index| {
                        (
                            ParameterId::from_bytes([index as u8; 16]),
                            ClientExpressionNode::Boolean { value: true },
                        )
                    })
                    .collect(),
                TypeId::from_bytes([0x51; 16]),
            ),
        }),
    });
    assert_eq!(
        oversized.encode(),
        Err(ClientPlanError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS
        })
    );

    let encoded = resource_plan().encode().expect("resource plan encodes");
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        ResourceClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );
    let mut wrong_version = encoded.clone();
    wrong_version[8..12].copy_from_slice(&CAPABILITY_FORMAT_VERSION.to_be_bytes());
    assert_eq!(
        ResourceClientPlan::decode(&wrong_version),
        Err(ClientPlanError::UnsupportedVersion(
            CAPABILITY_FORMAT_VERSION
        ))
    );
    let mut wrong_operation = encoded.clone();
    wrong_operation[12] = RETURN_EXPRESSION_OPERATION;
    assert_eq!(
        ResourceClientPlan::decode(&wrong_operation),
        Err(ClientPlanError::InvalidOperation(
            RETURN_EXPRESSION_OPERATION
        ))
    );
    for length in 0..encoded.len() {
        assert_eq!(
            ResourceClientPlan::decode(&encoded[..length]),
            Err(ClientPlanError::Truncated),
            "resource prefix length {length} must be truncated"
        );
    }
}

#[test]
fn procedural_plan_round_trips_locals_statements_local_reads_and_resources() {
    let local = LocalId::from_bytes([0x71; 16]);
    let function = FunctionId::from_bytes([0x21; 16]);
    let resource = ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            function,
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x22; 16]),
                CatalogueRevisionId::from_bytes([0x23; 16]),
            ),
            CallSiteId::from_bytes([0x24; 16]),
            Vec::new(),
            TypeId::from_bytes([0x25; 16]),
        ),
    };
    let plan = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            TypeId::from_bytes([0x25; 16]),
            ClientLocalKind::Resource(ResourceKind::Scalar),
        )],
        vec![ClientStatement::let_(local, resource)],
        ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::LocalRead { local }),
        },
    );
    let encoded = plan.encode().expect("procedural plan encodes");
    assert_eq!(&encoded[..8], &MAGIC);
    assert_eq!(&encoded[8..12], &PROCEDURAL_FORMAT_VERSION.to_be_bytes());
    assert_eq!(encoded[12], RETURN_PROCEDURAL_OPERATION);
    assert_eq!(ProceduralClientPlan::decode(&encoded), Ok(plan.clone()));
    assert_eq!(plan.format_version(), PROCEDURAL_FORMAT_VERSION);
    let capability = CapabilityClientPlan::new(
        InnerClientPlan::Procedural(plan.clone()),
        vec![CapabilityRequirement::new(
            "std.data.query",
            CapabilityArgumentSource::Text("scope".to_owned()),
        )],
    );
    let decoded_capability =
        CapabilityClientPlan::decode(&capability.encode().expect("capability envelope encodes"))
            .expect("capability envelope decodes");
    assert_eq!(
        decoded_capability.inner_plan_version(),
        PROCEDURAL_FORMAT_VERSION
    );
    assert_eq!(
        decoded_capability.inner_plan(),
        &InnerClientPlan::Procedural(plan)
    );
}
#[test]
fn procedural_plan_round_trips_stream_resource_awaits_in_let_assignment_and_return() {
    let resource_local = LocalId::from_bytes([0x79; 16]);
    let value_local = LocalId::from_bytes([0x7a; 16]);
    let result_type = TypeId::from_bytes([0x7b; 16]);
    let operation = || ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Stream,
            FunctionId::from_bytes([0x7c; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x7d; 16]),
                CatalogueRevisionId::from_bytes([0x7e; 16]),
            ),
            CallSiteId::from_bytes([0x7f; 16]),
            vec![(
                ParameterId::from_bytes([0x80; 16]),
                ClientExpressionNode::Boolean { value: true },
            )],
            result_type,
        ),
    };
    let plan = ProceduralClientPlan::new(
        vec![
            ClientLocal::new(
                resource_local,
                result_type,
                ClientLocalKind::Resource(ResourceKind::Stream),
            ),
            ClientLocal::new(value_local, result_type, ClientLocalKind::Value),
        ],
        vec![
            ClientStatement::let_(resource_local, operation()),
            ClientStatement::let_(
                value_local,
                ClientExpressionNode::Await {
                    expression: Box::new(operation()),
                },
            ),
            ClientStatement::assignment(
                value_local,
                ClientExpressionNode::Await {
                    expression: Box::new(ClientExpressionNode::LocalRead {
                        local: resource_local,
                    }),
                },
            ),
        ],
        ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::LocalRead {
                local: resource_local,
            }),
        },
    );

    let encoded = plan.encode().expect("stream procedural plan encodes");
    assert_eq!(ProceduralClientPlan::decode(&encoded), Ok(plan));
}

#[test]
fn procedural_plan_rejects_resource_local_kind_mismatch_at_encode_and_decode_boundaries() {
    let local = LocalId::from_bytes([0x81; 16]);
    let result_type = TypeId::from_bytes([0x82; 16]);
    let resource = ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x83; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x84; 16]),
                CatalogueRevisionId::from_bytes([0x85; 16]),
            ),
            CallSiteId::from_bytes([0x86; 16]),
            Vec::new(),
            result_type,
        ),
    };
    let plan = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            result_type,
            ClientLocalKind::Resource(ResourceKind::Stream),
        )],
        vec![ClientStatement::let_(local, resource)],
        ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::LocalRead { local }),
        },
    );
    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::ProceduralLocalKindMismatch(local))
    );

    let valid = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            result_type,
            ClientLocalKind::Resource(ResourceKind::Scalar),
        )],
        vec![ClientStatement::let_(
            local,
            ClientExpressionNode::Resource {
                operation: ResourceOperationNode::new(
                    ResourceKind::Scalar,
                    FunctionId::from_bytes([0x83; 16]),
                    RevisionPair::new(
                        SourceRevisionId::from_bytes([0x84; 16]),
                        CatalogueRevisionId::from_bytes([0x85; 16]),
                    ),
                    CallSiteId::from_bytes([0x86; 16]),
                    Vec::new(),
                    result_type,
                ),
            },
        )],
        ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::LocalRead { local }),
        },
    );
    let mut malformed = valid.encode().expect("valid procedural plan encodes");
    let local_kind_offset = 13 + 4 + 16 + 16;
    malformed[local_kind_offset] = LOCAL_KIND_RESOURCE_STREAM;
    assert_eq!(
        ProceduralClientPlan::decode(&malformed),
        Err(ClientPlanError::ProceduralLocalKindMismatch(local))
    );
}

#[test]
fn procedural_plan_rejects_nested_resource_and_await_placements() {
    let value_local = LocalId::from_bytes([0x87; 16]);
    let result_type = TypeId::from_bytes([0x88; 16]);
    let resource = ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x89; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x8a; 16]),
                CatalogueRevisionId::from_bytes([0x8b; 16]),
            ),
            CallSiteId::from_bytes([0x8c; 16]),
            Vec::new(),
            result_type,
        ),
    };
    let nested_resource = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            value_local,
            result_type,
            ClientLocalKind::Value,
        )],
        vec![ClientStatement::let_(
            value_local,
            ClientExpressionNode::Call {
                function: FunctionId::from_bytes([0x8d; 16]),
                arguments: vec![(ParameterId::from_bytes([0x8e; 16]), resource.clone())],
            },
        )],
        ClientExpressionNode::Boolean { value: true },
    );
    assert_eq!(
        nested_resource.encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_RESOURCE))
    );

    let nested_await = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            value_local,
            result_type,
            ClientLocalKind::Value,
        )],
        vec![ClientStatement::let_(
            value_local,
            ClientExpressionNode::Concat {
                left: Box::new(ClientExpressionNode::Await {
                    expression: Box::new(resource),
                }),
                right: Box::new(ClientExpressionNode::String {
                    value: "suffix".to_owned(),
                }),
            },
        )],
        ClientExpressionNode::Boolean { value: true },
    );
    assert_eq!(
        nested_await.encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT))
    );
}

#[test]
fn procedural_plan_round_trips_inspector_operations() {
    let local = LocalId::from_bytes([0x76; 16]);
    let parameter = ParameterId::from_bytes([0x77; 16]);
    let snapshot = ClientExpressionNode::Inspect {
        operation: InspectOperationNode::snapshot(ClientExpressionNode::ParameterRead {
            parameter,
        }),
    };
    let plan = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            TypeId::from_bytes([0x78; 16]),
            ClientLocalKind::Value,
        )],
        vec![ClientStatement::let_(local, snapshot)],
        ClientExpressionNode::Inspect {
            operation: InspectOperationNode::projection(
                InspectProjection::UiNodes,
                ClientExpressionNode::LocalRead { local },
            ),
        },
    );
    let encoded = plan.encode().expect("procedural Inspector plan encodes");
    assert_eq!(ProceduralClientPlan::decode(&encoded), Ok(plan));
}

#[test]
fn procedural_plan_rejects_forward_local_reads_at_encode_and_decode_boundaries() {
    let first = LocalId::from_bytes([0x71; 16]);
    let second = LocalId::from_bytes([0x72; 16]);
    let type_id = TypeId::from_bytes([0x73; 16]);
    let forward = ProceduralClientPlan::new(
        vec![
            ClientLocal::new(first, type_id, ClientLocalKind::Value),
            ClientLocal::new(second, type_id, ClientLocalKind::Value),
        ],
        vec![
            ClientStatement::let_(first, ClientExpressionNode::LocalRead { local: second }),
            ClientStatement::let_(second, ClientExpressionNode::Boolean { value: true }),
        ],
        ClientExpressionNode::Boolean { value: true },
    );
    assert_eq!(
        forward.encode(),
        Err(ClientPlanError::ProceduralLocalReadBeforeLet(second))
    );

    let valid = ProceduralClientPlan::new(
        vec![
            ClientLocal::new(first, type_id, ClientLocalKind::Value),
            ClientLocal::new(second, type_id, ClientLocalKind::Value),
        ],
        vec![
            ClientStatement::let_(first, ClientExpressionNode::Boolean { value: true }),
            ClientStatement::let_(second, ClientExpressionNode::Boolean { value: false }),
        ],
        ClientExpressionNode::Boolean { value: true },
    );
    let mut malformed = valid.encode().expect("valid procedural plan encodes");
    let first_expression = 13 + 4 + 2 * (16 + 16 + 1) + 4 + 1 + 16;
    let mut replacement = vec![NODE_LOCAL_READ];
    replacement.extend_from_slice(&second.to_bytes());
    malformed.splice(first_expression..first_expression + 2, replacement);
    assert_eq!(
        ProceduralClientPlan::decode(&malformed),
        Err(ClientPlanError::ProceduralLocalReadBeforeLet(second))
    );
}

#[test]
fn procedural_plan_rejects_unknown_locals_and_legacy_local_reads() {
    let local = LocalId::from_bytes([0x72; 16]);
    let undeclared = LocalId::from_bytes([0x73; 16]);
    let plan = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            TypeId::from_bytes([0x74; 16]),
            ClientLocalKind::Value,
        )],
        vec![ClientStatement::assignment(
            undeclared,
            ClientExpressionNode::Boolean { value: true },
        )],
        ClientExpressionNode::Boolean { value: false },
    );
    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::UnknownProceduralLocal(undeclared))
    );
    let local_read = ExpressionClientPlan::new(ClientExpressionNode::LocalRead { local });
    assert_eq!(
        local_read.encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_LOCAL_READ))
    );
    let mut legacy = TRUE_BYTES.to_vec();
    legacy[8..12].copy_from_slice(&EXPRESSION_FORMAT_VERSION.to_be_bytes());
    legacy[12] = RETURN_EXPRESSION_OPERATION;
    legacy.truncate(13);
    legacy.push(NODE_LOCAL_READ);
    legacy.extend_from_slice(&local.to_bytes());
    assert_eq!(
        ExpressionClientPlan::decode(&legacy),
        Err(ClientPlanError::InvalidExpressionNode(NODE_LOCAL_READ))
    );
    assert_eq!(
        ProceduralClientPlan::new(
            vec![],
            vec![],
            ClientExpressionNode::Await {
                expression: Box::new(ClientExpressionNode::Boolean { value: true }),
            },
        )
        .encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_AWAIT))
    );
    let value_local = LocalId::from_bytes([0x75; 16]);
    assert_eq!(
        ProceduralClientPlan::new(
            vec![ClientLocal::new(
                value_local,
                TypeId::from_bytes([0x76; 16]),
                ClientLocalKind::Value
            )],
            vec![ClientStatement::let_(
                value_local,
                ClientExpressionNode::Boolean { value: true },
            )],
            ClientExpressionNode::Await {
                expression: Box::new(ClientExpressionNode::LocalRead { local: value_local }),
            },
        )
        .encode(),
        Err(ClientPlanError::InvalidAwaitOperand(value_local))
    );
}

#[test]
fn procedural_plan_rejects_unawaited_resource_local_return() {
    let local = LocalId::from_bytes([0x81; 16]);
    let result_type = TypeId::from_bytes([0x82; 16]);
    let resource = ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x83; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x84; 16]),
                CatalogueRevisionId::from_bytes([0x85; 16]),
            ),
            CallSiteId::from_bytes([0x86; 16]),
            Vec::new(),
            result_type,
        ),
    };
    let plan = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            result_type,
            ClientLocalKind::Resource(ResourceKind::Scalar),
        )],
        vec![ClientStatement::let_(local, resource)],
        ClientExpressionNode::LocalRead { local },
    );

    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::UnawaitedResourceLocal(local))
    );
}

#[test]
fn procedural_plan_rejects_resource_local_result_type_mismatch() {
    let local = LocalId::from_bytes([0x91; 16]);
    let expected = TypeId::from_bytes([0x92; 16]);
    let actual = TypeId::from_bytes([0x93; 16]);
    let resource = ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x94; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x95; 16]),
                CatalogueRevisionId::from_bytes([0x96; 16]),
            ),
            CallSiteId::from_bytes([0x97; 16]),
            Vec::new(),
            actual,
        ),
    };
    let plan = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            expected,
            ClientLocalKind::Resource(ResourceKind::Scalar),
        )],
        vec![ClientStatement::let_(local, resource)],
        ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::LocalRead { local }),
        },
    );

    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::ProceduralLocalTypeMismatch {
            local,
            expected,
            actual,
        })
    );
}

#[test]
fn procedural_plan_rejects_awaited_value_type_mismatch_for_resource_and_local_read() {
    let value_local = LocalId::from_bytes([0x95; 16]);
    let expected = TypeId::from_bytes([0x96; 16]);
    let actual = TypeId::from_bytes([0x97; 16]);
    let operation = |result_type| {
        ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x98; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x99; 16]),
                CatalogueRevisionId::from_bytes([0x9a; 16]),
            ),
            CallSiteId::from_bytes([0x9b; 16]),
            Vec::new(),
            result_type,
        )
    };
    let direct = |local_type, result_type| {
        ProceduralClientPlan::new(
            vec![ClientLocal::new(
                value_local,
                local_type,
                ClientLocalKind::Value,
            )],
            vec![ClientStatement::let_(
                value_local,
                ClientExpressionNode::Await {
                    expression: Box::new(ClientExpressionNode::Resource {
                        operation: operation(result_type),
                    }),
                },
            )],
            ClientExpressionNode::LocalRead { local: value_local },
        )
    };
    assert_eq!(
        direct(expected, actual).encode(),
        Err(ClientPlanError::ProceduralLocalTypeMismatch {
            local: value_local,
            expected,
            actual,
        })
    );

    let mut malformed = direct(expected, expected)
        .encode()
        .expect("matching awaited value plan encodes");
    let expected_bytes = expected.to_bytes();
    let operation_result_offset = malformed
        .windows(expected_bytes.len())
        .rposition(|window| window == expected_bytes.as_slice())
        .expect("resource result type is encoded");
    malformed[operation_result_offset..operation_result_offset + 16]
        .copy_from_slice(&actual.to_bytes());
    assert_eq!(
        ProceduralClientPlan::decode(&malformed),
        Err(ClientPlanError::ProceduralLocalTypeMismatch {
            local: value_local,
            expected,
            actual,
        })
    );

    let resource_local = LocalId::from_bytes([0x9c; 16]);
    let target_local = LocalId::from_bytes([0x9d; 16]);
    let local_read = ProceduralClientPlan::new(
        vec![
            ClientLocal::new(
                resource_local,
                actual,
                ClientLocalKind::Resource(ResourceKind::Scalar),
            ),
            ClientLocal::new(target_local, expected, ClientLocalKind::Value),
        ],
        vec![
            ClientStatement::let_(
                resource_local,
                ClientExpressionNode::Resource {
                    operation: operation(actual),
                },
            ),
            ClientStatement::let_(
                target_local,
                ClientExpressionNode::Await {
                    expression: Box::new(ClientExpressionNode::LocalRead {
                        local: resource_local,
                    }),
                },
            ),
        ],
        ClientExpressionNode::LocalRead {
            local: target_local,
        },
    );
    assert_eq!(
        local_read.encode(),
        Err(ClientPlanError::ProceduralLocalTypeMismatch {
            local: target_local,
            expected,
            actual,
        })
    );
}

#[test]
fn procedural_plan_rejects_resource_local_copy_type_mismatch() {
    let target_local = LocalId::from_bytes([0x98; 16]);
    let source_local = LocalId::from_bytes([0x99; 16]);
    let expected = TypeId::from_bytes([0x9a; 16]);
    let actual = TypeId::from_bytes([0x9b; 16]);
    let resource = |function_byte, result_type| ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([function_byte; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x9c; 16]),
                CatalogueRevisionId::from_bytes([0x9d; 16]),
            ),
            CallSiteId::from_bytes([function_byte; 16]),
            Vec::new(),
            result_type,
        ),
    };
    let plan = ProceduralClientPlan::new(
        vec![
            ClientLocal::new(
                target_local,
                expected,
                ClientLocalKind::Resource(ResourceKind::Scalar),
            ),
            ClientLocal::new(
                source_local,
                actual,
                ClientLocalKind::Resource(ResourceKind::Scalar),
            ),
        ],
        vec![
            ClientStatement::let_(target_local, resource(0x9e, expected)),
            ClientStatement::let_(source_local, resource(0x9f, actual)),
            ClientStatement::assignment(
                target_local,
                ClientExpressionNode::LocalRead {
                    local: source_local,
                },
            ),
        ],
        ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::LocalRead {
                local: target_local,
            }),
        },
    );

    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::ProceduralLocalTypeMismatch {
            local: target_local,
            expected,
            actual,
        })
    );
}

#[test]
fn procedural_plan_rejects_value_local_copy_type_mismatch() {
    let target_local = LocalId::from_bytes([0x9f; 16]);
    let source_local = LocalId::from_bytes([0xa0; 16]);
    let expected = TypeId::from_bytes([0xa1; 16]);
    let actual = TypeId::from_bytes([0xa2; 16]);
    let plan = ProceduralClientPlan::new(
        vec![
            ClientLocal::new(target_local, expected, ClientLocalKind::Value),
            ClientLocal::new(source_local, actual, ClientLocalKind::Value),
        ],
        vec![
            ClientStatement::let_(target_local, ClientExpressionNode::Boolean { value: true }),
            ClientStatement::let_(source_local, ClientExpressionNode::Boolean { value: false }),
            ClientStatement::assignment(
                target_local,
                ClientExpressionNode::LocalRead {
                    local: source_local,
                },
            ),
        ],
        ClientExpressionNode::LocalRead {
            local: target_local,
        },
    );

    assert_eq!(
        plan.encode(),
        Err(ClientPlanError::ProceduralLocalTypeMismatch {
            local: target_local,
            expected,
            actual,
        })
    );
}

#[test]
fn procedural_plan_rejects_invalid_let_assignment_ordering() {
    let local = LocalId::from_bytes([0xa1; 16]);
    let value = ClientExpressionNode::Boolean { value: true };
    let assignment_before_let = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            TypeId::from_bytes([0xa2; 16]),
            ClientLocalKind::Value,
        )],
        vec![ClientStatement::assignment(local, value.clone())],
        value.clone(),
    );
    assert_eq!(
        assignment_before_let.encode(),
        Err(ClientPlanError::ProceduralAssignmentBeforeLet(local))
    );

    let duplicate_let = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            TypeId::from_bytes([0xa3; 16]),
            ClientLocalKind::Value,
        )],
        vec![
            ClientStatement::let_(local, value.clone()),
            ClientStatement::let_(local, value.clone()),
        ],
        value.clone(),
    );
    assert_eq!(
        duplicate_let.encode(),
        Err(ClientPlanError::DuplicateProceduralLet(local))
    );

    let missing_let = ProceduralClientPlan::new(
        vec![ClientLocal::new(
            local,
            TypeId::from_bytes([0xa4; 16]),
            ClientLocalKind::Value,
        )],
        Vec::new(),
        value,
    );
    assert_eq!(
        missing_let.encode(),
        Err(ClientPlanError::MissingProceduralLet(local))
    );
}

fn action_plan() -> ActionClientPlan {
    let parameter = ParameterId::from_bytes([0x31; 16]);
    ActionClientPlan::new(ActionOperationNode::new(
        ActionTargetDomain::Server,
        FunctionId::from_bytes([0x21; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x41; 16]),
            CatalogueRevisionId::from_bytes([0x42; 16]),
        ),
        CallSiteId::from_bytes([0x43; 16]),
        vec![(
            parameter,
            ClientExpressionNode::String {
                value: "owner".to_owned(),
            },
        )],
        TypeId::from_bytes([0x44; 16]),
    ))
}

#[test]
fn action_plan_round_trips_canonical_descriptor_and_accessors() {
    let plan = action_plan();
    let bytes = plan.encode().expect("the action plan encodes");
    let mut expected = Vec::new();
    expected.extend_from_slice(&MAGIC);
    expected.extend_from_slice(&ACTION_FORMAT_VERSION.to_be_bytes());
    expected.push(RETURN_ACTION_OPERATION);
    expected.push(2);
    expected.extend_from_slice(&[0x21; 16]);
    expected.extend_from_slice(&[0x41; 16]);
    expected.extend_from_slice(&[0x42; 16]);
    expected.extend_from_slice(&[0x43; 16]);
    expected.extend_from_slice(&[0x44; 16]);
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.extend_from_slice(&[0x31; 16]);
    expected.push(NODE_STRING);
    expected.extend_from_slice(&5_u32.to_be_bytes());
    expected.extend_from_slice(b"owner");
    assert_eq!(bytes, expected);

    let decoded = ActionClientPlan::decode(&bytes).expect("the action plan decodes");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.format_version(), ACTION_FORMAT_VERSION);
    assert_eq!(decoded.operation().domain(), ActionTargetDomain::Server);
    assert_eq!(
        decoded.operation().target(),
        FunctionId::from_bytes([0x21; 16])
    );
    assert_eq!(
        decoded.operation().target_function(),
        FunctionId::from_bytes([0x21; 16])
    );
    assert_eq!(
        decoded.operation().target_revision(),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x41; 16]),
            CatalogueRevisionId::from_bytes([0x42; 16]),
        )
    );
    assert_eq!(
        decoded.operation().call_site(),
        CallSiteId::from_bytes([0x43; 16])
    );
    assert_eq!(
        decoded.operation().call_site_id(),
        CallSiteId::from_bytes([0x43; 16])
    );
    assert_eq!(decoded.operation().arguments().len(), 1);
    assert_eq!(
        decoded.operation().result_type(),
        TypeId::from_bytes([0x44; 16])
    );
    assert_eq!(
        decoded.operation().declared_result_type(),
        TypeId::from_bytes([0x44; 16])
    );
}

#[test]
fn action_plan_decode_rejects_noncanonical_and_duplicate_argument_parameter_ids() {
    let source_plan = action_plan();
    let operation = source_plan.operation();
    let first = ParameterId::from_bytes([0x31; 16]);
    let second = ParameterId::from_bytes([0x32; 16]);
    let plan = ActionClientPlan::new(ActionOperationNode::new(
        operation.domain(),
        operation.target(),
        operation.target_revision(),
        operation.call_site(),
        vec![
            (first, ClientExpressionNode::Boolean { value: true }),
            (second, ClientExpressionNode::Boolean { value: false }),
        ],
        operation.result_type(),
    ));
    let encoded = plan.encode().expect("the two-argument action plan encodes");
    let count_offset = 8 + 4 + 1 + 1 + 16 * 5;
    let second_parameter_offset = count_offset + 4 + 16 + 2;

    let mut unsorted = encoded.clone();
    unsorted[second_parameter_offset..second_parameter_offset + 16].fill(0x30);
    assert_eq!(
        ActionClientPlan::decode(&unsorted),
        Err(ClientPlanError::NonCanonicalActionArgumentOrder)
    );

    let mut duplicate = encoded;
    duplicate[second_parameter_offset..second_parameter_offset + 16].fill(0x31);
    assert_eq!(
        ActionClientPlan::decode(&duplicate),
        Err(ClientPlanError::DuplicateActionArgument(first))
    );
}

#[test]
fn action_plan_round_trips_client_target_domain() {
    let source_plan = action_plan();
    let operation = source_plan.operation();
    let plan = ActionClientPlan::new(ActionOperationNode::new(
        ActionTargetDomain::Client,
        operation.target(),
        operation.target_revision(),
        operation.call_site(),
        operation.arguments().to_vec(),
        operation.result_type(),
    ));

    let bytes = plan.encode().expect("the client action plan encodes");
    assert_eq!(bytes[13], ActionTargetDomain::Client.tag());

    let decoded = ActionClientPlan::decode(&bytes).expect("the client action plan decodes");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.operation().domain(), ActionTargetDomain::Client);
}

#[test]
fn action_plan_encode_rejects_zero_identity_fields() {
    let make = |target, source, catalogue, call_site, result_type, parameter| {
        ActionClientPlan::new(ActionOperationNode::new(
            ActionTargetDomain::Server,
            FunctionId::from_bytes(target),
            RevisionPair::new(
                SourceRevisionId::from_bytes(source),
                CatalogueRevisionId::from_bytes(catalogue),
            ),
            CallSiteId::from_bytes(call_site),
            vec![(
                ParameterId::from_bytes(parameter),
                ClientExpressionNode::String {
                    value: "owner".to_owned(),
                },
            )],
            TypeId::from_bytes(result_type),
        ))
    };
    let cases = [
        (
            [0; 16], [0x41; 16], [0x42; 16], [0x43; 16], [0x44; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0; 16], [0x42; 16], [0x43; 16], [0x44; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x41; 16], [0; 16], [0x43; 16], [0x44; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x41; 16], [0x42; 16], [0; 16], [0x44; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x41; 16], [0x42; 16], [0x43; 16], [0; 16], [0x31; 16],
        ),
        (
            [0x21; 16], [0x41; 16], [0x42; 16], [0x43; 16], [0x44; 16], [0; 16],
        ),
    ];
    for (target, source, catalogue, call_site, result_type, parameter) in cases {
        assert_eq!(
            make(target, source, catalogue, call_site, result_type, parameter).encode(),
            Err(ClientPlanError::InvalidActionIdentity),
        );
    }
}

#[test]
fn action_plan_decode_rejects_zero_identity_fields() {
    let encoded = action_plan().encode().expect("the action plan encodes");
    let body_offset = 8 + 4 + 1;
    let identity_offsets = [1, 17, 33, 49, 65];
    for relative_offset in identity_offsets {
        let mut corrupted = encoded.clone();
        corrupted[body_offset + relative_offset..body_offset + relative_offset + 16].fill(0);
        assert_eq!(
            ActionClientPlan::decode(&corrupted),
            Err(ClientPlanError::InvalidActionIdentity),
            "identity field at offset {relative_offset} must be rejected"
        );
    }

    let mut corrupted_parameter = encoded;
    let parameter_offset = body_offset + 1 + (16 * 5) + 4;
    corrupted_parameter[parameter_offset..parameter_offset + 16].fill(0);
    assert_eq!(
        ActionClientPlan::decode(&corrupted_parameter),
        Err(ClientPlanError::InvalidActionIdentity)
    );
}

#[test]
fn action_plan_rejects_malformed_domain_order_trailing_and_expression_tags() {
    let encoded = action_plan().encode().expect("the action plan encodes");
    let count_offset = 8 + 4 + 1 + 1 + 16 * 5;
    let argument_offset = count_offset + 4;
    let expression_tag_offset = argument_offset + 16;

    let mut invalid_domain = encoded.clone();
    invalid_domain[13] = 3;
    assert_eq!(
        ActionClientPlan::decode(&invalid_domain),
        Err(ClientPlanError::InvalidActionDomain(3))
    );

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        ActionClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );

    let mut truncated = encoded.clone();
    truncated.pop();
    assert_eq!(
        ActionClientPlan::decode(&truncated),
        Err(ClientPlanError::Truncated)
    );

    let mut invalid_node = encoded;
    invalid_node[expression_tag_offset] = 0xff;
    assert_eq!(
        ActionClientPlan::decode(&invalid_node),
        Err(ClientPlanError::InvalidExpressionNode(0xff))
    );

    let nested = ActionClientPlan::new(ActionOperationNode::new(
        ActionTargetDomain::Client,
        FunctionId::from_bytes([0x51; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x52; 16]),
            CatalogueRevisionId::from_bytes([0x53; 16]),
        ),
        CallSiteId::from_bytes([0x54; 16]),
        vec![(
            ParameterId::from_bytes([0x55; 16]),
            ClientExpressionNode::Action {
                operation: action_plan().operation().clone(),
            },
        )],
        TypeId::from_bytes([0x56; 16]),
    ));
    assert_eq!(
        nested.encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_ACTION))
    );
    assert_eq!(
        ExpressionClientPlan::new(ClientExpressionNode::Action {
            operation: action_plan().operation().clone(),
        })
        .encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_ACTION))
    );
}

#[test]
fn action_plan_rejects_duplicate_unsorted_and_oversized_arguments() {
    let first = ParameterId::from_bytes([0x10; 16]);
    let second = ParameterId::from_bytes([0x20; 16]);
    let value = ClientExpressionNode::Boolean { value: true };
    let unsorted = ActionClientPlan::new(ActionOperationNode::new(
        ActionTargetDomain::Client,
        FunctionId::from_bytes([0x61; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x62; 16]),
            CatalogueRevisionId::from_bytes([0x63; 16]),
        ),
        CallSiteId::from_bytes([0x64; 16]),
        vec![(second, value.clone()), (first, value.clone())],
        TypeId::from_bytes([0x65; 16]),
    ));
    assert_eq!(
        unsorted.encode(),
        Err(ClientPlanError::NonCanonicalActionArgumentOrder)
    );

    let duplicate = ActionClientPlan::new(ActionOperationNode::new(
        ActionTargetDomain::Client,
        FunctionId::from_bytes([0x61; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x62; 16]),
            CatalogueRevisionId::from_bytes([0x63; 16]),
        ),
        CallSiteId::from_bytes([0x64; 16]),
        vec![(first, value.clone()), (first, value)],
        TypeId::from_bytes([0x65; 16]),
    ));
    assert_eq!(
        duplicate.encode(),
        Err(ClientPlanError::DuplicateActionArgument(first))
    );

    let oversized = ActionClientPlan::new(ActionOperationNode::new(
        ActionTargetDomain::Client,
        FunctionId::from_bytes([0x66; 16]),
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x67; 16]),
            CatalogueRevisionId::from_bytes([0x68; 16]),
        ),
        CallSiteId::from_bytes([0x69; 16]),
        (0..=MAX_ACTION_ARGUMENTS)
            .map(|index| {
                (
                    ParameterId::from_bytes([index as u8; 16]),
                    ClientExpressionNode::Boolean { value: false },
                )
            })
            .collect(),
        TypeId::from_bytes([0x6a; 16]),
    ));
    assert_eq!(
        oversized.encode(),
        Err(ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS
        })
    );

    let mut crafted = action_plan().encode().expect("the action plan encodes");
    let count_offset = 8 + 4 + 1 + 1 + 16 * 5;
    crafted[count_offset..count_offset + 4]
        .copy_from_slice(&((MAX_ACTION_ARGUMENTS + 1) as u32).to_be_bytes());
    assert_eq!(
        ActionClientPlan::decode(&crafted),
        Err(ClientPlanError::ActionArgumentLimitExceeded {
            limit: MAX_ACTION_ARGUMENTS
        })
    );
}

#[test]
fn capability_plan_accepts_version_eight_action_inner_plan() {
    let inner = InnerClientPlan::Action(action_plan());
    let plan = CapabilityClientPlan::new(
        inner.clone(),
        vec![CapabilityRequirement::new(
            "std.action.trigger",
            CapabilityArgumentSource::Parameter("p_action".to_owned()),
        )],
    );
    let bytes = plan.encode().expect("the capability plan encodes");
    let decoded = CapabilityClientPlan::decode(&bytes).expect("the capability plan decodes");
    assert_eq!(decoded.inner_plan_version(), ACTION_FORMAT_VERSION);
    assert_eq!(decoded.inner_plan(), &inner);
}
#[test]

fn procedural_plan_round_trips_scalar_resource_await_in_assignment() {
    let resource_local = LocalId::from_bytes([0x91; 16]);
    let value_local = LocalId::from_bytes([0x92; 16]);
    let result_type = TypeId::from_bytes([0x93; 16]);
    let operation = || ClientExpressionNode::Resource {
        operation: ResourceOperationNode::new(
            ResourceKind::Scalar,
            FunctionId::from_bytes([0x94; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x95; 16]),
                CatalogueRevisionId::from_bytes([0x96; 16]),
            ),
            CallSiteId::from_bytes([0x97; 16]),
            vec![
                (
                    ParameterId::from_bytes([0x98; 16]),
                    ClientExpressionNode::String {
                        value: "first".to_owned(),
                    },
                ),
                (
                    ParameterId::from_bytes([0x99; 16]),
                    ClientExpressionNode::Boolean { value: true },
                ),
            ],
            result_type,
        ),
    };
    let plan = ProceduralClientPlan::new(
        vec![
            ClientLocal::new(
                resource_local,
                result_type,
                ClientLocalKind::Resource(ResourceKind::Scalar),
            ),
            ClientLocal::new(value_local, result_type, ClientLocalKind::Value),
        ],
        vec![
            ClientStatement::let_(resource_local, operation()),
            ClientStatement::let_(
                value_local,
                ClientExpressionNode::Await {
                    expression: Box::new(operation()),
                },
            ),
            ClientStatement::assignment(
                value_local,
                ClientExpressionNode::Await {
                    expression: Box::new(operation()),
                },
            ),
        ],
        ClientExpressionNode::Await {
            expression: Box::new(ClientExpressionNode::LocalRead {
                local: resource_local,
            }),
        },
    );

    let encoded = plan.encode().expect("scalar procedural plan encodes");
    let decoded = ProceduralClientPlan::decode(&encoded).expect("scalar plan decodes");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.locals()[0].local_id(), resource_local);
    assert_eq!(decoded.locals()[1].local_id(), value_local);
    assert_eq!(decoded.statements()[2].local(), value_local);
    let ClientExpressionNode::Await { expression } = decoded.statements()[2].expression() else {
        panic!("scalar assignment must retain its AWAIT expression");
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        panic!("scalar assignment AWAIT must retain its resource operation");
    };
    assert_eq!(operation.kind(), ResourceKind::Scalar);
    assert_eq!(
        operation.arguments(),
        &[
            (
                ParameterId::from_bytes([0x98; 16]),
                ClientExpressionNode::String {
                    value: "first".to_owned(),
                },
            ),
            (
                ParameterId::from_bytes([0x99; 16]),
                ClientExpressionNode::Boolean { value: true },
            ),
        ],
    );
}
#[test]
fn control_flow_plan_round_trips_operators_blocks_and_returns() {
    let local = LocalId::from_bytes([0x11; 16]);
    let value_type = TypeId::from_bytes([0x12; 16]);
    let integer = |value| ClientExpressionNode::Integer { value };
    let read = || ClientExpressionNode::LocalRead { local };
    let increment = || ClientExpressionNode::Binary {
        operator: ControlFlowBinaryOperator::Add,
        left: Box::new(read()),
        right: Box::new(integer(1)),
    };
    let plan = ControlFlowClientPlan::new(
        vec![ClientLocal::new(local, value_type, ClientLocalKind::Value)],
        vec![
            ControlFlowStatement::let_(local, integer(0)),
            ControlFlowStatement::If(ControlFlowIfStatement::new(
                vec![ControlFlowIfBranch::new(
                    ClientExpressionNode::Unary {
                        operator: ControlFlowUnaryOperator::Not,
                        expression: Box::new(ClientExpressionNode::Boolean { value: false }),
                    },
                    vec![ControlFlowStatement::assignment(local, increment())],
                )],
                Some(vec![ControlFlowStatement::assignment(local, increment())]),
            )),
            ControlFlowStatement::While(ControlFlowWhileStatement::new(
                ClientExpressionNode::Boolean { value: false },
                vec![ControlFlowStatement::assignment(local, increment())],
            )),
            ControlFlowStatement::return_(Some(read())),
        ],
    );
    let encoded = plan.encode().expect("control-flow plan encodes");
    let decoded = ControlFlowClientPlan::decode(&encoded).expect("control-flow plan decodes");
    assert_eq!(decoded, plan);
    assert_eq!(decoded.format_version(), CONTROL_FLOW_FORMAT_VERSION);
    assert_eq!(decoded.locals()[0].local_id(), local);
    assert_eq!(decoded.statements().len(), 4);
}

#[test]
fn control_flow_plan_rejects_legacy_operator_nodes_and_malformed_v10_tags() {
    let unary = ClientExpressionNode::Unary {
        operator: ControlFlowUnaryOperator::Minus,
        expression: Box::new(ClientExpressionNode::Integer { value: 1 }),
    };
    assert_eq!(
        ExpressionClientPlan::new(unary.clone()).encode(),
        Err(ClientPlanError::InvalidExpressionNode(NODE_UNARY))
    );
    let plan =
        ControlFlowClientPlan::new(Vec::new(), vec![ControlFlowStatement::return_(Some(unary))]);
    let mut encoded = plan.encode().expect("control-flow plan encodes");
    encoded[24] = 99;
    assert_eq!(
        ControlFlowClientPlan::decode(&encoded),
        Err(ClientPlanError::InvalidControlFlowUnaryOperator(99))
    );
    let mut trailing = plan.encode().expect("control-flow plan encodes");
    trailing.push(0);
    assert_eq!(
        ControlFlowClientPlan::decode(&trailing),
        Err(ClientPlanError::TrailingBytes)
    );
}

#[test]
fn control_flow_plan_rejects_statement_and_branch_limits() {
    let statements = (0..=MAX_CONTROL_FLOW_STATEMENTS)
        .map(|_| ControlFlowStatement::return_empty())
        .collect();
    assert_eq!(
        ControlFlowClientPlan::new(Vec::new(), statements).encode(),
        Err(ClientPlanError::ControlFlowStatementLimitExceeded {
            limit: MAX_CONTROL_FLOW_STATEMENTS
        })
    );

    let branches = (0..=MAX_CONTROL_FLOW_BRANCHES)
        .map(|_| {
            ControlFlowIfBranch::new(ClientExpressionNode::Boolean { value: true }, Vec::new())
        })
        .collect();
    assert_eq!(
        ControlFlowClientPlan::new(
            Vec::new(),
            vec![ControlFlowStatement::If(ControlFlowIfStatement::new(
                branches, None,
            ))],
        )
        .encode(),
        Err(ClientPlanError::ControlFlowBranchLimitExceeded {
            limit: MAX_CONTROL_FLOW_BRANCHES
        })
    );
}
#[test]
fn capability_plan_accepts_version_ten_control_flow_inner_plan() {
    let inner = InnerClientPlan::ControlFlow(ControlFlowClientPlan::new(
        Vec::new(),
        vec![ControlFlowStatement::return_value(
            ClientExpressionNode::Integer { value: 7 },
        )],
    ));
    let plan = CapabilityClientPlan::new(
        inner.clone(),
        vec![CapabilityRequirement::new(
            "std.control.execute",
            CapabilityArgumentSource::Text("scope".to_owned()),
        )],
    );
    let encoded = plan.encode().expect("the capability plan encodes");
    let decoded = CapabilityClientPlan::decode(&encoded).expect("the capability plan decodes");
    assert_eq!(decoded.inner_plan_version(), CONTROL_FLOW_FORMAT_VERSION);
    assert_eq!(decoded.inner_plan(), &inner);
}
