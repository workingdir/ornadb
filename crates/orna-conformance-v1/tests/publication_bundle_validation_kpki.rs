use std::{collections::HashSet, fs, path::PathBuf};

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Heading {
    id: String,
    level: usize,
    source: String,
    source_line: usize,
}

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

fn heading_index() -> Vec<Heading> {
    serde_json::from_str(&read_text("publication/heading-index.json"))
        .expect("heading index is a JSON array of source locations")
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
        add(name);
        add(name.split(['<', '(']).next().expect("value type base name"));
    }
    for name in api["enums"].as_object().expect("enum object").keys() {
        add(name);
    }
    for value in api["relations"].as_array().expect("relations array") {
        add(value["name"].as_str().expect("relation name"));
        add(value["grouped_handle"].as_str().expect("relation handle"));
    }
    for value in api["functions"].as_array().expect("functions array") {
        let name = value["name"].as_str().expect("function name");
        add(name);
        add(name.split(['<', '(']).next().expect("function base name"));
    }
    names
}

#[test]
fn heading_index_ids_are_unique_and_point_to_real_heading_lines() {
    let headings = heading_index();
    let mut ids = HashSet::new();
    for entry in headings {
        assert!(ids.insert(entry.id.clone()), "duplicate heading id {}", entry.id);
        assert!(entry.source.starts_with("source/"), "unexpected source {}", entry.source);
        let source = read_text(&entry.source);
        let line = source
            .lines()
            .nth(entry.source_line.saturating_sub(1))
            .unwrap_or_else(|| panic!("{}:{} is outside its source", entry.source, entry.source_line));
        let actual_level = line.chars().take_while(|character| *character == '#').count();
        assert_eq!(actual_level, entry.level, "{}:{} is not the indexed heading", entry.source, entry.source_line);
        assert!(line[actual_level..].starts_with(' '), "{}:{} is not an ATX heading", entry.source, entry.source_line);
        if let Some((_, explicit)) = line.split_once("{#") {
            let explicit = explicit.trim_end_matches('}').trim();
            assert_eq!(entry.id, explicit, "explicit heading anchor at {}:{}", entry.source, entry.source_line);
        }
    }
}

#[test]
fn heading_index_covers_every_source_heading_outside_code_fences() {
    let headings = heading_index();
    let manifest = read_json("source/manifest.json");
    let entries = manifest.as_array().expect("source manifest array");
    for chapter in entries {
        let source = chapter["file"].as_str().expect("manifest source path");
        let text = read_text(source);
        let mut in_fence = false;
        let actual = text
            .lines()
            .filter(|line| {
                if line.starts_with("```") {
                    in_fence = !in_fence;
                    return false;
                }
                !in_fence && {
                    let level = line.chars().take_while(|character| *character == '#').count();
                    (1..=6).contains(&level) && line[level..].starts_with(' ')
                }
            })
            .count();
        let indexed = headings.iter().filter(|heading| heading.source == source).count();
        assert_eq!(indexed, actual, "heading-index coverage for {source}");
    }
}

#[test]
fn api_index_names_match_the_declared_system_api_surface() {
    let api = read_json("api/sys.json");
    let index = read_json("publication/api-index.json");
    let indexed = index.as_object().expect("API index object");
    let expected = expected_api_names(&api);
    let actual: HashSet<_> = indexed.keys().cloned().collect();
    assert_eq!(actual, expected, "publication API index must cover exactly the declared sys names");
}

#[test]
fn api_index_targets_resolve_in_both_published_html_documents() {
    let index = read_json("publication/api-index.json");
    let targets: HashSet<_> = index
        .as_object()
        .expect("API index object")
        .values()
        .map(|target| target.as_str().expect("API target string").to_owned())
        .collect();
    for document in ["index.html", "sys-reference.html"] {
        let html = read_text(document);
        let ids = html_ids(&html);
        let unique: HashSet<_> = ids.iter().cloned().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate id in {document}");
        for target in &targets {
            assert!(unique.contains(target), "{document} has no id target {target}");
        }
    }
}

#[test]
fn publication_requirement_ids_are_published_as_direct_anchors() {
    let source = read_text("source/22-publication.md");
    let expected: HashSet<_> = source
        .split("**")
        .filter_map(|part| part.strip_prefix("ORNA-PUB-"))
        .filter_map(|part| part.split("**").next())
        .map(|suffix| format!("ORNA-PUB-{suffix}"))
        .collect();
    assert_eq!(expected.len(), 17, "the publication chapter's normative PUB inventory");
    let html = read_text("index.html");
    let ids: HashSet<_> = html_ids(&html).into_iter().collect();
    for requirement in expected {
        assert!(ids.contains(&requirement), "published book is missing {requirement}");
    }
}
