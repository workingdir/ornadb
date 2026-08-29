use super::*;

#[test]
fn records_standard_client_boolean_body_uses_with_the_resolved_type_id() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let clients = checked.client_functions().collect::<Vec<_>>();
    assert_eq!(clients.len(), 1);
    assert_eq!(checked.uses().len(), 3);

    let boolean = TypeId::from_bytes([3; 16]);
    let expected_kinds = [
        CheckedTypeUseKind::Return {
            owner: clients[0].id(),
            ordinal: 0,
        },
        CheckedTypeUseKind::Expression {
            owner: clients[0].id(),
            ordinal: 0,
        },
        CheckedTypeUseKind::Result {
            owner: clients[0].id(),
            ordinal: 0,
        },
    ];
    assert_eq!(
        checked
            .uses()
            .iter()
            .map(CheckedApplicationTypeUse::kind)
            .collect::<Vec<_>>(),
        expected_kinds
    );
    let literal_start = source.find("TRUE").unwrap();
    for type_use in &checked.uses()[1..] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
        assert_eq!(type_use.location().span().start(), literal_start);
        assert_eq!(
            type_use.location().span().end(),
            literal_start + "TRUE".len()
        );
    }
    assert!(
        checked_use_index(
            checked.uses(),
            expected_kinds[1],
            literal_start,
            literal_start + "TRUE".len(),
        ) < checked_use_index(
            checked.uses(),
            expected_kinds[2],
            literal_start,
            literal_start + "TRUE".len(),
        )
    );
}

#[test]
fn records_standard_client_state_slot_use_as_declaration_evidence() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.state() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN; BEGIN RETURN TRUE; END;";
    let report = check_standard_application(&bundle([("state.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let function = checked.client_functions().next().unwrap();
    let state_kind = CheckedTypeUseKind::State {
        owner: function.id(),
        ordinal: 0,
    };
    let state_start = source.find("STATE flag BOOLEAN").unwrap() + "STATE flag ".len();
    let state_use = checked
        .uses()
        .iter()
        .find(|type_use| type_use.kind() == state_kind)
        .expect("state type use");

    assert_eq!(checked.uses().len(), 4);
    assert_eq!(
        state_use.value().map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16]))
    );
    assert_type_use_span(state_use, state_start, "BOOLEAN");
    assert!(
        checked
            .preparation_evidence
            .declaration_uses
            .iter()
            .any(|type_use| type_use.kind() == state_kind)
    );
    assert_eq!(checked.standard_type_references().len(), 1);
    assert_eq!(checked.standard_type_references()[0].owner(), function.id());
    assert_eq!(checked.standard_type_references()[0].ordinal(), 0);
}

#[test]
fn rejects_nested_client_stream_call_operands() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE EXTERNAL CLIENT FUNCTION app.events() RETURNS STREAM<BOOLEAN> \
            RUNTIME CONTRACT 'app.events@1'; \
            CREATE CLIENT FUNCTION app.forward() RETURNS STREAM<BOOLEAN> IS BEGIN RETURN app.events(); END;";
    let report = check_standard_application(&bundle([("stream-call.orna", source)]), &context);

    assert!(
        report.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == DiagnosticCode::TypeMismatch
                && diagnostic
                    .message()
                    .contains("CLIENT STREAM function app.events")
        }),
        "{:?}",
        report.diagnostics()
    );
}

#[test]
fn records_client_stream_return_shape_and_element_evidence() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE EXTERNAL CLIENT FUNCTION app.events() RETURNS STREAM<BOOLEAN> \
            RUNTIME CONTRACT 'app.events@1';";
    let report = check_standard_application(&bundle([("stream-client.orna", source)]), &context);

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let view = report.preparation_view().unwrap();
    let checked = view.checked();
    let function = &checked.client_functions()[0];
    assert_eq!(function.return_shape(), CheckedClientReturnShape::Stream,);
    assert_eq!(
        function.return_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert_eq!(view.uses().len(), 1);
    assert_eq!(
        view.uses()[0].kind(),
        CheckedTypeUseKind::Return {
            owner: function.id(),
            ordinal: 0,
        },
    );
    assert_eq!(
        view.uses()[0].value().map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16])),
    );
}

#[test]
fn retains_standard_preparation_evidence_from_canonical_uses_and_references() {
    let changed_boolean = TypeId::from_bytes([0x53; 16]);
    let snapshot = verified_standard_library_for_relational_test_with_boolean_id(
        changed_boolean,
        [
            0xa2, 0x5b, 0xcf, 0x20, 0x76, 0x46, 0x26, 0xdf, 0xe3, 0x77, 0x67, 0xca, 0x79, 0xc9,
            0x3e, 0x5f, 0xdc, 0x53, 0x8c, 0xc0, 0x7b, 0x74, 0xce, 0xac, 0x54, 0x2d, 0xb9, 0x31,
            0x3c, 0x56, 0xe1, 0x82,
        ],
    );
    let standard = check_standard_library_source(&snapshot).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let first_server = "CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
    let client = "CREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;";
    let second_server = "CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) \
            RETURNS ROWS (value std.BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT TRUE FROM app.item item WHERE REF(item) = p_ref;";
    let declarations = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);";
    let report = check_standard_application(
        &bundle([
            ("z-first-server.orna", first_server),
            ("a-client.orna", client),
            ("y-second-server.orna", second_server),
            ("m-declarations.orna", declarations),
        ]),
        &context,
    );

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let server_functions = checked.server_functions().collect::<Vec<_>>();
    let client_functions = checked.client_functions().collect::<Vec<_>>();
    let [create, list] = server_functions.as_slice() else {
        assert_eq!(server_functions.len(), 2);
        return;
    };
    let [enabled] = client_functions.as_slice() else {
        assert_eq!(client_functions.len(), 1);
        return;
    };

    let first_boolean = first_server.find("p_boolean BOOLEAN").unwrap() + "p_boolean ".len();
    let first_alias = first_server.find("p_alias std.BOOLEAN").unwrap() + "p_alias ".len();
    let client_boolean = client.find("std.BOOLEAN").unwrap();
    let second_boolean = second_server.find("value std.BOOLEAN").unwrap() + "value ".len();
    assert_eq!(
        checked
            .standard_type_references()
            .iter()
            .map(|reference| {
                (
                    reference.owner(),
                    reference.ordinal(),
                    reference.target(),
                    reference.location().logical_path(),
                    reference.location().span().start(),
                    reference.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                create.id(),
                1,
                changed_boolean,
                "z-first-server.orna",
                first_boolean,
                first_boolean + "BOOLEAN".len(),
            ),
            (
                create.id(),
                2,
                changed_boolean,
                "z-first-server.orna",
                first_alias,
                first_alias + "std.BOOLEAN".len(),
            ),
            (
                enabled.id(),
                0,
                changed_boolean,
                "a-client.orna",
                client_boolean,
                client_boolean + "std.BOOLEAN".len(),
            ),
            (
                list.id(),
                1,
                changed_boolean,
                "y-second-server.orna",
                second_boolean,
                second_boolean + "std.BOOLEAN".len(),
            ),
        ]
    );
    assert_eq!(
        checked.preparation_evidence.type_uses,
        checked.uses(),
        "preparation evidence must retain the canonical type-use arena after sorting"
    );
    let evidence_paths =
        checked
            .preparation_evidence
            .type_uses
            .iter()
            .fold(Vec::new(), |mut paths, type_use| {
                let path = type_use.location().logical_path();
                if paths.last().is_none_or(|previous| *previous != path) {
                    paths.push(path);
                }
                paths
            });
    assert_eq!(
        evidence_paths,
        vec![
            "z-first-server.orna",
            "a-client.orna",
            "y-second-server.orna",
            "m-declarations.orna",
        ],
        "canonical source-unit order is insertion order, not logical-path order"
    );
    assert_eq!(
        checked.preparation_evidence.standard_type_references, checked.standard_type_references,
        "preparation evidence must retain the canonical flattened signature references"
    );

    let object_types = checked.object_types().collect::<Vec<_>>();
    let [item] = object_types.as_slice() else {
        assert_eq!(object_types.len(), 1);
        return;
    };
    let fields = item.fields().collect::<Vec<_>>();
    let [done] = fields.as_slice() else {
        assert_eq!(fields.len(), 1);
        return;
    };
    let first_ref = first_server.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
    let created_ref = first_server.find("created REF app.item").unwrap() + "created REF ".len();
    let second_ref = second_server.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
    let field_boolean = declarations.find("done BOOLEAN").unwrap() + "done ".len();
    assert_eq!(
        [
            done.resolved_type(),
            create.parameters().next().unwrap().resolved_type(),
            create.parameters().nth(1).unwrap().resolved_type(),
            create.parameters().nth(2).unwrap().resolved_type(),
            create.return_columns().next().unwrap().resolved_type(),
            enabled.return_type(),
            list.parameters().next().unwrap().resolved_type(),
            list.return_columns().next().unwrap().resolved_type(),
        ]
        .into_iter()
        .map(|type_use| match type_use {
            CheckedApplicationTypeUse::Value(value) => (Some(value.type_id()), None),
            CheckedApplicationTypeUse::Named { .. } => (None, None),
            CheckedApplicationTypeUse::ObjectReference(reference) => {
                (None, Some(reference.target()))
            }
        })
        .collect::<Vec<_>>(),
        vec![
            (Some(changed_boolean), None),
            (None, Some(item.id())),
            (Some(changed_boolean), None),
            (Some(changed_boolean), None),
            (None, Some(item.id())),
            (Some(changed_boolean), None),
            (None, Some(item.id())),
            (Some(changed_boolean), None),
        ],
        "public scalar-free views must retain each value ID and REF target"
    );
    assert_eq!(
        checked
            .preparation_evidence
            .declaration_uses
            .iter()
            .map(|type_use| {
                (
                    type_use.kind(),
                    type_use.value().map(CheckedValueTypeUse::type_id),
                    type_use
                        .object_reference()
                        .map(|reference| reference.target()),
                    type_use.location().logical_path().to_owned(),
                    type_use.location().span().start(),
                    type_use.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                CheckedTypeUseKind::Parameter {
                    owner: create.id(),
                    parameter: create.parameters().next().unwrap().id(),
                },
                None,
                Some(item.id()),
                "z-first-server.orna".to_owned(),
                first_ref,
                first_ref + "app.item".len(),
            ),
            (
                CheckedTypeUseKind::Parameter {
                    owner: create.id(),
                    parameter: create.parameters().nth(1).unwrap().id(),
                },
                Some(changed_boolean),
                None,
                "z-first-server.orna".to_owned(),
                first_boolean,
                first_boolean + "BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Parameter {
                    owner: create.id(),
                    parameter: create.parameters().nth(2).unwrap().id(),
                },
                Some(changed_boolean),
                None,
                "z-first-server.orna".to_owned(),
                first_alias,
                first_alias + "std.BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Return {
                    owner: create.id(),
                    ordinal: 0,
                },
                None,
                Some(item.id()),
                "z-first-server.orna".to_owned(),
                created_ref,
                created_ref + "app.item".len(),
            ),
            (
                CheckedTypeUseKind::Return {
                    owner: enabled.id(),
                    ordinal: 0,
                },
                Some(changed_boolean),
                None,
                "a-client.orna".to_owned(),
                client_boolean,
                client_boolean + "std.BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Parameter {
                    owner: list.id(),
                    parameter: list.parameters().next().unwrap().id(),
                },
                None,
                Some(item.id()),
                "y-second-server.orna".to_owned(),
                second_ref,
                second_ref + "app.item".len(),
            ),
            (
                CheckedTypeUseKind::Return {
                    owner: list.id(),
                    ordinal: 0,
                },
                Some(changed_boolean),
                None,
                "y-second-server.orna".to_owned(),
                second_boolean,
                second_boolean + "std.BOOLEAN".len(),
            ),
            (
                CheckedTypeUseKind::Field {
                    owner: item.id(),
                    field: done.id(),
                },
                Some(changed_boolean),
                None,
                "m-declarations.orna".to_owned(),
                field_boolean,
                field_boolean + "BOOLEAN".len(),
            ),
        ]
    );
    let made_ref = first_server.find("REF(made)").unwrap();
    assert_eq!(
        checked
            .preparation_evidence
            .type_uses
            .iter()
            .filter(|type_use| {
                type_use.location().logical_path() == "z-first-server.orna"
                    && type_use.location().span().start() == made_ref
                    && type_use.location().span().end() == made_ref + "REF(made)".len()
            })
            .map(CheckedApplicationTypeUse::kind)
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: create.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Result {
                owner: create.id(),
                ordinal: 0,
            },
        ],
        "the sealed full arena must retain Expression-before-Result at a coincident span"
    );

    let create_declaration_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Parameter { owner, .. }
                    | CheckedTypeUseKind::Return { owner, .. }
                    if owner == create.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(create_declaration_uses.len(), 4);
    assert!(create_declaration_uses[0].object_reference().is_some());
    assert!(create_declaration_uses[1].value().is_some());
    assert!(create_declaration_uses[2].value().is_some());
    assert!(create_declaration_uses[3].object_reference().is_some());
    assert_eq!(
        create
            .references()
            .iter()
            .map(|reference| reference.kind())
            .collect::<Vec<_>>(),
        vec![
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceKind::ObjectReference,
        ]
    );
}

#[test]
fn accepts_standard_server_scalar_select_and_preserves_references() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (); \
            CREATE SERVER FUNCTION app.find() RETURNS BOOLEAN \
            AS SELECT TRUE FROM app.item item;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let functions = checked.server_functions().collect::<Vec<_>>();
    assert_eq!(functions.len(), 1);
    let function = functions[0];
    assert_eq!(function.return_columns().count(), 0);
    assert_eq!(function.references().len(), 1);
    assert!(matches!(
        function.references()[0].target(),
        CheckedDefinitionReferenceTarget::ObjectType(_)
    ));
}

#[test]
fn rejects_a_client_boolean_literal_when_the_checked_standard_lacks_boolean() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 1);
    let [diagnostic] = report.diagnostics() else {
        return;
    };
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "the checked standard library does not provide a Boolean value type"
    );
    let literal_start = source.find("TRUE").unwrap();
    assert_eq!(diagnostic.location().span().start(), literal_start);
    assert_eq!(
        diagnostic.location().span().end(),
        literal_start + "TRUE".len()
    );
}

#[test]
fn rejects_qualified_client_boolean_literals_when_the_checked_standard_lacks_boolean() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let cases = [
        (
            "std.BOOLEAN",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;",
        ),
        (
            "std.types.BOOLEAN",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS std.types.BOOLEAN RETURN TRUE;",
        ),
        (
            "\"std\".\"boolean\"",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS \"std\".\"boolean\" RETURN TRUE;",
        ),
        (
            "\"std\".\"types\".\"boolean\"",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS \"std\".\"types\".\"boolean\" RETURN TRUE;",
        ),
    ];

    for (spelling, source) in cases {
        let report = check_standard_application(&bundle([("application.orna", source)]), &context);

        assert!(report.checked_bundle().is_none(), "spelling: {spelling}");
        assert_eq!(report.diagnostics().len(), 1, "spelling: {spelling}");
        let [diagnostic] = report.diagnostics() else {
            return;
        };
        assert_eq!(
            diagnostic.code(),
            DiagnosticCode::DomainIncompatible,
            "spelling: {spelling}"
        );
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type",
            "spelling: {spelling}"
        );
        let literal_start = source.find("TRUE").unwrap();
        assert_eq!(
            diagnostic.location().span().start(),
            literal_start,
            "spelling: {spelling}"
        );
        assert_eq!(
            diagnostic.location().span().end(),
            literal_start + "TRUE".len(),
            "spelling: {spelling}"
        );
    }
}

#[test]
fn rejects_a_standard_query_equality_before_both_boolean_literals_when_boolean_is_missing() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT ();\
            CREATE SERVER FUNCTION app.matches() RETURNS ROWS (matches BOOLEAN) \
            AS SELECT TRUE = FALSE FROM app.task t;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 3);
    let [parent, left, right] = report.diagnostics() else {
        return;
    };
    let expected = [
        ("TRUE", source.find("TRUE").unwrap()),
        ("TRUE = FALSE", source.find("TRUE = FALSE").unwrap()),
        ("FALSE", source.find("FALSE").unwrap()),
    ];
    for (diagnostic, (text, start)) in [parent, left, right].into_iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type"
        );
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + text.len());
    }
}

#[test]
fn rejects_an_identity_selected_query_before_its_missing_boolean_selector_result() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT ();\
            CREATE SERVER FUNCTION app.matches(p_task REF app.task) RETURNS ROWS (matches BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT TRUE FROM app.task t WHERE REF(t) = p_task;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 2);
    let [projection, selector] = report.diagnostics() else {
        return;
    };
    let expected = [
        ("TRUE", source.find("TRUE").unwrap()),
        ("REF(t) = p_task", source.find("REF(t) = p_task").unwrap()),
    ];
    for (diagnostic, (text, start)) in [projection, selector].into_iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(
            diagnostic.message(),
            "the checked standard library does not provide a Boolean value type"
        );
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + text.len());
    }
}

#[test]
fn records_standard_relational_body_uses_in_all_three_query_families() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL, other BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.ordinary() RETURNS ROWS (done BOOLEAN, task REF app.task) \
            AS SELECT t.done, REF(t) FROM app.task t WHERE t.done = TRUE ORDER BY t.done;\
            CREATE SERVER FUNCTION app.distinct() RETURNS ROWS (done BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT DISTINCT t.done FROM app.task t WHERE t.done;\
            CREATE SERVER FUNCTION app.by_ref(p_task REF app.task) RETURNS ROWS (done BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.done FROM app.task t WHERE REF(t) = p_task;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    assert!(checked.uses().windows(2).all(|pair| {
        let first = pair[0].location().span();
        let second = pair[1].location().span();
        (first.start(), first.end()) <= (second.start(), second.end())
    }));
    let functions = checked.server_functions().collect::<Vec<_>>();
    let [ordinary, distinct, by_ref] = functions.as_slice() else {
        assert_eq!(checked.server_functions().count(), 3);
        return;
    };
    let object = checked.object_types().next().unwrap();

    let body_uses = |owner| {
        checked
            .uses()
            .iter()
            .filter(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { owner: candidate, .. }
                        | CheckedTypeUseKind::Result { owner: candidate, .. }
                        if candidate == owner
                )
            })
            .collect::<Vec<_>>()
    };
    let boolean = TypeId::from_bytes([3; 16]);

    let ordinary_uses = body_uses(ordinary.id());
    assert_eq!(ordinary_uses.len(), 8);
    let ordinary_projection = expression_use(&ordinary_uses, 0);
    let ordinary_result = result_use(&ordinary_uses, 0);
    let ordinary_reference = expression_use(&ordinary_uses, 1);
    let ordinary_reference_result = result_use(&ordinary_uses, 1);
    let ordinary_equality = expression_use(&ordinary_uses, 2);
    let ordinary_left = expression_use(&ordinary_uses, 3);
    let ordinary_literal = expression_use(&ordinary_uses, 4);
    let ordinary_ordering = expression_use(&ordinary_uses, 5);
    let distinct_start = source.find("CREATE SERVER FUNCTION app.distinct").unwrap();
    let ordinary_done = source
        .match_indices("t.done")
        .filter(|(start, _)| *start < distinct_start)
        .collect::<Vec<_>>();
    assert_eq!(ordinary_done.len(), 3);
    assert_type_use_span(ordinary_projection, ordinary_done[0].0, "t.done");
    assert_type_use_span(ordinary_result, ordinary_done[0].0, "t.done");
    assert_type_use_span(ordinary_equality, ordinary_done[1].0, "t.done = TRUE");
    assert_type_use_span(ordinary_left, ordinary_done[1].0, "t.done");
    assert_type_use_span(ordinary_literal, source.find("TRUE").unwrap(), "TRUE");
    assert_type_use_span(ordinary_ordering, ordinary_done[2].0, "t.done");
    let ordinary_reference_start = source
        .match_indices("REF(t)")
        .find(|(start, _)| *start < distinct_start)
        .map(|(start, _)| start);
    assert!(ordinary_reference_start.is_some());
    let Some(ordinary_reference_start) = ordinary_reference_start else {
        return;
    };
    assert_type_use_span(ordinary_reference, ordinary_reference_start, "REF(t)");
    assert_type_use_span(
        ordinary_reference_result,
        ordinary_reference_start,
        "REF(t)",
    );
    for type_use in [
        ordinary_projection,
        ordinary_result,
        ordinary_equality,
        ordinary_left,
        ordinary_literal,
        ordinary_ordering,
    ] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
    }
    for type_use in [ordinary_reference, ordinary_reference_result] {
        assert_eq!(
            type_use
                .object_reference()
                .map(|reference| reference.target()),
            Some(object.id())
        );
    }
    assert!(
        checked_use_index(
            checked.uses(),
            ordinary_projection.kind(),
            ordinary_done[0].0,
            ordinary_done[0].0 + "t.done".len(),
        ) < checked_use_index(
            checked.uses(),
            ordinary_result.kind(),
            ordinary_done[0].0,
            ordinary_done[0].0 + "t.done".len(),
        )
    );
    assert!(
        checked_use_index(
            checked.uses(),
            ordinary_reference.kind(),
            ordinary_reference_start,
            ordinary_reference_start + "REF(t)".len(),
        ) < checked_use_index(
            checked.uses(),
            ordinary_reference_result.kind(),
            ordinary_reference_start,
            ordinary_reference_start + "REF(t)".len(),
        )
    );

    let distinct_uses = body_uses(distinct.id());
    assert_eq!(distinct_uses.len(), 3);
    let distinct_projection = expression_use(&distinct_uses, 0);
    let distinct_result = result_use(&distinct_uses, 0);
    let distinct_predicate = expression_use(&distinct_uses, 1);
    for type_use in [distinct_projection, distinct_result, distinct_predicate] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
    }
    let identity_start = source.find("CREATE SERVER FUNCTION app.by_ref").unwrap();
    let distinct_done = source
        .match_indices("t.done")
        .filter(|(start, _)| *start > distinct.location().span().start() && *start < identity_start)
        .collect::<Vec<_>>();
    assert_eq!(distinct_done.len(), 2);
    assert_type_use_span(distinct_projection, distinct_done[0].0, "t.done");
    assert_type_use_span(distinct_result, distinct_done[0].0, "t.done");
    assert_type_use_span(distinct_predicate, distinct_done[1].0, "t.done");
    assert!(
        checked_use_index(
            checked.uses(),
            distinct_projection.kind(),
            distinct_done[0].0,
            distinct_done[0].0 + "t.done".len(),
        ) < checked_use_index(
            checked.uses(),
            distinct_result.kind(),
            distinct_done[0].0,
            distinct_done[0].0 + "t.done".len(),
        )
    );

    let selector_uses = body_uses(by_ref.id());
    assert_eq!(selector_uses.len(), 5);
    let selector_projection = expression_use(&selector_uses, 0);
    let selector_result = result_use(&selector_uses, 0);
    let selector_equality = expression_use(&selector_uses, 1);
    let selector_left = expression_use(&selector_uses, 2);
    let selector_right = expression_use(&selector_uses, 3);
    for type_use in [selector_projection, selector_result, selector_equality] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(boolean)
        );
    }
    assert_eq!(
        selector_left
            .object_reference()
            .map(|reference| reference.target()),
        Some(object.id())
    );
    assert_eq!(
        selector_right
            .object_reference()
            .map(|reference| reference.target()),
        Some(object.id())
    );
    let selector_done = source
        .match_indices("t.done")
        .find(|(start, _)| *start > identity_start)
        .map(|(start, _)| start);
    assert!(selector_done.is_some());
    let Some(selector_done) = selector_done else {
        return;
    };
    let selector_equality_start = source.find("REF(t) = p_task").unwrap();
    let selector_left_start = source
        .match_indices("REF(t)")
        .find(|(start, _)| *start > identity_start)
        .map(|(start, _)| start);
    assert!(selector_left_start.is_some());
    let Some(selector_left_start) = selector_left_start else {
        return;
    };
    let selector_right_start = source.rfind("p_task").unwrap();
    assert_type_use_span(selector_projection, selector_done, "t.done");
    assert_type_use_span(selector_result, selector_done, "t.done");
    assert_type_use_span(
        selector_equality,
        selector_equality_start,
        "REF(t) = p_task",
    );
    assert_type_use_span(selector_left, selector_left_start, "REF(t)");
    assert_type_use_span(selector_right, selector_right_start, "p_task");
    assert!(
        checked_use_index(
            checked.uses(),
            selector_projection.kind(),
            selector_done,
            selector_done + "t.done".len(),
        ) < checked_use_index(
            checked.uses(),
            selector_result.kind(),
            selector_done,
            selector_done + "t.done".len(),
        )
    );
    assert!(selector_uses.iter().all(|type_use| {
        !matches!(
            type_use.kind(),
            CheckedTypeUseKind::Result { ordinal: 1, .. }
        )
    }));
}

#[test]
fn retains_a_non_golden_checked_boolean_id_through_relational_and_client_bodies() {
    let changed_boolean = TypeId::from_bytes([0x53; 16]);
    let snapshot = verified_standard_library_for_relational_test_with_boolean_id(
        changed_boolean,
        [
            0xa2, 0x5b, 0xcf, 0x20, 0x76, 0x46, 0x26, 0xdf, 0xe3, 0x77, 0x67, 0xca, 0x79, 0xc9,
            0x3e, 0x5f, 0xdc, 0x53, 0x8c, 0xc0, 0x7b, 0x74, 0xce, 0xac, 0x54, 0x2d, 0xb9, 0x31,
            0x3c, 0x56, 0xe1, 0x82,
        ],
    );
    let standard = check_standard_library_source(&snapshot).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.matches() RETURNS ROWS (matches BOOLEAN) \
            AS SELECT t.done = TRUE FROM app.task t;\
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let servers = checked.server_functions().collect::<Vec<_>>();
    let clients = checked.client_functions().collect::<Vec<_>>();
    let [server] = servers.as_slice() else {
        assert_eq!(servers.len(), 1);
        return;
    };
    let [client] = clients.as_slice() else {
        assert_eq!(clients.len(), 1);
        return;
    };
    let server_body = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == server.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(server_body.len(), 4);
    for type_use in [
        expression_use(&server_body, 0),
        expression_use(&server_body, 1),
        expression_use(&server_body, 2),
        result_use(&server_body, 0),
    ] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(changed_boolean)
        );
    }
    let equality_start = source.find("t.done = TRUE").unwrap();
    assert_eq!(
        expression_use(&server_body, 0).location().span().start(),
        equality_start
    );
    assert_eq!(
        expression_use(&server_body, 0).location().span().end(),
        equality_start + "t.done = TRUE".len()
    );

    let client_body = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == client.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(client_body.len(), 2);
    for type_use in [expression_use(&client_body, 0), result_use(&client_body, 0)] {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(changed_boolean)
        );
    }
}

#[test]
fn records_standard_mutation_body_uses_in_committed_traversal_order() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL, note BOOLEAN, parent REF app.task);\
            CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.task) \
            TRANSACTION ATOMIC AS INSERT INTO app.task AS made (done, note) VALUES (p_done, NULL) RETURNING REF(made);\
            CREATE SERVER FUNCTION app.change(p_task REF app.task, p_done BOOLEAN) RETURNS ROWS (changed REF app.task) \
            TRANSACTION ATOMIC AS UPDATE app.task AS changed SET done = p_done, note = NULL WHERE REF(changed) = p_task RETURNING REF(changed);\
            CREATE SERVER FUNCTION app.remove(p_task REF app.task) RETURNS ROWS (deleted BOOLEAN) \
            TRANSACTION ATOMIC AS DELETE FROM app.task AS deleted WHERE REF(deleted) = p_task RETURNING TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked_bundle = report.checked_bundle();
    assert!(
        checked_bundle.is_some(),
        "a diagnostic-free standard application report must contain a checked bundle"
    );
    let Some(checked) = checked_bundle else {
        return;
    };
    let functions = checked.server_functions().collect::<Vec<_>>();
    assert_eq!(functions.len(), 3);
    let [insert, update, delete] = functions.as_slice() else {
        return;
    };
    let boolean = TypeId::from_bytes([3; 16]);
    let task = checked.object_types().next().map(|object| object.id());
    assert!(task.is_some());
    let Some(task) = task else {
        return;
    };

    let insert_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == insert.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(insert_uses.len(), 4);
    assert_eq!(
        insert_uses
            .iter()
            .map(|type_use| type_use.kind())
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: insert.id(),
                ordinal: 0,
            },
            CheckedTypeUseKind::Expression {
                owner: insert.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: insert.id(),
                ordinal: 2,
            },
            CheckedTypeUseKind::Result {
                owner: insert.id(),
                ordinal: 0,
            },
        ]
    );
    assert_eq!(
        insert_uses[0].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        insert_uses[1].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        insert_uses[2]
            .object_reference()
            .map(|value| value.target()),
        Some(task)
    );
    assert_eq!(
        insert_uses[3]
            .object_reference()
            .map(|value| value.target()),
        Some(task)
    );
    assert_type_use_span(
        insert_uses[0],
        source.find("p_done, NULL) RETURNING").unwrap(),
        "p_done",
    );
    assert_type_use_span(
        insert_uses[1],
        source.find("NULL) RETURNING").unwrap(),
        "NULL",
    );
    let insert_returning = source.find("REF(made)").unwrap();
    assert_type_use_span(insert_uses[2], insert_returning, "REF(made)");
    assert_type_use_span(insert_uses[3], insert_returning, "REF(made)");

    let update_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == update.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(update_uses.len(), 7);
    assert_eq!(
        update_uses
            .iter()
            .map(|type_use| type_use.kind())
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 0,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 3,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 2,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 4,
            },
            CheckedTypeUseKind::Expression {
                owner: update.id(),
                ordinal: 5,
            },
            CheckedTypeUseKind::Result {
                owner: update.id(),
                ordinal: 0,
            },
        ]
    );
    assert_eq!(
        update_uses[0].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        update_uses[1].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        update_uses[3].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    for type_use in [
        &update_uses[2],
        &update_uses[4],
        &update_uses[5],
        &update_uses[6],
    ] {
        assert_eq!(
            type_use.object_reference().map(|value| value.target()),
            Some(task)
        );
    }
    let update_assignment = source.find("done = p_done, note").unwrap() + "done = ".len();
    let update_null = source.find("note = NULL WHERE").unwrap() + "note = ".len();
    let update_selector = source.find("REF(changed) = p_task").unwrap();
    let update_left = source.find("REF(changed)").unwrap();
    let update_right = source.find("p_task RETURNING REF(changed)").unwrap();
    let update_returning = source.rfind("REF(changed)").unwrap();
    assert_type_use_span(update_uses[0], update_assignment, "p_done");
    assert_type_use_span(update_uses[1], update_null, "NULL");
    assert_type_use_span(update_uses[3], update_selector, "REF(changed) = p_task");
    assert_type_use_span(update_uses[2], update_left, "REF(changed)");
    assert_type_use_span(update_uses[4], update_right, "p_task");
    assert_type_use_span(update_uses[5], update_returning, "REF(changed)");
    assert_type_use_span(update_uses[6], update_returning, "REF(changed)");

    let delete_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner, .. }
                    | CheckedTypeUseKind::Result { owner, .. }
                    if owner == delete.id()
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(delete_uses.len(), 5);
    assert_eq!(
        delete_uses
            .iter()
            .map(|type_use| type_use.kind())
            .collect::<Vec<_>>(),
        vec![
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 1,
            },
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 0,
            },
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 2,
            },
            CheckedTypeUseKind::Expression {
                owner: delete.id(),
                ordinal: 3,
            },
            CheckedTypeUseKind::Result {
                owner: delete.id(),
                ordinal: 0,
            },
        ]
    );
    assert_eq!(
        delete_uses[1].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        delete_uses[3].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    assert_eq!(
        delete_uses[4].value().map(CheckedValueTypeUse::type_id),
        Some(boolean)
    );
    for type_use in [&delete_uses[0], &delete_uses[2]] {
        assert_eq!(
            type_use.object_reference().map(|value| value.target()),
            Some(task)
        );
    }
    let delete_selector = source.find("REF(deleted) = p_task").unwrap();
    let delete_left = source.find("REF(deleted)").unwrap();
    let delete_right = source.find("p_task RETURNING TRUE").unwrap();
    let delete_true = source.rfind("TRUE").unwrap();
    assert_type_use_span(delete_uses[1], delete_selector, "REF(deleted) = p_task");
    assert_type_use_span(delete_uses[0], delete_left, "REF(deleted)");
    assert_type_use_span(delete_uses[2], delete_right, "p_task");
    assert_type_use_span(delete_uses[3], delete_true, "TRUE");
    assert_type_use_span(delete_uses[4], delete_true, "TRUE");
}

#[test]
fn missing_standard_boolean_rejects_insert_and_update_before_any_checked_bundle() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.create() RETURNS ROWS (created REF app.task) \
            TRANSACTION ATOMIC AS INSERT INTO app.task AS made (done) VALUES (TRUE) RETURNING REF(made);\
            CREATE SERVER FUNCTION app.change(p_task REF app.task) RETURNS ROWS (changed REF app.task) \
            TRANSACTION ATOMIC AS UPDATE app.task AS changed SET done = TRUE, other = FALSE \
            WHERE REF(changed) = p_task RETURNING REF(changed);";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    let insert_true = source.find("VALUES (TRUE)").unwrap() + "VALUES (".len();
    let update_first = source.find("done = TRUE").unwrap() + "done = ".len();
    let update_second = source.find("other = FALSE").unwrap() + "other = ".len();
    let selector = source.find("REF(changed) = p_task").unwrap();
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.code(),
                    diagnostic.message(),
                    diagnostic.location().span().start(),
                    diagnostic.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                insert_true,
                insert_true + "TRUE".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                update_first,
                update_first + "TRUE".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                update_second,
                update_second + "FALSE".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                selector,
                selector + "REF(changed) = p_task".len(),
            ),
        ]
    );
}

#[test]
fn missing_standard_boolean_rejects_delete_before_return_column_compatibility() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.integer@1")],
    )
    .unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT ();\
            CREATE SERVER FUNCTION app.remove(p_task REF app.task) RETURNS ROWS (deleted BOOLEAN) \
            TRANSACTION ATOMIC AS DELETE FROM app.task AS deleted \
            WHERE REF(deleted) = p_task RETURNING TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert!(report.checked_bundle().is_none());
    let selector = source.find("REF(deleted) = p_task").unwrap();
    let returned_true = source.rfind("TRUE").unwrap();
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.code(),
                    diagnostic.message(),
                    diagnostic.location().span().start(),
                    diagnostic.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                selector,
                selector + "REF(deleted) = p_task".len(),
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                returned_true,
                returned_true + "TRUE".len(),
            ),
        ]
    );
}

#[test]
fn retains_a_non_golden_boolean_identity_through_every_mutation_boolean_path() {
    let changed_boolean = TypeId::from_bytes([0x53; 16]);
    let snapshot = verified_standard_library_for_relational_test_with_boolean_id(
        changed_boolean,
        [
            0xa2, 0x5b, 0xcf, 0x20, 0x76, 0x46, 0x26, 0xdf, 0xe3, 0x77, 0x67, 0xca, 0x79, 0xc9,
            0x3e, 0x5f, 0xdc, 0x53, 0x8c, 0xc0, 0x7b, 0x74, 0xce, 0xac, 0x54, 0x2d, 0xb9, 0x31,
            0x3c, 0x56, 0xe1, 0x82,
        ],
    );
    let standard = check_standard_library_source(&snapshot).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL, other BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.task) \
            TRANSACTION ATOMIC AS INSERT INTO app.task AS made (done, other) VALUES (p_done, TRUE) RETURNING REF(made);\
            CREATE SERVER FUNCTION app.change(p_task REF app.task, p_done BOOLEAN) RETURNS ROWS (changed REF app.task) \
            TRANSACTION ATOMIC AS UPDATE app.task AS changed SET done = p_done, other = TRUE \
            WHERE REF(changed) = p_task RETURNING REF(changed);\
            CREATE SERVER FUNCTION app.remove(p_task REF app.task) RETURNS ROWS (deleted BOOLEAN) \
            TRANSACTION ATOMIC AS DELETE FROM app.task AS deleted WHERE REF(deleted) = p_task RETURNING TRUE;";
    let report = check_standard_application(&bundle([("application.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked_bundle = report.checked_bundle();
    assert!(
        checked_bundle.is_some(),
        "a diagnostic-free standard application report must contain a checked bundle"
    );
    let Some(checked) = checked_bundle else {
        return;
    };
    let functions = checked.server_functions().collect::<Vec<_>>();
    let [insert, update, delete] = functions.as_slice() else {
        assert_eq!(functions.len(), 3);
        return;
    };
    let body_uses = |owner| {
        checked
            .uses()
            .iter()
            .filter(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { owner: candidate, .. }
                        | CheckedTypeUseKind::Result { owner: candidate, .. }
                        if candidate == owner
                )
            })
            .collect::<Vec<_>>()
    };
    let insert_uses = body_uses(insert.id());
    let update_uses = body_uses(update.id());
    let delete_uses = body_uses(delete.id());
    let retained = [
        expression_use(&insert_uses, 0),
        expression_use(&insert_uses, 1),
        expression_use(&update_uses, 0),
        expression_use(&update_uses, 1),
        expression_use(&update_uses, 2),
        expression_use(&delete_uses, 0),
        expression_use(&delete_uses, 3),
        result_use(&delete_uses, 0),
    ];
    for type_use in retained {
        assert_eq!(
            type_use.value().map(CheckedValueTypeUse::type_id),
            Some(changed_boolean)
        );
    }
    let insert_parameter = source.find("p_done, TRUE)").unwrap();
    let insert_true = source.find("TRUE) RETURNING").unwrap();
    let update_parameter = source.find("done = p_done, other").unwrap() + "done = ".len();
    let update_true = source.find("other = TRUE WHERE").unwrap() + "other = ".len();
    let update_selector = source.find("REF(changed) = p_task").unwrap();
    let delete_selector = source.find("REF(deleted) = p_task").unwrap();
    let delete_true = source.rfind("TRUE").unwrap();
    assert_type_use_span(expression_use(&insert_uses, 0), insert_parameter, "p_done");
    assert_type_use_span(expression_use(&insert_uses, 1), insert_true, "TRUE");
    assert_type_use_span(expression_use(&update_uses, 0), update_parameter, "p_done");
    assert_type_use_span(expression_use(&update_uses, 1), update_true, "TRUE");
    assert_type_use_span(
        expression_use(&update_uses, 2),
        update_selector,
        "REF(changed) = p_task",
    );
    assert_type_use_span(
        expression_use(&delete_uses, 0),
        delete_selector,
        "REF(deleted) = p_task",
    );
    assert_type_use_span(expression_use(&delete_uses, 3), delete_true, "TRUE");
    assert_type_use_span(result_use(&delete_uses, 0), delete_true, "TRUE");
}

pub(super) fn rebase_standard_origins_to_source(
    origins: &mut [DefinitionOrigin],
    parsed_unit: &ParsedSourceUnit,
) {
    assert_eq!(origins.len(), 5);
    assert_eq!(parsed_unit.parsed().schemas().len(), 2);
    assert_eq!(parsed_unit.parsed().primitive_value_types().len(), 1);
    assert_eq!(parsed_unit.parsed().type_exports().len(), 2);
    let identities = origins
        .iter()
        .map(DefinitionOrigin::identity)
        .collect::<Vec<_>>();
    origins[0] = parsed_origin(identities[0], &parsed_unit.parsed().schemas()[0].span);
    origins[1] = parsed_origin(identities[1], &parsed_unit.parsed().schemas()[1].span);
    origins[2] = parsed_origin(
        identities[2],
        &parsed_unit.parsed().primitive_value_types()[0].span,
    );
    origins[3] = parsed_origin(identities[3], &parsed_unit.parsed().type_exports()[0].span);
    origins[4] = parsed_origin(identities[4], &parsed_unit.parsed().type_exports()[1].span);
}

pub(super) fn assert_standard_source_mismatch(source: &str) {
    let (stored_unit, parsed_unit, catalogue, origins) = standard_reconciliation_inputs(source);
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );
}

pub(super) fn two_type_reconciliation_inputs(
    source: &str,
) -> (
    StoredSourceUnit,
    ParsedSourceUnit,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
) {
    let stored_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        source,
        source_unit_content_digest(source).unwrap(),
    )
    .unwrap();
    let parsed_unit = parsed_standard_unit(source);
    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        QualifiedSemanticName::new(["std", "types", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "boolean@1",
    );
    let integer = ValueTypeDefinition::primitive(
        TypeId::from_bytes([4; 16]),
        QualifiedSemanticName::new(["std", "types", "integer"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Transient,
        "int@1",
    );
    let qualified_boolean = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        boolean.id(),
    )
    .unwrap();
    let qualified_integer = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"]).unwrap(),
        integer.id(),
    )
    .unwrap();
    let prelude_boolean =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
    let prelude_integer =
        TypeBinding::prelude(PreludeTypeName::new(["integer"]).unwrap(), integer.id()).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ],
        vec![],
        vec![boolean, integer],
        vec![
            prelude_boolean,
            qualified_integer,
            prelude_integer,
            qualified_boolean,
        ],
    )
    .unwrap();
    let canonical_unit = parsed_standard_unit(TWO_TYPE_STANDARD_SOURCE);
    let mut origins = Vec::new();
    for declaration in canonical_unit.parsed().schemas() {
        let name = QualifiedSemanticName::new(
            declaration
                .name
                .parts
                .iter()
                .map(|part| part.text.to_ascii_lowercase()),
        )
        .unwrap();
        let id = catalogue.schema_by_name(&name).unwrap().id();
        origins.push(parsed_origin(
            DefinitionIdentity::Schema(id),
            &declaration.span,
        ));
    }
    for declaration in canonical_unit.parsed().primitive_value_types() {
        let name = QualifiedSemanticName::new(
            declaration
                .name
                .parts
                .iter()
                .map(|part| part.text.to_ascii_lowercase()),
        )
        .unwrap();
        let id = catalogue.value_type_by_name(&name).unwrap().id();
        origins.push(parsed_origin(
            DefinitionIdentity::ValueType(id),
            &declaration.span,
        ));
    }
    for declaration in canonical_unit.parsed().type_exports() {
        let name = match &declaration.target {
            orna_syntax::TypeExportTarget::Qualified { name } => TypeLookupName::qualified(
                QualifiedSemanticName::new(
                    name.parts.iter().map(|part| part.text.to_ascii_lowercase()),
                )
                .unwrap(),
            ),
            orna_syntax::TypeExportTarget::Prelude { words, .. } => TypeLookupName::prelude(
                PreludeTypeName::new(words.iter().map(|word| word.text.as_str())).unwrap(),
            ),
        };
        let id = catalogue.type_binding_by_name(&name).unwrap().id();
        origins.push(parsed_origin(
            DefinitionIdentity::TypeBinding(id),
            &declaration.span,
        ));
    }
    origins.reverse();

    (stored_unit, parsed_unit, catalogue, origins)
}

pub(super) fn parsed_standard_unit(source: &str) -> ParsedSourceUnit {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/types.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty());
    report.units()[0].clone()
}

pub(super) fn parsed_origin(identity: DefinitionIdentity, span: &SourceSpan) -> DefinitionOrigin {
    DefinitionOrigin::new(
        identity,
        SourceOrigin::new(
            STANDARD_SOURCE_UNIT_ID,
            u32::try_from(span.start).unwrap(),
            u32::try_from(span.end).unwrap(),
        )
        .unwrap(),
    )
}
