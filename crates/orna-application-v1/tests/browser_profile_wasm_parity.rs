use orna_application_v1::execute_browser_profile;
use std::{env, path::PathBuf, process::Command};

const SOURCE: &str = include_str!("fixtures/browser-profile-renderer.orna");

/// ORNA-APP-015: the Wasm build of the shared evaluator matches native canonical output.
#[test]
#[ignore = "requires the wasm32-unknown-unknown target and Node.js"]
fn browser_profile_wasm_matches_native_canonical_output() {
    let expected = execute_browser_profile(SOURCE, "client")
        .expect("native shared evaluator admits the selected client")
        .encode()
        .expect("native selected result is canonical OVB");
    let expected_hex = expected
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .join("../..")
        .canonicalize()
        .expect("workspace root exists");
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let target_dir = if target_dir.is_absolute() {
        target_dir
    } else {
        workspace.join(target_dir)
    };
    let build = Command::new("cargo")
        .current_dir(&workspace)
        .args([
            "build",
            "--locked",
            "--target",
            "wasm32-unknown-unknown",
            "--no-default-features",
            "--features",
            "browser-wasm",
            "-p",
            "orna-evaluator-v1",
        ])
        .env("CARGO_BUILD_JOBS", "4")
        .status()
        .expect("cargo is available");
    assert!(build.success(), "shared evaluator Wasm build failed");

    let wasm = target_dir.join("wasm32-unknown-unknown/debug/orna_evaluator_v1.wasm");
    let script = manifest_dir.join("tests/browser_profile_wasm_parity.mjs");
    let source = manifest_dir.join("tests/fixtures/browser-profile-renderer.orna");
    let result = Command::new("node")
        .current_dir(&manifest_dir)
        .arg(script)
        .arg(wasm)
        .arg(source)
        .arg("client")
        .arg(expected_hex)
        .status()
        .expect("Node.js is available");
    assert!(result.success(), "shared evaluator Wasm output differs from native OVB output");
}
