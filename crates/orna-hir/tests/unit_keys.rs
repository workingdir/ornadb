//! Unit keys of lowered families (units.md sections 3.3, 3.5 and ORNA-UNIT-009 to 011): base scales,
//! derived factors evaluated after lowering, and the diagnostics of an unknown factor and a cycle.

use num_bigint::BigInt;
use orna_hir::{
    DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit,
    diag::{UNIT_CYCLE, UNIT_UNRESOLVED},
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

fn lower_one(source: &str) -> Program {
    let tree = parse(source);
    let units = [SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }];
    lower(&units)
}

fn defined(program: &Program, name: &str) -> DefId {
    program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| program.defs[*def].name.as_str() == name)
        .unwrap_or_else(|| panic!("`{name}` is declared"))
}

/// The variant `name` of the enum `family`.
fn variant(program: &Program, family: &str, name: &str) -> DefId {
    let DefKind::Enum(enum_def) = &program.defs[defined(program, family)].kind else {
        panic!("`{family}` is an enum");
    };
    enum_def
        .variants
        .iter()
        .copied()
        .find(|def| program.defs[*def].name.as_str() == name)
        .unwrap_or_else(|| panic!("`{family}.{name}` is a variant"))
}

/// The unit diagnostics of `program`. Prelude name lookups (`ORNA-S012`) are not unit diagnostics and are ignored.
fn codes(program: &Program) -> Vec<String> {
    program
        .modules
        .iter()
        .flat_map(|module| module.diagnostics.iter())
        .filter(|diagnostic| diagnostic.code.starts_with("ORNA-UNIT"))
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect()
}

const TIME_AND_LENGTH: &str = "pub enum Length {\n    metre,\n    kilometre = 1000 metre,\n    mile = 1609.344 metre,\n    impl Unit {}\n}\n\
pub enum Duration {\n    second,\n    minute = 60 second,\n    hour = 3600 second,\n    impl Unit {}\n}\n";

#[test]
fn a_base_family_unit_has_its_axis_and_scale() {
    let program = lower_one(TIME_AND_LENGTH);
    assert!(
        codes(&program).is_empty(),
        "no diagnostics: {:?}",
        codes(&program)
    );
    let metre = variant(&program, "Length", "metre");
    let kilometre = variant(&program, "Length", "kilometre");
    let metre_key = program
        .units
        .key(program.units.unit_key(metre).expect("metre has a key"));
    let kilometre_key = program.units.key(
        program
            .units
            .unit_key(kilometre)
            .expect("kilometre has a key"),
    );
    assert_eq!(metre_key.scale().numerator(), &BigInt::from(1));
    assert_eq!(kilometre_key.scale().numerator(), &BigInt::from(1000));
    assert!(metre_key.same_dimension(kilometre_key));
    assert_eq!(metre_key.exponents().get(metre), 1);
}

#[test]
fn a_derived_family_has_the_dimension_and_scale_of_its_factors() {
    let source = format!(
        "{TIME_AND_LENGTH}pub enum Speed {{\n    kmh = Length.kilometre / Duration.hour,\n    mph = Length.mile / Duration.hour,\n    impl Unit {{}}\n}}\n"
    );
    let program = lower_one(&source);
    assert!(
        codes(&program).is_empty(),
        "no diagnostics: {:?}",
        codes(&program)
    );
    let metre = variant(&program, "Length", "metre");
    let second = variant(&program, "Duration", "second");
    let speed = defined(&program, "Speed");
    let dimension = program
        .units
        .family_dimension(speed)
        .expect("Speed has a dimension");
    assert_eq!(dimension.get(metre), 1);
    assert_eq!(dimension.get(second), -1);

    let kmh = program.units.key(
        program
            .units
            .unit_key(variant(&program, "Speed", "kmh"))
            .expect("kmh key"),
    );
    assert_eq!(kmh.scale().numerator(), &BigInt::from(5));
    assert_eq!(kmh.scale().denominator(), &BigInt::from(18));

    let mph = program.units.key(
        program
            .units
            .unit_key(variant(&program, "Speed", "mph"))
            .expect("mph key"),
    );
    let ratio = mph.ratio_to(kmh).expect("an exact ratio");
    assert_eq!(ratio.numerator(), &BigInt::from(25_146));
    assert_eq!(ratio.denominator(), &BigInt::from(15_625));
}

#[test]
fn an_unknown_factor_is_reported_and_has_no_key() {
    let source = format!(
        "{TIME_AND_LENGTH}pub enum Speed {{\n    kmh = Length.furlong / Duration.hour,\n    impl Unit {{}}\n}}\n"
    );
    let program = lower_one(&source);
    assert!(
        codes(&program)
            .iter()
            .any(|code| code.starts_with(UNIT_UNRESOLVED)),
        "{:?}",
        codes(&program)
    );
    assert!(
        program
            .units
            .unit_key(variant(&program, "Speed", "kmh"))
            .is_none()
    );
}

#[test]
fn a_cycle_of_factors_is_reported_and_has_no_key() {
    let source = "pub enum Loop {\n    a = 2 b,\n    b = 3 a,\n    impl Unit {}\n}\n";
    let program = lower_one(source);
    assert!(
        codes(&program)
            .iter()
            .any(|code| code.starts_with(UNIT_CYCLE)),
        "{:?}",
        codes(&program)
    );
    assert!(
        program
            .units
            .unit_key(variant(&program, "Loop", "a"))
            .is_none()
    );
}
