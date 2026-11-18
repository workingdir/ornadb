//! The tags of a repository, as `sys.Tag` projects them: the object each tag
//! names and, for an annotated tag, what its tag object records.
//!
//! One `for-each-ref` run lists `refs/tags` together with the facts of the
//! objects the refs name, so the tags of one listing and the annotation of each
//! are one observation. Nothing here is guessed: a record that does not have
//! the shape Git writes, a name that is not a ref name, or an object id of the
//! wrong width refuses the whole listing.

use crate::{
    NativeObjectId, Repository, RepositoryError, listing_text, require_native_object_id,
    run_bounded_listing, valid_branch_name,
};

/// A person as a tag object records its tagger: `Name <email>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitPerson {
    name: String,
    email: Option<String>,
}

impl GitPerson {
    /// The name the identity line records. It may be empty: Git accepts
    /// `<email>` with no name before it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The address between the angle brackets, or `None` when the identity line
    /// records none (`Name <>`).
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }
}

/// What the tag object of an annotated tag records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagAnnotation {
    message: String,
    tagger: Option<GitPerson>,
}

impl TagAnnotation {
    /// The message as Git reports it for a tag (`%(contents)`): everything after
    /// the object's headers, with the leading blank lines Git skips dropped. A
    /// signed tag's signature block is part of it, as it is part of the object.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The tagger, or `None` for a tag object that has no `tagger` header
    /// (Git's earliest tag objects have none).
    pub fn tagger(&self) -> Option<&GitPerson> {
        self.tagger.as_ref()
    }
}

/// One tag of `refs/tags`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedTag {
    name: String,
    object: NativeObjectId,
    annotation: Option<TagAnnotation>,
}

impl CommittedTag {
    /// The tag's name without its `refs/tags/` prefix.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The object the tag names: the object an annotated tag's `object` header
    /// records (one level, so a tag of a tag names the inner tag object), or the
    /// object the ref itself names for a lightweight tag.
    pub fn object(&self) -> &NativeObjectId {
        &self.object
    }

    /// Whether the tag is annotated: its ref names a tag object.
    pub fn is_annotated(&self) -> bool {
        self.annotation.is_some()
    }

    /// The annotation of an annotated tag; `None` for a lightweight one.
    pub fn annotation(&self) -> Option<&TagAnnotation> {
        self.annotation.as_ref()
    }
}

/// Fields of one record of the listing behind [`Repository::local_tags`].
const TAG_FIELDS: usize = 7;

/// The `for-each-ref` format of one tag record: the tag name, the type and id
/// of the object the ref names, the `object` header of a tag object (the id of
/// the object it tags, one level and not peeled), the tagger's name and email
/// and the tag message, each field ended by a NUL.
///
/// The message is requested only for a ref that names a tag object, so the
/// message of the commit a lightweight tag names is never read. Git reads every
/// field as a C string, so no field can hold a NUL byte and the NUL that ends
/// each one is an unambiguous terminator.
const TAG_FORMAT: &str = concat!(
    "--format=",
    "%(refname:strip=2)%00",
    "%(objecttype)%00",
    "%(objectname)%00",
    "%(object)%00",
    "%(taggername)%00",
    "%(taggeremail)%00",
    "%(if:equals=tag)%(objecttype)%(then)%(contents)%(end)%00",
);

impl Repository {
    /// Lists the tags of `refs/tags` in name order: the object each names, and
    /// for an annotated tag its message and tagger.
    ///
    /// One `for-each-ref` run observes the refs and the tag objects together.
    /// More than `max_tags` tags, or a listing larger than the bounded-listing
    /// limit, is refused rather than served truncated, and so is a tag whose
    /// object is missing: nothing is reported for a tag that cannot be read.
    pub fn local_tags(&self, max_tags: usize) -> Result<Vec<CommittedTag>, RepositoryError> {
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["for-each-ref", "--sort=refname", "--count"])
            .arg(max_tags.saturating_add(1).to_string())
            .arg(TAG_FORMAT)
            .arg("refs/tags");
        let output = run_bounded_listing(command)?;

        let tags = parse_committed_tags(&output, self.native_object_id_length()?)?;
        if tags.len() > max_tags {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(tags)
    }
}

/// Whether `name` is a tag name Git could have written. A ref under `refs/tags`
/// may start with a dash (`git update-ref refs/tags/-odd`; `git tag` refuses to
/// create one but lists it), so the dashes are set aside before the ref-name
/// rules a branch name follows are applied.
fn valid_tag_name(name: &str) -> bool {
    valid_branch_name(name.trim_start_matches('-'))
}

/// Parses the whole listing: records of [`TAG_FIELDS`] NUL-terminated fields,
/// each followed by the newline `for-each-ref` ends its lines with. Anything
/// else is refused.
fn parse_committed_tags(
    listing: &[u8],
    object_id_length: usize,
) -> Result<Vec<CommittedTag>, RepositoryError> {
    let mut rest = listing;
    let mut tags = Vec::new();
    while !rest.is_empty() {
        let mut fields: [&[u8]; TAG_FIELDS] = [&[]; TAG_FIELDS];
        for field in &mut fields {
            let end = rest
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(RepositoryError::GitOperationFailed)?;
            (*field, rest) = (&rest[..end], &rest[end + 1..]);
        }
        rest = rest
            .strip_prefix(b"\n")
            .ok_or(RepositoryError::GitOperationFailed)?;
        tags.push(parse_committed_tag(fields, object_id_length)?);
    }
    Ok(tags)
}

/// Reads one record. A ref naming a tag object is an annotated tag and must
/// name the object the tag object tags; a ref naming a commit, tree or blob is a
/// lightweight tag and carries no tag object, so every field about one is empty.
fn parse_committed_tag(
    fields: [&[u8]; TAG_FIELDS],
    object_id_length: usize,
) -> Result<CommittedTag, RepositoryError> {
    let [
        name,
        kind,
        named,
        tagged,
        tagger_name,
        tagger_email,
        contents,
    ] = fields;
    let name = listing_text(name)?;
    if !valid_tag_name(name) {
        return Err(RepositoryError::GitOperationFailed);
    }
    let object_id = |bytes: &[u8]| {
        let hex = listing_text(bytes)?;
        require_native_object_id(hex, object_id_length)?;
        NativeObjectId::new(hex)
    };
    let (object, annotation) = match kind {
        b"tag" => (
            object_id(tagged)?,
            Some(TagAnnotation {
                message: listing_text(contents)?.to_owned(),
                tagger: parse_tagger(tagger_name, tagger_email)?,
            }),
        ),
        b"commit" | b"tree" | b"blob" => {
            if ![tagged, tagger_name, tagger_email, contents]
                .iter()
                .all(|field| field.is_empty())
            {
                return Err(RepositoryError::GitOperationFailed);
            }
            (object_id(named)?, None)
        }
        _ => return Err(RepositoryError::GitOperationFailed),
    };
    Ok(CommittedTag {
        name: name.to_owned(),
        object,
        annotation,
    })
}

/// Reads the tagger from `%(taggername)` and `%(taggeremail)`. Both are empty
/// when the tag object has no `tagger` header; otherwise the email is the
/// address in angle brackets, empty for `Name <>`.
fn parse_tagger(name: &[u8], email: &[u8]) -> Result<Option<GitPerson>, RepositoryError> {
    if name.is_empty() && email.is_empty() {
        return Ok(None);
    }
    let address = listing_text(email)?
        .strip_prefix('<')
        .and_then(|address| address.strip_suffix('>'))
        .ok_or(RepositoryError::GitOperationFailed)?;
    Ok(Some(GitPerson {
        name: listing_text(name)?.to_owned(),
        email: (!address.is_empty()).then(|| address.to_owned()),
    }))
}

#[cfg(test)]
mod tests {
    use super::{CommittedTag, parse_committed_tags};
    use crate::RepositoryError;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const TAG_OBJECT: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const TREE: &str = "89abcdef0123456789abcdef0123456789abcdef";

    /// One record as `for-each-ref` prints it.
    fn record(fields: [&str; 7]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in fields {
            bytes.extend_from_slice(field.as_bytes());
            bytes.push(0);
        }
        bytes.push(b'\n');
        bytes
    }

    fn lightweight(name: &str, kind: &str, object: &str) -> Vec<u8> {
        record([name, kind, object, "", "", "", ""])
    }

    fn annotated(name: &str, tagger: (&str, &str), contents: &str) -> Vec<u8> {
        record([
            name, "tag", TAG_OBJECT, COMMIT, tagger.0, tagger.1, contents,
        ])
    }

    fn parse(bytes: &[u8]) -> Result<Vec<CommittedTag>, RepositoryError> {
        parse_committed_tags(bytes, 40)
    }

    #[test]
    fn a_lightweight_tag_names_what_its_ref_names_and_has_no_annotation() {
        for kind in ["commit", "tree", "blob"] {
            let tags = parse(&lightweight("v1", kind, TREE)).expect("a lightweight tag");
            let [tag] = tags.as_slice() else {
                panic!("one tag");
            };
            assert_eq!(tag.name(), "v1");
            assert_eq!(tag.object().as_str(), TREE);
            assert!(!tag.is_annotated());
            assert_eq!(tag.annotation(), None);
        }
    }

    #[test]
    fn an_annotated_tag_names_its_target_and_carries_its_message_and_tagger() {
        let tags = parse(&annotated(
            "v2",
            ("Ada Lovelace", "<ada@example.invalid>"),
            "Release 2\n\nnotes\n",
        ))
        .expect("an annotated tag");
        let [tag] = tags.as_slice() else {
            panic!("one tag");
        };
        assert_eq!(
            tag.object().as_str(),
            COMMIT,
            "the object the tag object tags, not the tag object"
        );
        assert!(tag.is_annotated());
        let annotation = tag.annotation().expect("an annotation");
        assert_eq!(annotation.message(), "Release 2\n\nnotes\n");
        let tagger = annotation.tagger().expect("a tagger");
        assert_eq!(tagger.name(), "Ada Lovelace");
        assert_eq!(tagger.email(), Some("ada@example.invalid"));
    }

    #[test]
    fn an_annotated_tag_may_have_an_empty_message_no_tagger_or_no_address() {
        let mut listing = annotated("a-empty", ("T", "<t@example.invalid>"), "");
        listing.extend(annotated("b-old", ("", ""), "old tag\n"));
        listing.extend(annotated("c-nameless", ("", "<n@example.invalid>"), "m\n"));
        listing.extend(annotated("d-addressless", ("A", "<>"), "m\n"));
        let tags = parse(&listing).expect("four annotated tags");
        let annotations = tags
            .iter()
            .map(|tag| tag.annotation().expect("annotated"))
            .collect::<Vec<_>>();
        assert_eq!(annotations[0].message(), "", "an empty message is present");
        assert_eq!(annotations[1].tagger(), None, "no tagger header");
        let nameless = annotations[2].tagger().expect("a tagger");
        assert_eq!(
            (nameless.name(), nameless.email()),
            ("", Some("n@example.invalid"))
        );
        let addressless = annotations[3].tagger().expect("a tagger");
        assert_eq!((addressless.name(), addressless.email()), ("A", None));
    }

    #[test]
    fn records_are_read_in_order_and_an_empty_listing_has_no_tag() {
        assert_eq!(parse(b"").expect("an empty listing"), Vec::new());
        let mut listing = lightweight("a", "commit", COMMIT);
        listing.extend(annotated("b", ("T", "<t@example.invalid>"), "m\n"));
        listing.extend(lightweight("c/d", "commit", COMMIT));
        let names = parse(&listing)
            .expect("three tags")
            .iter()
            .map(|tag| tag.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["a", "b", "c/d"]);
    }

    #[test]
    fn a_message_may_hold_text_that_looks_like_another_record() {
        let message = "first\n\nv9\ncommit\n0123456789abcdef0123456789abcdef01234567\n";
        let mut listing = annotated("v1", ("T", "<t@example.invalid>"), message);
        listing.extend(lightweight("v2", "commit", COMMIT));
        let tags = parse(&listing).expect("two tags");
        let names = tags.iter().map(CommittedTag::name).collect::<Vec<_>>();
        assert_eq!(names, ["v1", "v2"]);
        assert_eq!(tags[0].annotation().expect("annotated").message(), message);
    }

    #[test]
    fn a_tag_name_may_start_with_a_dash_unlike_a_branch_name() {
        let tags = parse(&lightweight("-odd", "commit", COMMIT)).expect("a tag named -odd");
        assert_eq!(tags[0].name(), "-odd");
    }

    #[test]
    fn a_record_git_did_not_write_is_refused() {
        let good = annotated("v1", ("T", "<t@example.invalid>"), "m\n");
        let refused: Vec<(&str, Vec<u8>)> = vec![
            ("no final newline", good[..good.len() - 1].to_vec()),
            ("a short record", b"v1\0commit\0".to_vec()),
            ("an unterminated field", b"v1".to_vec()),
            (
                "an unknown object type",
                lightweight("v1", "snapshot", COMMIT),
            ),
            ("an invalid tag name", lightweight("a..b", "commit", COMMIT)),
            ("an empty tag name", lightweight("", "commit", COMMIT)),
            ("a tag name of dashes", lightweight("--", "commit", COMMIT)),
            (
                "an object id of the wrong width",
                lightweight("v1", "commit", "0123"),
            ),
            (
                "an object id that is not hexadecimal",
                lightweight("v1", "commit", &"g".repeat(40)),
            ),
            (
                "a lightweight tag with a message",
                record(["v1", "commit", COMMIT, "", "", "", "stray"]),
            ),
            (
                "a lightweight tag with a deref object",
                record(["v1", "commit", COMMIT, COMMIT, "", "", ""]),
            ),
            (
                "an annotated tag with no target",
                record(["v1", "tag", TAG_OBJECT, "", "T", "<t@example.invalid>", ""]),
            ),
            (
                "a tagger name with no address",
                annotated("v1", ("T", ""), "m\n"),
            ),
            (
                "a tagger address without brackets",
                annotated("v1", ("T", "t@example.invalid"), "m\n"),
            ),
            (
                "a message that is not text",
                [
                    b"v1\0tag\0".as_slice(),
                    TAG_OBJECT.as_bytes(),
                    b"\0",
                    COMMIT.as_bytes(),
                    b"\0T\0<t@example.invalid>\0\xff\xfe\0\n",
                ]
                .concat(),
            ),
        ];
        for (what, bytes) in refused {
            assert!(
                matches!(
                    parse(&bytes),
                    Err(RepositoryError::GitOperationFailed | RepositoryError::InvalidObjectId)
                ),
                "{what}"
            );
        }
    }
}
