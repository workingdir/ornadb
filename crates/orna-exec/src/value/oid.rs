//! The typed `sys.GitOid` as an executor value.
//!
//! A `sys.GitOid` is the tag-60023 value `["sys.GitOid", [algorithm, bytes]]` (`source/29-formats.md`, "Portable
//! opaque and contextual values"): a Git object id that compares by the object it names and is never a `Str`, a
//! `Uuid` or a digest (ORNA-SYS-016). Each one is a checked [`GitObjectId`] held as a [`Value::GitOid`], so a
//! provider reads the typed id through [`Value::as_git_oid`] instead of an opaque `Raw`. Its canonical bytes are
//! the ones [`Value::to_raw`] produces, so a stored id reads back as an equal value.

use std::rc::Rc;

use orna_value::{GitObjectId, Raw};

use super::Value;

impl Value {
    /// The executor value of a checked `sys.GitOid`.
    #[must_use]
    pub fn git_oid(oid: GitObjectId) -> Self {
        Self::GitOid(Rc::new(oid))
    }

    /// The `sys.GitOid` this value holds, if it is one.
    #[must_use]
    pub fn as_git_oid(&self) -> Option<&GitObjectId> {
        match self {
            Self::GitOid(oid) => Some(oid.as_ref()),
            _ => None,
        }
    }
}

/// The typed `sys.GitOid` that the canonical raw value `raw` holds, or `None` when `raw` is not exactly the canonical
/// form of one: another opaque identifier (a `sys.RunId` is tag 60023 too) or an id whose algorithm and width
/// disagree. The caller keeps `None` as an opaque [`Value::RowRef`].
pub(crate) fn git_oid_from_raw(raw: &Raw) -> Option<Value> {
    let value = Value::git_oid(GitObjectId::from_opaque_raw(raw)?);
    (value.to_raw().ok()? == *raw).then_some(value)
}

#[cfg(test)]
mod tests {
    use orna_value::{GitHash, GitObjectId, OPAQUE_SYSTEM_ID_TAG, Raw};

    use super::{Value, git_oid_from_raw};

    fn sha1(byte: u8) -> GitObjectId {
        GitObjectId::new(GitHash::Sha1, vec![byte; 20]).expect("a SHA-1 object id")
    }

    fn sha256(byte: u8) -> GitObjectId {
        GitObjectId::new(GitHash::Sha256, vec![byte; 32]).expect("a SHA-256 object id")
    }

    // Specified: ORNA-SYS-016; Orna-1.1.0 29-formats.md, tag 60023 and `sys.GitOid`.
    #[test]
    fn a_git_oid_is_its_tag_60023_value_and_reads_back_equal() {
        for oid in [sha1(0x3f), sha256(0xc0)] {
            let value = Value::git_oid(oid.clone());
            let raw = value.to_raw().expect("a sys.GitOid encodes");
            assert_eq!(raw, oid.to_opaque_raw());
            assert!(matches!(&raw, Raw::Tag(OPAQUE_SYSTEM_ID_TAG, _)));
            let read = crate::raw_value::from_raw(&raw).expect("a sys.GitOid reads back");
            assert_eq!(read, value);
            assert_eq!(read.as_git_oid(), Some(&oid));
        }
    }

    // Specified: ORNA-SYS-016; Orna-1.1.0 29-formats.md, tag 60023 and `sys.GitOid`.
    #[test]
    fn ids_compare_by_the_object_they_name_and_never_as_another_identifier() {
        assert_eq!(Value::git_oid(sha1(1)), Value::git_oid(sha1(1)));
        assert_ne!(Value::git_oid(sha1(1)), Value::git_oid(sha1(2)));
        // The same bytes under another algorithm name another object.
        assert_ne!(
            Value::git_oid(sha1(1))
                .as_git_oid()
                .map(GitObjectId::algorithm),
            Value::git_oid(sha256(1))
                .as_git_oid()
                .map(GitObjectId::algorithm)
        );
        assert_ne!(Value::git_oid(sha1(1)), Value::Digest([1; 32]));
        assert_ne!(Value::git_oid(sha1(1)), Value::Uuid([1; 16]));
        assert_ne!(Value::git_oid(sha1(1)), Value::Blob(vec![1; 20].into()));
        assert!(Value::git_oid(sha1(1)).as_sys_ref().is_none());
    }

    // Specified: ORNA-SYS-016, ORNA-FORMAT-001; Orna-1.1.0 29-formats.md, tag 60023.
    #[test]
    fn only_a_well_formed_sys_git_oid_is_typed() {
        let opaque = |name: &str, representation: Raw| {
            Raw::Tag(
                OPAQUE_SYSTEM_ID_TAG,
                Box::new(Raw::Array(vec![Raw::Text(name.to_owned()), representation])),
            )
        };
        let git = |algorithm: &str, bytes: Vec<u8>| {
            Raw::Array(vec![Raw::Text(algorithm.to_owned()), Raw::Bytes(bytes)])
        };
        assert!(git_oid_from_raw(&opaque("sys.GitOid", git("sha1", vec![7; 20]))).is_some());
        assert!(git_oid_from_raw(&opaque("sys.GitOid", git("sha256", vec![7; 32]))).is_some());
        for rejected in [
            // The width disagrees with the algorithm.
            opaque("sys.GitOid", git("sha1", vec![7; 32])),
            opaque("sys.GitOid", git("sha256", vec![7; 20])),
            // An unknown algorithm.
            opaque("sys.GitOid", git("md5", vec![7; 16])),
            // Another identifier is not a Git object id.
            opaque("sys.RevisionId", Raw::Bytes(vec![7; 32])),
            // Neither is a bare `[algorithm, bytes]` key.
            git("sha1", vec![7; 20]),
        ] {
            assert!(git_oid_from_raw(&rejected).is_none(), "{rejected:?}");
        }
    }

    // Specified: ORNA-SYS-016; an opaque identifier that is not a `sys.GitOid` stays opaque.
    #[test]
    fn another_opaque_identifier_is_kept_opaque() {
        let revision = Raw::Tag(
            OPAQUE_SYSTEM_ID_TAG,
            Box::new(Raw::Array(vec![
                Raw::Text("sys.RevisionId".to_owned()),
                Raw::Bytes(vec![9; 32]),
            ])),
        );
        let read = crate::raw_value::from_raw(&revision).expect("an opaque identifier reads back");
        assert!(matches!(&read, Value::RowRef(raw) if *raw == revision));
        assert!(read.as_git_oid().is_none());
    }
}
