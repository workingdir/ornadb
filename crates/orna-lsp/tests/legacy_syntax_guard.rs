//! CI guard for ORNA-LEX-007 (ornadb-vixo3). Editor tooling and every other
//! workspace crate must reach the lexer through `orna-syntax-v1`. The pre-1.0.0
//! `orna-syntax` package and its `orna_syntax` module are rejected in manifests,
//! Rust sources and editor integrations. Fixture directories are data, not
//! tooling, so they are skipped by the workspace scan and checked directly.

use std::{
    fs,
    path::{Path, PathBuf},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-guard");

/// True when a manifest line declares the pre-1.0.0 `orna-syntax` package.
/// `orna-syntax-v1` is a different package and is accepted.
fn is_legacy_manifest_line(line: &str) -> bool {
    let line = line.trim_start();
    if line.starts_with("[dependencies.orna-syntax]")
        || line.starts_with("[dev-dependencies.orna-syntax]")
        || line.starts_with("[build-dependencies.orna-syntax]")
    {
        return true;
    }
    if line.contains("package = \"orna-syntax\"") {
        return true;
    }
    match line.strip_prefix("orna-syntax") {
        Some(rest) => rest.trim_start().starts_with('='),
        None => false,
    }
}

/// True when source text names the pre-1.0.0 `orna_syntax` module as a path
/// (`orna_syntax::`), a `use` target or an `extern crate`. `orna_syntax_v1`
/// does not match because the identifier continues past `orna_syntax`.
fn is_legacy_source_line(line: &str) -> bool {
    line.match_indices("orna_syntax").any(|(index, matched)| {
        let before = line[..index].chars().next_back();
        let after = line[index + matched.len()..].chars().next();
        let starts_identifier = before.is_some_and(is_identifier_char);
        !starts_identifier && matches!(after, Some(':') | Some(';'))
    })
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Returns every `(line number, text)` in `text` that a legacy matcher rejects.
fn legacy_hits(text: &str, manifest: bool) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            if manifest {
                is_legacy_manifest_line(line)
            } else {
                is_legacy_source_line(line)
            }
        })
        .map(|(index, line)| (index + 1, line.trim().to_owned()))
        .collect()
}

/// Files the workspace guard scans: manifests, Rust sources and editor
/// integrations. Fixture and build-output directories are skipped.
fn scanned_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        // Symbolic links can point back into the tree and are not scanned.
        if path.is_symlink() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(name.as_str(), "target" | "fixtures" | ".git") {
                continue;
            }
            scanned_files(&path, files);
        } else if name == "legacy_syntax_guard.rs" {
            // This file names the legacy patterns it rejects.
            continue;
        } else if name == "Cargo.toml" || path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        } else if path
            .components()
            .any(|component| component.as_os_str() == "editors")
        {
            files.push(path);
        }
    }
}

#[test]
fn guard_flags_legacy_manifest_and_source_fixtures_and_accepts_v1() {
    let fixtures = Path::new(FIXTURES);
    let read = |name: &str| fs::read_to_string(fixtures.join(name)).unwrap();

    assert_eq!(legacy_hits(&read("legacy-manifest.toml"), true).len(), 1);
    assert!(legacy_hits(&read("current-manifest.toml"), true).is_empty());
    assert_eq!(legacy_hits(&read("legacy-source.rs"), false).len(), 1);
    assert!(legacy_hits(&read("current-source.rs"), false).is_empty());
}

#[test]
fn workspace_has_no_pre_1_0_0_orna_syntax_references() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    scanned_files(&root, &mut files);
    assert!(
        files.len() > 100,
        "the guard must scan the whole workspace, found {} files",
        files.len()
    );

    let mut violations = Vec::new();
    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let manifest = path.file_name().is_some_and(|name| name == "Cargo.toml");
        for (line, text) in legacy_hits(&text, manifest) {
            let relative = path.strip_prefix(&root).unwrap_or(path);
            violations.push(format!("{}:{line}: {text}", relative.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "pre-1.0.0 orna-syntax references must use orna-syntax-v1 (ORNA-LEX-007):\n{}",
        violations.join("\n")
    );
}
