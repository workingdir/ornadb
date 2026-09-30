use std::path::{Path, PathBuf};

#[derive(Debug)]
struct StringLiteral {
    start: usize,
    end: usize,
    value: String,
}

#[derive(Debug)]
struct RustSource {
    code_without_comments: Vec<u8>,
    strings: Vec<StringLiteral>,
}

fn parse_string(bytes: &[u8], start: usize) -> Option<(usize, String)> {
    if bytes.get(start) == Some(&b'"') {
        let mut cursor = start + 1;
        let mut value = String::new();
        while let Some(&byte) = bytes.get(cursor) {
            match byte {
                b'"' => return Some((cursor + 1, value)),
                b'\\' => {
                    cursor += 1;
                    let escaped = *bytes.get(cursor)?;
                    match escaped {
                        b'\\' => value.push('\\'),
                        b'"' => value.push('"'),
                        b'n' => value.push('\n'),
                        b'r' => value.push('\r'),
                        b't' => value.push('\t'),
                        b'0' => value.push('\0'),
                        b'\n' => {
                            cursor += 1;
                            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                                cursor += 1;
                            }
                            continue;
                        }
                        other => value.push(char::from(other)),
                    }
                }
                _ => value.push(char::from(byte)),
            }
            cursor += 1;
        }
        return None;
    }

    if bytes.get(start) != Some(&b'r') {
        return None;
    }
    let mut quote = start + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let hashes = quote - start - 1;
    let content_start = quote + 1;
    let mut cursor = content_start;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..cursor + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            let value = String::from_utf8_lossy(&bytes[content_start..cursor]).into_owned();
            return Some((cursor + hashes + 1, value));
        }
        cursor += 1;
    }
    None
}

fn scan_rust_source(source: &str) -> RustSource {
    let bytes = source.as_bytes();
    let mut code_without_comments = bytes.to_vec();
    let mut strings = Vec::new();
    let mut cursor = 0;

    while cursor < bytes.len() {
        if let Some((end, value)) = parse_string(bytes, cursor) {
            strings.push(StringLiteral {
                start: cursor,
                end,
                value,
            });
            cursor = end;
            continue;
        }

        if bytes.get(cursor..cursor + 2) == Some(b"//") {
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            for byte in &mut code_without_comments[start..cursor] {
                *byte = b' ';
            }
            continue;
        }

        if bytes.get(cursor..cursor + 2) == Some(b"/*") {
            let start = cursor;
            cursor += 2;
            let mut depth = 1usize;
            while cursor < bytes.len() && depth > 0 {
                match bytes.get(cursor..cursor + 2) {
                    Some(b"/*") => {
                        depth += 1;
                        cursor += 2;
                    }
                    Some(b"*/") => {
                        depth -= 1;
                        cursor += 2;
                    }
                    _ => cursor += 1,
                }
            }
            for byte in &mut code_without_comments[start..cursor] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            continue;
        }

        cursor += 1;
    }

    RustSource {
        code_without_comments,
        strings,
    }
}

fn is_external_reference_path(value: &str) -> bool {
    let normalized = value.replace('\\', "/");
    let components = normalized
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    if !normalized.contains('/') {
        return false;
    }

    let local_fixture = components
        .windows(3)
        .any(|parts| parts == ["tests", "fixtures", "reference"]);
    if local_fixture {
        return false;
    }

    let Some(reference) = components.iter().position(|component| *component == "reference")
    else {
        return false;
    };
    let absolute = normalized.starts_with('/')
        || normalized.as_bytes().get(1) == Some(&b':');
    let traverses_out = components[..reference].contains(&"..");
    let repository_root_reference = reference == 0
        && components.get(reference + 1) == Some(&"Orna-1.0.0");

    absolute || traverses_out || repository_root_reference
}

fn walk_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("read Rust source directory") {
        let entry = entry.expect("read Rust source entry");
        let path = entry.path();
        let kind = entry.file_type().expect("read Rust source entry type");
        if kind.is_dir() {
            walk_rust_files(&path, files);
        } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn include_argument_fragments(source: &RustSource, open_paren: usize) -> Vec<String> {
    let code = &source.code_without_comments;
    let mut fragments = Vec::new();
    let mut cursor = open_paren + 1;
    let mut nested_parens = 0usize;

    while cursor < code.len() {
        if let Some(literal) = source.strings.iter().find(|literal| literal.start == cursor) {
            fragments.push(literal.value.clone());
            cursor = literal.end;
            continue;
        }
        match code[cursor] {
            b'(' => nested_parens += 1,
            b')' if nested_parens == 0 => break,
            b')' => nested_parens -= 1,
            _ => {}
        }
        cursor += 1;
    }
    fragments
}

fn assert_include_paths_stay_in_checkout(
    source_path: &Path,
    root: &Path,
    source: &RustSource,
) {
    let code = &source.code_without_comments;
    for macro_name in [b"include_str!".as_slice(), b"include_bytes!", b"include!"] {
        let mut search_from = 0;
        while let Some(relative) = code[search_from..]
            .windows(macro_name.len())
            .position(|window| window == macro_name)
        {
            let macro_start = search_from + relative;
            search_from = macro_start + macro_name.len();
            if source
                .strings
                .iter()
                .any(|literal| literal.start <= macro_start && macro_start < literal.end)
            {
                continue;
            }

            let mut argument = search_from;
            while code.get(argument).is_some_and(u8::is_ascii_whitespace) {
                argument += 1;
            }
            if code.get(argument) != Some(&b'(') {
                continue;
            }
            let fragments = include_argument_fragments(source, argument);
            assert!(
                !fragments.is_empty(),
                "compile-time include path must contain a statically inspectable string: {}",
                source_path.display()
            );
            let include_value = fragments.concat();
            assert!(
                !is_external_reference_path(&include_value),
                "compile-time include uses an external reference tree: {} -> {:?}",
                source_path.display(),
                include_value
            );
            let include = Path::new(&include_value);
            let resolved = if include.is_absolute() {
                normalize_path(include)
            } else {
                normalize_path(&source_path.parent().expect("source parent").join(include))
            };
            let resolved = resolved.canonicalize().unwrap_or(resolved);
            assert!(
                resolved.starts_with(root),
                "compile-time include escapes the repository: {} -> {}",
                source_path.display(),
                include_value
            );
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("conformance crate is under crates/")
        .canonicalize()
        .expect("canonicalize workspace root")
}

#[test]
fn external_reference_path_fragments_are_rejected() {
    let escaped_sibling = concat!("../", "reference/", "Orna-1.0.0/source/01.md");
    let absolute_reference = concat!("/home/example/", "reference/", "Orna-1.0.0");
    let vendored_fixture = "tests/fixtures/reference/examples/valid/minimal.orna";

    assert!(is_external_reference_path(escaped_sibling));
    assert!(is_external_reference_path(absolute_reference));
    assert!(!is_external_reference_path(vendored_fixture));
}

#[test]
#[should_panic(expected = "compile-time include uses an external reference tree")]
fn split_compile_time_path_fragments_are_rejected() {
    let source_text = [
        b"include_str!(concat!(\"".as_slice(),
        b"../",
        b"\", \"reference/",
        b"Orna-1.0.0/source/01.md\"))",
    ]
    .concat();
    let source = scan_rust_source(std::str::from_utf8(&source_text).expect("synthetic Rust source"));
    let root = workspace_root();
    let source_path = root.join("crates/orna-syntax/tests/synthetic.rs");
    assert_include_paths_stay_in_checkout(&source_path, &root, &source);
}

#[test]
fn workspace_rust_sources_keep_test_inputs_inside_the_checkout() {
    let root = workspace_root();
    let mut files = Vec::new();
    walk_rust_files(&root.join("crates"), &mut files);
    files.sort();

    let mut violations = Vec::new();
    for path in files {
        let source_text = std::fs::read_to_string(&path).expect("read Rust source");
        let source = scan_rust_source(&source_text);
        for literal in &source.strings {
            if is_external_reference_path(&literal.value) {
                violations.push(format!(
                    "{} contains external reference path {:?}",
                    path.strip_prefix(&root).unwrap_or(&path).display(),
                    literal.value
                ));
            }
        }
        assert_include_paths_stay_in_checkout(&path, &root, &source);
    }

    assert!(
        violations.is_empty(),
        "external reference paths must be vendored under crate tests/fixtures:\n{}",
        violations.join("\n")
    );
}
