use orna_conformance_v1::AdmittedReplSession;
use orna_evaluator_v1::Limits;
use orna_foundation_v1::{OvbRaw, Value};

#[test]
fn runtime_failure_does_not_publish_a_semantic_binding() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit("40 + 2").unwrap(),
        Some(Value::int(42.into()))
    );
    let source = "let pending: Int = 1 / 0;";
    let parsed = orna_syntax_v1::parse_repl(source);
    assert!(parsed.is_ok());
    assert!(
        orna_semantic_v1::ReplContext::empty()
            .stage(&parsed.value)
            .is_ok()
    );
    assert_eq!(
        session.submit(source).unwrap_err().code(),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert!(session.submit("pending").is_err());
    assert_eq!(session.submit("$_").unwrap(), Some(Value::int(42.into())));
    assert_eq!(
        session.submit("let pending: Str = \"ready\";").unwrap(),
        None
    );
    assert_eq!(
        session.submit("pending").unwrap(),
        Some(Value::new(OvbRaw::Text("ready".into())).unwrap())
    );
}

#[test]
fn rejected_function_does_not_occupy_its_session_name() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert!(session.submit("fn answer(): Int = \"wrong\";").is_err());
    assert_eq!(session.submit("fn answer(): Int = 42;").unwrap(), None);
    assert_eq!(
        session.submit("answer()").unwrap(),
        Some(Value::int(42.into()))
    );
    assert!(session.submit("answer(99)").is_err());
    assert_eq!(session.submit("$_").unwrap(), Some(Value::int(42.into())));
}

#[test]
fn preview_never_publishes_declarations_or_last_result() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(session.submit("let seed: Int = 40;").unwrap(), None);
    assert_eq!(
        session.submit("seed + 2").unwrap(),
        Some(Value::int(42.into()))
    );
    assert_eq!(session.preview("seed + 3").unwrap(), Value::int(43.into()));
    assert!(session.preview("let preview_only: Int = 9;").is_err());
    assert!(session.submit("preview_only").is_err());
    assert_eq!(session.submit("$_").unwrap(), Some(Value::int(42.into())));
}

#[test]
fn typed_declarations_and_results_do_not_escape_a_session() {
    let mut first = AdmittedReplSession::new(Limits::default());
    first.submit("let seed: Int = 21;").unwrap();
    first
        .submit("fn twice(value: Int): Int = value + value;")
        .unwrap();
    assert_eq!(
        first.submit("twice(seed)").unwrap(),
        Some(Value::int(42.into()))
    );
    let mut second = AdmittedReplSession::new(Limits::default());
    for source in ["seed", "twice(21)", "$_"] {
        assert!(second.submit(source).is_err());
    }
}

#[test]
fn retained_function_keeps_its_typed_last_result_capture() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit("fn previous(): Int = $_;")
            .unwrap_err()
            .code(),
        "ORNA-S012-UNRESOLVED"
    );
    session.submit("42").unwrap();
    session.submit("fn previous(): Int = $_;").unwrap();
    session.submit("\"later\"").unwrap();
    assert_eq!(
        session.submit("previous()").unwrap(),
        Some(Value::int(42.into()))
    );
}

#[test]
fn known_effect_is_rejected_by_preview_before_execution() {
    let mut session = AdmittedReplSession::new(Limits::default());
    session.submit("42").unwrap();
    let source = "std.net.http.get(\"https://example.com\")";
    let parsed = orna_syntax_v1::parse_repl(source);
    let admission = orna_semantic_v1::ReplContext::empty()
        .stage(&parsed.value)
        .unwrap();
    assert!(!admission.effects.effects.is_empty());
    assert_eq!(
        session.preview(source).unwrap_err().code(),
        "ORNA-REPL-EFFECT"
    );
    assert_eq!(session.submit("$_").unwrap(), Some(Value::int(42.into())));
}

#[test]
fn loaded_project_snapshot_pairs_visibility_and_executable_bodies() {
    let project = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(project.path())
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(project.path().join("main.orna"), "use library;").unwrap();
    let library = project.path().join("library.orna");
    std::fs::write(&library, "pub fn value(): Int = 42; fn hidden(): Int = 99;").unwrap();
    let repository = orna_repository_v1::Repository::discover(project.path()).unwrap();
    let loaded = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .unwrap();

    // Later disk edits must not change either side of an already loaded source snapshot.
    std::fs::write(
        &library,
        "pub fn value(): Str = \"changed\"; pub fn hidden(): Int = 99;",
    )
    .unwrap();
    let mut session = AdmittedReplSession::from_loaded_project(
        &loaded,
        std::iter::empty::<(String, String)>(),
        Limits::default(),
    )
    .unwrap();
    session.submit("use library;").unwrap();
    assert_eq!(
        session.submit("library.value()").unwrap(),
        Some(Value::int(42.into()))
    );
    assert_eq!(
        session.submit("library.hidden()").unwrap_err().code(),
        "ORNA-S012-UNRESOLVED"
    );
}
