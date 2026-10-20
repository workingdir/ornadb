use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits,
    invoke_named_with_effects_and_reuse_report,
};
use orna_syntax_v1::{Declaration, Expr};
use orna_value_v1::Value;

#[derive(Default)]
struct NoteEffects {
    calls: Vec<Vec<Value>>,
}

impl EffectHandler for NoteEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text, .. } = base.as_ref() else {
            return Ok(None);
        };
        if text != "Note" || name != "insert" {
            return Ok(None);
        }
        self.calls.push(arguments.to_vec());
        Ok(Some(Value::int(arguments.len().into())))
    }
}

fn functions_from_source(source: &str) -> Functions {
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    parsed
        .value
        .items
        .into_iter()
        .map(|item| {
            let Declaration::Function { signature, body } = item.declaration else {
                panic!("function expected")
            };
            (
                signature.name,
                orna_evaluator_v1::PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            )
        })
        .collect()
}

/// ORNA-CALL-001: equal pure calls in one activation return equal values, and
/// the second call is served from the activation memo.
#[test]
fn equal_pure_calls_in_one_activation_reuse_the_stored_value() {
    let functions = functions_from_source(
        "fn helper(value: Int) = value + 1;\nfn entry() = helper(41) + helper(41);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(84.into()));
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.stores, 1);
    assert_eq!(stats.invalidations, 0);
    assert!(effects.calls.is_empty());
}

/// ORNA-CALL-001: reuse is keyed by the supplied arguments, so different
/// arguments never observe each other's result.
#[test]
fn calls_with_different_arguments_do_not_reuse() {
    let functions = functions_from_source(
        "fn helper(value: Int) = value + 1;\nfn entry() = helper(41) + helper(42);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(85.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
}

/// ORNA-CALL-002: an external effect between two equal pure calls is a
/// boundary, so the second call must be evaluated again rather than reused.
#[test]
fn external_effect_between_equal_calls_invalidates_reuse() {
    let functions = functions_from_source(
        "fn helper(value: Int) = value + 1;\nfn entry() = helper(41) + Note.insert(1) + helper(41);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(85.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.invalidations, 1);
    assert_eq!(effects.calls.len(), 1);
}

/// ORNA-CALL-002: a function whose body performs an effect is never stored, so
/// equal source and arguments do not collapse its effects.
#[test]
fn effectful_function_calls_are_not_collapsed() {
    let functions = functions_from_source(
        "fn record(value: Int) = Note.insert(value);\nfn entry() = record(1) + record(1);",
    );
    let mut effects = NoteEffects::default();

    let (value, stats) = invoke_named_with_effects_and_reuse_report(
        "entry",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap();

    assert_eq!(value, Value::int(2.into()));
    assert_eq!(stats.hits, 0);
    assert_eq!(stats.stores, 0);
    assert_eq!(effects.calls.len(), 2);
}
