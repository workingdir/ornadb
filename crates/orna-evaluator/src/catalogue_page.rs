//! Bounded paging over catalogue rows that are built once per activation.
//!
//! The `sys.catalog.*` grouped handles serve precomputed canonical rows. The
//! cursor is the row index, big-endian, so every continuation resumes at the
//! same position in the same row order.

use orna_value::CanonicalValue;

use super::{EvaluationError, RelationPage, StepBudget, error};

/// Pages over precomputed catalogue rows, using the row index as the cursor.
pub fn page_precomputed_rows(
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
