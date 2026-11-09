//! The MIME-1 media annotation registry and media-type syntax.

use crate::error::Error;
use crate::{Result, mime1};

/// Canonical MIME-1 annotation stored with each Blob value.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MediaAnnotation {
    media_type: String,
    suffix: Option<String>,
}

/// The closed MIME-1 registry selected by format 3, read from the pinned
/// `profiles/media-1.json` tables in `mime1.rs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct MimeRegistry {
    profile: &'static str,
}

impl MimeRegistry {
    pub const fn mime1() -> Self {
        Self {
            profile: mime1::PROFILE,
        }
    }

    pub const fn profile(self) -> &'static str {
        self.profile
    }

    pub fn normalize(self, media_type: &str) -> Result<String> {
        normalize_media_type(media_type)
    }

    pub fn annotation(self, media_type: &str, suffix: Option<&str>) -> Result<MediaAnnotation> {
        MediaAnnotation::with_mime1(media_type, suffix)
    }

    /// Canonicalize caller-supplied MIME-1 annotation input. Wire values
    /// continue to use `annotation` and must already be canonical.
    pub fn canonical_annotation(
        self,
        media_type: &str,
        suffix: Option<&str>,
    ) -> Result<MediaAnnotation> {
        let media_type = self.normalize(media_type)?;
        let suffix = match suffix {
            Some(hint) => mime1::canonical_hint(mime1::essence(&media_type), hint)?,
            None => None,
        };
        Ok(MediaAnnotation { media_type, suffix })
    }

    /// The annotation a uniquely mapped file suffix selects (ORNA-MIME-006).
    ///
    /// Only a suffix that exactly one entry lists as compatible selects that
    /// entry's essence, kept as the hint unless it is the preferred suffix. A
    /// suffix that two entries list, such as `mp4`, or that no entry lists
    /// selects nothing, so the caller keeps its octet-stream/null fallback.
    pub fn annotation_for_suffix(self, suffix: &str) -> Option<MediaAnnotation> {
        let suffix = suffix.to_lowercase();
        let entry = mime1::unique_entry_for_suffix(&suffix)?;
        Some(MediaAnnotation {
            media_type: entry.essence.to_owned(),
            suffix: (suffix != entry.preferred).then_some(suffix),
        })
    }
}

impl Default for MediaAnnotation {
    fn default() -> Self {
        Self {
            media_type: mime1::OCTET_STREAM.to_owned(),
            suffix: None,
        }
    }
}

impl MediaAnnotation {
    pub fn new(media_type: &str, suffix: Option<&str>) -> Result<Self> {
        MimeRegistry::mime1().annotation(media_type, suffix)
    }

    fn with_mime1(media_type: &str, suffix: Option<&str>) -> Result<Self> {
        let canonical_media_type = normalize_media_type(media_type)?;
        if canonical_media_type != media_type {
            return Err(Error::NonCanonical);
        }
        let suffix = match suffix {
            Some(value) => {
                let canonical =
                    mime1::canonical_hint(mime1::essence(&canonical_media_type), value)?;
                if canonical.as_deref() != Some(value) {
                    return Err(Error::NonCanonical);
                }
                canonical
            }
            None => None,
        };
        Ok(Self {
            media_type: canonical_media_type,
            suffix,
        })
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }

    /// The canonical essence: the media type without its parameters.
    pub fn essence(&self) -> &str {
        mime1::essence(&self.media_type)
    }

    pub fn selected_suffix(&self) -> &str {
        self.suffix
            .as_deref()
            .unwrap_or_else(|| mime1::preferred_suffix(self.essence()))
    }
}

pub fn normalize_media_type(media_type: &str) -> Result<String> {
    if media_type.len() > mime1::MAX_MEDIA_TYPE_BYTES
        || media_type.bytes().any(|byte| !(32..=126).contains(&byte))
    {
        return Err(Error::InvalidMediaType);
    }
    let input = media_type.trim_matches(' ');
    let bytes = input.as_bytes();
    let mut at = 0;
    let type_end = mime_token_end(bytes, at).ok_or(Error::InvalidMediaType)?;
    let kind = input[at..type_end].to_ascii_lowercase();
    at = type_end;
    if bytes.get(at) != Some(&b'/') {
        return Err(Error::InvalidMediaType);
    }
    at += 1;
    let subtype_end = mime_token_end(bytes, at).ok_or(Error::InvalidMediaType)?;
    let subtype = input[at..subtype_end].to_ascii_lowercase();
    if kind == "*" || subtype == "*" {
        return Err(Error::InvalidMediaType);
    }
    at = subtype_end;
    let essence = format!("{kind}/{subtype}");
    let essence = mime1::alias_target(&essence).map_or(essence, str::to_owned);
    let mut parameters = Vec::<(String, String)>::new();
    while at < bytes.len() {
        while bytes.get(at) == Some(&b' ') {
            at += 1;
        }
        if at == bytes.len() {
            break;
        }
        if bytes[at] != b';' {
            return Err(Error::InvalidMediaType);
        }
        at += 1;
        while bytes.get(at) == Some(&b' ') {
            at += 1;
        }
        let name_end = mime_token_end(bytes, at).ok_or(Error::InvalidMediaType)?;
        let name = input[at..name_end].to_ascii_lowercase();
        at = name_end;
        if bytes.get(at) != Some(&b'=') {
            return Err(Error::InvalidMediaType);
        }
        at += 1;
        let value = if bytes.get(at) == Some(&b'"') {
            at += 1;
            let mut value = String::new();
            let mut closed = false;
            while at < bytes.len() {
                let byte = bytes[at];
                at += 1;
                if byte == b'"' {
                    closed = true;
                    break;
                }
                if byte == b'\\' {
                    let escaped = *bytes.get(at).ok_or(Error::InvalidMediaType)?;
                    at += 1;
                    value.push(escaped as char);
                } else {
                    value.push(byte as char);
                }
            }
            if !closed {
                return Err(Error::InvalidMediaType);
            }
            value
        } else {
            let value_end = mime_token_end(bytes, at).ok_or(Error::InvalidMediaType)?;
            let value = input[at..value_end].to_owned();
            at = value_end;
            value
        };
        if parameters.iter().any(|(existing, _)| existing == &name)
            || parameters.len() >= mime1::MAX_PARAMETERS
        {
            return Err(Error::InvalidMediaType);
        }
        parameters.push((name, value));
        if let Some((name, value)) = parameters.last_mut()
            && name == "charset"
        {
            *value = value.to_ascii_lowercase();
        }
    }
    parameters.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical = essence;
    for (name, value) in parameters {
        canonical.push(';');
        canonical.push_str(&name);
        canonical.push('=');
        if valid_mime_token(value.as_bytes()) {
            canonical.push_str(&value);
        } else {
            canonical.push('"');
            for byte in value.bytes() {
                if byte == b'\\' || byte == b'"' {
                    canonical.push('\\');
                }
                canonical.push(byte as char);
            }
            canonical.push('"');
        }
    }
    Ok(canonical)
}

pub fn selected_suffix(media_type: &str, suffix: Option<&str>) -> Result<String> {
    Ok(MediaAnnotation::new(media_type, suffix)?
        .selected_suffix()
        .to_owned())
}

fn mime_token_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut at = start;
    while let Some(byte) = bytes.get(at).copied() {
        if !is_mime_token(byte) {
            break;
        }
        at += 1;
    }
    (at > start).then_some(at)
}

fn is_mime_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn valid_mime_token(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().copied().all(is_mime_token)
}
