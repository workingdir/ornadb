//! Typed, project-pinned admission for the bounded pure REPL.
//!
//! This is an execution-facing boundary: a caller supplies one already loaded
//! immutable project and optional verified standard sources, then receives an
//! isolated session which parses source once, stages semantic state, rejects
//! effects before evaluation, and publishes semantic/runtime successors
//! together. Ordinary pure submission has no external effects; the explicit
//! host-binding path admits only capabilities installed by the caller.

use std::collections::{BTreeMap, BTreeSet};

use orna_foundation_v1::{
    CanonicalValue, Diagnostic as FoundationDiagnostic, DiagnosticSeverity, SafeText,
};
use orna_project_v1::{AttachedDatabaseSession, LoadedProject};
use orna_semantic_v1::StandardDependencyProfile;
use orna_semantic_v1::{
    Analysis, Catalogue, EffectSummary, ModuleInput, ReplAdmission, ReplContext, SymbolKind, Type,
    SystemProviders, analyze_with_providers, unresolved_member, unresolved_name,
};
use orna_syntax_v1::{
    Declaration, ImportSegment, ReplInput, SyntaxDiagnostic, TokenKind, TypeMember,
    TypeRepresentation, UseTail, Visibility, lex, parse_module, parse_repl,
};
use sha2::{Digest as _, Sha256};

use crate::{
    CancellationToken, Environment, EvaluationError, Functions, Limits, NominalDefinition,
    NominalDefinitions, NominalField, PureFunction, ReplSession, module_function_alias_key,
    source_enum_definitions,
};
use crate::{reference_standard_profile, reference_standard_sources};

/// Redacted failure from the admitted REPL boundary.
///
/// [`Self::diagnostic`] never contains submitted source. [`Self::detail`]
/// carries parser or semantic specifics for the local operator and may quote
/// the submitted input, so it must not cross a protocol boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplError {
    diagnostic: Box<FoundationDiagnostic>,
    detail: Option<Box<ReplDetail>>,
}

/// Explanation of a rejected REPL input, located in its source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplDetail {
    /// Parser or semantic code, for example `ORNA-PARSE-001`.
    pub code: String,
    /// What the user did wrong, naming the offending identifier when known.
    pub message: String,
    /// Byte range of the offending text in the submitted source, when located.
    pub span: Option<std::ops::Range<usize>>,
    /// Supporting facts about the failure.
    pub notes: Vec<String>,
    /// Concrete remedies the user can apply.
    pub help: Vec<String>,
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
            detail: None,
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

    /// Returns the located explanation of a parse or static-name failure, when
    /// one is available. Runtime and host failures carry none.
    pub fn detail(&self) -> Option<&ReplDetail> {
        self.detail.as_deref()
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
    repository_scope: Option<RepositoryScope>,
}

/// Database and repository identity that one REPL session is authority-scoped
/// to. The identity is supplied by the executable host at session creation and
/// is never inferred from source or from a global registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryScope {
    database_id: [u8; 16],
    repository_id: [u8; 16],
}

impl RepositoryScope {
    /// Binds a session to one database and repository identity.
    #[must_use]
    pub const fn new(database_id: [u8; 16], repository_id: [u8; 16]) -> Self {
        Self {
            database_id,
            repository_id,
        }
    }

    /// Returns the database identity this scope is bound to.
    #[must_use]
    pub const fn database_id(&self) -> [u8; 16] {
        self.database_id
    }

    /// Returns the repository identity this scope is bound to.
    #[must_use]
    pub const fn repository_id(&self) -> [u8; 16] {
        self.repository_id
    }
}

impl AdmittedReplSession {
    /// Binds this session to the database and repository the executable host
    /// selected. Sessions created without a scope stay unscoped.
    #[must_use]
    pub fn with_repository_scope(mut self, scope: RepositoryScope) -> Self {
        self.repository_scope = Some(scope);
        self
    }

    /// Attaches the activation facts `sys.rt.info()` reports to this session's
    /// pure submissions. A session without a context reports
    /// `sys.version.incompatible` rather than inventing host facts.
    #[cfg(feature = "system-host-bindings")]
    #[must_use]
    pub fn with_sys_host_context(mut self, context: crate::SysHostContext) -> Self {
        self.runtime = self.runtime.with_sys_host_context(context);
        self
    }

    /// Returns the repository scope this session was bound to, if any.
    #[must_use]
    pub const fn repository_scope(&self) -> Option<RepositoryScope> {
        self.repository_scope
    }

    /// Starts a core-only typed REPL with no project bindings.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            semantic: ReplContext::with_providers(SystemProviders::Production),
            runtime: ReplSession::new(limits),
            attached_databases: None,
            repository_scope: None,
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
        let analysis = analyze_with_providers(modules, &catalogue, SystemProviders::Production);
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
            repository_scope: None,
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
        let analysis = analyze_with_providers(project.modules(), &catalogue, SystemProviders::Production);
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
            repository_scope: None,
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
        let admission = match self.stage_input(&input, source) {
            Ok(admission) => admission,
            Err(error) => return Err(self.publish_failure(error)),
        };
        if admission.effects.is_effectful() {
            return Err(self.publish_failure(effect_rejection(&admission.effects)));
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

    /// Executes a checked input with explicitly installed native sys providers.
    /// Environment names, process executables and roots, and clock waits remain
    /// bounded by the capabilities supplied in `bindings`. Missing providers
    /// fail closed; this path does not grant database effects.
    pub fn submit_with_sys_host_bindings(
        &mut self,
        source: &str,
        bindings: &mut crate::SysHostBindingRegistry,
    ) -> Result<Option<CanonicalValue>, ReplError> {
        let input = match self.parse(source) {
            Ok(input) => input,
            Err(error) => return Err(self.publish_failure(error)),
        };
        let admission = match self.stage_input(&input, source) {
            Ok(admission) => admission,
            Err(error) => return Err(self.publish_failure(error)),
        };
        let mut runtime = self.runtime.clone();
        let value = match runtime
            .submit_admitted_with_effects(&input, bindings)
            .map_err(ReplError::runtime)
        {
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
        let admission = self.stage_input(&input, source)?;
        if !admission.effects.is_effectful() {
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
        // This admitted-input path has no source text, so diagnostics carry no spans.
        let admission = candidate.stage_input(input, "")?;
        if admission.effects.is_effectful() {
            return Err(effect_rejection(&admission.effects));
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
        let admission = self.stage_input(&input, source)?;
        if admission.effects.is_effectful() {
            return Err(effect_rejection(&admission.effects));
        }
        let mut runtime = self.runtime.clone();
        runtime
            .submit_admitted(&input)
            .map_err(ReplError::runtime)?
            .ok_or_else(|| ReplError::fixed("ORNA-EVAL-UNSUPPORTED"))
    }

    fn parse(&self, source: &str) -> Result<ReplInput, ReplError> {
        self.limits
            .check_source(source)
            .map_err(ReplError::runtime)?;
        let parsed = parse_repl(source);
        if parsed.is_ok() {
            return Ok(parsed.value);
        }
        Err(parse_error(&parsed.diagnostics, source))
    }

    fn stage_input(&self, input: &ReplInput, source: &str) -> Result<ReplAdmission, ReplError> {
        self.semantic
            .stage(input)
            .map_err(|diagnostics| stage_error(&self.semantic, diagnostics, source))
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

    let mut parsed_modules = Vec::with_capacity(modules.len());
    for (namespace, source) in modules {
        limits.check_source(&source).map_err(ReplError::runtime)?;
        let parsed = parse_module(&source);
        if !parsed.is_ok() {
            return Err(ReplError::fixed("ORNA-EVAL-PARSE"));
        }
        parsed_modules.push((namespace, parsed.value));
    }

    let mut function_names = BTreeSet::new();
    for (namespace, module) in &parsed_modules {
        let Some(namespace) = namespace else {
            continue;
        };
        for item in &module.items {
            if let Declaration::Function { signature, .. } = &item.declaration
                && matches!(item.visibility, Visibility::Public { .. })
            {
                function_names.insert(format!("{namespace}.{}", signature.name));
            }
        }
    }

    let has_main_module = parsed_modules
        .iter()
        .any(|(namespace, _)| namespace.as_deref() == Some("main"));
    let mut module_aliases = BTreeMap::new();
    if !has_main_module {
        for (namespace, module) in &parsed_modules {
            if namespace.is_some() {
                continue;
            }
            for item in &module.items {
                if let Declaration::Function { signature, .. } = &item.declaration
                    && matches!(item.visibility, Visibility::Public { .. })
                {
                    // The root module executes as `main`, so its public functions
                    // resolve through the same qualified key a committed run uses.
                    let qualified = format!("main.{}", signature.name);
                    module_aliases.insert(qualified.clone(), qualified);
                }
            }
        }
    }
    for (namespace, module) in &parsed_modules {
        let Some(namespace) = namespace else {
            continue;
        };
        for item in &module.items {
            if let Declaration::Use { path, tail } = &item.declaration {
                add_module_import_aliases(
                    &mut module_aliases,
                    namespace,
                    path,
                    tail,
                    &function_names,
                )?;
            }
        }
    }

    let mut functions = Functions::new();
    let mut nominal_definitions = NominalDefinitions::new();
    for (namespace, module) in parsed_modules {
        let standard = namespace
            .as_deref()
            .is_some_and(|namespace| namespace == "std" || namespace.starts_with("std."));
        if !standard {
            // ORNA-ENUM-001..003: project enums execute as construction plans
            // shared by every evaluation in the session. The root module is
            // admitted under `main`, the namespace the committed run derives for
            // `main.orna`, so REPL enum values carry the run's identities.
            let enum_namespace = namespace.as_deref().unwrap_or("main");
            for (name, definition) in source_enum_definitions(Some(enum_namespace), &module.items) {
                insert_repl_nominal(&mut nominal_definitions, name, definition)?;
            }
        }
        for item in module.items {
            match item.declaration {
                Declaration::Function { signature, body } => {
                    // With no source `main` module the root executes as `main`,
                    // so its functions are keyed `main.<name>`. A source `main`
                    // module keeps the root functions under their bare names, and
                    // the root alias yields to it.
                    let name = match namespace.as_deref() {
                        Some(namespace) => format!("{namespace}.{}", signature.name),
                        None if has_main_module => signature.name,
                        None => format!("main.{}", signature.name),
                    };
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
                Declaration::Use { .. } | Declaration::Table { .. } | Declaration::Enum { .. } => {}
                // Catalogue-only declarations have no runtime representation. The
                // REPL admits them only in the root project; elsewhere they stay
                // unsupported below.
                Declaration::Protocol { .. }
                | Declaration::Dimension { .. }
                | Declaration::Unit { .. }
                | Declaration::Type {
                    representation: TypeRepresentation::Alias { .. },
                    ..
                } if namespace.is_none() => {}
                // Project-root nominal types are ephemeral REPL definitions. They
                // resolve only through the `main.` spelling, like root enums, so a
                // bare name never reaches them.
                Declaration::Type {
                    name,
                    representation: TypeRepresentation::Nominal { members },
                    ..
                } if namespace.is_none() => {
                    let definition = repl_nominal_definition(&name, members)?;
                    insert_repl_nominal(
                        &mut nominal_definitions,
                        format!("main.{name}"),
                        definition,
                    )?;
                }
                // The bounded REPL admits pinned standard functions as
                // executable source, while declaration-only standard items
                // remain represented by the semantic catalogue. Keep these
                // items from preventing unrelated standard functions from
                // loading; other project declarations remain outside this
                // evaluator boundary. This arm also admits standard Protocol,
                // Dimension, Unit and alias declarations, which have no runtime
                // representation. Non-standard namespaced declarations of those
                // kinds do not match it and fall through to the unsupported arm.
                _ if standard => {}
                _ => return Err(ReplError::fixed("ORNA-REPL-UNSUPPORTED")),
            }
        }
    }
    let session = ReplSession::with_bindings(limits, Environment::new(), functions)
        .map_err(ReplError::runtime)?
        .with_module_aliases(module_aliases)
        .map(|session| session.with_table_names(table_names))
        .map_err(ReplError::runtime)?;
    session
        .with_nominal_definitions(nominal_definitions)
        .map_err(ReplError::runtime)
}

/// Derives a session-local nominal identity from source spellings. It is
/// deterministic for one admitted project load and is not a persisted OVB
/// identity.
fn repl_nominal_id(parts: &[&str]) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"orna.repl.nominal\0");
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

fn repl_nominal_definition(
    name: &str,
    members: Vec<TypeMember>,
) -> Result<NominalDefinition, ReplError> {
    let mut fields = Vec::with_capacity(members.len());
    for member in members {
        let TypeMember::Field {
            visibility,
            name: field,
            initializer,
            ..
        } = member
        else {
            // Assertions and conversions carry checks this boundary does not
            // evaluate; admitting the type without them would drop the checks.
            return Err(ReplError::fixed("ORNA-REPL-UNSUPPORTED"));
        };
        let id = repl_nominal_id(&[name, "field", field.as_str()]);
        fields.push(match (visibility, initializer) {
            (true, Some(default)) => NominalField::public_with_default(id, field, default),
            (true, None) => NominalField::public(id, field),
            (false, Some(default)) => NominalField::private_with_default(id, field, default),
            (false, None) => NominalField::private(id, field),
        });
    }
    Ok(NominalDefinition::new(
        repl_nominal_id(&[name, "type"]),
        None,
        fields,
    ))
}

fn insert_repl_nominal(
    nominals: &mut NominalDefinitions,
    name: String,
    definition: NominalDefinition,
) -> Result<(), ReplError> {
    if nominals.contains_key(&name) {
        return Err(ReplError::fixed("ORNA-REPL-PROJECT"));
    }
    nominals.insert(name, definition);
    Ok(())
}

fn add_module_import_aliases(
    aliases: &mut BTreeMap<String, String>,
    namespace: &str,
    path: &[ImportSegment],
    tail: &UseTail,
    function_names: &BTreeSet<String>,
) -> Result<(), ReplError> {
    let imported_path = path
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join(".");
    match tail {
        UseTail::Names(names) => {
            for name in names {
                register_module_import_alias(
                    aliases,
                    namespace,
                    &name.name,
                    &format!("{imported_path}.{}", name.name),
                );
            }
        }
        UseTail::Glob { .. } => {
            add_imported_module_functions(
                aliases,
                namespace,
                &imported_path,
                "",
                true,
                function_names,
            )?;
        }
        UseTail::Alias { name, .. } if name == "_" => {
            add_imported_module_functions(
                aliases,
                namespace,
                &imported_path,
                "",
                true,
                function_names,
            )?;
        }
        UseTail::Alias { name, .. } => {
            add_import_or_namespace_alias(
                aliases,
                namespace,
                &imported_path,
                name,
                function_names,
            )?;
        }
        UseTail::None => {
            let name = path
                .last()
                .map(|segment| segment.name.as_str())
                .unwrap_or_default();
            add_import_or_namespace_alias(
                aliases,
                namespace,
                &imported_path,
                name,
                function_names,
            )?;
        }
    }
    Ok(())
}

fn add_import_or_namespace_alias(
    aliases: &mut BTreeMap<String, String>,
    namespace: &str,
    imported_path: &str,
    local_name: &str,
    function_names: &BTreeSet<String>,
) -> Result<(), ReplError> {
    if function_names.contains(imported_path) {
        register_module_import_alias(aliases, namespace, local_name, imported_path);
        Ok(())
    } else {
        add_imported_module_functions(
            aliases,
            namespace,
            imported_path,
            local_name,
            false,
            function_names,
        )
    }
}

fn add_imported_module_functions(
    aliases: &mut BTreeMap<String, String>,
    namespace: &str,
    imported_path: &str,
    local_prefix: &str,
    direct_children_only: bool,
    function_names: &BTreeSet<String>,
) -> Result<(), ReplError> {
    let prefix = format!("{imported_path}.");
    for target in function_names {
        let Some(suffix) = target.strip_prefix(&prefix) else {
            continue;
        };
        if suffix.is_empty() || (direct_children_only && suffix.contains('.')) {
            continue;
        }
        let local_name = if local_prefix.is_empty() {
            suffix.to_owned()
        } else {
            format!("{local_prefix}.{suffix}")
        };
        register_module_import_alias(aliases, namespace, &local_name, target);
    }
    Ok(())
}

fn register_module_import_alias(
    aliases: &mut BTreeMap<String, String>,
    namespace: &str,
    local_name: &str,
    target: &str,
) {
    let key = module_function_alias_key(namespace, local_name);
    // An unqualified name imported twice from different modules is the same
    // ambiguity the semantic layer records as `DIAG_AMBIGUOUS` and resolves by
    // keeping the first binding. Qualified calls never consult this map, so a
    // program that imports `value` from two graph branches still loads and
    // executes through its module-qualified spelling.
    aliases.entry(key).or_insert_with(|| target.to_owned());
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

/// The rejection for input that performs effects in a session that never
/// runs them. Names the effects, so the person can see what made it effectful.
pub fn effect_rejection(effects: &EffectSummary) -> ReplError {
    let mut names: Vec<&str> = effects.effects.iter().map(|kind| kind.as_str()).collect();
    if effects.opaque {
        names.push("opaque");
    }
    let names = names.join(", ");
    let mut error = ReplError::redacted("ORNA-REPL-EFFECT");
    error.detail = Some(Box::new(ReplDetail {
        code: "ORNA-REPL-EFFECT".to_owned(),
        message: format!("this input performs effects: {names}"),
        span: None,
        notes: vec!["the core REPL session never runs effects".to_owned()],
        help: vec![
            "evaluate a pure expression here".to_owned(),
            "or run the input in a project repository REPL, where effects are committed"
                .to_owned(),
        ],
    }));
    error
}

/// The span of the submitted input without its surrounding whitespace. Used
/// when analysis cannot narrow a diagnostic below the whole input.
fn whole_input_span(source: &str) -> Option<std::ops::Range<usize>> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = source.len() - source.trim_start().len();
    Some(start..start + trimmed.len())
}

fn parse_error(diagnostics: &[SyntaxDiagnostic], source: &str) -> ReplError {
    let mut error = ReplError::redacted("ORNA-EVAL-PARSE");
    if let Some(diagnostic) = diagnostics.first() {
        // End-of-input tokens sit after the trailing newline the terminal
        // adds; point just past the last written character instead.
        let content_end = source.trim_end().len();
        let start = diagnostic.span.start.min(content_end);
        let end = diagnostic.span.end.min(content_end).max(start);
        let mut help = diagnostic.help.clone();
        if help.is_empty()
            && let Some(remedy) = parse_remedy(&diagnostic.message)
        {
            help.push(remedy.to_owned());
        }
        let mut notes = diagnostic.notes.clone();
        if diagnostic.message.starts_with("expected `)`")
            && let Some(opener) = unclosed_opener(source)
        {
            let (line, column) = line_column(source, opener.start);
            notes.push(format!(
                "the `(` at line {line}, column {column} is not closed"
            ));
        }
        error.detail = Some(Box::new(ReplDetail {
            code: diagnostic.code.to_owned(),
            message: diagnostic.message.clone(),
            span: Some(start..end),
            notes,
            help,
        }));
    }
    error
}

/// The remedy for a parser message the syntax crate leaves without `help`.
fn parse_remedy(message: &str) -> Option<&'static str> {
    if message == "expected namespace path" {
        Some("name the module to import after `use`, for example `use library;`")
    } else if message == "expected pattern" {
        Some("name the binding after `let`, for example `let x = 3;`")
    } else if message.starts_with("expected `)`") {
        Some("close the parenthesis with `)`")
    } else {
        None
    }
}

/// The innermost `(` that is still open at the end of `source`.
fn unclosed_opener(source: &str) -> Option<std::ops::Range<usize>> {
    let mut open = Vec::new();
    for token in lex(source).ok()? {
        match token.kind {
            TokenKind::Punct("(") => open.push(token.span.start..token.span.end),
            TokenKind::Punct(")") => {
                open.pop();
            }
            _ => {}
        }
    }
    open.pop()
}

/// One-based line and column of byte `offset` in `source`.
fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    (line, before[line_start..].chars().count() + 1)
}

fn stage_error(
    context: &ReplContext,
    diagnostics: Vec<orna_foundation_v1::Diagnostic>,
    source: &str,
) -> ReplError {
    let Some(first) = diagnostics.first() else {
        return ReplError::fixed("ORNA-REPL-SEMANTIC");
    };
    let code = first.code();
    let detail = match (unresolved_name(first), unresolved_member(first)) {
        (Some(name), _) => unresolved_detail(context, code, name, source),
        (None, Some((module, member))) => unresolved_member_detail(code, module, member, source),
        (None, None) => ReplDetail {
            code: code.to_owned(),
            message: first.message().to_owned(),
            span: whole_input_span(source),
            notes: first
                .notes()
                .iter()
                .map(|note| note.as_str().to_owned())
                .collect(),
            help: Vec::new(),
        },
    };
    let mut error = ReplError::redacted(code);
    error.detail = Some(Box::new(detail));
    error
}

fn unresolved_detail(context: &ReplContext, code: &str, name: &str, source: &str) -> ReplDetail {
    let hints = context.name_hints(name);
    let mut help: Vec<String> = hints
        .similar
        .iter()
        .map(|candidate| format!("did you mean `{candidate}`?"))
        .collect();
    help.extend(
        hints
            .modules
            .iter()
            .map(|module| format!("add `use {module}.{name}` to bring `{name}` into scope")),
    );
    if hints.namespaces.is_empty() {
        help.push(format!(
            "declare `{name}` before using it, for example `let {name} = <value>`"
        ));
    }
    help.extend(
        hints
            .namespaces
            .iter()
            .map(|namespace| format!("add `use {namespace};` to bring module `{name}` into scope")),
    );
    ReplDetail {
        code: code.to_owned(),
        message: format!("name `{name}` cannot be resolved"),
        span: locate_identifier(source, name, false),
        notes: vec![format!(
            "`{name}` is not declared in this session and is not imported"
        )],
        help,
    }
}

fn unresolved_member_detail(code: &str, module: &str, member: &str, source: &str) -> ReplDetail {
    ReplDetail {
        code: code.to_owned(),
        message: format!("`{member}` is not a member of module `{module}`"),
        span: locate_identifier(source, member, true),
        notes: Vec::new(),
        help: vec![
            format!("check the spelling of `{member}` against the exports of `{module}`"),
            format!("if `{member}` is private in `{module}`, mark it `pub` to use it here"),
        ],
    }
}

/// Locates the first identifier `name` in `source` as a byte span. With
/// `qualified`, only an occurrence directly after `.` (a member access) counts.
fn locate_identifier(source: &str, name: &str, qualified: bool) -> Option<std::ops::Range<usize>> {
    lex(source).ok()?.into_iter().find_map(|token| match &token.kind {
        TokenKind::Identifier { normalized } if normalized == name || token.text == name => {
            let start = token.span.start;
            (!qualified || source[..start].trim_end().ends_with('.')).then(|| start..token.span.end)
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_admitted_repl;
    use orna_project_v1::{AttachedDatabaseSession, PinnedDatabase, ProjectLoader};
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
    fn root_main_runtime_alias_yields_to_a_source_main_module() {
        let mut root = admitted_runtime_sources(
            vec![(None, "pub fn add(value: Int) = value + 10;".into())],
            Vec::new(),
            Vec::new(),
            Limits::default(),
        )
        .expect("root functions load");
        assert_eq!(root.submit("main.add(1)"), Ok(Some(Value::int(11.into()))));

        let mut with_main = admitted_runtime_sources(
            vec![
                (None, "pub fn add(value: Int) = value + 10;".into()),
                (Some("main".into()), "pub fn add(value: Int) = value + 1;".into()),
            ],
            Vec::new(),
            Vec::new(),
            Limits::default(),
        )
        .expect("root and user main functions load");
        assert!(with_main.submit("main.add(1)").is_err());
        with_main.submit("use main;").expect("user main module imports");
        assert_eq!(
            with_main.submit("main.add(1)"),
            Ok(Some(Value::int(2.into())))
        );
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
        assert!(staged.effects().effects.contains(&orna_semantic_v1::EffectKind::Network));
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
            session.submit(include_str!(
                "fixtures/repl-inline-std-query-filter-2165.orna"
            )),
            Ok(Some(
                Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())])).unwrap(),
            ))
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-std-collection-one-2165.orna"
            )),
            Ok(Some(Value::int(4.into())))
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/repl-inline-std-query-every-2165.orna"
            )),
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
    fn parse_errors_point_past_the_last_character_with_a_remedy() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error = session.submit("use\n").unwrap_err();

        assert_eq!(error.code(), "ORNA-EVAL-PARSE");
        let detail = error.detail.as_ref().expect("parse errors carry a located detail");
        assert_eq!(detail.code, "ORNA-PARSE-001");
        assert_eq!(detail.message, "expected namespace path");
        assert_eq!(detail.span, Some(3..3));
        assert!(
            detail
                .help
                .iter()
                .any(|help| help.contains("`use library;`")),
            "{:?}",
            detail.help
        );
    }

    #[test]
    fn unclosed_parenthesis_names_its_opener_and_the_missing_close() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error = session.submit("(1 + 2\n").unwrap_err();

        let detail = error.detail.as_ref().expect("parse errors carry a located detail");
        assert_eq!(detail.message, "expected `)`");
        assert_eq!(detail.span, Some(6..6));
        assert_eq!(
            detail.notes,
            vec!["the `(` at line 1, column 1 is not closed".to_owned()]
        );
        assert!(
            detail
                .help
                .iter()
                .any(|help| help == "close the parenthesis with `)`"),
            "{:?}",
            detail.help
        );
    }

    #[test]
    fn unresolved_name_points_at_the_identifier_with_a_declare_remedy() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error = session.submit("nofn(1)\n").unwrap_err();

        assert_eq!(error.code(), "ORNA-S012-UNRESOLVED");
        let detail = error.detail.as_ref().expect("unresolved names carry a located detail");
        assert_eq!(detail.message, "name `nofn` cannot be resolved");
        assert_eq!(detail.span, Some(0..4));
        assert!(
            detail
                .help
                .iter()
                .any(|help| help.contains("declare `nofn` before using it")),
            "{:?}",
            detail.help
        );
    }

    #[test]
    fn loaded_module_path_resolves_after_use_and_reports_the_missing_member() {
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
            None,
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            &project,
            Vec::<(String, String)>::new(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(session.submit("use library;\n"), Ok(None));
        assert_eq!(session.submit("library.local(40)\n"), Ok(Some(Value::int(41.into()))));

        let error = session.submit("library.twice(\"x\")\n").unwrap_err();
        let detail = error.detail.as_ref().expect("unresolved members carry a located detail");
        assert_eq!(detail.message, "`twice` is not a member of module `library`");
        assert_eq!(detail.span, Some(8..13));
        assert!(
            detail
                .help
                .iter()
                .any(|help| help == "check the spelling of `twice` against the exports of `library`"),
            "{:?}",
            detail.help
        );
    }

    #[test]
    fn binding_without_a_name_points_at_the_offending_token() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error = session.submit("let = 3;").unwrap_err();

        let detail = error.detail.as_ref().expect("parse errors carry a located detail");
        assert_eq!(detail.message, "expected pattern");
        assert_eq!(detail.span, Some(4..5));
        assert!(
            detail
                .help
                .iter()
                .any(|help| help.contains("`let x = 3;`")),
            "{:?}",
            detail.help
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

        let (_primary_dir, primary_repository, primary_commit) =
            pinned_repo(include_str!("fixtures/attached-primary-main.orna"));
        let (_package_dir, package_repository, package_commit) =
            pinned_repo(include_str!("fixtures/attached-package-main.orna"));
        let loader = ProjectLoader::default();
        let primary =
            PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader).unwrap();
        let package = PinnedDatabase::resolve(
            "widgets",
            package_repository.clone(),
            &package_commit,
            loader,
        )
        .unwrap();
        let mut databases = AttachedDatabaseSession::new(primary.clone()).unwrap();
        databases.attach_database(package).unwrap();

        let mut session =
            AdmittedReplSession::from_attached_database_session(&databases, Limits::default())
                .unwrap();
        assert_eq!(session.submit("use widgets;"), Ok(None));
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

        let std_package =
            PinnedDatabase::resolve("std", package_repository, &package_commit, loader).unwrap();
        let mut std_databases = AttachedDatabaseSession::new(primary).unwrap();
        std_databases.attach_database(std_package).unwrap();
        let mut std_session =
            AdmittedReplSession::from_attached_database_session(&std_databases, Limits::default())
                .unwrap();
        assert_eq!(std_session.submit("use std;"), Ok(None));
        assert_eq!(
            std_session.submit("std.answer()"),
            Ok(Some(Value::int(42.into())))
        );
    }

    const ROOT_TYPES: &str = "pub enum Availability { ready, busy, }\npub type Queue { pub size: Int, }\n";

    fn root_types_session() -> (TempDir, AdmittedReplSession) {
        let (directory, project) = loaded_project(&[("main.orna", ROOT_TYPES)], None);
        let session = AdmittedReplSession::from_loaded_project(
            &project,
            std::iter::empty::<(String, String)>(),
            Limits::default(),
        )
        .unwrap();
        (directory, session)
    }

    #[test]
    fn root_types_resolve_only_through_the_main_spelling() {
        let (_directory, mut session) = root_types_session();
        assert!(session.submit("Queue { size: 1 }").is_err());
        assert!(session.submit("Availability.ready").is_err());
        assert!(matches!(
            session.submit("main.Queue { size: 1 }"),
            Ok(Some(value)) if matches!(value.raw(), Raw::Tag(60009, _))
        ));
        assert!(matches!(
            session.submit("main.Availability.ready"),
            Ok(Some(value)) if matches!(value.raw(), Raw::Tag(60008, _))
        ));
    }

    #[test]
    fn local_binding_named_like_a_root_type_does_not_capture_the_qualified_spelling() {
        // Shadowing rule: a session binding shadows only the bare spelling of its
        // own name. Root types are reached through `main.`, so `let Availability`
        // never captures `main.Availability.ready`.
        let (_directory, mut session) = root_types_session();
        assert_eq!(session.submit("let Availability = 3;"), Ok(None));
        assert_eq!(
            session.submit("Availability"),
            Ok(Some(Value::int(3.into())))
        );
        assert!(matches!(
            session.submit("main.Availability.ready"),
            Ok(Some(value)) if matches!(value.raw(), Raw::Tag(60008, _))
        ));
    }

    #[test]
    fn repl_root_enum_identity_matches_the_committed_main_namespace() {
        let parsed = orna_syntax_v1::parse_module(ROOT_TYPES);
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let committed = crate::source_enums::source_enum_definitions(
            Some("main"),
            &parsed.value.items,
        );
        let definition = &committed["main.Availability"];
        let ready = definition
            .variants
            .iter()
            .find(|variant| variant.name == "ready")
            .unwrap();
        let (_directory, mut session) = root_types_session();
        let value = session.submit("main.Availability.ready").unwrap().unwrap();
        assert_eq!(
            value.raw(),
            &Raw::Tag(
                60008,
                Box::new(Raw::Array(vec![
                    definition.type_id.clone(),
                    ready.variant_id.clone(),
                    Raw::Null,
                ])),
            )
        );
    }

    #[test]
    fn namespaced_protocols_are_rejected_before_repl_admission() {
        let (_directory, project) = loaded_project(
            &[
                ("main.orna", "use sensors.greenhouse;\npub fn answer() = 1;\n"),
                ("sensors/greenhouse/main.orna", "pub protocol Sink {}\n"),
            ],
            None,
        );
        let Err(error) = AdmittedReplSession::from_loaded_project(
            &project,
            std::iter::empty::<(String, String)>(),
            Limits::default(),
        ) else {
            panic!("a namespaced protocol must not be admitted into the REPL");
        };
        assert_eq!(error.code(), "ORNA-REPL-UNSUPPORTED");
    }

    #[test]
    fn namespaced_dimensions_units_and_aliases_are_rejected_before_repl_admission() {
        let cases = [
            ("dim", "pub dim Length;\n"),
            ("unit", "pub dim Length;\npub unit meter: Length base;\n"),
            ("alias", "pub type Label = Str;\n"),
        ];
        for (kind, source) in cases {
            let (_directory, project) = loaded_project(
                &[
                    ("main.orna", "use sensors.greenhouse;\npub fn answer() = 1;\n"),
                    ("sensors/greenhouse/main.orna", source),
                ],
                None,
            );
            let Err(error) = AdmittedReplSession::from_loaded_project(
                &project,
                std::iter::empty::<(String, String)>(),
                Limits::default(),
            ) else {
                panic!("a namespaced {kind} must not be admitted into the REPL");
            };
            assert_eq!(error.code(), "ORNA-REPL-UNSUPPORTED", "{kind}");
        }
    }
}

#[cfg(test)]
mod repository_scope_tests {
    use super::*;

    #[test]
    fn repl_session_carries_the_repository_scope_it_was_bound_to() {
        let scope = RepositoryScope::new([1; 16], [2; 16]);
        let session = AdmittedReplSession::new(Limits::default());
        assert_eq!(session.repository_scope(), None);

        let scoped = session.with_repository_scope(scope);
        assert_eq!(scoped.repository_scope(), Some(scope));
        assert_eq!(scope.database_id(), [1; 16]);
        assert_eq!(scope.repository_id(), [2; 16]);
    }
}
