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
    let ids: Vec<_> = all.keys().cloned().collect();
    let mut cited = BTreeSet::new();
    for file in rust_files(crates_root)? {
        let source = fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        for code in test_code(&file, &source) {
            for id in &ids {
                if contains_id(code, id) { cited.insert(id.clone()); }
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
            cursor = end + 1;
        } else {
            break;
        }
    }
    // Outside cfg(test) modules, only explicit #[test] function bodies qualify, regardless of file location.
    if is_test_file {
        cursor = 0;
        while let Some(start) = find_code_pattern(source, cursor, b"#[test]") {
            let Some(open) = find_code_byte(source, start, b'{') else { break; };
            if let Some(end) = matching_brace(source, open) {
                code.push(&source[start..=end]);
                cursor = end + 1;
            } else {
                break;
            }
        }
    }
    code
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
    fn test_code_selects_only_cfg_test_modules_not_surrounding_or_tests_directory_code() {
        let source = r##"#[cfg(test)] mod first { const A: &str = "REQ-IN"; } #[cfg(test)] mod second { const B: &str = "REQ-AFTER"; }"##;
        let selected = test_code(Path::new("src/lib.rs"), source);
        assert_eq!(selected.len(), 2);
        assert!(selected[0].contains("REQ-IN"));
        assert!(selected[1].contains("REQ-AFTER"));
        assert!(test_code(Path::new("tests/integration.rs"), "const HELPER: &str = \"REQ-TESTS\"; #[test] fn actual() { let _ = \"REQ-TESTS\"; }").len() == 1);
        assert!(test_code(Path::new("tests/integration.rs"), "fn f() { // REQ-TESTS\n }").is_empty());
    }

    #[test]
    fn commented_out_test_attribute_does_not_count_its_requirement_id() {
        let source = "// #[test] fn disabled() { REQ-001 }";
        let selected = test_code(Path::new("tests/integration.rs"), source);
        assert!(!selected.iter().any(|code| contains_id(code, "REQ-001")));
    }

    #[test]
    fn requirement_id_inside_test_string_does_not_count_as_citation() {
        let cfg_string = r##""#[cfg(test)] mod fake { REQ-001 }""##;
        let cfg_selected = test_code(Path::new("src/lib.rs"), cfg_string);
        assert!(!cfg_selected.iter().any(|code| contains_id(code, "REQ-001")));

        let test_source = r##"#[test] fn example() { let note = "REQ-001"; }"##;
        let selected = test_code(Path::new("tests/integration.rs"), test_source);
        assert!(!selected.iter().any(|code| contains_id(code, "REQ-001")));
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
