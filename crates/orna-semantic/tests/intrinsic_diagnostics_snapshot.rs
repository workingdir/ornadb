//! Whole-corpus diagnostic snapshot for the intrinsic typing reform.
//!
//! The snapshot in `intrinsic-diagnostics.snapshot` was captured from
//! `origin/main` before the checker started reading the `orna-intrinsics`
//! catalogue. Every `.orna` fixture under `crates/orna-semantic/tests/fixtures`
//! and `crates/orna-conformance` is analysed on its own, and each diagnostic is
//! recorded as `code`, `message` and its spans. The test requires the
//! post-reform output to be identical, except for the diagnostic changes the
//! owner accepted as bug fixes (decision D4).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orna_semantic::{Catalogue, ModuleInput, analyze_with_catalogue};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root is reachable from the semantic crate")
}

fn collect_orna_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{} is unreadable: {error}", directory.display()));
    for entry in entries {
        let entry = entry.expect("corpus entry is readable");
        let path = entry.path();
        let file_type = entry.file_type().expect("corpus entry has a file type");
        if file_type.is_dir() {
            collect_orna_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "orna")
        {
            files.push(path);
        }
    }
}

fn corpus_files() -> Vec<(String, String)> {
    let root = workspace_root();
    let mut paths = Vec::new();
    for corpus in [
        "crates/orna-semantic/tests/fixtures",
        "crates/orna-conformance",
    ] {
        collect_orna_files(&root.join(corpus), &mut paths);
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let logical = path
                .strip_prefix(&root)
                .expect("corpus file is inside the workspace")
                .to_string_lossy()
                .replace('\\', "/");
            let source =
                std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{logical}: {error}"));
            (logical, source)
        })
        .collect()
}

/// Renders one diagnostic as `code`, message and its exact normative encoding.
///
/// The binary encoding carries the spans as well, and unlike the JSON boundary
/// projection it is not redacted: the snapshot must compare the diagnostic the
/// checker produced, not a display projection of it.
fn render_diagnostic(diagnostic: &orna_foundation::Diagnostic) -> String {
    let encoded = diagnostic
        .encode_ovb()
        .expect("diagnostic encodes")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}\t{}\t{encoded}", diagnostic.code(), diagnostic.message())
}

fn diagnostics_snapshot() -> Vec<String> {
    let catalogue = Catalogue::authoritative_fixture();
    let mut lines = Vec::new();
    for (logical_path, source) in corpus_files() {
        let analysis = analyze_with_catalogue(
            &[ModuleInput::new(logical_path.clone(), source)],
            &catalogue,
        );
        let rendered = analysis
            .diagnostics
            .iter()
            .map(render_diagnostic)
            .collect::<BTreeSet<_>>();
        for diagnostic in rendered.iter() {
            lines.push(format!("{logical_path}\t{diagnostic}"));
        }
    }
    lines
}

const SNAPSHOT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/intrinsic-diagnostics.snapshot"
);

#[test]
fn whole_corpus_diagnostics_match_the_origin_main_snapshot() {
    let snapshot = diagnostics_snapshot();
    if std::env::var_os("ORNA_SNAPSHOT_UPDATE").is_some() {
        std::fs::write(SNAPSHOT_PATH, snapshot.join("\n") + "\n").expect("snapshot is writable");
        return;
    }
    let expected = std::fs::read_to_string(SNAPSHOT_PATH)
        .expect("the captured origin/main snapshot is present")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if snapshot == expected {
        return;
    }
    let expected = expected.into_iter().collect::<BTreeSet<_>>();
    let observed = snapshot.iter().cloned().collect::<BTreeSet<_>>();
    let added = observed.difference(&expected).cloned().collect::<Vec<_>>();
    let removed = expected.difference(&observed).cloned().collect::<Vec<_>>();
    panic!(
        "corpus diagnostics drifted from origin/main:\nadded ({}):\n{}\nremoved ({}):\n{}",
        added.len(),
        added.join("\n"),
        removed.len(),
        removed.join("\n"),
    );
}
