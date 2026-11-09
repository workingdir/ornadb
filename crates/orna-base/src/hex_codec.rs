//! The workspace's one hexadecimal codec.

use crate::CodecError;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Encodes `bytes` as lowercase hexadecimal, two digits per byte.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Decodes the canonical hexadecimal spelling: lowercase digits in an even
/// number of pairs. Uppercase, whitespace, `0x` prefixes and odd lengths are
/// rejected, so a decoded value has exactly one accepted text form.
pub fn decode(text: &str) -> Result<Vec<u8>, CodecError> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(CodecError::InvalidHex);
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[high, low]| Some((canonical_digit(*high)? << 4) | canonical_digit(*low)?))
        .collect::<Option<Vec<u8>>>()
        .ok_or(CodecError::InvalidHex)
}

/// The value of one hexadecimal digit of either case, for callers that accept
/// the wider human spelling (UUID text and `%XX` escapes) and validate their
/// own canonical form afterwards.
#[must_use]
pub const fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

const fn canonical_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_is_lowercase_and_zero_padded() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn decode_accepts_only_the_canonical_spelling() {
        assert_eq!(decode("000fa0ff"), Ok(vec![0x00, 0x0f, 0xa0, 0xff]));
        assert_eq!(decode(""), Ok(Vec::new()));
        for rejected in ["0", "0g", "FF", "0x00", "0 0", "00\n"] {
            assert_eq!(
                decode(rejected),
                Err(CodecError::InvalidHex),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn digit_accepts_either_case() {
        for (byte, value) in [
            ('0', 0),
            ('9', 9),
            ('a', 10),
            ('f', 15),
            ('A', 10),
            ('F', 15),
        ] {
            assert_eq!(digit(byte as u8), Some(value));
        }
        assert_eq!(digit(b'g'), None);
        assert_eq!(digit(b'/'), None);
    }
}
