use crate::*;

pub(crate) fn parse_local_datetime(text: &str) -> Option<LocalDateTime> {
    // Keep this wire spelling intentionally narrow: ISO local civil seconds
    // plus an optional decimal fraction, without an offset or host locale.
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => {
            if fraction.is_empty()
                || fraction.len() > 9
                || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            (whole, Some(fraction))
        }
        None => (text, None),
    };
    if whole.len() != 19
        || whole.as_bytes()[4] != b'-'
        || whole.as_bytes()[7] != b'-'
        || whole.as_bytes()[10] != b'T'
        || whole.as_bytes()[13] != b':'
        || whole.as_bytes()[16] != b':'
        || !whole
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
    {
        return None;
    }
    let parse = |range: std::ops::Range<usize>| whole[range].parse::<u32>().ok();
    let year = i32::try_from(parse(0..4)?).ok()?;
    let month = u8::try_from(parse(5..7)?).ok()?;
    let day = u8::try_from(parse(8..10)?).ok()?;
    let hour = u8::try_from(parse(11..13)?).ok()?;
    let minute = u8::try_from(parse(14..16)?).ok()?;
    let second = u8::try_from(parse(17..19)?).ok()?;
    let nanosecond = fraction.map_or(Some(0), |fraction| {
        format!("{fraction:0<9}").parse::<u32>().ok()
    })?;
    LocalDateTime::new(year, month, day, hour, minute, second, nanosecond).ok()
}

pub(crate) fn valid_date_literal(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = value[0..4].parse::<u32>().ok();
    let month = value[5..7].parse::<u32>().ok();
    let day = value[8..10].parse::<u32>().ok();
    let Some((year, month, day)) = year
        .zip(month)
        .zip(day)
        .map(|((year, month), day)| (year, month, day))
    else {
        return false;
    };
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let maximum = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=maximum).contains(&day)
}

/// Parses an Instant literal into the OVB-1 representation defined by the
/// immutable format profile.  The stored seconds are floor-normalised UTC
/// seconds, so fractions before the Unix epoch retain a nonnegative remainder.
pub(crate) fn parse_instant_literal(value: &str) -> Option<(i64, u32)> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || !bytes.is_ascii()
        || !valid_date_literal(value.get(..10)?)
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let component = |start, end| {
        bytes
            .get(start..end)
            .filter(|part| part.iter().all(u8::is_ascii_digit))
            .and_then(|part| std::str::from_utf8(part).ok())
            .and_then(|part| part.parse::<u32>().ok())
    };
    let (hour, minute, second) = (component(11, 13)?, component(14, 16)?, component(17, 19)?);
    if hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }

    let mut at = 19;
    let nanosecond = if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        let digits = at.checked_sub(start)?;
        if !(1..=9).contains(&digits) {
            return None;
        }
        let fraction = component(start, at)?;
        fraction.checked_mul(10u32.pow((9 - digits) as u32))?
    } else {
        0
    };

    let offset_seconds = match bytes.get(at) {
        Some(b'Z') if at + 1 == bytes.len() => 0i64,
        Some(b'+' | b'-') if at + 6 == bytes.len() && bytes.get(at + 3) == Some(&b':') => {
            let hours = component(at + 1, at + 3)?;
            let minutes = component(at + 4, at + 6)?;
            if hours > 23 || minutes >= 60 {
                return None;
            }
            let seconds = i64::from(hours) * 3_600 + i64::from(minutes) * 60;
            if bytes[at] == b'+' { seconds } else { -seconds }
        }
        _ => return None,
    };

    let year = component(0, 4)?;
    let month = component(5, 7)?;
    let day = component(8, 10)?;
    let days = days_since_unix_epoch(year, month, day)?;
    let local_seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))?;
    local_seconds
        .checked_sub(offset_seconds)
        .map(|seconds| (seconds, nanosecond))
}

fn days_since_unix_epoch(year: u32, month: u32, day: u32) -> Option<i64> {
    if !valid_date_literal(&format!("{year:04}-{month:02}-{day:02}")) {
        return None;
    }
    let year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    let leap_days = |year: i64| year / 4 - year / 100 + year / 400;
    let years_before = year - 1;
    let days_before_year = years_before * 365 + leap_days(years_before);
    let days_before_month =
        match month {
            1 => 0,
            2 => 31,
            3 => 59,
            4 => 90,
            5 => 120,
            6 => 151,
            7 => 181,
            8 => 212,
            9 => 243,
            10 => 273,
            11 => 304,
            12 => 334,
            _ => return None,
        } + if month > 2 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            1
        } else {
            0
        };
    let unix_epoch_days = 719_162;
    Some(days_before_year + days_before_month + day - 1 - unix_epoch_days)
}
