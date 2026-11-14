//! The embedded `core.units` declarations of `C` and `kWh` must match the checker's observer catalogue
//! (`orna_check::FIXTURE_UNIT_SUPPORT`). HIR carries the offset and affine clauses of a top-level unit; the factor
//! of a top-level derived unit is not carried yet, so only the declaration text and the carried clauses are compared.

use orna_hir::{DefKind, ModulePath, Name, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

/// The HIR unit definition named `name` among the embedded core declarations.
fn core_unit(name: &str) -> orna_hir::UnitDef {
    let parsed = parse_module_with_file("pub fn run() = 0;\n", "main.orna");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let tree = parsed.value;
    let units = [SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }];
    let program = lower(&units);
    program
        .defs
        .iter()
        .find_map(|def| match &def.kind {
            DefKind::Unit(unit) if def.name == Name::new(name) && unit.family.is_none() => Some(unit.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no core unit `{name}`"))
}

#[test]
fn c_is_the_affine_kelvin_offset_of_the_checker_catalogue() {
    assert!(orna_check::FIXTURE_UNIT_SUPPORT.contains("pub unit C : Temperature = K offset 273.15 affine;"));
    let c = core_unit("C");
    assert_eq!(c.offset.as_deref(), Some("273.15"));
    assert!(c.affine);
}

#[test]
fn kwh_is_declared_as_the_checker_catalogue_declares_it() {
    assert!(orna_check::FIXTURE_UNIT_SUPPORT.contains("pub unit kWh : Energy = 3.6e6.J;"));
    let kwh = core_unit("kWh");
    assert_eq!(kwh.offset, None);
    assert!(!kwh.affine);
}
