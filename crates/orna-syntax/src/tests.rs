use crate::parser::SyntaxKind;

use super::{
    ClientExpression, ClientFunctionBody, ClientProceduralStatement, FunctionReturnType,
    FunctionSecurity, FunctionTransaction, FunctionVolatility, InsertValue, MutationValue,
    NullOrdering, OnDeletePolicy, OptionTypeSpelling, OrderingDirection,
    PrimitiveValueTypePersistence, QueryExpression, RecordConstructorFieldValue, SelectQuantifier,
    ServerFunctionBody, SourceSpan, StandardLargeObjectKind, StateDefault, StateScope,
    TypeExportTarget, TypeSpecification, parse,
};

mod client;
mod cst;
mod server;

#[test]
fn rejects_sql_function_declarations_as_a_separate_unsupported_domain() {
    let source = "CREATE SQL FUNCTION app.total() RETURNS INTEGER AS SELECT 1;";
    let parsed = parse(source);

    assert!(parsed.server_functions().is_empty());
    assert!(parsed.client_functions().is_empty());
    assert!(!parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
}
#[test]
fn parses_schema_declarations_case_insensitively_without_rewriting_source() {
    let source = "cReAtE sChEmA crm.sales;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "crm");
    assert_eq!(parsed.schemas()[0].name.parts[1].text, "sales");
}

#[test]
fn parses_enum_labels_losslessly_in_declaration_order() {
    let source = "CREATE TYPE crm.stage AS ENUM (\n    'lead', /* keep */ 'qual''ified',\n    'customer'\n);";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let declaration = &parsed.enum_types()[0];
    assert_eq!(declaration.name.parts[0].text, "crm");
    assert_eq!(declaration.name.parts[1].text, "stage");
    assert_eq!(
        declaration
            .labels
            .iter()
            .map(|label| label.literal.text.as_str())
            .collect::<Vec<_>>(),
        ["'lead'", "'qual''ified'", "'customer'"]
    );
    assert_eq!(
        declaration.labels[1].literal.span.start,
        source.find("'qual''ified'").unwrap()
    );
    assert_eq!(
        declaration.span,
        SourceSpan {
            start: 0,
            end: source.len()
        }
    );
}

#[test]
fn reports_closed_enum_syntax_diagnostics_without_partial_declarations() {
    let cases = [
        (
            "CREATE TYPE app.stage AS ENUM ();",
            "enum type must declare at least one label",
        ),
        (
            "CREATE TYPE app.stage AS ENUM (lead);",
            "expected a string literal enum label",
        ),
        (
            "CREATE TYPE app.stage AS ENUM ('lead',);",
            "enum type cannot have a trailing comma",
        ),
        (
            "CREATE TYPE app.stage AS ENUM ('lead' 'customer');",
            "expected ',' or ')' after enum label",
        ),
        (
            "CREATE TYPE app.stage AS ENUM ('lead';",
            "expected ')' after enum labels",
        ),
        (
            "CREATE TYPE app.stage AS ENUM ('lead')",
            "expected ';' after enum type declaration",
        ),
    ];

    for (source, message) in cases {
        let parsed = parse(source);
        assert!(parsed.enum_types().is_empty(), "{source}");
        assert_eq!(parsed.diagnostics().len(), 1, "{source}");
        assert_eq!(parsed.diagnostics()[0].message, message, "{source}");
        assert_eq!(parsed.syntax().text(), source, "{source}");
    }
}

#[test]
fn recovers_from_an_invalid_enum_to_a_later_declaration() {
    let source = "CREATE TYPE app.stage AS ENUM ('lead',); CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.diagnostics().len(), 1);
    assert!(parsed.enum_types().is_empty());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
}

#[test]
fn parses_immutable_record_value_type_losslessly() {
    let source = "CREATE TYPE example.point AS VALUE (\n    x INT,\n    /* ordinate */ y INTEGER,\n)\nIMMUTABLE\nPERSISTABLE;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let declaration = &parsed.record_value_types()[0];
    assert_eq!(declaration.name.parts[0].text, "example");
    assert_eq!(declaration.name.parts[1].text, "point");
    assert_eq!(declaration.fields.len(), 2);
    assert_eq!(declaration.fields[0].name.text, "x");
    assert_eq!(declaration.fields[0].order, 0);
    assert_eq!(declaration.fields[1].name.text, "y");
    assert_eq!(declaration.fields[1].order, 1);
    assert_eq!(
        declaration.fields[1].span,
        SourceSpan {
            start: source.find("y INTEGER").unwrap(),
            end: source.find("y INTEGER").unwrap() + "y INTEGER".len(),
        }
    );
    assert_eq!(
        declaration.immutable_span.start,
        source.find("IMMUTABLE").unwrap()
    );
    assert_eq!(
        declaration.immutable_span.end,
        source.find("IMMUTABLE").unwrap() + "IMMUTABLE".len()
    );
    assert_eq!(
        declaration.persistable_span.start,
        source.find("PERSISTABLE").unwrap()
    );
    assert_eq!(
        declaration.persistable_span.end,
        source.find("PERSISTABLE").unwrap() + "PERSISTABLE".len()
    );
    assert_eq!(
        declaration.span,
        SourceSpan {
            start: 0,
            end: source.len()
        }
    );

    let without_trailing_comma =
        parse("CREATE TYPE example.point AS VALUE (x INT) IMMUTABLE PERSISTABLE;");
    assert!(without_trailing_comma.diagnostics().is_empty());
    assert_eq!(
        without_trailing_comma.record_value_types()[0].fields.len(),
        1
    );
}

#[test]
fn reports_closed_record_value_type_diagnostics() {
    let cases = [
        (
            "CREATE TYPE app.empty AS VALUE () IMMUTABLE PERSISTABLE;",
            "record value type must declare at least one field",
            ")",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT y INT) IMMUTABLE PERSISTABLE;",
            "expected ',' or ')' after record value field",
            "y",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT;",
            "expected ')' after record value fields",
            ";",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT) PERSISTABLE IMMUTABLE;",
            "expected keyword IMMUTABLE",
            "PERSISTABLE",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE;",
            "expected keyword PERSISTABLE",
            ";",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x REF app.object) IMMUTABLE PERSISTABLE;",
            "record value fields cannot use REF",
            "REF",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT NOT NULL) IMMUTABLE PERSISTABLE;",
            "record value fields do not accept modifiers",
            "NOT",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT NULL) IMMUTABLE PERSISTABLE;",
            "record value fields do not accept modifiers",
            "NULL",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT DEFAULT 0) IMMUTABLE PERSISTABLE;",
            "record value fields do not accept modifiers",
            "DEFAULT",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT CHECK true) IMMUTABLE PERSISTABLE;",
            "record value fields do not accept modifiers",
            "CHECK",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE IMMUTABLE PERSISTABLE;",
            "expected keyword PERSISTABLE",
            "IMMUTABLE",
            true,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE PERSISTABLE;",
            "expected ';' after record value type declaration",
            "PERSISTABLE",
            true,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE EXTRA;",
            "expected ';' after record value type declaration",
            "EXTRA",
            false,
        ),
        (
            "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE",
            "expected ';' after record value type declaration",
            "",
            false,
        ),
    ];

    for (source, message, offending, use_last_occurrence) in cases {
        let parsed = parse(source);
        assert!(parsed.record_value_types().is_empty(), "{source}");
        assert_eq!(parsed.diagnostics().len(), 1, "{source}");
        assert_eq!(parsed.diagnostics()[0].message, message, "{source}");
        let start = if use_last_occurrence {
            source.rfind(offending).unwrap()
        } else if offending.is_empty() {
            source.len()
        } else {
            source.find(offending).unwrap()
        };
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start,
                end: start + offending.len(),
            },
            "{source}"
        );
        assert_eq!(parsed.syntax().text(), source, "{source}");
    }
}

#[test]
fn captures_documentation_modifiers() {
    let object_field = "CREATE TYPE app.task AS OBJECT (title TEXT DOCUMENTATION 'the title');";
    let parsed = parse(object_field);
    assert!(parsed.diagnostics().is_empty(), "{object_field}");
    let documentation = parsed.object_types()[0].fields[0]
        .documentation
        .as_ref()
        .expect("field documentation");
    assert_eq!(documentation.text, "'the title'");

    let object_type =
        "CREATE TYPE app.task AS OBJECT (title TEXT) FINAL DOCUMENTATION 'a final task';";
    let parsed = parse(object_type);
    assert!(parsed.diagnostics().is_empty(), "{object_type}");
    let declaration = &parsed.object_types()[0];
    assert!(declaration.final_type);
    assert_eq!(
        declaration
            .documentation
            .as_ref()
            .expect("type documentation")
            .text,
        "'a final task'"
    );

    let value_field =
        "CREATE TYPE app.point AS VALUE (x INT DOCUMENTATION 'the x') IMMUTABLE PERSISTABLE;";
    let parsed = parse(value_field);
    assert!(parsed.record_value_types().is_empty(), "{value_field}");
    assert_eq!(parsed.diagnostics().len(), 1, "{value_field}");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "record value fields do not accept modifiers"
    );
    let documentation_start = value_field.find("DOCUMENTATION").unwrap();
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan {
            start: documentation_start,
            end: documentation_start + "DOCUMENTATION".len(),
        }
    );

    let record =
        "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE DOCUMENTATION 'a point';";
    let parsed = parse(record);
    assert!(parsed.diagnostics().is_empty(), "{record}");
    assert_eq!(
        parsed.record_value_types()[0]
            .documentation
            .as_ref()
            .expect("record documentation")
            .text,
        "'a point'"
    );

    let primitive = "CREATE TYPE app.tick AS VALUE PRIMITIVE KERNEL CONTRACT 'k' IMMUTABLE PERSISTABLE DOCUMENTATION 'a primitive';";
    let parsed = parse(primitive);
    assert!(parsed.diagnostics().is_empty(), "{primitive}");
    assert_eq!(
        parsed.primitive_value_types()[0]
            .documentation
            .as_ref()
            .expect("primitive documentation")
            .text,
        "'a primitive'"
    );

    let opaque = "CREATE TYPE app.blob AS VALUE OPAQUE KERNEL CONTRACT 'k' IMMUTABLE TRANSIENT DOCUMENTATION 'an opaque';";
    let parsed = parse(opaque);
    assert!(parsed.diagnostics().is_empty(), "{opaque}");
    assert_eq!(
        parsed.opaque_value_types()[0]
            .documentation
            .as_ref()
            .expect("opaque documentation")
            .text,
        "'an opaque'"
    );

    let parameter = "CREATE SERVER FUNCTION app.overdue (p_before TIMESTAMP DOCUMENTATION 'cutoff') RETURNS BOOL AS SELECT probe.stored FROM app.probe probe;";
    let parsed = parse(parameter);
    assert!(parsed.diagnostics().is_empty(), "{parameter}");
    assert_eq!(
        parsed.server_functions()[0].parameters[0]
            .documentation
            .as_ref()
            .expect("parameter documentation")
            .text,
        "'cutoff'"
    );
}

#[test]
fn recovers_after_invalid_record_value_type() {
    let source = "CREATE TYPE app.point AS VALUE (x INT NOT NULL) IMMUTABLE PERSISTABLE; CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.diagnostics().len(), 1);
    assert!(parsed.record_value_types().is_empty());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
}

#[test]
fn parses_persistable_primitive_value_type_losslessly() {
    let source = "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let declaration = &parsed.primitive_value_types()[0];
    assert_eq!(declaration.name.parts[0].text, "std");
    assert_eq!(
        declaration.kernel_contract.text,
        "'orna.kernel.value.boolean@1'"
    );
    assert_eq!(
        declaration.persistence,
        PrimitiveValueTypePersistence::Persistable
    );
    assert_eq!(
        declaration.kernel_contract_modifier_span.start,
        source.find("KERNEL").unwrap()
    );
    assert_eq!(
        declaration.persistence_span.start,
        source.find("PERSISTABLE").unwrap()
    );
    assert_eq!(declaration.span.end, source.len());
}

#[test]
fn parses_transient_primitive_and_type_exports_losslessly() {
    let source = "CREATE TYPE std.types.VOID AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.void@1' IMMUTABLE TRANSIENT;\n\
            EXPORT TYPE std.types.VOID AS std.VOID;\n\
            EXPORT TYPE std.VOID TO /* binding */ PRELUDE AS CHARACTER  LARGE\nOBJECT;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(
        parsed.primitive_value_types()[0].persistence,
        PrimitiveValueTypePersistence::Transient
    );
    assert!(matches!(
        parsed.type_exports()[0].target,
        TypeExportTarget::Qualified { .. }
    ));
    if let TypeExportTarget::Qualified { name } = &parsed.type_exports()[0].target {
        assert_eq!(name.parts[1].text, "VOID");
    }
    assert!(matches!(
        parsed.type_exports()[1].target,
        TypeExportTarget::Prelude { .. }
    ));
    if let TypeExportTarget::Prelude {
        words,
        name_span,
        modifier_span,
    } = &parsed.type_exports()[1].target
    {
        assert_eq!(
            words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>(),
            ["CHARACTER", "LARGE", "OBJECT"]
        );
        assert_eq!(name_span.start, source.rfind("CHARACTER").unwrap());
        assert_eq!(
            name_span.end,
            source.rfind("OBJECT").unwrap() + "OBJECT".len()
        );
        assert_eq!(modifier_span.start, source.rfind("TO").unwrap());
    }
}

#[test]
fn parses_opaque_value_type_losslessly() {
    let source = "CREATE TYPE std.example.token AS VALUE OPAQUE KERNEL CONTRACT 'std.example.token@1' IMMUTABLE TRANSIENT;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    let declaration = &parsed.opaque_value_types()[0];
    assert_eq!(declaration.name.parts[0].text, "std");
    assert_eq!(declaration.name.parts[2].text, "token");
    assert_eq!(declaration.kernel_contract.text, "'std.example.token@1'");
    assert_eq!(
        declaration.opaque_span.start,
        source.find("OPAQUE").unwrap()
    );
    assert_eq!(
        declaration.kernel_contract_modifier_span,
        SourceSpan {
            start: source.find("KERNEL").unwrap(),
            end: source.find("CONTRACT").unwrap() + "CONTRACT".len(),
        }
    );
    assert_eq!(
        declaration.immutable_span.start,
        source.find("IMMUTABLE").unwrap()
    );
    assert_eq!(
        declaration.transient_span.start,
        source.find("TRANSIENT").unwrap()
    );
    assert_eq!(declaration.span.end, source.len());
}

#[test]
fn rejects_every_malformed_opaque_value_shape_and_recovers() {
    let cases = [
        (
            "CREATE TYPE std.bad AS VALUE OPAQUE CONTRACT 'std.bad@1' IMMUTABLE TRANSIENT;",
            "expected KERNEL after OPAQUE",
        ),
        (
            "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL 'std.bad@1' IMMUTABLE TRANSIENT;",
            "expected CONTRACT after KERNEL",
        ),
        (
            "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT IMMUTABLE TRANSIENT;",
            "expected a string literal after KERNEL CONTRACT",
        ),
        (
            "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' TRANSIENT;",
            "expected IMMUTABLE after opaque codec contract",
        ),
        (
            "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' IMMUTABLE PERSISTABLE;",
            "expected TRANSIENT after IMMUTABLE",
        ),
        (
            "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' IMMUTABLE TRANSIENT EXTRA;",
            "expected ';' after opaque value type declaration",
        ),
        (
            "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' IMMUTABLE TRANSIENT",
            "expected ';' after opaque value type declaration",
        ),
    ];

    for (invalid, message) in cases {
        let source = format!("{invalid} CREATE SCHEMA later;");
        let parsed = parse(&source);
        assert!(parsed.opaque_value_types().is_empty(), "{invalid}");
        assert_eq!(parsed.diagnostics().len(), 1, "{invalid}");
        assert_eq!(parsed.diagnostics()[0].message, message, "{invalid}");
        assert_eq!(parsed.schemas().len(), 1, "{invalid}");
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
        assert_eq!(parsed.syntax().text(), source);
    }
}

#[test]
fn reports_closed_primitive_and_export_syntax_diagnostics() {
    let cases = [
        (
            "CREATE TYPE app.value AS ;",
            "expected OBJECT, ENUM, or VALUE after AS",
        ),
        (
            "CREATE TYPE app.value AS VALUE ;",
            "expected keyword PRIMITIVE",
        ),
        (
            "CREATE TYPE app.value AS VALUE PRIMITIVE ;",
            "expected keyword KERNEL",
        ),
        (
            "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL ;",
            "expected keyword CONTRACT",
        ),
        (
            "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT ;",
            "expected a string literal after KERNEL CONTRACT",
        ),
        (
            "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' ;",
            "expected keyword IMMUTABLE",
        ),
        (
            "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE ;",
            "expected PERSISTABLE or TRANSIENT after IMMUTABLE",
        ),
        (
            "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE PERSISTABLE",
            "expected ';' after primitive value type declaration",
        ),
        ("EXPORT ;", "expected keyword TYPE"),
        ("EXPORT TYPE ;", "expected a type name after EXPORT TYPE"),
        (
            "EXPORT TYPE app.value ;",
            "expected AS or TO after exported type name",
        ),
        ("EXPORT TYPE app.value TO ;", "expected keyword PRELUDE"),
        ("EXPORT TYPE app.value TO PRELUDE ;", "expected keyword AS"),
        (
            "EXPORT TYPE app.value TO PRELUDE AS ;",
            "expected an unquoted prelude type name after AS",
        ),
        (
            "EXPORT TYPE app.value AS ;",
            "expected a qualified type name after AS",
        ),
        (
            "EXPORT TYPE app.value AS app.binding",
            "expected ';' after type export declaration",
        ),
    ];

    for (source, message) in cases {
        let parsed = parse(source);
        assert_eq!(parsed.diagnostics().len(), 1, "{source}");
        assert_eq!(parsed.diagnostics()[0].message, message, "{source}");
    }
}

#[test]
fn recovers_from_primitive_and_export_errors_to_later_exports() {
    let source = "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE;\n\
            EXPORT TYPE app.value AS app.binding;";
    let parsed = parse(source);

    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected PERSISTABLE or TRANSIENT after IMMUTABLE"
    );
    assert_eq!(parsed.type_exports().len(), 1);
}

#[test]
fn recovers_from_malformed_qualified_and_prelude_exports() {
    let qualified_source = "EXPORT TYPE app.value AS ; CREATE SCHEMA later;";
    let qualified = parse(qualified_source);
    assert_eq!(qualified.diagnostics().len(), 1);
    assert_eq!(
        qualified.diagnostics()[0].message,
        "expected a qualified type name after AS"
    );
    assert_eq!(qualified.schemas().len(), 1);
    assert_eq!(qualified.schemas()[0].name.parts[0].text, "later");

    let prelude_source =
        "EXPORT TYPE app.value TO PRELUDE AS ; EXPORT TYPE app.value AS app.binding;";
    let prelude = parse(prelude_source);
    assert_eq!(prelude.diagnostics().len(), 1);
    assert_eq!(
        prelude.diagnostics()[0].message,
        "expected an unquoted prelude type name after AS"
    );
    assert_eq!(prelude.type_exports().len(), 1);
    assert!(matches!(
        prelude.type_exports()[0].target,
        TypeExportTarget::Qualified { .. }
    ));
}

#[test]
fn recovers_from_missing_object_fields_without_panicking() {
    let source = "CREATE TYPE app.value AS OBJECT ; CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.object_types().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected '(' after AS OBJECT"
    );
    assert_eq!(
        parsed.diagnostics()[0].span.start,
        source.find(';').unwrap()
    );
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
}

#[test]
fn recovers_missing_server_parameters_at_root_level() {
    let source = "CREATE SERVER FUNCTION app.f ; CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected '(' after server function name"
    );
    assert_eq!(
        parsed.diagnostics()[0].span.start,
        source.find(';').unwrap()
    );
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");

    let root = &parsed.syntax().root;
    assert_eq!(root.kind(), SyntaxKind::Root);
    assert_eq!(
        root.children().map(|node| node.kind()).collect::<Vec<_>>(),
        [
            SyntaxKind::CreateServerFunctionStatement,
            SyntaxKind::CreateSchemaStatement,
        ]
    );
}

#[test]
fn preserves_a_create_declaration_after_a_missing_prelude_export_semicolon() {
    let source = "EXPORT TYPE std.X TO PRELUDE AS X CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.type_exports().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected ';' after type export declaration"
    );
    let boundary = source.find("CREATE").unwrap();
    assert_eq!(parsed.diagnostics()[0].span.start, boundary);
    assert_eq!(parsed.diagnostics()[0].span.end, boundary + "CREATE".len());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
}

#[test]
fn preserves_a_create_declaration_after_a_missing_prelude_alias() {
    let source = "EXPORT TYPE std.X TO PRELUDE AS CREATE SCHEMA later;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.type_exports().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected an unquoted prelude type name after AS"
    );
    let boundary = source.find("CREATE").unwrap();
    assert_eq!(parsed.diagnostics()[0].span.start, boundary);
    assert_eq!(parsed.diagnostics()[0].span.end, boundary + "CREATE".len());
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
}

#[test]
fn preserves_an_export_declaration_after_a_missing_prelude_export_semicolon() {
    let source = "EXPORT TYPE std.X TO PRELUDE AS X EXPORT TYPE std.X AS std.Y;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected ';' after type export declaration"
    );
    let boundary = source.rfind("EXPORT").unwrap();
    assert_eq!(parsed.diagnostics()[0].span.start, boundary);
    assert_eq!(parsed.diagnostics()[0].span.end, boundary + "EXPORT".len());
    assert_eq!(parsed.type_exports().len(), 1);
    assert_eq!(parsed.type_exports()[0].source_type.parts[1].text, "X");
    assert!(matches!(
        parsed.type_exports()[0].target,
        TypeExportTarget::Qualified { .. }
    ));
    if let TypeExportTarget::Qualified { name } = &parsed.type_exports()[0].target {
        assert_eq!(name.parts[1].text, "Y");
    }
}

#[test]
fn preserves_an_alter_declaration_after_a_missing_prelude_export_semicolon() {
    let source = "EXPORT TYPE std.X TO PRELUDE AS X ALTER TYPE later.item RENAME FIELD old TO new;";
    let parsed = parse(source);

    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected ';' after type export declaration"
    );
    let boundary = source.find("ALTER").unwrap();
    assert_eq!(parsed.diagnostics()[0].span.start, boundary);
    assert_eq!(parsed.diagnostics()[0].span.end, boundary + "ALTER".len());
    assert_eq!(parsed.field_renames().len(), 1);
    assert_eq!(parsed.field_renames()[0].type_name.parts[0].text, "later");
}

#[test]
fn reports_the_missing_qualified_export_target_at_its_source_span() {
    let source = "EXPORT TYPE app.value AS ;";
    let parsed = parse(source);

    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected a qualified type name after AS"
    );
    let semicolon = source.find(';').unwrap();
    assert_eq!(parsed.diagnostics()[0].span.start, semicolon);
    assert_eq!(parsed.diagnostics()[0].span.end, semicolon + 1);
}

#[test]
fn preserves_trivia_across_multiple_schema_declarations() {
    let source =
        "-- initial namespace\nCREATE SCHEMA people; /* task data */\nCREATE SCHEMA tasks;\n";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.schemas().len(), 2);
    assert_eq!(parsed.schemas()[1].span.start, 59);
}

#[test]
fn reports_malformed_schema_declarations_with_source_spans() {
    let source = "CREATE SCHEMA crm.;\nCREATE SCHEMA tasks";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.schemas().len(), 0);
    assert_eq!(parsed.diagnostics().len(), 2);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(parsed.diagnostics()[0].span.start, 18);
    assert_eq!(parsed.diagnostics()[1].code, "ORNA0001");
    assert_eq!(parsed.diagnostics()[1].span.start, source.len());
}

#[test]
fn reports_unterminated_comments_without_losing_source() {
    let source = "CREATE SCHEMA crm; /* unfinished";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0002");
    assert_eq!(parsed.diagnostics()[0].span.start, 19);
    assert_eq!(parsed.diagnostics()[0].span.end, source.len());
}

#[test]
fn parses_object_type_fields_without_rewriting_aliases_or_defaults() {
    let source = "CREATE TYPE tasks.task AS OBJECT (\n\
            title TEXT NOT NULL,\n\
            project REF tasks.project ON DELETE CASCADE,\n\
            completed BOOL NOT NULL DEFAULT FALSE,\n\
            object_id INT UNIQUE\n\
        );";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.object_types().len(), 1);

    let object_type = &parsed.object_types()[0];
    assert_eq!(object_type.name.parts[0].text, "tasks");
    assert_eq!(object_type.name.parts[1].text, "task");
    assert_eq!(object_type.fields.len(), 4);

    let title = &object_type.fields[0];
    assert_eq!(title.name.text, "title");
    assert_eq!(title.order, 0);
    assert!(!title.nullable);
    assert!(!title.unique);
    assert_named_type(&title.type_specification, "TEXT");
    assert_eq!(
        title.span.start,
        source.find("title").expect("title exists")
    );
    assert_eq!(
        title.span.end,
        source.find("NOT NULL").expect("NOT NULL exists") + 8
    );

    let project = &object_type.fields[1];
    assert!(project.nullable);
    assert_eq!(project.on_delete, Some(OnDeletePolicy::Cascade));
    match &project.type_specification {
        TypeSpecification::Reference { target, .. } => {
            let TypeSpecification::Named(target) = target.as_ref() else {
                panic!("project reference target must be named");
            };
            assert_eq!(target.parts[0].text, "tasks");
            assert_eq!(target.parts[1].text, "project");
        }
        _ => panic!("project must be a reference"),
    }

    let completed = &object_type.fields[2];
    assert_named_type(&completed.type_specification, "BOOL");
    assert!(!completed.nullable);
    assert_eq!(
        completed
            .default_expression
            .as_ref()
            .map(|expression| expression.text.as_str()),
        Some("FALSE")
    );

    let object_id = &object_type.fields[3];
    assert_eq!(object_id.name.text, "object_id");
    assert_named_type(&object_id.type_specification, "INT");
    assert!(object_id.unique);
}

#[test]
fn parses_each_supported_on_delete_policy() {
    let source = "CREATE TYPE crm.contact AS OBJECT (\n\
            restricted REF crm.person ON DELETE RESTRICT,\n\
            cleared REF crm.organisation ON DELETE SET NULL,\n\
            cascading REF crm.account ON DELETE CASCADE\n\
        );";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let fields = &parsed.object_types()[0].fields;
    assert_eq!(fields[0].on_delete, Some(OnDeletePolicy::Restrict));
    assert_eq!(fields[1].on_delete, Some(OnDeletePolicy::SetNull));
    assert_eq!(fields[2].on_delete, Some(OnDeletePolicy::Cascade));
}

#[test]
fn parses_simple_and_qualified_field_rename_declarations() {
    let source = "ALTER TYPE person RENAME FIELD email TO primary_email;\n\
            ALTER TYPE people.person RENAME FIELD email TO primary_email;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.field_renames().len(), 2);

    let simple = &parsed.field_renames()[0];
    assert_eq!(simple.type_name.parts.len(), 1);
    assert_eq!(simple.type_name.parts[0].text, "person");
    assert_eq!(simple.old_field_name.text, "email");
    assert_eq!(simple.new_field_name.text, "primary_email");
    assert_eq!(simple.span.start, 0);
    assert_eq!(
        simple.span.end,
        source.find('\n').expect("first declaration ends")
    );

    let qualified = &parsed.field_renames()[1];
    assert_eq!(qualified.type_name.parts[0].text, "people");
    assert_eq!(qualified.type_name.parts[1].text, "person");
    assert_eq!(qualified.old_field_name.text, "email");
    assert_eq!(qualified.new_field_name.text, "primary_email");
}

#[test]
fn preserves_quoted_field_rename_identifiers_and_spans() {
    let source = "ALTER TYPE \"People\".\"Person\" RENAME FIELD \"Email\" TO \"Primary\"\"Email\";";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    let rename = &parsed.field_renames()[0];
    assert_eq!(rename.type_name.parts[0].text, "\"People\"");
    assert_eq!(rename.type_name.parts[1].text, "\"Person\"");
    assert_eq!(rename.old_field_name.text, "\"Email\"");
    assert_eq!(rename.new_field_name.text, "\"Primary\"\"Email\"");
    let people_start = source.find("\"People\"").unwrap();
    let person_start = source.find("\"Person\"").unwrap();
    let old_start = source.find("\"Email\"").unwrap();
    let new_start = source.find("\"Primary\"\"Email\"").unwrap();
    assert_eq!(
        rename.type_name.parts[0].span,
        SourceSpan {
            start: people_start,
            end: people_start + "\"People\"".len(),
        }
    );
    assert_eq!(
        rename.type_name.parts[1].span,
        SourceSpan {
            start: person_start,
            end: person_start + "\"Person\"".len(),
        }
    );
    assert_eq!(
        rename.type_name.span,
        SourceSpan {
            start: people_start,
            end: person_start + "\"Person\"".len(),
        }
    );
    assert_eq!(
        rename.old_field_name.span,
        SourceSpan {
            start: old_start,
            end: old_start + "\"Email\"".len(),
        }
    );
    assert_eq!(
        rename.new_field_name.span,
        SourceSpan {
            start: new_start,
            end: source.len() - 1,
        }
    );
}

#[test]
fn reports_field_rename_syntax_errors_with_exact_diagnostics() {
    let cases = [
        (
            "ALTER people.person RENAME FIELD email TO primary_email;",
            "ALTER must be followed by TYPE",
            "people",
        ),
        (
            "ALTER TYPE RENAME FIELD email TO primary_email;",
            "expected the type name after ALTER TYPE",
            "RENAME",
        ),
        (
            "ALTER TYPE people. RENAME FIELD email TO primary_email;",
            "expected the type name after '.'",
            "RENAME",
        ),
        (
            "ALTER TYPE people.person FIELD email TO primary_email;",
            "expected RENAME after the type name",
            "FIELD",
        ),
        (
            "ALTER TYPE people.person RENAME email TO primary_email;",
            "expected FIELD after RENAME",
            "email",
        ),
        (
            "ALTER TYPE people.person RENAME FIELD TO primary_email;",
            "expected the old field name after RENAME FIELD",
            "TO",
        ),
        (
            "ALTER TYPE people.person RENAME FIELD email primary_email;",
            "expected TO after the old field name",
            "primary_email",
        ),
        (
            "ALTER TYPE people.person RENAME FIELD email TO;",
            "expected the new field name after TO",
            ";",
        ),
        (
            "ALTER TYPE people.person RENAME FIELD email TO primary_email",
            "expected ';' after field rename declaration",
            "",
        ),
        (
            "ALTER TYPE people.person RENAME FIELD email TO primary_email EXTRA;",
            "expected ';' after field rename declaration",
            "EXTRA",
        ),
        (
            "ALTER SCHEMA people RENAME FIELD email TO primary_email;",
            "ALTER must be followed by TYPE",
            "SCHEMA",
        ),
        (
            "ALTER TYPE people.person RENAME TO person;",
            "ALTER TYPE only supports RENAME FIELD",
            "TO",
        ),
    ];

    for (source, message, marker) in cases {
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text(), source);
        assert!(
            parsed.field_renames().is_empty(),
            "invalid source: {source}"
        );
        assert_eq!(parsed.diagnostics().len(), 1, "invalid source: {source}");
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(diagnostic.message, message);
        let offset = if marker.is_empty() {
            source.len()
        } else {
            source.find(marker).expect("diagnostic marker exists")
        };
        assert_eq!(diagnostic.span.start, offset);
        assert_eq!(diagnostic.span.end, offset + marker.len());

        let recovered = parse(&format!("{source}\nCREATE SCHEMA recovered;"));
        assert_eq!(
            recovered.schemas().len(),
            1,
            "later declaration lost after: {source}"
        );
    }
}

#[test]
fn field_rename_recovery_preserves_later_declarations() {
    let source = "ALTER TYPE people.person RENAME FIELD email TO;\n\
            CREATE TYPE people.person AS OBJECT (primary_email TEXT);\n\
            ALTER TYPE people.person RENAME FIELD email TO primary_email;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.object_types().len(), 1);
    assert_eq!(parsed.field_renames().len(), 1);
    assert_eq!(
        parsed.field_renames()[0].new_field_name.text,
        "primary_email"
    );
    assert_eq!(parsed.diagnostics().len(), 1);
}

#[test]
fn malformed_field_rename_quotes_use_the_existing_lexer_diagnostic() {
    let source = "ALTER TYPE people.person RENAME FIELD \"email TO primary_email;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.field_renames().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0002");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "unterminated quoted identifier"
    );
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan {
            start: source.find('"').unwrap(),
            end: source.len(),
        }
    );
}

#[test]
fn unsupported_top_level_statements_report_one_clear_error() {
    let source = "DROP TYPE people.person;";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.schemas().is_empty());
    assert!(parsed.object_types().is_empty());
    assert!(parsed.field_renames().is_empty());
    assert!(parsed.server_functions().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert_eq!(
        parsed.diagnostics()[0].message,
        "expected a CREATE, ALTER, or EXPORT declaration"
    );
    assert_eq!(
        parsed.diagnostics()[0].span,
        SourceSpan { start: 0, end: 4 }
    );
}

#[test]
fn field_rename_parsing_does_not_change_create_or_select_parsing() {
    let source = "CREATE TYPE people.person AS OBJECT (primary_email TEXT);\n\
            ALTER TYPE people.person RENAME FIELD email TO primary_email;\n\
            CREATE SERVER FUNCTION people.list_emails() RETURNS ROWS (email TEXT) AS SELECT person.primary_email FROM people.person person;";
    let parsed = parse(source);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.object_types().len(), 1);
    assert_eq!(parsed.field_renames().len(), 1);
    assert_eq!(parsed.server_functions().len(), 1);
    let query = parsed.server_functions()[0]
        .body
        .as_sql_query()
        .expect("function must retain its SELECT query");
    assert_eq!(query.query.projections.len(), 1);
    assert_eq!(query.query.source_object.alias.text, "person");
}

#[test]
fn rejects_primary_keys_in_object_types_with_an_explanatory_diagnostic() {
    let source = "CREATE TYPE people.person AS OBJECT (id INT PRIMARY KEY);";
    let parsed = parse(source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.object_types().is_empty());
    assert_eq!(parsed.diagnostics().len(), 1);
    assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
    assert!(
        parsed.diagnostics()[0]
            .message
            .contains("use UNIQUE NOT NULL for a business identity")
    );
    assert_eq!(
        parsed.diagnostics()[0].span.start,
        source.find("PRIMARY").expect("PRIMARY exists")
    );
}

fn assert_body_diagnostic(
    parameters: &str,
    result_column: &str,
    body: &str,
    message: &str,
    marker: &str,
    span_offset: usize,
    span_length: usize,
) {
    let source = format!(
        "CREATE SERVER FUNCTION tasks.bad({parameters}) RETURNS ROWS ({result_column}) AS {body};"
    );
    let parsed = parse(&source);

    assert_eq!(parsed.syntax().text(), source);
    assert!(parsed.server_functions().is_empty(), "invalid body: {body}");
    assert_eq!(parsed.diagnostics().len(), 1, "invalid body: {body}");
    let diagnostic = &parsed.diagnostics()[0];
    assert_eq!(diagnostic.code, "ORNA0001");
    assert_eq!(diagnostic.message, message);
    let offending = source.find(marker).expect("offending syntax exists") + span_offset;
    assert_eq!(diagnostic.span.start, offending);
    assert_eq!(diagnostic.span.end, offending + span_length);
}

fn assert_named_type(type_specification: &TypeSpecification, expected: &str) {
    match type_specification {
        TypeSpecification::Named(name) => {
            assert_eq!(
                name.parts
                    .iter()
                    .map(|part| part.text.as_str())
                    .collect::<Vec<_>>(),
                expected.split('.').collect::<Vec<_>>()
            );
        }
        _ => panic!("field must use a named type"),
    }
}

fn assert_standard_large_object_type(
    type_specification: &TypeSpecification,
    expected_kind: StandardLargeObjectKind,
    expected_source: &str,
) {
    match type_specification {
        TypeSpecification::StandardLargeObject { kind, source } => {
            assert_eq!(*kind, expected_kind);
            assert_eq!(source.text, expected_source);
        }
        _ => panic!("field must use a standard large object type"),
    }
}

fn assert_reference_type(
    type_specification: &TypeSpecification,
    first: &str,
    second: &str,
    third: &str,
) {
    match type_specification {
        TypeSpecification::Reference { target, .. } => {
            let TypeSpecification::Named(target) = target.as_ref() else {
                panic!("reference target must be a named type");
            };
            assert_eq!(target.parts[0].text, first);
            assert_eq!(target.parts[1].text, second);
            if third.is_empty() {
                assert_eq!(target.parts.len(), 2);
            } else {
                assert_eq!(target.parts[2].text, third);
            }
        }
        _ => panic!("type must be a reference"),
    }
}
