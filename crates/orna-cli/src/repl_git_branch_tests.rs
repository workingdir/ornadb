//! The REPL serves `sys.git.branches` rows (`sys.Branch`) read from the
//! repository the session is bound to, with every reference pinned to the commit
//! the session is bound to, on both of its paths: a table-read-only input over
//! the committed read scope, and an input that also writes through the runtime
//! writer (ORNA-SYS-111, ORNA-SYS-112).

use super::repl_git_harness::{IDENTITY, Recorded, Repl, field, present, texts};
use crate::format3::{git, git_output};
use orna_repository::Repository;
use orna_value::OvbRaw;

const SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n\n\
pub fn record_and_count_branches(): Int {\n    \
Music.insert({ id: \"a\", title: \"A\" });\n    \
sys.git.branches | count()\n}\n\n\
pub fn record_and_current_branch(): Str? {\n    \
Music.insert({ id: \"b\", title: \"B\" });\n    \
sys.git.branches | filter(b => b.current) | map(b => b.name) | first()\n}\n";

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

// Specified: 34-system-reference.md `sys.Branch`; the committed read path.
#[test]
fn a_committed_read_serves_one_row_per_branch_with_distinct_commit_references() {
    let mut tips = (String::new(), String::new());
    let mut repl = Repl::open(SOURCE, |repository| {
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
        texts(&repl.rows("sys.git.branches | map(b => b.name)")),
        expected,
        "one row per local branch, in name order"
    );
    assert_eq!(
        texts(&repl.rows("sys.git.branches | filter(b => b.current) | map(b => b.name)")),
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
    let mut repl = Repl::open(SOURCE, |repository| {
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
    let mut repl = Repl::open(SOURCE, |repository| {
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
