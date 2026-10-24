//! Option `case` arms with a `null` arm nested under a `Some` arm
//! (ORNA-CASE-001 exhaustive closed scrutinee; ORNA-TYPE-001 `T?` is `Option<T>`).
//! `std.option.xor` uses this shape verbatim, so the analyzer must accept it.

use orna_semantic_v1::{Analysis, ModuleInput, analyze};

fn analysis(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("option_xor.orna", source)])
}

fn diagnostics(analysis: &Analysis) -> Vec<(&str, &str)> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

#[test]
fn nested_null_first_case_arm_under_some_is_accepted() {
    let analysis = analysis(
        r#"
pub fn xor<T>(left: T?, right: T?): T? = case left {
    Some(_): case right {
        null: left,
        Some(_): null,
    },
    null: right,
};
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}
