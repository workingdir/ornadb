use orna_conformance_v1::AdmittedReplSession;
use orna_evaluator_v1::Limits;
use orna_foundation_v1::{OvbRaw, Value};

const RUNTIME_FAILURE: &str = include_str!("fixtures/repl-admission/runtime-failure.orna");
const REJECTED_FUNCTION: &str = include_str!("fixtures/repl-admission/rejected-function.orna");
const PREVIEW: &str = include_str!("fixtures/repl-admission/preview.orna");
const SESSION_ISOLATION: &str = include_str!("fixtures/repl-admission/session-isolation.orna");
const RETAINED_RESULT: &str = include_str!("fixtures/repl-admission/retained-result.orna");
const EFFECT: &str = include_str!("fixtures/repl-admission/effect.orna");
const PROJECT_MAIN: &str = include_str!("fixtures/repl-admission/project-main.orna");
const PROJECT_LIBRARY: &str = include_str!("fixtures/repl-admission/project-library.orna");
const PROJECT_LIBRARY_EDITED: &str =
    include_str!("fixtures/repl-admission/project-library-edited.orna");
const PROJECT_CALLS: &str = include_str!("fixtures/repl-admission/project-calls.orna");

fn source(fixture: &str, index: usize) -> &str {
    fixture.lines().nth(index).expect("fixture source line")
}

#[test]
fn runtime_failure_does_not_publish_a_semantic_binding() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(source(RUNTIME_FAILURE, 0)).unwrap(),
        Some(Value::int(42.into()))
    );
    let pending = source(RUNTIME_FAILURE, 1);
    let parsed = orna_syntax_v1::parse_repl(pending);
    assert!(parsed.is_ok());
    assert!(
        orna_semantic_v1::ReplContext::empty()
            .stage(&parsed.value)
            .is_ok()
    );
    assert_eq!(
        session.submit(pending).unwrap_err().code(),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert!(session.submit(source(RUNTIME_FAILURE, 2)).is_err());
    assert_eq!(
        session.submit(source(RUNTIME_FAILURE, 3)).unwrap(),
        Some(Value::int(42.into()))
    );
    assert_eq!(session.submit(source(RUNTIME_FAILURE, 4)).unwrap(), None);
    assert_eq!(
        session.submit(source(RUNTIME_FAILURE, 5)).unwrap(),
        Some(Value::new(OvbRaw::Text("ready".into())).unwrap())
    );
}

#[test]
fn rejected_function_does_not_occupy_its_session_name() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert!(session.submit(source(REJECTED_FUNCTION, 0)).is_err());
    assert_eq!(session.submit(source(REJECTED_FUNCTION, 1)).unwrap(), None);
    assert_eq!(
        session.submit(source(REJECTED_FUNCTION, 2)).unwrap(),
        Some(Value::int(42.into()))
    );
    assert!(session.submit(source(REJECTED_FUNCTION, 3)).is_err());
    assert_eq!(
        session.submit(source(REJECTED_FUNCTION, 4)).unwrap(),
        Some(Value::int(42.into()))
    );
}

#[test]
fn preview_never_publishes_declarations_or_last_result() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(session.submit(source(PREVIEW, 0)).unwrap(), None);
    assert_eq!(
        session.submit(source(PREVIEW, 1)).unwrap(),
        Some(Value::int(42.into()))
    );
    assert_eq!(
        session.preview(source(PREVIEW, 2)).unwrap(),
        Value::int(43.into())
    );
    assert!(session.preview(source(PREVIEW, 3)).is_err());
    assert!(session.submit(source(PREVIEW, 4)).is_err());
    assert_eq!(
        session.submit(source(PREVIEW, 5)).unwrap(),
        Some(Value::int(42.into()))
    );
}

#[test]
fn typed_declarations_and_results_do_not_escape_a_session() {
    let mut first = AdmittedReplSession::new(Limits::default());
    first.submit(source(SESSION_ISOLATION, 0)).unwrap();
    first.submit(source(SESSION_ISOLATION, 1)).unwrap();
    assert_eq!(
        first.submit(source(SESSION_ISOLATION, 2)).unwrap(),
        Some(Value::int(42.into()))
    );
    let mut second = AdmittedReplSession::new(Limits::default());
    for source_text in [
        source(SESSION_ISOLATION, 3),
        source(SESSION_ISOLATION, 4),
        source(SESSION_ISOLATION, 5),
    ] {
        assert!(second.submit(source_text).is_err());
    }
}

#[test]
fn retained_function_keeps_its_typed_last_result_capture() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(source(RETAINED_RESULT, 0))
            .unwrap_err()
            .code(),
        "ORNA-S012-UNRESOLVED"
    );
    session.submit(source(RETAINED_RESULT, 1)).unwrap();
    session.submit(source(RETAINED_RESULT, 2)).unwrap();
    session.submit(source(RETAINED_RESULT, 3)).unwrap();
    assert_eq!(
        session.submit(source(RETAINED_RESULT, 4)).unwrap(),
        Some(Value::int(42.into()))
    );
}

#[test]
fn known_effect_is_rejected_by_preview_before_execution() {
    let mut session = AdmittedReplSession::new(Limits::default());
    session.submit(source(EFFECT, 0)).unwrap();
    let effect_source = source(EFFECT, 1);
    let parsed = orna_syntax_v1::parse_repl(effect_source);
    let admission = orna_semantic_v1::ReplContext::empty()
        .stage(&parsed.value)
        .unwrap();
    assert!(!admission.effects.effects.is_empty());
    assert_eq!(
        session.preview(effect_source).unwrap_err().code(),
        "ORNA-REPL-EFFECT"
    );
    assert_eq!(
        session.submit(source(EFFECT, 0)).unwrap(),
        Some(Value::int(42.into()))
    );
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
    std::fs::write(project.path().join("main.orna"), PROJECT_MAIN).unwrap();
    let library = project.path().join("library.orna");
    std::fs::write(&library, PROJECT_LIBRARY).unwrap();
    let repository = orna_repository_v1::Repository::discover(project.path()).unwrap();
    let loaded = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .unwrap();

    // Later disk edits must not change either side of an already loaded source snapshot.
    std::fs::write(&library, PROJECT_LIBRARY_EDITED).unwrap();
    let mut session = AdmittedReplSession::from_loaded_project(
        &loaded,
        std::iter::empty::<(String, String)>(),
        Limits::default(),
    )
    .unwrap();
    session.submit(PROJECT_MAIN.trim()).unwrap();
    assert_eq!(
        session.submit(source(PROJECT_CALLS, 0)).unwrap(),
        Some(Value::int(42.into()))
    );
    assert_eq!(
        session.submit(source(PROJECT_CALLS, 1)).unwrap_err().code(),
        "ORNA-S012-UNRESOLVED"
    );
}
