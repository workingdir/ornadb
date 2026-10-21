use std::{collections::HashSet, fs, path::PathBuf};

use serde_json::Value;

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference"))
}

fn read_text(relative: &str) -> String {
    fs::read_to_string(reference_root().join(relative))
        .unwrap_or_else(|error| panic!("read reference {relative}: {error}"))
}

fn read_json(relative: &str) -> Value {
    serde_json::from_str(&read_text(relative))
        .unwrap_or_else(|error| panic!("parse reference {relative}: {error}"))
}

fn html_ids(html: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut remaining = html;
    while let Some((_, after_name)) = remaining.split_once("id=\"") {
        let (id, after_id) = after_name
            .split_once('"')
            .expect("HTML id attribute has a closing quote");
        ids.push(id.to_owned());
        remaining = after_id;
    }
    ids
}

fn source_chapters() -> Vec<String> {
    let manifest = read_json("source/manifest.json");
    manifest
        .as_array()
        .expect("source manifest array")
        .iter()
        .map(|chapter| chapter["file"].as_str().expect("manifest source path").to_owned())
        .collect()
}

fn expected_api_names(api: &Value) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut add = |name: &str| {
        names.insert(name.to_owned());
    };
    for value in api["singletons"].as_array().expect("singletons array") {
        add(value["name"].as_str().expect("singleton name"));
    }
    for value in api["opaque_identifiers"].as_array().expect("opaque identifiers array") {
        add(value.as_str().expect("opaque identifier name"));
    }
    for value in api["reference_aliases"].as_array().expect("reference aliases array") {
        add(value["name"].as_str().expect("reference alias name"));
    }
    for value in api["value_types"].as_array().expect("value types array") {
        let name = value["name"].as_str().expect("value type name");
        add(name.split(['<', '(']).next().expect("value type base name"));
    }
    for name in api["enums"].as_object().expect("enum object").keys() {
        add(name);
    }
    for value in api["relations"].as_array().expect("relations array") {
        add(value["name"].as_str().expect("relation name"));
    }
    for value in api["functions"].as_array().expect("functions array") {
        let name = value["name"].as_str().expect("function name");
        add(name.split(['<', '(']).next().expect("function base name"));
    }
    names
}

#[test]
fn explicit_source_anchors_are_unique_across_the_book() {
    let mut seen = HashSet::new();
    for source in source_chapters() {
        for line in read_text(&source).lines() {
            if let Some((_, rest)) = line.split_once("{#") {
                let anchor = rest.split('}').next().expect("anchor closing brace").trim();
                assert!(seen.insert(anchor.to_owned()), "duplicate explicit anchor {anchor} in {source}");
            }
        }
    }
    assert!(!seen.is_empty(), "1.1.0 source must declare explicit heading anchors");
}

#[test]
fn every_sys_api_name_is_documented_in_the_system_reference() {
    let api = read_json("api/sys.json");
    let reference = read_text("source/34-system-reference.md");
    for name in expected_api_names(&api) {
        assert!(reference.contains(&name), "system reference does not document {name}");
    }
}

#[test]
fn published_index_contains_every_publication_requirement() {
    let source = read_text("source/22-publication.md");
    let expected: HashSet<_> = source
        .split("**")
        .filter_map(|part| part.strip_prefix("ORNA-PUB-"))
        .filter_map(|part| part.split("**").next())
        .map(|suffix| format!("ORNA-PUB-{suffix}"))
        .collect();
    assert_eq!(expected.len(), 21, "the 1.1.0 publication chapter's normative PUB inventory");
    let html = read_text("index.html");
    for requirement in expected {
        assert!(html.contains(&requirement), "published book is missing {requirement}");
    }
}

#[test]
fn published_index_ids_are_unique() {
    let html = read_text("index.html");
    let ids = html_ids(&html);
    let unique: HashSet<_> = ids.iter().cloned().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate id in index.html");
}

#[test]
fn published_sha256sums_cover_the_book_artifacts() {
    let sums = read_text("SHA256SUMS");
    for artifact in ["Orna-1.1.0.md", "index.html"] {
        assert!(
            sums.lines().any(|line| line.ends_with(artifact)),
            "SHA256SUMS does not list {artifact}"
        );
    }
}
