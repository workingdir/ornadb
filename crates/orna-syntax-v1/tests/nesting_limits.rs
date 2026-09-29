use orna_syntax_v1::{
    Declaration, parse_expression, parse_expression_with_file, parse_module,
    parse_module_with_file, parse_repl_with_file, parse_row,
};

const LIMIT_ERROR: &str = "maximum syntax nesting exceeded";

fn source(case: &str) -> &'static str {
    let fixture = include_str!("fixtures/nesting_limits.orna");
    let marker = format!("// CASE {case}\n");
    let (_, source) = fixture
        .split_once(&marker)
        .unwrap_or_else(|| panic!("missing nesting fixture case {case}"));
    let end = source.find("\n// CASE ").unwrap_or(source.len());
    source[..end].trim_end()
}

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
    let parsed = parse_expression(source("prefix-deep"));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn recursive_types_and_patterns_share_the_parser_limit() {
    let parsed = parse_module(source("type-deep"));
    assert_limited(&parsed.diagnostics);

    let parsed = parse_module(source("pattern-deep"));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn ordinary_recursive_forms_below_the_budget_are_accepted() {
    let parsed = parse_expression(source("prefix-shallow"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);

    let parsed = parse_module(source("type-shallow"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);

    let parsed = parse_module(source("pattern-shallow"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
}

#[test]
fn mixed_delimiters_and_postfix_spines_share_one_ast_budget() {
    let parsed = parse_expression_with_file(source("mixed-postfix"), "memory.orna");
    assert_limited(&parsed.diagnostics);
}

#[test]
fn nested_control_blocks_are_limited_at_active_recursion_entrance() {
    let parsed = parse_expression(source("control-deep"));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn overdeep_control_recovers_to_the_next_top_level_declaration() {
    let parsed = parse_module_with_file(source("control-recovery"), "recovery.orna");
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
    let parsed = parse_repl_with_file(source("repl-deep"), "repl.orna");
    assert_limited(&parsed.diagnostics);
    assert!(
        parsed
            .diagnostics
            .iter()
            .all(|error| error.span.file.as_deref() == Some("repl.orna"))
    );
}

#[test]
fn row_entrypoint_reports_the_same_nesting_limit_without_panicking() {
    let parsed = parse_row(source("row-deep"));
    assert_limited(&parsed.diagnostics);

    let shallow = parse_row(source("row-shallow"));
    assert!(shallow.is_ok(), "{:?}", shallow.diagnostics);
}

#[test]
fn list_wrappers_compose_with_field_spines() {
    let parsed = parse_expression(source("list-postfix"));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn optional_types_and_assignment_targets_cannot_form_unbounded_spines() {
    let parsed = parse_module(source("optional-type"));
    assert_limited(&parsed.diagnostics);

    let parsed = parse_module(source("assignment-target"));
    assert_limited(&parsed.diagnostics);
}

#[test]
fn wide_shallow_lists_remain_valid() {
    let parsed = parse_expression(source("wide-list"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
}
