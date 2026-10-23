use std::{collections::HashSet, fs, path::PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

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

/// Exact values of every `id="..."` attribute, in document order.
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

/// Exact `ORNA-PUB-NNN` tokens: the prefix must be followed by exactly three
/// digits and no further digit, so no substring of a longer identifier counts.
fn html_publication_tokens(html: &str) -> Vec<String> {
    const PREFIX: &str = "ORNA-PUB-";
    let mut tokens = Vec::new();
    let mut remaining = html;
    while let Some(position) = remaining.find(PREFIX) {
        let after = &remaining[position + PREFIX.len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if digits.len() == 3 && !after[3..].starts_with(|c: char| c.is_ascii_digit()) {
            tokens.push(format!("{PREFIX}{digits}"));
        }
        remaining = &after[digits.len()..];
    }
    tokens
}

fn source_chapters() -> Vec<String> {
    read_json("source/manifest.json")
        .as_array()
        .expect("source manifest array")
        .iter()
        .map(|chapter| chapter["file"].as_str().expect("manifest source path").to_owned())
        .collect()
}

/// Anchor slug used by `tools/generate_api_reference.py`.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// The exact `api-*` anchors the generator emits for `api/sys.json`.
fn expected_api_anchors(api: &Value) -> HashSet<String> {
    let mut anchors = HashSet::new();
    for value in api["opaque_identifiers"].as_array().expect("opaque identifiers array") {
        let name = value
            .as_str()
            .or_else(|| value["name"].as_str())
            .expect("opaque identifier name");
        anchors.insert(format!("api-type-{}", slug(name)));
    }
    for name in api["enums"].as_object().expect("enum object").keys() {
        anchors.insert(format!("api-type-{}", slug(name)));
    }
    for group in ["value_types", "relations"] {
        for value in api[group].as_array().expect("api group array") {
            let name = value["name"].as_str().expect("api type name");
            anchors.insert(format!("api-type-{}", slug(name)));
        }
    }
    for value in api["functions"].as_array().expect("functions array") {
        let name = value["name"].as_str().expect("function name");
        anchors.insert(format!("api-call-{}", slug(name)));
    }
    anchors
}

#[test]
fn explicit_source_anchors_are_unique_across_the_book() {
    let mut seen = HashSet::new();
    for source in source_chapters() {
        for line in read_text(&source).lines() {
            if let Some((_, rest)) = line.split_once("{#") {
                let anchor = rest.split('}').next().expect("anchor closing brace").trim();
                assert!(
                    seen.insert(anchor.to_owned()),
                    "duplicate explicit anchor {anchor} in {source}"
                );
            }
        }
    }
    assert!(!seen.is_empty(), "1.1.0 source must declare explicit heading anchors");
}

#[test]
fn published_api_anchors_equal_the_sys_api_surface() {
    let api = read_json("api/sys.json");
    let html = read_text("index.html");
    let published: HashSet<String> = html_ids(&html)
        .into_iter()
        .filter(|id| id.starts_with("api-"))
        .collect();
    let expected = expected_api_anchors(&api);
    assert!(!expected.is_empty(), "api/sys.json declares API surface");
    let missing: Vec<_> = expected.difference(&published).collect();
    let extra: Vec<_> = published.difference(&expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "rendered api anchors differ from api/sys.json: missing {missing:?}, extra {extra:?}"
    );
}

#[test]
fn published_index_publication_ids_equal_the_publication_manifest() {
    let expected: HashSet<String> = read_json("tests/requirements.json")
        .as_array()
        .expect("requirements manifest array")
        .iter()
        .filter(|requirement| requirement["chapter"] == "publication")
        .filter_map(|requirement| requirement["id"].as_str())
        .filter(|id| id.starts_with("ORNA-PUB-"))
        .map(str::to_owned)
        .collect();
    assert!(!expected.is_empty(), "publication manifest declares ORNA-PUB requirements");

    let tokens = html_publication_tokens(&read_text("index.html"));
    let published: HashSet<String> = tokens.iter().cloned().collect();
    assert_eq!(
        published.len(),
        tokens.len(),
        "each ORNA-PUB requirement appears exactly once in the rendered book"
    );
    assert_eq!(
        published, expected,
        "rendered ORNA-PUB requirements equal the publication manifest"
    );
}

#[test]
fn published_index_ids_are_unique() {
    let ids = html_ids(&read_text("index.html"));
    let unique: HashSet<_> = ids.iter().cloned().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate id in index.html");
}

#[test]
fn canonical_markdown_matches_its_sha256sums_entry() {
    let root = reference_root();
    let bytes = fs::read(root.join("Orna-1.1.0.md")).expect("read canonical markdown");
    let digest = format!("{:x}", Sha256::digest(bytes));
    let published = read_text("SHA256SUMS")
        .lines()
        .find_map(|line| {
            let (digest, name) = line.split_once("  ")?;
            (name == "Orna-1.1.0.md").then(|| digest.to_owned())
        })
        .expect("SHA256SUMS lists Orna-1.1.0.md");
    assert_eq!(digest, published, "Orna-1.1.0.md matches its SHA256SUMS entry");
}
