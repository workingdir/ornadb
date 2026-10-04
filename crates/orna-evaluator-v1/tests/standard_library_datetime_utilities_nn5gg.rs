use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const TIME_MODULES: [&str; 7] = [
    "std/time.orna",
    "std/time/calendar.orna",
    "std/time/duration/compact.orna",
    "std/time/duration/clock.orna",
    "std/time/duration/words.orna",
    "std/time/duration/iso.orna",
    "std/time/utilities.orna",
];

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

fn pinned_time_session() -> (AdmittedReplSession, StandardDependencyProfile) {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| TIME_MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), TIME_MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/nn5gg-date-time-utilities",
        sources.clone(),
    )
    .expect("captured time, calendar and utility sources form a dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("time utility imports resolve against the pinned time modules");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| {
                panic!("captured time profile failed to load: {}", error.code())
            });
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed time source matches the captured profile");
    }
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-time-utilities-use-nn5gg.orna"
        )),
        Ok(None)
    );
    (session, profile)
}

fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = orna_syntax_v1::parse_repl(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let actual = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "date-time utility proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(actual, Some(boolean(true)), "{expression}");
    }
}

#[test]
fn weekend_and_business_day_helpers_preserve_invalid_date_information() {
    let (mut session, _) = pinned_time_session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-time-utilities-weekdays-nn5gg.orna"),
    );
}

#[test]
fn iso_week_bounds_cross_month_and_year_edges() {
    let (mut session, _) = pinned_time_session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-time-utilities-weeks-nn5gg.orna"),
    );
}

#[test]
fn quarter_helpers_return_inclusive_boundaries_and_reject_invalid_months() {
    let (mut session, _) = pinned_time_session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-time-utilities-quarters-nn5gg.orna"),
    );
}

#[test]
fn instant_ranges_are_closed_and_order_independent() {
    let (mut session, _) = pinned_time_session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-time-utilities-instant-range-nn5gg.orna"),
    );
}

#[test]
fn instant_distance_is_absolute_and_preserves_nanoseconds() {
    let (mut session, _) = pinned_time_session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-time-utilities-instant-distance-nn5gg.orna"),
    );
}
