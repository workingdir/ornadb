//! Unicode 16.0.0 `toNFKC_Casefold` support for portable path ownership.
//!
//! The normalization primitive comes from `unicode-normalization` 0.1.24,
//! whose generated tables are pinned to Unicode 16.  The full default case
//! folding table lives in `casefold/table_low.rs` and `casefold/table_high.rs`,
//! generated from the Unicode 16 CaseFolding data.

mod table_high;
mod table_low;

use table_high::CASE_FOLD_HIGH;
use table_low::CASE_FOLD_LOW;
use unicode_normalization::UnicodeNormalization;

const DEFAULT_IGNORABLE_RANGES: &[(u32, u32)] = &[
    (0x00AD, 0x00AD),
    (0x034F, 0x034F),
    (0x061C, 0x061C),
    (0x115F, 0x1160),
    (0x17B4, 0x17B5),
    (0x180B, 0x180D),
    (0x180E, 0x180E),
    (0x180F, 0x180F),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2065, 0x2065),
    (0x2066, 0x206F),
    (0x3164, 0x3164),
    (0xFE00, 0xFE0F),
    (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0),
    (0xFFF0, 0xFFF8),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0000, 0xE0000),
    (0xE0001, 0xE0001),
    (0xE0002, 0xE001F),
    (0xE0020, 0xE007F),
    (0xE0080, 0xE00FF),
    (0xE0100, 0xE01EF),
    (0xE01F0, 0xE0FFF),
];

/// Full case-folding mapping for `character`, if the table has one.
fn case_fold_mapping(character: char) -> Option<&'static str> {
    let code_point = character as u32;
    [CASE_FOLD_LOW, CASE_FOLD_HIGH]
        .into_iter()
        .find_map(|table| {
            table
                .binary_search_by_key(&code_point, |&(key, _)| key)
                .ok()
                .map(|index| table[index].1)
        })
}

fn is_default_ignorable(code_point: u32) -> bool {
    DEFAULT_IGNORABLE_RANGES
        .iter()
        .any(|&(start, end)| (start..=end).contains(&code_point))
}

pub fn to_nfkc_casefold(input: &str) -> String {
    let normalized = input.nfkc().collect::<String>();
    let mut output = String::with_capacity(normalized.len());
    for character in normalized.chars() {
        match case_fold_mapping(character) {
            Some(folded_text) => {
                for folded in folded_text.chars() {
                    if !is_default_ignorable(folded as u32) {
                        output.push(folded);
                    }
                }
            }
            None if !is_default_ignorable(character as u32) => output.push(character),
            None => {}
        }
    }
    output.nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::to_nfkc_casefold;

    #[test]
    fn matches_unicode_16_default_fold_vectors() {
        assert_eq!(to_nfkc_casefold("Straße"), "strasse");
        assert_eq!(to_nfkc_casefold("Σ"), "σ");
        assert_eq!(to_nfkc_casefold("İ"), "i\u{307}");
        assert_eq!(to_nfkc_casefold("ﬃ"), "ffi");
        assert_eq!(to_nfkc_casefold("a\u{00AD}b\u{034F}c"), "abc");
    }
}
