//! Gate E projection edge cases that run on the builder alone: long text keys,
//! and a generated table component that a source path already occupies. No
//! FUSE mount is involved.

use std::{collections::BTreeSet, ffi::OsStr};

use orna_check::ModuleInput;
use orna_repository::TypedKey;
use orna_vfs::project_key_component;

use super::{FsBuilder, NodeKind, ROOT_INODE, key_component, key_directory};

fn source_module(logical_path: &str) -> ModuleInput {
    ModuleInput {
        logical_path: logical_path.to_owned(),
        source: String::new(),
        prelude_exports: BTreeSet::new(),
    }
}

#[test]
fn long_text_keys_sharing_a_prefix_take_distinct_digest_aliases() {
    // A text key over the component bound takes the `~key-` digest alias of its
    // canonical encoding. Two keys that differ only past the bound must not share
    // one row directory (VFS-016).
    let prefix = "k".repeat(250);
    let first = TypedKey::Text(format!("{prefix}a"));
    let second = TypedKey::Text(format!("{prefix}b"));

    let first_component = key_component(&first).expect("first key component");
    let second_component = key_component(&second).expect("second key component");
    assert!(first_component.starts_with("~key-"));
    assert!(second_component.starts_with("~key-"));
    assert_ne!(first_component, second_component);

    // The builder nests both keys under one table as distinct key directories.
    let mut builder = FsBuilder::new();
    let table = builder.table_directory("Music", "Music").expect("table");
    let first_row = key_directory(&mut builder, table, &first).expect("first row");
    let second_row = key_directory(&mut builder, table, &second).expect("second row");
    assert_ne!(first_row, second_row);

    // Re-projecting the same long key reuses its directory instead of colliding.
    let again = key_directory(&mut builder, table, &first).expect("repeat row");
    assert_eq!(again, first_row);

    // The alias is the digest of the typed key's canonical encoding, so the same
    // long text under another key type cannot claim its host name.
    let text_bytes = first.canonical_bytes().expect("canonical encoding");
    assert_eq!(
        first_component,
        project_key_component(&format!("{prefix}a"), &text_bytes)
    );
}

#[test]
fn generated_table_component_escapes_when_a_source_path_holds_the_plain_name() {
    // A source file named `Music` occupies the plain table name, so the generated
    // table takes its escaped spelling and stays projected (profiles/vfs-1.md:31-33).
    let mut builder = FsBuilder::new();
    builder
        .add_source_modules(&[source_module("Music")])
        .expect("source module");
    let table = builder.table_directory("", "Music").expect("table");

    let NodeKind::Directory(root) = &builder.nodes.get(&ROOT_INODE).unwrap().kind else {
        panic!("root is a directory");
    };
    let escaped = root
        .entry(OsStr::new("%4Dusic"))
        .expect("escaped table entry");
    assert_eq!(escaped.inode, table);
    assert!(matches!(
        builder.nodes.get(&table).unwrap().kind,
        NodeKind::Directory(_)
    ));
    // The source file keeps its plain name.
    let source = root.entry(OsStr::new("Music")).expect("source entry");
    assert!(matches!(
        builder.nodes.get(&source.inode).unwrap().kind,
        NodeKind::Bytes(_)
    ));
}

#[test]
fn generated_table_component_is_refused_when_source_paths_hold_both_spellings() {
    // When the plain and escaped spellings are both source paths, the generated
    // table has no free host name and the projection is refused, not merged.
    let mut builder = FsBuilder::new();
    builder
        .add_source_modules(&[source_module("Music"), source_module("%4Dusic")])
        .expect("source modules");
    let error = builder
        .table_directory("", "Music")
        .expect_err("both spellings are occupied");
    assert!(error.contains("collides with a source path"), "{error}");
}
