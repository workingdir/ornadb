//! Decoding of Orna string-literal bodies.
//!
//! The escape table is the Orna 1.1.0 lexical profile: `\"`, `\\`, `\n`, `\r`,
//! `\t`, `\0` and `\u{hex}`. A Unicode escape holds one to six hexadecimal
//! digits naming a Unicode scalar value; surrogates and values above U+10FFFF
//! are refused. Literal string data is not normalised.

use std::fmt;

/// A string literal contains an escape outside the Orna escape table, or a
/// Unicode escape that does not name a scalar value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringEscapeError;

impl fmt::Display for StringEscapeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid string escape")
    }
}

impl std::error::Error for StringEscapeError {}

/// Decodes a complete, delimited string literal token spelling such as
/// `"a\nb"`.
pub fn unescape_string_literal(text: &str) -> Result<String, StringEscapeError> {
    text.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or(StringEscapeError)
        .and_then(unescape_string_body)
}

/// Decodes the characters between the delimiters of a string literal (or one
/// text segment of an interpolated string).
pub fn unescape_string_body(body: &str) -> Result<String, StringEscapeError> {
    let mut output = String::with_capacity(body.len());
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next().ok_or(StringEscapeError)? {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            '0' => output.push('\0'),
            '\\' => output.push('\\'),
            '"' => output.push('"'),
            'u' => {
                if characters.next() != Some('{') {
                    return Err(StringEscapeError);
                }
                let mut value: u32 = 0;
                let mut digits = 0_u8;
                loop {
                    let character = characters.next().ok_or(StringEscapeError)?;
                    if character == '}' {
                        break;
                    }
                    let digit = character.to_digit(16).ok_or(StringEscapeError)?;
                    digits += 1;
                    if digits > 6 {
                        return Err(StringEscapeError);
                    }
                    value = value * 16 + digit;
                }
                if digits == 0 {
                    return Err(StringEscapeError);
                }
                output.push(char::from_u32(value).ok_or(StringEscapeError)?);
            }
            _ => return Err(StringEscapeError),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{StringEscapeError, unescape_string_body, unescape_string_literal};

    fn decoded(body: &str) -> String {
        unescape_string_body(body).expect("body decodes")
    }
    #[test]
    fn decodes_each_spec_escape() {
        assert_eq!(decoded(r#"\""#), "\"");
        assert_eq!(decoded(r"\\"), "\\");
        assert_eq!(decoded(r"\n"), "\n");
        assert_eq!(decoded(r"\r"), "\r");
        assert_eq!(decoded(r"\t"), "\t");
        assert_eq!(decoded(r"\0"), "\0");
        assert_eq!(decoded(r"\u{7}"), "\u{7}");
        assert_eq!(decoded(r"\u{7b}"), "{");
        assert_eq!(decoded(r"\u{1F600}"), "\u{1F600}");
        assert_eq!(decoded(r"\u{0}"), "\0");
        assert_eq!(decoded(r"\u{10FFFF}"), "\u{10FFFF}");
    }

    #[test]
    fn literal_characters_pass_through_without_normalisation() {
        assert_eq!(decoded("plain {brace} \u{e9}"), "plain {brace} \u{e9}");
        assert_eq!(decoded("e\u{301}"), "e\u{301}");
        assert_eq!(decoded("raw\nnewline\ttab"), "raw\nnewline\ttab");
    }

    #[test]
    fn refuses_unicode_escapes_that_are_not_scalars() {
        for body in [
            r"\u{D800}",
            r"\u{DFFF}",
            r"\u{110000}",
            r"\u{FFFFFFF}",
            r"\u{00000041}",
            r"\u{}",
            r"\u{ 41}",
            r"\u{41",
            r"\u41",
            r"\u",
        ] {
            assert_eq!(unescape_string_body(body), Err(StringEscapeError), "{body}");
        }
    }

    #[test]
    fn refuses_escapes_outside_the_orna_table() {
        for body in [r"\x41", r"\/", r"\b", r"\f", r"\a", "trailing\\"] {
            assert_eq!(unescape_string_body(body), Err(StringEscapeError), "{body}");
        }
    }

    #[test]
    fn literal_form_requires_delimiters() {
        assert_eq!(unescape_string_literal(r#""a\nb""#).as_deref(), Ok("a\nb"));
        assert_eq!(unescape_string_literal(r#""""#).as_deref(), Ok(""));
        assert_eq!(
            unescape_string_literal("\"").as_deref(),
            Err(&StringEscapeError)
        );
        assert_eq!(
            unescape_string_literal("plain").as_deref(),
            Err(&StringEscapeError)
        );
    }
}
