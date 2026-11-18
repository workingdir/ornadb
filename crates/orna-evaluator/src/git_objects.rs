//! The `sys.git.objects` rows: the `sys.GitObject` projection of the objects in the
//! object store of the repository the host attached (ORNA-SYS-052, ORNA-SYS-111,
//! ORNA-SYS-112).
//!
//! One row per object, in object-id order: `kind` is the `sys.GitObjectKind` Git reads
//! the object as and `size` its size in bytes. The store lists only the objects it
//! holds, so every row is available locally and has no promisor remote. A partial
//! clone, whose store does not enumerate the objects it has promised, is refused with
//! `sys.snapshot.incomplete`. [`crate::git_rows`] holds what every Git member shares:
//! the repository read, the commit pin and the failure when either is unavailable.

use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_repository::{CommittedGitObject, GitObjectKind};
use orna_sys::{SysEnum, SysHostContext};
use orna_value::{enum_type_id, enum_variant_id, git_object_reference};

use crate::git_rows::{GitPage, RowPin, incomplete, object_id, page_ordered, repository};
use crate::{EvaluationError, Value, object_id_raw};

/// Most objects one read lists. A repository with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_OBJECTS: usize = 50_000;

/// Serves the page of `sys.git.objects` that follows the object whose id is `after`,
/// from the repository `context` attached. The cursor of a page is the hexadecimal id
/// of the last object it served, so objects order by id as Git spells them.
pub(crate) fn objects_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let objects = repository(context)?
        .committed_git_objects(MAX_OBJECTS)
        .map_err(|_| incomplete())?;
    let facts = objects.iter().map(ObjectFacts::of).collect::<Vec<_>>();
    page_ordered(
        context,
        &facts,
        |object| object.oid.as_bytes().to_vec(),
        after,
        rows,
        object_row,
    )
}

/// What one `sys.GitObject` row is built from.
struct ObjectFacts<'a> {
    /// The object's id, in Git's hexadecimal spelling.
    oid: &'a str,
    kind: GitObjectKind,
    size: u64,
}

impl<'a> ObjectFacts<'a> {
    fn of(object: &'a CommittedGitObject) -> Self {
        Self {
            oid: object.oid().as_str(),
            kind: object.kind(),
            size: object.size(),
        }
    }
}

/// The `sys.GitObject` row of one object: `{ reference, oid, kind, size,
/// available_locally, promisor_remote }` (34-system-reference.md, `sys.GitObject`).
fn object_row(pin: &RowPin, object: &ObjectFacts<'_>) -> Result<Value, EvaluationError> {
    let oid = object_id(object.oid)?;
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| {
                git_object_reference(database, snapshot, oid.clone())
            })?,
        ),
        ("oid".to_owned(), Value::GitOid(oid)),
        ("kind".to_owned(), kind_value(object.kind)),
        (
            "size".to_owned(),
            Value::Option(Some(Box::new(Value::Int(BigInt::from(object.size))))),
        ),
        ("available_locally".to_owned(), Value::Bool(true)),
        ("promisor_remote".to_owned(), Value::Option(None)),
    ])))
}

/// The `sys.GitObjectKind` value of `kind`, named as the spec names its variants.
///
/// The type and the variant are the canonical object ids of the enum and of the
/// variant, the identity every host derives from the qualified enum name.
fn kind_value(kind: GitObjectKind) -> Value {
    let variant = match kind {
        GitObjectKind::Commit => "commit",
        GitObjectKind::Tree => "tree",
        GitObjectKind::Blob => "blob",
        GitObjectKind::Tag => "tag",
    };
    let enum_name = SysEnum::GitObjectKind.name();
    Value::Enum {
        type_id: object_id_raw(enum_type_id(enum_name)),
        variant_id: object_id_raw(enum_variant_id(enum_name, variant)),
        payload: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ObjectFacts, kind_value, object_row};
    use crate::Value;
    use crate::git_rows::testing::{DATABASE, fields, oid, pin, reference_of, row_pin};
    use crate::object_id_raw;
    use num_bigint::BigInt;
    use orna_repository::GitObjectKind;
    use orna_value::{enum_type_id, enum_variant_id, git_object_reference, relations};

    const BLOB_OID: &str = "00112233445566778899aabbccddeeff00112233";

    fn entry(kind: GitObjectKind, size: u64) -> ObjectFacts<'static> {
        ObjectFacts {
            oid: BLOB_OID,
            kind,
            size,
        }
    }

    #[test]
    fn an_object_row_carries_its_id_kind_size_and_local_availability() {
        let row = object_row(&row_pin(), &entry(GitObjectKind::Blob, 7)).expect("a row");
        let row = fields(&row);
        assert_eq!(
            row.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "available_locally",
                "kind",
                "oid",
                "promisor_remote",
                "reference",
                "size"
            ]
        );
        assert_eq!(row["oid"], Value::GitOid(oid(BLOB_OID)));
        assert_eq!(row["kind"], kind_value(GitObjectKind::Blob));
        assert_eq!(
            row["size"],
            Value::Option(Some(Box::new(Value::Int(BigInt::from(7)))))
        );
        assert_eq!(row["available_locally"], Value::Bool(true));
        assert_eq!(row["promisor_remote"], Value::Option(None));

        let reference = reference_of(&row["reference"]);
        assert!(reference.is_of(&relations::GIT_OBJECT));
        assert_eq!(
            reference,
            git_object_reference(DATABASE, pin(), oid(BLOB_OID)).expect("a reference")
        );
    }

    #[test]
    fn a_zero_byte_object_keeps_its_size_as_present() {
        let row = object_row(&row_pin(), &entry(GitObjectKind::Tree, 0)).expect("a row");
        assert_eq!(
            fields(&row)["size"],
            Value::Option(Some(Box::new(Value::Int(BigInt::from(0)))))
        );
    }

    #[test]
    fn kinds_are_named_as_the_spec_names_them() {
        for (kind, variant) in [
            (GitObjectKind::Commit, "commit"),
            (GitObjectKind::Tree, "tree"),
            (GitObjectKind::Blob, "blob"),
            (GitObjectKind::Tag, "tag"),
        ] {
            assert_eq!(
                kind_value(kind),
                Value::Enum {
                    type_id: object_id_raw(enum_type_id("sys.GitObjectKind")),
                    variant_id: object_id_raw(enum_variant_id("sys.GitObjectKind", variant)),
                    payload: None,
                }
            );
        }
    }
}
