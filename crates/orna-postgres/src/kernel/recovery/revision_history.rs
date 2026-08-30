//! Immutable revision-pair history listing and validation.

use super::*;

/// One immutable source/catalogue revision pair retained by the kernel.
///
/// Entries expose only revision identities and their stored parent links. The
/// pair whose identities match the durable active marker is reported by
/// [`RevisionPairHistoryEntry::is_active`]. Returned entries are ordered by ascending source then
/// catalogue identity, using the canonical ordering of the opaque IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionPairHistoryEntry {
    source_revision_id: SourceRevisionId,
    source_parent_revision_id: Option<SourceRevisionId>,
    catalogue_revision_id: CatalogueRevisionId,
    catalogue_parent_revision_id: Option<CatalogueRevisionId>,
    is_active: bool,
}

impl RevisionPairHistoryEntry {
    /// Returns the source revision identity.
    pub const fn source_revision_id(self) -> SourceRevisionId {
        self.source_revision_id
    }

    /// Returns the optional parent source revision identity.
    pub const fn source_parent_revision_id(self) -> Option<SourceRevisionId> {
        self.source_parent_revision_id
    }

    /// Returns the catalogue revision identity.
    pub const fn catalogue_revision_id(self) -> CatalogueRevisionId {
        self.catalogue_revision_id
    }

    /// Returns the optional parent catalogue revision identity.
    pub const fn catalogue_parent_revision_id(self) -> Option<CatalogueRevisionId> {
        self.catalogue_parent_revision_id
    }

    /// Returns whether this pair is the durable active revision pair.
    pub const fn is_active(self) -> bool {
        self.is_active
    }
}

impl PostgresKernel {
    /// Lists every immutable source/catalogue revision pair retained by the
    /// kernel, marking the pair selected by the durable active marker.
    ///
    /// The listing runs in one read-only repeatable-read transaction after the
    /// trusted search path and current migration registry have been checked.
    /// Entries are ordered by ascending source then catalogue identity. This
    /// method only reads revision metadata; it does not activate or reconstruct
    /// a historical revision.
    pub async fn list_revision_pairs(
        &self,
    ) -> Result<Vec<RevisionPairHistoryEntry>, PostgresKernelError> {
        let mut session = self.open().await?;
        let listing_result = list_revision_pairs_client(&mut session.client).await;
        let shutdown_result = session.shutdown().await;

        match (listing_result, shutdown_result) {
            (Ok(entries), Ok(())) => Ok(entries),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

async fn list_revision_pairs_client(
    client: &mut Client,
) -> Result<Vec<RevisionPairHistoryEntry>, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;

    establish_trusted_search_path(&transaction).await?;
    require_current_migrations(&transaction).await?;
    let active_pair = load_active_revision_pair(&transaction).await?;
    let rows = transaction
        .query(
            "SELECT
                catalogue.source_revision_id AS source_id,
                source.parent_source_revision_id AS source_parent_id,
                catalogue.id AS catalogue_id,
                catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                source.id IS NOT NULL AS source_exists
             FROM _orna_kernel.catalogue_revisions AS catalogue
             LEFT JOIN _orna_kernel.source_revisions AS source
               ON source.id = catalogue.source_revision_id
             ORDER BY catalogue.source_revision_id ASC, catalogue.id ASC",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let entries = rows
        .iter()
        .enumerate()
        .map(|(index, row)| decode_revision_pair_row(row, index, active_pair))
        .collect::<Result<Vec<_>, _>>()?;
    validate_revision_pair_listing(&entries)?;

    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(entries)
}

async fn load_active_revision_pair(
    transaction: &Transaction<'_>,
) -> Result<RevisionPair, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT singleton,
                    source_revision_id AS active_source_id,
                    catalogue_revision_id AS active_catalogue_id
             FROM _orna_kernel.active_revision",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let record = DurableRecord::new(ACTIVE_RELATION, "singleton=true");
    if rows.len() != 1 {
        return Err(
            record.invariant("exactly one active source and catalogue revision pair must exist")
        );
    }

    let row = &rows[0];
    if !record.column::<bool>(
        row,
        "singleton",
        "active revision singleton flag must be true",
    )? {
        return Err(record.invariant("active revision singleton flag must be true"));
    }
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "active_source_id",
            "active source revision identity must be 16 bytes",
        )?,
        &record,
        "active source revision identity must be 16 bytes",
    )?);
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "active_catalogue_id",
            "active catalogue revision identity must be 16 bytes",
        )?,
        &record,
        "active catalogue revision identity must be 16 bytes",
    )?);
    Ok(RevisionPair::new(source, catalogue))
}

fn decode_revision_pair_row(
    row: &Row,
    row_index: usize,
    active_pair: RevisionPair,
) -> Result<RevisionPairHistoryEntry, PostgresKernelError> {
    let source_record = DurableRecord::new(SOURCE_REVISION_RELATION, format!("row={row_index}"));
    let catalogue_record =
        DurableRecord::new(CATALOGUE_REVISION_RELATION, format!("row={row_index}"));
    if !catalogue_record.column::<bool>(
        row,
        "source_exists",
        "catalogue source join must identify a source row",
    )? {
        return Err(catalogue_record
            .invariant("each catalogue revision must have a matching source revision"));
    }
    decode_revision_pair_values(
        source_record.column(
            row,
            "source_id",
            "source revision identity must be 16 bytes",
        )?,
        source_record.column(
            row,
            "source_parent_id",
            "source parent identity must be null or 16 bytes",
        )?,
        catalogue_record.column(
            row,
            "catalogue_id",
            "catalogue revision identity must be 16 bytes",
        )?,
        catalogue_record.column(
            row,
            "catalogue_parent_id",
            "catalogue parent identity must be null or 16 bytes",
        )?,
        active_pair,
        &source_record,
        &catalogue_record,
    )
}

pub(super) fn decode_revision_pair_values(
    source_id: Vec<u8>,
    source_parent_id: Option<Vec<u8>>,
    catalogue_id: Vec<u8>,
    catalogue_parent_id: Option<Vec<u8>>,
    active_pair: RevisionPair,
    source_record: &DurableRecord,
    catalogue_record: &DurableRecord,
) -> Result<RevisionPairHistoryEntry, PostgresKernelError> {
    let source_revision_id = SourceRevisionId::from_bytes(identity_bytes(
        source_id,
        source_record,
        "source revision identity must be 16 bytes",
    )?);
    let source_parent_revision_id = optional_identity_bytes(
        source_parent_id,
        source_record,
        "source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let catalogue_revision_id = CatalogueRevisionId::from_bytes(identity_bytes(
        catalogue_id,
        catalogue_record,
        "catalogue revision identity must be 16 bytes",
    )?);
    let catalogue_parent_revision_id = optional_identity_bytes(
        catalogue_parent_id,
        catalogue_record,
        "catalogue parent identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    if source_parent_revision_id.is_some() != catalogue_parent_revision_id.is_some() {
        return Err(catalogue_record.invariant(
            "source and catalogue parent identities must both be null or both be present",
        ));
    }

    Ok(RevisionPairHistoryEntry {
        source_revision_id,
        source_parent_revision_id,
        catalogue_revision_id,
        catalogue_parent_revision_id,
        is_active: RevisionPair::new(source_revision_id, catalogue_revision_id) == active_pair,
    })
}

pub(super) fn validate_revision_pair_listing(
    entries: &[RevisionPairHistoryEntry],
) -> Result<(), PostgresKernelError> {
    let mut by_catalogue = BTreeMap::new();
    let mut source_ids = BTreeSet::new();
    for entry in entries {
        let catalogue = entry.catalogue_revision_id();
        let catalogue_record =
            DurableRecord::new(CATALOGUE_REVISION_RELATION, catalogue.canonical());
        if by_catalogue.insert(catalogue, *entry).is_some() {
            return Err(catalogue_record.invariant("catalogue revision identities must be unique"));
        }
        if !source_ids.insert(entry.source_revision_id()) {
            return Err(DurableRecord::new(
                SOURCE_REVISION_RELATION,
                entry.source_revision_id().canonical(),
            )
            .invariant("source revision identities must be unique"));
        }
        if entry.source_parent_revision_id().is_some()
            != entry.catalogue_parent_revision_id().is_some()
        {
            return Err(catalogue_record.invariant(
                "source and catalogue parent identities must both be null or both be present",
            ));
        }
    }

    for entry in entries {
        let Some(parent_catalogue) = entry.catalogue_parent_revision_id() else {
            continue;
        };
        let Some(parent_source) = entry.source_parent_revision_id() else {
            return Err(DurableRecord::new(
                CATALOGUE_REVISION_RELATION,
                entry.catalogue_revision_id().canonical(),
            )
            .invariant(
                "each catalogue parent must exist and identify the corresponding source parent",
            ));
        };
        let Some(parent_entry) = by_catalogue.get(&parent_catalogue) else {
            return Err(DurableRecord::new(
                CATALOGUE_REVISION_RELATION,
                parent_catalogue.canonical(),
            )
            .invariant(
                "each catalogue parent must exist and identify the corresponding source parent",
            ));
        };
        if parent_entry.source_revision_id() != parent_source {
            return Err(DurableRecord::new(
                CATALOGUE_REVISION_RELATION,
                entry.catalogue_revision_id().canonical(),
            )
            .invariant(
                "each catalogue parent must exist and identify the corresponding source parent",
            ));
        }
    }

    let mut state = BTreeMap::new();
    for entry in entries {
        let mut current = entry.catalogue_revision_id();
        let mut path = Vec::new();
        loop {
            match state.get(&current).copied().unwrap_or(0) {
                0 => {
                    state.insert(current, 1);
                    path.push(current);
                    let Some(current_entry) = by_catalogue.get(&current) else {
                        return Err(DurableRecord::new(
                            CATALOGUE_REVISION_RELATION,
                            current.canonical(),
                        )
                        .invariant(
                            "each catalogue parent must exist and identify the corresponding source parent",
                        ));
                    };
                    let Some(parent) = current_entry.catalogue_parent_revision_id() else {
                        break;
                    };
                    current = parent;
                }
                1 => {
                    return Err(DurableRecord::new(
                        CATALOGUE_REVISION_RELATION,
                        current.canonical(),
                    )
                    .invariant(
                        "catalogue and source revision ancestry must terminate without repeated identities",
                    ));
                }
                2 => break,
                _ => unreachable!("revision listing state has only three values"),
            }
        }
        for catalogue in path {
            state.insert(catalogue, 2);
        }
    }

    if entries.iter().filter(|entry| entry.is_active()).count() != 1 {
        return Err(DurableRecord::new(ACTIVE_RELATION, "singleton=true")
            .invariant("exactly one listed revision pair must match the active marker"));
    }
    Ok(())
}
