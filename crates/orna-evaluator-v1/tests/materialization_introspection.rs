use num_bigint::BigInt;
use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits, PureFunction, RelationPage,
    StepBudget, invoke_named_with_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Expr, parse_expression};
use orna_value_v1::Raw;

fn integer(value: i64) -> Raw {
    Raw::Int(BigInt::from(value))
}

fn instant() -> CanonicalValue {
    CanonicalValue::new(Raw::Tag(
        60002,
        Box::new(Raw::Array(vec![
            Raw::Int(1_700_000_000.into()),
            Raw::Int(7.into()),
        ])),
    ))
    .expect("instant is canonical")
}

struct StorageEffects {
    rows: Vec<i64>,
}

impl EffectHandler for StorageEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if matches!(callee, Expr::Name { text, .. } if text == "now") {
            assert!(arguments.is_empty());
            return Ok(Some(instant()));
        }
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        _limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if source != "sys.Storage" {
            return Ok(None);
        }
        budget.debit(1)?;
        let index = after.map_or(0, |cursor| usize::from(cursor[0]));
        let Some(row) = self.rows.get(index) else {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        };
        let row = CanonicalValue::new(integer(*row)).expect("row is canonical");
        Ok(Some(RelationPage {
            rows: vec![row],
            next: (index + 1 < self.rows.len()).then(|| vec![(index + 1) as u8]),
        }))
    }
}

fn run(source: &str, rows: Vec<i64>) -> CanonicalValue {
    let parsed = parse_expression(source);
    assert!(
        parsed.is_ok(),
        "fixture should parse: {source}: {:?}",
        parsed.diagnostics
    );
    let functions = Functions::from([(
        "query".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment: Environment::new(),
        },
    )]);
    let mut effects = StorageEffects { rows };
    invoke_named_with_effects(
        "query",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .expect("query should evaluate")
}

#[test]
fn materialization_is_empty_before_any_buffered_decision() {
    assert_eq!(
        run("sys.Materialization | count()", vec![2, 1]).raw(),
        &integer(0)
    );
}

#[test]
fn each_buffered_decision_is_visible_to_later_introspection_in_the_same_activation() {
    // List elements evaluate left to right: each sum buffers one decision over
    // sys.Storage before the introspection count reads the activation log.
    let result = run(
        "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | count()]",
        vec![2, 1],
    );

    assert_eq!(
        result.raw(),
        &Raw::Array(vec![integer(3), integer(3), integer(2)])
    );
}

#[test]
fn materialization_digest_is_deterministic_for_equal_rows_and_differs_otherwise() {
    // The digest is observed through a projection after one buffered decision.
    let query = "[sys.Storage | filter(value => value >= 0) | sum(),
          sys.Materialization | project(m => m.digest) | first()]";
    let same_a = run(query, vec![2, 1]);
    let same_b = run(query, vec![2, 1]);
    let different = run(query, vec![5, 1]);

    assert_eq!(same_a, same_b, "equal buffered rows must yield equal digests");
    assert_ne!(
        same_a, different,
        "different buffered rows must yield different digests"
    );
}
