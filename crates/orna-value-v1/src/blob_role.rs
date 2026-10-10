//! Context dispatch for annotated Blobs.
//!
//! The canonical MIME-1 annotation selects the behavior an OVB-2 Blob takes in
//! a consuming context. Dispatch reads only the annotation that the shared
//! format-3 validation path already admitted (`MimeRegistry::annotation`), so
//! it never hydrates or reads Blob content.

use crate::{Blob, ContextValue, Error, MediaAnnotation, Result};

/// Top-level MIME family of a canonical media type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MediaFamily {
    Application,
    Audio,
    Image,
    Text,
    Video,
}

impl MediaFamily {
    /// The family of an annotation, or `None` for registry-admitted types
    /// outside the five families that Orna dispatches on.
    pub fn of(annotation: &MediaAnnotation) -> Option<Self> {
        match annotation.media_type().split('/').next()? {
            "application" => Some(Self::Application),
            "audio" => Some(Self::Audio),
            "image" => Some(Self::Image),
            "text" => Some(Self::Text),
            "video" => Some(Self::Video),
            _ => None,
        }
    }
}

/// The consuming behavior that an annotated Blob is dispatched to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BlobRole {
    /// Bytes-only consumers, such as an explicit raw download. Admits any
    /// canonical annotation, including unknown families.
    Opaque,
    Image,
    Audio,
    Video,
    /// Human-readable text: `text/*` and `application/json`.
    Text,
    /// Document payloads: `application/pdf`.
    Document,
}

impl MediaFamily {
    /// The lowercase top-level type name, as it appears before `/` in MIME-1.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Audio => "audio",
            Self::Image => "image",
            Self::Text => "text",
            Self::Video => "video",
        }
    }
}

impl BlobRole {
    /// Selects the behavior an annotation takes by default.
    pub fn for_annotation(annotation: &MediaAnnotation) -> Self {
        let base = annotation
            .media_type()
            .split(';')
            .next()
            .unwrap_or_default();
        match base {
            "application/json" => return Self::Text,
            "application/pdf" => return Self::Document,
            _ => {}
        }
        match MediaFamily::of(annotation) {
            Some(MediaFamily::Image) => Self::Image,
            Some(MediaFamily::Audio) => Self::Audio,
            Some(MediaFamily::Video) => Self::Video,
            Some(MediaFamily::Text) => Self::Text,
            Some(MediaFamily::Application) | None => Self::Opaque,
        }
    }

    /// Whether a canonical annotation may be consumed in this role.
    pub fn admits(self, annotation: &MediaAnnotation) -> bool {
        self == Self::Opaque || Self::for_annotation(annotation) == self
    }
}

impl Blob {
    /// The role selected by this Blob's canonical annotation.
    pub fn role(&self) -> BlobRole {
        BlobRole::for_annotation(self.annotation())
    }

    /// Admits this Blob for `role` from its annotation alone.
    pub fn admit_as(&self, role: BlobRole) -> Result<&Blob> {
        if role.admits(self.annotation()) {
            Ok(self)
        } else {
            Err(Error::RoleMismatch {
                role,
                family: MediaFamily::of(self.annotation()),
                length: self.length(),
                expected: BlobRole::for_annotation(self.annotation()),
                digest: self.identity.sha256(),
            })
        }
    }
}

impl ContextValue {
    /// Decodes this annotated Blob value and admits it for `role`. The
    /// annotation was validated when the value was decoded, so no content is
    /// read here.
    pub fn blob_as(&self, role: BlobRole) -> Result<Blob> {
        let blob = self.blob()?;
        blob.admit_as(role)?;
        Ok(blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, ValueFormat};
    use std::collections::BTreeMap;

    const FIXTURE: &str = include_str!("../tests/fixtures/blob-role-dispatch.orna");

    /// Reads the `let name = "value"` bindings of the Orna fixture.
    fn bindings() -> BTreeMap<&'static str, &'static str> {
        FIXTURE
            .lines()
            .filter_map(|line| {
                let (name, value) = line.strip_prefix("let ")?.split_once(" = ")?;
                Some((name, value.trim_matches('"')))
            })
            .collect()
    }

    fn asset(bindings: &BTreeMap<&str, &str>, name: &str) -> (String, String, BlobRole) {
        let media_type = bindings[format!("{name}_type").as_str()];
        let suffix = bindings[format!("{name}_suffix").as_str()];
        let role = match bindings[format!("{name}_role").as_str()] {
            "opaque" => BlobRole::Opaque,
            "image" => BlobRole::Image,
            "audio" => BlobRole::Audio,
            "video" => BlobRole::Video,
            "text" => BlobRole::Text,
            "document" => BlobRole::Document,
            other => panic!("unknown fixture role {other}"),
        };
        (media_type.to_owned(), suffix.to_owned(), role)
    }

    fn annotated_value(media_type: &str, suffix: &str) -> ContextValue {
        let suffix = (!suffix.is_empty()).then_some(suffix);
        let blob = Blob::from_bytes_with_annotation(b"payload".to_vec(), media_type, suffix)
            .expect("fixture annotation is canonical MIME-1");
        ContextValue::from_blob(&blob, ValueFormat::Ovb2).expect("encode OVB-2 Blob")
    }

    #[test]
    fn fixture_annotations_dispatch_to_their_recorded_role() {
        let bindings = bindings();
        for name in ["hero", "clip", "voice", "notes", "config", "report", "blob"] {
            let (media_type, suffix, expected) = asset(&bindings, name);
            let value = annotated_value(&media_type, &suffix);
            let blob = value.blob().expect("decode OVB-2 Blob");
            assert_eq!(blob.role(), expected, "role for {media_type}");
            assert!(
                value.blob_as(expected).is_ok(),
                "admit {media_type} as {expected:?}"
            );
        }
    }

    #[test]
    fn mismatched_roles_are_rejected_from_the_annotation_alone() {
        let bindings = bindings();
        let (media_type, suffix, _) = asset(&bindings, "hero");
        let value = annotated_value(&media_type, &suffix);
        assert!(matches!(
            value.blob_as(BlobRole::Video),
            Err(Error::RoleMismatch { .. })
        ));
        assert!(matches!(
            value.blob_as(BlobRole::Document),
            Err(Error::RoleMismatch { .. })
        ));
        // Opaque is the bytes-only consumer and admits every canonical annotation.
        assert!(value.blob_as(BlobRole::Opaque).is_ok());
    }

    #[test]
    fn mismatch_message_names_the_blob_size() {
        let bindings = bindings();
        let (media_type, suffix, _) = asset(&bindings, "hero");
        let value = annotated_value(&media_type, &suffix);
        let error = value.blob_as(BlobRole::Video).unwrap_err();
        assert_eq!(
            error.to_string(),
            "OVB2_ROLE_MISMATCH: OVB-2 Blob role Video does not admit MIME family image (7 bytes, sha256 239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5); admit as Image or Opaque"
        );
        assert_eq!(error.code(), Some(ErrorCode::RoleMismatch));
        assert_eq!(ErrorCode::RoleMismatch.as_str(), "OVB2_ROLE_MISMATCH");
    }

    #[test]
    fn json_is_text_and_pdf_is_a_document_rather_than_their_families() {
        let bindings = bindings();
        let (config_type, config_suffix, _) = asset(&bindings, "config");
        let config = annotated_value(&config_type, &config_suffix);
        assert!(config.blob_as(BlobRole::Text).is_ok());
        assert_eq!(
            config.blob_as(BlobRole::Opaque).map(|blob| blob.role()),
            Ok(BlobRole::Text)
        );

        let (report_type, report_suffix, _) = asset(&bindings, "report");
        let report = annotated_value(&report_type, &report_suffix);
        assert!(matches!(
            report.blob_as(BlobRole::Text),
            Err(Error::RoleMismatch { .. })
        ));
    }
}
