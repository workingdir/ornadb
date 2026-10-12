use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Output},
};

use tempfile::tempdir;

const SOURCE: &str = include_str!(
    "../../orna-conformance-v1/tests/fixtures/traceability-diff-trust-secret.orna"
);

fn git(repository: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .output()
        .expect("git starts")
}

fn git_succeeds(repository: &Path, arguments: &[&str]) {
    let output = git(repository, arguments);
    assert!(
        output.status.success(),
        "git {arguments:?} failed: stdout={:?} stderr={:?}",
        output.stdout,
        output.stderr
    );
}

#[test]
fn raw_git_diff_remains_available_to_a_local_invocation() {
    let repository = tempdir().expect("temporary repository");
    let root_owner = std::fs::metadata(repository.path())
        .expect("read repository ownership")
        .uid();
    let source_path = repository.path().join("source.orna");
    std::fs::write(&source_path, SOURCE).expect("write checked-in Orna fixture");
    assert_eq!(
        std::fs::metadata(&source_path)
            .expect("read fixture ownership")
            .uid(),
        root_owner,
        "the fixture remains owned by the invoking OS account"
    );

    git_succeeds(repository.path(), &["init", "--quiet", "--initial-branch=main"]);
    git_succeeds(repository.path(), &["add", "--", "source.orna"]);

    let changed = SOURCE.replace("google.personal", "google.audit");
    assert_ne!(changed, SOURCE, "the real fixture contains the stable ref");
    std::fs::write(&source_path, changed).expect("modify fixture source");

    let raw = git(
        repository.path(),
        &["diff", "--no-color", "--", "source.orna"],
    );
    let cli = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["diff", "--no-color", "--", "source.orna"])
        .current_dir(repository.path())
        .output()
        .expect("Orna CLI starts");

    assert_eq!(raw.status.code(), Some(0));
    assert_eq!(cli.status.code(), raw.status.code());
    assert_eq!(cli.stdout, raw.stdout);
    assert_eq!(cli.stderr, raw.stderr);
    assert!(String::from_utf8_lossy(&cli.stdout).contains("google.personal"));
    assert!(String::from_utf8_lossy(&cli.stdout).contains("google.audit"));
}
