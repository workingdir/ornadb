//! The REPL serves `sys.git.tags` and `sys.git.remotes` rows (`sys.Tag`,
//! `sys.Remote`) read from the repository the session is bound to, with every
//! reference pinned to the commit the session is bound to, on both of its paths:
//! a table-read-only input over the committed read scope, and an input that also
//! writes through the runtime writer (ORNA-SYS-111, ORNA-SYS-112). No row holds a
//! plaintext credential.

use super::repl_git_harness::{Recorded, Repl, field, present, raw_field, texts};
use crate::format3::git;
use orna_value::OvbRaw;

const SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n\n\
pub fn record_and_count_tags(): Int {\n    \
Music.insert({ id: \"a\", title: \"A\" });\n    \
sys.git.tags | count()\n}\n\n\
pub fn record_and_annotated_tag(): Str? {\n    \
Music.insert({ id: \"b\", title: \"B\" });\n    \
sys.git.tags | filter(t => t.annotated) | map(t => t.name) | first()\n}\n\n\
pub fn record_and_count_remotes(): Int {\n    \
Music.insert({ id: \"c\", title: \"C\" });\n    \
sys.git.remotes | count()\n}\n\n\
pub fn record_and_promisor_remote(): Str? {\n    \
Music.insert({ id: \"d\", title: \"D\" });\n    \
sys.git.remotes | filter(r => r.promisor) | map(r => r.name) | first()\n}\n";

/// Shapes a repository with a lightweight tag `light` and an annotated tag `v1`,
/// both at `HEAD`, and records the commit `HEAD` names in `commit`.
fn tagged(commit: &mut String) -> impl FnOnce(&orna_repository::Repository) + '_ {
    move |repository| {
        let root = repository.worktree();
        *commit = repository.head().unwrap().unwrap().to_string();
        git(root, &["tag", "light"]);
        git(root, &["tag", "-a", "-m", "Release 1\n\nbody", "v1"]);
    }
}

/// A repository with the remotes `fork` and `origin`, the latter a promisor and
/// its URL carrying a credential.
fn with_remotes(repository: &orna_repository::Repository) {
    let root = repository.worktree();
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://account:secret@example.invalid/private.git",
        ],
    );
    git(
        root,
        &["remote", "add", "fork", "git@example.invalid:me/repo.git"],
    );
    git(root, &["config", "remote.origin.promisor", "true"]);
}

fn urls(urls: &[&str]) -> OvbRaw {
    OvbRaw::Array(
        urls.iter()
            .map(|url| OvbRaw::Text((*url).to_owned()))
            .collect(),
    )
}

// Specified: 34-system-reference.md `sys.Tag`; the committed read path.
#[test]
fn a_committed_read_serves_one_row_per_tag_with_its_references() {
    let mut commit = String::new();
    let mut repl = Repl::open(SOURCE, tagged(&mut commit));
    let recorded = Recorded::read(repl.repository());

    assert_eq!(
        texts(&repl.rows("sys.git.tags | map(t => t.name)")),
        ["light", "v1"],
        "one row per tag, in name order"
    );
    let rows = repl.rows("sys.git.tags");
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let OvbRaw::Text(name) = field(row, "name") else {
            panic!("a tag name is text");
        };
        assert_eq!(field(row, "reference"), &recorded.tag(name), "{name}");
        assert_eq!(
            field(row, "object"),
            &recorded.object(&commit),
            "{name} names the commit, not its tag object"
        );
        assert_eq!(
            field(row, "annotated"),
            &OvbRaw::Bool(name == "v1"),
            "{name}"
        );
        if name == "v1" {
            assert_eq!(
                present(field(row, "message")),
                Some(&OvbRaw::Text("Release 1\n\nbody\n".to_owned()))
            );
            let tagger = present(field(row, "tagger")).expect("an annotated tag has a tagger");
            assert_eq!(
                raw_field(tagger, "name"),
                &OvbRaw::Text("kierandrewett".to_owned())
            );
            assert_eq!(
                present(raw_field(tagger, "email")),
                Some(&OvbRaw::Text("kieran@drewett.dev".to_owned()))
            );
        } else {
            assert_eq!(
                present(field(row, "message")),
                None,
                "{name} has no message"
            );
            assert_eq!(present(field(row, "tagger")), None, "{name} has no tagger");
        }
    }
    assert!(
        repl.transactions.writer.is_none(),
        "a table-read-only input takes no writer"
    );
}

// Specified: ORNA-SYS-111; the language reads the absent message as null.
#[test]
fn a_lightweight_tag_reads_as_null_message_in_source() {
    let mut commit = String::new();
    let mut repl = Repl::open(SOURCE, tagged(&mut commit));
    assert_eq!(
        texts(&repl.rows("sys.git.tags | map(t => t.message ?? \"none\")")),
        ["none", "Release 1\n\nbody\n"]
    );
}

// Specified: ORNA-SYS-111; an input that also writes reads the tags through the
// writer, over the same repository and pin.
#[test]
fn an_input_that_writes_reads_the_same_tags_through_the_writer() {
    let mut commit = String::new();
    let mut repl = Repl::open(SOURCE, tagged(&mut commit));

    assert_eq!(
        repl.value("main.record_and_count_tags()"),
        OvbRaw::Int(2.into())
    );
    assert!(
        repl.transactions.writer.is_some(),
        "the input ran through the runtime writer"
    );
    assert_eq!(
        present(&repl.value("main.record_and_annotated_tag()")),
        Some(&OvbRaw::Text("v1".to_owned()))
    );
}

// Specified: 34-system-reference.md `sys.Remote`; the committed read path.
#[test]
fn a_committed_read_serves_one_row_per_remote_with_its_urls_and_no_credential() {
    let mut repl = Repl::open(SOURCE, with_remotes);
    let recorded = Recorded::read(repl.repository());

    assert_eq!(
        texts(&repl.rows("sys.git.remotes | map(r => r.name)")),
        ["fork", "origin"],
        "one row per remote, in name order"
    );
    let rows = repl.rows("sys.git.remotes");
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let OvbRaw::Text(name) = field(row, "name") else {
            panic!("a remote name is text");
        };
        assert_eq!(field(row, "reference"), &recorded.remote(name), "{name}");
        assert_eq!(
            present(field(row, "last_error")),
            None,
            "{name}: no remote failure is retained"
        );
        let (fetch, promisor) = match name.as_str() {
            "origin" => (
                urls(&["https://<redacted>@example.invalid/private.git"]),
                true,
            ),
            "fork" => (urls(&["git@example.invalid:me/repo.git"]), false),
            other => panic!("unexpected remote {other}"),
        };
        assert_eq!(field(row, "fetch_urls"), &fetch, "{name}");
        assert_eq!(
            field(row, "push_urls"),
            &fetch,
            "{name}: pushes go to the urls"
        );
        assert_eq!(field(row, "promisor"), &OvbRaw::Bool(promisor), "{name}");
    }
    let shown = format!("{rows:?}");
    for secret in ["secret", "account"] {
        assert!(!shown.contains(secret), "{secret} reached a row: {shown}");
    }
    assert!(
        repl.transactions.writer.is_none(),
        "a table-read-only input takes no writer"
    );
}

// Specified: ORNA-SYS-111; an input that also writes reads the remotes through
// the writer, over the same repository and pin.
#[test]
fn an_input_that_writes_reads_the_same_remotes_through_the_writer() {
    let mut repl = Repl::open(SOURCE, with_remotes);

    assert_eq!(
        repl.value("main.record_and_count_remotes()"),
        OvbRaw::Int(2.into())
    );
    assert!(
        repl.transactions.writer.is_some(),
        "the input ran through the runtime writer"
    );
    assert_eq!(
        present(&repl.value("main.record_and_promisor_remote()")),
        Some(&OvbRaw::Text("origin".to_owned()))
    );
}
