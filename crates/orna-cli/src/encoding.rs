//! Byte and text codecs the verbs share: lowercase hex and percent decoding.

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

/// The value of one ASCII hex digit of either case.
pub(crate) fn hex_digit(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// Decodes the `%XX` escapes of `value` and, for a form field
/// (`plus_as_space`), the `+` that stands for a space. `None` for a malformed
/// escape or a result that is not UTF-8.
pub(crate) fn percent_decode(value: &str, plus_as_space: bool) -> Option<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut rest = value.bytes();
    while let Some(byte) = rest.next() {
        match byte {
            b'+' if plus_as_space => decoded.push(b' '),
            b'%' => {
                let high = hex_digit(rest.next()?)?;
                let low = hex_digit(rest.next()?)?;
                decoded.push((high << 4) | low);
            }
            other => decoded.push(other),
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::{hex, hex_digit, percent_decode};

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn hex_digit_accepts_either_case_and_nothing_else() {
        for (byte, value) in [(b'0', 0), (b'9', 9), (b'a', 10), (b'F', 15), (b'f', 15)] {
            assert_eq!(hex_digit(byte), Some(value), "{}", char::from(byte));
        }
        for byte in [b'g', b'G', b' ', b'%', 0xc3, 0xff] {
            assert_eq!(hex_digit(byte), None, "{byte:#x}");
        }
    }

    #[test]
    fn percent_decode_resolves_escapes_of_either_case() {
        assert_eq!(
            percent_decode("a%20b%2fc%2Fd", false).as_deref(),
            Some("a b/c/d")
        );
        assert_eq!(percent_decode("%C3%A9", false).as_deref(), Some("\u{e9}"));
    }

    #[test]
    fn plus_is_a_space_only_in_form_fields() {
        assert_eq!(percent_decode("a+b", true).as_deref(), Some("a b"));
        assert_eq!(percent_decode("a+b", false).as_deref(), Some("a+b"));
        assert_eq!(percent_decode("a%2Bb", true).as_deref(), Some("a+b"));
    }

    #[test]
    fn malformed_escapes_and_invalid_utf8_are_refused() {
        for value in ["%", "%2", "%zz", "a%2", "%2g", "%ff", "%c3"] {
            assert_eq!(percent_decode(value, false), None, "{value}");
        }
    }
}
