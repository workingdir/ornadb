//! The REPL serves `sys.git.refs` and `sys.git.stashes` rows (`sys.Ref`,
//! `sys.Stash`) read from the repository the session is bound to, with every
//! reference pinned to the commit the session is bound to, on both of its paths:
//! a table-read-only input over the committed read scope, and an input that also
//! writes through the runtime writer (ORNA-SYS-050, ORNA-SYS-111,
//! ORNA-SYS-112). The expectations are read from Git, never from the REPL.

use std::path::Path;

use super::repl_git_harness::{Recorded, Repl, field, present, texts};
use crate::format3::{git, git_output};
use orna_repository::Repository;
use orna_value::OvbRaw;

const SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n\n\
pub fn record_and_count_refs(): Int {\n    \
Music.insert({ id: \"a\", title: \"A\" });\n    \
sys.git.refs | count()\n}\n\n\
pub fn record_and_hidden_ref(): Str? {\n    \
Music.insert({ id: \"b\", title: \"B\" });\n    \
sys.git.refs | filter(r => r.hidden) | map(r => r.name) | first()\n}\n\n\
pub fn record_and_count_stashes(): Int {\n    \
Music.insert({ id: \"c\", title: \"C\" });\n    \
sys.git.stashes | count()\n}\n\n\
pub fn record_and_newest_stash(): Str? {\n    \
Music.insert({ id: \"d\", title: \"D\" });\n    \
sys.git.stashes | filter(s => s.index == 0) | map(s => s.message) | first()\n}\n";

/// What Git itself prints for one command run in `root`, trimmed.
fn git_text(root: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_output(root, arguments, None))
        .expect("Git prints UTF-8")
        .trim()
        .to_owned()
}

/// Shapes a repository with a branch, a lightweight and an annotated tag, a
/// configured remote with a symbolic remote-tracking `HEAD`, a remote-tracking
/// ref of a remote that is not configured, and a hidden Orna ref.
fn with_refs(repository: &Repository) {
    let root = repository.worktree();
    let head = git_text(root, &["rev-parse", "HEAD"]);
    git(root, &["branch", "topic"]);
    git(root, &["tag", "light"]);
    git(root, &["tag", "-a", "-m", "Release 1", "v1"]);
    git(
        root,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(root, &["update-ref", "refs/remotes/gone/main", &head]);
    git(
        root,
        &[
            "update-ref",
            "refs/orna/ids/00112233445566778899aabbccddeeff",
            &head,
        ],
    );
}

/// What Git lists for one ref: its full name, the object it names and the ref a
/// symbolic ref points at.
struct ListedRef {
    name: String,
    object: String,
    symbolic: Option<String>,
}

fn listed_refs(root: &Path) -> Vec<ListedRef> {
    git_text(
        root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname) %(symref)",
        ],
    )
    .lines()
    .map(|line| {
        let mut parts = line.splitn(3, ' ');
        let name = parts.next().expect("a name").to_owned();
        let object = parts.next().expect("an object").to_owned();
        let symbolic = parts
            .next()
            .filter(|symref| !symref.is_empty())
            .map(str::to_owned);
        ListedRef {
            name,
            object,
            symbolic,
        }
    })
    .collect()
}

/// Appends a line to the tracked `main.orna` and stashes the change as
/// `message`.
fn stash(root: &Path, round: usize, message: &str) {
    let source = std::fs::read_to_string(root.join("main.orna")).expect("the project source");
    std::fs::write(
        root.join("main.orna"),
        format!("{source}// stash {round}\n"),
    )
    .expect("a change to stash");
    git(root, &["stash", "push", "-m", message]);
}

fn with_stashes(repository: &Repository) {
    let root = repository.worktree();
    stash(root, 1, "first stash");
    stash(root, 2, "second stash");
}

fn instant(unix_seconds: &str) -> OvbRaw {
    OvbRaw::Tag(
        60002,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Int(unix_seconds.parse::<i64>().expect("a Unix time").into()),
            OvbRaw::Int(0.into()),
        ])),
    )
}

// Specified: 34-system-reference.md `sys.Ref`; the committed read path.
#[test]
fn a_committed_read_serves_one_row_per_ref_with_its_references() {
    let mut repl = Repl::open(SOURCE, with_refs);
    let recorded = Recorded::read(repl.repository());
    let expected = listed_refs(repl.repository().worktree());

    let rows = repl.rows("sys.git.refs");
    assert_eq!(rows.len(), expected.len(), "one row per ref");
    assert_eq!(
        texts(&repl.rows("sys.git.refs | map(r => r.name)")),
        expected
            .iter()
            .map(|listed| listed.name.clone())
            .collect::<Vec<_>>(),
        "in name order, each named in full"
    );
    for (row, listed) in rows.iter().zip(&expected) {
        let name = &listed.name;
        assert_eq!(field(row, "name"), &OvbRaw::Text(name.clone()), "{name}");
        assert_eq!(field(row, "reference"), &recorded.git_ref(name), "{name}");
        assert_eq!(
            field(row, "target"),
            &recorded.object(&listed.object),
            "{name}"
        );
        assert_eq!(
            present(field(row, "symbolic_target")),
            listed.symbolic.clone().map(OvbRaw::Text).as_ref(),
            "{name}"
        );
        assert_eq!(
            field(row, "hidden"),
            &OvbRaw::Bool(name.starts_with("refs/orna/")),
            "{name}"
        );
        let remote = if name.starts_with("refs/remotes/origin/") {
            Some(recorded.remote("origin"))
        } else {
            None
        };
        assert_eq!(present(field(row, "remote")), remote.as_ref(), "{name}");
    }
    let names = expected
        .iter()
        .map(|listed| listed.name.as_str())
        .collect::<Vec<_>>();
    for needed in [
        "refs/tags/light",
        "refs/tags/v1",
        "refs/remotes/origin/HEAD",
        "refs/remotes/gone/main",
        "refs/orna/ids/00112233445566778899aabbccddeeff",
    ] {
        assert!(names.contains(&needed), "the fixture lists {needed}");
    }
    assert!(
        repl.transactions.writer.is_none(),
        "a table-read-only input takes no writer"
    );
}

// Specified: ORNA-SYS-111; an input that also writes reads the refs through the
// writer, over the same repository and pin.
#[test]
fn an_input_that_writes_reads_the_same_refs_through_the_writer() {
    let mut repl = Repl::open(SOURCE, with_refs);
    let expected = listed_refs(repl.repository().worktree());

    assert_eq!(
        repl.value("main.record_and_count_refs()"),
        OvbRaw::Int(expected.len().into())
    );
    assert!(
        repl.transactions.writer.is_some(),
        "the input ran through the runtime writer"
    );
    assert_eq!(
        present(&repl.value("main.record_and_hidden_ref()")),
        Some(&OvbRaw::Text(
            "refs/orna/ids/00112233445566778899aabbccddeeff".to_owned()
        ))
    );
}

// Specified: 34-system-reference.md `sys.Stash`; the committed read path.
#[test]
fn a_committed_read_serves_one_row_per_stash_entry_newest_first() {
    let mut repl = Repl::open(SOURCE, with_stashes);
    let recorded = Recorded::read(repl.repository());
    let root = repl.repository().worktree().to_path_buf();
    let branch = git_text(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);

    assert_eq!(
        texts(&repl.rows("sys.git.stashes | map(s => s.message)")),
        [
            format!("On {branch}: second stash"),
            format!("On {branch}: first stash")
        ],
        "newest first"
    );
    let rows = repl.rows("sys.git.stashes");
    assert_eq!(rows.len(), 2);
    for (position, row) in rows.iter().enumerate() {
        let selector = format!("stash@{{{position}}}");
        assert_eq!(
            field(row, "index"),
            &OvbRaw::Int(position.into()),
            "{selector}"
        );
        assert_eq!(
            field(row, "reference"),
            &recorded.stash(position as u64),
            "{selector}"
        );
        assert_eq!(
            field(row, "commit"),
            &recorded.commit(&git_text(&root, &["rev-parse", &selector])),
            "{selector}"
        );
        assert_eq!(
            present(field(row, "created")),
            Some(&instant(&git_text(
                &root,
                &["log", "-1", "--format=%ct", &selector]
            ))),
            "{selector}"
        );
    }
    assert!(
        repl.transactions.writer.is_none(),
        "a table-read-only input takes no writer"
    );
}

// Specified: ORNA-SYS-111; an input that also writes reads the stash entries
// through the writer, over the same repository and pin.
#[test]
fn an_input_that_writes_reads_the_same_stash_entries_through_the_writer() {
    let mut repl = Repl::open(SOURCE, with_stashes);
    let branch = git_text(
        repl.repository().worktree(),
        &["rev-parse", "--abbrev-ref", "HEAD"],
    );

    assert_eq!(
        repl.value("main.record_and_count_stashes()"),
        OvbRaw::Int(2.into())
    );
    assert!(
        repl.transactions.writer.is_some(),
        "the input ran through the runtime writer"
    );
    assert_eq!(
        present(&repl.value("main.record_and_newest_stash()")),
        Some(&OvbRaw::Text(format!("On {branch}: second stash")))
    );
}

// Specified: 34-system-reference.md `sys.Stash`; a project with no stash has an
// empty relation, which needs no pin.
#[test]
fn a_repository_with_no_stash_serves_an_empty_relation() {
    let mut repl = Repl::open(SOURCE, |_| {});
    assert!(repl.rows("sys.git.stashes").is_empty());
}
