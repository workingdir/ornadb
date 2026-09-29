use std::{collections::BTreeSet, process::Command};

use orna_syntax_v1::parse_module;
use serde_json::json;

const SOURCE_PROBES: &str = include_str!("fixtures/reference-tools/source-probes.orna");
const INDEX_HTML: &str = include_str!("../../../../reference/Orna-1.0.0/index.html");

const NAVIGATION_HARNESS: &str = r#"
const fs = require('fs'), vm = require('vm');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
let listener;
class Element {
  constructor(tag='') { this.tag=tag; this.children=[]; this.hidden=false; this.textContent=''; }
  append(...x) { this.children.push(...x); }
  replaceChildren(...x) { this.children=x; this.textContent=''; }
}
const results=new Element('div'), outline=new Element('nav');
const search={addEventListener:(kind,fn)=>{if(kind!=='input')throw Error(kind);listener=fn;}};
const document={getElementById:(id)=>id==='nav-search'?search:results,
 querySelector:()=>outline,createElement:(tag)=>new Element(tag),createTextNode:(x)=>({textContent:x})};
const context=vm.createContext({document}); vm.runInContext(input.script,context);
const entries=vm.runInContext('entries',context);
const existing=new Set(input.ids);
for(const entry of entries) if(!existing.has(entry.id)) throw Error('Missing anchor '+entry.id);
const checks=[];
for(const q of ['sys.rt.info','ORNA-CONCUR-001','Branching with pending changes','', '<img onerror=bad()>']) {
 listener.call({value:q});
 if(!q && (!results.hidden || outline.hidden)) throw Error('Empty search');
 if(q && (results.hidden || !outline.hidden)) throw Error('Search visibility');
 if(q && !q.startsWith('<') && results.children.length===0) throw Error('No hits for '+q);
 if(q.startsWith('<') && results.children.length!==0) throw Error('Unexpected query match');
 checks.push({query:q,hits:results.children.length,pass:true});
}
console.log(JSON.stringify({node:process.version,entries:entries.length,all_targets_exist:true,checks}));
"#;

fn html_attribute_values(html: &str, wanted: &str) -> Vec<String> {
    let bytes = html.as_bytes();
    let mut values = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = html[cursor..].find('<') {
        let start = cursor + relative;
        if html[start..].starts_with("<!--") {
            cursor = html[start + 4..]
                .find("-->")
                .map(|end| start + 4 + end + 3)
                .unwrap_or(bytes.len());
            continue;
        }
        let mut end = start + 1;
        let mut quote = None;
        while end < bytes.len() {
            let byte = bytes[end];
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
            } else if byte == b'\'' || byte == b'"' {
                quote = Some(byte);
            } else if byte == b'>' {
                break;
            }
            end += 1;
        }
        if end == bytes.len() {
            break;
        }
        let tag = &html[start + 1..end];
        let mut i = tag
            .find(char::is_whitespace)
            .unwrap_or(tag.len());
        if tag.starts_with('/') {
            cursor = end + 1;
            continue;
        }
        while i < tag.len() {
            while i < tag.len() && tag.as_bytes()[i].is_ascii_whitespace() {
                i += 1;
            }
            let name_start = i;
            while i < tag.len()
                && !tag.as_bytes()[i].is_ascii_whitespace()
                && !matches!(tag.as_bytes()[i], b'=' | b'/')
            {
                i += 1;
            }
            if i == name_start {
                i += 1;
                continue;
            }
            let name = &tag[name_start..i];
            while i < tag.len() && tag.as_bytes()[i].is_ascii_whitespace() {
                i += 1;
            }
            if i == tag.len() || tag.as_bytes()[i] != b'=' {
                continue;
            }
            i += 1;
            while i < tag.len() && tag.as_bytes()[i].is_ascii_whitespace() {
                i += 1;
            }
            let (value_start, value_end) = if i < tag.len() && matches!(tag.as_bytes()[i], b'\'' | b'"') {
                let delimiter = tag.as_bytes()[i];
                i += 1;
                let value_start = i;
                while i < tag.len() && tag.as_bytes()[i] != delimiter {
                    i += 1;
                }
                let value_end = i;
                i += usize::from(i < tag.len());
                (value_start, value_end)
            } else {
                let value_start = i;
                while i < tag.len()
                    && !tag.as_bytes()[i].is_ascii_whitespace()
                    && tag.as_bytes()[i] != b'>'
                {
                    i += 1;
                }
                (value_start, i)
            };
            if name.eq_ignore_ascii_case(wanted) {
                values.push(tag[value_start..value_end].to_owned());
            }
        }

        let name = tag
            .trim_start()
            .split(|ch: char| ch.is_whitespace() || ch == '/')
            .next()
            .unwrap_or_default();
        cursor = end + 1;
        if name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style") {
            let close = format!("</{name}");
            if let Some(relative) = html[cursor..].to_ascii_lowercase().find(&close) {
                cursor += relative;
            }
        }
    }
    values
}

#[test]
fn frozen_source_probe_cases_match_parser_acceptance() {
    let mut cases = Vec::new();
    let mut name = None;
    let mut expected = None;
    let mut source = String::new();
    for line in SOURCE_PROBES.lines() {
        if let Some(header) = line.strip_prefix("// CASE ") {
            if let (Some(name), Some(expected)) = (name.take(), expected.take()) {
                cases.push((name, expected, std::mem::take(&mut source)));
            }
            let mut fields = header.split_whitespace();
            name = fields.next();
            expected = fields.next().map(|value| value == "accept");
            assert!(fields.next().is_none(), "unexpected source fixture header: {header}");
        } else if name.is_some() {
            source.push_str(line);
            source.push('\n');
        }
    }
    if let (Some(name), Some(expected)) = (name, expected) {
        cases.push((name, expected, source));
    }

    assert_eq!(cases.len(), 14);
    let mut pinned_divergence = false;
    for (name, expected, source) in cases {
        if name == "unparenthesised-pipeline-lambda" {
            assert!(
                expected == false,
                "frozen reference source probe must continue to expect rejection"
            );
            assert!(
                parse_module(&source).is_ok(),
                "Rust parser behavior changed; re-evaluate the recorded ORNA-PIPE-005 divergence"
            );
            pinned_divergence = true;
            println!(
                "PINNED-DIVERGENCE {name}: frozen probe expects reject; Rust parser accepts"
            );
            continue;
        }
        assert_eq!(
            parse_module(&source).is_ok(),
            expected,
            "bounded reference source probe {name}"
        );
    }
    assert!(pinned_divergence);
}

#[test]
fn reference_project_modules_parse_as_module_units() {
    let modules = [
        include_str!("../../../../reference/Orna-1.0.0/examples/reference/main.orna"),
        include_str!("../../../../reference/Orna-1.0.0/examples/reference/library.orna"),
        include_str!("../../../../reference/Orna-1.0.0/examples/reference/sensors.orna"),
        include_str!("../../../../reference/Orna-1.0.0/examples/reference/values.orna"),
        include_str!("../../../../reference/Orna-1.0.0/examples/reference/warehouse.orna"),
    ];
    assert_eq!(modules.len(), 5);
    for (index, source) in modules.iter().enumerate() {
        assert!(parse_module(source).is_ok(), "reference module index {index}");
    }
}

#[test]
fn navigation_search_script_resolves_targets_and_matches_reference_queries() {
    let ids = html_attribute_values(INDEX_HTML, "id");
    let unique_ids: BTreeSet<_> = ids.iter().cloned().collect();
    assert_eq!(ids.len(), unique_ids.len(), "duplicate navigation anchors");
    assert!(unique_ids.contains("nav-search"));

    let scripts = INDEX_HTML
        .split_once("<script>")
        .expect("index search script opening tag")
        .1;
    let script = scripts
        .split_once("</script>")
        .expect("index search script closing tag")
        .0;
    let input = json!({"script": script, "ids": unique_ids});
    let mut child = Command::new("node")
        .args(["-e", NAVIGATION_HARNESS])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node is required to execute the frozen navigation model");
    serde_json::to_writer(child.stdin.as_mut().unwrap(), &input).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "navigation model failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["entries"], 1849);
    assert_eq!(result["all_targets_exist"], true);
    let checks = result["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 5);
    assert_eq!(checks[0]["hits"], 2);
    assert_eq!(checks[1]["hits"], 1);
    assert_eq!(checks[2]["hits"], 1);
    assert_eq!(checks[3]["hits"], 0);
    assert_eq!(checks[4]["hits"], 0);
}
