//! Quantity types written as a numeric type applied to a unit (units.md section 3.4).
//!
//! `Decimal<kWh>`, `Float<kWh>` and `Quantity<N, kWh>` lower to [`TyKind::Quantity`] over the numeric
//! type and the unit's key, and a unit argument is never lowered as a type name. Each program must pass
//! [`Program::validate`].

use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, TyKind, lower};
use orna_syntax::parse_module_with_file;

const ENERGY: &str = "pub enum Energy {\n    kWh,\n    impl Unit {}\n}\n";

fn lower_one(source: &str) -> Program {
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
    let program = lower(&units);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn parameter_kind(program: &Program, name: &str) -> TyKind {
    let module = &program.modules[program
        .modules
        .iter_enumerated()
        .find(|(_, module)| {
            module
                .path
                .segments()
                .last()
                .is_some_and(|segment| segment.as_str() == "main")
        })
        .map(|(id, _)| id)
        .expect("module `main` exists")];
    assert!(
        module.diagnostics.is_empty(),
        "unit-typed parameter must lower without diagnostics: {:?}",
        module.diagnostics
    );
    let def: DefId = module
        .items
        .iter()
        .copied()
        .find(|def| program.defs[*def].name.as_str() == name)
        .expect("function is declared");
    let DefKind::Fn(function) = &program.defs[def].kind else {
        panic!("`{name}` is a function");
    };
    program
        .tys
        .kind(function.params[0].ty)
        .cloned()
        .expect("parameter type is interned")
}

#[test]
fn decimal_applied_to_a_unit_is_a_quantity() {
    let program = lower_one(&format!("{ENERGY}pub fn run(x: Decimal<kWh>): Int = 0;\n"));
    let TyKind::Quantity(number, _) = parameter_kind(&program, "run") else {
        panic!("`Decimal<kWh>` lowers to a quantity");
    };
    assert!(matches!(program.tys.kind(number), Some(TyKind::Decimal)));
}

#[test]
fn float_applied_to_a_unit_is_a_quantity() {
    let program = lower_one(&format!("{ENERGY}pub fn run(x: Float<kWh>): Int = 0;\n"));
    let TyKind::Quantity(number, _) = parameter_kind(&program, "run") else {
        panic!("`Float<kWh>` lowers to a quantity");
    };
    assert!(matches!(program.tys.kind(number), Some(TyKind::Float)));
}

#[test]
fn quantity_with_an_explicit_number_is_a_quantity() {
    let program = lower_one(&format!(
        "{ENERGY}pub fn run(x: Quantity<Int, kWh>): Int = 0;\n"
    ));
    let TyKind::Quantity(number, _) = parameter_kind(&program, "run") else {
        panic!("`Quantity<Int, kWh>` lowers to a quantity");
    };
    assert!(matches!(program.tys.kind(number), Some(TyKind::Int)));
}
