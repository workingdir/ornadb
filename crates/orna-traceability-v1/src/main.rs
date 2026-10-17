use std::{env, fs, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    if args.next().is_some_and(|arg| arg == "cited") { return cited(args.collect()); }
    let root = env::args_os().nth(1).or_else(|| env::var_os("ORNA_REFERENCE_DIR")).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference"), PathBuf::from,
    );
    match orna_traceability_v1::generate(&root) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => { println!("{json}"); ExitCode::SUCCESS }
            Err(_) => ExitCode::FAILURE,
        },
        Err(error) => { eprintln!("traceability generation failed: {error}"); ExitCode::FAILURE }
    }
}

fn cited(args: Vec<std::ffi::OsString>) -> ExitCode {
    let mut baseline_path = None;
    let mut cited_output = None;
    let mut locations_output = None;
    let mut crates_root = PathBuf::from(".");
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--baseline") => baseline_path = args.next().map(PathBuf::from),
            Some("--cited-output") => cited_output = args.next().map(PathBuf::from),
            Some("--locations-output") => locations_output = args.next().map(PathBuf::from),
            Some("--crates-root") => {
                let Some(root) = args.next() else { return usage("missing value for --crates-root") };
                crates_root = PathBuf::from(root);
            }
            Some("--help" | "-h") => { print_help(); return ExitCode::SUCCESS; }
            Some(option) => return usage(&format!("unknown option {option}")),
            None => return usage("arguments must be valid UTF-8"),
        }
    }
    let Some(baseline_path) = baseline_path else { return usage("missing required --baseline <file>") };
    let catalogue = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference/tests/requirements.json");
    let report = match orna_traceability_v1::citation_ratchet::cite_requirements(&catalogue, &crates_root.join("crates")) {
        Ok(report) => report,
        Err(error) => { eprintln!("citation count failed: {error}"); return ExitCode::FAILURE; }
    };
    if let Some(path) = cited_output {
        if let Err(error) = write_json(&path, &report.cited_ids) { eprintln!("cannot write cited IDs to {}: {error}", path.display()); return ExitCode::FAILURE; }
    }
    if let Some(path) = locations_output {
        if let Err(error) = write_json(&path, &report.locations) { eprintln!("cannot write citation locations to {}: {error}", path.display()); return ExitCode::FAILURE; }
    }
    for (chapter, counts) in &report.chapters { println!("{chapter}: cited={}, uncited={}", counts.cited, counts.uncited); }
    println!("total: cited={}, uncited={}", report.cited_ids.len(), report.uncited_ids.len());
    println!("cited IDs (deterministic JSON):");
    println!("{}", serde_json::to_string_pretty(&report.cited_ids).expect("BTreeSet serializes"));
    let baseline: orna_traceability_v1::citation_ratchet::CitationBaseline = match fs::read(&baseline_path)
        .map_err(|e| format!("read {}: {e}", baseline_path.display()))
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", baseline_path.display()))) {
            Ok(value) => value,
            Err(error) => { eprintln!("citation baseline failed: {error}"); return ExitCode::FAILURE; }
        };
    match orna_traceability_v1::citation_ratchet::check_ratchet(&report, &baseline) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            if !error.dropped_ids.is_empty() { eprintln!("dropped: {}", error.dropped_ids.into_iter().collect::<Vec<_>>().join(", ")); }
            ExitCode::FAILURE
        }
    }
}

fn write_json(path: &std::path::Path, value: &impl serde::Serialize) -> Result<(), std::io::Error> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("JSON serializing cited IDs cannot fail");
    bytes.push(b'\n');
    fs::write(path, bytes)
}

fn usage(message: &str) -> ExitCode { eprintln!("{message}"); print_help(); ExitCode::FAILURE }
fn print_help() {
    eprintln!("Usage: orna-traceability-v1 cited --baseline <file> [--crates-root <workspace>] [--cited-output <file>] [--locations-output <file>]\nCounts requirement-ID citations in Rust test code; this counts citations, not proof of conformance. The baseline stores max_uncited and previously cited_ids. The optional locations output maps each cited ID to files and lines.");
}
