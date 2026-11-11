//! Typing diagnostics carry the code, a message and the source span of the
//! offending construct, and the program exposes them through
//! [`Program::diagnostics`].

use super::typeck;
use crate::diag::{GENERIC_OVERLAP, INFER_AMBIGUOUS, INFER_CONFLICT, PUBLIC_ANNOTATION};
use crate::{HirDiagnostic, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

pub(super) fn lower_source(source: &str) -> Program {
    let tree = parse_module_with_file(source, "test.orna").value;
    lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("test")]),
    }])
}

/// The source text a diagnostic's span covers.
pub(super) fn spanned<'a>(source: &'a str, diagnostic: &HirDiagnostic) -> &'a str {
    &source[diagnostic.span.start..diagnostic.span.end]
}

/// Every diagnostic of one code the program holds, with its module's path.
pub(super) fn with_code<'a>(program: &'a Program, code: &str) -> Vec<(String, &'a HirDiagnostic)> {
    program
        .diagnostics()
        .filter(|(_, diagnostic)| diagnostic.code == code)
        .map(|(module, diagnostic)| (program.modules[module].path.to_string(), diagnostic))
        .collect()
}

#[test]
fn a_constraint_conflict_is_a_spanned_diagnostic_on_the_program() {
    let source = "pub fn f(x: Int): Str = x + 1;\n";
    let mut program = lower_source(source);
    typeck(&mut program);
    let found = with_code(&program, INFER_CONFLICT);
    assert_eq!(found.len(), 1, "{found:?}");
    let (module, conflict) = &found[0];
    assert_eq!(module, "test");
    assert!(conflict.message.contains("Int"), "{}", conflict.message);
    assert!(conflict.message.contains("Str"), "{}", conflict.message);
    assert!(
        conflict.span.start < conflict.span.end,
        "{:?}",
        conflict.span
    );
    assert!(
        spanned(source, conflict).contains("x + 1"),
        "span covers {:?}",
        spanned(source, conflict)
    );
}

#[test]
fn an_unsolved_variable_is_a_spanned_diagnostic() {
    let source = "pub fn f() { let xs = []; let ys = xs; }\n";
    let mut program = lower_source(source);
    typeck(&mut program);
    let found = with_code(&program, INFER_AMBIGUOUS);
    assert!(
        !found.is_empty(),
        "no ambiguity diagnostic: {:?}",
        program.modules
    );
    for (_, diagnostic) in &found {
        assert!(
            diagnostic.span.start < diagnostic.span.end,
            "{:?}",
            diagnostic.span
        );
        assert!(diagnostic.span.end <= source.len(), "{:?}", diagnostic.span);
    }
}

#[test]
fn a_public_parameter_without_annotation_is_spanned_at_its_function() {
    let source = "pub fn id(x) = x;\n";
    let mut program = lower_source(source);
    typeck(&mut program);
    let found = with_code(&program, PUBLIC_ANNOTATION);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        spanned(source, found[0].1).starts_with("pub fn id"),
        "span covers {:?}",
        spanned(source, found[0].1)
    );
    assert!(found[0].1.message.contains("`x`"), "{}", found[0].1.message);
}

#[test]
fn two_protocol_implementations_are_a_spanned_overlap_diagnostic() {
    let source = "pub protocol P {\n    fn value(self): Int;\n}\npub type Box {\n    impl P {\n        fn value(self): Int = 1;\n    }\n    impl P {\n        fn value(self): Int = 2;\n    }\n}\npub fn f(b: Box): Int = b.value();\n";
    let mut program = lower_source(source);
    typeck(&mut program);
    let found = with_code(&program, GENERIC_OVERLAP);
    assert!(
        !found.is_empty(),
        "no overlap diagnostic: {:?}",
        program.modules
    );
    for (_, diagnostic) in &found {
        assert!(
            diagnostic.span.start < diagnostic.span.end,
            "{:?}",
            diagnostic.span
        );
        assert!(diagnostic.span.end <= source.len(), "{:?}", diagnostic.span);
    }
}

#[test]
fn a_typed_program_has_no_diagnostics_on_its_module() {
    let mut program = lower_source("pub fn f(x: Int): Int = x + 1;\n");
    typeck(&mut program);
    let module = program
        .modules
        .iter()
        .find(|module| module.path.to_string() == "test")
        .expect("the test module is registered");
    assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
}
