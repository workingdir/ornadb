use super::{
    Diagnostic, Endpoint, ProjectEnvironment, ProjectExpectations, ProjectUnit, RuntimeIdentity,
    SourceUnit, cli_check_backend, cli_commit, local_project_path, project_load_diagnostic,
};

pub(crate) fn load_project(endpoint: &Endpoint) -> Result<orna_project::LoadedProject, Diagnostic> {
    let project = load_project_without_standard_rejection(endpoint)?;
    reject_uncaptured_standard_modules(&project)?;
    Ok(project)
}

pub(crate) fn load_project_without_standard_rejection(
    endpoint: &Endpoint,
) -> Result<orna_project::LoadedProject, Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "project Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let project = orna_project::ProjectLoader::default()
        .load(&repository)
        .map_err(project_load_diagnostic)?;
    Ok(project)
}

pub(crate) fn reject_uncaptured_standard_modules(
    project: &orna_project::LoadedProject,
) -> Result<(), Diagnostic> {
    if !project.has_uncaptured_standard_imports() {
        return Ok(());
    }
    Err(Diagnostic::target(
        "ORNA-S010-IMPORT",
        "imported module is unavailable",
        "use a captured standard dependency or remove the import",
    ))
}

pub(crate) fn semantic_catalogue(
    project: &orna_project::LoadedProject,
) -> Result<orna_check::Catalogue, Diagnostic> {
    project.production_catalogue().map_err(|_| {
        Diagnostic::target(
            "ORNA-S010-IMPORT",
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    })
}

pub(crate) fn check_project(endpoint: &Endpoint, color_enabled: bool) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    cli_check_backend::check_project(&project, color_enabled)
}

pub(crate) fn execution_project(project: &orna_project::LoadedProject) -> ProjectUnit {
    let mut modules: Vec<SourceUnit> = project
        .modules()
        .iter()
        .zip(project.identities())
        .map(|(module, identity)| SourceUnit {
            fixture_id: "cli-project".into(),
            source_id: identity.logical_path().into(),
            parse_as: "module_unit".into(),
            source: module.source.clone(),
        })
        .collect();
    let loose_rows: Vec<SourceUnit> = project
        .loose_rows()
        .iter()
        .map(|row| SourceUnit {
            fixture_id: "cli-project".into(),
            source_id: row.logical_path().into(),
            parse_as: row.parse_as().into(),
            source: row.source().into(),
        })
        .collect();
    modules.extend(
        project
            .standard_sources()
            .iter()
            .filter(|(logical_path, _)| project.standard_runtime_modules().contains(logical_path))
            .map(|(logical_path, source)| SourceUnit {
                fixture_id: "cli-project".into(),
                source_id: logical_path.clone(),
                parse_as: "module_unit".into(),
                source: source.clone(),
            }),
    );
    ProjectUnit {
        fixture_id: "cli-project".into(),
        project_id: "cli-project".into(),
        environment_id: None,
        modules,
        loose_rows,
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.1.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

/// The bounded pure path receives reachable standard modules through
/// `load_standard_sources`, which verifies them against the captured profile.
/// The project unit therefore carries neither those source units nor a
/// standard dependency, so admission has nothing further to verify.
pub(crate) fn pure_execution_project(project: &orna_project::LoadedProject) -> ProjectUnit {
    let mut execution = execution_project(project);
    execution
        .modules
        .retain(|unit| !project.standard_runtime_modules().contains(&unit.source_id));
    execution
}

pub(crate) fn runtime_identity(
    repository: &orna_repository::Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), Diagnostic> {
    orna_cli_support::runtime_identity::repository_runtime_identity(repository).map_err(|refusal| match refusal.0 {
        orna_runtime::RuntimeError::UnknownHeadFormat(
            refusal @ orna_repository::FormatContextError::MixedFormats { .. },
        ) => cli_commit::mixed_formats_at_head(&refusal),
        orna_runtime::RuntimeError::UnknownHeadFormat(refusal) => Diagnostic::target(
            refusal.code(),
            "repository format is unknown",
            "the repository HEAD records a format this build does not know; use a build that supports it",
        ),
        _ => Diagnostic::target(
            "E2200",
            "repository runtime metadata is unavailable",
            "initialize the Orna repository before running a durable project invocation",
        ),
    })
}
