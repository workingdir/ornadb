//! The REPL serves `sys.git.branches` rows (`sys.Branch`) read from the
//! repository the session is bound to, with every reference pinned to the commit
//! the session is bound to, on both of its paths: a table-read-only input over
//! the committed read scope, and an input that also writes through the runtime
//! writer (ORNA-SYS-111, ORNA-SYS-112).

use super::*;
use crate::format3::{format3_repository_for_table, git, git_output};
use orna_evaluator::ReplValue;
use orna_repository::Repository;
use orna_value::{
    CanonicalSnapshot, GitHash, GitObjectId, OvbRaw, SysRef, branch_reference, commit_reference,
};
use tempfile::TempDir;

const SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n\n\
pub fn record_and_count_branches(): Int {\n    \
Music.insert({ id: \"a\", title: \"A\" });\n    \
sys.git.branches | count()\n}\n\n\
pub fn record_and_current_branch(): Str? {\n    \
Music.insert({ id: \"b\", title: \"B\" });\n    \
sys.git.branches | filter(b => b.current) | map(b => b.name) | first()\n}\n";

/// The identity every test commit is written under; no ambient Git identity is
/// assumed.
const IDENTITY: [&str; 4] = [
    "-c",
    "user.name=orna-test",
    "-c",
    "user.email=orna-test@example.invalid",
];

/// Creates the local branch `name` at a commit of its own: a child of `HEAD`
/// with the same tree, so `HEAD` and the checked-out branch do not move.
fn branch_at_new_commit(repository: &Repository, name: &str) -> String {
    let root = repository.worktree();
    let mut arguments = IDENTITY.to_vec();
    arguments.extend(["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", name]);
    let commit = String::from_utf8(git_output(root, &arguments, None))
        .expect("a commit id is text")
        .trim()
        .to_owned();
    git(root, &["branch", name, &commit]);
    commit
}

fn oid_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("a hexadecimal object id"))
        .collect()
}

fn algorithm(bytes: &[u8]) -> GitHash {
    if bytes.len() == 20 {
        GitHash::Sha1
    } else {
        GitHash::Sha256
    }
}

/// What the repository records: the database and the commit every reference is
/// pinned to.
struct Recorded {
    database: [u8; 16],
    pin: CanonicalSnapshot,
}

impl Recorded {
    /// Reads the expectations from the repository, never from the REPL.
    fn read(repository: &Repository) -> Self {
        let head = repository
            .head()
            .expect("HEAD is readable")
            .expect("a committed HEAD");
        let database = *repository
            .open_pinned_format_context(head.as_str())
            .expect("a format context at HEAD")
            .require_database_id()
            .expect("the store has a database identity")
            .as_bytes();
        let oid = oid_bytes(head.as_str());
        Self {
            database,
            pin: CanonicalSnapshot::Commit {
                database,
                algorithm: algorithm(&oid),
                oid,
            },
        }
    }

    fn raw(reference: &SysRef) -> OvbRaw {
        reference
            .to_canonical()
            .expect("a reference encodes")
            .raw()
            .clone()
    }

    /// The `sys.BranchRef` of the branch `name`.
    fn branch(&self, name: &str) -> OvbRaw {
        let reference =
            branch_reference(self.database, self.pin.clone(), name).expect("a branch reference");
        Self::raw(&reference)
    }

    /// The `sys.CommitRef` of the commit `hex`.
    fn commit(&self, hex: &str) -> OvbRaw {
        let bytes = oid_bytes(hex);
        let oid = GitObjectId::new(algorithm(&bytes), bytes).expect("an object id");
        let reference =
            commit_reference(self.database, self.pin.clone(), oid).expect("a commit reference");
        Self::raw(&reference)
    }
}

struct Repl {
    _directory: TempDir,
    repository: Option<Repository>,
    session: AdmittedReplSession,
    transactions: ReplTransactions,
}

impl Repl {
    /// Opens a REPL over a committed project, after `prepare` has shaped the
    /// repository's branches.
    fn open(prepare: impl FnOnce(&Repository)) -> Self {
        let (directory, repository, _relation) = format3_repository_for_table(SOURCE, "Music");
        prepare(&repository);
        let ReplRuntime {
            repository,
            session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        Self {
            _directory: directory,
            repository,
            session,
            transactions: transactions.expect("repl transactions"),
        }
    }

    fn repository(&self) -> &Repository {
        self.repository.as_ref().expect("a bound repository")
    }

    fn submit(&mut self, source: &str) -> ReplValue {
        run_repl_input(
            self.repository.as_ref(),
            Some(&mut self.transactions),
            &mut self.session,
            source,
        )
        .unwrap_or_else(|failure| panic!("{source}: {}", failure.code))
        .value
        .unwrap_or_else(|| panic!("{source} has a value"))
    }

    fn rows(&mut self, source: &str) -> Vec<orna_value::CanonicalValue> {
        let ReplValue::Relation(window) = self.submit(source) else {
            panic!("{source} is a relation");
        };
        window.rows
    }

    fn value(&mut self, source: &str) -> OvbRaw {
        let ReplValue::Value(value) = self.submit(source) else {
            panic!("{source} is one value");
        };
        value.raw().clone()
    }
}

/// The raw field `name` of a `sys.Branch` row.
fn field<'a>(row: &'a orna_value::CanonicalValue, name: &str) -> &'a OvbRaw {
    let OvbRaw::Map(entries) = row.raw() else {
        panic!("a sys.Branch row is a record");
    };
    entries
        .iter()
        .find_map(|(key, value)| matches!(key, OvbRaw::Text(text) if text == name).then_some(value))
        .unwrap_or_else(|| panic!("the row has no `{name}` field"))
}

/// The payload of an optional raw value, or `None` for an absent one.
fn present(option: &OvbRaw) -> Option<&OvbRaw> {
    let OvbRaw::Tag(_, inner) = option else {
        panic!("an optional value is a tagged option");
    };
    let OvbRaw::Array(parts) = inner.as_ref() else {
        panic!("an option holds its presence and payload");
    };
    match parts.as_slice() {
        [absent] if *absent == OvbRaw::Int(0.into()) => None,
        [tag, payload] if *tag == OvbRaw::Int(1.into()) => Some(payload),
        other => panic!("not an option: {other:?}"),
    }
}

fn names(rows: &[orna_value::CanonicalValue]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.raw() {
            OvbRaw::Text(name) => name.clone(),
            other => panic!("a branch name is text, not {other:?}"),
        })
        .collect()
}

// Specified: 34-system-reference.md `sys.Branch`; the committed read path.
#[test]
fn a_committed_read_serves_one_row_per_branch_with_distinct_commit_references() {
    let mut tips = (String::new(), String::new());
    let mut repl = Repl::open(|repository| {
        tips.0 = repository.head().unwrap().unwrap().to_string();
        tips.1 = branch_at_new_commit(repository, "topic");
    });
    let recorded = Recorded::read(repl.repository());
    let current = repl
        .repository()
        .checked_out_branch()
        .expect("HEAD is readable")
        .expect("HEAD is attached to a branch");
    let (current_tip, topic_tip) = tips;
    assert_ne!(current_tip, topic_tip, "the branches name distinct commits");

    let mut expected = [current.clone(), "topic".to_owned()];
    expected.sort();
    assert_eq!(
        names(&repl.rows("sys.git.branches | map(b => b.name)")),
        expected,
        "one row per local branch, in name order"
    );
    assert_eq!(
        names(&repl.rows("sys.git.branches | filter(b => b.current) | map(b => b.name)")),
        std::slice::from_ref(&current),
        "exactly the branch HEAD is attached to is current"
    );

    let rows = repl.rows("sys.git.branches");
    assert_eq!(rows.len(), 2);
    let mut commits = Vec::new();
    for row in &rows {
        let OvbRaw::Text(name) = field(row, "name") else {
            panic!("a branch name is text");
        };
        let tip = if *name == current {
            &current_tip
        } else {
            &topic_tip
        };
        assert_eq!(field(row, "reference"), &recorded.branch(name), "{name}");
        assert_eq!(field(row, "commit"), &recorded.commit(tip), "{name}");
        assert_eq!(
            field(row, "current"),
            &OvbRaw::Bool(*name == current),
            "{name}"
        );
        for absent in ["upstream", "ahead", "behind"] {
            assert_eq!(
                present(field(row, absent)),
                None,
                "{name}: a branch with no upstream has no {absent}"
            );
        }
        commits.push(field(row, "commit").clone());
    }
    assert_ne!(commits[0], commits[1], "distinct commit references");
    assert!(
        repl.transactions.writer.is_none(),
        "a table-read-only input takes no writer"
    );
}

// Specified: ORNA-SYS-111; the language reads the absent counts as null.
#[test]
fn a_branch_with_no_upstream_reads_as_null_in_source() {
    let mut repl = Repl::open(|repository| {
        branch_at_new_commit(repository, "topic");
    });
    let rows = repl.rows("sys.git.branches | map(b => (b.ahead ?? -1) + (b.behind ?? -1))");
    assert_eq!(
        rows.iter().map(|row| row.raw().clone()).collect::<Vec<_>>(),
        [OvbRaw::Int((-2).into()), OvbRaw::Int((-2).into())]
    );
}

// Specified: ORNA-SYS-111; an input that also writes reads the branches
// through the writer, over the same repository and pin.
#[test]
fn an_input_that_writes_reads_the_same_branches_through_the_writer() {
    let mut repl = Repl::open(|repository| {
        branch_at_new_commit(repository, "topic");
    });
    let current = repl
        .repository()
        .checked_out_branch()
        .expect("HEAD is readable")
        .expect("HEAD is attached to a branch");

    assert_eq!(
        repl.value("main.record_and_count_branches()"),
        OvbRaw::Int(2.into())
    );
    assert!(
        repl.transactions.writer.is_some(),
        "the input ran through the runtime writer"
    );
    let OvbRaw::Tag(_, option) = repl.value("main.record_and_current_branch()") else {
        panic!("an optional name is a tagged option");
    };
    let OvbRaw::Array(parts) = option.as_ref() else {
        panic!("an option holds its presence and payload");
    };
    assert_eq!(parts.last(), Some(&OvbRaw::Text(current)));
}
