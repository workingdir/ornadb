//! The committed identities the `sys.catalog.*` rows carry.
//!
//! `sys.Table` and `sys.Column` rows expose `reference`, `object` and the cross
//! references between them as typed row references (ORNA-SYS-111,
//! ORNA-SYS-112, `docs/design/sys-row-references.md`). A reference states the
//! row's natural key and the snapshot it resolves in, so building one needs the
//! stable object identity of the table or column and the snapshot those
//! identities were read at. Both exist only in committed metadata: the host
//! reads the identity inventory of one commit and hands it over as
//! [`CatalogueIdentities`]. Without it there is no spec-correct row, so the
//! rows are not built and a read of them fails closed.

use std::collections::BTreeMap;

use orna_value::{
    CanonicalSnapshot, KeyPart, OvbRaw, RelationSpec, RowRef, SysRef, SystemReferenceError,
    object_keyed_reference, object_reference, relations,
};

/// Why the catalogue rows cannot be built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueIdentityError {
    /// The snapshot pins another database than the identities name.
    DatabaseMismatch,
    /// The activation holds no committed identities, so a table of the catalogue
    /// has no reference to carry.
    Unavailable,
    /// A table of the catalogue has no committed identity.
    MissingTable(String),
    /// A column of the catalogue has no committed identity.
    MissingColumn {
        /// The table that declares the column.
        table: String,
        /// The column's declared name.
        column: String,
    },
    /// A reference over a committed identity is not a valid reference.
    Reference(SystemReferenceError),
    /// A reference did not encode as a canonical value.
    Encoding,
}

/// The committed identities of one table and of its columns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableIdentity {
    object: [u8; 16],
    columns: BTreeMap<String, [u8; 16]>,
}

impl TableIdentity {
    /// The table's object identity and the object identity of each column, by
    /// declared column name.
    #[must_use]
    pub fn new(object: [u8; 16], columns: BTreeMap<String, [u8; 16]>) -> Self {
        Self { object, columns }
    }

    /// The table's object identity.
    #[must_use]
    pub fn object(&self) -> [u8; 16] {
        self.object
    }

    /// The object identity of the column declared as `name`.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<[u8; 16]> {
        self.columns.get(name).copied()
    }
}

/// The committed object identities of the admitted tables and columns, with the
/// database and the snapshot those identities were read at.
///
/// `sys.Object` is keyed by identity and snapshot, so every reference built
/// over these identities is pinned to [`Self::snapshot`] and names
/// [`Self::database_id`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueIdentities {
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    tables: BTreeMap<String, TableIdentity>,
}

impl CatalogueIdentities {
    /// The identities of `tables`, keyed by the name each table is addressed by,
    /// as read at `snapshot` of the database `database_id`.
    ///
    /// # Errors
    ///
    /// [`CatalogueIdentityError::DatabaseMismatch`] when `snapshot` pins another
    /// database.
    pub fn new(
        database_id: [u8; 16],
        snapshot: CanonicalSnapshot,
        tables: BTreeMap<String, TableIdentity>,
    ) -> Result<Self, CatalogueIdentityError> {
        let pinned = match &snapshot {
            CanonicalSnapshot::Cwd { database, .. }
            | CanonicalSnapshot::Commit { database, .. } => *database,
        };
        if pinned != database_id {
            return Err(CatalogueIdentityError::DatabaseMismatch);
        }
        Ok(Self {
            database_id,
            snapshot,
            tables,
        })
    }

    /// The database every reference names.
    #[must_use]
    pub fn database_id(&self) -> [u8; 16] {
        self.database_id
    }

    /// The snapshot every reference is pinned to.
    #[must_use]
    pub fn snapshot(&self) -> &CanonicalSnapshot {
        &self.snapshot
    }

    /// The committed identities of the table addressed as `name`.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<&TableIdentity> {
        self.tables.get(name)
    }

    /// The canonical `sys.ObjectRef` of the catalogue object `id`.
    pub(crate) fn object(&self, id: [u8; 16]) -> Result<OvbRaw, CatalogueIdentityError> {
        let object = object_reference(self.database_id, id, self.snapshot.clone())
            .map_err(CatalogueIdentityError::Reference)?;
        canonical_reference(object.as_row_ref())
    }

    /// The canonical reference to the row of the object-keyed `relation`
    /// (`sys.Table`, `sys.Column`) whose `object` is the catalogue object `id`.
    pub(crate) fn keyed(
        &self,
        relation: &'static RelationSpec,
        id: [u8; 16],
    ) -> Result<OvbRaw, CatalogueIdentityError> {
        let object = object_reference(self.database_id, id, self.snapshot.clone())
            .map_err(CatalogueIdentityError::Reference)?;
        let reference =
            object_keyed_reference(relation, object).map_err(CatalogueIdentityError::Reference)?;
        canonical_reference(reference.as_row_ref())
    }

    /// The canonical `sys.DefinitionRef` whose `sys.DefinitionId` is `id`.
    pub(crate) fn definition(&self, id: [u8; 16]) -> Result<OvbRaw, CatalogueIdentityError> {
        let reference = SysRef::new(
            &relations::DEFINITION,
            self.database_id,
            self.snapshot.clone(),
            vec![KeyPart::Uuid(id)],
        )
        .map_err(CatalogueIdentityError::Reference)?;
        canonical_reference(reference.as_row_ref())
    }
}

/// The tag-60010 form of `reference`, ready to sit in a row.
fn canonical_reference(reference: &RowRef) -> Result<OvbRaw, CatalogueIdentityError> {
    reference
        .to_canonical()
        .map(|value| value.raw().clone())
        .map_err(|_| CatalogueIdentityError::Encoding)
}
