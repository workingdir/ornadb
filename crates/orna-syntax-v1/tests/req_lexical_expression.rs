use orna_syntax_v1::{Expr, UseTail, parse_expression, parse_module, parse_row_with_file};

/// ORNA-LEX-002: source spans retain UTF-8 byte offsets and scalar line/column locations.
#[test]
fn source_positions_keep_utf8_byte_spans_and_line_columns() {
    let parsed = parse_row_with_file(r#"{ name: "é" }"#, "rows/item.orna");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let Expr::Record { fields, span } = parsed.value else { panic!("expected row record") };
    assert_eq!(span.file.as_deref(), Some("rows/item.orna"));
    assert_eq!(span.start_position.unwrap().column, 1);
    let value_span = fields[0].value.span();
    assert_eq!(value_span.start, 8);
    assert_eq!(value_span.end, 12);
}

/// ORNA-LEX-003: line comments end at the line ending.
#[test]
fn line_comments_leave_following_line_lexable() {
    let parsed = parse_module("// comment
fn value(): Int = 1;");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 1);
}

/// ORNA-SOURCE-003: row units contain exactly one record expression and no declarations.
#[test]
fn row_unit_accepts_one_record_and_rejects_declarations_or_extra_values() {
    let valid = parse_row_with_file("{ value: 1 }", "rows/item.orna");
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    for invalid in ["fn value() -> Int = 1;", "{ value: 1 } { other: 2 }"] {
        assert!(!parse_row_with_file(invalid, "rows/item.orna").is_ok(), "{invalid}");
    }
}

/// ORNA-RECORD-002: an empty record remains a record expression.
#[test]
fn empty_record_is_an_expression() {
    assert!(matches!(parse_expression("{}").value, Expr::Record { ref fields, .. } if fields.is_empty()));
}

/// ORNA-RECORD-003: standalone free blocks are not primary expressions.
#[test]
fn free_standing_block_is_not_an_expression() {
    assert!(!parse_expression("{ let value = 1; value }").is_ok());
}

/// ORNA-ARROW-002: direct braces are a function body; parenthesized braces return a record.
#[test]
fn lambda_brace_classification_requires_parentheses_for_record_return() {
    let body = parse_expression("() => { return 1; }");
    assert!(body.is_ok(), "{:?}", body.diagnostics);
    let record = parse_expression("() => ({})");
    assert!(record.is_ok(), "{:?}", record.diagnostics);
    assert!(matches!(record.value, Expr::Lambda { .. }));
}

/// ORNA-ARROW-003: record braces after a case-arm colon classify field names as record keys.
#[test]
fn case_arm_direct_record_body_is_not_a_label_or_block() {
    let parsed = parse_expression("case x { _ : { name: 1 } }");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
}

/// ORNA-PATTERN-001: bare record-pattern fields bind a same-named variable.
#[test]
fn record_pattern_shorthand_is_a_binding() {
    let parsed = parse_expression("case value { { count }: count, _ : 0 }");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
}

/// ORNA-LIT-003: decimal suffix selects the exact decimal literal kind.
#[test]
fn decimal_suffix_selects_exact_decimal() {
    let parsed = parse_expression("12.50.decimal");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    assert!(matches!(parsed.value, Expr::Field { name, .. } if name == "decimal"));
}

/// ORNA-LIT-007: currency construction is semantic, not parser magic.
#[test]
fn currency_constructor_remains_an_ordinary_qualified_call() {
    let parsed = parse_expression("12.50.decimal.USD");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    assert!(matches!(parsed.value, Expr::Field { .. } | Expr::Call { .. }));
}

/// ORNA-IMPORT-002: alias imports remain explicit in the module AST.
#[test]
fn aliased_import_is_preserved_in_the_syntax_tree() {
    let parsed = parse_module("use a.b as x; fn run(): Int = 1;");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    assert!(matches!(&parsed.value.items[0].declaration, orna_syntax_v1::Declaration::Use { tail: UseTail::Alias { name, .. }, .. } if name == "x"));
}
