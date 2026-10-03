use std::collections::BTreeMap;

use orna_evaluator_v1::{
    AdmittedReplSession, Environment, Functions, Limits, PureFunction,
    evaluate_expression_with_functions,
};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn int(value: i64) -> Raw {
    Raw::Int(value.into())
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

fn pinned_functions() -> Functions {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_standard::reference_standard_sources_v1() {
        let parsed = orna_syntax_v1::parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let module = path
            .strip_prefix("std/")
            .and_then(|path| path.strip_suffix(".orna"))
            .expect("pinned std paths have a std/ prefix and .orna suffix")
            .replace('/', ".");
        for item in parsed.value.items {
            if let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration {
                let name = format!("std.{module}.{}", signature.name);
                assert!(
                    functions
                        .insert(
                            name.clone(),
                            PureFunction {
                                parameters: signature.parameters,
                                body,
                                environment: Environment::new(),
                            },
                        )
                        .is_none(),
                    "duplicate pinned standard function {name}"
                );
            }
        }
    }
    functions
}

#[test]
fn pinned_iterator_and_lazy_modules_are_snapshot_bound_and_importable() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/collection.orna"
                || path == "std/iterator.orna"
                || path == "std/lazy.orna"
        })
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/7hqga-iterator-lazy",
        sources.clone(),
    )
    .expect("iterator and lazy modules are captured in an immutable std snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("selected std module bytes match their captured snapshot");
    }
    for path in ["std/iterator.orna", "std/lazy.orna"] {
        let mut changed = sources
            .iter()
            .find(|(source_path, _)| source_path == path)
            .expect("selected module source")
            .1
            .clone();
        changed.push_str("\n// changed after snapshot capture\n");
        assert!(profile.verify_source(path, &changed).is_err());
    }

    let iterator_source = &sources
        .iter()
        .find(|(path, _)| path == "std/iterator.orna")
        .expect("the captured iterator module")
        .1;
    for declaration in [
        "pub fn chain<T>(left: Iterator<T>, right: Iterator<T>): Iterator<T>",
        "pub fn enumerate<T>(source: Iterator<T>): Iterator<(Int, T)>",
        "pub fn fold<T, U>(source: Iterator<T>, initial: U, combine: fn(U, T): U): U",
    ] {
        assert!(iterator_source.contains(declaration), "missing iterator export `{declaration}`");
    }
    for contract in [
        "Pulls every item from left before requesting an item from right.",
        "Enumeration starts at zero",
        "Folds a finite iterator from left to right",
    ] {
        assert!(iterator_source.contains(contract), "missing iterator contract `{contract}`");
    }
    let lazy_source = &sources
        .iter()
        .find(|(path, _)| path == "std/lazy.orna")
        .expect("the captured lazy module")
        .1;
    assert!(
        lazy_source.contains("pub fn from_pull<T>(")
            && lazy_source.contains("std.iterator.Iterator<T>"),
        "missing lazy pull-to-iterator export"
    );
    assert!(
        lazy_source.contains("callback runs only when that cursor is advanced"),
        "missing lazy pull evaluation boundary"
    );

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("selected iterator and lazy modules resolve in the captured source bundle");
    let mut session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured iterator/lazy source failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-iterator-lazy-s58ir.orna"))
        .unwrap_or_else(|error| panic!("iterator/lazy imports failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-lazy-s58ir.orna"))
        .unwrap_or_else(|error| panic!("lazy import failed: {}", error.code()));
}

#[test]
fn core_assertions_work_without_std_and_both_modules_remain_optional() {
    let mut session = AdmittedReplSession::new(Limits::default());
    session
        .submit(include_str!("fixtures/stdlib-core-assertions-without-iterator-lazy-s58ir.orna"))
        .unwrap_or_else(|error| panic!("core assertion declaration failed without std: {}", error.code()));
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-core-assertions-call-without-iterator-lazy-s58ir.orna"))
            .unwrap_or_else(|error| panic!("core assertion failed without std: {}", error.code())),
        Some(bool_value(true))
    );

    let mut without_iterator = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_iterator
            .submit(include_str!("fixtures/stdlib-iterator-without-snapshot-s58ir.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    let mut without_lazy = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_lazy
            .submit(include_str!("fixtures/stdlib-lazy-without-snapshot-s58ir.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}

#[test]
fn lazy_thunk_mapping_returns_the_computed_value() {
    assert_eq!(
        evaluate_expression_with_functions(
            include_str!("fixtures/stdlib-lazy-map-value-7hqga.orna"),
            &Environment::new(),
            &pinned_functions(),
            Limits::default(),
        ),
        Ok(canonical(int(6)))
    );
}
