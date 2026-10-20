#![cfg(feature = "system-host-bindings")]

//! ORNA-PLAN-002: `sys.explain(Query)` must expose planner choices through the
//! typed `sys.Plan` record, pinned by the host catalogue, and must fail closed
//! for stages the host cannot represent. Inspection must not scan relation
//! rows or go through effect handlers.

use std::sync::Arc;

use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, ExplainCatalogue, Functions, Limits,
    PureFunction, RelationPage, StepBudget, invoke_named_with_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::{ObjectRef, SnapshotRef};
use orna_syntax_v1::{Expr, parse_expression};
use orna_value_v1::Raw;

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

#[derive(Debug, Default)]
struct ExplainEffects {
    catalogue: Option<Arc<FixedCatalogue>>,
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
        _: Option<&[u8]>,
        _: usize,
        _: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        self.scans += 1;
        Ok(None)
    }

    fn explain_catalogue(&self) -> Option<&dyn ExplainCatalogue> {
        self.catalogue
            .as_deref()
            .map(|catalogue| catalogue as &dyn ExplainCatalogue)
    }
}

fn explain_outcome(
    query: &str,
    effects: &mut ExplainEffects,
) -> Result<CanonicalValue, EvaluationError> {
    let parsed = parse_expression(&format!("sys.explain({query})"));
    assert!(
        parsed.is_ok(),
        "fixture should parse: {query}: {:?}",
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

fn storage_catalogue() -> ExplainEffects {
    ExplainEffects {
        catalogue: Some(Arc::new(FixedCatalogue {
            source: "sys.Storage",
        })),
        ..ExplainEffects::default()
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
    let mut effects = storage_catalogue();
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

/// ORNA-PLAN-002: inspection is effect-free; explain must neither scan relation
/// rows nor route through effect handlers.
#[test]
fn explain_does_not_scan_or_invoke_effects() {
    let mut effects = storage_catalogue();
    explain_outcome("sys.Storage | distinct() | take(2)", &mut effects)
        .expect("distinct then take should explain");
    assert_eq!(effects.scans, 0, "explain must not scan relation rows");
    assert_eq!(effects.effect_calls, 0, "explain must not invoke effect handlers");
}

/// ORNA-PLAN-003: stages without a snapshot-pinned representation (filter)
/// fail closed instead of being silently dropped from the plan.
#[test]
fn explain_filter_fails_closed() {
    let mut effects = storage_catalogue();
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
    let mut effects = storage_catalogue();
    assert_eq!(
        error_code("sys.Storage | take(2) | distinct()", &mut effects),
        "sys.abi.provider_failed"
    );
}

/// ORNA-PLAN-002: a source the host catalogue cannot resolve yields a typed
/// provider failure; no reference is synthesised.
#[test]
fn explain_unknown_source_fails_closed() {
    let mut effects = storage_catalogue();
    assert_eq!(
        error_code("sys.MaintenanceJob | distinct()", &mut effects),
        "sys.abi.provider_failed"
    );
}
