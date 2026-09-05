//! A small in-memory table transaction primitive.
//!
//! Committed rows remain owned by [`TableRuntime`]. An activation holds only a
//! private overlay of inserts, replacements, and deletions. Nested ordinary
//! calls borrow that same activation through [`ChildScope`], so they can read
//! their parent's writes but cannot publish independently. Only the root
//! [`Activation`] can publish its overlay.

use std::{
    collections::{BTreeMap, BTreeSet},
    iter::Peekable,
    ops::RangeBounds,
};

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

    /// Scans committed rows in ascending canonical key order.
    pub fn scan(&self) -> impl Iterator<Item = (&Key, &Row)> {
        self.committed.iter()
    }

    /// Observes at most `limit` committed rows without materialising the
    /// complete relation.
    pub fn scan_window(&self, limit: usize) -> impl Iterator<Item = (&Key, &Row)> {
        self.scan().take(limit)
    }

    /// Scans a canonical key range in ascending order without materialising
    /// rows outside the requested bounds.
    pub fn scan_range<R>(&self, range: R) -> impl Iterator<Item = (&Key, &Row)>
    where
        R: RangeBounds<Key>,
    {
        self.committed.range(range)
    }

    /// Returns a lazy ordered relation over committed rows.
    pub fn relation(&self) -> Relation<'_, (Key, Row)> {
        Relation::new(self.scan().map(|(key, row)| (key.clone(), row.clone())))
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

type CommittedRows<'a, Key, Row> = Box<dyn Iterator<Item = (&'a Key, &'a Row)> + 'a>;
type OverlayRows<'a, Key, Row> = Box<dyn Iterator<Item = (&'a Key, &'a Option<Row>)> + 'a>;

/// A lazy ordered relation value.
pub struct Relation<'a, Item> {
    source: Box<dyn Iterator<Item = Item> + 'a>,
}

/// Cardinality failure returned when a relation is required to contain one value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardinalityError {
    Empty,
    Multiple,
}

impl<'a, Item: 'a> Relation<'a, Item> {
    fn new<I>(source: I) -> Self
    where
        I: Iterator<Item = Item> + 'a,
    {
        Self {
            source: Box::new(source),
        }
    }

    /// Keeps input order while retaining items accepted by `predicate`.
    pub fn filter<P>(self, predicate: P) -> Self
    where
        P: FnMut(&Item) -> bool + 'a,
    {
        Self::new(self.source.filter(predicate))
    }

    /// Maps items without changing their order.
    pub fn map<Output: 'a, M>(self, mapper: M) -> Relation<'a, Output>
    where
        M: FnMut(Item) -> Output + 'a,
    {
        Relation::new(self.source.map(mapper))
    }

    /// Maps each value to an ordered inner sequence and flattens it.
    pub fn flat_map<Output: 'a, M, Inner>(self, mapper: M) -> Relation<'a, Output>
    where
        M: FnMut(Item) -> Inner + 'a,
        Inner: IntoIterator<Item = Output> + 'a,
        Inner::IntoIter: 'a,
    {
        Relation::new(self.source.flat_map(mapper))
    }

    /// Concatenates two relations, retaining the complete left sequence first.
    pub fn union(self, other: Relation<'a, Item>) -> Self {
        Self::new(self.source.chain(other.source))
    }

    /// Returns adjacent overlapping pairs in input order.
    pub fn pairs(self) -> Relation<'a, (Item, Item)>
    where
        Item: Clone,
    {
        let mut source = self.source.peekable();
        Relation::new(std::iter::from_fn(move || {
            let left = source.next()?;
            let right = source.peek()?.clone();
            Some((left, right))
        }))
    }

    /// Returns the first value, or `None` when the relation is empty.
    pub fn first(mut self) -> Option<Item> {
        self.next()
    }

    /// Returns the only value, failing without enumerating beyond the second.
    pub fn one(mut self) -> Result<Item, CardinalityError> {
        let first = self.next().ok_or(CardinalityError::Empty)?;
        if self.next().is_some() {
            Err(CardinalityError::Multiple)
        } else {
            Ok(first)
        }
    }

    /// Returns whether every value satisfies the predicate, short-circuiting on false.
    pub fn every<P>(mut self, mut predicate: P) -> bool
    where
        P: FnMut(&Item) -> bool,
    {
        self.source.all(|item| predicate(&item))
    }

    /// Returns whether any value satisfies the predicate, short-circuiting on true.
    pub fn exists<P>(mut self, mut predicate: P) -> bool
    where
        P: FnMut(&Item) -> bool,
    {
        self.source.any(|item| predicate(&item))
    }

    /// Takes a bounded prefix without enumerating later items.
    pub fn take(self, limit: usize) -> Self {
        Self::new(self.source.take(limit))
    }

    /// Drops an ordered prefix.
    pub fn drop(self, count: usize) -> Self {
        Self::new(self.source.skip(count))
    }

    /// Establishes a stable order by the derived key.
    pub fn sort_by_key<Key, F>(self, mut key: F) -> Self
    where
        Key: Ord,
        F: FnMut(&Item) -> Key + 'a,
    {
        let mut items = self.source.collect::<Vec<_>>();
        items.sort_by_key(&mut key);
        Self::new(items.into_iter())
    }

    /// Keeps the first occurrence of each value in the established order.
    pub fn distinct(self) -> Self
    where
        Item: Clone + Ord,
    {
        let mut seen = BTreeSet::new();
        Self::new(self.source.filter(move |item| seen.insert((*item).clone())))
    }

    /// Groups values by key order while preserving input order in each group.
    pub fn group_by<Key, F>(self, mut key: F) -> Relation<'a, (Key, Vec<Item>)>
    where
        Key: Ord + 'a,
        F: FnMut(&Item) -> Key + 'a,
    {
        let mut groups = BTreeMap::<Key, Vec<Item>>::new();
        for item in self.source {
            groups.entry(key(&item)).or_default().push(item);
        }
        Relation::new(groups.into_iter())
    }

    /// Joins each left value with right values sharing a key.
    pub fn join<Right, Key, LeftKey, RightKey>(
        self,
        right: Relation<'a, Right>,
        mut left_key: LeftKey,
        mut right_key: RightKey,
    ) -> Relation<'a, (Item, Right)>
    where
        Item: Clone,
        Right: Clone + 'a,
        Key: Ord + 'a,
        LeftKey: FnMut(&Item) -> Key + 'a,
        RightKey: FnMut(&Right) -> Key + 'a,
    {
        let mut right_groups = BTreeMap::<Key, Vec<Right>>::new();
        for item in right.source {
            right_groups.entry(right_key(&item)).or_default().push(item);
        }
        Relation::new(self.source.flat_map(move |left| {
            right_groups
                .get(&left_key(&left))
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |right| (left.clone(), right))
        }))
    }
}

impl<Item> Iterator for Relation<'_, Item> {
    type Item = Item;

    fn next(&mut self) -> Option<Self::Item> {
        self.source.next()
    }
}

struct CandidateScan<'a, Key, Row> {
    committed: Peekable<CommittedRows<'a, Key, Row>>,
    overlay: Peekable<OverlayRows<'a, Key, Row>>,
}

impl<'a, Key, Row> CandidateScan<'a, Key, Row>
where
    Key: Clone + Ord,
    Row: Clone,
{
    fn new<C, O>(committed: C, overlay: O) -> Self
    where
        C: Iterator<Item = (&'a Key, &'a Row)> + 'a,
        O: Iterator<Item = (&'a Key, &'a Option<Row>)> + 'a,
    {
        let committed: CommittedRows<'a, Key, Row> = Box::new(committed);
        let overlay: OverlayRows<'a, Key, Row> = Box::new(overlay);
        Self {
            committed: committed.peekable(),
            overlay: overlay.peekable(),
        }
    }
}

impl<Key, Row> Iterator for CandidateScan<'_, Key, Row>
where
    Key: Clone + Ord,
    Row: Clone,
{
    type Item = (Key, Row);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match (self.committed.peek(), self.overlay.peek()) {
                (None, None) => return None,
                (Some(_), None) => {
                    let (key, row) = self.committed.next().expect("peeked committed row");
                    return Some((key.clone(), row.clone()));
                }
                (None, Some(_)) => {
                    let (key, row) = self.overlay.next().expect("peeked overlay row");
                    if let Some(row) = row {
                        return Some((key.clone(), row.clone()));
                    }
                }
                (Some((committed_key, _)), Some((overlay_key, _))) => {
                    match committed_key.cmp(overlay_key) {
                        std::cmp::Ordering::Less => {
                            let (key, row) = self.committed.next().expect("peeked committed row");
                            return Some((key.clone(), row.clone()));
                        }
                        std::cmp::Ordering::Equal => {
                            self.committed.next();
                            let (key, row) = self.overlay.next().expect("peeked overlay row");
                            if let Some(row) = row {
                                return Some((key.clone(), row.clone()));
                            }
                        }
                        std::cmp::Ordering::Greater => {
                            let (key, row) = self.overlay.next().expect("peeked overlay row");
                            if let Some(row) = row {
                                return Some((key.clone(), row.clone()));
                            }
                        }
                    }
                }
            }
        }
    }
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

    /// Scans at most `limit` candidate rows in canonical key order.
    ///
    /// Rows are owned so callers cannot retain references to an unpublished
    /// overlay after the activation closes.
    pub fn candidate_scan_window(
        &self,
        limit: usize,
    ) -> Result<impl Iterator<Item = (Key, Row)>, TableError> {
        self.require_open()?;
        Ok(CandidateScan::new(self.runtime.committed.iter(), self.overlay.iter()).take(limit))
    }

    /// Scans candidate rows in a canonical key range.
    pub fn candidate_scan_range<R>(
        &self,
        range: R,
    ) -> Result<impl Iterator<Item = (Key, Row)>, TableError>
    where
        R: RangeBounds<Key> + Clone,
    {
        self.require_open()?;
        Ok(CandidateScan::new(
            self.runtime.committed.range(range.clone()),
            self.overlay.range(range),
        ))
    }

    /// Returns a lazy ordered relation over this activation's candidate rows.
    pub fn candidate_relation(&self) -> Result<Relation<'_, (Key, Row)>, TableError> {
        self.require_open()?;
        Ok(Relation::new(CandidateScan::new(
            self.runtime.committed.iter(),
            self.overlay.iter(),
        )))
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

    pub fn candidate_scan_window(
        &self,
        limit: usize,
    ) -> Result<impl Iterator<Item = (Key, Row)>, TableError> {
        self.activation.candidate_scan_window(limit)
    }

    pub fn candidate_scan_range<R>(
        &self,
        range: R,
    ) -> Result<impl Iterator<Item = (Key, Row)>, TableError>
    where
        R: RangeBounds<Key> + Clone,
    {
        self.activation.candidate_scan_range(range)
    }

    pub fn candidate_relation(&self) -> Result<Relation<'_, (Key, Row)>, TableError> {
        self.activation.candidate_relation()
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

    /// Scans one committed relation in ascending canonical key order.
    pub fn scan(&self, table: &Table) -> impl Iterator<Item = (&Key, &Row)> + '_ {
        self.committed
            .get(table)
            .into_iter()
            .flat_map(BTreeMap::iter)
    }

    /// Observes at most `limit` rows from one committed relation.
    pub fn scan_window(
        &self,
        table: &Table,
        limit: usize,
    ) -> impl Iterator<Item = (&Key, &Row)> + '_ {
        self.scan(table).take(limit)
    }

    /// Scans one committed relation over a canonical key range.
    pub fn scan_range<'a, R>(
        &'a self,
        table: &Table,
        range: R,
    ) -> impl Iterator<Item = (&'a Key, &'a Row)> + 'a
    where
        R: Clone + RangeBounds<Key> + 'a,
    {
        self.committed
            .get(table)
            .into_iter()
            .flat_map(move |relation| relation.range(range.clone()))
    }

    /// Returns a lazy ordered relation over one committed relation.
    pub fn relation(&self, table: &Table) -> Relation<'_, (Key, Row)> {
        Relation::new(
            self.scan(table)
                .map(|(key, row)| (key.clone(), row.clone())),
        )
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

    /// Scans at most `limit` candidate rows in one relation.
    pub fn candidate_scan_window(
        &self,
        table: &Table,
        limit: usize,
    ) -> Result<impl Iterator<Item = (Key, Row)>, TableError> {
        self.require_open()?;
        let committed = self
            .runtime
            .committed
            .get(table)
            .into_iter()
            .flat_map(BTreeMap::iter);
        let overlay = self.overlay.get(table).into_iter().flat_map(BTreeMap::iter);
        Ok(CandidateScan::new(committed, overlay).take(limit))
    }

    /// Scans one candidate relation in a canonical key range.
    pub fn candidate_scan_range<'a, R>(
        &'a self,
        table: &Table,
        range: R,
    ) -> Result<impl Iterator<Item = (Key, Row)> + 'a, TableError>
    where
        R: RangeBounds<Key> + Clone + 'a,
    {
        self.require_open()?;
        let committed_range = range.clone();
        let committed = self
            .runtime
            .committed
            .get(table)
            .into_iter()
            .flat_map(move |relation| relation.range(committed_range.clone()));
        let overlay = self
            .overlay
            .get(table)
            .into_iter()
            .flat_map(move |relation| relation.range(range.clone()));
        Ok(CandidateScan::new(committed, overlay))
    }

    /// Returns a lazy ordered relation over one activation candidate relation.
    pub fn candidate_relation(
        &self,
        table: &Table,
    ) -> Result<Relation<'_, (Key, Row)>, TableError> {
        self.require_open()?;
        let committed = self
            .runtime
            .committed
            .get(table)
            .into_iter()
            .flat_map(BTreeMap::iter);
        let overlay = self.overlay.get(table).into_iter().flat_map(BTreeMap::iter);
        Ok(Relation::new(CandidateScan::new(committed, overlay)))
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

    pub fn candidate_scan_window(
        &self,
        table: &Table,
        limit: usize,
    ) -> Result<impl Iterator<Item = (Key, Row)>, TableError> {
        self.activation.candidate_scan_window(table, limit)
    }

    pub fn candidate_scan_range<'a, R>(
        &'a self,
        table: &Table,
        range: R,
    ) -> Result<impl Iterator<Item = (Key, Row)> + 'a, TableError>
    where
        R: RangeBounds<Key> + Clone + 'a,
    {
        self.activation.candidate_scan_range(table, range)
    }

    pub fn candidate_relation(
        &self,
        table: &Table,
    ) -> Result<Relation<'_, (Key, Row)>, TableError> {
        self.activation.candidate_relation(table)
    }

    /// Nested calls cannot independently publish the root's overlays.
    pub fn commit(&mut self) -> Result<(), TableError> {
        Err(TableError::ChildCannotCommit)
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivationError, CardinalityError, DatabaseRuntime, TableError, TableRuntime};
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

    #[test]
    fn committed_scans_are_in_ascending_key_order() {
        let mut table = TableRuntime::<u64, &'static str>::default();
        table
            .activate(|activation| {
                activation.insert(3, "three")?;
                activation.insert(1, "one")?;
                activation.insert(2, "two")?;
                Ok::<_, TableError>(())
            })
            .unwrap();

        let keys = table.scan().map(|(key, _)| *key).collect::<Vec<_>>();
        assert_eq!(keys, vec![1, 2, 3]);
    }

    #[test]
    fn committed_database_scans_are_in_ascending_key_order() {
        let mut database = DatabaseRuntime::<&'static str, u64, &'static str>::default();
        database
            .activate(|activation| {
                activation.insert("notes", 3, "three")?;
                activation.insert("notes", 1, "one")?;
                activation.insert("notes", 2, "two")?;
                Ok::<_, TableError>(())
            })
            .unwrap();

        let keys = database
            .scan(&"notes")
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        assert_eq!(keys, vec![1, 2, 3]);
    }

    #[test]
    fn bounded_and_range_scans_preserve_canonical_order() {
        let mut database = DatabaseRuntime::<&'static str, u64, &'static str>::default();
        database
            .activate(|activation| {
                for key in [1, 2, 3, 4] {
                    activation.insert("notes", key, "note")?;
                }
                Ok::<_, TableError>(())
            })
            .unwrap();

        assert_eq!(
            database
                .scan_window(&"notes", 2)
                .map(|(key, _)| *key)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            database
                .scan_range(&"notes", 2..=3)
                .map(|(key, _)| *key)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn candidate_scans_include_staged_mutations_but_rollback_keeps_committed_rows() {
        let mut table = TableRuntime::<u64, &'static str>::default();
        table
            .activate(|activation| {
                for (key, row) in [(1, "one"), (2, "two"), (3, "three")] {
                    activation.insert(key, row)?;
                }
                Ok::<_, TableError>(())
            })
            .unwrap();

        let mut activation = table.begin();
        activation.update(2, "updated").unwrap();
        activation.delete(1).unwrap();
        activation.insert(4, "four").unwrap();

        assert_eq!(
            activation
                .candidate_scan_window(2)
                .unwrap()
                .collect::<Vec<_>>(),
            vec![(2, "updated"), (3, "three")]
        );
        assert_eq!(
            activation
                .candidate_scan_range(2..=4)
                .unwrap()
                .collect::<Vec<_>>(),
            vec![(2, "updated"), (3, "three"), (4, "four")]
        );

        activation.rollback();
        assert_eq!(
            table
                .scan()
                .map(|(key, row)| (*key, *row))
                .collect::<Vec<_>>(),
            vec![(1, "one"), (2, "two"), (3, "three")]
        );
    }

    #[test]
    fn database_candidate_scans_are_relation_local_and_private() {
        let mut database = DatabaseRuntime::<&'static str, u64, &'static str>::default();
        database
            .activate(|activation| {
                activation.insert("orders", 1, "old")?;
                activation.insert("orders", 3, "three")?;
                activation.insert("audits", 2, "audit")?;
                Ok::<_, TableError>(())
            })
            .unwrap();

        let mut activation = database.begin();
        activation.update("orders", 1, "new").unwrap();
        activation.insert("orders", 2, "two").unwrap();

        assert_eq!(
            activation
                .candidate_scan_range(&"orders", 1..=2)
                .unwrap()
                .collect::<Vec<_>>(),
            vec![(1, "new"), (2, "two")]
        );
        assert_eq!(
            activation
                .candidate_scan_window(&"audits", 10)
                .unwrap()
                .collect::<Vec<_>>(),
            vec![(2, "audit")]
        );

        activation.rollback();
        assert_eq!(database.committed(&"orders", &2), None);
        assert_eq!(database.committed(&"orders", &1).copied(), Some("old"));
    }

    #[test]
    fn lazy_relations_compose_in_order_over_committed_and_candidate_rows() {
        let mut database = DatabaseRuntime::<&'static str, u64, &'static str>::default();
        database
            .activate(|activation| {
                for (key, row) in [(1, "one"), (2, "two"), (3, "three"), (4, "four")] {
                    activation.insert("notes", key, row)?;
                }
                Ok::<_, TableError>(())
            })
            .unwrap();

        assert_eq!(
            database
                .relation(&"notes")
                .filter(|(_, row)| *row != "two")
                .map(|(key, row)| (key, row.to_ascii_uppercase()))
                .drop(1)
                .take(2)
                .collect::<Vec<_>>(),
            vec![(3, "THREE".into()), (4, "FOUR".into())]
        );
        assert_eq!(
            database
                .relation(&"notes")
                .sort_by_key(|(_, row)| *row)
                .map(|(key, row)| (key, row))
                .collect::<Vec<_>>(),
            vec![(4, "four"), (1, "one"), (3, "three"), (2, "two")]
        );
        assert_eq!(
            database
                .relation(&"notes")
                .map(|(_, row)| row.chars().next().expect("nonempty row"))
                .distinct()
                .collect::<Vec<_>>(),
            vec!['o', 't', 'f']
        );
        assert_eq!(
            database
                .relation(&"notes")
                .group_by(|(_, row)| row.chars().next().expect("nonempty row"))
                .collect::<Vec<_>>(),
            vec![
                ('f', vec![(4, "four")]),
                ('o', vec![(1, "one")]),
                ('t', vec![(2, "two"), (3, "three")]),
            ]
        );
        assert_eq!(
            database
                .relation(&"notes")
                .join(
                    database.relation(&"notes"),
                    |(_, row)| row.chars().next().expect("nonempty row"),
                    |(_, row)| row.chars().next().expect("nonempty row"),
                )
                .map(|((left_key, _), (right_key, _))| (left_key, right_key))
                .collect::<Vec<_>>(),
            vec![(1, 1), (2, 2), (2, 3), (3, 2), (3, 3), (4, 4)]
        );
        assert_eq!(
            database
                .relation(&"notes")
                .flat_map(|(_, row)| row.chars())
                .collect::<String>(),
            "onetwothreefour"
        );
        assert_eq!(
            database
                .relation(&"notes")
                .take(2)
                .union(database.relation(&"notes").take(1))
                .map(|(key, _)| key)
                .collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
        assert_eq!(database.relation(&"notes").first(), Some((1, "one")));
        assert_eq!(database.relation(&"notes").take(1).one(), Ok((1, "one")));
        assert_eq!(
            database.relation(&"notes").one(),
            Err(CardinalityError::Multiple)
        );
        assert_eq!(
            database.relation(&"notes").take(0).one(),
            Err(CardinalityError::Empty)
        );
        assert!(
            database
                .relation(&"notes")
                .every(|(_, row)| !row.is_empty())
        );
        assert!(
            database
                .relation(&"notes")
                .exists(|(_, row)| *row == "three")
        );
        assert!(
            !database
                .relation(&"notes")
                .exists(|(_, row)| *row == "missing")
        );

        let mut activation = database.begin();
        activation.update("notes", 2, "updated").unwrap();
        activation.delete("notes", 3).unwrap();
        activation.insert("notes", 5, "five").unwrap();
        assert_eq!(
            activation
                .candidate_relation(&"notes")
                .unwrap()
                .take(3)
                .collect::<Vec<_>>(),
            vec![(1, "one"), (2, "updated"), (4, "four")]
        );
        activation.rollback();
    }

    #[test]
    fn relation_pairs_are_adjacent_overlapping_and_ordered() {
        let pairs = super::Relation::new([1, 2, 3, 4].into_iter())
            .pairs()
            .collect::<Vec<_>>();

        assert_eq!(pairs, vec![(1, 2), (2, 3), (3, 4)]);
        assert!(
            super::Relation::new(std::iter::empty::<u8>())
                .pairs()
                .next()
                .is_none()
        );
    }
}
