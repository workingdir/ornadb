//! A refined type's predicate may omit its subject: `assert >= 0` compares the refined base value,
//! which the body names `self`. The predicate is typed against that base, so a comparison with an
//! incompatible operand is rejected.

use orna_hir::{ModulePath, Name, Program, SourceId, SourceUnit, lower, typeck};
use orna_syntax::parse_module_with_file;

fn checked(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let tree = parsed.value;
    let units = [SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }];
    let mut program = lower(&units);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    typeck(&mut program);
    program
}

fn main_diagnostics(program: &Program) -> usize {
    program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].path.to_string() == "main")
        .count()
}

#[test]
fn an_implicit_subject_comparison_is_typed_against_the_base() {
    let program = checked("pub type Score = Int {\n    assert >= 0;\n    assert <= 100;\n}\n");
    assert_eq!(
        main_diagnostics(&program),
        0,
        "a comparison with the base is accepted"
    );
}

#[test]
fn an_implicit_subject_comparison_matches_the_named_subject() {
    let implicit = checked("pub type Score = Int {\n    assert >= 0;\n}\n");
    let named = checked("pub type Score = Int {\n    assert self >= 0;\n}\n");
    assert_eq!(main_diagnostics(&implicit), main_diagnostics(&named));
    assert_eq!(main_diagnostics(&implicit), 0);
}

#[test]
fn an_implicit_subject_comparison_with_an_incompatible_operand_is_rejected() {
    let program = checked("pub type Score = Int {\n    assert >= \"low\";\n}\n");
    assert!(
        main_diagnostics(&program) > 0,
        "the operand must be checked against the refined base"
    );
    assert!(
        program
            .diagnostics()
            .all(|(_, diagnostic)| diagnostic.code != orna_hir::diag::UNSUPPORTED),
        "the predicate lowers as a comparison, not as unsupported syntax"
    );
}
