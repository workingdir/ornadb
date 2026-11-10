use super::{TypeCheckSummary, typeck};
use crate::{
    DefId, DefKind, ModuleId, ModulePath, Name, Program, SourceId, SourceUnit, lower, print_type,
};
use orna_syntax::parse_module_with_file;

fn program(source: &str) -> Program {
    let tree = parse_module_with_file(source, "test.orna").value;
    lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("test")]),
    }])
}

fn definition(program: &Program, name: &str) -> DefId {
    program
        .defs
        .iter_enumerated()
        .find(|(_, definition)| definition.name.as_str() == name)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no definition named {name}"))
}

/// The module the test source lowered to. Lowering registers the engine-embedded
/// `core` and `sys` modules first, so the index is not zero.
fn test_module(program: &Program) -> ModuleId {
    program
        .modules
        .iter_enumerated()
        .find(|(_, module)| module.path.to_string() == "test")
        .map(|(id, _)| id)
        .expect("the test module is registered")
}

/// Whether every body of the test module types. The embedded `core` and `sys`
/// bodies belong to the interface, not to the program under test, so one
/// program's verdict ignores them.
fn test_accepted(program: &Program, summary: &TypeCheckSummary) -> bool {
    let module = test_module(program);
    summary
        .outcomes
        .iter()
        .filter(|outcome| outcome.module == module)
        .all(|outcome| outcome.typed)
}

/// How many bodies of the test module the subset cannot type yet.
fn test_not_yet(program: &Program, summary: &TypeCheckSummary) -> usize {
    let module = test_module(program);
    summary
        .outcomes
        .iter()
        .filter(|outcome| outcome.module == module && outcome.not_yet.is_some())
        .count()
}

fn result(program: &Program, name: &str) -> String {
    let definition = definition(program, name);
    match &program.defs[definition].kind {
        DefKind::Fn(function) => print_type(program, function.ret),
        other => panic!("{name} is not a function: {other:?}"),
    }
}

#[test]
fn annotated_arithmetic_and_result_inference() {
    let mut program = program("pub fn add(a: Int, b: Int) = a + b;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "add"), "Int");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
}

#[test]
fn let_annotations_are_the_initializer_expected_type() {
    let mut program = program("pub fn scaled(x: Int): Int { let y: Int = x + 1; y * 2 }\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "scaled"), "Int");
}

#[test]
fn a_call_is_checked_against_the_resolved_signature() {
    let mut program =
        program("fn twice(x: Int): Int = x * 2;\npub fn f(y: Int): Int = twice(y);\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "f"), "Int");
}

#[test]
fn a_lambda_receives_the_element_type_from_the_pipeline() {
    let mut program =
        program("pub fn inc(values: [Int]): [Int] = values | map(value => value + 1);\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "inc"), "[Int]");
}

#[test]
fn an_empty_list_is_solved_by_its_context() {
    let mut program = program("pub fn f(): [Int] { let xs = []; xs }\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn an_unconstrained_variable_is_an_ambiguity_diagnostic() {
    let mut program = program("pub fn f() { let xs = []; let ys = xs; }\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == crate::diag::INFER_AMBIGUOUS),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}

#[test]
fn a_constraint_conflict_names_both_sides() {
    let mut program = program("pub fn f(x: Int): Str = x + 1;\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    let conflict = program.modules[test_module(&program)]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == crate::diag::INFER_CONFLICT)
        .expect("a conflict is reported");
    assert!(conflict.message.contains("Int"), "{}", conflict.message);
    assert!(conflict.message.contains("Str"), "{}", conflict.message);
}

#[test]
fn an_enum_constructor_and_case_are_typed() {
    let mut program = program(
        "pub enum Status { draft, published }\n\
             pub fn label(status: Status): Str = case status {\n\
                 Status.draft: \"Draft\",\n\
                 Status.published: \"Published\",\n\
             };\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "label"), "Str");
}

#[test]
fn a_construct_outside_the_subset_is_reported_not_rejected() {
    let mut program = program("pub fn f(): Unit = parallel([1, 2]);\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(test_not_yet(&program, &summary), 1);
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty(),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}

#[test]
fn a_printed_signature_uses_the_specification_spelling() {
    let mut program = program("pub fn add(a: Int, b: [Str?]): Int = a;\n");
    typeck(&mut program);
    let definition = definition(&program, "add");
    assert_eq!(
        crate::print_signature(&program, definition).as_deref(),
        Some("add(Int, [Str?]) -> Int")
    );
}

#[test]
fn an_enum_implementation_static_is_read_through_the_enum() {
    let mut program = program(
        "pub protocol Named { fn name(self): Str; }\n\
             pub enum Rounding {\n\
                 half_up,\n\
                 half_even,\n\
                 impl Named {\n\
                     static label = \"rounding\";\n\
                     fn name(self): Str = \"rounding\";\n\
                 }\n\
             }\n\
             pub fn f(): Str = Rounding.label;\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "f"), "Str");
}

#[test]
fn an_unknown_enum_static_is_not_a_member() {
    let mut program = program(
        "pub protocol Named { fn name(self): Str; }\n\
             pub enum Rounding {\n\
                 half_up,\n\
                 impl Named {\n\
                     static label = \"rounding\";\n\
                     fn name(self): Str = \"rounding\";\n\
                 }\n\
             }\n\
             pub fn f(): Str = Rounding.missing;\n",
    );
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn a_bare_variant_is_named_by_its_expected_enum_type() {
    let mut program =
        program("pub enum Status { draft, published }\npub fn start(): Status = draft;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty(),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}

#[test]
fn a_bare_name_that_is_no_expected_variant_is_unresolved() {
    let mut program =
        program("pub enum Status { draft, published }\npub fn start(): Status = archived;\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == crate::diag::UNRESOLVED),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}
