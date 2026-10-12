use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
};

use orna_conformance_v1::Corpus;
use sha2::{Digest, Sha256};
use serde_json::Value;

fn reference_root() -> PathBuf {
    env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference"))
}

fn full_publication_root_is_explicit() -> bool {
    env::var_os("ORNA_REFERENCE_DIR").is_some()
}

#[test]
fn default_corpus_loads_from_the_checked_in_fixture_root() {
    let corpus = Corpus::load_default().expect("checked-in conformance corpus loads by default");
    assert_eq!(corpus.publication_digests.len(), 46);
    if !full_publication_root_is_explicit() {
        assert_eq!(
            Corpus::default_root(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference")
        );
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn collect_distribution_files(root: &Path, directory: &Path, output: &mut BTreeSet<String>) {
    for entry in fs::read_dir(directory).expect("read release directory") {
        let entry = entry.expect("read release directory entry");
        let path = entry.path();
        let kind = entry.file_type().expect("read release entry type");
        assert!(!kind.is_symlink(), "release input cannot be a symlink: {path:?}");
        if kind.is_dir() {
            if entry.file_name() != "__pycache__" {
                collect_distribution_files(root, &path, output);
            }
            continue;
        }
        if !kind.is_file() || path.extension().is_some_and(|ext| ext == "pyc") {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            !matches!(extension.as_str(), "ttf" | "otf" | "woff" | "woff2" | "ttc"),
            "raw font is not a distributable file: {path:?}"
        );
        let relative = path.strip_prefix(root).expect("release member under root");
        assert!(
            relative
                .components()
                .all(|part| !matches!(part, Component::ParentDir | Component::RootDir))
        );
        let relative = relative
            .to_str()
            .expect("release member path is UTF-8")
            .replace('\\', "/");
        assert!(!relative.contains('\\'));
        assert!(!relative.contains('\n'));
        assert!(!relative.contains('\r'));
        output.insert(relative);
    }
}

fn actual_distribution_files(root: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    collect_distribution_files(root, root, &mut paths);
    paths
}

#[test]
fn frozen_release_inventory_and_provenance_bind_the_complete_bundle() {
    let root = reference_root();
    let corpus = Corpus::load(&root).expect("conformance consumer verifies normative release payloads");
    if !full_publication_root_is_explicit() {
        // The checked-in subset covers runtime corpus consumers; whole-package
        // distributor inventory checks remain available with an explicit bundle.
        assert_eq!(corpus.publication_digests.len(), 46);
        return;
    }
    let release: Value = serde_json::from_slice(
        &fs::read(root.join("release.json")).expect("read release record"),
    )
    .expect("parse release record");
    assert_eq!(release["version"], "1.0.0");
    let normative_digests: BTreeMap<String, String> =
        serde_json::from_value(release["normative_payload_sha256"].clone())
            .expect("normative digest map");
    assert_eq!(normative_digests.len(), 46);
    assert_eq!(corpus.publication_digests, normative_digests);

    let manifest: Value = serde_json::from_slice(
        &fs::read(root.join("file-manifest.json")).expect("read file manifest"),
    )
    .expect("parse file manifest");
    assert_eq!(
        manifest["scope"],
        "All distributed files except this manifest and SHA256SUMS"
    );
    let actual_files = actual_distribution_files(&root);
    let expected_manifest_paths: BTreeSet<_> = actual_files
        .iter()
        .filter(|path| {
            path.rsplit('/').next() != Some("file-manifest.json")
                && path.rsplit('/').next() != Some("SHA256SUMS")
        })
        .cloned()
        .collect();
    let mut manifest_paths = BTreeSet::new();
    let entries = manifest["files"].as_array().expect("manifest file entries");
    for entry in entries {
        let relative = entry["path"].as_str().expect("manifest path");
        let path = Path::new(relative);
        assert!(!path.is_absolute(), "absolute file-manifest path: {relative}");
        assert!(path.components().all(|part| !matches!(part, Component::ParentDir)));
        assert!(manifest_paths.insert(relative.to_owned()), "duplicate path: {relative}");
        let bytes = fs::read(root.join(path)).expect("manifest member exists");
        assert_eq!(entry["bytes"].as_u64(), Some(bytes.len() as u64), "size: {relative}");
        assert_eq!(entry["sha256"].as_str(), Some(digest(&bytes).as_str()), "digest: {relative}");
    }
    assert_eq!(entries.len(), 415);
    assert_eq!(manifest_paths, expected_manifest_paths, "file-manifest completeness");

    for publication in release["publications"].as_array().expect("publication entries") {
        let relative = publication["file"].as_str().expect("published member path");
        let bytes = fs::read(root.join(relative)).expect("published member exists");
        assert_eq!(
            publication["sha256"].as_str(),
            Some(digest(&bytes).as_str()),
            "published digest: {relative}"
        );
        assert!(manifest_paths.contains(relative), "publication absent from file-manifest: {relative}");
    }
    for field in [
        "authoritative_input_inventory",
        "finding_dispositions",
        "evidence_report",
    ] {
        let relative = release[field].as_str().expect("release metadata path");
        assert!(manifest_paths.contains(relative), "release metadata input absent: {relative}");
    }

    let sums_text = fs::read_to_string(root.join("SHA256SUMS")).expect("read SHA256SUMS");
    let expected_sum_paths: BTreeSet<_> = actual_files
        .iter()
        .filter(|path| path.rsplit('/').next() != Some("SHA256SUMS"))
        .cloned()
        .collect();
    let mut sum_paths = BTreeSet::new();
    for line in sums_text.lines() {
        let (expected, relative) = line.split_once("  ").expect("two-space checksum separator");
        assert_eq!(expected.len(), 64);
        assert!(expected.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(sum_paths.insert(relative.to_owned()), "duplicate sum path: {relative}");
        let bytes = fs::read(root.join(relative)).expect("checksum member exists");
        assert_eq!(expected, digest(&bytes), "SHA256SUMS: {relative}");
    }
    assert_eq!(sum_paths.len(), 416);
    assert_eq!(sum_paths, expected_sum_paths, "SHA256SUMS completeness");

    let inputs: Value = serde_json::from_slice(
        &fs::read(root.join("provenance/inputs.json")).expect("read provenance inputs"),
    )
    .expect("parse provenance inputs");
    let input_records = inputs.as_object().expect("provenance input inventory");
    assert_eq!(input_records.len(), 3);
    for (name, record) in input_records {
        let relative = format!("provenance/inputs/{name}");
        let bytes = fs::read(root.join(&relative)).expect("provenance input archive exists");
        assert_eq!(record["bytes"].as_u64(), Some(bytes.len() as u64), "size: {name}");
        assert_eq!(record["sha256"].as_str(), Some(digest(&bytes).as_str()), "digest: {name}");
        assert!(manifest_paths.contains(&relative), "provenance input not in file-manifest: {name}");
    }

    println!(
        "verified normative_payloads={} file_manifest_files={} checksum_files={} provenance_inputs={}",
        corpus.publication_digests.len(),
        manifest_paths.len(),
        sum_paths.len(),
        input_records.len(),
    );
}

#[test]
fn release_file_walk_matches_the_reference_packager_exclusion_rules() {
    let root = reference_root();
    if !full_publication_root_is_explicit() {
        let corpus = Corpus::load(&root).expect("checked-in corpus is complete for conformance");
        assert_eq!(corpus.publication_digests.len(), 46);
        return;
    }
    let files = actual_distribution_files(&root);
    assert!(files.contains("release.json"));
    assert!(files.contains("file-manifest.json"));
    assert!(files.contains("SHA256SUMS"));
    assert!(files.contains("provenance/inputs.json"));
    assert!(!files.iter().any(|path| path.ends_with(".pyc")));
    assert!(!files.iter().any(|path| path.contains("/__pycache__/")));
    assert_eq!(files.len(), 417);
}
