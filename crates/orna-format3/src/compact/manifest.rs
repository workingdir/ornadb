//! Canonical compact manifest and shard reader.
//!
//! The reader is handed bytes only. It validates the manifest header, the
//! shard records and the declared key bounds/event-time bounds, and returns
//! relative paths as plain strings. The caller owns object resolution and
//! converts those paths into its own managed type.

use std::{cmp::Ordering, collections::BTreeSet};

use orna_syntax::parse_row;

use orna_syntax_ast::{Expr, LiteralKind, RecordField};
use orna_value::{CanonicalValue, compare_primary_keys};
use uuid::Uuid;

use super::CompactError;

/// Profile marker of the one retained compact writer.
pub const COMPACT_PROFILE: &str = "compact-storage-v1";

/// Largest number of manifest entries one shard may declare.
pub const COMPACT_MANIFEST_SHARD_LIMIT: usize = 256;

/// The compact role of one immutable segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentRole {
    Data,
    Replacement,
    Deletion,
}

impl SegmentRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Replacement => "replacement",
            Self::Deletion => "deletion",
        }
    }

    pub fn parse(value: &str) -> Result<Self, CompactError> {
        match value {
            "data" => Ok(Self::Data),
            "replacement" => Ok(Self::Replacement),
            "deletion" => Ok(Self::Deletion),
            _ => Err(CompactError::InvalidManifest),
        }
    }
}

/// One shard record declared by a canonical manifest header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardHeader {
    /// Path relative to the table's compact root, e.g. `shards/00000000.orna`.
    pub file: String,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
    pub entries: usize,
    pub hash: [u8; 32],
}

/// A decoded canonical compact manifest header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestHeader {
    pub table: Uuid,
    pub schema: [u8; 32],
    pub next_generation: u64,
    pub shards: Vec<ShardHeader>,
}

/// One entry decoded from a canonical compact shard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardEntry {
    pub segment_id: Uuid,
    pub role: SegmentRole,
    pub generation: u64,
    pub schema: [u8; 32],
    pub encoder_version: String,
    /// Repository-relative path recorded by the shard.
    pub relative_path: String,
    pub git_object_id: String,
    pub sha256: [u8; 32],
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
    pub min_event_time: Option<String>,
    pub max_event_time: Option<String>,
    pub row_count: u64,
    pub compressed_bytes: u64,
    pub columns: Vec<u8>,
    pub row_group_index: bool,
    pub bloom: bool,
}

/// Decodes and validates the canonical compact manifest header.
pub fn parse_manifest_header(bytes: &[u8]) -> Result<ManifestHeader, CompactError> {
    let record = parse_record(bytes)?;
    reject_unknown(
        &record,
        &["profile", "table", "schema", "next_generation", "shards"],
    )?;
    if string_field(&record, "profile")? != COMPACT_PROFILE {
        return Err(CompactError::InvalidManifest);
    }
    let table = Uuid::parse_str(string_field(&record, "table")?)
        .map_err(|_| CompactError::InvalidManifest)?;
    let schema = parse_hex_32(string_field(&record, "schema")?)?;
    let next_generation = integer_field(&record, "next_generation")?;
    if next_generation == 0 {
        return Err(CompactError::InvalidManifest);
    }
    let mut shards = Vec::new();
    let mut expected_number = 0_u64;
    let mut expected_shard_paths = BTreeSet::new();
    for shard in list_field(&record, "shards")? {
        let shard = as_record(shard)?;
        reject_unknown(
            &shard,
            &["number", "min_key", "max_key", "entries", "file", "hash"],
        )?;
        let number = integer_field(&shard, "number")?;
        let declared_entries = usize::try_from(integer_field(&shard, "entries")?)
            .map_err(|_| CompactError::InvalidManifest)?;
        if number != expected_number
            || declared_entries == 0
            || declared_entries > COMPACT_MANIFEST_SHARD_LIMIT
        {
            return Err(CompactError::InvalidManifest);
        }
        expected_number = expected_number
            .checked_add(1)
            .ok_or(CompactError::InvalidManifest)?;
        let file = format!("shards/{number:08}.orna");
        if string_field(&shard, "file")? != file || !expected_shard_paths.insert(file.clone()) {
            return Err(CompactError::InvalidManifest);
        }
        let hash = parse_hex_32(string_field(&shard, "hash")?)?;
        let min_key = decode_hex(string_field(&shard, "min_key")?)?;
        let max_key = decode_hex(string_field(&shard, "max_key")?)?;
        shards.push(ShardHeader {
            file,
            min_key,
            max_key,
            entries: declared_entries,
            hash,
        });
    }
    Ok(ManifestHeader {
        table,
        schema,
        next_generation,
        shards,
    })
}

/// Decodes every entry of one canonical compact shard.
pub fn parse_shard(bytes: &[u8]) -> Result<Vec<ShardEntry>, CompactError> {
    let record = parse_record(bytes)?;
    reject_unknown(&record, &["entries"])?;
    let mut entries = Vec::new();
    for value in list_field(&record, "entries")? {
        let fields = as_record(value)?;
        reject_unknown(
            &fields,
            &[
                "segment_id",
                "role",
                "generation",
                "schema_id",
                "profile_version",
                "encoder_version",
                "relative_path",
                "git_object_id",
                "sha256",
                "min_key",
                "max_key",
                "min_event_time",
                "max_event_time",
                "row_count",
                "compressed_bytes",
                "columns",
                "row_group_index",
                "bloom",
            ],
        )?;
        if integer_field(&fields, "profile_version")? != 1 {
            return Err(CompactError::InvalidManifest);
        }
        let segment_id = Uuid::parse_str(string_field(&fields, "segment_id")?)
            .map_err(|_| CompactError::InvalidManifest)?;
        let min_event_time = nullable_string_field(&fields, "min_event_time")?;
        let max_event_time = nullable_string_field(&fields, "max_event_time")?;
        entries.push(ShardEntry {
            segment_id,
            role: SegmentRole::parse(string_field(&fields, "role")?)?,
            generation: integer_field(&fields, "generation")?,
            schema: parse_hex_32(string_field(&fields, "schema_id")?)?,
            encoder_version: string_field(&fields, "encoder_version")?.to_owned(),
            relative_path: string_field(&fields, "relative_path")?.to_owned(),
            git_object_id: string_field(&fields, "git_object_id")?.to_owned(),
            sha256: parse_hex_32(string_field(&fields, "sha256")?)?,
            min_key: decode_hex(string_field(&fields, "min_key")?)?,
            max_key: decode_hex(string_field(&fields, "max_key")?)?,
            min_event_time,
            max_event_time,
            row_count: integer_field(&fields, "row_count")?,
            compressed_bytes: integer_field(&fields, "compressed_bytes")?,
            columns: decode_base64(string_field(&fields, "columns")?)?,
            row_group_index: boolean_field(&fields, "row_group_index")?,
            bloom: boolean_field(&fields, "bloom")?,
        });
    }
    if entries.is_empty() || entries.len() > COMPACT_MANIFEST_SHARD_LIMIT {
        return Err(CompactError::InvalidManifest);
    }
    Ok(entries)
}

/// Orders two canonical key bytes by Orna primary-key comparison.
pub fn canonical_key_order(left: &[u8], right: &[u8]) -> Result<Ordering, CompactError> {
    let left_value = CanonicalValue::decode(left).map_err(|_| CompactError::InvalidManifest)?;
    let right_value = CanonicalValue::decode(right).map_err(|_| CompactError::InvalidManifest)?;
    compare_primary_keys(&left_value, &right_value).map_err(|_| CompactError::InvalidManifest)
}

/// Returns the canonical key extrema of `keys`, which must not be empty.
pub fn canonical_key_extrema<'a, I>(mut keys: I) -> Result<(&'a [u8], &'a [u8]), CompactError>
where
    I: Iterator<Item = &'a [u8]>,
{
    let first = keys.next().ok_or(CompactError::InvalidManifest)?;
    keys.try_fold((first, first), |(min, max), key| {
        let min = if canonical_key_order(key, min)? == Ordering::Less {
            key
        } else {
            min
        };
        let max = if canonical_key_order(key, max)? == Ordering::Greater {
            key
        } else {
            max
        };
        Ok::<_, CompactError>((min, max))
    })
}

/// Validates that an optional event-time pair is either absent or canonical
/// and ordered.
pub fn validate_event_time_bounds(
    min_event_time: Option<&str>,
    max_event_time: Option<&str>,
) -> Result<(), CompactError> {
    let (Some(min_event_time), Some(max_event_time)) = (min_event_time, max_event_time) else {
        return if min_event_time.is_none() && max_event_time.is_none() {
            Ok(())
        } else {
            Err(CompactError::InvalidManifest)
        };
    };
    let min = parse_canonical_utc_instant(min_event_time)?;
    let max = parse_canonical_utc_instant(max_event_time)?;
    if min > max {
        return Err(CompactError::InvalidManifest);
    }
    Ok(())
}

/// Reports whether `byte` is safe in a compact atom (encoder version, event
/// time and path segment).
pub fn is_safe_atom(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'+')
}

fn parse_canonical_utc_instant(value: &str) -> Result<(i64, u32), CompactError> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || !bytes.is_ascii()
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return Err(CompactError::InvalidManifest);
    }
    let component = |start: usize, end: usize| {
        bytes
            .get(start..end)
            .filter(|part| part.iter().all(u8::is_ascii_digit))
            .and_then(|part| std::str::from_utf8(part).ok())
            .and_then(|part| part.parse::<u32>().ok())
    };
    let (year, month, day, hour, minute, second) = (
        component(0, 4),
        component(5, 7),
        component(8, 10),
        component(11, 13),
        component(14, 16),
        component(17, 19),
    );
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) =
        (year, month, day, hour, minute, second)
    else {
        return Err(CompactError::InvalidManifest);
    };
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return Err(CompactError::InvalidManifest);
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let maximum_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(CompactError::InvalidManifest),
    };
    if !(1..=maximum_day).contains(&day) {
        return Err(CompactError::InvalidManifest);
    }

    let mut at = 19;
    let nanosecond = if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        let digits = at - start;
        if !(1..=9).contains(&digits) {
            return Err(CompactError::InvalidManifest);
        }
        let fraction = component(start, at).ok_or(CompactError::InvalidManifest)?;
        fraction * 10_u32.pow((9 - digits) as u32)
    } else {
        0
    };
    if bytes.get(at..).is_none_or(|tail| tail != b"Z") {
        return Err(CompactError::InvalidManifest);
    }

    let year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    let years_before = year - 1;
    let days_before_year =
        years_before * 365 + years_before / 4 - years_before / 100 + years_before / 400;
    let days_before_epoch = 1969 * 365 + 1969 / 4 - 1969 / 100 + 1969 / 400;
    let days_before_month = match month {
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
        _ => unreachable!("month validated above"),
    } + i64::from(month > 2 && leap);
    let days = days_before_year + days_before_month + day - 1 - days_before_epoch;
    let seconds =
        days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    Ok((seconds, nanosecond))
}

fn parse_record(bytes: &[u8]) -> Result<Vec<RecordField>, CompactError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CompactError::InvalidManifest)?;
    let parsed = parse_row(text);
    if !parsed.is_ok() {
        return Err(CompactError::InvalidManifest);
    }
    as_record(&parsed.value)
}

fn as_record(value: &Expr) -> Result<Vec<RecordField>, CompactError> {
    match value {
        Expr::Record { fields, .. } => Ok(fields.clone()),
        _ => Err(CompactError::InvalidManifest),
    }
}

fn reject_unknown(fields: &[RecordField], names: &[&str]) -> Result<(), CompactError> {
    let mut seen = BTreeSet::new();
    if fields
        .iter()
        .any(|field| !names.contains(&field.name.as_str()) || !seen.insert(field.name.as_str()))
    {
        return Err(CompactError::InvalidManifest);
    }
    Ok(())
}

fn field<'a>(fields: &'a [RecordField], name: &str) -> Result<&'a Expr, CompactError> {
    let mut values = fields
        .iter()
        .filter(|field| field.name == name)
        .map(|field| &field.value);
    let Some(value) = values.next() else {
        return Err(CompactError::InvalidManifest);
    };
    if values.next().is_some() {
        return Err(CompactError::InvalidManifest);
    }
    Ok(value)
}

fn string_field<'a>(fields: &'a [RecordField], name: &str) -> Result<&'a str, CompactError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::String,
        ..
    } = field(fields, name)?
    else {
        return Err(CompactError::InvalidManifest);
    };
    let value = text
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(CompactError::InvalidManifest)?;
    if value.contains('\\') {
        return Err(CompactError::InvalidManifest);
    }
    Ok(value)
}

fn integer_field(fields: &[RecordField], name: &str) -> Result<u64, CompactError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::Integer,
        ..
    } = field(fields, name)?
    else {
        return Err(CompactError::InvalidManifest);
    };
    text.parse().map_err(|_| CompactError::InvalidManifest)
}

fn nullable_string_field(
    fields: &[RecordField],
    name: &str,
) -> Result<Option<String>, CompactError> {
    match field(fields, name)? {
        Expr::Literal {
            text,
            kind: LiteralKind::Null,
            ..
        } if text == "null" => Ok(None),
        Expr::Literal {
            text,
            kind: LiteralKind::String,
            ..
        } => {
            let value = text
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .ok_or(CompactError::InvalidManifest)?;
            if value.is_empty() || value.contains('\\') || !value.bytes().all(is_safe_atom) {
                return Err(CompactError::InvalidManifest);
            }
            Ok(Some(value.to_owned()))
        }
        _ => Err(CompactError::InvalidManifest),
    }
}

fn boolean_field(fields: &[RecordField], name: &str) -> Result<bool, CompactError> {
    let Expr::Literal {
        text,
        kind: LiteralKind::Boolean,
        ..
    } = field(fields, name)?
    else {
        return Err(CompactError::InvalidManifest);
    };
    match text.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(CompactError::InvalidManifest),
    }
}

fn list_field<'a>(fields: &'a [RecordField], name: &str) -> Result<&'a [Expr], CompactError> {
    let Expr::List { elements, .. } = field(fields, name)? else {
        return Err(CompactError::InvalidManifest);
    };
    Ok(elements)
}

fn decode_hex(value: &str) -> Result<Vec<u8>, CompactError> {
    orna_base::hex_decode(value).map_err(|_| CompactError::InvalidManifest)
}

fn parse_hex_32(value: &str) -> Result<[u8; 32], CompactError> {
    decode_hex(value)?
        .try_into()
        .map_err(|_| CompactError::InvalidManifest)
}

fn decode_base64(value: &str) -> Result<Vec<u8>, CompactError> {
    orna_base::base64_decode(value).map_err(|_| CompactError::InvalidManifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_text(profile: &str) -> Vec<u8> {
        format!(
            "{{profile: \"{profile}\", table: \"00000000-0000-0000-0000-000000000001\", \
             schema: \"{}\", next_generation: 1, shards: []}}\n",
            "00".repeat(32)
        )
        .into_bytes()
    }

    fn shard_text(role: &str) -> Vec<u8> {
        format!(
            "{{entries: [{{segment_id: \"00000000-0000-0000-0000-000000000002\", \
             role: \"{role}\", generation: 1, schema_id: \"{}\", profile_version: 1, \
             encoder_version: \"v1\", relative_path: \".orna/storage/table/data/00/x.parquet\", \
             git_object_id: \"00\", sha256: \"{}\", min_key: \"00\", max_key: \"00\", \
             min_event_time: null, max_event_time: null, row_count: 1, compressed_bytes: 1, \
             columns: \"\", row_group_index: false, bloom: false}}]}}\n",
            "11".repeat(32),
            "22".repeat(32)
        )
        .into_bytes()
    }

    #[test]
    fn manifest_header_decodes_the_declared_coordinates() {
        let header = parse_manifest_header(&manifest_text(COMPACT_PROFILE)).unwrap();
        assert_eq!(header.next_generation, 1);
        assert_eq!(header.schema, [0; 32]);
        assert!(header.shards.is_empty());
    }

    #[test]
    fn manifest_header_rejects_another_profile() {
        assert_eq!(
            parse_manifest_header(&manifest_text("loose-storage-v1")),
            Err(CompactError::InvalidManifest)
        );
    }

    #[test]
    fn shard_entry_decodes_every_declared_field() {
        let entries = parse_shard(&shard_text("data")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, SegmentRole::Data);
        assert_eq!(entries[0].generation, 1);
        assert_eq!(entries[0].row_count, 1);
        assert_eq!(entries[0].compressed_bytes, 1);
        assert_eq!(
            entries[0].relative_path,
            ".orna/storage/table/data/00/x.parquet"
        );
    }

    #[test]
    fn shard_entry_rejects_an_unknown_role() {
        assert_eq!(
            parse_shard(&shard_text("shadow")),
            Err(CompactError::InvalidManifest)
        );
    }
}
