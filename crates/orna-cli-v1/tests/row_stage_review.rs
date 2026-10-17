use std::{path::Path, process::{Command, Output}};
use orna_project_v1::ProjectLoader;
use orna_repository_v1::{ManagedPath, Repository};
use tempfile::tempdir;

fn git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

fn init_repository(root: &Path, files: &[(&str, &str)]) {
    git_ok(root, &["init", "--quiet"]);
    git_ok(root, &["config", "user.name", "Test User"]);
    git_ok(root, &["config", "user.email", "test@example.invalid"]);
    for (relative, contents) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent().filter(|parent| *parent != root) {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
    git_ok(root, &["add", "--all"]);
    git_ok(root, &["commit", "--quiet", "-m", "initial"]);
}

#[test]
fn row_add_stages_one_row_and_status_separates_staged_and_unstaged() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    init_repository(root, &[
        ("main.orna", "pub table Music(id: Str) { title: Str, }\npub fn main() {}\n"),
        ("Music/london.orna", "{ title: \"old\", }\n"),
        ("Music/paris.orna", "{ title: \"old paris\", }\n"),
    ]);

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

#[test]
fn row_add_matches_full_table_namespace_and_nested_key_path() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    init_repository(root, &[
        ("main.orna", "use gallery;\npub table Music(id: Str) { title: Str, }\n"),
        ("gallery/main.orna", "pub table Music(id: Str) { title: Str, }\n"),
        ("Music/parent/london.orna", "{ title: \"root target\", }\n"),
        ("gallery/Music/parent/london.orna", "{ title: \"target\", }\n"),
        ("gallery/Music/other/london.orna", "{ title: \"other\", }\n"),
    ]);
    std::fs::write(root.join("Music/parent/london.orna"), "{ title: \"root changed\", }\n").unwrap();
    std::fs::write(root.join("gallery/Music/parent/london.orna"), "{ title: \"target changed\", }\n").unwrap();
    std::fs::write(root.join("gallery/Music/other/london.orna"), "{ title: \"other changed\", }\n").unwrap();

    let staged = cli(root, &["add", "--row", "gallery/Music", "parent/london"]);
    assert_eq!(staged.status.code(), Some(0), "{}", String::from_utf8_lossy(&staged.stderr));
    let status = String::from_utf8(git(root, &["status", "--porcelain"]).stdout).unwrap();
    assert!(status.lines().any(|line| line == "M  gallery/Music/parent/london.orna"), "{status}");
    assert!(status.lines().any(|line| line == " M Music/parent/london.orna"), "{status}");
    assert!(status.lines().any(|line| line == " M gallery/Music/other/london.orna"), "{status}");
    assert_eq!(status.lines().count(), 3, "{status}");
}

#[test]
fn add_row_rejects_trailing_arguments_without_staging() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    init_repository(root, &[
        ("main.orna", "pub table Music(id: Str) { title: Str, }\n"),
        ("Music/london.orna", "{ title: \"old\", }\n"),
    ]);

    let rejected = cli(root, &["add", "--row", "Music", "\"london\"", "surplus"]);
    assert_ne!(rejected.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("unexpected trailing arguments"));
    assert!(git(root, &["status", "--porcelain"]).stdout.is_empty());
}

#[test]
fn row_add_rejects_changed_source_and_stages_refreshed_generation() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    init_repository(root, &[
        ("main.orna", "pub table Music(id: Str) { title: Str, }\n"),
        ("Music/london.orna", "{ title: \"old\", }\n"),
    ]);

    let row_path = root.join("Music/london.orna");
    let candidate_bytes = b"{ title: \"resolved snapshot\", }\n";
    let later_bytes = b"{ title: \"later worktree edit\", }\n";
    std::fs::write(&row_path, candidate_bytes).unwrap();
    let repository = Repository::discover(root).unwrap();
    let project = ProjectLoader::default().load(&repository).unwrap();
    let candidate = project.loose_rows().iter().find(|row| row.logical_path() == "Music/london.orna").unwrap();
    let path = ManagedPath::new("Music/london.orna").unwrap();
    let index = repository.index_generation().unwrap();
    std::fs::write(&row_path, later_bytes).unwrap();

    let error = repository.stage_managed_contents(&index, &path, candidate.source_bytes()).unwrap_err();
    assert!(error.to_string().contains("changed since capture"), "{error}");
    assert_eq!(git(root, &["status", "--porcelain"]).stdout, b" M Music/london.orna\n");

    let refreshed_project = ProjectLoader::default().load(&repository).unwrap();
    let refreshed = refreshed_project.loose_rows().iter().find(|row| row.logical_path() == "Music/london.orna").unwrap();
    repository.stage_managed_contents(&index, &path, refreshed.source_bytes()).unwrap();
    let staged = git(root, &["show", ":Music/london.orna"]);
    assert_eq!(staged.status.code(), Some(0), "{}", String::from_utf8_lossy(&staged.stderr));
    assert_eq!(staged.stdout, later_bytes);
    assert_eq!(std::fs::read(row_path).unwrap(), later_bytes);
}
