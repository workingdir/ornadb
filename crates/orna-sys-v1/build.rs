use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod build_support;

use build_support::{Collector, validate_api_document, validate_collection};
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

fn descriptor_constant(method: &str) -> String {
    format!("{}_DESCRIPTOR", method.to_ascii_uppercase())
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let source_root = manifest.join("src");
    let base_path = manifest.join("src/system_api_base.json");
    println!("cargo:rerun-if-changed={}", base_path.display());
    println!("cargo:rerun-if-changed=build.rs");

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

    let base_text = fs::read_to_string(&base_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", base_path.display()));
    let mut api: Value = serde_json::from_str(&base_text).expect("valid system API base JSON");
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

    let mut generated_json = serde_json::to_string_pretty(&api).expect("serialize system API");
    generated_json.push('\n');

    let mut generated_rust = String::new();
    for function in &collector.functions {
        let metadata = &function.metadata;
        let constant = descriptor_constant(&function.method);
        let effect = match metadata["effect"].as_str().expect("validated effect") {
            "read" => "Read",
            "invoke" => "Invoke",
            "admin" => "Admin",
            _ => unreachable!("effect was validated"),
        };
        generated_rust.push_str(&format!(
            "pub const {constant}: SystemFunctionDescriptor = SystemFunctionDescriptor {{\n\
             name: {name:?}, effect: SystemEffect::{effect}, signature: {signature:?}, purpose: {purpose:?},\n\
             }};\n",
            name = metadata["name"].as_str().expect("validated name"),
            signature = metadata["signature"].as_str().expect("validated signature"),
            purpose = metadata["purpose"].as_str().expect("validated purpose"),
        ));
    }
    generated_rust.push_str("\npub static SYSTEM_FUNCTION_DESCRIPTORS: &[SystemFunctionDescriptor] = &[\n");
    for function in &collector.functions {
        generated_rust.push_str(&format!("    {},\n", descriptor_constant(&function.method)));
    }
    generated_rust.push_str("];\n");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    fs::write(out_dir.join("api_sys.json"), generated_json).expect("write generated api/sys.json");
    fs::write(out_dir.join("system_api_catalog.rs"), generated_rust)
        .expect("write generated Rust descriptor catalog");
}
