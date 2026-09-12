use orna_syntax_v1::{Expr, parse_expression};

fn parse(source: &str) -> Expr {
    let parsed = parse_expression(source);
    assert!(parsed.is_ok(), "{source:?}: {:?}", parsed.diagnostics);
    parsed.value
}

#[test]
fn unary_consumes_the_complete_postfix_spine() {
    let field = parse("-note.id");
    assert!(matches!(
        field,
        Expr::Unary { rhs, .. }
            if matches!(&*rhs, Expr::Field { base, name, .. }
                if name == "id" && matches!(&**base, Expr::Name { text, .. } if text == "note"))
    ));

    let call = parse("-note.render()");
    assert!(matches!(
        call,
        Expr::Unary { rhs, .. }
            if matches!(&*rhs, Expr::Call { callee, .. }
                if matches!(&**callee, Expr::Field { name, .. } if name == "render"))
    ));

    let index = parse("-note.items[0]");
    assert!(matches!(
        index,
        Expr::Unary { rhs, .. }
            if matches!(&*rhs, Expr::Index { base, .. }
                if matches!(&**base, Expr::Field { name, .. } if name == "items"))
    ));
}

#[test]
fn nested_prefixes_still_bind_outside_the_postfix_spine() {
    let parsed = parse("--note.id");
    assert!(matches!(
        parsed,
        Expr::Unary { op, rhs, .. }
            if op == "-" && matches!(&*rhs, Expr::Unary { op, rhs, .. }
                if op == "-" && matches!(&**rhs, Expr::Field { name, .. } if name == "id"))
    ));
}

#[test]
fn unary_postfix_operand_stops_before_power_and_multiplication() {
    for source in ["-note.id ^ 2", "-note.id * 2"] {
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
    for source in ["!note.ready", "+note.value"] {
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
    let parsed = parse("(-note).id");
    assert!(matches!(
        parsed,
        Expr::Field { base, name, .. }
            if name == "id" && matches!(&*base, Expr::Group { inner, .. }
                if matches!(&**inner, Expr::Unary { rhs, .. }
                    if matches!(&**rhs, Expr::Name { text, .. } if text == "note")))
    ));
}
