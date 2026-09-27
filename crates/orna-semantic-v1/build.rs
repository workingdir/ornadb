use std::{env, path::PathBuf};

fn main() {
    const FIXTURE: &str = "reference/Orna-1.0.0/examples/valid/historical-program.orna";

    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
    );
    let fixture = manifest_dir
        .ancestors()
        .map(|ancestor| ancestor.join(FIXTURE))
        .find(|candidate| candidate.is_file())
        .expect("could not find the authoritative Orna 1.0.0 historical program fixture");

    println!("cargo:rerun-if-changed={}", fixture.display());
    println!(
        "cargo:rustc-env=ORNA_HISTORICAL_PROGRAM_FIXTURE={}",
        fixture.display()
    );
}
