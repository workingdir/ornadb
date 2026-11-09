//! The workspace's one RFC 4648 Base64 codec.

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};

use crate::CodecError;

/// Encodes standard padded Base64, the only spelling OrnaDB stores.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Decodes canonical standard padded Base64. Whitespace, URL-safe aliases,
/// missing or non-canonical padding and nonzero trailing bits are rejected.
pub fn decode(input: &str) -> Result<Vec<u8>, CodecError> {
    STANDARD
        .decode(input)
        .map_err(|_| CodecError::InvalidBase64)
}

/// Encodes unpadded URL-safe Base64, the live-transport token spelling.
#[must_use]
pub fn encode_url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes canonical unpadded URL-safe Base64.
pub fn decode_url(input: &str) -> Result<Vec<u8>, CodecError> {
    URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|_| CodecError::InvalidBase64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_round_trip_uses_mandatory_padding() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(decode("Zm9v"), Ok(b"foo".to_vec()));
        assert_eq!(decode("Zg=="), Ok(b"f".to_vec()));
    }

    #[test]
    fn decode_rejects_noncanonical_spellings() {
        for rejected in ["Zg", "Zg=", "Zm9v=", "Zg===", "Z h==", "Zg--", "Zh=="] {
            assert_eq!(
                decode(rejected),
                Err(CodecError::InvalidBase64),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn url_round_trip_is_unpadded_and_rejects_the_standard_alphabet() {
        assert_eq!(encode_url(&[0xfb, 0xff]), "-_8");
        assert_eq!(decode_url("-_8"), Ok(vec![0xfb, 0xff]));
        assert_eq!(decode_url("+/8"), Err(CodecError::InvalidBase64));
        assert_eq!(decode_url("-_8="), Err(CodecError::InvalidBase64));
    }
}
