//! Parser regressions for bead ornadb-5il1.118, the A01 syntax audit of the
//! Orna 1.1.0 grammar. Each test pins one audited reproducer.

use orna_syntax_v1::{ControlKind, Declaration, Expr, Statement, parse_module};

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

fn block_parts(body: &Expr) -> (&[Statement], Option<&Expr>) {
    let Expr::Block {
        statements, tail, ..
    } = body
    else {
        panic!("expected a block, got {body:?}");
    };
    (statements, tail.as_deref())
}

fn control_statement_kind(statement: &Statement) -> ControlKind {
    let Statement::Control {
        value: Expr::Control { kind, .. },
        ..
    } = statement
    else {
        panic!("expected a control statement, got {statement:?}");
    };
    *kind
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

// A01-02 (ORNA-PARSE-002): a braced control expression followed by another
// block item is a statement, so `(..)` on the next line is the block tail
// rather than a call of the loop.
#[test]
fn control_statement_is_not_called_by_a_following_tuple() {
    let body = function_body(
        "pub fn total(xs: [Int]): (Int, [Int]) {\n    let total = 0;\n    for x in xs { total += x; }\n    (total, xs)\n}",
    );
    let (statements, tail) = block_parts(&body);
    assert_eq!(statements.len(), 2, "{statements:?}");
    assert_eq!(control_statement_kind(&statements[1]), ControlKind::For);
    assert!(
        matches!(tail, Some(Expr::Tuple { elements, .. }) if elements.len() == 2),
        "{tail:?}"
    );
}

// A01-02: `[a, b]` after a control statement is a list tail, not an index.
#[test]
fn control_statement_is_not_indexed_by_a_following_list() {
    let body = function_body(
        "pub fn pair(done: Bool, a: Int, b: Int): [Int] {\n    if done { return [a]; }\n    [a, b]\n}",
    );
    let (statements, tail) = block_parts(&body);
    assert_eq!(statements.len(), 1, "{statements:?}");
    assert_eq!(control_statement_kind(&statements[0]), ControlKind::If);
    assert!(
        matches!(tail, Some(Expr::List { elements, .. }) if elements.len() == 2),
        "{tail:?}"
    );
}

// A01-02: `-1` after a control statement is a negated tail, not a subtraction.
#[test]
fn control_statement_is_not_the_left_operand_of_a_following_negation() {
    let body = function_body("pub fn sign(c: Bool): Int {\n    if c { f() }\n    -1\n}");
    let (statements, tail) = block_parts(&body);
    assert_eq!(statements.len(), 1, "{statements:?}");
    assert_eq!(control_statement_kind(&statements[0]), ControlKind::If);
    assert!(
        matches!(tail, Some(Expr::Unary { op, rhs, .. })
            if op == "-" && matches!(rhs.as_ref(), Expr::Literal { text, .. } if text == "1")),
        "{tail:?}"
    );
}

// A01-02: a binary-only operator cannot begin a block item, so it still
// continues the control expression.
#[test]
fn binary_only_operator_continues_a_control_expression() {
    let body = function_body("pub fn doubled(c: Bool): Int { if c { 1 } else { 2 } * 2 }");
    let (statements, tail) = block_parts(&body);
    assert!(statements.is_empty(), "{statements:?}");
    let Some(tail) = tail else {
        panic!("expected a block tail");
    };
    let (lhs, op, _) = binary(tail);
    assert_eq!(op, "*");
    assert!(matches!(lhs, Expr::Control { .. }), "{lhs:?}");
}

// A01-18 (lexical structure, "Contextual names and construction"): in a
// control header an unparenthesised `{` begins the body, so the operand `n`
// of `0..n` is not a constructor and the tuple after the loop is the tail.
#[test]
fn range_header_operand_does_not_construct_the_loop_body() {
    let body = function_body(
        "pub fn sum_to(n: Int): (Int, Int) {\n    let total = 0;\n    for i in 0..n { total += i; }\n    (total, n)\n}",
    );
    let (statements, tail) = block_parts(&body);
    assert_eq!(statements.len(), 2, "{statements:?}");
    let Statement::Control {
        value:
            Expr::Control {
                kind: ControlKind::For,
                condition: Some(condition),
                body: Some(loop_body),
                ..
            },
        ..
    } = &statements[1]
    else {
        panic!("expected a for statement, got {:?}", statements[1]);
    };
    assert!(
        matches!(condition.as_ref(), Expr::Range { upper: Some(upper), .. }
            if name(upper) == "n"),
        "{condition:?}"
    );
    assert!(
        matches!(loop_body.as_ref(), Expr::Block { .. }),
        "{loop_body:?}"
    );
    assert!(
        matches!(tail, Some(Expr::Tuple { elements, .. }) if elements.len() == 2),
        "{tail:?}"
    );
}

// A01-18: the comparison operand `n` keeps the `while` body, and `-i` is the
// block tail.
#[test]
fn comparison_header_operand_does_not_construct_the_loop_body() {
    let body = function_body(
        "pub fn count(n: Int): Int {\n    let i = 0;\n    while i < n { i += 1; }\n    -i\n}",
    );
    let (statements, tail) = block_parts(&body);
    assert_eq!(statements.len(), 2, "{statements:?}");
    assert_eq!(control_statement_kind(&statements[1]), ControlKind::While);
    assert!(
        matches!(tail, Some(Expr::Unary { op, rhs, .. }) if op == "-" && name(rhs) == "i"),
        "{tail:?}"
    );
}

// A01-18: the unary operand `ready` keeps the `if` body, and `[x]` is the
// block tail.
#[test]
fn unary_header_operand_does_not_construct_the_if_body() {
    let body = function_body(
        "pub fn poll(ready: Bool, x: Int): [Int] {\n    if !ready { wait() }\n    [x]\n}",
    );
    let (statements, tail) = block_parts(&body);
    assert_eq!(statements.len(), 1, "{statements:?}");
    assert_eq!(control_statement_kind(&statements[0]), ControlKind::If);
    assert!(
        matches!(tail, Some(Expr::List { elements, .. }) if elements.len() == 1),
        "{tail:?}"
    );
}

// A01-18: inside an argument list, the `case` scrutinee operand `b` keeps
// the arms.
#[test]
fn case_scrutinee_operand_does_not_construct_the_arms() {
    let body = function_body(
        "pub fn pick(a: Int, b: Int, x: Int, y: Int): Int = f(case a + b { 0: x, _: y });",
    );
    let Expr::Call { arguments, .. } = &body else {
        panic!("expected a call, got {body:?}");
    };
    let [argument] = arguments.as_slice() else {
        panic!("expected one argument, got {arguments:?}");
    };
    let Expr::Control {
        kind: ControlKind::Case,
        condition: Some(condition),
        arms,
        ..
    } = &argument.value
    else {
        panic!("expected a case argument, got {:?}", argument.value);
    };
    let (lhs, op, rhs) = binary(condition);
    assert_eq!((name(lhs), op, name(rhs)), ("a", "+", "b"));
    assert_eq!(arms.len(), 2, "{arms:?}");
}

// A01-18: a constructor operand in a control header must be parenthesised;
// unparenthesised, its brace is the body and the parser says so.
#[test]
fn unparenthesized_header_constructor_is_rejected() {
    let parsed =
        parse_module("pub fn same(p: Point): Int = if p == Point { x: 1 } { 1 } else { 0 };");
    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    assert_eq!(parsed.diagnostics[0].code, "ORNA-PARSE-001");
    assert_eq!(
        parsed.diagnostics[0].message,
        "nominal constructors before a control body must be parenthesized"
    );

    let body =
        function_body("pub fn same(p: Point): Int = if p == (Point { x: 1 }) { 1 } else { 0 };");
    let Expr::Control {
        condition: Some(condition),
        ..
    } = &body
    else {
        panic!("expected an if expression, got {body:?}");
    };
    let (_, op, rhs) = binary(condition);
    assert_eq!(op, "==");
    assert!(
        matches!(rhs, Expr::Group { inner, .. } if matches!(inner.as_ref(), Expr::Nominal { .. })),
        "{rhs:?}"
    );
}
