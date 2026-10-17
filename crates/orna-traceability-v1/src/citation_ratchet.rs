use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationBaseline {
    pub max_uncited: usize,
    pub cited_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CitationReport {
    pub cited_ids: BTreeSet<String>,
    pub uncited_ids: BTreeSet<String>,
    pub chapters: BTreeMap<String, ChapterCounts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChapterCounts {
    pub cited: usize,
    pub uncited: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatchetFailure {
    pub uncited: usize,
    pub max_uncited: usize,
    pub dropped_ids: BTreeSet<String>,
}

impl std::fmt::Display for RatchetFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "citation ratchet failed: {} uncited IDs exceed baseline {}; {} previously cited IDs dropped", self.uncited, self.max_uncited, self.dropped_ids.len())
    }
}
impl std::error::Error for RatchetFailure {}

/// Count requirement-ID citations in Rust test code. This measures citations, not proof of conformance.
pub fn cite_requirements(catalogue: &Path, crates_root: &Path) -> Result<CitationReport, String> {
    let requirements: Vec<Requirement> = serde_json::from_slice(&fs::read(catalogue).map_err(|e| format!("read {}: {e}", catalogue.display()))?)
        .map_err(|e| format!("parse {}: {e}", catalogue.display()))?;
    let mut all = BTreeMap::new();
    for requirement in requirements {
        if all.insert(requirement.id.clone(), requirement.chapter).is_some() {
            return Err(format!("duplicate requirement ID {}", requirement.id));
        }
    }
    let mut cited = BTreeSet::new();
    for file in rust_files(crates_root)? {
        let source = fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        for code in test_code(&file, &source) {
            let bytes = code.as_bytes();
            let mut cursor = 0;
            while cursor < bytes.len() {
                if !bytes[cursor].is_ascii_alphanumeric() ||
                    (cursor > 0 && (bytes[cursor - 1].is_ascii_alphanumeric() || bytes[cursor - 1] == b'-'))
                { cursor += 1; continue; }
                let start = cursor;
                while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'-') { cursor += 1; }
                let id = &code[start..cursor];
                if id.contains('-') && all.contains_key(id) && !cited.contains(id) && contains_id(code, id) {
                    cited.insert(id.to_owned());
                }
            }
        }
    }
    let uncited: BTreeSet<_> = all.keys().filter(|id| !cited.contains(*id)).cloned().collect();
    let mut chapters = BTreeMap::<String, ChapterCounts>::new();
    for (id, chapter) in all {
        let entry = chapters.entry(chapter).or_insert(ChapterCounts { cited: 0, uncited: 0 });
        if cited.contains(&id) { entry.cited += 1; } else { entry.uncited += 1; }
    }
    Ok(CitationReport { cited_ids: cited, uncited_ids: uncited, chapters })
}

pub fn check_ratchet(report: &CitationReport, baseline: &CitationBaseline) -> Result<(), RatchetFailure> {
    let dropped_ids = baseline.cited_ids.difference(&report.cited_ids).cloned().collect();
    let failure = RatchetFailure { uncited: report.uncited_ids.len(), max_uncited: baseline.max_uncited, dropped_ids };
    if failure.uncited > baseline.max_uncited || !failure.dropped_ids.is_empty() { Err(failure) } else { Ok(()) }
}

#[derive(Deserialize)]
struct Requirement { id: String, chapter: String }

fn rust_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("read directory entry: {e}"))?;
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            if rel.components().any(|part| matches!(part.as_os_str().to_str(), Some("target" | "fixtures"))) { continue; }
            if path.is_dir() { dirs.push(path); }
            else if path.extension().is_some_and(|ext| ext == "rs") { files.push(path); }
        }
    }
    files.sort();
    Ok(files)
}

fn test_code<'a>(path: &Path, source: &'a str) -> Vec<&'a str> {
    let is_test_file = path.components().any(|part| part.as_os_str() == "tests");
    let mut code = Vec::new();
    let mut cursor = 0;
    while let Some(start) = find_code_pattern(source, cursor, b"#[cfg(test)]") {
        let Some(open) = find_code_byte(source, start, b'{') else { break; };
        if let Some(end) = matching_brace(source, open) {
            code.push(&source[start..=end]);
            append_test_comments(source, open + 1, end, &mut code);
            cursor = end + 1;
        } else {
            break;
        }
    }
    // In integration-test files, select only functions carrying a test attribute.
    if is_test_file {
        cursor = 0;
        while let Some(start) = find_code_pattern(source, cursor, b"#[") {
            let Some(attribute_end) = matching_attribute(source, start) else { break; };
            if is_test_attribute(&source[start + 2..attribute_end - 1]) {
                let Some(function) = find_code_pattern(source, attribute_end, b"fn") else { break; };
                let Some(open) = find_code_byte(source, function, b'{') else { break; };
                if let Some(end) = matching_brace(source, open) {
                    if let Some(comments) = preceding_line_comments(source, start) {
                        code.push(comments);
                    }
                    code.push(&source[start..=end]);
                    append_test_line_comments(source, open + 1, end, &mut code);
                    cursor = end + 1;
                } else {
                    break;
                }
            } else {
                cursor = attribute_end;
            }
        }
        append_referenced_test_functions(source, &mut code);
        append_referenced_test_constants(source, &mut code);
    }
    code
}
fn append_referenced_test_functions<'a>(source: &'a str, selected: &mut Vec<&'a str>) {
    let bytes = source.as_bytes();
    loop {
        let mut cursor = 0;
        let mut brace_depth = 0usize;
        let mut added = false;
        while cursor < bytes.len() {
            if let Some(end) = skip_non_code(source, cursor) {
                cursor = end;
            } else if brace_depth == 0
                && bytes[cursor..].starts_with(b"fn")
                && (cursor == 0 || !(bytes[cursor - 1].is_ascii_alphanumeric() || bytes[cursor - 1] == b'_'))
                && bytes.get(cursor + 2).is_some_and(u8::is_ascii_whitespace)
            {
                let mut name_start = cursor + 2;
                while bytes.get(name_start).is_some_and(u8::is_ascii_whitespace) { name_start += 1; }
                let mut name_end = name_start;
                while bytes.get(name_end).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
                    name_end += 1;
                }
                if name_end == name_start { cursor += 2; continue; }
                let name = &source[name_start..name_end];
                let Some(open) = find_code_byte(source, name_end, b'{') else { break; };
                let Some(end) = matching_brace(source, open) else { break; };
                let function = &source[cursor..=end];
                if !selected.iter().any(|code| code.contains(function))
                    && selected.iter().any(|code| contains_identifier_in_code(code, name))
                {
                    selected.push(function);
                    added = true;
                }
                cursor = end + 1;
            } else {
                match bytes[cursor] {
                    b'{' => brace_depth += 1,
                    b'}' => brace_depth = brace_depth.saturating_sub(1),
                    _ => {}
                }
                cursor += 1;
            }
        }
        if !added { break; }
    }
}

fn append_referenced_test_constants<'a>(source: &'a str, selected: &mut Vec<&'a str>) {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut brace_depth = 0usize;
    while cursor < bytes.len() {
        if let Some(end) = skip_non_code(source, cursor) {
            cursor = end;
        } else if brace_depth == 0
            && bytes[cursor..].starts_with(b"const")
            && (cursor == 0 || !(bytes[cursor - 1].is_ascii_alphanumeric() || bytes[cursor - 1] == b'_'))
            && bytes.get(cursor + 5).is_some_and(|byte| byte.is_ascii_whitespace())
        {
            let mut name_start = cursor + 5;
            while bytes.get(name_start).is_some_and(u8::is_ascii_whitespace) { name_start += 1; }
            let mut name_end = name_start;
            while bytes.get(name_end).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') { name_end += 1; }
            if name_end == name_start { cursor += 5; continue; }
            let name = &source[name_start..name_end];
            let Some(equal) = find_code_byte(source, name_end, b'=') else { break; };
            let Some(end) = declaration_semicolon(source, equal + 1) else { break; };
            let referenced = selected.iter().any(|test| contains_identifier_in_code(test, name));
            if referenced { append_string_literal_contents(source, equal + 1, end, selected); }
            cursor = end + 1;
        } else {
            match bytes[cursor] { b'{' => brace_depth += 1, b'}' => brace_depth = brace_depth.saturating_sub(1), _ => {} }
            cursor += 1;
        }
    }
}

fn declaration_semicolon(source: &str, mut cursor: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let (mut parentheses, mut brackets, mut braces) = (0usize, 0usize, 0usize);
    while cursor < bytes.len() {
        if let Some(end) = skip_non_code(source, cursor) {
            cursor = end;
            continue;
        }
        match bytes[cursor] {
            b'(' => parentheses += 1,
            b')' => parentheses = parentheses.saturating_sub(1),
            b'[' => brackets += 1,
            b']' => brackets = brackets.saturating_sub(1),
            b'{' => braces += 1,
            b'}' => braces = braces.saturating_sub(1),
            b';' if parentheses == 0 && brackets == 0 && braces == 0 => return Some(cursor),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn contains_identifier_in_code(source: &str, name: &str) -> bool {
    let bytes = source.as_bytes();
    let name = name.as_bytes();
    let mut cursor = 0;
    while cursor + name.len() <= bytes.len() {
        if let Some(end) = skip_non_code(source, cursor) {
            cursor = end;
        } else if bytes[cursor..].starts_with(name)
            && (cursor == 0 || !(bytes[cursor - 1].is_ascii_alphanumeric() || bytes[cursor - 1] == b'_'))
            && bytes.get(cursor + name.len()).is_none_or(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
        {
            return true;
        } else {
            cursor += 1;
        }
    }
    false
}

fn append_string_literal_contents<'a>(source: &'a str, mut cursor: usize, end: usize, selected: &mut Vec<&'a str>) {
    while cursor < end {
        if let Some((literal_end, contents)) = string_literal(source, cursor) {
            selected.push(contents);
            cursor = literal_end;
        } else if let Some(non_code_end) = skip_non_code(source, cursor) {
            cursor = non_code_end;
        } else {
            cursor += 1;
        }
    }
}
fn preceding_line_comments(source: &str, start: usize) -> Option<&str> {
    let mut line_start = source[..start].rfind('\n').map_or(0, |newline| newline + 1);
    if !source[line_start..start].trim().is_empty() {
        return None;
    }

    let mut first_comment = None;
    while line_start > 0 {
        let previous_end = line_start - 1;
        let previous_start = source[..previous_end].rfind('\n').map_or(0, |newline| newline + 1);
        let line = source[previous_start..previous_end].trim();
        if line.starts_with("//") {
            first_comment = Some(previous_start);
            line_start = previous_start;
        } else {
            break;
        }
    }

    first_comment.map(|comment_start| &source[comment_start..start])
}
fn append_test_line_comments<'a>(source: &'a str, mut cursor: usize, end: usize, code: &mut Vec<&'a str>) {
    let bytes = source.as_bytes();
    while cursor < end {
        if bytes[cursor..].starts_with(b"//") {
            let comment_end = cursor
                + bytes[cursor..end]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(end - cursor);
            code.push(&source[cursor..comment_end]);
            cursor = comment_end;
        } else if let Some(non_code_end) = skip_non_code(source, cursor) {
            cursor = non_code_end;
        } else {
            cursor += 1;
        }
    }
}

fn append_test_comments<'a>(source: &'a str, mut cursor: usize, end: usize, code: &mut Vec<&'a str>) {
    while let Some(start) = find_code_pattern(source, cursor, b"#[") {
        if start >= end {
            break;
        }
        let Some(attribute_end) = matching_attribute(source, start) else { break; };
        if is_test_attribute(&source[start + 2..attribute_end - 1]) {
            if let Some(comments) = preceding_line_comments(source, start) {
                code.push(comments);
            }
            let Some(function) = find_code_pattern(source, attribute_end, b"fn") else { break; };
            let Some(open) = find_code_byte(source, function, b'{') else { break; };
            let Some(function_end) = matching_brace(source, open) else { break; };
            if function_end >= end {
                break;
            }
            append_test_line_comments(source, open + 1, function_end, code);
            cursor = function_end + 1;
        } else {
            cursor = attribute_end;
        }
    }
}
fn matching_attribute(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if !bytes.get(start..)?.starts_with(b"#[") { return None; }
    let mut depth = 0usize;
    let mut i = start + 1;
    while i < bytes.len() {
        if let Some(end) = skip_non_code(source, i) {
            i = end;
            continue;
        }
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 { return Some(i + 1); }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_test_attribute(attribute: &str) -> bool {
    let path = attribute.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == ':')).next().unwrap_or("");
    matches!(path.rsplit("::").next(), Some("test" | "rstest" | "test_case" | "proptest" | "quickcheck"))
}

fn find_code_pattern(source: &str, mut i: usize, pattern: &[u8]) -> Option<usize> {
    let bytes = source.as_bytes();
    while i + pattern.len() <= bytes.len() {
        if let Some(end) = skip_non_code(source, i) {
            i = end;
        } else if bytes[i..].starts_with(pattern) {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

fn find_code_byte(source: &str, mut i: usize, target: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    while i < bytes.len() {
        if let Some(end) = skip_non_code(source, i) {
            i = end;
        } else if bytes[i] == target {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

// Return the end of a comment or literal at i, so all code scanners share one lexer.
fn skip_non_code(source: &str, i: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes[i..].starts_with(b"//") {
        return Some(i + bytes[i..].iter().position(|byte| *byte == b'\n').unwrap_or(bytes.len() - i));
    }
    if bytes[i..].starts_with(b"/*") {
        let mut cursor = i + 2;
        let mut depth = 1usize;
        while cursor < bytes.len() && depth > 0 {
            if bytes[cursor..].starts_with(b"/*") { depth += 1; cursor += 2; }
            else if bytes[cursor..].starts_with(b"*/") { depth -= 1; cursor += 2; }
            else { cursor += 1; }
        }
        return Some(cursor);
    }
    let raw_start = if bytes[i] == b'r' { Some(i) }
        else if bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'r') { Some(i + 1) }
        else { None };
    if let Some(start) = raw_start {
        let mut quote = start + 1;
        while bytes.get(quote) == Some(&b'#') { quote += 1; }
        if bytes.get(quote) == Some(&b'"') {
            let hashes = quote - start - 1;
            let mut cursor = quote + 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'"'
                    && bytes.get(cursor + 1..).is_some_and(|suffix| {
                        suffix.len() >= hashes && suffix[..hashes].iter().all(|byte| *byte == b'#')
                    })
                {
                    return Some(cursor + 1 + hashes);
                }
                cursor += 1;
            }
            return Some(bytes.len());
        }
    }
    let quote = if bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'\'') { i + 1 }
        else if bytes[i] == b'\'' { i }
        else { usize::MAX };
    if quote != usize::MAX {
        let content = quote + 1;
        let end = if bytes.get(content) == Some(&b'\\') {
            match bytes.get(content + 1) {
                Some(b'u') if bytes.get(content + 2) == Some(&b'{') => {
                    bytes[content + 3..].iter().position(|byte| *byte == b'}').map(|offset| content + 3 + offset + 1)
                }
                Some(b'x') => Some(content + 4),
                Some(_) => Some(content + 2),
                None => None,
            }
        } else {
            source.get(content..).and_then(|rest| rest.chars().next()).map(|ch| content + ch.len_utf8())
        };
        if let Some(end) = end.filter(|end| bytes.get(*end) == Some(&b'\'')) {
            return Some(end + 1);
        }
    }
    let string_quote = if bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'"') { i + 1 }
        else if bytes[i] == b'"' { i }
        else { usize::MAX };
    if string_quote != usize::MAX {
        let mut cursor = string_quote + 1;
        while cursor < bytes.len() {
            if bytes[cursor] == b'\\' { cursor = (cursor + 2).min(bytes.len()); }
            else if bytes[cursor] == b'"' { return Some(cursor + 1); }
            else { cursor += 1; }
        }
        return Some(bytes.len());
    }
    None
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if let Some(end) = skip_non_code(source, i) {
            i = end;
            continue;
        }
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => { depth -= 1; if depth == 0 { return Some(i); } },
            _ => {}
        }
        i += 1;
    }
    None
}

fn contains_id(source: &str, id: &str) -> bool {
    if is_line_comment_block(source) {
        return contains_id_in_comment_block(source, id);
    }
    if !source.contains(id) { return false; }
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut code_start = 0;
    while cursor < bytes.len() {
        if let Some(end) = skip_non_code(source, cursor) {
            if contains_id_in_code(&source[code_start..cursor], id) { return true; }
            cursor = end;
            code_start = end;
        } else {
            cursor += 1;
        }
    }
    contains_id_in_code(&source[code_start..], id)
        || contains_id_in_assertion_args(source, id)
        || contains_id_in_test_string_operands(source, id)
}

fn contains_id_in_test_string_operands(source: &str, id: &str) -> bool {
    let bytes = source.as_bytes();
    let mut delimiters: Vec<(u8, Option<usize>, usize, bool)> = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if let Some((end, contents)) = string_literal(source, cursor) {
            let assertion_argument = delimiters.iter().rev().find_map(|(_, limit, argument, _)| {
                limit.map(|limit| *argument < limit)
            });
            let in_diagnostic = delimiters.iter().any(|(_, _, _, diagnostic)| *diagnostic);
            if assertion_argument.is_none()
                && !in_diagnostic
                && contains_id_in_code(contents, id)
                && is_active_test_string_operand(source, cursor, end)
            {
                return true;
            }
            cursor = end;
        } else if let Some(end) = skip_non_code(source, cursor) {
            cursor = end;
        } else if let Some((after_open, close, arguments)) = assertion_macro(source, cursor) {
            delimiters.push((close, Some(arguments), 0, false));
            cursor = after_open;
        } else if let Some((after_open, close)) = diagnostic_macro(source, cursor) {
            delimiters.push((close, None, 0, true));
            cursor = after_open;
        } else if let Some((after_open, close)) = diagnostic_call(source, cursor) {
            delimiters.push((close, None, 0, true));
            cursor = after_open;
        } else {
            match bytes[cursor] {
                b'(' => delimiters.push((b')', None, 0, false)),
                b'[' => delimiters.push((b']', None, 0, false)),
                b'{' => delimiters.push((b'}', None, 0, false)),
                b')' | b']' | b'}' => {
                    if delimiters.last().is_some_and(|(close, _, _, _)| *close == bytes[cursor]) {
                        delimiters.pop();
                    }
                }
                b',' => {
                    if let Some((_, Some(_), argument, _)) = delimiters.last_mut() {
                        *argument += 1;
                    }
                }
                _ => {}
            }
            cursor += 1;
        }
    }
    false
}

fn diagnostic_macro(source: &str, start: usize) -> Option<(usize, u8)> {
    const MACROS: [&[u8]; 7] = [
        b"println", b"eprintln", b"format", b"panic", b"todo", b"unimplemented", b"dbg",
    ];
    let bytes = source.as_bytes();
    for name in MACROS {
        if !bytes[start..].starts_with(name)
            || (start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_'))
            || bytes.get(start + name.len()).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            continue;
        }
        let mut cursor = start + name.len();
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) { cursor += 1; }
        if bytes.get(cursor) != Some(&b'!') { continue; }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) { cursor += 1; }
        return match bytes.get(cursor) {
            Some(b'(') => Some((cursor + 1, b')')),
            Some(b'{') => Some((cursor + 1, b'}')),
            _ => None,
        };
    }
    None
}

fn diagnostic_call(source: &str, start: usize) -> Option<(usize, u8)> {
    const CALLS: [&[u8]; 4] = [b"expect", b"expect_err", b"unwrap_or_else", b"unwrap_or_default"];
    let bytes = source.as_bytes();
    for name in CALLS {
        if !bytes[start..].starts_with(name)
            || (start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_'))
            || bytes.get(start + name.len()).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            continue;
        }
        let mut cursor = start + name.len();
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) { cursor += 1; }
        if bytes.get(cursor) == Some(&b'(') { return Some((cursor + 1, b')')); }
    }
    None
}

fn is_active_test_string_operand(source: &str, start: usize, end: usize) -> bool {
    let prefix = source[..start].trim_end();
    let suffix = source[end..].trim_start();
    if [
        "println!(", "eprintln!(", "format!(", "panic!(", "expect(",
        "expect_err(", "unwrap_or_else(",
    ]
    .iter()
    .any(|diagnostic| prefix.ends_with(diagnostic))
    {
        return false;
    }
    if prefix.ends_with("==") || prefix.ends_with("!=") {
        return true;
    }
    let next_is_operand_delimiter = suffix.starts_with(',')
        || suffix.starts_with(']')
        || suffix.starts_with(')')
        || suffix.starts_with(".into(");
    matches!(prefix.chars().next_back(), Some(':'))
        || (matches!(prefix.chars().next_back(), Some('[' | ',' | '('))
            && next_is_operand_delimiter)
}
fn is_line_comment_block(source: &str) -> bool {
    if source.lines().any(|line| {
        !line.trim().is_empty() && !line.trim_start().starts_with("//")
    }) {
        return false;
    }
    let mut lines = source.lines().filter(|line| !line.trim().is_empty());
    lines.next().is_some() && lines.all(|line| line.trim_start().starts_with("//"))
}

fn contains_id_in_comment_block(source: &str, id: &str) -> bool {
    source.lines().filter_map(|line| line.trim_start().strip_prefix("//")).any(|comment| {
        let mut cursor = 0;
        let mut code_start = 0;
        while cursor < comment.len() {
            if let Some((end, _)) = string_literal(comment, cursor) {
                if contains_id_in_code(&comment[code_start..cursor], id) {
                    return true;
                }
                cursor = end;
                code_start = end;
            } else {
                cursor += 1;
            }
        }
        contains_id_in_code(&comment[code_start..], id)
    })
}

fn contains_id_in_assertion_args(source: &str, id: &str) -> bool {
    let bytes = source.as_bytes();
    let mut delimiters: Vec<(u8, Option<usize>, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some((end, contents)) = string_literal(source, i) {
            let active_argument = delimiters.iter().rev().find_map(|(_, limit, argument)| {
                limit.map(|limit| *argument < limit)
            });
            if active_argument == Some(true) && contains_id_in_code(contents, id) { return true; }
            if active_argument == Some(false) { i = end; continue; }
            i = end;
        } else if let Some(end) = skip_non_code(source, i) {
            i = end;
        } else if let Some((after_open, close, arguments)) = assertion_macro(source, i) {
            delimiters.push((close, Some(arguments), 0));
            i = after_open;
        } else {
            match bytes[i] {
                b'(' => delimiters.push((b')', None, 0)),
                b'[' => delimiters.push((b']', None, 0)),
                b'{' => delimiters.push((b'}', None, 0)),
                b')' | b']' | b'}' => {
                    if delimiters.last().is_some_and(|(close, _, _)| *close == bytes[i]) { delimiters.pop(); }
                }
                b',' => {
                    if let Some((_, Some(_), argument)) = delimiters.last_mut() { *argument += 1; }
                }
                _ => {}
            }
            i += 1;
        }
    }
    false
}

fn string_literal(source: &str, start: usize) -> Option<(usize, &str)> {
    let bytes = source.as_bytes();
    let raw_start = if bytes.get(start) == Some(&b'r') { Some(start) }
        else if bytes.get(start) == Some(&b'b') && bytes.get(start + 1) == Some(&b'r') { Some(start + 1) }
        else { None };
    let (quote, hashes) = if let Some(raw_start) = raw_start {
        let mut quote = raw_start + 1;
        while bytes.get(quote) == Some(&b'#') { quote += 1; }
        if bytes.get(quote) != Some(&b'"') { return None; }
        (quote, quote - raw_start - 1)
    } else {
        let quote = if bytes.get(start) == Some(&b'"') { start }
            else if bytes.get(start) == Some(&b'b') && bytes.get(start + 1) == Some(&b'"') { start + 1 }
            else { return None; };
        (quote, 0)
    };
    let end = skip_non_code(source, start)?;
    let contents_end = end.checked_sub(hashes + 1)?;
    Some((end, &source[quote + 1..contents_end]))
}

fn assertion_macro(source: &str, start: usize) -> Option<(usize, u8, usize)> {
    const MACROS: [(&[u8], usize); 6] = [
        (b"debug_assert_eq", 2), (b"debug_assert_ne", 2), (b"assert_eq", 2),
        (b"assert_ne", 2), (b"debug_assert", 1), (b"assert", 1),
    ];
    let bytes = source.as_bytes();
    for (name, arguments) in MACROS {
        if !bytes[start..].starts_with(name) ||
            (start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')) ||
            bytes.get(start + name.len()).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        { continue; }
        let mut i = start + name.len();
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) { i += 1; }
        if bytes.get(i) != Some(&b'!') { continue; }
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) { i += 1; }
        return match bytes.get(i) {
            Some(b'(') => Some((i + 1, b')', arguments)),
            Some(b'{') => Some((i + 1, b'}', arguments)),
            _ => None,
        };
    }
    None
}

fn contains_id_in_code(source: &str, id: &str) -> bool {
    source.match_indices(id).any(|(start, _)| {
        let before = source[..start].chars().next_back();
        let after = source[start + id.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-') &&
            !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("orna-citations-{n}"));
            fs::create_dir_all(&path).unwrap(); Self(path)
        }
    }
    impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn report(cited: &[&str], uncited: &[&str]) -> CitationReport {
        CitationReport { cited_ids: cited.iter().map(|s| (*s).to_owned()).collect(), uncited_ids: uncited.iter().map(|s| (*s).to_owned()).collect(), chapters: BTreeMap::new() }
    }

    #[test]
    fn counts_catalogue_citations_by_chapter_and_only_in_test_code() {
        let tmp = Temp::new();
        let root = &tmp.0;
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::create_dir_all(root.join("crates/a/tests/fixtures")).unwrap();
        let catalogue = root.join("requirements.json");
        fs::write(&catalogue, r#"[{"id":"REQ-001","chapter":"one"},{"id":"REQ-002","chapter":"one"},{"id":"REQ-003","chapter":"two"}]"#).unwrap();
        fs::write(root.join("crates/a/src/lib.rs"), "const PROD: &str = \"REQ-002\"; #[cfg(test)] mod tests { fn actual_test() { let _ = REQ-001; } }").unwrap();
        fs::write(root.join("crates/a/tests/a.rs"), "const HELPER: &str = \"REQ-002\"; #[test] fn actual_test() { let _ = REQ-003; }").unwrap();
        fs::write(root.join("crates/a/tests/fixtures/ignored.rs"), "// REQ-002").unwrap();
        let result = cite_requirements(&catalogue, root).unwrap();
        assert_eq!(result.cited_ids, ["REQ-001", "REQ-003"].into_iter().map(str::to_owned).collect());
        assert_eq!(result.chapters["one"], ChapterCounts { cited: 1, uncited: 1 });
        assert_eq!(result.chapters["two"], ChapterCounts { cited: 1, uncited: 0 });
    }
    #[test]
    fn counts_requirement_comment_directly_above_test_function() {
        let tmp = Temp::new();
        let root = &tmp.0;
        fs::create_dir_all(root.join("crates/a/tests")).unwrap();
        let catalogue = root.join("requirements.json");
        fs::write(&catalogue, r#"[{"id":"REQ-001","chapter":"one"},{"id":"REQ-002","chapter":"one"}]"#).unwrap();
        fs::write(
            root.join("crates/a/tests/a.rs"),
            "// REQ-001 is the behavior covered here\n#[test]\nfn actual_test() { assert!(true); }\n\n// REQ-002 belongs to an ordinary helper, not a test.\nfn helper() {}",
        )
        .unwrap();

        let result = cite_requirements(&catalogue, root).unwrap();
        assert_eq!(result.cited_ids, ["REQ-001"].into_iter().map(str::to_owned).collect());
    }

    #[test]
    fn counts_citation_comments_above_unit_test_functions_only() {
        let tmp = Temp::new();
        let root = &tmp.0;
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::create_dir_all(root.join("crates/a/tests")).unwrap();
        let catalogue = root.join("requirements.json");
        fs::write(
            &catalogue,
            r#"[{"id":"REQ-001","chapter":"one"},{"id":"REQ-002","chapter":"one"},{"id":"REQ-003","chapter":"one"}]"#,
        )
        .unwrap();
        fs::write(
            root.join("crates/a/src/lib.rs"),
            "#[cfg(test)] mod tests {\n    // REQ-002 directly annotates this test.\n    #[test]\n    fn unit_test() {}\n}",
        )
        .unwrap();
        let unit_test_code = fs::read_to_string(root.join("crates/a/src/lib.rs")).unwrap();
        let selected = test_code(Path::new("crates/a/src/lib.rs"), &unit_test_code);
        assert!(selected.iter().any(|code| contains_id(code, "REQ-002")), "{selected:?}");
        fs::write(
            root.join("crates/a/tests/a.rs"),
            "// REQ-001 directly annotates this test.\n#[test]\nfn integration_test() {}\n\n// REQ-003 is outside test code.\nfn helper() {}",
        )
        .unwrap();

        let result = cite_requirements(&catalogue, root).unwrap();
        assert_eq!(
            result.cited_ids,
            ["REQ-001", "REQ-002"].into_iter().map(str::to_owned).collect()
        );
    }
    #[test]
    fn test_code_selects_only_cfg_test_modules_not_surrounding_or_tests_directory_code() {
        let source = r##"#[cfg(test)] mod first { const A: &str = "REQ-IN"; } #[cfg(test)] mod second { const B: &str = "REQ-AFTER"; }"##;
        let selected = test_code(Path::new("src/lib.rs"), source);
        assert_eq!(selected.len(), 2);
        assert!(selected[0].contains("REQ-IN"));
        assert!(selected[1].contains("REQ-AFTER"));
        assert!(test_code(Path::new("tests/integration.rs"), "const HELPER: &str = \"REQ-TESTS\"; #[test] fn actual() { let _ = \"REQ-TESTS\"; }").len() == 1);
        let async_source = "#[tokio::test] async fn actual() { assert_eq!(value, \"REQ-ASYNC\"); }";
        let async_selected = test_code(Path::new("tests/integration.rs"), async_source);
        assert_eq!(async_selected.len(), 1);
        assert!(contains_id(async_selected[0], "REQ-ASYNC"));
        assert!(test_code(Path::new("tests/integration.rs"), "fn f() { // REQ-TESTS\n }").is_empty());
    }

    #[test]
    fn commented_out_test_attribute_does_not_count_its_requirement_id() {
        let source = "// #[test] fn disabled() { REQ-001 }";
        let selected = test_code(Path::new("tests/integration.rs"), source);
        assert!(!selected.iter().any(|code| contains_id(code, "REQ-001")));
    }

    #[test]
    fn only_test_assertion_operands_count_requirement_strings() {
        let cfg_string = r##""#[cfg(test)] mod fake { REQ-001 }""##;
        let cfg_selected = test_code(Path::new("src/lib.rs"), cfg_string);
        assert!(!cfg_selected.iter().any(|code| contains_id(code, "REQ-001")));

        let inert_source = r##"#[test] fn example() { let note = "REQ-001"; }"##;
        let inert_selected = test_code(Path::new("tests/integration.rs"), inert_source);
        assert!(!inert_selected.iter().any(|code| contains_id(code, "REQ-001")));

        let expected_source = r##"#[test] fn example() { assert_eq!(diagnostic.code(), "REQ-001"); }"##;
        let expected_selected = test_code(Path::new("tests/integration.rs"), expected_source);
        assert!(expected_selected.iter().any(|code| contains_id(code, "REQ-001")));

        let not_equal_source = r##"#[test] fn example() { assert_ne!(actual, "REQ-001"); }"##;
        let not_equal_selected = test_code(Path::new("tests/integration.rs"), not_equal_source);
        assert!(not_equal_selected.iter().any(|code| contains_id(code, "REQ-001")));

        let condition_source = r##"#[test] fn example() { assert!(values.contains(&"REQ-001")); }"##;
        let condition_selected = test_code(Path::new("tests/integration.rs"), condition_source);
        assert!(condition_selected.iter().any(|code| contains_id(code, "REQ-001")));

        let comment_source = "#[test] fn example() { // assert_eq!(actual, \"REQ-001\");\n}";
        let comment_selected = test_code(Path::new("tests/integration.rs"), comment_source);
        assert!(!comment_selected.iter().any(|code| contains_id(code, "REQ-001")));
        let citation_source = "#[test] fn example() {\n    // REQ-001 is cited by this active test.\n    assert!(true);\n}";
        let citation_selected = test_code(Path::new("tests/integration.rs"), citation_source);
        assert!(citation_selected.iter().any(|code| contains_id(code, "REQ-001")));

        let message_source = r##"#[test] fn example() { assert_eq!(actual, expected, "REQ-001"); }"##;
        let message_selected = test_code(Path::new("tests/integration.rs"), message_source);
        assert!(!message_selected.iter().any(|code| contains_id(code, "REQ-001")));
    }

    #[test]
    fn counts_requirement_ids_used_as_active_test_operands_beyond_assertions() {
        let source = r##"#[test] fn example() {
            let binding = Binding { requirement_id: "REQ-001" };
            let witnesses = witnesses_for(&root, "REQ-002");
            for requirement_id in ["REQ-003", "REQ-004"] {
                assert_eq!(requirement.requirement_id, requirement_id);
            }
            let found = report.items().find(|item| item.requirement_id == "REQ-005");
            assert_eq!(found, "REQ-006");
            assert_eq!(actual, expected, "REQ-007");
            println!("REQ-008");
            println!("{}", "REQ-010");
            let _ = Ok::<(), &str>(()).expect("REQ-012");
            let inert_note = "REQ-009";
        }"##;
        let selected = test_code(Path::new("tests/integration.rs"), source);
        for id in ["REQ-001", "REQ-002", "REQ-003", "REQ-004", "REQ-005", "REQ-006"] {
            assert!(selected.iter().any(|code| contains_id(code, id)), "{id}");
        }
        for id in ["REQ-007", "REQ-008", "REQ-009", "REQ-010", "REQ-012"] {
            assert!(!selected.iter().any(|code| contains_id(code, id)), "{id}");
        }
    }
    #[test]
    fn referenced_integration_test_constants_are_selected_but_unused_constants_are_not() {
        let source = concat!(
            "const ROWS: &[&str] = &[\"REQ-001\"];\n",
            "const UNUSED: &str = \"REQ-002\";\n",
            "fn rows() -> &'static [&'static str] { ROWS }\n",
            "#[test] fn example() { assert_eq!(rows().len(), 1); }",
        );
        let selected = test_code(Path::new("tests/integration.rs"), source);
        assert!(selected.iter().any(|code| contains_id(code, "REQ-001")));
        assert!(!selected.iter().any(|code| contains_id(code, "REQ-002")));
    }

    #[test]
    fn matching_brace_ignores_char_byte_and_string_literal_closers() {
        let source = r####"{ fn borrow<'a>(_: &'a str) {} let a = '}'; let b = b'}'; let c = "}"; let d = br#"}"#; } trailing"####;
        let end = matching_brace(source, 0).unwrap();
        assert_eq!(&source[end + 1..], " trailing");
    }

    #[test]
    fn fails_when_uncited_count_rises() {
        let baseline = CitationBaseline { max_uncited: 1, cited_ids: ["REQ-1".to_owned()].into() };
        assert!(check_ratchet(&report(&["REQ-1"], &["A", "B"]), &baseline).is_err());
    }

    #[test]
    fn fails_when_previously_cited_id_drops_even_if_count_does_not_rise() {
        let baseline = CitationBaseline { max_uncited: 2, cited_ids: ["REQ-1".to_owned()].into() };
        assert!(check_ratchet(&report(&[], &["REQ-1"]), &baseline).unwrap_err().dropped_ids.contains("REQ-1"));
    }

    #[test]
    fn permits_lowering_uncited_baseline_without_dropping_citations() {
        let baseline = CitationBaseline { max_uncited: 1, cited_ids: ["REQ-1".to_owned()].into() };
        assert!(check_ratchet(&report(&["REQ-1"], &[]), &baseline).is_ok());
    }
}
