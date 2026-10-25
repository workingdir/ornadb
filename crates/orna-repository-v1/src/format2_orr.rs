//! Repository-format-2 ORR-2 Blob field decode (read-only, descriptive).
//!
//! Source: the superseded draft compatibility material `reference/Orna-1.1.0/
//! compatibility/previous-1.1.0-draft.zip`, entry `Orna-1.1.0/Orna-1.1.0.md`,
//! section "Repository row profile ORR-2" (draft ORNA-OGB-007, lines 4922-4946;
//! superseded ID). Authority for reading retained bytes: ORNA-ROW-013 and
//! ORNA-UPGRADE-002.
//!
//! In a Blob-typed slot the ORR-2 decoder admits exactly two spellings:
//! - a storage record `{ blob: "sha256:<64 lowercase hex>", length: <decimal> }`
//!   that names a content root under `.orna/blobs/<2>/<62>/`; and
//! - a legacy inline padded-Base64 string (draft ORNA-OGB-007).
//!
//! This module only decodes one field value. It does not resolve the root, does
//! not hydrate payloads, and does not produce an OVB-1 Blob value. A record in a
//! non-Blob field is ordinary data and is not passed here.

use crate::format2_rov::ROV2_MAX_LENGTH;

/// A parsed ORR-2 storage record. `length` is the exact declared byte count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orr2BlobRef {
    pub content_sha256: [u8; 32],
    pub length: u64,
}

/// One decoded ORR-2 Blob field value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Orr2BlobField {
    /// Legacy inline padded-Base64 payload (draft ORNA-OGB-007).
    Inline(Vec<u8>),
    /// External storage record resolved through `.orna/blobs/`.
    Reference(Orr2BlobRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Orr2DecodeError {
    /// The text starts with `{` but is not a complete, exact record.
    MalformedRecord(&'static str),
    /// The text is neither a record nor valid padded Base64.
    InvalidInline,
    /// The record `length` exceeds 2^63 - 1.
    LengthOutOfRange,
}

impl std::fmt::Display for Orr2DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedRecord(why) => write!(f, "ORR-2 Blob record is malformed: {why}"),
            Self::InvalidInline => {
                write!(f, "ORR-2 Blob field is neither a record nor padded Base64")
            }
            Self::LengthOutOfRange => write!(f, "ORR-2 Blob record length exceeds 2^63 - 1"),
        }
    }
}

impl std::error::Error for Orr2DecodeError {}

/// Decodes one ORR-2 Blob-slot value.
pub fn decode_orr2_blob_field(text: &str) -> Result<Orr2BlobField, Orr2DecodeError> {
    let trimmed = text.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.starts_with('{') {
        return parse_record(trimmed).map(Orr2BlobField::Reference);
    }
    orna_value_v1::base64_decode(trimmed)
        .map(Orr2BlobField::Inline)
        .map_err(|_| Orr2DecodeError::InvalidInline)
}

fn parse_record(text: &str) -> Result<Orr2BlobRef, Orr2DecodeError> {
    let inner = text
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .ok_or(Orr2DecodeError::MalformedRecord("missing braces"))?;

    let mut digest: Option<[u8; 32]> = None;
    let mut length: Option<u64> = None;
    let mut fields = inner.split(',').map(str::trim);
    let mut count = 0usize;
    for field in &mut fields {
        if field.is_empty() {
            return Err(Orr2DecodeError::MalformedRecord("empty field"));
        }
        count += 1;
        let (name, value) = field
            .split_once(':')
            .map(|(n, v)| (n.trim(), v.trim()))
            .ok_or(Orr2DecodeError::MalformedRecord("field without colon"))?;
        match name {
            "blob" => {
                if digest.is_some() {
                    return Err(Orr2DecodeError::MalformedRecord("duplicate blob field"));
                }
                digest = Some(parse_digest_literal(value)?);
            }
            "length" => {
                if length.is_some() {
                    return Err(Orr2DecodeError::MalformedRecord("duplicate length field"));
                }
                length = Some(parse_length(value)?);
            }
            _ => return Err(Orr2DecodeError::MalformedRecord("unknown field")),
        }
    }
    if count != 2 {
        return Err(Orr2DecodeError::MalformedRecord(
            "record must have exactly blob and length",
        ));
    }
    match (digest, length) {
        (Some(content_sha256), Some(length)) => Ok(Orr2BlobRef {
            content_sha256,
            length,
        }),
        _ => Err(Orr2DecodeError::MalformedRecord("missing blob or length")),
    }
}

fn parse_digest_literal(value: &str) -> Result<[u8; 32], Orr2DecodeError> {
    let hex = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .and_then(|rest| rest.strip_prefix("sha256:"))
        .ok_or(Orr2DecodeError::MalformedRecord(
            "blob must be a quoted sha256 literal",
        ))?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Orr2DecodeError::MalformedRecord(
            "digest must be 64 lowercase hex digits",
        ));
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| Orr2DecodeError::MalformedRecord("digest hex"))?;
    }
    Ok(out)
}

fn parse_length(value: &str) -> Result<u64, Orr2DecodeError> {
    // Canonical decimal: no sign, no leading zero except the single digit 0.
    if value.is_empty()
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(Orr2DecodeError::MalformedRecord(
            "length must be a canonical decimal integer",
        ));
    }
    let parsed: u64 = value
        .parse()
        .map_err(|_| Orr2DecodeError::LengthOutOfRange)?;
    if parsed > ROV2_MAX_LENGTH {
        return Err(Orr2DecodeError::LengthOutOfRange);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The spelling and digest come from the draft example row in
    // `Orna-1.1.0/Orna-1.1.0.md` (draft ORR-2 example, lines 4934-4944). That digest
    // is illustrative only; the test checks parsing, not content.
    const EXAMPLE_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn digest_bytes(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn decodes_spec_example_record() {
        let text = format!("{{ blob: \"sha256:{EXAMPLE_DIGEST}\", length: 7340032 }}");
        assert_eq!(
            decode_orr2_blob_field(&text).unwrap(),
            Orr2BlobField::Reference(Orr2BlobRef {
                content_sha256: digest_bytes(EXAMPLE_DIGEST),
                length: 7_340_032,
            })
        );
    }

    #[test]
    fn decodes_legacy_inline_padded_base64() {
        // "abc" -> "YWJj"; padded form required.
        assert_eq!(
            decode_orr2_blob_field("YWJj").unwrap(),
            Orr2BlobField::Inline(b"abc".to_vec())
        );
        assert_eq!(
            decode_orr2_blob_field("YQ==").unwrap(),
            Orr2BlobField::Inline(b"a".to_vec())
        );
    }

    #[test]
    fn rejects_non_exact_records() {
        let d = EXAMPLE_DIGEST;
        for bad in [
            format!("{{ blob: \"sha256:{d}\" }}"),
            format!("{{ blob: \"sha256:{d}\", length: 1, extra: 2 }}"),
            format!("{{ blob: \"sha256:{d}\", blob: \"sha256:{d}\" }}"),
            format!("{{ blob: \"sha256:{d}\", length: 01 }}"),
            format!("{{ blob: \"sha256:{d}\", length: -1 }}"),
            format!("{{ blob: \"sha256:{}\", length: 1 }}", d.to_uppercase()),
            format!("{{ blob: \"sha256:{}\", length: 1 }}", &d[..63]),
            format!("{{ blob: \"sha1:{d}\", length: 1 }}"),
            format!("{{ blob: \"sha256:{d}\", length: 9223372036854775808 }}"),
            format!("{{ blob: \"sha256:{d}\", length: 1, }}"),
            format!("{{ blob: \"sha256:{d}\", , length: 1 }}"),
        ] {
            assert!(decode_orr2_blob_field(&bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn rejects_invalid_inline_text() {
        assert_eq!(
            decode_orr2_blob_field("YWJ"),
            Err(Orr2DecodeError::InvalidInline)
        );
    }
}
