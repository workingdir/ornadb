//! Typed REPL admission paired with the bounded evaluator namespace.
//!
//! This is deliberately an integration seam, not a second parser or semantic
//! model. Source is parsed once by the evaluator's bounded parser entrypoint,
//! staged as that exact AST, then executed as that same AST.

use crate::{
    BoundedEvaluator, ProjectEnvironment, ProjectExpectations, ProjectUnit, RuntimeEvaluator,
    SourceUnit, StageOutcome,
};
use orna_evaluator_v1::{EvaluationError, Limits, ReplSession, parse_admitted_repl};
use orna_foundation_v1::CanonicalValue;
use orna_project_v1::LoadedProject;
use orna_semantic_v1::{Catalogue, ReplContext, analyze_with_catalogue};
use orna_syntax_v1::ReplInput;

/// Redacted REPL admission failure. Only a stable code crosses this API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplError {
    code: String,
}

impl ReplError {
    fn runtime(error: EvaluationError) -> Self {
        Self {
            code: error.code().into(),
        }
    }

    fn semantic(code: &str) -> Self {
        Self { code: code.into() }
    }

    fn fixed(code: &'static str) -> Self {
        Self { code: code.into() }
    }

    /// Stable machine-readable failure code.
    pub fn code(&self) -> &str {
        &self.code
    }
}

/// A transactional typed REPL session.
///
/// Semantic and runtime successors are both staged before either is published.
/// An execution failure therefore retains the prior semantic bindings, runtime
/// bindings, and `$_` result together.
#[derive(Clone, Debug)]
pub struct AdmittedReplSession {
    limits: Limits,
    semantic: ReplContext,
    runtime: ReplSession,
}

impl AdmittedReplSession {
    /// Starts a typed REPL against the authoritative core semantic catalogue.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            semantic: ReplContext::empty(),
            runtime: ReplSession::new(limits),
        }
    }

    /// Builds the typed session from one immutable loaded-project source set.
    ///
    /// The semantic catalogue and executable standard namespace are both
    /// built from the same supplied standard bytes after pinned-profile
    /// verification. No externally paired `Analysis` or runtime namespace is
    /// accepted at this boundary.
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

        let runtime_project = runtime_project(project, standard_sources);
        let mut evaluator = BoundedEvaluator::new(limits);
        match evaluator.evaluate_project(&runtime_project) {
            StageOutcome::Passed => {}
            StageOutcome::Failed(diagnostic) => {
                return Err(ReplError::semantic(diagnostic.code()));
            }
            StageOutcome::Skipped { .. } => {
                return Err(ReplError::fixed("ORNA-REPL-UNSUPPORTED"));
            }
        }
        let semantic = ReplContext::from_analysis(&analysis).map_err(semantic_error)?;
        let runtime = evaluator.repl_session().map_err(ReplError::runtime)?;
        Ok(Self {
            limits,
            semantic,
            runtime,
        })
    }

    /// Typechecks then executes a single source input. Declarations return
    /// `None`; expressions retain their successful result as `$_`.
    pub fn submit(&mut self, source: &str) -> Result<Option<CanonicalValue>, ReplError> {
        let input = self.parse(source)?;
        let admission = self.semantic.stage(&input).map_err(semantic_error)?;
        if !admission.effects.effects.is_empty() {
            return Err(ReplError::fixed("ORNA-REPL-EFFECT"));
        }

        // Both commits are fallible/staged. Publish only the fully successful
        // pair, so a runtime error cannot leak a semantic declaration.
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

    /// Typechecks and evaluates an expression without changing either session
    /// state. Effects are rejected before the runtime preview boundary.
    pub fn preview(&self, source: &str) -> Result<CanonicalValue, ReplError> {
        let input = self.parse(source)?;
        let ReplInput::Expression(_) = input else {
            return Err(ReplError::fixed("ORNA-EVAL-UNSUPPORTED"));
        };
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

fn runtime_project(
    project: &LoadedProject,
    standard_sources: Vec<(String, String)>,
) -> ProjectUnit {
    let mut modules = project
        .modules()
        .iter()
        .zip(project.identities())
        .map(|(module, identity)| SourceUnit {
            fixture_id: "admitted-repl-project".into(),
            source_id: identity.logical_path().into(),
            parse_as: "module_unit".into(),
            source: module.source.clone(),
        })
        .collect::<Vec<_>>();
    modules.extend(
        standard_sources
            .into_iter()
            .map(|(source_id, source)| SourceUnit {
                fixture_id: "admitted-repl-standard".into(),
                source_id,
                parse_as: "module_unit".into(),
                source,
            }),
    );
    ProjectUnit {
        fixture_id: "admitted-repl-project".into(),
        project_id: "admitted-repl-project".into(),
        environment_id: None,
        modules,
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
        },
    }
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
                    "pub fn increment(value: Int): Int = value + 2;".into()
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
}
