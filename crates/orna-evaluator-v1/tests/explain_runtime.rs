#![cfg(feature = "system-host-bindings")]

//! ORNA-PLAN-002, ORNA-SYS-053: `sys.explain` must expose planner choices
//! through the typed `sys.Plan` record pinned by the host catalogue, explain a
//! `sys.Diagnostic` as a structured `sys.Explanation`, and fail closed with
//! portable failure codes (ORNA-SYS-096/097) for inputs the host cannot pin or
//! represent. Inspection must not scan relation rows or go through effect
//! handlers.

use std::collections::BTreeMap;
use std::sync::Arc;

use orna_evaluator_v1::{
    ActivationExplainCatalogue, ContextValueEffectHandler, EffectHandler, Environment,
    EvaluationError, ExplainCatalogue, Functions, Limits, Ovb2EffectHandler, PureFunction,
    RelationPage, StepBudget, evaluate_expression_ovb2_with_effects,
    invoke_named_with_context_effects, invoke_named_with_effects,
};
use orna_foundation_v1::{
    CanonicalSnapshot, CanonicalValue, FunctionRef, GitHash, OvbRaw, RowRef, Snapshot,
    function_reference, object_reference, snapshot_reference,
};
use orna_sys_v1::{
    DependencyConfidence, DependencyInput, DependencyKind, FunctionPlanDescription, ObjectRef,
    QueryPlanDescription, SnapshotRef, explain_function, explain_query,
};
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
    catalogue: Option<Arc<dyn ExplainCatalogue>>,
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
        self.catalogue.as_deref()
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

fn storage_explain_catalogue() -> Option<Arc<dyn ExplainCatalogue>> {
    storage_catalogue().map(|catalogue| catalogue as Arc<dyn ExplainCatalogue>)
}

fn storage_effects() -> ExplainEffects {
    ExplainEffects {
        catalogue: storage_explain_catalogue(),
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

/// ORNA-PLAN-002, ORNA-SYS-008: explain without a host catalogue cannot pin the
/// source and must report the unavailable coordinate with the portable
/// `sys.version.incompatible` code rather than inventing a reference.
#[test]
fn explain_without_catalogue_reports_unavailable() {
    let mut effects = ExplainEffects::default();
    assert_eq!(
        error_code("sys.Storage | distinct() | take(2)", &mut effects),
        "sys.version.incompatible"
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
            catalogue: storage_explain_catalogue(),
            rows: rows.clone(),
            ..ExplainEffects::default()
        };
        let result_without =
            invoke_expression(&query, &mut without).expect("query without explain");

        let mut with = ExplainEffects {
            catalogue: storage_explain_catalogue(),
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

/// ORNA-PLAN-003, ORNA-SYS-008: stages without a snapshot-pinned
/// representation (filter) fail closed with `sys.version.incompatible` instead
/// of being silently dropped from the plan.
#[test]
fn explain_filter_fails_closed() {
    let mut effects = storage_effects();
    assert_eq!(
        error_code(
            "sys.Storage | filter(storage => storage.pending_rows > 0)",
            &mut effects
        ),
        "sys.version.incompatible"
    );
}

/// ORNA-PLAN-002: take followed by distinct is not the same query as distinct
/// followed by take, so the host must not lower it to one representation.
#[test]
fn explain_take_then_distinct_fails_closed() {
    let mut effects = storage_effects();
    assert_eq!(
        error_code("sys.Storage | take(2) | distinct()", &mut effects),
        "sys.version.incompatible"
    );
}

/// ORNA-PLAN-002: a source the host catalogue does not hold is
/// `sys.object.not_found`; no reference is synthesised.
#[test]
fn explain_unknown_source_fails_closed() {
    let mut effects = storage_effects();
    assert_eq!(
        error_code("sys.MaintenanceJob | distinct()", &mut effects),
        "sys.object.not_found"
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
    assert_eq!(error.code(), "sys.version.incompatible");
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

const DATABASE: [u8; 16] = [0x42; 16];

fn commit_snapshot() -> CanonicalSnapshot {
    Snapshot::Commit {
        database: DATABASE,
        algorithm: GitHash::Sha1,
        oid: vec![0x11; 20],
    }
}

fn canonical_reference(reference: &RowRef) -> CanonicalValue {
    CanonicalValue::decode(&reference.encode().expect("reference encodes"))
        .expect("reference decodes")
}

fn object_value(id: u8) -> CanonicalValue {
    let object = object_reference(DATABASE, [id; 16], commit_snapshot()).expect("object reference");
    canonical_reference(object.as_row_ref())
}

fn function_ref(id: u8) -> FunctionRef {
    let object = object_reference(DATABASE, [id; 16], commit_snapshot()).expect("object reference");
    function_reference(object).expect("function reference")
}

/// Evaluates `expression` with `binding` captured as an immutable value.
fn invoke_with_binding(
    expression: &str,
    binding: &str,
    value: CanonicalValue,
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
            environment: Environment::from([(binding.to_owned(), value)]),
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

/// A catalogue that holds one function identity, as a catalogue with function
/// object identities would.
#[derive(Debug)]
struct FunctionCatalogue {
    function: FunctionRef,
}

fn report_function_description() -> FunctionPlanDescription {
    FunctionPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot-explain"),
        function: orna_sys_v1::FunctionRef::descriptive("function-report"),
        dependencies: vec![DependencyInput::new(
            ObjectRef::descriptive("function-report"),
            ObjectRef::descriptive("object-storage"),
            DependencyKind::TableRead,
            None,
            None,
            DependencyConfidence::Exact,
            false,
        )],
    }
}

impl ExplainCatalogue for FunctionCatalogue {
    fn resolve_source(&self, _: &str) -> Option<(SnapshotRef, ObjectRef)> {
        None
    }

    fn resolves_functions(&self) -> bool {
        true
    }

    fn describe_function(&self, function: &FunctionRef) -> Option<FunctionPlanDescription> {
        (*function == self.function).then(report_function_description)
    }
}

fn function_effects() -> ExplainEffects {
    ExplainEffects {
        catalogue: Some(Arc::new(FunctionCatalogue {
            function: function_ref(7),
        })),
        ..ExplainEffects::default()
    }
}

/// ORNA-SYS-053: `sys.explain(FunctionRef)` returns the structured `sys.Plan`
/// the published planner produces for the catalogue's function description,
/// in both the explicit and the piped form, without scanning or effects.
#[test]
fn explain_function_returns_catalogue_function_plan() {
    let mut effects = function_effects();
    let target = canonical_reference(function_ref(7).as_row_ref());
    let value = invoke_with_binding(
        "sys.explain(target)",
        "target",
        target.clone(),
        &mut effects,
    )
    .expect("a resolved function should explain");
    let explained = explain_function(&report_function_description()).expect("explain_function");
    let plan = explained.plan();
    let raw = value.raw();
    assert_eq!(text(field(raw, "reference")), plan.reference().as_str());
    assert_eq!(text(field(raw, "id")), plan.id());
    assert_eq!(text(field(raw, "snapshot")), "snapshot-explain");
    assert_eq!(text(field(raw, "root")), plan.root().as_str());
    assert_eq!(
        field(raw, "actual_available"),
        &Raw::Bool(plan.actual_available())
    );
    assert_eq!(field(raw, "warnings"), &Raw::Array(Vec::new()));
    let piped = invoke_with_binding("target | sys.explain()", "target", target, &mut effects)
        .expect("the piped form should explain");
    assert_eq!(piped, value);
    assert_eq!(effects.scans, 0, "explain must not scan relation rows");
    assert_eq!(
        effects.effect_calls, 0,
        "explain must not invoke effect handlers"
    );
}

/// A function reference a resolving catalogue does not hold is
/// `sys.object.not_found`. The production activation catalogue holds no
/// function object identity, so it cannot describe any function reference: the
/// function coordinate is unavailable (`sys.version.incompatible`), not absent.
#[test]
fn explain_unresolved_function_is_version_incompatible() {
    let mut effects = function_effects();
    let other = canonical_reference(function_ref(8).as_row_ref());
    assert_eq!(
        invoke_with_binding("sys.explain(target)", "target", other, &mut effects)
            .expect_err("an unresolved function must fail")
            .code(),
        "sys.object.not_found"
    );
    let mut production = ExplainEffects {
        catalogue: Some(Arc::new(
            ActivationExplainCatalogue::new(&commit_snapshot(), ["sys.Storage"])
                .expect("activation catalogue"),
        )),
        ..ExplainEffects::default()
    };
    let target = canonical_reference(function_ref(7).as_row_ref());
    assert_eq!(
        invoke_with_binding("sys.explain(target)", "target", target, &mut production)
            .expect_err("the activation catalogue resolves no function")
            .code(),
        "sys.version.incompatible"
    );
}

/// A row reference of another relation is not a function reference:
/// `sys.object.wrong_kind`.
#[test]
fn explain_object_reference_is_wrong_kind() {
    let mut effects = function_effects();
    assert_eq!(
        invoke_with_binding(
            "sys.explain(target)",
            "target",
            object_value(7),
            &mut effects
        )
        .expect_err("an object reference is not a function")
        .code(),
        "sys.object.wrong_kind"
    );
}

/// ORNA-SYS-008: without a host catalogue a function reference cannot be
/// resolved, so the coordinate is unavailable.
#[test]
fn explain_function_without_catalogue_reports_unavailable() {
    let mut effects = ExplainEffects::default();
    let target = canonical_reference(function_ref(7).as_row_ref());
    assert_eq!(
        invoke_with_binding("sys.explain(target)", "target", target, &mut effects)
            .expect_err("explain without a catalogue must fail")
            .code(),
        "sys.version.incompatible"
    );
}

fn canonical_map(entries: Vec<(&str, OvbRaw)>) -> OvbRaw {
    let mut entries = entries
        .into_iter()
        .map(|(key, value)| (OvbRaw::Text(key.to_owned()), value))
        .collect::<Vec<_>>();
    entries.sort_by_cached_key(|(key, _)| {
        CanonicalValue::new(key.clone())
            .expect("text key")
            .encode()
            .expect("text key encodes")
    });
    OvbRaw::Map(entries)
}

fn absent() -> OvbRaw {
    Value::option(None).expect("none option").raw().clone()
}

/// One `sys.Diagnostic` row with every field of the published relation.
fn diagnostic_row(
    id: &str,
    code: &str,
    message: &str,
    redacted: bool,
    object: Option<u8>,
    help: &[&str],
    causes: Vec<OvbRaw>,
) -> OvbRaw {
    let reference = RowRef::new(
        DATABASE,
        [0x0d; 16],
        OvbRaw::Text(id.to_owned()),
        commit_snapshot(),
    )
    .expect("diagnostic reference");
    let object = match object {
        Some(id) => Value::option(Some(object_value(id)))
            .expect("present option")
            .raw()
            .clone(),
        None => absent(),
    };
    canonical_map(vec![
        ("reference", canonical_reference(&reference).raw().clone()),
        ("id", OvbRaw::Text(id.to_owned())),
        ("severity", OvbRaw::Text("error".to_owned())),
        ("code", OvbRaw::Text(code.to_owned())),
        ("message", OvbRaw::Text(message.to_owned())),
        ("object", object),
        ("definition", absent()),
        ("primary_span", absent()),
        ("labels", OvbRaw::Array(Vec::new())),
        ("causes", OvbRaw::Array(causes)),
        (
            "help",
            OvbRaw::Array(
                help.iter()
                    .map(|item| OvbRaw::Text((*item).to_owned()))
                    .collect(),
            ),
        ),
        ("data", absent()),
        ("redacted", OvbRaw::Bool(redacted)),
        ("trace", absent()),
    ])
}

/// ORNA-SYS-046, ORNA-SYS-048: `sys.explain(Diagnostic)` returns a structured
/// `sys.Explanation`: the row itself, its safe message as the summary, its
/// causes, its help as suggestions, the objects of the row and its causes in
/// first-occurrence order, and no plan. It needs no host catalogue and
/// performs no scan or effect.
#[test]
fn explain_diagnostic_returns_structured_explanation() {
    let cause = diagnostic_row(
        "diagnostic-cause",
        "sys.object.not_found",
        "table Contact is not in the selected snapshot",
        false,
        Some(9),
        &[],
        Vec::new(),
    );
    let row = diagnostic_row(
        "diagnostic-root",
        "ORNA-A091-009",
        "table assertion failed for Contact 7",
        false,
        Some(7),
        &["inspect the deterministic safe witness"],
        vec![cause.clone()],
    );
    let mut effects = ExplainEffects::default();
    let value = invoke_with_binding(
        "sys.explain(diagnostic)",
        "diagnostic",
        CanonicalValue::new(row.clone()).expect("canonical diagnostic row"),
        &mut effects,
    )
    .expect("a diagnostic should explain");
    let raw = value.raw();
    assert_eq!(
        text(field(raw, "summary")),
        "table assertion failed for Contact 7"
    );
    assert_eq!(field(raw, "diagnostic"), &row);
    assert_eq!(field(raw, "causes"), &Raw::Array(vec![cause]));
    assert_eq!(
        field(raw, "suggestions"),
        &Raw::Array(vec![Raw::Text(
            "inspect the deterministic safe witness".to_owned()
        )])
    );
    assert_eq!(
        field(raw, "related_objects"),
        &Raw::Array(vec![
            object_value(7).raw().clone(),
            object_value(9).raw().clone(),
        ])
    );
    assert_eq!(field(raw, "plan"), &absent());
    assert_eq!(effects.scans, 0, "explain must not scan relation rows");
    assert_eq!(
        effects.effect_calls, 0,
        "explain must not invoke effect handlers"
    );
}

/// ORNA-SYS-100: a redacted diagnostic keeps its explicit marker; explain
/// neither reveals nor fabricates text for it.
#[test]
fn explain_redacted_diagnostic_keeps_the_explicit_marker() {
    let row = diagnostic_row(
        "diagnostic-secret",
        "ORNA-EVAL-ERROR",
        "<redacted>",
        true,
        None,
        &[],
        Vec::new(),
    );
    let mut effects = ExplainEffects::default();
    let value = invoke_with_binding(
        "diagnostic | sys.explain()",
        "diagnostic",
        CanonicalValue::new(row.clone()).expect("canonical diagnostic row"),
        &mut effects,
    )
    .expect("a redacted diagnostic should explain");
    let raw = value.raw();
    assert_eq!(text(field(raw, "summary")), "<redacted>");
    assert_eq!(field(raw, "diagnostic"), &row);
    assert_eq!(field(raw, "related_objects"), &Raw::Array(Vec::new()));
}

/// A record without the `sys.Diagnostic` fields is not a diagnostic.
#[test]
fn explain_record_without_diagnostic_fields_is_a_type_error() {
    let OvbRaw::Map(mut entries) = diagnostic_row(
        "diagnostic-partial",
        "ORNA-EVAL-ERROR",
        "partial",
        false,
        None,
        &[],
        Vec::new(),
    ) else {
        panic!("diagnostic rows are maps");
    };
    entries.retain(|(key, _)| key != &OvbRaw::Text("trace".to_owned()));
    let mut effects = ExplainEffects::default();
    assert_eq!(
        invoke_with_binding(
            "sys.explain(diagnostic)",
            "diagnostic",
            CanonicalValue::new(OvbRaw::Map(entries)).expect("canonical record"),
            &mut effects,
        )
        .expect_err("a partial row is not a diagnostic")
        .code(),
        "ORNA-EVAL-TYPE"
    );
}

/// The production activation catalogue pins every source to the
/// `sys.Snapshot` natural key of the activation snapshot, names each object by
/// its canonical relation name and resolves nothing else.
#[test]
fn activation_catalogue_pins_sources_to_the_snapshot_key() {
    let snapshot = commit_snapshot();
    let catalogue = ActivationExplainCatalogue::new(&snapshot, ["sys.Storage", "main.Contact"])
        .expect("activation catalogue");
    let reference = snapshot_reference(DATABASE, snapshot).expect("snapshot reference");
    let OvbRaw::Bytes(id) = &reference.as_row_ref().key else {
        panic!("sys.Snapshot keys are byte identities");
    };
    let expected = format!(
        "snapshot:{}",
        id.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    for source in ["sys.Storage", "main.Contact"] {
        assert_eq!(
            catalogue.resolve_source(source),
            Some((
                SnapshotRef::descriptive(expected.clone()),
                ObjectRef::descriptive(source)
            ))
        );
    }
    assert_eq!(catalogue.resolve_source("sys.MaintenanceJob"), None);
    let mut effects = ExplainEffects {
        catalogue: Some(Arc::new(catalogue)),
        ..ExplainEffects::default()
    };
    let value = explain_outcome("sys.Storage | take(1)", &mut effects)
        .expect("the activation catalogue should pin sys.Storage");
    assert_eq!(text(field(value.raw(), "snapshot")), expected);
}
