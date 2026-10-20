//! `orna check` admits a project against the providers that production hosts
//! link (ORNA-SYS-008): a portable `sys` callable without a linked provider is
//! rejected with `sys.version.incompatible`, while a linked one checks clean.

use std::process::{Command, Output};

use tempfile::TempDir;

fn project(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("sys admission fixture project");
    std::fs::write(directory.path().join("main.orna"), source).expect("write fixture source");
    let initialized = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("initialize fixture project");
    assert!(
        initialized.status.success(),
        "init stderr: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    directory
}

fn check(project: &TempDir) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            project.path().to_str().expect("UTF-8 path"),
            "--color",
            "never",
            "check",
        ])
        .output()
        .expect("run orna check")
}

#[test]
fn check_rejects_unlinked_sys_callables_with_version_incompatible() {
    for (source, member) in [
        ("pub fn info() = sys.rt.info();\n", "`sys.rt.info`"),
        (
            "pub fn head() = sys.snapshot(\"HEAD~1\");\n",
            "`sys.snapshot(Str)`",
        ),
    ] {
        let project = project(source);
        let output = check(&project);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{source}: {stderr}");
        assert!(stderr.contains("sys.version.incompatible"), "{source}: {stderr}");
        assert!(stderr.contains(member), "{source}: {stderr}");
    }
}

#[test]
fn check_admits_the_linked_blob_annotation() {
    let project = project(
        "pub fn label(value: Blob): Blob = sys.blob.annotate(value, \"text/plain\");\n",
    );
    let output = check(&project);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
