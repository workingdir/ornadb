//! A small in-memory table transaction primitive.
//!
//! Committed rows remain owned by [`TableRuntime`]. An activation holds only a
//! private overlay of inserts, replacements, and deletions. Nested ordinary
//! calls borrow that same activation through [`ChildScope`], so they can read
//! their parent's writes but cannot publish independently. Only the root
//! [`Activation`] can publish its overlay.

use std::collections::BTreeMap;

/// The committed relation for one table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRuntime<Key, Row> {
    committed: BTreeMap<Key, Row>,
}

impl<Key, Row> Default for TableRuntime<Key, Row> {
    fn default() -> Self {
        Self {
            committed: BTreeMap::new(),
        }
    }
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

    /// Runs one activation and publishes its overlay only when the operation
    /// succeeds. Returned errors roll the overlay back; a panic also leaves
    /// the committed relation untouched because publication happens last.
    pub fn activate<T, E, F>(&mut self, operation: F) -> Result<T, ActivationError<E>>
    where
        F: FnOnce(&mut Activation<'_, Key, Row>) -> Result<T, E>,
    {
        let mut activation = self.begin();
        match operation(&mut activation) {
            Ok(value) => activation
                .commit()
                .map(|()| value)
                .map_err(ActivationError::Commit),
            Err(error) => {
                activation.rollback();
                Err(ActivationError::Operation(error))
            }
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

/// The result of an activation closure or its root publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationError<E> {
    Operation(E),
    Commit(TableError),
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

/// Committed relations for one in-memory database.
///
/// A [`DatabaseActivation`] owns a single private overlay spanning every
/// relation it changes. Consequently no relation becomes visible before the
/// root publishes the complete overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseRuntime<Table, Key, Row> {
    committed: BTreeMap<Table, BTreeMap<Key, Row>>,
}

impl<Table, Key, Row> Default for DatabaseRuntime<Table, Key, Row> {
    fn default() -> Self {
        Self {
            committed: BTreeMap::new(),
        }
    }
}

impl<Table, Key, Row> DatabaseRuntime<Table, Key, Row>
where
    Table: Clone + Ord,
    Key: Clone + Ord,
    Row: Clone,
{
    /// Starts a root activation spanning all named relations.
    pub fn begin(&mut self) -> DatabaseActivation<'_, Table, Key, Row> {
        DatabaseActivation {
            runtime: self,
            overlay: BTreeMap::new(),
            state: ActivationState::Open,
        }
    }

    /// Runs one database activation and publishes all relation overlays only
    /// when the operation returns successfully.
    pub fn activate<T, E, F>(&mut self, operation: F) -> Result<T, ActivationError<E>>
    where
        F: FnOnce(&mut DatabaseActivation<'_, Table, Key, Row>) -> Result<T, E>,
    {
        let mut activation = self.begin();
        match operation(&mut activation) {
            Ok(value) => activation
                .commit()
                .map(|()| value)
                .map_err(ActivationError::Commit),
            Err(error) => {
                activation.rollback();
                Err(ActivationError::Operation(error))
            }
        }
    }

    /// Returns the committed row for one relation/key pair.
    pub fn committed(&self, table: &Table, key: &Key) -> Option<&Row> {
        self.committed.get(table)?.get(key)
    }

    /// Returns one committed relation, if it has published rows.
    pub fn committed_rows(&self, table: &Table) -> Option<&BTreeMap<Key, Row>> {
        self.committed.get(table)
    }
}

/// Root transaction owner for all relations in a [`DatabaseRuntime`].
pub struct DatabaseActivation<'runtime, Table, Key, Row> {
    runtime: &'runtime mut DatabaseRuntime<Table, Key, Row>,
    overlay: BTreeMap<Table, BTreeMap<Key, Option<Row>>>,
    state: ActivationState,
}

impl<'runtime, Table, Key, Row> DatabaseActivation<'runtime, Table, Key, Row>
where
    Table: Clone + Ord,
    Key: Clone + Ord,
    Row: Clone,
{
    /// Borrows the same database activation for a nested ordinary call.
    pub fn child<'scope>(
        &'scope mut self,
    ) -> Result<DatabaseChildScope<'scope, 'runtime, Table, Key, Row>, TableError> {
        self.require_open()?;
        Ok(DatabaseChildScope { activation: self })
    }

    /// Stages a new row in one relation.
    pub fn insert(&mut self, table: Table, key: Key, row: Row) -> Result<(), TableError> {
        self.require_open()?;
        if self.candidate(&table, &key).is_some() {
            return Err(TableError::DuplicateKey);
        }
        self.overlay
            .entry(table)
            .or_default()
            .insert(key, Some(row));
        Ok(())
    }

    /// Stages a replacement of one existing candidate row.
    pub fn update(&mut self, table: Table, key: Key, row: Row) -> Result<(), TableError> {
        self.require_open()?;
        if self.candidate(&table, &key).is_none() {
            return Err(TableError::MissingRow);
        }
        self.overlay
            .entry(table)
            .or_default()
            .insert(key, Some(row));
        Ok(())
    }

    /// Stages deletion of one existing candidate row.
    pub fn delete(&mut self, table: Table, key: Key) -> Result<(), TableError> {
        self.require_open()?;
        if self.candidate(&table, &key).is_none() {
            return Err(TableError::MissingRow);
        }
        self.overlay.entry(table).or_default().insert(key, None);
        Ok(())
    }

    /// Reads the candidate relation, including all writes staged by this root.
    pub fn read(&self, table: &Table, key: &Key) -> Result<Option<&Row>, TableError> {
        self.require_open()?;
        Ok(self.candidate(table, key))
    }

    /// Materialises one unpublished candidate relation for validation.
    pub fn candidate_rows(&self, table: &Table) -> Result<BTreeMap<Key, Row>, TableError> {
        self.require_open()?;
        let mut candidate = self
            .runtime
            .committed
            .get(table)
            .cloned()
            .unwrap_or_default();
        if let Some(overlay) = self.overlay.get(table) {
            for (key, mutation) in overlay {
                match mutation {
                    Some(row) => {
                        candidate.insert(key.clone(), row.clone());
                    }
                    None => {
                        candidate.remove(key);
                    }
                }
            }
        }
        Ok(candidate)
    }

    /// Publishes the overlays of every changed relation together.
    pub fn commit(&mut self) -> Result<(), TableError> {
        match self.state {
            ActivationState::Committed => return Err(TableError::DoubleCommit),
            ActivationState::RolledBack => return Err(TableError::UseAfterClose),
            ActivationState::Open => {}
        }
        for (table, overlay) in &self.overlay {
            let relation = self.runtime.committed.entry(table.clone()).or_default();
            for (key, mutation) in overlay {
                match mutation {
                    Some(row) => {
                        relation.insert(key.clone(), row.clone());
                    }
                    None => {
                        relation.remove(key);
                    }
                }
            }
        }
        self.state = ActivationState::Committed;
        Ok(())
    }

    /// Discards every relation overlay. Repeated cleanup is harmless.
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

    fn candidate(&self, table: &Table, key: &Key) -> Option<&Row> {
        match self
            .overlay
            .get(table)
            .and_then(|relation| relation.get(key))
        {
            Some(Some(row)) => Some(row),
            Some(None) => None,
            None => self.runtime.committed.get(table)?.get(key),
        }
    }
}

/// A nested ordinary call borrowing a [`DatabaseActivation`].
pub struct DatabaseChildScope<'scope, 'runtime, Table, Key, Row> {
    activation: &'scope mut DatabaseActivation<'runtime, Table, Key, Row>,
}

impl<'scope, 'runtime, Table, Key, Row> DatabaseChildScope<'scope, 'runtime, Table, Key, Row>
where
    Table: Clone + Ord,
    Key: Clone + Ord,
    Row: Clone,
{
    /// Creates a deeper nested ordinary call in the same root activation.
    pub fn child<'child>(
        &'child mut self,
    ) -> Result<DatabaseChildScope<'child, 'runtime, Table, Key, Row>, TableError> {
        self.activation.child()
    }

    pub fn insert(&mut self, table: Table, key: Key, row: Row) -> Result<(), TableError> {
        self.activation.insert(table, key, row)
    }

    pub fn update(&mut self, table: Table, key: Key, row: Row) -> Result<(), TableError> {
        self.activation.update(table, key, row)
    }

    pub fn delete(&mut self, table: Table, key: Key) -> Result<(), TableError> {
        self.activation.delete(table, key)
    }

    pub fn read(&self, table: &Table, key: &Key) -> Result<Option<&Row>, TableError> {
        self.activation.read(table, key)
    }

    /// Nested calls cannot independently publish the root's overlays.
    pub fn commit(&mut self) -> Result<(), TableError> {
        Err(TableError::ChildCannotCommit)
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivationError, DatabaseRuntime, TableError, TableRuntime};
    use std::panic::AssertUnwindSafe;

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

    #[test]
    fn activation_helper_commits_success_and_rolls_back_operation_error() {
        let mut table = TableRuntime::<u64, String>::default();
        assert_eq!(
            table.activate(|root| {
                root.insert(1, "committed".into())?;
                Ok::<_, TableError>(())
            }),
            Ok(())
        );
        assert_eq!(
            table.activate(|root| {
                root.insert(2, "discarded".into())?;
                Err::<(), _>(TableError::UseAfterClose)
            }),
            Err(ActivationError::Operation(TableError::UseAfterClose))
        );
        assert_eq!(table.committed(&1).map(String::as_str), Some("committed"));
        assert_eq!(table.committed(&2), None);
    }

    #[test]
    fn activation_helper_does_not_publish_when_operation_panics() {
        let mut table = TableRuntime::<u64, String>::default();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _: Result<(), ActivationError<()>> = table.activate(|root| {
                root.insert(1, "unpublished".into()).unwrap();
                panic!("activation failed");
            });
        }));

        assert!(result.is_err());
        assert_eq!(table.committed(&1), None);
    }

    #[test]
    fn database_relations_remain_private_before_root_commit() {
        let mut database = DatabaseRuntime::<&str, u64, String>::default();
        {
            let mut root = database.begin();
            {
                let mut child = root.child().unwrap();
                child.insert("orders", 1, "order".into()).unwrap();
                child.insert("audits", 1, "audit".into()).unwrap();
                assert_eq!(child.commit(), Err(TableError::ChildCannotCommit));
            }
            assert_eq!(
                root.read(&"orders", &1).unwrap().map(String::as_str),
                Some("order")
            );
            assert_eq!(
                root.read(&"audits", &1).unwrap().map(String::as_str),
                Some("audit")
            );
        }
        assert_eq!(database.committed(&"orders", &1), None);
        assert_eq!(database.committed(&"audits", &1), None);
    }

    #[test]
    fn database_root_commit_publishes_all_relations_together() {
        let mut database = DatabaseRuntime::<&str, u64, String>::default();
        database
            .activate(|root| {
                let mut child = root.child()?;
                child.insert("orders", 1, "order".into())?;
                child.insert("audits", 1, "audit".into())?;
                Ok::<_, TableError>(())
            })
            .unwrap();

        assert_eq!(
            database.committed(&"orders", &1).map(String::as_str),
            Some("order")
        );
        assert_eq!(
            database.committed(&"audits", &1).map(String::as_str),
            Some("audit")
        );
    }

    #[test]
    fn database_activation_error_rolls_back_every_relation() {
        let mut database = DatabaseRuntime::<&str, u64, String>::default();
        assert_eq!(
            database.activate(|root| {
                root.insert("orders", 1, "order".into())?;
                root.insert("audits", 1, "audit".into())?;
                Err::<(), _>(TableError::UseAfterClose)
            }),
            Err(ActivationError::Operation(TableError::UseAfterClose))
        );

        assert_eq!(database.committed(&"orders", &1), None);
        assert_eq!(database.committed(&"audits", &1), None);
    }

    #[test]
    fn database_activation_panic_rolls_back_every_relation() {
        let mut database = DatabaseRuntime::<&str, u64, String>::default();
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _: Result<(), ActivationError<()>> = database.activate(|root| {
                root.insert("orders", 1, "order".into()).unwrap();
                root.insert("audits", 1, "audit".into()).unwrap();
                panic!("activation failed");
            });
        }));

        assert!(result.is_err());
        assert_eq!(database.committed(&"orders", &1), None);
        assert_eq!(database.committed(&"audits", &1), None);
    }
}
