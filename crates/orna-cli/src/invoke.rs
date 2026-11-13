use super::{
    ApplicationAuthority, BoundedEvaluator, Cow, Diagnostic, DurableTransactionalEvaluator,
    Endpoint, Environment, Exit, Limits, ProjectUnit, RuntimeEvaluator, RuntimeMode, StageOutcome,
    StdSnapshotPin, cancellation_diagnostic, committed_storage_orders, committed_table_object_ids,
    diagnostic_documentation, execution_project, load_project, local_project_path,
    project_load_diagnostic, pure_execution_project, pure_sys_host_context,
    refuse_legacy_bare_module_rows, request_nonce, run_committed_table_invocation,
    runtime_identity, runtime_publication_metadata_rows, semantic_catalogue, sys_host,
    write_success_line,
};

pub(crate) fn run_project_invocation(
    endpoint: &Endpoint,
    root_entry: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    run_project_invocation_with_arguments(endpoint, root_entry, &Environment::new(), color_enabled)
}

pub(crate) fn public_project_function(analysis: &orna_check::Analysis, target: &str) -> bool {
    let Some((module, function)) = target.rsplit_once('.') else {
        return false;
    };
    let namespace = if module == "main" {
        Vec::new()
    } else {
        module.split('.').map(str::to_owned).collect()
    };
    if function.is_empty() || namespace.iter().any(String::is_empty) {
        return false;
    }
    analysis
        .modules
        .get(&orna_check::Namespace(namespace))
        .and_then(|module| module.exports.get(function))
        .is_some_and(|symbol| symbol.kind == orna_check::SymbolKind::Function)
}
pub(crate) fn run_public_project_function(
    endpoint: &Endpoint,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    run_project_function(endpoint, target, false, color_enabled)
}
pub(crate) fn run_default_project_main(
    endpoint: &Endpoint,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    run_project_function(endpoint, "main.main", true, color_enabled)
}
pub(crate) fn run_project_function(
    endpoint: &Endpoint,
    target: &str,
    allow_private_root_main: bool,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = std::sync::Arc::new(semantic_catalogue(&project)?);
    let analysis = orna_check::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_check::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    let private_default_main_present = allow_private_root_main
        && target == "main.main"
        && analysis
            .modules
            .get(&orna_check::Namespace(Vec::new()))
            .and_then(|module| module.symbols.get("main"))
            .is_some_and(|symbol| symbol.kind == orna_check::SymbolKind::Function);
    if allow_private_root_main && !private_default_main_present {
        return Err(Diagnostic::target(
            "E2103",
            "no root main(): main.orna declares no `main()` function",
            "declare `main()` in main.orna, or run a qualified function such as `orna run MODULE.FUNCTION`",
        ));
    }
    if !private_default_main_present && !public_project_function(&analysis, target) {
        return Err(Diagnostic::unavailable(
            "durable project invocation is not available",
            "use a supported table transaction; stream roots require the explicit stream runtime",
        ));
    }
    let execution = execution_project(&project);
    if DurableTransactionalEvaluator::default()
        .with_catalogue(std::sync::Arc::clone(&catalogue))
        .project_stream_root_admitted(&execution, target)
    {
        return run_project_stream_invocation_with_project(
            endpoint,
            target,
            &execution,
            sys_host::std_snapshot_pin(&project),
            catalogue,
            color_enabled,
        );
    }
    run_project_invocation(endpoint, target, color_enabled)
}
pub(crate) fn run_project_stream_invocation(
    endpoint: &Endpoint,
    root_entry: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = std::sync::Arc::new(semantic_catalogue(&project)?);
    let analysis = orna_check::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_check::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    let execution = execution_project(&project);
    run_project_stream_invocation_with_project(
        endpoint,
        root_entry,
        &execution,
        sys_host::std_snapshot_pin(&project),
        catalogue,
        color_enabled,
    )
}
pub(crate) fn run_project_invocation_with_arguments(
    endpoint: &Endpoint,
    root_entry: &str,
    arguments: &Environment,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository =
        orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not be discovered",
                "run the command inside an initialized local Git worktree",
            )
        })?;
    let project = load_project(endpoint)?;
    let catalogue = admit_project_analysis(&project)?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    refuse_legacy_bare_module_rows(&repository, identity, initial_digest, &project)?;
    let (request, fingerprint) = DurableTransactionalEvaluator::project_request_identity(
        identity,
        initial_digest,
        root_entry,
        arguments,
        request_nonce(),
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "project request identity could not be created",
            "retry the durable project invocation",
        )
    })?;
    let std_snapshot = sys_host::std_snapshot_pin(&project);
    let table_object_ids = committed_table_object_ids(&repository, &project)?;
    let storage_orders = committed_storage_orders(&repository, &project)?;
    // Admission is bound to the committed snapshot when there is one; a
    // repository with no commit yet runs from the worktree project, which is
    // what the seed test exercises (bead ornadb-jm0d).
    let loaded_project = match repository.head() {
        Ok(Some(head)) => orna_project::ProjectLoader::default()
            .load_committed_snapshot(&repository, &head)
            .map_err(project_load_diagnostic)?,
        _ => project.clone(),
    };
    let project = execution_project(&loaded_project);
    let owner_id = orna_runtime::process_owner_id();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not start",
                "retry the durable project invocation",
            )
        })?
        .block_on(
            DurableTransactionalEvaluator::default()
                .with_catalogue(catalogue)
                .with_sys_host_context(sys_host::sys_host_context(
                    repository.worktree(),
                    identity,
                    RuntimeMode::Run,
                    std_snapshot,
                ))
                .execute_project_request_with_arguments(
                    orna_conformance::RuntimeTarget {
                        repository: &repository,
                        identity,
                        owner_id,
                        initial_digest,
                    },
                    request,
                    fingerprint,
                    &project,
                    root_entry,
                    arguments,
                    &loaded_project,
                    &table_object_ids,
                    &storage_orders,
                ),
        )
        .map_err(|error| durable_invocation_failed(&error))?;
    report_project_outcome(endpoint, root_entry, arguments, color_enabled, outcome)
}

/// The production catalogue of a project that passes semantic analysis.
pub(crate) fn admit_project_analysis(
    project: &orna_project::LoadedProject,
) -> Result<std::sync::Arc<orna_check::Catalogue>, Diagnostic> {
    let catalogue = std::sync::Arc::new(semantic_catalogue(project)?);
    let analysis = orna_check::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_check::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    Ok(catalogue)
}

/// Routes a stream-root skip to the stream seam and reports the transaction's
/// outcome.
pub(crate) fn report_project_outcome(
    endpoint: &Endpoint,
    root_entry: &str,
    arguments: &Environment,
    color_enabled: bool,
    outcome: StageOutcome<orna_value::Diagnostic>,
) -> Result<(), Diagnostic> {
    // Project transaction admission does not run stream roots; those route to
    // the explicit finite-list stream seam. The adapter reports that skip as a
    // typed reason, so the routing cannot drift from its text.
    if arguments.is_empty() && outcome.is_stream_root_admission_skip() {
        return run_project_stream_invocation(endpoint, root_entry, color_enabled);
    }
    match outcome {
        StageOutcome::Passed => write_success_line("invocation completed", color_enabled),
        StageOutcome::Failed(diagnostic) => Err(
            if let Some(mut published) = published_runtime_failure(&diagnostic) {
                published.detail = Some(
                    "the project transaction failed atomically; no partial changes were committed"
                        .into(),
                );
                published
            } else {
                let mut rejection = Diagnostic::target(
                    "E2200",
                    "durable project invocation was rejected",
                    "the project transaction failed atomically; no partial changes were committed",
                );
                rejection.detail = Some(failure_detail(&diagnostic));
                rejection
            },
        ),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "durable project invocation is not available",
            "use a supported table transaction; stream roots require the explicit stream runtime",
        )),
    }
}
pub(crate) fn run_project_stream_invocation_with_project(
    endpoint: &Endpoint,
    root_entry: &str,
    project: &ProjectUnit,
    std_snapshot: StdSnapshotPin,
    catalogue: std::sync::Arc<orna_check::Catalogue>,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository =
        orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not be discovered",
                "run the command inside an initialized local Git worktree",
            )
        })?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let owner_id = orna_runtime::process_owner_id();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not start",
                "retry the durable project invocation",
            )
        })?
        .block_on(
            DurableTransactionalEvaluator::default()
                .with_catalogue(catalogue)
                .with_sys_host_context(sys_host::sys_host_context(
                    repository.worktree(),
                    identity,
                    RuntimeMode::Run,
                    std_snapshot,
                ))
                .execute_project_stream(
                    &repository,
                    identity,
                    owner_id,
                    initial_digest,
                    project,
                    root_entry,
                ),
        )
        .map_err(|error| durable_invocation_failed(&error))?;
    match outcome {
        StageOutcome::Passed => write_success_line("invocation completed", color_enabled),
        StageOutcome::Failed(diagnostic) => Err(stream_delivery_failed(&diagnostic)),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "durable project stream invocation is not available",
            "use a supported finite-list stream root",
        )),
    }
}

/// Publishes a runtime failure under its catalogued Orna code, with the
/// catalogue title and help. Uncatalogued evaluator codes return `None` so each
/// caller keeps its own fallback; no unpublished code is promoted.
pub(crate) fn published_runtime_failure(diagnostic: &orna_value::Diagnostic) -> Option<Diagnostic> {
    let documentation = diagnostic_documentation(diagnostic.code())?;
    Some(Diagnostic {
        code: documentation.code,
        title: Cow::Borrowed(documentation.title),
        help: documentation.help,
        exit: Exit::Target,
        detail: None,
        body: None,
    })
}

/// Carries the runtime failure's public code, or its non-disclosing display
/// text, into the diagnostic detail. The detail is shown with `--debug` only
/// (ORNA-UX-004 keeps technical causes out of the default rendering).
pub(crate) fn runtime_failure_detail(error: &orna_runtime::RuntimeError) -> String {
    if let orna_runtime::RuntimeError::LegacyHeadReadOnly(refusal) = error {
        return format!("runtime failure: {} ({})", refusal.code(), refusal.title());
    }
    let cause = error
        .public_code()
        .map_or_else(|| error.to_string(), |code| code.as_str().to_owned());
    format!("runtime failure: {cause}")
}

/// Renders the evaluator failure code with the codes of its nested causes.
/// Messages and spans stay redacted at the evaluator boundary.
pub(crate) fn failure_detail(diagnostic: &orna_value::Diagnostic) -> String {
    let causes = diagnostic.causes();
    if causes.is_empty() {
        return format!("failure code: {}", diagnostic.code());
    }
    let codes = causes
        .iter()
        .map(orna_value::Diagnostic::code)
        .collect::<Vec<_>>()
        .join(", ");
    format!("failure code: {} (causes: {codes})", diagnostic.code())
}

/// Maps a failed durable project invocation to `E2200`. The runtime's public
/// code, or its non-disclosing display text, is carried in the detail, which
/// renders only with `--debug` (ORNA-UX-004). A legacy head refusal names its
/// own remedy, because retrying the transaction cannot clear it.
pub(crate) fn durable_invocation_failed(error: &orna_runtime::RuntimeError) -> Diagnostic {
    let help = match error {
        orna_runtime::RuntimeError::LegacyHeadReadOnly(refusal) => refusal.remedy(),
        _ => "retry the project transaction after checking its durable state",
    };
    let mut diagnostic = Diagnostic::target("E2200", "durable project invocation failed", help);
    diagnostic.detail = Some(runtime_failure_detail(error));
    diagnostic
}

/// Maps a stream delivery the runtime rolled back to `E2200`, carrying the
/// evaluator failure code and its nested cause codes in the `--debug` detail.
pub(crate) fn stream_delivery_failed(diagnostic: &orna_value::Diagnostic) -> Diagnostic {
    let mut delivery = Diagnostic::target(
        "E2200",
        "durable project stream delivery failed",
        "the failed delivery was rolled back; prior committed deliveries and their checkpoint progress remain; inspect the recorded failure before retrying the stream",
    );
    delivery.detail = Some(failure_detail(diagnostic));
    delivery
}

/// A multi-module project that declares a table cannot run one-shot: the bounded
/// pure evaluator rejects any table declaration, and the committed-table route
/// admits a single module only. Single-module projects never reach this check.
pub(crate) fn multi_module_declares_tables(project: &orna_project::LoadedProject) -> bool {
    project.modules().len() != 1
        && project.modules().iter().any(|module| {
            orna_syntax::parse_module(&module.source)
                .value
                .items
                .iter()
                .any(|item| matches!(item.declaration, orna_syntax_ast::Declaration::Table { .. }))
        })
}

pub(crate) fn run_pure_invocation(
    endpoint: &Endpoint,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    if run_authority_invocation(endpoint, &project, target, color_enabled)? {
        return Ok(());
    }
    admit_function_only_project(&project)?;
    run_bounded_invocation(&project, target, color_enabled)
}

/// Admits and evaluates `target` through the authority route when the only
/// module is the one `target` names. Returns whether it answered, so every
/// other project takes the bounded function-only path.
pub(crate) fn run_authority_invocation(
    endpoint: &Endpoint,
    project: &orna_project::LoadedProject,
    target: &str,
    color_enabled: bool,
) -> Result<bool, Diagnostic> {
    // Single-module projects that import pinned standard modules take the
    // authority route only when they need publication metadata. Every other
    // standard-importing project takes the bounded path below, which loads the
    // reachable standard bodies through the verified profile.
    let std_imports = !project.standard_runtime_modules().is_empty();
    if project.modules().len() != 1 {
        return Ok(false);
    }
    let identity = &project.identities()[0];
    let module = &project.modules()[0];
    let (module_name, function) = target.rsplit_once('.').unwrap_or(("", target));
    let namespace = if module_name.is_empty() {
        Vec::new()
    } else {
        module_name
            .split('.')
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    if identity.namespace() != namespace {
        return Ok(false);
    }
    let authority = ApplicationAuthority::new(
        orna_check::Catalogue::authoritative_core(),
        Limits::default(),
    )
    .with_loaded_standard(project)
    .map_err(|_| {
        Diagnostic::target(
            "ORNA-S010-IMPORT",
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    })?;
    let admission = authority
        .admit_module(identity.logical_path(), module.source.clone(), function)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2101",
                "project semantic analysis failed",
                "fix the first reported source contract error, then run check again",
                error.to_string(),
            )
        });
    let admitted = match admission {
        Ok(admitted) => Some(admitted),
        // The bounded path reports the analysis error for std-importing projects.
        Err(_) if std_imports => None,
        Err(error) => return Err(error),
    };
    let Some(admitted) =
        admitted.filter(|candidate| !std_imports || candidate.requires_publication_metadata())
    else {
        return Ok(false);
    };
    if !admitted.table_schemas().is_empty() && admitted.entry_references_tables() {
        run_committed_table_invocation(
            endpoint,
            authority,
            &admitted,
            identity.logical_path(),
            &module.source,
            sys_host::std_snapshot_pin(project),
        )?;
    } else if admitted.requires_publication_metadata() {
        let (publication_rows, sys_host_context) =
            runtime_publication_metadata_rows(endpoint, sys_host::std_snapshot_pin(project))?;
        let authority = authority.with_sys_host_context(sys_host_context);
        let activation = authority
            .evaluate_staged_with_publication_rows(&admitted, &Environment::new(), publication_rows)
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "pure project invocation failed",
                    "fix the admitted function or retry the invocation",
                    error.to_string(),
                )
            })?;
        if !activation.mutations().is_empty() {
            return Err(Diagnostic::unavailable(
                "one-shot invocation cannot commit table mutations",
                "use a supported table transaction; stream roots require the explicit stream runtime",
            ));
        }
    } else {
        let std_snapshot = sys_host::std_snapshot_pin(project);
        let authority = match pure_sys_host_context(endpoint, std_snapshot) {
            Some(sys_host_context) => authority.with_sys_host_context(sys_host_context),
            None => authority,
        };
        authority
            .evaluate(&admitted, &Environment::new())
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "pure project invocation failed",
                    "fix the admitted function or retry the invocation",
                    error.to_string(),
                )
            })?;
    }
    write_success_line("invocation completed", color_enabled)?;
    Ok(true)
}

/// Proves the project is function-only and can be read by the bounded
/// evaluator.
pub(crate) fn admit_function_only_project(
    project: &orna_project::LoadedProject,
) -> Result<(), Diagnostic> {
    let catalogue = semantic_catalogue(project)?;
    let analysis = orna_check::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_check::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run check again",
        ));
    }
    if multi_module_declares_tables(project) {
        return Err(Diagnostic::target(
            "E2200",
            "table reads in multi-module projects are not supported by one-shot invocation; use the REPL or orna run",
            "one-shot invocation admits table declarations only in single-module projects",
        ));
    }
    Ok(())
}

/// Runs `target` through the bounded function-only evaluator, loading the
/// verified standard profile first when the project pins one.
pub(crate) fn run_bounded_invocation(
    project: &orna_project::LoadedProject,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let mut evaluator = BoundedEvaluator::default();
    if let Some(profile) = project.standard_profile() {
        // Admission supplies the runtime-reachable closure (requested roots plus
        // `main`) and the profile must accept exactly that key set, so every
        // supplied module digest is verified before any body is evaluated.
        let sources = project.standard_sources().to_vec();
        match evaluator.load_standard_sources(profile, sources) {
            StageOutcome::Passed => {}
            StageOutcome::Cancelled(diagnostic) => {
                return Err(cancellation_diagnostic(&diagnostic));
            }
            StageOutcome::Failed(diagnostic) => {
                return Err(published_runtime_failure(&diagnostic).unwrap_or_else(|| {
                    Diagnostic::target_with_detail(
                        "ORNA-S010-IMPORT",
                        "imported module is unavailable",
                        "use a captured standard dependency or remove the import",
                        format!(
                            "standard dependency load rejected with {}",
                            diagnostic.code()
                        ),
                    )
                }));
            }
            StageOutcome::Skipped { .. } => {
                return Err(Diagnostic::target(
                    "ORNA-S010-IMPORT",
                    "imported module is unavailable",
                    "use a captured standard dependency or remove the import",
                ));
            }
        }
    }
    match evaluator.evaluate_project(&pure_execution_project(project)) {
        StageOutcome::Passed => {}
        StageOutcome::Cancelled(diagnostic) => return Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Failed(diagnostic) => {
            return Err(published_runtime_failure(&diagnostic).unwrap_or_else(|| {
                Diagnostic::unavailable(
                    "one-shot invocation requires an executable function-only project",
                    "use reachable modules containing only pure functions; tables, effects, and streams require the integrated runtime",
                )
            }));
        }
        StageOutcome::Skipped { .. } => {
            return Err(Diagnostic::unavailable(
                "one-shot invocation requires an executable function-only project",
                "use reachable modules containing only pure functions; tables, effects, and streams require the integrated runtime",
            ));
        }
    }
    match evaluator.invoke(target) {
        StageOutcome::Passed => write_success_line("invocation completed", color_enabled),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Failed(diagnostic) => Err(published_runtime_failure(&diagnostic)
            .unwrap_or_else(|| {
                Diagnostic::unavailable(
                    "requested invocation is not available",
                    "define a reachable zero-argument pure function with the requested name",
                )
            })),
        StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "requested invocation is not available",
            "define a reachable zero-argument pure function with the requested name",
        )),
    }
}
