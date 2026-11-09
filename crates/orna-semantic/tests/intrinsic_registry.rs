//! Owner-accepted typing changes for the intrinsic registry (decision D4).
//!
//! Stage S3 replaced the hand-written native signature tables with the
//! `orna-intrinsics` catalogue. Four root operations that the evaluator already
//! implemented become typeable, `project` stays unimplemented and rejected,
//! and a `std.collection` core spelling that the pinned source does not
//! re-export stays rejected until the std repo declares it (stage S4).

use orna_semantic::{
    Catalogue, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, Namespace, StandardDependencyProfile, Type,
    analyze, analyze_with_catalogue,
};

fn has(analysis: &orna_semantic::Analysis, code: &str) -> bool {
    analysis
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == code)
}

fn collection_catalogue() -> Catalogue {
    let source = include_str!("fixtures/collection-catalogue.orna");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/v1-collection",
        [("std/collection.orna".into(), source.into())],
    )
    .expect("collection profile");
    Catalogue::authoritative_core()
        .with_standard_sources(&profile, [("std/collection.orna".into(), source.into())])
        .expect("verified collection catalogue")
}

fn module<'a>(
    analysis: &'a orna_semantic::Analysis,
    name: &str,
) -> &'a orna_semantic::ModuleHeader {
    analysis
        .modules
        .get(&Namespace(vec![name.into()]))
        .expect("analyzed module is present")
}

fn result_of(module: &orna_semantic::ModuleHeader, name: &str) -> Type {
    let Type::Function { result, .. } = &module.symbols.get(name).expect("fixture function").ty
    else {
        panic!("{name} is not a function");
    };
    result.as_ref().clone()
}

/// D4: `last` and `pairs` gain root typing, for a direct call and a pipeline.
#[test]
fn last_and_pairs_typecheck_at_root() {
    let analysis = analyze(&[ModuleInput::new(
        "root-collections.orna",
        "pub fn tail(values: [Int]) = values | last();\n\
         pub fn tail_direct(values: [Int]) = last(values);\n\
         pub fn paired(values: [Int]) = values | pairs();\n",
    )]);
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = module(&analysis, "root-collections");
    for name in ["tail", "tail_direct"] {
        assert_eq!(result_of(module, name), Type::Optional(Box::new(Type::Int)));
    }
    assert_eq!(
        result_of(module, "paired"),
        Type::List(Box::new(Type::Tuple(vec![Type::Int, Type::Int])))
    );
}

/// D4: `rank` gains root typing for both collection overloads.
#[test]
fn rank_typechecks_at_root_for_lists_and_relations() {
    let analysis = analyze(&[ModuleInput::new(
        "root-rank.orna",
        "table Note(id: Int) { value: Int, }\n\
         pub fn list_ranked(values: [Int]) = values | rank(value => value);\n\
         pub fn relation_ranked(rows: Relation<Note>) = rows | rank(note => note.id);\n",
    )]);
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = module(&analysis, "root-rank");
    assert_eq!(
        result_of(module, "list_ranked"),
        Type::List(Box::new(Type::Tuple(vec![Type::Int, Type::Int])))
    );
    assert_eq!(
        result_of(module, "relation_ranked"),
        Type::Relation(Box::new(Type::Tuple(vec![
            Type::Named("root-rank.Note".into()),
            Type::Int,
        ])))
    );
}

/// D4: `bucket_by` gains root typing for both collection overloads.
#[test]
fn bucket_by_typechecks_at_root_for_lists_and_relations() {
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "root-bucket.orna",
            "table Reading(id: Int) { time: Instant, }\n\
             pub fn list_bucket(values: [Instant]) = values | bucket_by(1.day, zone: Europe.London);\n\
             pub fn relation_bucket(rows: Relation<Reading>) = rows | bucket_by(1.day, zone: Europe.London);\n",
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = module(&analysis, "root-bucket");
    assert_eq!(
        result_of(module, "list_bucket"),
        Type::List(Box::new(Type::List(Box::new(Type::Instant))))
    );
    assert_eq!(
        result_of(module, "relation_bucket"),
        Type::Relation(Box::new(Type::Named("root-bucket.Reading".into())))
    );
}

/// D4: `project` still has no implementation and stays rejected.
#[test]
fn project_is_rejected_at_root() {
    let analysis = analyze(&[ModuleInput::new(
        "project.orna",
        "table Reading(id: Int) { value: Int, }\n\
         fn project(reading: Reading) = [reading.value];\n\
         fn bad() = Reading | flat_map(project);\n\
         fn bad_direct(rows: Relation<Reading>) = project(rows, value => value);\n",
    )]);
    assert!(!analysis.is_ok(), "project must not be admitted");
    assert!(
        has(&analysis, DIAG_TYPE) || has(&analysis, DIAG_UNRESOLVED),
        "{:?}",
        analysis.diagnostics
    );
}

/// D4: a `std.collection` core spelling is admitted only when the captured
/// module declares it. The pinned source re-exports no `count`, so that
/// spelling stays rejected until the std repo adds the re-export in stage S4.
#[test]
fn undeclared_core_collection_spellings_are_rejected() {
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            "use std.collection;\n\
             pub fn size(values: [Int]): Int = std.collection.count(values);\n\
             pub fn mapped(values: [Int]) = std.collection.map(transform: value => value + 1, rows: values);\n",
        )],
        &collection_catalogue(),
    );
    assert!(
        has(&analysis, DIAG_UNRESOLVED),
        "std.collection.count must stay rejected: {:?}",
        analysis.diagnostics
    );
    let module = module(&analysis, "consumer");
    assert_eq!(
        result_of(module, "mapped"),
        Type::List(Box::new(Type::Int)),
        "a declared re-export keeps working: {:?}",
        analysis.diagnostics
    );
}
