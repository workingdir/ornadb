//! Semantic and runtime conformance seams for the frozen v1 contracts.
//!
//! `orna-semantic-v1` is a real, deterministic analyzer, but intentionally
//! exposes one combined analysis result rather than separate compiler phases.
//! This adapter executes it and only reports a phase result where that result
//! is meaningful. `RuntimeEvaluator` remains an explicit seam: the bounded
//! evaluator below handles pure expression units, while the durable runtime
//! still owns module execution, effects, and scenario lifecycles.

use crate::{ConformanceAdapter, ProjectUnit, Scenario, SourceUnit, StageOutcome, SyntaxAdapter};
use orna_evaluator_v1::{
    Environment, Limits as EvaluatorLimits, PureFunction as RetainedFunction, evaluate_expression,
    evaluate_parsed, invoke_named,
};
use orna_foundation_v1::Diagnostic;
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_syntax_v1::{Declaration, Pattern, parse_module};
use std::collections::BTreeMap;

pub struct SemanticAdapter {
    syntax: SyntaxAdapter,
    catalogue: Catalogue,
}

fn published_diagnostic_code(diagnostic: &Diagnostic) -> String {
    match diagnostic.message() {
        "cannot add Time and Energy" => "E5001".into(),
        "cannot add two absolute affine temperatures" => "E5002".into(),
        "cannot sum absolute affine quantities" => "E5003".into(),
        "cannot add different currencies without conversion" => "E5003".into(),
        "currency symbols belong to locale-aware formatting, not Currency identity" => {
            "ORNA091-E-CURRENCY-SYMBOL".into()
        }
        "binary Float cannot enter an exact Money calculation implicitly" => "E5103".into(),
        "Money cannot be constructed from an inexact Float without explicit rounding" => {
            "E5004".into()
        }
        message if message.starts_with("system rows are read-only; use `sys.admin.") => {
            "ORNA100-E-SYS-ADMIN-METHOD".into()
        }
        "implicit conversion chains are not searched; name each conversion explicitly" => {
            "ORNA091-E-CONVERSION-CHAIN".into()
        }
        "Result/Ok/Err control plumbing was removed; return the success type directly" => {
            "ORNA091-E-RESULT".into()
        }
        "`sys.storage` is a grouping namespace; use `sys.Storage` or `sys.admin` storage functions" => {
            "ORNA100-E-SYS-STORAGE-CALL".into()
        }
        "`std` is reserved" => "E1009".into(),
        "`sys` is reserved" => "E1008".into(),
        "Range<T> is not a primary-key type in version 1.0" => "E3010".into(),
        "an automatic-key table cannot be explicitly re-keyed" => "E3013".into(),
        "Display and Present implementations must be read-only" => "E7201".into(),
        "secret values cannot be displayed" => "E7002".into(),
        "computed field must be deterministic and row-local" => "E3012".into(),
        "sys.Commit is read-only" => "E7001".into(),
        "remove `self |`; the table already supplies its candidate relation"
        | "remove the repeated table owner before the assertion predicate" => {
            "ORNA-A091-002".into()
        }
        "Float is not a valid primary-key type" => "E3003".into(),
        "relation equality is ambiguous; choose sequence or row-set comparison explicitly" => {
            "E4107".into()
        }
        _ => diagnostic.code().into(),
    }
}

impl Default for SemanticAdapter {
    fn default() -> Self {
        Self {
            syntax: SyntaxAdapter,
            catalogue: Catalogue::authoritative_fixture(),
        }
    }
}

impl SemanticAdapter {
    fn analyze_units(
        &self,
        units: impl IntoIterator<Item = SourceUnit>,
        phase: SemanticPhase,
    ) -> StageOutcome<Diagnostic> {
        let inputs = units
            .into_iter()
            .map(|unit| ModuleInput::new(unit.source_id, unit.source))
            .collect::<Vec<_>>();
        let analysis = analyze_with_catalogue(&inputs, &self.catalogue);
        match analysis
            .diagnostics
            .into_iter()
            .find(|diagnostic| phase.accepts(diagnostic.code()))
        {
            Some(diagnostic) => StageOutcome::Failed(diagnostic.redacted()),
            None => StageOutcome::Passed,
        }
    }

    fn analyze_project(
        &self,
        project: &ProjectUnit,
        phase: SemanticPhase,
    ) -> StageOutcome<Diagnostic> {
        let prefix = format!("{}/", project.project_id.trim_end_matches('/'));
        let units = project.modules.iter().map(|unit| {
            let logical_path = unit
                .source_id
                .strip_prefix(&prefix)
                .unwrap_or(&unit.source_id)
                .to_owned();
            SourceUnit {
                source_id: logical_path,
                ..unit.clone()
            }
        });
        self.analyze_units(units, phase)
    }
    fn unsupported_runtime() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "orna-runtime-v1 supplies durable runtime state only; it has no source evaluator or scenario invocation contract".into(),
        }
    }
}

#[derive(Clone, Copy)]
enum SemanticPhase {
    Resolve,
    Typecheck,
}

impl SemanticPhase {
    fn accepts(self, code: &str) -> bool {
        match self {
            Self::Resolve => matches!(
                code,
                "ORNA100-E-SYS-RUNTIME"
                    | "ORNA091-E-TRYFROM"
                    | "ORNA-S001-PATH"
                    | "ORNA-S002-NAMESPACE"
                    | "ORNA-S003-RESERVED"
                    | "ORNA-S010-IMPORT"
                    | "ORNA-S011-AMBIGUOUS"
                    | "ORNA-S012-UNRESOLVED"
                    | "ORNA-S013-DUPLICATE"
            ),
            Self::Typecheck => matches!(
                code,
                "ORNA-S020-ANNOTATION"
                    | "ORNA-S021-TYPE"
                    | "ORNA-S022-UNSUPPORTED"
                    | "ORNA-A091-004"
                    | "ORNA-A091-003"
                    | "ORNA-A091-007"
                    | "ORNA-A091-012"
            ),
        }
    }
}

impl ConformanceAdapter for SemanticAdapter {
    type Diagnostic = Diagnostic;
    fn diagnostic_code(&self, diagnostic: &Diagnostic) -> String {
        published_diagnostic_code(diagnostic)
    }
    fn diagnostic_message(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.message().into()
    }
    fn parse(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.syntax.parse(unit)
    }
    fn resolve(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units([unit.clone()], SemanticPhase::Resolve)
    }
    fn typecheck(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units([unit.clone()], SemanticPhase::Typecheck)
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
        self.analyze_project(project, SemanticPhase::Resolve)
    }
    fn typecheck_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.analyze_project(project, SemanticPhase::Typecheck)
    }
    fn evaluate_project(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
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
    fn evaluate_project(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "project execution requires the integrated Orna runtime".into(),
        }
    }
    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic>;
    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic>;
    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic>;
}

/// Runs the bounded, side-effect-free evaluator for row/expression units and
/// modules composed only of immutable bindings and pure functions with named
/// parameters.
///
/// Module execution, table access, mutations, external effects, and prose
/// scenarios remain explicit skips until their owning runtime contracts exist.
#[derive(Default)]
pub struct BoundedEvaluator {
    environment: Environment,
    limits: EvaluatorLimits,
    functions: BTreeMap<String, RetainedFunction>,
}

impl BoundedEvaluator {
    #[must_use]
    pub fn new(limits: EvaluatorLimits) -> Self {
        Self {
            environment: BTreeMap::new(),
            limits,
            functions: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_environment(limits: EvaluatorLimits, environment: Environment) -> Self {
        Self {
            environment,
            limits,
            functions: BTreeMap::new(),
        }
    }

    /// Explicitly invokes one zero-argument function retained during module
    /// loading. Loading a module never evaluates a retained function body.
    pub fn invoke(&self, function: &str) -> StageOutcome<Diagnostic> {
        self.invoke_with(function, &Environment::new())
    }

    /// Explicitly invokes one retained pure function with named arguments.
    /// Defaults are evaluated only after earlier parameters have been bound.
    pub fn invoke_with(&self, function: &str, arguments: &Environment) -> StageOutcome<Diagnostic> {
        if !self.functions.contains_key(function) {
            return StageOutcome::Skipped {
                reason: "function is not retained by the bounded evaluator".into(),
            };
        }
        match invoke_named(function, &self.functions, arguments, self.limits) {
            Ok(_) => StageOutcome::Passed,
            Err(error) => StageOutcome::Failed(error.diagnostic().clone()),
        }
    }

    fn evaluate_unit(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        match unit.parse_as.as_str() {
            "row_unit" | "expression_unit" | "repl_unit" => {
                match evaluate_expression(&unit.source, &self.environment, self.limits) {
                    Ok(_) => StageOutcome::Passed,
                    Err(error) => StageOutcome::Failed(error.diagnostic().clone()),
                }
            }
            "module_unit" => self.evaluate_module(unit),
            _ => Self::unsupported_module(),
        }
    }

    fn evaluate_module(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        let parsed = parse_module(&unit.source);
        if !parsed.is_ok() {
            let mut syntax = SyntaxAdapter;
            return syntax.parse(unit);
        }
        if !Self::supports_pure_declarations(&parsed.value.items) {
            return Self::unsupported_module();
        }
        let mut environment = self.environment.clone();
        let mut functions = self.functions.clone();
        for item in parsed.value.items {
            match item.declaration {
                Declaration::Let {
                    pattern: Pattern::Name(name, _),
                    value,
                    ..
                } => match evaluate_parsed(&value, &environment, self.limits) {
                    Ok(value) => {
                        environment.insert(name, value);
                    }
                    Err(error) => return StageOutcome::Failed(error.diagnostic().clone()),
                },
                Declaration::Function { signature, body } => {
                    functions.insert(
                        signature.name,
                        RetainedFunction {
                            body,
                            environment: environment.clone(),
                            parameters: signature.parameters,
                        },
                    );
                }
                _ => return Self::unsupported_module(),
            }
        }
        self.environment = environment;
        self.functions = functions;
        StageOutcome::Passed
    }

    fn supports_pure_module(unit: &SourceUnit) -> bool {
        let parsed = parse_module(&unit.source);
        parsed.is_ok() && Self::supports_pure_declarations(&parsed.value.items)
    }

    fn supports_pure_declarations(items: &[orna_syntax_v1::Item]) -> bool {
        items.iter().all(|item| {
            matches!(
                &item.declaration,
                Declaration::Let {
                    pattern: Pattern::Name(_, _),
                    ..
                } | Declaration::Function { .. }
            )
        })
    }

    fn supports_pure_project(project: &ProjectUnit) -> bool {
        let environment = &project.expectations.environment;
        !environment.network
            && !environment.credentials
            && environment.intrinsics == "Orna 1.0.0 core"
            && environment.stdlib.is_none()
            && environment.initial_tables == "empty"
            && project.modules.iter().all(Self::supports_pure_module)
    }

    fn unsupported_module() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "module execution requires an effect-free module with immutable bindings and pure functions".into(),
        }
    }

    fn unsupported_project() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "project execution requires an offline empty-state project whose every module has only immutable bindings and pure functions; tables, effects, and streams require the integrated runtime".into(),
        }
    }
}

impl RuntimeEvaluator for BoundedEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.evaluate_unit(unit)
    }

    fn evaluate_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        // Preflight the complete project before loading any module.  This
        // leaves evaluator state unchanged when a later module needs tables,
        // effects, streams, or another integrated-runtime feature.
        if !Self::supports_pure_project(project) {
            return Self::unsupported_project();
        }
        for module in &project.modules {
            match self.evaluate_module(module) {
                StageOutcome::Passed => {}
                outcome => return outcome,
            }
        }
        StageOutcome::Passed
    }

    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.evaluate_unit(unit)
    }

    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        if project.loose_rows.is_empty() {
            return StageOutcome::Passed;
        }
        for row in &project.loose_rows {
            match self.evaluate_unit(row) {
                StageOutcome::Passed => {}
                outcome => return outcome,
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
        published_diagnostic_code(diagnostic)
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
    fn evaluate_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.runtime.evaluate_project(project)
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
