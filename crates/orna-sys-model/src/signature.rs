//! Shared parsing for the textual signatures in the sys and provider ABIs.

/// Failure to split or validate a textual signature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureError {
    UnbalancedDelimiters,
}

/// Splits `input` at top-level `delimiter` characters.
///
/// Angle, square and parenthesis nesting all suppress splitting, so one
/// implementation serves both the provider type grammar and the portable
/// function signatures. Delimiters must balance; nesting that closes more than
/// it opens is rejected.
pub fn split_top_level(input: &str, delimiter: char) -> Result<Vec<&str>, SignatureError> {
    let mut parts = Vec::new();
    let (mut angles, mut squares, mut parens) = (0_i32, 0_i32, 0_i32);
    let mut start = 0;
    for (index, character) in input.char_indices() {
        match character {
            '<' => angles += 1,
            '>' => angles -= 1,
            '[' => squares += 1,
            ']' => squares -= 1,
            '(' => parens += 1,
            ')' => parens -= 1,
            value if value == delimiter && angles == 0 && squares == 0 && parens == 0 => {
                parts.push(input[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
        if angles < 0 || squares < 0 || parens < 0 {
            return Err(SignatureError::UnbalancedDelimiters);
        }
    }
    if angles != 0 || squares != 0 || parens != 0 {
        return Err(SignatureError::UnbalancedDelimiters);
    }
    if !input[start..].trim().is_empty() {
        parts.push(input[start..].trim());
    }
    Ok(parts)
}

/// Returns the byte index of the delimiter closing the one at `open`, if any.
///
/// `open` names the opening delimiter itself; scanning starts there so the
/// first `left` increments the depth.
pub fn matching_delimiter(input: &str, open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, character) in input.char_indices().skip_while(|(index, _)| *index < open) {
        if character == left {
            depth = depth.checked_add(1)?;
        } else if character == right {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// Whether `identifier` is one ASCII identifier with no qualification.
pub fn valid_identifier(identifier: &str) -> bool {
    let mut chars = identifier.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}
