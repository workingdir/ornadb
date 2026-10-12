use std::process::Command;

#[test]
fn capability_demo_runs_declaration_resolution_cases() {
    let test_binary = std::env::current_exe().expect("test binary path should be available");
    let demo = test_binary
        .parent()
        .and_then(std::path::Path::parent)
        .expect("test binary should live below the target directory")
        .join("examples/client_capability_demo");
    let output = Command::new(demo)
        .output()
        .expect("capability demo should start");

    assert!(
        output.status.success(),
        "capability demo exited with {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!(
            "local grant matching: literal declaration allowed\n",
            "local grant matching: parameter declaration allowed\n",
            "local grant matching: unresolved parameter denied\n",
            "local grant matching: child path allowed (/home/demo/project/src/main.orna)\n",
            "local grant matching: sibling path denied (/home/demo/project-other/src/main.orna)\n",
        )
    );
    assert!(
        output.stderr.is_empty(),
        "capability demo wrote unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
