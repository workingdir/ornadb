//! Semantic and runtime conformance seams for the frozen v1 contracts.
//!
//! `orna-semantic-v1` is a real, deterministic analyzer, but intentionally
//! exposes one combined analysis result rather than separate compiler phases.
//! This adapter executes it and only reports a phase result where that result
//! is meaningful. `RuntimeEvaluator` remains an explicit seam: the bounded
//! evaluator below handles pure expression units, while the durable runtime
//! still owns module execution, effects, and scenario lifecycles.

use crate::{ConformanceAdapter, ProjectUnit, Scenario, SourceUnit, StageOutcome, SyntaxAdapter};
use orna_evaluator_v1::{Environment, Limits as EvaluatorLimits, evaluate_expression};
use orna_foundation_v1::Diagnostic;
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use std::collections::BTreeMap;

pub struct SemanticAdapter {
    syntax: SyntaxAdapter,
    catalogue: Catalogue,
}

impl Default for SemanticAdapter {
    fn default() -> Self {
        Self {
            syntax: SyntaxAdapter,
            catalogue: Catalogue::authoritative_core(),
        }
    }
}

impl SemanticAdapter {
    fn analyze_units(
        &self,
        units: impl IntoIterator<Item = SourceUnit>,
    ) -> StageOutcome<Diagnostic> {
        let inputs = units
            .into_iter()
            .map(|unit| ModuleInput::new(unit.source_id, unit.source))
            .collect::<Vec<_>>();
        let analysis = analyze_with_catalogue(&inputs, &self.catalogue);
        match analysis.diagnostics.into_iter().next() {
            Some(diagnostic) => StageOutcome::Failed(diagnostic.redacted()),
            None => StageOutcome::Passed,
        }
    }
    fn unsupported_runtime() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "orna-runtime-v1 supplies durable runtime state only; it has no source evaluator or scenario invocation contract".into(),
        }
    }
}

impl ConformanceAdapter for SemanticAdapter {
    type Diagnostic = Diagnostic;
    fn diagnostic_code(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.code().into()
    }
    fn diagnostic_message(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.message().into()
    }
    fn parse(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.syntax.parse(unit)
    }
    fn resolve(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units([unit.clone()])
    }
    fn typecheck(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units([unit.clone()])
    }
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
    }
    fn parse_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        let mut syntax = SyntaxAdapter;
        for unit in &project.modules {
            if let outcome @ StageOutcome::Failed(_) = syntax.parse(unit) {
                return outcome;
            }
        }
        StageOutcome::Passed
    }
    fn resolve_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units(project.modules.clone())
    }
    fn typecheck_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units(project.modules.clone())
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
    }
    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
    }
}

/// The executable runtime boundary.  Implementors receive only logical units
/// and typed project/scenario contracts, never host paths or corpus roots.
pub trait RuntimeEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic>;
    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic>;
    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic>;
    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic>;
}

/// Runs the bounded, side-effect-free evaluator for row/expression units.
///
/// Module execution, table access, mutations, external effects, and prose
/// scenarios remain explicit skips until their owning runtime contracts exist.
#[derive(Default)]
pub struct BoundedEvaluator {
    environment: Environment,
    limits: EvaluatorLimits,
}

impl BoundedEvaluator {
    #[must_use]
    pub fn new(limits: EvaluatorLimits) -> Self {
        Self {
            environment: BTreeMap::new(),
            limits,
        }
    }

    #[must_use]
    pub fn with_environment(limits: EvaluatorLimits, environment: Environment) -> Self {
        Self {
            environment,
            limits,
        }
    }

    fn evaluate_unit(&self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        match unit.parse_as.as_str() {
            "row_unit" | "expression_unit" | "repl_unit" => {
                match evaluate_expression(&unit.source, &self.environment, self.limits) {
                    Ok(_) => StageOutcome::Passed,
                    Err(error) => StageOutcome::Failed(error.diagnostic().clone()),
                }
            }
            _ => StageOutcome::Skipped {
                reason: "module execution requires the integrated Orna runtime".into(),
            },
        }
    }
}

impl RuntimeEvaluator for BoundedEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.evaluate_unit(unit)
    }

    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.evaluate_unit(unit)
    }

    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        if project.loose_rows.is_empty() {
            return StageOutcome::Skipped {
                reason: "project has no executable loose row units".into(),
            };
        }
        for row in &project.loose_rows {
            if let outcome @ StageOutcome::Failed(_) = self.evaluate_unit(row) {
                return outcome;
            }
        }
        StageOutcome::Passed
    }

    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "scenario requires the integrated Orna runtime and durable test fixture".into(),
        }
    }
}

pub struct RuntimeAdapter<R> {
    semantic: SemanticAdapter,
    runtime: R,
}
impl<R> RuntimeAdapter<R> {
    pub fn new(runtime: R) -> Self {
        Self {
            semantic: SemanticAdapter::default(),
            runtime,
        }
    }
    pub fn into_runtime(self) -> R {
        self.runtime
    }
}
impl<R: RuntimeEvaluator> ConformanceAdapter for RuntimeAdapter<R> {
    type Diagnostic = Diagnostic;
    fn diagnostic_code(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.code().into()
    }
    fn diagnostic_message(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.message().into()
    }
    fn parse(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.semantic.parse(unit)
    }
    fn resolve(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.semantic.resolve(unit)
    }
    fn typecheck(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.semantic.typecheck(unit)
    }
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.runtime.evaluate(unit)
    }
    fn parse_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.semantic.parse_project(project)
    }
    fn resolve_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.semantic.resolve_project(project)
    }
    fn typecheck_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.semantic.typecheck_project(project)
    }
    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.runtime.validate_row(unit)
    }
    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.runtime.validate_rows(project)
    }
    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        self.runtime.run_scenario(scenario)
    }
}
