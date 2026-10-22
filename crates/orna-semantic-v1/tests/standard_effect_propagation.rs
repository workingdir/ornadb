use orna_semantic_v1::{Catalogue, ModuleInput, StandardDependencyProfile, analyze_with_catalogue};

fn catalogue_with_wrappers() -> Catalogue {
    let sources = vec![
        (
            "std/wrap_net.orna".to_owned(),
            "pub fn fetch(url: Str): Str = std.net.http.get(url);\n".to_owned(),
        ),
        (
            "std/wrap_relay.orna".to_owned(),
            "use std.wrap_net as net;\npub fn relay(url: Str): Str = net.fetch(url);\n".to_owned(),
        ),
        (
            "std/wrap_dice.orna".to_owned(),
            "pub fn coin(): Int = sys.random.bit();\n".to_owned(),
        ),
        (
            "std/net/pure.orna".to_owned(),
            "pub fn identity(url: Str): Str = url;\n".to_owned(),
        ),
        (
            "std/io/fs/pure.orna".to_owned(),
            "pub fn passthrough(path: Str): Str = path;\n".to_owned(),
        ),
    ];
    let profile = StandardDependencyProfile::from_sources("orna.std/v1-wrappers", sources.clone())
        .expect("wrapper dependency profile");
    Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources)
        .expect("verified wrapper catalogue")
}

/// Returns the consumer function's `may_fail`, its effect names, and the
/// `code: message` of each analysis diagnostic so a failure shows the cause.
fn consumer_effects(source: &str, function: &str) -> (bool, Vec<String>, Vec<String>) {
    let result = analyze_with_catalogue(
        &[ModuleInput::new("consumer.orna", source)],
        &catalogue_with_wrappers(),
    );
    let diagnostics = result
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
        .collect();
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "consumer")
        .expect("consumer module");
    let symbol = module
        .symbols
        .get(function)
        .expect("consumer function summary");
    (
        symbol.effects.may_fail,
        symbol.effects.effects.iter().cloned().collect(),
        diagnostics,
    )
}

#[test]
fn std_wrapper_calling_std_net_propagates_network_through_module_alias() {
    let (may_fail, effects, diagnostics) = consumer_effects(
        "use std.wrap_relay.{relay};\npub fn outer(url: Str): Str = relay(url);\n",
        "outer",
    );

    assert!(
        may_fail,
        "effects: {effects:?}; diagnostics: {diagnostics:?}"
    );
    assert_eq!(effects, vec!["network".to_owned()]);
}

#[test]
fn std_wrapper_calling_sys_random_propagates_randomness() {
    let (may_fail, effects, diagnostics) =
        consumer_effects("use std.wrap_dice.{coin};\npub fn flip(): Int = coin();\n", "flip");

    assert!(
        may_fail,
        "effects: {effects:?}; diagnostics: {diagnostics:?}"
    );
    assert_eq!(effects, vec!["randomness".to_owned()]);
}

#[test]
fn pure_parameter_reference_in_std_net_module_has_no_effects() {
    let (may_fail, effects, diagnostics) = consumer_effects(
        "use std.net.pure as p;\npub fn outer(u: Str): Str = p.identity(u);\n",
        "outer",
    );

    assert!(
        !may_fail && effects.is_empty(),
        "effects: {effects:?}; diagnostics: {diagnostics:?}"
    );
}

#[test]
fn pure_parameter_reference_in_std_io_fs_module_has_no_effects() {
    let (may_fail, effects, diagnostics) = consumer_effects(
        "use std.io.fs.pure as p;\npub fn outer(u: Str): Str = p.passthrough(u);\n",
        "outer",
    );

    assert!(
        !may_fail && effects.is_empty(),
        "effects: {effects:?}; diagnostics: {diagnostics:?}"
    );
}
