//! Parser regressions for bead ornadb-5il1.118, the A01 syntax audit of the
//! Orna 1.1.0 grammar. Each test pins one audited reproducer.

use orna_syntax_v1::{Declaration, Expr, Statement, parse_module};

fn function_body(source: &str) -> Expr {
    let parsed = parse_module(source);
    assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
    let mut items = parsed.value.items.into_iter();
    let (Some(item), None) = (items.next(), items.next()) else {
        panic!("expected exactly one item in {source}");
    };
    let Declaration::Function { body, .. } = item.declaration else {
        panic!("expected a function declaration in {source}");
    };
    body
}

fn binary(expr: &Expr) -> (&Expr, &str, &Expr) {
    let Expr::Binary { lhs, op, rhs, .. } = expr else {
        panic!("expected a binary expression, got {expr:?}");
    };
    (lhs, op, rhs)
}

fn name(expr: &Expr) -> &str {
    let Expr::Name { text, .. } = expr else {
        panic!("expected a name, got {expr:?}");
    };
    text
}

// A01-01 (ORNA-OP-001): a declined generic-call speculation rewinds, so the
// first comparison keeps `< b` although a later `> (` exists.
#[test]
fn declined_generic_call_keeps_both_if_conditions() {
    let body = function_body(
        "pub fn pick(a: Int, b: Int): Int = if a < b { 1 } else if a > (b + 1) { 2 } else { 3 };",
    );
    let Expr::Control {
        condition: Some(condition),
        alternate: Some(alternate),
        ..
    } = &body
    else {
        panic!("expected an if expression, got {body:?}");
    };
    let (lhs, op, rhs) = binary(condition);
    assert_eq!((name(lhs), op, name(rhs)), ("a", "<", "b"));
    let Expr::Control {
        condition: Some(condition),
        ..
    } = alternate.as_ref()
    else {
        panic!("expected an else-if expression, got {alternate:?}");
    };
    let (lhs, op, rhs) = binary(condition);
    assert_eq!((name(lhs), op), ("a", ">"));
    assert!(matches!(rhs, Expr::Group { .. }), "{rhs:?}");
}

// A01-01: `lo < x && x > (hi - 1)` is a conjunction of two comparisons, not
// `lo && x > (hi - 1)`.
#[test]
fn declined_generic_call_keeps_a_conjunction_of_comparisons() {
    let body = function_body(
        "pub fn within(lo: Int, x: Int, hi: Int): Bool { let ok = false; ok = lo < x && x > (hi - 1); ok }",
    );
    let Expr::Block { statements, .. } = &body else {
        panic!("expected a function block, got {body:?}");
    };
    let Some(Statement::Assignment { value, .. }) = statements.get(1) else {
        panic!("expected the assignment statement, got {statements:?}");
    };
    let (lower, op, upper) = binary(value);
    assert_eq!(op, "&&");
    let (lhs, op, rhs) = binary(lower);
    assert_eq!((name(lhs), op, name(rhs)), ("lo", "<", "x"));
    let (lhs, op, rhs) = binary(upper);
    assert_eq!((name(lhs), op), ("x", ">"));
    assert!(matches!(rhs, Expr::Group { .. }), "{rhs:?}");
}
