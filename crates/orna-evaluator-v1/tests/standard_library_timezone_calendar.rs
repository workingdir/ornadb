use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

#[path = "support/pinned_time_text_std.rs"]
mod pinned_time_text_std;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_timezone_and_calendar_sources_cover_zone_policy_and_civil_arithmetic() {
    let mut session = pinned_time_text_std::time_session();
    let import_source = include_str!("fixtures/stdlib-timezone-calendar-use-b8tgd.orna");
    let import_parsed = orna_syntax_v1::parse_repl(import_source);
    assert!(
        import_parsed.diagnostics.is_empty(),
        "{:#?}",
        import_parsed.diagnostics
    );
    let import = session.submit(import_source);
    assert!(
        import.is_ok(),
        "import error: {}",
        import
            .err()
            .map(|error| error.code().to_owned())
            .unwrap_or_default()
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-calendar-use-b8tgd.orna")),
        Ok(None)
    );

    for (source, assertion_count) in [
        (
            include_str!("fixtures/stdlib-timezone-contract-b8tgd.orna"),
            6,
        ),
        (
            include_str!("fixtures/stdlib-calendar-number-contract-b8tgd.orna"),
            4,
        ),
        (
            include_str!("fixtures/stdlib-calendar-day-arithmetic-contract-b8tgd.orna"),
            11,
        ),
        (
            include_str!("fixtures/stdlib-calendar-month-arithmetic-contract-b8tgd.orna"),
            5,
        ),
    ] {
        let parsed = orna_syntax_v1::parse_repl(source);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(canonical(Raw::Array(vec![
                Raw::Bool(true);
                assertion_count
            ])))),
            "diagnostic={:?} source={source}",
            result.as_ref().err().map(|error| error.code())
        );
    }
}

#[test]
fn timezone_overlap_and_gap_policies_fail_closed_and_core_dates_need_no_std() {
    let mut with_std = pinned_time_text_std::time_session();
    let import_source = include_str!("fixtures/stdlib-timezone-calendar-use-b8tgd.orna");
    let import_parsed = orna_syntax_v1::parse_repl(import_source);
    assert!(
        import_parsed.diagnostics.is_empty(),
        "{:#?}",
        import_parsed.diagnostics
    );
    let import = with_std.submit(import_source);
    assert!(
        import.is_ok(),
        "import error: {}",
        import
            .err()
            .map(|error| error.code().to_owned())
            .unwrap_or_default()
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-time-calendar-use-b8tgd.orna")),
        Ok(None)
    );
    for source in [
        include_str!("fixtures/stdlib-timezone-overlap-reject-b8tgd.orna"),
        include_str!("fixtures/stdlib-timezone-gap-b8tgd.orna"),
    ] {
        assert_eq!(
            with_std.submit(source).unwrap_err().code(),
            "ORNA-EVAL-VALUE",
            "{source}"
        );
    }

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std.submit(include_str!(
            "fixtures/stdlib-core-date-without-std-b8tgd.orna"
        )),
        Ok(Some(canonical(Raw::Array(vec![Raw::Bool(true); 3]))))
    );
    assert_eq!(
        without_std
            .submit(include_str!(
                "fixtures/stdlib-timezone-calendar-without-snapshot-b8tgd.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
