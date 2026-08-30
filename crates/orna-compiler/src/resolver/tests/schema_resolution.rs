use super::*;

#[test]
fn resolves_forward_references_across_source_units() {
    let report = check(
        &bundle([
            (
                "tasks.orna",
                "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (assignee REF people.person);",
            ),
            (
                "people.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT NOT NULL);",
            ),
        ]),
        &empty_catalogue(),
    );

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.schemas()[0].name().to_string(), "tasks");
    assert_eq!(checked.schemas()[1].name().to_string(), "people");
    let task = &checked.object_types()[0];
    let person = &checked.object_types()[1];
    assert_eq!(
        task.fields()[0].semantic_type(),
        SemanticType::reference(person.id())
    );
    assert_eq!(task.id().to_string(), "provisional:type:0");
    assert_eq!(person.id().to_string(), "provisional:type:1");
}

#[test]
fn empty_schema_declaration_persists_with_a_stable_identity() {
    let schema_id = SchemaId::from_bytes([2; 16]);
    let base = catalogue(vec![schema(2, &["crm"])], Vec::new(), Vec::new());
    let report = check(&bundle([("schema.orna", "CREATE SCHEMA CRM;")]), &base);

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert_eq!(checked.base_catalogue_revision(), base.revision());
    assert_eq!(checked.schemas().len(), 1);
    assert_eq!(checked.schemas()[0].name().to_string(), "crm");
    assert_eq!(checked.schemas()[0].id().existing(), Some(schema_id));
}

#[test]
fn requires_submitted_schema_declarations_even_when_base_has_them() {
    let base = catalogue(vec![schema(1, &["crm"])], Vec::new(), Vec::new());

    let object_report = check(
        &bundle([(
            "types.orna",
            "CREATE TYPE crm.contact AS OBJECT (name TEXT);",
        )]),
        &base,
    );
    assert_eq!(object_report.diagnostics().len(), 1);
    assert_eq!(
        object_report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_no_checked_bundle(&object_report);

    let function_report = check(
        &bundle([(
            "functions.orna",
            "CREATE SERVER FUNCTION crm.probe_status() RETURNS ROWS (enabled BOOL) \
                 AS SELECT p.enabled FROM crm.probe p;",
        )]),
        &base,
    );
    assert_eq!(function_report.diagnostics().len(), 1);
    assert_eq!(
        function_report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_no_checked_bundle(&function_report);
}

#[test]
fn maps_alias_defaults_nullability_and_delete_policies() {
    let report = check(
        &bundle([(
            "schema.orna",
            "CREATE SCHEMA people; CREATE SCHEMA tasks;\
                 CREATE TYPE people.person AS OBJECT (name TEXT NOT NULL);\
                 CREATE TYPE tasks.task AS OBJECT (\
                     done BOOL NOT NULL DEFAULT FALSE,\
                     count INT DEFAULT 7,\
                     note TEXT DEFAULT 'it''s fine',\
                     owner REF people.person ON DELETE SET NULL,\
                     document TEXT,\
                     payload BYTES\
                 );",
        )]),
        &empty_catalogue(),
    );

    assert!(report.diagnostics().is_empty());
    let fields = report.checked_bundle().unwrap().object_types()[1].fields();
    assert_eq!(
        fields[0].semantic_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert!(!fields[0].nullable());
    assert_eq!(
        fields[0].default().unwrap().value(),
        &ConstantValue::Boolean(false)
    );
    assert_eq!(
        fields[1].semantic_type(),
        SemanticType::scalar(StandardScalar::Integer)
    );
    assert_eq!(
        fields[1].default().unwrap().value(),
        &ConstantValue::Integer(7)
    );
    assert_eq!(
        fields[2].default().unwrap().value(),
        &ConstantValue::Text("it's fine".to_owned())
    );
    assert!(fields[3].nullable());
    assert_eq!(fields[3].on_delete(), Some(OnDeleteAction::SetNull));
    assert_eq!(
        fields[4].semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert_eq!(
        fields[5].semantic_type(),
        SemanticType::scalar(StandardScalar::BinaryLargeObject)
    );
}

#[test]
fn rejects_non_public_large_object_aliases_at_their_type_spans() {
    for spelling in ["CLOB", "BLOB"] {
        let source =
            format!("CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (value {spelling});");
        let source_bundle =
            SourceBundle::new([SourceUnit::new("types.orna", source.as_str())]).unwrap();
        let report = check(&source_bundle, &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1, "{spelling}");
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            diagnostic.message(),
            format!("unknown type name {}", spelling.to_lowercase())
        );
        assert_eq!(diagnostic.location().logical_path(), "types.orna");
        let start = source.find(spelling).expect("type spelling is present");
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + spelling.len());
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn resolves_required_unique_references_with_forward_targets_and_replay_ids() {
    let source = "CREATE SCHEMA tasks; CREATE SCHEMA people; \
            CREATE TYPE tasks.assignment AS OBJECT (owner REF people.owner UNIQUE NOT NULL); \
            CREATE TYPE people.owner AS OBJECT ();";

    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let assignment = &checked.object_types()[0];
    let owner = &checked.object_types()[1];
    let field = &assignment.fields()[0];
    assert!(assignment.id().is_provisional());
    assert!(field.id().is_provisional());
    assert_eq!(field.semantic_type(), SemanticType::reference(owner.id()));
    assert!(!field.nullable());
    assert!(field.unique());

    let owner_id = TypeId::from_bytes([3; 16]);
    let assignment_id = TypeId::from_bytes([4; 16]);
    let owner_field = FieldId::from_bytes([5; 16]);
    let base = catalogue(
        vec![schema(1, &["people"]), schema(2, &["tasks"])],
        vec![
            object_type(
                4,
                &["tasks", "assignment"],
                vec![FieldDefinition::new(
                    owner_field,
                    "owner",
                    0,
                    ResolvedType::reference(owner_id),
                    false,
                    true,
                    None,
                    None,
                )],
            ),
            object_type(3, &["people", "owner"], Vec::new()),
        ],
        Vec::new(),
    );
    let replay = check(&bundle([("unique.orna", source)]), &base);

    assert!(replay.diagnostics().is_empty());
    let assignment = &replay.checked_bundle().unwrap().object_types()[0];
    let field = &assignment.fields()[0];
    assert_eq!(assignment.id().existing(), Some(assignment_id));
    assert_eq!(field.id().existing(), Some(owner_field));
    assert_eq!(
        field.semantic_type(),
        SemanticType::reference(CheckedTypeId::Existing(owner_id))
    );
}

#[test]
fn resolves_nullable_and_required_unique_text_with_required_unique_reference() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA crm; \
            CREATE TYPE crm.contact AS OBJECT (\
                email TEXT UNIQUE,\
                name CHARACTER LARGE OBJECT NOT NULL UNIQUE,\
                owner REF people.owner NOT NULL UNIQUE\
            ); \
            CREATE TYPE people.owner AS OBJECT ();";

    let report = check(&bundle([("unique_text.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let fields = report.checked_bundle().unwrap().object_types()[0].fields();
    assert_eq!(fields.len(), 3);
    assert_eq!(
        fields[0].semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(fields[0].nullable());
    assert!(fields[0].unique());
    assert_eq!(
        fields[1].semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(!fields[1].nullable());
    assert!(fields[1].unique());
    assert!(matches!(
        fields[2].semantic_type(),
        SemanticType::Reference { .. }
    ));
    assert!(!fields[2].nullable());
    assert!(fields[2].unique());
}

#[test]
fn rejects_unique_fields_outside_the_required_reference_shape() {
    for spelling in LEGACY_CANONICAL_SCALAR_SPELLINGS
        .iter()
        .copied()
        .filter(|spelling| *spelling != "CHARACTER LARGE OBJECT")
    {
        let source = format!(
            "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (value {} UNIQUE);",
            spelling
        );
        let bundle = SourceBundle::new([SourceUnit::new("unique.orna", source.clone())]).unwrap();
        let report = check(&bundle, &empty_catalogue());

        assert_eq!(report.diagnostics().len(), 1, "{source}");
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
        assert_eq!(diagnostic.code().as_str(), "ORNA0201");
        assert_eq!(
            diagnostic.message(),
            "UNIQUE is only available for TEXT fields or REF fields that are NOT NULL"
        );
        assert_eq!(diagnostic.location().logical_path(), "unique.orna");
        let start = source.find("value").unwrap();
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(
            diagnostic.location().span().end(),
            start + "value ".len() + spelling.len() + " UNIQUE".len()
        );
        assert_no_checked_bundle(&report);
    }

    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE tasks.assignment AS OBJECT (owner REF people.owner UNIQUE); \
            CREATE TYPE people.owner AS OBJECT ();";
    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "UNIQUE is only available for TEXT fields or REF fields that are NOT NULL"
    );
    let start = source.find("owner REF").unwrap();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(
        diagnostic.location().span().end(),
        start + "owner REF people.owner UNIQUE".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn unique_field_validation_preserves_existing_field_diagnostic_precedence() {
    let source = "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (\
            repeated TEXT, repeated TEXT UNIQUE,\
            missing REF demo.missing UNIQUE,\
            scalar_target REF TEXT UNIQUE,\
            deleted TEXT UNIQUE ON DELETE RESTRICT,\
            defaulted INT UNIQUE DEFAULT TRUE\
        );";
    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    let expected = [
        (
            DiagnosticCode::DuplicateDefinition,
            "duplicate field definition repeated in demo.item",
        ),
        (
            DiagnosticCode::UnknownQualifiedName,
            "unknown object type demo.missing",
        ),
        (
            DiagnosticCode::InvalidReferenceTarget,
            "REF target text is a scalar type",
        ),
        (
            DiagnosticCode::TypeMismatch,
            "ON DELETE is only valid for REF fields",
        ),
        (
            DiagnosticCode::TypeMismatch,
            "UNIQUE is only available for TEXT fields or REF fields that are NOT NULL",
        ),
        (
            DiagnosticCode::TypeMismatch,
            "default constant does not match the field type and nullability",
        ),
    ];
    assert_eq!(report.diagnostics().len(), expected.len());
    for (diagnostic, (code, message)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.code(), code);
        assert_eq!(diagnostic.message(), message);
    }
    assert_no_checked_bundle(&report);
}

#[test]
fn required_unique_reference_preserves_set_null_diagnostic_precedence() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE tasks.assignment AS OBJECT (\
                owner REF people.owner NOT NULL UNIQUE ON DELETE SET NULL\
            ); \
            CREATE TYPE people.owner AS OBJECT ();";
    let report = check(&bundle([("unique.orna", source)]), &empty_catalogue());

    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.code().as_str(), "ORNA0201");
    assert_eq!(
        diagnostic.message(),
        "ON DELETE SET NULL requires a nullable field"
    );
    assert_eq!(diagnostic.location().logical_path(), "unique.orna");
    let start = source.find("owner REF").unwrap();
    assert_eq!(diagnostic.location().span().start(), start);
    assert_eq!(
        diagnostic.location().span().end(),
        start + "owner REF people.owner NOT NULL UNIQUE ON DELETE SET NULL".len()
    );
    assert_no_checked_bundle(&report);
}

#[test]
fn unique_text_or_required_reference_support_is_closed_to_accepted_shapes() {
    let type_id = CheckedTypeId::Existing(TypeId::from_bytes([1; 16]));

    assert!(super::super::supports_unique_text_or_required_reference(
        SemanticType::reference(type_id),
        false
    ));
    assert!(!super::super::supports_unique_text_or_required_reference(
        SemanticType::reference(type_id),
        true
    ));
    assert!(super::super::supports_unique_text_or_required_reference(
        SemanticType::scalar(StandardScalar::CharacterLargeObject),
        true
    ));
    assert!(super::super::supports_unique_text_or_required_reference(
        SemanticType::scalar(StandardScalar::CharacterLargeObject),
        false
    ));
    assert!(!super::super::supports_unique_text_or_required_reference(
        SemanticType::Named(type_id),
        false
    ));
    for scalar in StandardScalar::ALL {
        assert_eq!(
            super::super::supports_unique_text_or_required_reference(
                SemanticType::scalar(scalar),
                false
            ),
            scalar == StandardScalar::CharacterLargeObject
        );
    }
}

#[test]
fn resolves_canonical_multiword_large_object_scalars() {
    let report = check(
        &bundle([(
            "schema.orna",
            "CREATE SCHEMA files; CREATE TYPE files.document AS OBJECT (body cHaRaCtEr /* retained */ LaRgE ObJeCt, content bInArY LARGE object);",
        )]),
        &empty_catalogue(),
    );

    assert!(report.diagnostics().is_empty());
    let fields = report.checked_bundle().unwrap().object_types()[0].fields();
    assert_eq!(
        fields[0].semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert_eq!(
        fields[1].semantic_type(),
        SemanticType::scalar(StandardScalar::BinaryLargeObject)
    );
}

#[test]
fn repeated_checks_preserve_matching_ids_even_when_fields_reorder() {
    let name_id = FieldId::from_bytes([3; 16]);
    let age_id = FieldId::from_bytes([4; 16]);
    let default_id = ExpressionId::from_bytes([5; 16]);
    let base = catalogue(
        vec![schema(1, &["people"])],
        vec![object_type(
            2,
            &["people", "person"],
            vec![
                field(
                    3,
                    "name",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                ),
                field(
                    4,
                    "age",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                    Some(default_id),
                ),
            ],
        )],
        Vec::new(),
    );

    let report = check(
        &bundle([(
            "renamed-file.orna",
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (age INT DEFAULT 1, name TEXT);",
        )]),
        &base,
    );

    assert!(report.diagnostics().is_empty());
    let revised = &report.checked_bundle().unwrap().object_types()[0];
    assert_eq!(revised.id().existing(), Some(TypeId::from_bytes([2; 16])));
    assert_eq!(revised.fields()[0].name(), "age");
    assert_eq!(revised.fields()[0].id().existing(), Some(age_id));
    assert_eq!(revised.fields()[1].name(), "name");
    assert_eq!(revised.fields()[1].id().existing(), Some(name_id));
    assert_eq!(
        revised.fields()[0].default().unwrap().id().existing(),
        Some(default_id)
    );
}

#[test]
fn added_field_gets_a_new_identity() {
    let name_id = FieldId::from_bytes([3; 16]);
    let base = catalogue(
        vec![schema(1, &["people"])],
        vec![object_type(
            2,
            &["people", "person"],
            vec![field(
                3,
                "name",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            )],
        )],
        Vec::new(),
    );
    let report = check(
        &bundle([(
            "schema.orna",
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT, email TEXT);",
        )]),
        &base,
    );

    assert!(report.diagnostics().is_empty());
    let revised = &report.checked_bundle().unwrap().object_types()[0];
    assert_eq!(revised.fields()[0].id().existing(), Some(name_id));
    assert_eq!(revised.fields()[1].id().to_string(), "provisional:field:0");
}

fn rename_base(fields: Vec<FieldDefinition>) -> CatalogueSnapshot {
    catalogue(
        vec![schema(1, &["people"])],
        vec![object_type(2, &["people", "person"], fields)],
        Vec::new(),
    )
}

#[test]
fn field_rename_binds_the_old_identity_default_and_quoted_name() {
    let field_id = FieldId::from_bytes([3; 16]);
    let expression_id = ExpressionId::from_bytes([4; 16]);
    let base = rename_base(vec![field(
        3,
        "Email",
        0,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        Some(expression_id),
    )]);
    let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (\"Primary Email\" TEXT DEFAULT 'x'); ALTER TYPE people.person RENAME FIELD \"Email\" TO \"Primary Email\";";

    let report = check(&bundle([("rename.orna", source)]), &base);

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let field = &checked.object_types()[0].fields()[0];
    assert_eq!(field.id().existing(), Some(field_id));
    assert_eq!(
        field.default().unwrap().id().existing(),
        Some(expression_id)
    );
    assert_eq!(field.name(), "Primary Email");
    assert_eq!(checked.field_renames().len(), 1);
    assert_eq!(checked.field_renames()[0].old_name, "Email");
    assert_eq!(checked.field_renames()[0].new_name, "Primary Email");
}

#[test]
fn field_rename_is_source_order_independent_and_replay_safe() {
    let field_id = FieldId::from_bytes([3; 16]);
    let base = rename_base(vec![field(
        3,
        "email",
        0,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        None,
    )]);
    let create_then_alter = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;";
    let alter_then_create = "ALTER TYPE people.person RENAME FIELD email TO primary_email; CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT);";
    let first = check(&bundle([("rename.orna", create_then_alter)]), &base);
    let second = check(&bundle([("rename.orna", alter_then_create)]), &base);
    let first_checked = first.checked_bundle().unwrap();
    let second_checked = second.checked_bundle().unwrap();
    assert_eq!(
        first_checked.object_types()[0].id(),
        second_checked.object_types()[0].id()
    );
    assert_eq!(
        first_checked.object_types()[0].fields()[0].id(),
        second_checked.object_types()[0].fields()[0].id()
    );
    assert_eq!(
        first_checked.field_renames(),
        second_checked.field_renames()
    );
    let replay_base = rename_base(vec![field(
        3,
        "primary_email",
        0,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        None,
    )]);
    let replay = check(&bundle([("rename.orna", create_then_alter)]), &replay_base);
    assert!(replay.diagnostics().is_empty());
    assert_eq!(
        replay.checked_bundle().unwrap().object_types()[0].fields()[0]
            .id()
            .existing(),
        Some(field_id)
    );
}

#[test]
fn replacing_a_same_shape_field_without_a_rename_is_provisional() {
    let base = rename_base(vec![field(
        3,
        "email",
        0,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        None,
    )]);
    let report = check(
        &bundle([(
            "rename.orna",
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT);",
        )]),
        &base,
    );
    assert!(report.diagnostics().is_empty());
    assert!(
        report.checked_bundle().unwrap().object_types()[0].fields()[0]
            .id()
            .is_provisional()
    );
}

#[test]
fn field_rename_rejects_a_base_without_either_name() {
    let base = rename_base(vec![field(
        3,
        "other",
        0,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        None,
    )]);
    let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;";
    let report = check(&bundle([("rename.orna", source)]), &base);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
    assert_eq!(
        diagnostic.message(),
        "object type people.person has no field named email"
    );
    let old = source.find("RENAME FIELD email").unwrap() + "RENAME FIELD ".len();
    assert_eq!(diagnostic.location().span().start(), old);
    assert_eq!(diagnostic.location().span().end(), old + "email".len());
    assert_no_checked_bundle(&report);
}

#[test]
fn invalid_rename_owners_take_precedence_over_chain_detection() {
    let source = "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (last TEXT); ALTER TYPE people.missing RENAME FIELD email TO first; ALTER TYPE people.missing RENAME FIELD first TO last;";
    let report = check(&bundle([("rename.orna", source)]), &empty_catalogue());
    assert_eq!(report.diagnostics().len(), 2);
    for diagnostic in report.diagnostics() {
        assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(
            diagnostic.message(),
            "object type people.missing must be declared in this source"
        );
    }
    assert_no_checked_bundle(&report);
}

#[test]
fn field_rename_negative_contracts_use_exact_diagnostics() {
    struct Case {
        source: &'static str,
        base: CatalogueSnapshot,
        name: &'static str,
        code: DiagnosticCode,
        message: &'static str,
    }
    let old = || {
        field(
            3,
            "email",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        )
    };
    let new = || {
        field(
            4,
            "primary_email",
            1,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            None,
        )
    };
    let cases = vec![
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (email TEXT); ALTER TYPE people.person RENAME FIELD email TO email;",
            base: rename_base(vec![old()]),
            name: "email",
            code: DiagnosticCode::DuplicateDefinition,
            message: "field email cannot be renamed to the same name",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
            base: catalogue(vec![schema(1, &["people"])], Vec::new(), Vec::new()),
            name: "people.person",
            code: DiagnosticCode::UnknownQualifiedName,
            message: "field rename requires existing object type people.person",
        },
        Case {
            source: "CREATE SCHEMA people; ALTER TYPE people.person RENAME FIELD email TO primary_email;",
            base: rename_base(vec![old()]),
            name: "people.person",
            code: DiagnosticCode::UnknownQualifiedName,
            message: "object type people.person must be declared in this source",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (other TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
            base: rename_base(vec![old()]),
            name: "primary_email",
            code: DiagnosticCode::UnknownQualifiedName,
            message: "object type people.person must declare renamed field primary_email",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
            base: rename_base(vec![old()]),
            name: "email",
            code: DiagnosticCode::DuplicateDefinition,
            message: "object type people.person still declares old field email",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email;",
            base: rename_base(vec![old(), new()]),
            name: "primary_email",
            code: DiagnosticCode::DuplicateDefinition,
            message: "object type people.person already has a different field named primary_email",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (first TEXT, primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email; ALTER TYPE people.person RENAME FIELD email TO first;",
            base: rename_base(vec![old()]),
            name: "email",
            code: DiagnosticCode::DuplicateDefinition,
            message: "field email is renamed more than once",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (first TEXT, primary_email TEXT); ALTER TYPE people.person RENAME FIELD email TO primary_email; ALTER TYPE people.person RENAME FIELD first TO primary_email;",
            base: rename_base(vec![
                old(),
                field(
                    5,
                    "first",
                    1,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                ),
            ]),
            name: "primary_email",
            code: DiagnosticCode::DuplicateDefinition,
            message: "more than one field is renamed to primary_email",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (last TEXT); ALTER TYPE people.person RENAME FIELD email TO first; ALTER TYPE people.person RENAME FIELD first TO last;",
            base: rename_base(vec![
                old(),
                field(
                    5,
                    "first",
                    1,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                ),
            ]),
            name: "first",
            code: DiagnosticCode::DuplicateDefinition,
            message: "field rename chain or swap is not supported: email to first",
        },
        Case {
            source: "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (email TEXT, first TEXT); ALTER TYPE people.person RENAME FIELD email TO first; ALTER TYPE people.person RENAME FIELD first TO email;",
            base: rename_base(vec![
                old(),
                field(
                    5,
                    "first",
                    1,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                ),
            ]),
            name: "first",
            code: DiagnosticCode::DuplicateDefinition,
            message: "field rename chain or swap is not supported: email to first",
        },
    ];
    for case in cases {
        let report = check(&bundle([("rename.orna", case.source)]), &case.base);
        assert_eq!(report.diagnostics().len(), 1, "{}", case.message);
        let diagnostic = &report.diagnostics()[0];
        assert_eq!(diagnostic.code(), case.code, "{}", case.source);
        assert_eq!(diagnostic.message(), case.message);
        let start = if case.message == "field email cannot be renamed to the same name"
            || case.message == "field email is renamed more than once"
            || case.message == "object type people.person still declares old field email"
        {
            case.source.rfind("RENAME FIELD email").unwrap() + "RENAME FIELD ".len()
        } else if case.message == "more than one field is renamed to primary_email" {
            case.source
                .rfind("RENAME FIELD first TO primary_email")
                .unwrap()
                + "RENAME FIELD first TO ".len()
        } else if case.message.starts_with("field rename chain or swap") {
            case.source.find("RENAME FIELD email TO").unwrap() + "RENAME FIELD email TO ".len()
        } else {
            case.source.rfind(case.name).unwrap()
        };
        assert_eq!(
            diagnostic.location().span().start(),
            start,
            "{}",
            case.source
        );
        assert_eq!(diagnostic.location().span().end(), start + case.name.len());
        assert_no_checked_bundle(&report);
    }
}

#[test]
fn identical_checks_return_equal_checked_bundles() {
    let source = "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (value INT DEFAULT 1);";
    let first = check(&bundle([("demo.orna", source)]), &empty_catalogue());
    let second = check(&bundle([("demo.orna", source)]), &empty_catalogue());

    assert!(first.diagnostics().is_empty());
    assert_eq!(first.checked_bundle(), second.checked_bundle());
}

#[test]
fn syntax_errors_do_not_return_a_checked_bundle() {
    let report = check(
        &bundle([("broken.orna", "CREATE SCHEMA ;")]),
        &empty_catalogue(),
    );

    assert_no_checked_bundle(&report);
}

#[test]
fn assigns_exact_kind_local_provisional_counters() {
    let source = "CREATE SCHEMA alpha; CREATE SCHEMA beta; \
            CREATE TYPE alpha.one AS OBJECT (number INT DEFAULT 1); \
            CREATE TYPE beta.two AS OBJECT (one REF alpha.one, number INT DEFAULT 2); \
            CREATE SERVER FUNCTION alpha.first(p_one REF alpha.one) \
            RETURNS ROWS (number INT) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT o.number FROM alpha.one o WHERE REF(o) = p_one; \
            CREATE SERVER FUNCTION beta.second(p_two REF beta.two) \
            RETURNS ROWS (number INT) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.number FROM beta.two t WHERE REF(t) = p_two;";
    let report = check(&bundle([("counters.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert_eq!(
        checked.schemas()[0].id().to_string(),
        "provisional:schema:0"
    );
    assert_eq!(
        checked.schemas()[1].id().to_string(),
        "provisional:schema:1"
    );
    assert_eq!(
        checked.object_types()[0].id().to_string(),
        "provisional:type:0"
    );
    assert_eq!(
        checked.object_types()[1].id().to_string(),
        "provisional:type:1"
    );
    assert_eq!(
        checked.object_types()[0].fields()[0].id().to_string(),
        "provisional:field:0"
    );
    assert_eq!(
        checked.object_types()[1].fields()[0].id().to_string(),
        "provisional:field:1"
    );
    assert_eq!(
        checked.object_types()[1].fields()[1].id().to_string(),
        "provisional:field:2"
    );
    assert_eq!(
        checked.object_types()[0].fields()[0]
            .default()
            .unwrap()
            .id()
            .to_string(),
        "provisional:expression:0"
    );
    assert_eq!(
        checked.object_types()[1].fields()[1]
            .default()
            .unwrap()
            .id()
            .to_string(),
        "provisional:expression:1"
    );
    assert_eq!(
        checked.server_functions()[0].id().to_string(),
        "provisional:function:0"
    );
    assert_eq!(
        checked.server_functions()[1].id().to_string(),
        "provisional:function:1"
    );
    assert_eq!(
        checked.server_functions()[0].parameters()[0]
            .id()
            .to_string(),
        "provisional:parameter:0"
    );
    assert_eq!(
        checked.server_functions()[1].parameters()[0]
            .id()
            .to_string(),
        "provisional:parameter:1"
    );
}

#[test]
fn preserves_existing_schema_type_field_default_function_and_parameter_identities() {
    let schema_id = SchemaId::from_bytes([1; 16]);
    let type_id = TypeId::from_bytes([2; 16]);
    let field_id = FieldId::from_bytes([3; 16]);
    let default_id = ExpressionId::from_bytes([4; 16]);
    let function_id = FunctionId::from_bytes([5; 16]);
    let parameter_id = ParameterId::from_bytes([6; 16]);
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
                Some(default_id),
            )],
        )],
        vec![server_function(
            5,
            &["tasks", "open"],
            vec![parameter(6, "p_task", 0, ResolvedType::reference(type_id))],
            vec![rows_column(
                "title",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            )],
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    );
    let report = check(
        &bundle([(
            "tasks.orna",
            "CREATE SCHEMA TASKS; CREATE TYPE tasks.task AS OBJECT (title TEXT DEFAULT 'old'); \
                 CREATE SERVER FUNCTION TASKS.OPEN(P_TASK REF tasks.task) RETURNS ROWS (title TEXT) \
                 SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
                 AS SELECT t.title FROM tasks.task t WHERE REF(t) = P_TASK;",
        )]),
        &base,
    );

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert_eq!(checked.schemas()[0].id().existing(), Some(schema_id));
    assert_eq!(checked.object_types()[0].id().existing(), Some(type_id));
    assert_eq!(
        checked.object_types()[0].fields()[0].id().existing(),
        Some(field_id)
    );
    assert_eq!(
        checked.object_types()[0].fields()[0]
            .default()
            .unwrap()
            .id()
            .existing(),
        Some(default_id)
    );
    assert_eq!(
        checked.server_functions()[0].id().existing(),
        Some(function_id)
    );
    assert_eq!(
        checked.server_functions()[0].parameters()[0]
            .id()
            .existing(),
        Some(parameter_id)
    );
}

#[test]
fn distinct_new_defaults_receive_distinct_provisional_expression_ids() {
    let report = check(
        &bundle([(
            "defaults.orna",
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (first INT DEFAULT 1, second INT DEFAULT 2);",
        )]),
        &empty_catalogue(),
    );

    assert!(report.diagnostics().is_empty());
    let fields = report.checked_bundle().unwrap().object_types()[0].fields();
    assert_eq!(
        fields[0].default().unwrap().id().to_string(),
        "provisional:expression:0"
    );
    assert_eq!(
        fields[1].default().unwrap().id().to_string(),
        "provisional:expression:1"
    );
    assert_ne!(
        fields[0].default().unwrap().id(),
        fields[1].default().unwrap().id()
    );
}

#[test]
fn checked_function_plan_uses_checked_type_and_field_ids() {
    let report = check(
        &bundle([(
            "tasks.orna",
            "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
                 CREATE SERVER FUNCTION tasks.open() RETURNS ROWS (title TEXT) \
                 AS SELECT t.title FROM tasks.task t;",
        )]),
        &empty_catalogue(),
    );

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let task = &checked.object_types()[0];
    let title = &task.fields()[0];
    let plan = checked.server_functions()[0]
        .query_plan()
        .expect("fixture has a SELECT body");
    assert_eq!(plan.scan().object_type(), task.id());
    let ExpressionKind::FieldPath { steps, .. } = plan.projections()[0].kind() else {
        panic!("expected a field projection");
    };
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].owner(), task.id());
    assert_eq!(steps[0].field(), title.id());
    assert_eq!(
        plan.projections()[0].value_type().semantic_type(),
        title.semantic_type()
    );
}

#[test]
fn resolves_unique_text_selected_query_with_separate_plan_and_evidence() {
    let source = "CREATE SCHEMA people; \
            CREATE TYPE people.person AS OBJECT (email TEXT UNIQUE, name TEXT); \
            CREATE SERVER FUNCTION people.by_email(p_email TEXT) \
            RETURNS ROWS (name TEXT) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT p.name FROM people.person p WHERE p.email = p_email;";
    let report = check(&bundle([("unique_text.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let person = &checked.object_types()[0];
    let email = &person.fields()[0];
    let function = &checked.server_functions()[0];
    let plan = function
        .unique_text_selected_query_plan()
        .expect("fixture has a unique-Text-selected SELECT body");
    assert!(function.query_plan().is_none());
    assert!(function.distinct_query_plan().is_none());
    assert!(function.identity_selected_query_plan().is_none());
    assert!(function.mutation_plan().is_none());
    assert!(function.delete_plan().is_none());
    assert_eq!(plan.scan().object_type(), person.id());
    assert_eq!(plan.selector().scan_object_type(), person.id());
    assert_eq!(plan.selector().field_owner(), person.id());
    assert_eq!(plan.selector().field(), email.id());
    assert_eq!(plan.selector().parameter_owner(), function.id());
    assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
    assert_eq!(
        plan.selector().text_type().semantic_type(),
        SemanticType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(plan.selector().field_nullable());
    assert!(plan.selector().parameter_required_non_null());

    let selector_field_start = source.rfind("p.email").unwrap() + 2;
    let parameter_start = source.rfind("p_email").unwrap();
    assert!(function.references().iter().any(|reference| {
        reference.kind() == DefinitionReferenceKind::QueryField
            && reference.target()
                == CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: email.id(),
                }
            && reference.location().span().start() == selector_field_start
    }));
    assert!(function.references().iter().any(|reference| {
        reference.kind() == DefinitionReferenceKind::ParameterRead
            && reference.target()
                == CheckedDefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                }
            && reference.location().span().start() == parameter_start
    }));
}

#[test]
fn records_signature_and_identity_selected_query_references_in_order_with_exact_spans() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE TYPE tasks.task AS OBJECT (assignee REF people.person, completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.find(p_task REF tasks.task) \
            RETURNS ROWS (task REF tasks.task, name TEXT) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(t), t.assignee.name FROM tasks.task t \
            WHERE REF(t) = p_task;";
    let report = check(&bundle([("references.orna", source)]), &empty_catalogue());

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let person = &checked.object_types()[0];
    let task = &checked.object_types()[1];
    let assignee = &task.fields()[0];
    let name = &person.fields()[0];
    let function = &checked.server_functions()[0];
    let plan = function
        .identity_selected_query_plan()
        .expect("fixture has an identity-selected SELECT body");
    assert!(function.query_plan().is_none());
    assert!(function.distinct_query_plan().is_none());
    assert_eq!(plan.scan().object_type(), task.id());
    assert_eq!(plan.selector().owner(), function.id());
    assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
    assert_eq!(plan.projections().len(), 2);
    let query_start = source.find("SELECT REF(t)").unwrap();
    let assignee_start = source.find("t.assignee.name").unwrap();
    let parameter_target_start =
        source.find("p_task REF tasks.task").unwrap() + "p_task REF ".len();
    let return_target_start =
        source.find("RETURNS ROWS (task REF tasks.task").unwrap() + "RETURNS ROWS (task REF ".len();
    let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
    let projection_reference_start =
        query_start + source[query_start..].find("REF(t)").unwrap() + 4;
    let selector_reference_start = source.rfind("REF(t)").unwrap() + 4;
    let parameter_read_start = source.rfind("p_task").unwrap();
    let expected = [
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            parameter_target_start,
            "tasks.task".len(),
        ),
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            return_target_start,
            "tasks.task".len(),
        ),
        (
            DefinitionReferenceKind::QueryObject,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            query_object_start,
            "tasks.task".len(),
        ),
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            projection_reference_start,
            1,
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: assignee.id(),
            },
            assignee_start + 2,
            "assignee".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: person.id(),
                field: name.id(),
            },
            assignee_start + 11,
            "name".len(),
        ),
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            selector_reference_start,
            1,
        ),
        (
            DefinitionReferenceKind::ParameterRead,
            CheckedDefinitionReferenceTarget::Parameter {
                owner: function.id(),
                parameter: function.parameters()[0].id(),
            },
            parameter_read_start,
            "p_task".len(),
        ),
    ];

    assert_eq!(
        function.parameters()[0].location().span().start(),
        source.find("p_task REF").unwrap()
    );
    assert_eq!(
        function.return_columns()[0].location().span().start(),
        source.find("RETURNS ROWS (task REF").unwrap() + "RETURNS ROWS (".len()
    );
    assert_eq!(function.references().len(), expected.len());
    for (reference, (kind, target, start, length)) in function.references().iter().zip(expected) {
        assert_eq!(reference.kind(), kind);
        assert_eq!(reference.target(), target);
        assert_eq!(reference.location().logical_path(), "references.orna");
        assert_eq!(reference.location().span().start(), start);
        assert_eq!(reference.location().span().end(), start + length);
    }
}

#[test]
fn preserves_v1_signature_and_query_references_in_order_with_exact_spans() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (name TEXT); \
            CREATE TYPE tasks.task AS OBJECT (assignee REF people.person, completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.find() \
            RETURNS ROWS (task REF tasks.task, name TEXT) \
            AS SELECT REF(t), t.assignee.name FROM tasks.task t \
            WHERE t.completed = t.completed ORDER BY t.assignee.name DESC;";
    let report = check(
        &bundle([("v1_references.orna", source)]),
        &empty_catalogue(),
    );

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let person = &checked.object_types()[0];
    let task = &checked.object_types()[1];
    let assignee = &task.fields()[0];
    let completed = &task.fields()[1];
    let name = &person.fields()[0];
    let function = &checked.server_functions()[0];
    assert!(function.query_plan().is_some());
    assert!(function.identity_selected_query_plan().is_none());
    assert!(function.distinct_query_plan().is_none());
    let query_start = source.find("SELECT REF(t)").unwrap();
    let assignee_starts = source
        .match_indices("t.assignee.name")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    let completed_starts = source
        .match_indices("t.completed")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    let return_target_start = source.find("task REF tasks.task").unwrap() + "task REF ".len();
    let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
    let object_reference_start = query_start + source[query_start..].find("REF(t)").unwrap() + 4;
    let expected = [
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            return_target_start,
            "tasks.task".len(),
        ),
        (
            DefinitionReferenceKind::QueryObject,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            query_object_start,
            "tasks.task".len(),
        ),
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            object_reference_start,
            1,
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: assignee.id(),
            },
            assignee_starts[0] + 2,
            "assignee".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: person.id(),
                field: name.id(),
            },
            assignee_starts[0] + 11,
            "name".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: completed.id(),
            },
            completed_starts[0] + 2,
            "completed".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: completed.id(),
            },
            completed_starts[1] + 2,
            "completed".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: assignee.id(),
            },
            assignee_starts[1] + 2,
            "assignee".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: person.id(),
                field: name.id(),
            },
            assignee_starts[1] + 11,
            "name".len(),
        ),
    ];

    assert_eq!(
        function.return_columns()[0].location().span().start(),
        source.find("task REF").unwrap()
    );
    assert_eq!(function.references().len(), expected.len());
    for (reference, (kind, target, start, length)) in function.references().iter().zip(expected) {
        assert_eq!(reference.kind(), kind);
        assert_eq!(reference.target(), target);
        assert_eq!(reference.location().logical_path(), "v1_references.orna");
        assert_eq!(reference.location().span().start(), start);
        assert_eq!(reference.location().span().end(), start + length);
    }
}

#[test]
fn records_direct_boolean_predicate_paths_after_projections_with_exact_spans() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (active BOOL NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (owner REF people.person, enabled BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.enabled() RETURNS ROWS (enabled BOOL) \
            AS SELECT t.enabled FROM tasks.task t WHERE t.enabled; \
            CREATE SERVER FUNCTION tasks.active() RETURNS ROWS (active BOOL) \
            AS SELECT t.owner.active FROM tasks.task t WHERE t.owner.active;";
    let report = check(
        &bundle([("direct_predicates.orna", source)]),
        &empty_catalogue(),
    );

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report
        .checked_bundle()
        .expect("direct predicates must check");
    let person = &checked.object_types()[0];
    let task = &checked.object_types()[1];
    let owner = &task.fields()[0];
    let enabled = &task.fields()[1];
    let active = &person.fields()[0];
    let enabled_function = &checked.server_functions()[0];
    let active_function = &checked.server_functions()[1];

    let enabled_starts = source
        .match_indices("t.enabled")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    assert_eq!(enabled_starts.len(), 2);
    assert_eq!(
        enabled_function
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: enabled.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: enabled.id(),
                },
            ),
        ]
    );
    for (reference, start) in enabled_function
        .references()
        .iter()
        .skip(1)
        .zip(enabled_starts)
    {
        assert_eq!(
            reference.location().logical_path(),
            "direct_predicates.orna"
        );
        assert_eq!(reference.location().span().start(), start + "t.".len());
        assert_eq!(reference.location().span().end(), start + "t.enabled".len());
    }

    let active_starts = source
        .match_indices("t.owner.active")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    assert_eq!(active_starts.len(), 2);
    assert_eq!(
        active_function
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: owner.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: active.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: owner.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                CheckedDefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: active.id(),
                },
            ),
        ]
    );
    let active_plan = active_function
        .query_plan()
        .expect("direct Boolean function must use the v1 query plan");
    assert!(active_plan.selection().is_some());
    assert!(active_plan.selection().unwrap().value_type().nullable());
    let expected_spans = [
        (active_starts[0] + 2, "owner".len()),
        (active_starts[0] + 8, "active".len()),
        (active_starts[1] + 2, "owner".len()),
        (active_starts[1] + 8, "active".len()),
    ];
    for (reference, (start, length)) in active_function
        .references()
        .iter()
        .skip(1)
        .zip(expected_spans)
    {
        assert_eq!(reference.location().span().start(), start);
        assert_eq!(reference.location().span().end(), start + length);
    }
}

#[test]
fn direct_boolean_literals_add_no_predicate_references() {
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (enabled BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.all_tasks() RETURNS ROWS (enabled BOOL) \
            AS SELECT t.enabled FROM tasks.task t WHERE TRUE; \
            CREATE SERVER FUNCTION tasks.no_tasks() RETURNS ROWS (enabled BOOL) \
            AS SELECT t.enabled FROM tasks.task t WHERE FALSE;";
    let report = check(&bundle([("literals.orna", source)]), &empty_catalogue());

    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let checked = report
        .checked_bundle()
        .expect("literal predicates must check");
    let task = &checked.object_types()[0];
    let enabled = &task.fields()[0];
    for function in checked.server_functions() {
        assert_eq!(
            function
                .references()
                .iter()
                .map(|reference| (reference.kind(), reference.target()))
                .collect::<Vec<_>>(),
            vec![
                (
                    DefinitionReferenceKind::QueryObject,
                    CheckedDefinitionReferenceTarget::ObjectType(task.id()),
                ),
                (
                    DefinitionReferenceKind::QueryField,
                    CheckedDefinitionReferenceTarget::Field {
                        owner: task.id(),
                        field: enabled.id(),
                    },
                ),
            ]
        );
    }
}

#[test]
fn rejects_non_boolean_direct_predicates_at_the_complete_predicate() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (title TEXT); \
            CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (title TEXT) \
            AS SELECT t.title FROM tasks.task t WHERE t.title;";
    let report = check(&bundle([("direct_type.orna", source)]), &empty_catalogue());

    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::TypeMismatch);
    assert_eq!(diagnostic.message(), "WHERE requires a BOOLEAN expression");
    let predicate_start = source.rfind("t.title").expect("predicate exists");
    assert_eq!(diagnostic.location().logical_path(), "direct_type.orna");
    assert_eq!(diagnostic.location().span().start(), predicate_start);
    assert_eq!(
        diagnostic.location().span().end(),
        predicate_start + "t.title".len()
    );
}

#[test]
fn rejects_parameterised_direct_predicates_through_the_identity_selector_boundary() {
    let source = "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (enabled BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (enabled BOOL) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT t.enabled FROM tasks.task t WHERE t.enabled;";
    let report = check(
        &bundle([("parameter_direct.orna", source)]),
        &empty_catalogue(),
    );

    assert_no_checked_bundle(&report);
    assert_eq!(report.diagnostics().len(), 1);
    let diagnostic = &report.diagnostics()[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::DomainIncompatible);
    assert_eq!(
        diagnostic.message(),
        "parameterised SELECT SERVER functions require WHERE REF(source_alias) = selector_parameter"
    );
    let predicate_start = source.rfind("t.enabled").expect("predicate exists");
    assert_eq!(
        diagnostic.location().logical_path(),
        "parameter_direct.orna"
    );
    assert_eq!(diagnostic.location().span().start(), predicate_start);
    assert_eq!(
        diagnostic.location().span().end(),
        predicate_start + "t.enabled".len()
    );
}

#[test]
fn checks_distinct_query_identities_and_orders_signature_then_body_evidence() {
    let source = "CREATE SCHEMA people; CREATE SCHEMA tasks; \
            CREATE TYPE people.person AS OBJECT (active BOOL NOT NULL); \
            CREATE TYPE tasks.task AS OBJECT (assignee REF people.person, completed BOOL NOT NULL); \
            CREATE SERVER FUNCTION tasks.values() \
            RETURNS ROWS (task REF tasks.task, active BOOL, completed BOOL) \
            SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT DISTINCT REF(t), t.assignee.active, t.completed FROM tasks.task t \
            WHERE t.assignee.active;";
    let report = check(
        &bundle([("distinct_references.orna", source)]),
        &empty_catalogue(),
    );

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let person = &checked.object_types()[0];
    let task = &checked.object_types()[1];
    let active = &person.fields()[0];
    let assignee = &task.fields()[0];
    let completed = &task.fields()[1];
    let function = &checked.server_functions()[0];
    let plan = function
        .distinct_query_plan()
        .expect("fixture has a DISTINCT SELECT body");
    assert!(function.query_plan().is_none());
    assert!(function.identity_selected_query_plan().is_none());
    assert_eq!(plan.scan().object_type(), task.id());
    assert_eq!(plan.projections().len(), 3);
    assert!(!plan.projections()[0].value_type().nullable());
    assert!(plan.projections()[1].value_type().nullable());
    assert!(!plan.projections()[2].value_type().nullable());
    let ExpressionKind::FieldPath { steps, .. } = plan.projections()[1].kind() else {
        panic!("second DISTINCT projection must be a field path");
    };
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].owner(), task.id());
    assert_eq!(steps[0].field(), assignee.id());
    assert_eq!(steps[1].owner(), person.id());
    assert_eq!(steps[1].field(), active.id());
    let selection = plan.selection().expect("fixture has a direct predicate");
    let ExpressionKind::FieldPath { steps, .. } = selection.kind() else {
        panic!("direct DISTINCT predicate must be a field path");
    };
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].owner(), task.id());
    assert_eq!(steps[0].field(), assignee.id());
    assert_eq!(steps[1].owner(), person.id());
    assert_eq!(steps[1].field(), active.id());
    assert_eq!(
        selection.value_type().semantic_type(),
        SemanticType::scalar(StandardScalar::Boolean)
    );
    assert!(selection.value_type().nullable());

    let query_start = source.find("SELECT DISTINCT").unwrap();
    let query_object_start = query_start + source[query_start..].find("tasks.task").unwrap();
    let projection_reference_start =
        query_start + source[query_start..].find("REF(t)").unwrap() + "REF(".len();
    let assignee_starts = source
        .match_indices("t.assignee.active")
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    assert_eq!(assignee_starts.len(), 2);
    let completed_start = source.find("t.completed").unwrap();
    let return_target_start = source.find("task REF tasks.task").unwrap() + "task REF ".len();
    let expected = [
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            return_target_start,
            "tasks.task".len(),
        ),
        (
            DefinitionReferenceKind::QueryObject,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            query_object_start,
            "tasks.task".len(),
        ),
        (
            DefinitionReferenceKind::ObjectReference,
            CheckedDefinitionReferenceTarget::ObjectType(task.id()),
            projection_reference_start,
            1,
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: assignee.id(),
            },
            assignee_starts[0] + "t.".len(),
            "assignee".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: person.id(),
                field: active.id(),
            },
            assignee_starts[0] + "t.assignee.".len(),
            "active".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: completed.id(),
            },
            completed_start + "t.".len(),
            "completed".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: task.id(),
                field: assignee.id(),
            },
            assignee_starts[1] + "t.".len(),
            "assignee".len(),
        ),
        (
            DefinitionReferenceKind::QueryField,
            CheckedDefinitionReferenceTarget::Field {
                owner: person.id(),
                field: active.id(),
            },
            assignee_starts[1] + "t.assignee.".len(),
            "active".len(),
        ),
    ];

    assert_eq!(function.references().len(), expected.len());
    for (reference, (kind, target, start, length)) in function.references().iter().zip(expected) {
        assert_eq!(reference.kind(), kind);
        assert_eq!(reference.target(), target);
        assert_eq!(
            reference.location().logical_path(),
            "distinct_references.orna"
        );
        assert_eq!(reference.location().span().start(), start);
        assert_eq!(reference.location().span().end(), start + length);
    }
}

#[test]
fn rejects_duplicates_unknown_names_invalid_references_and_defaults() {
    let report = check(
        &bundle([(
            "invalid.orna",
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (\
                     duplicated TEXT, duplicated INT,\
                     unknown missing.type,\
                     ref_scalar REF TEXT,\
                     plain_person people.person ON DELETE RESTRICT,\
                     required_ref REF people.person NOT NULL ON DELETE SET NULL,\
                     bad_default INT DEFAULT TRUE\
                 );\
                 CREATE TYPE people.person AS OBJECT (name TEXT);",
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
    assert!(codes.contains(&DiagnosticCode::InvalidReferenceTarget));
    assert!(codes.contains(&DiagnosticCode::TypeMismatch));
    assert_no_checked_bundle(&report);
}

#[test]
fn checked_bundle_contains_only_submitted_schemas_and_object_types() {
    let base = catalogue(
        vec![schema(1, &["people"]), schema(2, &["tasks"])],
        vec![
            object_type(
                3,
                &["people", "person"],
                vec![field(
                    4,
                    "name",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                )],
            ),
            object_type(
                5,
                &["tasks", "task"],
                vec![field(
                    6,
                    "title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                )],
            ),
        ],
        Vec::new(),
    );
    let report = check(
        &bundle([(
            "schema.orna",
            "CREATE SCHEMA people; CREATE TYPE people.customer AS OBJECT (name TEXT);",
        )]),
        &base,
    );

    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    assert_eq!(checked.schemas().len(), 1);
    assert_eq!(checked.schemas()[0].name().to_string(), "people");
    assert_eq!(checked.object_types().len(), 1);
    assert_eq!(
        checked.object_types()[0].name().to_string(),
        "people.customer"
    );
    assert_eq!(
        checked.object_types()[0].id().to_string(),
        "provisional:type:0"
    );
    assert!(checked.server_functions().is_empty());
}

#[test]
fn rejects_references_to_base_object_types_omitted_from_the_bundle() {
    let base = catalogue(
        vec![schema(1, &["people"])],
        vec![object_type(
            2,
            &["people", "person"],
            vec![field(
                3,
                "name",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            )],
        )],
        Vec::new(),
    );
    let source = "CREATE SCHEMA tasks; \
            CREATE TYPE tasks.task AS OBJECT (owner REF people.person);";

    let report = check(&bundle([("tasks.orna", source)]), &base);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_eq!(
        report.diagnostics()[0].location().span().start(),
        source.find("people.person").unwrap()
    );
    assert_no_checked_bundle(&report);
}
