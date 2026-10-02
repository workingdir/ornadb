//! Typed, project-pinned admission for the bounded pure REPL.
//!
//! This is an execution-facing boundary: a caller supplies one already loaded
//! immutable project and optional verified standard sources, then receives an
//! isolated session which parses source once, stages semantic state, rejects
//! effects before evaluation, and publishes semantic/runtime successors
//! together. It intentionally does not provide tables, activation writes,
//! clocks, external effects, or presentation execution.

use orna_foundation_v1::{
    CanonicalValue, Diagnostic as FoundationDiagnostic, DiagnosticSeverity, SafeText,
};
use orna_project_v1::{AttachedDatabaseSession, LoadedProject};
use orna_semantic_v1::{
    Analysis, Catalogue, EffectSummary, ModuleInput, ReplAdmission, ReplContext, SymbolKind, Type,
    StandardDependencyProfile, analyze_with_catalogue,
};
use orna_syntax_v1::{Declaration, ReplInput, parse_module};

use crate::{
    CancellationToken, Environment, EvaluationError, Functions, Limits, PureFunction, ReplSession,
    parse_admitted_repl, reference_standard_profile, reference_standard_sources,
};

/// Redacted failure from the admitted REPL boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplError {
    diagnostic: Box<FoundationDiagnostic>,
}

impl ReplError {
    fn runtime(error: EvaluationError) -> Self {
        Self::redacted(error.code())
    }

    fn semantic(code: &str) -> Self {
        Self::redacted(code)
    }

    fn fixed(code: &'static str) -> Self {
        Self::redacted(code)
    }

    fn redacted(code: &str) -> Self {
        let diagnostic = FoundationDiagnostic::new(
            SafeText::new(code.to_owned()).expect("stable diagnostic code"),
            DiagnosticSeverity::Error,
            SafeText::redacted(),
        )
        .expect("valid redacted diagnostic")
        .redacted();
        Self {
            diagnostic: Box::new(diagnostic),
        }
    }

    /// Stable redacted diagnostic code.
    pub fn code(&self) -> &str {
        self.diagnostic.code()
    }

    /// Returns the structured diagnostic with source and host text redacted.
    ///
    /// The caller may attach a request reference before crossing a protocol
    /// boundary; this value never contains the submitted source.
    pub fn diagnostic(&self) -> &FoundationDiagnostic {
        self.diagnostic.as_ref()
    }

    fn status_value(&self) -> CanonicalValue {
        CanonicalValue::new(orna_foundation_v1::OvbRaw::Map(vec![
            (
                orna_foundation_v1::OvbRaw::Text("code".into()),
                orna_foundation_v1::OvbRaw::Text(self.code().into()),
            ),
            (
                orna_foundation_v1::OvbRaw::Text("message".into()),
                orna_foundation_v1::OvbRaw::Text("<redacted>".into()),
            ),
            (
                orna_foundation_v1::OvbRaw::Text("redacted".into()),
                orna_foundation_v1::OvbRaw::Bool(true),
            ),
            (
                orna_foundation_v1::OvbRaw::Text("severity".into()),
                orna_foundation_v1::OvbRaw::Text("error".into()),
            ),
        ]))
        .expect("redacted status record is canonical")
    }
}

/// Canonically admitted source ready for a transaction-aware evaluator.
///
/// This boundary owns no table writes or runtime commit. It carries the
/// original parsed input together with the semantic result type and effect
/// summary produced by [`ReplContext::stage`], plus the opaque semantic
/// successor that may be committed after a downstream activation succeeds.
#[derive(Clone, Debug)]
pub struct StagedReplActivation {
    input: ReplInput,
    result_type: Option<Type>,
    effects: EffectSummary,
    admission: ReplAdmission,
}

impl StagedReplActivation {
    /// The exact AST admitted by the parser and semantic checker.
    #[must_use]
    pub fn input(&self) -> &ReplInput {
        &self.input
    }

    /// The statically inferred successful result type, when the input yields
    /// one. Declarations and imports have no result type.
    #[must_use]
    pub fn result_type(&self) -> Option<&Type> {
        self.result_type.as_ref()
    }

    /// The canonical effect/failure summary for the admitted input.
    #[must_use]
    pub fn effects(&self) -> &EffectSummary {
        &self.effects
    }

    /// Publishes only the semantic successor after the downstream evaluator
    /// has completed its activation successfully.
    ///
    /// Runtime/table state is deliberately not touched here. A stale or
    /// foreign session rejects the handoff before semantic state changes.
    pub fn commit_semantic(self, session: &mut AdmittedReplSession) -> Result<(), ReplError> {
        let mut semantic = session.semantic.clone();
        semantic
            .commit(self.admission)
            .map_err(|_| ReplError::fixed("ORNA-REPL-COMMIT"))?;
        session.semantic = semantic;
        Ok(())
    }
}

/// An isolated, typed session against one admitted project snapshot.
///
/// Both the semantic and evaluator successors are staged before either is
/// published. A rejected input therefore leaves declarations, imports, and
/// the `$_` binding exactly as they were before the request.
#[derive(Clone, Debug)]
pub struct AdmittedReplSession {
    limits: Limits,
    semantic: ReplContext,
    runtime: ReplSession,
    attached_databases: Option<AttachedDatabaseSession>,
}

impl AdmittedReplSession {
    /// Starts a core-only typed REPL with no project bindings.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            semantic: ReplContext::empty(),
            runtime: ReplSession::new(limits),
            attached_databases: None,
        }
    }

    /// Creates a typed REPL over one primary committed database plus its
    /// exact, read-only attachments. Non-`std` modules are namespaced by the
    /// attachment alias; an optional `std` attachment is admitted as ordinary
    /// pinned source while core language and `sys` remain intrinsic.
    pub fn from_attached_database_session(
        databases: &AttachedDatabaseSession,
        limits: Limits,
    ) -> Result<Self, ReplError> {
        let modules = databases.module_inputs();
        let standard_sources = databases.standard_sources();
        let catalogue = if standard_sources.is_empty() {
            Catalogue::authoritative_core()
        } else {
            let snapshot = databases
                .standard_snapshot()
                .ok_or_else(|| ReplError::fixed("ORNA-REPL-STANDARD"))?;
            let profile = StandardDependencyProfile::from_sources(
                snapshot.as_str().to_owned(),
                standard_sources.clone(),
            )
            .map_err(|_| ReplError::fixed("ORNA-REPL-STANDARD"))?;
            Catalogue::authoritative_core()
                .with_standard_sources(&profile, standard_sources.clone())
                .map_err(|_| ReplError::fixed("ORNA-REPL-STANDARD"))?
        };
        let mut session = Self::from_catalogue(&modules, catalogue, standard_sources, limits)?;
        session.attached_databases = Some(databases.clone());
        Ok(session)
    }

    /// The committed database snapshots retained by this evaluator session,
    /// when it was created from an [`AttachedDatabaseSession`].
    pub fn attached_databases(&self) -> Option<&AttachedDatabaseSession> {
        self.attached_databases.as_ref()
    }

    /// Starts a typed session with the verified reference standard modules.
    ///
    /// The source bundle and semantic catalogue share the profile used by the
    /// local bounded REPL, so ordinary imports (including wildcard imports)
    /// resolve and execute under the same rules.
    pub fn with_reference_standard(limits: Limits) -> Result<Self, ReplError> {
        let standard_sources = reference_standard_sources().into_iter().collect::<Vec<_>>();
        let catalogue = Catalogue::authoritative_core()
            .with_standard_sources(&reference_standard_profile(), standard_sources.clone())
            .map_err(|_| ReplError::fixed("ORNA-REPL-STANDARD"))?;
        Self::from_catalogue(&[], catalogue, standard_sources, limits)
    }

    /// Starts an isolated session from an already admitted catalogue and
    /// caller supplied module and standard sources.
    pub fn from_catalogue(
        modules: &[ModuleInput],
        catalogue: Catalogue,
        standard_sources: impl IntoIterator<Item = (String, String)>,
        limits: Limits,
    ) -> Result<Self, ReplError> {
        let standard_sources = standard_sources.into_iter().collect::<Vec<_>>();
        let analysis = analyze_with_catalogue(modules, &catalogue);
        if !analysis.is_ok() {
            return Err(semantic_error(analysis.diagnostics));
        }
        limits
            .check_items(analysis.modules.len())
            .map_err(ReplError::runtime)?;
        let runtime_modules = modules
            .iter()
            .map(|module| {
                (
                    module_namespace(&module.logical_path),
                    module.source.clone(),
                )
            })
            .collect();
        let table_names = admitted_table_names(&analysis);
        let runtime =
            admitted_runtime_sources(runtime_modules, standard_sources, table_names, limits)?;
        Ok(Self {
            limits,
            semantic: ReplContext::from_analysis(&analysis).map_err(semantic_error)?,
            runtime,
            attached_databases: None,
        })
    }

    /// Admits one loaded project and verified standard-source set.
    ///
    /// The supplied project is the caller's pinned source set. This boundary
    /// never discovers a host worktree or standard library itself.
    pub fn from_loaded_project(
        project: &LoadedProject,
        standard_sources: impl IntoIterator<Item = (String, String)>,
        limits: Limits,
    ) -> Result<Self, ReplError> {
        let standard_sources = standard_sources.into_iter().collect::<Vec<_>>();
        let catalogue = match project.standard_profile() {
            Some(profile) => {
                if project
                    .standard_modules()
                    .iter()
                    .any(|module| !profile.module_digests().contains_key(module))
                {
                    return Err(ReplError::fixed("ORNA-REPL-STANDARD"));
                }
                Catalogue::authoritative_core()
                    .with_standard_sources(profile, standard_sources.clone())
                    .map_err(|_| ReplError::fixed("ORNA-REPL-STANDARD"))?
            }
            None if project.has_standard_imports() || !standard_sources.is_empty() => {
                return Err(ReplError::fixed("ORNA-REPL-STANDARD"));
            }
            None => Catalogue::authoritative_core(),
        };
        let analysis = analyze_with_catalogue(project.modules(), &catalogue);
        if !analysis.is_ok() {
            return Err(semantic_error(analysis.diagnostics));
        }
        let runtime = admitted_runtime(
            project,
            standard_sources,
            admitted_table_names(&analysis),
            limits,
        )?;
        let semantic = ReplContext::from_analysis(&analysis).map_err(semantic_error)?;
        Ok(Self {
            limits,
            semantic,
            runtime,
            attached_databases: None,
        })
    }

    /// Typechecks and executes one source input, retaining only a complete
    /// paired semantic/runtime success. Declarations yield no value. Every
    /// completed submission updates `$?`: successful submissions publish
    /// `null`; rejected submissions publish their redacted diagnostic.
    pub fn submit(&mut self, source: &str) -> Result<Option<CanonicalValue>, ReplError> {
        let input = match self.parse(source) {
            Ok(input) => input,
            Err(error) => return Err(self.publish_failure(error)),
        };
        let admission = match self.semantic.stage(&input).map_err(semantic_error) {
            Ok(admission) => admission,
            Err(error) => return Err(self.publish_failure(error)),
        };
        if !admission.effects.effects.is_empty() {
            return Err(self.publish_failure(ReplError::fixed("ORNA-REPL-EFFECT")));
        }
        let mut runtime = self.runtime.clone();
        let value = match runtime.submit_admitted(&input).map_err(ReplError::runtime) {
            Ok(value) => value,
            Err(error) => return Err(self.publish_failure(error)),
        };
        let mut semantic = self.semantic.clone();
        if semantic.commit(admission).is_err() {
            return Err(self.publish_failure(ReplError::fixed("ORNA-REPL-COMMIT")));
        }
        runtime.set_last_status(
            CanonicalValue::new(orna_foundation_v1::OvbRaw::Null).expect("null is canonical"),
        );
        self.runtime = runtime;
        self.semantic = semantic;
        Ok(value)
    }

    /// Parses and semantically admits an effectful input without executing or
    /// publishing it.
    ///
    /// The returned AST and effect/type metadata are the handoff consumed by
    /// a transaction-aware evaluator. Pure inputs remain on [`Self::submit`]
    /// and are rejected here so this boundary cannot be mistaken for a
    /// committed activation.
    pub fn stage_activation(&self, source: &str) -> Result<StagedReplActivation, ReplError> {
        let input = self.parse(source)?;
        let admission = self.semantic.stage(&input).map_err(semantic_error)?;
        if admission.effects.effects.is_empty() {
            return Err(ReplError::fixed("ORNA-REPL-EFFECT"));
        }
        let result_type = admission.ty.clone();
        let effects = admission.effects.clone();
        Ok(StagedReplActivation {
            input,
            result_type,
            effects,
            admission,
        })
    }

    /// Evaluates one admitted effectful input into an unpublished session
    /// successor. The caller publishes that successor only after its host
    /// transaction commits.
    pub fn evaluate_staged_with_effects(
        &self,
        staged: StagedReplActivation,
        effects: &mut dyn crate::EffectHandler,
    ) -> Result<(Option<CanonicalValue>, Self), ReplError> {
        let mut successor = self.clone();
        let value = successor
            .runtime
            .submit_admitted_with_effects(staged.input(), effects)
            .map_err(ReplError::runtime)?;
        staged.commit_semantic(&mut successor)?;
        successor.runtime.set_last_status(
            CanonicalValue::new(orna_foundation_v1::OvbRaw::Null).expect("null is canonical"),
        );
        Ok((value, successor))
    }

    /// Executes one already-admitted input with an explicit cancellation
    /// token. The session is updated only after the semantic and evaluator
    /// candidates both complete successfully; cancellation therefore leaves
    /// bindings, $_, and $? unchanged and cannot be recovered by source.
    pub fn submit_admitted_with_cancellation(
        &mut self,
        input: &ReplInput,
        cancellation: &CancellationToken,
    ) -> Result<Option<CanonicalValue>, ReplError> {
        cancellation.check().map_err(ReplError::runtime)?;
        let mut candidate = self.clone();
        let admission = candidate.semantic.stage(input).map_err(semantic_error)?;
        if !admission.effects.effects.is_empty() {
            return Err(ReplError::fixed("ORNA-REPL-EFFECT"));
        }
        let mut runtime = candidate.runtime.clone();
        let value = runtime
            .submit_admitted_with_cancellation(input, cancellation)
            .map_err(ReplError::runtime)?;
        let mut semantic = candidate.semantic.clone();
        if semantic.commit(admission).is_err() {
            return Err(ReplError::fixed("ORNA-REPL-COMMIT"));
        }
        runtime.set_last_status(
            CanonicalValue::new(orna_foundation_v1::OvbRaw::Null).expect("null is canonical"),
        );
        candidate.runtime = runtime;
        candidate.semantic = semantic;
        cancellation.check().map_err(ReplError::runtime)?;
        *self = candidate;
        Ok(value)
    }

    /// Evaluates one expression without publishing any session state.
    pub fn preview(&self, source: &str) -> Result<CanonicalValue, ReplError> {
        let input = self.parse(source)?;
        if !matches!(input, ReplInput::Expression(_)) {
            return Err(ReplError::fixed("ORNA-EVAL-UNSUPPORTED"));
        }
        let admission = self.semantic.stage(&input).map_err(semantic_error)?;
        if !admission.effects.effects.is_empty() {
            return Err(ReplError::fixed("ORNA-REPL-EFFECT"));
        }
        let mut runtime = self.runtime.clone();
        runtime
            .submit_admitted(&input)
            .map_err(ReplError::runtime)?
            .ok_or_else(|| ReplError::fixed("ORNA-EVAL-UNSUPPORTED"))
    }

    fn parse(&self, source: &str) -> Result<ReplInput, ReplError> {
        parse_admitted_repl(source, self.limits).map_err(ReplError::runtime)
    }

    fn publish_failure(&mut self, error: ReplError) -> ReplError {
        self.runtime.set_last_status(error.status_value());
        error
    }
}

fn admitted_runtime(
    project: &LoadedProject,
    standard_sources: Vec<(String, String)>,
    table_names: Vec<String>,
    limits: Limits,
) -> Result<ReplSession, ReplError> {
    let modules = project
        .modules()
        .iter()
        .zip(project.identities())
        .map(|(module, identity)| {
            (
                (!identity.namespace().is_empty()).then(|| identity.namespace().join(".")),
                module.source.clone(),
            )
        })
        .collect::<Vec<_>>();
    admitted_runtime_sources(modules, standard_sources, table_names, limits)
}

fn admitted_runtime_sources(
    mut modules: Vec<(Option<String>, String)>,
    standard_sources: Vec<(String, String)>,
    table_names: Vec<String>,
    limits: Limits,
) -> Result<ReplSession, ReplError> {
    let module_count = modules
        .len()
        .checked_add(standard_sources.len())
        .ok_or_else(|| ReplError::fixed("ORNA-EVAL-LIMIT"))?;
    limits
        .check_items(module_count)
        .map_err(ReplError::runtime)?;
    modules.extend(
        standard_sources
            .into_iter()
            .map(|(logical_path, source)| (module_namespace(&logical_path), source)),
    );
    modules.sort_by(|left, right| left.0.cmp(&right.0));

    let mut functions = Functions::new();
    for (namespace, source) in modules {
        limits.check_source(&source).map_err(ReplError::runtime)?;
        let parsed = parse_module(&source);
        if !parsed.is_ok() {
            return Err(ReplError::fixed("ORNA-EVAL-PARSE"));
        }
        for item in parsed.value.items {
            match item.declaration {
                Declaration::Function { signature, body } => {
                    let name = namespace
                        .as_ref()
                        .map(|namespace| format!("{namespace}.{}", signature.name))
                        .unwrap_or(signature.name);
                    if functions.contains_key(&name) {
                        return Err(ReplError::fixed("ORNA-REPL-PROJECT"));
                    }
                    functions.insert(
                        name,
                        PureFunction {
                            parameters: signature.parameters,
                            body,
                            environment: Environment::new(),
                        },
                    );
                }
                Declaration::Use { .. } => {}
                // The bounded REPL admits pinned standard functions as
                // executable source, while enum constructors are still
                // represented by the semantic catalogue only. Keep an
                // optional std enum module from preventing unrelated std
                // functions from loading; ordinary project enums remain
                // outside this evaluator boundary.
                Declaration::Enum { .. }
                    if namespace
                        .as_deref()
                        .is_some_and(|namespace| namespace.starts_with("std.")) => {}
                _ => return Err(ReplError::fixed("ORNA-REPL-UNSUPPORTED")),
            }
        }
    }
    ReplSession::with_bindings(limits, Environment::new(), functions)
        .map(|session| session.with_table_names(table_names))
        .map_err(ReplError::runtime)
}

fn admitted_table_names(analysis: &Analysis) -> Vec<String> {
    analysis
        .modules
        .values()
        .flat_map(|header| {
            header
                .symbols
                .iter()
                .filter(|(_, symbol)| symbol.kind == SymbolKind::Table)
                .map(|(name, _)| {
                    if header.namespace.0.is_empty() {
                        name.clone()
                    } else {
                        format!("{}.{}", header.namespace.0.join("."), name)
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn module_namespace(logical_path: &str) -> Option<String> {
    let mut components = logical_path.split('/').collect::<Vec<_>>();
    let file = components.pop()?;
    let stem = file.strip_suffix(".orna")?;
    if stem == "main" {
        return (!components.is_empty()).then(|| components.join("."));
    }
    components.push(stem);
    Some(components.join("."))
}

fn semantic_error(diagnostics: Vec<orna_foundation_v1::Diagnostic>) -> ReplError {
    diagnostics.first().map_or_else(
        || ReplError::fixed("ORNA-REPL-SEMANTIC"),
        |diagnostic| ReplError::semantic(diagnostic.code()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_project_v1::{
        AttachedDatabaseSession, PinnedDatabase, ProjectLoader,
    };
    use orna_repository_v1::Repository;
    use orna_semantic_v1::StandardDependencyProfile;
    use orna_value_v1::{Raw, Value};
    use std::{fs, process::Command};
    use tempfile::TempDir;

    fn loaded_project(
        files: &[(&str, &str)],
        profile: Option<StandardDependencyProfile>,
    ) -> (TempDir, LoadedProject) {
        let directory = tempfile::tempdir().unwrap();
        for (path, source) in files {
            let path = directory.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        let repository = Repository::discover(directory.path()).unwrap();
        let project = ProjectLoader::default()
            .load_with_standard_profile(&repository, profile)
            .unwrap();
        (directory, project)
    }

    #[test]
    fn effectful_source_stages_canonical_activation_without_publishing() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let staged = session
            .stage_activation(include_str!(
                "fixtures/repl-inline-std-net-http-get-https-example-com-74c176f7.orna"
            ))
            .expect("effectful source is semantically admitted");

        assert!(matches!(staged.input(), ReplInput::Expression(_)));
        assert!(staged.result_type().is_some());
        assert!(staged.effects().effects.contains("network"));
        assert!(staged.effects().may_fail);

        assert_eq!(
            session
                .submit(include_str!(
                    "fixtures/repl-inline-std-net-http-get-https-example-com-74c176f7.orna"
                ))
                .unwrap_err()
                .code(),
            "ORNA-REPL-EFFECT"
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-40-2-0fcd2493.orna")),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn typed_declarations_execute_without_erasing_annotations() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-let-n-int-21-fe1325a7.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-fn-twice-value-int-int-value-value-596adf3f.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-twice-n-21d0ebc9.orna")),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn semantic_and_runtime_failures_do_not_publish_pending_bindings() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let mismatch = session
            .submit(include_str!(
                "fixtures/repl-inline-let-text-int-wrong-12395f03.orna"
            ))
            .unwrap_err();
        assert_eq!(mismatch.code(), "ORNA-S021-TYPE");
        assert_eq!(
            session
                .submit(include_str!("fixtures/repl-inline-text-982d9e3e.orna"))
                .unwrap_err()
                .code(),
            "ORNA-S012-UNRESOLVED"
        );

        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-40-2-0fcd2493.orna")),
            Ok(Some(Value::int(42.into())))
        );
        let runtime = session
            .submit(include_str!(
                "fixtures/repl-inline-let-pending-int-1-0-9baa0e02.orna"
            ))
            .unwrap_err();
        assert_eq!(runtime.code(), "ORNA-EVAL-DIVIDE-BY-ZERO");
        assert_eq!(
            session
                .submit(include_str!("fixtures/repl-inline-pending-62a2fed3.orna"))
                .unwrap_err()
                .code(),
            "ORNA-S012-UNRESOLVED"
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-source-ba1da4b7.orna")),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn preview_is_semantic_and_runtime_state_isolated() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-let-n-int-41-1debaaa2.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.preview(include_str!("fixtures/repl-inline-n-1-60910b50.orna")),
            Ok(Value::int(42.into()))
        );
        assert_eq!(
            session
                .submit(include_str!("fixtures/repl-inline-source-ba1da4b7.orna"))
                .unwrap_err()
                .code(),
            "ORNA-S012-UNRESOLVED"
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-n-1b16b1df.orna")),
            Ok(Some(Value::int(41.into())))
        );
    }

    #[test]
    fn effectful_sources_are_rejected_before_runtime_execution() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-42-73475cb4.orna")),
            Ok(Some(Value::int(42.into())))
        );

        let error = session
            .submit(include_str!(
                "fixtures/repl-inline-std-net-http-get-https-example-com-74c176f7.orna"
            ))
            .unwrap_err();
        assert_eq!(error.code(), "ORNA-REPL-EFFECT");
        assert_eq!(error.diagnostic().message(), "<redacted>");
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-source-ba1da4b7.orna")),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn generics_fail_explicitly_in_the_bounded_runtime() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session
                .submit(include_str!(
                    "fixtures/repl-inline-fn-identity-t-value-t-t-value-ab04ee2f.orna"
                ))
                .unwrap_err()
                .code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
        assert_eq!(
            session
                .submit(include_str!(
                    "fixtures/repl-inline-identity-1-4a47b267.orna"
                ))
                .unwrap_err()
                .code(),
            "ORNA-S012-UNRESOLVED"
        );
    }

    #[test]
    fn standard_bytes_are_verified_before_runtime_seeding() {
        let standard = include_str!(
            "fixtures/repl-inline-pub-fn-increment-value-int-int-value-1-97766a10.orna"
        );
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot",
            [("std/math.orna".into(), standard.into())],
        )
        .unwrap();
        let (_directory, project) = loaded_project(
            &[(
                "main.orna",
                include_str!(
                    "fixtures/repl-inline-use-std-math-increment-pub-fn-run-valu-83d2a2a0.orna"
                ),
            )],
            Some(profile),
        );
        assert_eq!(
            AdmittedReplSession::from_loaded_project(
                &project,
                [(
                    "std/math.orna".into(),
                    include_str!(
                        "fixtures/repl-inline-pub-fn-increment-value-int-int-value-2-724ecb19.orna"
                    )
                    .into(),
                )],
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-REPL-STANDARD"
        );

        let mut session = AdmittedReplSession::from_loaded_project(
            &project,
            [("std/math.orna".into(), standard.into())],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-std-math-increment-477a9dd9.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-increment-41-fa3331ac.orna"
            )),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn admitted_standard_functions_execute_their_verified_source_bodies() {
        let profile = crate::reference_standard_profile();
        let (_directory, project) = loaded_project(
            &[(
                "main.orna",
                include_str!("fixtures/repl-inline-empty-module.orna"),
            )],
            Some(profile),
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            &project,
            crate::reference_standard_sources(),
            Limits::default(),
        )
        .unwrap();

        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-std-math-6b6c1741.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-math-clamp-value-99-lower-20-upper-22-5a6b11dc.orna"
            )),
            Ok(Some(Value::int(22.into())))
        );
        assert_eq!(
            session
                .submit(include_str!(
                    "fixtures/repl-inline-math-clamp-value-99-min-20-max-22-9984937d.orna"
                ))
                .unwrap_err()
                .code(),
            "ORNA-S021-TYPE"
        );
    }

    #[test]
    fn pinned_collection_and_query_exports_use_the_shared_intrinsic_bindings() {
        let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-std-query-collection-2165.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-std-collection-2165.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-std-query-alias-2165.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-std-query-map-2165.orna")),
            Ok(Some(
                Value::new(Raw::Array(vec![
                    Raw::Int(2.into()),
                    Raw::Int(4.into()),
                    Raw::Int(6.into()),
                ]))
                .unwrap(),
            ))
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-std-query-alias-map-2165.orna"
            )),
            Ok(Some(
                Value::new(Raw::Array(vec![
                    Raw::Int(2.into()),
                    Raw::Int(3.into()),
                    Raw::Int(4.into()),
                ]))
                .unwrap(),
            ))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-std-query-filter-2165.orna")),
            Ok(Some(
                Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())])).unwrap(),
            ))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-std-collection-one-2165.orna")),
            Ok(Some(Value::int(4.into())))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-std-query-every-2165.orna")),
            Ok(Some(Value::new(Raw::Bool(true)).unwrap()))
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-std-collection-first-2165.orna"
            )),
            Ok(Some(Value::option(Some(Value::int(4.into()))).unwrap()))
        );
    }

    #[test]
    fn unprofiled_qualified_collection_fallbacks_are_rejected_without_state_change() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-let-answer-int-40-32ff602e.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-answer-2-1e6074ea.orna")),
            Ok(Some(Value::int(42.into())))
        );

        for source in [
            include_str!("fixtures/repl-inline-std-collection-first-1-2-3-07a8406e.orna"),
            include_str!("fixtures/repl-inline-std-collection-map-1-2-3-value-value-401a69f4.orna"),
        ] {
            assert_eq!(
                session.submit(source).unwrap_err().code(),
                "ORNA-S012-UNRESOLVED",
                "{source}"
            );
        }

        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-answer-0db52f40.orna")),
            Ok(Some(Value::int(40.into())))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-source-ba1da4b7.orna")),
            Ok(Some(Value::int(40.into())))
        );
    }

    #[test]
    fn repl_can_import_verified_standard_modules_absent_from_project_source() {
        let standard = include_str!(
            "fixtures/repl-inline-pub-fn-increment-value-int-int-value-1-97766a10.orna"
        );
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot",
            [("std/math.orna".into(), standard.into())],
        )
        .unwrap();
        let (_directory, project) = loaded_project(
            &[
                (
                    "main.orna",
                    include_str!(
                        "fixtures/repl-inline-use-library-pub-fn-run-int-library-loc-22e03d25.orna"
                    ),
                ),
                (
                    "library.orna",
                    include_str!(
                        "fixtures/repl-inline-pub-fn-local-value-int-int-value-1-be07d8e1.orna"
                    ),
                ),
            ],
            Some(profile),
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            &project,
            [("std/math.orna".into(), standard.into())],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-std-math-6b6c1741.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-library-a84bcc62.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-math-increment-library-local-40-c176360e.orna"
            )),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn loaded_project_snapshot_pairs_visibility_and_executable_bodies() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            directory.path().join("main.orna"),
            include_str!("fixtures/repl-inline-use-library-a84bcc62.orna"),
        )
        .unwrap();
        let library = directory.path().join("library.orna");
        fs::write(
            &library,
            include_str!("fixtures/repl-inline-pub-fn-value-int-42-fn-hidden-int-99-3655e003.orna"),
        )
        .unwrap();
        let repository = Repository::discover(directory.path()).unwrap();
        let loaded = ProjectLoader::default().load(&repository).unwrap();

        fs::write(
            &library,
            include_str!(
                "fixtures/repl-inline-pub-fn-value-str-changed-pub-fn-hidden-4a870d7d.orna"
            ),
        )
        .unwrap();
        let mut session = AdmittedReplSession::from_loaded_project(
            &loaded,
            std::iter::empty::<(String, String)>(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-library-a84bcc62.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-library-value-43f7e801.orna"
            )),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(
            session
                .submit(include_str!(
                    "fixtures/repl-inline-library-hidden-eddb18cc.orna"
                ))
                .unwrap_err()
                .code(),
            "ORNA-S012-UNRESOLVED"
        );
    }

    #[test]
    fn loaded_directory_main_uses_the_loader_namespace() {
        let (_directory, project) = loaded_project(
            &[
                (
                    "main.orna",
                    include_str!("fixtures/repl-inline-use-sensors-greenhouse-7575c118.orna"),
                ),
                (
                    "sensors/greenhouse/main.orna",
                    include_str!("fixtures/repl-inline-pub-fn-seeded-int-40-d5abd82d.orna"),
                ),
            ],
            None,
        );
        assert_eq!(
            project
                .identities()
                .iter()
                .find(|identity| identity.logical_path() == "sensors/greenhouse/main.orna")
                .unwrap()
                .namespace(),
            ["sensors", "greenhouse"]
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            &project,
            std::iter::empty::<(String, String)>(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-sensors-greenhouse-7575c118.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-greenhouse-seeded-2-8fad7abb.orna"
            )),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn typed_submission_preserves_prior_state_after_rejection() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-let-answer-int-40-32ff602e.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-answer-2-1e6074ea.orna")),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(
            session
                .submit(include_str!(
                    "fixtures/repl-inline-let-answer-int-wrong-13d92e60.orna"
                ))
                .unwrap_err()
                .code(),
            "ORNA-S021-TYPE"
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-answer-0db52f40.orna")),
            Ok(Some(Value::int(40.into())))
        );
    }

    #[test]
    fn rejected_inputs_retain_only_a_redacted_structured_diagnostic() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error = session
            .submit(include_str!(
                "fixtures/repl-inline-let-secret-name-int-private-178755d6.orna"
            ))
            .unwrap_err();

        assert_eq!(error.code(), "ORNA-S021-TYPE");
        assert_eq!(error.diagnostic().message(), "<redacted>");
        let encoded = error.diagnostic().encode_ovb().unwrap();
        assert!(
            !encoded
                .windows(b"secret_name".len())
                .any(|window| window == b"secret_name")
        );
    }

    #[test]
    fn last_status_tracks_redacted_failures_and_successes_without_preview_mutation() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session.preview(include_str!("fixtures/repl-inline-source-3a7494a2.orna")),
            Ok(CanonicalValue::new(orna_foundation_v1::OvbRaw::Null).unwrap())
        );
        let error = session
            .submit(include_str!(
                "fixtures/repl-inline-let-secret-name-int-private-178755d6.orna"
            ))
            .unwrap_err();
        assert_eq!(error.code(), "ORNA-S021-TYPE");
        let status = session
            .preview(include_str!("fixtures/repl-inline-source-3a7494a2.orna"))
            .expect("status value");
        assert_eq!(
            status.raw(),
            &orna_foundation_v1::OvbRaw::Map(vec![
                (
                    orna_foundation_v1::OvbRaw::Text("code".into()),
                    orna_foundation_v1::OvbRaw::Text("ORNA-S021-TYPE".into()),
                ),
                (
                    orna_foundation_v1::OvbRaw::Text("message".into()),
                    orna_foundation_v1::OvbRaw::Text("<redacted>".into()),
                ),
                (
                    orna_foundation_v1::OvbRaw::Text("redacted".into()),
                    orna_foundation_v1::OvbRaw::Bool(true),
                ),
                (
                    orna_foundation_v1::OvbRaw::Text("severity".into()),
                    orna_foundation_v1::OvbRaw::Text("error".into()),
                ),
            ])
        );

        assert_eq!(
            session.preview(include_str!("fixtures/repl-inline-1-1-72fce594.orna")),
            Ok(Value::int(2.into()))
        );
        let status = session
            .preview(include_str!("fixtures/repl-inline-source-3a7494a2.orna"))
            .expect("status value");
        assert_eq!(
            status.raw(),
            &orna_foundation_v1::OvbRaw::Map(vec![
                (
                    orna_foundation_v1::OvbRaw::Text("code".into()),
                    orna_foundation_v1::OvbRaw::Text("ORNA-S021-TYPE".into()),
                ),
                (
                    orna_foundation_v1::OvbRaw::Text("message".into()),
                    orna_foundation_v1::OvbRaw::Text("<redacted>".into()),
                ),
                (
                    orna_foundation_v1::OvbRaw::Text("redacted".into()),
                    orna_foundation_v1::OvbRaw::Bool(true),
                ),
                (
                    orna_foundation_v1::OvbRaw::Text("severity".into()),
                    orna_foundation_v1::OvbRaw::Text("error".into()),
                ),
            ])
        );

        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-40-2-0fcd2493.orna")),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(
            session.preview(include_str!("fixtures/repl-inline-source-3a7494a2.orna")),
            Ok(CanonicalValue::new(orna_foundation_v1::OvbRaw::Null).unwrap())
        );
    }

    #[test]
    fn cancellation_in_long_loop_rolls_back_candidate_and_status() {
        let mut session = ReplSession::new(Limits::default());
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-let-answer-41-c31c6eb5.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-answer-0db52f40.orna")),
            Ok(Some(Value::int(41.into())))
        );
        let before_status = session
            .preview(include_str!("fixtures/repl-inline-source-3a7494a2.orna"))
            .unwrap();
        let input = parse_admitted_repl(
            include_str!(
                "fixtures/repl-inline-if-true-for-value-in-1-100000-value-0-cb81a3a8.orna"
            ),
            Limits::default(),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.request_after_checks(8);

        let error = session
            .submit_admitted_with_cancellation(&input, &cancellation)
            .unwrap_err();
        assert_eq!(error.code(), "ORNA-EVAL-CANCELLED");
        assert_eq!(
            session.preview(include_str!("fixtures/repl-inline-answer-0db52f40.orna")),
            Ok(Value::int(41.into()))
        );
        assert_eq!(
            session.preview(include_str!("fixtures/repl-inline-source-3a7494a2.orna")),
            Ok(before_status)
        );
    }

    #[test]
    fn cancellation_in_recursive_calls_is_not_recoverable_or_published() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-fn-recurse-n-if-n-0-0-else-recurse-n-1-5cd2a50e.orna"
            )),
            Ok(None)
        );
        let input = parse_admitted_repl(
            include_str!("fixtures/repl-inline-recurse-100000-6ed6c372.orna"),
            Limits::default(),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.request_after_checks(12);

        let error = session
            .submit_admitted_with_cancellation(&input, &cancellation)
            .unwrap_err();
        assert_eq!(error.code(), "ORNA-EVAL-CANCELLED");
        assert_eq!(
            session.submit(include_str!("fixtures/repl-inline-recurse-0-d5e629d3.orna")),
            Ok(Some(Value::int(0.into())))
        );
    }

    #[test]
    fn module_source_cannot_admit_the_session_local_status_binding() {
        let (_directory, project) = loaded_project(
            &[(
                "main.orna",
                include_str!("fixtures/repl-inline-pub-fn-status-988936de.orna"),
            )],
            None,
        );
        assert_eq!(
            AdmittedReplSession::from_loaded_project(&project, [], Limits::default())
                .unwrap_err()
                .code(),
            "ORNA-S012-UNRESOLVED"
        );
    }

    #[test]
    fn loaded_project_is_pinned_before_qualified_execution() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("main.orna"),
            include_str!("fixtures/repl-inline-use-library-f2cd609f.orna"),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("library.orna"),
            include_str!("fixtures/repl-inline-pub-fn-seeded-int-40-df4e01cd.orna"),
        )
        .unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        let repository = Repository::discover(directory.path()).unwrap();
        let project = ProjectLoader::default().load(&repository).unwrap();
        let mut session =
            AdmittedReplSession::from_loaded_project(&project, [], Limits::default()).unwrap();
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-use-library-a84bcc62.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-library-seeded-2-7f27f8c8.orna"
            )),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn pinned_attachments_are_available_to_typed_repl_sessions() {
        fn git(directory: &std::path::Path, args: &[&str]) -> String {
            let output = Command::new("git")
                .args(args)
                .current_dir(directory)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        fn pinned_repo(source: &str) -> (TempDir, Repository, String) {
            let directory = tempfile::tempdir().unwrap();
            fs::write(directory.path().join("main.orna"), source).unwrap();
            git(directory.path(), &["init", "--quiet"]);
            git(directory.path(), &["config", "user.name", "kierandrewett"]);
            git(
                directory.path(),
                &["config", "user.email", "kieran@drewett.dev"],
            );
            git(directory.path(), &["config", "commit.gpgsign", "false"]);
            git(directory.path(), &["add", "main.orna"]);
            git(directory.path(), &["commit", "--quiet", "-m", "snapshot"]);
            let commit = git(directory.path(), &["rev-parse", "HEAD"]);
            let repository = Repository::discover(directory.path()).unwrap();
            (directory, repository, commit)
        }

        let (_primary_dir, primary_repository, primary_commit) = pinned_repo(include_str!(
            "fixtures/attached-primary-main.orna"
        ));
        let (_package_dir, package_repository, package_commit) = pinned_repo(include_str!(
            "fixtures/attached-package-main.orna"
        ));
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &primary_commit,
            loader,
        )
        .unwrap();
        let package = PinnedDatabase::resolve(
            "widgets",
            package_repository.clone(),
            &package_commit,
            loader,
        )
        .unwrap();
        let mut databases = AttachedDatabaseSession::new(primary.clone()).unwrap();
        databases.attach_database(package).unwrap();

        let mut session = AdmittedReplSession::from_attached_database_session(
            &databases,
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            session.submit("use widgets;"),
            Ok(None)
        );
        assert_eq!(
            session.submit("widgets.answer()"),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(
            session
                .attached_databases()
                .unwrap()
                .database("widgets")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_commit
        );

        let std_package = PinnedDatabase::resolve(
            "std",
            package_repository,
            &package_commit,
            loader,
        )
        .unwrap();
        let mut std_databases = AttachedDatabaseSession::new(primary).unwrap();
        std_databases.attach_database(std_package).unwrap();
        let mut std_session = AdmittedReplSession::from_attached_database_session(
            &std_databases,
            Limits::default(),
        )
        .unwrap();
        assert_eq!(std_session.submit("use std;"), Ok(None));
        assert_eq!(
            std_session.submit("std.answer()"),
            Ok(Some(Value::int(42.into())))
        );
    }
}
