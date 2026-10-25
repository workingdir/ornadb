//! Repository-format-2 ROV-2 storage value decode (read-only, descriptive).
//!
//! Sole authority for this codec is the draft compatibility material in
//! `reference/Orna-1.1.0/compatibility/previous-1.1.0-draft.zip`, entry
//! `Orna-1.1.0/Orna-1.1.0.md`, section "Repository value profile ROV-2 and
//! semantic substitution SOV-2" (clause ORNA-OGB-008). Format-2 is not an
//! active 1.1.0 final requirement; this module only decodes the storage
//! reference value so that retained format-2 bytes can be inspected. It never
//! writes, never resolves the reference against `.orna/blobs/`, and never
//! produces an OVB-1 Blob value.
//!
//! The wire form is CBOR tag 60100 around `[content_sha256_bytes32,
//! total_length_uint]`, using the deterministic shortest-form integer subset.

/// CBOR tag used only by ROV-2 storage references (OGB-008). Tag 60100 is
/// forbidden in OVB-1 and portable text.
pub const ROV2_TAG: u64 = 60100;

/// Largest admitted content length: 2^63 - 1 bytes (OGB-1 maximum).
pub const ROV2_MAX_LENGTH: u64 = (1 << 63) - 1;

/// A decoded ROV-2 storage reference: the raw-content SHA-256 commitment and
/// the exact total length of the referenced Blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rov2Blob {
    pub content_sha256: [u8; 32],
    pub total_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rov2DecodeError {
    /// The input ended before the value was complete.
    Truncated { offset: usize },
    /// The value is complete but bytes follow it.
    TrailingBytes { offset: usize },
    /// A CBOR item used a major type or additional-information code that the
    /// ROV-2 subset does not admit at this position.
    UnexpectedItem { offset: usize },
    /// An integer or length was not in shortest form.
    NonShortestForm { offset: usize },
    /// The outer tag is present but is not 60100.
    WrongTag { found: u64 },
    /// The tagged array does not have exactly two elements.
    WrongArity { found: u64 },
    /// The digest byte string is not exactly 32 bytes.
    WrongDigestWidth { found: u64 },
    /// The length exceeds 2^63 - 1.
    LengthOutOfRange { found: u64 },
}

impl std::fmt::Display for Rov2DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { offset } => write!(f, "ROV-2 value truncated at byte {offset}"),
            Self::TrailingBytes { offset } => {
                write!(f, "ROV-2 value has trailing bytes at byte {offset}")
            }
            Self::UnexpectedItem { offset } => {
                write!(
                    f,
                    "ROV-2 value has an unexpected CBOR item at byte {offset}"
                )
            }
            Self::NonShortestForm { offset } => {
                write!(f, "ROV-2 value is not shortest-form at byte {offset}")
            }
            Self::WrongTag { found } => write!(f, "ROV-2 value has tag {found}, expected 60100"),
            Self::WrongArity { found } => {
                write!(f, "ROV-2 value array has {found} elements, expected 2")
            }
            Self::WrongDigestWidth { found } => {
                write!(f, "ROV-2 digest has {found} bytes, expected 32")
            }
            Self::LengthOutOfRange { found } => {
                write!(f, "ROV-2 length {found} exceeds 2^63 - 1")
            }
        }
    }
}

impl std::error::Error for Rov2DecodeError {}

/// Decodes exactly one ROV-2 storage reference occupying all of `bytes`.
pub fn decode_rov2_blob(bytes: &[u8]) -> Result<Rov2Blob, Rov2DecodeError> {
    let mut cursor = Cursor { bytes, pos: 0 };

    let (major, tag) = cursor.head()?;
    if major != 6 {
        return Err(Rov2DecodeError::UnexpectedItem { offset: 0 });
    }
    if tag != ROV2_TAG {
        return Err(Rov2DecodeError::WrongTag { found: tag });
    }

    let (major, arity) = cursor.head()?;
    if major != 4 || arity != 2 {
        return Err(Rov2DecodeError::WrongArity { found: arity });
    }

    let (major, width) = cursor.head()?;
    if major != 2 {
        return Err(Rov2DecodeError::UnexpectedItem { offset: cursor.pos });
    }
    if width != 32 {
        return Err(Rov2DecodeError::WrongDigestWidth { found: width });
    }
    let mut content_sha256 = [0u8; 32];
    content_sha256.copy_from_slice(cursor.take(32)?);

    let (major, length) = cursor.head()?;
    if major != 0 {
        return Err(Rov2DecodeError::UnexpectedItem { offset: cursor.pos });
    }
    if length > ROV2_MAX_LENGTH {
        return Err(Rov2DecodeError::LengthOutOfRange { found: length });
    }

    if cursor.pos != bytes.len() {
        return Err(Rov2DecodeError::TrailingBytes { offset: cursor.pos });
    }

    Ok(Rov2Blob {
        content_sha256,
        total_length: length,
    })
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Reads one CBOR initial byte plus its argument, enforcing shortest form.
    /// Indefinite lengths and reserved additional-information values are
    /// rejected.
    fn head(&mut self) -> Result<(u8, u64), Rov2DecodeError> {
        let start = self.pos;
        let initial = *self
            .bytes
            .get(self.pos)
            .ok_or(Rov2DecodeError::Truncated { offset: start })?;
        self.pos += 1;
        let major = initial >> 5;
        let info = initial & 0x1f;
        let value = match info {
            0..=23 => u64::from(info),
            24 => {
                let v = u64::from(self.take(1)?[0]);
                if v < 24 {
                    return Err(Rov2DecodeError::NonShortestForm { offset: start });
                }
                v
            }
            25 => {
                let v = u64::from(u16::from_be_bytes(self.take_array::<2>()?));
                if v < 0x100 {
                    return Err(Rov2DecodeError::NonShortestForm { offset: start });
                }
                v
            }
            26 => {
                let v = u64::from(u32::from_be_bytes(self.take_array::<4>()?));
                if v < 0x1_0000 {
                    return Err(Rov2DecodeError::NonShortestForm { offset: start });
                }
                v
            }
            27 => {
                let v = u64::from_be_bytes(self.take_array::<8>()?);
                if v < 0x1_0000_0000 {
                    return Err(Rov2DecodeError::NonShortestForm { offset: start });
                }
                v
            }
            _ => return Err(Rov2DecodeError::UnexpectedItem { offset: start }),
        };
        Ok((major, value))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Rov2DecodeError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(Rov2DecodeError::Truncated { offset: self.pos })?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], Rov2DecodeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        assert!(text.len() % 2 == 0, "odd hex length");
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digit"))
            .collect()
    }

    // Golden `stored_reference_cbor` values copied byte-for-byte from
    // `Orna-1.1.0/tests/blob-vectors.json` inside
    // `reference/Orna-1.1.0/compatibility/previous-1.1.0-draft.zip`.
    // Each entry: (vector name, stored reference hex, expected length).
    const GOLDEN: &[(&str, &str, u64)] = &[
        (
            "empty",
            "d9eac4825820e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85500",
            0,
        ),
        (
            "abc",
            "d9eac4825820ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad03",
            3,
        ),
        (
            "octets",
            "d9eac48258204234f1064f32788dc9487507f9eeecd0cdf7df6bd010fd1dad8026226e22281303",
            3,
        ),
        (
            "counter-300k",
            "d9eac4825820fd27231cc9897b21084f4f87ab19976a69e77179aab9d180feb2125b491a091e1a000493e0",
            300_000,
        ),
    ];

    #[test]
    fn decodes_draft_golden_rov2_vectors_byte_for_byte() {
        for (name, encoded, length) in GOLDEN {
            let bytes = hex(encoded);
            let decoded = decode_rov2_blob(&bytes)
                .unwrap_or_else(|err| panic!("{name}: decode failed: {err}"));
            // Bytes 0..4 are tag 60100 + array(2) + bstr(32) header "d9eac4 82 5820"
            // i.e. 6 bytes = 12 hex digits; the digest follows as 64 hex digits.
            assert_eq!(
                decoded.content_sha256.to_vec(),
                hex(&encoded[12..76]),
                "{name} digest"
            );
            assert_eq!(decoded.total_length, *length, "{name} length");
        }
    }

    // When the draft compatibility archive is readable, every inline golden
    // value must equal its `stored_reference_cbor` and `length` in the archive's
    // `Orna-1.1.0/tests/blob-vectors.json`. The archive lives outside the
    // checkout, so its location comes from `ORNA_DRAFT_ZIP` (set it to
    // `reference/Orna-1.1.0/compatibility/previous-1.1.0-draft.zip`). Skipped
    // with a note when unset, unreadable, or `unzip` is unavailable.
    #[test]
    fn inline_golden_vectors_match_draft_zip_when_readable() {
        let Some(zip) = std::env::var_os("ORNA_DRAFT_ZIP") else {
            eprintln!("skipping: ORNA_DRAFT_ZIP not set");
            return;
        };
        let output = match std::process::Command::new("unzip")
            .arg("-p")
            .arg(&zip)
            .arg("Orna-1.1.0/tests/blob-vectors.json")
            .output()
        {
            Ok(output) if output.status.success() => output,
            _ => {
                eprintln!("skipping: draft zip not readable at {zip:?}");
                return;
            }
        };
        let doc: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("blob-vectors.json parses");
        let vectors = doc["vectors"].as_array().expect("vectors array");
        for (name, encoded, length) in GOLDEN {
            let vector = vectors
                .iter()
                .find(|v| v["name"] == *name)
                .unwrap_or_else(|| panic!("{name}: missing in draft zip"));
            assert_eq!(
                vector["stored_reference_cbor"].as_str(),
                Some(*encoded),
                "{name} stored_reference_cbor"
            );
            assert_eq!(vector["length"].as_u64(), Some(*length), "{name} length");
        }
    }

    #[test]
    fn rejects_malformed_storage_references() {
        let golden = hex(GOLDEN[1].1);

        // Tag 60101 instead of 60100 (0xEAC5).
        let mut wrong_tag = golden.clone();
        wrong_tag[1] = 0xea;
        wrong_tag[2] = 0xc5;
        assert!(matches!(
            decode_rov2_blob(&wrong_tag),
            Err(Rov2DecodeError::WrongTag { found: 60101 })
        ));

        // Extra byte after the complete value.
        let mut trailing = golden.clone();
        trailing.push(0x00);
        assert!(matches!(
            decode_rov2_blob(&trailing),
            Err(Rov2DecodeError::TrailingBytes { .. })
        ));

        // Truncated length item.
        assert!(matches!(
            decode_rov2_blob(&golden[..golden.len() - 1]),
            Err(Rov2DecodeError::Truncated { .. })
        ));

        // Length 3 spelled as a 2-byte argument (not shortest form).
        let mut non_shortest = golden[..golden.len() - 1].to_vec();
        non_shortest.extend_from_slice(&[0x19, 0x00, 0x03]);
        assert!(matches!(
            decode_rov2_blob(&non_shortest),
            Err(Rov2DecodeError::NonShortestForm { .. })
        ));

        // 31-byte digest string.
        let mut short = hex("d9eac48258");
        short.push(0x1f);
        short.extend_from_slice(&[0u8; 31]);
        short.push(0x00);
        assert!(matches!(
            decode_rov2_blob(&short),
            Err(Rov2DecodeError::WrongDigestWidth { found: 31 })
        ));

        // Length 2^63 is one past the admitted maximum.
        let mut too_long = hex(&format!("d9eac4825820{}", "00".repeat(32)));
        too_long.extend_from_slice(&[0x1b, 0x80, 0, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(
            decode_rov2_blob(&too_long),
            Err(Rov2DecodeError::LengthOutOfRange { .. })
        ));
    }
}
