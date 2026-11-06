//! `case` arm bodies (ORNA-CASE-004: all reachable value-producing arm bodies
//! MUST type-unify; ORNA-CFLOW-003 gives `if` the same rule). The arm bodies
//! join through the same rule as `if` branches: an ordinary value and its
//! optional join to `T?`, and `null` joins with `T?` to `T?` (ORNA-TYPE-001).

use orna_semantic::{Analysis, DIAG_TYPE, ModuleInput, Type, analyze};

fn analysis(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("case_arms.orna", source)])
}

fn diagnostics(analysis: &Analysis) -> Vec<(&str, &str)> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

fn result_type<'a>(analysis: &'a Analysis, name: &str) -> &'a Type {
    let symbol = analysis
        .modules
        .values()
        .find_map(|module| module.symbols.get(name))
        .unwrap_or_else(|| panic!("`{name}` is not declared"));
    match &symbol.ty {
        Type::Function { result, .. } => result,
        other => panic!("`{name}` is not a function: {other:?}"),
    }
}

#[test]
fn case_with_int_arm_before_optional_arm_unifies_to_optional() {
    let analysis = analysis(
        r#"
pub fn int_first(value: Int?) = case value {
    Some(n): n,
    null: Some(0),
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
        result_type(&analysis, "int_first"),
        &Type::Optional(Box::new(Type::Int))
    );
}

#[test]
fn case_with_optional_arm_before_int_arm_unifies_to_optional() {
    let analysis = analysis(
        r#"
pub fn optional_first(value: Int?) = case value {
    Some(n): Some(n),
    null: 0,
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
        result_type(&analysis, "optional_first"),
        &Type::Optional(Box::new(Type::Int))
    );
}

#[test]
fn declared_optional_result_accepts_int_and_null_arms() {
    let analysis = analysis(
        r#"
pub fn declared(value: Int?): Int? = case value {
    Some(n): n,
    null: null,
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
        result_type(&analysis, "declared"),
        &Type::Optional(Box::new(Type::Int))
    );
}

#[test]
fn genuinely_incompatible_arms_report_one_diagnostic() {
    let analysis = analysis(
        r#"
pub fn mismatched(value: Int?) = case value {
    Some(n): n,
    null: "none",
};
"#,
    );

    let type_diagnostics = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == DIAG_TYPE)
        .count();
    assert_eq!(type_diagnostics, 1, "{:?}", analysis.diagnostics);
}

#[test]
fn diverging_case_arm_contributes_no_type() {
    let analysis = analysis(
        r#"
pub fn guarded(value: Int?) = case value {
    null: {
        return 0;
    },
    Some(n): n,
};
"#,
    );

    assert_eq!(
        diagnostics(&analysis),
        Vec::<(&str, &str)>::new(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(result_type(&analysis, "guarded"), &Type::Int);
}

#[test]
fn null_first_arm_joins_with_int_arm_to_optional() {
    let analysis = analysis(
        r#"
pub fn null_first(value: Int?) = case value {
    Some(_): null,
    null: 0,
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
        result_type(&analysis, "null_first"),
        &Type::Optional(Box::new(Type::Int))
    );
}
