//! Calendar dates: the proleptic Gregorian day count the executor keeps, and the `YYYY-MM-DD` text the canonical Date
//! form stores, with the old evaluator's validity rules.

use orna_value::{DATE_TAG, Raw};

/// The `(year, month, day)` of a day count since 1970-01-01.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

/// The day count since 1970-01-01 of a proleptic Gregorian date. The date is valid: see [`parse_date_text`].
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The days in `month` of `year`, or `None` when the month is outside 1 to 12.
fn days_in_month(year: i64, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

/// The day count of a date written `YYYY-MM-DD`, when it is a real date in years 1 to 9999. This is the old
/// evaluator's `valid_date_literal` rule.
pub(crate) fn parse_date_text(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let year = text[0..4].parse::<i64>().ok()?;
    let month = text[5..7].parse::<u32>().ok()?;
    let day = text[8..10].parse::<u32>().ok()?;
    if !(1..=9_999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if !(1..=days_in_month(year, month)?).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// The canonical `YYYY-MM-DD` text of a day count. A day outside years 1 to 9999 has no canonical text.
pub(crate) fn date_text(days: i64) -> Result<String, &'static str> {
    let (year, month, day) = civil_from_days(days);
    if !(1..=9_999).contains(&year) {
        return Err("ORNA-EVAL-VALUE");
    }
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// The canonical raw form of a Date: a date tag over its text.
pub(crate) fn date_raw(days: i64) -> Result<Raw, &'static str> {
    Ok(Raw::Tag(DATE_TAG, Box::new(Raw::Text(date_text(days)?))))
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, date_text, days_from_civil, parse_date_text};

    #[test]
    fn epoch_and_known_dates_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(
            date_text(days_from_civil(2026, 10, 26)),
            Ok("2026-10-26".to_owned())
        );
    }

    #[test]
    fn valid_date_text_matches_the_old_rules() {
        assert_eq!(
            parse_date_text("2024-02-29"),
            Some(days_from_civil(2024, 2, 29))
        );
        assert_eq!(parse_date_text("2023-02-29"), None);
        assert_eq!(parse_date_text("2026-13-01"), None);
        assert_eq!(parse_date_text("0000-01-01"), None);
        assert_eq!(parse_date_text("2026-1-01"), None);
        assert_eq!(parse_date_text("+026-01-01"), None);
    }

    #[test]
    fn a_day_outside_years_one_to_9999_has_no_text() {
        assert_eq!(
            date_text(days_from_civil(10_000, 1, 1)),
            Err("ORNA-EVAL-VALUE")
        );
        assert_eq!(
            date_text(days_from_civil(0, 12, 31)),
            Err("ORNA-EVAL-VALUE")
        );
    }
}
