//! `orna history <relation-hex> <key>`: lists the revision history of one
//! committed row through the OGS-1 commit-graph walk. Only commit headers and
//! tree listings are read; no blob payload is opened or hydrated.

use super::*;
use orna_repository_v1::{Repository, TypedKey};

/// Most revisions one history listing reports, newest first.
const HISTORY_LIMIT: usize = 64;

pub(super) fn run(arguments: &[String]) -> Result<(), Diagnostic> {
    let [relation, key] = arguments else {
        return Err(history_error(
            "History expects a relation and a row key",
            "usage: orna history <relation-hex> <key>",
        ));
    };
    let relation = parse_relation_id(relation)?;
    let repository = Repository::discover(
        std::env::current_dir()
            .map_err(|error| history_error("Current directory is unavailable", error.to_string()))?,
    )
    .map_err(|error| history_error("Repository could not be opened", format!("{error:?}")))?;
    let format = repository
        .open_format_context()
        .map_err(|error| history_error("Format context could not be opened", format!("{error:?}")))?;
    let row_map = format
        .load_row_map(relation)
        .map_err(|error| history_error("Row map could not be loaded", format!("{error:?}")))?;
    let graph = format
        .open_native_graph(&row_map)
        .map_err(|error| history_error("Native graph could not be opened", format!("{error:?}")))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| history_error("Read scope could not be opened", format!("{error:?}")))?;
    let row = graph
        .lookup_row(&TypedKey::Text(key.clone()), &scope)
        .map_err(|error| history_error("Row lookup failed", format!("{error:?}")))?
        .ok_or_else(|| {
            history_error("Row is not committed", format!("no row with key {key:?}"))
        })?;
    let revisions = graph
        .list_row_revisions(&row, HISTORY_LIMIT, &scope)
        .map_err(|error| history_error("Revision history could not be listed", format!("{error:?}")))?;
    for revision in revisions {
        let state = if revision.present() { "present" } else { "absent" };
        println!(
            "{} {} {state}",
            revision.commit().to_hex(),
            revision.tree().to_hex()
        );
    }
    Ok(())
}

/// Parses a 32-digit hexadecimal relation id into its 16 raw bytes.
fn parse_relation_id(value: &str) -> Result<[u8; 16], Diagnostic> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(history_error(
            "Relation id is not 32 hexadecimal digits",
            format!("got {value:?}"),
        ));
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .expect("hexadecimal digits were checked above");
    }
    Ok(bytes)
}

fn history_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna history <relation-hex> <key>` inside an initialized repository",
        detail.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::parse_relation_id;

    #[test]
    fn relation_id_parses_sixteen_bytes_and_rejects_bad_input() {
        let parsed = parse_relation_id("000102030405060708090a0b0c0d0e0f").unwrap();
        assert_eq!(parsed[0], 0x00);
        assert_eq!(parsed[15], 0x0f);
        assert!(parse_relation_id("0123").is_err());
        assert!(parse_relation_id(&"zz".repeat(16)).is_err());
    }
}
