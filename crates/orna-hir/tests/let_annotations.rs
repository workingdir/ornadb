//! A module-level `let` keeps its written annotation: lowering records it, and typeck checks the
//! initializer against it.

use orna_hir::{
    DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower, print_type, typeck,
};
use orna_syntax::{EntryPoint, ReplInput, SyntaxTree, parse_repl};

/// Lowers one session-style `let` item the way the REPL session does: parsed as an item, then
/// wrapped as the only item of a module.
fn let_program(source: &str) -> Program {
    let parsed = parse_repl(source);
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let ReplInput::Item(item) = parsed.value else {
        panic!("a `let` is an item");
    };
    let span = item.span.clone();
    let tree = SyntaxTree {
        entry: EntryPoint::Module,
        items: vec![item],
        span,
    };
    let path = ModulePath::new([Name::new("main")]);
    let mut program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path,
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    typeck(&mut program);
    program
}

fn let_declared(program: &Program, name: &str) -> Option<orna_hir::Ty> {
    let module = program
        .modules
        .iter()
        .find(|module| module.path.to_string() == "main")
        .expect("module `main` exists");
    let def = module
        .items
        .iter()
        .copied()
        .find(|def| program.defs[*def].name.as_str() == name)
        .unwrap_or_else(|| panic!("`{name}` is declared"));
    match &program.defs[def].kind {
        DefKind::Let(binding) => binding.declared,
        other => panic!("`{name}` must be a let, found {other:?}"),
    }
}

#[test]
fn an_annotated_let_records_its_written_type() {
    let program = let_program("let seed: Int = 10;\n");
    let declared = let_declared(&program, "seed").expect("the annotation is kept");
    assert_eq!(print_type(&program, declared), "Int");
    assert_eq!(main_diagnostics(&program), 0, "a matching let is accepted");
}

#[test]
fn an_unannotated_let_has_no_declared_type() {
    let program = let_program("let seed = 10;\n");
    assert_eq!(let_declared(&program, "seed"), None);
    assert_eq!(main_diagnostics(&program), 0);
}

#[test]
fn an_initializer_that_contradicts_its_annotation_is_rejected() {
    let program = let_program("let seed: Text = 10;\n");
    assert!(
        main_diagnostics(&program) > 0,
        "the initializer must be checked against the annotation"
    );
}

/// The diagnostics raised in the test's own module, as the REPL session counts them.
fn main_diagnostics(program: &Program) -> usize {
    program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].path.to_string() == "main")
        .count()
}
