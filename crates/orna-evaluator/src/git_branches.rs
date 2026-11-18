//! The `sys.git.branches` rows: the `sys.Branch` projection of the local
//! branches of the repository the host attached (ORNA-SYS-111, ORNA-SYS-112).
//!
//! `reference` is the `sys.BranchRef` of the name, `commit` the `sys.CommitRef` of
//! the tip. A branch has no `upstream`, `ahead` or `behind` unless it has an
//! upstream ref to name and count against; with one, all three are present.
//! [`crate::git_rows`] holds what every Git member shares: the repository read,
//! the commit pin and the failure when either is unavailable.

use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_repository::CommittedBranch;
use orna_sys::SysHostContext;
use orna_value::{branch_reference, commit_reference, git_ref_reference};

use crate::git_rows::{GitPage, RowPin, incomplete, object_id, page_rows, repository};
use crate::{EvaluationError, Value};

/// Most local branches one read lists. A repository with more is refused with
/// `sys.snapshot.incomplete` rather than served truncated.
const MAX_BRANCHES: usize = 10_000;

/// Serves the page of `sys.git.branches` that follows the branch named by
/// `after`, from the repository `context` attached.
pub(crate) fn branches_page(
    context: &SysHostContext,
    after: Option<&[u8]>,
    rows: usize,
) -> Result<GitPage, EvaluationError> {
    let mut branches = repository(context)?
        .local_branches(MAX_BRANCHES)
        .map_err(|_| incomplete())?;
    branches.sort_by(|left, right| left.name().cmp(right.name()));
    let facts = branches.iter().map(BranchFacts::of).collect::<Vec<_>>();
    page_rows(
        context,
        &facts,
        |branch| branch.name,
        after,
        rows,
        branch_row,
    )
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

/// The `sys.Branch` row of one branch: `{ reference, name, commit, current,
/// upstream, ahead, behind }` (34-system-reference.md, `sys.Branch`).
fn branch_row(pin: &RowPin, branch: &BranchFacts<'_>) -> Result<Value, EvaluationError> {
    let tip = object_id(branch.commit)?;
    let upstream = branch
        .upstream
        .as_ref()
        .map(|upstream| {
            pin.reference(|database, snapshot| {
                git_ref_reference(database, snapshot, upstream.reference)
            })
            .map(Box::new)
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
            pin.reference(|database, snapshot| branch_reference(database, snapshot, branch.name))?,
        ),
        ("name".to_owned(), Value::String(branch.name.to_owned())),
        (
            "commit".to_owned(),
            pin.reference(|database, snapshot| commit_reference(database, snapshot, tip))?,
        ),
        ("current".to_owned(), Value::Bool(branch.current)),
        ("upstream".to_owned(), Value::Option(upstream)),
        ("ahead".to_owned(), count(|upstream| upstream.ahead)),
        ("behind".to_owned(), count(|upstream| upstream.behind)),
    ])))
}

#[cfg(test)]
mod tests {
    use super::{BranchFacts, UpstreamFacts, branch_row};
    use crate::Value;
    use crate::git_rows::testing::{DATABASE, fields, oid, pin, reference_of, row_pin};
    use orna_value::{branch_reference, commit_reference, git_ref_reference, relations};

    const MAIN_TIP: &str = "00112233445566778899aabbccddeeff00112233";
    const TOPIC_TIP: &str = "ffeeddccbbaa99887766554433221100ffeeddcc";

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

    // Specified: 34-system-reference.md `sys.Branch`; ORNA-SYS-111, ORNA-SYS-112.
    #[test]
    fn a_branch_without_an_upstream_has_no_upstream_or_counts() {
        let row = branch_row(&row_pin(), &main_branch()).expect("a branch row");
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
        let row = branch_row(&row_pin(), &tracking_branch()).expect("a branch row");
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
        let row = branch_row(&row_pin(), &level).expect("a branch row");
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
        let row = branch_row(&row_pin(), &branch).expect("a branch row");
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
            let failure = branch_row(&row_pin(), &branch)
                .expect_err("no row is built over a commit that is not an object id");
            assert_eq!(failure.code(), "sys.snapshot.incomplete", "{commit:?}");
        }
    }
}
