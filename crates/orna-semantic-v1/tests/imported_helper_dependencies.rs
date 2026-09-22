use std::collections::BTreeSet;

use orna_semantic_v1::{
    analyze, Analysis, EffectSummary, ModuleHeader, ModuleInput, Namespace, DIAG_ASSERTION_EFFECT,
    DIAG_ASSERTION_SCOPE, DIAG_IMPORT,
};

const TABLE_HELPER: &str = r#"
    pub table User(id: Uuid) { name: Str, }
    pub table Account(id: Uuid) { user_id: Uuid, }

    pub fn related(): Bool =
        every(User, user =>
            exists(Account, account => account.user_id == user.id)
        );
"#;

fn module<'a>(analysis: &'a Analysis, name: &str) -> &'a ModuleHeader {
    analysis
        .modules
        .get(&Namespace(vec![name.to_owned()]))
        .unwrap_or_else(|| panic!("missing module {name:?}"))
}

fn consumer_plan(analysis: &Analysis) -> &orna_semantic_v1::AssertionPlan {
    let plans = analysis
        .assertions
        .get(&Namespace(vec!["consumer".to_owned()]))
        .expect("consumer assertion plans");
    assert_eq!(plans.len(), 1, "unexpected consumer plans: {plans:?}");
    &plans[0]
}

fn assert_database_helper_summary(analysis: &Analysis) {
    let helper = module(analysis, "checks")
        .exports
        .get("related")
        .expect("exported imported helper");
    assert_eq!(
        helper.effects,
        EffectSummary {
            effects: BTreeSet::from(["database read".to_owned()]),
            may_fail: true,
        }
    );
}

#[test]
fn explicit_imported_helper_assertion_keeps_transitive_dependencies_and_effects() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", TABLE_HELPER),
        ModuleInput::new(
            "consumer.orna",
            "use checks.{related}; assert related();",
        ),
    ]);

    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DIAG_ASSERTION_EFFECT)
            .count(),
        1,
        "the imported helper's database-read/failure summary must remain visible: {:?}",
        analysis.diagnostics
    );
    assert_database_helper_summary(&analysis);
    let plan = consumer_plan(&analysis);
    assert_eq!(
        plan.dependencies,
        BTreeSet::from(["User".to_owned(), "Account".to_owned()])
    );
    assert_eq!(
        plan.effects,
        EffectSummary {
            effects: BTreeSet::from(["database read".to_owned()]),
            may_fail: true,
        }
    );
}

#[test]
fn aliased_imported_helper_assertion_keeps_transitive_dependencies() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", TABLE_HELPER),
        ModuleInput::new(
            "consumer.orna",
            "use checks as c; assert c.related();",
        ),
    ]);

    assert_database_helper_summary(&analysis);
    let plan = consumer_plan(&analysis);
    assert_eq!(
        plan.dependencies,
        BTreeSet::from(["User".to_owned(), "Account".to_owned()])
    );
    assert_eq!(plan.effects.may_fail, true);
    assert_eq!(
        plan.effects.effects,
        BTreeSet::from(["database read".to_owned()])
    );
}

#[test]
fn wildcard_imported_helper_assertion_keeps_transitive_dependencies() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", TABLE_HELPER),
        ModuleInput::new("consumer.orna", "use checks.*; assert related();"),
    ]);

    assert_database_helper_summary(&analysis);
    assert_eq!(
        consumer_plan(&analysis).dependencies,
        BTreeSet::from(["User".to_owned(), "Account".to_owned()])
    );
}

#[test]
fn private_named_import_reports_import_diagnostic_without_unresolved_name_noise() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", "fn hidden(): Bool = true;"),
        ModuleInput::new(
            "consumer.orna",
            "use checks.{hidden}; pub fn ok(): Bool = true;",
        ),
    ]);

    assert_eq!(analysis.diagnostics.len(), 1, "unexpected diagnostics: {:?}", analysis.diagnostics);
    assert_eq!(analysis.diagnostics[0].code(), DIAG_IMPORT);
    assert_eq!(
        analysis.diagnostics[0].message(),
        "named import is unavailable or private"
    );
}

#[test]
fn table_free_imported_helper_reports_scope_diagnostic_and_empty_plan() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", "pub fn always(): Bool = true;"),
        ModuleInput::new(
            "consumer.orna",
            "use checks.{always}; assert always();",
        ),
    ]);

    assert_eq!(analysis.diagnostics.len(), 1, "unexpected diagnostics: {:?}", analysis.diagnostics);
    assert_eq!(analysis.diagnostics[0].code(), DIAG_ASSERTION_SCOPE);
    assert_eq!(
        analysis.diagnostics[0].message(),
        "module assertions must depend on at least two distinct tables"
    );
    let plan = consumer_plan(&analysis);
    assert!(plan.dependencies.is_empty());
    assert_eq!(plan.effects, EffectSummary::default());
}
