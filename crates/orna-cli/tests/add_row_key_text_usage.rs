//! `add --row` names its key argument `KEY_TEXT`, the canonical Orna key
//! expression of spec chapter 18 (`orna add --row TABLE KEY_TEXT`).

use std::process::Command;

#[test]
fn add_row_usage_remedy_names_key_text() {
    let directory = tempfile::tempdir().expect("empty directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["add", "--row", "Contact"])
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run orna add --row Contact");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(
        combined.contains("use `add --row TABLE KEY_TEXT`"),
        "{combined}"
    );
    assert!(!combined.contains("TABLE KEY`"), "{combined}");
}
