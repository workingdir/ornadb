//! The Unicode 16.0.0 simple case mappings and the White_Space property, with the Final_Sigma context rule. Moved from
//! `orna-exec` (L1) so the profile does not depend on the toolchain's own Unicode tables.

use crate::case_properties::{is_case_ignorable, is_cased};

/// The White_Space property of Unicode 16.0.0, listed explicitly.
pub fn white_space(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{0085}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

/// The lowercase mapping of `value`, with Greek capital sigma becoming final sigma where the context requires it.
pub fn lowercase(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let scalars = value.chars().collect::<Vec<_>>();
    for (index, scalar) in scalars.iter().copied().enumerate() {
        if scalar == '\u{03a3}' && is_final_sigma(&scalars, index) {
            output.push('\u{03c2}');
            continue;
        }
        let mapping = unicode_case_mapping::to_lowercase(scalar);
        if mapping[0] == 0 {
            output.push(scalar);
            continue;
        }
        for codepoint in mapping.into_iter().take_while(|codepoint| *codepoint != 0) {
            output.push(char::from_u32(codepoint).expect("Unicode casing table contains scalars"));
        }
    }
    output
}

fn is_final_sigma(scalars: &[char], index: usize) -> bool {
    let preceding_cased = scalars[..index]
        .iter()
        .rev()
        .copied()
        .find(|scalar| !is_case_ignorable(*scalar))
        .is_some_and(is_cased);
    let following_cased = scalars[index + 1..]
        .iter()
        .copied()
        .find(|scalar| !is_case_ignorable(*scalar))
        .is_some_and(is_cased);
    preceding_cased && !following_cased
}

/// The uppercase mapping of `value`, scalar by scalar.
pub fn uppercase(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for scalar in value.chars() {
        let mapping = unicode_case_mapping::to_uppercase(scalar);
        if mapping[0] == 0 {
            output.push(scalar);
            continue;
        }
        for codepoint in mapping.into_iter().take_while(|codepoint| *codepoint != 0) {
            output.push(char::from_u32(codepoint).expect("Unicode casing table contains scalars"));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{lowercase, uppercase, white_space};

    #[test]
    fn final_sigma_applies_only_at_word_end() {
        assert_eq!(lowercase("ΟΔΟΣ"), "οδος");
        assert_eq!(lowercase("ΣΑ"), "σα");
        assert_eq!(lowercase("Σ"), "σ");
    }

    #[test]
    fn final_sigma_skips_case_ignorable_scalars() {
        // U+0301 COMBINING ACUTE ACCENT is Case_Ignorable, so the cased base still governs the context.
        assert_eq!(lowercase("ΟΔΟΣ\u{0301}"), "οδος\u{0301}");
    }

    #[test]
    fn case_mappings_expand_multi_scalar_results() {
        assert_eq!(uppercase("ß"), "SS");
        assert_eq!(uppercase("ﬁ"), "FI");
        assert_eq!(lowercase("İ"), "i\u{0307}");
    }

    #[test]
    fn white_space_matches_unicode_16_list() {
        for value in ['\t', '\n', ' ', '\u{0085}', '\u{00A0}', '\u{3000}'] {
            assert!(white_space(value), "{value:?}");
        }
        for value in ['a', '\u{200B}', '\u{FEFF}'] {
            assert!(!white_space(value), "{value:?}");
        }
    }
}
