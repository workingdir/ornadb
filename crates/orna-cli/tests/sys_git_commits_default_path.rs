//! The default path serves the linked `sys.git.commits` member (`sys.Commit`)
//! from the repository of a project, through both `orna run` and `orna repl`:
//! one row per commit reachable from `HEAD`, newest first, each with its
//! `parents` relation served as the parents' own rows (ORNA-SYS-111,
//! ORNA-SYS-112). A repository with no commit serves no row and needs no pin.
//! The expectations are read from Git.

#[path = "support/git_project.rs"]
mod git_project;

use std::path::Path;

use git_project::{combined, committed_project, git, repl, run, uncommitted_project};

const SOURCE: &str = r#"pub table Item(id: Str) {
    name: Str,
}

pub fn commit_count(): Int {
    sys.git.commits | count()
}

pub fn parent_links(): Int {
    sys.git.commits | flat_map(c => c.parents | map(p => 1)) | count()
}

pub fn newest_message(): Str {
    sys.git.commits | map(c => c.message) | first() ?? "none"
}
"#;

/// Adds one commit with `message` to the current branch of `root`. The commit
/// has the empty tree, so the tracked source is unchanged.
fn commit_again(root: &Path, message: &str) {
    git(root, &["commit", "--allow-empty", "-m", message]);
}

#[test]
// Specified: 34-system-reference.md `sys.Commit`; ORNA-SYS-111. One row per commit
// reachable from `HEAD`: a new commit adds one row.
fn orna_run_counts_the_commits_reachable_from_head() {
    let project = committed_project(SOURCE);
    let root = project.path();
    assert_eq!(run(root, "commit_count"), "1 : Int\n");
    commit_again(root, "second");
    assert_eq!(run(root, "commit_count"), "2 : Int\n");
}

#[test]
// Specified: 34-system-reference.md `sys.Commit`, `parents`; ORNA-SYS-112. The root
// commit serves no parent row; the commit after it serves its one parent's row.
fn orna_run_serves_each_parent_of_a_commit_as_a_row() {
    let project = committed_project(SOURCE);
    let root = project.path();
    assert_eq!(run(root, "parent_links"), "0 : Int\n");
    commit_again(root, "second");
    assert_eq!(run(root, "parent_links"), "1 : Int\n");
}

#[test]
// Specified: 34-system-reference.md `sys.Commit`; rows are newest first, so the first
// row is the commit `HEAD` names.
fn orna_run_lists_the_commit_head_names_first() {
    let project = committed_project(SOURCE);
    let root = project.path();
    commit_again(root, "second");
    assert_eq!(run(root, "newest_message"), "\"second\\n\" : Str\n");
}

#[test]
fn repl_reads_the_commits_of_the_project() {
    let project = committed_project(SOURCE);
    commit_again(project.path(), "second");
    let output = repl(project.path(), "sys.git.commits | map(c => c.message)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("\"second") && text.contains("publish project"),
        "one row per commit, each with its message: {text}"
    );
}

#[test]
// Specified: ORNA-SYS-111. A repository with no commit has no commit row, and an
// empty relation needs no pin.
fn orna_run_of_a_project_with_no_commit_has_no_commits() {
    let project = uncommitted_project(SOURCE);
    assert_eq!(run(project.path(), "commit_count"), "0 : Int\n");
    assert_eq!(run(project.path(), "parent_links"), "0 : Int\n");
}
