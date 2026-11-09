//! Writes or checks the deterministic `std_intrinsics.json` artifact.

use std::{env, fs, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let artifact = manifest.join("std_intrinsics.json");
    let rendered = orna_intrinsics::export::json();
    let check = env::args().skip(1).any(|argument| argument == "--check");

    if check {
        let current = fs::read_to_string(&artifact).unwrap_or_default();
        if current == rendered {
            return ExitCode::SUCCESS;
        }
        eprintln!(
            "{} is stale; run `cargo run -p orna-intrinsics --bin std-api-export`",
            artifact.display()
        );
        return ExitCode::FAILURE;
    }

    fs::write(&artifact, rendered).unwrap_or_else(|error| {
        panic!("write {}: {error}", artifact.display());
    });
    ExitCode::SUCCESS
}
