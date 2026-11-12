//! The percent decoder the `serve` and mount surfaces share. The hexadecimal
//! codec is `orna-base`'s.

use orna_base::hex_digit;

/// Decodes the `%XX` escapes of `value` and, for a form field
/// (`plus_as_space`), the `+` that stands for a space. `None` for a malformed
/// escape or a result that is not UTF-8.
#[must_use]
pub fn percent_decode(value: &str, plus_as_space: bool) -> Option<String> {
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
    use super::percent_decode;

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
