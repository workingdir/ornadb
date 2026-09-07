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
use orna_project_v1::LoadedProject;
use orna_semantic_v1::{Catalogue, ReplContext, analyze_with_catalogue};
use orna_syntax_v1::{Declaration, ReplInput, parse_module};

use crate::{
    Environment, EvaluationError, Functions, Limits, PureFunction, ReplSession, parse_admitted_repl,
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
}

impl AdmittedReplSession {
    /// Starts a core-only typed REPL with no project bindings.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            semantic: ReplContext::empty(),
            runtime: ReplSession::new(limits),
        }
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
        let runtime = admitted_runtime(project, standard_sources, limits)?;
        let semantic = ReplContext::from_analysis(&analysis).map_err(semantic_error)?;
        Ok(Self {
            limits,
            semantic,
            runtime,
        })
    }

    /// Typechecks and executes one source input, retaining only a complete
    /// paired semantic/runtime success. Declarations yield no value.
    pub fn submit(&mut self, source: &str) -> Result<Option<CanonicalValue>, ReplError> {
        let input = self.parse(source)?;
        let admission = self.semantic.stage(&input).map_err(semantic_error)?;
        if !admission.effects.effects.is_empty() {
            return Err(ReplError::fixed("ORNA-REPL-EFFECT"));
        }
        let mut runtime = self.runtime.clone();
        let value = runtime
            .submit_admitted(&input)
            .map_err(ReplError::runtime)?;
        let mut semantic = self.semantic.clone();
        semantic
            .commit(admission)
            .map_err(|_| ReplError::fixed("ORNA-REPL-COMMIT"))?;
        self.runtime = runtime;
        self.semantic = semantic;
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
}

fn admitted_runtime(
    project: &LoadedProject,
    standard_sources: Vec<(String, String)>,
    limits: Limits,
) -> Result<ReplSession, ReplError> {
    let module_count = project
        .modules()
        .len()
        .checked_add(standard_sources.len())
        .ok_or_else(|| ReplError::fixed("ORNA-EVAL-LIMIT"))?;
    limits
        .check_items(module_count)
        .map_err(ReplError::runtime)?;
    let mut modules = project
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
                _ => return Err(ReplError::fixed("ORNA-REPL-UNSUPPORTED")),
            }
        }
    }
    ReplSession::with_bindings(limits, Environment::new(), functions).map_err(ReplError::runtime)
}

fn module_namespace(logical_path: &str) -> Option<String> {
    let mut components = logical_path.split('/').collect::<Vec<_>>();
    let file = components.pop()?;
    let stem = file.strip_suffix(".orna")?;
    if components.is_empty() && stem == "main" {
        return None;
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
    use orna_project_v1::ProjectLoader;
    use orna_repository_v1::Repository;
    use orna_semantic_v1::StandardDependencyProfile;
    use orna_value_v1::Value;
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
    fn typed_declarations_execute_without_erasing_annotations() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(session.submit("let n: Int = 21;"), Ok(None));
        assert_eq!(
            session.submit("fn twice(value: Int): Int = value + value;"),
            Ok(None)
        );
        assert_eq!(session.submit("twice(n)"), Ok(Some(Value::int(42.into()))));
    }

    #[test]
    fn semantic_and_runtime_failures_do_not_publish_pending_bindings() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let mismatch = session.submit("let text: Int = \"wrong\";").unwrap_err();
        assert_eq!(mismatch.code(), "ORNA-S021-TYPE");
        assert_eq!(
            session.submit("text").unwrap_err().code(),
            "ORNA-S012-UNRESOLVED"
        );

        assert_eq!(session.submit("40 + 2"), Ok(Some(Value::int(42.into()))));
        let runtime = session.submit("let pending: Int = 1 / 0;").unwrap_err();
        assert_eq!(runtime.code(), "ORNA-EVAL-DIVIDE-BY-ZERO");
        assert_eq!(
            session.submit("pending").unwrap_err().code(),
            "ORNA-S012-UNRESOLVED"
        );
        assert_eq!(session.submit("$_"), Ok(Some(Value::int(42.into()))));
    }

    #[test]
    fn preview_is_semantic_and_runtime_state_isolated() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(session.submit("let n: Int = 41;"), Ok(None));
        assert_eq!(session.preview("n + 1"), Ok(Value::int(42.into())));
        assert_eq!(
            session.submit("$_").unwrap_err().code(),
            "ORNA-S012-UNRESOLVED"
        );
        assert_eq!(session.submit("n"), Ok(Some(Value::int(41.into()))));
    }

    #[test]
    fn effectful_sources_are_rejected_before_runtime_execution() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(session.submit("42"), Ok(Some(Value::int(42.into()))));

        let error = session
            .submit("std.net.http.get(\"https://example.com\")")
            .unwrap_err();
        assert_eq!(error.code(), "ORNA-REPL-EFFECT");
        assert_eq!(error.diagnostic().message(), "<redacted>");
        assert_eq!(session.submit("$_"), Ok(Some(Value::int(42.into()))));
    }

    #[test]
    fn generics_fail_explicitly_in_the_bounded_runtime() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            session
                .submit("fn identity<T>(value: T): T = value;")
                .unwrap_err()
                .code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
        assert_eq!(
            session.submit("identity(1)").unwrap_err().code(),
            "ORNA-S012-UNRESOLVED"
        );
    }

    #[test]
    fn standard_bytes_are_verified_before_runtime_seeding() {
        let standard = "pub fn increment(value: Int): Int = value + 1;";
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot",
            [("std/math.orna".into(), standard.into())],
        )
        .unwrap();
        let (_directory, project) = loaded_project(
            &[(
                "main.orna",
                "use std.math.{increment}; pub fn run(value: Int): Int = increment(value);",
            )],
            Some(profile),
        );
        assert_eq!(
            AdmittedReplSession::from_loaded_project(
                &project,
                [(
                    "std/math.orna".into(),
                    "pub fn increment(value: Int): Int = value + 2;".into(),
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
        assert_eq!(session.submit("use std.math.{increment};"), Ok(None));
        assert_eq!(
            session.submit("increment(41)"),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn admitted_standard_functions_execute_their_verified_source_bodies() {
        let standard = "pub fn clamp(value: Int, lower: Int, upper: Int): Int = lower + upper;";
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot",
            [("std/math.orna".into(), standard.into())],
        )
        .unwrap();
        let (_directory, project) = loaded_project(&[("main.orna", "")], Some(profile));
        let mut session = AdmittedReplSession::from_loaded_project(
            &project,
            [("std/math.orna".into(), standard.into())],
            Limits::default(),
        )
        .unwrap();

        assert_eq!(session.submit("use std.math;"), Ok(None));
        assert_eq!(
            session.submit("math.clamp(value: 99, lower: 20, upper: 22)"),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(
            session
                .submit("math.clamp(value: 99, min: 20, max: 22)")
                .unwrap_err()
                .code(),
            "ORNA-S021-TYPE"
        );
    }

    #[test]
    fn repl_can_import_verified_standard_modules_absent_from_project_source() {
        let standard = "pub fn increment(value: Int): Int = value + 1;";
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot",
            [("std/math.orna".into(), standard.into())],
        )
        .unwrap();
        let (_directory, project) = loaded_project(
            &[
                (
                    "main.orna",
                    "use library; pub fn run(): Int = library.local(40);",
                ),
                ("library.orna", "pub fn local(value: Int): Int = value + 1;"),
            ],
            Some(profile),
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            &project,
            [("std/math.orna".into(), standard.into())],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(session.submit("use std.math;"), Ok(None));
        assert_eq!(session.submit("use library;"), Ok(None));
        assert_eq!(
            session.submit("math.increment(library.local(40))"),
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
        fs::write(directory.path().join("main.orna"), "use library;").unwrap();
        let library = directory.path().join("library.orna");
        fs::write(&library, "pub fn value(): Int = 42; fn hidden(): Int = 99;").unwrap();
        let repository = Repository::discover(directory.path()).unwrap();
        let loaded = ProjectLoader::default().load(&repository).unwrap();

        fs::write(
            &library,
            "pub fn value(): Str = \"changed\"; pub fn hidden(): Int = 99;",
        )
        .unwrap();
        let mut session = AdmittedReplSession::from_loaded_project(
            &loaded,
            std::iter::empty::<(String, String)>(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(session.submit("use library;"), Ok(None));
        assert_eq!(
            session.submit("library.value()"),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(
            session.submit("library.hidden()").unwrap_err().code(),
            "ORNA-S012-UNRESOLVED"
        );
    }

    #[test]
    fn loaded_directory_main_uses_the_loader_namespace() {
        let (_directory, project) = loaded_project(
            &[
                ("main.orna", "use sensors.greenhouse;"),
                ("sensors/greenhouse/main.orna", "pub fn seeded(): Int = 40;"),
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
        assert_eq!(session.submit("use sensors.greenhouse;"), Ok(None));
        assert_eq!(
            session.submit("greenhouse.seeded() + 2"),
            Ok(Some(Value::int(42.into())))
        );
    }

    #[test]
    fn typed_submission_preserves_prior_state_after_rejection() {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(session.submit("let answer: Int = 40;"), Ok(None));
        assert_eq!(
            session.submit("answer + 2"),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(
            session
                .submit("let answer: Int = \"wrong\";")
                .unwrap_err()
                .code(),
            "ORNA-S021-TYPE"
        );
        assert_eq!(session.submit("answer"), Ok(Some(Value::int(40.into()))));
    }

    #[test]
    fn rejected_inputs_retain_only_a_redacted_structured_diagnostic() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error = session
            .submit("let secret_name: Int = \"private\";")
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
    fn loaded_project_is_pinned_before_qualified_execution() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("main.orna"), "use library;\n").unwrap();
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn seeded(): Int = 40;\n",
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
        assert_eq!(session.submit("use library;"), Ok(None));
        assert_eq!(
            session.submit("library.seeded() + 2"),
            Ok(Some(Value::int(42.into())))
        );
    }
}
