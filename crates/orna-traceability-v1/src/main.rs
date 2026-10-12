use std::{env, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let root = env::args_os()
        .nth(1)
        .or_else(|| env::var_os("ORNA_REFERENCE_DIR"))
        .map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference"),
            PathBuf::from,
        );
    match orna_traceability_v1::generate(&root) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(_) => ExitCode::FAILURE,
        },
        Err(error) => {
            eprintln!("traceability generation failed: {error}");
            ExitCode::FAILURE
        }
    }
}
