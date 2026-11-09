//! Lowercase hexadecimal text: the one spelling of object IDs, digests and
//! record identities used across this crate.

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hexadecimal spelling of `bytes`.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Whether `text` is spelled exactly as [`hex_encode`] would spell it: even
/// length and lowercase digits only. Readers use this so a record has one
/// accepted form rather than one per case or radix spelling.
pub(crate) fn is_hex(text: &str) -> bool {
    text.len().is_multiple_of(2) && text.bytes().all(|byte| digit(byte).is_some())
}

/// The inverse of [`hex_encode`]: exactly its lowercase, even-length
/// spelling, so every byte string has one accepted form.
pub(crate) fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let (pairs, []) = text.as_bytes().as_chunks::<2>() else {
        return None;
    };
    pairs
        .iter()
        .map(|[high, low]| Some((digit(*high)? << 4) | digit(*low)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's readers accept exactly the spelling the writers produce:
    /// lowercase, even length. Case, radix prefixes/signs and odd lengths
    /// have no second spelling.
    #[test]
    fn decode_accepts_only_the_canonical_spelling() {
        assert_eq!(hex_decode("00ff"), Some(vec![0x00, 0xff]));
        assert_eq!(hex_decode(""), Some(Vec::new()));
        for noncanonical in ["00FF", "0A", "+f", "0x00", "0f ", "0", "\u{0660}0"] {
            assert_eq!(hex_decode(noncanonical), None, "{noncanonical:?}");
            assert!(!is_hex(noncanonical), "{noncanonical:?}");
        }
        assert!(is_hex(""));
        assert!(is_hex("00ff"));
    }
}
