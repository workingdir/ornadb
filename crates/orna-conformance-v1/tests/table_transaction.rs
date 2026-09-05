use orna_table_v1::{TableError, TableRuntime};

/// ORNA-TXN-001/002/003 at the relation boundary: ordinary child work is
/// visible to the root activation, but only the root can publish it.
#[test]
fn nested_child_write_rolls_back_when_the_root_propagates_an_error() {
    let mut notes = TableRuntime::<u64, &'static str>::default();
    let mut root = notes.begin();

    let child_result: Result<(), TableError> = (|| {
        let mut child = root.child()?;
        child.insert(7, "nested")?;
        Err(TableError::UseAfterClose)
    })();

    assert_eq!(child_result, Err(TableError::UseAfterClose));
    root.rollback();
    assert_eq!(notes.committed(&7), None);
}

#[test]
fn successful_root_commit_publishes_all_staged_rows_together() {
    let mut notes = TableRuntime::<u64, &'static str>::default();
    let mut root = notes.begin();
    root.insert(1, "first").unwrap();
    root.child().unwrap().insert(2, "second").unwrap();

    assert_eq!(root.read(&1).unwrap(), Some(&"first"));
    assert_eq!(root.read(&2).unwrap(), Some(&"second"));
    root.commit().unwrap();

    assert_eq!(
        notes.committed_rows(),
        &[(1, "first"), (2, "second")].into_iter().collect()
    );
}

#[test]
fn nested_scope_cannot_publish_or_outlive_the_root_activation() {
    let mut notes = TableRuntime::<u64, &'static str>::default();
    let mut root = notes.begin();
    {
        let mut child = root.child().unwrap();
        assert_eq!(child.commit(), Err(TableError::ChildCannotCommit));
        child.insert(1, "nested").unwrap();
    }

    assert_eq!(root.read(&1).unwrap(), Some(&"nested"));
    root.rollback();
    assert_eq!(notes.committed_rows().len(), 0);
}
