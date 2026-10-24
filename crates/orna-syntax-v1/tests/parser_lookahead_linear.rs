use orna_syntax_v1::parse_module;
use std::time::{Duration, Instant};

/// Each shape makes the parser ask a forward lookahead at many positions.
/// The old scans rescanned to end of input from every probe, so doubling the
/// input roughly quadrupled the parse time. The assertion compares the 8k and
/// 4k timings (best of three each), which is a ratio rather than a wall-clock
/// budget. Linear work gives about 2x; quadratic work gives about 4x. The
/// absolute cap is only a backstop.
const SIZES: [usize; 2] = [4_000, 8_000];
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

fn assert_linear(label: &str, prefix: &str, unit: &str, suffix: &str) {
    let source = |count: usize| format!("{prefix}{}{suffix}", unit.repeat(count));
    let small = best_of(&source(SIZES[0]));
    let large = best_of(&source(SIZES[1]));
    assert!(
        large < BACKSTOP,
        "{label}: 8k parse took {large:?}, backstop {BACKSTOP:?}"
    );
    let ratio = large.as_secs_f64() / small.as_secs_f64();
    assert!(
        ratio < MAX_RATIO,
        "{label}: 8k/4k time ratio {ratio:.2} (4k {small:?}, 8k {large:?}); \
         linear lookahead should stay near 2, quadratic is near 4"
    );
}

#[test]
fn var_declarations_without_terminators_parse_in_linear_time() {
    assert_linear("var", "", "{ var x = 1 ", "");
}

/// The probes must sit in expression position. Item recovery skips top-level
/// text such as `{ a < b`, so only an expression parse reaches the generic-call
/// lookahead; inside an array literal every `<` scans to the closing `]`.
#[test]
fn angle_brackets_without_closers_parse_in_linear_time() {
    assert_linear("angle", "fn f() { [", "a < b, ", "] }");
}
