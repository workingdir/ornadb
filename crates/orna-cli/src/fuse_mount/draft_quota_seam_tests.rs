use std::sync::Arc;

#[test]
fn default_build_ignores_draft_quota_env() {
    const CHILD_MARKER: &str = "ORNA_DRAFT_QUOTA_SEAM_CHILD";
    const TEST_NAME: &str =
        "fuse_mount::draft_quota_seam_tests::default_build_ignores_draft_quota_env";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_MARKER, "1")
            .env("ORNA_DRAFT_QUOTA_BYTES", "1")
            .output()
            .expect("re-run the test binary");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "child run did not pass the default-build probe:\n{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let quota = super::mount_draft_quota().expect("default build has a quota");
    assert!(
        Arc::ptr_eq(&quota, &orna_runtime::vfs::default_draft_quota()),
        "default build must charge the production budget whatever ORNA_DRAFT_QUOTA_BYTES says"
    );
}
