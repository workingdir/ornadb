use crate::*;

#[cfg(feature = "system-host-bindings")]
pub(crate) fn system_relation_source(expression: &Expr) -> Option<&'static str> {
    let name = function_name(expression)?;
    match name.as_str() {
        name if name == SysRelation::Storage.name() => Some(SysRelation::Storage.name()),
        name if name == SysRelation::MaintenanceJob.name() => {
            Some(SysRelation::MaintenanceJob.name())
        }
        name if name == SysRelation::Materialization.name() => {
            Some(SysRelation::Materialization.name())
        }
        name if name == SysGroupedHandle::CatalogTables.name() => {
            Some(SysGroupedHandle::CatalogTables.name())
        }
        name if name == SysGroupedHandle::CatalogColumns.name() => {
            Some(SysGroupedHandle::CatalogColumns.name())
        }
        name => git_rows::served_source(name),
    }
}

/// Without host bindings the generated identity crate is not linked; the exact
/// portable name bytes are preserved here.
#[cfg(not(feature = "system-host-bindings"))]
pub(crate) fn system_relation_source(expression: &Expr) -> Option<&'static str> {
    match function_name(expression)?.as_str() {
        name if name == non_host_names::STORAGE => Some(non_host_names::STORAGE),
        name if name == non_host_names::MAINTENANCE_JOB => Some(non_host_names::MAINTENANCE_JOB),
        name if name == non_host_names::MATERIALIZATION => Some(non_host_names::MATERIALIZATION),
        name if name == non_host_names::CATALOG_TABLES => Some(non_host_names::CATALOG_TABLES),
        name if name == non_host_names::CATALOG_COLUMNS => Some(non_host_names::CATALOG_COLUMNS),
        name if name == non_host_names::GIT_BRANCHES => Some(non_host_names::GIT_BRANCHES),
        name if name == non_host_names::GIT_REFS => Some(non_host_names::GIT_REFS),
        name if name == non_host_names::GIT_REMOTES => Some(non_host_names::GIT_REMOTES),
        name if name == non_host_names::GIT_STASHES => Some(non_host_names::GIT_STASHES),
        name if name == non_host_names::GIT_TAGS => Some(non_host_names::GIT_TAGS),
        name if name == non_host_names::GIT_COMMITS => Some(non_host_names::GIT_COMMITS),
        name if name == non_host_names::GIT_OBJECTS => Some(non_host_names::GIT_OBJECTS),
        name if name == non_host_names::GIT_TREE_ENTRIES => Some(non_host_names::GIT_TREE_ENTRIES),
        name if name == non_host_names::GIT_REPOSITORIES => Some(non_host_names::GIT_REPOSITORIES),
        name if name == non_host_names::GIT_WORKTREE => Some(non_host_names::GIT_WORKTREE),
        _ => None,
    }
}

#[cfg(feature = "system-host-bindings")]
pub(crate) const SYS_MATERIALIZATION_SOURCE: &str = SysRelation::Materialization.name();
#[cfg(not(feature = "system-host-bindings"))]
pub(crate) const SYS_MATERIALIZATION_SOURCE: &str = non_host_names::MATERIALIZATION;

/// Exact portable name bytes for builds without the generated identity crate.
#[cfg(not(feature = "system-host-bindings"))]
pub(crate) mod non_host_names {
    pub(crate) const STORAGE: &str = "sys.Storage";
    pub(crate) const MAINTENANCE_JOB: &str = "sys.MaintenanceJob";
    pub(crate) const MATERIALIZATION: &str = "sys.Materialization";
    pub(crate) const CATALOG_TABLES: &str = "sys.catalog.tables";
    pub(crate) const CATALOG_COLUMNS: &str = "sys.catalog.columns";
    pub(crate) const GIT_BRANCHES: &str = "sys.git.branches";
    pub(crate) const GIT_REFS: &str = "sys.git.refs";
    pub(crate) const GIT_REMOTES: &str = "sys.git.remotes";
    pub(crate) const GIT_STASHES: &str = "sys.git.stashes";
    pub(crate) const GIT_TAGS: &str = "sys.git.tags";
    pub(crate) const GIT_COMMITS: &str = "sys.git.commits";
    pub(crate) const GIT_OBJECTS: &str = "sys.git.objects";
    pub(crate) const GIT_TREE_ENTRIES: &str = "sys.git.tree_entries";
    pub(crate) const GIT_REPOSITORIES: &str = "sys.git.repositories";
    pub(crate) const GIT_WORKTREE: &str = "sys.git.worktree";
    pub(crate) const BLOB_UNAVAILABLE: &str = "sys.blob.unavailable";
    pub(crate) const BLOB_CORRUPT: &str = "sys.blob.corrupt";
    pub(crate) const BLOB_REFERENCE_CONTEXT: &str = "sys.blob.reference_context";
    pub(crate) const CODEC_UNSUPPORTED_VALUE_PROFILE: &str = "sys.codec.unsupported_value_profile";
}

/// A closed `sys.<Enum>.<variant>` constant, unless a local binding shadows
/// the `sys` root. Enum constants are pure values, so no host is consulted.
pub(crate) fn system_enum_constant(expression: &Expr, scope: &Scope) -> Option<(String, String)> {
    let mut root = expression;
    while let Expr::Field { base, .. } = root {
        root = base;
    }
    let Expr::Name { text, .. } = root else {
        return None;
    };
    if text != "sys" || scope.0.contains_key("sys") {
        return None;
    }
    let mut segments = Vec::new();
    let mut current = expression;
    while let Expr::Field { base, name, .. } = current {
        segments.push(name.as_str());
        current = base;
    }
    segments.push("sys");
    segments.reverse();
    orna_check::system_enum_constant(&segments)
}
