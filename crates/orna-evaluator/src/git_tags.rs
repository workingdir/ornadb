//! The `sys.git.tags` rows: the `sys.Tag` projection of the tags of the
//! repository the host attached (ORNA-SYS-111, ORNA-SYS-112).
//!
//! `reference` is the `sys.TagRef` of the name and `object` the `sys.GitObjectRef`
//! of the object the tag names: what an annotated tag's tag object tags (one
//! level, so a tag of a tag names the inner tag object) or what a lightweight
//! tag's ref names. `annotated` says which. `message` and `tagger` are the tag
//! object's, so a lightweight tag has neither, and an annotated tag has no
//! `tagger` only when its tag object records none. [`crate::git_rows`] holds what
//! every Git member shares: the repository read, the commit pin and the failure
//! when either is unavailable.

use std::collections::BTreeMap;

use orna_repository::CommittedTag;
use orna_sys::SysHostContext;
use orna_value::{git_object_reference, tag_reference};

use crate::git_rows::{
    GitPage, RowPin, incomplete, object_id, page_rows, person_identity, repository,
};
use crate::{EvaluationError, Value};

/// Most tags one read lists. A repository with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_TAGS: usize = 10_000;

/// Serves the page of `sys.git.tags` that follows the tag named by `after`, from
/// the repository `context` attached.
pub(crate) fn tags_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let mut tags = repository(context)?
        .local_tags(MAX_TAGS)
        .map_err(|_| incomplete())?;
    tags.sort_by(|left, right| left.name().cmp(right.name()));
    let facts = tags.iter().map(TagFacts::of).collect::<Vec<_>>();
    page_rows(context, &facts, |tag| tag.name, after, rows, tag_row)
}

/// What one `sys.Tag` row is built from.
struct TagFacts<'a> {
    name: &'a str,
    /// The object the tag names, in Git's hexadecimal spelling.
    object: &'a str,
    /// What the tag object of an annotated tag records.
    annotation: Option<AnnotationFacts<'a>>,
}

struct AnnotationFacts<'a> {
    message: &'a str,
    tagger: Option<TaggerFacts<'a>>,
}

struct TaggerFacts<'a> {
    name: &'a str,
    email: Option<&'a str>,
}

impl<'a> TagFacts<'a> {
    fn of(tag: &'a CommittedTag) -> Self {
        Self {
            name: tag.name(),
            object: tag.object().as_str(),
            annotation: tag.annotation().map(|annotation| AnnotationFacts {
                message: annotation.message(),
                tagger: annotation.tagger().map(|tagger| TaggerFacts {
                    name: tagger.name(),
                    email: tagger.email(),
                }),
            }),
        }
    }
}

/// The `sys.Tag` row of one tag: `{ reference, name, object, annotated, message,
/// tagger }` (34-system-reference.md, `sys.Tag`).
fn tag_row(pin: &RowPin, tag: &TagFacts<'_>) -> Result<Value, EvaluationError> {
    let object = object_id(tag.object)?;
    let (message, tagger) = match &tag.annotation {
        Some(annotation) => (
            Value::Option(Some(Box::new(Value::String(annotation.message.to_owned())))),
            Value::Option(
                annotation
                    .tagger
                    .as_ref()
                    .map(|tagger| Box::new(person_identity(tagger.name, tagger.email))),
            ),
        ),
        None => (Value::Option(None), Value::Option(None)),
    };
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| tag_reference(database, snapshot, tag.name))?,
        ),
        ("name".to_owned(), Value::String(tag.name.to_owned())),
        (
            "object".to_owned(),
            pin.reference(|database, snapshot| git_object_reference(database, snapshot, object))?,
        ),
        (
            "annotated".to_owned(),
            Value::Bool(tag.annotation.is_some()),
        ),
        ("message".to_owned(), message),
        ("tagger".to_owned(), tagger),
    ])))
}

#[cfg(test)]
mod tests {
    use super::{AnnotationFacts, TagFacts, TaggerFacts, tag_row};
    use crate::Value;
    use crate::git_rows::testing::{DATABASE, fields, oid, pin, reference_of, row_pin};
    use orna_value::{git_object_reference, relations, tag_reference};

    const COMMIT: &str = "00112233445566778899aabbccddeeff00112233";

    fn lightweight() -> TagFacts<'static> {
        TagFacts {
            name: "v1",
            object: COMMIT,
            annotation: None,
        }
    }

    fn annotated(tagger: Option<TaggerFacts<'static>>) -> TagFacts<'static> {
        TagFacts {
            name: "v2",
            object: COMMIT,
            annotation: Some(AnnotationFacts {
                message: "Release 2\n\nnotes\n",
                tagger,
            }),
        }
    }

    fn some(value: Value) -> Value {
        Value::Option(Some(Box::new(value)))
    }

    // Specified: 34-system-reference.md `sys.Tag`; ORNA-SYS-111, ORNA-SYS-112.
    #[test]
    fn a_lightweight_tag_has_no_message_or_tagger() {
        let row = tag_row(&row_pin(), &lightweight()).expect("a tag row");
        let fields = fields(&row);
        assert_eq!(
            fields.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "annotated",
                "message",
                "name",
                "object",
                "reference",
                "tagger"
            ],
            "exactly the declared fields"
        );
        assert_eq!(fields["name"], Value::String("v1".to_owned()));
        assert_eq!(fields["annotated"], Value::Bool(false));
        assert_eq!(fields["message"], Value::Option(None));
        assert_eq!(fields["tagger"], Value::Option(None));

        let tag = reference_of(&fields["reference"]);
        assert!(tag.is_of(&relations::TAG), "a sys.TagRef");
        assert_eq!(tag.key_text(), Some("v1"));
        assert_eq!(
            tag.as_row_ref().to_canonical().expect("encodes"),
            tag_reference(DATABASE, pin(), "v1")
                .expect("tag reference")
                .to_canonical()
                .expect("encodes")
        );
        let object = reference_of(&fields["object"]);
        assert!(object.is_of(&relations::GIT_OBJECT), "a sys.GitObjectRef");
        assert_eq!(object.key_oid(), Some(oid(COMMIT)));
        assert_eq!(
            object.as_row_ref().to_canonical().expect("encodes"),
            git_object_reference(DATABASE, pin(), oid(COMMIT))
                .expect("object reference")
                .to_canonical()
                .expect("encodes")
        );
    }

    #[test]
    fn an_annotated_tag_has_its_message_and_tagger() {
        let tagger = TaggerFacts {
            name: "Ada Lovelace",
            email: Some("ada@example.invalid"),
        };
        let row = tag_row(&row_pin(), &annotated(Some(tagger))).expect("a tag row");
        let fields = fields(&row);
        assert_eq!(fields["annotated"], Value::Bool(true));
        assert_eq!(
            fields["message"],
            some(Value::String("Release 2\n\nnotes\n".to_owned()))
        );
        let Value::Option(Some(identity)) = &fields["tagger"] else {
            panic!("an annotated tag with a tagger names it");
        };
        let Value::Record(identity) = identity.as_ref() else {
            panic!("a sys.PersonIdentity is a record");
        };
        assert_eq!(
            identity.keys().map(String::as_str).collect::<Vec<_>>(),
            ["email", "name"]
        );
        assert_eq!(identity["name"], Value::String("Ada Lovelace".to_owned()));
        assert_eq!(
            identity["email"],
            some(Value::String("ada@example.invalid".to_owned()))
        );
    }

    #[test]
    fn a_tagger_without_an_address_has_a_null_email_and_a_missing_tagger_is_null() {
        let nameless_address = TaggerFacts {
            name: "A",
            email: None,
        };
        let row = tag_row(&row_pin(), &annotated(Some(nameless_address))).expect("a tag row");
        let Value::Option(Some(identity)) = &fields(&row)["tagger"] else {
            panic!("a tagger");
        };
        let Value::Record(identity) = identity.as_ref() else {
            panic!("a record");
        };
        assert_eq!(identity["email"], Value::Option(None));

        let row = tag_row(&row_pin(), &annotated(None)).expect("a tag row");
        let fields = fields(&row);
        assert_eq!(fields["tagger"], Value::Option(None), "no tagger header");
        assert_eq!(
            fields["message"],
            some(Value::String("Release 2\n\nnotes\n".to_owned())),
            "the message is the tag object's, whether or not it has a tagger"
        );
        assert_eq!(fields["annotated"], Value::Bool(true));
    }

    #[test]
    fn a_sha256_object_is_a_sha256_object_reference() {
        let object = "a".repeat(64);
        let tag = TagFacts {
            object: &object,
            ..lightweight()
        };
        let row = tag_row(&row_pin(), &tag).expect("a tag row");
        assert_eq!(
            reference_of(&fields(&row)["object"]).key_oid(),
            Some(oid(&object))
        );
    }

    #[test]
    fn an_object_that_is_not_an_object_id_fails_closed() {
        for object in ["", "v1", &"g".repeat(40), &"a".repeat(41)] {
            let tag = TagFacts {
                object,
                ..lightweight()
            };
            let failure = tag_row(&row_pin(), &tag)
                .expect_err("no row is built over an object that is not an object id");
            assert_eq!(failure.code(), "sys.snapshot.incomplete", "{object:?}");
        }
    }

    #[test]
    fn a_tag_with_no_name_has_no_reference() {
        let tag = TagFacts {
            name: "",
            ..lightweight()
        };
        let failure = tag_row(&row_pin(), &tag).expect_err("a reference over an empty name");
        assert_eq!(failure.code(), "sys.snapshot.incomplete");
    }
}
