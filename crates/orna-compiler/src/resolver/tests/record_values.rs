use super::*;

#[test]
fn resolves_enum_labels_and_rejects_decoded_duplicates_before_a_checked_bundle() {
    let accepted = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'owner''s');",
        )]),
        &empty_catalogue(),
    );
    assert!(accepted.diagnostics().is_empty());
    let checked = accepted.checked_bundle().unwrap();
    let enum_types = checked.enum_types().collect::<Vec<_>>();
    assert_eq!(enum_types.len(), 1);
    assert_eq!(enum_types[0].1.to_string(), "crm.stage");
    assert_eq!(enum_types[0].2, &["lead", "owner's"]);

    let existing_id = TypeId::from_bytes([0x44; 16]);
    let base = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x45; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x46; 16]),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            existing_id,
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            ["lead"],
        )],
        vec![],
    )
    .unwrap();
    let changed = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'customer');",
        )]),
        &base,
    );
    assert_eq!(
        changed
            .checked_bundle()
            .unwrap()
            .enum_types()
            .next()
            .unwrap()
            .0,
        CheckedTypeId::Existing(existing_id)
    );

    let duplicate = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('owner''s', 'owner''s');",
        )]),
        &empty_catalogue(),
    );
    assert!(duplicate.checked_bundle().is_none());
    assert_eq!(duplicate.diagnostics().len(), 1);
    assert_eq!(
        duplicate.diagnostics()[0].message(),
        "duplicate enum label \"owner's\" in crm.stage"
    );
}

#[test]
fn resolves_record_value_fields_through_the_closed_standard_and_enum_family() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let source = bundle([(
        "types.orna",
        "CREATE SCHEMA app;\nCREATE TYPE app.phase AS ENUM ('new', 'done');\nCREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;",
    )]);
    let report = check_new_application(&source, &standard).unwrap();

    assert_eq!(report.diagnostics(), &[]);
    assert!(report.preparation_view().is_some());
    let checked = report.checked_bundle().unwrap();
    let records = checked.record_value_types().collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let record = records[0];
    assert!(record.id().is_provisional());
    assert_eq!(record.name().to_string(), "app.status");
    let fields = record.fields().collect::<Vec<_>>();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name(), "active");
    assert_eq!(fields[0].ordinal(), 0);
    assert_eq!(
        fields[0]
            .resolved_type()
            .value()
            .map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(fields[1].name(), "phase");
    assert_eq!(fields[1].ordinal(), 1);
    assert!(
        fields[1]
            .resolved_type()
            .named_type()
            .is_some_and(CheckedTypeId::is_provisional)
    );
    assert!(fields.iter().all(|field| field.id().is_provisional()));
    assert_eq!(checked.uses().len(), 2);
}

#[test]
fn checked_bundle_preserves_object_enum_and_record_value_categories_together() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let source = bundle([(
        "categories.orna",
        "CREATE SCHEMA app;\n\
                CREATE TYPE app.phase AS ENUM ('new', 'done');\n\
                CREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;\n\
                CREATE TYPE app.item AS OBJECT (status app.status NOT NULL, phase app.phase NOT NULL);",
    )]);
    let report = check_new_application(&source, &standard).unwrap();

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let object = checked.object_types().next().unwrap();
    assert_eq!(checked.object_types().count(), 1);
    assert_eq!(object.name().to_string(), "app.item");

    let enum_types = checked.inner.enum_types().collect::<Vec<_>>();
    assert_eq!(enum_types.len(), 1);
    let (enum_id, enum_name, labels, _) = enum_types[0];
    assert_eq!(enum_name.to_string(), "app.phase");
    assert_eq!(labels, &["new".to_owned(), "done".to_owned()]);

    let record = checked.record_value_types().next().unwrap();
    assert_eq!(checked.record_value_types().count(), 1);
    assert_eq!(record.name().to_string(), "app.status");
    assert_ne!(object.id(), enum_id);
    assert_ne!(object.id(), record.id());
    assert_ne!(enum_id, record.id());

    let object_fields = object.fields().collect::<Vec<_>>();
    assert_eq!(object_fields.len(), 2);
    assert_eq!(
        object_fields[0].resolved_type().named_type(),
        Some(record.id())
    );
    assert_eq!(object_fields[1].resolved_type().named_type(), Some(enum_id));
}

#[test]
fn checks_record_constructor_identities_in_declaration_order_and_prepares_artifact() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\n\
            CREATE TYPE app.flags AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.item AS OBJECT (flags app.flags NOT NULL);\n\
            CREATE SERVER FUNCTION app.create(p_visible BOOLEAN)\n\
            RETURNS ROWS (item REF app.item) SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
            AS INSERT INTO app.item AS made (flags)\n\
            VALUES (app.flags{visible: p_visible, active: TRUE}) RETURNING REF(made);";
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle([("constructor.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let preparation = report.preparation_view().unwrap();
    let raw = preparation.checked();
    let record = &raw.record_value_types()[0];
    let record_fields = record.fields();
    let object = &raw.object_types()[0];
    let function = &raw.server_functions()[0];
    let plan = function.mutation_plan().unwrap();
    let MutationExpressionKind::RecordConstructor {
        record_type,
        fields,
    } = plan.assignments()[0].expression().kind()
    else {
        panic!("checked INSERT value must be a record constructor");
    };
    assert_eq!(*record_type, record.id());
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].owner(), record.id());
    assert_eq!(fields[0].field(), record_fields[0].id());
    assert!(matches!(
        fields[0].kind(),
        MutationRecordFieldExpressionKind::BooleanLiteral { value: true }
    ));
    assert_eq!(fields[1].field(), record_fields[1].id());
    assert!(matches!(
        fields[1].kind(),
        MutationRecordFieldExpressionKind::ParameterRead { parameter, .. }
            if *parameter == function.parameters()[0].id()
    ));
    assert_eq!(
        function
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(object.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                CheckedDefinitionReferenceTarget::ObjectType(object.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: object.id(),
                    field: object.fields()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::NamedType,
                CheckedDefinitionReferenceTarget::ValueType(record.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: record.id(),
                    field: record_fields[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: record.id(),
                    field: record_fields[1].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                CheckedDefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                CheckedDefinitionReferenceTarget::ObjectType(object.id()),
            ),
        ]
    );
    let expression_ordinals = checked
        .uses()
        .iter()
        .filter_map(|type_use| match type_use.kind() {
            CheckedTypeUseKind::Expression { ordinal, .. } => Some(ordinal),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(expression_ordinals, vec![2, 1, 3]);
    let constructor_value_uses = checked
        .uses()
        .iter()
        .filter_map(|type_use| {
            let value = type_use.value()?;
            let CheckedTypeUseKind::Expression { ordinal, .. } = value.kind() else {
                return None;
            };
            Some((
                ordinal,
                value.type_id(),
                value.location().span().start(),
                value.location().span().end(),
            ))
        })
        .collect::<Vec<_>>();
    let parameter_start = source.rfind("p_visible").unwrap();
    let literal_start = source.rfind("TRUE").unwrap();
    assert_eq!(
        constructor_value_uses,
        vec![
            (
                2,
                TypeId::from_bytes([3; 16]),
                parameter_start,
                parameter_start + "p_visible".len(),
            ),
            (
                1,
                TypeId::from_bytes([3; 16]),
                literal_start,
                literal_start + "TRUE".len(),
            ),
        ]
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let candidate = prepared.candidate();
    let durable_record = candidate
        .record_value_type_by_name(&QualifiedSemanticName::new(["app", "flags"]).unwrap())
        .unwrap();
    let durable_object = candidate
        .object_type_by_name(&QualifiedSemanticName::new(["app", "item"]).unwrap())
        .unwrap();
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().version(), RECORD_INSERT_FORMAT_VERSION);
    let artifact = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(artifact.target(), durable_object.id());
    assert_eq!(artifact.assignments().len(), 1);
    assert_eq!(artifact.assignments()[0].owner(), durable_object.id());
    assert_eq!(
        artifact.assignments()[0].field(),
        durable_object.fields()[0].id()
    );
    let ServerMutationExpressionKind::RecordConstructor { fields } =
        artifact.assignments()[0].expression().kind()
    else {
        panic!("prepared INSERT value must be a record constructor");
    };
    assert_eq!(
        artifact.assignments()[0].expression().resolved_type(),
        ResolvedType::named(durable_record.id())
    );
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].owner(), durable_record.id());
    assert_eq!(fields[0].field(), durable_record.fields()[0].id());
    assert!(matches!(
        fields[0].kind(),
        ServerRecordFieldExpressionKind::BooleanLiteral { value: true }
    ));
    assert_eq!(fields[1].owner(), durable_record.id());
    assert_eq!(fields[1].field(), durable_record.fields()[1].id());
    assert!(matches!(
        fields[1].kind(),
        ServerRecordFieldExpressionKind::Parameter { owner, parameter }
            if *owner == candidate.functions()[0].id()
                && *parameter == candidate.functions()[0].parameters()[0].id()
    ));
}

#[test]
fn record_constructor_source_order_does_not_change_checked_plan() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let source = |fields: &str| {
        format!(
            "CREATE SCHEMA app;\n\
                 CREATE TYPE app.flags AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.item AS OBJECT (flags app.flags NOT NULL);\n\
                 CREATE SERVER FUNCTION app.create(p_visible BOOLEAN)\n\
                 RETURNS ROWS (item REF app.item) TRANSACTION ATOMIC\n\
                 AS INSERT INTO app.item AS made (flags) VALUES (app.flags{{{fields}}}) RETURNING REF(made);"
        )
    };
    let first_bundle = SourceBundle::new([SourceUnit::new(
        "first.orna",
        source("active: TRUE, visible: p_visible"),
    )])
    .unwrap();
    let second_bundle = SourceBundle::new([SourceUnit::new(
        "second.orna",
        source("visible: p_visible, active: TRUE"),
    )])
    .unwrap();
    let first = check_new_application(&first_bundle, &standard).unwrap();
    let second = check_new_application(&second_bundle, &standard).unwrap();

    assert_eq!(first.diagnostics(), &[]);
    assert_eq!(second.diagnostics(), &[]);
    assert_eq!(
        first
            .preparation_view()
            .unwrap()
            .checked()
            .server_functions()[0]
            .mutation_plan(),
        second
            .preparation_view()
            .unwrap()
            .checked()
            .server_functions()[0]
            .mutation_plan()
    );
}

#[test]
fn record_constructor_accepts_an_exact_active_enum_parameter() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\n\
            CREATE TYPE app.phase AS ENUM ('new', 'done');\n\
            CREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.item AS OBJECT (status app.status NOT NULL);\n\
            CREATE SERVER FUNCTION app.create(p_phase app.phase) RETURNS ROWS (item REF app.item)\n\
            TRANSACTION ATOMIC AS INSERT INTO app.item AS made (status)\n\
            VALUES (app.status{phase: p_phase}) RETURNING REF(made);";
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle([("enum_constructor.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let raw = report.preparation_view().unwrap();
    let checked = raw.checked();
    let enum_type = checked.enum_types().next().unwrap().0;
    let plan = checked.server_functions()[0].mutation_plan().unwrap();
    let MutationExpressionKind::RecordConstructor { fields, .. } =
        plan.assignments()[0].expression().kind()
    else {
        panic!("checked INSERT value must be a record constructor");
    };
    assert_eq!(
        fields[0].value_type().semantic_type(),
        SemanticType::Named(enum_type)
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let candidate = prepared.candidate();
    let durable_enum = candidate
        .enum_type_by_name(&QualifiedSemanticName::new(["app", "phase"]).unwrap())
        .unwrap();
    let revision = &prepared.new_function_revisions()[0];
    let artifact = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    let ServerMutationExpressionKind::RecordConstructor { fields } =
        artifact.assignments()[0].expression().kind()
    else {
        panic!("prepared INSERT value must be a record constructor");
    };
    assert_eq!(
        fields[0].resolved_type(),
        ResolvedType::named(durable_enum.id())
    );
    assert!(matches!(
        fields[0].kind(),
        ServerRecordFieldExpressionKind::Parameter { owner, parameter }
            if *owner == candidate.functions()[0].id()
                && *parameter == candidate.functions()[0].parameters()[0].id()
    ));
}

#[test]
fn record_constructor_rejects_scalar_values_for_enum_fields() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    for (value, expected) in [
        (
            "p_active",
            "parameter p_active cannot initialise record field phase because their types do not match",
        ),
        (
            "TRUE",
            "record field phase is not BOOLEAN, so it cannot accept TRUE or FALSE",
        ),
    ] {
        let source = format!(
            "CREATE SCHEMA app;\n\
                 CREATE TYPE app.phase AS ENUM ('new', 'done');\n\
                 CREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.item AS OBJECT (status app.status NOT NULL);\n\
                 CREATE SERVER FUNCTION app.create(p_active BOOLEAN) RETURNS ROWS (item REF app.item)\n\
                 TRANSACTION ATOMIC AS INSERT INTO app.item AS made (status)\n\
                 VALUES (app.status{{phase: {value}}}) RETURNING REF(made);"
        );
        let value_start = source.rfind(value).unwrap();
        let source_bundle =
            SourceBundle::new([SourceUnit::new("enum_mismatch.orna", source)]).unwrap();
        let report = check_new_application(&source_bundle, &standard).unwrap();

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(report.diagnostics()[0].message(), expected);
        assert_eq!(
            report.diagnostics()[0].location().span().start(),
            value_start
        );
        assert_eq!(
            report.diagnostics()[0].location().span().end(),
            value_start + value.len()
        );
        assert!(report.checked_bundle().is_none());
    }
}

#[test]
fn record_constructor_rejects_a_record_typed_parameter_for_a_nested_child() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let source = "CREATE SCHEMA app;\n\
            CREATE TYPE app.inner AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.outer AS VALUE (child app.inner) IMMUTABLE PERSISTABLE;\n\
            CREATE TYPE app.item AS OBJECT (outer app.outer NOT NULL);\n\
            CREATE SERVER FUNCTION app.create(p_inner app.inner) RETURNS ROWS (item REF app.item)\n\
            TRANSACTION ATOMIC AS INSERT INTO app.item AS made (outer)\n\
            VALUES (app.outer{child: p_inner}) RETURNING REF(made);";
    let source_bundle =
        SourceBundle::new([SourceUnit::new("nested_constructor.orna", source)]).unwrap();
    let report = check_new_application(&source_bundle, &standard).unwrap();

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "INSERT does not yet support the type of parameter p_inner; supported types are BOOLEAN, INTEGER, BIGINT, FLOAT, CHARACTER LARGE OBJECT, BINARY LARGE OBJECT, and REF"
    );
    let value_start = source.find("p_inner").unwrap();
    assert_eq!(diagnostic.location().span().start(), value_start);
    assert_eq!(
        diagnostic.location().span().end(),
        value_start + "p_inner".len()
    );
    assert_eq!(
        &source[value_start..value_start + "p_inner".len()],
        "p_inner"
    );
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_constructor_semantics_reject_incomplete_or_incompatible_values() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    for (object_field, parameter, constructor, expected) in [
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.flags{active: TRUE}",
            "record field visible is required, but this constructor does not provide it",
        ),
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.flags{active: TRUE, visible: p_visible, extra: TRUE}",
            "record value type app.flags has no field named extra",
        ),
        (
            "flags app.flags",
            "p_visible BOOLEAN",
            "app.flags{active: TRUE, visible: p_visible}",
            "record constructor app.flags requires a non-null field of that exact record type, but field flags does not match",
        ),
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.missing{active: TRUE, visible: p_visible}",
            "unknown record value type app.missing",
        ),
        (
            "flags app.flags NOT NULL",
            "p_visible BOOLEAN",
            "app.other{active: TRUE, visible: p_visible}",
            "record constructor app.other requires a non-null field of that exact record type, but field flags does not match",
        ),
        (
            "flags app.flags NOT NULL",
            "p_flags app.flags",
            "app.flags{active: TRUE, visible: p_flags}",
            "INSERT does not yet support the type of parameter p_flags",
        ),
    ] {
        let source = format!(
            "CREATE SCHEMA app;\n\
                 CREATE TYPE app.flags AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.other AS VALUE (active BOOLEAN, visible BOOLEAN) IMMUTABLE PERSISTABLE;\n\
                 CREATE TYPE app.item AS OBJECT ({object_field});\n\
                 CREATE SERVER FUNCTION app.create({parameter}) RETURNS ROWS (item REF app.item)\n\
                 TRANSACTION ATOMIC AS INSERT INTO app.item AS made (flags)\n\
                 VALUES ({constructor}) RETURNING REF(made);"
        );
        let source_bundle =
            SourceBundle::new([SourceUnit::new("invalid_constructor.orna", source)]).unwrap();
        let report = check_new_application(&source_bundle, &standard).unwrap();

        assert!(
            report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message().contains(expected)),
            "expected {expected:?}, got {:?}",
            report.diagnostics()
        );
        assert!(report.checked_bundle().is_none());
    }
}

#[test]
fn prepares_and_replays_record_value_identities_with_exact_evidence() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.phase AS ENUM ('new', 'done');\nCREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("records.orna", source)]);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let checked = report.checked_bundle().unwrap();
    let checked_record = checked.record_value_types().next().unwrap();
    let checked_fields = checked_record.fields().collect::<Vec<_>>();
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let record = &prepared.candidate().record_value_types()[0];
    let enum_type = &prepared.candidate().enum_types()[0];
    assert_eq!(record.name().to_string(), "app.status");
    assert_eq!(record.fields().len(), 2);
    assert_eq!(
        record.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(
        record.fields()[1].descriptor(),
        &orna_core::types::TypeDescriptor::named(enum_type.id())
    );
    let unit = prepared.source().units()[0].id();
    assert!(prepared.origins().iter().any(|origin| {
        origin.identity() == DefinitionIdentity::ValueType(record.id())
            && origin.source()
                == SourceOrigin::new(
                    unit,
                    u32::try_from(checked_record.location().span().start()).unwrap(),
                    u32::try_from(checked_record.location().span().end()).unwrap(),
                )
                .unwrap()
    }));
    for (checked_field, field) in checked_fields.iter().zip(record.fields()) {
        assert!(prepared.origins().iter().any(|origin| {
            origin.identity()
                == DefinitionIdentity::Field {
                    owner: record.id(),
                    field: field.id(),
                }
                && origin.source()
                    == SourceOrigin::new(
                        unit,
                        u32::try_from(checked_field.location().span().start()).unwrap(),
                        u32::try_from(checked_field.location().span().end()).unwrap(),
                    )
                    .unwrap()
        }));
    }

    let record_id = record.id();
    let field_ids = record
        .fields()
        .iter()
        .map(|field| field.id())
        .collect::<Vec<_>>();
    let active = active_from_prepared(&prepared);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let replay_report = check_standard_application(&bundle, &context);
    assert_eq!(replay_report.diagnostics(), &[]);
    let replay = prepare_standard_application(&replay_report, active.pair(), &active).unwrap();
    let replay_record = &replay.candidate().record_value_types()[0];
    assert_eq!(replay_record.id(), record_id);
    assert_eq!(
        replay_record
            .fields()
            .iter()
            .map(|field| field.id())
            .collect::<Vec<_>>(),
        field_ids
    );

    let mut hostile = report.clone();
    let boolean_index = hostile
        .checked_bundle()
        .unwrap()
        .uses()
        .iter()
        .position(|type_use| type_use.value().is_some())
        .unwrap();
    assert!(hostile.replace_value_type_id_for_test(boolean_index, TypeId::from_bytes([0xef; 16]),));
    assert!(matches!(
        prepare_standard_application(&hostile, initial.pair(), &initial),
        Err(
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                kind: CheckedTypeUseKind::Field { .. },
            }
        )
    ));
}

#[test]
fn record_value_preparation_rejects_every_deferred_existing_shape_change() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let original = "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;";
    let original_bundle = bundle([("records.orna", original)]);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&original_bundle, &context);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let active = active_from_prepared(&prepared);

    for (source, reason) in [
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active BOOLEAN, phase app.phase, extra BOOLEAN) IMMUTABLE PERSISTABLE;",
            "record value field addition or removal is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;",
            "record value field addition or removal is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (phase app.phase, active BOOLEAN) IMMUTABLE PERSISTABLE;",
            "record value field reordering is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (active app.phase, phase app.phase) IMMUTABLE PERSISTABLE;",
            "record value field type change is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.status AS VALUE (enabled BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;",
            "record value field replacement is not supported",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done'); CREATE TYPE app.state AS VALUE (active BOOLEAN, phase app.phase) IMMUTABLE PERSISTABLE;",
            "existing record value type is absent from the candidate catalogue",
        ),
        (
            "CREATE SCHEMA app; CREATE TYPE app.phase AS ENUM ('new', 'done');",
            "existing record value type is absent from the candidate catalogue",
        ),
    ] {
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let report = check_standard_application(&bundle([("records.orna", source)]), &context);
        assert_eq!(report.diagnostics(), &[], "{source}");
        assert!(matches!(
            prepare_standard_application(&report, active.pair(), &active),
            Err(PrepareStandardApplicationError::Prepare {
                source: PrepareError::InvalidCheckedBundle { reason: actual },
            }) if actual == reason
        ));
    }
}

#[test]
fn record_value_resolution_rejects_legacy_nested_object_and_duplicate_shapes() {
    let source = bundle([(
        "types.orna",
        "CREATE SCHEMA app; CREATE TYPE app.status AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;",
    )]);
    let legacy = check(&source, &empty_catalogue());
    assert_eq!(legacy.diagnostics().len(), 1);
    assert_eq!(
        legacy.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible
    );
    assert_eq!(
        legacy.diagnostics()[0].message(),
        "record value types require checked standard-library authority"
    );
    assert!(legacy.checked_bundle().is_none());

    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let invalid = bundle([(
        "invalid.orna",
        "CREATE SCHEMA app;\nCREATE TYPE app.object AS OBJECT ();\nCREATE TYPE app.first AS VALUE (duplicate BOOLEAN, duplicate BOOLEAN) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.second AS VALUE (nested app.first, object app.object) IMMUTABLE PERSISTABLE;",
    )]);
    let report = check_new_application(&invalid, &standard).unwrap();
    assert_eq!(
        report
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>(),
        vec![
            (
                DiagnosticCode::DuplicateDefinition,
                "duplicate record field definition duplicate in app.first",
            ),
            (
                DiagnosticCode::TypeMismatch,
                "object type app.object must be declared with REF",
            ),
        ]
    );
    assert!(report.checked_bundle().is_none());

    let collision = bundle([(
        "collision.orna",
        "CREATE SCHEMA app; CREATE TYPE app.same AS ENUM ('x'); CREATE TYPE app.same AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;",
    )]);
    let report = check_new_application(&collision, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DuplicateDefinition
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "duplicate record value type definition app.same"
    );

    let decimal_standard = checked_standard_library_with_contract_overrides_for_test(
        &snapshot,
        &[(0, "orna.kernel.value.decimal@1")],
    )
    .unwrap();
    let unsupported = check_new_application(&source, &decimal_standard).unwrap();
    assert_eq!(unsupported.diagnostics().len(), 1);
    assert_eq!(
        unsupported.diagnostics()[0].code(),
        DiagnosticCode::TypeMismatch
    );
    assert_eq!(
        unsupported.diagnostics()[0].message(),
        "record value field uses a type outside the initial record family"
    );

    let mut transient_standard = check_standard_library_source(&snapshot).unwrap();
    transient_standard
        .value_types
        .iter_mut()
        .find(|value_type| value_type.representation_contract == "orna.kernel.value.boolean@1")
        .unwrap()
        .persistence = ValueTypePersistence::Transient;
    let unsupported = check_new_application(&source, &transient_standard).unwrap();
    assert_eq!(unsupported.diagnostics().len(), 1);
    assert_eq!(
        unsupported.diagnostics()[0].code(),
        DiagnosticCode::TypeMismatch
    );
    assert_eq!(
        unsupported.diagnostics()[0].message(),
        "record value field uses a type outside the initial record family"
    );
}

#[test]
fn record_value_self_cycle_rejects_with_exact_orna0201_evidence() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source =
        "CREATE SCHEMA app; CREATE TYPE app.loop AS VALUE (next app.loop) IMMUTABLE PERSISTABLE;";
    let report = check_new_application(&bundle([("cycle.orna", source)]), &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.loop"
    );
    let start = source.find("AS VALUE (next ").unwrap() + "AS VALUE (next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.loop".len());
    assert_eq!(&source[start..start + "app.loop".len()], "app.loop");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_multi_record_cycle_reports_the_exact_closing_edge() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.a AS VALUE (left app.b) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.b AS VALUE (right app.a) IMMUTABLE PERSISTABLE;";
    let report = check_new_application(&bundle([("cycle.orna", source)]), &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.a"
    );
    let start = source.find("right app.a").unwrap() + "right ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.a".len());
    assert_eq!(&source[start..start + "app.a".len()], "app.a");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_cycle_phase_precedes_depth_validation() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..=32 {
        source.push_str(&format!(
            "CREATE TYPE app.d{index} AS VALUE (next app.d{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.d33 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    source.push_str(
            "CREATE TYPE app.c1 AS VALUE (next app.c2) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.c2 AS VALUE (next app.c1) IMMUTABLE PERSISTABLE;\n",
        );
    let bundle = SourceBundle::new([SourceUnit::new("cycle.orna", source.clone())]).unwrap();
    let report = check_new_application(&bundle, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.c1"
    );
    let start = source.find("next app.c1").unwrap() + "next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.c1".len());
    assert_eq!(&source[start..start + "app.c1".len()], "app.c1");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_depth_thirty_two_chain_is_accepted_and_prepared() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..32 {
        source.push_str(&format!(
            "CREATE TYPE app.r{index} AS VALUE (next app.r{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.r32 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    let bundle = SourceBundle::new([SourceUnit::new("chain.orna", source.clone())]).unwrap();
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let records = prepared.candidate().record_value_types();
    assert_eq!(records.len(), 33);
    let first = records
        .iter()
        .find(|record| record.name().to_string() == "app.r0")
        .unwrap();
    let second = records
        .iter()
        .find(|record| record.name().to_string() == "app.r1")
        .unwrap();
    let last = records
        .iter()
        .find(|record| record.name().to_string() == "app.r31")
        .unwrap();
    let leaf = records
        .iter()
        .find(|record| record.name().to_string() == "app.r32")
        .unwrap();
    assert_eq!(
        first.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(second.id())
    );
    assert_eq!(
        last.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(leaf.id())
    );
}

#[test]
fn record_value_depth_thirty_three_chain_rejects_the_r32_edge_exactly() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..=32 {
        source.push_str(&format!(
            "CREATE TYPE app.r{index} AS VALUE (next app.r{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.r33 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    let bundle = SourceBundle::new([SourceUnit::new("chain.orna", source.clone())]).unwrap();
    let report = check_new_application(&bundle, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value nesting exceeds 32 levels through app.r33"
    );
    let start = source.find("next app.r33").unwrap() + "next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.r33".len());
    assert_eq!(&source[start..start + "app.r33".len()], "app.r33");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_shared_acyclic_dag_is_accepted_and_prepared() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.d AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.b AS VALUE (next app.d) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.c AS VALUE (next app.d) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.a AS VALUE (left app.b, right app.c) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("dag.orna", source)]);
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let records = prepared.candidate().record_value_types();
    assert_eq!(records.len(), 4);
    let a = records
        .iter()
        .find(|record| record.name().to_string() == "app.a")
        .unwrap();
    let b = records
        .iter()
        .find(|record| record.name().to_string() == "app.b")
        .unwrap();
    let c = records
        .iter()
        .find(|record| record.name().to_string() == "app.c")
        .unwrap();
    let d = records
        .iter()
        .find(|record| record.name().to_string() == "app.d")
        .unwrap();
    assert_eq!(a.fields().len(), 2);
    assert_eq!(
        a.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(b.id())
    );
    assert_eq!(
        a.fields()[1].descriptor(),
        &orna_core::types::TypeDescriptor::named(c.id())
    );
    assert_eq!(
        b.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(d.id())
    );
    assert_eq!(
        c.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(d.id())
    );
}

#[test]
fn record_value_enum_named_field_remains_accepted_and_never_forms_a_graph_edge() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.phase AS ENUM ('new', 'done');\nCREATE TYPE app.status AS VALUE (phase app.phase) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("enum_field.orna", source)]);
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let records = prepared.candidate().record_value_types();
    assert_eq!(records.len(), 1);
    let status = &records[0];
    assert_eq!(status.name().to_string(), "app.status");
    assert_eq!(status.fields().len(), 1);
    let phase = prepared
        .candidate()
        .enum_types()
        .iter()
        .find(|enum_type| enum_type.name().to_string() == "app.phase")
        .unwrap();
    assert_eq!(
        status.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(phase.id())
    );
}

#[test]
fn record_value_cycle_selection_follows_source_and_field_order() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let z_source = "CREATE SCHEMA app;\nCREATE TYPE app.z1 AS VALUE (first app.z2, second app.z3) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.z2 AS VALUE (back app.z1) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.z3 AS VALUE (back app.z1) IMMUTABLE PERSISTABLE;\n";
    let a_source = "CREATE TYPE app.a1 AS VALUE (next app.a2) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.a2 AS VALUE (back app.a1) IMMUTABLE PERSISTABLE;\n";
    let bundle = SourceBundle::new([
        SourceUnit::new("z.orna", z_source),
        SourceUnit::new("a.orna", a_source),
    ])
    .unwrap();
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(
        diagnostic.code(),
        DiagnosticCode::TypeMismatch,
        "{}",
        diagnostic.message()
    );
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value fields must not form a recursive cycle through app.z1"
    );
    assert_eq!(diagnostic.location().logical_path(), "z.orna");
    let start = z_source.find("back app.z1").unwrap() + "back ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.z1".len());
    assert_eq!(&z_source[start..start + "app.z1".len()], "app.z1");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_depth_validation_revisits_a_shallow_cached_suffix() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let mut source = String::from(
        "CREATE SCHEMA app;\nCREATE TYPE app.x0 AS VALUE (next app.x1) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.x1 AS VALUE (next app.s0) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s0 AS VALUE (next app.s1) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s1 AS VALUE (next app.s2) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s2 AS VALUE (next app.s3) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.s3 AS VALUE (flag BOOLEAN) IMMUTABLE PERSISTABLE;\n",
    );
    for index in 0..29 {
        source.push_str(&format!(
            "CREATE TYPE app.y{index} AS VALUE (next app.y{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.y29 AS VALUE (next app.s0) IMMUTABLE PERSISTABLE;\n");
    let bundle = SourceBundle::new([SourceUnit::new("depth.orna", source.clone())]).unwrap();
    let report = check_new_application(&bundle, &standard).unwrap();
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "record value nesting exceeds 32 levels through app.s3"
    );
    let start = source.find("next app.s3").unwrap() + "next ".len();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(diagnostic.location().span().end(), start + "app.s3".len());
    assert_eq!(&source[start..start + "app.s3".len()], "app.s3");
    assert!(report.checked_bundle().is_none());
}

#[test]
fn record_value_resolution_binds_object_fields_and_server_rows_to_one_identity() {
    let snapshot = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    let source = bundle([(
        "records.orna",
        "CREATE SCHEMA app; \
             CREATE TYPE app.status AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE; \
             CREATE TYPE app.task AS OBJECT (status app.status NOT NULL); \
             CREATE SERVER FUNCTION app.read() RETURNS ROWS (status app.status) \
             TRANSACTION READ ONLY VOLATILITY STABLE \
             AS SELECT task.status FROM app.task task;",
    )]);

    let report = check_new_application(&source, &standard).unwrap();

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let record_type = checked.record_value_types().next().unwrap().id();
    assert_eq!(
        checked
            .object_types()
            .next()
            .unwrap()
            .fields()
            .next()
            .unwrap()
            .resolved_type()
            .named_type(),
        Some(record_type)
    );
    assert_eq!(
        checked
            .server_functions()
            .next()
            .unwrap()
            .return_columns()
            .next()
            .unwrap()
            .resolved_type()
            .named_type(),
        Some(record_type)
    );
}

#[test]
fn record_value_scalar_family_is_exact() {
    for scalar in [
        StandardScalar::Boolean,
        StandardScalar::Integer,
        StandardScalar::BigInt,
        StandardScalar::Float,
        StandardScalar::CharacterLargeObject,
        StandardScalar::BinaryLargeObject,
    ] {
        assert!(supports_record_value_scalar(scalar));
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
        assert!(!supports_record_value_scalar(scalar));
    }
}

#[test]
fn enum_and_object_declarations_share_one_resolved_type_namespace() {
    let report = check(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS OBJECT (); CREATE TYPE crm.stage AS ENUM ('lead');",
        )]),
        &empty_catalogue(),
    );

    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].message(),
        "duplicate enum type definition crm.stage"
    );
}

#[test]
fn resolves_application_enum_uses_as_named_values_and_rejects_ref() {
    let standard =
        check_standard_library_source(&verified_standard_library_for_relational_test()).unwrap();
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'qualified'); \
            CREATE TYPE crm.customer AS OBJECT (stage crm.stage NOT NULL);";
    let report = check_standard_application(&bundle([("types.orna", source)]), &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let enum_type = checked.inner.enum_types().next().unwrap().0;
    let object = checked.object_types().next().unwrap();
    let field = object.fields().next().unwrap();
    assert_eq!(field.resolved_type().named_type(), Some(enum_type));
    assert!(field.resolved_type().value().is_none());
    assert!(field.resolved_type().object_reference().is_none());
    let type_start = source.rfind("crm.stage").unwrap();
    assert_type_use_span(field.resolved_type(), type_start, "crm.stage");

    let rejected = check_standard_application(
        &bundle([(
            "types.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead'); \
                 CREATE TYPE crm.customer AS OBJECT (stage REF crm.stage);",
        )]),
        &context,
    );
    assert!(rejected.checked_bundle().is_none());
    assert_eq!(rejected.diagnostics().len(), 1);
    assert_eq!(
        rejected.diagnostics()[0].code(),
        DiagnosticCode::InvalidReferenceTarget
    );
    assert_eq!(
        rejected.diagnostics()[0].message(),
        "REF target crm.stage is an enum type"
    );
}

pub(super) fn standard_reconciliation_inputs(
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
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/types.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty());
    let parsed_unit = report.units()[0].clone();

    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        QualifiedSemanticName::new(["std", "types", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        boolean.id(),
    )
    .unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
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
        vec![boolean],
        vec![qualified.clone(), prelude.clone()],
    )
    .unwrap();
    let origins = vec![
        standard_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
            0,
            18,
        ),
        standard_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
            18,
            42,
        ),
        standard_origin(
            DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
            42,
            159,
        ),
        standard_origin(DefinitionIdentity::TypeBinding(qualified.id()), 159, 204),
        standard_origin(DefinitionIdentity::TypeBinding(prelude.id()), 204, 250),
    ];

    (stored_unit, parsed_unit, catalogue, origins)
}

pub(super) fn opaque_standard_reconciliation_inputs(
    source: &str,
    name: QualifiedSemanticName,
    contract: &str,
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
    let opaque = ValueTypeDefinition::opaque(TypeId::from_bytes([3; 16]), name, contract);
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([1; 16]),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![opaque],
        vec![],
    )
    .unwrap();
    let origins = vec![
        parsed_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
            &parsed_unit.parsed().schemas()[0].span,
        ),
        parsed_origin(
            DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
            &parsed_unit.parsed().opaque_value_types()[0].span,
        ),
    ];
    (stored_unit, parsed_unit, catalogue, origins)
}

pub(super) fn standard_origin(
    identity: DefinitionIdentity,
    byte_start: u32,
    byte_end: u32,
) -> DefinitionOrigin {
    DefinitionOrigin::new(
        identity,
        SourceOrigin::new(STANDARD_SOURCE_UNIT_ID, byte_start, byte_end).unwrap(),
    )
}

#[test]
fn accepts_nested_record_value_fields_with_provisional_and_durable_evidence() {
    let verified = verified_standard_library_for_relational_test();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let source = "CREATE SCHEMA app;\nCREATE TYPE app.outer AS VALUE (inner app.inner) IMMUTABLE PERSISTABLE;\nCREATE TYPE app.inner AS VALUE (active BOOLEAN) IMMUTABLE PERSISTABLE;";
    let bundle = bundle([("nested.orna", source)]);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let checked = report.checked_bundle().unwrap();
    let records = checked.record_value_types().collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].name().to_string(), "app.outer");
    assert_eq!(records[1].name().to_string(), "app.inner");
    let outer = records[0];
    let inner = records[1];
    let CheckedTypeId::Provisional(_) = outer.id() else {
        panic!("outer record must be provisional at check time");
    };
    let CheckedTypeId::Provisional(_) = inner.id() else {
        panic!("inner record must be provisional at check time");
    };

    let outer_fields = outer.fields().collect::<Vec<_>>();
    assert_eq!(outer_fields.len(), 1);
    assert_eq!(outer_fields[0].name(), "inner");
    let type_use = outer_fields[0].resolved_type();
    let CheckedTypeUseKind::Field { owner, field } = type_use.kind() else {
        panic!("outer field must carry Field type-use evidence");
    };
    assert_eq!(owner, outer.id());
    assert_eq!(field, outer_fields[0].id());
    assert_eq!(type_use.named_type(), Some(inner.id()));
    let span = type_use.location().span();
    assert_eq!(&source[span.start()..span.end()], "app.inner");

    let prepared = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let candidate = prepared.candidate();
    let durable_outer = candidate
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == "app.outer")
        .unwrap();
    let durable_inner = candidate
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == "app.inner")
        .unwrap();
    assert_eq!(
        durable_outer.fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(durable_inner.id())
    );
    let unit = prepared.source().units()[0].id();
    assert!(prepared.origins().iter().any(|origin| {
        origin.identity()
            == DefinitionIdentity::Field {
                owner: durable_outer.id(),
                field: durable_outer.fields()[0].id(),
            }
            && origin.source()
                == SourceOrigin::new(
                    unit,
                    u32::try_from(outer_fields[0].location().span().start()).unwrap(),
                    u32::try_from(outer_fields[0].location().span().end()).unwrap(),
                )
                .unwrap()
    }));
    assert!(prepared.origins().iter().any(|origin| {
        origin.identity() == DefinitionIdentity::ValueType(durable_inner.id())
            && origin.source()
                == SourceOrigin::new(
                    unit,
                    u32::try_from(inner.location().span().start()).unwrap(),
                    u32::try_from(inner.location().span().end()).unwrap(),
                )
                .unwrap()
    }));

    let active = active_from_prepared(&prepared);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let replay_report = check_standard_application(&bundle, &context);
    assert_eq!(replay_report.diagnostics(), &[]);
    let replay_checked = replay_report.checked_bundle().unwrap();
    let replay_records = replay_checked.record_value_types().collect::<Vec<_>>();
    assert_eq!(
        replay_records[0].id(),
        CheckedTypeId::Existing(durable_outer.id())
    );
    assert_eq!(
        replay_records[1].id(),
        CheckedTypeId::Existing(durable_inner.id())
    );
    let replay = prepare_standard_application(&replay_report, active.pair(), &active).unwrap();
    let replay_candidate = replay.candidate();
    assert_eq!(
        replay_candidate.record_value_types()[0].id(),
        durable_outer.id()
    );
    assert_eq!(
        replay_candidate.record_value_types()[1].id(),
        durable_inner.id()
    );
    assert_eq!(
        replay_candidate.record_value_types()[0].fields()[0].descriptor(),
        &orna_core::types::TypeDescriptor::named(durable_inner.id())
    );
}

pub(super) fn verified_standard_library_for_relational_test()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const DIGEST: [u8; 32] = [
        0x72, 0x4b, 0x41, 0xcf, 0x68, 0x5c, 0x93, 0xa8, 0xc9, 0x8d, 0xf9, 0x3d, 0x96, 0x77, 0x98,
        0x98, 0x12, 0x34, 0xc0, 0x98, 0xf6, 0xc1, 0x00, 0xfa, 0x57, 0xe9, 0xac, 0x00, 0xdd, 0x03,
        0xfb, 0x6d,
    ];
    verified_standard_library_for_relational_test_with_boolean_id(
        TypeId::from_bytes([3; 16]),
        DIGEST,
    )
}

pub(super) fn verified_standard_library_for_relational_test_with_boolean_id(
    boolean_id: TypeId,
    digest: [u8; 32],
) -> orna_core::revision::VerifiedStandardLibrarySnapshot {
    let source_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        STANDARD_SOURCE,
        source_unit_content_digest(STANDARD_SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([5; 16]),
        SourceRevisionId::from_bytes([6; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let boolean = ValueTypeDefinition::primitive(
        boolean_id,
        QualifiedSemanticName::new(["std", "types", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        boolean.id(),
    )
    .unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
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
        vec![boolean],
        vec![qualified.clone(), prelude.clone()],
    )
    .unwrap();
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        vec![
            standard_origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
                0,
                18,
            ),
            standard_origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
                18,
                42,
            ),
            standard_origin(DefinitionIdentity::ValueType(boolean_id), 42, 159),
            standard_origin(DefinitionIdentity::TypeBinding(qualified.id()), 159, 204),
            standard_origin(DefinitionIdentity::TypeBinding(prelude.id()), 204, 250),
        ],
        Sha256Digest::from_bytes(digest),
    )
    .unwrap();

    let digest = calculate_standard_library_digest(&snapshot).unwrap();
    let snapshot = StandardLibrarySnapshot::new(
        snapshot.revision(),
        snapshot.digest_version(),
        snapshot.source().clone(),
        snapshot.language_version(),
        snapshot.catalogue().clone(),
        snapshot.origins().to_vec(),
        digest,
    )
    .unwrap();
    verify_standard_library_snapshot(snapshot).unwrap()
}

pub(super) fn verified_standard_library_with_opaque_for_test()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = "CREATE SCHEMA std;CREATE TYPE std.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;CREATE TYPE std.TOKEN AS VALUE OPAQUE KERNEL CONTRACT 'std.token@1' IMMUTABLE TRANSIENT;";
    let parsed = parsed_standard_unit(SOURCE);
    let source_unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x32; 16]),
        SourceRevisionId::from_bytes([0x33; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x32; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([0x34; 16]),
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let opaque = ValueTypeDefinition::opaque(
        TypeId::from_bytes([0x35; 16]),
        QualifiedSemanticName::new(["std", "token"]).unwrap(),
        "std.token@1",
    );
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x36; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x37; 16]),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![boolean, opaque],
        vec![],
    )
    .unwrap();
    let source_unit = STANDARD_SOURCE_UNIT_ID;
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x37; 16])),
            SourceOrigin::new(
                source_unit,
                parsed.parsed().schemas()[0].span.start as u32,
                parsed.parsed().schemas()[0].span.end as u32,
            )
            .unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes([0x34; 16])),
            SourceOrigin::new(
                source_unit,
                parsed.parsed().primitive_value_types()[0].span.start as u32,
                parsed.parsed().primitive_value_types()[0].span.end as u32,
            )
            .unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes([0x35; 16])),
            SourceOrigin::new(
                source_unit,
                parsed.parsed().opaque_value_types()[0].span.start as u32,
                parsed.parsed().opaque_value_types()[0].span.end as u32,
            )
            .unwrap(),
        ),
    ];
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([0x38; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        origins,
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&snapshot).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            snapshot.revision(),
            snapshot.digest_version(),
            snapshot.source().clone(),
            snapshot.language_version(),
            snapshot.catalogue().clone(),
            snapshot.origins().to_vec(),
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

pub(super) fn verified_standard_library_with_action_for_test()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE SCHEMA std.action;CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.integer@1' IMMUTABLE PERSISTABLE;CREATE TYPE std.action.Action AS VALUE OPAQUE KERNEL CONTRACT 'orna.std.value.action@1' IMMUTABLE TRANSIENT;EXPORT TYPE std.types.INTEGER AS std.INTEGER;EXPORT TYPE std.action.Action AS std.Action;EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;";
    let parsed = parsed_standard_unit(SOURCE);
    let source_unit_id = STANDARD_SOURCE_UNIT_ID;
    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let integer = ValueTypeDefinition::primitive(
        TypeId::from_bytes([0x48; 16]),
        QualifiedSemanticName::new(["std", "types", "integer"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.integer@1",
    );
    let integer_id = integer.id();
    let action = ValueTypeDefinition::opaque(
        STD_ACTION_TYPE_ID,
        QualifiedSemanticName::new(["std", "action", "action"]).unwrap(),
        STD_ACTION_CONTRACT,
    );
    let integer_binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"]).unwrap(),
        integer_id,
    )
    .unwrap();
    let action_binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "action"]).unwrap(),
        action.id(),
    )
    .unwrap();
    let integer_prelude =
        TypeBinding::prelude(PreludeTypeName::new(["integer"]).unwrap(), integer_id).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([0x45; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([0x46; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([0x49; 16]),
                QualifiedSemanticName::new(["std", "action"]).unwrap(),
            ),
        ],
        vec![],
        vec![integer, action],
        vec![
            integer_binding.clone(),
            action_binding.clone(),
            integer_prelude.clone(),
        ],
    )
    .unwrap();
    let action_origin = |identity, byte_start, byte_end| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(source_unit_id, byte_start, byte_end).unwrap(),
        )
    };
    let origins = vec![
        action_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x45; 16])),
            parsed.parsed().schemas()[0].span.start as u32,
            parsed.parsed().schemas()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x46; 16])),
            parsed.parsed().schemas()[1].span.start as u32,
            parsed.parsed().schemas()[1].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0x49; 16])),
            parsed.parsed().schemas()[2].span.start as u32,
            parsed.parsed().schemas()[2].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::ValueType(integer_id),
            parsed.parsed().primitive_value_types()[0].span.start as u32,
            parsed.parsed().primitive_value_types()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
            parsed.parsed().opaque_value_types()[0].span.start as u32,
            parsed.parsed().opaque_value_types()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::TypeBinding(integer_binding.id()),
            parsed.parsed().type_exports()[0].span.start as u32,
            parsed.parsed().type_exports()[0].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::TypeBinding(action_binding.id()),
            parsed.parsed().type_exports()[1].span.start as u32,
            parsed.parsed().type_exports()[1].span.end as u32,
        ),
        action_origin(
            DefinitionIdentity::TypeBinding(integer_prelude.id()),
            parsed.parsed().type_exports()[2].span.start as u32,
            parsed.parsed().type_exports()[2].span.end as u32,
        ),
    ];
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([0x47; 16]),
        StandardLibraryDigestVersion::Version1,
        source.clone(),
        "orna.language/1",
        catalogue.clone(),
        origins.clone(),
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&snapshot).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            snapshot.revision(),
            snapshot.digest_version(),
            source,
            snapshot.language_version(),
            catalogue,
            origins,
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}
