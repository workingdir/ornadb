//! Shortest-form definite CBOR heads and the small helpers built on them.
//!
//! Every canonical encoder in this crate writes a length or integer through
//! [`head`] or sizes it through [`head_bytes`], so one table decides the
//! encoding: values 0..=23 inline, then the shortest of 1, 2, 4 or 8 extra
//! big-endian bytes. The two functions share that table, which keeps a size
//! estimate exactly as long as the bytes [`head`] appends.

/// The shortest definite head for `major` and `value`, and its length. The
/// unused tail of the returned array is zero and must not be written.
pub(crate) fn head_bytes(major: u8, value: u64) -> ([u8; 9], usize) {
    let base = major << 5;
    let mut head = [0; 9];
    let length = match value {
        0..=23 => {
            head[0] = base | value as u8;
            1
        }
        24..=255 => {
            head[0] = base | 24;
            head[1] = value as u8;
            2
        }
        256..=65_535 => {
            head[0] = base | 25;
            head[1..3].copy_from_slice(&(value as u16).to_be_bytes());
            3
        }
        65_536..=4_294_967_295 => {
            head[0] = base | 26;
            head[1..5].copy_from_slice(&(value as u32).to_be_bytes());
            5
        }
        _ => {
            head[0] = base | 27;
            head[1..].copy_from_slice(&value.to_be_bytes());
            9
        }
    };
    (head, length)
}

/// Appends the shortest definite head for `major` and `value`.
pub(crate) fn head(output: &mut Vec<u8>, major: u8, value: u64) {
    let (head, length) = head_bytes(major, value);
    output.extend_from_slice(&head[..length]);
}

/// The encoded length [`head`] uses for `value`: the size a node's header
/// costs before its entries are sized.
pub(crate) fn head_len(value: u64) -> usize {
    head_bytes(0, value).1
}

/// Appends a definite byte string.
pub(crate) fn bytes(output: &mut Vec<u8>, value: &[u8]) {
    head(output, 2, value.len() as u64);
    output.extend_from_slice(value);
}

/// Appends a definite UTF-8 text string.
pub(crate) fn text(output: &mut Vec<u8>, value: &str) {
    head(output, 3, value.len() as u64);
    output.extend_from_slice(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boundary values of each head width, as the canonical profile
    /// spells them (store-format-3 §1).
    #[test]
    fn heads_are_shortest_form_at_every_boundary() {
        let golden = [
            (0_u64, &[0x00][..]),
            (23, &[0x17][..]),
            (24, &[0x18, 0x18][..]),
            (255, &[0x18, 0xff][..]),
            (256, &[0x19, 0x01, 0x00][..]),
            (65_535, &[0x19, 0xff, 0xff][..]),
            (65_536, &[0x1a, 0x00, 0x01, 0x00, 0x00][..]),
            (4_294_967_295, &[0x1a, 0xff, 0xff, 0xff, 0xff][..]),
            (
                4_294_967_296,
                &[0x1b, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00][..],
            ),
            (
                u64::MAX,
                &[0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..],
            ),
        ];
        for (value, expected) in golden {
            let mut encoded = Vec::new();
            head(&mut encoded, 0, value);
            assert_eq!(encoded, expected, "value {value}");
            let (head, length) = head_bytes(0, value);
            assert_eq!(&head[..length], expected, "sized value {value}");
        }
        let mut encoded = Vec::new();
        bytes(&mut encoded, b"ab");
        assert_eq!(encoded, [0x42, b'a', b'b']);
        let mut encoded = Vec::new();
        text(&mut encoded, "ab");
        assert_eq!(encoded, [0x62, b'a', b'b']);
        let mut encoded = Vec::new();
        head(&mut encoded, 4, 1);
        head(&mut encoded, 0, 2);
        assert_eq!(encoded, [0x81, 0x02]);
    }
}
