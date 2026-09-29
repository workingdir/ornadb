use std::{fs, path::PathBuf};

use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_sys_v1::system_function_descriptor;
use serde_json::Value;

const REPOSITORY_SYS_API: &str = include_str!("../../../api/sys.json");
const SYS_META_SOURCE: &str = include_str!("fixtures/sys-api-audit-meta.orna");

fn named_entry<'a>(entries: &'a [Value], name: &str) -> &'a Value {
    entries
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing declared entry {name}"))
}

#[test]
fn sys_meta_contract_is_accepted_semantically_but_lacks_sys_v1_descriptor() {
    let reference_dir = PathBuf::from(
        std::env::var_os("ORNA_REFERENCE_DIR").expect("frozen reference directory is set"),
    );
    let frozen_bytes = fs::read(reference_dir.join("api/sys.json"))
        .expect("read frozen system API contract");
    let frozen: Value = serde_json::from_slice(&frozen_bytes).expect("frozen sys API JSON");
    let repository: Value =
        serde_json::from_str(REPOSITORY_SYS_API).expect("repository sys API JSON");

    let frozen_meta = named_entry(frozen["functions"].as_array().unwrap(), "sys.meta");
    let repository_meta = named_entry(repository["functions"].as_array().unwrap(), "sys.meta");
    assert_eq!(frozen_meta["signature"], "fn sys.meta<T>(value: T): sys.ValueMetadata<T>");
    assert_eq!(frozen_meta["effect"], "read");
    assert_eq!(repository_meta["signature"], frozen_meta["signature"]);
    assert_eq!(repository_meta["effect"], frozen_meta["effect"]);

    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", SYS_META_SOURCE)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "semantic admission: {:?}", analysis.diagnostics);
    assert!(
        system_function_descriptor("sys.meta").is_none(),
        "sys.meta has no executable descriptor in the sys-v1 intrinsic registry"
    );

    let frozen_type_count = frozen["counts"]["value_types"].as_u64().unwrap();
    let repository_type_count = repository["counts"]["value_types"].as_u64().unwrap();
    assert_eq!(frozen_type_count, 34);
    assert_eq!(repository_type_count, 35);
    named_entry(
        repository["value_types"].as_array().unwrap(),
        "sys.PublicationPolicy",
    );
    assert!(frozen["value_types"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["name"] != "sys.PublicationPolicy"));
}
