//! Bounded paging over catalogue rows that are built once per activation.
//!
//! The `sys.catalog.*` grouped handles serve precomputed canonical rows. The
//! cursor is the row index, big-endian, so every continuation resumes at the
//! same position in the same row order.

use orna_value::CanonicalValue;

use super::{EvaluationError, RelationPage, StepBudget, error};

/// `sys.snapshot.incomplete`: the required snapshot objects are unavailable.
#[cfg(feature = "system-host-bindings")]
const SNAPSHOT_INCOMPLETE: &str = orna_sys::SysFailureCode::SnapshotIncomplete.as_str();
/// Without host bindings the generated identity crate is not linked; the exact
/// portable failure code bytes are preserved here.
#[cfg(not(feature = "system-host-bindings"))]
const SNAPSHOT_INCOMPLETE: &str = "sys.snapshot.incomplete";

/// Pages the rows of one `sys.catalog.*` relation, or fails closed.
///
/// `rows` is `None` when the activation holds no committed identity for its
/// admitted tables. The rows then cannot carry the references the specification
/// declares non-null (ORNA-SYS-111), so the read fails with
/// `sys.snapshot.incomplete` (required snapshot objects are unavailable) and
/// never serves a null or textual stand-in.
pub fn page_catalogue_rows(
    rows: Option<&[CanonicalValue]>,
    after: Option<&[u8]>,
    limit: usize,
    budget: &mut StepBudget,
) -> Result<RelationPage, EvaluationError> {
    let rows = rows.ok_or_else(|| error(SNAPSHOT_INCOMPLETE))?;
    page_precomputed_rows(rows, after, limit, budget)
}

/// Pages over precomputed catalogue rows, using the row index as the cursor.
fn page_precomputed_rows(
    rows: &[CanonicalValue],
    after: Option<&[u8]>,
    limit: usize,
    budget: &mut StepBudget,
) -> Result<RelationPage, EvaluationError> {
    let start = match after {
        None => 0,
        Some(cursor) => {
            let bytes: [u8; 8] = cursor.try_into().map_err(|_| error("ORNA-EVAL-VALUE"))?;
            usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| error("ORNA-EVAL-VALUE"))?
        }
    };
    if start > rows.len() {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let end = start.saturating_add(limit).min(rows.len());
    budget.debit(u64::try_from(end - start).map_err(|_| error("ORNA-EVAL-LIMIT"))?)?;
    Ok(RelationPage {
        rows: rows[start..end].to_vec(),
        next: (end < rows.len()).then(|| (end as u64).to_be_bytes().to_vec()),
    })
}
