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
    if path.components().any(|part| part.as_os_str() == "tests") { return vec![source]; }
    let mut code = Vec::new();
    let mut cursor = 0;
    while let Some(attr) = source[cursor..].find("#[cfg(test)]") {
        let start = cursor + attr;
        let Some(open_rel) = source[start..].find('{') else { break; };
        let open = start + open_rel;
        if let Some(end) = matching_brace(source, open) { code.push(&source[start..=end]); cursor = end + 1; }
        else { break; }
    }
    code
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            i = source[i..].find('\n').map_or(bytes.len(), |end| i + end);
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut comments = 1usize;
            while i < bytes.len() && comments > 0 {
                if bytes[i..].starts_with(b"/*") { comments += 1; i += 2; }
                else if bytes[i..].starts_with(b"*/") { comments -= 1; i += 2; }
                else { i += 1; }
            }
            continue;
        }
        let raw_start = if bytes[i] == b'r' { Some(i) }
            else if bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'r') { Some(i + 1) }
            else { None };
        if let Some(start) = raw_start {
            let mut quote = start + 1;
            while bytes.get(quote) == Some(&b'#') { quote += 1; }
            if bytes.get(quote) == Some(&b'"') {
                let hashes = quote - start - 1;
                let terminator = format!("\"{}", "#".repeat(hashes));
                if let Some(end) = source[quote + 1..].find(&terminator) { i = quote + 1 + end + terminator.len(); continue; }
                return None;
            }
        }
        if bytes[i] == b'"' || (bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'"')) {
            if bytes[i] == b'b' { i += 1; }
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' { i = (i + 2).min(bytes.len()); }
                else if bytes[i] == b'"' { i += 1; break; }
                else { i += 1; }
            }
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
        fs::write(root.join("crates/a/src/lib.rs"), "const PROD: &str = \"REQ-002\"; #[cfg(test)] mod tests { const TEST: &str = \"{ REQ-001 }\"; }").unwrap();
        fs::write(root.join("crates/a/tests/a.rs"), "// REQ-003").unwrap();
        fs::write(root.join("crates/a/tests/fixtures/ignored.rs"), "// REQ-002").unwrap();
        let result = cite_requirements(&catalogue, root).unwrap();
        assert_eq!(result.cited_ids, ["REQ-001", "REQ-003"].into_iter().map(str::to_owned).collect());
        assert_eq!(result.chapters["one"], ChapterCounts { cited: 1, uncited: 1 });
        assert_eq!(result.chapters["two"], ChapterCounts { cited: 1, uncited: 0 });
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
