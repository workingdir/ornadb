//! Shortest-form definite heads and the length-prefixed items built on them.

/// Appends the shortest definite head for `major` and `value`: values 0..=23
/// inline, otherwise the shortest of 1, 2, 4 or 8 extra big-endian bytes.
pub fn head(output: &mut Vec<u8>, major: u8, value: u64) {
    if value < 24 {
        output.push(major << 5 | value as u8)
    } else if value <= u8::MAX as u64 {
        output.extend([major << 5 | 24, value as u8])
    } else if value <= u16::MAX as u64 {
        output.push(major << 5 | 25);
        output.extend((value as u16).to_be_bytes())
    } else if value <= u32::MAX as u64 {
        output.push(major << 5 | 26);
        output.extend((value as u32).to_be_bytes())
    } else {
        output.push(major << 5 | 27);
        output.extend(value.to_be_bytes())
    }
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
    /// spells them.
    #[test]
    fn heads_are_shortest_form_at_every_boundary() {
        let cases: [(u64, &[u8]); 9] = [
            (23, &[0x17]),
            (24, &[0x18, 24]),
            (255, &[0x18, 0xff]),
            (256, &[0x19, 0x01, 0x00]),
            (65_535, &[0x19, 0xff, 0xff]),
            (65_536, &[0x1a, 0x00, 0x01, 0x00, 0x00]),
            (u32::MAX as u64, &[0x1a, 0xff, 0xff, 0xff, 0xff]),
            (u32::MAX as u64 + 1, &[0x1b, 0, 0, 0, 1, 0, 0, 0, 0]),
            (
                u64::MAX,
                &[0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            ),
        ];
        for (value, expected) in cases {
            let mut output = Vec::new();
            head(&mut output, 0, value);
            assert_eq!(output, expected, "head of {value}");
        }
    }
}
