//! Opaque codec registry and framed payload tests.

use super::*;
#[test]
fn opaque_codec_registry_is_complete_unique_and_exact() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let accepted = opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT);
    assert!(OpaqueCodecRegistry::new(standard, [accepted.clone()]).is_ok());
    assert_eq!(
        OpaqueCodecRegistry::new(standard, Vec::<OpaqueCodecRegistration>::new()).unwrap_err(),
        OpaqueCodecRegistryError::EmptyRegistry
    );

    let duplicate_type = opaque_registration(
        OPAQUE_TYPE,
        ["std", "types", "other_token"],
        "orna.std.value.other-token@1",
    );
    assert_eq!(
        OpaqueCodecRegistry::new(standard, [accepted.clone(), duplicate_type]).unwrap_err(),
        OpaqueCodecRegistryError::DuplicateType {
            opaque_type: OPAQUE_TYPE,
        }
    );
    let duplicate_name = opaque_registration(
        OTHER_OPAQUE_TYPE,
        OPAQUE_NAME,
        "orna.std.value.other-token@1",
    );
    assert_eq!(
        OpaqueCodecRegistry::new(standard, [accepted.clone(), duplicate_name]).unwrap_err(),
        OpaqueCodecRegistryError::DuplicateName {
            semantic_name: QualifiedSemanticName::new(OPAQUE_NAME).unwrap(),
        }
    );
    let duplicate_contract = opaque_registration(
        OTHER_OPAQUE_TYPE,
        ["std", "types", "other_token"],
        OPAQUE_CONTRACT,
    );
    assert_eq!(
        OpaqueCodecRegistry::new(standard, [accepted.clone(), duplicate_contract]).unwrap_err(),
        OpaqueCodecRegistryError::DuplicateContract {
            representation_contract: OPAQUE_CONTRACT.into(),
        }
    );

    for (registration, expected) in [
        (
            opaque_registration(
                OTHER_OPAQUE_TYPE,
                ["std", "types", "missing"],
                "orna.std.value.missing@1",
            ),
            OpaqueCodecRegistryError::MissingDefinition {
                opaque_type: OTHER_OPAQUE_TYPE,
            },
        ),
        (
            opaque_registration(
                STANDARD_BOOLEAN,
                ["std", "boolean"],
                "orna.kernel.value.boolean@1",
            ),
            OpaqueCodecRegistryError::WrongDefinitionKind {
                opaque_type: STANDARD_BOOLEAN,
            },
        ),
        (
            opaque_registration(
                OPAQUE_TYPE,
                ["std", "types", "wrong_token"],
                OPAQUE_CONTRACT,
            ),
            OpaqueCodecRegistryError::SemanticNameMismatch {
                opaque_type: OPAQUE_TYPE,
            },
        ),
        (
            opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, "orna.std.value.wrong-token@1"),
            OpaqueCodecRegistryError::ContractMismatch {
                opaque_type: OPAQUE_TYPE,
            },
        ),
    ] {
        assert_eq!(
            OpaqueCodecRegistry::new(standard, [registration]).unwrap_err(),
            expected
        );
    }

    let expanded_standard = verified_standard_with_value_types(vec![
        standard_boolean_definition(),
        opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
        opaque_definition(
            OTHER_OPAQUE_TYPE,
            ["std", "types", "other_token"],
            "orna.std.value.other-token@1",
        ),
    ]);
    assert_eq!(
        OpaqueCodecRegistry::new(&expanded_standard, [accepted]).unwrap_err(),
        OpaqueCodecRegistryError::UnregisteredOpaqueDefinition {
            opaque_type: OTHER_OPAQUE_TYPE,
        }
    );
}

#[test]
fn opaque_values_require_the_same_active_standard_and_exact_payload() {
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

    for position in 0..16 {
        for byte in u8::MIN..=u8::MAX {
            let mut payload = [0; 16];
            payload[position] = byte;
            let value = OpaqueValue::new(&active, &registry, OPAQUE_TYPE, payload).unwrap();
            assert_eq!(value.opaque_type(), OPAQUE_TYPE);
            assert_eq!(value.canonical_payload(), payload);
            assert_eq!(
                RuntimeValue::Opaque(value.clone()).runtime_type(),
                RuntimeType::Flat(ResolvedType::value(OPAQUE_TYPE))
            );
            assert_eq!(value, value.clone());
        }
    }

    for length in (0..=32).filter(|length| *length != 16) {
        assert_eq!(
            OpaqueValue::new(&active, &registry, OPAQUE_TYPE, vec![0; length]),
            Err(OpaqueValueError::WrongPayloadLength {
                opaque_type: OPAQUE_TYPE,
                expected: 16,
                actual: length,
            })
        );
    }
    assert_eq!(
        OpaqueValue::new(&active, &registry, OTHER_OPAQUE_TYPE, [0; 16]),
        Err(OpaqueValueError::UnregisteredType {
            opaque_type: OTHER_OPAQUE_TYPE,
        })
    );

    let stale = active_record_revision_with_opaque_contract(
        TypeId::from_bytes([0x4b; 16]),
        "orna.std.value.opaque-token@2",
    );
    assert_eq!(
        OpaqueValue::new(&stale, &registry, OPAQUE_TYPE, [0; 16]),
        Err(OpaqueValueError::ActiveStandardMismatch)
    );

    let value = OpaqueValue::new(&active, &registry, OPAQUE_TYPE, [0; 16]).unwrap();
    let runtime_value = RuntimeValue::Opaque(value.clone());
    let parameter = ParameterId::from_bytes([0x4c; 16]);
    assert_eq!(
        FunctionArgument::new(parameter, runtime_value.clone())
            .unwrap()
            .value(),
        &runtime_value
    );
    assert_eq!(
        ResultRows::new(
            [ResultColumn::new("opaque", ResolvedType::value(OPAQUE_TYPE), false).unwrap()],
            [ResultRow::new([runtime_value])],
        ),
        Err(ResultRowsError::OpaqueValueNotAccepted {
            row: 0,
            column: 0,
            opaque_type: OPAQUE_TYPE,
        })
    );
}

#[test]
fn action_frame_rejects_zero_target_revision_call_site_result_and_parameter_identities() {
    let mut integer = b"ORV3".to_vec();
    integer.push(0x03);
    integer.extend_from_slice(&[0x12; 16]);
    integer.extend_from_slice(&4_u32.to_be_bytes());
    integer.extend_from_slice(&7_i32.to_be_bytes());

    let mut body = vec![ACTION_DOMAIN_CLIENT];
    for byte in 0x21..=0x25 {
        body.extend_from_slice(&[byte; ACTION_IDENTITY_BYTES]);
    }
    body.extend_from_slice(&1_u32.to_be_bytes());
    body.extend_from_slice(&[0x31; ACTION_IDENTITY_BYTES]);
    body.extend_from_slice(&(integer.len() as u32).to_be_bytes());
    body.extend_from_slice(&integer);
    let mut payload = b"ORNA-ACTION/1 ".to_vec();
    payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
    payload.extend_from_slice(&body);
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = OpaqueCodecRegistry::new(
        standard,
        [OpaqueCodecRegistration::length_prefixed_action(
            OPAQUE_TYPE,
            QualifiedSemanticName::new(OPAQUE_NAME).unwrap(),
            OPAQUE_CONTRACT,
            "ORNA-ACTION/1 ",
        )
        .unwrap()],
    )
    .unwrap();

    OpaqueValue::new(&active, &registry, OPAQUE_TYPE, &payload)
        .expect("valid action identities are accepted");
    for offset in [1, 17, 33, 49, 65, 85] {
        let mut corrupted = payload.clone();
        let body_offset = b"ORNA-ACTION/1 ".len() + 4;
        corrupted[body_offset + offset..body_offset + offset + ACTION_IDENTITY_BYTES].fill(0);
        assert!(matches!(
            OpaqueValue::new(&active, &registry, OPAQUE_TYPE, &corrupted),
            Err(OpaqueValueError::InvalidActionFrame { .. })
        ));
    }
}

#[test]
fn ui_value_codec_enforces_closed_canonical_shape_after_framing() {
    const UI_TYPE: TypeId = TypeId::from_bytes([0x55; 16]);
    const UI_NAME: [&str; 3] = ["std", "ui", "ui"];
    const UI_CONTRACT: &str = "orna.std.value.ui@1";
    const UI_MAGIC: &str = "ORNA-UI/1 ";

    let active = active_record_revision_with_standard(
        RECORD_TYPE,
        verified_standard_with_value_types_and_schemas(
            vec![
                standard_boolean_definition(),
                opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                opaque_definition(UI_TYPE, UI_NAME, UI_CONTRACT),
            ],
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x56; 16]),
                QualifiedSemanticName::new(["std", "ui"]).unwrap(),
            )],
        ),
    );
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = OpaqueCodecRegistry::new(
        standard,
        [
            opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
            OpaqueCodecRegistration::length_prefixed_canonical_json(
                UI_TYPE,
                QualifiedSemanticName::new(UI_NAME).unwrap(),
                UI_CONTRACT,
                UI_MAGIC,
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let frame = |body: &[u8]| {
        let mut payload = Vec::from(UI_MAGIC.as_bytes());
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(body);
        payload
    };

    for body in [
        br#"{"kind":"empty"}"#.as_slice(),
        br#"{"children":[{"kind":"empty"}],"kind":"fragment"}"#.as_slice(),
        br#"{"actions":{"activate":{"action_id":"activate","debug_kind":null,"input_type":"std.ui.event","trace":true}},"call_site_id":null,"contract":{"id":"std.ui.window@1","name":"std.ui.window","version":"1.0"},"function_instance_id":"fn-1","key":{"id":1},"kind":"node","properties":{"title":{"type":"std.text","value":"Hello"}},"slots":{"content":[{"kind":"empty"}]},"source_origin":{"end":2,"source_unit_id":"unit-1","start":1}}"#.as_slice(),
    ] {
        let payload = frame(body);
        let value = OpaqueValue::new(&active, &registry, UI_TYPE, &payload)
            .expect("the closed canonical UI shape constructs");
        assert_eq!(value.canonical_payload(), payload.as_slice());
    }

    for body in [
        br#"{"kind":"not-a-ui-kind"}"#.as_slice(),
        br#"{"actions":{},"contract":{"id":"std.ui.window@1","name":"std.ui.window","version":"1.0"},"kind":"node","properties":{},"slots":{},"unknown":null}"#.as_slice(),
        br#"{"children":[{"kind":"not-a-ui-kind"}],"kind":"fragment"}"#.as_slice(),
    ] {
        assert_eq!(
            OpaqueValue::new(&active, &registry, UI_TYPE, frame(body)),
            Err(OpaqueValueError::InvalidJsonBody { opaque_type: UI_TYPE })
        );
    }
    let mut deep = serde_json::json!({"kind": "empty"});
    for _ in 0..40 {
        deep = serde_json::json!({"children": [deep], "kind": "fragment"});
    }
    let deep_body = serde_json::to_vec(&deep).unwrap();
    let deep_value = OpaqueValue::new(&active, &registry, UI_TYPE, frame(&deep_body))
        .expect("schema-valid UI values do not have an arbitrary depth limit");
    assert_eq!(deep_value.canonical_payload(), frame(&deep_body));
}

#[test]
fn framed_codec_constructors_reject_invalid_magic_prefixes() {
    let name = ["std", "terminal", "document"];
    for magic in [
        "",
        "ORNA-TERMINAL-DOCUMENT/1 \u{00e9}",
        "x".repeat(65).as_str(),
    ] {
        assert_eq!(
            OpaqueCodecRegistration::length_prefixed_utf8(
                OPAQUE_TYPE,
                QualifiedSemanticName::new(name).unwrap(),
                OPAQUE_CONTRACT,
                magic,
            )
            .unwrap_err(),
            OpaqueCodecRegistryError::InvalidMagic {
                opaque_type: OPAQUE_TYPE,
            }
        );
        assert_eq!(
            OpaqueCodecRegistration::media_type_framed(
                OPAQUE_TYPE,
                QualifiedSemanticName::new(name).unwrap(),
                OPAQUE_CONTRACT,
                magic,
            )
            .unwrap_err(),
            OpaqueCodecRegistryError::InvalidMagic {
                opaque_type: OPAQUE_TYPE,
            }
        );
        assert_eq!(
            OpaqueCodecRegistration::length_prefixed_canonical_json(
                OPAQUE_TYPE,
                QualifiedSemanticName::new(name).unwrap(),
                OPAQUE_CONTRACT,
                magic,
            )
            .unwrap_err(),
            OpaqueCodecRegistryError::InvalidMagic {
                opaque_type: OPAQUE_TYPE,
            }
        );
    }
    assert!(
        OpaqueCodecRegistration::length_prefixed_utf8(
            OPAQUE_TYPE,
            QualifiedSemanticName::new(name).unwrap(),
            OPAQUE_CONTRACT,
            " ",
        )
        .is_ok()
    );
}

#[test]
fn terminal_document_codec_enforces_canonical_text_payloads() {
    const DOCUMENT_TYPE: TypeId = TypeId::from_bytes([0x4d; 16]);
    const DOCUMENT_MAGIC: &str = "ORNA-TERMINAL-DOCUMENT/1 ";
    const DOCUMENT_NAME: [&str; 3] = ["std", "terminal", "document"];
    const DOCUMENT_CONTRACT: &str = "orna.std.value.terminal-document@1";

    let active = active_record_revision_with_standard(
        RECORD_TYPE,
        verified_standard_with_value_types_and_schemas(
            vec![
                standard_boolean_definition(),
                opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                opaque_definition(DOCUMENT_TYPE, DOCUMENT_NAME, DOCUMENT_CONTRACT),
            ],
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x4f; 16]),
                QualifiedSemanticName::new(["std", "terminal"]).unwrap(),
            )],
        ),
    );
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = OpaqueCodecRegistry::new(
        standard,
        [
            opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
            OpaqueCodecRegistration::length_prefixed_utf8(
                DOCUMENT_TYPE,
                QualifiedSemanticName::new(DOCUMENT_NAME).unwrap(),
                DOCUMENT_CONTRACT,
                DOCUMENT_MAGIC,
            )
            .unwrap(),
        ],
    )
    .unwrap();

    let mut payload = Vec::from(DOCUMENT_MAGIC.as_bytes());
    payload.extend_from_slice(&6_u32.to_be_bytes());
    payload.extend_from_slice(b"hello\n");
    let value = OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &payload).unwrap();
    assert_eq!(value.opaque_type(), DOCUMENT_TYPE);
    assert_eq!(value.canonical_payload(), payload);

    let mut empty_body = Vec::from(DOCUMENT_MAGIC.as_bytes());
    empty_body.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &empty_body),
        Err(OpaqueValueError::InvalidDocumentBody {
            opaque_type: DOCUMENT_TYPE,
        })
    );
    let mut missing_final_newline = Vec::from(DOCUMENT_MAGIC.as_bytes());
    missing_final_newline.extend_from_slice(&5_u32.to_be_bytes());
    missing_final_newline.extend_from_slice(b"hello");
    assert_eq!(
        OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &missing_final_newline),
        Err(OpaqueValueError::InvalidDocumentBody {
            opaque_type: DOCUMENT_TYPE,
        })
    );

    for body in [
        b"\0\n".as_slice(),
        b"\t\n".as_slice(),
        b"\r\n".as_slice(),
        b"\x7f\n".as_slice(),
        "\u{0085}\n".as_bytes(),
    ] {
        let mut control_byte = Vec::from(DOCUMENT_MAGIC.as_bytes());
        control_byte.extend_from_slice(&(body.len() as u32).to_be_bytes());
        control_byte.extend_from_slice(body);
        assert_eq!(
            OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &control_byte),
            Err(OpaqueValueError::InvalidDocumentBody {
                opaque_type: DOCUMENT_TYPE,
            })
        );
    }

    let mut over_limit = Vec::from(DOCUMENT_MAGIC.as_bytes());
    over_limit.extend_from_slice(
        &u32::try_from(MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(
        OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &over_limit),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: DOCUMENT_TYPE,
        })
    );

    let bad_magic = b"WRONG-DOCUMENT/1 \0\0\0\0".to_vec();
    assert_eq!(
        OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &bad_magic),
        Err(OpaqueValueError::InvalidMagic {
            opaque_type: DOCUMENT_TYPE,
        })
    );

    let truncated = Vec::from(DOCUMENT_MAGIC.as_bytes());
    assert_eq!(
        OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &truncated),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: DOCUMENT_TYPE,
        })
    );

    let mut short_body = Vec::from(DOCUMENT_MAGIC.as_bytes());
    short_body.extend_from_slice(&5_u32.to_be_bytes());
    short_body.extend_from_slice(b"hi");
    assert_eq!(
        OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &short_body),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: DOCUMENT_TYPE,
        })
    );

    let mut invalid_utf8 = Vec::from(DOCUMENT_MAGIC.as_bytes());
    invalid_utf8.extend_from_slice(&2_u32.to_be_bytes());
    invalid_utf8.extend_from_slice(&[0xff, 0xfe]);
    assert_eq!(
        OpaqueValue::new(&active, &registry, DOCUMENT_TYPE, &invalid_utf8),
        Err(OpaqueValueError::InvalidUtf8Body {
            opaque_type: DOCUMENT_TYPE,
        })
    );
}

#[test]
fn canonical_json_codec_accepts_canonical_payload() {
    const JSON_TYPE: TypeId = TypeId::from_bytes([0x4f; 16]);
    const JSON_MAGIC: &str = "ORNA-JSON-VALUE/1 ";
    const JSON_NAME: [&str; 3] = ["std", "json", "value"];
    const JSON_CONTRACT: &str = "orna.std.value.json@1";

    let active = active_record_revision_with_standard(
        RECORD_TYPE,
        verified_standard_with_value_types_and_schemas(
            vec![
                standard_boolean_definition(),
                opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                opaque_definition(JSON_TYPE, JSON_NAME, JSON_CONTRACT),
            ],
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x60; 16]),
                QualifiedSemanticName::new(["std", "json"]).unwrap(),
            )],
        ),
    );
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = OpaqueCodecRegistry::new(
        standard,
        [
            opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
            OpaqueCodecRegistration::length_prefixed_canonical_json(
                JSON_TYPE,
                QualifiedSemanticName::new(JSON_NAME).unwrap(),
                JSON_CONTRACT,
                JSON_MAGIC,
            )
            .unwrap(),
        ],
    )
    .unwrap();

    let body = br#"{"a":[1,true],"z":"ok"}"#;
    let mut payload = Vec::from(JSON_MAGIC.as_bytes());
    payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
    payload.extend_from_slice(body);
    let value = OpaqueValue::new(&active, &registry, JSON_TYPE, &payload).unwrap();
    assert_eq!(value.opaque_type(), JSON_TYPE);
    assert_eq!(value.canonical_payload(), payload);
}

#[test]
fn canonical_json_codec_rejects_invalid_and_noncanonical_payloads() {
    const JSON_TYPE: TypeId = TypeId::from_bytes([0x4f; 16]);
    const JSON_MAGIC: &str = "ORNA-JSON-VALUE/1 ";
    const JSON_NAME: [&str; 3] = ["std", "json", "value"];
    const JSON_CONTRACT: &str = "orna.std.value.json@1";

    let active = active_record_revision_with_standard(
        RECORD_TYPE,
        verified_standard_with_value_types_and_schemas(
            vec![
                standard_boolean_definition(),
                opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                opaque_definition(JSON_TYPE, JSON_NAME, JSON_CONTRACT),
            ],
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x60; 16]),
                QualifiedSemanticName::new(["std", "json"]).unwrap(),
            )],
        ),
    );
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = OpaqueCodecRegistry::new(
        standard,
        [
            opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
            OpaqueCodecRegistration::length_prefixed_canonical_json(
                JSON_TYPE,
                QualifiedSemanticName::new(JSON_NAME).unwrap(),
                JSON_CONTRACT,
                JSON_MAGIC,
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let frame = |body: &[u8]| {
        let mut payload = Vec::from(JSON_MAGIC.as_bytes());
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(body);
        payload
    };
    let reject =
        |payload: &[u8]| OpaqueValue::new(&active, &registry, JSON_TYPE, payload).unwrap_err();

    assert_eq!(
        reject(b"WRONG-JSON-VALUE/1 \0\0\0\0null"),
        OpaqueValueError::InvalidMagic {
            opaque_type: JSON_TYPE,
        }
    );
    assert_eq!(
        reject(JSON_MAGIC.as_bytes()),
        OpaqueValueError::InvalidFrameLength {
            opaque_type: JSON_TYPE,
        }
    );

    let mut short_body = frame(br#"null"#);
    short_body.pop();
    assert_eq!(
        reject(&short_body),
        OpaqueValueError::InvalidFrameLength {
            opaque_type: JSON_TYPE,
        }
    );
    let mut trailing = frame(br#"null"#);
    trailing.push(0);
    assert_eq!(
        reject(&trailing),
        OpaqueValueError::InvalidFrameLength {
            opaque_type: JSON_TYPE,
        }
    );
    let invalid_utf8 = frame(&[0xff]);
    assert_eq!(
        reject(&invalid_utf8),
        OpaqueValueError::InvalidUtf8Body {
            opaque_type: JSON_TYPE,
        }
    );

    for body in [
        br#"{"a":}"#.as_slice(),
        br#" null"#.as_slice(),
        br#"{"z":1,"a":2}"#.as_slice(),
        br#"{"a":1,"a":1}"#.as_slice(),
        br#"1e0"#.as_slice(),
    ] {
        assert_eq!(
            reject(&frame(body)),
            OpaqueValueError::InvalidJsonBody {
                opaque_type: JSON_TYPE,
            }
        );
    }

    let mut oversized = Vec::from(JSON_MAGIC.as_bytes());
    oversized.extend_from_slice(
        &u32::try_from(MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(
        reject(&oversized),
        OpaqueValueError::InvalidFrameLength {
            opaque_type: JSON_TYPE,
        }
    );
}

#[test]
fn framed_codecs_validate_media_type_payloads() {
    const BYTE_STREAM_TYPE: TypeId = TypeId::from_bytes([0x4e; 16]);
    const BYTE_STREAM_MAGIC: &str = "ORNA-BYTE-STREAM/1 ";
    const BYTE_STREAM_NAME: [&str; 3] = ["std", "io", "bytestream"];
    const BYTE_STREAM_CONTRACT: &str = "orna.std.value.byte-stream@1";

    let active = active_record_revision_with_standard(
        RECORD_TYPE,
        verified_standard_with_value_types_and_schemas(
            vec![
                standard_boolean_definition(),
                opaque_definition(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
                opaque_definition(BYTE_STREAM_TYPE, BYTE_STREAM_NAME, BYTE_STREAM_CONTRACT),
            ],
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x4f; 16]),
                QualifiedSemanticName::new(["std", "io"]).unwrap(),
            )],
        ),
    );
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = OpaqueCodecRegistry::new(
        standard,
        [
            opaque_registration(OPAQUE_TYPE, OPAQUE_NAME, OPAQUE_CONTRACT),
            OpaqueCodecRegistration::media_type_framed(
                BYTE_STREAM_TYPE,
                QualifiedSemanticName::new(BYTE_STREAM_NAME).unwrap(),
                BYTE_STREAM_CONTRACT,
                BYTE_STREAM_MAGIC,
            )
            .unwrap(),
        ],
    )
    .unwrap();

    let mut payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    let media_type = b"application/json";
    payload.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
    payload.extend_from_slice(media_type);
    payload.extend_from_slice(&2_u32.to_be_bytes());
    payload.extend_from_slice(b"{}");
    let value = OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &payload).unwrap();
    assert_eq!(value.opaque_type(), BYTE_STREAM_TYPE);
    assert_eq!(value.canonical_payload(), payload);

    let mut empty_media_type = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    empty_media_type.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &empty_media_type),
        Err(OpaqueValueError::InvalidMediaType {
            opaque_type: BYTE_STREAM_TYPE,
        })
    );

    let mut truncated = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    truncated.extend_from_slice(&3_u32.to_be_bytes());
    truncated.extend_from_slice(b"abc");
    truncated.extend_from_slice(&5_u32.to_be_bytes());
    truncated.extend_from_slice(b"hi");
    assert_eq!(
        OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &truncated),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: BYTE_STREAM_TYPE,
        })
    );

    let bad_magic = b"WRONG-STREAM/1 \0\0\0\0".to_vec();
    assert_eq!(
        OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &bad_magic),
        Err(OpaqueValueError::InvalidMagic {
            opaque_type: BYTE_STREAM_TYPE,
        })
    );
    let mut over_limit = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    over_limit.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
    over_limit.extend_from_slice(media_type);
    over_limit.extend_from_slice(
        &u32::try_from(MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1)
            .unwrap()
            .to_be_bytes(),
    );
    over_limit.extend(std::iter::repeat_n(
        0_u8,
        MAX_OPAQUE_CODEC_PAYLOAD_LENGTH + 1,
    ));
    assert_eq!(
        OpaqueValue::new(&active, &registry, BYTE_STREAM_TYPE, &over_limit),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: BYTE_STREAM_TYPE,
        })
    );
}
