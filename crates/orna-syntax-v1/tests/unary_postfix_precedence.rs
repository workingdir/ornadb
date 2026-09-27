use orna_syntax_v1::{Expr, parse_expression};

const EXPRESSIONS: &str = include_str!("fixtures/unary-postfix-precedence.orna");

fn expression(index: usize) -> &'static str {
    EXPRESSIONS
        .lines()
        .nth(index)
        .expect("unary/postfix expression fixture line")
}

fn parse(source: &str) -> Expr {
    let parsed = parse_expression(source);
    assert!(parsed.is_ok(), "{source:?}: {:?}", parsed.diagnostics);
    parsed.value
}

#[test]
fn unary_consumes_the_complete_postfix_spine() {
    let field = parse(expression(0));
    assert!(matches!(
        field,
        Expr::Unary { rhs, .. }
            if matches!(&*rhs, Expr::Field { base, name, .. }
                if name == "id" && matches!(&**base, Expr::Name { text, .. } if text == "note"))
    ));

    let call = parse(expression(1));
    assert!(matches!(
        call,
        Expr::Unary { rhs, .. }
            if matches!(&*rhs, Expr::Call { callee, .. }
                if matches!(&**callee, Expr::Field { name, .. } if name == "render"))
    ));

    let index = parse(expression(2));
    assert!(matches!(
        index,
        Expr::Unary { rhs, .. }
            if matches!(&*rhs, Expr::Index { base, .. }
                if matches!(&**base, Expr::Field { name, .. } if name == "items"))
    ));
}

#[test]
fn nested_prefixes_still_bind_outside_the_postfix_spine() {
    let parsed = parse(expression(3));
    assert!(matches!(
        parsed,
        Expr::Unary { op, rhs, .. }
            if op == "-" && matches!(&*rhs, Expr::Unary { op, rhs, .. }
                if op == "-" && matches!(&**rhs, Expr::Field { name, .. } if name == "id"))
    ));
}

#[test]
fn unary_postfix_operand_stops_before_power_and_multiplication() {
    for source in [expression(4), expression(5)] {
        let parsed = parse(source);
        assert!(
            matches!(
                parsed,
                Expr::Binary { lhs, .. }
                    if matches!(&*lhs, Expr::Unary { rhs, .. }
                        if matches!(&**rhs, Expr::Field { name, .. } if name == "id"))
            ),
            "expected unary field on the left side of {source}"
        );
    }
}

#[test]
fn every_prefix_operator_consumes_a_following_field() {
    for source in [expression(6), expression(7)] {
        let parsed = parse(source);
        assert!(
            matches!(
                parsed,
                Expr::Unary { rhs, .. }
                    if matches!(&*rhs, Expr::Field { name, .. } if name == source.split('.').nth(1).unwrap())
            ),
            "expected prefix operator over field for {source}"
        );
    }
}

#[test]
fn parentheses_continue_to_override_unary_postfix_precedence() {
    let parsed = parse(expression(8));
    assert!(matches!(
        parsed,
        Expr::Field { base, name, .. }
            if name == "id" && matches!(&*base, Expr::Group { inner, .. }
                if matches!(&**inner, Expr::Unary { rhs, .. }
                    if matches!(&**rhs, Expr::Name { text, .. } if text == "note")))
    ));
}
