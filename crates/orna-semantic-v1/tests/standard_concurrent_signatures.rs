//! `std.concurrent` catalogue signatures (ORNA-CONCUR-002, spec 09-standard-library.md:128):
//! `sleep(duration)` exists, `parallel(callbacks)` yields the declared list of
//! callback results, and literal callback lists with mismatching result types
//! are rejected.

use orna_semantic_v1::{Analysis, Catalogue, DIAG_TYPE, EffectKind, ModuleInput, analyze_with_catalogue};

fn analysis(source: &str) -> Analysis {
    analyze_with_catalogue(
        &[ModuleInput::new("concurrency.orna", source)],
        &Catalogue::authoritative_core(),
    )
}

fn diagnostics(analysis: &Analysis) -> Vec<(&str, &str)> {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
        .collect()
}

#[test]
fn sleep_accepts_a_duration_and_returns_null() {
    let analysis = analysis(
        r#"
use std.concurrent as concurrent;
pub fn pause(): Null = concurrent.sleep(1.second);
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}

/// ORNA-CONCUR-002 / effect classification: `sleep` is the clock/waiting host
/// effect (09-standard-library.md:128; effects.rs host_effect). The catalogue
/// entry is a type signature with no effect field, so the effect is asserted on
/// the analysed symbol produced by the real pipeline.
#[test]
fn sleep_carries_the_clock_host_effect() {
    let analysis = analysis(
        r#"
use std.concurrent as concurrent;
pub fn pause(): Null = concurrent.sleep(1.second);
"#,
    );

    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    let module = analysis.modules.values().next().expect("concurrency module");
    assert!(module.symbols["pause"].effects.effects.contains(&EffectKind::Clock));
}

#[test]
fn parallel_over_a_callback_list_value_returns_the_declared_list() {
    let analysis = analysis(
        r#"
use std.concurrent as concurrent;
pub fn run(callbacks: [fn(): Int]): [Int] = concurrent.parallel(callbacks);
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}

#[test]
fn race_over_a_callback_list_value_returns_the_declared_result() {
    let analysis = analysis(
        r#"
use std.concurrent as concurrent;
pub fn first(callbacks: [fn(): Int]): Int = concurrent.race(callbacks);
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}

#[test]
fn timeout_wraps_one_callback_with_a_duration() {
    let analysis = analysis(
        r#"
use std.concurrent as concurrent;
pub fn bounded(work: fn(): Int): Int = concurrent.timeout(work, 1.second);
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}

#[test]
fn literal_parallel_callbacks_with_mismatching_results_are_rejected() {
    let analysis = analysis(
        r#"
pub fn run(count: fn(): Int, label: fn(): Text): [Int] =
    std.concurrent.parallel([count, label]);
"#,
    );

    assert!(
        diagnostics(&analysis).iter().any(|(code, message)| *code
            == DIAG_TYPE
            && *message == "parallel callbacks must have compatible result types"),
        "mismatching callback result types must be rejected: {:?}",
        diagnostics(&analysis)
    );
}

#[test]
fn root_parallel_intrinsic_over_a_callback_list_value_returns_the_declared_list() {
    // The root intrinsic path (intrinsic_value_type) must agree with the
    // std.concurrent catalogue: parallel yields a list (ORNA-CONCUR-002), not a stream.
    let analysis = analysis(
        r#"
pub fn run(callbacks: [fn(): Int]): [Int] = parallel(callbacks);
"#,
    );

    assert_eq!(diagnostics(&analysis), Vec::<(&str, &str)>::new());
}
