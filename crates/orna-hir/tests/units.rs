//! Unit families and unit suffixes over real source (units.md section 2 and ORNA-UNIT-008).
//!
//! A numeric literal with a unit suffix lowers to a call of that unit's constructor. A family member
//! called through its family lowers to the same constructor call. Several units of one name in scope
//! are ambiguous and reported by name. Each program must pass [`Program::validate`].

use orna_hir::{
    ArgValue, Callee, DefId, DefKind, ExprKind, ModulePath, Name, Program, SourceId, SourceUnit,
    lower,
};
use orna_syntax::{SyntaxTree, parse_module_with_file};

fn parse(source: &str) -> SyntaxTree {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    parsed.value
}

fn lower_modules(modules: &[(&str, &str)]) -> Program {
    let trees = modules
        .iter()
        .map(|(_, source)| parse(source))
        .collect::<Vec<_>>();
    let units = modules
        .iter()
        .zip(&trees)
        .enumerate()
        .map(|(index, ((path, _), tree))| SourceUnit {
            source: SourceId::new(index as u32),
            tree,
            path: ModulePath::new([Name::new(path)]),
        })
        .collect::<Vec<_>>();
    let program = lower(&units);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn lower_one(source: &str) -> Program {
    lower_modules(&[("main", source)])
}

fn module_of(program: &Program, path: &str) -> orna_hir::ModuleId {
    program
        .modules
        .iter_enumerated()
        .find(|(_, module)| {
            module
                .path
                .segments()
                .last()
                .is_some_and(|name| name.as_str() == path)
        })
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("module `{path}` exists"))
}

fn defined(program: &Program, path: &str, name: &str) -> DefId {
    let module = &program.modules[module_of(program, path)];
    module
        .items
        .iter()
        .copied()
        .find(|def| program.defs[*def].name.as_str() == name)
        .unwrap_or_else(|| panic!("`{name}` is declared in `{path}`"))
}

/// The root expression of `run`'s body.
fn root_of(program: &Program, run: DefId) -> &orna_hir::Expr {
    let body = program.source_body(run).expect("run has a body");
    &program.bodies[body].exprs[program.bodies[body].root]
}

const LENGTH: &str =
    "pub enum Length {\n    metre,\n    kilometre = 1000 metre,\n    impl Unit {}\n}\n";

#[test]
fn a_unit_suffix_lowers_to_a_constructor_call() {
    let program = lower_one(&format!("{LENGTH}pub fn run(): Int = 5.kilometre;\n"));
    let run = defined(&program, "main", "run");
    let ExprKind::Call(call) = &root_of(&program, run).kind else {
        panic!("a unit suffix lowers to a call");
    };
    let Callee::Ctor { def } = call.callee else {
        panic!("a unit suffix calls a constructor, got {:?}", call.callee);
    };
    assert!(matches!(program.defs[def].kind, DefKind::Unit(_)));
    assert_eq!(program.defs[def].name.as_str(), "kilometre");
    assert_eq!(call.args.len(), 1);
    assert_eq!(call.args[0].slot.as_u32(), 0);
    let ArgValue::Expr(value) = call.args[0].value else {
        panic!("the literal is the argument");
    };
    let body = program.source_body(run).expect("run has a body");
    assert!(matches!(
        program.bodies[body].exprs[value].kind,
        ExprKind::Lit(_)
    ));
}

#[test]
fn a_suffix_without_a_unit_falls_back_to_the_builtin_postfix() {
    let program = lower_one(&format!("{LENGTH}pub fn run(): Int = 1.day;\n"));
    let run = defined(&program, "main", "run");
    assert!(matches!(root_of(&program, run).kind, ExprKind::Lit(_)));
}

#[test]
fn a_family_member_call_is_a_unit_constructor() {
    let program = lower_one(&format!(
        "{LENGTH}pub fn run(): Int = Length.kilometre(5);\n"
    ));
    let run = defined(&program, "main", "run");
    let ExprKind::Call(call) = &root_of(&program, run).kind else {
        panic!("a family member call is a call");
    };
    assert!(matches!(
        call.callee,
        Callee::Ctor { def } if program.defs[def].name.as_str() == "kilometre"
    ));
    assert_eq!(call.args.len(), 1);
}

#[test]
fn an_ambiguous_unit_suffix_is_reported_by_name() {
    let program = lower_modules(&[
        ("one", LENGTH),
        ("two", LENGTH),
        ("user", "pub fn run(): Int = 5.kilometre;\n"),
    ]);
    let module = &program.modules[module_of(&program, "user")];
    assert!(
        module
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-S011-AMBIGUOUS"
                && diagnostic.message.contains("Length")),
        "{:?}",
        module.diagnostics
    );
}

#[test]
fn a_unit_in_the_same_module_resolves_without_ambiguity() {
    let program = lower_modules(&[
        ("one", LENGTH),
        ("two", LENGTH),
        (
            "user",
            &format!("{LENGTH}pub fn run(): Int = 5.kilometre;\n"),
        ),
    ]);
    let module = &program.modules[module_of(&program, "user")];
    assert!(
        !module
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-S011-AMBIGUOUS"),
        "{:?}",
        module.diagnostics
    );
    let run = defined(&program, "user", "run");
    assert!(matches!(
        &root_of(&program, run).kind,
        ExprKind::Call(call) if matches!(call.callee, Callee::Ctor { .. })
    ));
}
