use orna_exec::Value;
use orna_repl::Session;

fn run(session: &mut Session, input: &str) -> Option<Value> {
    let staged = session.stage(input).expect("stage");
    session.commit(staged).expect("commit")
}

fn code(session: &mut Session, input: &str) -> &'static str {
    let staged = match session.stage(input) {
        Ok(staged) => staged,
        Err(error) => return error.code(),
    };
    match session.commit(staged) {
        Ok(_) => "ok",
        Err(error) => error.code(),
    }
}

#[test]
fn expression_yields_value() {
    let mut session = Session::new();
    assert_eq!(run(&mut session, "1 + 2"), Some(Value::from(3_i64)));
}

#[test]
fn function_defined_then_called() {
    let mut session = Session::new();
    assert_eq!(code(&mut session, "fn double(x: Int): Int = x * 2;"), "ok");
    assert_eq!(run(&mut session, "double(21)"), Some(Value::from(42_i64)));
}

// Expected difference from `generics_fail_explicitly_in_the_bounded_runtime` in orna-project-repl: the HIR
// session accepts a generic function at the prompt and instantiates it per call instead of rejecting it.
#[test]
fn generic_function_is_accepted_and_instantiated_per_call() {
    let mut session = Session::new();
    assert_eq!(
        code(&mut session, "fn identity<T>(value: T): T = value;"),
        "ok"
    );
    assert_eq!(run(&mut session, "identity(1)"), Some(Value::from(1_i64)));
    assert_eq!(
        run(&mut session, "identity(\"text\")"),
        Some(Value::Text("text".into()))
    );
}

#[test]
fn redefinition_shadows_earlier_function() {
    let mut session = Session::new();
    code(&mut session, "fn value(): Int = 1;");
    code(&mut session, "fn value(): Int = 2;");
    assert_eq!(run(&mut session, "value()"), Some(Value::from(2_i64)));
}

#[test]
fn let_binding_persists_for_later_inputs() {
    let mut session = Session::new();
    assert_eq!(code(&mut session, "let seed = 10;"), "ok");
    assert_eq!(run(&mut session, "seed + 5"), Some(Value::from(15_i64)));
}

#[test]
fn let_rebinding_replaces_earlier_value() {
    let mut session = Session::new();
    code(&mut session, "let seed = 10;");
    code(&mut session, "let seed = 20;");
    assert_eq!(run(&mut session, "seed"), Some(Value::from(20_i64)));
}

#[test]
fn last_result_is_available_as_underscore() {
    let mut session = Session::new();
    run(&mut session, "7");
    assert_eq!(run(&mut session, "$_ * 6"), Some(Value::from(42_i64)));
}

#[test]
fn underscore_without_previous_result_is_rejected() {
    let mut session = Session::new();
    assert_eq!(code(&mut session, "$_ + 1"), "ORNA-EVAL-NAME");
}

#[test]
fn expression_statement_keeps_previous_result() {
    let mut session = Session::new();
    run(&mut session, "7");
    code(&mut session, "8;");
    assert_eq!(run(&mut session, "$_"), Some(Value::from(7_i64)));
}

#[test]
fn stale_staged_input_is_rejected_on_commit() {
    let mut session = Session::new();
    let staged = session.stage("1 + 1").expect("stage");
    run(&mut session, "5");
    let error = session.commit(staged).expect_err("stale");
    assert_eq!(error.code(), "ORNA-REPL-STALE");
}

#[test]
fn rollback_leaves_session_unchanged() {
    let mut session = Session::new();
    let staged = session.stage("let kept = 3;").expect("stage");
    session.rollback(staged);
    assert_eq!(code(&mut session, "kept"), "ORNA-S012-UNRESOLVED");
}

#[test]
fn effectful_input_is_rejected_and_not_run() {
    let mut session = Session::new();
    assert_eq!(
        code(&mut session, "sys.snapshot_str(\"main\")"),
        "ORNA-REPL-EFFECT"
    );
}

#[test]
fn reserved_repl_prefix_is_rejected() {
    let mut session = Session::new();
    assert_eq!(code(&mut session, "let __repl_x = 1;"), "ORNA-REPL-NAME");
}
