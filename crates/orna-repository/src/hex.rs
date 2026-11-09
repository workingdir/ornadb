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

/// The inverse of [`hex_encode`]: exactly its lowercase, even-length
/// spelling, so every byte string has one accepted form.
pub(crate) fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let (pairs, []) = text.as_bytes().as_chunks::<2>() else {
        return None;
    };
    pairs
        .iter()
        .map(|[high, low]| Some((digit(*high)? << 4) | digit(*low)?))
        .collect()
}
