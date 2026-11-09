//! `VersionedId` round-trips every `name@version` literal in the tree.
//!
//! The ABI namespace is open and versioned, so the acceptance property is not a fixed
//! set of names: it is that every identifier the repository already writes parses to
//! the same bytes, and that malformed values are refused at construction.

use orna_sys_id::{AbiVersion, IdError, Namespace, VersionedId};
use std::{collections::BTreeSet, fs, path::Path, path::PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn source_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("reading {} failed: {error}", directory.display()));
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Returns every ordinary Rust string literal with escapes skipped.
fn string_literals(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        let start = index + 1;
        index = start;
        let mut escaped = false;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' if !escaped => {
                    escaped = true;
                    index += 1;
                }
                b'"' if !escaped => break,
                _ => {
                    escaped = false;
                    index += 1;
                }
            }
        }
        if index < bytes.len() {
            literals.push(&source[start..index]);
            index += 1;
        }
    }
    literals
}

fn is_segment(segment: &str) -> bool {
    let mut characters = segment.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn is_version(version: &str) -> bool {
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next();
    parts.next().is_none() && numeric(major) && minor.is_none_or(numeric)
}

/// Whether a literal has exactly the `namespace.path@major[.minor]` shape the ABI uses.
fn looks_versioned(text: &str) -> bool {
    let mut halves = text.split('@');
    let (Some(name), Some(version), None) = (halves.next(), halves.next(), halves.next()) else {
        return false;
    };
    if !is_version(version) {
        return false;
    }
    let mut segments = name.split('.');
    let Some(namespace) = segments.next() else {
        return false;
    };
    is_segment(namespace) && name.contains('.') && segments.all(is_segment)
}

#[test]
fn every_versioned_literal_in_the_tree_round_trips() {
    let mut files = Vec::new();
    source_files(&workspace_root().join("crates"), &mut files);
    assert!(files.len() > 100, "the tree scan must find the source tree");

    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut unknown_namespaces: BTreeSet<String> = BTreeSet::new();
    let mut inspected = 0usize;
    for file in files {
        let source = fs::read_to_string(&file).expect("Rust sources are UTF-8");
        for literal in string_literals(&source) {
            if !looks_versioned(literal) {
                continue;
            }
            inspected += 1;
            let (name, _) = literal.split_once('@').expect("shape was checked");
            let prefix = name.split('.').next().expect("name has a dot");
            if Namespace::parse(prefix).is_some() {
                let identifier: VersionedId = literal.parse().unwrap_or_else(|error| {
                    panic!("`{literal}` in {} must parse: {error}", file.display())
                });
                assert_eq!(identifier.to_string(), literal);
                assert_eq!(identifier.namespace().as_str(), prefix);
                known.insert(literal.to_owned());
            } else {
                // These are synthetic representation strings inside `#[cfg(test)]`
                // modules, not ABI identifiers; the timeout construction refuses them.
                unknown_namespaces.insert(prefix.to_owned());
                assert!(
                    matches!(
                        literal.parse::<VersionedId>(),
                        Err(IdError::UnknownNamespace { .. })
                    ),
                    "`{literal}` uses an unknown namespace and must be refused"
                );
            }
        }
    }
    assert!(
        inspected > 40,
        "expected the tree's versioned identifiers, inspected {inspected}"
    );
    assert!(
        unknown_namespaces
            .iter()
            .all(|prefix| matches!(prefix.as_str(), "codec" | "crm" | "kernel")),
        "unexpected unknown namespaces: {unknown_namespaces:?}"
    );
    for expected in [
        "host.std.io.fs.read@1.0",
        "langitem.sys.explain@1.0",
        "orna.kernel.value.boolean@1",
        "orna.sys.source.function@1",
        "std.token@1",
    ] {
        assert!(known.contains(expected), "`{expected}` was not found");
    }
}

#[test]
fn both_version_spellings_round_trip_byte_for_byte() {
    let major_only: VersionedId = "orna.kernel.value.boolean@1".parse().unwrap();
    assert_eq!(major_only.namespace(), Namespace::Orna);
    assert_eq!(major_only.path(), "kernel.value.boolean");
    assert_eq!(major_only.version(), AbiVersion::new(1, None));
    assert_eq!(major_only.to_string(), "orna.kernel.value.boolean@1");

    let minor: VersionedId = "host.std.io.fs.read@1.0".parse().unwrap();
    assert_eq!(minor.namespace(), Namespace::Host);
    assert_eq!(minor.path(), "std.io.fs.read");
    assert_eq!(minor.version(), AbiVersion::new(1, Some(0)));
    assert_eq!(minor.to_string(), "host.std.io.fs.read@1.0");

    assert!(minor.same_role(&"host.std.io.fs.read@2.0".parse().unwrap()));
    assert!(!minor.same_role(&"host.std.io.fs.write@1.0".parse().unwrap()));

    let constructed = VersionedId::new(
        Namespace::Vendor,
        "example.widget",
        AbiVersion::new(3, None),
    )
    .unwrap();
    assert_eq!(constructed.to_string(), "vendor.example.widget@3");
}

#[test]
fn malformed_identifiers_are_refused() {
    assert_eq!(
        "host.std.io.fs.read".parse::<VersionedId>(),
        Err(IdError::MissingVersion)
    );
    assert_eq!("host".parse::<VersionedId>(), Err(IdError::MissingVersion));
    assert_eq!("host@1.0".parse::<VersionedId>(), Err(IdError::MissingPath));
    assert!(matches!(
        "hostish.std@x".parse::<VersionedId>(),
        Err(IdError::UnknownNamespace { text }) if text == "hostish"
    ));
    assert_eq!("host.@x".parse::<VersionedId>(), Err(IdError::EmptyPath));
    assert_eq!(
        "host.Std.io@x".parse::<VersionedId>(),
        Err(IdError::InvalidPathSegment {
            segment: "Std".into()
        })
    );
    assert_eq!(
        format!("host.std.io@{}", "1.x").parse::<VersionedId>(),
        Err(IdError::InvalidVersion { text: "1.x".into() })
    );
    assert_eq!(
        format!("host.std.io{}", "@1@2").parse::<VersionedId>(),
        Err(IdError::InvalidVersion { text: "1@2".into() })
    );
    assert_eq!(
        format!("host.std.io@{}", "01").parse::<VersionedId>(),
        Err(IdError::InvalidVersion { text: "01".into() })
    );
    assert_eq!(
        format!("host.std.io@{}", "65536.0").parse::<VersionedId>(),
        Err(IdError::VersionOverflow {
            text: "65536.0".into()
        })
    );
    assert_eq!(
        VersionedId::new(Namespace::Host, "std..io", AbiVersion::new(1, None)),
        Err(IdError::InvalidPathSegment { segment: "".into() })
    );
    assert_eq!(
        VersionedId::new(Namespace::Host, "", AbiVersion::new(1, None)),
        Err(IdError::EmptyPath)
    );
}
