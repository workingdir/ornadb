//! What the `sys.git.*` members the evaluator serves from the repository the host
//! attached have in common (ORNA-SYS-011, ORNA-SYS-111, ORNA-SYS-112).
//!
//! The evaluator reads the repository named by `SysHostContext::root` itself, so
//! every path that attaches a host context (`orna run`, the REPL's committed
//! reads and its writer, `orna serve`) serves the same rows. Each reference is
//! pinned to the commit the host attached at (`SysHostContext::head_commit`),
//! the pin the `sys.catalog.*` rows carry.
//!
//! Nothing is fabricated. A missing pin, a repository that cannot be read and an
//! object id that is not a Git object id each fail the read with
//! `sys.snapshot.incomplete` (required snapshot objects are unavailable).
//!
//! Rows are ordered by name, and the cursor of a page is the name of the last row
//! it served, so a row created or deleted between two pages of one read never
//! repeats or skips a row that persists.

use std::collections::BTreeMap;

use orna_repository::{GitHashAlgorithm, NativeOid, Repository};
use orna_sys::{SysFailureCode, SysGroupedHandle, SysHostContext};
use orna_value::{CanonicalSnapshot, GitHash, GitObjectId, SysRef, SystemReferenceError};

use crate::{EvaluationError, Value, error, git_branches, git_remotes, git_tags};

/// Rows one page serves, unless the activation's collection limit is lower.
///
/// The evaluator asks storage-backed relations for pages of one row so a scan
/// stays demand-driven. Every page here is one observation of the repository (a
/// few Git processes, however many rows it lists), so a page is as large as the
/// limit allows and a read of a typical repository is one observation.
const PAGE_ROWS: usize = 256;

/// One page of rows and the cursor of the next page, when there is one.
pub(crate) type GitPage = (Vec<Value>, Option<Vec<u8>>);

/// The failure of a read the repository cannot back: required snapshot objects
/// are unavailable.
pub(crate) fn incomplete() -> EvaluationError {
    error(SysFailureCode::SnapshotIncomplete.as_str())
}

/// The page of one member's rows that follows a cursor, at most `rows` of them.
type MemberPage = fn(&SysHostContext, Option<&[u8]>, usize) -> Result<GitPage, EvaluationError>;

/// One member the evaluator serves from the repository the host attached: its
/// grouped handle, and the page of its rows that follows a cursor.
struct Member {
    handle: SysGroupedHandle,
    page: MemberPage,
}

/// The members the evaluator serves from the repository. A handle linked in the
/// `orna-sys-id` generator (`LINKED_GROUPED_HANDLES`) with no entry here would be
/// admitted and then unserved, so the two lists name the same members.
const MEMBERS: [Member; 3] = [
    Member {
        handle: SysGroupedHandle::GitBranches,
        page: git_branches::branches_page,
    },
    Member {
        handle: SysGroupedHandle::GitRemotes,
        page: git_remotes::remotes_page,
    },
    Member {
        handle: SysGroupedHandle::GitTags,
        page: git_tags::tags_page,
    },
];

/// The grouped handle named `name` when the evaluator serves it from the
/// repository the host attached.
pub(crate) fn served_source(name: &str) -> Option<&'static str> {
    MEMBERS
        .iter()
        .map(|member| member.handle.name())
        .find(|handle| *handle == name)
}

/// Serves the page of the grouped handle `source` that follows the row named by
/// `after`, from the repository `context` attached; `None` when `source` is not
/// a member the evaluator serves from the repository. The page holds at least
/// `limit` rows when that many remain, and at most `max_items` unless `limit`
/// asks for more.
pub(crate) fn served_page(
    source: &str,
    context: &SysHostContext,
    after: Option<&[u8]>,
    limit: usize,
    max_items: usize,
) -> Option<Result<GitPage, EvaluationError>> {
    let member = MEMBERS
        .iter()
        .find(|member| member.handle.name() == source)?;
    Some((member.page)(
        context,
        after,
        limit.max(PAGE_ROWS.min(max_items)),
    ))
}

/// The repository the host attached, or `sys.snapshot.incomplete` when its root
/// is not one.
pub(crate) fn repository(context: &SysHostContext) -> Result<Repository, EvaluationError> {
    Repository::discover(&context.root).map_err(|_| incomplete())
}

/// The coordinates every reference of one read carries: the attached database
/// and the commit the host attached at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RowPin {
    database: [u8; 16],
    snapshot: CanonicalSnapshot,
}

impl RowPin {
    /// The pin of `context`: the commit `HEAD` named when the host attached. A
    /// host attached at no readable commit has none, and no row is pinned to a
    /// commit the activation does not read.
    pub(crate) fn of(context: &SysHostContext) -> Result<Self, EvaluationError> {
        let oid = context.head_commit.as_deref().ok_or_else(incomplete)?;
        let algorithm = match oid.len() {
            20 => GitHash::Sha1,
            32 => GitHash::Sha256,
            _ => return Err(incomplete()),
        };
        Ok(Self {
            database: context.database_id,
            snapshot: CanonicalSnapshot::Commit {
                database: context.database_id,
                algorithm,
                oid: oid.to_vec(),
            },
        })
    }

    /// The reference `build` makes of the pin, as the value a row holds.
    pub(crate) fn reference(
        &self,
        build: impl FnOnce([u8; 16], CanonicalSnapshot) -> Result<SysRef, SystemReferenceError>,
    ) -> Result<Value, EvaluationError> {
        build(self.database, self.snapshot.clone())
            .ok()
            .and_then(|reference| reference.to_canonical().ok())
            .map(Value::Reference)
            .ok_or_else(incomplete)
    }
}

/// The object id a Git object's hexadecimal spelling names, in the width its
/// hash algorithm has.
pub(crate) fn object_id(hex: &str) -> Result<GitObjectId, EvaluationError> {
    let (hash, algorithm) = match hex.len() {
        40 => (GitHash::Sha1, GitHashAlgorithm::Sha1),
        64 => (GitHash::Sha256, GitHashAlgorithm::Sha256),
        _ => return Err(incomplete()),
    };
    let oid = NativeOid::from_hex(algorithm, hex).map_err(|_| incomplete())?;
    GitObjectId::new(hash, oid.as_bytes().to_vec()).map_err(|_| incomplete())
}

/// A `sys.PersonIdentity` (`{ name, email }`): `email` is absent when the
/// identity records none (34-system-reference.md, `sys.PersonIdentity`).
pub(crate) fn person_identity(name: &str, email: Option<&str>) -> Value {
    Value::Record(BTreeMap::from([
        ("name".to_owned(), Value::String(name.to_owned())),
        (
            "email".to_owned(),
            Value::Option(email.map(|email| Box::new(Value::String(email.to_owned())))),
        ),
    ]))
}

/// Builds the page of `rows` (in name order) that follows the row named by
/// `after`, building each row served with `row`. The pin is read only when the
/// page holds a row to pin.
pub(crate) fn page_rows<T>(
    context: &SysHostContext,
    rows: &[T],
    name: impl Fn(&T) -> &str,
    after: Option<&[u8]>,
    limit: usize,
    row: impl Fn(&RowPin, &T) -> Result<Value, EvaluationError>,
) -> Result<GitPage, EvaluationError> {
    let start = after.map_or(0, |cursor| {
        rows.partition_point(|candidate| name(candidate).as_bytes() <= cursor)
    });
    let remaining = &rows[start..];
    let served = &remaining[..limit.min(remaining.len())];
    let Some(last) = served.last() else {
        return Ok((Vec::new(), None));
    };
    let pin = RowPin::of(context)?;
    let values = served
        .iter()
        .map(|served| row(&pin, served))
        .collect::<Result<Vec<_>, _>>()?;
    let next = (served.len() < remaining.len()).then(|| name(last).as_bytes().to_vec());
    Ok((values, next))
}

/// What the unit tests of the members share.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use orna_sys::{RuntimeMode, StdSnapshotPin, SysHostContext};
    use orna_value::{CanonicalSnapshot, GitHash, GitObjectId, RowRef, SysRef};

    use super::RowPin;
    use crate::Value;

    pub(crate) const DATABASE: [u8; 16] = [7; 16];

    /// A host context attached at `head_commit`.
    pub(crate) fn context(head_commit: Option<Vec<u8>>) -> SysHostContext {
        SysHostContext {
            database_id: DATABASE,
            repository_id: [2; 16],
            runtime_id: [0x11; 16],
            root: PathBuf::from("/repo"),
            read_only: false,
            mode: RuntimeMode::Run,
            repository_layout_version: "3".to_owned(),
            storage_manifest_version: "store-3".to_owned(),
            std_snapshot: StdSnapshotPin::Unpinned,
            head_commit,
        }
    }

    /// The commit pin of a host attached at `[0xab; 20]`.
    pub(crate) fn pin() -> CanonicalSnapshot {
        CanonicalSnapshot::Commit {
            database: DATABASE,
            algorithm: GitHash::Sha1,
            oid: vec![0xab; 20],
        }
    }

    /// A host context attached at the commit [`pin`] names.
    pub(crate) fn pinned() -> SysHostContext {
        context(Some(vec![0xab; 20]))
    }

    /// The [`RowPin`] of [`pinned`].
    pub(crate) fn row_pin() -> RowPin {
        RowPin::of(&pinned()).expect("a pin")
    }

    pub(crate) fn oid(hex: &str) -> GitObjectId {
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hexadecimal"))
            .collect::<Vec<_>>();
        let algorithm = if bytes.len() == 20 {
            GitHash::Sha1
        } else {
            GitHash::Sha256
        };
        GitObjectId::new(algorithm, bytes).expect("an object id of its algorithm's width")
    }

    pub(crate) fn fields(row: &Value) -> &BTreeMap<String, Value> {
        let Value::Record(fields) = row else {
            panic!("a row is a record");
        };
        fields
    }

    pub(crate) fn reference_of(value: &Value) -> SysRef {
        let Value::Reference(canonical) = value else {
            panic!("a reference field holds a reference");
        };
        let bytes = canonical.encode().expect("a canonical reference encodes");
        let row = RowRef::decode(&bytes).expect("a tag-60010 reference");
        SysRef::from_row_ref(row).expect("a checked sys reference")
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{context, pin, pinned, row_pin};
    use super::{RowPin, object_id, page_rows};
    use crate::Value;
    use orna_value::{CanonicalSnapshot, GitHash};

    #[test]
    fn the_pin_is_the_commit_the_host_attached_at() {
        assert_eq!(RowPin::of(&pinned()).expect("a pin").snapshot, pin());
        let sha256 = context(Some(vec![0xcd; 32]));
        assert_eq!(
            RowPin::of(&sha256).expect("a pin").snapshot,
            CanonicalSnapshot::Commit {
                database: super::testing::DATABASE,
                algorithm: GitHash::Sha256,
                oid: vec![0xcd; 32],
            }
        );
        for unreadable in [None, Some(vec![1; 19]), Some(Vec::new())] {
            let failure = RowPin::of(&context(unreadable)).expect_err("no pin");
            assert_eq!(failure.code(), "sys.snapshot.incomplete");
        }
    }

    #[test]
    fn an_object_id_has_the_width_of_its_hash_algorithm() {
        assert_eq!(
            object_id(&"ab".repeat(20)).expect("sha1").as_bytes().len(),
            20
        );
        assert_eq!(
            object_id(&"ab".repeat(32))
                .expect("sha256")
                .as_bytes()
                .len(),
            32
        );
        for bad in [
            "",
            "main",
            &"g".repeat(40),
            &"a".repeat(41),
            &"a".repeat(63),
        ] {
            let failure = object_id(bad).expect_err("not an object id");
            assert_eq!(failure.code(), "sys.snapshot.incomplete", "{bad:?}");
        }
    }

    fn name<'a>(row: &'a &str) -> &'a str {
        row
    }

    fn row(_: &RowPin, name: &&str) -> Result<Value, crate::EvaluationError> {
        Ok(Value::String((*name).to_owned()))
    }

    fn served(rows: &[Value]) -> Vec<&str> {
        rows.iter()
            .map(|row| match row {
                Value::String(text) => text.as_str(),
                other => panic!("a row name is text, not {other:?}"),
            })
            .collect()
    }

    #[test]
    fn pages_resume_after_the_last_row_served() {
        let rows = ["alpha", "beta", "feature/x", "main"];
        let context = pinned();

        let (page, next) = page_rows(&context, &rows, name, None, 2, row).expect("first page");
        assert_eq!(served(&page), ["alpha", "beta"]);
        assert_eq!(next.as_deref(), Some(b"beta".as_slice()));

        let (page, next) =
            page_rows(&context, &rows, name, next.as_deref(), 2, row).expect("second page");
        assert_eq!(served(&page), ["feature/x", "main"]);
        assert_eq!(next, None, "the last page names no continuation");

        let (page, next) = page_rows(&context, &rows, name, None, 4, row).expect("one page");
        assert_eq!(served(&page), ["alpha", "beta", "feature/x", "main"]);
        assert_eq!(next, None);

        // A cursor between two names resumes at the next name, so a row deleted
        // since the previous page does not break the read.
        let (page, _) =
            page_rows(&context, &rows, name, Some(b"bravo"), 8, row).expect("after a gap");
        assert_eq!(served(&page), ["feature/x", "main"]);
        let (page, next) =
            page_rows(&context, &rows, name, Some(b"main"), 8, row).expect("past the end");
        assert!(page.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn an_empty_listing_needs_no_pin() {
        let rows: [&str; 0] = [];
        let (page, next) =
            page_rows(&context(None), &rows, name, None, 8, row).expect("no rows, no pin");
        assert!(page.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn rows_without_a_pin_are_refused_not_fabricated() {
        let failure = page_rows(&context(None), &["main"], name, None, 8, row)
            .expect_err("a row cannot be pinned without a commit");
        assert_eq!(failure.code(), "sys.snapshot.incomplete");
    }

    #[test]
    fn a_reference_is_built_over_the_pin_or_refused() {
        let pin = row_pin();
        let built = pin
            .reference(|database, snapshot| orna_value::tag_reference(database, snapshot, "v1"))
            .expect("a reference");
        assert!(matches!(built, Value::Reference(_)));
        let failure = pin
            .reference(|database, snapshot| orna_value::tag_reference(database, snapshot, ""))
            .expect_err("a reference over an empty name is not one");
        assert_eq!(failure.code(), "sys.snapshot.incomplete");
    }

    // A handle linked for the evaluator with no member here is admitted and then
    // unserved; a member here with no link is never admitted.
    #[test]
    fn the_served_members_are_exactly_the_linked_git_handles() {
        use orna_sys::{SystemSurfaceMember, system_provider_linkage};
        use std::collections::BTreeSet;

        let linked = system_provider_linkage()
            .links()
            .filter_map(|link| match link.member {
                SystemSurfaceMember::GroupedHandle(handle)
                    if handle.name().starts_with("sys.git.") =>
                {
                    Some(handle.name())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let served = super::MEMBERS
            .iter()
            .map(|member| member.handle.name())
            .collect::<BTreeSet<_>>();
        assert_eq!(linked, served);
        assert_eq!(served.len(), super::MEMBERS.len(), "no member twice");
    }

    #[test]
    fn only_the_served_members_are_served() {
        for served in ["sys.git.branches", "sys.git.remotes", "sys.git.tags"] {
            assert_eq!(super::served_source(served), Some(served));
            assert!(
                super::served_page(served, &context(None), None, 8, 256).is_some(),
                "{served}"
            );
        }
        for other in ["sys.git.refs", "sys.catalog.tables", "Music", ""] {
            assert_eq!(super::served_source(other), None, "{other:?}");
            assert!(
                super::served_page(other, &context(None), None, 8, 256).is_none(),
                "{other:?}"
            );
        }
    }
}
