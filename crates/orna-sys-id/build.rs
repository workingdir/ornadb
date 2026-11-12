//! Generates the `orna-sys-id` identity tables from the hand-written `sys` declarations.
//!
//! `orna-sys-interface` decodes `crates/orna-hir/interface/sys.orna` and renders the catalogue document in memory, so no
//! catalogue JSON is read here. The build fails if the declarations do not decode, if any catalogue count disagrees
//! with its entries, or if the engine inventory loses or duplicates a name.

#[path = "build/generator.rs"]
mod generator;
#[path = "build/sha256.rs"]
mod sha256;

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=catalogue/engine-1.1.0.json");
    println!("cargo:rerun-if-changed=build/generator.rs");
    println!("cargo:rerun-if-changed=build/sha256.rs");
    println!("cargo:rerun-if-changed=../orna-hir/interface/sys.orna");
    println!("cargo:rerun-if-changed=../orna-sys-interface/src");

    let declarations = orna_sys_interface::native_raw_document()
        .expect("crates/orna-hir/interface/sys.orna decodes into the sys descriptor");
    let sys_json = serde_json::to_string(&orna_sys_interface::catalogue_json(&declarations))
        .expect("the rendered catalogue serialises");
    let engine_json = fs::read_to_string("catalogue/engine-1.1.0.json")
        .expect("catalogue/engine-1.1.0.json is vendored in the crate");
    let generated = generator::generate(&sys_json, &engine_json)
        .unwrap_or_else(|error| panic!("sys identity generation failed: {error}"));
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    for (name, contents) in generated.files() {
        fs::write(output.join(name), contents)
            .unwrap_or_else(|error| panic!("writing generated {name} failed: {error}"));
    }
}
