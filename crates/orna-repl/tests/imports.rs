use orna_exec::Value;
use orna_repl::Session;

/// `std.text.unicode_version` is a plain literal: the std text leaves that route through natives are not runnable
/// on the HIR executor yet, so these tests exercise import resolution without calling them.
const VERSION: &str = "16.0.0";

fn run(session: &mut Session, input: &str) -> Option<Value> {
    let staged = session.stage(input).expect("stage");
    session.commit(staged).expect("commit")
}

#[test]
fn use_binds_a_standard_module_for_later_inputs() {
    let mut session = Session::new();
    assert_eq!(run(&mut session, "use std.text as text;"), None);
    assert_eq!(
        run(&mut session, "text.unicode_version()"),
        Some(Value::Text(VERSION.into()))
    );
}

#[test]
fn qualified_standard_path_needs_no_use() {
    let mut session = Session::new();
    assert_eq!(
        run(&mut session, "std.text.unicode_version()"),
        Some(Value::Text(VERSION.into()))
    );
}

#[test]
fn function_reads_an_imported_module() {
    let mut session = Session::new();
    run(&mut session, "use std.text as text;");
    run(&mut session, "fn version(): Str = text.unicode_version();");
    assert_eq!(
        run(&mut session, "version()"),
        Some(Value::Text(VERSION.into()))
    );
}

#[test]
fn unknown_module_is_rejected_and_leaves_the_session_unchanged() {
    let mut session = Session::new();
    assert!(session.stage("use std.nope as nope;").is_err());
    assert_eq!(run(&mut session, "1 + 1"), Some(Value::from(2_i64)));
}

#[test]
fn repeated_use_is_a_no_op() {
    let mut session = Session::new();
    run(&mut session, "use std.text as text;");
    assert_eq!(run(&mut session, "use std.text as text;"), None);
    assert_eq!(
        run(&mut session, "text.unicode_version()"),
        Some(Value::Text(VERSION.into()))
    );
}
