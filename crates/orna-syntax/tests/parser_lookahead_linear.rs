use orna_syntax::parse_module;
use std::time::{Duration, Instant};

/// Each shape makes the parser ask a forward lookahead at many positions.
/// The old scans rescanned to end of input from every probe, so doubling the
/// input roughly quadrupled the parse time. The assertion compares the 8k and
/// 4k timings (best of three each), which is a ratio rather than a wall-clock
/// budget. Linear work gives about 2x; quadratic work gives about 4x. The
/// absolute cap is only a backstop.
const SIZES: [usize; 2] = [4_000, 8_000];
/// Shapes whose quadratic form is expensive per unit use half the size.
const SMALL_SIZES: [usize; 2] = [1_000, 2_000];
const RUNS: usize = 3;
const MAX_RATIO: f64 = 3.0;
const BACKSTOP: Duration = Duration::from_secs(120);

fn best_of(source: &str) -> Duration {
    (0..RUNS)
        .map(|_| {
            let started = Instant::now();
            let _ = parse_module(source).is_malformed();
            started.elapsed()
        })
        .min()
        .expect("RUNS > 0")
}

fn assert_linear(label: &str, sizes: [usize; 2], prefix: &str, unit: &str, suffix: &str) {
    let source = |count: usize| format!("{prefix}{}{suffix}", unit.repeat(count));
    let small = best_of(&source(sizes[0]));
    let large = best_of(&source(sizes[1]));
    assert!(
        large < BACKSTOP,
        "{label}: {} units parsed in {large:?}, backstop {BACKSTOP:?}",
        sizes[1]
    );
    let ratio = large.as_secs_f64() / small.as_secs_f64();
    assert!(
        ratio < MAX_RATIO,
        "{label}: {}/{} time ratio {ratio:.2} ({} {small:?}, {} {large:?}); \
         linear lookahead should stay near 2, quadratic is near 4",
        sizes[1],
        sizes[0],
        sizes[0],
        sizes[1]
    );
}

#[test]
fn var_declarations_without_terminators_parse_in_linear_time() {
    assert_linear("var", SIZES, "", "{ var x = 1 ", "");
}

/// The probes must sit in expression position. Item recovery skips top-level
/// text such as `{ a < b`, so only an expression parse reaches the generic-call
/// lookahead; inside an array literal every `<` scans to the closing `]`.
#[test]
fn angle_brackets_without_closers_parse_in_linear_time() {
    assert_linear("angle", SIZES, "fn f() { [", "a < b, ", "] }");
}

/// Recovering from a case arm with no `:` scans forward and asks, at every
/// token that could begin the next arm, whether a pattern and guard parse up
/// to an arm colon. Each probe must cost the length of the pattern it parses,
/// not the length of the file: every `foo(1)` below is such a probe and none
/// of them finds an arm.
#[test]
fn case_arm_recovery_probes_parse_in_linear_time() {
    assert_linear(
        "case recovery",
        SMALL_SIZES,
        "fn f(x) { case x { a ",
        "foo(1) ",
        "} }",
    );
}
