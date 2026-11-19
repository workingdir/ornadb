//! The traceability report carries the profile model that `load_checked` binds
//! to `release.json`. These tests pin that model to the published records, so
//! the gate reads what the reference publishes rather than a copied count.

use orna_conformance::release_profile::load_checked;
use orna_traceability::generate;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const COORDINATE_KEYS: [&str; 11] = [
    "language",
    "publication_revision",
    "repository_format",
    "graph",
    "rows",
    "blob_storage",
    "stored_values",
    "semantic_values",
    "portable_values",
    "virtual_filesystem",
    "media_policy",
];

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference")
        })
}

fn read_json(root: &Path, name: &str) -> serde_json::Value {
    let path = root.join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// A profile's requirement list. An absent or null field is empty; any other
/// non-array value is a malformed record and fails the test.
fn json_strings(record: &serde_json::Value, key: &str) -> Vec<String> {
    match &record[key] {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .expect("requirement IDs are strings")
                    .to_owned()
            })
            .collect(),
        other => panic!("{key} must be an array of strings, found {other}"),
    }
}

#[test]
fn typed_profiles_match_the_release_bound_record() {
    let root = reference_root();
    let (profiles, _) = load_checked(&root).expect("reference profiles bind to release.json");
    let record = read_json(&root, "profiles/conformance-1.1.json");

    assert_eq!(record["version"].as_str(), Some(profiles.version.as_str()));
    assert_eq!(
        record["publication_revision"].as_str(),
        Some(profiles.publication_revision.as_str())
    );
    let typed_names: BTreeSet<&str> = profiles.profiles.keys().map(String::as_str).collect();
    let record_names: BTreeSet<&str> = record["profiles"]
        .as_object()
        .expect("profiles is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(typed_names, record_names);

    for (name, requirements) in &profiles.profiles {
        let profile = &record["profiles"][name.as_str()];
        assert_eq!(
            requirements.required,
            json_strings(profile, "required"),
            "{name}: required"
        );
        assert_eq!(
            requirements.required_prefixes,
            json_strings(profile, "required_prefixes"),
            "{name}: required_prefixes"
        );
        assert_eq!(
            requirements.requires.as_deref(),
            profile["requires"].as_str(),
            "{name}: requires"
        );
        assert!(
            !requirements.required.is_empty()
                || !requirements.required_prefixes.is_empty()
                || requirements.requires.is_some(),
            "{name} names at least one requirement source"
        );
        if let Some(target) = &requirements.requires {
            assert!(
                profiles.profiles.contains_key(target),
                "{name} requires a declared profile, found {target}"
            );
        }
    }
}

#[test]
fn typed_coordinates_keep_every_identifier_the_gate_reads() {
    let root = reference_root();
    let (_, coordinates) = load_checked(&root).expect("reference coordinates bind to release.json");
    let record = read_json(&root, "profiles/coordinates.json");
    let typed = serde_json::to_value(&coordinates).expect("coordinates serialize");
    let typed_keys: BTreeSet<&str> = typed
        .as_object()
        .expect("coordinates serialize as an object")
        .keys()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<&str> = COORDINATE_KEYS.iter().copied().collect();
    assert_eq!(typed_keys, expected);
    for key in COORDINATE_KEYS {
        assert_eq!(
            typed[key], record[key],
            "coordinate {key} matches the release-bound record"
        );
    }
}

#[test]
fn report_carries_the_release_bound_profile_model() {
    let root = reference_root();
    let report = generate(&root).expect("reference corpus generates a traceability report");
    let (profiles, coordinates) =
        load_checked(&root).expect("reference profiles bind to release.json");
    assert_eq!(report.conformance_profiles, profiles);
    assert_eq!(report.coordinates, coordinates);
    assert_eq!(report.conformance_profiles.version, "1.1.0");
    assert_eq!(
        report.conformance_profiles.publication_revision,
        "final-2026-10-05"
    );
    let release = read_json(&root, "release.json");
    assert_eq!(
        release["version"].as_str(),
        Some(report.conformance_profiles.version.as_str())
    );
    assert_eq!(
        release["publication_revision"].as_str(),
        Some(report.conformance_profiles.publication_revision.as_str())
    );
}
