use orna_exec::Value;
use orna_repl::Session;

fn run(session: &mut Session, input: &str) -> Option<Value> {
    let staged = session.stage(input).expect("stage");
    session.commit(staged).expect("commit")
}

#[test]
fn annotated_global_let_is_read_by_later_inputs() {
    let mut session = Session::new();
    run(&mut session, "let x: Int = 3;");
    assert_eq!(run(&mut session, "x + 1"), Some(Value::from(4_i64)));
}

#[test]
fn dependent_let_keeps_its_value_after_its_source_is_rebound() {
    let mut session = Session::new();
    run(&mut session, "let a = 1;");
    run(&mut session, "let b = a + 1;");
    run(&mut session, "let a = 5;");
    assert_eq!(run(&mut session, "b"), Some(Value::from(2_i64)));
    assert_eq!(run(&mut session, "a"), Some(Value::from(5_i64)));
}

#[test]
fn let_may_read_its_own_previous_binding() {
    let mut session = Session::new();
    run(&mut session, "let seed = 10;");
    run(&mut session, "let seed = seed + 1;");
    assert_eq!(run(&mut session, "seed"), Some(Value::from(11_i64)));
}

#[test]
fn global_let_is_rebound_to_a_different_type() {
    let mut session = Session::new();
    run(&mut session, "let t = 1;");
    run(&mut session, "let t = \"text\";");
    assert_eq!(run(&mut session, "t"), Some(Value::Text("text".into())));
}

#[test]
fn global_let_is_read_by_a_function_call_input() {
    let mut session = Session::new();
    run(&mut session, "fn double(x: Int): Int = x * 2;");
    run(&mut session, "let amount = 21;");
    assert_eq!(
        run(&mut session, "double(amount)"),
        Some(Value::from(42_i64))
    );
}
