use super::*;

#[test]
fn accepts_single_return_select_at_the_declared_return() {
    let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT p.name FROM people.person p;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let functions = checked.server_functions();
    assert_eq!(functions.len(), 1);
    let function = &functions[0];
    assert!(matches!(
        function.return_type(),
        super::super::CheckedServerFunctionReturn::Single {
            semantic_type: super::super::SemanticType::Scalar(StandardScalar::CharacterLargeObject),
            ..
        }
    ));
    let query = function.query_plan().expect("scalar SELECT query plan");
    assert_eq!(query.projections().len(), 1);
    assert_eq!(
        query.projections()[0].value_type().semantic_type(),
        super::super::SemanticType::Scalar(StandardScalar::CharacterLargeObject)
    );
}

#[test]
fn rejects_invalid_server_function_headers_before_body_planning() {
    let report = check(
        &bundle([(
            "functions.orna",
            "CREATE SERVER FUNCTION find() RETURNS TEXT AS SELECT TRUE FROM people.person p;\
                 CREATE SCHEMA people;\
                 CREATE SERVER FUNCTION people.find() RETURNS TEXT TRANSACTION MANUAL AS SELECT TRUE FROM people.person p;",
        )]),
        &empty_catalogue(),
    );

    let diagnostics = report.diagnostics();
    assert_eq!(diagnostics[0].code(), DiagnosticCode::UnknownQualifiedName);
    assert_eq!(diagnostics[1].code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostics[1].message(),
        "SERVER functions do not yet support TRANSACTION MANUAL"
    );
    assert!(diagnostics.iter().all(|diagnostic| {
        diagnostic.message() != "SERVER functions do not yet support this body form"
    }));
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_duplicate_server_function_names_after_normalisation() {
    let report = check(
        &bundle([(
            "functions.orna",
            "CREATE SCHEMA people;\
                 CREATE SERVER FUNCTION People.Find() RETURNS TEXT AS SELECT TRUE FROM people.person p;\
                 CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT FALSE FROM people.person p;",
        )]),
        &empty_catalogue(),
    );

    let diagnostics = report.diagnostics();
    assert_eq!(diagnostics[0].code(), DiagnosticCode::DuplicateDefinition);
    assert_eq!(
        diagnostics[0].message(),
        "duplicate server function definition people.find"
    );
    assert_eq!(diagnostics.len(), 1);
    assert_no_checked_bundle(&report);
}

#[test]
fn preserves_server_header_and_duplicate_diagnostic_order() {
    let source = "CREATE SCHEMA people;\
            CREATE SERVER FUNCTION people.find() RETURNS TEXT TRANSACTION MANUAL AS SELECT TRUE FROM people.person p;\
            CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT FALSE FROM people.person p;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report.diagnostics()[0].message(),
        "SERVER functions do not yet support TRANSACTION MANUAL"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("CREATE SERVER").unwrap()
    );
    assert_eq!(
        report.diagnostics()[1].message(),
        "duplicate server function definition people.find"
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.rfind("people.find").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn accepts_a_checked_server_function_with_a_relational_plan() {
    let source = "CREATE SCHEMA tasks; \
            CREATE SERVER FUNCTION tasks.open() RETURNS ROWS (title TEXT, completed BOOL) \
            SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.title, t.completed FROM tasks.task t WHERE t.completed = FALSE; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT, completed BOOL NOT NULL);";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = &report.checked_bundle().unwrap().server_functions()[0];
    assert_eq!(checked.security(), FunctionSecurity::Definer);
    assert_eq!(checked.transaction(), Some(FunctionTransaction::ReadOnly));
    assert_eq!(checked.volatility(), FunctionVolatility::Stable);
    assert!(checked.parameters().is_empty());
    assert_eq!(checked.return_columns().len(), 2);
    let plan = checked.query_plan().expect("fixture has a SELECT body");
    assert_eq!(plan.projections().len(), 2);
    assert!(plan.selection().is_some());
}

#[test]
fn checks_server_insert_with_exact_body_identities_and_evidence() {
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person); \
            CREATE SERVER FUNCTION tasks.create(p_title TEXT, p_unused INT, p_owner REF tasks.person) \
            RETURNS ROWS (result REF tasks.task) TRANSACTION ATOMIC \
            AS INSERT INTO tasks.task AS created (title, done, note, owner) \
            VALUES (p_title, FALSE, NULL, p_owner) RETURNING REF(created);";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = &report.checked_bundle().unwrap().server_functions()[0];
    assert!(checked.query_plan().is_none());
    let task = &report.checked_bundle().unwrap().object_types()[1];
    let person = &report.checked_bundle().unwrap().object_types()[0];
    let plan = checked.mutation_plan().expect("expected an INSERT body");
    assert_eq!(plan.target_object(), task.id());
    assert_eq!(plan.returned_object(), task.id());
    assert_eq!(plan.assignments().len(), 4);
    assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
    assert_eq!(plan.assignments()[1].field(), task.fields()[1].id());
    assert_eq!(plan.assignments()[2].field(), task.fields()[2].id());
    assert_eq!(plan.assignments()[3].field(), task.fields()[3].id());
    assert_eq!(checked.return_columns()[0].name(), "result");
    assert_eq!(checked.security(), FunctionSecurity::Invoker);
    assert_eq!(checked.transaction(), Some(FunctionTransaction::Atomic));
    assert_eq!(checked.volatility(), FunctionVolatility::Volatile);

    let parameter_ids = checked
        .parameters()
        .iter()
        .map(|parameter| parameter.id())
        .collect::<Vec<_>>();
    assert_eq!(
        checked
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(person.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[0].id()
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: checked.id(),
                    parameter: parameter_ids[0]
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[1].id()
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[2].id()
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[3].id()
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: checked.id(),
                    parameter: parameter_ids[2]
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
        ]
    );
    assert!(
        checked
            .references()
            .iter()
            .all(|reference| reference.location().logical_path() == "functions.orna")
    );
    assert_eq!(
        checked
            .references()
            .iter()
            .map(|reference| {
                (
                    reference.location().span().start(),
                    reference.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            {
                let start = source.find("p_owner REF tasks.person").unwrap() + "p_owner REF ".len();
                (start, start + "tasks.person".len())
            },
            {
                let start = source.find("result REF tasks.task").unwrap() + "result REF ".len();
                (start, start + "tasks.task".len())
            },
            {
                let start = source.rfind("tasks.task AS created").unwrap();
                (start, start + "tasks.task".len())
            },
            {
                let start = source.rfind("(title, done").unwrap() + 1;
                (start, start + "title".len())
            },
            {
                let start = source.rfind("p_title").unwrap();
                (start, start + "p_title".len())
            },
            {
                let start = source.rfind("done, note").unwrap();
                (start, start + "done".len())
            },
            {
                let start = source.rfind("note, owner").unwrap();
                (start, start + "note".len())
            },
            {
                let start = source.rfind("note, owner)").unwrap() + "note, ".len();
                (start, start + "owner".len())
            },
            {
                let start = source.rfind("p_owner").unwrap();
                (start, start + "p_owner".len())
            },
            {
                let start = source.rfind("created)").unwrap();
                (start, start + "created".len())
            },
        ]
    );
}

#[test]
fn checks_server_update_with_selector_and_exact_evidence_order() {
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, owner REF tasks.person); \
            CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT, p_owner REF tasks.person) \
            RETURNS ROWS (updated REF tasks.task) TRANSACTION ATOMIC \
            AS UPDATE tasks.task AS changed SET title = p_title, owner = p_owner \
            WHERE REF(changed) = p_task RETURNING REF(changed);";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let bundle = report.checked_bundle().unwrap();
    let checked = &bundle.server_functions()[0];
    let person = &bundle.object_types()[0];
    let task = &bundle.object_types()[1];
    let plan = checked.mutation_plan().expect("expected an UPDATE body");
    let parameters = checked.parameters();
    assert_eq!(
        plan.operation(),
        &crate::mutation::MutationOperation::Update {
            selector_owner: checked.id(),
            selector_parameter: parameters[0].id(),
        }
    );
    assert_eq!(plan.target_object(), task.id());
    assert_eq!(plan.returned_object(), task.id());
    assert_eq!(plan.assignments().len(), 2);
    assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
    assert_eq!(plan.assignments()[1].field(), task.fields()[2].id());
    assert_eq!(
        checked
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(person.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: checked.id(),
                    parameter: parameters[1].id(),
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[2].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: checked.id(),
                    parameter: parameters[2].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: checked.id(),
                    parameter: parameters[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
        ]
    );
    let token_span = |context: &str, prefix: &str, token: &str| {
        let context_start = source.find(context).unwrap();
        let start = context_start + prefix.len();
        (start, start + token.len())
    };
    assert_eq!(
        checked
            .references()
            .iter()
            .map(|reference| {
                (
                    reference.location().span().start(),
                    reference.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            token_span("p_task REF tasks.task", "p_task REF ", "tasks.task"),
            token_span("p_owner REF tasks.person", "p_owner REF ", "tasks.person"),
            token_span("updated REF tasks.task", "updated REF ", "tasks.task"),
            token_span("UPDATE tasks.task", "UPDATE ", "tasks.task"),
            token_span("SET title", "SET ", "title"),
            token_span("= p_title", "= ", "p_title"),
            {
                let start = source.rfind(", owner").unwrap() + ", ".len();
                (start, start + "owner".len())
            },
            token_span("= p_owner", "= ", "p_owner"),
            token_span("WHERE REF(changed)", "WHERE REF(", "changed"),
            token_span("= p_task RETURNING", "= ", "p_task"),
            token_span("RETURNING REF(changed)", "RETURNING REF(", "changed"),
        ]
    );
}

#[test]
fn checks_server_delete_with_boolean_result_and_exact_evidence_order() {
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); \
            CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) \
            RETURNS ROWS (deleted BOOL) TRANSACTION ATOMIC \
            AS DELETE FROM tasks.task AS deleted_task \
            WHERE REF(deleted_task) = p_task RETURNING TRUE;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let bundle = report.checked_bundle().expect("DELETE source is valid");
    let checked = &bundle.server_functions()[0];
    let task = &bundle.object_types()[0];
    let parameter = &checked.parameters()[0];
    let plan = checked.delete_plan().expect("expected a DELETE body");

    assert_eq!(plan.target_object(), task.id());
    assert_eq!(plan.selector_owner(), checked.id());
    assert_eq!(plan.selector_parameter(), parameter.id());
    assert_eq!(checked.return_columns()[0].name(), "deleted");
    assert_eq!(
        checked.return_columns()[0].semantic_type(),
        SemanticType::Scalar(StandardScalar::Boolean)
    );
    assert_eq!(
        checked
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: checked.id(),
                    parameter: parameter.id(),
                },
            ),
        ]
    );
    let span = |context: &str, prefix: &str, token: &str| {
        let start = source.find(context).unwrap() + prefix.len();
        (start, start + token.len())
    };
    assert_eq!(
        checked
            .references()
            .iter()
            .map(|reference| {
                (
                    reference.location().span().start(),
                    reference.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            span("p_task REF tasks.task", "p_task REF ", "tasks.task"),
            span("DELETE FROM tasks.task", "DELETE FROM ", "tasks.task"),
            span("WHERE REF(deleted_task)", "WHERE REF(", "deleted_task",),
            span("= p_task RETURNING", "= ", "p_task"),
        ]
    );
}

#[test]
fn rejects_delete_return_shape_and_execution_modes_exactly() {
    let prefix = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); ";
    let body = "AS DELETE FROM tasks.task AS removed WHERE REF(removed) = p_task RETURNING TRUE;";
    let cases = [
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) RETURNS ROWS (a BOOL, b BOOL) TRANSACTION ATOMIC {body}"
            ),
            DiagnosticCode::TypeMismatch,
            "A DELETE SERVER function must declare exactly one column in RETURNS ROWS (...)",
            "ROWS (a BOOL, b BOOL)",
        ),
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) RETURNS ROWS (deleted REF tasks.task) TRANSACTION ATOMIC {body}"
            ),
            DiagnosticCode::TypeMismatch,
            "The RETURNS ROWS (...) column for a DELETE SERVER function must use BOOLEAN",
            "deleted REF tasks.task",
        ),
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) RETURNS BOOL TRANSACTION ATOMIC {body}"
            ),
            DiagnosticCode::TypeMismatch,
            "DELETE SERVER functions require RETURNS ROWS (...)",
            "BOOL",
        ),
    ];

    for (source, code, message, marker) in cases {
        let source_bundle =
            SourceBundle::new([SourceUnit::new("functions.orna", &source)]).unwrap();
        let report = check(&source_bundle, &empty_catalogue());
        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), 1);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), code);
        assert_eq!(diagnostic.message(), message);
        let start = source.rfind(marker).unwrap();
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + marker.len());
    }

    let source = format!(
        "{prefix}CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task) \
             RETURNS ROWS (deleted BOOL) SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE {body}"
    );
    let source_bundle = SourceBundle::new([SourceUnit::new("functions.orna", &source)]).unwrap();
    let report = check(&source_bundle, &empty_catalogue());
    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 3);
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DomainIncompatible,
                "DELETE SERVER functions require SECURITY INVOKER",
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "DELETE SERVER functions require TRANSACTION ATOMIC",
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "DELETE SERVER functions require VOLATILITY VOLATILE",
            ),
        ]
    );
    let declaration_start = source.find("CREATE SERVER FUNCTION").unwrap();
    for diagnostic in report.diagnostics() {
        assert_eq!(diagnostic.location().span().start(), declaration_start);
        assert_eq!(diagnostic.location().span().end(), source.len());
    }
}

#[test]
fn rejects_an_unused_delete_parameter_outside_the_runtime_types() {
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); \
            CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task, unused DECIMAL) \
            RETURNS ROWS (deleted BOOL) TRANSACTION ATOMIC \
            AS DELETE FROM tasks.task AS removed \
            WHERE REF(removed) = p_task RETURNING TRUE;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "DELETE does not yet support the type of parameter unused; supported types are BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF"
    );
    let start = source.find("unused DECIMAL").unwrap();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "unused".len());
}

#[test]
fn rejects_insert_return_and_execution_modes() {
    let prefix = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); ";
    let cases = [
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task, b REF tasks.task) TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
            ),
            vec![(
                DiagnosticCode::TypeMismatch,
                "An INSERT SERVER function must declare exactly one column in RETURNS ROWS (...)",
                "ROWS (a",
            )],
        ),
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a TEXT) TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
            ),
            vec![(
                DiagnosticCode::TypeMismatch,
                "The RETURNS ROWS (...) column for an INSERT SERVER function must use REF",
                "a TEXT",
            )],
        ),
        (
            format!(
                "{prefix}CREATE TYPE tasks.other AS OBJECT (title TEXT NOT NULL); CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.other) TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
            ),
            vec![(
                DiagnosticCode::TypeMismatch,
                "The returned REF must point to the object type being inserted",
                "tasks.other",
            )],
        ),
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) SECURITY DEFINER TRANSACTION ATOMIC AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
            ),
            vec![(
                DiagnosticCode::DomainIncompatible,
                "INSERT SERVER functions require SECURITY INVOKER",
                "CREATE SERVER FUNCTION",
            )],
        ),
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
            ),
            vec![(
                DiagnosticCode::DomainIncompatible,
                "INSERT SERVER functions require TRANSACTION ATOMIC",
                "CREATE SERVER FUNCTION",
            )],
        ),
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) TRANSACTION READ ONLY AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
            ),
            vec![(
                DiagnosticCode::DomainIncompatible,
                "INSERT SERVER functions require TRANSACTION ATOMIC",
                "CREATE SERVER FUNCTION",
            )],
        ),
        (
            format!(
                "{prefix}CREATE SERVER FUNCTION tasks.create(p TEXT) RETURNS ROWS (a REF tasks.task) TRANSACTION ATOMIC VOLATILITY STABLE AS INSERT INTO tasks.task AS made (title) VALUES (p) RETURNING REF(made);"
            ),
            vec![(
                DiagnosticCode::DomainIncompatible,
                "INSERT SERVER functions require VOLATILITY VOLATILE",
                "CREATE SERVER FUNCTION",
            )],
        ),
    ];
    for (source, expected) in cases {
        let bundle = SourceBundle::new([SourceUnit::new("functions.orna", &source)]).unwrap();
        let report = check(&bundle, &empty_catalogue());
        assert_no_checked_bundle(&report);
        assert_eq!(report.diagnostics().len(), expected.len());
        for (diagnostic, (code, message, marker)) in report.diagnostics().iter().zip(expected) {
            assert_eq!(diagnostic.code(), code);
            assert_eq!(diagnostic.message(), message);
            assert_eq!(diagnostic.location().logical_path(), "functions.orna");
            let expected_start = source.rfind(marker).unwrap();
            assert_eq!(diagnostic.location().span().start(), expected_start);
            let expected_end = match message {
                "An INSERT SERVER function must declare exactly one column in RETURNS ROWS (...)" => {
                    source.find(") TRANSACTION").unwrap() + 1
                }
                "The RETURNS ROWS (...) column for an INSERT SERVER function must use REF" => {
                    expected_start + "a TEXT".len()
                }
                "The returned REF must point to the object type being inserted" => {
                    expected_start + "tasks.other".len()
                }
                _ => source.len(),
            };
            assert_eq!(diagnostic.location().span().end(), expected_end);
        }
    }
}

#[test]
fn rejects_update_return_target_and_execution_modes_exactly() {
    let prefix = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL); \
            CREATE TYPE tasks.other AS OBJECT (title TEXT NOT NULL); ";
    let wrong_modes = format!(
        "{prefix}CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT) \
             RETURNS ROWS (updated REF tasks.task) SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE \
             AS UPDATE tasks.task AS changed SET title = p_title WHERE REF(changed) = p_task RETURNING REF(changed);"
    );
    let source_bundle =
        SourceBundle::new([SourceUnit::new("functions.orna", &wrong_modes)]).unwrap();
    let report = check(&source_bundle, &empty_catalogue());
    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 3);
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DomainIncompatible,
                "UPDATE SERVER functions require SECURITY INVOKER",
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "UPDATE SERVER functions require TRANSACTION ATOMIC",
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "UPDATE SERVER functions require VOLATILITY VOLATILE",
            ),
        ]
    );
    assert!(report.diagnostics().iter().all(|diagnostic| {
        diagnostic.location().span().start() == wrong_modes.rfind("CREATE SERVER FUNCTION").unwrap()
            && diagnostic.location().span().end() == wrong_modes.len()
    }));

    let wrong_return = format!(
        "{prefix}CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT) \
             RETURNS ROWS (updated REF tasks.other) TRANSACTION ATOMIC \
             AS UPDATE tasks.task AS changed SET title = p_title WHERE REF(changed) = p_task RETURNING REF(changed);"
    );
    let source_bundle =
        SourceBundle::new([SourceUnit::new("functions.orna", &wrong_return)]).unwrap();
    let report = check(&source_bundle, &empty_catalogue());
    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "The returned REF must point to the object type being updated"
    );
    let start = wrong_return.rfind("tasks.other").unwrap();
    assert_eq!(report.diagnostics()[0].location().span().start(), start);
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        start + "tasks.other".len()
    );
}

#[test]
fn rejects_distinct_function_shape_with_four_ordered_declaration_diagnostics() {
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.values(p_flag BOOL) RETURNS ROWS (completed BOOL) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY IMMUTABLE \
            AS SELECT DISTINCT t.completed FROM tasks.task t;";
    let report = check(
        &bundle([("distinct_shape.orna", source)]),
        &empty_catalogue(),
    );

    assert_no_checked_bundle(&report);
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DomainIncompatible,
                "SELECT DISTINCT SERVER functions require SECURITY INVOKER",
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "SELECT DISTINCT SERVER functions require TRANSACTION READ ONLY",
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "SELECT DISTINCT SERVER functions require VOLATILITY STABLE",
            ),
            (
                DiagnosticCode::DomainIncompatible,
                "SELECT DISTINCT SERVER functions require zero declared parameters",
            ),
        ]
    );
    for diagnostic in report.diagnostics() {
        assert_eq!(diagnostic.location().logical_path(), "distinct_shape.orna");
        assert_eq!(
            diagnostic.location().span().start(),
            source.find("CREATE SERVER FUNCTION").unwrap()
        );
        assert_eq!(diagnostic.location().span().end(), source.len());
    }
}

#[test]
fn distinct_semantic_and_return_errors_precede_function_shape_diagnostics() {
    let semantic_source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (completed BOOL NOT NULL, title TEXT); \
            CREATE SERVER FUNCTION tasks.values(p_flag BOOL) RETURNS ROWS (completed BOOL) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY IMMUTABLE \
            AS SELECT DISTINCT t.completed FROM tasks.task t WHERE t.title;";
    let report = check(
        &bundle([("distinct_semantic.orna", semantic_source)]),
        &empty_catalogue(),
    );
    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.message(), "WHERE requires a BOOLEAN expression");
    assert_eq!(
        diagnostic.location().logical_path(),
        "distinct_semantic.orna"
    );
    let predicate_start = semantic_source.rfind("t.title").unwrap();
    assert_eq!(diagnostic.location().span().start(), predicate_start);
    assert_eq!(
        diagnostic.location().span().end(),
        predicate_start + "t.title".len()
    );

    let return_source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.values(p_flag BOOL) RETURNS ROWS (completed TEXT) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY IMMUTABLE \
            AS SELECT DISTINCT t.completed FROM tasks.task t;";
    let report = check(
        &bundle([("distinct_return.orna", return_source)]),
        &empty_catalogue(),
    );
    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        diagnostic.message(),
        "SELECT column 1 does not have the same type as RETURNS ROWS column completed"
    );
    assert_eq!(diagnostic.location().logical_path(), "distinct_return.orna");
    let return_start = return_source.find("completed TEXT").unwrap();
    assert_eq!(diagnostic.location().span().start(), return_start);
    assert_eq!(
        diagnostic.location().span().end(),
        return_start + "completed TEXT".len()
    );
}

#[test]
fn rejects_unsupported_distinct_projections_with_the_relational_diagnostic() {
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.values() RETURNS ROWS (title TEXT) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT DISTINCT t.title FROM tasks.task t;";
    let report = check(
        &bundle([("distinct_domain.orna", source)]),
        &empty_catalogue(),
    );

    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "SELECT DISTINCT projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values"
    );
    assert_eq!(diagnostic.location().logical_path(), "distinct_domain.orna");
    let projection_start = source.rfind("t.title").unwrap();
    assert_eq!(diagnostic.location().span().start(), projection_start);
    assert_eq!(
        diagnostic.location().span().end(),
        projection_start + "t.title".len()
    );
}

#[test]
fn rejects_select_projection_count_and_type_at_rows_declarations() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.count() RETURNS ROWS (first TEXT, second TEXT) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.kind() RETURNS ROWS (title BOOL) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.wide() RETURNS ROWS (only TEXT) \
            AS SELECT t.title, t.title FROM tasks.task t;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 3);
    assert_eq!(
        report.diagnostics()[0].message(),
        "SELECT returns 1 column, but RETURNS ROWS (...) declares 2 columns"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("ROWS (first").unwrap()
    );
    assert_eq!(
        report.diagnostics()[1].message(),
        "SELECT column 1 does not have the same type as RETURNS ROWS column title"
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.find("title BOOL").unwrap()
    );
    assert_eq!(
        report.diagnostics()[2].message(),
        "SELECT returns 2 columns, but RETURNS ROWS (...) declares 1 column"
    );
    assert_eq!(
        report.diagnostics()[2].location().span().start(),
        source.rfind("ROWS (only").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_parameterised_select_with_more_than_one_declared_parameter() {
    let _function_id = FunctionId::from_bytes([4; 16]);
    let _parameter_id = ParameterId::from_bytes([5; 16]);
    let _offset_parameter_id = ParameterId::from_bytes([6; 16]);
    let base = catalogue(
        vec![schema(1, &["tasks"])],
        vec![object_type(
            2,
            &["tasks", "task"],
            vec![field(
                3,
                "title",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            )],
        )],
        vec![server_function(
            4,
            &["tasks", "open"],
            vec![
                parameter(
                    5,
                    "p_limit",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
                parameter(
                    6,
                    "p_offset",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
            ],
            vec![rows_column(
                "title",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Volatile,
        )],
    );

    let report = check(
        &bundle([(
            "functions.orna",
            "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
                 CREATE SERVER FUNCTION tasks.open(p_offset INT, p_limit INT) RETURNS ROWS (title TEXT) \
                 SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
                 AS SELECT t.title FROM tasks.task t;",
        )]),
        &base,
    );

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "parameterised SELECT SERVER functions require exactly one declared parameter"
    );
    assert_eq!(
        report.diagnostics()[0].location().logical_path(),
        "functions.orna"
    );
    assert_eq!(
            report.diagnostics()[0].location().span().start(),
            "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
                 CREATE SERVER FUNCTION tasks.open(p_offset INT, p_limit INT) RETURNS ROWS (title TEXT) \
                 SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
                 AS SELECT t.title FROM tasks.task t;"
                .find("SELECT t.title")
                .unwrap()
        );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_identity_selected_query_candidates_with_exact_diagnostics() {
    let prefix = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); ";
    let suffix = " SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE";
    let cases = [
        (
            "no_predicate",
            "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT)",
            " AS SELECT t.title FROM tasks.task t;",
            DiagnosticCode::DomainIncompatible,
            "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter",
            "SELECT t.title",
        ),
        (
            "wrong_name",
            "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT)",
            " AS SELECT t.title FROM tasks.task t WHERE REF(t) = other;",
            DiagnosticCode::UnknownQualifiedName,
            "this function has no parameter named other",
            "other",
        ),
        (
            "wrong_type",
            "CREATE SERVER FUNCTION tasks.get(p_task INT) RETURNS ROWS (title TEXT)",
            " AS SELECT t.title FROM tasks.task t WHERE REF(t) = p_task;",
            DiagnosticCode::TypeMismatch,
            "selector parameter p_task must use REF tasks.task",
            "p_task;",
        ),
        (
            "wrong_alias",
            "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT)",
            " AS SELECT t.title FROM tasks.task t WHERE REF(other) = p_task;",
            DiagnosticCode::UnknownQualifiedName,
            "unknown query alias other",
            "other",
        ),
        (
            "return_type",
            "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title BOOL)",
            " AS SELECT t.title FROM tasks.task t WHERE REF(t) = p_task;",
            DiagnosticCode::TypeMismatch,
            "SELECT column 1 does not have the same type as RETURNS ROWS column title",
            "title BOOL",
        ),
    ];

    for (path, header, body, code, message, marker) in cases {
        let source = format!("{prefix}{header}{suffix}{body}");
        let bundle = SourceBundle::new([SourceUnit::new(path, source.as_str())]).unwrap();
        let report = check(&bundle, &empty_catalogue());
        assert_eq!(report.diagnostics().len(), 1, "{path}");
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), code, "{path}");
        assert_eq!(diagnostic.message(), message, "{path}");
        assert_eq!(diagnostic.location().logical_path(), path, "{path}");
        let expected_start = source.rfind(marker).unwrap();
        assert_eq!(
            diagnostic.location().span().start(),
            expected_start,
            "{path}"
        );
        assert_eq!(
            diagnostic.location().span().end(),
            if path == "no_predicate" {
                source.len() - 1
            } else {
                expected_start + marker.len().saturating_sub((path == "wrong_type") as usize)
            },
            "{path}"
        );
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn reports_identity_selected_query_mode_failures_before_body_checking() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY VOLATILE \
            AS SELECT t.title FROM tasks.task t;";
    let report = check(&bundle([("modes.orna", source)]), &empty_catalogue());
    let messages = report
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.message())
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        vec![
            "parameterised SELECT SERVER functions require SECURITY INVOKER",
            "parameterised SELECT SERVER functions require TRANSACTION READ ONLY",
            "parameterised SELECT SERVER functions require VOLATILITY STABLE",
        ]
    );
    for diagnostic in report.diagnostics() {
        assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
        assert_eq!(diagnostic.location().logical_path(), "modes.orna");
        assert_eq!(
            diagnostic.location().span().start(),
            source.find("CREATE SERVER FUNCTION").unwrap()
        );
        assert_eq!(diagnostic.location().span().end(), source.len());
    }
    assert_no_checked_bundle(&report);
}

#[test]
fn syntax_errors_take_precedence_over_identity_selected_query_modes() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (title TEXT) \
            SECURITY DEFINER TRANSACTION ATOMIC VOLATILITY VOLATILE \
            AS SELECT t.title FROM tasks.task t WHERE p_task = REF(t);";
    let report = check(&bundle([("syntax.orna", source)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnexpectedToken
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "the current Orna SELECT parser does not yet implement selector parameters on the left side of WHERE equality; expected WHERE REF(alias) = selector_parameter"
    );
    assert_eq!(
        report.diagnostics()[0].location().logical_path(),
        "syntax.orna"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.rfind("p_task").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn any_server_function_error_rejects_all_checked_definitions() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.valid() RETURNS ROWS (title TEXT) \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SERVER FUNCTION tasks.invalid() RETURNS ROWS (title BOOL) \
            AS SELECT t.title FROM tasks.task t;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    assert_no_checked_bundle(&report);
}

#[test]
fn does_not_add_body_planning_diagnostics_after_object_errors() {
    let report = check(
        &bundle([(
            "functions.orna",
            "CREATE SCHEMA people;\
                 CREATE TYPE people.person AS OBJECT (manager REF missing.person);\
                 CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT TRUE FROM people.person p;",
        )]),
        &empty_catalogue(),
    );

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_ne!(
        report.diagnostics()[0].message(),
        "SERVER functions do not yet support this body form"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_definitions_in_base_schemas_that_are_omitted_from_the_bundle() {
    let base = catalogue(
        vec![schema(1, &["sys"])],
        Vec::new(),
        vec![server_function(
            2,
            &["sys", "health"],
            Vec::new(),
            vec![rows_column(
                "enabled",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )],
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Volatile,
        )],
    );

    let report = check(
        &bundle([(
            "functions.orna",
            "CREATE TYPE sys.probe AS OBJECT (enabled BOOL); \
                 CREATE SERVER FUNCTION sys.probe_status() RETURNS ROWS (enabled BOOL) \
                 AS SELECT p.enabled FROM sys.probe p;",
        )]),
        &base,
    );

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn server_function_metadata_preserves_ids_and_maps_modifiers() {
    let base = catalogue(
        vec![schema(1, &["sys"])],
        vec![object_type(
            2,
            &["sys", "health"],
            vec![field(
                3,
                "enabled",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
                None,
            )],
        )],
        vec![server_function(
            4,
            &["sys", "health"],
            Vec::new(),
            vec![rows_column(
                "enabled",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )],
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Volatile,
        )],
    );
    let report = check(
        &bundle([(
            "functions.orna",
            "CREATE SCHEMA sys; CREATE TYPE sys.health AS OBJECT (enabled BOOL);\
                 CREATE SERVER FUNCTION Sys.Health() RETURNS ROWS (enabled BOOL) SECURITY DEFINER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT h.enabled FROM sys.health h;\
                 CREATE SERVER FUNCTION sys.defaults() RETURNS ROWS (enabled BOOL) AS SELECT h.enabled FROM sys.health h;",
        )]),
        &base,
    );

    assert!(report.diagnostics().is_empty());
    let functions = report.checked_bundle().unwrap().server_functions();
    assert_eq!(functions.len(), 2);
    assert_eq!(
        functions[0].id().existing(),
        Some(FunctionId::from_bytes([4; 16]))
    );
    assert_eq!(functions[0].security(), FunctionSecurity::Definer);
    assert_eq!(
        functions[0].transaction(),
        Some(FunctionTransaction::ReadOnly)
    );
    assert_eq!(functions[0].volatility(), FunctionVolatility::Stable);
    assert_eq!(functions[1].id().to_string(), "provisional:function:0");
    assert_eq!(functions[1].security(), FunctionSecurity::Invoker);
    assert_eq!(functions[1].transaction(), None);
    assert_eq!(functions[1].volatility(), FunctionVolatility::Volatile);
}

#[test]
fn resolves_server_stream_element_and_preserves_checked_shape() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t;";
    let report = check(&bundle([("stream.orna", source)]), &empty_catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let function = &report.checked_bundle().unwrap().server_functions()[0];
    let super::super::CheckedServerFunctionReturn::Stream {
        semantic_type,
        standard_value_type,
        ..
    } = function.return_type()
    else {
        panic!("expected a checked STREAM return");
    };
    assert_eq!(
        *semantic_type,
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert_eq!(*standard_value_type, None);
    assert!(function.return_columns().is_empty());
}

#[test]
fn discovers_stream_resource_target_with_resolved_element_type() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS STREAM<TEXT> IS \
            BEGIN RETURN AWAIT std.data.stream_resource(target => tasks.events, arguments => std.call.args()); END;";
    let report = check(
        &bundle([("stream-resource.orna", source)]),
        &empty_catalogue(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let client = &report.checked_bundle().unwrap().client_functions()[0];
    let super::super::CheckedClientFunctionBody::Expression {
        expression: return_expression,
    } = client.body()
    else {
        panic!("expected a checked CLIENT expression body");
    };
    let super::super::CheckedClientExpression::Await {
        expression,
        location: await_location,
    } = return_expression
    else {
        panic!("expected AWAIT expression");
    };
    let await_text =
        "AWAIT std.data.stream_resource(target => tasks.events, arguments => std.call.args())";
    let await_start = source
        .find(await_text)
        .expect("await expression is present");
    assert_eq!(await_location.logical_path(), "stream-resource.orna");
    assert_eq!(await_location.span().start(), await_start);
    assert_eq!(await_location.span().end(), await_start + await_text.len());
    let super::super::CheckedClientExpression::Resource { operation } = expression.as_ref() else {
        panic!("expected stream resource expression");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(
        operation.result_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    let resource_text =
        "std.data.stream_resource(target => tasks.events, arguments => std.call.args())";
    let resource_start = source
        .find(resource_text)
        .expect("resource constructor is present");
    assert_eq!(operation.location().logical_path(), "stream-resource.orna");
    assert_eq!(operation.location().span().start(), resource_start);
    assert_eq!(
        operation.location().span().end(),
        resource_start + resource_text.len()
    );
}

#[test]
fn stream_await_requires_optional_list_return_and_local_shape() {
    let valid = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS STREAM<TEXT> IS \
            LET rows std.data.StreamResource<TEXT> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("stream-await-valid.orna", valid)]),
        &empty_catalogue(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let invalid_return = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS TEXT IS \
            LET rows std.data.StreamResource<TEXT> := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("stream-await-return.orna", invalid_return)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);

    let invalid_assignment = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title FROM tasks.task t; \
            CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.read() RETURNS STREAM<TEXT> IS \
            LET rows TEXT := std.data.stream_resource(target => tasks.events, arguments => std.call.args()); \
            BEGIN RETURN AWAIT rows; END;";
    let report = check(
        &bundle([("stream-await-assignment.orna", invalid_assignment)]),
        &empty_catalogue(),
    );
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
}

#[test]
fn rejects_server_stream_queries_with_multiple_projected_columns() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT, done BOOL); \
            CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<TEXT> \
            AS SELECT t.title, t.done FROM tasks.task t;";
    let report = check(&bundle([("stream-shape.orna", source)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
    assert_eq!(
        report.diagnostics()[0].message(),
        "SELECT returns 2 columns, but RETURNS STREAM<T> declares one element"
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_duplicate_server_function_parameters_and_rows_columns() {
    let report = check(
        &bundle([(
            "functions.orna",
            "CREATE SCHEMA people;\
                 CREATE SERVER FUNCTION people.duplicate(p_value TEXT, P_VALUE INT)\
                 RETURNS ROWS (value TEXT, VALUE INT) AS SELECT TRUE FROM people.person p;\
                 CREATE SERVER FUNCTION people.empty() RETURNS ROWS () AS SELECT TRUE FROM people.person p;",
        )]),
        &empty_catalogue(),
    );

    let diagnostics = report.diagnostics();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DiagnosticCode::DuplicateDefinition)
            .count(),
        2
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DiagnosticCode::TypeMismatch
            && diagnostic.message() == "ROWS return type must contain at least one column"
    }));
    assert!(diagnostics.iter().all(|diagnostic| {
        diagnostic.message() != "SERVER functions do not yet support this body form"
    }));
    assert_no_checked_bundle(&report);
}

#[test]
fn rejects_server_defaults_and_capabilities_at_their_source() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.find(p_name TEXT DEFAULT 'open') \
            RETURNS ROWS (title TEXT) REQUIRES CAPABILITY sys.fs.read(p_name) \
            AS SELECT t.title FROM tasks.task t;";
    let report = check(&bundle([("functions.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report.diagnostics()[0].message(),
        "SERVER function parameters do not yet support default values"
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("'open'").unwrap()
    );
    assert_eq!(
        report.diagnostics()[1].message(),
        "SERVER functions do not yet support REQUIRES CAPABILITY"
    );
    assert_eq!(
        report.diagnostics()[1].location().span().start(),
        source.find("sys.fs.read").unwrap()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn checked_bundle_omits_unsubmitted_base_functions_and_schemas() {
    let base = catalogue(
        vec![schema(1, &["sys"])],
        Vec::new(),
        vec![server_function(
            2,
            &["sys", "health"],
            Vec::new(),
            vec![rows_column(
                "enabled",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )],
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        )],
    );

    let report = check(
        &bundle([(
            "people.orna",
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT);",
        )]),
        &base,
    );

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert!(checked.server_functions().is_empty());
    assert_eq!(checked.schemas().len(), 1);
    assert_eq!(checked.schemas()[0].name().to_string(), "people");
}

#[test]
fn rejects_duplicate_and_unknown_schema_names_after_normalisation() {
    let report = check(
        &bundle([(
            "schemas.orna",
            "CREATE SCHEMA People;\
                 CREATE SCHEMA people;\
                 CREATE TYPE missing.contact AS OBJECT (name TEXT);",
        )]),
        &empty_catalogue(),
    );

    let codes = report
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code())
        .collect::<Vec<_>>();
    assert!(codes.contains(&DiagnosticCode::DuplicateDefinition));
    assert!(codes.contains(&DiagnosticCode::UnknownQualifiedName));
    assert_no_checked_bundle(&report);
}
