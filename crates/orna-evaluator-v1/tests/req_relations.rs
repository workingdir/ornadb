use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits, PureFunction, RelationPage,
    StepBudget, evaluate_expression_with_effects, invoke_named_with_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{Expr, LambdaParameter, LiteralKind, Pattern, SyntaxSpan};
use orna_value_v1::{Raw, Value};

struct OrderedRows {
    rows: Vec<CanonicalValue>,
    requests: Vec<Option<Vec<u8>>>,
}

impl EffectHandler for OrderedRows {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        _: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        assert_eq!(source, "sys.Storage");
        budget.debit(1)?;
        self.requests.push(after.map(ToOwned::to_owned));
        let index = after.map_or(0, |cursor| usize::from(cursor[0]));
        let Some(row) = self.rows.get(index).cloned() else {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        };
        let next = (index + 1 < self.rows.len()).then(|| vec![(index + 1) as u8]);
        Ok(Some(RelationPage {
            rows: vec![row],
            next,
        }))
    }
}

fn span() -> SyntaxSpan {
    SyntaxSpan::new(0, 0)
}

fn argument(value: Expr, span: &SyntaxSpan) -> orna_syntax_v1::Argument {
    orna_syntax_v1::Argument {
        name: None,
        value,
        span: span.clone(),
    }
}

fn relation_source() -> Expr {
    let span = span();
    Expr::Call {
        callee: Box::new(Expr::Field {
            base: Box::new(Expr::ReplBinding {
                text: "$__orna_relation".into(),
                span: span.clone(),
            }),
            name: "source".into(),
            span: span.clone(),
        }),
        arguments: vec![argument(
            Expr::Literal {
                text: "\"sys.Storage\"".into(),
                kind: LiteralKind::String,
                span: span.clone(),
            },
            &span,
        )],
        span,
    }
}

fn integer(value: i64) -> Expr {
    let span = span();
    Expr::Literal {
        text: value.to_string(),
        kind: LiteralKind::Integer,
        span,
    }
}

fn relation_stage(input: Expr, name: &str, arguments: Vec<Expr>) -> Expr {
    let span = span();
    Expr::Binary {
        lhs: Box::new(input),
        op: "|".into(),
        rhs: Box::new(Expr::Call {
            callee: Box::new(Expr::Name {
                text: name.into(),
                span: span.clone(),
            }),
            arguments: arguments.into_iter().map(|value| argument(value, &span)).collect(),
            span: span.clone(),
        }),
        span,
    }
}

fn filter_above_one(input: Expr) -> Expr {
    let span = span();
    let value = Expr::Name {
        text: "value".into(),
        span: span.clone(),
    };
    let predicate = Expr::Binary {
        lhs: Box::new(value),
        op: ">".into(),
        rhs: Box::new(integer(1)),
        span: span.clone(),
    };
    let lambda = Expr::Lambda {
        parameters: vec![LambdaParameter {
            pattern: Pattern::Name("value".into(), span.clone()),
            annotation: None,
            span: span.clone(),
        }],
        body: Box::new(predicate),
        span,
    };
    relation_stage(input, "filter", vec![lambda])
}

fn invoke(body: Expr, effects: &mut OrderedRows) -> CanonicalValue {
    let functions = Functions::from([(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body,
            environment: Environment::new(),
        },
    )]);
    invoke_named_with_effects("run", &functions, &Environment::new(), Limits::default(), effects)
        .unwrap_or_else(|error| panic!("relation evaluation failed: {}", error.code()))
}

fn rows(values: &[i64]) -> Vec<CanonicalValue> {
    values
        .iter()
        .map(|value| CanonicalValue::new(Raw::Int((*value).into())).unwrap())
        .collect()
}

fn first_window(values: &[i64]) -> CanonicalValue {
    let window = Value::new(Raw::Array(
        values.iter().map(|value| Raw::Int((*value).into())).collect(),
    ))
    .unwrap();
    let option = Value::option(Some(window)).unwrap();
    CanonicalValue::new(option.raw().clone()).unwrap()
}

/// ORNA-REL-002: observing a bounded relation window reads only the rows needed for that window.
#[test]
fn first_window_observes_only_its_bounded_source_prefix() {
    let mut effects = OrderedRows {
        rows: rows(&[10, 20, 30, 40, 50]),
        requests: Vec::new(),
    };
    let windows = relation_stage(relation_source(), "window", vec![integer(3)]);
    let result = invoke(relation_stage(windows, "first", Vec::new()), &mut effects);
    assert_eq!(result, first_window(&[10, 20, 30]));
    assert_eq!(effects.requests, vec![None, Some(vec![1]), Some(vec![2])]);
}

/// This asserts the tested pipeline’s order; it does not demonstrate optimizer reordering.
#[test]
fn filtered_window_observes_source_order() {
    let mut effects = OrderedRows {
        rows: rows(&[4, 1, 3, 2]),
        requests: Vec::new(),
    };
    let filtered = filter_above_one(relation_source());
    let windows = relation_stage(filtered, "window", vec![integer(2)]);
    let result = invoke(relation_stage(windows, "first", Vec::new()), &mut effects);
    assert_eq!(result, first_window(&[4, 3]));
}

/// Admitted committed table rows: the table's name, piped into a relation
/// operation, reads that table's rows in key order through the same paged
/// scan that `sys.*` sources use.
struct TableRows {
    table: &'static str,
    rows: Vec<CanonicalValue>,
}

impl EffectHandler for TableRows {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn admitted_relation_source(&self, name: &str) -> Option<String> {
        (name == self.table).then(|| name.to_owned())
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        assert_eq!(source, self.table);
        budget.debit(1)?;
        let start = after.map_or(0, |cursor| usize::from(cursor[0]));
        let end = (start + limit.max(1)).min(self.rows.len());
        let next = (end < self.rows.len()).then(|| vec![end as u8]);
        Ok(Some(RelationPage {
            rows: self.rows[start..end].to_vec(),
            next,
        }))
    }
}

fn table_rows(values: &[i64]) -> TableRows {
    TableRows {
        table: "Note",
        rows: rows(values),
    }
}

fn int_result(value: i64) -> CanonicalValue {
    CanonicalValue::new(Raw::Int(value.into())).unwrap()
}

/// ORNA-REL-001: a committed table name piped into `count()` reads every row of that table.
#[test]
fn committed_table_name_counts_its_rows() {
    let mut effects = table_rows(&[3, 1, 2]);
    let result = evaluate_expression_with_effects(
        "Note | count()",
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap_or_else(|error| panic!("table count failed: {}", error.code()));
    assert_eq!(result, int_result(3));
}

/// ORNA-REL-001: an empty committed table yields an empty relation, not an error.
#[test]
fn empty_committed_table_counts_zero() {
    let mut effects = table_rows(&[]);
    let result = evaluate_expression_with_effects(
        "Note | count()",
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap_or_else(|error| panic!("empty table count failed: {}", error.code()));
    assert_eq!(result, int_result(0));
}

/// ORNA-REL-001 with ORNA-ORDER-001: a table pipeline filters the committed rows in order.
#[test]
fn committed_table_name_filters_its_rows() {
    let mut effects = table_rows(&[3, 1, 2]);
    let result = evaluate_expression_with_effects(
        "Note | filter(row => row > 1) | count()",
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .unwrap_or_else(|error| panic!("filtered table count failed: {}", error.code()));
    assert_eq!(result, int_result(2));
}

/// Without an admitted table source, a bare name keeps its ordinary resolution and fails closed.
#[test]
fn unadmitted_name_in_relation_pipeline_fails_closed() {
    let mut effects = table_rows(&[3, 1, 2]);
    let error = evaluate_expression_with_effects(
        "Other | count()",
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .expect_err("an unadmitted name must not read a table");
    assert_eq!(error.code(), "ORNA-EVAL-NAME");
}
