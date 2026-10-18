use orna_semantic_v1::{analyze, analyze_with_catalogue, Catalogue, ModuleInput, Namespace, StandardDependencyProfile, Type};

fn module(path: &str, source: &str) -> ModuleInput {
    ModuleInput::new(path, source)
}

/// ORNA-NS-002 and ORNA-NS-003: non-main filenames append their stem; root main defines root namespace.
#[test]
fn filename_stem_and_root_main_determine_namespaces() {
    let result = analyze(&[module("main.orna", "fn root(): Int = 1;"), module("lib.orna", "fn item(): Int = 2;"), module("sub/main.orna", "fn child(): Int = 3;")]);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics.iter().map(|diagnostic| diagnostic.code()).collect::<Vec<_>>());
    assert!(result.modules.contains_key(&Namespace(vec![])));
    assert!(result.modules.contains_key(&Namespace(vec!["lib".into()])));
    assert!(result.modules.contains_key(&Namespace(vec!["sub".into()])));
}

/// ORNA-NS-006 and ORNA-NS-007: sys is protected; std is optional, reserved when present, and replaceable.
#[test]
fn sys_is_protected_and_std_is_optional_replaceable_and_reserved() {
    let sys = analyze(&[module("sys/main.orna", "fn replacement(): Int = 1;")]);
    assert!(sys.diagnostics.iter().any(|diagnostic| diagnostic.code() == orna_semantic_v1::DIAG_RESERVED));

    let std_shadow = analyze(&[module("std/main.orna", "pub fn answer(): Int = 42;")]);
    assert!(std_shadow.diagnostics.iter().any(|diagnostic| diagnostic.code() == orna_semantic_v1::DIAG_RESERVED));

    let source = module("main.orna", "use std as s; fn run(): Int = s.answer();");
    assert!(!analyze(&[source.clone()]).is_ok(), "std is absent from the default catalogue");

    let standard_source = "pub fn answer(): Int = 42;";
    let profile = StandardDependencyProfile::from_sources("std/replacement-v1", [("std/main.orna".into(), standard_source.into())]).expect("profile");
    let std = Catalogue::from_standard_sources(&profile, [("std/main.orna".into(), standard_source.into())]).expect("replaceable std catalogue");
    assert!(std.standard_dependency_profile().is_some());
    let replacement = analyze_with_catalogue(&[source], &std);
    assert!(replacement.diagnostics.is_empty(), "{:?}", replacement.diagnostics.iter().map(|diagnostic| diagnostic.code()).collect::<Vec<_>>());
}

/// ORNA-IMPORT-002: an alias scopes the imported namespace under the requested name.
#[test]
fn aliased_import_resolves_qualified_public_name() {
    let result = analyze(&[module("main.orna", "use lib as x; fn run(): Int = x.answer();"), module("lib.orna", "pub fn answer(): Int = 42;")]);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics.iter().map(|diagnostic| diagnostic.code()).collect::<Vec<_>>());
}

/// ORNA-IMPORT-003: glob imports expose public names without overriding local declarations.
#[test]
fn glob_import_exposes_public_name_to_lexical_scope() {
    let result = analyze(&[module("main.orna", "use lib.*; fn run(): Int = answer();"), module("lib.orna", "pub fn answer(): Int = 42;")]);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics.iter().map(|diagnostic| diagnostic.code()).collect::<Vec<_>>());
}

/// ORNA-IMPORT-005: resolving an ordinary module does not evaluate an uncalled function body.
#[test]
fn importing_module_does_not_evaluate_unreferenced_function_bodies() {
    let source = module("main.orna", "use lib as x; fn run(): Int = x.answer();");
    let imported = module(
        "lib.orna",
        "pub fn answer(): Int = 42; pub fn fail_if_evaluated(): Int = 1 / 0;",
    );
    let result = analyze_with_catalogue(&[source, imported], &Catalogue::authoritative_fixture());
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics.iter().map(|diagnostic| (diagnostic.code(), diagnostic.message())).collect::<Vec<_>>());
    let library = &result.modules[&Namespace(vec!["lib".into()])];
    assert!(library.exports.contains_key("fail_if_evaluated"));
}

/// ORNA-PATTERN-001: record-pattern shorthand resolves its name as a local semantic binding.
#[test]
fn record_pattern_shorthand_resolves_as_semantic_binding() {
    let result = analyze(&[module(
        "main.orna",
        r#"pub fn dispatch({ related }: { related: fn(): Bool }): Bool = related();"#,
    )]);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics.iter().map(|diagnostic| (diagnostic.code(), diagnostic.message())).collect::<Vec<_>>());
}

/// ORNA-IMPORT-007: multiple wildcard providers make an unresolved name ambiguous regardless of order.
#[test]
fn conflicting_wildcard_imports_are_ambiguous() {
    let main = module("main.orna", "use a.*; use b.*; fn run(): Int = answer();");
    let result = analyze(&[main, module("a.orna", "pub fn answer(): Int = 1;"), module("b.orna", "pub fn answer(): Int = 2;")]);
    assert!(result.diagnostics.iter().any(|diagnostic| diagnostic.code() == orna_semantic_v1::DIAG_AMBIGUOUS));
    let reversed = analyze(&[
        module("main.orna", "use b.*; use a.*; fn run(): Int = answer();"),
        module("a.orna", "pub fn answer(): Int = 1;"),
        module("b.orna", "pub fn answer(): Int = 2;"),
    ]);
    assert!(reversed.diagnostics.iter().any(|diagnostic| diagnostic.code() == orna_semantic_v1::DIAG_AMBIGUOUS));
}

/// ORNA-LIT-003 and ORNA-LIT-007: explicit decimal currency construction remains exact Money, without Float inference.
#[test]
fn exact_decimal_currency_constructor_has_money_type() {
    let analysis = analyze(&[ModuleInput::new(
        "exact-money.orna",
        include_str!("fixtures/traceability-money-exact-decimal.orna"),
    )]);
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics.iter().map(|diagnostic| diagnostic.code()).collect::<Vec<_>>());
    let amount = &analysis.modules.values().next().unwrap().exports["amount"];
    assert!(matches!(
        &amount.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "Money".into(),
                arguments: vec![Type::Named("exact-money.GBP".into())],
            }
    ));
    let implicit_source = include_str!("fixtures/traceability-money-exact-decimal.orna").replace("12.34.GBP", "12.34");
    let implicit_money = analyze(&[ModuleInput::new("implicit_money.orna", implicit_source)]);
    assert!(implicit_money.diagnostics.iter().any(|diagnostic| diagnostic.code() == orna_semantic_v1::DIAG_TYPE), "a bare decimal must not implicitly construct Money");
}

/// ORNA-LIT-007: decimal suffix construction rejects a nominal type without a Currency implementation.
#[test]
fn currency_suffix_rejects_nominal_without_currency_implementation() {
    let non_currency_source = include_str!("fixtures/traceability-money-exact-decimal.orna")
        .replace("Currency", "NotCurrency")
        .replace("GBP", "USD");
    let analysis = analyze(&[ModuleInput::new("not-currency.orna", non_currency_source)]);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| diagnostic.code() == orna_semantic_v1::DIAG_TYPE),
        "decimal suffix construction must require a Currency implementation: {:?}",
        analysis.diagnostics.iter().map(|diagnostic| (diagnostic.code(), diagnostic.message())).collect::<Vec<_>>(),
    );
}
