use std::{ffi::OsStr, sync::Arc};

use orna_value::Raw;

use super::{
    FsBuilder, NestedDirectory, NodeKind, ROOT_INODE, ROW_PAGE_LIMIT, has_nested_children,
    nested_entries, nested_entry, nested_leaf_text, nested_value_at,
};

fn text(value: &str) -> Raw {
    Raw::Text(value.to_owned())
}

fn record(fields: &[(&str, Raw)]) -> Raw {
    let entries = fields
        .iter()
        .map(|(name, value)| Raw::Array(vec![text(name), value.clone()]))
        .collect();
    Raw::Tag(
        60009,
        Box::new(Raw::Array(vec![Raw::Null, Raw::Array(entries)])),
    )
}

fn optional(present: bool, payload: Raw) -> Raw {
    let index = i32::from(present);
    Raw::Tag(
        60013,
        Box::new(Raw::Array(vec![Raw::Int(index.into()), payload])),
    )
}

fn names(value: &Raw) -> Option<Vec<String>> {
    let entries = nested_entries(value)?;
    (0..entries.child_count())
        .map(|index| nested_entry(value, index).map(|(name, _)| name.project(index)))
        .collect()
}

#[test]
fn record_fields_are_listed_under_escaped_names() {
    let value = record(&[("a b", text("x")), ("n", Raw::Int(2.into()))]);
    let expected = vec![
        orna_runtime::vfs::project_field_name("a b"),
        orna_runtime::vfs::project_field_name("n"),
    ];
    assert_eq!(names(&value), Some(expected));
}

#[test]
fn tuples_and_lists_are_listed_by_position() {
    let tuple = Raw::Tag(60015, Box::new(Raw::Array(vec![text("p"), text("q")])));
    let list = Raw::Array(vec![text("p"), text("q"), text("r")]);
    assert_eq!(names(&tuple), Some(vec!["0".into(), "1".into()]));
    assert_eq!(names(&list), Some(vec!["0".into(), "1".into(), "2".into()]));
}

#[test]
fn text_keyed_maps_list_escaped_keys() {
    let map = Raw::Map(vec![(text("k/1"), text("v"))]);
    let expected = vec![orna_runtime::vfs::project_component("k/1")];
    assert_eq!(names(&map), Some(expected));
}

#[test]
fn non_text_map_keys_and_scalars_have_no_directory() {
    let map = Raw::Map(vec![(Raw::Int(1.into()), text("v"))]);
    assert_eq!(names(&map), None);
    assert_eq!(names(&text("plain")), None);
    assert_eq!(names(&Raw::Null), None);
}

#[test]
fn present_optional_projects_its_payload_and_absent_has_no_children() {
    let present = optional(true, record(&[("a", text("x"))]));
    let absent = optional(false, Raw::Null);
    assert_eq!(
        names(&present),
        Some(vec![orna_runtime::vfs::project_field_name("a")])
    );
    assert_eq!(names(&absent), None);
}

#[test]
fn nested_value_at_follows_positions_from_the_root() {
    let root = record(&[("outer", record(&[("inner", text("leaf"))]))]);
    let value = nested_value_at(&root, &[0, 0]).expect("the inner field exists");
    assert_eq!(nested_leaf_text(value), Ok("leaf".to_owned()));
    assert!(nested_value_at(&root, &[1]).is_none());
}

#[test]
fn leaf_text_is_unquoted_and_absent_optionals_read_as_null() {
    assert_eq!(nested_leaf_text(&text("plain")), Ok("plain".to_owned()));
    assert_eq!(
        nested_leaf_text(&optional(false, Raw::Null)),
        Ok("null".to_owned())
    );
    assert_eq!(
        nested_leaf_text(&optional(true, Raw::Int(7.into()))),
        Ok("7".to_owned())
    );
}

#[test]
fn has_nested_children_agrees_with_the_listing() {
    let cases = [
        record(&[("a", text("x"))]),
        Raw::Tag(60015, Box::new(Raw::Array(vec![text("p")]))),
        Raw::Array(Vec::new()),
        Raw::Map(vec![(text("k"), text("v"))]),
        Raw::Map(vec![(Raw::Int(1.into()), text("v"))]),
        optional(true, record(&[("a", text("x"))])),
        optional(false, Raw::Null),
        text("plain"),
    ];
    for value in &cases {
        assert_eq!(has_nested_children(value), names(value).is_some());
    }
}

#[test]
fn a_large_list_is_indexed_without_listing_it() {
    let items = (0_i64..100_000)
        .map(|index| Raw::Int(index.into()))
        .collect();
    let list = Raw::Array(items);
    assert!(has_nested_children(&list));
    assert_eq!(
        nested_value_at(&list, &[99_999]),
        Some(&Raw::Int(99_999_i64.into()))
    );
    assert!(nested_value_at(&list, &[100_000]).is_none());
}

#[test]
fn a_nested_list_is_paged_by_lookup() {
    let mut builder = FsBuilder::new();
    let items: Vec<Raw> = (0_i64..600).map(|index| Raw::Int(index.into())).collect();
    let root = Arc::new(Raw::Array(items));
    let list = builder
        .add_file(
            ROOT_INODE,
            "list",
            NodeKind::NestedDirectory(NestedDirectory::new(root, Vec::new())),
        )
        .expect("the list directory is added");
    assert!(matches!(
        builder.lookup_child(list, OsStr::new("0")),
        Ok(Some(_))
    ));
    assert_eq!(
        listed(&builder, list),
        (ROW_PAGE_LIMIT, false, ROW_PAGE_LIMIT)
    );
    assert!(matches!(
        builder.lookup_child(list, OsStr::new("599")),
        Ok(Some(_))
    ));
    assert_eq!(listed(&builder, list), (600, true, 600));
}

/// (next page index, exhausted, listed entries) of a nested directory.
fn listed(builder: &FsBuilder, inode: u64) -> (usize, bool, usize) {
    let kind = builder.nodes.get(&inode).map(|node| &node.kind);
    let Some(NodeKind::NestedDirectory(nested)) = kind else {
        panic!("the list is a nested directory");
    };
    (
        nested.next_index,
        nested.exhausted,
        nested.directory.entries.len(),
    )
}
