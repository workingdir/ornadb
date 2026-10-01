use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod build_support;

use build_support::{Collector, canonical_pretty_json, validate_api_document, validate_collection};
use serde_json::{Value, json};

fn rust_sources(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
    Ok(())
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("orna-sys-v1 must be two levels below the workspace root");
    let source_root = manifest.join("src");
    let api_path = workspace.join("api/sys.json");
    let schema_path = workspace.join("api/sys.schema.json");
    let build_support_path = manifest.join("build_support.rs");
    println!("cargo:rerun-if-changed={}", api_path.display());
    println!("cargo:rerun-if-changed={}", schema_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", build_support_path.display());
    // Watch the directory recursively so adding a new annotated module also
    // invalidates the collected schema, even before that file is known here.
    println!("cargo:rerun-if-changed={}", source_root.display());

    // `syn` scans written Rust source and cannot see items emitted later by
    // `macro_rules!` or procedural-macro expansion. Annotated methods in impls
    // and trait declarations are collected; methods emitted by macros remain
    // unsupported and are documented at the source declaration.
    let mut sources = Vec::new();
    rust_sources(&source_root, &mut sources).expect("walk sys crate Rust source");
    let mut collector = Collector::default();
    for source in &sources {
        println!("cargo:rerun-if-changed={}", source.display());
        let text = fs::read_to_string(source)
            .unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
        collector.collect_source(&source.display().to_string(), &text);
    }
    assert!(collector.errors.is_empty(), "{}", collector.errors.join("\n"));
    assert!(
        !collector.functions.is_empty(),
        "no #[ornasys] trait or implementation methods were collected"
    );
    validate_collection(&collector.functions)
        .expect("annotated system API collection must be unique and internally consistent");

    let schema_text = fs::read_to_string(&schema_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", schema_path.display()));
    let schema: Value = serde_json::from_str(&schema_text).expect("valid published JSON Schema");
    assert_eq!(
        schema["$schema"].as_str(),
        Some("https://json-schema.org/draft/2020-12/schema"),
        "published system API schema must use JSON Schema 2020-12"
    );
    assert_eq!(schema["type"].as_str(), Some("object"));

    // The normative API artifact owns the non-function type/relation graph;
    // function declarations are replaced by the build-time method collection.
    let base_text = fs::read_to_string(&api_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", api_path.display()));
    let mut api: Value = serde_json::from_str(&base_text).expect("valid normative system API JSON");
    let functions = collector
        .functions
        .iter()
        .map(|function| function.metadata.clone())
        .collect::<Vec<_>>();
    api["functions"] = Value::Array(functions);
    api["counts"]["functions"] = json!(collector.functions.len());
    api["source_of_truth"] = json!(
        "crates/orna-sys-v1/src/system_api.rs #[ornasys] descriptor methods; normative semantics in source chapters and generated system reference"
    );

    validate_api_document(&api).expect("generated system API schema is internally consistent");

    let mut generated_json = canonical_pretty_json(&api).expect("serialize system API canonically");
    generated_json.push('\n');

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    fs::write(out_dir.join("api_sys.json"), generated_json).expect("write generated api/sys.json");
}
