use std::{path::Path, process::{Command, Output}};
use tempfile::tempdir;

fn git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git process")
}

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("Orna process")
}

fn git_ok(root: &Path, args: &[&str]) {
    let output = git(root, args);
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn row_add_stages_one_row_and_status_separates_staged_and_unstaged() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    git_ok(root, &["init", "--quiet"]);
    git_ok(root, &["config", "user.name", "Test User"]);
    git_ok(root, &["config", "user.email", "test@example.invalid"]);
    std::fs::create_dir_all(root.join("Music")).unwrap();
    std::fs::write(root.join("main.orna"), "pub table Music(id: Str) { title: Str, }\npub fn main() {}\n").unwrap();
    std::fs::write(root.join("Music/london.orna"), "{ title: \"old\", }\n").unwrap();
    std::fs::write(root.join("Music/paris.orna"), "{ title: \"old paris\", }\n").unwrap();
    git_ok(root, &["add", "."]);
    git_ok(root, &["commit", "--quiet", "-m", "initial"]);

    std::fs::write(root.join("Music/london.orna"), "{ title: \"London updated\", }\n").unwrap();
    std::fs::write(root.join("Music/paris.orna"), "{ title: \"Paris unstaged\", }\n").unwrap();

    let staged = cli(root, &["add", "--row", "Music", "\"london\""]);
    assert_eq!(staged.status.code(), Some(0), "{}", String::from_utf8_lossy(&staged.stderr));
    let status = git(root, &["status", "--porcelain"]);
    assert_eq!(status.stdout, b"M  Music/london.orna\n M Music/paris.orna\n");

    let status = cli(root, &["status", "--porcelain"]);
    assert_eq!(status.status.code(), Some(0), "{}", String::from_utf8_lossy(&status.stderr));
    assert_eq!(status.stdout, b"M  Music/london.orna\n M Music/paris.orna\n");
}
