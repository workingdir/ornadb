use orna_evaluator_v1::{
    Environment, Functions, Limits, NominalDefinition, NominalDefinitions, NominalVariant,
    PureFunction, evaluate_with_functions_and_nominals,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Declaration, parse_expression, parse_module};
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("fixture values are canonical")
}

fn object_id(marker: u8) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(vec![marker; 16])))
}

fn result_enum(variant: u8, field: &str, value: Raw) -> Raw {
    Raw::Tag(
        60008,
        Box::new(Raw::Array(vec![
            object_id(1),
            object_id(variant),
            Raw::Tag(
                60009,
                Box::new(Raw::Array(vec![
                    Raw::Null,
                    Raw::Array(vec![Raw::Array(vec![Raw::Text(field.to_owned()), value])]),
                ])),
            ),
        ])),
    )
}

fn pinned_runtime() -> (Functions, NominalDefinitions) {
    let selected = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            matches!(
                path.as_str(),
                "std/collection.orna"
                    | "std/concurrent/main.orna"
                    | "std/concurrent/result.orna"
                    | "std/result.orna"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 4, "the result adapter's pinned modules exist");

    let mut functions = Functions::new();
    for (path, source) in selected {
        let parsed = parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let namespace = path
            .strip_prefix("std/")
            .expect("standard module paths start with std/")
            .strip_suffix(".orna")
            .expect("standard module paths end with .orna")
            .strip_suffix("/main")
            .unwrap_or_else(|| {
                path.strip_prefix("std/")
                    .unwrap()
                    .strip_suffix(".orna")
                    .unwrap()
            })
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

    let result_definition = NominalDefinition::new(
        [1; 16],
        Some("std.result".into()),
        Vec::new(),
    )
    .with_enum_variants(vec![
        NominalVariant::new([2; 16], "Ok"),
        NominalVariant::new([3; 16], "Err"),
    ]);
    let nominal_definitions = NominalDefinitions::from([(
        "std.result.Result".into(),
        result_definition,
    )]);
    (functions, nominal_definitions)
}

fn outcomes() -> CanonicalValue {
    canonical(Raw::Array(vec![
        result_enum(2, "value", Raw::Int(4.into())),
        result_enum(3, "error", Raw::Text("first".into())),
        result_enum(2, "value", Raw::Null),
        result_enum(3, "error", Raw::Text("second".into())),
        result_enum(2, "value", Raw::Int(9.into())),
    ]))
}

fn evaluate_fixture(fixture: &str) -> CanonicalValue {
    let (functions, nominal_definitions) = pinned_runtime();
    let expression = parse_expression(fixture);
    assert!(expression.is_ok(), "{:#?}", expression.diagnostics);
    let environment = Environment::from([("outcomes".into(), outcomes())]);
    evaluate_with_functions_and_nominals(
        &expression.value,
        &environment,
        &functions,
        &nominal_definitions,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("pinned async result helper failed: {}", error.code()))
}

#[test]
fn success_and_error_payloads_keep_their_own_order_and_duplicates() {
    assert_eq!(
        evaluate_fixture(include_str!(
            "fixtures/stdlib-concurrent-result-partition-w8xxwk.orna"
        )),
        canonical(Raw::Bool(true))
    );
}

#[test]
fn empty_results_partition_into_two_empty_lists() {
    assert_eq!(
        evaluate_fixture(include_str!(
            "fixtures/stdlib-concurrent-result-empty-w8xxwk.orna"
        )),
        canonical(Raw::Bool(true))
    );
}

#[test]
fn concurrent_callback_results_partition_in_callback_input_order() {
    assert_eq!(
        evaluate_fixture(include_str!(
            "fixtures/stdlib-concurrent-result-parallel-w8xxwk.orna"
        )),
        canonical(Raw::Bool(true))
    );
}

#[test]
fn parallel_map_results_partition_in_value_input_order() {
    assert_eq!(
        evaluate_fixture(include_str!(
            "fixtures/stdlib-concurrent-result-parallel-map-w8xxwk.orna"
        )),
        canonical(Raw::Bool(true))
    );
}
