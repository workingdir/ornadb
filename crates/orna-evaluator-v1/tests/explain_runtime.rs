#![cfg(feature = "system-host-bindings")]

//! ORNA-PLAN-002: `sys.explain(Query)` must expose planner choices through the
//! typed `sys.Plan` record, pinned by the host catalogue, and must fail closed
//! for stages the host cannot represent. Inspection must not scan relation
//! rows or go through effect handlers.

use std::collections::BTreeMap;
use std::sync::Arc;

use orna_evaluator_v1::{
    ContextValueEffectHandler, EffectHandler, Environment, EvaluationError, ExplainCatalogue,
    Functions, Limits, Ovb2EffectHandler, PureFunction, RelationPage, StepBudget,
    evaluate_expression_ovb2_with_effects, invoke_named_with_context_effects,
    invoke_named_with_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::{ObjectRef, QueryPlanDescription, SnapshotRef, explain_query};
use orna_syntax_v1::{Expr, parse_expression};
use orna_value_v1::{ContextValue, Raw, Value};

#[derive(Debug)]
struct FixedCatalogue {
    source: &'static str,
}

impl ExplainCatalogue for FixedCatalogue {
    fn resolve_source(&self, source: &str) -> Option<(SnapshotRef, ObjectRef)> {
        (source == self.source).then(|| {
            (
                SnapshotRef::descriptive("snapshot-explain"),
                ObjectRef::descriptive("object-storage"),
            )
        })
    }
}

fn storage_catalogue() -> Option<Arc<FixedCatalogue>> {
    Some(Arc::new(FixedCatalogue {
        source: "sys.Storage",
    }))
}

#[derive(Debug, Default)]
struct ExplainEffects {
    catalogue: Option<Arc<FixedCatalogue>>,
    rows: Vec<CanonicalValue>,
    requests: Vec<Option<Vec<u8>>>,
    scans: usize,
    effect_calls: usize,
}

impl EffectHandler for ExplainEffects {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.effect_calls += 1;
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        _: &str,
        after: Option<&[u8]>,
        _: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        self.scans += 1;
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

    fn explain_catalogue(&self) -> Option<&dyn ExplainCatalogue> {
        self.catalogue
            .as_deref()
            .map(|catalogue| catalogue as &dyn ExplainCatalogue)
    }
}

/// Evaluates `expression` as the body of a zero-argument named function.
fn invoke_expression(
    expression: &str,
    effects: &mut ExplainEffects,
) -> Result<CanonicalValue, EvaluationError> {
    let parsed = parse_expression(expression);
    assert!(
        parsed.is_ok(),
        "fixture should parse: {expression}: {:?}",
        parsed.diagnostics
    );
    let functions = Functions::from([(
        "explain_fixture".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment: Environment::new(),
        },
    )]);
    invoke_named_with_effects(
        "explain_fixture",
        &functions,
        &Environment::new(),
        Limits::default(),
        effects,
    )
}

fn explain_outcome(
    query: &str,
    effects: &mut ExplainEffects,
) -> Result<CanonicalValue, EvaluationError> {
    invoke_expression(&format!("sys.explain({query})"), effects)
}

fn error_code(query: &str, effects: &mut ExplainEffects) -> String {
    explain_outcome(query, effects)
        .expect_err("explain should fail closed")
        .code()
        .to_owned()
}

fn field<'a>(record: &'a Raw, name: &str) -> &'a Raw {
    let Raw::Map(entries) = record else {
        panic!("sys.Plan must be a record, got {record:?}");
    };
    entries
        .iter()
        .find_map(|(key, value)| match key {
            Raw::Text(text) if text == name => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("sys.Plan is missing field {name}"))
}

fn text(value: &Raw) -> &str {
    match value {
        Raw::Text(text) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

fn storage_effects() -> ExplainEffects {
    ExplainEffects {
        catalogue: storage_catalogue(),
        ..ExplainEffects::default()
    }
}

/// The pinned description `sys.explain(sys.Storage | distinct() | take(2))`
/// must lower to.
fn distinct_take_description() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot-explain"),
        source: ObjectRef::descriptive("object-storage"),
        source_statistics: None,
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct: true,
        ordering: Vec::new(),
        limit: Some(2),
        mutations: Vec::new(),
        materialize_into: None,
    }
}

/// ORNA-PLAN-002: explain without a host catalogue cannot pin the source and
/// must report the unavailable boundary rather than inventing a reference.
#[test]
fn explain_without_catalogue_reports_unavailable() {
    let mut effects = ExplainEffects::default();
    assert_eq!(
        error_code("sys.Storage | distinct() | take(2)", &mut effects),
        "sys.abi.unavailable"
    );
}

/// ORNA-PLAN-002: a pinned source with distinct and take lowers to a typed
/// `sys.Plan` record carrying the pinned snapshot and a plan-node root.
#[test]
fn explain_distinct_then_take_returns_pinned_plan_record() {
    let mut effects = storage_effects();
    let value = explain_outcome("sys.Storage | distinct() | take(2)", &mut effects)
        .expect("distinct then take should explain");
    let raw = value.raw();
    assert_eq!(text(field(raw, "snapshot")), "snapshot-explain");
    assert!(
        text(field(raw, "root")).starts_with("plan:"),
        "root is a plan-node reference"
    );
    assert_eq!(field(raw, "warnings"), &Raw::Array(Vec::new()));
}

/// ORNA-PLAN-002: all seven `sys.Plan` fields are the values the published
/// `orna-sys-v1` explanation produces for the same pinned description.
#[test]
fn explain_record_carries_all_seven_plan_fields() {
    let mut effects = storage_effects();
    let value = explain_outcome("sys.Storage | distinct() | take(2)", &mut effects)
        .expect("distinct then take should explain");
    let raw = value.raw();

    let explained = explain_query(&distinct_take_description()).expect("explain_query");
    let plan = explained.plan();

    assert_eq!(
        text(field(raw, "reference")),
        plan.reference().as_str(),
        "reference"
    );
    assert_eq!(text(field(raw, "id")), plan.id(), "id");
    assert_eq!(
        text(field(raw, "snapshot")),
        plan.snapshot().as_str(),
        "snapshot"
    );
    assert_eq!(text(field(raw, "root")), plan.root().as_str(), "root");
    match plan.estimated_cost() {
        None => assert_eq!(
            field(raw, "estimated_cost"),
            Value::option(None).expect("none option").raw(),
            "estimated_cost"
        ),
        Some(_) => assert_ne!(
            field(raw, "estimated_cost"),
            Value::option(None).expect("none option").raw(),
            "estimated_cost"
        ),
    }
    assert_eq!(
        field(raw, "actual_available"),
        &Raw::Bool(plan.actual_available()),
        "actual_available"
    );
    assert!(plan.warnings().is_empty(), "explain_query emits no warnings");
    assert_eq!(field(raw, "warnings"), &Raw::Array(Vec::new()), "warnings");
}

/// ORNA-PLAN-002: explain is effect-free; it must neither scan relation rows
/// nor route through effect handlers.
#[test]
fn explain_does_not_scan_or_invoke_effects() {
    let mut effects = storage_effects();
    explain_outcome("sys.Storage | distinct() | take(2)", &mut effects)
        .expect("distinct then take should explain");
    assert_eq!(effects.scans, 0, "explain must not scan relation rows");
    assert_eq!(effects.effect_calls, 0, "explain must not invoke effect handlers");
}

/// ORNA-PLAN-002: the piped form `rel | sys.explain()` explains the piped
/// relation, identically to the explicit `sys.explain(rel)` form.
#[test]
fn piped_explain_matches_explicit_argument() {
    let mut piped = storage_effects();
    let piped_value = invoke_expression("sys.Storage | distinct() | take(2) | sys.explain()", &mut piped)
        .expect("piped explain should explain");
    let mut explicit = storage_effects();
    let explicit_value = explain_outcome("sys.Storage | distinct() | take(2)", &mut explicit)
        .expect("explicit explain should explain");
    assert_eq!(piped_value, explicit_value);
    assert_eq!(piped.scans, 0, "piped explain must not scan relation rows");
}

/// ORNA-PLAN-002: an explicit argument alongside piped input is rejected.
#[test]
fn piped_explain_with_explicit_argument_fails_closed() {
    let mut effects = storage_effects();
    assert_eq!(
        invoke_expression(
            "sys.Storage | distinct() | take(2) | sys.explain(sys.Storage | distinct() | take(2))",
            &mut effects,
        )
        .expect_err("piped input plus argument must fail")
        .code(),
        "ORNA-EVAL-ARGUMENT"
    );
}

/// ORNA-PLAN-002: the same query evaluated with and without a preceding
/// `sys.explain` of it yields identical rows, scan counts and cursors.
#[test]
fn explain_leaves_same_query_rows_scans_and_cursors_unchanged() {
    let rows = vec![
        CanonicalValue::new(Raw::Int(1.into())).expect("row"),
        CanonicalValue::new(Raw::Int(2.into())).expect("row"),
        CanonicalValue::new(Raw::Int(3.into())).expect("row"),
    ];
    for terminal in ["count()", "first()"] {
        let query = format!("sys.Storage | distinct() | take(2) | {terminal}");

        let mut without = ExplainEffects {
            catalogue: storage_catalogue(),
            rows: rows.clone(),
            ..ExplainEffects::default()
        };
        let result_without =
            invoke_expression(&query, &mut without).expect("query without explain");

        let mut with = ExplainEffects {
            catalogue: storage_catalogue(),
            rows: rows.clone(),
            ..ExplainEffects::default()
        };
        explain_outcome("sys.Storage | distinct() | take(2)", &mut with)
            .expect("preceding explain");
        assert_eq!(with.scans, 0, "preceding explain must not scan");
        assert!(with.requests.is_empty(), "preceding explain must not read cursors");
        with.scans = 0;
        with.requests.clear();
        let result_with = invoke_expression(&query, &mut with).expect("query after explain");

        assert!(without.scans > 0, "fixture must exercise relation scans: {terminal}");
        assert_eq!(result_with, result_without, "rows: {terminal}");
        assert_eq!(with.scans, without.scans, "scan count: {terminal}");
        assert_eq!(with.requests, without.requests, "cursors: {terminal}");
    }
}

/// ORNA-PLAN-003: stages without a snapshot-pinned representation (filter)
/// fail closed instead of being silently dropped from the plan.
#[test]
fn explain_filter_fails_closed() {
    let mut effects = storage_effects();
    assert_eq!(
        error_code(
            "sys.Storage | filter(storage => storage.pending_rows > 0)",
            &mut effects
        ),
        "sys.abi.provider_failed"
    );
}

/// ORNA-PLAN-002: take followed by distinct is not the same query as distinct
/// followed by take, so the host must not lower it to one representation.
#[test]
fn explain_take_then_distinct_fails_closed() {
    let mut effects = storage_effects();
    assert_eq!(
        error_code("sys.Storage | take(2) | distinct()", &mut effects),
        "sys.abi.provider_failed"
    );
}

/// ORNA-PLAN-002: a source the host catalogue cannot resolve yields a typed
/// provider failure; no reference is synthesised.
#[test]
fn explain_unknown_source_fails_closed() {
    let mut effects = storage_effects();
    assert_eq!(
        error_code("sys.MaintenanceJob | distinct()", &mut effects),
        "sys.abi.provider_failed"
    );
}

/// OVB-2 entry point: a handler that supplies a host catalogue lets
/// `sys.explain` resolve the pinned source through `evaluate_expression_ovb2`.
struct Ovb2ExplainHandler {
    catalogue: Option<Arc<FixedCatalogue>>,
    effect_calls: usize,
}

impl Ovb2EffectHandler for Ovb2ExplainHandler {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.effect_calls += 1;
        Ok(None)
    }

    fn explain_catalogue(&self) -> Option<&dyn ExplainCatalogue> {
        self.catalogue
            .as_deref()
            .map(|catalogue| catalogue as &dyn ExplainCatalogue)
    }
}

#[test]
fn ovb2_entry_point_forwards_explain_catalogue() {
    let mut handler = Ovb2ExplainHandler {
        catalogue: storage_catalogue(),
        effect_calls: 0,
    };
    let value = evaluate_expression_ovb2_with_effects(
        "sys.explain(sys.Storage | distinct() | take(2))",
        &BTreeMap::new(),
        Limits::default(),
        &mut handler,
    )
    .expect("OVB-2 entry should explain through the forwarded catalogue");
    assert_eq!(
        text(field(value.raw(), "snapshot")),
        "snapshot-explain"
    );
    assert_eq!(handler.effect_calls, 0, "explain must not invoke effect handlers");
}

#[test]
fn ovb2_entry_point_without_catalogue_reports_unavailable() {
    let mut handler = Ovb2ExplainHandler {
        catalogue: None,
        effect_calls: 0,
    };
    let error = evaluate_expression_ovb2_with_effects(
        "sys.explain(sys.Storage | distinct() | take(2))",
        &BTreeMap::new(),
        Limits::default(),
        &mut handler,
    )
    .expect_err("OVB-2 explain without a catalogue must fail");
    assert_eq!(error.code(), "sys.abi.unavailable");
}

/// Context-value entry point: a handler that supplies a host catalogue lets
/// `sys.explain` resolve the pinned source through `invoke_named_with_context_effects`.
struct ContextExplainHandler {
    catalogue: Option<Arc<FixedCatalogue>>,
}

impl EffectHandler for ContextExplainHandler {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn explain_catalogue(&self) -> Option<&dyn ExplainCatalogue> {
        self.catalogue
            .as_deref()
            .map(|catalogue| catalogue as &dyn ExplainCatalogue)
    }
}

impl ContextValueEffectHandler for ContextExplainHandler {
    fn handle_context(
        &mut self,
        _: &Expr,
        _: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        Ok(None)
    }
}

#[test]
fn context_entry_point_forwards_explain_catalogue() {
    let parsed = parse_expression("sys.explain(sys.Storage | distinct() | take(2))");
    assert!(parsed.is_ok(), "fixture should parse: {:?}", parsed.diagnostics);
    let functions = Functions::from([(
        "explain_fixture".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment: Environment::new(),
        },
    )]);
    let mut handler = ContextExplainHandler {
        catalogue: storage_catalogue(),
    };
    let mut budget = StepBudget::new(Limits::default().max_steps);
    let value = invoke_named_with_context_effects(
        "explain_fixture",
        &functions,
        &BTreeMap::new(),
        Limits::default(),
        &mut handler,
        &mut budget,
    )
    .expect("context entry should explain through the forwarded catalogue");
    assert_eq!(
        text(field(value.raw(), "snapshot")),
        "snapshot-explain"
    );
}
