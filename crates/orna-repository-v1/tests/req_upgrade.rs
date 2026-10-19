use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use tempfile::TempDir;

const DATABASE: &str = include_str!("fixtures/format-context/database-final.orna");

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

#[test]
fn req_upgrade_003_admits_only_canonical_format3_store_writes() {
    // ORNA-UPGRADE-003: new-format writes use one canonical row store; loose,
    // compact, and hybrid dual-writing is forbidden.
    let root = TempDir::new().expect("temporary repository");
    git(root.path(), &["init", "--quiet", "--template="]);
    git(root.path(), &["config", "user.name", "Format-3 conformance"]);
    git(root.path(), &["config", "user.email", "format3@example.invalid"]);
    fs::write(
        root.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .expect("write source root");
    fs::create_dir_all(root.path().join(".orna/store")).expect("create canonical store root");
    fs::write(root.path().join(".orna/store/data"), "store root").expect("write store root");
    fs::write(root.path().join(".orna/database.orna"), DATABASE).expect("write format metadata");
    git(root.path(), &["add", "--all"]);
    git(root.path(), &["commit", "--quiet", "-m", "format-3 writer fixture"]);

    let repository = Repository::discover(root.path()).expect("discover repository");
    let context = repository.open_format_context().expect("admit format-3 context");
    let capability = context
        .final_format_capability()
        .expect("format-3 writer capability validates schema and store roots");
    assert_eq!(
        capability.database_id().to_string(),
        "9f237b7e-6844-4498-bcd5-d24641c07449"
    );

    let tracked = git(root.path(), &["ls-tree", "-r", "--name-only", "HEAD"]);
    let paths: Vec<_> = tracked.lines().collect();
    assert!(paths.contains(&".orna/store/data"));
    assert!(paths.contains(&".orna/database.orna"));
    assert!(!paths.iter().any(|path| path.starts_with(".orna/rows/")));
    assert!(!paths.iter().any(|path| path.starts_with(".orna/compact/")));
    assert!(!paths.iter().any(|path| *path == ".orna/format.orna"));
}
