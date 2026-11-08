use std::{path::PathBuf, process::Command};

// ORNA-APP-016 (ornadb-w2gdn): executes the shipped browser host JS under Node and
// requires it to touch only the platform profile surface. Node must be installed;
// a missing interpreter fails the test rather than skipping it.
#[test]
fn browser_profile_host_js_binds_platform_surface_only() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = manifest_dir.join("tests/browser_profile_host_surface.mjs");
    let output = Command::new("node")
        .current_dir(&manifest_dir)
        .arg(&script)
        .output()
        .expect("Node.js is available");
    assert!(
        output.status.success(),
        "browser host JS surface check failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
