//! Generates the `orna-sys-id` identity tables from the vendored catalogues.
//!
//! The build fails if the portable catalogue bytes differ from the reviewed
//! `api/sys.json`, if any catalogue count disagrees with its entries, or if the
//! engine inventory loses or duplicates a name. Generation is therefore a check on
//! the spec contract, not a formatting step.

#[path = "build/generator.rs"]
mod generator;
#[path = "build/sha256.rs"]
mod sha256;

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=catalogue/sys-1.1.0.json");
    println!("cargo:rerun-if-changed=catalogue/engine-1.1.0.json");
    println!("cargo:rerun-if-changed=build/generator.rs");
    println!("cargo:rerun-if-changed=build/sha256.rs");

    let sys_json = fs::read_to_string("catalogue/sys-1.1.0.json")
        .expect("catalogue/sys-1.1.0.json is vendored in the crate");
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
