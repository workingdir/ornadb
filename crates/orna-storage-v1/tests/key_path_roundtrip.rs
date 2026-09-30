use orna_storage_v1::{LoosePath, LooseRow};
use orna_value_v1::path_decode_key_components;

const ROW: &str = include_str!("fixtures/key-path-roundtrip-row.orna");

fn encoded_components(path: &LoosePath, table: &str) -> Vec<String> {
    path.as_managed_path()
        .as_path()
        .to_str()
        .expect("encoded managed paths are UTF-8")
        .strip_prefix(&format!("{table}/"))
        .expect("path has the requested table root")
        .split('/')
        .map(str::to_owned)
        .collect()
}

#[test]
fn fixture_row_round_trips_reserved_unicode_and_nested_key_components() {
    let keys = [
        "",
        "a/b",
        "é🛰",
        "e\u{301}",
        "nul\0line\n",
        "CON.txt",
        ".git.",
        "CLOCK$",
        "leaf.orna",
    ]
    .map(str::to_owned);
    let path = LoosePath::for_key("Entry", &keys).expect("logical keys have portable paths");
    assert_eq!(
        path.as_managed_path().as_path().to_str().unwrap(),
        "Entry/~ff/a~2fb/~c3~a9~f0~9f~9b~b0/e~cc~81/nul~00line~0a/~43ON.txt/~2egit~2e/~43LOCK~24/leaf.orna.orna"
    );

    let components = encoded_components(&path, "Entry");
    assert_eq!(path_decode_key_components(&components).unwrap(), keys);
    assert_eq!(
        LoosePath::from_encoded_key("Entry", &components).unwrap(),
        path
    );

    let row = LooseRow::new(ROW.as_bytes().to_vec()).expect("fixture is a bounded row body");
    assert_eq!(row.bytes(), ROW.as_bytes());
}

#[test]
fn key_path_component_and_table_relative_limits_round_trip_at_the_boundary() {
    let mut keys = vec!["a".repeat(200); 5];
    keys.push("b".repeat(14));
    let path = LoosePath::for_key("Boundary", &keys).expect("1024-byte key path is admitted");
    let components = encoded_components(&path, "Boundary");
    assert_eq!(components.join("/").len(), 1024);
    assert_eq!(
        path_decode_key_components(&components).unwrap(),
        keys
    );

    keys[5].push('b');
    assert!(LoosePath::for_key("Boundary", &keys).is_err());
    assert!(LoosePath::for_key("Boundary", &["x".repeat(201)]).is_err());
}

#[test]
fn discovered_paths_reject_aliases_and_reserved_empty_sentinel_misuse() {
    for component in ["~61lice.orna", "~FF.orna", "x~ff.orna", "~e.orna"] {
        assert!(LoosePath::from_encoded_key("Entry", &[component.into()]).is_err());
    }
    assert!(LoosePath::from_encoded_key("Entry", &["..".into(), "row.orna".into()]).is_err());
}
