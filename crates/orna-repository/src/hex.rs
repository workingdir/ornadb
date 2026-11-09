//! Lowercase hexadecimal text: the one spelling of object IDs, digests and
//! record identities used across this crate.
//!
//! The codec itself is `orna-base`'s; this module keeps the crate-local names
//! the repository's readers and writers already use.

pub(crate) use orna_base::hex as hex_encode;

/// Whether `text` is spelled exactly as [`hex_encode`] would spell it: even
/// length and lowercase digits only. Readers use this so a record has one
/// accepted form rather than one per case or radix spelling.
pub(crate) fn is_hex(text: &str) -> bool {
    orna_base::hex_decode(text).is_ok()
}

/// The inverse of [`hex_encode`]: exactly its lowercase, even-length
/// spelling, so every byte string has one accepted form.
pub(crate) fn decode_text(text: &str) -> Option<Vec<u8>> {
    orna_base::hex_decode(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's readers accept exactly the spelling the writers produce:
    /// lowercase, even length. Case, radix prefixes/signs and odd lengths
    /// have no second spelling.
    #[test]
    fn decode_accepts_only_the_canonical_spelling() {
        assert_eq!(decode_text("00ff"), Some(vec![0x00, 0xff]));
        assert_eq!(decode_text(""), Some(Vec::new()));
        for noncanonical in ["00FF", "0A", "+f", "0x00", "0f ", "0", "\u{0660}0"] {
            assert_eq!(decode_text(noncanonical), None, "{noncanonical:?}");
            assert!(!is_hex(noncanonical), "{noncanonical:?}");
        }
        assert!(is_hex(""));
        assert!(is_hex("00ff"));
    }
}
