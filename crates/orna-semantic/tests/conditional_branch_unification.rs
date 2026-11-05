//! Conditional branch typing (ORNA-CFLOW-003: `if` is an expression and all
//! value-producing branches MUST type-unify; ORNA-TYPE-001: `T?` means
//! `Option<T>`, which `null` inhabits, so `null` unifies with `T?` as `T?`;
//! ORNA-INFER-001: the expression settles on one static type before
//! evaluation).
//!
//! A branch is checked against the conditional's declared or contextual type
//! when one exists, and the two branch types are joined when it does not, so
//! neither branch is typed against the other's spelling.

use orna_semantic::{Analysis, DIAG_TYPE, ModuleInput, Type, analyze};

fn analysis(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("conditional.orna", source)])
}

fn diagnostics(analysis: &Analysis) -> Vec<(&str, &str)> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

fn notes(analysis: &Analysis) -> Vec<String> {
    analysis
        .diagnostics
        .iter()
        .flat_map(|diagnostic| {
            diagnostic
                .notes()
                .iter()
                .map(|note| note.as_str().to_owned())
        })
        .collect()
}

fn result_type<'a>(analysis: &'a Analysis, name: &str) -> &'a Type {
    let module = analysis
        .modules
        .values()
        .find(|module| module.namespace.display() == "conditional")
        .expect("conditional module");
    match &module.symbols[name].ty {
        Type::Function { result, .. } => result,
        other => panic!("`{name}` is not a function: {other:?}"),
    }
}

#[test]
fn declared_optional_return_accepts_null_and_some_branches() {
    let analysis =
        analysis("pub fn half(value: Int): Int? = if value < 0 { null } else { Some(value) };\n");

    assert_eq!(
        diagnostics(&analysis),
        Vec::<(&str, &str)>::new(),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn unannotated_conditional_joins_null_with_the_optional_branch() {
    let analysis = analysis(
        r#"
pub fn absent_first(value: Int) = if value < 0 { null } else { Some(value) };

pub fn some_first(value: Int) = if value < 0 { Some(value) } else { null };
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        Vec::<(&str, &str)>::new(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        result_type(&analysis, "absent_first"),
        &Type::Optional(Box::new(Type::Int))
    );
    assert_eq!(
        result_type(&analysis, "some_first"),
        &Type::Optional(Box::new(Type::Int))
    );
}

#[test]
fn declared_optional_let_binding_accepts_null_and_some_branches() {
    let analysis = analysis(
        r#"
pub fn half(value: Int): Int? {
    let absent: Int? = if value < 0 { null } else { Some(value) };
    absent
}
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        Vec::<(&str, &str)>::new(),
        "{:?}",
        analysis.diagnostics
    );
}

#[test]
fn nested_conditional_branches_unify_through_the_declared_type() {
    let analysis = analysis(
        r#"
pub fn half(value: Int): Int? =
    if value < -10 {
        null
    } else {
        if value < 0 { null } else { Some(value) }
    };

pub fn unannotated(value: Int) =
    if value < -10 {
        null
    } else {
        if value < 0 { null } else { Some(value) }
    };
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        Vec::<(&str, &str)>::new(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(
        result_type(&analysis, "unannotated"),
        &Type::Optional(Box::new(Type::Int))
    );
}

#[test]
fn declared_non_optional_return_names_the_declared_type_first() {
    let analysis =
        analysis("pub fn half(value: Int): Int = if value < 0 { null } else { value };\n");

    assert_eq!(
        diagnostics(&analysis),
        vec![(DIAG_TYPE, "static types are incompatible")]
    );
    assert_eq!(
        notes(&analysis),
        vec![String::from("expected Int, found Null")]
    );
}

#[test]
fn declared_optional_return_rejects_a_branch_of_another_type() {
    let analysis = analysis(
        "pub fn half(value: Int): Int? = if value < 0 { \"none\" } else { Some(value) };\n",
    );

    assert_eq!(
        diagnostics(&analysis),
        vec![(DIAG_TYPE, "static types are incompatible")]
    );
    assert_eq!(
        notes(&analysis),
        vec![String::from("expected Optional<Int>, found Text")]
    );
}

#[test]
fn branches_that_share_a_type_report_one_mismatch_against_the_declared_type() {
    // Both branches spell `Text`, so the conditional is wrong once, not once
    // per branch.
    let analysis =
        analysis("pub fn both(value: Int): Int = if value < 0 { \"low\" } else { \"high\" };\n");

    assert_eq!(
        diagnostics(&analysis),
        vec![(DIAG_TYPE, "static types are incompatible")]
    );
    assert_eq!(notes(&analysis), vec![String::from("expected Int, found Text")]);
}

#[test]
fn logical_operators_require_bool_operands_not_value_branches() {
    // `&&` unifies nothing: both operands must already be `Bool`
    // (ORNA-CFLOW-009 has no implicit truthiness), so a conditional that
    // produces `Int?` is rejected at the operand and never reaches a branch
    // comparison of its own.
    let analysis = analysis(
        "pub fn both(flag: Bool, value: Int) = (if flag { null } else { Some(value) }) && true;\n",
    );

    assert_eq!(
        diagnostics(&analysis),
        vec![(DIAG_TYPE, "static types are incompatible")]
    );
    assert_eq!(
        notes(&analysis),
        vec![String::from("expected Bool, found Optional<Int>")]
    );
}

#[test]
fn optional_case_arms_already_unify_null_with_some_in_source_order() {
    // ORNA-CASE-004 joins reachable arm bodies, and the `null` arm below comes
    // first, so this documents that the conditional defect does not extend to
    // `case`.
    let analysis = analysis(
        r#"
pub fn pick(value: Int?): Int? = case value {
    null: null,
    Some(inner): Some(inner),
};
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        Vec::<(&str, &str)>::new(),
        "{:?}",
        analysis.diagnostics
    );
}
