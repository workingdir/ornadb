use std::collections::BTreeSet;

use orna_semantic_v1::{
    analyze, Analysis, EffectSummary, ModuleHeader, ModuleInput, Namespace,
    DIAG_ASSERTION_EFFECT, DIAG_ASSERTION_ONE_TABLE, DIAG_ASSERTION_SCOPE,
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

fn plan<'a>(analysis: &'a Analysis, name: &str) -> &'a orna_semantic_v1::AssertionPlan {
    let plans = analysis
        .assertions
        .get(&Namespace(vec![name.to_owned()]))
        .unwrap_or_else(|| panic!("missing assertion plans for {name:?}"));
    assert_eq!(plans.len(), 1, "unexpected assertion plans: {plans:?}");
    &plans[0]
}

fn assert_shadowed_plan(analysis: &Analysis, expected: BTreeSet<String>) {
    let expected_diagnostic = if expected.is_empty() {
        DIAG_ASSERTION_SCOPE
    } else {
        DIAG_ASSERTION_ONE_TABLE
    };
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == expected_diagnostic),
        "shadowed helper should retain the expected assertion diagnostic: {:?}",
        analysis.diagnostics
    );
    assert_eq!(plan(analysis, "consumer").dependencies, expected);
}

#[test]
fn imported_helper_shadowed_by_callback_local_does_not_expand_imported_tables() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", TABLE_HELPER),
        ModuleInput::new(
            "consumer.orna",
            r#"
                use checks.{related};
                pub table User(id: Uuid) { name: Str, }
                pub table Account(id: Uuid) { user_id: Uuid, }

                pub fn callback(): Bool = every(User, user => {
                    let related = () => true;
                    related()
                });

                assert callback();
            "#,
        ),
    ]);

    assert_shadowed_plan(&analysis, BTreeSet::from(["User".to_owned()]));
}

#[test]
fn imported_helper_shadowed_by_local_binding_does_not_expand_imported_tables() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", TABLE_HELPER),
        ModuleInput::new(
            "consumer.orna",
            r#"
                use checks.{related};
                pub table User(id: Uuid) { name: Str, }
                pub table Account(id: Uuid) { user_id: Uuid, }

                pub fn local(): Bool {
                    let related = () => true;
                    return related();
                }

                assert local();
            "#,
        ),
    ]);

    assert_shadowed_plan(&analysis, BTreeSet::new());
}

#[test]
fn imported_helper_shadowed_by_parameter_does_not_expand_imported_tables() {
    let analysis = analyze(&[
        ModuleInput::new("checks.orna", TABLE_HELPER),
        ModuleInput::new(
            "consumer.orna",
            r#"
                use checks.{related};
                pub table User(id: Uuid) { name: Str, }
                pub table Account(id: Uuid) { user_id: Uuid, }

                pub fn parameter(related: fn(Int): Bool): Bool = related(0);

                assert parameter(_ => true);
            "#,
        ),
    ]);

    assert_shadowed_plan(&analysis, BTreeSet::new());
}

#[test]
fn same_module_helper_call_keeps_exact_transitive_table_dependencies() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"
            pub table User(id: Uuid) { name: Str, }
            pub table Account(id: Uuid) { user_id: Uuid, }

            pub fn related(): Bool =
                every(User, user =>
                    exists(Account, account => account.user_id == user.id)
                );

            assert related();
        "#,
    )]);

    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DIAG_ASSERTION_EFFECT)
            .count(),
        1,
        "same-module helper effects must remain visible: {:?}",
        analysis.diagnostics
    );
    assert_eq!(
        plan(&analysis, "consumer").dependencies,
        BTreeSet::from(["Account".to_owned(), "User".to_owned()])
    );
}

#[test]
fn imported_helper_call_keeps_exact_transitive_table_dependencies() {
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
        "imported helper effects must remain visible: {:?}",
        analysis.diagnostics
    );
    assert_eq!(
        plan(&analysis, "consumer").dependencies,
        BTreeSet::from(["User".to_owned(), "Account".to_owned()])
    );
    assert_eq!(
        module(&analysis, "checks")
            .exports
            .get("related")
            .expect("imported helper export")
            .effects,
        EffectSummary {
            effects: BTreeSet::from(["database read".to_owned()]),
            may_fail: true,
        }
    );
}

#[test]
fn direct_return_stops_dependency_walk_before_later_every_expression() {
    let analysis = analyze(&[ModuleInput::new(
        "consumer.orna",
        r#"
            pub table User(id: Uuid) { name: Str, }
            pub table Account(id: Uuid) { user_id: Uuid, }

            pub fn helper(): Bool {
                return true;
                every(User, user => true);
            }

            assert helper();
        "#,
    )]);

    assert_eq!(
        analysis.diagnostics.len(),
        1,
        "unreachable post-return expression must not be checked as a dependency: {:?}",
        analysis.diagnostics
    );
    assert_eq!(analysis.diagnostics[0].code(), DIAG_ASSERTION_SCOPE);
    assert!(plan(&analysis, "consumer").dependencies.is_empty());
}
