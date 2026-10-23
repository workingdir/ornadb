//! Project-context admission for editable loose rows.
//!
//! The admission rules live in `orna_evaluator_v1::row_admission`, the boundary
//! the production application also uses. This module only adapts conformance
//! project units onto that boundary so the corpus exercises the same code.

use crate::ProjectUnit;
use orna_evaluator_v1::{
    Limits,
    row_admission::{self, RowSource},
};
use orna_semantic_v1::Analysis;

pub use orna_evaluator_v1::row_admission::{
    AdmittedLooseRow, RowAdmissionIdentity, RowResult, TableOwner,
};

/// Reject resource-invalid project input before semantic/module or row parsing.
pub fn preflight_project_rows(project: &ProjectUnit, limits: Limits) -> RowResult<()> {
    let modules = module_sources(project);
    let loose_rows = loose_row_sources(project);
    row_admission::preflight_row_sources(&modules, &loose_rows, limits)
}

pub fn admit_project_rows(
    project: &ProjectUnit,
    analysis: &Analysis,
    identity: &RowAdmissionIdentity,
    limits: Limits,
) -> RowResult<Vec<AdmittedLooseRow>> {
    let modules = module_sources(project);
    let loose_rows = loose_row_sources(project);
    row_admission::admit_row_sources(
        &project.project_id,
        &modules,
        analysis,
        &loose_rows,
        identity,
        limits,
    )
}

fn module_sources(project: &ProjectUnit) -> Vec<&str> {
    project
        .modules
        .iter()
        .map(|unit| unit.source.as_str())
        .collect()
}

fn loose_row_sources(project: &ProjectUnit) -> Vec<RowSource<'_>> {
    project
        .loose_rows
        .iter()
        .map(|row| RowSource {
            source_id: row.source_id.as_str(),
            source: row.source.as_str(),
        })
        .collect()
}
