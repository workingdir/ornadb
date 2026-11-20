//! Committed relation reads: the rows a table's bare name and its committed
//! `oid:<hex>` identities hold in `table_row`, read whole or in key-ordered pages.

use super::{TableObjectId, table_storage_key, validate_table_name};
use crate::error::RuntimeError;
use crate::table_snapshot::MAX_RUNTIME_QUERY_PAGE_ROWS;
use libsql::{Connection, params};

/// One page of a committed relation: the rows after a key, in canonical key order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedTablePage {
    /// `(runtime key, stored tuple)` rows of this page, in key order.
    pub rows: Vec<(Vec<u8>, Vec<u8>)>,
    /// The key to resume after, present only when more rows exist.
    pub next: Option<Vec<u8>>,
}

/// The storage names a committed relation's rows may sit under: the bare
/// table name and the first two `oid:<hex>` names of its committed identity
/// (bead ornadb-jm0d). Rows under a later identity name are not read.
async fn committed_storage_names(
    connection: &Connection,
    table: &str,
) -> Result<[String; 3], RuntimeError> {
    let mut names = [table.to_owned(), String::new(), String::new()];
    let mut slot = 1;
    let mut identities = connection
        .query(
            "SELECT object_id FROM table_identity WHERE table_name = ?1",
            params![table],
        )
        .await?;
    while let Some(row) = identities.next().await? {
        let bytes: Vec<u8> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
        let Ok(id) = <[u8; 16]>::try_from(bytes.as_slice()) else {
            continue;
        };
        if slot == names.len() {
            break;
        }
        names[slot] = table_storage_key(table, Some(TableObjectId::new(id)));
        slot += 1;
    }
    Ok(names)
}

/// Reads one complete committed relation in canonical key order.
pub(super) async fn committed_relation_rows(
    connection: &Connection,
    table: &str,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, RuntimeError> {
    validate_table_name(table)?;
    let storage = committed_storage_names(connection, table).await?;
    let mut rows = connection
        .query(
            "SELECT row_key, row_value FROM table_row
             WHERE table_id = ?1 OR table_id = ?2 OR table_id = ?3
             ORDER BY row_key, table_id",
            params![storage[0].clone(), storage[1].clone(), storage[2].clone()],
        )
        .await?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().await? {
        result.push((
            row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
        ));
    }
    Ok(result)
}

/// Reads at most `limit` rows of the committed relation `table` whose key
/// follows `after` (the first rows when `None`), in canonical key order. The
/// page holds at most `limit` rows, plus one lookahead row that says whether
/// another page follows.
pub(super) async fn committed_relation_page(
    connection: &Connection,
    table: &str,
    after: Option<&[u8]>,
    limit: usize,
) -> Result<CommittedTablePage, RuntimeError> {
    validate_table_name(table)?;
    if limit == 0 || limit > MAX_RUNTIME_QUERY_PAGE_ROWS {
        return Err(RuntimeError::InvalidTablePageLimit);
    }
    let storage = committed_storage_names(connection, table).await?;
    let row_limit = i64::try_from(limit + 1).map_err(|_| RuntimeError::InvalidTablePageLimit)?;
    let mut rows = connection
        .query(
            "SELECT row_key, row_value FROM (
                 SELECT * FROM (SELECT row_key, row_value, table_id FROM table_row
                     WHERE table_id = ?1 AND row_key >= ?5 AND (?4 = 0 OR row_key > ?5)
                     ORDER BY row_key LIMIT ?6)
                 UNION ALL
                 SELECT * FROM (SELECT row_key, row_value, table_id FROM table_row
                     WHERE table_id = ?2 AND row_key >= ?5 AND (?4 = 0 OR row_key > ?5)
                     ORDER BY row_key LIMIT ?6)
                 UNION ALL
                 SELECT * FROM (SELECT row_key, row_value, table_id FROM table_row
                     WHERE table_id = ?3 AND row_key >= ?5 AND (?4 = 0 OR row_key > ?5)
                     ORDER BY row_key LIMIT ?6)
             )
             ORDER BY row_key, table_id
             LIMIT ?6",
            params![
                storage[0].clone(),
                storage[1].clone(),
                storage[2].clone(),
                i64::from(after.is_some()),
                after.unwrap_or_default().to_vec(),
                row_limit
            ],
        )
        .await?;
    let mut fetched: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(limit + 1);
    while let Some(row) = rows.next().await? {
        fetched.push((
            row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?,
        ));
    }
    if fetched.len() <= limit {
        return Ok(CommittedTablePage {
            rows: fetched,
            next: None,
        });
    }
    // A key stored under two names that straddles the boundary has no single
    // resume position, so the page fails closed rather than skip a row.
    if fetched[limit - 1].0 == fetched[limit].0 {
        return Err(RuntimeError::RecoveryInvalid);
    }
    fetched.truncate(limit);
    let next = fetched.last().map(|(key, _)| key.clone());
    Ok(CommittedTablePage {
        rows: fetched,
        next,
    })
}
