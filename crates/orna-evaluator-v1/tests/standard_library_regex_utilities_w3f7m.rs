use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, Functions, Limits, NominalDefinition, NominalDefinitions, NominalVariant,
    PureFunction, evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::Raw;

const MODULES: [&str; 3] = [
    "std/collection.orna",
    "std/regex.orna",
    "std/regex/utilities.orna",
];
const MATCH_TYPE_ID: [u8; 16] = [0x61; 16];
const MATCH_FOUND_VARIANT_ID: [u8; 16] = [0x62; 16];

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("regex utility proof value is canonical")
}

fn object_id(bytes: [u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(bytes.to_vec())))
}

fn found_match(text: &str, start: i64, end: i64, captures: &[Option<&str>]) -> Raw {
    let payload = Raw::Tag(
        60009,
        Box::new(Raw::Array(vec![
            Raw::Null,
            Raw::Array(vec![
                Raw::Array(vec![Raw::Text("end".into()), Raw::Int(end.into())]),
                Raw::Array(vec![Raw::Text("text".into()), Raw::Text(text.into())]),
                Raw::Array(vec![Raw::Text("start".into()), Raw::Int(start.into())]),
                Raw::Array(vec![
                    Raw::Text("captures".into()),
                    Raw::Array(
                        captures
                            .iter()
                            .map(|capture| {
                                capture.map_or(Raw::Null, |text| {
                                    Raw::Tag(
                                        60013,
                                        Box::new(Raw::Array(vec![
                                            Raw::Int(1.into()),
                                            Raw::Text(text.into()),
                                        ])),
                                    )
                                })
                            })
                            .collect(),
                    ),
                ]),
            ]),
        ])),
    );
    Raw::Tag(
        60008,
        Box::new(Raw::Array(vec![
            object_id(MATCH_TYPE_ID),
            object_id(MATCH_FOUND_VARIANT_ID),
            payload,
        ])),
    )
}

fn pinned_runtime() -> (Functions, NominalDefinitions) {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
    {
        let parsed = parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let module = path
            .strip_prefix("std/")
            .and_then(|path| path.strip_suffix(".orna"))
            .expect("pinned std paths have a std/ prefix and .orna suffix")
            .replace('/', ".");
        for item in parsed.value.items {
            if let Declaration::Function { signature, body } = item.declaration {
                let name = format!("std.{module}.{}", signature.name);
                assert!(
                    functions
                        .insert(
                            name.clone(),
                            PureFunction {
                                parameters: signature.parameters,
                                body,
                                environment: Environment::new(),
                            },
                        )
                        .is_none(),
                    "duplicate pinned standard function {name}"
                );
            }
        }
    }

    let nominals = NominalDefinitions::from([(
        "std.regex.Match".into(),
        NominalDefinition::new(MATCH_TYPE_ID, Some("std.regex".into()), Vec::new())
            .with_enum_variants(vec![NominalVariant::new(MATCH_FOUND_VARIANT_ID, "Found")]),
    )]);
    (functions, nominals)
}

fn assert_true_fixture(fixture: &str, environment: &Environment) {
    let (functions, nominals) = pinned_runtime();
    let parsed = parse_expression(fixture);
    assert!(parsed.is_ok(), "{fixture}: {:#?}", parsed.diagnostics);
    let actual = evaluate_with_functions_and_nominals(
        &parsed.value,
        environment,
        &functions,
        &nominals,
        Limits::default(),
    )
    .unwrap_or_else(|error| {
        panic!(
            "regex utility proof failed for `{fixture}`: {}",
            error.code()
        )
    });
    assert_eq!(actual, canonical(Raw::Bool(true)), "{fixture}");
}

#[test]
fn capture_lookup_handles_full_matched_unmatched_and_invalid_groups() {
    let environment = Environment::from([(
        "sample".into(),
        canonical(found_match("cat", 4, 7, &[Some("cat"), Some("a"), None])),
    )]);
    assert_true_fixture(
        include_str!("fixtures/stdlib-regex-utility-captures-w3f7m.orna"),
        &environment,
    );
}

#[test]
fn spans_are_half_open_unicode_scalar_positions_and_empty_matches_are_zero_width() {
    let environment = Environment::from([
        (
            "nonempty".into(),
            canonical(found_match("é🦊", 1, 3, &[Some("é🦊")])),
        ),
        (
            "empty".into(),
            canonical(found_match("", 3, 3, &[Some("")])),
        ),
    ]);
    assert_true_fixture(
        include_str!("fixtures/stdlib-regex-utility-spans-w3f7m.orna"),
        &environment,
    );
}

#[test]
fn match_projections_preserve_order_duplicates_and_empty_matches() {
    let environment = Environment::from([(
        "matches".into(),
        canonical(Raw::Array(vec![
            found_match("a", 0, 1, &[Some("a")]),
            found_match("", 1, 1, &[Some("")]),
            found_match("a", 2, 3, &[Some("a")]),
        ])),
    )]);
    assert_true_fixture(
        include_str!("fixtures/stdlib-regex-utility-match-list-w3f7m.orna"),
        &environment,
    );
}

#[test]
fn capture_columns_remain_aligned_when_groups_are_unmatched_or_absent() {
    let environment = Environment::from([(
        "matches".into(),
        canonical(Raw::Array(vec![
            found_match("red", 0, 3, &[Some("red"), Some("r")]),
            found_match("blue", 4, 8, &[Some("blue"), None]),
            found_match("green", 9, 14, &[Some("green")]),
        ])),
    )]);
    assert_true_fixture(
        include_str!("fixtures/stdlib-regex-utility-capture-column-w3f7m.orna"),
        &environment,
    );
}
