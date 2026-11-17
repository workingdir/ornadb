//! A refined value is not implicitly its base value, and a base value is not implicitly refined.
//! The base is reachable only as `.base` inside a nested implementation of the refined type
//! (ORNA-NOMINAL-006), it is read-only, and converting into the refined type needs the checked
//! construction path rather than a plain binding.

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

/// The `(code, message)` of every diagnostic in the test module.
fn main_diagnostics(program: &Program) -> Vec<(String, String)> {
    program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].path.to_string() == "main")
        .map(|(_, d)| (d.code.to_string(), d.message.to_string()))
        .collect()
}

const SCORE: &str = "pub type Score = Int {\n    assert >= 0;\n}\n";

#[test]
fn a_nested_implementation_reads_the_base_explicitly() {
    let program = checked(
        "pub protocol Greeter { fn greet(self, punctuation: Str): Int; }\npub type Score = Int {\n    assert >= 0;\n    impl Greeter { fn greet(self, punctuation: Str): Int = self.base; }\n}\n",
    );
    assert_eq!(
        main_diagnostics(&program),
        Vec::new(),
        "reading the base inside a nested implementation is accepted"
    );
}

#[test]
fn reading_the_base_outside_the_refined_type_is_rejected() {
    let program = checked(&format!("{SCORE}fn f(s: Score): Int = s.base;\n"));
    let diagnostics = main_diagnostics(&program);
    assert!(
        diagnostics.iter().any(|(code, message)| {
            code == "ORNA-S021-TYPE"
                && message
                    == "the base of a refined type is read outside the implementations of that type"
        }),
        "an outside `.base` read is rejected under NOMINAL-006: {diagnostics:?}"
    );
}

#[test]
fn a_refined_value_is_not_widened_into_its_base_implicitly() {
    let program = checked(&format!(
        "{SCORE}fn f(s: Score): Int {{\n    let x: Int = s;\n    x\n}}\n"
    ));
    let diagnostics = main_diagnostics(&program);
    assert!(
        diagnostics.iter().any(|(code, _)| code == "ORNA-INFER-011"),
        "a refined value needs explicit access to reach its base: {diagnostics:?}"
    );
}

#[test]
fn a_base_value_is_not_narrowed_into_the_refined_type_implicitly() {
    let program = checked(&format!(
        "{SCORE}fn f(n: Int): Score {{\n    let s: Score = n;\n    s\n}}\n"
    ));
    let diagnostics = main_diagnostics(&program);
    assert!(
        diagnostics.iter().any(|(code, _)| code == "ORNA-INFER-011"),
        "a base value needs the checked construction path to reach a refined type: {diagnostics:?}"
    );
}

#[test]
fn a_base_value_returned_as_the_refined_type_is_rejected() {
    let program = checked(&format!("{SCORE}fn f(n: Int): Score = n;\n"));
    let diagnostics = main_diagnostics(&program);
    assert!(
        diagnostics.iter().any(|(code, _)| code == "ORNA-INFER-011"),
        "returning a base value as a refined type is not an implicit conversion: {diagnostics:?}"
    );
}

#[test]
fn assigning_the_base_is_rejected() {
    let program = checked(&format!(
        "{SCORE}fn f(s: Score): Int {{\n    s.base = 1;\n    0\n}}\n"
    ));
    let diagnostics = main_diagnostics(&program);
    assert!(
        diagnostics
            .iter()
            .any(|(code, message)| { code == "ORNA-S021-TYPE" && message.contains("read-only") }),
        "the base of a refined value is read-only: {diagnostics:?}"
    );
}

#[test]
fn the_assertion_subject_is_the_base_through_self() {
    let program =
        checked("pub type Score = Int {\n    assert self >= 0;\n    assert self <= 100;\n}\n");
    assert_eq!(
        main_diagnostics(&program),
        Vec::new(),
        "the assertion subject `self` is typed against the base"
    );
}
