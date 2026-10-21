//! Declared units drive unit, dimension, affine and money-rate typing
//! (05-types.md "Dimensions and units", ORNA-UNIT-001..008, ORNA-MONEY-003).
//! Assertions compare diagnostic codes; nothing depends on a unit's spelling.

use orna_semantic_v1::{
    Analysis, Catalogue, DIAG_AFFINE_ADDITION, DIAG_AFFINE_SUM, DIAG_AMBIGUOUS, DIAG_FLOAT_MONEY,
    DIAG_INCOMPATIBLE_DIMENSIONS, DIAG_UNRESOLVED, ModuleInput, analyze, analyze_with_catalogue,
};

const LENGTH: &str = "
pub dim Length;
pub dim Mass;
pub dim Clock;
pub dim Velocity = Length / Clock;
pub unit m : Length base;
pub unit km : Length = 1000.m;
pub unit kg : Mass base;
pub unit sec : Clock base;
pub unit mps : Velocity = m / sec;
";

const TEMPERATURE: &str = "
pub dim Temperature;
pub unit K : Temperature base;
pub unit C : Temperature = K offset 273.15 affine;
pub unit deltaC : Temperature = K;
pub unit F : Temperature = K offset 255.372 affine;
";

const CURRENCY: &str = "
pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}

pub dim Energy;
pub unit kWh : Energy base;
";

fn analysis(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("units.orna", source)])
}

fn codes(analysis: &Analysis) -> Vec<&str> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code())
        .collect()
}

fn assert_valid(source: &str) {
    let analysis = analysis(source);
    assert!(analysis.is_ok(), "{source}\n{:?}", analysis.diagnostics);
}

fn assert_code(source: &str, code: &str) {
    let analysis = analysis(source);
    assert!(codes(&analysis).contains(&code), "{source}\n{:?}", analysis.diagnostics);
}

#[test]
fn declared_unit_literals_match_their_annotations_and_convert_by_declaration() {
    for body in [
        "pub fn d(): Int<m> = 5.m;",
        "fn d() = 5.m;\npub fn e(): Int<m> = d();",
        "pub fn d(): Int<m> = 1.km + 1.m;",
        "pub fn d(a: Float<m>, b: Float<km>): Float<m> = a + b;",
        "pub fn d(): Int<m> = 2 * 3.m;",
        "pub fn v(d: Float<m>, t: Float<sec>): Float<mps> = d / t;",
        "pub fn v(x: Float<m / sec>): Float<mps> = x;",
        "pub fn f(x: Quantity<Float, m>): Float<m> = x;",
        "pub fn shorter(a: Float<m>, b: Float<m>): Bool = a < b;",
    ] {
        assert_valid(&format!("{LENGTH}{body}\n"));
    }
    // A declared unit shadows the intrinsic elapsed-time suffix of the same
    // name (ORNA-UNIT-008); undeclared temporal suffixes stay intrinsic.
    assert_valid("pub dim Time;\npub unit s : Time base;\npub fn f(): Int<s> = 5.s;\n");
    assert_valid("pub fn recent() = now() - 7.days;\npub fn soon() = now() + 90.min;\n");
}

#[test]
fn incompatible_dimensions_are_rejected_with_e5001() {
    for body in [
        "pub fn bad() = 1.m + 1.kg;",
        "pub fn bad(a: Float<m>, b: Float<kg>) = a - b;",
    ] {
        assert_code(&format!("{LENGTH}{body}\n"), DIAG_INCOMPATIBLE_DIMENSIONS);
    }
    let named = analysis(&format!("{LENGTH}pub fn bad() = 1.m + 1.kg;\n"));
    assert!(
        named
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "cannot add Length and Mass"),
        "{:?}",
        named.diagnostics
    );
    assert!(!analysis(&format!("{LENGTH}pub fn bad(d: Float<m>, t: Float<sec>): Float<m> = d / t;\n")).is_ok());
}

#[test]
fn affine_units_follow_point_and_vector_algebra_whatever_their_name() {
    for body in [
        "pub fn d(): Int<deltaC> = 30.C - 20.C;",
        "pub fn t(): Int<C> = 20.C + 5.deltaC;",
        "pub fn d(): Int<deltaC> = 2 * 5.deltaC;",
        "pub fn hottest(values: [Float<C>]) = values | max;",
    ] {
        assert_valid(&format!("{TEMPERATURE}{body}\n"));
    }
    for body in [
        "pub fn bad() = 20.C + 5.C;",
        "pub fn bad(a: Float<C>, b: Float<C>) = a + b;",
        "pub fn bad() = 20.F + 5.F;",
    ] {
        assert_code(&format!("{TEMPERATURE}{body}\n"), DIAG_AFFINE_ADDITION);
    }
    for body in [
        "pub fn bad() = [20.C, 22.C] | sum();",
        "pub fn bad(values: [Float<C>]) = values | sum;",
        "pub fn bad(values: [Float<F>]) = values | sum;",
    ] {
        assert_code(&format!("{TEMPERATURE}{body}\n"), DIAG_AFFINE_SUM);
    }
    assert!(!analysis(&format!("{TEMPERATURE}pub fn bad() = 2 * 20.C;\n")).is_ok());
    assert!(!analysis(&format!("{TEMPERATURE}pub fn bad(): Int<C> = 30.C - 20.C;\n")).is_ok());
}

#[test]
fn money_rates_reduce_over_declared_units() {
    assert_valid(&format!(
        "{CURRENCY}pub table Tariff(effective_from: Date) {{\n    rate: Money<GBP> / kWh,\n}}\n\npub fn cost(energy: Decimal<kWh>, tariff: Tariff): Money<GBP> =\n    energy * tariff.rate;\n"
    ));
    assert_code(
        &format!("{CURRENCY}pub fn bad(energy: Float<kWh>, rate: Money<GBP> / kWh) = energy * rate;\n"),
        DIAG_FLOAT_MONEY,
    );
}

#[test]
fn undeclared_and_ambiguous_units_are_rejected() {
    for source in [
        "pub fn bad() = 5.banana;\n",
        "pub fn bad(x: Float<kWh>): Float<kWh> = x;\n",
        "pub fn d(): Int<m> = 5.m;\n",
        "pub fn total(left: Float<nowhere.kWh>, right: Float<elsewhere.kWh>) = left + right;\n",
    ] {
        assert_code(source, DIAG_UNRESOLVED);
    }
    let ambiguous = analyze(&[
        ModuleInput::new("main.orna", "use a.*;\nuse b.*;\n\npub fn bad() = 5.m;\n"),
        ModuleInput::new("a.orna", "pub dim L;\npub unit m : L base;\n"),
        ModuleInput::new("b.orna", "pub dim L2;\npub unit m : L2 base;\n"),
    ]);
    assert!(codes(&ambiguous).contains(&DIAG_AMBIGUOUS), "{:?}", ambiguous.diagnostics);
    let imported = analyze(&[
        ModuleInput::new("main.orna", "use units;\nuse units.{m};\n\npub fn d(): Int<units.m> = 5.m;\n"),
        ModuleInput::new("units.orna", "pub dim L;\npub unit m : L base;\n"),
    ]);
    assert!(!codes(&imported).contains(&DIAG_UNRESOLVED), "{:?}", imported.diagnostics);
}

#[test]
fn fixture_environment_supplies_the_corpus_units() {
    let catalogue = Catalogue::authoritative_fixture();
    let check = |source: &str| analyze_with_catalogue(&[ModuleInput::new("fixture.orna", source)], &catalogue);
    for source in [
        "pub fn hottest(values: [Float<C>]) = values | max;\n",
        "pub fn converted(speed: Float<mph>) = speed.mph;\npub fn duration() = 90.min;\n",
        "pub fn compatible(a: Float<std.units.si.kWh>, b: Float<work.units.kWh>) = a + b;\n",
    ] {
        let analysis = check(source);
        assert!(analysis.is_ok(), "{source}\n{:?}", analysis.diagnostics);
    }
    let dimensions = check("pub fn bad() = 90.days + 4.kWh;\n");
    assert!(
        dimensions.diagnostics.iter().any(|diagnostic| diagnostic.code() == DIAG_INCOMPATIBLE_DIMENSIONS
            && diagnostic.message() == "cannot add Time and Energy"),
        "{:?}",
        dimensions.diagnostics
    );
    assert!(codes(&check("pub fn bad() = 20.C + 5.C;\n")).contains(&DIAG_AFFINE_ADDITION));
    assert!(codes(&check("pub fn bad(values: [Float<C>]) = values | sum;\n")).contains(&DIAG_AFFINE_SUM));
}
