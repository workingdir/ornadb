//! Syntax of unit families: the `= unit_expr` variant form and the `impl Unit {}` marker
//! (docs/design/units.md section 2; docs/language/reference/01-lexical-and-grammar.md).

use orna_syntax::parse_module;

use orna_syntax_ast::{Declaration, EnumVariant, Implementation, UnitExpr};

const FAMILY_BASE_AND_SCALES: &str =
    include_str!("fixtures/units/valid/family-base-and-scales.orna");
const FAMILY_QUALIFIED_DERIVED: &str =
    include_str!("fixtures/units/valid/family-qualified-derived.orna");
const FAMILY_PRODUCTS_AND_POWERS: &str =
    include_str!("fixtures/units/valid/family-products-and-powers.orna");
const PLAIN_ENUM_WITH_PAYLOAD: &str =
    include_str!("fixtures/units/valid/plain-enum-with-payload.orna");
const PLAIN_ENUM_BARE_VARIANTS: &str =
    include_str!("fixtures/units/valid/plain-enum-bare-variants.orna");

const SCALE_IN_UNMARKED_ENUM: &str =
    include_str!("fixtures/units/invalid/scale-in-unmarked-enum.orna");
const PAYLOAD_IN_UNIT_FAMILY: &str =
    include_str!("fixtures/units/invalid/payload-in-unit-family.orna");
const PAYLOAD_WITH_SCALE: &str = include_str!("fixtures/units/invalid/payload-with-scale.orna");
const MARKER_NOT_LAST: &str = include_str!("fixtures/units/invalid/marker-not-last.orna");
const MISSING_UNIT_NAME: &str = include_str!("fixtures/units/invalid/missing-unit-name.orna");
const DANGLING_OPERATOR: &str = include_str!("fixtures/units/invalid/dangling-operator.orna");
const MISSING_EXPONENT: &str = include_str!("fixtures/units/invalid/missing-exponent.orna");

/// The enum named `name` in a parsed module: its variants, its remaining implementations, and its family flag.
fn enum_named<'a>(
    module: &'a orna_syntax::Parse<orna_syntax_ast::SyntaxTree>,
    name: &str,
) -> (&'a [EnumVariant], &'a [Implementation], bool) {
    module
        .value
        .items
        .iter()
        .find_map(|item| match &item.declaration {
            Declaration::Enum {
                name: found,
                variants,
                implementations,
                unit_family,
                ..
            } if found == name => Some((
                variants.as_slice(),
                implementations.as_slice(),
                *unit_family,
            )),
            _ => None,
        })
        .unwrap_or_else(|| panic!("enum {name} is declared"))
}

fn unit_of<'a>(variants: &'a [EnumVariant], name: &str) -> &'a UnitExpr {
    variants
        .iter()
        .find(|variant| variant.name == name)
        .and_then(|variant| variant.unit.as_ref())
        .unwrap_or_else(|| panic!("variant {name} has a unit expression"))
}

fn codes(source: &str) -> Vec<&'static str> {
    let parsed = parse_module(source);
    assert!(!parsed.is_ok(), "expected diagnostics for {source:?}");
    parsed
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn marked_enum_is_a_unit_family_and_loses_its_marker() {
    let parsed = parse_module(FAMILY_BASE_AND_SCALES);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, implementations, unit_family) = enum_named(&parsed, "Length");
    assert!(unit_family);
    assert!(
        implementations.is_empty(),
        "the marker is not an implementation: {implementations:?}"
    );
    assert_eq!(
        variants
            .iter()
            .map(|variant| variant.name.as_str())
            .collect::<Vec<_>>(),
        ["metre", "kilometre", "mile", "m"]
    );
}

#[test]
fn bare_variant_is_the_base_unit_without_an_expression() {
    let parsed = parse_module(FAMILY_BASE_AND_SCALES);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, _, _) = enum_named(&parsed, "Length");
    assert!(variants[0].unit.is_none());
}

#[test]
fn scale_keeps_coefficient_and_unit_name_as_written() {
    let parsed = parse_module(FAMILY_BASE_AND_SCALES);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, _, _) = enum_named(&parsed, "Length");
    let mile = unit_of(variants, "mile");
    assert_eq!(mile.terms.len(), 1);
    assert!(!mile.terms[0].divide);
    assert_eq!(mile.terms[0].coefficient.as_deref(), Some("1609.344"));
    assert_eq!(mile.terms[0].name, "metre");
    assert_eq!(mile.terms[0].family, None);
    assert_eq!(mile.terms[0].exponent, None);
}

#[test]
fn bare_alias_is_a_unit_expression_with_no_coefficient() {
    let parsed = parse_module(FAMILY_BASE_AND_SCALES);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, _, _) = enum_named(&parsed, "Length");
    let alias = unit_of(variants, "m");
    assert_eq!(alias.terms.len(), 1);
    assert_eq!(alias.terms[0].coefficient, None);
    assert_eq!(alias.terms[0].name, "metre");
}

#[test]
fn derived_family_keeps_qualified_names_and_division_order() {
    let parsed = parse_module(FAMILY_QUALIFIED_DERIVED);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, implementations, unit_family) = enum_named(&parsed, "Speed");
    assert!(unit_family);
    assert!(implementations.is_empty());
    let kmh = unit_of(variants, "kmh");
    assert_eq!(
        kmh.terms
            .iter()
            .map(|term| (term.divide, term.family.as_deref(), term.name.as_str()))
            .collect::<Vec<_>>(),
        [
            (false, Some("Length"), "kilometre"),
            (true, Some("Duration"), "hour")
        ]
    );
    assert!(variants.iter().all(|variant| variant.unit.is_some()));
}

#[test]
fn products_and_powers_keep_operators_and_exponents() {
    let parsed = parse_module(FAMILY_PRODUCTS_AND_POWERS);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, _, unit_family) = enum_named(&parsed, "Acceleration");
    assert!(unit_family);
    let g = unit_of(variants, "g");
    assert_eq!(
        g.terms
            .iter()
            .map(|term| (
                term.divide,
                term.coefficient.as_deref(),
                term.name.as_str(),
                term.exponent.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            (false, Some("9.80665"), "metre", None),
            (true, None, "second", Some("2"))
        ]
    );
}

#[test]
fn enum_without_marker_is_ordinary_and_keeps_payloads() {
    let parsed = parse_module(PLAIN_ENUM_WITH_PAYLOAD);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, _, unit_family) = enum_named(&parsed, "Shape");
    assert!(!unit_family);
    assert_eq!(variants.len(), 2);
    assert!(variants.iter().all(|variant| variant.unit.is_none()));
    assert_eq!(variants[0].fields.len(), 1);
}

#[test]
fn enum_without_marker_with_bare_variants_is_not_a_family() {
    let parsed = parse_module(PLAIN_ENUM_BARE_VARIANTS);
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);
    let (variants, _, unit_family) = enum_named(&parsed, "Colour");
    assert!(!unit_family);
    assert_eq!(variants.len(), 3);
}

#[test]
fn scale_in_enum_without_marker_is_rejected() {
    let found = codes(SCALE_IN_UNMARKED_ENUM);
    assert!(found.contains(&"ORNA-UNIT-009"), "codes={found:?}");
}

#[test]
fn payload_variant_in_unit_family_is_rejected() {
    let found = codes(PAYLOAD_IN_UNIT_FAMILY);
    assert!(found.contains(&"ORNA-UNIT-009"), "codes={found:?}");
}

#[test]
fn payload_with_scale_is_rejected() {
    let found = codes(PAYLOAD_WITH_SCALE);
    assert!(found.contains(&"ORNA-UNIT-009"), "codes={found:?}");
}

#[test]
fn marker_must_be_the_last_line() {
    let found = codes(MARKER_NOT_LAST);
    assert!(found.contains(&"ORNA-UNIT-009"), "codes={found:?}");
}

#[test]
fn scale_without_unit_name_is_a_parse_error() {
    let found = codes(MISSING_UNIT_NAME);
    assert!(found.contains(&"ORNA-PARSE-001"), "codes={found:?}");
}

#[test]
fn dangling_operator_is_a_parse_error() {
    let found = codes(DANGLING_OPERATOR);
    assert!(found.contains(&"ORNA-PARSE-001"), "codes={found:?}");
}

#[test]
fn missing_exponent_is_a_parse_error() {
    let found = codes(MISSING_EXPONENT);
    assert!(found.contains(&"ORNA-PARSE-001"), "codes={found:?}");
}
