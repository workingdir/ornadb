use std::ffi::{OsStr, OsString};

use super::{Directory, FileType};

/// A directory whose entries sit in the given order, laid out as `insert_node` does.
fn directory_of(entries: &[(u64, &str)]) -> Directory {
    let mut directory = Directory::new();
    for &(inode, name) in entries {
        directory.push(OsString::from(name), inode, FileType::RegularFile);
    }
    directory
}

fn names(directory: &Directory) -> Vec<&str> {
    directory
        .entries
        .iter()
        .map(|entry| entry.name.to_str().expect("test names are UTF-8"))
        .collect()
}

#[test]
fn removing_an_entry_keeps_sibling_readdir_positions() {
    let mut directory = directory_of(&[(2, "a"), (3, "b"), (4, "c"), (5, "d")]);
    let removed = directory
        .remove_entry(OsStr::new("b"))
        .expect("b is present before removal");
    assert_eq!(removed.inode, 3);
    assert_eq!(
        names(&directory),
        ["a", "c", "d"],
        "siblings keep their readdir order"
    );
    assert_eq!(
        directory.entry(OsStr::new("c")).map(|entry| entry.inode),
        Some(4)
    );
    assert_eq!(
        directory.entry(OsStr::new("d")).map(|entry| entry.inode),
        Some(5)
    );
    assert!(directory.entry(OsStr::new("b")).is_none());
}

#[test]
fn renaming_an_entry_keeps_its_readdir_position() {
    let mut directory = directory_of(&[(2, "a"), (3, "b"), (4, "c")]);
    assert!(directory.rename_entry(OsStr::new("b"), OsStr::new("b.mp1")));
    assert_eq!(
        names(&directory),
        ["a", "b.mp1", "c"],
        "the renamed entry keeps its index"
    );
    assert_eq!(
        directory
            .entry(OsStr::new("b.mp1"))
            .map(|entry| entry.inode),
        Some(3)
    );
    assert!(directory.entry(OsStr::new("b")).is_none());
}

#[test]
fn renaming_onto_a_taken_name_leaves_the_directory_unchanged() {
    let mut directory = directory_of(&[(2, "a"), (3, "b")]);
    assert!(!directory.rename_entry(OsStr::new("a"), OsStr::new("b")));
    assert_eq!(names(&directory), ["a", "b"]);
    assert_eq!(
        directory.entry(OsStr::new("a")).map(|entry| entry.inode),
        Some(2)
    );
    assert_eq!(
        directory.entry(OsStr::new("b")).map(|entry| entry.inode),
        Some(3)
    );
}
