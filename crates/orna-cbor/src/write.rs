use crate::head::{bytes, head, text};
use crate::{CANONICAL_NAN_BITS, Encode, Error, View, is_nan_bits};

/// Encodes `value` as canonical CBOR: shortest-form heads, the canonical NaN,
/// and map entries sorted by their encoded keys. Two keys that encode alike
/// are rejected.
pub fn encode<T: Encode>(value: &T) -> Result<Vec<u8>, Error> {
    let mut output = Vec::new();
    encode_into(value, &mut output)?;
    Ok(output)
}

/// Appends the canonical CBOR of `value` to `output`. On error `output` may
/// hold a partial encoding, so callers discard it.
pub fn encode_into<T: Encode>(value: &T, output: &mut Vec<u8>) -> Result<(), Error> {
    write(value, output)
}

fn write<T: Encode>(value: &T, output: &mut Vec<u8>) -> Result<(), Error> {
    match value.view() {
        View::Null => output.push(0xf6),
        View::Boolean(false) => output.push(0xf4),
        View::Boolean(true) => output.push(0xf5),
        View::Unsigned(number) => head(output, 0, number),
        View::Negative(number) => head(output, 1, number),
        View::Bignum {
            negative,
            magnitude,
        } => {
            head(output, 6, if negative { 3 } else { 2 });
            bytes(output, &magnitude);
        }
        View::Float(bits) => {
            let bits = if is_nan_bits(bits) {
                CANONICAL_NAN_BITS
            } else {
                bits
            };
            output.push(0xfb);
            output.extend_from_slice(&bits.to_be_bytes());
        }
        View::Bytes(value) => bytes(output, value),
        View::Text(value) => text(output, value),
        View::Array(values) => {
            head(output, 4, values.len() as u64);
            for value in values {
                write(value, output)?;
            }
        }
        View::Map(entries) => write_map(entries, output)?,
        View::Tag(number, value) => {
            head(output, 6, number);
            write(value, output)?;
        }
    }
    Ok(())
}

/// Sorts entries by encoded key. Keys are encoded once into one buffer, and
/// the sort works on spans of that buffer.
fn write_map<T: Encode>(entries: &[(T, T)], output: &mut Vec<u8>) -> Result<(), Error> {
    let mut keys = Vec::new();
    let mut spans = Vec::with_capacity(entries.len());
    for (key, _) in entries {
        let start = keys.len();
        write(key, &mut keys)?;
        spans.push(start..keys.len());
    }
    let encoded_key = |index: usize| &keys[spans[index].clone()];
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|left, right| encoded_key(*left).cmp(encoded_key(*right)));
    if order
        .windows(2)
        .any(|pair| encoded_key(pair[0]) == encoded_key(pair[1]))
    {
        return Err(Error::DuplicateOrUnorderedMapKey);
    }
    head(output, 5, entries.len() as u64);
    for index in order {
        output.extend_from_slice(encoded_key(index));
        write(&entries[index].1, output)?;
    }
    Ok(())
}
