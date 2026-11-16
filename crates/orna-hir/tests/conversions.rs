//! Explicit conversions (CONVERT-001..003, CONVERT-006): `Target.from(source)` selects the nested
//! `From<Source>` implementation whose source accepts the value. A conversion is never implicit, and
//! a conversion call takes exactly one positional source value.

use orna_hir::diag::{GENERIC_OVERLAP, INFER_CONFLICT};
use orna_hir::typeck::typeck;
use orna_hir::{ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::SyntaxTree;

fn lower_source(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let tree: SyntaxTree = parsed.value;
    let path = ModulePath::new([Name::new("main")]);
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path,
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

/// Lowers and types `source`, then returns the diagnostic codes the `main` module carries.
fn typed_codes(source: &str) -> Vec<&'static str> {
    let mut program = lower_source(source);
    typeck(&mut program);
    program
        .modules
        .iter()
        .filter(|module| module.path.to_string() == "main")
        .flat_map(|module| module.diagnostics.iter())
        .map(|diagnostic| diagnostic.code)
        .collect()
}

/// `Temperature` nests one `From<Int>` implementation, which builds it from the integer.
const TEMPERATURE: &str = "pub type Temperature {\n\
    value: Int,\n\
    impl From<Int> {\n\
        fn from(value): Temperature { Temperature { value: value } }\n\
    }\n\
}\n";

#[test]
fn a_positional_source_selects_the_from_implementation_that_accepts_it() {
    let source = format!("{TEMPERATURE}pub fn f(): Temperature = Temperature.from(21);\n");
    assert_eq!(typed_codes(&source), Vec::<&str>::new());
}

#[test]
fn each_source_selects_its_own_from_implementation() {
    let source = "pub type Temperature {\n\
        value: Int,\n\
        impl From<Int> {\n\
            fn from(value): Temperature { Temperature { value: value } }\n\
        }\n\
        impl From<Str> {\n\
            fn from(text): Temperature { Temperature { value: 0 } }\n\
        }\n\
    }\n\
    pub fn by_int(): Temperature = Temperature.from(21);\n\
    pub fn by_str(): Temperature = Temperature.from(\"21\");\n";
    assert_eq!(typed_codes(source), Vec::<&str>::new());
}

#[test]
fn two_from_implementations_for_one_source_overlap() {
    let source = "pub type Temperature {\n\
        value: Int,\n\
        impl From<Int> {\n\
            fn from(value): Temperature { Temperature { value: value } }\n\
        }\n\
        impl From<Int> {\n\
            fn from(value): Temperature { Temperature { value: 0 } }\n\
        }\n\
    }\n";
    assert!(typed_codes(source).contains(&GENERIC_OVERLAP));
}

#[test]
fn a_source_no_from_implementation_accepts_is_a_conflict() {
    let source = format!("{TEMPERATURE}pub fn f(): Temperature = Temperature.from(true);\n");
    assert!(typed_codes(&source).contains(&INFER_CONFLICT));
}

#[test]
fn a_bare_source_is_never_converted_implicitly() {
    let source = format!("{TEMPERATURE}pub fn f(): Temperature = 21;\n");
    assert!(typed_codes(&source).contains(&INFER_CONFLICT));
}

#[test]
fn a_named_source_is_rejected() {
    let source = format!("{TEMPERATURE}pub fn f(): Temperature = Temperature.from(value: 21);\n");
    assert!(typed_codes(&source).contains(&INFER_CONFLICT));
}

#[test]
fn generic_from_sources_that_overlap_are_refused_at_the_declaration() {
    let source = "pub type Box<T> {\n\
        value: T,\n\
        impl From<T> {\n\
            fn from(value): Box<T> { Box { value: value } }\n\
        }\n\
        impl From<Int> {\n\
            fn from(value): Box<T> { Box { value: value } }\n\
        }\n\
    }\n";
    assert!(typed_codes(source).contains(&GENERIC_OVERLAP));
}

#[test]
fn generic_from_sources_that_stay_apart_are_not_refused() {
    let source = "pub type Box<T> {\n\
        value: T,\n\
        impl From<Int> {\n\
            fn from(value): Box<T> { Box { value: value } }\n\
        }\n\
        impl From<Str> {\n\
            fn from(text): Box<T> { Box { value: text } }\n\
        }\n\
    }\n";
    assert!(!typed_codes(source).contains(&GENERIC_OVERLAP));
}
