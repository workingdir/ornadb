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

fn nested_window_aggregate_functions() -> Functions {
    let parsed = parse_module(include_str!("fixtures/table_relation_nested_window_aggregate_bg57u.orna"));
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

fn nested_aggregate_refresh_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_nested_aggregate_refresh_restore_i6k3c.orna"
    ));
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

fn nested_aggregate_compaction_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_nested_aggregate_compaction_3xa7q.orna"
    ));
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

fn sparse_window_fold_functions() -> Functions {
    let parsed = parse_module(include_str!("fixtures/table_relation_sparse_window_fold_d4441.orna"));
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

fn paired_window_refresh_compaction_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_paired_window_refresh_compaction_lzevs.orna"
    ));
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

fn paired_window_scope_compaction_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_paired_window_scope_compaction_4piqu.orna"
    ));
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

fn paired_sparse_window_checkpoint_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_paired_sparse_window_checkpoint_vjh8e.orna"
    ));
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

fn paired_refresh_sparse_rotation_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_paired_refresh_sparse_rotation_9wumm.orna"
    ));
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

fn paired_snapshot_sparse_escalation_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_paired_snapshot_sparse_escalation_5jpxd.orna"
    ));
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

fn nested_pagination_compaction_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_nested_pagination_compaction_gwlx9.orna"
    ));
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

fn paired_checkpoint_rotation_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_paired_window_checkpoint_rotation_n37re.orna"
    ));
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

fn paired_limit_refresh_compaction_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_paired_limit_refresh_compaction_la0gy.orna"
    ));
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

fn scoped_refresh_aggregate_functions() -> Functions {
    let parsed = parse_module(include_str!(
        "fixtures/table_relation_scoped_refresh_aggregate_restore_4plgl.orna"
    ));
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

fn integer_pair(left: i64, right: i64) -> CanonicalValue {
    CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![Raw::Int(left.into()), Raw::Int(right.into())])),
    ))
    .unwrap()
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

type CursorRestore = BTreeMap<Option<Vec<u8>>, RelationPage>;

struct PairedCursorRestoreSource {
    pending: BTreeMap<String, VecDeque<CursorRestore>>,
    lanes: Vec<(String, RelationReadScope, CursorRestore)>,
    cursors: Vec<(String, RelationReadScope, Option<Vec<u8>>)>,
}

impl PairedCursorRestoreSource {
    fn new(restores: impl IntoIterator<Item = (&'static str, CursorRestore)>) -> Self {
        let mut pending = BTreeMap::<String, VecDeque<CursorRestore>>::new();
        for (source, restore) in restores {
            pending.entry(source.to_owned()).or_default().push_back(restore);
        }
        Self {
            pending,
            lanes: Vec::new(),
            cursors: Vec::new(),
        }
    }
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

impl EffectHandler for PairedCursorRestoreSource {
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
                let Some(restore) = pending.pop_front() else {
                    return Ok(None);
                };
                self.lanes.push((source.to_owned(), scope, restore));
                self.lanes.len() - 1
            }
        };
        let cursor = after.map(ToOwned::to_owned);
        self.cursors.push((source.to_owned(), scope, cursor.clone()));
        Ok(self.lanes[lane].2.get(&cursor).cloned())
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

fn paired_scoped_view_refresh_body() -> Expr {
    let subscription = |source: &str, parity: &str, bound: &str| {
        relation_stage(
            relation_stage(
                relation_source(source),
                "filter",
                vec![named_function(parity)],
            ),
            "filter",
            vec![named_function(bound)],
        )
    };
    let left = relation_stage(
        subscription("View.Left", "is_odd", "below_five"),
        "union",
        vec![subscription("View.Left", "is_even", "below_eight")],
    );
    let right = relation_stage(
        subscription("View.Right", "is_odd", "below_five"),
        "union",
        vec![subscription("View.Right", "is_even", "below_eight")],
    );
    let paired = relation_stage(left, "union", vec![right]);
    let positive = relation_stage(
        paired,
        "filter",
        vec![named_function("is_positive")],
    );
    let mapped = relation_stage(positive, "map", vec![named_function("add_one")]);
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

#[test]
fn paired_view_handoffs_rebind_old_cursor_tokens_to_fresh_scopes() {
    // Cursor bytes are opaque; the reference is silent on token reuse after
    // multiple paired view refreshes. Each `(source, scope)` owns its stream,
    // so returning to an older token starts from that refresh's first page.
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
        (
            "View.Left",
            vec![
                page(&[-1], Some(vec![11])),
                page(&[3], Some(vec![33])),
                page(&[7], None),
            ],
        ),
        (
            "View.Right",
            vec![
                page(&[2], Some(vec![11])),
                page(&[4], Some(vec![44])),
                page(&[10], None),
            ],
        ),
    ]);

    for (fold, expected) in [21, 3, 12].into_iter().enumerate() {
        assert_eq!(
            run_with_fixture_functions(incremental_scoped_view_body(), &mut source).unwrap(),
            integer(expected),
            "paired view refresh fold {fold} computes only that generation's filtered rows"
        );
    }

    assert_eq!(source.lanes.len(), 6);
    for generation in 0..3 {
        let left = &source.lanes[generation * 2];
        let right = &source.lanes[generation * 2 + 1];
        assert_eq!(left.0, "View.Left");
        assert_eq!(right.0, "View.Right");
        assert_ne!(left.1, right.1, "generation {generation} keeps sibling view scopes distinct");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "each refresh binds its view to a new scope; repeated cursor bytes do not reuse scope {scope:?}"
        );
    }
    assert_eq!(
        source.cursors,
        vec![
            ("View.Left".into(), scopes[0], None),
            ("View.Left".into(), scopes[0], Some(vec![11])),
            ("View.Left".into(), scopes[0], Some(vec![33])),
            ("View.Right".into(), scopes[1], None),
            ("View.Right".into(), scopes[1], Some(vec![11])),
            ("View.Right".into(), scopes[1], Some(vec![44])),
            ("View.Left".into(), scopes[2], None),
            ("View.Left".into(), scopes[2], Some(vec![11])),
            ("View.Left".into(), scopes[2], Some(vec![55])),
            ("View.Right".into(), scopes[3], None),
            ("View.Right".into(), scopes[3], Some(vec![11])),
            ("View.Right".into(), scopes[3], Some(vec![66])),
            ("View.Left".into(), scopes[4], None),
            ("View.Left".into(), scopes[4], Some(vec![11])),
            ("View.Left".into(), scopes[4], Some(vec![33])),
            ("View.Right".into(), scopes[5], None),
            ("View.Right".into(), scopes[5], Some(vec![11])),
            ("View.Right".into(), scopes[5], Some(vec![44])),
        ],
        "old cursors may recur after refresh but must only advance within the new scope"
    );
}

#[test]
fn paired_view_subscriptions_keep_cursor_identity_across_refresh_handoffs() {
    // Each scoped view has two sibling subscriptions. Reusing the same opaque
    // cursor after refresh must bind every subscription to its new page stream.
    let mut source = PairedViewRefreshSource::new([
        ("View.Left", vec![page(&[1], Some(vec![11])), page(&[3], None)]),
        ("View.Left", vec![page(&[2], Some(vec![11])), page(&[4], None)]),
        ("View.Right", vec![page(&[-1], Some(vec![11])), page(&[5], None)]),
        ("View.Right", vec![page(&[6], Some(vec![11])), page(&[8], None)]),
        ("View.Left", vec![page(&[7], Some(vec![11])), page(&[9], None)]),
        ("View.Left", vec![page(&[8], Some(vec![11])), page(&[10], None)]),
        ("View.Right", vec![page(&[5], Some(vec![11])), page(&[7], None)]),
        ("View.Right", vec![page(&[2], Some(vec![11])), page(&[8], None)]),
        ("View.Left", vec![page(&[-1], Some(vec![11])), page(&[3], None)]),
        ("View.Left", vec![page(&[2], Some(vec![11])), page(&[4], None)]),
        ("View.Right", vec![page(&[1], Some(vec![11])), page(&[5], None)]),
        ("View.Right", vec![page(&[2], Some(vec![11])), page(&[4], None)]),
    ]);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(21));
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(3));
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(22));
    assert_eq!(first, integer(21), "the initial paired-view fold remains captured across refreshes");
    assert_eq!(second, integer(3), "the second paired-view fold remains captured after the third");

    assert_eq!(source.lanes.len(), 12);
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
        assert_ne!(source.lanes[start].1, source.lanes[start + 1].1);
        assert_ne!(source.lanes[start + 2].1, source.lanes[start + 3].1);
    }
}

#[test]
fn paired_view_refresh_cursor_chains_keep_identity_across_handoffs() {
    // Each refresh folds two paired views through three-page cursor chains.
    // The first and last refresh deliberately reuse cursor bytes.
    let subscriptions = [
        ("View.Left", vec![page(&[1], Some(vec![11])), page(&[3], Some(vec![33])), page(&[5], None)]),
        ("View.Left", vec![page(&[2], Some(vec![11])), page(&[4], Some(vec![44])), page(&[6], None)]),
        ("View.Right", vec![page(&[-1], Some(vec![11])), page(&[1], Some(vec![55])), page(&[5], None)]),
        ("View.Right", vec![page(&[2], Some(vec![11])), page(&[4], Some(vec![66])), page(&[8], None)]),
        ("View.Left", vec![page(&[7], Some(vec![71])), page(&[9], Some(vec![73])), page(&[11], None)]),
        ("View.Left", vec![page(&[8], Some(vec![81])), page(&[10], Some(vec![83])), page(&[12], None)]),
        ("View.Right", vec![page(&[-3], Some(vec![91])), page(&[3], Some(vec![93])), page(&[7], None)]),
        ("View.Right", vec![page(&[6], Some(vec![101])), page(&[8], Some(vec![103])), page(&[10], None)]),
        ("View.Left", vec![page(&[-1], Some(vec![11])), page(&[3], Some(vec![33])), page(&[5], None)]),
        ("View.Left", vec![page(&[2], Some(vec![11])), page(&[4], Some(vec![44])), page(&[8], None)]),
        ("View.Right", vec![page(&[1], Some(vec![11])), page(&[3], Some(vec![55])), page(&[5], None)]),
        ("View.Right", vec![page(&[2], Some(vec![11])), page(&[4], Some(vec![66])), page(&[8], None)]),
    ];
    let mut source = PairedViewRefreshSource::new(subscriptions);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(31));
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(11));
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(26));
    assert_eq!(first, integer(31), "the first refresh result remains captured");
    assert_eq!(second, integer(11), "the intermediate refresh result remains captured");

    assert_eq!(source.lanes.len(), 12, "four scoped subscriptions bind on each refresh");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "refresh and sibling subscriptions retain distinct scopes despite repeated cursor bytes: {scope:?}"
        );
    }

    assert_eq!(source.cursors.len(), 36, "each subscription consumes its three-page chain");
    let chains: [(&str, &[u8], &[u8]); 12] = [
        ("View.Left", &[11], &[33]),
        ("View.Left", &[11], &[44]),
        ("View.Right", &[11], &[55]),
        ("View.Right", &[11], &[66]),
        ("View.Left", &[71], &[73]),
        ("View.Left", &[81], &[83]),
        ("View.Right", &[91], &[93]),
        ("View.Right", &[101], &[103]),
        ("View.Left", &[11], &[33]),
        ("View.Left", &[11], &[44]),
        ("View.Right", &[11], &[55]),
        ("View.Right", &[11], &[66]),
    ];
    let mut expected_cursors = Vec::new();
    for (index, (source_name, first_cursor, second_cursor)) in chains.into_iter().enumerate() {
        let scope = scopes[index];
        expected_cursors.extend([
            (source_name.to_owned(), scope, None),
            (source_name.to_owned(), scope, Some(first_cursor.to_vec())),
            (source_name.to_owned(), scope, Some(second_cursor.to_vec())),
        ]);
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "replayed cursor bytes advance only their own subscription scope across refresh chains"
    );
}

#[test]
fn paired_view_refresh_cursor_omissions_keep_handoff_identity() {
    // Each refresh omits the continuation cursor from selected sibling reads;
    // later refreshes reuse cursor bytes on different sibling lanes.
    let subscriptions = [
        (
            "View.Left",
            vec![page(&[1], Some(vec![51])), page(&[3], None)],
        ),
        ("View.Left", vec![page(&[2], None)]),
        ("View.Right", vec![page(&[-1], None)]),
        (
            "View.Right",
            vec![page(&[4], Some(vec![61])), page(&[6], None)],
        ),
        ("View.Left", vec![page(&[3], None)]),
        (
            "View.Left",
            vec![page(&[6], Some(vec![51])), page(&[8], None)],
        ),
        (
            "View.Right",
            vec![page(&[1], Some(vec![61])), page(&[3], None)],
        ),
        ("View.Right", vec![page(&[10], None)]),
        (
            "View.Left",
            vec![page(&[1], Some(vec![51])), page(&[5], None)],
        ),
        ("View.Left", vec![page(&[2], None)]),
        ("View.Right", vec![page(&[3], None)]),
        (
            "View.Right",
            vec![page(&[4], Some(vec![61])), page(&[8], None)],
        ),
    ];
    let mut source = PairedViewRefreshSource::new(subscriptions);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(21));
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(17));
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(14));
    assert_eq!(first, integer(21), "the first omission pattern remains captured");
    assert_eq!(second, integer(17), "the intermediate omission pattern remains captured");

    assert_eq!(source.lanes.len(), 12, "four subscriptions receive fresh scopes per refresh");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "omitted continuations and cursor reuse do not merge handoff scopes: {scope:?}"
        );
    }
    assert_eq!(source.cursors.len(), 18, "omitted continuations do not issue another page read");
    assert_eq!(
        source.cursors,
        vec![
            ("View.Left".into(), scopes[0], None),
            ("View.Left".into(), scopes[0], Some(vec![51])),
            ("View.Left".into(), scopes[1], None),
            ("View.Right".into(), scopes[2], None),
            ("View.Right".into(), scopes[3], None),
            ("View.Right".into(), scopes[3], Some(vec![61])),
            ("View.Left".into(), scopes[4], None),
            ("View.Left".into(), scopes[5], None),
            ("View.Left".into(), scopes[5], Some(vec![51])),
            ("View.Right".into(), scopes[6], None),
            ("View.Right".into(), scopes[6], Some(vec![61])),
            ("View.Right".into(), scopes[7], None),
            ("View.Left".into(), scopes[8], None),
            ("View.Left".into(), scopes[8], Some(vec![51])),
            ("View.Left".into(), scopes[9], None),
            ("View.Right".into(), scopes[10], None),
            ("View.Right".into(), scopes[11], None),
            ("View.Right".into(), scopes[11], Some(vec![61])),
        ],
        "cursor omission ends only its lane, and reused tokens stay with their new handoff scope"
    );
}

#[test]
fn paired_view_refresh_filtered_page_omissions_keep_scoped_pagination() {
    // Rows filtered out by one page still advance its valid cursor chain;
    // terminal filtered pages contribute nothing. Refreshes reuse tokens.
    let subscriptions = [
        (
            "View.Left",
            vec![page(&[2], Some(vec![71])), page(&[1, 3], None)],
        ),
        ("View.Left", vec![page(&[2], None)]),
        (
            "View.Right",
            vec![page(&[-1], Some(vec![71])), page(&[5], None)],
        ),
        (
            "View.Right",
            vec![page(&[9], Some(vec![72])), page(&[4, 6], None)],
        ),
        (
            "View.Left",
            vec![page(&[2], Some(vec![71])), page(&[1], None)],
        ),
        (
            "View.Left",
            vec![page(&[-1], Some(vec![72])), page(&[2], None)],
        ),
        (
            "View.Right",
            vec![page(&[-1], Some(vec![71])), page(&[3], None)],
        ),
        ("View.Right", vec![page(&[10], None)]),
        (
            "View.Left",
            vec![page(&[2], Some(vec![71])), page(&[3], None)],
        ),
        (
            "View.Left",
            vec![page(&[1], Some(vec![72])), page(&[2, 4], None)],
        ),
        (
            "View.Right",
            vec![page(&[5], Some(vec![71])), page(&[1], None)],
        ),
        (
            "View.Right",
            vec![page(&[9], Some(vec![72])), page(&[6], None)],
        ),
    ];
    let mut source = PairedViewRefreshSource::new(subscriptions);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(21));
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(9));
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(21));
    assert_eq!(first, integer(21), "the first refresh retains rows after filtered pages");
    assert_eq!(second, integer(9), "the middle refresh keeps its own omission result");

    assert_eq!(source.lanes.len(), 12, "each paired refresh binds four scoped reads");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "filtered page continuations remain isolated by handoff scope: {scope:?}"
        );
    }
    assert_eq!(source.cursors.len(), 22, "each nonterminal page follows its own continuation");
    assert_eq!(
        source.cursors,
        vec![
            ("View.Left".into(), scopes[0], None),
            ("View.Left".into(), scopes[0], Some(vec![71])),
            ("View.Left".into(), scopes[1], None),
            ("View.Right".into(), scopes[2], None),
            ("View.Right".into(), scopes[2], Some(vec![71])),
            ("View.Right".into(), scopes[3], None),
            ("View.Right".into(), scopes[3], Some(vec![72])),
            ("View.Left".into(), scopes[4], None),
            ("View.Left".into(), scopes[4], Some(vec![71])),
            ("View.Left".into(), scopes[5], None),
            ("View.Left".into(), scopes[5], Some(vec![72])),
            ("View.Right".into(), scopes[6], None),
            ("View.Right".into(), scopes[6], Some(vec![71])),
            ("View.Right".into(), scopes[7], None),
            ("View.Left".into(), scopes[8], None),
            ("View.Left".into(), scopes[8], Some(vec![71])),
            ("View.Left".into(), scopes[9], None),
            ("View.Left".into(), scopes[9], Some(vec![72])),
            ("View.Right".into(), scopes[10], None),
            ("View.Right".into(), scopes[10], Some(vec![71])),
            ("View.Right".into(), scopes[11], None),
            ("View.Right".into(), scopes[11], Some(vec![72])),
        ],
        "filtered omissions do not detach continuation tokens from their refresh scope"
    );
}

#[test]
fn paired_view_variable_depth_pagination_keeps_scope_through_refresh_folds() {
    // Sibling reads have one, two, three, or four pages. Cursor bytes recur
    // across lanes and refreshes, while each continuation remains scope-local.
    let subscriptions = [
        (
            "View.Left",
            vec![
                page(&[1], Some(vec![11])),
                page(&[3], Some(vec![21])),
                page(&[5], None),
            ],
        ),
        (
            "View.Left",
            vec![page(&[2], Some(vec![11])), page(&[4], None)],
        ),
        ("View.Right", vec![page(&[-1], None)]),
        (
            "View.Right",
            vec![page(&[6], Some(vec![11])), page(&[8], None)],
        ),
        ("View.Left", vec![page(&[3], None)]),
        (
            "View.Left",
            vec![
                page(&[2], Some(vec![11])),
                page(&[4], Some(vec![21])),
                page(&[6], None),
            ],
        ),
        (
            "View.Right",
            vec![page(&[1], Some(vec![11])), page(&[3], None)],
        ),
        (
            "View.Right",
            vec![
                page(&[4], Some(vec![11])),
                page(&[6], Some(vec![21])),
                page(&[8], None),
            ],
        ),
        (
            "View.Left",
            vec![page(&[-1], Some(vec![11])), page(&[1], None)],
        ),
        ("View.Left", vec![page(&[8], None)]),
        (
            "View.Right",
            vec![page(&[3], Some(vec![11])), page(&[5], None)],
        ),
        (
            "View.Right",
            vec![
                page(&[2], Some(vec![11])),
                page(&[4], Some(vec![21])),
                page(&[6], Some(vec![31])),
                page(&[8], None),
            ],
        ),
    ];
    let mut source = PairedViewRefreshSource::new(subscriptions);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(21));
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(37));
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(21));
    assert_eq!(first, integer(21), "the shallow first refresh result remains captured");
    assert_eq!(second, integer(37), "the deeper middle refresh result remains captured");

    assert_eq!(source.lanes.len(), 12, "four scoped subscriptions bind per refresh");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "pagination depth and repeated cursor bytes do not merge scopes: {scope:?}"
        );
    }
    assert_eq!(source.cursors.len(), 26, "all variable-depth page chains are consumed");
    assert_eq!(
        source.cursors,
        vec![
            ("View.Left".into(), scopes[0], None),
            ("View.Left".into(), scopes[0], Some(vec![11])),
            ("View.Left".into(), scopes[0], Some(vec![21])),
            ("View.Left".into(), scopes[1], None),
            ("View.Left".into(), scopes[1], Some(vec![11])),
            ("View.Right".into(), scopes[2], None),
            ("View.Right".into(), scopes[3], None),
            ("View.Right".into(), scopes[3], Some(vec![11])),
            ("View.Left".into(), scopes[4], None),
            ("View.Left".into(), scopes[5], None),
            ("View.Left".into(), scopes[5], Some(vec![11])),
            ("View.Left".into(), scopes[5], Some(vec![21])),
            ("View.Right".into(), scopes[6], None),
            ("View.Right".into(), scopes[6], Some(vec![11])),
            ("View.Right".into(), scopes[7], None),
            ("View.Right".into(), scopes[7], Some(vec![11])),
            ("View.Right".into(), scopes[7], Some(vec![21])),
            ("View.Left".into(), scopes[8], None),
            ("View.Left".into(), scopes[8], Some(vec![11])),
            ("View.Left".into(), scopes[9], None),
            ("View.Right".into(), scopes[10], None),
            ("View.Right".into(), scopes[10], Some(vec![11])),
            ("View.Right".into(), scopes[11], None),
            ("View.Right".into(), scopes[11], Some(vec![11])),
            ("View.Right".into(), scopes[11], Some(vec![21])),
            ("View.Right".into(), scopes[11], Some(vec![31])),
        ],
        "variable-depth cursor chains remain attached to their paired refresh scopes"
    );
}

#[test]
fn paired_view_long_cursor_spill_chains_keep_refresh_identity() {
    let cursor = |tag: u8| {
        let mut token = vec![0x5a; 128];
        token.push(tag);
        token
    };
    let subscriptions = [
        (
            "View.Left",
            vec![
                page(&[1], Some(cursor(1))),
                page(&[3], Some(cursor(2))),
                page(&[5], Some(cursor(3))),
                page(&[7], None),
            ],
        ),
        (
            "View.Left",
            vec![page(&[2], Some(cursor(1))), page(&[4], Some(cursor(2))), page(&[6], None)],
        ),
        ("View.Right", vec![page(&[-1], Some(cursor(1))), page(&[1], None)]),
        ("View.Right", vec![page(&[2], None)]),
        ("View.Left", vec![page(&[3], Some(cursor(1))), page(&[5], None)]),
        (
            "View.Left",
            vec![
                page(&[2], Some(cursor(1))),
                page(&[4], Some(cursor(2))),
                page(&[6], Some(cursor(3))),
                page(&[8], None),
            ],
        ),
        (
            "View.Right",
            vec![page(&[1], Some(cursor(1))), page(&[3], Some(cursor(2))), page(&[5], None)],
        ),
        ("View.Right", vec![page(&[4], Some(cursor(1))), page(&[6], None)]),
        ("View.Left", vec![page(&[1], None)]),
        ("View.Left", vec![page(&[2], Some(cursor(1))), page(&[4], None)]),
        (
            "View.Right",
            vec![
                page(&[-1], Some(cursor(1))),
                page(&[1], Some(cursor(2))),
                page(&[3], Some(cursor(3))),
                page(&[5], None),
            ],
        ),
        (
            "View.Right",
            vec![page(&[2], Some(cursor(1))), page(&[4], Some(cursor(2))), page(&[6], None)],
        ),
    ];
    let mut source = PairedViewRefreshSource::new(subscriptions);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(26));
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(37));
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(31));
    assert_eq!(first, integer(26), "the first spilled cursor fold remains captured");
    assert_eq!(second, integer(37), "the middle spilled cursor fold remains captured");

    assert_eq!(source.lanes.len(), 12, "four scoped subscriptions bind per refresh");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "long cursor payloads remain isolated to their paired refresh scope: {scope:?}"
        );
    }
    assert_eq!(source.cursors.len(), 31, "all one-to-four-page chains are consumed");
    let depths = [4, 3, 2, 1, 2, 4, 3, 2, 1, 2, 4, 3];
    let mut expected_cursors = Vec::new();
    for (lane, depth) in depths.into_iter().enumerate() {
        let (source_name, scope, _) = &source.lanes[lane];
        expected_cursors.push((source_name.clone(), *scope, None));
        for tag in 1..depth {
            expected_cursors.push((source_name.clone(), *scope, Some(cursor(tag))));
        }
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "128-byte shared cursor prefixes retain their exact payload and scope through each spill chain"
    );
}

#[test]
fn paired_view_prefix_spill_cursors_keep_scope_across_handoffs() {
    let cursor_prefix = vec![0x61; 96];
    let mut cursor_one = cursor_prefix.clone();
    cursor_one.push(0);
    let mut cursor_two = cursor_one.clone();
    cursor_two.push(1);
    assert!(cursor_prefix.as_slice() < cursor_one.as_slice());
    assert!(cursor_one.as_slice() < cursor_two.as_slice());
    let subscriptions = [
        (
            "View.Left",
            vec![page(&[3], Some(cursor_prefix.clone())), page(&[1], None)],
        ),
        ("View.Left", vec![page(&[4], None)]),
        (
            "View.Right",
            vec![
                page(&[-1], Some(cursor_prefix.clone())),
                page(&[1], Some(cursor_one.clone())),
                page(&[3], None),
            ],
        ),
        (
            "View.Right",
            vec![page(&[6], Some(cursor_prefix.clone())), page(&[7], None)],
        ),
        ("View.Left", vec![page(&[1], None)]),
        (
            "View.Left",
            vec![
                page(&[2], Some(cursor_prefix.clone())),
                page(&[4], Some(cursor_one.clone())),
                page(&[6], Some(cursor_two.clone())),
                page(&[8], None),
            ],
        ),
        (
            "View.Right",
            vec![page(&[-3], Some(cursor_prefix.clone())), page(&[5], None)],
        ),
        (
            "View.Right",
            vec![page(&[2], Some(cursor_prefix.clone())), page(&[6], None)],
        ),
        (
            "View.Left",
            vec![page(&[-1], Some(cursor_prefix.clone())), page(&[3], None)],
        ),
        (
            "View.Left",
            vec![page(&[2], Some(cursor_prefix.clone())), page(&[4], None)],
        ),
        (
            "View.Right",
            vec![
                page(&[1], Some(cursor_prefix.clone())),
                page(&[3], Some(cursor_one.clone())),
                page(&[5], Some(cursor_two.clone())),
                page(&[7], None),
            ],
        ),
        (
            "View.Right",
            vec![
                page(&[2], Some(cursor_prefix.clone())),
                page(&[4], Some(cursor_one.clone())),
                page(&[6], None),
            ],
        ),
    ];
    let mut source = PairedViewRefreshSource::new(subscriptions);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(24));
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(27));
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(33));
    assert_eq!(first, integer(24), "the first prefix-spill fold remains captured");
    assert_eq!(second, integer(27), "the middle prefix-spill fold remains captured");

    assert_eq!(source.lanes.len(), 12, "four pagination scopes bind per refresh");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "nested-prefix cursor payloads remain bound to distinct scopes: {scope:?}"
        );
    }
    assert_eq!(source.cursors.len(), 28, "all prefix-spill page chains are consumed");
    let continuations = [cursor_prefix, cursor_one, cursor_two];
    let depths = [2, 1, 3, 2, 1, 4, 2, 2, 2, 2, 4, 3];
    let mut expected_cursors = Vec::new();
    for (lane, depth) in depths.into_iter().enumerate() {
        let (source_name, scope, _) = &source.lanes[lane];
        expected_cursors.push((source_name.clone(), *scope, None));
        for token in continuations.iter().take(depth - 1) {
            expected_cursors.push((source_name.clone(), *scope, Some(token.clone())));
        }
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "prefix-related cursor byte strings stay exact and scope-local through paired handoffs"
    );
}

#[test]
fn paired_view_refresh_spill_folds_keep_cursor_identity() {
    let cursor_255 = vec![0x6d; 255];
    let mut cursor_256 = cursor_255.clone();
    cursor_256.push(0x00);
    let mut cursor_511 = cursor_256.clone();
    cursor_511.extend(vec![0x7e; 255]);
    let mut cursor_512 = cursor_511.clone();
    cursor_512.push(0xff);
    assert_eq!(
        [cursor_255.len(), cursor_256.len(), cursor_511.len(), cursor_512.len()],
        [255, 256, 511, 512]
    );
    assert!(cursor_255.as_slice() < cursor_256.as_slice());
    assert!(cursor_256.as_slice() < cursor_511.as_slice());
    assert!(cursor_511.as_slice() < cursor_512.as_slice());
    let continuations = [cursor_255, cursor_256, cursor_511, cursor_512];

    let generations = [
        ([1, 2, 3, 4], [5, 8, 5, 8], [5, 4, 3, 2]),
        ([3, 4, 1, 6], [5, 8, 5, 8], [2, 5, 4, 3]),
        ([1, 6, 3, 2], [5, 8, 5, 8], [3, 2, 5, 4]),
    ];
    let mut subscriptions = Vec::new();
    for (values, filtered_values, lane_depths) in generations {
        for lane in 0..4 {
            let source = if lane < 2 { "View.Left" } else { "View.Right" };
            let depth = lane_depths[lane];
            let pages = (0..depth)
                .map(|page_index| {
                    let next = (page_index + 1 < depth)
                        .then(|| continuations[page_index].clone());
                    let value = if page_index == 0 {
                        values[lane]
                    } else {
                        filtered_values[lane]
                    };
                    page(&[value], next)
                })
                .collect();
            subscriptions.push((source, pages));
        }
    }
    let mut source = PairedViewRefreshSource::new(subscriptions);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(14), "the initial scoped fold filters rows before mapping and summing");
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(18), "the next refresh observes its own four source values");
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(16), "the final refresh keeps its own scoped snapshot values");
    assert_eq!(first, integer(14), "later spill folds do not rewrite the first result");
    assert_eq!(second, integer(18), "later spill folds do not rewrite the middle result");

    assert_eq!(source.lanes.len(), 12, "three refreshes bind four independent scopes each");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "each spill fold retains a separate paired read scope: {scope:?}"
        );
    }

    let depths = generations
        .iter()
        .flat_map(|(_, _, lane_depths)| lane_depths)
        .copied()
        .collect::<Vec<_>>();
    let mut expected_cursors = Vec::new();
    for (lane, depth) in depths.into_iter().enumerate() {
        let (source_name, scope, _) = &source.lanes[lane];
        expected_cursors.push((source_name.clone(), *scope, None));
        for token in continuations.iter().take(depth - 1) {
            expected_cursors.push((source_name.clone(), *scope, Some(token.clone())));
        }
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "255/256/511/512-byte cursor tokens retain every byte and paired scope across refresh folds"
    );
}

#[test]
fn paired_cursor_restore_chains_retain_scope_and_snapshot_values() {
    let cursor_one = vec![0x72, 0x00, 0xff];
    let cursor_two = vec![0x72, 0x01];
    assert!(cursor_one.as_slice() < cursor_two.as_slice());
    let restore = |values: [i64; 3]| {
        BTreeMap::from([
            (None, page(&[values[0]], Some(cursor_one.clone()))),
            (
                Some(cursor_one.clone()),
                page(&[values[1]], Some(cursor_two.clone())),
            ),
            (Some(cursor_two.clone()), page(&[values[2]], None)),
        ])
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Paired", restore([-1, 1, 3])),
        ("View.Paired", restore([2, 4, 6])),
        ("View.Paired", restore([7, 9, 11])),
        ("View.Paired", restore([2, 8, 10])),
        ("View.Paired", restore([-1, 3, 5])),
        ("View.Paired", restore([2, 4, 8])),
    ]);

    let first = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(first, integer(21), "first restore pair folds its filtered source pages");
    let second = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(second, integer(3), "second restore pair ignores rows outside its filters");
    let third = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(third, integer(12), "third restore pair computes from its own restored pages");
    assert_eq!(first, integer(21), "later restores leave the first snapshot value captured");
    assert_eq!(second, integer(3), "later restores leave the second snapshot value captured");

    assert_eq!(source.lanes.len(), 6, "three handoffs bind two paired restore scopes each");
    let scopes = source
        .lanes
        .iter()
        .map(|(name, scope, _)| {
            assert_eq!(name, "View.Paired");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "restored sibling and handoff chains retain independent scopes: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for scope in scopes.iter().copied() {
        expected_cursors.extend([
            ("View.Paired".to_owned(), scope, None),
            ("View.Paired".to_owned(), scope, Some(cursor_one.clone())),
            ("View.Paired".to_owned(), scope, Some(cursor_two.clone())),
        ]);
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "each restored chain resolves the exact repeated cursor bytes inside its own scope"
    );
    assert!(source.pending["View.Paired"].is_empty(), "all six restored page maps are consumed");
}

fn nested_window_aggregate(source: &str) -> Expr {
    let inner_frames = relation_stage(
        relation_source(source),
        "window",
        vec![integer_literal(2), integer_literal(1)],
    );
    let inner_totals = relation_stage(
        inner_frames,
        "map",
        vec![named_function("sum_frame")],
    );
    let outer_frames = relation_stage(
        inner_totals,
        "window",
        vec![integer_literal(2), integer_literal(1)],
    );
    relation_stage(
        outer_frames,
        "map",
        vec![named_function("sum_frame")],
    )
}

fn paired_nested_window_aggregate_body() -> Expr {
    let paired = relation_stage(
        nested_window_aggregate("View.Paired"),
        "union",
        vec![nested_window_aggregate("View.Paired")],
    );
    terminal(paired, "sum")
}

fn paired_nested_aggregate_refresh_body() -> Expr {
    Expr::Tuple {
        elements: vec![
            terminal(nested_window_aggregate("View.Left"), "sum"),
            terminal(nested_window_aggregate("View.Right"), "sum"),
        ],
        span: span(),
    }
}

fn sparse_window_fold(source: &str, predicate: &str) -> Expr {
    let filtered = relation_stage(
        relation_source(source),
        "filter",
        vec![named_function(predicate)],
    );
    let inner_frames = relation_stage(
        filtered,
        "window",
        vec![integer_literal(2), integer_literal(1)],
    );
    let inner_totals = relation_stage(
        inner_frames,
        "map",
        vec![named_function("sum_frame")],
    );
    let outer_frames = relation_stage(
        inner_totals,
        "window",
        vec![integer_literal(2), integer_literal(1)],
    );
    relation_stage(
        outer_frames,
        "map",
        vec![named_function("sum_frame")],
    )
}

fn paired_sparse_window_fold_body() -> Expr {
    let paired = relation_stage(
        sparse_window_fold("View.Paired", "is_odd"),
        "union",
        vec![sparse_window_fold("View.Paired", "is_even")],
    );
    terminal(paired, "sum")
}

fn paired_window_refresh_compaction_body() -> Expr {
    Expr::Tuple {
        elements: vec![
            terminal(sparse_window_fold("View.Left", "is_odd"), "sum"),
            terminal(sparse_window_fold("View.Right", "is_even"), "sum"),
        ],
        span: span(),
    }
}

fn paired_window_scope_compaction_body() -> Expr {
    let window_sum = |source: &str| {
        let frames = relation_stage(
            relation_source(source),
            "window",
            vec![integer_literal(3), integer_literal(2)],
        );
        let frame_totals = relation_stage(frames, "map", vec![named_function("sum_frame")]);
        terminal(frame_totals, "sum")
    };
    Expr::Tuple {
        elements: vec![window_sum("View.Left"), window_sum("View.Right")],
        span: span(),
    }
}

fn paired_sparse_window_checkpoint_body() -> Expr {
    let fold = |source: &str, predicate: &str| {
        let filtered = relation_stage(
            relation_source(source),
            "filter",
            vec![named_function(predicate)],
        );
        let frames = relation_stage(
            filtered,
            "window",
            vec![integer_literal(3), integer_literal(2)],
        );
        let frame_totals = relation_stage(frames, "map", vec![named_function("sum_frame")]);
        terminal(frame_totals, "sum")
    };
    Expr::Tuple {
        elements: vec![
            fold("View.Left", "is_odd"),
            fold("View.Right", "is_even"),
        ],
        span: span(),
    }
}

fn paired_refresh_sparse_rotation_body() -> Expr {
    let fold = |source: &str, predicate: &str| {
        let filtered = relation_stage(
            relation_source(source),
            "filter",
            vec![named_function(predicate)],
        );
        let frames = relation_stage(
            filtered,
            "window",
            vec![integer_literal(3), integer_literal(2)],
        );
        let totals = relation_stage(frames, "map", vec![named_function("sum_frame")]);
        terminal(totals, "sum")
    };
    Expr::Tuple {
        elements: vec![
            fold("View.Left", "is_odd"),
            fold("View.Right", "is_even"),
        ],
        span: span(),
    }
}

fn paired_snapshot_sparse_escalation_body(depth: usize) -> Expr {
    let fold = |source: &str, predicate: &str| {
        let filtered = relation_stage(
            relation_source(source),
            "filter",
            vec![named_function(predicate)],
        );
        let folded = (0..depth).fold(filtered, |input, _| {
            let frames = relation_stage(
                input,
                "window",
                vec![integer_literal(2), integer_literal(1)],
            );
            relation_stage(frames, "map", vec![named_function("sum_frame")])
        });
        terminal(folded, "sum")
    };
    Expr::Tuple {
        elements: vec![
            fold("View.Left", "is_odd"),
            fold("View.Right", "is_even"),
        ],
        span: span(),
    }
}

fn paired_sparse_checkpoint_rotation_body() -> Expr {
    Expr::Tuple {
        elements: vec![
            terminal(sparse_window_fold("View.Left", "is_odd"), "sum"),
            terminal(sparse_window_fold("View.Right", "is_even"), "sum"),
        ],
        span: span(),
    }
}

fn paired_limit_refresh_compaction_body() -> Expr {
    let limited_sum = |source: &str| {
        let first_limit = relation_stage(
            relation_source(source),
            "take",
            vec![integer_literal(4)],
        );
        let tightened_limit = relation_stage(first_limit, "take", vec![integer_literal(3)]);
        let scaled = relation_stage(tightened_limit, "map", vec![named_function("scale_row")]);
        terminal(scaled, "sum")
    };
    Expr::Tuple {
        elements: vec![limited_sum("View.Left"), limited_sum("View.Right")],
        span: span(),
    }
}

fn paired_scoped_aggregate_restore_body() -> Expr {
    let left = terminal(
        relation_stage(
            relation_source("View.Left"),
            "filter",
            vec![named_function("is_odd")],
        ),
        "sum",
    );
    let right = terminal(
        relation_stage(
            relation_source("View.Right"),
            "filter",
            vec![named_function("is_even")],
        ),
        "sum",
    );
    Expr::Tuple {
        elements: vec![left, right],
        span: span(),
    }
}

#[test]
fn paired_aggregate_restores_keep_refresh_scope_identity_and_values() {
    let cursor_one = vec![0x54, 0x00];
    let cursor_two = vec![0x54, 0x01];
    let restore = |values: [i64; 3]| {
        BTreeMap::from([
            (None, page(&[values[0]], Some(cursor_one.clone()))),
            (
                Some(cursor_one.clone()),
                page(&[values[1]], Some(cursor_two.clone())),
            ),
            (Some(cursor_two.clone()), page(&[values[2]], None)),
        ])
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Left", restore([1, 2, 3])),
        ("View.Right", restore([2, 3, 4])),
        ("View.Left", restore([5, 6, 7])),
        ("View.Right", restore([6, 7, 8])),
        ("View.Left", restore([2, 9, 10])),
        ("View.Right", restore([9, 10, 11])),
    ]);
    let functions = scoped_refresh_aggregate_functions();
    let run_pair = |source: &mut PairedCursorRestoreSource| {
        let mut functions = functions.clone();
        functions.insert(
            "run".into(),
            PureFunction {
                parameters: Vec::new(),
                body: paired_scoped_aggregate_restore_body(),
                environment: Environment::new(),
            },
        );
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_pair(&mut source);
    assert_eq!(first, integer_pair(4, 6), "odd left rows sum to 4 and even right rows sum to 6");
    let second = run_pair(&mut source);
    assert_eq!(second, integer_pair(12, 14), "the second paired restore sums its own rows");
    let third = run_pair(&mut source);
    assert_eq!(third, integer_pair(9, 10), "the third paired restore sums its own rows");
    assert_eq!(first, integer_pair(4, 6), "later refreshes retain the first aggregate pair");
    assert_eq!(second, integer_pair(12, 14), "later refreshes retain the second aggregate pair");

    assert_eq!(source.lanes.len(), 6, "three refreshes bind independent left and right scopes");
    for generation in 0..3 {
        let start = generation * 2;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired aggregate restore chains keep fresh scope identities: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (name, scope, _) in &source.lanes {
        expected_cursors.extend([
            (name.clone(), *scope, None),
            (name.clone(), *scope, Some(cursor_one.clone())),
            (name.clone(), *scope, Some(cursor_two.clone())),
        ]);
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "reused compact cursor bytes resume only within each refresh's source scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn nested_window_aggregates_recompute_real_values_for_paired_restore_chains() {
    let cursor_one = vec![0x73, 0x00, 0xff];
    let cursor_two = vec![0x73, 0x01];
    let restore = |values: [i64; 3]| {
        BTreeMap::from([
            (None, page(&[values[0]], Some(cursor_one.clone()))),
            (
                Some(cursor_one.clone()),
                page(&[values[1]], Some(cursor_two.clone())),
            ),
            (Some(cursor_two.clone()), page(&[values[2]], None)),
        ])
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Paired", restore([1, 2, 3])),
        ("View.Paired", restore([10, 20, 30])),
        ("View.Paired", restore([4, 5, 6])),
        ("View.Paired", restore([2, 4, 6])),
        ("View.Paired", restore([-1, 3, 5])),
        ("View.Paired", restore([2, 4, 8])),
    ]);
    let mut functions = fixture_functions();
    functions.extend(nested_window_aggregate_functions());
    let run_paired = |source: &mut PairedCursorRestoreSource| {
        let mut functions = functions.clone();
        functions.insert(
            "run".into(),
            PureFunction {
                parameters: Vec::new(),
                body: paired_nested_window_aggregate_body(),
                environment: Environment::new(),
            },
        );
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_paired(&mut source);
    assert_eq!(first, integer(88), "[1,2,3] and [10,20,30] fold to nested totals 8 and 80");
    let second = run_paired(&mut source);
    assert_eq!(second, integer(36), "[4,5,6] and [2,4,6] fold to nested totals 20 and 16");
    let third = run_paired(&mut source);
    assert_eq!(third, integer(28), "[-1,3,5] and [2,4,8] fold to nested totals 10 and 18");
    assert_eq!(first, integer(88), "later restored snapshots do not mutate the first aggregate");
    assert_eq!(second, integer(36), "later restored snapshots do not mutate the second aggregate");

    assert_eq!(source.lanes.len(), 6, "three restore generations each bind a paired source");
    let scopes = source
        .lanes
        .iter()
        .map(|(name, scope, _)| {
            assert_eq!(name, "View.Paired");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "compacted cursor bytes remain isolated by restored source scope: {scope:?}"
        );
    }
    let expected_cursors = scopes
        .iter()
        .copied()
        .flat_map(|scope| {
            [
                ("View.Paired".to_owned(), scope, None),
                ("View.Paired".to_owned(), scope, Some(cursor_one.clone())),
                ("View.Paired".to_owned(), scope, Some(cursor_two.clone())),
            ]
        })
        .collect::<Vec<_>>();
    assert_eq!(
        source.cursors,
        expected_cursors,
        "each nested window chain restores all cursor pages within only its own paired scope"
    );
    assert!(source.pending["View.Paired"].is_empty(), "all six restored snapshots are consumed");

}

#[test]
fn nested_aggregates_keep_values_across_paired_refresh_restore_chains() {
    // The reference leaves cross-refresh cursor reuse unspecified; each refreshed
    // source snapshot therefore owns an independent `(source, scope, cursor)` chain.
    let cursors = [vec![0x69], vec![0x69, 0x00], vec![0x69, 0x00, 0xff]];
    let restore = |groups: &[&[i64]], tokens: &[Vec<u8>]| {
        assert_eq!(groups.len(), tokens.len() + 1);
        groups
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let after = index.checked_sub(1).map(|previous| tokens[previous].clone());
                let next = tokens.get(index).cloned();
                (after, page(values, next))
            })
            .collect::<CursorRestore>()
    };
    let mut source = PairedCursorRestoreSource::new([
        (
            "View.Left",
            restore(&[&[1], &[2], &[3]], &cursors[..2]),
        ),
        (
            "View.Right",
            restore(&[&[10, 20], &[30]], &cursors[..1]),
        ),
        (
            "View.Left",
            restore(&[&[4, 5], &[6]], &cursors[..1]),
        ),
        (
            "View.Right",
            restore(&[&[2], &[4], &[6]], &cursors[..2]),
        ),
        (
            "View.Left",
            restore(&[&[-1], &[3, 5]], &cursors[..1]),
        ),
        (
            "View.Right",
            restore(&[&[2, 4], &[8]], &cursors[..1]),
        ),
    ]);
    let mut functions = nested_aggregate_refresh_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_nested_aggregate_refresh_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(8, 80), "nested window folds compute the first paired snapshot");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(20, 16), "the next restore chain computes its own nested totals");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(10, 18), "the final restore chain retains both nested totals");
    assert_eq!(first, integer_pair(8, 80), "later paired restores leave the first value snapshot intact");
    assert_eq!(second, integer_pair(20, 16), "later paired restores leave the second value snapshot intact");

    let lane_names = [
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
    ];
    let cursor_traces = [
        vec![None, Some(cursors[0].clone()), Some(cursors[1].clone())],
        vec![None, Some(cursors[0].clone())],
        vec![None, Some(cursors[0].clone())],
        vec![None, Some(cursors[0].clone()), Some(cursors[1].clone())],
        vec![None, Some(cursors[0].clone())],
        vec![None, Some(cursors[0].clone())],
    ];
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names)
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each refresh restores left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired restores retain distinct identity across sibling sources and refreshes: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (index, trace) in cursor_traces.into_iter().enumerate() {
        expected_cursors.extend(
            trace
                .into_iter()
                .map(|cursor| (lane_names[index].to_owned(), scopes[index], cursor)),
        );
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "repeated opaque cursors resume only their own nested aggregate restore chain"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_sparse_window_folds_keep_identity_across_compacted_cursor_chains() {
    let cursors = [
        vec![0x73, 0x00],
        vec![0x73, 0x01],
        vec![0x73, 0x02],
        vec![0x73, 0x03],
    ];
    let restore = |values: [i64; 5]| {
        let mut pages = BTreeMap::new();
        for index in 0..values.len() {
            let after = (index > 0).then(|| cursors[index - 1].clone());
            let next = (index + 1 < values.len()).then(|| cursors[index].clone());
            pages.insert(after, page(&[values[index]], next));
        }
        pages
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Paired", restore([1, 0, 3, 2, 5])),
        ("View.Paired", restore([2, 1, 4, 3, 6])),
        ("View.Paired", restore([1, 2, 5, 0, 7])),
        ("View.Paired", restore([4, 1, 6, 3, 8])),
        ("View.Paired", restore([3, 4, 7, 2, 9])),
        ("View.Paired", restore([2, 3, 8, 5, 10])),
    ]);
    let functions = sparse_window_fold_functions();
    let run_paired = |source: &mut PairedCursorRestoreSource| {
        let mut functions = functions.clone();
        functions.insert(
            "run".into(),
            PureFunction {
                parameters: Vec::new(),
                body: paired_sparse_window_fold_body(),
                environment: Environment::new(),
            },
        );
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_paired(&mut source);
    assert_eq!(first, integer(28), "sparse odd/even pages fold to 12 and 16");
    let second = run_paired(&mut source);
    assert_eq!(second, integer(42), "the second paired compact chain folds to 18 and 24");
    let third = run_paired(&mut source);
    assert_eq!(third, integer(54), "the third paired compact chain folds to 26 and 28");
    assert_eq!(first, integer(28), "later cursor restores preserve the first paired fold");
    assert_eq!(second, integer(42), "later cursor restores preserve the second paired fold");

    assert_eq!(source.lanes.len(), 6, "three restores bind two independent source scopes each");
    let scopes = source
        .lanes
        .iter()
        .map(|(name, scope, _)| {
            assert_eq!(name, "View.Paired");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "identical compact cursor tokens remain scoped to their paired lane: {scope:?}"
        );
    }
    let expected_cursors = scopes
        .iter()
        .copied()
        .flat_map(|scope| {
            std::iter::once(("View.Paired".to_owned(), scope, None)).chain(
                cursors
                    .iter()
                    .cloned()
                    .map(move |cursor| ("View.Paired".to_owned(), scope, Some(cursor))),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        source.cursors,
        expected_cursors,
        "each sparse window fold follows the same compacted page sequence in its own scope"
    );
    assert!(source.pending["View.Paired"].is_empty(), "all six sparse page chains are consumed");
}

#[test]
fn paired_window_folds_keep_values_across_sparse_refresh_compaction_chains() {
    // Cursor tokens intentionally repeat across sibling sources and refreshes.
    // A new evaluation is a new snapshot, so its `(source, scope, cursor)` chain
    // starts at that snapshot's first page even when the opaque cursor bytes match.
    let cursors = [vec![0x5a], vec![0x5a, 0x00], vec![0x5a, 0x00, 0xff]];
    let restore = |groups: &[&[i64]], tokens: &[Vec<u8>]| {
        assert_eq!(groups.len(), tokens.len() + 1);
        groups
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let after = index.checked_sub(1).map(|previous| tokens[previous].clone());
                let next = tokens.get(index).cloned();
                (after, page(values, next))
            })
            .collect::<CursorRestore>()
    };
    let mut source = PairedCursorRestoreSource::new([
        (
            "View.Left",
            restore(&[&[1, 2], &[3], &[4, 5]], &cursors[..2]),
        ),
        (
            "View.Right",
            restore(&[&[2, 3], &[4, 5, 6]], &cursors[..1]),
        ),
        (
            "View.Left",
            restore(&[&[1], &[4, 5], &[2], &[7]], &cursors),
        ),
        (
            "View.Right",
            restore(&[&[4, 1], &[6], &[3, 8]], &cursors[..2]),
        ),
        (
            "View.Left",
            restore(&[&[3, 4], &[7, 2], &[9]], &cursors[..2]),
        ),
        (
            "View.Right",
            restore(&[&[2], &[3, 8, 5], &[10]], &cursors[..2]),
        ),
    ]);
    let mut functions = paired_window_refresh_compaction_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_window_refresh_compaction_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(12, 16), "sparse odd/even windows fold to (12, 16)");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(18, 24), "the compacted refresh folds its own rows to (18, 24)");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(26, 28), "the next refresh folds its own rows to (26, 28)");
    assert_eq!(first, integer_pair(12, 16), "later refreshes do not mutate the first snapshot result");
    assert_eq!(second, integer_pair(18, 24), "later refreshes do not mutate the second snapshot result");

    let lane_names = [
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
    ];
    let cursor_traces = [
        vec![None, Some(cursors[0].clone()), Some(cursors[1].clone())],
        vec![None, Some(cursors[0].clone())],
        vec![
            None,
            Some(cursors[0].clone()),
            Some(cursors[1].clone()),
            Some(cursors[2].clone()),
        ],
        vec![None, Some(cursors[0].clone()), Some(cursors[1].clone())],
        vec![None, Some(cursors[0].clone()), Some(cursors[1].clone())],
        vec![None, Some(cursors[0].clone()), Some(cursors[1].clone())],
    ];
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names)
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each refresh reads left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired refreshes cannot alias sibling or prior snapshot scopes: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (index, trace) in cursor_traces.into_iter().enumerate() {
        expected_cursors.extend(
            trace
                .into_iter()
                .map(|cursor| (lane_names[index].to_owned(), scopes[index], cursor)),
        );
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "reused compact cursor bytes advance only within each sparse refresh source scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_limit_compaction_refreshes_keep_scoped_prefix_values() {
    // Nested `take` stages tighten the requested prefix. Compacted opaque
    // continuations are snapshot-local even when all six chains reuse them.
    let compacted_cursor = vec![0xf0];
    let after_compaction_cursor = vec![0xf1];
    let restore = |values: [i64; 4], long_cursor: &[u8]| {
        BTreeMap::from([
            (None, page(&[values[0]], Some(long_cursor.to_vec()))),
            (
                Some(long_cursor.to_vec()),
                page(&[values[1]], Some(compacted_cursor.clone())),
            ),
            (
                Some(compacted_cursor.clone()),
                page(&[values[2]], Some(after_compaction_cursor.clone())),
            ),
            (
                Some(after_compaction_cursor.clone()),
                page(&[values[3]], None),
            ),
        ])
    };
    let generations = [
        ([1, 2, 3, 999], [10, 20, 30, 999]),
        ([4, 5, 6, 888], [2, 4, 6, 888]),
        ([3, 7, 9, 777], [1, 8, 10, 777]),
    ];
    let mut restores = Vec::new();
    let mut lane_names = Vec::new();
    let mut long_cursors_by_lane = Vec::new();
    for (generation, (left, right)) in generations.into_iter().enumerate() {
        for (lane, (source_name, values)) in
            [("View.Left", left), ("View.Right", right)].into_iter().enumerate()
        {
            let tag = 0x20 + (generation * 2 + lane) as u8;
            let long_cursor = vec![tag; 128];
            restores.push((source_name, restore(values, &long_cursor)));
            lane_names.push(source_name);
            long_cursors_by_lane.push(long_cursor);
        }
    }
    let mut source = PairedCursorRestoreSource::new(restores);
    let mut functions = paired_limit_refresh_compaction_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_limit_refresh_compaction_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(60, 600), "the first paired prefix scales and sums its first three rows");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(150, 120), "the next compacted pair computes from its own prefixes");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(190, 190), "the final pair keeps each refreshed limit prefix separate");
    assert_eq!(first, integer_pair(60, 600), "later restores leave the first limited result intact");
    assert_eq!(second, integer_pair(150, 120), "later restores leave the second limited result intact");

    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names.iter().copied())
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each refresh reads left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "fresh paired limits cannot reuse a sibling or prior refresh scope: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for index in 0..lane_names.len() {
        let source_name = lane_names[index].to_owned();
        expected_cursors.extend([
            (source_name.clone(), scopes[index], None),
            (
                source_name.clone(),
                scopes[index],
                Some(long_cursors_by_lane[index].clone()),
            ),
            (
                source_name,
                scopes[index],
                Some(compacted_cursor.clone()),
            ),
        ]);
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "each limited prefix follows only its scoped cursor chain through compaction"
    );
    assert!(
        source
            .cursors
            .iter()
            .all(|(_, _, cursor)| cursor.as_ref() != Some(&after_compaction_cursor)),
        "take(3) completes before requesting the fourth, out-of-prefix page"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_sparse_window_folds_keep_identity_across_checkpoint_rotation_chains() {
    // The reference is silent on provider checkpoint rotation between refreshes;
    // pin every `(source, scope)` to one forward-moving rotation chain.
    let checkpoint_epochs = [0x41, 0x42, 0x43, 0x44];
    let restore = |groups: &[&[i64]], tokens: &[Vec<u8>]| {
        assert_eq!(groups.len(), tokens.len() + 1);
        groups
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let after = index.checked_sub(1).map(|previous| tokens[previous].clone());
                let next = tokens.get(index).cloned();
                (after, page(values, next))
            })
            .collect::<CursorRestore>()
    };
    let mut restores = Vec::new();
    let mut lane_names = Vec::new();
    let mut rotated_chains = Vec::new();
    let mut add_restore = |source_name: &'static str, groups: &[&[i64]], rotation: usize| {
        let tokens = (0..checkpoint_epochs.len())
            .map(|step| vec![checkpoint_epochs[rotation], step as u8])
            .collect::<Vec<_>>();
        restores.push((source_name, restore(groups, &tokens)));
        lane_names.push(source_name);
        rotated_chains.push(tokens);
    };
    add_restore("View.Left", &[&[1, 2], &[3, 4], &[5, 6], &[7, 8], &[9, 10]], 0);
    add_restore("View.Right", &[&[2, 1], &[4, 3], &[6, 5], &[8, 7], &[10, 9]], 1);
    add_restore("View.Left", &[&[3, 2], &[5, 4], &[7, 6], &[9, 8], &[11, 10]], 2);
    add_restore("View.Right", &[&[2, 1], &[6, 3], &[8, 5], &[10, 7], &[12, 9]], 3);
    add_restore("View.Left", &[&[1, 2], &[5, 4], &[7, 6], &[11, 8], &[13, 10]], 1);
    add_restore("View.Right", &[&[4, 3], &[8, 5], &[10, 7], &[12, 9], &[14, 11]], 0);
    drop(add_restore);

    let mut source = PairedCursorRestoreSource::new(restores);
    let mut functions = paired_checkpoint_rotation_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_sparse_checkpoint_rotation_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(60, 72), "the first sparse paired windows fold to their nested totals");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(84, 94), "the rotated checkpoint pair computes its own sparse folds");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(90, 118), "the next rotation retains independent left/right window folds");
    assert_eq!(first, integer_pair(60, 72), "later rotations leave the first fold snapshot unchanged");
    assert_eq!(second, integer_pair(84, 94), "later rotations leave the middle fold snapshot unchanged");

    assert_eq!(source.lanes.len(), lane_names.len());
    assert_eq!(
        rotated_chains
            .iter()
            .map(|chain| chain[0][0])
            .collect::<Vec<_>>(),
        [0x41, 0x42, 0x43, 0x44, 0x42, 0x41],
        "checkpoint epochs rotate across both source lanes and all refreshes"
    );
    for chain in &rotated_chains {
        for adjacent in chain.windows(2) {
            assert!(
                adjacent[0] < adjacent[1],
                "each rotated checkpoint chain still advances lexicographically"
            );
        }
    }
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names.iter().copied())
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each refresh folds left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "sibling checkpoint rotations cannot alias an earlier source scope: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (index, chain) in rotated_chains.iter().enumerate() {
        let source_name = lane_names[index].to_owned();
        expected_cursors.push((source_name.clone(), scopes[index], None));
        expected_cursors.extend(chain.iter().cloned().map(|cursor| {
            (source_name.clone(), scopes[index], Some(cursor))
        }));
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "each sparse window fold restores all pages only through its scoped rotation chain"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_refresh_cursor_restores_preserve_source_scope_identity() {
    let continuations = [vec![0x62], vec![0x62, 0x00], vec![0x62, 0x00, 0x00]];
    assert!(continuations[0].as_slice() < continuations[1].as_slice());
    assert!(continuations[1].as_slice() < continuations[2].as_slice());
    let generations = [
        ([1, 2, 3, 4], [5, 8, 5, 8], [4, 3, 2, 1]),
        ([3, 4, 1, 6], [5, 8, 5, 8], [2, 4, 3, 1]),
        ([1, 6, 3, 2], [5, 8, 5, 8], [3, 2, 4, 3]),
    ];
    let mut restores = Vec::new();
    for (values, filtered_values, depths) in generations {
        for lane in 0..4 {
            let source_name = if lane < 2 { "View.Left" } else { "View.Right" };
            let depth = depths[lane];
            let restored_pages = (0..depth)
                .map(|page_index| {
                    let after = (page_index > 0)
                        .then(|| continuations[page_index - 1].clone());
                    let next = (page_index + 1 < depth)
                        .then(|| continuations[page_index].clone());
                    let value = if page_index == 0 {
                        values[lane]
                    } else {
                        filtered_values[lane]
                    };
                    (after, page(&[value], next))
                })
                .collect::<BTreeMap<_, _>>();
            restores.push((source_name, restored_pages));
        }
    }
    let mut source = PairedCursorRestoreSource::new(restores);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(14), "first paired refresh folds the restored left and right pages");
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(18), "second refresh uses its own cursor-keyed source snapshots");
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(16), "third refresh restores its own paired page chains");
    assert_eq!(first, integer(14), "later restores leave the first fold result captured");
    assert_eq!(second, integer(18), "later restores leave the second fold result captured");

    assert_eq!(source.lanes.len(), 12, "three refreshes bind four source scopes apiece");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "source-qualified restored chains keep fresh scope identities: {scope:?}"
        );
    }
    let depths = generations
        .iter()
        .flat_map(|(_, _, lane_depths)| lane_depths)
        .copied()
        .collect::<Vec<_>>();
    let mut expected_cursors = Vec::new();
    for (lane, depth) in depths.into_iter().enumerate() {
        let (source_name, scope, _) = &source.lanes[lane];
        expected_cursors.push((source_name.clone(), *scope, None));
        for cursor in continuations.iter().take(depth - 1) {
            expected_cursors.push((source_name.clone(), *scope, Some(cursor.clone())));
        }
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "reused restoration cursors resolve only within their source and fresh read scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn nested_aggregate_folds_keep_identity_across_paired_refresh_compaction_chains() {
    // The reference does not specify whether opaque provider cursors survive
    // refresh compaction. Keep the documented lexicographic progress within a
    // chain, and treat each `(source, scope)` snapshot as its own chain even
    // when both long and compact cursor tokens are reused by sibling snapshots.
    let long_checkpoint_cursor = vec![0x5a, 0xff, 0x00];
    let compacted_cursor = vec![0x5b];
    let restore = |groups: &[&[i64]]| {
        assert_eq!(groups.len(), 3, "each checkpoint chain has three pages");
        BTreeMap::from([
            (
                None,
                page(groups[0], Some(long_checkpoint_cursor.clone())),
            ),
            (
                Some(long_checkpoint_cursor.clone()),
                page(groups[1], Some(compacted_cursor.clone())),
            ),
            (
                Some(compacted_cursor.clone()),
                page(groups[2], None),
            ),
        ])
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Left", restore(&[&[1, 2], &[3], &[4]])),
        ("View.Right", restore(&[&[10], &[20, 30], &[40]])),
        ("View.Left", restore(&[&[4], &[5, 6], &[7]])),
        ("View.Right", restore(&[&[2, 4], &[6], &[8]])),
        ("View.Left", restore(&[&[-1, 3], &[5], &[7]])),
        ("View.Right", restore(&[&[3], &[6, 9], &[12]])),
    ]);
    let mut functions = nested_aggregate_compaction_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_nested_aggregate_refresh_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(20, 200), "nested folds compute the first paired snapshot");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(44, 40), "the next compacted pair computes its own nested totals");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(30, 60), "the final pair folds its own pages after compaction");
    assert_eq!(first, integer_pair(20, 200), "later refreshes retain the first paired value snapshot");
    assert_eq!(second, integer_pair(44, 40), "later refreshes retain the second paired value snapshot");

    let lane_names = [
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
    ];
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names)
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each generation restores left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired compaction chains retain independent snapshot scopes: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (index, scope) in scopes.iter().copied().enumerate() {
        expected_cursors.extend([
            (lane_names[index].to_owned(), scope, None),
            (
                lane_names[index].to_owned(),
                scope,
                Some(long_checkpoint_cursor.clone()),
            ),
            (
                lane_names[index].to_owned(),
                scope,
                Some(compacted_cursor.clone()),
            ),
        ]);
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "the shared compact token resumes only inside its paired refresh and source scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_pagination_restores_keep_forked_cursor_chains_scoped() {
    let shared_cursor = vec![0x61, 0x00];
    let forked_chains = (0..4u8)
        .map(|branch| {
            let mut cursor = shared_cursor.clone();
            cursor.push(branch + 1);
            let mut chain = vec![shared_cursor.clone(), cursor.clone()];
            cursor.push(0x00);
            chain.push(cursor.clone());
            cursor.push(0xff);
            chain.push(cursor);
            for pair in chain.windows(2) {
                assert!(pair[0].as_slice() < pair[1].as_slice());
            }
            chain
        })
        .collect::<Vec<_>>();
    let generations = [
        ([1, 2, 3, 4], [5, 8, 5, 8], [5, 4, 3, 2]),
        ([3, 4, 1, 6], [5, 8, 5, 8], [2, 5, 4, 3]),
        ([1, 6, 3, 2], [5, 8, 5, 8], [3, 2, 5, 4]),
    ];
    let mut restores = Vec::new();
    for (values, filtered_values, depths) in generations {
        for lane in 0..4 {
            let source_name = if lane < 2 { "View.Left" } else { "View.Right" };
            let depth = depths[lane];
            let chain = &forked_chains[lane];
            let restored_pages = (0..depth)
                .map(|page_index| {
                    let after = (page_index > 0).then(|| chain[page_index - 1].clone());
                    let next = (page_index + 1 < depth).then(|| chain[page_index].clone());
                    let value = if page_index == 0 {
                        values[lane]
                    } else {
                        filtered_values[lane]
                    };
                    (after, page(&[value], next))
                })
                .collect::<BTreeMap<_, _>>();
            restores.push((source_name, restored_pages));
        }
    }
    let mut source = PairedCursorRestoreSource::new(restores);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(14), "first paired fold restores its branch-specific cursor tails");
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(18), "second fold restores values from its own paired lanes");
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(16), "third fold restores values after a second handoff");
    assert_eq!(first, integer(14), "later restore chains preserve the first folded value");
    assert_eq!(second, integer(18), "later restore chains preserve the second folded value");

    assert_eq!(source.lanes.len(), 12, "three refreshes bind four independent scopes each");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired restore forks retain distinct fresh scopes: {scope:?}"
        );
    }
    let depths = generations
        .iter()
        .flat_map(|(_, _, lane_depths)| lane_depths)
        .copied()
        .collect::<Vec<_>>();
    let mut expected_cursors = Vec::new();
    for (lane, depth) in depths.into_iter().enumerate() {
        let (source_name, scope, _) = &source.lanes[lane];
        expected_cursors.push((source_name.clone(), *scope, None));
        for cursor in forked_chains[lane % 4].iter().take(depth - 1) {
            expected_cursors.push((source_name.clone(), *scope, Some(cursor.clone())));
        }
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "paired restore requests preserve the shared checkpoint and each scope-specific cursor tail"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_compacted_cursor_restores_keep_scope_identity() {
    let cursor_128 = vec![0x20; 128];
    let continuations = [cursor_128, vec![0x21], vec![0x22], vec![0x23]];
    assert_eq!(
        continuations.iter().map(Vec::len).collect::<Vec<_>>(),
        [128, 1, 1, 1],
        "checkpoint compaction shrinks cursor payloads without dropping the chain"
    );
    for pair in continuations.windows(2) {
        assert!(pair[0].as_slice() < pair[1].as_slice());
    }
    let generations = [
        ([1, 2, 3, 4], [5, 8, 5, 8], [5, 4, 3, 2]),
        ([3, 4, 1, 6], [5, 8, 5, 8], [2, 5, 4, 3]),
        ([1, 6, 3, 2], [5, 8, 5, 8], [3, 2, 5, 4]),
    ];
    let mut restores = Vec::new();
    for (values, filtered_values, depths) in generations {
        for lane in 0..4 {
            let source_name = if lane < 2 { "View.Left" } else { "View.Right" };
            let depth = depths[lane];
            let restored_pages = (0..depth)
                .map(|page_index| {
                    let after = (page_index > 0)
                        .then(|| continuations[page_index - 1].clone());
                    let next = (page_index + 1 < depth)
                        .then(|| continuations[page_index].clone());
                    let value = if page_index == 0 {
                        values[lane]
                    } else {
                        filtered_values[lane]
                    };
                    (after, page(&[value], next))
                })
                .collect::<BTreeMap<_, _>>();
            restores.push((source_name, restored_pages));
        }
    }
    let mut source = PairedCursorRestoreSource::new(restores);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(14), "first fold restores its compacting paired page chains");
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(18), "second fold observes its own refreshed values");
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(16), "third fold observes values after another compacting restore");
    assert_eq!(first, integer(14), "later compacted restores preserve the first fold value");
    assert_eq!(second, integer(18), "later compacted restores preserve the second fold value");

    assert_eq!(source.lanes.len(), 12, "three refreshes bind four independent scopes each");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "compacted cursor chains stay separated by fresh source scope: {scope:?}"
        );
    }
    let depths = generations
        .iter()
        .flat_map(|(_, _, lane_depths)| lane_depths)
        .copied()
        .collect::<Vec<_>>();
    let mut expected_cursors = Vec::new();
    for (lane, depth) in depths.into_iter().enumerate() {
        let (source_name, scope, _) = &source.lanes[lane];
        expected_cursors.push((source_name.clone(), *scope, None));
        for cursor in continuations.iter().take(depth - 1) {
            expected_cursors.push((source_name.clone(), *scope, Some(cursor.clone())));
        }
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "each long or compacted cursor restores only inside its paired read scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_refresh_compaction_collisions_remain_scope_local() {
    let compacted = vec![0xf0];
    let generations = [
        ([1, 2, 3, 4], [5, 8, 5, 8], [5, 4, 3, 2]),
        ([3, 4, 1, 6], [5, 8, 5, 8], [2, 5, 4, 3]),
        ([1, 6, 3, 2], [5, 8, 5, 8], [3, 2, 5, 4]),
    ];
    let mut restores = Vec::new();
    let mut continuations_by_lane = Vec::new();
    for (generation, (values, filtered_values, depths)) in generations.into_iter().enumerate() {
        for lane in 0..4 {
            let source_name = if lane < 2 { "View.Left" } else { "View.Right" };
            let depth = depths[lane];
            let long_tag = 0x10 + (generation * 4 + lane) as u8;
            let chain = vec![
                vec![long_tag; 128],
                compacted.clone(),
                vec![0xf1],
                vec![0xf2],
            ];
            for pair in chain.windows(2) {
                assert!(pair[0].as_slice() < pair[1].as_slice());
            }
            let restored_pages = (0..depth)
                .map(|page_index| {
                    let after = (page_index > 0).then(|| chain[page_index - 1].clone());
                    let next = (page_index + 1 < depth).then(|| chain[page_index].clone());
                    let value = if page_index == 0 {
                        values[lane]
                    } else {
                        filtered_values[lane]
                    };
                    (after, page(&[value], next))
                })
                .collect::<BTreeMap<_, _>>();
            restores.push((source_name, restored_pages));
            continuations_by_lane.push(chain);
        }
    }
    assert_eq!(compacted.len(), 1, "distinct long checkpoints compact to one shared cursor");
    let mut source = PairedCursorRestoreSource::new(restores);

    let first = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(first, integer(14), "first paired refresh restores its values after compaction");
    let second = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(second, integer(18), "second refresh keeps compacted cursors in its own scopes");
    let third = run_with_fixture_functions(paired_scoped_view_refresh_body(), &mut source).unwrap();
    assert_eq!(third, integer(16), "third refresh restores from the converged cursor independently");
    assert_eq!(first, integer(14), "later compacted refreshes preserve the initial value");
    assert_eq!(second, integer(18), "later compacted refreshes preserve the middle value");

    assert_eq!(source.lanes.len(), 12, "three refreshes bind four independently compacted scopes");
    for generation in 0..3 {
        let start = generation * 4;
        assert_eq!(source.lanes[start].0, "View.Left");
        assert_eq!(source.lanes[start + 1].0, "View.Left");
        assert_eq!(source.lanes[start + 2].0, "View.Right");
        assert_eq!(source.lanes[start + 3].0, "View.Right");
    }
    let scopes = source.lanes.iter().map(|(_, scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "compaction collisions remain isolated by refresh scope: {scope:?}"
        );
    }
    let depths = generations
        .iter()
        .flat_map(|(_, _, lane_depths)| lane_depths)
        .copied()
        .collect::<Vec<_>>();
    let mut expected_cursors = Vec::new();
    for (lane, depth) in depths.into_iter().enumerate() {
        let (source_name, scope, _) = &source.lanes[lane];
        expected_cursors.push((source_name.clone(), *scope, None));
        for cursor in continuations_by_lane[lane].iter().take(depth - 1) {
            expected_cursors.push((source_name.clone(), *scope, Some(cursor.clone())));
        }
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "divergent long checkpoints converge on the compact cursor without crossing scopes"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
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

#[test]
fn paired_subscription_handoffs_rebind_reused_batch_cursors_to_fresh_scopes() {
    // Cursor tokens may recur after a refresh. The provider keys each stream
    // by `RelationReadScope`, so same-source paired subscriptions do not
    // resume a prior fold's page batches when their opaque cursors reappear.
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
            page(&[-3], Some(vec![11])),
            page(&[5], Some(vec![55])),
            page(&[7], None),
        ],
        vec![
            page(&[2], Some(vec![11])),
            page(&[9], Some(vec![66])),
            page(&[8], None),
        ],
        vec![
            page(&[-1], Some(vec![11])),
            page(&[3], Some(vec![33])),
            page(&[7], None),
        ],
        vec![
            page(&[2], Some(vec![11])),
            page(&[4], Some(vec![44])),
            page(&[10], None),
        ],
    ]);

    for (fold, expected) in [21, 3, 12].into_iter().enumerate() {
        assert_eq!(
            run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap(),
            integer(expected),
            "same-source paired refresh fold {fold} computes only its own filtered pages"
        );
    }

    assert_eq!(source.lanes.len(), 6);
    for generation in 0..3 {
        let left = source.lanes[generation * 2].0;
        let right = source.lanes[generation * 2 + 1].0;
        assert_ne!(left, right, "generation {generation} keeps sibling subscriptions distinct");
    }
    let scopes = source.lanes.iter().map(|(scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "every paired refresh owns fresh read scopes even when cursor bytes recur"
        );
    }
    assert_eq!(
        source.cursors,
        vec![
            (scopes[0], None),
            (scopes[0], Some(vec![11])),
            (scopes[0], Some(vec![33])),
            (scopes[1], None),
            (scopes[1], Some(vec![11])),
            (scopes[1], Some(vec![44])),
            (scopes[2], None),
            (scopes[2], Some(vec![11])),
            (scopes[2], Some(vec![55])),
            (scopes[3], None),
            (scopes[3], Some(vec![11])),
            (scopes[3], Some(vec![66])),
            (scopes[4], None),
            (scopes[4], Some(vec![11])),
            (scopes[4], Some(vec![33])),
            (scopes[5], None),
            (scopes[5], Some(vec![11])),
            (scopes[5], Some(vec![44])),
        ],
        "a repeated continuation resumes only within its new paired subscription scope"
    );
}

#[test]
fn paired_subscription_refresh_chain_retains_values_across_variable_page_handoffs() {
    // The reference is silent about retaining source subscriptions across
    // refresh folds with different page counts. Each new scoped fold binds a
    // fresh pair while captured outputs remain values from their own snapshot.
    let mut source = PairedSubscriptionSource::new([
        vec![
            page(&[1], Some(vec![11])),
            page(&[3], Some(vec![33])),
            page(&[5], None),
        ],
        vec![page(&[2], Some(vec![11])), page(&[4], None)],
        vec![page(&[-3], Some(vec![11])), page(&[3], None)],
        vec![
            page(&[2], Some(vec![11])),
            page(&[9], Some(vec![66])),
            page(&[8], None),
        ],
        vec![page(&[1], None)],
        vec![page(&[2], Some(vec![11])), page(&[4], None)],
    ]);

    let first_fold = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(first_fold, integer(14));
    let second_fold = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(second_fold, integer(7));
    let third_fold = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(third_fold, integer(10));

    assert_eq!(first_fold, integer(14), "the first fold remains pinned after two refresh handoffs");
    assert_eq!(second_fold, integer(7), "the second fold remains pinned after the third fold");
    assert_eq!(source.lanes.len(), 6);
    for generation in 0..3 {
        assert_ne!(
            source.lanes[generation * 2].0,
            source.lanes[generation * 2 + 1].0,
            "refresh generation {generation} owns distinct paired subscription scopes"
        );
    }
    let scopes = source.lanes.iter().map(|(scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "all refresh handoffs allocate fresh subscription scopes"
        );
    }
    assert_eq!(
        source.cursors,
        vec![
            (scopes[0], None),
            (scopes[0], Some(vec![11])),
            (scopes[0], Some(vec![33])),
            (scopes[1], None),
            (scopes[1], Some(vec![11])),
            (scopes[2], None),
            (scopes[2], Some(vec![11])),
            (scopes[3], None),
            (scopes[3], Some(vec![11])),
            (scopes[3], Some(vec![66])),
            (scopes[4], None),
            (scopes[5], None),
            (scopes[5], Some(vec![11])),
        ],
        "each variable-length fold terminates and resumes only inside its own handoff scope"
    );
}

#[test]
fn paired_subscription_read_batches_preserve_four_page_cursor_fold_chains() {
    // Each pair traverses four pages before the next refresh handoff. The
    // first and third generations reuse their complete cursor sequences to
    // prove that batch identity follows the fresh read scope, not the token.
    let mut source = PairedSubscriptionSource::new([
        vec![
            page(&[-1], Some(vec![11])),
            page(&[1], Some(vec![22])),
            page(&[3], Some(vec![33])),
            page(&[5], None),
        ],
        vec![
            page(&[2], Some(vec![11])),
            page(&[4], Some(vec![24])),
            page(&[6], Some(vec![34])),
            page(&[8], None),
        ],
        vec![
            page(&[7], Some(vec![11])),
            page(&[9], Some(vec![52])),
            page(&[11], Some(vec![53])),
            page(&[13], None),
        ],
        vec![
            page(&[2], Some(vec![11])),
            page(&[8], Some(vec![64])),
            page(&[10], Some(vec![65])),
            page(&[12], None),
        ],
        vec![
            page(&[-1], Some(vec![11])),
            page(&[3], Some(vec![22])),
            page(&[5], Some(vec![33])),
            page(&[7], None),
        ],
        vec![
            page(&[2], Some(vec![11])),
            page(&[4], Some(vec![24])),
            page(&[8], Some(vec![34])),
            page(&[10], None),
        ],
    ]);

    let first_fold = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(first_fold, integer(21));
    let second_fold = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(second_fold, integer(3));
    let third_fold = run_with_fixture_functions(paired_subscription_cascade_body(), &mut source).unwrap();
    assert_eq!(third_fold, integer(12));

    assert_eq!(first_fold, integer(21), "the first folded result survives both later handoffs");
    assert_eq!(second_fold, integer(3), "the second folded result survives the third handoff");
    assert_eq!(source.lanes.len(), 6);
    for generation in 0..3 {
        assert_ne!(
            source.lanes[generation * 2].0,
            source.lanes[generation * 2 + 1].0,
            "generation {generation} keeps its left and right read scopes independent"
        );
    }
    let scopes = source.lanes.iter().map(|(scope, _)| *scope).collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "each refreshed four-page pair receives new read-batch identities"
        );
    }
    assert_eq!(
        source.cursors,
        vec![
            (scopes[0], None),
            (scopes[0], Some(vec![11])),
            (scopes[0], Some(vec![22])),
            (scopes[0], Some(vec![33])),
            (scopes[1], None),
            (scopes[1], Some(vec![11])),
            (scopes[1], Some(vec![24])),
            (scopes[1], Some(vec![34])),
            (scopes[2], None),
            (scopes[2], Some(vec![11])),
            (scopes[2], Some(vec![52])),
            (scopes[2], Some(vec![53])),
            (scopes[3], None),
            (scopes[3], Some(vec![11])),
            (scopes[3], Some(vec![64])),
            (scopes[3], Some(vec![65])),
            (scopes[4], None),
            (scopes[4], Some(vec![11])),
            (scopes[4], Some(vec![22])),
            (scopes[4], Some(vec![33])),
            (scopes[5], None),
            (scopes[5], Some(vec![11])),
            (scopes[5], Some(vec![24])),
            (scopes[5], Some(vec![34])),
        ],
        "each exact cursor chain stays local to its paired read scope during handoff"
    );
}

#[test]
fn paired_refresh_windows_keep_scope_identity_across_compaction_chains() {
    // The reference requires complete windows but leaves cross-refresh opaque
    // cursor reuse unspecified. Each refreshed source scope owns its chain;
    // window(3, 2) also has to carry a boundary row across cursor compaction.
    let long_cursor = vec![0x48, 0xff, 0x00];
    let compact_cursor = vec![0x49];
    let restore = |groups: &[&[i64]]| {
        assert_eq!(groups.len(), 3, "each six-row snapshot spans three pages");
        BTreeMap::from([
            (None, page(groups[0], Some(long_cursor.clone()))),
            (
                Some(long_cursor.clone()),
                page(groups[1], Some(compact_cursor.clone())),
            ),
            (Some(compact_cursor.clone()), page(groups[2], None)),
        ])
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Left", restore(&[&[1, 2], &[3], &[4, 5, 99]])),
        ("View.Right", restore(&[&[2], &[4, 6], &[8, 10, 99]])),
        ("View.Left", restore(&[&[-1], &[3, 5], &[7, 9, -99]])),
        ("View.Right", restore(&[&[3, 6], &[9], &[12, 15, -99]])),
        ("View.Left", restore(&[&[2, 0, 4], &[6], &[8, 99]])),
        ("View.Right", restore(&[&[-2], &[5, 7], &[11, 13, 99]])),
    ]);
    let mut functions = paired_window_scope_compaction_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_window_scope_compaction_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(18, 36), "the first complete windows sum to (18, 36)");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(28, 54), "the next compacted pair sums to (28, 54)");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(24, 41), "the final pair sums complete restored windows");
    assert_eq!(first, integer_pair(18, 36), "later refreshes keep the first window snapshot intact");
    assert_eq!(second, integer_pair(28, 54), "later refreshes keep the second window snapshot intact");

    let lane_names = [
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
    ];
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names)
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each refresh binds left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired window refreshes retain six distinct source scopes: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (index, scope) in scopes.iter().copied().enumerate() {
        expected_cursors.extend([
            (lane_names[index].to_owned(), scope, None),
            (
                lane_names[index].to_owned(),
                scope,
                Some(long_cursor.clone()),
            ),
            (
                lane_names[index].to_owned(),
                scope,
                Some(compact_cursor.clone()),
            ),
        ]);
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "shared opaque cursors resume only within each paired refresh scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_window_folds_keep_values_across_sparse_checkpoint_restore_chains() {
    let cursors = [vec![0x61, 0x00], vec![0x61, 0x01], vec![0x61, 0x02]];
    let restore = |groups: &[&[i64]]| {
        assert_eq!(groups.len(), cursors.len() + 1);
        groups
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let after = index.checked_sub(1).map(|previous| cursors[previous].clone());
                let next = cursors.get(index).cloned();
                (after, page(values, next))
            })
            .collect::<CursorRestore>()
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Left", restore(&[&[1, 3], &[2, 4, 5], &[6, 7, 8], &[9, 10]])),
        ("View.Right", restore(&[&[1, 3], &[2, 4, 5], &[6, 7, 8], &[9, 10]])),
        ("View.Left", restore(&[&[11, 13], &[12, 14, 15], &[16, 17, 18], &[19, 20]])),
        ("View.Right", restore(&[&[11, 13], &[12, 14, 15], &[16, 17, 18], &[19, 20]])),
        ("View.Left", restore(&[&[-9, -7], &[-8, -6, -5], &[-4, -3, -2], &[-1, 0]])),
        ("View.Right", restore(&[&[-10, -8], &[-9, -7, -6], &[-5, -4, -3], &[-2, -1]])),
    ]);
    let mut functions = paired_sparse_window_checkpoint_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_sparse_window_checkpoint_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(30, 36), "the initial sparse windows fold to (30, 36)");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(90, 96), "the second checkpoint pair folds to (90, 96)");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(-30, -36), "the final checkpoint pair folds to (-30, -36)");
    assert_eq!(first, integer_pair(30, 36), "later restores leave the first sparse fold unchanged");
    assert_eq!(second, integer_pair(90, 96), "later restores leave the second sparse fold unchanged");
    let lane_names = [
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
    ];
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names)
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each restored pair reads left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "repeated sparse checkpoint bytes bind to distinct refreshed source scopes: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (index, scope) in scopes.iter().copied().enumerate() {
        expected_cursors.extend(
            std::iter::once((lane_names[index].to_owned(), scope, None)).chain(
                cursors.iter().cloned().map(|cursor| {
                    (lane_names[index].to_owned(), scope, Some(cursor))
                }),
            ),
        );
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "the same sparse checkpoint sequence advances independently in each source scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn nested_aggregates_keep_values_across_paired_pagination_compaction_chains() {
    // Cross-refresh cursor reuse is unspecified by the reference. Each source
    // snapshot owns its page chain; the short checkpoint remains greater than
    // its long predecessor and is reused only inside that scope.
    let cursors = [vec![0x51, 0xff], vec![0x52], vec![0x52, 0x01]];
    let restore = |groups: &[&[i64]]| {
        assert_eq!(groups.len(), cursors.len() + 1);
        groups
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let after = index.checked_sub(1).map(|previous| cursors[previous].clone());
                let next = cursors.get(index).cloned();
                (after, page(values, next))
            })
            .collect::<CursorRestore>()
    };
    let mut source = PairedCursorRestoreSource::new([
        ("View.Left", restore(&[&[1, 2], &[3], &[4, 5], &[6]])),
        ("View.Right", restore(&[&[10], &[20, 30], &[40], &[50, 60]])),
        ("View.Left", restore(&[&[4], &[5, 6], &[7], &[8, 9]])),
        ("View.Right", restore(&[&[2], &[4, 6], &[8], &[10, 12]])),
        ("View.Left", restore(&[&[-1], &[3, 5], &[7], &[9, 11]])),
        ("View.Right", restore(&[&[3], &[6, 9], &[12], &[15, 18]])),
    ]);
    let mut functions = nested_pagination_compaction_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_nested_aggregate_refresh_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(56, 560), "the first nested page folds compute (56, 560)");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(104, 112), "the compacted second pair computes (104, 112)");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(94, 168), "the final page pair computes (94, 168)");
    assert_eq!(first, integer_pair(56, 560), "later refreshes preserve the first nested aggregate pair");
    assert_eq!(second, integer_pair(104, 112), "later refreshes preserve the second nested aggregate pair");
    let lane_names = [
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
        "View.Left",
        "View.Right",
    ];
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names)
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "nested refreshes read left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired nested page chains keep distinct source scope identities: {scope:?}"
        );
    }
    let mut expected_cursors = Vec::new();
    for (index, scope) in scopes.iter().copied().enumerate() {
        expected_cursors.extend(
            std::iter::once((lane_names[index].to_owned(), scope, None)).chain(
                cursors.iter().cloned().map(|cursor| {
                    (lane_names[index].to_owned(), scope, Some(cursor))
                }),
            ),
        );
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "reused compact pagination cursors restore only the corresponding nested source snapshot"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_refreshes_keep_values_across_sparse_checkpoint_rotation_chains() {
    let checkpoint_epochs = [
        [
            vec![0x71, 0x02],
            vec![0x71, 0x08],
            vec![0x71, 0x20],
            vec![0x71, 0x40],
        ],
        [
            vec![0x72, 0x01],
            vec![0x72, 0x09],
            vec![0x72, 0x21],
            vec![0x72, 0x41],
        ],
        [
            vec![0x73, 0x03],
            vec![0x73, 0x0a],
            vec![0x73, 0x22],
            vec![0x73, 0x42],
        ],
    ];
    let restore = |groups: &[&[i64]], tokens: &[Vec<u8>]| {
        assert_eq!(groups.len(), tokens.len() + 1);
        groups
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let after = index.checked_sub(1).map(|previous| tokens[previous].clone());
                let next = tokens.get(index).cloned();
                (after, page(values, next))
            })
            .collect::<CursorRestore>()
    };
    let mut restores = Vec::new();
    let mut lane_names = Vec::new();
    let mut rotated_chains = Vec::new();
    let mut add_restore = |source_name: &'static str, groups: &[&[i64]], epoch: usize| {
        let tokens = checkpoint_epochs[epoch][..groups.len() - 1].to_vec();
        restores.push((source_name, restore(groups, &tokens)));
        lane_names.push(source_name);
        rotated_chains.push(tokens);
    };
    add_restore("View.Left", &[&[1, 2], &[4, 3, 6], &[5, 8], &[7, 9], &[10]], 0);
    add_restore("View.Right", &[&[2, 1, 4], &[6, 3], &[8, 5, 10], &[7]], 1);
    add_restore("View.Left", &[&[11, 12, 13], &[14, 15], &[16, 17, 18], &[19, 20]], 2);
    add_restore("View.Right", &[&[21, 22, 23, 24], &[25, 26, 27, 28], &[29, 30]], 0);
    add_restore("View.Left", &[&[-9, -8, -7], &[-6, -5], &[-4, -3, -2], &[-1, 0]], 1);
    add_restore("View.Right", &[&[-10, -8], &[-9, -7, -6], &[-5, -4], &[-3, -2]], 2);
    add_restore("View.Left", &[&[101, 102, 103], &[104, 105, 106, 107], &[108, 109, 110]], 0);
    add_restore("View.Right", &[&[101, 102], &[103, 104, 105], &[106, 107, 108], &[109, 110]], 1);
    drop(add_restore);

    let mut source = PairedCursorRestoreSource::new(restores);
    let mut functions = paired_refresh_sparse_rotation_functions();
    functions.insert(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body: paired_refresh_sparse_rotation_body(),
            environment: Environment::new(),
        },
    );
    let run_refresh = |source: &mut PairedCursorRestoreSource| {
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            source,
        )
        .unwrap()
    };

    let first = run_refresh(&mut source);
    assert_eq!(first, integer_pair(30, 36), "the first sparse pair computes (30, 36)");
    let second = run_refresh(&mut source);
    assert_eq!(second, integer_pair(90, 156), "the second rotated pair computes (90, 156)");
    let third = run_refresh(&mut source);
    assert_eq!(third, integer_pair(-30, -36), "the third rotated pair computes (-30, -36)");
    let fourth = run_refresh(&mut source);
    assert_eq!(fourth, integer_pair(630, 636), "the fourth rotated pair computes (630, 636)");
    assert_eq!(first, integer_pair(30, 36), "later refreshes preserve the first result");
    assert_eq!(second, integer_pair(90, 156), "later refreshes preserve the second result");
    assert_eq!(third, integer_pair(-30, -36), "later refreshes preserve the third result");
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names.iter().copied())
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each refresh reads left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "reused checkpoint epochs bind to distinct paired refresh scopes: {scope:?}"
        );
    }
    assert_eq!(
        rotated_chains.iter().map(Vec::len).collect::<Vec<_>>(),
        [4, 3, 3, 2, 3, 3, 2, 3],
        "each sparse restore follows only the checkpoints present in its page chain"
    );
    assert_eq!(
        rotated_chains
            .iter()
            .map(|chain| chain[0][0])
            .collect::<Vec<_>>(),
        [0x71, 0x72, 0x73, 0x71, 0x72, 0x73, 0x71, 0x72],
        "checkpoint epochs rotate and are reused across left/right refresh scopes"
    );
    let mut expected_cursors = Vec::new();
    for (index, chain) in rotated_chains.iter().enumerate() {
        let source_name = lane_names[index].to_owned();
        expected_cursors.push((source_name.clone(), scopes[index], None));
        expected_cursors.extend(chain.iter().cloned().map(|cursor| {
            (source_name.clone(), scopes[index], Some(cursor))
        }));
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "rotated sparse checkpoints resume only their paired source scope"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}

#[test]
fn paired_snapshots_keep_values_across_sparse_escalation_fold_chains() {
    // The brief leaves “escalation” undefined; pin it to one additional
    // complete window-and-sum stage per successive paired refresh.
    let checkpoint_epochs = [
        vec![
            vec![0x81],
            vec![0x81, 0x00],
            vec![0x81, 0x00, 0x00],
            vec![0x81, 0x00, 0x00, 0x00],
        ],
        vec![
            vec![0x82],
            vec![0x82, 0x01],
            vec![0x82, 0x01, 0x00],
            vec![0x82, 0x01, 0x00, 0x00],
        ],
        vec![
            vec![0x83],
            vec![0x83, 0x02],
            vec![0x83, 0x02, 0x00],
            vec![0x83, 0x02, 0x00, 0x00],
        ],
    ];
    let restore = |groups: &[&[i64]], tokens: &[Vec<u8>]| {
        assert_eq!(groups.len(), tokens.len() + 1);
        groups
            .iter()
            .enumerate()
            .map(|(index, values)| {
                let after = index.checked_sub(1).map(|previous| tokens[previous].clone());
                let next = tokens.get(index).cloned();
                (after, page(values, next))
            })
            .collect::<CursorRestore>()
    };
    let mut restores = Vec::new();
    let mut lane_names = Vec::new();
    let mut rotated_chains = Vec::new();
    let mut add_restore = |source_name: &'static str, groups: &[&[i64]], epoch: usize| {
        let tokens = checkpoint_epochs[epoch][..groups.len() - 1].to_vec();
        restores.push((source_name, restore(groups, &tokens)));
        lane_names.push(source_name);
        rotated_chains.push(tokens);
    };
    add_restore("View.Left", &[&[1, 2, 3], &[4, 5], &[6, 7, 8], &[9, 10]], 0);
    add_restore("View.Right", &[&[1, 2], &[3, 4], &[5, 6], &[7, 8], &[9, 10]], 1);
    add_restore("View.Left", &[&[11, 12, 13], &[14, 15, 16, 17], &[18, 19, 20]], 2);
    add_restore("View.Right", &[&[11, 12, 13, 14], &[15], &[16, 17], &[18, 19, 20]], 0);
    add_restore("View.Left", &[&[-9, -8], &[-7, -6], &[-5, -4], &[-3, -2], &[-1, 0]], 1);
    add_restore("View.Right", &[&[-9, -8, -7], &[-6, -5, -4, -3], &[-2, -1, 0]], 2);
    add_restore("View.Left", &[&[101, 102, 103, 104], &[105], &[106, 107, 108], &[109, 110]], 0);
    add_restore("View.Right", &[&[101, 102], &[103, 104], &[105, 106], &[107, 108], &[109, 110]], 1);
    drop(add_restore);

    let mut source = PairedCursorRestoreSource::new(restores);
    let mut functions = paired_snapshot_sparse_escalation_functions();
    let mut snapshots = Vec::new();
    for (depth, left, right) in [(1, 40, 48), (2, 180, 192), (3, -80, -64), (4, 1680, 1696)] {
        functions.insert(
            "run".into(),
            PureFunction {
                parameters: Vec::new(),
                body: paired_snapshot_sparse_escalation_body(depth),
                environment: Environment::new(),
            },
        );
        snapshots.push(
            invoke_named_with_effects(
                "run",
                &functions,
                &Environment::new(),
                Limits::default(),
                &mut source,
            )
            .unwrap(),
        );
        assert_eq!(snapshots.last(), Some(&integer_pair(left, right)));
    }
    assert_eq!(snapshots[0], integer_pair(40, 48), "one fold stage computes (40, 48)");
    assert_eq!(snapshots[1], integer_pair(180, 192), "two fold stages compute (180, 192)");
    assert_eq!(snapshots[2], integer_pair(-80, -64), "three fold stages compute (-80, -64)");
    assert_eq!(snapshots[3], integer_pair(1680, 1696), "four fold stages compute (1680, 1696)");
    assert_eq!(source.lanes.len(), lane_names.len());
    let scopes = source
        .lanes
        .iter()
        .zip(lane_names.iter().copied())
        .map(|((source_name, scope, _), expected_name)| {
            assert_eq!(source_name, expected_name, "each escalation reads left then right");
            *scope
        })
        .collect::<Vec<_>>();
    for (index, scope) in scopes.iter().enumerate() {
        assert!(
            !scopes[..index].contains(scope),
            "paired fold escalation binds each captured snapshot to a fresh source scope: {scope:?}"
        );
    }
    assert_eq!(
        rotated_chains.iter().map(Vec::len).collect::<Vec<_>>(),
        [3, 4, 2, 3, 4, 2, 3, 4],
        "each sparse snapshot restores exactly its own number of checkpoint transitions"
    );
    assert_eq!(
        rotated_chains
            .iter()
            .map(|chain| chain[0][0])
            .collect::<Vec<_>>(),
        [0x81, 0x82, 0x83, 0x81, 0x82, 0x83, 0x81, 0x82],
        "checkpoint epochs rotate and recur across the paired escalation snapshots"
    );
    let mut expected_cursors = Vec::new();
    for (index, chain) in rotated_chains.iter().enumerate() {
        let source_name = lane_names[index].to_owned();
        expected_cursors.push((source_name.clone(), scopes[index], None));
        expected_cursors.extend(chain.iter().cloned().map(|cursor| {
            (source_name.clone(), scopes[index], Some(cursor))
        }));
    }
    assert_eq!(
        source.cursors,
        expected_cursors,
        "each fold depth consumes only the sparse cursors belonging to its captured source snapshot"
    );
    assert!(source.pending["View.Left"].is_empty());
    assert!(source.pending["View.Right"].is_empty());
}
