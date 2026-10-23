use orna_syntax_v1::parse_module;
use std::time::{Duration, Instant};

/// Each shape makes the parser ask a forward lookahead at many positions.
/// Before the precomputed table, every probe rescanned to the end of input,
/// so 30k repetitions took minutes. Linear lookahead keeps it well under the
/// budget even in a debug build.
const REPEATS: usize = 30_000;
const BUDGET: Duration = Duration::from_secs(30);

fn assert_linear(label: &str, source: &str) {
    let started = Instant::now();
    let parsed = parse_module(source);
    let elapsed = started.elapsed();
    assert!(
        elapsed < BUDGET,
        "{label}: parse took {elapsed:?}, budget {BUDGET:?} (lookahead is not linear)"
    );
    let _ = parsed.is_malformed();
}

#[test]
fn var_declarations_without_terminators_parse_in_linear_time() {
    assert_linear("var", &"{ var x = 1 ".repeat(REPEATS));
}

#[test]
fn angle_brackets_without_closers_parse_in_linear_time() {
    assert_linear("angle", &"{ a < b ".repeat(REPEATS));
}
