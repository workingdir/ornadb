//! The citation ratchet must scan exactly the crates linked into the shipped binaries.

use orna_traceability_v1::citation_ratchet::CitationBaseline;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Crate directory names under `crates/` reached from every workspace binary through normal and build path dependencies.
/// Build dependencies are included on purpose: they may run during the shipped build, so the set over-approximates and never hides a shipped crate. Dev-dependencies are excluded.
fn shipped_closure() -> BTreeSet<String> {
    let cargo = env::var_os("CARGO").expect("cargo test sets CARGO");
    let output = Command::new(cargo)
        .arg("metadata")
        .args(["--no-deps", "--format-version", "1", "--locked"])
        .arg("--manifest-path")
        .arg(workspace_root().join("Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(output.status.success(), "cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr));
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("cargo metadata emits JSON");
    let packages: BTreeMap<&str, &Value> = metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .map(|package| (package["name"].as_str().expect("package name"), package))
        .collect();
    let mut pending: Vec<&str> = packages
        .iter()
        .filter(|(_, package)| {
            package["targets"]
                .as_array()
                .expect("targets array")
                .iter()
                .any(|target| target["kind"].as_array().expect("target kinds").iter().any(|kind| kind == "bin"))
        })
        .map(|(name, _)| *name)
        .collect();
    assert!(!pending.is_empty(), "workspace has no binary targets");
    let mut visited = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !visited.insert(name) {
            continue;
        }
        for dependency in packages[name]["dependencies"].as_array().expect("dependencies array") {
            let shipped = dependency["kind"].is_null() || dependency["kind"] == "build";
            let path_dependency = !dependency["path"].is_null();
            let dependency_name = dependency["name"].as_str().expect("dependency name");
            if shipped && path_dependency && packages.contains_key(dependency_name) {
                pending.push(dependency_name);
            }
        }
    }
    visited
        .into_iter()
        .map(|name| {
            Path::new(packages[name]["manifest_path"].as_str().expect("manifest path"))
                .parent()
                .and_then(Path::file_name)
                .and_then(|directory| directory.to_str())
                .expect("crate directory name")
                .to_owned()
        })
        .collect()
}

#[test]
fn scanned_crates_equal_the_shipped_binary_closure() {
    let baseline_path = workspace_root().join("crates/orna-traceability-v1/citation-baseline.json");
    let baseline: CitationBaseline =
        serde_json::from_slice(&fs::read(&baseline_path).expect("read citation baseline")).expect("parse citation baseline");
    let closure = shipped_closure();
    let missing: Vec<&String> = closure.difference(&baseline.scanned_crates).collect();
    let extra: Vec<&String> = baseline.scanned_crates.difference(&closure).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "citation ratchet scope diverges from the shipped-binary closure: missing from baseline {missing:?}, extra in baseline {extra:?}"
    );
}
