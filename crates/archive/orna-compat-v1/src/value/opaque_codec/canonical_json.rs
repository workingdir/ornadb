//! Canonical JSON writer for the opaque JSON codec.

/// Writes `value` in the canonical JSON form that `canonical_json_body` accepts
/// only when it is byte-identical to the received body: no insignificant
/// whitespace, object keys sorted by UTF-8 bytes, arrays in source order, and
/// numbers re-serialised from their parsed value. This is written here instead
/// of using `serde_json::to_vec` because serde_json's `preserve_order` and
/// `arbitrary_precision` features change what `to_vec` emits for the same
/// value, so the codec must not depend on which features other crates enable.
pub(super) fn write_canonical_json(value: &serde_json::Value, out: &mut Vec<u8>) -> Option<()> {
    match value {
        serde_json::Value::Null => out.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => out.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => out.extend_from_slice(b"false"),
        serde_json::Value::Number(number) => {
            if let Some(unsigned) = number.as_u64() {
                serde_json::to_writer(&mut *out, &unsigned).ok()?;
            } else if let Some(signed) = number.as_i64() {
                serde_json::to_writer(&mut *out, &signed).ok()?;
            } else {
                serde_json::to_writer(&mut *out, &number.as_f64()?).ok()?;
            }
        }
        serde_json::Value::String(text) => serde_json::to_writer(&mut *out, text).ok()?,
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical_json(item, out)?;
            }
            out.push(b']');
        }
        serde_json::Value::Object(entries) => {
            let mut sorted: Vec<(&String, &serde_json::Value)> = entries.iter().collect();
            sorted.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            out.push(b'{');
            for (index, (key, item)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, key).ok()?;
                out.push(b':');
                write_canonical_json(item, out)?;
            }
            out.push(b'}');
        }
    }
    Some(())
}
