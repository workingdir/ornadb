//! Bounded evidence for editable-row Git behavior. Git text merge is only a
//! lower-level witness; these checks do not claim compact semantic merge.

use orna_syntax_v1::parse_row;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

const BASE_ROW: &str = include_str!("fixtures/traceability-row-base.orna");
const UPDATED_ROW: &str = include_str!("fixtures/traceability-row-updated.orna");
const SECOND_ROW: &str = include_str!("fixtures/traceability-row-second.orna");
const MERGE_NAME: &str = include_str!("fixtures/traceability-row-merge-name.orna");
const MERGE_CITY: &str = include_str!("fixtures/traceability-row-merge-city.orna");
const MERGED_ROW: &str = include_str!("fixtures/traceability-row-merged.orna");
const CONFLICT_NAME: &str = include_str!("fixtures/traceability-row-conflict-name.orna");

fn repository() -> TempDir {
    let temp = tempfile::tempdir().expect("temporary Git repository");
    git(temp.path(), &["init", "--quiet"]);
    git(temp.path(), &["config", "user.name", "Traceability Test"]);
    git(temp.path(), &["config", "user.email", "traceability@example.invalid"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    temp
}

fn git(path: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run Git")
}

fn succeed(path: &Path, args: &[&str]) -> std::process::Output {
    let output = git(path, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn write_row(path: &Path, relative: &str, row: &str) {
    let target = path.join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, row).unwrap();
}

fn commit(path: &Path, message: &str) {
    succeed(path, &["add", "--all"]);
    succeed(path, &["commit", "--quiet", "-m", message]);
}

// ORNA-STORAGE-004 (source/23-storage.md:15): editable row edits stay visible
// through ordinary Git diffs.
#[test]
fn direct_editable_row_change_is_reviewable_in_git_diff() {
    assert!(parse_row(BASE_ROW).diagnostics.is_empty());
    assert!(parse_row(UPDATED_ROW).diagnostics.is_empty());
    let temp = repository();
    let path = temp.path();
    let row_path = "crm/Contact/1.orna";
    write_row(path, row_path, BASE_ROW);
    commit(path, "add contact");
    write_row(path, row_path, UPDATED_ROW);

    let diff = succeed(path, &["diff", "--", row_path]);
    let diff = String::from_utf8(diff.stdout).unwrap();
    assert!(diff.contains("-  name: \"Ada\""));
    assert!(diff.contains("+  name: \"Grace\""));
}

// ORNA-GIT-009 (source/24-git-history.md:56): later commits do not silently
// remove older logical rows from HEAD.
#[test]
fn committing_a_new_row_retains_prior_rows_in_head() {
    for row in [BASE_ROW, SECOND_ROW] {
        assert!(parse_row(row).diagnostics.is_empty());
    }
    let temp = repository();
    let path = temp.path();
    write_row(path, "crm/Contact/1.orna", BASE_ROW);
    commit(path, "add first contact");
    write_row(path, "crm/Contact/2.orna", SECOND_ROW);
    commit(path, "add second contact");

    for (row_path, expected) in [
        ("crm/Contact/1.orna", BASE_ROW),
        ("crm/Contact/2.orna", SECOND_ROW),
    ] {
        let output = succeed(path, &["show", &format!("HEAD:{row_path}")]);
        let actual = String::from_utf8(output.stdout).unwrap();
        assert_eq!(actual.trim(), expected.trim());
    }
}

fn divergent_row_branches(ours: &str, theirs: &str) -> TempDir {
    let temp = repository();
    let path = temp.path();
    let row_path = "crm/Contact/1.orna";
    write_row(path, row_path, BASE_ROW);
    commit(path, "base row");
    succeed(path, &["branch", "ours"]);
    succeed(path, &["branch", "theirs"]);

    succeed(path, &["checkout", "ours"]);
    write_row(path, row_path, ours);
    commit(path, "ours row change");

    succeed(path, &["checkout", "theirs"]);
    write_row(path, row_path, theirs);
    commit(path, "theirs row change");

    succeed(path, &["checkout", "ours"]);
    temp
}

// ORNA-MERGE-008 (source/25-evolution.md:91): independent field edits of
// one keyed editable row can be combined. This is Git text-merge evidence only.
#[test]
fn git_merges_disjoint_editable_row_field_lines() {
    for row in [BASE_ROW, MERGE_NAME, MERGE_CITY, MERGED_ROW] {
        assert!(parse_row(row).diagnostics.is_empty());
    }
    let temp = divergent_row_branches(MERGE_NAME, MERGE_CITY);
    let path = temp.path();
    let merge = succeed(path, &["merge", "--no-edit", "theirs"]);
    assert!(merge.status.success());
    let actual = fs::read_to_string(path.join("crm/Contact/1.orna")).unwrap();
    assert_eq!(actual.trim(), MERGED_ROW.trim());
}

// ORNA-MERGE-009 (source/25-evolution.md:93): competing edits to one field
// surface as a conflict rather than Git choosing one branch's value.
#[test]
fn git_conflicts_on_different_edits_to_one_editable_row_field() {
    for row in [BASE_ROW, MERGE_NAME, CONFLICT_NAME] {
        assert!(parse_row(row).diagnostics.is_empty());
    }
    let temp = divergent_row_branches(MERGE_NAME, CONFLICT_NAME);
    let path = temp.path();
    let output = git(path, &["merge", "--no-edit", "theirs"]);
    assert!(!output.status.success());
    let status = succeed(path, &["status", "--porcelain"]);
    assert!(String::from_utf8(status.stdout).unwrap().contains("UU crm/Contact/1.orna"));
    succeed(path, &["merge", "--abort"]);
}
