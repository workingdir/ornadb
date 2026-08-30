use super::*;

#[test]
fn standard_json_encode_executes_and_returns_the_framed_byte_stream() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = json_encode_function(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    );
    let revision = json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_PARAMETER_ID);
    let argument = json_encode_argument(
        STD_JSON_ENCODE_PARAMETER_ID,
        RuntimeValue::Text("hello".to_owned()),
    );
    let RuntimeValue::Opaque(value) =
        execute_standard_json_encode(&function, &revision, &[argument], &active, &registry)
            .expect("the exact standard artifact must execute")
    else {
        panic!("the json-encode presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    );
    let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    expected.extend_from_slice(&16_u32.to_be_bytes());
    expected.extend_from_slice(b"application/json");
    expected.extend_from_slice(&7_u32.to_be_bytes());
    expected.extend_from_slice(b"\"hello\"");
    assert_eq!(value.canonical_payload(), expected);
}

#[test]
fn standard_json_encode_dispatches_without_function_name_or_id_matching() {
    // A different function identity, revision identity, and name with the
    // same closed artifact shape executes identically: the engine
    // dispatches only on artifact kind, format, and version, then
    // validates the pinned signature and decodes the artifact.
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let other_function = FunctionId::from_bytes([0x41; 16]);
    let other_revision = FunctionRevisionId::from_bytes([0x43; 16]);
    let function = FunctionDefinition::new(
        other_function,
        name(&["other", "encode"]),
        FunctionDomain::Server,
        vec![json_encode_parameter(STD_JSON_ENCODE_PARAMETER_ID)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        other_revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let revision = json_encode_revision(other_function, STD_JSON_ENCODE_PARAMETER_ID);
    let argument = json_encode_argument(STD_JSON_ENCODE_PARAMETER_ID, RuntimeValue::Integer(3));
    let RuntimeValue::Opaque(value) =
        execute_standard_json_encode(&function, &revision, &[argument], &active, &registry)
            .expect("the same artifact shape must execute identically")
    else {
        panic!("the json-encode presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    );
    let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    expected.extend_from_slice(&16_u32.to_be_bytes());
    expected.extend_from_slice(b"application/json");
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.extend_from_slice(b"3");
    assert_eq!(value.canonical_payload(), expected);
}

#[test]
fn json_encoding_converts_each_scalar_and_reference_form_without_loss() {
    let standard = presenter_standard();
    let active = presenter_active(&standard);

    assert_eq!(
        encode_json_value(
            &active,
            &RuntimeValue::null(ResolvedType::scalar(StandardScalar::Integer))
                .expect("a typed INTEGER null is valid"),
        )
        .expect("a null encodes"),
        serde_json::json!(null)
    );
    assert_eq!(
        encode_json_value(&active, &RuntimeValue::Boolean(true)).expect("a boolean encodes"),
        serde_json::json!(true)
    );
    assert_eq!(
        encode_json_value(&active, &RuntimeValue::Integer(-41)).expect("an integer encodes"),
        serde_json::json!(-41)
    );
    assert_eq!(
        encode_json_value(&active, &RuntimeValue::BigInt(i64::MAX)).expect("a bigint encodes"),
        serde_json::json!(i64::MAX)
    );
    assert_eq!(
        encode_json_value(
            &active,
            &RuntimeValue::Float(RuntimeFloat::new(1.5).expect("1.5 is finite")),
        )
        .expect("a float encodes"),
        serde_json::json!(1.5)
    );
    assert_eq!(
        encode_json_value(&active, &RuntimeValue::Text("a\"b\\c\n".to_owned()))
            .expect("text encodes"),
        serde_json::json!("a\"b\\c\n")
    );
    assert_eq!(
        encode_json_value(&active, &RuntimeValue::Bytes(vec![0x00, 0xff, 0x10]))
            .expect("bytes encode as base64"),
        serde_json::json!("AP8Q")
    );

    let object = ObjectId::from_bytes([0x55; 16]);
    assert_eq!(
        encode_json_value(
            &active,
            &RuntimeValue::Reference {
                target: PRESENTER_OBJECT_TYPE,
                object,
            },
        )
        .expect("a reference encodes"),
        serde_json::json!({
            "$ref": format!("orna://app.item/{}", object.canonical()),
            "$type": "app.item",
        })
    );
}

#[test]
fn json_encoding_converts_lists_and_maps_without_loss() {
    let standard = presenter_standard();
    let active = presenter_active(&standard);

    let integer = TypeDescriptor::named(orna_standard::INTEGER_TYPE_ID);
    let list = RuntimeValue::list(
        &active,
        TypeDescriptor::list(integer.clone()).expect("a list descriptor is valid"),
        vec![
            RuntimeValue::Integer(1),
            RuntimeValue::Integer(2),
            RuntimeValue::Integer(3),
        ],
    )
    .expect("the integer list is valid");
    assert_eq!(
        encode_json_value(&active, &list).expect("a list encodes"),
        serde_json::json!([1, 2, 3])
    );

    let map = RuntimeValue::map(
        &active,
        TypeDescriptor::map(integer.clone(), integer.clone()).expect("a map descriptor is valid"),
        vec![
            (RuntimeValue::Integer(2), RuntimeValue::Integer(20)),
            (RuntimeValue::Integer(1), RuntimeValue::Integer(10)),
        ],
    )
    .expect("the integer map is valid");
    assert_eq!(
        encode_json_value(&active, &map).expect("a map encodes"),
        serde_json::json!({ "1": 10, "2": 20 })
    );

    let nested = RuntimeValue::list(
        &active,
        TypeDescriptor::list(TypeDescriptor::list(integer).expect("a list descriptor is valid"))
            .expect("a list descriptor is valid"),
        vec![list],
    )
    .expect("the nested list is valid");
    assert_eq!(
        encode_json_value(&active, &nested).expect("a nested list encodes"),
        serde_json::json!([[1, 2, 3]])
    );
}

#[test]
fn json_encoding_accepts_std_json_value_without_reencoding_loss() {
    let standard = presenter_v5_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V5 opaque codecs register");
    let active = presenter_active(&standard);
    let body = br#"{"items":[1,2],"ok":true}"#;
    let mut payload = Vec::from(JSON_MAGIC.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(body.len())
            .expect("the JSON body length fits the canonical frame")
            .to_be_bytes(),
    );
    payload.extend_from_slice(body);
    let value = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, payload)
            .expect("the canonical std.json.Value payload constructs"),
    );

    assert_eq!(
        encode_json_value(&active, &value).expect("std.json.Value encodes"),
        serde_json::json!({"items": [1, 2], "ok": true})
    );
}

#[test]
fn json_encoding_rejects_every_non_lossless_runtime_form() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);

    let enum_value = RuntimeValue::Enum(
        EnumValue::new(active.catalogue(), PRESENTER_ENUM_TYPE, "lead")
            .expect("the enum label is declared"),
    );
    assert_presenter_conversion_rule(&active, enum_value, "ENUM");

    let record_value = RuntimeValue::Record(
        RecordValue::new(
            &active,
            PRESENTER_RECORD_TYPE,
            vec![
                ("x".to_owned(), RuntimeValue::Integer(1)),
                ("y".to_owned(), RuntimeValue::Text("a".to_owned())),
            ],
        )
        .expect("the record value is valid"),
    );
    assert_presenter_conversion_rule(&active, record_value, "RECORD");

    let mut byte_stream_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    byte_stream_payload.extend_from_slice(&16_u32.to_be_bytes());
    byte_stream_payload.extend_from_slice(b"application/json");
    byte_stream_payload.extend_from_slice(&2_u32.to_be_bytes());
    byte_stream_payload.extend_from_slice(b"{}");
    let opaque_value = RuntimeValue::Opaque(
        OpaqueValue::new(
            &active,
            &registry,
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            &byte_stream_payload,
        )
        .expect("the byte-stream payload constructs"),
    );
    assert_presenter_conversion_rule(&active, opaque_value, "OPAQUE");

    let option_value = RuntimeValue::option(
        &active,
        TypeDescriptor::option(TypeDescriptor::named(orna_standard::INTEGER_TYPE_ID))
            .expect("an option descriptor is valid"),
        Some(RuntimeValue::Integer(1)),
    )
    .expect("the option value is valid");
    assert_presenter_conversion_rule(&active, option_value, "OPTION");

    let carrier = RuntimeValue::InvokeValue(
        InvokeValue::new(RuntimeValue::Integer(1)).expect("the invoke value is valid"),
    );
    assert_presenter_conversion_rule(&active, carrier, "invocation carrier");

    let foreign_reference = RuntimeValue::Reference {
        target: TypeId::from_bytes([0x61; 16]),
        object: ObjectId::from_bytes([0x62; 16]),
    };
    assert_presenter_conversion_rule(&active, foreign_reference, "outside the active catalogue");
}

#[test]
fn standard_json_encode_rejects_wrong_kind_format_and_version() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = json_encode_function(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    );
    let parameter = STD_JSON_ENCODE_PARAMETER_ID;
    let argument = || json_encode_argument(parameter, RuntimeValue::Integer(1));

    let wrong_kind = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Client,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            json_encode_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_json_encode(&function, &wrong_kind, &[argument()], &active, &registry),
        function.id(),
        "current revision must contain a SERVER artifact",
    );

    let wrong_format = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_parameter_echo::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            json_encode_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_json_encode(&function, &wrong_format, &[argument()], &active, &registry),
        function.id(),
        "current SERVER artifact must use orna.server-json-encode",
    );

    let wrong_version = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION + 1,
            json_encode_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_json_encode(&function, &wrong_version, &[argument()], &active, &registry),
        function.id(),
        "current SERVER artifact must use orna.server-json-encode version 1",
    );

    let wrong_language = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        "orna.language/9",
        json_encode_artifact(parameter),
    );
    assert_presenter_artifact_rule(
        execute_standard_json_encode(
            &function,
            &wrong_language,
            &[argument()],
            &active,
            &registry,
        ),
        function.id(),
        "current SERVER revision must use the json-encode language version",
    );

    assert_eq!(
        execute_standard_json_encode(
            &function,
            &json_encode_revision(function.id(), parameter),
            &[argument()],
            &active,
            &registry
        )
        .expect("the exact artifact must execute"),
        RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                frame_byte_stream(b"application/json", b"1"),
            )
            .expect("the framed byte stream constructs"),
        )
    );
}

#[test]
fn standard_json_encode_artifacts_reject_each_decode_deviation() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = json_encode_function(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    );
    let parameter = STD_JSON_ENCODE_PARAMETER_ID;
    let argument = || json_encode_argument(parameter, RuntimeValue::Integer(1));

    let mut invalid_magic = json_encode_payload(parameter);
    invalid_magic[0] = b'X';
    let revision = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            invalid_magic,
        ),
    );
    assert_json_encode_decode_rule(
        execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
        JsonEncodePlanError::InvalidMagic,
    );

    let other_parameter = ParameterId::from_bytes([0x51; 16]);
    let revision = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            json_encode_payload(other_parameter),
        ),
    );
    assert_json_encode_decode_rule(
        execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
        JsonEncodePlanError::UnexpectedParameter {
            actual: other_parameter,
            expected: parameter,
        },
    );

    let other_type = orna_standard::BIGINT_TYPE_ID;
    let revision = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            JsonEncodePlan::new(parameter, other_type)
                .expect("any identities form a valid json-encode model")
                .encode()
                .expect("the canonical json-encode model encodes"),
        ),
    );
    assert_json_encode_decode_rule(
        execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
        JsonEncodePlanError::UnexpectedType {
            actual: other_type,
            expected: STD_JSON_VALUE_TYPE_ID,
        },
    );

    let truncated = json_encode_payload(parameter)[..40].to_vec();
    let revision = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            truncated,
        ),
    );
    assert_json_encode_decode_rule(
        execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
        JsonEncodePlanError::Truncated,
    );

    let mut trailing = json_encode_payload(parameter);
    trailing.push(0);
    let revision = presenter_revision(
        function.id(),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            trailing,
        ),
    );
    assert_json_encode_decode_rule(
        execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
        JsonEncodePlanError::TrailingBytes,
    );
}

#[test]
fn standard_json_encode_signature_rejects_each_shape_deviation() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let parameter = STD_JSON_ENCODE_PARAMETER_ID;
    let revision = json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, parameter);
    let argument = || json_encode_argument(parameter, RuntimeValue::Integer(1));
    let run = |function: &FunctionDefinition| {
        execute_standard_json_encode(function, &revision, &[argument()], &active, &registry)
    };

    let client = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Client,
        vec![json_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_presenter_domain_rule(run(&client));

    let mut missing = json_encode_function(
        STD_JSON_ENCODE_FUNCTION_ID,
        parameter,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    );
    missing = FunctionDefinition::new(
        missing.id(),
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&missing),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard json-encode presenters must declare exactly one required non-null std.json.Value parameter",
    );

    let defaulted = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter,
            "p_value",
            0,
            ResolvedType::named(STD_JSON_VALUE_TYPE_ID),
            Some(ExpressionId::from_bytes([0x72; 16])),
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&defaulted),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard json-encode presenters must declare exactly one required non-null std.json.Value parameter",
    );

    let rows_result = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![json_encode_parameter(parameter)],
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
        )]),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&rows_result),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard json-encode presenters must return a single std.io.ByteStream value",
    );

    let wrong_parameter_type = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter,
            "p_value",
            0,
            ResolvedType::named(orna_standard::BIGINT_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&wrong_parameter_type),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard json-encode presenters must declare one std.json.Value parameter and one std.io.ByteStream result",
    );

    let wrong_result_type = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![json_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&wrong_result_type),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard json-encode presenters must declare one std.json.Value parameter and one std.io.ByteStream result",
    );

    let definer = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![json_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Definer,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&definer),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard presenter functions must use INVOKER security",
    );

    let manual = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![json_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&manual),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard presenter functions must use READ ONLY transactions",
    );

    let volatile = FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![json_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Immutable,
    );
    assert_signature_rule(
        run(&volatile),
        STD_JSON_ENCODE_FUNCTION_ID,
        "standard presenter functions must use STABLE volatility",
    );

    // The exact pinned shape still executes after every rejection.
    assert_eq!(
        execute_standard_json_encode(
            &json_encode_function(
                STD_JSON_ENCODE_FUNCTION_ID,
                parameter,
                STD_JSON_ENCODE_FUNCTION_REVISION_ID
            ),
            &revision,
            &[argument()],
            &active,
            &registry,
        )
        .expect("the pinned shape must execute"),
        RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                frame_byte_stream(b"application/json", b"1"),
            )
            .expect("the framed byte stream constructs"),
        )
    );
}

#[test]
fn standard_json_encode_rejects_a_mismatched_opaque_codec_registry() {
    // The engine constructs its ByteStream against the codec registry of
    // the active verified standard. A registry bound to a different
    // standard snapshot (here the version-one registry, which registers
    // only the opaque-token codec) cannot validate the presented opaque
    // value and is rejected without producing a value.
    let standard = presenter_standard();
    let active = presenter_active(&standard);
    let version_one = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()
            .expect("the retained V1 standard source is valid"),
    )
    .expect("the retained V1 standard source verifies");
    let mismatched_registry = orna_standard::registered_opaque_codecs(&version_one)
        .expect("the V1 opaque codecs register");
    let function = json_encode_function(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    );
    let revision = json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_PARAMETER_ID);
    let argument = json_encode_argument(STD_JSON_ENCODE_PARAMETER_ID, RuntimeValue::Integer(1));
    assert_presenter_opaque_rule(execute_standard_json_encode(
        &function,
        &revision,
        &[argument],
        &active,
        &mismatched_registry,
    ));
}

#[test]
fn standard_json_encode_arguments_are_exact_complete_and_typed() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = json_encode_function(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_PARAMETER_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
    );
    let revision = json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_PARAMETER_ID);
    let parameter = STD_JSON_ENCODE_PARAMETER_ID;

    // Missing argument.
    assert_argument_rule(
        execute_standard_json_encode(&function, &revision, &[], &active, &registry),
        None,
        "standard json-encode calls require exactly one argument",
    );

    // Extra argument.
    let first = json_encode_argument(parameter, RuntimeValue::Integer(1));
    let second = json_encode_argument(parameter, RuntimeValue::Integer(2));
    assert_argument_rule(
        execute_standard_json_encode(&function, &revision, &[first, second], &active, &registry),
        None,
        "standard json-encode calls require exactly one argument",
    );

    // Argument bound to a different parameter identity.
    let other = ParameterId::from_bytes([0x46; 16]);
    let wrong = json_encode_argument(other, RuntimeValue::Integer(1));
    assert_argument_rule(
        execute_standard_json_encode(&function, &revision, &[wrong], &active, &registry),
        Some(other),
        "standard json-encode arguments must bind the pinned parameter identity",
    );

    // A typed null cannot cross the bound-argument boundary, so the engine
    // can never receive one: FunctionArgument::new rejects it.
    let null = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Integer))
        .expect("a typed INTEGER null is valid");
    assert!(matches!(
        FunctionArgument::new(parameter, null),
        Err(orna_core::value::FunctionArgumentError::NullValue {
            parameter: actual,
            ..
        }) if actual == parameter
    ));
}

#[test]
fn retained_table_target_uses_v8_v9_executables_and_legacy_compatibility() {
    let v8 = presenter_v8_standard();
    let active_v8 = presenter_active(&v8);
    let (function, revision) = retained_terminal_table_target(&active_v8)
        .expect("the V8 retained table target resolves")
        .expect("V8 must not use the compatibility target");
    let expected_function = v8
        .catalogue()
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .expect("the V8 standard catalogue contains present_table");
    let expected_revision = v8
        .executables()
        .iter()
        .find(|executable| executable.function() == STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .expect("the V8 standard retains the table executable")
        .revision();
    assert_eq!(function, expected_function);
    assert_eq!(revision, expected_revision);
    assert_eq!(revision.function(), function.id());
    assert_eq!(revision.id(), function.current_revision());
    assert_eq!(
        revision.artifact().format(),
        server_terminal_table::FORMAT_IDENTITY
    );
    assert_eq!(
        revision.artifact().version(),
        server_terminal_table::FORMAT_VERSION
    );

    let v9 = presenter_v9_standard();
    let active_v9 = presenter_active(&v9);
    let (function, revision) = retained_terminal_table_target(&active_v9)
        .expect("the V9 retained table target resolves")
        .expect("V9 must not use the compatibility target");
    let expected_function = v9
        .catalogue()
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .expect("the V9 standard catalogue contains present_table");
    let expected_revision = v9
        .executables()
        .iter()
        .find(|executable| executable.function() == STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .expect("the V9 standard retains the table executable")
        .revision();
    assert_eq!(function, expected_function);
    assert_eq!(revision, expected_revision);

    let v7 = presenter_standard();
    let active_v7 = presenter_active(&v7);
    assert!(
        retained_terminal_table_target(&active_v7)
            .expect("the legacy target lookup is closed")
            .is_none(),
        "V1-V7 must retain the explicit compatibility presenter path"
    );
}

#[test]
fn standard_terminal_table_executes_and_returns_the_framed_document() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = terminal_table_function(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
    );
    let revision = terminal_table_revision(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
    );
    let rows = ResultRows::new(
        [
            ResultColumn::new("id", ResolvedType::scalar(StandardScalar::Integer), false)
                .expect("the id column is valid"),
            ResultColumn::new(
                "name",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            )
            .expect("the name column is valid"),
        ],
        [
            ResultRow::new([
                RuntimeValue::Integer(1),
                RuntimeValue::Text("alpha".to_owned()),
            ]),
            ResultRow::new([
                RuntimeValue::Integer(2),
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject))
                    .expect("a typed TEXT null is valid"),
            ]),
        ],
    )
    .expect("the presenter rows are valid");
    let RuntimeValue::Opaque(value) =
        execute_standard_terminal_table(&function, &revision, &rows, &active, &registry)
            .expect("the exact standard artifact must execute")
    else {
        panic!("the terminal-table presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
    );
    let document = "id name\n-- -----\n1  alpha\n2  NULL\n(2 rows)\n";
    assert_eq!(value.canonical_payload(), frame_terminal_document(document));
}

#[test]
fn standard_terminal_table_dispatches_without_function_name_or_id_matching() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let other_function = FunctionId::from_bytes([0x41; 16]);
    let other_revision = FunctionRevisionId::from_bytes([0x43; 16]);
    let function = FunctionDefinition::new(
        other_function,
        name(&["other", "table"]),
        FunctionDomain::Server,
        vec![terminal_table_parameter(
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        other_revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let revision = terminal_table_revision(other_function, STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID);
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(3)])],
    )
    .expect("the presenter rows are valid");
    let RuntimeValue::Opaque(value) =
        execute_standard_terminal_table(&function, &revision, &rows, &active, &registry)
            .expect("the same artifact shape must execute identically")
    else {
        panic!("the terminal-table presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
    );
    assert_eq!(
        value.canonical_payload(),
        frame_terminal_document("value\n-----\n3\n(1 row)\n")
    );
}

#[test]
fn standard_csv_encode_dispatches_without_function_name_or_id_matching() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let other_function = FunctionId::from_bytes([0x42; 16]);
    let other_revision = FunctionRevisionId::from_bytes([0x44; 16]);
    let function = FunctionDefinition::new(
        other_function,
        name(&["other", "csv"]),
        FunctionDomain::Server,
        vec![csv_encode_parameter(STD_CSV_ENCODE_PARAMETER_ID)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        other_revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let revision = csv_encode_revision(other_function, STD_CSV_ENCODE_PARAMETER_ID);
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(3)])],
    )
    .expect("the presenter rows are valid");
    let RuntimeValue::Opaque(value) =
        execute_standard_csv_encode(&function, &revision, &rows, &active, &registry)
            .expect("the same artifact shape must execute identically")
    else {
        panic!("the csv-encode presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    );
    assert_eq!(
        value.canonical_payload(),
        frame_byte_stream(b"text/csv", b"value\n3\n")
    );
}

#[test]
fn sealed_output_csv_requirement_emits_the_byte_stream_in_the_final_value_batch() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let requirement = InvocationOutputRequirement::new(
        Some(String::from("csv")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the csv output requirement is valid");
    let presented = present_sealed_standard_output(
        &requirement,
        RuntimeValue::Integer(42),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect("the csv presenter must execute on the sealed canonical result");
    let RuntimeValue::Opaque(value) = &presented else {
        panic!("the csv presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    );
    let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    expected.extend_from_slice(&8_u32.to_be_bytes());
    expected.extend_from_slice(b"text/csv");
    expected.extend_from_slice(&10_u32.to_be_bytes());
    expected.extend_from_slice(b"result\n42\n");
    assert_eq!(value.canonical_payload(), expected);

    let principal = PrincipalId::from_bytes([0x65; 16]);
    let invocation = InvocationId::from_bytes([0x66; 16]);
    let events = crate::kernel::security::sealed_completed_events(principal, invocation, presented)
        .expect("the presented events are valid");
    let records = events.records();
    assert_eq!(records.len(), 3);
    match records[1].event().body() {
        InvocationEventBody::ValueBatch { values, .. } => {
            let [value] = values.as_slice() else {
                panic!("the final ValueBatch must carry exactly one value");
            };
            let RuntimeValue::Opaque(opaque) = value.value() else {
                panic!("the final ValueBatch must carry the presented opaque value");
            };
            assert_eq!(
                opaque.opaque_type(),
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
            );
            assert_eq!(opaque.canonical_payload(), expected);
        }
        other => panic!("expected a ValueBatch event, got {other:?}"),
    }
}

#[test]
fn sealed_output_json_requirement_emits_the_byte_stream_in_the_final_value_batch() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let requirement = InvocationOutputRequirement::new(
        Some(String::from("json")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the json output requirement is valid");
    let presented = present_sealed_standard_output(
        &requirement,
        RuntimeValue::Integer(42),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect("the json presenter must execute on the sealed canonical result");
    let RuntimeValue::Opaque(value) = &presented else {
        panic!("the json presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    );
    let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    expected.extend_from_slice(&16_u32.to_be_bytes());
    expected.extend_from_slice(b"application/json");
    expected.extend_from_slice(&2_u32.to_be_bytes());
    expected.extend_from_slice(b"42");
    assert_eq!(value.canonical_payload(), expected);

    let principal = PrincipalId::from_bytes([0x61; 16]);
    let invocation = InvocationId::from_bytes([0x62; 16]);
    let events = crate::kernel::security::sealed_completed_events(principal, invocation, presented)
        .expect("the presented events are valid");
    let records = events.records();
    assert_eq!(records.len(), 3);
    match records[1].event().body() {
        InvocationEventBody::ValueBatch { values, .. } => {
            let [value] = values.as_slice() else {
                panic!("the final ValueBatch must carry exactly one value");
            };
            let RuntimeValue::Opaque(opaque) = value.value() else {
                panic!("the final ValueBatch must carry the presented opaque value");
            };
            assert_eq!(
                opaque.opaque_type(),
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
            );
            assert_eq!(opaque.canonical_payload(), expected);
        }
        other => panic!("expected a ValueBatch event, got {other:?}"),
    }
}

#[test]
fn sealed_output_json_requirement_preserves_null_and_non_null_json_bytes() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let requirement = InvocationOutputRequirement::new(
        Some(String::from("json")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the json output requirement is valid");
    let typed_null = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Integer))
        .expect("a typed INTEGER null is valid");

    let presented = present_sealed_standard_output(
        &requirement,
        typed_null,
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect("the json presenter must encode the sealed typed null");
    let RuntimeValue::Opaque(value) = presented else {
        panic!("the json presenter must return one opaque value");
    };
    assert_eq!(
        value.canonical_payload(),
        frame_byte_stream(b"application/json", b"null")
    );

    let presented = present_sealed_standard_output(
        &requirement,
        RuntimeValue::Integer(42),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect("the json presenter must preserve the sealed non-null result");
    let RuntimeValue::Opaque(value) = presented else {
        panic!("the json presenter must return one opaque value");
    };
    assert_eq!(
        value.canonical_payload(),
        frame_byte_stream(b"application/json", b"42")
    );
}

#[test]
fn sealed_output_table_requirement_emits_the_terminal_document_in_the_final_value_batch() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let requirement = InvocationOutputRequirement::new(
        Some(String::from("table")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the table output requirement is valid");
    let presented = present_sealed_standard_output(
        &requirement,
        RuntimeValue::Integer(42),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect("the terminal-table presenter must execute on the sealed canonical result");
    let RuntimeValue::Opaque(value) = &presented else {
        panic!("the terminal-table presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
    );
    assert_eq!(
        value.canonical_payload(),
        frame_terminal_document("result\n------\n42\n(1 row)\n")
    );

    let principal = PrincipalId::from_bytes([0x63; 16]);
    let invocation = InvocationId::from_bytes([0x64; 16]);
    let events = crate::kernel::security::sealed_completed_events(principal, invocation, presented)
        .expect("the presented events are valid");
    let records = events.records();
    assert_eq!(records.len(), 3);
    match records[1].event().body() {
        InvocationEventBody::ValueBatch { values, .. } => {
            let [value] = values.as_slice() else {
                panic!("the final ValueBatch must carry exactly one value");
            };
            let RuntimeValue::Opaque(opaque) = value.value() else {
                panic!("the final ValueBatch must carry the presented opaque value");
            };
            assert_eq!(
                opaque.opaque_type(),
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
            );
            assert_eq!(
                opaque.canonical_payload(),
                frame_terminal_document("result\n------\n42\n(1 row)\n")
            );
        }
        other => panic!("expected a ValueBatch event, got {other:?}"),
    }
}

#[test]
fn sealed_rows_value_preserves_complete_shape_for_table_and_csv() {
    let standard = presenter_v8_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V8 opaque codecs register");
    let active = presenter_active(&standard);
    let rows = ResultRows::new(
        [
            ResultColumn::new("id", ResolvedType::scalar(StandardScalar::Integer), false)
                .expect("the id column is valid"),
            ResultColumn::new(
                "name",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            )
            .expect("the name column is valid"),
        ],
        [
            ResultRow::new([
                RuntimeValue::Integer(2),
                RuntimeValue::Text("beta".to_owned()),
            ]),
            ResultRow::new([
                RuntimeValue::Integer(1),
                RuntimeValue::Text("alpha".to_owned()),
            ]),
        ],
    )
    .expect("the multi-column result rows are valid");
    let value = orna_protocol::encode_rows_value(&active, &registry, &rows)
        .expect("the complete Rows value encodes");
    let RuntimeValue::Opaque(opaque) = &value else {
        panic!("Rows encoding must produce one opaque value");
    };
    assert_eq!(opaque.opaque_type(), STD_DATA_ROWS_TYPE_ID);

    let decoded = sealed_result_rows(value.clone(), &active, &registry)
        .expect("the complete Rows value decodes");
    assert_eq!(decoded, rows);

    let table = InvocationOutputRequirement::new(
        Some(String::from("table")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the table requirement is valid");
    let presented = present_sealed_standard_output(
        &table,
        value.clone(),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect("the table presenter accepts the complete Rows value");
    let RuntimeValue::Opaque(document) = presented else {
        panic!("the table presenter must return one opaque document");
    };
    assert_eq!(
        document.canonical_payload(),
        frame_terminal_document("id name\n-- -----\n2  beta\n1  alpha\n(2 rows)\n")
    );

    let csv = InvocationOutputRequirement::new(
        Some(String::from("csv")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the CSV requirement is valid");
    let presented =
        present_sealed_standard_output(&csv, value, &presenter_client_offer(), &active, &registry)
            .expect("the CSV presenter accepts the complete Rows value");
    let RuntimeValue::Opaque(stream) = presented else {
        panic!("the CSV presenter must return one opaque stream");
    };
    assert_eq!(
        stream.canonical_payload(),
        frame_byte_stream(b"text/csv", b"id,name\n2,beta\n1,alpha\n")
    );
}

#[test]
fn sealed_result_rows_preserves_scalar_synthetic_column() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let rows = sealed_result_rows(RuntimeValue::Integer(42), &active, &registry)
        .expect("scalar presentation retains its legacy wrapper");

    assert_eq!(rows.columns().len(), 1);
    assert_eq!(rows.columns()[0].name(), "result");
    assert_eq!(
        rows.columns()[0].resolved_type(),
        ResolvedType::scalar(StandardScalar::Integer)
    );
    assert_eq!(rows.rows(), &[ResultRow::new([RuntimeValue::Integer(42)])]);
}

#[test]
fn sealed_rows_zero_row_result_stays_one_value_batch_item() {
    let standard = presenter_v8_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V8 opaque codecs register");
    let active = presenter_active(&standard);
    let rows = ResultRows::new(
        [
            ResultColumn::new("id", ResolvedType::scalar(StandardScalar::Integer), false)
                .expect("the id column is valid"),
        ],
        std::iter::empty::<ResultRow>(),
    )
    .expect("the zero-row result shape is valid");
    let value = orna_protocol::encode_rows_value(&active, &registry, &rows)
        .expect("the zero-row Rows value encodes");
    let events = crate::kernel::security::sealed_completed_events(
        PrincipalId::from_bytes([0x67; 16]),
        InvocationId::from_bytes([0x68; 16]),
        value,
    )
    .expect("the zero-row Rows event batch is valid");

    assert_eq!(events.records().len(), 3);
    let InvocationEventBody::ValueBatch { values, .. } = events.records()[1].event().body() else {
        panic!("a zero-row Rows result must still emit a ValueBatch");
    };
    let [value] = values.as_slice() else {
        panic!("a zero-row Rows result must emit exactly one value");
    };
    let RuntimeValue::Opaque(opaque) = value.value() else {
        panic!("the ValueBatch item must be the Rows opaque value");
    };
    assert_eq!(opaque.opaque_type(), STD_DATA_ROWS_TYPE_ID);
}
#[test]
fn sealed_output_requires_matching_sink_descriptor_and_media_type() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let json = InvocationOutputRequirement::new(
        Some(String::from("json")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the json requirement is valid");
    let table = InvocationOutputRequirement::new(
        Some(String::from("table")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the table requirement is valid");
    let offer = |descriptor: TypeDescriptor, media_types: &[&str]| {
        let sink =
            InvocationSinkOffer::new(descriptor, media_types.iter().copied(), false, 0, None)
                .expect("the sink offer is valid");
        InvocationClientOffer::new(
            5,
            "en-GB",
            "Europe/London",
            [sink],
            [],
            1_024,
            0,
            None,
            None,
        )
        .expect("the client offer is valid")
    };

    let empty =
        InvocationClientOffer::new(5, "en-GB", "Europe/London", [], [], 1_024, 0, None, None)
            .expect("an empty client offer is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &json,
            RuntimeValue::Integer(42),
            &empty,
            &active,
            &registry
        ),
        Err(SealedPresentationError::NoPath)
    ));

    let wrong_descriptor = offer(
        TypeDescriptor::named(orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID),
        &["text/plain"],
    );
    assert!(matches!(
        present_sealed_standard_output(
            &json,
            RuntimeValue::Integer(42),
            &wrong_descriptor,
            &active,
            &registry,
        ),
        Err(SealedPresentationError::NoPath)
    ));

    let wrong_media = offer(
        TypeDescriptor::named(orna_standard::STD_IO_BYTE_STREAM_TYPE_ID),
        &["text/plain"],
    );
    assert!(matches!(
        present_sealed_standard_output(
            &json,
            RuntimeValue::Integer(42),
            &wrong_media,
            &active,
            &registry,
        ),
        Err(SealedPresentationError::NoPath)
    ));

    let matching_byte_stream = offer(
        TypeDescriptor::named(orna_standard::STD_IO_BYTE_STREAM_TYPE_ID),
        &["application/json"],
    );
    assert!(matches!(
        present_sealed_standard_output(
            &json,
            RuntimeValue::Integer(42),
            &matching_byte_stream,
            &active,
            &registry,
        ),
        Ok(RuntimeValue::Opaque(value))
            if value.opaque_type() == orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    ));

    let wildcard_byte_stream = offer(
        TypeDescriptor::named(orna_standard::STD_IO_BYTE_STREAM_TYPE_ID),
        &["application/octet-stream"],
    );
    assert!(matches!(
        present_sealed_standard_output(
            &json,
            RuntimeValue::Integer(42),
            &wildcard_byte_stream,
            &active,
            &registry,
        ),
        Ok(RuntimeValue::Opaque(value))
            if value.opaque_type() == orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    ));

    let matching_document = offer(
        TypeDescriptor::named(orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID),
        &["text/plain"],
    );
    assert!(matches!(
        present_sealed_standard_output(
            &table,
            RuntimeValue::Integer(42),
            &matching_document,
            &active,
            &registry,
        ),
        Ok(RuntimeValue::Opaque(value))
            if value.opaque_type() == orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
    ));
}

#[test]
fn sealed_output_media_type_requirement_resolves_to_the_json_presenter() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let requirement = InvocationOutputRequirement::new(
        None,
        Some(String::from("application/json")),
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the media-type output requirement is valid");
    let presented = present_sealed_standard_output(
        &requirement,
        RuntimeValue::Text("hello".to_owned()),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect("the media-type requirement must resolve to the json presenter");
    let RuntimeValue::Opaque(value) = &presented else {
        panic!("the json presenter must return one opaque value");
    };
    assert_eq!(
        value.opaque_type(),
        orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    );
    let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    expected.extend_from_slice(&16_u32.to_be_bytes());
    expected.extend_from_slice(b"application/json");
    expected.extend_from_slice(&7_u32.to_be_bytes());
    expected.extend_from_slice(b"\"hello\"");
    assert_eq!(value.canonical_payload(), expected);
}

#[test]
fn sealed_output_qualified_standard_json_resolves_before_presenter_selection() {
    let standard = presenter_v5_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let json_name = QualifiedSemanticName::new(["std", "json", "value"])
        .expect("the standard JSON value name is qualified");

    assert_eq!(
        resolve_sealed_presenter_type_name(&json_name, &active),
        Ok(STD_JSON_VALUE_TYPE_ID)
    );

    let requirement = InvocationOutputRequirement::new(
        None,
        None,
        Some(
            InvocationOutputTypeSelector::qualified_name(json_name.clone())
                .expect("the JSON selector is valid"),
        ),
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the JSON output requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &requirement,
            RuntimeValue::Integer(1),
            &presenter_client_offer(),
            &active,
            &registry,
        ),
        Ok(RuntimeValue::Opaque(value))
            if value.opaque_type() == orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    ));
}

#[test]
fn sealed_output_qualified_application_type_without_presenter_preserves_requested_name() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let requested = name(&["app", "stage"]);
    assert_eq!(
        resolve_sealed_presenter_type_name(&requested, &active),
        Ok(PRESENTER_ENUM_TYPE)
    );
    let requirement = InvocationOutputRequirement::new(
        None,
        None,
        Some(
            InvocationOutputTypeSelector::qualified_name(requested.clone())
                .expect("the application selector is valid"),
        ),
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the application type requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &requirement,
            RuntimeValue::Integer(1),
            &presenter_client_offer(),
            &active,
            &registry,
        ),
        Err(SealedPresentationError::OutputResolution(
            OutputResolutionError::UnresolvedTypeName { name }
        )) if name == requested.to_string()
    ));
}

#[test]
fn sealed_output_catalogue_collision_without_a_presenter_tie_stays_unresolved() {
    let standard = presenter_v5_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V5 opaque codecs register");
    let active = presenter_active_with_application_json_value_type(&standard);
    let requested = name(&["std", "json", "value"]);
    assert_eq!(
        resolve_sealed_presenter_type_name(&requested, &active),
        Err(OutputResolutionError::UnresolvedTypeName {
            name: requested.to_string(),
        })
    );
    let requirement = InvocationOutputRequirement::new(
        None,
        None,
        Some(
            InvocationOutputTypeSelector::qualified_name(requested.clone())
                .expect("the colliding selector is valid"),
        ),
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the colliding type requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &requirement,
            RuntimeValue::Integer(1),
            &presenter_client_offer(),
            &active,
            &registry,
        ),
        Err(SealedPresentationError::OutputResolution(
            OutputResolutionError::UnresolvedTypeName { name }
        )) if name == requested.to_string()
    ));
}

#[test]
fn sealed_output_streaming_requirement_respects_non_streaming_presenters() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let required = InvocationOutputRequirement::new(
        Some(String::from("json")),
        None,
        None,
        InvocationStreamingRequirement::Required,
    )
    .expect("the required streaming output is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &required,
            RuntimeValue::Integer(1),
            &presenter_client_offer(),
            &active,
            &registry,
        ),
        Err(SealedPresentationError::NoPath)
    ));

    // The accepted first slice has only non-streaming sealed entries, so
    // Forbidden is compatible while Required is deliberately closed.
    let forbidden = InvocationOutputRequirement::new(
        Some(String::from("json")),
        None,
        None,
        InvocationStreamingRequirement::Forbidden,
    )
    .expect("the forbidden streaming output is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &forbidden,
            RuntimeValue::Integer(1),
            &presenter_client_offer(),
            &active,
            &registry,
        ),
        Ok(RuntimeValue::Opaque(value))
            if value.opaque_type() == orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
    ));
}

#[test]
fn sealed_output_unresolved_requirement_failures_are_closed() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);

    let alias = InvocationOutputRequirement::new(
        Some(String::from("xml")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the alias requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(&alias, RuntimeValue::Integer(1), &presenter_client_offer(), &active, &registry),
        Err(SealedPresentationError::OutputResolution(
            OutputResolutionError::UnresolvedAlias { alias }
        )) if alias == "xml"
    ));

    let media = InvocationOutputRequirement::new(
        None,
        Some(String::from("application/xml")),
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the media requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(&media, RuntimeValue::Integer(1), &presenter_client_offer(), &active, &registry),
        Err(SealedPresentationError::OutputResolution(
            OutputResolutionError::UnresolvedMediaType { media_type }
        )) if media_type == "application/xml"
    ));

    let type_name = InvocationOutputRequirement::new(
        None,
        None,
        Some(
            InvocationOutputTypeSelector::qualified_name(
                QualifiedSemanticName::new(["std", "xml", "Value"]).expect("a qualified name"),
            )
            .expect("the type-name selector is valid"),
        ),
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the type-name requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &type_name,
            RuntimeValue::Integer(1),
            &presenter_client_offer(),
            &active,
            &registry
        ),
        Err(SealedPresentationError::OutputResolution(
            OutputResolutionError::UnresolvedTypeName { .. }
        ))
    ));

    // The retained V3 snapshot used by this fixture does not yet contain
    // the proposal-only std.data.Rows type, so the pinned lookup remains
    // explicitly unresolved rather than consulting an unpinned catalogue.
    let rows_name = InvocationOutputRequirement::new(
        None,
        None,
        Some(
            InvocationOutputTypeSelector::qualified_name(
                QualifiedSemanticName::new(["std", "data", "rows"]).expect("a qualified Rows name"),
            )
            .expect("the Rows selector is valid"),
        ),
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the Rows requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &rows_name,
            RuntimeValue::Integer(1),
            &presenter_client_offer(),
            &active,
            &registry
        ),
        Err(SealedPresentationError::OutputResolution(
            OutputResolutionError::UnresolvedTypeName { name }
        )) if name == "std.data.rows"
    ));

    let error = present_sealed_standard_output(
        &alias,
        RuntimeValue::Integer(1),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect_err("an unresolved alias is a closed output-resolution failure");
    assert_eq!(error.spec_code(), "ORNA0702");
    assert_eq!(error.exit_code(), 5);
}

#[test]
fn sealed_output_no_path_failures_are_closed() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);

    // An opaque canonical result has no path to the table sink: opaque
    // values cannot ride a ResultRows cell.
    let opaque = RuntimeValue::Opaque(
        OpaqueValue::new(
            &active,
            &registry,
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            frame_terminal_document("x\n-\nx\n(1 row)\n"),
        )
        .expect("the opaque test value is valid"),
    );
    let table = InvocationOutputRequirement::new(
        Some(String::from("table")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the table requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &table,
            opaque,
            &presenter_client_offer(),
            &active,
            &registry
        ),
        Err(SealedPresentationError::NoPath)
    ));

    // A record canonical result has no path to the json sink: records are
    // rejected by both the argument channel and the json conversion.
    let record = RuntimeValue::Record(
        RecordValue::new(
            &active,
            PRESENTER_RECORD_TYPE,
            [
                ("x".to_owned(), RuntimeValue::Integer(1)),
                ("y".to_owned(), RuntimeValue::Text("a".to_owned())),
            ],
        )
        .expect("the record test value is valid"),
    );
    let json = InvocationOutputRequirement::new(
        Some(String::from("json")),
        None,
        None,
        InvocationStreamingRequirement::Unspecified,
    )
    .expect("the json requirement is valid");
    assert!(matches!(
        present_sealed_standard_output(
            &json,
            record,
            &presenter_client_offer(),
            &active,
            &registry
        ),
        Err(SealedPresentationError::NoPath)
    ));

    let error = present_sealed_standard_output(
        &table,
        RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
                frame_terminal_document("x\n-\nx\n(1 row)\n"),
            )
            .expect("the opaque test value is valid"),
        ),
        &presenter_client_offer(),
        &active,
        &registry,
    )
    .expect_err("a result with no path to the offered sink is closed");
    assert_eq!(error.spec_code(), "ORNA0701");
    assert_eq!(error.exit_code(), 5);
}

#[test]
fn terminal_table_renders_each_cell_form_and_the_fixed_layout() {
    let standard = presenter_standard();
    let active = presenter_active(&standard);

    let status = ResultRows::new(
        [
            ResultColumn::new("b", ResolvedType::scalar(StandardScalar::Boolean), false)
                .expect("the boolean column is valid"),
            ResultColumn::new("n", ResolvedType::scalar(StandardScalar::BigInt), false)
                .expect("the bigint column is valid"),
            ResultColumn::new("f", ResolvedType::scalar(StandardScalar::Float), false)
                .expect("the float column is valid"),
            ResultColumn::new(
                "t",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            )
            .expect("the text column is valid"),
            ResultColumn::new(
                "x",
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                false,
            )
            .expect("the bytes column is valid"),
            ResultColumn::new("r", ResolvedType::reference(PRESENTER_OBJECT_TYPE), false)
                .expect("the reference column is valid"),
            ResultColumn::new("e", ResolvedType::named(PRESENTER_ENUM_TYPE), false)
                .expect("the enum column is valid"),
            ResultColumn::new("c", ResolvedType::named(PRESENTER_RECORD_TYPE), false)
                .expect("the record column is valid"),
        ],
        [ResultRow::new([
            RuntimeValue::Boolean(true),
            RuntimeValue::BigInt(-9_007_199_254_740_993),
            RuntimeValue::Float(RuntimeFloat::new(10.5).expect("10.5 is finite")),
            RuntimeValue::Text("héllo".to_owned()),
            RuntimeValue::Bytes(vec![0x00, 0xff]),
            RuntimeValue::Reference {
                target: PRESENTER_OBJECT_TYPE,
                object: ObjectId::from_bytes([0x55; 16]),
            },
            RuntimeValue::Enum(
                EnumValue::new(active.catalogue(), PRESENTER_ENUM_TYPE, "qualified")
                    .expect("the enum label is declared"),
            ),
            RuntimeValue::Record(
                RecordValue::new(
                    &active,
                    PRESENTER_RECORD_TYPE,
                    vec![
                        ("x".to_owned(), RuntimeValue::Integer(7)),
                        ("y".to_owned(), RuntimeValue::Text("z".to_owned())),
                    ],
                )
                .expect("the record value is valid"),
            ),
        ])],
    )
    .expect("the presenter rows are valid");
    let document = render_terminal_table(&active, &status).expect("the table renders");
    let object = ObjectId::from_bytes([0x55; 16]).canonical();
    let expected = format!(
        "b    n                 f    t     x    r                                 e         c\n\
             ---- ----------------- ---- ----- ---- --------------------------------- --------- --------------------\n\
             true -9007199254740993 10.5 héllo AP8= {object} qualified app.status{{x=7, y=z}}\n\
             (1 row)\n"
    );
    assert_eq!(document, expected);
}

#[test]
fn terminal_table_rejects_control_characters_in_cells_and_headers() {
    let standard = presenter_standard();
    let active = presenter_active(&standard);

    let newline_text = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Text("a\nb".to_owned())])],
    )
    .expect("the presenter rows are valid");
    assert_presenter_rule(
        render_terminal_table(&active, &newline_text)
            .map(RuntimeValue::Text)
            .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
        "terminal table cells cannot contain control characters",
    );

    let tab_header = ResultRows::new(
        [ResultColumn::new(
            "val\tue",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");
    assert_presenter_rule(
        render_terminal_table(&active, &tab_header)
            .map(RuntimeValue::Text)
            .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
        "terminal table column names cannot contain control characters",
    );
}

#[test]
fn csv_renders_each_cell_form_and_quotes_embedded_delimiters() {
    let standard = presenter_standard();
    let active = presenter_active(&standard);

    let status = ResultRows::new(
        [
            ResultColumn::new("b", ResolvedType::scalar(StandardScalar::Boolean), false)
                .expect("the boolean column is valid"),
            ResultColumn::new("n", ResolvedType::scalar(StandardScalar::BigInt), false)
                .expect("the bigint column is valid"),
            ResultColumn::new("f", ResolvedType::scalar(StandardScalar::Float), false)
                .expect("the float column is valid"),
            ResultColumn::new(
                "t",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            )
            .expect("the text column is valid"),
            ResultColumn::new(
                "x",
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                false,
            )
            .expect("the bytes column is valid"),
            ResultColumn::new("r", ResolvedType::reference(PRESENTER_OBJECT_TYPE), false)
                .expect("the reference column is valid"),
            ResultColumn::new("e", ResolvedType::named(PRESENTER_ENUM_TYPE), false)
                .expect("the enum column is valid"),
            ResultColumn::new("c", ResolvedType::named(PRESENTER_RECORD_TYPE), false)
                .expect("the record column is valid"),
        ],
        [ResultRow::new([
            RuntimeValue::Boolean(true),
            RuntimeValue::BigInt(-9_007_199_254_740_993),
            RuntimeValue::Float(RuntimeFloat::new(10.5).expect("10.5 is finite")),
            RuntimeValue::Text("a,b\"c".to_owned()),
            RuntimeValue::Bytes(vec![0x00, 0xff]),
            RuntimeValue::Reference {
                target: PRESENTER_OBJECT_TYPE,
                object: ObjectId::from_bytes([0x55; 16]),
            },
            RuntimeValue::Enum(
                EnumValue::new(active.catalogue(), PRESENTER_ENUM_TYPE, "qualified")
                    .expect("the enum label is declared"),
            ),
            RuntimeValue::Record(
                RecordValue::new(
                    &active,
                    PRESENTER_RECORD_TYPE,
                    vec![
                        ("x".to_owned(), RuntimeValue::Integer(7)),
                        ("y".to_owned(), RuntimeValue::Text("z".to_owned())),
                    ],
                )
                .expect("the record value is valid"),
            ),
        ])],
    )
    .expect("the presenter rows are valid");
    let document = render_csv_document(&active, &status).expect("the csv renders");
    let object = ObjectId::from_bytes([0x55; 16]).canonical();
    let expected = format!(
        "b,n,f,t,x,r,e,c\n\
             true,-9007199254740993,10.5,\"a,b\"\"c\",AP8=,{object},qualified,\"app.status{{x=7, y=z}}\"\n"
    );
    assert_eq!(document, expected);
}

#[test]
fn csv_rejects_control_characters_in_cells_and_headers() {
    let standard = presenter_standard();
    let active = presenter_active(&standard);

    let newline_text = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Text("a\nb".to_owned())])],
    )
    .expect("the presenter rows are valid");
    let carriage_and_newline = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Text("a\r\nb".to_owned())])],
    )
    .expect("the presenter rows are valid");
    assert_eq!(
        render_csv_document(&active, &newline_text).expect("LF is valid CSV data"),
        "value\n\"a\nb\"\n",
    );
    assert_eq!(
        render_csv_document(&active, &carriage_and_newline).expect("CR/LF are valid CSV data"),
        "value\n\"a\r\nb\"\n",
    );

    let nul_text = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Text("a\0b".to_owned())])],
    )
    .expect("the presenter rows are valid");
    assert_presenter_rule(
        render_csv_document(&active, &nul_text)
            .map(RuntimeValue::Text)
            .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
        "terminal table cells cannot contain control characters",
    );

    let comma_header = ResultRows::new(
        [
            ResultColumn::new("a,b", ResolvedType::scalar(StandardScalar::Integer), false)
                .expect("the value column is valid"),
        ],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");
    let document = render_csv_document(&active, &comma_header).expect("the csv renders");
    assert_eq!(document, "\"a,b\"\n1\n");

    let tab_header = ResultRows::new(
        [ResultColumn::new(
            "val\tue",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");
    assert_presenter_rule(
        render_csv_document(&active, &tab_header)
            .map(RuntimeValue::Text)
            .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
        "csv column names cannot contain control characters",
    );
}

#[test]
fn standard_csv_encode_rejects_wrong_kind_format_and_version() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = csv_encode_function(
        STD_CSV_ENCODE_FUNCTION_ID,
        STD_CSV_ENCODE_PARAMETER_ID,
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
    );
    let parameter = STD_CSV_ENCODE_PARAMETER_ID;
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");

    let wrong_kind = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Client,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            csv_encode_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_csv_encode(&function, &wrong_kind, &rows, &active, &registry),
        function.id(),
        "current revision must contain a SERVER artifact",
    );

    let wrong_format = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            csv_encode_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_csv_encode(&function, &wrong_format, &rows, &active, &registry),
        function.id(),
        "current SERVER artifact must use orna.server-csv-encode",
    );

    let wrong_version = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION + 1,
            csv_encode_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_csv_encode(&function, &wrong_version, &rows, &active, &registry),
        function.id(),
        "current SERVER artifact must use orna.server-csv-encode version 1",
    );

    let wrong_language = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        "orna.language/9",
        csv_encode_artifact(parameter),
    );
    assert_presenter_artifact_rule(
        execute_standard_csv_encode(&function, &wrong_language, &rows, &active, &registry),
        function.id(),
        "current SERVER revision must use the csv-encode language version",
    );

    assert_eq!(
        execute_standard_csv_encode(
            &function,
            &csv_encode_revision(function.id(), parameter),
            &rows,
            &active,
            &registry,
        )
        .expect("the exact artifact must execute"),
        RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                frame_byte_stream(b"text/csv", b"value\n1\n"),
            )
            .expect("the framed byte stream constructs"),
        )
    );
}

#[test]
fn standard_csv_encode_artifacts_reject_each_decode_deviation() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = csv_encode_function(
        STD_CSV_ENCODE_FUNCTION_ID,
        STD_CSV_ENCODE_PARAMETER_ID,
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
    );
    let parameter = STD_CSV_ENCODE_PARAMETER_ID;
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");

    let mut invalid_magic = csv_encode_payload(parameter);
    invalid_magic[0] = b'X';
    let revision = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            invalid_magic,
        ),
    );
    assert_csv_encode_decode_rule(
        execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
        CsvEncodePlanError::InvalidMagic,
    );

    let other_parameter = ParameterId::from_bytes([0x52; 16]);
    let revision = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            csv_encode_payload(other_parameter),
        ),
    );
    assert_csv_encode_decode_rule(
        execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
        CsvEncodePlanError::UnexpectedParameter {
            actual: other_parameter,
            expected: parameter,
        },
    );

    let other_type = orna_standard::BIGINT_TYPE_ID;
    let revision = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            CsvEncodePlan::new(parameter, other_type)
                .expect("any identities form a valid csv-encode model")
                .encode()
                .expect("the canonical csv-encode model encodes"),
        ),
    );
    assert_csv_encode_decode_rule(
        execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
        CsvEncodePlanError::UnexpectedType {
            actual: other_type,
            expected: STD_DATA_ROWS_TYPE_ID,
        },
    );

    let truncated = csv_encode_payload(parameter)[..40].to_vec();
    let revision = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            truncated,
        ),
    );
    assert_csv_encode_decode_rule(
        execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
        CsvEncodePlanError::Truncated,
    );

    let mut trailing = csv_encode_payload(parameter);
    trailing.push(0);
    let revision = presenter_revision(
        function.id(),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            trailing,
        ),
    );
    assert_csv_encode_decode_rule(
        execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
        CsvEncodePlanError::TrailingBytes,
    );
}

#[test]
fn standard_csv_encode_signature_rejects_each_shape_deviation() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let parameter = STD_CSV_ENCODE_PARAMETER_ID;
    let revision = csv_encode_revision(STD_CSV_ENCODE_FUNCTION_ID, parameter);
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");
    let run = |function: &FunctionDefinition| {
        execute_standard_csv_encode(function, &revision, &rows, &active, &registry)
    };

    let client = FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Client,
        vec![csv_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_presenter_domain_rule(run(&client));

    let missing = FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&missing),
        STD_CSV_ENCODE_FUNCTION_ID,
        "standard csv-encode presenters must declare exactly one required non-null std.data.Rows parameter",
    );

    let wrong_parameter_type = FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter,
            "p_rows",
            0,
            ResolvedType::named(orna_standard::BIGINT_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&wrong_parameter_type),
        STD_CSV_ENCODE_FUNCTION_ID,
        "standard csv-encode presenters must declare one std.data.Rows parameter and one std.io.ByteStream result",
    );

    let wrong_result_type = FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Server,
        vec![csv_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&wrong_result_type),
        STD_CSV_ENCODE_FUNCTION_ID,
        "standard csv-encode presenters must declare one std.data.Rows parameter and one std.io.ByteStream result",
    );

    let definer = FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Server,
        vec![csv_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Definer,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&definer),
        STD_CSV_ENCODE_FUNCTION_ID,
        "standard presenter functions must use INVOKER security",
    );

    let manual = FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Server,
        vec![csv_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&manual),
        STD_CSV_ENCODE_FUNCTION_ID,
        "standard presenter functions must use READ ONLY transactions",
    );

    let volatile = FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Server,
        vec![csv_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Immutable,
    );
    assert_signature_rule(
        run(&volatile),
        STD_CSV_ENCODE_FUNCTION_ID,
        "standard presenter functions must use STABLE volatility",
    );

    // The exact pinned shape still executes after every rejection.
    assert_eq!(
        execute_standard_csv_encode(
            &csv_encode_function(
                STD_CSV_ENCODE_FUNCTION_ID,
                parameter,
                STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            ),
            &revision,
            &rows,
            &active,
            &registry,
        )
        .expect("the pinned shape must execute"),
        RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                frame_byte_stream(b"text/csv", b"value\n1\n"),
            )
            .expect("the framed byte stream constructs"),
        )
    );
}

#[test]
fn standard_terminal_table_rejects_wrong_kind_format_and_version() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = terminal_table_function(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
    );
    let parameter = STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID;
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");

    let wrong_kind = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Client,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            terminal_table_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_terminal_table(&function, &wrong_kind, &rows, &active, &registry),
        function.id(),
        "current revision must contain a SERVER artifact",
    );

    let wrong_format = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            terminal_table_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_terminal_table(&function, &wrong_format, &rows, &active, &registry),
        function.id(),
        "current SERVER artifact must use orna.server-terminal-table",
    );

    let wrong_version = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION + 1,
            terminal_table_payload(parameter),
        ),
    );
    assert_presenter_artifact_rule(
        execute_standard_terminal_table(&function, &wrong_version, &rows, &active, &registry),
        function.id(),
        "current SERVER artifact must use orna.server-terminal-table version 1",
    );

    let wrong_language = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        "orna.language/9",
        terminal_table_artifact(parameter),
    );
    assert_presenter_artifact_rule(
        execute_standard_terminal_table(&function, &wrong_language, &rows, &active, &registry),
        function.id(),
        "current SERVER revision must use the terminal-table language version",
    );

    assert_eq!(
        execute_standard_terminal_table(
            &function,
            &terminal_table_revision(function.id(), parameter),
            &rows,
            &active,
            &registry,
        )
        .expect("the exact artifact must execute"),
        RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
                frame_terminal_document("value\n-----\n1\n(1 row)\n"),
            )
            .expect("the framed document constructs"),
        )
    );
}

#[test]
fn standard_terminal_table_artifacts_reject_each_decode_deviation() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let function = terminal_table_function(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
    );
    let parameter = STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID;
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");

    let mut invalid_magic = terminal_table_payload(parameter);
    invalid_magic[0] = b'X';
    let revision = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            invalid_magic,
        ),
    );
    assert_terminal_table_decode_rule(
        execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
        TerminalTablePlanError::InvalidMagic,
    );

    let other_parameter = ParameterId::from_bytes([0x51; 16]);
    let revision = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            terminal_table_payload(other_parameter),
        ),
    );
    assert_terminal_table_decode_rule(
        execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
        TerminalTablePlanError::UnexpectedParameter {
            actual: other_parameter,
            expected: parameter,
        },
    );

    let other_type = orna_standard::BIGINT_TYPE_ID;
    let revision = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            TerminalTablePlan::new(parameter, other_type)
                .expect("any identities form a valid terminal-table model")
                .encode()
                .expect("the canonical terminal-table model encodes"),
        ),
    );
    assert_terminal_table_decode_rule(
        execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
        TerminalTablePlanError::UnexpectedType {
            actual: other_type,
            expected: STD_DATA_ROWS_TYPE_ID,
        },
    );

    let truncated = terminal_table_payload(parameter)[..40].to_vec();
    let revision = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            truncated,
        ),
    );
    assert_terminal_table_decode_rule(
        execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
        TerminalTablePlanError::Truncated,
    );

    let mut trailing = terminal_table_payload(parameter);
    trailing.push(0);
    let revision = presenter_revision(
        function.id(),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            trailing,
        ),
    );
    assert_terminal_table_decode_rule(
        execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
        TerminalTablePlanError::TrailingBytes,
    );
}

#[test]
fn standard_terminal_table_signature_rejects_each_shape_deviation() {
    let standard = presenter_standard();
    let registry =
        orna_standard::registered_opaque_codecs(&standard).expect("the V3 opaque codecs register");
    let active = presenter_active(&standard);
    let parameter = STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID;
    let revision = terminal_table_revision(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID, parameter);
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("the value column is valid")],
        [ResultRow::new([RuntimeValue::Integer(1)])],
    )
    .expect("the presenter rows are valid");
    let run = |function: &FunctionDefinition| {
        execute_standard_terminal_table(function, &revision, &rows, &active, &registry)
    };

    let client = FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Client,
        vec![terminal_table_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_presenter_domain_rule(run(&client));

    let missing = FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&missing),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        "standard terminal-table presenters must declare exactly one required non-null std.data.Rows parameter",
    );

    let wrong_parameter_type = FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter,
            "p_rows",
            0,
            ResolvedType::named(orna_standard::BIGINT_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&wrong_parameter_type),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        "standard terminal-table presenters must declare one std.data.Rows parameter and one std.terminal.Document result",
    );

    let wrong_result_type = FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Server,
        vec![terminal_table_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&wrong_result_type),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        "standard terminal-table presenters must declare one std.data.Rows parameter and one std.terminal.Document result",
    );

    let definer = FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Server,
        vec![terminal_table_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Definer,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&definer),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        "standard presenter functions must use INVOKER security",
    );

    let manual = FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Server,
        vec![terminal_table_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&manual),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        "standard presenter functions must use READ ONLY transactions",
    );

    let volatile = FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Server,
        vec![terminal_table_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Immutable,
    );
    assert_signature_rule(
        run(&volatile),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        "standard presenter functions must use STABLE volatility",
    );

    // The exact pinned shape still executes after every rejection.
    assert_eq!(
        execute_standard_terminal_table(
            &terminal_table_function(
                STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                parameter,
                STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            ),
            &revision,
            &rows,
            &active,
            &registry,
        )
        .expect("the pinned shape must execute"),
        RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
                frame_terminal_document("value\n-----\n1\n(1 row)\n"),
            )
            .expect("the framed document constructs"),
        )
    );
}

fn assert_presenter_conversion_rule(
    active: &ActiveDatabaseRevision,
    value: RuntimeValue,
    fragment: &str,
) {
    let error = encode_json_value(active, &value).expect_err("the value must be rejected");
    assert!(
        error.contains(fragment),
        "expected a rule mentioning {fragment:?}, got {error:?}"
    );
}
