//! Validation of raw OVB values against the tag registry.

use crate::cbor::encode_raw;
use crate::decimal::Decimal;
use crate::error::Error;
use crate::identity::uuid_array;
use crate::primary_key::is_permitted_key;
use crate::raw::{Raw, array, bytes, int_u64, integer, is_nan_bits, map, text};
use crate::schema::{validate_type, validate_value_as_type};
use crate::snapshot::Snapshot;
use crate::tags::{
    DATE_TAG, DECIMAL_TAG, DIAGNOSTIC_TAG, DIGEST_TAG, DURATION_TAG, ENUM_TAG, ERROR_TAG,
    INSTANT_TAG, LIVE_HANDLE_TAG, LOCAL_DATE_TIME_TAG, MONEY_TAG, OPAQUE_SYSTEM_ID_TAG, OPTION_TAG,
    PATH_TAG, PINNED_ROW_REFERENCE_TAG, PRESENT_NODE_TAG, QUANTITY_TAG, RANGE_TAG, RECORD_TAG,
    REFINED_NOMINAL_TAG, RELATION_TAG, STORED_TABLE_REFERENCE_TAG, SYS_VALUE_BOX_TAG,
    TIME_OF_DAY_TAG, TIME_ZONE_TAG, TUPLE_TAG, UNIT_TAG, UUID_TAG, ZONED_DATE_TIME_TAG,
};
use crate::{CANONICAL_NAN_BITS, MAX_DEPTH, Result};
use num_bigint::BigInt;
use num_bigint::Sign;
use num_traits::Zero;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

pub(crate) fn validate_raw(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    match v {
        // Resolved secrets are not portable values. Reject this marker at
        // every depth before values can enter snapshots, digests, or wire
        // bytes; schema and typed wrappers do not grant disclosure authority.
        Raw::Tag(0, _) => return Err(Error::ProtectedValue),
        Raw::Float(bits) if is_nan_bits(*bits) && *bits != CANONICAL_NAN_BITS => {
            return Err(Error::NonCanonical);
        }
        Raw::Array(a) => {
            for x in a {
                validate_raw(x, depth + 1)?
            }
        }
        Raw::Map(m) => {
            let mut previous = None;
            for (k, v) in m {
                validate_raw(k, depth + 1)?;
                validate_raw(v, depth + 1)?;
                let b = encode_raw(k)?;
                if previous.as_ref().is_some_and(|x: &Vec<u8>| b <= *x) {
                    return Err(Error::DuplicateOrUnorderedMapKey);
                }
                previous = Some(b)
            }
        }
        Raw::Tag(n, x) => {
            validate_tag(*n, x)?;
            validate_raw(x, depth + 1)?
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn validate_tag(n: u64, v: &Raw) -> Result<()> {
    match n {
        UUID_TAG => {
            if bytes(v)?.len() != 16 {
                return Err(Error::InvalidTag);
            }
        }
        DECIMAL_TAG => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let (c, e) = (integer(&a[0])?, integer(&a[1])?);
            let decimal = Decimal::try_new(c.clone(), e.clone())?;
            if decimal.coefficient() != c || decimal.exponent10() != e {
                return Err(Error::NonCanonical);
            }
        }
        DATE_TAG => {
            validate_date(text(v)?)?;
        }
        LOCAL_DATE_TIME_TAG => {
            let s = text(v)?;
            validate_local_date_time(s)?;
        }
        TIME_ZONE_TAG => {
            let s = text(v)?;
            if s.is_empty()
                || s.as_bytes()
                    .iter()
                    .any(|b| !matches!(*b, b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'_'|b'-'|b'+'|b'/'))
            {
                return Err(Error::InvalidTag);
            }
        }
        INSTANT_TAG | DURATION_TAG => {
            let a = array(v)?;
            if a.len() != 2 || !matches!(a[0], Raw::Int(_)) || int_u64(&a[1])? >= 1_000_000_000 {
                return Err(Error::InvalidTag);
            }
        }
        QUANTITY_TAG => {
            let a = array(v)?;
            if a.len() != 2
                || !matches!(a[0], Raw::Int(_) | Raw::Float(_) | Raw::Tag(DECIMAL_TAG, _))
            {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[1])?;
        }
        MONEY_TAG => {
            let a = array(v)?;
            if a.len() != 2 || !matches!(a[0], Raw::Tag(DECIMAL_TAG, _)) {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[1])?;
        }
        ENUM_TAG => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
            if !matches!(a[2], Raw::Null | Raw::Tag(RECORD_TAG, _)) {
                return Err(Error::InvalidTag);
            }
        }
        RECORD_TAG => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            if !matches!(a[0], Raw::Null | Raw::Tag(UUID_TAG, _)) {
                return Err(Error::InvalidTag);
            }
            validate_fields(&a[1])?;
        }
        PINNED_ROW_REFERENCE_TAG => {
            let a = array(v)?;
            if a.len() != 4 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
            Snapshot::decode(&a[3])?;
        }
        OPTION_TAG => {
            let a = array(v)?;
            match a.as_slice() {
                [Raw::Int(x)] if x.is_zero() => {}
                [Raw::Int(x), _] if *x == BigInt::from(1) => {}
                _ => return Err(Error::InvalidTag),
            }
        }
        TUPLE_TAG => {
            if array(v)?.is_empty() {
                return Err(Error::NonCanonical);
            }
        }
        ZONED_DATE_TIME_TAG => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            if !matches!(a[0], Raw::Tag(INSTANT_TAG, _))
                || !matches!(a[1], Raw::Tag(TIME_ZONE_TAG, _))
            {
                return Err(Error::InvalidTag);
            }
            let offset = integer(&a[2])?;
            // Exact zone-database agreement is contextual: this pure codec can
            // validate the pinned shape and legal offset range only. The
            // snapshot's recorded tzdata resolver must confirm the offset.
            if offset < &BigInt::from(-64_800) || offset > &BigInt::from(64_800) {
                return Err(Error::InvalidTag);
            }
        }
        RANGE_TAG => {
            let a = array(v)?;
            if a.len() != 3
                || !matches!(a[0], Raw::Tag(OPTION_TAG, _))
                || !matches!(a[1], Raw::Tag(OPTION_TAG, _))
                || !matches!(a[2], Raw::Bool(_))
            {
                return Err(Error::InvalidTag);
            }
        }
        REFINED_NOMINAL_TAG => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
        }
        STORED_TABLE_REFERENCE_TAG => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
            // ORNA-KEY-003/005 (07-tables.md:283-287): a reference key component
            // must be a permitted key value, on decode as well as construction.
            if !is_permitted_key(&a[2]) {
                return Err(Error::InvalidTag);
            }
        }
        LIVE_HANDLE_TAG => {
            let a = array(v)?;
            if a.len() != 4 || !matches!(a[3], Raw::Bool(false)) {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
        }
        OPAQUE_SYSTEM_ID_TAG => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let name = text(&a[0])?;
            match name {
                value if UUID_SYSTEM_IDS.contains(&value) => {
                    uuid_array(&a[1])?;
                }
                "sys.RevisionId" | "sys.SnapshotId" | "sys.ConsumerIdentity" => {
                    if bytes(&a[1])?.len() != 32 {
                        return Err(Error::InvalidTag);
                    }
                }
                "sys.CheckpointVersion" | "sys.FailureVersion" => {
                    if integer(&a[1])?.sign() == Sign::Minus {
                        return Err(Error::InvalidTag);
                    }
                }
                "sys.GitOid" => {
                    let git = array(&a[1])?;
                    if git.len() != 2 {
                        return Err(Error::InvalidTag);
                    }
                    let size = match text(&git[0])? {
                        "sha1" => 20,
                        "sha256" => 32,
                        _ => return Err(Error::InvalidTag),
                    };
                    if bytes(&git[1])?.len() != size {
                        return Err(Error::InvalidTag);
                    }
                }
                _ => return Err(Error::InvalidTag),
            }
        }
        PATH_TAG => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let flavour = text(&a[0])?;
            let value = text(&a[1])?;
            if !matches!(flavour, "repo" | "posix" | "windows")
                || (flavour == "repo"
                    && (!value.is_ascii()
                        || value.starts_with('/')
                        || value.contains('\\')
                        || value
                            .split('/')
                            .any(|x| x.is_empty() || x == "." || x == "..")))
            {
                return Err(Error::InvalidTag);
            }
        }
        DIGEST_TAG => {
            let a = array(v)?;
            if a.len() != 2 || text(&a[0])? != "sha256" || bytes(&a[1])?.len() != 32 {
                return Err(Error::InvalidTag);
            }
        }
        SYS_VALUE_BOX_TAG => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            validate_type(&a[0])?;
            validate_value_as_type(&a[1], &a[0])?;
        }
        RELATION_TAG => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let ordered = match a[0] {
                Raw::Bool(value) => value,
                _ => return Err(Error::InvalidTag),
            };
            let rows = array(&a[1])?;
            if !ordered {
                let mut previous = None;
                for row in rows {
                    let encoded = encode_raw(row)?;
                    if previous.as_ref().is_some_and(|x: &Vec<u8>| encoded < *x) {
                        return Err(Error::NonCanonical);
                    }
                    previous = Some(encoded);
                }
            }
        }
        DIAGNOSTIC_TAG => validate_diagnostic(v, 0)?,
        PRESENT_NODE_TAG => validate_present(v, 0)?,
        ERROR_TAG => validate_error(v, 0)?,
        UNIT_TAG => {
            if !array(v)?.is_empty() {
                return Err(Error::InvalidTag);
            }
        }
        TIME_OF_DAY_TAG => {
            if int_u64(v)? >= 86_400_000_000_000 {
                return Err(Error::InvalidTag);
            }
        }
        _ => return Err(Error::InvalidTag),
    }
    Ok(())
}

pub(crate) fn integer_map<'a>(
    v: &'a Raw,
    required: &[u64],
    optional: &[u64],
) -> Result<BTreeMap<u64, &'a Raw>> {
    let mut result = BTreeMap::new();
    for (key, value) in map(v)? {
        let key = int_u64(key)?;
        if !required.contains(&key) && !optional.contains(&key)
            || result.insert(key, value).is_some()
        {
            return Err(Error::InvalidTag);
        }
    }
    if required.iter().any(|key| !result.contains_key(key)) {
        return Err(Error::InvalidTag);
    }
    Ok(result)
}

fn validate_diagnostic(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let fields = integer_map(v, &[0, 1, 2, 3, 4, 5, 6], &[7])?;
    text(fields[&0])?;
    if int_u64(fields[&1])? > 4 {
        return Err(Error::InvalidTag);
    }
    text(fields[&2])?;
    for span in array(fields[&3])? {
        let span = array(span)?;
        if span.len() != 4 {
            return Err(Error::InvalidTag);
        }
        Snapshot::decode(&span[0])?;
        let path = text(&span[1])?;
        if !is_safe_diagnostic_path(path) {
            return Err(Error::InvalidTag);
        }
        let start = int_u64(&span[2])?;
        if int_u64(&span[3])? < start {
            return Err(Error::InvalidTag);
        }
    }
    for note in array(fields[&4])? {
        text(note)?;
    }
    for cause in array(fields[&5])? {
        if let Raw::Tag(DIAGNOSTIC_TAG, inner) = cause {
            validate_diagnostic(inner, depth + 1)?;
        } else {
            return Err(Error::InvalidTag);
        }
    }
    if !matches!(fields[&6], Raw::Bool(_)) {
        return Err(Error::InvalidTag);
    }
    if let Some(id) = fields.get(&7) {
        uuid_array(id)?;
    }
    Ok(())
}

/// Returns whether a diagnostic span path is safe outside trusted developer
/// inspection. The protocol permits repository-relative UTF-8 paths or the
/// explicit redaction marker, never host-specific path syntax.
fn is_safe_diagnostic_path(path: &str) -> bool {
    if path == "<redacted>" {
        return true;
    }

    let bytes = path.as_bytes();
    if bytes.is_empty()
        || bytes[0] == b'/'
        || bytes.contains(&b'\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path.chars().any(char::is_control)
    {
        return false;
    }

    !path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
}

pub(crate) fn validate_error(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let fields = integer_map(v, &[0, 1, 2, 3], &[])?;
    text(fields[&0])?;
    text(fields[&1])?;
    for cause in array(fields[&2])? {
        if let Raw::Tag(ERROR_TAG, inner) = cause {
            validate_error(inner, depth + 1)?;
        } else {
            return Err(Error::InvalidTag);
        }
    }
    for (key, value) in map(fields[&3])? {
        text(key)?;
        validate_raw(value, depth + 1)?;
    }
    Ok(())
}

fn validate_present(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let a = array(v)?;
    if a.len() != 4 {
        return Err(Error::InvalidTag);
    }
    if !matches!(a[0], Raw::Text(_) | Raw::Tag(UUID_TAG, _)) {
        return Err(Error::InvalidTag);
    }
    if !matches!(a[1], Raw::Null) {
        let key = array(&a[1])?;
        match key.as_slice() {
            [Raw::Int(kind), Raw::Text(_) | Raw::Tag(UUID_TAG, _)] if *kind == BigInt::from(0) => {}
            [Raw::Int(kind), table, _] if *kind == BigInt::from(1) => {
                let _ = uuid_array(table)?;
            }
            [Raw::Int(kind), _] if *kind == BigInt::from(3) => {}
            _ => return Err(Error::InvalidTag),
        }
    }
    for (key, value) in map(&a[2])? {
        if !matches!(key, Raw::Text(_) | Raw::Tag(UUID_TAG, _)) {
            return Err(Error::InvalidTag);
        }
        validate_raw(value, depth + 1)?;
    }
    let mut keys = BTreeSet::new();
    for child in array(&a[3])? {
        let Raw::Tag(PRESENT_NODE_TAG, node) = child else {
            return Err(Error::InvalidTag);
        };
        validate_present(node, depth + 1)?;
        let child_fields = array(node)?;
        if !matches!(child_fields[1], Raw::Null) {
            let k = encode_raw(&child_fields[1])?;
            if !keys.insert(k) {
                return Err(Error::DuplicateOrUnorderedMapKey);
            }
        }
    }
    Ok(())
}

const UUID_SYSTEM_IDS: [&str; 15] = [
    "sys.DatabaseId",
    "sys.FileId",
    "sys.DefinitionId",
    "sys.ObjectId",
    "sys.RuntimeId",
    "sys.TransactionId",
    "sys.InvocationId",
    "sys.RunId",
    "sys.QueryId",
    "sys.SessionId",
    "sys.ClientId",
    "sys.TraceId",
    "sys.SpanId",
    "sys.SegmentId",
    "sys.BuildId",
];

fn validate_fields(v: &Raw) -> Result<()> {
    let fields = array(v)?;
    let mut prior = None;
    for f in fields {
        let p = array(f)?;
        if p.len() != 2 {
            return Err(Error::InvalidTag);
        }
        match &p[0] {
            Raw::Text(name) if name.nfc().eq(name.chars()) => {}
            Raw::Tag(UUID_TAG, _) => {
                uuid_array(&p[0])?;
            }
            _ => return Err(Error::InvalidTag),
        }
        let k = encode_raw(&p[0])?;
        if prior.as_ref().is_some_and(|x: &Vec<u8>| k <= *x) {
            return Err(Error::DuplicateOrUnorderedMapKey);
        }
        prior = Some(k);
    }
    Ok(())
}

fn validate_date(s: &str) -> Result<()> {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, x)| matches!(i, 4 | 7) || x.is_ascii_digit())
    {
        return Err(Error::InvalidTag);
    }
    let y = s[..4].parse::<i32>().map_err(|_| Error::InvalidTag)?;
    if !(1..=9999).contains(&y) {
        return Err(Error::InvalidTag);
    }
    let m = s[5..7].parse::<u32>().map_err(|_| Error::InvalidTag)?;
    let d = s[8..].parse::<u32>().map_err(|_| Error::InvalidTag)?;
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        _ => return Err(Error::InvalidTag),
    };
    if d == 0 || d > max {
        return Err(Error::InvalidTag);
    }
    Ok(())
}

fn validate_local_date_time(s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    if bytes.len() != 29
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || !bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        })
    {
        return Err(Error::InvalidTag);
    }
    validate_date(&s[..10])?;
    let hour = s[11..13].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    let minute = s[14..16].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    let second = s[17..19].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    if hour > 23 || minute > 59 || second > 59 {
        return Err(Error::InvalidTag);
    }
    Ok(())
}

#[cfg(test)]
mod field_key_tests {
    use crate::*;

    #[test]
    fn records_reject_noncanonical_field_key_kinds_and_accept_text_or_uuid() {
        let record = |key: Raw| {
            tag(
                RECORD_TAG,
                Raw::Array(vec![
                    Raw::Null,
                    Raw::Array(vec![Raw::Array(vec![key, Raw::Int(1.into())])]),
                ]),
            )
        };

        for key in [
            Raw::Int(1.into()),
            Raw::Bool(false),
            Raw::Bytes(vec![1]),
            Raw::Text("e\u{301}".into()),
        ] {
            let raw = record(key);
            assert_eq!(Value::new(raw.clone()), Err(Error::InvalidTag));
            let mut bytes = Vec::new();
            write_raw(&raw, &mut bytes).unwrap();
            assert_eq!(Value::decode(&bytes), Err(Error::InvalidTag));
        }

        assert!(Value::new(record(Raw::Text("name".into()))).is_ok());
        assert!(Value::new(record(uuid_raw([7; 16]))).is_ok());
    }
}
