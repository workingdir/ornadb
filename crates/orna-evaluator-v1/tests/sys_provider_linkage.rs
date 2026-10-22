#![cfg(feature = "system-host-bindings")]

//! Cross-checks `orna_sys_v1::system_provider_linkage()` against the
//! evaluator's native `sys` dispatch. Semantic admission rejects every portable
//! member the linkage omits with `sys.version.incompatible`, so a member the
//! evaluator executes natively must be linked to it, and a member linked to a
//! host provider must reach the host effect boundary.

use std::collections::BTreeSet;
use std::sync::Arc;

use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, ExplainCatalogue, Functions, Limits,
    PureFunction, RelationPage, StepBudget, invoke_named_with_effects,
};
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::{
    ObjectRef, SnapshotRef, SystemProviderHost, SystemSurfaceMember, system_api_json,
    system_dispatch_table, system_provider_linkage,
};
use orna_syntax_v1::{Expr, parse_expression};

/// A host catalogue that pins one relation source for `sys.explain(Query)`.
#[derive(Debug)]
struct PinnedSource(&'static str);

impl ExplainCatalogue for PinnedSource {
    fn resolve_source(&self, source: &str) -> Option<(SnapshotRef, ObjectRef)> {
        (source == self.0).then(|| {
            (
                SnapshotRef::descriptive("snapshot-linkage"),
                ObjectRef::descriptive("object-linkage"),
            )
        })
    }
}

/// Records what reaches the host effect boundary and answers nothing, so every
/// observation comes from the evaluator alone.
#[derive(Default)]
struct RecordingHost {
    forwarded: Vec<String>,
    relation_sources: BTreeSet<String>,
    catalogue: Option<Arc<PinnedSource>>,
}

impl EffectHandler for RecordingHost {
    fn handle(
        &mut self,
        callee: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.forwarded
            .push(dotted_path(callee).unwrap_or_else(|| format!("{callee:?}")));
        Ok(None)
    }

    fn validate_relation_source(&mut self, source: &str) -> Result<(), EvaluationError> {
        self.relation_sources.insert(source.to_owned());
        Ok(())
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        _: Option<&[u8]>,
        _: usize,
        _: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        self.relation_sources.insert(source.to_owned());
        Ok(Some(RelationPage {
            rows: Vec::new(),
            next: None,
        }))
    }

    fn explain_catalogue(&self) -> Option<&dyn ExplainCatalogue> {
        self.catalogue
            .as_deref()
            .map(|catalogue| catalogue as &dyn ExplainCatalogue)
    }
}

fn dotted_path(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{name}", dotted_path(base)?)),
        _ => None,
    }
}

/// Evaluates `expression` as the body of a zero-argument function.
fn evaluate(
    expression: &str,
    host: &mut RecordingHost,
) -> Result<CanonicalValue, EvaluationError> {
    let parsed = parse_expression(expression);
    assert!(parsed.is_ok(), "{expression}: {:?}", parsed.diagnostics);
    let functions = Functions::from([(
        "probe".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment: Environment::new(),
        },
    )]);
    invoke_named_with_effects(
        "probe",
        &functions,
        &Environment::new(),
        Limits::default(),
        host,
    )
}

/// The callable name of an `api/sys.json` overload label.
fn callable_name(label: &str) -> &str {
    label.find(['(', '<']).map_or(label, |end| &label[..end])
}

fn linked_callables(host: SystemProviderHost) -> BTreeSet<&'static str> {
    system_provider_linkage()
        .links()
        .filter(|link| link.host == host)
        .filter_map(|link| match link.member {
            SystemSurfaceMember::Callable(label) => Some(callable_name(label)),
            _ => None,
        })
        .collect()
}

struct PublishedPaths {
    relations: BTreeSet<String>,
    singletons: BTreeSet<String>,
}

fn published_paths() -> PublishedPaths {
    let document: serde_json::Value =
        serde_json::from_str(&system_api_json()).expect("generated sys API is JSON");
    let names = |section: &str, keys: &[&str]| {
        document[section]
            .as_array()
            .expect("published API section is an array")
            .iter()
            .flat_map(|entry| {
                keys.iter().map(move |key| {
                    entry[*key]
                        .as_str()
                        .expect("published API path is a string")
                        .to_owned()
                })
            })
            .collect::<BTreeSet<_>>()
    };
    PublishedPaths {
        relations: names("relations", &["name", "grouped_handle"]),
        singletons: names("singletons", &["name"]),
    }
}

#[test]
fn native_sys_callables_are_exactly_the_evaluator_links() {
    let mut intercepted = BTreeSet::new();
    let mut forwarded = BTreeSet::new();
    let names = system_dispatch_table()
        .operations()
        .map(|operation| callable_name(operation.id.as_str()))
        .collect::<BTreeSet<_>>();
    for name in names {
        let mut host = RecordingHost::default();
        let outcome = evaluate(&format!("{name}()"), &mut host);
        match host.forwarded.as_slice() {
            [] => {
                intercepted.insert(name);
            }
            [callee] if callee == name => {
                assert!(outcome.is_err(), "{name}() has no provider in this probe");
                forwarded.insert(name);
            }
            other => panic!("{name}() reached the host boundary as {other:?}"),
        }
    }

    assert_eq!(
        intercepted,
        linked_callables(SystemProviderHost::Evaluator),
        "every sys callable the evaluator executes natively must be linked to \
         SystemProviderHost::Evaluator, and nothing else"
    );
    for host in [
        SystemProviderHost::HostOperationRegistry,
        SystemProviderHost::LiveAdministration,
    ] {
        let hosted = linked_callables(host);
        assert!(
            hosted.is_subset(&forwarded),
            "{host:?} callables must reach the host effect boundary: {hosted:?}"
        );
    }
}

#[test]
fn linked_explain_query_is_dispatched_natively_through_the_host_catalogue() {
    let linkage = system_provider_linkage();
    assert!(linkage.links().any(|link| {
        link.member == SystemSurfaceMember::Callable("sys.explain(Query)")
            && link.host == SystemProviderHost::Evaluator
    }));
    for unlinked in ["sys.explain(FunctionRef)", "sys.explain(Diagnostic)"] {
        assert!(!linkage.is_linked(SystemSurfaceMember::Callable(unlinked)));
    }

    let mut pinned = RecordingHost {
        catalogue: Some(Arc::new(PinnedSource("sys.Storage"))),
        ..RecordingHost::default()
    };
    evaluate("sys.Storage | sys.explain()", &mut pinned).unwrap_or_else(|error| {
        panic!("explain lowers a pinned query: {}", error.code())
    });
    assert!(pinned.forwarded.is_empty(), "explain is native dispatch");
    assert!(pinned.relation_sources.is_empty(), "explain never scans rows");

    let mut uncatalogued = RecordingHost::default();
    let error = evaluate("sys.Storage | sys.explain()", &mut uncatalogued)
        .expect_err("a host without a catalogue cannot pin the query source");
    assert_eq!(error.code(), "sys.version.incompatible");
    assert!(uncatalogued.forwarded.is_empty(), "explain is native dispatch");
}

#[test]
fn evaluator_relation_sources_are_exactly_the_relation_links() {
    let mut recognised = BTreeSet::new();
    for path in published_paths().relations {
        let mut host = RecordingHost::default();
        let outcome = evaluate(&format!("{path} | count()"), &mut host);
        if host.relation_sources.is_empty() {
            assert!(outcome.is_err(), "{path} counted rows without a relation source");
            continue;
        }
        assert_eq!(host.relation_sources, BTreeSet::from([path.clone()]));
        outcome.unwrap_or_else(|error| {
            panic!("{path} is a relation source but did not count: {}", error.code())
        });
        recognised.insert(path);
    }

    let linked = system_provider_linkage()
        .links()
        .filter_map(|link| match link.member {
            SystemSurfaceMember::Relation(path) => Some(path.to_owned()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(recognised, linked);
}

#[test]
fn singleton_and_historical_views_are_linked_only_when_evaluated_natively() {
    let linkage = system_provider_linkage();
    let published = published_paths();
    for singleton in &published.singletons {
        let mut host = RecordingHost::default();
        let evaluated = evaluate(singleton, &mut host).is_ok();
        assert_eq!(
            evaluated,
            linkage.is_linked(SystemSurfaceMember::Singleton(singleton)),
            "{singleton}"
        );
    }
    for view in published.relations.iter().chain(&published.singletons) {
        let mut host = RecordingHost::default();
        let evaluated = evaluate(&format!("{view}.as_of(null)"), &mut host).is_ok();
        assert_eq!(
            evaluated,
            linkage.is_linked(SystemSurfaceMember::HistoricalView(view)),
            "{view}.as_of"
        );
    }
}
