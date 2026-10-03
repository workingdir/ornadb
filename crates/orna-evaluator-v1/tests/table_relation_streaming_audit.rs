//! Fixture-backed audit probes for evaluator-owned relation plans.
//!
//! Table storage, schema, activation transactions, and primary-key allocation
//! are outside this crate's effect boundary; these tests cover the ordered,
//! paged relation behavior the evaluator itself owns.

use std::collections::{BTreeMap, VecDeque};

use orna_evaluator_v1::{
    invoke_named_with_effects, EffectHandler, Environment, EvaluationError, Functions, Limits,
    PureFunction, RelationPage, RelationReadScope, StepBudget,
};
use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{parse_module, Argument, Expr, LiteralKind, SyntaxSpan};
use orna_value_v1::Raw;

fn fixture_functions() -> Functions {
    let parsed = parse_module(include_str!("fixtures/table_relation_audit_callbacks.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    parsed
        .value
        .items
        .into_iter()
        .map(|item| {
            let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration else {
                panic!("fixture function expected")
            };
            (
                signature.name,
                PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            )
        })
        .collect()
}

fn integer(value: i64) -> CanonicalValue {
    CanonicalValue::new(Raw::Int(value.into())).unwrap()
}

fn optional(value: CanonicalValue) -> CanonicalValue {
    let value = orna_value_v1::Value::new(value.raw().clone()).unwrap();
    let option = orna_value_v1::Value::option(Some(value)).unwrap();
    CanonicalValue::new(option.raw().clone()).unwrap()
}

fn integer_list(values: &[i64]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values.iter().copied().map(|value| Raw::Int(value.into())).collect(),
    ))
    .unwrap()
}

fn span() -> SyntaxSpan {
    SyntaxSpan::new(0, 0)
}

fn argument(value: Expr, span: &SyntaxSpan) -> Argument {
    Argument { name: None, value, span: span.clone() }
}

fn relation_source(name: &str) -> Expr {
    let span = span();
    Expr::Call {
        callee: Box::new(Expr::Field {
            base: Box::new(Expr::ReplBinding { text: "$__orna_relation".into(), span: span.clone() }),
            name: "source".into(),
            span: span.clone(),
        }),
        arguments: vec![argument(
            Expr::Literal { text: format!("{name:?}"), kind: LiteralKind::String, span: span.clone() },
            &span,
        )],
        span,
    }
}

fn relation_stage(input: Expr, operation: &str, parameters: Vec<Expr>) -> Expr {
    let span = span();
    Expr::Binary {
        lhs: Box::new(input),
        op: "|".into(),
        rhs: Box::new(Expr::Call {
            callee: Box::new(Expr::Name { text: operation.into(), span: span.clone() }),
            arguments: parameters.into_iter().map(|value| argument(value, &span)).collect(),
            span: span.clone(),
        }),
        span,
    }
}

fn named_function(name: &str) -> Expr {
    let span = span();
    Expr::Name { text: name.into(), span }
}

fn integer_literal(value: i64) -> Expr {
    let span = span();
    Expr::Literal { text: value.to_string(), kind: LiteralKind::Integer, span }
}

fn terminal(input: Expr, name: &str) -> Expr {
    relation_stage(input, name, Vec::new())
}

struct PagedSource {
    pages: VecDeque<RelationPage>,
    cursors: Vec<Option<Vec<u8>>>,
    requested_limits: Vec<usize>,
}

impl PagedSource {
    fn new(pages: Vec<RelationPage>) -> Self {
        Self { pages: pages.into(), cursors: Vec::new(), requested_limits: Vec::new() }
    }
}

impl EffectHandler for PagedSource {
    fn handle(&mut self, _: &Expr, _: &[orna_value_v1::Value]) -> Result<Option<orna_value_v1::Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        _: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        budget.debit(1)?;
        self.cursors.push(after.map(ToOwned::to_owned));
        self.requested_limits.push(limit);
        Ok(Some(self.pages.pop_front().unwrap_or(RelationPage { rows: Vec::new(), next: None })))
    }
}

struct ScopedPagedSource {
    pages: BTreeMap<String, VecDeque<RelationPage>>,
    scopes: BTreeMap<String, RelationReadScope>,
    cursors: Vec<(String, RelationReadScope, Option<Vec<u8>>)>,
}

struct PairedViewRefreshSource {
    pending: BTreeMap<String, VecDeque<VecDeque<RelationPage>>>,
    lanes: Vec<(String, RelationReadScope, VecDeque<RelationPage>)>,
    cursors: Vec<(String, RelationReadScope, Option<Vec<u8>>)>,
}

impl PairedViewRefreshSource {
    fn new(
        subscriptions: impl IntoIterator<Item = (&'static str, Vec<RelationPage>)>,
    ) -> Self {
        let mut pending = BTreeMap::<String, VecDeque<VecDeque<RelationPage>>>::new();
        for (source, pages) in subscriptions {
            pending
                .entry(source.to_owned())
                .or_default()
                .push_back(VecDeque::from(pages));
        }
        Self {
            pending,
            lanes: Vec::new(),
            cursors: Vec::new(),
        }
    }
}

impl ScopedPagedSource {
    fn new(pages: impl IntoIterator<Item = (&'static str, Vec<RelationPage>)>) -> Self {
        Self {
            pages: pages
                .into_iter()
                .map(|(source, pages)| (source.to_owned(), pages.into()))
                .collect(),
            scopes: BTreeMap::new(),
            cursors: Vec::new(),
        }
    }
}

struct PairedSubscriptionSource {
    source: &'static str,
    unbound_pages: VecDeque<VecDeque<RelationPage>>,
    lanes: Vec<(RelationReadScope, VecDeque<RelationPage>)>,
    cursors: Vec<(RelationReadScope, Option<Vec<u8>>)>,
}

impl PairedSubscriptionSource {
    fn new(lanes: impl IntoIterator<Item = Vec<RelationPage>>) -> Self {
        Self {
            source: "View.Paired",
            unbound_pages: lanes.into_iter().map(VecDeque::from).collect(),
            lanes: Vec::new(),
            cursors: Vec::new(),
        }
    }
}

impl EffectHandler for PairedSubscriptionSource {
    fn handle(&mut self, _: &Expr, _: &[orna_value_v1::Value]) -> Result<Option<orna_value_v1::Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page_scoped(
        &mut self,
        source: &str,
        scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if source != self.source {
            return Ok(None);
        }
        if limit == 0 {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        budget.debit(1)?;
        let lane = match self.lanes.iter().position(|(existing, _)| *existing == scope) {
            Some(lane) => lane,
            None => {
                let pages = self
                    .unbound_pages
                    .pop_front()
                    .expect("each paired subscription binds to one page stream");
                self.lanes.push((scope, pages));
                self.lanes.len() - 1
            }
        };
        self.cursors.push((scope, after.map(ToOwned::to_owned)));
        Ok(Some(self.lanes[lane].1.pop_front().unwrap_or(RelationPage {
            rows: Vec::new(),
            next: None,
        })))
    }
}

impl EffectHandler for PairedViewRefreshSource {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[orna_value_v1::Value],
    ) -> Result<Option<orna_value_v1::Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page_scoped(
        &mut self,
        source: &str,
        scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if limit == 0 {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        budget.debit(1)?;
        let lane = match self
            .lanes
            .iter()
            .position(|(existing_source, existing_scope, _)| {
                existing_source == source && *existing_scope == scope
            }) {
            Some(lane) => lane,
            None => {
                let Some(pending) = self.pending.get_mut(source) else {
                    return Ok(None);
                };
                let pages = pending
                    .pop_front()
                    .expect("each paired view refresh binds one page stream per source");
                self.lanes.push((source.to_owned(), scope, pages));
                self.lanes.len() - 1
            }
        };
        self.cursors
            .push((source.to_owned(), scope, after.map(ToOwned::to_owned)));
        Ok(Some(self.lanes[lane].2.pop_front().unwrap_or(RelationPage {
            rows: Vec::new(),
            next: None,
        })))
    }
}

impl EffectHandler for ScopedPagedSource {
    fn handle(&mut self, _: &Expr, _: &[orna_value_v1::Value]) -> Result<Option<orna_value_v1::Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page_scoped(
        &mut self,
        source: &str,
        scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if limit == 0 {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        budget.debit(1)?;
        if let Some(previous) = self.scopes.insert(source.to_owned(), scope) {
            assert_eq!(previous, scope, "each page keeps its relation source identity");
        }
        self.cursors.push((source.to_owned(), scope, after.map(ToOwned::to_owned)));
        let Some(pages) = self.pages.get_mut(source) else {
            return Ok(None);
        };
        Ok(Some(pages.pop_front().unwrap_or(RelationPage {
            rows: Vec::new(),
            next: None,
        })))
    }
}

fn page(values: &[i64], next: Option<Vec<u8>>) -> RelationPage {
    RelationPage { rows: values.iter().copied().map(integer).collect(), next }
}

fn run(body: Expr, effects: &mut PagedSource) -> Result<CanonicalValue, EvaluationError> {
    invoke_named_with_effects("run", &Functions::from([(
        "run".into(),
        PureFunction { parameters: Vec::new(), body, environment: Environment::new() },
    )]), &Environment::new(), Limits::default(), effects)
}

fn run_with_fixture_functions(body: Expr, effects: &mut dyn EffectHandler) -> Result<CanonicalValue, EvaluationError> {
    let mut functions = fixture_functions();
    functions.insert("run".into(), PureFunction {
        parameters: Vec::new(),
        body,
        environment: Environment::new(),
    });
    invoke_named_with_effects("run", &functions, &Environment::new(), Limits::default(), effects)
}

#[test]
fn relation_page_rejects_a_cursor_that_stops_advancing() {
    // The public RelationPage cursor contract is strict progress; REL-001
    // relations remain composable only while paged source iteration advances.
    let body = terminal(relation_source("Rows"), "count");
    let mut effects = PagedSource::new(vec![page(&[1], Some(vec![1])), page(&[2], Some(vec![1]))]);

    let error = run(body, &mut effects).unwrap_err();
    assert_eq!(error.code(), "ORNA-EVAL-VALUE");
    assert_eq!(effects.cursors, vec![None, Some(vec![1])]);
}

#[test]
fn relation_page_rejects_empty_nonterminal_pages() {
    // Empty continuation pages cannot compose into a finite observable scan.
    let body = terminal(relation_source("Rows"), "count");
    let mut effects = PagedSource::new(vec![page(&[], Some(vec![1]))]);

    let error = run(body, &mut effects).unwrap_err();
    assert_eq!(error.code(), "ORNA-EVAL-VALUE");
    assert_eq!(effects.cursors, vec![None]);
}

#[test]
fn flat_map_preserves_page_then_inner_order_from_fixture_functions() {
    // ORNA-ORDER-002: input page order precedes each callback's emitted order.
    let flattened = relation_stage(relation_source("Rows"), "flat_map", vec![named_function("expand")]);
    let window = relation_stage(flattened, "window", vec![integer_literal(4)]);
    let first = terminal(window, "first");
    let mut effects = PagedSource::new(vec![
        page(&[1], Some(vec![1])),
        page(&[2], Some(vec![2])),
        page(&[3], None),
    ]);

    assert_eq!(
        run_with_fixture_functions(first, &mut effects).unwrap(),
        optional(integer_list(&[1, 11, 2, 12]))
    );
    assert_eq!(effects.cursors, vec![None, Some(vec![1])]);
}

#[test]
fn stable_sort_keeps_equal_keys_in_source_order_across_pages() {
    // ORNA-ORDER-003: sort_by is stable even when equal keys cross a page edge.
    let sorted = relation_stage(relation_source("Rows"), "sort_by", vec![named_function("parity")]);
    let first_two = relation_stage(sorted, "take", vec![integer_literal(2)]);
    let last = terminal(first_two, "last");
    let mut effects = PagedSource::new(vec![page(&[3, 1], Some(vec![1])), page(&[2, 4], None)]);

    assert_eq!(
        run_with_fixture_functions(last, &mut effects).unwrap(),
        optional(integer(4))
    );
    assert_eq!(effects.cursors, vec![None, Some(vec![1])]);
}

#[test]
fn filtered_mapped_take_stops_after_the_requested_prefix() {
    // ORNA-REL-001/002 and ORNA-ORDER-002: compose stages lazily and do not
    // request an additional source page after the bounded prefix is satisfied.
    let filtered = relation_stage(relation_source("Rows"), "filter", vec![named_function("is_positive")]);
    let mapped = relation_stage(filtered, "map", vec![named_function("add_one")]);
    let taken = relation_stage(mapped, "take", vec![integer_literal(2)]);
    let last = terminal(taken, "last");
    let mut effects = PagedSource::new(vec![page(&[1, -1], Some(vec![1])), page(&[2, 3], Some(vec![2])), page(&[4], None)]);

    assert_eq!(
        run_with_fixture_functions(last, &mut effects).unwrap(),
        optional(integer(3))
    );
    assert_eq!(effects.cursors, vec![None, Some(vec![1])]);
    assert!(effects.requested_limits.iter().all(|limit| *limit > 0));
}

fn incremental_scoped_view_body() -> Expr {
    let left = relation_stage(
        relation_stage(
            relation_source("View.Left"),
            "filter",
            vec![named_function("is_odd")],
        ),
        "filter",
        vec![named_function("below_five")],
    );
    let right = relation_stage(
        relation_stage(
            relation_source("View.Right"),
            "filter",
            vec![named_function("is_even")],
        ),
        "filter",
        vec![named_function("below_eight")],
    );
    let combined = relation_stage(left, "union", vec![right]);
    let refreshed = relation_stage(
        combined,
        "filter",
        vec![named_function("is_positive")],
    );
    let mapped = relation_stage(refreshed, "map", vec![named_function("add_one")]);
    terminal(mapped, "sum")
}

#[test]
fn incremental_view_refresh_keeps_values_scoped_across_read_batches() {
    let mut first_refresh = ScopedPagedSource::new([
        (
            "View.Left",
            vec![page(&[1, 2], Some(vec![1])), page(&[3, 4], None)],
        ),
        (
            "View.Right",
            vec![page(&[4, 5], Some(vec![1])), page(&[6, 7], None)],
        ),
    ]);
    assert_eq!(
        run_with_fixture_functions(incremental_scoped_view_body(), &mut first_refresh).unwrap(),
        integer(18),
        "left odd rows and right even rows each retain their source filter across both pages"
    );
    assert_eq!(
        first_refresh.cursors,
        vec![
            ("View.Left".into(), first_refresh.scopes["View.Left"], None),
            ("View.Left".into(), first_refresh.scopes["View.Left"], Some(vec![1])),
            ("View.Right".into(), first_refresh.scopes["View.Right"], None),
            ("View.Right".into(), first_refresh.scopes["View.Right"], Some(vec![1])),
        ],
        "each scoped source resumes with its own cursor and then advances independently"
    );
    assert_ne!(
        first_refresh.scopes["View.Left"],
        first_refresh.scopes["View.Right"],
        "sibling view sources have independent identities"
    );

    let mut second_refresh = ScopedPagedSource::new([
        (
            "View.Left",
            vec![page(&[-3, 5], Some(vec![2])), page(&[7, 8], None)],
        ),
        (
            "View.Right",
            vec![page(&[2, 9], Some(vec![2])), page(&[8, 11], None)],
        ),
    ]);
    assert_eq!(
        run_with_fixture_functions(incremental_scoped_view_body(), &mut second_refresh).unwrap(),
        integer(3),
        "a refresh computes only the positive even row below eight from its own batches"
    );
    assert_eq!(
        second_refresh.cursors,
        vec![
            ("View.Left".into(), second_refresh.scopes["View.Left"], None),
            ("View.Left".into(), second_refresh.scopes["View.Left"], Some(vec![2])),
            ("View.Right".into(), second_refresh.scopes["View.Right"], None),
            ("View.Right".into(), second_refresh.scopes["View.Right"], Some(vec![2])),
        ]
    );
    assert_ne!(
        first_refresh.scopes["View.Left"],
        second_refresh.scopes["View.Left"],
        "a later refresh receives a fresh scope for newly computed batches"
    );
}

#[test]
fn paired_view_refresh_handoff_keeps_scoped_batches_independent() {
    // The reference is silent about carrying a provider's page streams across
    // paired refresh plans. This test pins the runtime policy: `(source,
    // scope)` owns a batch stream, and a fresh scope rebinds that source from
    // its first page even when opaque cursor bytes repeat.
    let mut source = PairedViewRefreshSource::new([
        (
            "View.Left",
            vec![page(&[1, 2], Some(vec![11])), page(&[3, 4], None)],
        ),
        (
            "View.Right",
            vec![page(&[4, 5], Some(vec![11])), page(&[6, 7], None)],
        ),
        (
            "View.Left",
            vec![page(&[-3, 5], Some(vec![11])), page(&[7, 8], None)],
        ),
        (
            "View.Right",
            vec![page(&[2, 9], Some(vec![11])), page(&[8, 11], None)],
        ),
    ]);

    assert_eq!(
        run_with_fixture_functions(incremental_scoped_view_body(), &mut source).unwrap(),
        integer(18),
        "the first paired view fold combines left odd and right even values"
    );
    assert_eq!(source.lanes.len(), 2);
    assert_eq!(source.lanes[0].0, "View.Left");
    assert_eq!(source.lanes[1].0, "View.Right");
    let first_left = source.lanes[0].1;
    let first_right = source.lanes[1].1;
    assert_ne!(first_left, first_right);

    assert_eq!(
        run_with_fixture_functions(incremental_scoped_view_body(), &mut source).unwrap(),
        integer(3),
        "the refreshed pair folds only its new positive even row"
    );
    assert_eq!(source.lanes.len(), 4);
    assert_eq!(source.lanes[2].0, "View.Left");
    assert_eq!(source.lanes[3].0, "View.Right");
    let refreshed_left = source.lanes[2].1;
    let refreshed_right = source.lanes[3].1;
    assert_ne!(refreshed_left, refreshed_right);
    assert_ne!(first_left, refreshed_left);
    assert_ne!(first_right, refreshed_right);
    assert_eq!(
        source.cursors,
        vec![
            ("View.Left".into(), first_left, None),
            ("View.Left".into(), first_left, Some(vec![11])),
            ("View.Right".into(), first_right, None),
            ("View.Right".into(), first_right, Some(vec![11])),
            ("View.Left".into(), refreshed_left, None),
            ("View.Left".into(), refreshed_left, Some(vec![11])),
            ("View.Right".into(), refreshed_right, None),
            ("View.Right".into(), refreshed_right, Some(vec![11])),
        ],
        "paired sources keep independent batch cursors and refresh from fresh scopes"
    );
}

#[test]
fn paired_view_cursor_scopes_rebind_across_three_page_refresh_handoffs() {
    // Reuse the local `.orna` callback fixture while each scoped view crosses
    // two cursor handoffs. Left and right intentionally share their first
    // opaque checkpoint; each later refresh gets new page streams.
    let mut source = PairedViewRefreshSource::new([
        (
            "View.Left",
            vec![
                page(&[1], Some(vec![11])),
                page(&[3], Some(vec![33])),
                page(&[5], None),
            ],
        ),
        (
            "View.Right",
            vec![
                page(&[2], Some(vec![11])),
                page(&[4], Some(vec![44])),
                page(&[6], None),
            ],
        ),
        (
            "View.Left",
            vec![
                page(&[-3], Some(vec![11])),
                page(&[5], Some(vec![55])),
                page(&[7], None),
            ],
        ),
        (
            "View.Right",
            vec![
                page(&[2], Some(vec![11])),
                page(&[9], Some(vec![66])),
                page(&[8], None),
            ],
        ),
    ]);

    assert_eq!(
        run_with_fixture_functions(incremental_scoped_view_body(), &mut source).unwrap(),
        integer(21),
        "first paired fold computes the positive odd and even values over three pages"
    );
    assert_eq!(source.lanes.len(), 2);
    assert_eq!(source.lanes[0].0, "View.Left");
    assert_eq!(source.lanes[1].0, "View.Right");
    let first_left = source.lanes[0].1;
    let first_right = source.lanes[1].1;
    assert_ne!(first_left, first_right);

    assert_eq!(
        run_with_fixture_functions(incremental_scoped_view_body(), &mut source).unwrap(),
        integer(3),
        "refreshed fold computes only its new positive even value"
    );
    assert_eq!(source.lanes.len(), 4);
    assert_eq!(source.lanes[2].0, "View.Left");
    assert_eq!(source.lanes[3].0, "View.Right");
    let refreshed_left = source.lanes[2].1;
    let refreshed_right = source.lanes[3].1;
    assert_ne!(refreshed_left, refreshed_right);
    assert_ne!(first_left, refreshed_left);
    assert_ne!(first_right, refreshed_right);
    assert_eq!(
        source.cursors,
        vec![
            ("View.Left".into(), first_left, None),
            ("View.Left".into(), first_left, Some(vec![11])),
            ("View.Left".into(), first_left, Some(vec![33])),
            ("View.Right".into(), first_right, None),
            ("View.Right".into(), first_right, Some(vec![11])),
            ("View.Right".into(), first_right, Some(vec![44])),
            ("View.Left".into(), refreshed_left, None),
            ("View.Left".into(), refreshed_left, Some(vec![11])),
            ("View.Left".into(), refreshed_left, Some(vec![55])),
            ("View.Right".into(), refreshed_right, None),
            ("View.Right".into(), refreshed_right, Some(vec![11])),
            ("View.Right".into(), refreshed_right, Some(vec![66])),
        ],
        "each view advances within its own cursor scope and refresh begins both at the head"
    );
}

fn paired_subscription_cascade_body() -> Expr {
    let left = relation_stage(
        relation_stage(
            relation_source("View.Paired"),
            "filter",
            vec![named_function("is_odd")],
        ),
        "filter",
        vec![named_function("below_five")],
    );
    let right = relation_stage(
        relation_stage(
            relation_source("View.Paired"),
            "filter",
            vec![named_function("is_even")],
        ),
        "filter",
        vec![named_function("below_eight")],
    );
    let paired = relation_stage(left, "union", vec![right]);
    let cascaded = relation_stage(
        relation_stage(paired, "filter", vec![named_function("is_positive")]),
        "filter",
        vec![named_function("below_eight")],
    );
    let mapped = relation_stage(cascaded, "map", vec![named_function("add_one")]);
    terminal(mapped, "sum")
}

#[test]
fn paired_subscriptions_rebind_independent_scopes_through_refresh_cascades() {
    // The reference does not define same-name paired subscriptions here.
    // Treat each planned read as an independent binding: keep its identity
    // through every page in this refresh, then allocate fresh identities on
    // the next refresh rather than replaying either old cursor.
    let mut first_refresh = PairedSubscriptionSource::new([
        vec![page(&[1, 2], Some(vec![11])), page(&[3, 4], None)],
        vec![page(&[2, 9], Some(vec![22])), page(&[4, 6], None)],
    ]);
    assert_eq!(
        run_with_fixture_functions(paired_subscription_cascade_body(), &mut first_refresh).unwrap(),
        integer(21),
        "paired reads of one source retain their distinct odd/even subscription values"
    );
    assert_eq!(first_refresh.lanes.len(), 2);
    let first_scopes = [first_refresh.lanes[0].0, first_refresh.lanes[1].0];
    assert_ne!(first_scopes[0], first_scopes[1]);
    assert_eq!(
        first_refresh.cursors,
        vec![
            (first_scopes[0], None),
            (first_scopes[0], Some(vec![11])),
            (first_scopes[1], None),
            (first_scopes[1], Some(vec![22])),
        ],
        "nested view filters retain each subscription's independent continuation"
    );

    let mut second_refresh = PairedSubscriptionSource::new([
        vec![page(&[5, 7], Some(vec![31])), page(&[9, 11], None)],
        vec![page(&[2, 8], Some(vec![42])), page(&[10, 12], None)],
    ]);
    assert_eq!(
        run_with_fixture_functions(paired_subscription_cascade_body(), &mut second_refresh).unwrap(),
        integer(3),
        "the rebound pair computes from only its new source pages"
    );
    assert_eq!(second_refresh.lanes.len(), 2);
    let second_scopes = [second_refresh.lanes[0].0, second_refresh.lanes[1].0];
    assert_ne!(second_scopes[0], second_scopes[1]);
    for new_scope in second_scopes {
        assert!(!first_scopes.contains(&new_scope));
    }
    assert_eq!(
        second_refresh.cursors,
        vec![
            (second_scopes[0], None),
            (second_scopes[0], Some(vec![31])),
            (second_scopes[1], None),
            (second_scopes[1], Some(vec![42])),
        ],
        "a refresh starts both rebound subscriptions without stale continuation state"
    );
}

#[test]
fn paired_read_folds_keep_equal_cursor_bytes_scope_local() {
    // Cursor tokens are opaque to the evaluator and may be equal across two
    // subscriptions. Their read scopes still keep continuation batches apart.
    let mut source = PairedSubscriptionSource::new([
        vec![page(&[-3, 1], Some(vec![77])), page(&[3, 5], None)],
        vec![page(&[2, 7], Some(vec![77])), page(&[4, 8], None)],
    ]);

    assert_eq!(
        run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap(),
        integer(14),
        "the aggregate folds only the odd left and even right values after both page continuations"
    );
    assert_eq!(source.lanes.len(), 2);
    let scopes = [source.lanes[0].0, source.lanes[1].0];
    assert_ne!(scopes[0], scopes[1]);
    assert_eq!(
        source.cursors,
        vec![
            (scopes[0], None),
            (scopes[0], Some(vec![77])),
            (scopes[1], None),
            (scopes[1], Some(vec![77])),
        ],
        "equal cursor bytes resume only within their own read scope"
    );
}

#[test]
fn paired_subscription_handoffs_keep_identity_when_cursors_repeat_across_refreshes() {
    // This provider stays alive across both refresh folds. Every lane and
    // generation deliberately reuses one opaque checkpoint to prove that
    // scope identity, not cursor bytes, chooses the continuation stream.
    let mut source = PairedSubscriptionSource::new([
        vec![page(&[-3, 1], Some(vec![77])), page(&[3, 5], None)],
        vec![page(&[2, 7], Some(vec![77])), page(&[4, 8], None)],
        vec![page(&[5, 7], Some(vec![77])), page(&[9, 11], None)],
        vec![page(&[2, 8], Some(vec![77])), page(&[10, 12], None)],
    ]);

    assert_eq!(
        run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap(),
        integer(14),
        "first paired fold combines the positive odd and even rows from its own pages"
    );
    assert_eq!(source.lanes.len(), 2);
    let first_scopes = [source.lanes[0].0, source.lanes[1].0];
    assert_ne!(first_scopes[0], first_scopes[1]);

    assert_eq!(
        run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap(),
        integer(3),
        "refresh fold reads only the positive even row from the new paired pages"
    );
    assert_eq!(source.lanes.len(), 4);
    let refreshed_scopes = [source.lanes[2].0, source.lanes[3].0];
    assert_ne!(refreshed_scopes[0], refreshed_scopes[1]);
    for scope in refreshed_scopes {
        assert!(!first_scopes.contains(&scope));
    }
    assert_eq!(
        source.cursors,
        vec![
            (first_scopes[0], None),
            (first_scopes[0], Some(vec![77])),
            (first_scopes[1], None),
            (first_scopes[1], Some(vec![77])),
            (refreshed_scopes[0], None),
            (refreshed_scopes[0], Some(vec![77])),
            (refreshed_scopes[1], None),
            (refreshed_scopes[1], Some(vec![77])),
        ],
        "each paired refresh begins at the head and resumes only its own repeated cursor"
    );
}

#[test]
fn paired_subscription_batch_scopes_survive_three_page_handoffs_and_refresh() {
    // The `.orna` callback fixture drives real filtering and folding. Three
    // page batches per subscription make scope continuity observable beyond a
    // single continuation, while each refresh reuses opaque cursor bytes.
    let mut source = PairedSubscriptionSource::new([
        vec![
            page(&[1], Some(vec![11])),
            page(&[3], Some(vec![33])),
            page(&[5], None),
        ],
        vec![
            page(&[2], Some(vec![11])),
            page(&[4], Some(vec![44])),
            page(&[6], None),
        ],
        vec![
            page(&[-1], Some(vec![11])),
            page(&[7], Some(vec![55])),
            page(&[9], None),
        ],
        vec![
            page(&[8], Some(vec![11])),
            page(&[2], Some(vec![44])),
            page(&[10], None),
        ],
    ]);

    assert_eq!(
        run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap(),
        integer(21),
        "first fold combines the filtered odd and even values across three batches"
    );
    assert_eq!(source.lanes.len(), 2);
    let first_scopes = [source.lanes[0].0, source.lanes[1].0];
    assert_ne!(first_scopes[0], first_scopes[1]);

    assert_eq!(
        run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap(),
        integer(3),
        "refreshed fold uses only the newly rebound even value across its batches"
    );
    assert_eq!(source.lanes.len(), 4);
    let refreshed_scopes = [source.lanes[2].0, source.lanes[3].0];
    assert_ne!(refreshed_scopes[0], refreshed_scopes[1]);
    for scope in refreshed_scopes {
        assert!(!first_scopes.contains(&scope));
    }
    assert_eq!(
        source.cursors,
        vec![
            (first_scopes[0], None),
            (first_scopes[0], Some(vec![11])),
            (first_scopes[0], Some(vec![33])),
            (first_scopes[1], None),
            (first_scopes[1], Some(vec![11])),
            (first_scopes[1], Some(vec![44])),
            (refreshed_scopes[0], None),
            (refreshed_scopes[0], Some(vec![11])),
            (refreshed_scopes[0], Some(vec![55])),
            (refreshed_scopes[1], None),
            (refreshed_scopes[1], Some(vec![11])),
            (refreshed_scopes[1], Some(vec![44])),
        ],
        "each batch resumes its subscription scope and each refresh starts a new pair"
    );
}
