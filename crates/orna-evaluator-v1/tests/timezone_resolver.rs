use orna_evaluator_v1::{
    Instant, LocalDateTime, LocalTimeResolution, TimeZone, TimeZoneError, resolve_time_zone,
};

fn instant(unix_seconds: i64, nanosecond: u32) -> Instant {
    Instant::new(unix_seconds, nanosecond).expect("valid instant fixture")
}

fn local(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    nanosecond: u32,
) -> LocalDateTime {
    LocalDateTime::new(year, month, day, hour, minute, second, nanosecond)
        .expect("valid local date-time fixture")
}

fn assert_error_kind<T>(result: Result<T, TimeZoneError>, expected_kind: &str) {
    let error = match result {
        Ok(_) => panic!("operation unexpectedly succeeded"),
        Err(error) => error,
    };
    let debug = format!("{error:?}");
    assert!(
        debug.contains(expected_kind),
        "expected {expected_kind}, got {debug}"
    );
}

#[test]
fn utc_conversion_uses_unix_epoch_and_preserves_fraction() {
    let utc = resolve_time_zone("UTC").expect("UTC is pinned");
    let value = utc
        .at(instant(0, 123_456_789))
        .expect("epoch is representable");

    assert_eq!(value.zone, "UTC");
    assert_eq!(value.offset_seconds, 0);
    assert_eq!(value.local.year, 1970);
    assert_eq!(value.local.month, 1);
    assert_eq!(value.local.day, 1);
    assert_eq!(value.local.hour, 0);
    assert_eq!(value.local.minute, 0);
    assert_eq!(value.local.second, 0);
    assert_eq!(value.local.nanosecond, 123_456_789);
}

#[test]
fn london_has_distinct_winter_and_summer_offsets() {
    let london = resolve_time_zone("Europe/London").expect("London is pinned");

    let winter = london
        .at(instant(1_705_322_096, 0))
        .expect("winter instant is representable");
    assert_eq!(winter.local, local(2024, 1, 15, 12, 34, 56, 0));
    assert_eq!(winter.offset_seconds, 0);

    let summer = london
        .at(instant(1_721_046_896, 0))
        .expect("summer instant is representable");
    assert_eq!(summer.local, local(2024, 7, 15, 13, 34, 56, 0));
    assert_eq!(summer.offset_seconds, 3_600);
}

#[test]
fn london_spring_forward_reports_the_nonexistent_interval() {
    let london = resolve_time_zone("Europe/London").expect("London is pinned");
    let missing = local(2024, 3, 31, 1, 30, 0, 0);

    let resolution = london
        .resolve_local(missing)
        .expect("nonexistent is a successful explicit resolution result");
    let LocalTimeResolution::Nonexistent { before, after } = resolution else {
        panic!("expected the 2024 spring-forward gap, got {resolution:?}");
    };

    // The gap starts at 01:00 UTC: before/after are the adjacent timeline
    // instants, not an arbitrary fixed-offset interpretation of the local text.
    assert_eq!(before, instant(1_711_846_799, 999_999_999));
    assert_eq!(after, instant(1_711_846_800, 0));

    let before_local = london.at(before).expect("transition predecessor is valid");
    assert_eq!(before_local.local, local(2024, 3, 31, 0, 59, 59, 999_999_999));
    assert_eq!(before_local.offset_seconds, 0);

    let after_local = london.at(after).expect("transition instant is valid");
    assert_eq!(after_local.local, local(2024, 3, 31, 2, 0, 0, 0));
    assert_eq!(after_local.offset_seconds, 3_600);
}

#[test]
fn london_fall_back_reports_both_ambiguous_candidates() {
    let london = resolve_time_zone("Europe/London").expect("London is pinned");
    let repeated = local(2024, 10, 27, 1, 30, 0, 0);

    let resolution = london
        .resolve_local(repeated)
        .expect("ambiguous is a successful explicit resolution result");
    let LocalTimeResolution::Ambiguous { earlier, later } = resolution else {
        panic!("expected the 2024 fall-back overlap, got {resolution:?}");
    };

    assert_eq!(earlier, instant(1_729_989_000, 0));
    assert_eq!(later, instant(1_729_992_600, 0));

    let earlier_zoned = london.at(earlier).expect("earlier candidate is valid");
    assert_eq!(earlier_zoned.local, repeated);
    assert_eq!(earlier_zoned.offset_seconds, 3_600);

    let later_zoned = london.at(later).expect("later candidate is valid");
    assert_eq!(later_zoned.local, repeated);
    assert_eq!(later_zoned.offset_seconds, 0);
}

#[test]
fn resolver_rejects_unknown_and_unpinned_zones() {
    assert_error_kind(resolve_time_zone("America/New_York"), "UnknownZone");
    assert_error_kind(resolve_time_zone("Etc/UTC"), "UnknownZone");
}

#[test]
fn constructors_reject_invalid_civil_fields_and_nanoseconds() {
    assert_error_kind(Instant::new(0, 1_000_000_000), "InvalidInstant");
    assert_error_kind(
        LocalDateTime::new(2024, 1, 1, 0, 0, 0, 1_000_000_000),
        "InvalidLocalDateTime",
    );
    assert_error_kind(
        LocalDateTime::new(2024, 2, 30, 12, 0, 0, 0),
        "InvalidLocalDateTime",
    );
    assert_error_kind(
        LocalDateTime::new(2024, 13, 1, 12, 0, 0, 0),
        "InvalidLocalDateTime",
    );
    assert_error_kind(
        LocalDateTime::new(2024, 1, 1, 24, 0, 0, 0),
        "InvalidLocalDateTime",
    );
}

fn assert_canonical_offset_agreement(
    london: &TimeZone,
    instant_value: Instant,
    expected_offset: i32,
) {
    let zoned = london.at(instant_value).expect("instant is valid");
    assert_eq!(zoned.offset_seconds, expected_offset);

    let LocalTimeResolution::Unique {
        instant: resolved,
        offset_seconds,
    } = london
        .resolve_local(zoned.local)
        .expect("non-transition local time is unique")
    else {
        panic!("canonical local time unexpectedly changed resolution");
    };
    assert_eq!(resolved, instant_value);
    assert_eq!(offset_seconds, expected_offset);
}

#[test]
fn london_canonical_offset_agrees_in_both_standard_offsets() {
    let london = resolve_time_zone("Europe/London").expect("London is pinned");
    assert_canonical_offset_agreement(&london, instant(1_705_322_096, 987_654_321), 0);
    assert_canonical_offset_agreement(&london, instant(1_721_046_896, 987_654_321), 3_600);
}
