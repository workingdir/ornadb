use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod build_support;
mod build_host;

use build_support::{
    collect_rust_sources, generate_sys_artifacts,
};

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
    let source_root = manifest.join("src");
    let build_support_path = manifest.join("build_support.rs");
    let build_host_path = manifest.join("build_host.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", build_support_path.display());
    println!("cargo:rerun-if-changed={}", build_host_path.display());
    // Watch the directory recursively so adding a new annotated module also
    // invalidates the collected schema, even before that file is known here.
    println!("cargo:rerun-if-changed={}", source_root.display());

    // `syn` scans written Rust source and cannot see items emitted later by
    // `macro_rules!` or procedural-macro expansion. Annotated methods in impls
    // and trait declarations are collected; methods emitted by macros remain
    // unsupported and are documented at the source declaration.
    let mut sources = Vec::new();
    rust_sources(&source_root, &mut sources).expect("walk sys crate Rust source");
    for source in &sources {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    let collector = collect_rust_sources(&source_root)
        .expect("annotated system API collection must be unique and internally consistent");
    for asset in &collector.registry_assets {
        println!("cargo:rerun-if-changed={}", asset.display());
    }

    let registry = collector
        .type_graph
        .clone()
        .expect("annotated sys registry must attach the type graph");
    let schema = collector
        .schema
        .clone()
        .expect("annotated sys registry must attach its schema contract");
    // One deterministic projection generates every baked sys artifact. The
    // focused parity test reruns this exact path against the compiled outputs.
    let artifacts = generate_sys_artifacts(&collector.functions, registry, &schema)
        .expect("registry-generated sys artifacts must be internally consistent");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    fs::write(out_dir.join("api_sys.json"), artifacts.api_json)
        .expect("write generated api/sys.json");
    let host_registry = build_host::generate_host_registry(&source_root)
        .expect("annotated native sys host operations form a valid registry");
    fs::write(out_dir.join("system_host_operations.json"), host_registry)
        .expect("write generated native sys host-operation registry");
    fs::write(
        out_dir.join("system_api_schema.json"),
        artifacts.schema_json,
    )
    .expect("write generated embedded system API schema");
    fs::write(
        out_dir.join("system_provider_abi.json"),
        artifacts.provider_abi_json,
    )
        .expect("write generated typed system provider ABI");
    let binding_root = out_dir.join("system_bindings");
    if binding_root.exists() {
        fs::remove_dir_all(&binding_root).expect("remove stale generated sys binding stubs");
    }
    for (relative_path, source) in artifacts.binding_modules {
        let path = binding_root.join(relative_path);
        fs::create_dir_all(path.parent().expect("generated module has a parent"))
            .expect("create generated sys module directory");
        fs::write(path, source).expect("write generated Orna sys module stub");
    }
    fs::write(out_dir.join("system_bindings.orna"), artifacts.binding_bundle)
        .expect("write generated Orna sys binding stubs");
}
