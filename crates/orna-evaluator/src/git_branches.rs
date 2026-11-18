//! The `sys.git.branches` rows: the `sys.Branch` projection of the local
//! branches of the repository the host attached (ORNA-SYS-111, ORNA-SYS-112).
//!
//! The evaluator reads the repository named by `SysHostContext::root` itself, so
//! every path that attaches a host context (`orna run`, the REPL's committed
//! reads and its writer, `orna serve`) serves the same rows. Each reference is
//! pinned to the commit the host attached at (`SysHostContext::head_commit`),
//! the pin the `sys.catalog.*` rows carry.
//!
//! Nothing is fabricated. A missing pin, a repository that cannot be read and a
//! branch whose commit id is not a Git object id each fail the read with
//! `sys.snapshot.incomplete` (required snapshot objects are unavailable). A
//! branch has no `upstream`, `ahead` or `behind` unless it has an upstream ref
//! to name and count against; with one, all three are present.
//!
//! Rows are ordered by branch name, and the cursor of a page is the name of the
//! last branch it served, so a branch created or deleted between two pages of
//! one read never repeats or skips a branch that persists.

use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_repository::{CommittedBranch, GitHashAlgorithm, NativeOid, Repository};
use orna_sys::{SysFailureCode, SysHostContext};
use orna_value::{
    CanonicalSnapshot, GitHash, GitObjectId, SysRef, SystemReferenceError, branch_reference,
    commit_reference, git_ref_reference,
};

use crate::{EvaluationError, Value, error};

/// Most local branches one read lists. A repository with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_BRANCHES: usize = 10_000;

/// Rows one page serves, unless the activation's collection limit is lower.
///
/// The evaluator asks storage-backed relations for pages of one row so a scan
/// stays demand-driven. Every page here is one observation of the repository (a
/// few Git processes, however many branches it lists), so a page is as large as
/// the limit allows and a read of a typical repository is one observation.
const PAGE_ROWS: usize = 256;

/// One page of rows and the cursor of the next page, when there is one.
pub(crate) type BranchPage = (Vec<Value>, Option<Vec<u8>>);

fn incomplete() -> EvaluationError {
    error(SysFailureCode::SnapshotIncomplete.as_str())
}

/// Serves the page of `sys.git.branches` that follows the branch named by
/// `after`, from the repository `context` attached. The page holds at least
/// `limit` rows when that many remain, and at most `max_items` unless `limit`
/// asks for more.
pub(crate) fn branches_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    limit: usize,
    max_items: usize,
) -> Result<BranchPage, EvaluationError> {
    let repository = Repository::discover(&context.root).map_err(|_| incomplete())?;
    let mut branches = repository
        .local_branches(MAX_BRANCHES)
        .map_err(|_| incomplete())?;
    branches.sort_by(|left, right| left.name().cmp(right.name()));
    let facts = branches.iter().map(BranchFacts::of).collect::<Vec<_>>();
    page_rows(context, &facts, after, limit.max(PAGE_ROWS.min(max_items)))
}

/// What one `sys.Branch` row is built from.
struct BranchFacts<'a> {
    name: &'a str,
    /// The commit the branch ref names, in Git's hexadecimal spelling.
    commit: &'a str,
    current: bool,
    upstream: Option<UpstreamFacts<'a>>,
}

/// The upstream a branch tracks and how the two have diverged.
struct UpstreamFacts<'a> {
    /// The full name of the upstream ref.
    reference: &'a str,
    ahead: u64,
    behind: u64,
}

impl<'a> BranchFacts<'a> {
    fn of(branch: &'a CommittedBranch) -> Self {
        Self {
            name: branch.name(),
            commit: branch.commit().as_str(),
            current: branch.is_current(),
            upstream: branch.upstream().map(|upstream| UpstreamFacts {
                reference: upstream.reference(),
                ahead: upstream.ahead(),
                behind: upstream.behind(),
            }),
        }
    }
}

/// Builds the page of `branches` (in name order) after the one named by
/// `after`. The pin is read only when the page holds a row to pin.
fn page_rows(
    context: &SysHostContext,
    branches: &[BranchFacts<'_>],
    after: Option<&[u8]>,
    limit: usize,
) -> Result<BranchPage, EvaluationError> {
    let start = after.map_or(0, |cursor| {
        branches.partition_point(|branch| branch.name.as_bytes() <= cursor)
    });
    let remaining = &branches[start..];
    let served = &remaining[..limit.min(remaining.len())];
    let Some(last) = served.last() else {
        return Ok((Vec::new(), None));
    };
    let pin = head_pin(context)?;
    let rows = served
        .iter()
        .map(|branch| branch_row(context.database_id, &pin, branch))
        .collect::<Result<Vec<_>, _>>()?;
    let next = (served.len() < remaining.len()).then(|| last.name.as_bytes().to_vec());
    Ok((rows, next))
}

/// The commit pin every reference of a read carries.
fn head_pin(context: &SysHostContext) -> Result<CanonicalSnapshot, EvaluationError> {
    let oid = context.head_commit.as_deref().ok_or_else(incomplete)?;
    let algorithm = match oid.len() {
        20 => GitHash::Sha1,
        32 => GitHash::Sha256,
        _ => return Err(incomplete()),
    };
    Ok(CanonicalSnapshot::Commit {
        database: context.database_id,
        algorithm,
        oid: oid.to_vec(),
    })
}

/// The `sys.Branch` row of one branch: `{ reference, name, commit, current,
/// upstream, ahead, behind }` (34-system-reference.md, `sys.Branch`).
fn branch_row(
    database: [u8; 16],
    pin: &CanonicalSnapshot,
    branch: &BranchFacts<'_>,
) -> Result<Value, EvaluationError> {
    let reference = |built: Result<SysRef, SystemReferenceError>| {
        built
            .ok()
            .and_then(|reference| reference.to_canonical().ok())
            .map(Value::Reference)
            .ok_or_else(incomplete)
    };
    let tip = commit_object_id(branch.commit)?;
    let upstream = branch
        .upstream
        .as_ref()
        .map(|upstream| {
            reference(git_ref_reference(database, pin.clone(), upstream.reference)).map(Box::new)
        })
        .transpose()?;
    let count = |side: fn(&UpstreamFacts<'_>) -> u64| {
        Value::Option(
            branch
                .upstream
                .as_ref()
                .map(|upstream| Box::new(Value::Int(BigInt::from(side(upstream))))),
        )
    };
    Ok(Value::Record(BTreeMap::from([
        (
            "reference".to_owned(),
            reference(branch_reference(database, pin.clone(), branch.name))?,
        ),
        ("name".to_owned(), Value::String(branch.name.to_owned())),
        (
            "commit".to_owned(),
            reference(commit_reference(database, pin.clone(), tip))?,
        ),
        ("current".to_owned(), Value::Bool(branch.current)),
        ("upstream".to_owned(), Value::Option(upstream)),
        ("ahead".to_owned(), count(|upstream| upstream.ahead)),
        ("behind".to_owned(), count(|upstream| upstream.behind)),
    ])))
}

/// The object id a branch's commit names, in the width its hash algorithm has.
fn commit_object_id(hex: &str) -> Result<GitObjectId, EvaluationError> {
    let (hash, algorithm) = match hex.len() {
        40 => (GitHash::Sha1, GitHashAlgorithm::Sha1),
        64 => (GitHash::Sha256, GitHashAlgorithm::Sha256),
        _ => return Err(incomplete()),
    };
    let oid = NativeOid::from_hex(algorithm, hex).map_err(|_| incomplete())?;
    GitObjectId::new(hash, oid.as_bytes().to_vec()).map_err(|_| incomplete())
}

#[cfg(test)]
mod tests {
    use super::{BranchFacts, UpstreamFacts, branch_row, head_pin, page_rows};
    use crate::{EvaluationError, Value};
    use orna_sys::{RuntimeMode, StdSnapshotPin, SysHostContext};
    use orna_value::{
        CanonicalSnapshot, GitHash, GitObjectId, RowRef, SysRef, branch_reference,
        commit_reference, git_ref_reference, relations,
    };
    use std::path::PathBuf;

    const DATABASE: [u8; 16] = [7; 16];
    const MAIN_TIP: &str = "00112233445566778899aabbccddeeff00112233";
    const TOPIC_TIP: &str = "ffeeddccbbaa99887766554433221100ffeeddcc";

    fn context(head_commit: Option<Vec<u8>>) -> SysHostContext {
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

    fn pin() -> CanonicalSnapshot {
        CanonicalSnapshot::Commit {
            database: DATABASE,
            algorithm: GitHash::Sha1,
            oid: vec![0xab; 20],
        }
    }

    fn pinned() -> SysHostContext {
        context(Some(vec![0xab; 20]))
    }

    fn oid(hex: &str) -> GitObjectId {
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

    fn main_branch() -> BranchFacts<'static> {
        BranchFacts {
            name: "main",
            commit: MAIN_TIP,
            current: true,
            upstream: None,
        }
    }

    fn tracking_branch() -> BranchFacts<'static> {
        BranchFacts {
            name: "topic",
            commit: TOPIC_TIP,
            current: false,
            upstream: Some(UpstreamFacts {
                reference: "refs/remotes/origin/topic",
                ahead: 3,
                behind: 2,
            }),
        }
    }

    fn fields(row: &Value) -> &std::collections::BTreeMap<String, Value> {
        let Value::Record(fields) = row else {
            panic!("a sys.Branch row is a record");
        };
        fields
    }

    fn reference_of(value: &Value) -> SysRef {
        let Value::Reference(canonical) = value else {
            panic!("a reference field holds a reference");
        };
        let bytes = canonical.encode().expect("a canonical reference encodes");
        let row = RowRef::decode(&bytes).expect("a tag-60010 reference");
        SysRef::from_row_ref(row).expect("a checked sys reference")
    }

    // Specified: 34-system-reference.md `sys.Branch`; ORNA-SYS-111, ORNA-SYS-112.
    #[test]
    fn a_branch_without_an_upstream_has_no_upstream_or_counts() {
        let row = branch_row(DATABASE, &pin(), &main_branch()).expect("a branch row");
        let fields = fields(&row);
        assert_eq!(
            fields.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "ahead",
                "behind",
                "commit",
                "current",
                "name",
                "reference",
                "upstream"
            ],
            "exactly the declared fields"
        );
        assert_eq!(fields["name"], Value::String("main".to_owned()));
        assert_eq!(fields["current"], Value::Bool(true));
        assert_eq!(fields["upstream"], Value::Option(None));
        assert_eq!(fields["ahead"], Value::Option(None));
        assert_eq!(fields["behind"], Value::Option(None));

        let branch = reference_of(&fields["reference"]);
        assert!(branch.is_of(&relations::BRANCH), "a sys.BranchRef");
        assert_eq!(branch.key_text(), Some("main"));
        assert_eq!(
            branch.as_row_ref().to_canonical().expect("encodes"),
            branch_reference(DATABASE, pin(), "main")
                .expect("branch reference")
                .to_canonical()
                .expect("encodes")
        );
        let commit = reference_of(&fields["commit"]);
        assert!(commit.is_of(&relations::COMMIT), "a sys.CommitRef");
        assert_eq!(commit.key_oid(), Some(oid(MAIN_TIP)));
        assert_eq!(
            commit.as_row_ref().to_canonical().expect("encodes"),
            commit_reference(DATABASE, pin(), oid(MAIN_TIP))
                .expect("commit reference")
                .to_canonical()
                .expect("encodes")
        );
    }

    #[test]
    fn a_branch_with_an_upstream_has_the_upstream_and_both_counts() {
        let row = branch_row(DATABASE, &pin(), &tracking_branch()).expect("a branch row");
        let fields = fields(&row);
        assert_eq!(fields["current"], Value::Bool(false));
        assert_eq!(
            fields["ahead"],
            Value::Option(Some(Box::new(Value::Int(3.into()))))
        );
        assert_eq!(
            fields["behind"],
            Value::Option(Some(Box::new(Value::Int(2.into()))))
        );
        let Value::Option(Some(upstream)) = &fields["upstream"] else {
            panic!("a branch with an upstream names it");
        };
        let upstream = reference_of(upstream);
        assert!(upstream.is_of(&relations::REF), "a sys.RefRef");
        assert_eq!(upstream.key_text(), Some("refs/remotes/origin/topic"));
        assert_eq!(
            upstream.as_row_ref().to_canonical().expect("encodes"),
            git_ref_reference(DATABASE, pin(), "refs/remotes/origin/topic")
                .expect("ref reference")
                .to_canonical()
                .expect("encodes")
        );
    }

    #[test]
    fn a_branch_level_with_its_upstream_has_zero_counts_not_null() {
        let level = BranchFacts {
            upstream: Some(UpstreamFacts {
                reference: "refs/remotes/origin/main",
                ahead: 0,
                behind: 0,
            }),
            ..main_branch()
        };
        let row = branch_row(DATABASE, &pin(), &level).expect("a branch row");
        let fields = fields(&row);
        assert_eq!(
            fields["ahead"],
            Value::Option(Some(Box::new(Value::Int(0.into()))))
        );
        assert_eq!(
            fields["behind"],
            Value::Option(Some(Box::new(Value::Int(0.into()))))
        );
    }

    #[test]
    fn a_sha256_commit_is_a_sha256_commit_reference() {
        let tip = "a".repeat(64);
        let branch = BranchFacts {
            commit: &tip,
            ..main_branch()
        };
        let row = branch_row(DATABASE, &pin(), &branch).expect("a branch row");
        assert_eq!(
            reference_of(&fields(&row)["commit"]).key_oid(),
            Some(oid(&tip))
        );
    }

    #[test]
    fn a_commit_that_is_not_an_object_id_fails_closed() {
        for commit in ["", "main", &"g".repeat(40), &"a".repeat(41)] {
            let branch = BranchFacts {
                commit,
                ..main_branch()
            };
            let failure = branch_row(DATABASE, &pin(), &branch)
                .expect_err("no row is built over a commit that is not an object id");
            assert_eq!(failure.code(), "sys.snapshot.incomplete", "{commit:?}");
        }
    }

    #[test]
    fn the_pin_is_the_commit_the_host_attached_at() {
        assert_eq!(head_pin(&pinned()).expect("a pin"), pin());
        let sha256 = context(Some(vec![0xcd; 32]));
        assert_eq!(
            head_pin(&sha256).expect("a pin"),
            CanonicalSnapshot::Commit {
                database: DATABASE,
                algorithm: GitHash::Sha256,
                oid: vec![0xcd; 32],
            }
        );
        for unreadable in [None, Some(vec![1; 19]), Some(Vec::new())] {
            let failure = head_pin(&context(unreadable)).expect_err("no pin");
            assert_eq!(failure.code(), "sys.snapshot.incomplete");
        }
    }

    fn names(rows: &[Value]) -> Vec<String> {
        rows.iter()
            .map(|row| match &fields(row)["name"] {
                Value::String(name) => name.clone(),
                other => panic!("a branch name is text, not {other:?}"),
            })
            .collect()
    }

    #[test]
    fn pages_resume_after_the_last_branch_served() {
        let branches = ["alpha", "beta", "feature/x", "main"].map(|name| BranchFacts {
            name,
            ..main_branch()
        });
        let context = pinned();

        let (rows, next) = page_rows(&context, &branches, None, 2).expect("first page");
        assert_eq!(names(&rows), ["alpha", "beta"]);
        assert_eq!(next.as_deref(), Some(b"beta".as_slice()));

        let (rows, next) = page_rows(&context, &branches, next.as_deref(), 2).expect("second page");
        assert_eq!(names(&rows), ["feature/x", "main"]);
        assert_eq!(next, None, "the last page names no continuation");

        let (rows, next) = page_rows(&context, &branches, None, 4).expect("one page");
        assert_eq!(names(&rows), ["alpha", "beta", "feature/x", "main"]);
        assert_eq!(next, None);

        // A cursor between two names resumes at the next name, so a branch
        // deleted since the previous page does not break the read.
        let (rows, _) = page_rows(&context, &branches, Some(b"bravo"), 8).expect("after a gap");
        assert_eq!(names(&rows), ["feature/x", "main"]);
        let (rows, next) = page_rows(&context, &branches, Some(b"main"), 8).expect("past the end");
        assert!(rows.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn an_empty_listing_needs_no_pin() {
        let (rows, next) = page_rows(&context(None), &[], None, 8).expect("no branches, no rows");
        assert!(rows.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn rows_without_a_pin_are_refused_not_fabricated() {
        let failure: EvaluationError = page_rows(&context(None), &[main_branch()], None, 8)
            .expect_err("a row cannot be pinned without a commit");
        assert_eq!(failure.code(), "sys.snapshot.incomplete");
    }
}
