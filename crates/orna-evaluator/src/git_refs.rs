//! The `sys.git.refs` rows: the `sys.Ref` projection of the refs of the
//! repository the host attached (ORNA-SYS-050, ORNA-SYS-111, ORNA-SYS-112).
//!
//! `name` is the full ref name (`refs/heads/main`), the key of the `sys.RefRef`
//! that `reference` holds and that `sys.Branch.upstream` already names a
//! tracked ref by. `target` is the `sys.GitObjectRef` of the object the ref
//! names: for a symbolic ref the object the ref it points at names, for an
//! annotated tag its tag object. `symbolic_target` is the full name of the ref
//! a symbolic ref points at, and `hidden` says the ref lives in `refs/orna/`,
//! Orna's own namespace of ordinary Git refs. `remote` is the
//! `sys.RemoteRef` of the configured remote a remote-tracking ref
//! (`refs/remotes/<remote>/...`) belongs to, and null for every other ref and
//! for a remote that is not configured. [`crate::git_rows`] holds what every Git
//! member shares: the repository read, the commit pin and the failure when
//! either is unavailable.

use std::collections::BTreeMap;

use orna_repository::CommittedRef;
use orna_sys::SysHostContext;
use orna_value::{git_object_reference, git_ref_reference, remote_reference};

use crate::git_remotes::MAX_REMOTES;
use crate::git_rows::{GitPage, RowPin, incomplete, object_id, page_rows, repository};
use crate::{EvaluationError, Value};

/// Most refs one read lists. A repository with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_REFS: usize = 10_000;

/// Serves the page of `sys.git.refs` that follows the ref named by `after`,
/// from the repository `context` attached.
pub(crate) fn refs_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let repository = repository(context)?;
    let mut refs = repository
        .committed_refs(MAX_REFS)
        .map_err(|_| incomplete())?;
    refs.sort_by(|left, right| left.name().cmp(right.name()));
    // The configuration is read only when some ref could belong to a remote.
    let remotes = if refs.iter().any(CommittedRef::is_remote_tracking) {
        repository
            .configured_remotes(MAX_REMOTES)
            .map_err(|_| incomplete())?
    } else {
        Vec::new()
    };
    let remote_names = || remotes.iter().map(|remote| remote.name());
    let facts = refs
        .iter()
        .map(|listed| RefFacts::of(listed, remote_names()))
        .collect::<Vec<_>>();
    page_rows(context, &facts, |listed| listed.name, after, rows, ref_row)
}

/// What one `sys.Ref` row is built from.
struct RefFacts<'a> {
    /// The full ref name.
    name: &'a str,
    /// The object the ref names, in Git's hexadecimal spelling.
    target: &'a str,
    /// The full name of the ref a symbolic ref points at.
    symbolic_target: Option<&'a str>,
    hidden: bool,
    /// The configured remote a remote-tracking ref belongs to.
    remote: Option<&'a str>,
}

impl<'a> RefFacts<'a> {
    fn of(listed: &'a CommittedRef, remotes: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            name: listed.name(),
            target: listed.target().as_str(),
            symbolic_target: listed.symbolic_target(),
            hidden: listed.is_hidden(),
            remote: listed.remote(remotes),
        }
    }
}

/// The `sys.Ref` row of one ref: `{ reference, name, target, symbolic_target,
/// hidden, remote }` (34-system-reference.md, `sys.Ref`).
fn ref_row(pin: &RowPin, listed: &RefFacts<'_>) -> Result<Value, EvaluationError> {
    let target = object_id(listed.target)?;
    let remote = listed
        .remote
        .map(|remote| {
            pin.reference(|database, snapshot| remote_reference(database, snapshot, remote))
                .map(Box::new)
        })
        .transpose()?;
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            pin.reference(|database, snapshot| git_ref_reference(database, snapshot, listed.name))?,
        ),
        ("name".to_owned(), Value::String(listed.name.to_owned())),
        (
            "target".to_owned(),
            pin.reference(|database, snapshot| git_object_reference(database, snapshot, target))?,
        ),
        (
            "symbolic_target".to_owned(),
            Value::Option(
                listed
                    .symbolic_target
                    .map(|target| Box::new(Value::String(target.to_owned()))),
            ),
        ),
        ("hidden".to_owned(), Value::Bool(listed.hidden)),
        ("remote".to_owned(), Value::Option(remote)),
    ])))
}

#[cfg(test)]
mod tests {
    use super::{RefFacts, ref_row};
    use crate::Value;
    use crate::git_rows::testing::{DATABASE, fields, oid, pin, reference_of, row_pin};
    use orna_value::{git_object_reference, git_ref_reference, relations, remote_reference};

    const TIP: &str = "00112233445566778899aabbccddeeff00112233";

    fn branch() -> RefFacts<'static> {
        RefFacts {
            name: "refs/heads/main",
            target: TIP,
            symbolic_target: None,
            hidden: false,
            remote: None,
        }
    }

    #[test]
    fn a_ref_row_names_the_full_name_and_its_references() {
        let row = ref_row(&row_pin(), &branch()).expect("a row");
        let row = fields(&row);
        assert_eq!(
            row.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "hidden",
                "name",
                "reference",
                "remote",
                "symbolic_target",
                "target"
            ]
        );
        assert_eq!(
            row["name"],
            Value::String("refs/heads/main".to_owned()),
            "the full ref name, not the branch name"
        );
        let reference = reference_of(&row["reference"]);
        assert!(reference.is_of(&relations::REF));
        assert_eq!(reference.key_text(), Some("refs/heads/main"));
        assert_eq!(
            reference,
            git_ref_reference(DATABASE, pin(), "refs/heads/main").expect("a reference"),
        );
        assert_eq!(
            reference_of(&row["target"]),
            git_object_reference(DATABASE, pin(), oid(TIP)).expect("a reference")
        );
        assert_eq!(row["hidden"], Value::Bool(false));
        assert_eq!(row["symbolic_target"], Value::Option(None));
        assert_eq!(row["remote"], Value::Option(None));
    }

    #[test]
    fn a_symbolic_ref_names_the_ref_it_points_at() {
        let symbolic = RefFacts {
            name: "refs/remotes/origin/HEAD",
            symbolic_target: Some("refs/remotes/origin/main"),
            remote: Some("origin"),
            ..branch()
        };
        let row = ref_row(&row_pin(), &symbolic).expect("a row");
        let row = fields(&row);
        assert_eq!(
            row["symbolic_target"],
            Value::Option(Some(Box::new(Value::String(
                "refs/remotes/origin/main".to_owned()
            ))))
        );
        let Value::Option(Some(remote)) = &row["remote"] else {
            panic!("a remote-tracking ref has a remote");
        };
        let remote = reference_of(remote);
        assert!(remote.is_of(&relations::REMOTE));
        assert_eq!(
            remote,
            remote_reference(DATABASE, pin(), "origin").expect("a reference")
        );
    }

    #[test]
    fn the_orna_namespace_is_hidden() {
        let hidden = RefFacts {
            name: "refs/orna/ids/00112233",
            hidden: true,
            ..branch()
        };
        let row = ref_row(&row_pin(), &hidden).expect("a row");
        assert_eq!(fields(&row)["hidden"], Value::Bool(true));
    }

    #[test]
    fn an_object_id_of_the_wrong_width_is_refused_not_served() {
        let broken = RefFacts {
            target: "0123",
            ..branch()
        };
        let failure = ref_row(&row_pin(), &broken).expect_err("no object to name");
        assert_eq!(failure.code(), "sys.snapshot.incomplete");
    }
}
