//! Read-only views over the committed table state an activation or query is pinned to.

use crate::RuntimeActivationContext;
use crate::error::RuntimeError;
use crate::table_store::{TableMutation, TableObjectId};
use std::collections::BTreeMap;
use std::fmt;

/// The immutable admission view for a table-backed activation.
///
/// The context and every requested relation are read from one database
/// transaction, so evaluation cannot combine a generation from one durable
/// state with rows from another.
pub type RuntimeTableRows = BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>;

/// The published pin a committed base was read at: the mutation sequence of
/// the newest published checkpoint, zero before any publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedPin {
    pub(crate) mutation_sequence: i64,
}

impl PublishedPin {
    /// Mutation sequence of the pinned checkpoint.
    #[must_use]
    pub fn mutation_sequence(&self) -> i64 {
        self.mutation_sequence
    }
}

/// Committed `HEAD` rows of declared relations, bound to the published pin they
/// were read at. Every transaction that reads through the base verifies the pin
/// still matches the published checkpoint and fails closed with
/// [`RuntimeError::StaleCapture`] otherwise. Rows are keyed for direct lookup and
/// shared by `Arc`, so a commit or validator never copies the relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedCommittedRows {
    pub(crate) pin: PublishedPin,
    pub(crate) tables: BTreeMap<String, BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl PinnedCommittedRows {
    /// Binds `rows` to `pin`. A relation with a duplicate committed key is a
    /// corrupt base and fails closed.
    pub fn new(pin: PublishedPin, rows: RuntimeTableRows) -> Result<Self, RuntimeError> {
        let mut tables = BTreeMap::new();
        for (table, table_rows) in rows {
            let mut keyed = BTreeMap::new();
            for (key, value) in table_rows {
                if keyed.insert(key, value).is_some() {
                    return Err(RuntimeError::RecoveryInvalid);
                }
            }
            tables.insert(table, keyed);
        }
        Ok(Self { pin, tables })
    }

    /// The published pin these rows were read at.
    #[must_use]
    pub fn pin(&self) -> PublishedPin {
        self.pin
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTableActivationSnapshot {
    pub(crate) context: RuntimeActivationContext,
    pub(crate) table_rows: RuntimeTableRows,
    pub(crate) table_object_ids: BTreeMap<String, TableObjectId>,
}

impl RuntimeTableActivationSnapshot {
    pub fn context(&self) -> &RuntimeActivationContext {
        &self.context
    }

    pub fn table_rows(&self) -> &RuntimeTableRows {
        &self.table_rows
    }

    /// Returns the committed semantic identity admitted for `table`, if this
    /// activation used the identity-aware table admission API.
    #[must_use]
    pub fn table_object_id(&self, table: &str) -> Option<TableObjectId> {
        self.table_object_ids.get(table).copied()
    }

    /// All committed semantic identities captured for this activation.
    #[must_use]
    pub fn table_object_ids(&self) -> &BTreeMap<String, TableObjectId> {
        &self.table_object_ids
    }

    /// Starts a query session over this activation's captured table rows.
    ///
    /// The reference defines live-query and read-your-writes semantics but
    /// does not prescribe a runtime API shape. This API uses the canonical
    /// encoded primary key as its exact-match identity and gives each session
    /// a private mutation overlay.
    pub fn query_session(&self) -> RuntimeQuerySession<'_> {
        RuntimeQuerySession {
            snapshot: self,
            overlay: BTreeMap::new(),
            mutations: Vec::new(),
        }
    }

    /// Returns the captured row whose encoded primary key exactly matches
    /// `key`. Keys are canonical OVB bytes, so schema-aware callers normalize
    /// logical aliases before entering this schema-independent runtime API.
    pub fn query_exact(&self, table: &str, key: &[u8]) -> Result<Option<&[u8]>, RuntimeQueryError> {
        let rows = self
            .table_rows
            .get(table)
            .ok_or(RuntimeQueryError::TableNotAdmitted)?;
        Ok(rows
            .binary_search_by(|(candidate, _)| candidate.as_slice().cmp(key))
            .ok()
            .map(|index| rows[index].1.as_slice()))
    }
}

/// A private exact-query view over one activation snapshot.
///
/// Staged writes shadow captured rows immediately and disappear when the
/// session is dropped. The owner passes [`Self::staged_mutations`] to the
/// existing atomic activation commit when evaluation succeeds.
pub struct RuntimeQuerySession<'snapshot> {
    snapshot: &'snapshot RuntimeTableActivationSnapshot,
    overlay: BTreeMap<String, BTreeMap<Vec<u8>, Option<Vec<u8>>>>,
    mutations: Vec<TableMutation>,
}

impl RuntimeQuerySession<'_> {
    /// Checks that the activation captured this relation without reading rows.
    ///
    /// `take(0)` does not need to enumerate or project anything, but it still
    /// validates admission so an unavailable relation cannot masquerade as a
    /// known empty one. The reference requires bounded enumeration but leaves
    /// this zero-row admission boundary open; the runtime preserves its
    /// admitted-empty versus unadmitted distinction.
    pub fn ensure_table_admitted(&self, table: &str) -> Result<(), RuntimeQueryError> {
        self.snapshot
            .table_rows
            .contains_key(table)
            .then_some(())
            .ok_or(RuntimeQueryError::TableNotAdmitted)
    }

    /// Returns the session's current row for the exact canonical key.
    ///
    /// Every call checks the current overlay before the immutable base rows;
    /// query results are deliberately not memoized, preserving independent
    /// call semantics and making preceding writes visible immediately.
    pub fn query_exact(&self, table: &str, key: &[u8]) -> Result<Option<&[u8]>, RuntimeQueryError> {
        self.ensure_table_admitted(table)?;
        if let Some(value) = self.overlay.get(table).and_then(|rows| rows.get(key)) {
            return Ok(value.as_deref());
        }
        self.snapshot.query_exact(table, key)
    }

    /// Returns the exact row or the typed miss used by a required `lookup`
    /// effect. An admitted table with no matching key is distinct from a
    /// relation the activation did not admit.
    pub fn lookup_exact(&self, table: &str, key: &[u8]) -> Result<&[u8], RuntimeQueryError> {
        self.query_exact(table, key)?
            .ok_or(RuntimeQueryError::RowNotFound)
    }

    /// Reads one bounded page from the session's current relation view.
    ///
    /// Rows are ordered by canonical encoded key. `after` is exclusive, and
    /// `next` is the last returned key only when a lookahead row proves that
    /// another page exists. The reference specifies read-your-writes but not
    /// a runtime cursor shape, so this follows the evaluator's exclusive
    /// canonical-cursor contract and avoids empty continuation pages. Pages
    /// are capped at 1,024 rows as a pragmatic runtime bound; the evaluator
    /// currently requests one row per page. Each continuation reads the
    /// session's latest overlay: a newly staged key after the cursor is seen,
    /// while a key at or before the cursor does not restart the scan. This
    /// bounds refresh staleness to the exclusive cursor: callers that need
    /// earlier keys reconsidered start a fresh scan without a cursor. Rows
    /// already returned in a page, and values already folded from those rows,
    /// remain the values observed by that read; later overlay changes are
    /// visible to fresh reads, not retroactively.
    pub fn query_page(
        &self,
        table: &str,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RuntimeQueryPage, RuntimeQueryError> {
        if limit == 0 || limit > MAX_RUNTIME_QUERY_PAGE_ROWS {
            return Err(RuntimeQueryError::InvalidPageSize);
        }
        let base = self
            .snapshot
            .table_rows
            .get(table)
            .ok_or(RuntimeQueryError::TableNotAdmitted)?;

        let base_start = after.map_or(0, |cursor| {
            base.partition_point(|(key, _)| key.as_slice() <= cursor)
        });
        let mut base_rows = base[base_start..].iter().peekable();
        let mut staged_rows = self
            .overlay
            .get(table)
            .into_iter()
            .flat_map(|rows| {
                use std::ops::Bound::{Excluded, Unbounded};

                let bounds = after.map_or((Unbounded, Unbounded), |cursor| {
                    (Excluded(cursor), Unbounded)
                });
                rows.range::<[u8], _>(bounds)
            })
            .peekable();
        let mut rows = Vec::with_capacity(limit);
        let mut has_more = false;
        loop {
            let base_next = base_rows.peek().copied();
            let staged_next = staged_rows.peek().copied();
            let (key, value) = match (base_next, staged_next) {
                (Some((base_key, _)), Some((staged_key, _)))
                    if base_key.as_slice() == staged_key.as_slice() =>
                {
                    base_rows.next();
                    let (key, value) = staged_rows.next().expect("peeked staged row");
                    (key.as_slice(), value.as_deref())
                }
                (Some((base_key, _)), Some((staged_key, _)))
                    if base_key.as_slice() < staged_key.as_slice() =>
                {
                    let (key, value) = base_rows.next().expect("peeked base row");
                    (key.as_slice(), Some(value.as_slice()))
                }
                (Some(_), Some(_)) => {
                    let (key, value) = staged_rows.next().expect("peeked staged row");
                    (key.as_slice(), value.as_deref())
                }
                (Some(_), None) => {
                    let (key, value) = base_rows.next().expect("peeked base row");
                    (key.as_slice(), Some(value.as_slice()))
                }
                (None, Some(_)) => {
                    let (key, value) = staged_rows.next().expect("peeked staged row");
                    (key.as_slice(), value.as_deref())
                }
                (None, None) => break,
            };
            let Some(value) = value else {
                continue;
            };
            if after.is_some_and(|cursor| key <= cursor) {
                continue;
            }
            if rows.len() == limit {
                has_more = true;
                break;
            }
            rows.push((key.to_vec(), value.to_vec()));
        }
        let next = has_more
            .then(|| rows.last().map(|(key, _)| key.clone()))
            .flatten();
        Ok(RuntimeQueryPage { rows, next })
    }

    /// Adds one already validated mutation to this private session.
    ///
    /// A repeated key replaces the session's query view in order of staging,
    /// while all operations remain in the returned commit batch.
    pub fn stage_mutation(&mut self, mutation: TableMutation) -> Result<(), RuntimeQueryError> {
        if !self.snapshot.table_rows.contains_key(mutation.table()) {
            return Err(RuntimeQueryError::TableNotAdmitted);
        }
        self.overlay
            .entry(mutation.table().to_owned())
            .or_default()
            .insert(
                mutation.key().to_vec(),
                mutation.value().map(<[u8]>::to_vec),
            );
        self.mutations.push(mutation);
        Ok(())
    }

    /// Returns the mutations staged in source order for an atomic runtime commit.
    pub fn staged_mutations(&self) -> &[TableMutation] {
        &self.mutations
    }

    /// Consumes the session and returns its mutations in staging order.
    pub fn into_mutations(self) -> Vec<TableMutation> {
        self.mutations
    }
}

pub(crate) const MAX_RUNTIME_QUERY_PAGE_ROWS: usize = 1024;

/// A bounded page of visible runtime rows in canonical key order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeQueryPage {
    /// Canonical encoded primary keys and canonical row values.
    pub rows: Vec<(Vec<u8>, Vec<u8>)>,
    /// Exclusive cursor for a later page, present only when more rows exist.
    pub next: Option<Vec<u8>>,
}

/// A query requested a relation that was not included in its activation snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeQueryError {
    TableNotAdmitted,
    RowNotFound,
    InvalidPageSize,
}

impl fmt::Display for RuntimeQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TableNotAdmitted => "table was not admitted to this query session",
            Self::RowNotFound => "exact query did not match a row",
            Self::InvalidPageSize => "query page size is outside the supported range",
        })
    }
}

impl std::error::Error for RuntimeQueryError {}
