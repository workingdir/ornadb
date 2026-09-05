//! A small in-memory table transaction primitive.
//!
//! Committed rows remain owned by [`TableRuntime`]. An activation holds only a
//! private overlay of inserts, replacements, and deletions. Nested ordinary
//! calls borrow that same activation through [`ChildScope`], so they can read
//! their parent's writes but cannot publish independently. Only the root
//! [`Activation`] can publish its overlay.

use std::collections::BTreeMap;

/// The committed relation for one table.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableRuntime<Key, Row> {
    committed: BTreeMap<Key, Row>,
}

impl<Key, Row> TableRuntime<Key, Row>
where
    Key: Clone + Ord,
    Row: Clone,
{
    /// Starts a root activation with an empty, private mutation overlay.
    pub fn begin(&mut self) -> Activation<'_, Key, Row> {
        Activation {
            runtime: self,
            overlay: BTreeMap::new(),
            state: ActivationState::Open,
        }
    }

    /// Returns the currently committed row for a key.
    pub fn committed(&self, key: &Key) -> Option<&Row> {
        self.committed.get(key)
    }

    /// Returns the complete committed relation.
    pub fn committed_rows(&self) -> &BTreeMap<Key, Row> {
        &self.committed
    }
}

/// An error raised by a table mutation or transaction lifecycle operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableError {
    /// A mutation expected an absent key, but the candidate relation has one.
    DuplicateKey,
    /// A mutation expected a row in the candidate relation, but none exists.
    MissingRow,
    /// A nested ordinary call tried to publish an activation it does not own.
    ChildCannotCommit,
    /// The root activation has already committed.
    DoubleCommit,
    /// A mutation or read was attempted after the activation closed.
    UseAfterClose,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationState {
    Open,
    Committed,
    RolledBack,
}

/// The root owner of a table activation.
pub struct Activation<'runtime, Key, Row> {
    runtime: &'runtime mut TableRuntime<Key, Row>,
    /// `Some(row)` is an insert or replacement; `None` is a deletion.
    overlay: BTreeMap<Key, Option<Row>>,
    state: ActivationState,
}

impl<'runtime, Key, Row> Activation<'runtime, Key, Row>
where
    Key: Clone + Ord,
    Row: Clone,
{
    /// Borrows this activation for one nested ordinary call.
    pub fn child<'scope>(
        &'scope mut self,
    ) -> Result<ChildScope<'scope, 'runtime, Key, Row>, TableError> {
        self.require_open()?;
        Ok(ChildScope { activation: self })
    }

    /// Stages an insert. The committed relation remains unchanged until commit.
    pub fn insert(&mut self, key: Key, row: Row) -> Result<(), TableError> {
        self.require_open()?;
        if self.candidate(&key).is_some() {
            return Err(TableError::DuplicateKey);
        }
        self.overlay.insert(key, Some(row));
        Ok(())
    }

    /// Stages a replacement of an existing candidate row.
    pub fn update(&mut self, key: Key, row: Row) -> Result<(), TableError> {
        self.require_open()?;
        if self.candidate(&key).is_none() {
            return Err(TableError::MissingRow);
        }
        self.overlay.insert(key, Some(row));
        Ok(())
    }

    /// Stages deletion of an existing candidate row.
    pub fn delete(&mut self, key: Key) -> Result<(), TableError> {
        self.require_open()?;
        if self.candidate(&key).is_none() {
            return Err(TableError::MissingRow);
        }
        self.overlay.insert(key, None);
        Ok(())
    }

    /// Reads the candidate relation, including this activation's own writes.
    pub fn read(&self, key: &Key) -> Result<Option<&Row>, TableError> {
        self.require_open()?;
        Ok(self.candidate(key))
    }

    /// Materialises the unpublished candidate relation for validation.
    pub fn candidate_rows(&self) -> Result<BTreeMap<Key, Row>, TableError> {
        self.require_open()?;
        let mut candidate = self.runtime.committed.clone();
        for (key, mutation) in &self.overlay {
            match mutation {
                Some(row) => {
                    candidate.insert(key.clone(), row.clone());
                }
                None => {
                    candidate.remove(key);
                }
            }
        }
        Ok(candidate)
    }

    /// Publishes every staged change together. Nested scopes cannot call this.
    pub fn commit(&mut self) -> Result<(), TableError> {
        match self.state {
            ActivationState::Committed => return Err(TableError::DoubleCommit),
            ActivationState::RolledBack => return Err(TableError::UseAfterClose),
            ActivationState::Open => {}
        }
        for (key, mutation) in &self.overlay {
            match mutation {
                Some(row) => {
                    self.runtime.committed.insert(key.clone(), row.clone());
                }
                None => {
                    self.runtime.committed.remove(key);
                }
            }
        }
        self.state = ActivationState::Committed;
        Ok(())
    }

    /// Discards every staged change. The operation is idempotent for cleanup.
    pub fn rollback(&mut self) {
        if self.state == ActivationState::Open {
            self.overlay.clear();
            self.state = ActivationState::RolledBack;
        }
    }

    fn require_open(&self) -> Result<(), TableError> {
        if self.state == ActivationState::Open {
            Ok(())
        } else {
            Err(TableError::UseAfterClose)
        }
    }

    fn candidate(&self, key: &Key) -> Option<&Row> {
        self.overlay.get(key).and_then(Option::as_ref).or_else(|| {
            if self.overlay.contains_key(key) {
                None
            } else {
                self.runtime.committed.get(key)
            }
        })
    }
}

/// A nested ordinary call executing in its parent's activation.
pub struct ChildScope<'scope, 'runtime, Key, Row> {
    activation: &'scope mut Activation<'runtime, Key, Row>,
}

impl<'scope, 'runtime, Key, Row> ChildScope<'scope, 'runtime, Key, Row>
where
    Key: Clone + Ord,
    Row: Clone,
{
    /// Borrows the same root activation again for a deeper ordinary call.
    pub fn child<'child>(
        &'child mut self,
    ) -> Result<ChildScope<'child, 'runtime, Key, Row>, TableError> {
        self.activation.child()
    }

    pub fn insert(&mut self, key: Key, row: Row) -> Result<(), TableError> {
        self.activation.insert(key, row)
    }

    pub fn update(&mut self, key: Key, row: Row) -> Result<(), TableError> {
        self.activation.update(key, row)
    }

    pub fn delete(&mut self, key: Key) -> Result<(), TableError> {
        self.activation.delete(key)
    }

    pub fn read(&self, key: &Key) -> Result<Option<&Row>, TableError> {
        self.activation.read(key)
    }

    /// Nested calls have no publication capability.
    pub fn commit(&mut self) -> Result<(), TableError> {
        Err(TableError::ChildCannotCommit)
    }
}

#[cfg(test)]
mod tests {
    use super::{TableError, TableRuntime};

    #[test]
    fn nested_insert_then_root_rollback_leaves_committed_relation_empty() {
        let mut table = TableRuntime::<u64, String>::default();
        let mut root = table.begin();
        root.child().unwrap().insert(1, "note".into()).unwrap();
        root.rollback();

        assert_eq!(table.committed(&1), None);
    }

    #[test]
    fn nested_insert_then_root_commit_publishes_the_row_once() {
        let mut table = TableRuntime::<u64, String>::default();
        let mut root = table.begin();
        root.child().unwrap().insert(1, "note".into()).unwrap();
        root.commit().unwrap();

        assert_eq!(table.committed(&1).map(String::as_str), Some("note"));
        assert_eq!(table.committed_rows().len(), 1);
    }

    #[test]
    fn activation_reads_its_own_private_overlay() {
        let mut table = TableRuntime::<u64, String>::default();
        let mut root = table.begin();
        root.insert(1, "staged".into()).unwrap();

        assert_eq!(root.read(&1).unwrap().map(String::as_str), Some("staged"));
        assert_eq!(root.candidate_rows().unwrap().len(), 1);
    }

    #[test]
    fn child_cannot_commit_or_publish_parent_overlay() {
        let mut table = TableRuntime::<u64, String>::default();
        {
            let mut root = table.begin();
            {
                let mut child = root.child().unwrap();
                child.insert(1, "note".into()).unwrap();
                assert_eq!(child.commit(), Err(TableError::ChildCannotCommit));
            }
            assert_eq!(root.read(&1).unwrap().map(String::as_str), Some("note"));
        }
        assert_eq!(table.committed(&1), None);
    }

    #[test]
    fn root_reports_double_commit_and_use_after_close() {
        let mut table = TableRuntime::<u64, String>::default();
        let mut root = table.begin();
        root.commit().unwrap();

        assert_eq!(root.commit(), Err(TableError::DoubleCommit));
        assert_eq!(root.read(&1), Err(TableError::UseAfterClose));
    }
}
