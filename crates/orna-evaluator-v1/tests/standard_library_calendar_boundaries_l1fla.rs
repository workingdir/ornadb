use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::{Raw, Value};

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn tuple_date(year: i64, month: i64, day: i64) -> Raw {
    Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            Raw::Int(year.into()),
            Raw::Int(month.into()),
            Raw::Int(day.into()),
        ])),
    )
}

fn optional_date(year: i64, month: i64, day: i64) -> CanonicalValue {
    let date = Value::new(tuple_date(year, month, day)).expect("date tuple is canonical");
    canonical(
        Value::option(Some(date))
            .expect("date option is canonical")
            .raw()
            .clone(),
    )
}

fn instant(unix_seconds: i64, nanosecond: i64) -> CanonicalValue {
    canonical(Raw::Tag(
        60002,
        Box::new(Raw::Array(vec![
            Raw::Int(unix_seconds.into()),
            Raw::Int(nanosecond.into()),
        ])),
    ))
}

fn duration(seconds: i64, nanosecond: i64) -> CanonicalValue {
    canonical(Raw::Tag(
        60005,
        Box::new(Raw::Array(vec![
            Raw::Int(seconds.into()),
            Raw::Int(nanosecond.into()),
        ])),
    ))
}

fn captured_time_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/time.orna"
                || path == "std/time/calendar.orna"
                || path.starts_with("std/time/duration/")
        })
        .collect()
}

fn pinned_time_session() -> (AdmittedReplSession, StandardDependencyProfile) {
    let sources = captured_time_sources();
    assert_eq!(sources.len(), 6, "time and formatter modules are captured");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/l1fla-time-calendar-boundaries",
        sources.clone(),
    )
    .expect("selected time module sources form a captured dependency snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("loaded module bytes match their captured snapshot");
        let mut changed = source.clone();
        changed.push_str("\n// changed after snapshot capture");
        assert!(
            profile.verify_source(path, &changed).is_err(),
            "changed source must not replace captured {path}"
        );
    }
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("time modules resolve against the core catalogue");
    let session = AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .expect("captured time modules are admitted");
    (session, profile)
}

#[test]
fn pinned_time_arithmetic_and_calendar_boundaries_return_exact_values() {
    let (mut session, _profile) = pinned_time_session();
    for import in [
        include_str!("fixtures/stdlib-time-use-l1fla.orna"),
        include_str!("fixtures/stdlib-calendar-use-l1fla.orna"),
    ] {
        let result = session.submit(import);
        assert_eq!(
            result,
            Ok(None),
            "time/calendar import failed with {}: {import}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }

    for (source, expected) in [
        (
            include_str!("fixtures/stdlib-time-add-elapsed-dst-l1fla.orna"),
            instant(1_711_848_600, 0),
        ),
        (
            include_str!("fixtures/stdlib-time-elapsed-one-nanosecond-l1fla.orna"),
            duration(0, 1),
        ),
        (
            include_str!("fixtures/stdlib-calendar-month-start-l1fla.orna"),
            optional_date(2024, 2, 1),
        ),
        (
            include_str!("fixtures/stdlib-calendar-month-end-leap-l1fla.orna"),
            optional_date(2024, 2, 29),
        ),
        (
            include_str!("fixtures/stdlib-calendar-month-end-century-l1fla.orna"),
            optional_date(2100, 2, 28),
        ),
        (
            include_str!("fixtures/stdlib-calendar-month-end-400-year-l1fla.orna"),
            optional_date(2400, 2, 29),
        ),
        (
            include_str!("fixtures/stdlib-calendar-year-start-l1fla.orna"),
            optional_date(1, 1, 1),
        ),
        (
            include_str!("fixtures/stdlib-calendar-year-end-l1fla.orna"),
            optional_date(9999, 12, 31),
        ),
        (
            include_str!("fixtures/stdlib-calendar-invalid-boundary-l1fla.orna"),
            canonical(Raw::Null),
        ),
    ] {
        let actual = session
            .submit(source)
            .unwrap_or_else(|error| panic!("{source} failed with {}", error.code()));
        assert_eq!(actual, Some(expected), "input: {source}");
    }
}

#[test]
fn elapsed_instant_arithmetic_remains_core_without_std() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-core-elapsed-without-l1fla.orna"
            ))
            .unwrap_or_else(|error| panic!("core elapsed arithmetic failed with {}", error.code())),
        Some(instant(1_711_846_800, 999_999_999))
    );
}
