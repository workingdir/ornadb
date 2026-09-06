use orna_syntax_v1::{
    Declaration, parse_expression, parse_expression_with_file, parse_module,
    parse_module_with_file, parse_repl_with_file,
};

const LIMIT_ERROR: &str = "maximum syntax nesting exceeded";

fn assert_limited(diagnostics: &[orna_syntax_v1::ParseError]) {
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-PARSE-001"
                && diagnostic.message == LIMIT_ERROR),
        "expected syntax limit diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn prefix_recursion_is_limited_before_stack_exhaustion() {
    let parsed = parse_expression(&format!("{}value", "!".repeat(600)));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn recursive_types_and_patterns_share_the_parser_limit() {
    let nested_type = format!("{}Int{}", "[".repeat(600), "]".repeat(600));
    let parsed = parse_module(&format!("fn typed(value: {nested_type}) = value;"));
    assert_limited(&parsed.diagnostics);

    let nested_pattern = format!("{}value{}", "[".repeat(600), "]".repeat(600));
    let parsed = parse_module(&format!("fn destructure({nested_pattern}) = 1;"));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn mixed_delimiters_and_postfix_spines_share_one_ast_budget() {
    let source = format!(
        "{}value{}{}",
        "(".repeat(120),
        ".field".repeat(400),
        ")".repeat(120)
    );
    let parsed = parse_expression_with_file(&source, "memory.orna");
    assert_limited(&parsed.diagnostics);
}

#[test]
fn nested_control_blocks_are_limited_at_active_recursion_entrance() {
    let source = format!("{}1{}", "if true {".repeat(100), "}".repeat(100),);
    let parsed = parse_expression(&source);
    assert_limited(&parsed.diagnostics);
}

#[test]
fn overdeep_control_recovers_to_the_next_top_level_declaration() {
    let source = format!(
        "fn limited() {{ {}1{} }} fn retained() = 2;",
        "if true {".repeat(100),
        "}".repeat(100),
    );
    let parsed = parse_module_with_file(&source, "recovery.orna");
    assert_limited(&parsed.diagnostics);
    assert!(
        parsed
            .diagnostics
            .iter()
            .all(|error| { error.span.file.as_deref() == Some("recovery.orna") })
    );
    assert!(matches!(
        parsed.value.items.get(1).map(|item| &item.declaration),
        Some(Declaration::Function { signature, .. }) if signature.name == "retained"
    ));
}

#[test]
fn repl_entrypoint_reports_the_same_nesting_limit_with_file_context() {
    let parsed = parse_repl_with_file(&format!("{}value", "!".repeat(100)), "repl.orna");
    assert_limited(&parsed.diagnostics);
    assert!(
        parsed
            .diagnostics
            .iter()
            .all(|error| error.span.file.as_deref() == Some("repl.orna"))
    );
}

#[test]
fn list_wrappers_compose_with_field_spines() {
    let source = format!(
        "{}value{}{}",
        "[".repeat(40),
        ".field".repeat(30),
        "]".repeat(40),
    );
    let parsed = parse_expression(&source);
    assert_limited(&parsed.diagnostics);
}

#[test]
fn optional_types_and_assignment_targets_cannot_form_unbounded_spines() {
    let optional = format!("Int {}", "? ".repeat(600));
    let parsed = parse_module(&format!("fn typed(value: {optional}) = value;"));
    assert_limited(&parsed.diagnostics);

    let target = format!("value{}", ".field".repeat(600));
    let parsed = parse_module(&format!("fn assign() {{ {target} = 1; }}"));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn wide_shallow_lists_remain_valid() {
    let source = format!(
        "[{}]",
        std::iter::repeat_n("1", 1_000)
            .collect::<Vec<_>>()
            .join(",")
    );
    let parsed = parse_expression(&source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
}
