//! The project's native-library manifest, `orna.toml` at the project root
//! (docs/design/native-libraries.md, section 3).
//!
//! The file name and format are the design's placeholders. Unknown keys are
//! rejected at every level, so a typo in a digest or a path cannot silently
//! drop a library from the link.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::fs;
use std::io::ErrorKind;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use orna_base::hex_decode;
use toml_edit::{Document, Item, Table};

use crate::error::{LinkError, LinkErrorCode};

/// The manifest file name at the project root.
pub const MANIFEST_FILE: &str = "orna.toml";

/// The target triple of the host this build runs on: the `[native_library.target.<triple>]`
/// table the engine selects for its own build. A host with no triple in this table names
/// no entry, so every library native on it fails to link with a target error.
#[must_use]
pub fn host_target() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        _ => "unsupported-host",
    }
}

/// The engine-embedded modules. Their natives bind from the engine registry, so
/// a library may not list them.
const ENGINE_MODULES: [&str; 2] = ["core", "sys"];

const TOP_LEVEL_KEYS: [&str; 1] = ["native_library"];
const LIBRARY_KEYS: [&str; 4] = ["name", "modules", "abi", "target"];
const TARGET_KEYS: [&str; 2] = ["path", "sha256"];

/// The parsed `[[native_library]]` entries, in file order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectManifest {
    /// Every library the root project names.
    pub libraries: Vec<NativeLibrary>,
}

/// One `[[native_library]]` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeLibrary {
    /// The library's name, unique in the manifest.
    pub name: String,
    /// Dotted module paths whose native declarations this library serves.
    pub modules: Vec<String>,
    /// The library ABI the library was built against.
    pub abi: u32,
    /// The per-target file entries, keyed by target triple.
    pub targets: BTreeMap<String, LibraryTarget>,
}

/// One `[native_library.target.<triple>]` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryTarget {
    /// The file, relative to the project root.
    pub path: PathBuf,
    /// The SHA-256 of the file's bytes exactly as stored.
    pub sha256: [u8; 32],
}

impl ProjectManifest {
    /// Reads `<root>/orna.toml`. A missing file is an empty manifest: a project
    /// with no library natives has nothing to name.
    ///
    /// # Errors
    ///
    /// Returns every manifest problem found, or the read failure.
    pub fn load(root: &Path) -> Result<Self, Vec<LinkError>> {
        let path = root.join(MANIFEST_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(vec![LinkError::new(
                LinkErrorCode::Manifest,
                format!("`{}` cannot be read ({error})", path.display()),
            )]),
        }
    }

    /// Parses and validates the manifest text.
    ///
    /// # Errors
    ///
    /// Returns every problem found, each naming its location in the file.
    pub fn parse(text: &str) -> Result<Self, Vec<LinkError>> {
        let document = Document::parse(text).map_err(|error| {
            vec![located(
                LinkErrorCode::Manifest,
                text,
                error.span(),
                format!("invalid TOML: {}", error.message()),
            )]
        })?;
        let mut errors = Vec::new();
        reject_unknown_keys(text, document.as_table(), &TOP_LEVEL_KEYS, &mut errors);
        let mut libraries = Vec::new();
        let mut names = BTreeSet::new();
        if let Some(item) = document.get("native_library") {
            match item.as_array_of_tables() {
                Some(tables) => {
                    for table in tables.iter() {
                        let Some(library) = parse_library(text, table, &mut errors) else {
                            continue;
                        };
                        if !names.insert(library.name.clone()) {
                            errors.push(
                                located(
                                    LinkErrorCode::Manifest,
                                    text,
                                    table.span(),
                                    format!("library name `{}` is listed twice", library.name),
                                )
                                .with_library(&library.name),
                            );
                        }
                        libraries.push(library);
                    }
                }
                None => errors.push(located(
                    LinkErrorCode::Manifest,
                    text,
                    item.span(),
                    "`native_library` must be an array of tables, written `[[native_library]]`"
                        .to_owned(),
                )),
            }
        }
        if errors.is_empty() {
            Ok(Self { libraries })
        } else {
            Err(errors)
        }
    }
}

fn parse_library(text: &str, table: &Table, errors: &mut Vec<LinkError>) -> Option<NativeLibrary> {
    reject_unknown_keys(text, table, &LIBRARY_KEYS, errors);
    let name_item = required(text, table, "name", errors)?;
    let name = string_value(text, name_item, "name", errors)?;
    if !valid_name(name) {
        errors.push(located(
            LinkErrorCode::Manifest,
            text,
            name_item.span(),
            format!("library name `{name}` must be non-empty ASCII letters, digits, `_` or `-`"),
        ));
        return None;
    }
    let library = name.to_owned();
    let modules = parse_modules(text, table, &library, errors);
    let abi = parse_abi(text, table, &library, errors);
    let targets = parse_targets(text, table, &library, errors);
    Some(NativeLibrary {
        name: library,
        modules: modules?,
        abi: abi?,
        targets,
    })
}

fn parse_modules(
    text: &str,
    table: &Table,
    library: &str,
    errors: &mut Vec<LinkError>,
) -> Option<Vec<String>> {
    let item = required(text, table, "modules", errors)?;
    let Some(array) = item.as_array() else {
        errors.push(
            located(
                LinkErrorCode::Manifest,
                text,
                item.span(),
                "`modules` must be an array of dotted module paths".to_owned(),
            )
            .with_library(library),
        );
        return None;
    };
    let mut modules = Vec::new();
    let mut seen = BTreeSet::new();
    let mut valid = true;
    for value in array.iter() {
        let Some(path) = value.as_str() else {
            errors.push(
                located(
                    LinkErrorCode::Manifest,
                    text,
                    value.span(),
                    "each `modules` entry must be a string".to_owned(),
                )
                .with_library(library),
            );
            valid = false;
            continue;
        };
        if !valid_module(path) {
            errors.push(
                located(
                    LinkErrorCode::Manifest,
                    text,
                    value.span(),
                    format!("module `{path}` must be a dotted path of ASCII identifiers"),
                )
                .with_library(library),
            );
            valid = false;
        } else if ENGINE_MODULES.contains(&path.split('.').next().unwrap_or_default()) {
            errors.push(located(
                LinkErrorCode::Manifest,
                text,
                value.span(),
                format!("module `{path}` is engine-embedded; its natives bind from the registry, not a library"),
            ).with_library(library));
            valid = false;
        } else if !seen.insert(path) {
            errors.push(
                located(
                    LinkErrorCode::Manifest,
                    text,
                    value.span(),
                    format!("module `{path}` is listed twice"),
                )
                .with_library(library),
            );
            valid = false;
        } else {
            modules.push(path.to_owned());
        }
    }
    valid.then_some(modules)
}

fn parse_abi(text: &str, table: &Table, library: &str, errors: &mut Vec<LinkError>) -> Option<u32> {
    let item = required(text, table, "abi", errors)?;
    let abi = item
        .as_integer()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number >= 1);
    if abi.is_none() {
        errors.push(
            located(
                LinkErrorCode::Manifest,
                text,
                item.span(),
                "`abi` must be a positive integer".to_owned(),
            )
            .with_library(library),
        );
    }
    abi
}

fn parse_targets(
    text: &str,
    table: &Table,
    library: &str,
    errors: &mut Vec<LinkError>,
) -> BTreeMap<String, LibraryTarget> {
    let mut targets = BTreeMap::new();
    let Some(item) = table.get("target") else {
        return targets;
    };
    let Some(entries) = item.as_table() else {
        errors.push(
            located(
                LinkErrorCode::Manifest,
                text,
                item.span(),
                "`target` must be a table keyed by target triple".to_owned(),
            )
            .with_library(library),
        );
        return targets;
    };
    for (triple, entry) in entries.iter() {
        let Some(entry_table) = entry.as_table() else {
            errors.push(
                located(
                    LinkErrorCode::Manifest,
                    text,
                    entry.span(),
                    format!("target `{triple}` must be a table with `path` and `sha256`"),
                )
                .with_library(library),
            );
            continue;
        };
        if let Some(target) = parse_target(text, library, triple, entry_table, errors) {
            targets.insert(triple.to_owned(), target);
        }
    }
    targets
}

fn parse_target(
    text: &str,
    library: &str,
    triple: &str,
    table: &Table,
    errors: &mut Vec<LinkError>,
) -> Option<LibraryTarget> {
    reject_unknown_keys(text, table, &TARGET_KEYS, errors);
    let path_item = required(text, table, "path", errors)?;
    let path_text = string_value(text, path_item, "path", errors)?;
    let path = match validate_library_path(path_text) {
        Ok(path) => path,
        Err(reason) => {
            errors.push(
                located(
                    LinkErrorCode::Library,
                    text,
                    path_item.span(),
                    format!("target `{triple}` path `{path_text}` {reason}"),
                )
                .with_library(library),
            );
            return None;
        }
    };
    let digest_item = required(text, table, "sha256", errors)?;
    let digest_text = string_value(text, digest_item, "sha256", errors)?;
    match hex_decode(digest_text)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
    {
        Some(sha256) => Some(LibraryTarget { path, sha256 }),
        None => {
            errors.push(
                located(
                    LinkErrorCode::Manifest,
                    text,
                    digest_item.span(),
                    format!("target `{triple}` sha256 must be 64 lowercase hexadecimal digits"),
                )
                .with_library(library),
            );
            None
        }
    }
}

/// Checks a library path against the design's rules (section 3): relative to the
/// project root, no absolute prefix, and no `..` component. Any `..` is rejected,
/// even one a normalisation would resolve, to keep the manifest unambiguous.
fn validate_library_path(relative: &str) -> Result<PathBuf, &'static str> {
    let mut clean = PathBuf::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err("contains a `..` component"),
            Component::RootDir | Component::Prefix(_) => return Err("is absolute"),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("names no file");
    }
    Ok(clean)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_module(path: &str) -> bool {
    path.split('.').all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn required<'a>(
    text: &str,
    table: &'a Table,
    key: &str,
    errors: &mut Vec<LinkError>,
) -> Option<&'a Item> {
    let item = table.get(key);
    if item.is_none() {
        errors.push(located(
            LinkErrorCode::Manifest,
            text,
            table.span(),
            format!("missing required key `{key}`"),
        ));
    }
    item
}

fn string_value<'a>(
    text: &str,
    item: &'a Item,
    key: &str,
    errors: &mut Vec<LinkError>,
) -> Option<&'a str> {
    let value = item.as_str();
    if value.is_none() {
        errors.push(located(
            LinkErrorCode::Manifest,
            text,
            item.span(),
            format!("`{key}` must be a string"),
        ));
    }
    value
}

fn reject_unknown_keys(text: &str, table: &Table, allowed: &[&str], errors: &mut Vec<LinkError>) {
    for (key, item) in table.iter() {
        if !allowed.contains(&key) {
            errors.push(located(
                LinkErrorCode::Manifest,
                text,
                item.span(),
                format!("unknown key `{key}`"),
            ));
        }
    }
}

/// A manifest error naming its line and column in `orna.toml`.
fn located(
    code: LinkErrorCode,
    text: &str,
    span: Option<Range<usize>>,
    detail: impl Display,
) -> LinkError {
    let location = match span {
        Some(range) => {
            let before = text.get(..range.start).unwrap_or(text);
            let line = before.matches('\n').count() + 1;
            let column = before
                .rfind('\n')
                .map_or(before.len(), |newline| before.len() - newline - 1)
                + 1;
            format!("{MANIFEST_FILE}:{line}:{column}")
        }
        None => MANIFEST_FILE.to_owned(),
    };
    LinkError::new(code, format!("{detail} ({location})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rules_accept_relative_and_reject_escapes() {
        assert_eq!(
            validate_library_path("native/x86_64/libslug.so"),
            Ok(PathBuf::from("native/x86_64/libslug.so"))
        );
        assert_eq!(
            validate_library_path("./libslug.so"),
            Ok(PathBuf::from("libslug.so"))
        );
        assert!(validate_library_path("../libslug.so").is_err());
        assert!(validate_library_path("native/../../libslug.so").is_err());
        assert!(validate_library_path("/usr/lib/libslug.so").is_err());
        assert!(validate_library_path(".").is_err());
    }
}
