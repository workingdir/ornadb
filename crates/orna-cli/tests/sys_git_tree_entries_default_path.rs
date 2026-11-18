//! The default path serves the linked `sys.git.tree_entries` member (`sys.TreeEntry`)
//! from the repository of a project, through both `orna run` and `orna repl`: one row
//! per entry of the committed tree, directories included, each with the mode Git
//! records. An unborn `HEAD` serves no rows, and a tree holding a path that is not
//! UTF-8 is refused rather than served with a guessed name (see
//! docs/design/sys-row-references.md, "The tree entry rows"). The expected counts are
//! read from `git ls-tree -r -t HEAD`, so they follow what the committed tree holds
//! rather than a fixed layout.

#[path = "support/git_project.rs"]
mod git_project;

use std::path::Path;

use git_project::{combined, committed_project, git, orna, repl, run, uncommitted_project};

const SOURCE: &str = r"pub table Item(id: Str) {
    name: Str,
}

pub fn entry_count(): Int {
    sys.git.tree_entries | count()
}

pub fn directory_count(): Int {
    sys.git.tree_entries | filter(e => e.mode == 16384) | count()
}

pub fn executable_count(): Int {
    sys.git.tree_entries | filter(e => e.mode == 33261) | count()
}
";

/// The entries of the tree of `HEAD`, as `git ls-tree -r -t` lists them.
fn listed(root: &Path) -> Vec<String> {
    git(root, &["ls-tree", "-r", "-t", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// How many entries of the tree of `HEAD` have the mode `mode`, spelled as `ls-tree`
/// prints it (`040000` for a directory, `100755` for an executable file).
fn listed_with_mode(root: &Path, mode: &str) -> usize {
    listed(root)
        .iter()
        .filter(|line| line.split_whitespace().next() == Some(mode))
        .count()
}

#[test]
// A `sys.TreeEntry` is a Git tree entry (34-system-reference.md). Its rows are the
// entries `git ls-tree -r -t` lists, directories included (docs/design/
// sys-row-references.md, "The tree entry rows").
fn orna_run_counts_the_entries_of_the_committed_tree_including_directories() {
    let project = committed_project(SOURCE);
    let root = project.path();
    std::fs::create_dir(root.join("docs")).expect("a directory");
    std::fs::write(root.join("docs/readme.md"), "readme\n").expect("a file");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "docs"]);

    assert!(
        listed(root).iter().any(|line| line.ends_with("\tdocs")),
        "the committed tree holds the docs directory"
    );
    assert_eq!(
        run(root, "entry_count"),
        format!("{} : Int\n", listed(root).len())
    );
    assert_eq!(
        run(root, "directory_count"),
        format!("{} : Int\n", listed_with_mode(root, "040000"))
    );
}

#[test]
// `mode` is the Int Git records for the entry (34-system-reference.md, `sys.TreeEntry`):
// an executable file is `100755`, which is 33261.
fn orna_run_reports_an_executable_file_with_mode_33261() {
    let project = committed_project(SOURCE);
    let root = project.path();
    std::fs::write(root.join("run.sh"), "#!/bin/sh\n").expect("a script");
    git(root, &["add", "run.sh"]);
    git(root, &["update-index", "--chmod=+x", "run.sh"]);
    git(root, &["commit", "-q", "-m", "script"]);

    assert!(
        listed(root)
            .iter()
            .any(|line| line.starts_with("100755 blob") && line.ends_with("\trun.sh")),
        "git records run.sh as executable"
    );
    assert_eq!(
        run(root, "executable_count"),
        format!("{} : Int\n", listed_with_mode(root, "100755"))
    );
}

#[test]
// The REPL serves the same rows as `orna run`.
fn repl_counts_the_entries_of_the_project() {
    let project = committed_project(SOURCE);
    let root = project.path();
    let expected = format!("{} : Int", listed(root).len());
    let output = repl(root, "sys.git.tree_entries | count()");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(text.contains(&expected), "one row per entry: {text}");
}

#[test]
// An unborn `HEAD` names no commit, so there is no tree to list and no row to pin
// (docs/design/sys-row-references.md, "The tree entry rows").
fn orna_run_of_a_project_with_no_commit_has_no_entries() {
    let project = uncommitted_project(SOURCE);
    assert_eq!(run(project.path(), "entry_count"), "0 : Int\n");
    assert_eq!(run(project.path(), "executable_count"), "0 : Int\n");
}

#[test]
// An unborn `HEAD` serves no rows even when the repository holds commits on another
// branch (docs/design/sys-row-references.md, "The tree entry rows").
fn orna_run_of_an_orphan_head_has_no_entries() {
    let project = committed_project(SOURCE);
    let root = project.path();
    git(root, &["checkout", "-q", "--orphan", "fresh"]);
    assert_eq!(run(root, "entry_count"), "0 : Int\n");
}

#[cfg(unix)]
#[test]
// A path that is not UTF-8 cannot be served as a row with a guessed name, so the read
// is refused rather than served (docs/design/sys-row-references.md, "The tree entry
// rows").
fn orna_run_refuses_a_tree_with_a_path_that_is_not_utf8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let project = committed_project(SOURCE);
    let root = project.path();
    std::fs::write(root.join(OsStr::from_bytes(b"bad\xff.txt")), "x\n").expect("a file");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "non-UTF-8 name"]);

    let output = orna(root, &["run", "main.entry_count"]);
    let text = combined(&output);
    assert!(!output.status.success(), "the tree is served: {text}");
    assert!(!text.contains(": Int"), "no row was served: {text}");
}
