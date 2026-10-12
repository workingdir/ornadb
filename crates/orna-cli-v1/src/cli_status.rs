//! Stable JSON encoding for the Git-backed status command.

#[derive(Clone, Debug, Eq, PartialEq)]
struct Change<'a> {
    index: u8,
    worktree: u8,
    path: &'a [u8],
    original_path: Option<&'a [u8]>,
}

/// Convert Git porcelain-v1 `-z` output into compact, deterministic JSON.
///
/// Paths are represented as JSON strings. Git paths that are not valid UTF-8
/// use the standard replacement character so the output remains valid JSON.
pub(super) fn encode_status_json(branch: &[u8], porcelain: &[u8]) -> Option<Vec<u8>> {
    let changes = parse_changes(porcelain)?;
    let mut json = String::from("{\"branch\":");
    push_json_bytes(&mut json, branch);
    json.push_str(",\"changes\":[");
    for (index, change) in changes.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str("{\"index\":");
        push_json_char(&mut json, char::from(change.index));
        json.push_str(",\"worktree\":");
        push_json_char(&mut json, char::from(change.worktree));
        json.push_str(",\"path\":");
        push_json_bytes(&mut json, change.path);
        if let Some(original_path) = change.original_path {
            json.push_str(",\"original_path\":");
            push_json_bytes(&mut json, original_path);
        }
        json.push('}');
    }
    json.push_str("]}\n");
    Some(json.into_bytes())
}

pub(super) fn branch_name<'a>(symbolic_ref_succeeded: bool, stdout: &'a [u8]) -> &'a [u8] {
    if symbolic_ref_succeeded {
        stdout.strip_suffix(b"\n").unwrap_or(stdout)
    } else {
        b"HEAD"
    }
}

fn parse_changes(porcelain: &[u8]) -> Option<Vec<Change<'_>>> {
    let mut changes = Vec::new();
    let mut records = porcelain.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        if record.len() < 4 || record[2] != b' ' {
            return None;
        }
        let rename_or_copy = matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C');
        let original_path = if rename_or_copy {
            Some(records.next().filter(|path| !path.is_empty())?)
        } else {
            None
        };
        changes.push(Change {
            index: record[0],
            worktree: record[1],
            path: &record[3..],
            original_path,
        });
    }
    Some(changes)
}

fn push_json_bytes(json: &mut String, value: &[u8]) {
    let value = String::from_utf8_lossy(value);
    json.push('"');
    for character in value.chars() {
        push_escaped_char(json, character);
    }
    json.push('"');
}

fn push_json_char(json: &mut String, character: char) {
    json.push('"');
    push_escaped_char(json, character);
    json.push('"');
}

fn push_escaped_char(json: &mut String, character: char) {
    match character {
        '"' => json.push_str("\\\""),
        '\\' => json.push_str("\\\\"),
        '\n' => json.push_str("\\n"),
        '\r' => json.push_str("\\r"),
        '\t' => json.push_str("\\t"),
        character if character.is_control() => {
            use std::fmt::Write as _;
            write!(json, "\\u{:04x}", u32::from(character)).expect("writing to a String");
        }
        character => json.push(character),
    }
}

#[cfg(test)]
mod tests {
    use super::{branch_name, encode_status_json};

    #[test]
    fn names_attached_and_detached_head_states() {
        assert_eq!(branch_name(true, b"main\n"), b"main");
        assert_eq!(branch_name(false, b""), b"HEAD");
    }

    #[test]
    fn encodes_empty_and_multiple_status_records_deterministically() {
        assert_eq!(
            encode_status_json(b"main", b"").unwrap(),
            b"{\"branch\":\"main\",\"changes\":[]}\n"
        );
        assert_eq!(
            encode_status_json(b"feature/\"json\"", b" M main.orna\0?? new.orna\0").unwrap(),
            b"{\"branch\":\"feature/\\\"json\\\"\",\"changes\":[{\"index\":\" \",\"worktree\":\"M\",\"path\":\"main.orna\"},{\"index\":\"?\",\"worktree\":\"?\",\"path\":\"new.orna\"}]}\n"
        );
    }

    #[test]
    fn escapes_json_characters_and_preserves_rename_source_path() {
        assert_eq!(
            encode_status_json(b"main", b"R  new\\\"name\0old\nname\0").unwrap(),
            b"{\"branch\":\"main\",\"changes\":[{\"index\":\"R\",\"worktree\":\" \",\"path\":\"new\\\\\\\"name\",\"original_path\":\"old\\nname\"}]}\n"
        );
    }

    #[test]
    fn rejects_malformed_and_incomplete_rename_records() {
        assert!(encode_status_json(b"main", b"bad\0").is_none());
        assert!(encode_status_json(b"main", b"R  new.orna\0").is_none());
    }
}
