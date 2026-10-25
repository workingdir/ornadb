//! Resolving a caller's `KEY_TEXT` spelling to the row keys it can name.
//!
//! The CLI parses a row key as one canonical Orna key expression, not a host
//! path (ORNA-CLI-005). A row committed from imported media is keyed by the
//! raw bytes the capture was given, while a row written by hand may be keyed
//! by canonical text, and the same spelling is printed for both: a listing
//! renders a byte key that is UTF-8 as its own text. One command therefore
//! tries a spelling as canonical text, then as those same bytes, then — for
//! the `0x` form — as the bytes the hex digits spell.
//!
//! Every command that names one committed row by key resolves it here, so the
//! observed spelling is the same from `orna query` and `orna history`: a row a
//! listing printed under `KEY_TEXT` is a row either verb can name.

use orna_repository_v1::TypedKey;

use super::Diagnostic;

/// Every row key one `KEY_TEXT` spelling can name, in the order to try them.
///
/// `song` names canonical text `song` and then the bytes `song`; `0x736f6e67`
/// additionally names the bytes those digits spell. The candidates are tried in
/// order by the caller, so the first row that answers is the one the spelling
/// most directly names.
///
/// When the committed descriptor declares the primary key as one `Int` or
/// `UInt` component, the spelling is parsed as that type only: an integer key
/// is never reinterpreted as text or bytes.
pub(super) fn key_candidates(
    value: &str,
    key_type: Option<&str>,
) -> Result<Vec<TypedKey>, Diagnostic> {
    match key_type {
        Some("Int") => {
            return value
                .parse::<i64>()
                .map(|number| vec![TypedKey::Int(number)])
                .map_err(|_| {
                    key_error(
                        "Row key is not an Int key",
                        format!("got {value:?}; the table's primary key is Int"),
                    )
                });
        }
        Some("UInt") => {
            return value
                .parse::<u64>()
                .map(|number| vec![TypedKey::UInt(number)])
                .map_err(|_| {
                    key_error(
                        "Row key is not a UInt key",
                        format!("got {value:?}; the table's primary key is UInt"),
                    )
                });
        }
        _ => {}
    }
    let mut candidates = vec![
        TypedKey::Text(value.to_owned()),
        TypedKey::Bytes(value.as_bytes().to_vec()),
    ];
    if let Some(digits) = value.strip_prefix("0x") {
        if digits.is_empty()
            || !digits.len().is_multiple_of(2)
            || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(key_error(
                "Row key is not a byte key",
                format!("got {value:?}; a byte key is 0x followed by an even number of hex digits"),
            ));
        }
        let bytes = digits
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(std::str::from_utf8(pair).expect("hex digits are ASCII"), 16)
                    .expect("hexadecimal digits were checked above")
            })
            .collect();
        candidates.push(TypedKey::Bytes(bytes));
    }
    // An integer spelling also names the integer key it denotes. Text and
    // bytes are tried first, so a row keyed by the spelling itself still wins.
    if let Ok(number) = value.parse::<i64>() {
        candidates.push(TypedKey::Int(number));
    } else if let Ok(number) = value.parse::<u64>() {
        candidates.push(TypedKey::UInt(number));
    }
    Ok(candidates)
}

/// The typed refusal for a composite (tuple) primary key. Point keys and
/// predicate scans over such a table need the successor API tracked by
/// `ornadb-5il1.461`, which does not exist yet.
pub(super) fn composite_key_error() -> Diagnostic {
    key_error(
        "Composite primary keys are not supported by ornadb-5il1.461 yet",
        "the table's primary key has more than one component; tuple key lookup and \
         paged predicate scans need the row-map successor API tracked by bead ornadb-5il1.461",
    )
}

fn key_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "spell a text key as KEY_TEXT and a byte key as 0x followed by its hex digits",
        detail.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::key_candidates;
    use orna_repository_v1::TypedKey;

    #[test]
    fn key_spelling_names_the_text_bytes_and_hex_candidates() {
        // A listing prints a byte key that is UTF-8 as its text, so a key
        // argument must find that row by the printed spelling as well as by its
        // own bytes.
        assert_eq!(
            key_candidates("song", None).unwrap(),
            vec![
                TypedKey::Text("song".to_owned()),
                TypedKey::Bytes(b"song".to_vec()),
            ]
        );
        assert_eq!(
            key_candidates("0x736f6e67", None).unwrap(),
            vec![
                TypedKey::Text("0x736f6e67".to_owned()),
                TypedKey::Bytes(b"0x736f6e67".to_vec()),
                TypedKey::Bytes(b"song".to_vec()),
            ]
        );
        // An odd digit count or a non-hex digit is not a byte key, and is
        // refused rather than silently truncated into a different key.
        assert!(key_candidates("0x736f6", None).is_err());
        assert!(key_candidates("0xzz", None).is_err());
        assert!(key_candidates("0x", None).is_err());
    }

    #[test]
    fn a_schema_typed_integer_key_is_parsed_as_that_type_only() {
        // An Int primary key is named as Int and never as text or bytes, so a
        // row keyed `42` is found and `forty-two` is refused, not reinterpreted.
        assert_eq!(
            key_candidates("42", Some("Int")).unwrap(),
            vec![TypedKey::Int(42)]
        );
        assert_eq!(
            key_candidates("-7", Some("Int")).unwrap(),
            vec![TypedKey::Int(-7)]
        );
        assert!(key_candidates("forty-two", Some("Int")).is_err());
        assert_eq!(
            key_candidates("42", Some("UInt")).unwrap(),
            vec![TypedKey::UInt(42)]
        );
        assert!(key_candidates("-7", Some("UInt")).is_err());
    }

    #[test]
    fn a_composite_key_is_refused_with_the_successor_bead() {
        let error = super::composite_key_error();
        assert!(format!("{error:?}").contains("ornadb-5il1.461"));
    }
}
