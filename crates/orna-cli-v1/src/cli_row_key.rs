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
pub(super) fn key_candidates(value: &str) -> Result<Vec<TypedKey>, Diagnostic> {
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
    Ok(candidates)
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
            key_candidates("song").unwrap(),
            vec![
                TypedKey::Text("song".to_owned()),
                TypedKey::Bytes(b"song".to_vec()),
            ]
        );
        assert_eq!(
            key_candidates("0x736f6e67").unwrap(),
            vec![
                TypedKey::Text("0x736f6e67".to_owned()),
                TypedKey::Bytes(b"0x736f6e67".to_vec()),
                TypedKey::Bytes(b"song".to_vec()),
            ]
        );
        // An odd digit count or a non-hex digit is not a byte key, and is
        // refused rather than silently truncated into a different key.
        assert!(key_candidates("0x736f6").is_err());
        assert!(key_candidates("0xzz").is_err());
        assert!(key_candidates("0x").is_err());
    }
}
