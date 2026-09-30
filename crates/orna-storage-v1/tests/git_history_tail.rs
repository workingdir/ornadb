use std::{fs, path::Path, process::Command};

const OLD_ROW: &str = include_str!("fixtures/git-history-old-row.orna");
const NEW_ROW: &str = include_str!("fixtures/git-history-new-row.orna");

fn git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn adding_a_new_table_row_keeps_older_rows_in_head_and_prior_history() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-b", "main"]);
    git(root.path(), &["config", "user.name", "kierandrewett"]);
    git(root.path(), &["config", "user.email", "kieran@drewett.dev"]);
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    fs::create_dir_all(root.path().join("tables/Reading")).unwrap();
    fs::write(root.path().join("tables/Reading/1.orna"), OLD_ROW).unwrap();
    git(root.path(), &["add", "tables/Reading/1.orna"]);
    git(root.path(), &["commit", "-m", "seed old table row"]);
    let old_snapshot = git(root.path(), &["rev-parse", "HEAD"]);

    fs::write(root.path().join("tables/Reading/2.orna"), NEW_ROW).unwrap();
    git(root.path(), &["add", "tables/Reading/2.orna"]);
    git(root.path(), &["commit", "-m", "append new table row"]);

    let paths = git(root.path(), &["ls-tree", "-r", "--name-only", "HEAD", "--", "tables/Reading"]);
    assert_eq!(paths.lines().collect::<Vec<_>>(), ["tables/Reading/1.orna", "tables/Reading/2.orna"]);
    assert_eq!(git(root.path(), &["show", "HEAD:tables/Reading/1.orna"]), OLD_ROW.trim());
    assert_eq!(git(root.path(), &["show", "HEAD:tables/Reading/2.orna"]), NEW_ROW.trim());
    assert_eq!(
        git(root.path(), &["show", &format!("{old_snapshot}:tables/Reading/1.orna")]),
        OLD_ROW.trim()
    );
    assert!(git(root.path(), &["rev-list", "HEAD"]).lines().any(|commit| commit == old_snapshot));
}
