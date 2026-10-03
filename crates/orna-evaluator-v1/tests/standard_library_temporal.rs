use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

#[path = "support/pinned_time_text_std.rs"]
mod pinned_time_text_std;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_time_exports_use_the_captured_timezone_edition() {
    let mut session = pinned_time_text_std::time_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-time-7q1e.orna")),
        Ok(None)
    );
    for source in [
        include_str!("fixtures/stdlib-time-version-7q1e.orna"),
        include_str!("fixtures/stdlib-time-offsets-7q1e.orna"),
        include_str!("fixtures/stdlib-time-overlap-7q1e.orna"),
    ] {
        assert_eq!(
            session.submit(source),
            Ok(Some(canonical(Raw::Bool(true)))),
            "{source}"
        );
    }
    for source in [
        include_str!("fixtures/stdlib-time-ambiguous-reject-7q1e.orna"),
        include_str!("fixtures/stdlib-time-gap-7q1e.orna"),
    ] {
        assert_eq!(
            session.submit(source).unwrap_err().code(),
            "ORNA-EVAL-VALUE",
            "{source}"
        );
    }
}

#[test]
fn time_remains_an_optional_explicit_standard_module() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-without-snapshot-7q1e.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}

#[test]
fn pinned_calendar_helpers_use_source_and_core_dates_work_without_std() {
    let parsed = orna_syntax_v1::parse_repl(include_str!(
        "fixtures/stdlib-time-calendar-contract-r0asr.orna"
    ));
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);

    let mut session = pinned_time_text_std::time_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-calendar-use-r0asr.orna")),
        Ok(None)
    );
    let calendar_result =
        session.submit(include_str!("fixtures/stdlib-time-calendar-contract-r0asr.orna"));
    assert_eq!(
        calendar_result,
        Ok(Some(canonical(Raw::Array(vec![
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
        ])))),
        "diagnostic={:?}",
        calendar_result.as_ref().err().map(|error| error.code())
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std.submit(include_str!("fixtures/stdlib-core-date-time-without-std-r0asr.orna")),
        Ok(Some(canonical(Raw::Bool(true))))
    );
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-time-calendar-without-snapshot-r0asr.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
