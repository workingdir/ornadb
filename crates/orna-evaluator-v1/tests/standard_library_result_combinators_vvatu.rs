use orna_evaluator_v1::{
    Environment, EvaluationError, Functions, Limits, NominalDefinition, NominalDefinitions,
    NominalVariant, PureFunction, evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::{Raw, Value};

fn object_id(marker: u8) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(vec![marker; 16])))
}

fn result_value(variant: u8, field: &str, value: Raw) -> Raw {
    Raw::Tag(
        60008,
        Box::new(Raw::Array(vec![
            object_id(1),
            object_id(variant),
            Raw::Tag(
                60009,
                Box::new(Raw::Array(vec![
                    Raw::Null,
                    Raw::Array(vec![Raw::Array(vec![Raw::Text(field.into()), value])]),
                ])),
            ),
        ])),
    )
}

fn option_value(value: Option<Raw>) -> Raw {
    let value = value.map(|value| Value::new(value).expect("nested value is canonical"));
    Value::option(value)
        .expect("option is canonical")
        .raw()
        .clone()
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("fixture value is canonical")
}

fn pinned_runtime() -> (Functions, NominalDefinitions) {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/collection.orna" || path == "std/result.orna")
        .collect::<Vec<_>>();
    assert_eq!(
        sources.len(),
        2,
        "the proof loads result and its collection dependency"
    );

    let mut functions = Functions::new();
    for (path, source) in sources {
        let parsed = parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let namespace = path
            .strip_prefix("std/")
            .expect("standard module paths start with std/")
            .strip_suffix(".orna")
            .expect("standard module paths end with .orna")
            .replace('/', ".");
        for item in parsed.value.items {
            if let Declaration::Function { signature, body } = item.declaration {
                functions.insert(
                    format!("std.{namespace}.{}", signature.name),
                    PureFunction {
                        parameters: signature.parameters,
                        body,
                        environment: Environment::new(),
                    },
                );
            }
        }
    }

    let result_definition = NominalDefinition::new([1; 16], Some("std.result".into()), Vec::new())
        .with_enum_variants(vec![
            NominalVariant::new([2; 16], "Ok"),
            NominalVariant::new([3; 16], "Err"),
        ]);
    let nominal_definitions =
        NominalDefinitions::from([("std.result.Result".into(), result_definition)]);
    (functions, nominal_definitions)
}

fn environment() -> Environment {
    let ok_value = result_value(2, "value", Raw::Int(4.into()));
    let second_ok = result_value(2, "value", Raw::Int(6.into()));
    let err_value = result_value(3, "error", Raw::Text("failure".into()));
    Environment::from([
        ("ok_value".into(), canonical(ok_value.clone())),
        ("second_ok".into(), canonical(second_ok)),
        ("err_value".into(), canonical(err_value.clone())),
        (
            "nested_ok".into(),
            canonical(result_value(2, "value", ok_value.clone())),
        ),
        (
            "ok_none".into(),
            canonical(result_value(2, "value", option_value(None))),
        ),
    ])
}

fn assert_true_fixture(fixture: &str) {
    let (functions, nominal_definitions) = pinned_runtime();
    let environment = environment();
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = parse_expression(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let result = evaluate_with_functions_and_nominals(
            &parsed.value,
            &environment,
            &functions,
            &nominal_definitions,
            Limits::default(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "result behavior proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            canonical(Raw::Bool(true)),
            "result behavior fixture failed: {expression}",
        );
    }
}

fn evaluate_fixture(fixture: &str) -> Result<CanonicalValue, EvaluationError> {
    let (functions, nominal_definitions) = pinned_runtime();
    let environment = environment();
    let parsed = parse_expression(fixture);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    evaluate_with_functions_and_nominals(
        &parsed.value,
        &environment,
        &functions,
        &nominal_definitions,
        Limits::default(),
    )
}

#[test]
fn result_variant_queries_preserve_payloads() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-result-combinators-values-vvatu.orna"
    ));
}

#[test]
fn result_fallbacks_are_selected_by_the_active_variant() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-result-combinators-fallbacks-vvatu.orna"
    ));
}

#[test]
fn result_branch_combinators_preserve_existing_values() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-result-combinators-branches-vvatu.orna"
    ));
    assert_true_fixture(include_str!(
        "fixtures/stdlib-result-combinators-transpose-vvatu.orna"
    ));
}

#[test]
fn result_combinators_propagate_failures_from_selected_callbacks() {
    let error = evaluate_fixture(include_str!(
        "fixtures/stdlib-result-combinators-callback-failure-vvatu.orna"
    ))
    .expect_err("a failure raised by an active Result callback must propagate");
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}

#[test]
fn result_map_error_transforms_only_the_error_branch() {
    assert_true_fixture(include_str!("fixtures/stdlib-result-map-error-proof.orna"));
}

#[test]
fn result_map_error_composes_with_error_queries() {
    assert_true_fixture(include_str!(
        "fixtures/stdlib-result-map-error-compose.orna"
    ));
}

#[test]
fn result_map_error_propagates_failures_from_its_callback() {
    let error = evaluate_fixture(include_str!(
        "fixtures/stdlib-result-map-error-callback-failure.orna"
    ))
    .expect_err("a failure raised by the map_error callback on Err must propagate");
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}
