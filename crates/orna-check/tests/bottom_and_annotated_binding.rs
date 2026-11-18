//! Annotated `let` bindings take the annotated type (ORNA-TYPE-001: `Int?`
//! admits `null`, so `let x: Int? = null` binds `Int?`, not `Null`), and a
//! diverging `case` arm (`Bottom`) does not participate in the arm join
//! (ORNA-CASE-004 joins reachable bodies only).

use orna_check::{Analysis, ModuleInput, analyze};

fn analysis(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("bottom_annotated.orna", source)])
}

fn diagnostics(analysis: &Analysis) -> Vec<(&str, &str)> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

#[test]
fn annotated_optional_let_binds_the_annotated_type() {
    let analysis = analysis(
        r#"
pub fn pick(flag: Bool): Int? {
    let x: Int? = null;
    if flag {
        x
    } else {
        null
    }
}
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}

#[test]
fn diverging_case_arm_does_not_join_with_value_arms() {
    let analysis = analysis(
        r#"
pub fn value_or_stop(input: Int?) = case input {
    Some(value): value,
    null: {
        return 0;
    },
};
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}

#[test]
fn predicate_annotation_is_a_declared_type() {
    let analysis = analysis(
        r#"
pub fn positive(rows: Relation<Int>): Bool = true;
pub fn keep(): Predicate<Relation<Int>> = rows => positive(rows);
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}
