//! Semantic and runtime conformance seams for the frozen v1 contracts.
//!
//! `orna-semantic-v1` is a real, deterministic analyzer, but intentionally
//! exposes one combined analysis result rather than separate compiler phases.
//! This adapter executes it and only reports a phase result where that result
//! is meaningful. `RuntimeEvaluator` remains an explicit seam: the bounded
//! evaluator below handles pure expression units, while the durable runtime
//! still owns module execution, effects, and scenario lifecycles.

use crate::{
    ConformanceAdapter, ProjectUnit, Scenario, SourceUnit, StageOutcome, SyntaxAdapter,
    row_admission::{admit_project_rows, preflight_project_rows},
};
use num_bigint::BigInt;
use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits as EvaluatorLimits,
    PureFunction as RetainedFunction, evaluate_expression_with_functions, invoke_named,
    invoke_named_with_effects,
};
use orna_foundation_v1::{Diagnostic, DiagnosticSeverity, OvbRaw, SafeText, Value};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    ListStreamSource, NoFault, RuntimeError, RuntimeIdentity, RuntimeState, StreamHandler,
    StreamHandlerResult, StreamItem, StreamRunOutcome, StreamTableMutationBatch, TableMutation,
};
use orna_semantic_v1::{Catalogue, ModuleInput, StandardDependencyProfile, analyze_with_catalogue};
use orna_storage_v1::{LoosePath, RuntimePublicationCoordinator};
use orna_stream_v1::{
    CheckpointKey, Component, ConsumerIdentity, DiagnosticClass, DiagnosticCode, SafeDiagnostic,
};
use orna_syntax_v1::{Declaration, Expr, Pattern, parse_expression, parse_module};
use orna_table_v1::{ActivationError, DatabaseActivation, DatabaseRuntime, TableError};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub struct SemanticAdapter {
    syntax: SyntaxAdapter,
    catalogue: Catalogue,
}

fn published_diagnostic_code(diagnostic: &Diagnostic) -> String {
    match diagnostic.message() {
        "calendar bucketing of Instant requires a time zone" => "E6001".into(),
        message if message.starts_with("table write contains an unknown field `") => "E2003".into(),
        message if message.starts_with("table write field has an incompatible type:") => {
            "E2001".into()
        }
        message if message.starts_with("missing required field `") => "E2002".into(),
        message
            if message.starts_with("computed field `") && message.ends_with("during insert") =>
        {
            "E3010".into()
        }
        message if message.starts_with("computed field `") && message.ends_with("directly") => {
            "E3011".into()
        }
        "primary keys are immutable through update" => "E3002".into(),
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
        "same durable source consumed twice without distinct consumer identity" => "E9101".into(),
        "cross-database writes are not atomic/supported" => "E9001".into(),
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
    fn project_analysis(&self, project: &ProjectUnit) -> orna_semantic_v1::Analysis {
        let prefix = format!("{}/", project.project_id.trim_end_matches('/'));
        let inputs = project
            .modules
            .iter()
            .map(|unit| {
                let logical_path = unit
                    .source_id
                    .strip_prefix(&prefix)
                    .unwrap_or(&unit.source_id)
                    .to_owned();
                ModuleInput::new(logical_path, unit.source.clone())
            })
            .collect::<Vec<_>>();
        analyze_with_catalogue(&inputs, &self.catalogue)
    }

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
        let analysis = self.project_analysis(project);
        match analysis
            .diagnostics
            .into_iter()
            .find(|diagnostic| phase.accepts(diagnostic.code()))
        {
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
    /// Preflight before semantic analysis so configured resource bounds apply
    /// before any project module or loose row is parsed.
    fn preflight_row_validation(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Passed
    }
    /// Receives the resolved project analysis without replacing a supplied
    /// runtime's row-validation policy.
    fn validate_resolved_rows(
        &mut self,
        project: &ProjectUnit,
        _: &orna_semantic_v1::Analysis,
    ) -> StageOutcome<Diagnostic> {
        self.validate_rows(project)
    }
    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic>;
}

/// Runs the bounded, side-effect-free evaluator for row/expression units and
/// retains function-only modules for explicit invocation. Module-level `let`
/// is not part of the module grammar; immutable values enter via the host
/// environment or local bindings inside function bodies.
///
/// Table access, mutations, external effects, and scenarios without an exact
/// executable contract remain explicit skips. Supported pure scenarios run
/// isolated evaluations with exact value and syntax/diagnostic checks.
#[derive(Clone, Default)]
pub struct BoundedEvaluator {
    environment: Environment,
    limits: EvaluatorLimits,
    functions: BTreeMap<String, RetainedFunction>,
}

impl BoundedEvaluator {
    fn check_scenario_expression(
        &self,
        source: &str,
        environment: &Environment,
        expected: &Value,
    ) -> StageOutcome<Diagnostic> {
        match evaluate_expression_with_functions(source, environment, &self.functions, self.limits)
        {
            Ok(actual) if &actual == expected => StageOutcome::Passed,
            Ok(_) => scenario_mismatch(),
            Err(error) => StageOutcome::Failed(error.diagnostic().clone()),
        }
    }

    fn run_pipeline_precedence(&self) -> StageOutcome<Diagnostic> {
        let mut runtime = Self::new(self.limits);
        let module = SourceUnit {
            fixture_id: "PIPE-002".into(), source_id: "precedence.orna".into(), parse_as: "module_unit".into(),
            // Ordinary helper definitions isolate precedence from a claim of
            // integrated standard-library execution.
            source: "fn square(value: Int) = value * value; fn count(values: [Int]) { let total = 0; for item in values { total += 1; }; total }".into(),
        };
        match runtime.evaluate_module(&module) {
            StageOutcome::Passed => {}
            outcome => return outcome,
        }
        for length in [0, 3] {
            let environment = Environment::from([(
                "values".into(),
                Value::new(OvbRaw::Array(
                    (0..length).map(|value| OvbRaw::Int(value.into())).collect(),
                ))
                .expect("canonical values"),
            )]);
            for (source, outer, left_operator, expected) in [
                ("1 + 2 | square", "|", "+", OvbRaw::Int(9.into())),
                ("values | count > 0", ">", "|", OvbRaw::Bool(length > 0)),
                (
                    "(values | count) + 1",
                    "+",
                    "|",
                    OvbRaw::Int((length + 1).into()),
                ),
            ] {
                let parsed = parse_expression(source);
                let shape = match &parsed.value {
                    Expr::Binary { lhs, op, .. } if op == outer => {
                        let lhs = match lhs.as_ref() {
                            Expr::Group { inner, .. } => inner.as_ref(),
                            lhs => lhs,
                        };
                        matches!(lhs, Expr::Binary { op, .. } if op == left_operator)
                    }
                    _ => false,
                };
                if !parsed.is_ok() || !shape {
                    return scenario_mismatch();
                }
                let expected = Value::new(expected).expect("canonical expected result");
                match runtime.check_scenario_expression(source, &environment, &expected) {
                    StageOutcome::Passed => {}
                    outcome => return outcome,
                }
            }
        }
        StageOutcome::Passed
    }

    fn run_pipeline_insertion(&self) -> StageOutcome<Diagnostic> {
        let mut runtime = Self::new(self.limits);
        let module = SourceUnit {
            fixture_id: "PIPE-001".into(),
            source_id: "pipeline.orna".into(),
            parse_as: "module_unit".into(),
            // Returning every argument makes argument position observable; a
            // boolean predicate alone could conceal a swapped argument.
            source: "fn between(value: Int, low: Int, high: Int) = [value, low, high];".into(),
        };
        match runtime.evaluate_module(&module) {
            StageOutcome::Passed => {}
            outcome => return outcome,
        }
        let parsed = parse_expression("value | between(10, 20)");
        let structure_matches = match &parsed.value {
            Expr::Binary { lhs, op, rhs, .. }
                if op == "|"
                    && matches!(lhs.as_ref(), Expr::Name { text, .. } if text == "value") =>
            {
                matches!(rhs.as_ref(), Expr::Call { callee, arguments, .. }
                    if matches!(callee.as_ref(), Expr::Name { text, .. } if text == "between")
                    && arguments.len() == 2 && arguments.iter().all(|argument| argument.name.is_none()))
            }
            _ => false,
        };
        if !parsed.is_ok() || !structure_matches {
            return scenario_mismatch();
        }
        for number in [5, 15, 25] {
            let environment = Environment::from([(
                "value".into(),
                Value::new(OvbRaw::Int(number.into())).expect("canonical argument"),
            )]);
            let expected = Value::new(OvbRaw::Array(vec![
                OvbRaw::Int(number.into()),
                OvbRaw::Int(10.into()),
                OvbRaw::Int(20.into()),
            ]))
            .expect("canonical expectation");
            for expression in [
                "value | between(10, 20)",
                "between(value, 10, 20)",
                "value | between(low: 10, high: 20)",
            ] {
                match runtime.check_scenario_expression(expression, &environment, &expected) {
                    StageOutcome::Passed => {}
                    outcome => return outcome,
                }
            }
        }
        StageOutcome::Passed
    }

    fn run_let_rebinding(&self) -> StageOutcome<Diagnostic> {
        let runtime = Self::new(self.limits);
        let expected = Value::new(OvbRaw::Array(vec![
            OvbRaw::Int(2.into()),
            OvbRaw::Int(1.into()),
            OvbRaw::Int(1.into()),
        ]))
        .expect("scenario expectation is canonical");
        // Independent exact expectations cover scalar and aggregate values,
        // both copied bindings and closures captured before replacement.
        for source in [
            "if true { let slot = 1; let captured = slot; let snapshot = () => slot; slot = 2; [slot, captured, snapshot()] } else { [0, 0, 0] }",
            "if true { let slot = { value: 1 }; let captured = slot; let snapshot = () => slot; slot = { value: 2 }; [slot.value, captured.value, snapshot().value] } else { [0, 0, 0] }",
            "if true { let slot = [1]; let captured = slot; let snapshot = () => slot; slot = [2]; [slot[0], captured[0], snapshot()[0]] } else { [0, 0, 0] }",
        ] {
            match runtime.check_scenario_expression(source, &Environment::new(), &expected) {
                StageOutcome::Passed => {}
                outcome => return outcome,
            }
        }
        let parsed = parse_module("fn sample() { var slot = 1; slot }");
        if !parsed.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "ORNA091-E-VAR" && diagnostic.message.contains("`let`")
        }) {
            return scenario_mismatch();
        }
        StageOutcome::Passed
    }

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

    /// Loads pure standard-library source only after every module is verified
    /// against the caller's pinned dependency profile. The staged evaluator is
    /// published only when the complete bundle is admitted; effects, tables,
    /// and streams remain outside this bounded source-execution seam.
    pub fn load_standard_sources(
        &mut self,
        profile: &StandardDependencyProfile,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> StageOutcome<Diagnostic> {
        let mut sources = sources.into_iter().collect::<Vec<_>>();
        sources.sort_by(|left, right| left.0.cmp(&right.0));
        if Catalogue::from_standard_sources(profile, sources.clone()).is_err() {
            return StageOutcome::Failed(standard_profile_diagnostic());
        }
        let mut staged = self.clone();
        for (logical_path, source) in sources {
            let unit = SourceUnit {
                fixture_id: format!("standard:{logical_path}"),
                source_id: logical_path,
                parse_as: "module_unit".into(),
                source,
            };
            match staged.evaluate_module(&unit) {
                StageOutcome::Passed => {}
                outcome => return outcome,
            }
        }
        *self = staged;
        StageOutcome::Passed
    }

    fn evaluate_unit(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        match unit.parse_as.as_str() {
            "row_unit" | "expression_unit" | "repl_unit" => {
                match evaluate_expression_with_functions(
                    &unit.source,
                    &self.environment,
                    &self.functions,
                    self.limits,
                ) {
                    Ok(_) => StageOutcome::Passed,
                    Err(error) => StageOutcome::Failed(error.diagnostic().clone()),
                }
            }
            "module_unit" => self.evaluate_module(unit),
            _ => Self::unsupported_module(),
        }
    }

    fn evaluate_module(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.evaluate_module_with_namespace(unit, None)
    }

    fn evaluate_module_with_namespace(
        &mut self,
        unit: &SourceUnit,
        namespace: Option<&str>,
    ) -> StageOutcome<Diagnostic> {
        if let Err(error) = self.limits.check_source(&unit.source) {
            return StageOutcome::Failed(error.diagnostic().clone());
        }
        let parsed = parse_module(&unit.source);
        if !parsed.is_ok() {
            let mut syntax = SyntaxAdapter;
            return syntax.parse(unit);
        }
        if !Self::supports_pure_declarations(&parsed.value.items) {
            return Self::unsupported_module();
        }
        let names = self
            .functions
            .keys()
            .chain(
                parsed
                    .value
                    .items
                    .iter()
                    .filter_map(|item| match &item.declaration {
                        Declaration::Function { signature, .. } => Some(&signature.name),
                        _ => None,
                    }),
            )
            .collect::<BTreeSet<_>>();
        if let Err(error) = self
            .limits
            .check_items(parsed.value.items.len())
            .and_then(|_| self.limits.check_items(names.len()))
        {
            return StageOutcome::Failed(error.diagnostic().clone());
        }
        let mut functions = self.functions.clone();
        for item in parsed.value.items {
            match item.declaration {
                Declaration::Function { signature, body } => {
                    let function_name = namespace
                        .map(|namespace| format!("{namespace}.{}", signature.name))
                        .unwrap_or_else(|| signature.name.clone());
                    functions.insert(
                        function_name,
                        RetainedFunction {
                            body,
                            environment: self.environment.clone(),
                            parameters: signature.parameters,
                        },
                    );
                }
                Declaration::Use { .. } => {}
                _ => return Self::unsupported_module(),
            }
        }
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
                Declaration::Function { .. } | Declaration::Use { .. }
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
            reason: "bounded module loading accepts function declarations only; other declarations require the integrated runtime".into(),
        }
    }

    fn unsupported_project() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "project execution requires an offline empty-state project whose modules contain only function declarations; tables, effects, and streams require the integrated runtime".into(),
        }
    }
}

fn standard_profile_diagnostic() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-STANDARD-PROFILE").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("pinned standard dependency source was rejected").expect("static message"),
    )
    .expect("valid diagnostic")
    .redacted()
}

impl RuntimeEvaluator for BoundedEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.evaluate_unit(unit)
    }

    fn evaluate_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        // Preflight the complete project before loading any module.  This
        // leaves evaluator state unchanged when a later module needs tables,
        // effects, streams, or another integrated-runtime feature.
        if let Err(error) = self.limits.check_items(project.modules.len()) {
            return StageOutcome::Failed(error.diagnostic().clone());
        }
        for module in &project.modules {
            if let Err(error) = self.limits.check_source(&module.source) {
                return StageOutcome::Failed(error.diagnostic().clone());
            }
        }
        if !Self::supports_pure_project(project) {
            return Self::unsupported_project();
        }
        let mut staged = self.clone();
        for module in &project.modules {
            let namespace = module_namespace(module);
            match staged.evaluate_module_with_namespace(module, namespace.as_deref()) {
                StageOutcome::Passed => {}
                outcome => return outcome,
            }
        }
        *self = staged;
        StageOutcome::Passed
    }

    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "row validation requires a resolved table schema and path-derived key; expression evaluation alone is not row validation".into(),
        }
    }

    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        let _ = project;
        StageOutcome::Skipped {
            reason: "row validation requires resolved table schemas and path-derived keys".into(),
        }
    }

    fn preflight_row_validation(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        match preflight_project_rows(project, self.limits) {
            Ok(()) => StageOutcome::Passed,
            Err(diagnostic) => StageOutcome::Failed(*diagnostic),
        }
    }

    fn validate_resolved_rows(
        &mut self,
        project: &ProjectUnit,
        analysis: &orna_semantic_v1::Analysis,
    ) -> StageOutcome<Diagnostic> {
        match admit_project_rows(project, analysis, self.limits) {
            Ok(_) => StageOutcome::Passed,
            Err(diagnostic) => StageOutcome::Failed(*diagnostic),
        }
    }

    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        if let_rebinding_contract(scenario) {
            return self.run_let_rebinding();
        }
        if pipeline_insertion_contract(scenario) {
            return self.run_pipeline_insertion();
        }
        if pipeline_precedence_contract(scenario) {
            return self.run_pipeline_precedence();
        }
        StageOutcome::Skipped {
            reason: "scenario has no implemented execution contract in the bounded runtime".into(),
        }
    }
}

fn module_namespace(unit: &SourceUnit) -> Option<String> {
    let mut components = unit.source_id.split('/').collect::<Vec<_>>();
    let file = components.pop()?;
    let stem = file.strip_suffix(".orna")?;
    if components.is_empty() && stem == "main" {
        return None;
    }
    components.push(stem);
    Some(components.join("."))
}

type TransactionDatabase = DatabaseRuntime<String, Vec<u8>, Value>;
type TransactionActivation<'runtime> = DatabaseActivation<'runtime, String, Vec<u8>, Value>;

/// A bounded source executor for the first real table-transaction slice.
///
/// It admits a module through the v1 syntax and semantic analyzer, retains its
/// function bodies, and routes explicit-key table mutations through one root
/// activation. Other effects remain explicit skips until their own runtime
/// contracts exist.
pub struct TransactionalEvaluator {
    entry: String,
    limits: EvaluatorLimits,
    database: TransactionDatabase,
}

impl TransactionalEvaluator {
    #[must_use]
    pub fn new(entry: impl Into<String>, limits: EvaluatorLimits) -> Self {
        Self {
            entry: entry.into(),
            limits,
            database: TransactionDatabase::default(),
        }
    }

    /// Returns a committed row by its canonical encoded key.
    pub fn committed_row(&self, table: &str, key: &Value) -> Option<&Value> {
        let encoded = key.encode().ok()?;
        let table = table.to_owned();
        self.database.committed(&table, &encoded)
    }

    /// Executes the configured entry function inside one root activation.
    /// Errors escaping the function leave all table writes unpublished.
    pub fn execute_source(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        let (functions, key_fields) = match admit_transaction_source(unit, self.limits, &self.entry)
        {
            Ok(value) => value,
            Err(outcome) => return *outcome,
        };
        match self.execute_admitted(&functions, &key_fields) {
            Ok(_) => StageOutcome::Passed,
            Err(diagnostic) => StageOutcome::Failed(*diagnostic),
        }
    }

    fn run_transaction_scenario(&self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        let (entry, source) = match scenario.id.as_str() {
            "TXN-001" => (
                "parent",
                "pub table Note(id: Int) { text: Str, } fn child() { Note.insert({ id: 7, text: \"nested\" }); } fn parent() { child(); assert false; }",
            ),
            "TXN-002" => (
                "main",
                "pub table Order(id: Int) { text: Str, } pub table Payment(id: Int) { text: Str, } pub table Audit(id: Int) { text: Str, } fn main() { Order.insert({ id: 1, text: \"order\" }); Payment.insert({ id: 1, text: \"payment\" }); Audit.insert({ id: 1, text: \"audit\" }); assert Order.count() == 1; assert Payment.count() == 1; assert Audit.count() == 1; }",
            ),
            _ => {
                return StageOutcome::Skipped {
                    reason: "scenario has no implemented execution contract in the bounded runtime"
                        .into(),
                };
            }
        };
        let unit = SourceUnit {
            fixture_id: scenario.id.clone(),
            source_id: format!("{}.orna", scenario.id.to_lowercase()),
            parse_as: "module_unit".into(),
            source: source.into(),
        };
        let mut evaluator = TransactionalEvaluator::new(entry, self.limits);
        match scenario.id.as_str() {
            "TXN-001" => match evaluator.execute_source(&unit) {
                StageOutcome::Failed(diagnostic)
                    if diagnostic.code() == "ORNA-EVAL-ASSERT"
                        && evaluator
                            .committed_row("Note", &Value::int(7.into()))
                            .is_none() =>
                {
                    StageOutcome::Passed
                }
                _ => scenario_mismatch(),
            },
            "TXN-002" => match evaluator.execute_source(&unit) {
                StageOutcome::Passed
                    if evaluator
                        .committed_row("Order", &Value::int(1.into()))
                        .is_some()
                        && evaluator
                            .committed_row("Payment", &Value::int(1.into()))
                            .is_some()
                        && evaluator
                            .committed_row("Audit", &Value::int(1.into()))
                            .is_some() =>
                {
                    StageOutcome::Passed
                }
                _ => scenario_mismatch(),
            },
            _ => unreachable!("transaction scenario contract admitted only known IDs"),
        }
    }

    fn execute_admitted(
        &mut self,
        functions: &Functions,
        key_fields: &BTreeMap<String, String>,
    ) -> Result<Vec<TableMutation>, Box<Diagnostic>> {
        let entry = self.entry.clone();
        let limits = self.limits;
        let mut mutations = Vec::new();
        let result = self.database.activate(|activation| {
            let mut effects = TableEffectHandler {
                activation,
                key_fields,
                mutations: &mut mutations,
                next_mutation: 0,
            };
            invoke_named_with_effects(&entry, functions, &Environment::new(), limits, &mut effects)
                .map(|_| ())
        });
        match result {
            Ok(()) => Ok(mutations),
            Err(ActivationError::Operation(error)) => Err(Box::new(error.diagnostic().clone())),
            Err(ActivationError::Commit(error)) => {
                Err(Box::new(transaction_error_diagnostic(error)))
            }
        }
    }

    fn seed_committed(
        &mut self,
        table: String,
        key: Vec<u8>,
        row: Value,
    ) -> Result<(), RuntimeError> {
        self.database
            .activate(|activation| activation.insert(table, key, row))
            .map_err(|_| RuntimeError::InvalidTableMutation)
    }
}

/// A source evaluator whose successful root activation is committed to the
/// durable runtime and whose next invocation rehydrates from that state.
pub struct DurableTransactionalEvaluator {
    entry: String,
    limits: EvaluatorLimits,
}

impl DurableTransactionalEvaluator {
    #[must_use]
    pub fn new(entry: impl Into<String>, limits: EvaluatorLimits) -> Self {
        Self {
            entry: entry.into(),
            limits,
        }
    }

    /// Executes one admitted source activation against durable committed rows.
    /// Failed source evaluation never reaches the durable commit boundary.
    pub async fn execute_source(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        unit: &SourceUnit,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let (functions, key_fields) = match admit_transaction_source(unit, self.limits, &self.entry)
        {
            Ok(value) => value,
            Err(outcome) => return Ok(*outcome),
        };
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let lease = state.acquire_lease(owner_id).await?;
        let tables = key_fields.keys().map(String::as_str).collect::<Vec<_>>();
        let snapshot = state.begin_table_activation(&tables).await?;
        let context = snapshot.context();
        let mut evaluator = TransactionalEvaluator::new(&self.entry, self.limits);
        for (table, rows) in snapshot.table_rows() {
            for (key, row) in rows {
                let row = Value::decode(row).map_err(|_| RuntimeError::RecoveryInvalid)?;
                evaluator.seed_committed(table.clone(), key.clone(), row)?;
            }
        }
        let mutations = match evaluator.execute_admitted(&functions, &key_fields) {
            Ok(mutations) => mutations,
            Err(diagnostic) => return Ok(StageOutcome::Failed(*diagnostic)),
        };
        if mutations.is_empty() {
            return Ok(StageOutcome::Passed);
        }
        let next_digest =
            durable_activation_digest(context.capture().generation_digest(), &mutations);
        state
            .commit_table_activation(lease, context, &mutations, next_digest, &NoFault)
            .await?;
        Ok(StageOutcome::Passed)
    }

    /// Publishes the caller-selected frozen runtime prefix through the loose
    /// Git projection. Publication is explicit: source execution only stages
    /// durable runtime mutations and never advances the repository by itself.
    pub async fn publish_pending(
        &self,
        repository: &Repository,
        runtime: &RuntimeState,
        intent_id: [u8; 16],
        checkpoint: &orna_runtime_v1::Checkpoint,
        path_for: impl Fn(&TableMutation) -> Result<LoosePath, orna_storage_v1::Error>,
        message: &str,
    ) -> Result<orna_repository_v1::IndexGeneration, RuntimeError> {
        let freeze = runtime.freeze(intent_id, checkpoint).await?;
        let head = repository
            .head()
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let index = repository
            .index_generation()
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let mut coordinator = RuntimePublicationCoordinator::prepare_from_runtime(
            repository, runtime, &head, index, &freeze, path_for, message,
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
        let published = coordinator
            .publish(repository)
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        coordinator
            .complete(repository, runtime, &freeze)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(published)
    }
}

impl Default for DurableTransactionalEvaluator {
    fn default() -> Self {
        Self::new("main", EvaluatorLimits::default())
    }
}

impl DurableTransactionalEvaluator {
    /// Runs the deliberately narrow declarative finite-list bridge through the
    /// durable stream runner. It admits one literal `Stream.from_list`, one
    /// explicitly keyed table, and one `for_each` insert body; it does not
    /// interpret arbitrary stream expressions or administrative APIs.
    pub async fn execute_list_stream_source(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        unit: &SourceUnit,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let mut bridge = match admit_list_stream_source(unit, self.limits, &self.entry) {
            Ok(bridge) => bridge,
            Err(outcome) => return Ok(*outcome),
        };
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let writer = state.acquire_lease(owner_id).await?;
        let key = bridge.checkpoint_key()?;
        let mut source = ListStreamSource::new(key.clone(), std::mem::take(&mut bridge.payloads));
        let mut handler = ListTableHandler::new(bridge);
        match state
            .run_stream(
                writer,
                &key,
                &mut source,
                &mut handler,
                &orna_runtime_v1::NeverCancelled,
            )
            .await
        {
            Ok(StreamRunOutcome::Exhausted { .. }) => Ok(StageOutcome::Passed),
            Ok(_) => Ok(StageOutcome::Skipped {
                reason: "literal list stream did not exhaust".into(),
            }),
            Err(error) => Ok(StageOutcome::Failed(stream_error_diagnostic(error))),
        }
    }
}

struct ListStreamBridge {
    source_identity: String,
    source_id: String,
    entry: String,
    table: String,
    key_field: String,
    parameter: String,
    insert_row: Expr,
    payloads: Vec<Vec<u8>>,
}

impl ListStreamBridge {
    fn checkpoint_key(&self) -> Result<CheckpointKey, RuntimeError> {
        fn component(value: &str) -> Result<Component, RuntimeError> {
            Component::new(value).map_err(|_| RuntimeError::RecoveryInvalid)
        }
        Ok(CheckpointKey {
            consumer: ConsumerIdentity {
                principal: component("conformance")?,
                root: component(&self.source_id)?,
                function: component(&self.entry)?,
                binding: component("from_list")?,
            },
            source_format: component("orna-stream-v1")?,
            source: component(&self.source_identity)?,
            partition_format: component("literal-list")?,
            partition: component("default")?,
            position_format: component("ordinal")?,
        })
    }
}

struct ListTableHandler {
    bridge: ListStreamBridge,
    next_mutation: u64,
    digest: [u8; 32],
}

impl ListTableHandler {
    fn new(bridge: ListStreamBridge) -> Self {
        Self {
            bridge,
            next_mutation: 0,
            digest: [9; 32],
        }
    }
}

impl StreamHandler for ListTableHandler {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult {
        let value = match Value::decode(&item.payload) {
            Ok(value) => value,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        let row =
            match substitute_list_item(&self.bridge.insert_row, &self.bridge.parameter, &value) {
                Some(row) => row,
                None => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
            };
        let key = match table_key(&row, &self.bridge.key_field) {
            Ok(key) => key,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        let mut hasher = Sha256::new();
        hasher.update(b"ORNA-LIST-STREAM-MUTATION\0");
        hasher.update(item.delivery.canonical().as_bytes());
        hasher.update(self.next_mutation.to_be_bytes());
        let id = hasher.finalize()[..16]
            .try_into()
            .expect("fixed digest length");
        let encoded = match row.encode() {
            Ok(encoded) => encoded,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        let mutation = match TableMutation::new(id, &self.bridge.table, key, Some(encoded)) {
            Ok(mutation) => mutation,
            Err(_) => return StreamHandlerResult::Fail(stream_handler_diagnostic()),
        };
        self.next_mutation = self.next_mutation.saturating_add(1);
        let mut digest = Sha256::new();
        digest.update(b"ORNA-LIST-STREAM-COMMIT\0");
        digest.update(self.digest);
        digest.update(mutation.id());
        self.digest = digest.finalize().into();
        StreamHandlerResult::CommitTable(StreamTableMutationBatch {
            mutations: vec![mutation],
            next_digest: self.digest,
        })
    }
}

fn stream_handler_diagnostic() -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::DecodeRejected,
        class: DiagnosticClass::Permanent,
    }
}

fn stream_error_diagnostic(error: orna_runtime_v1::StreamStepError) -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-LIST-STREAM").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new(format!("literal list stream failed: {error}"))
            .expect("bounded stream error"),
    )
    .expect("valid diagnostic")
    .redacted()
}

fn admit_list_stream_source(
    unit: &SourceUnit,
    limits: EvaluatorLimits,
    entry: &str,
) -> Result<ListStreamBridge, AdmissionFailure> {
    let (_, key_fields) = admit_transaction_source(unit, limits, entry)?;
    if key_fields.len() != 1 {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "literal list stream bridge requires one explicit-key table".into(),
        }));
    }
    let parsed = parse_module(&unit.source);
    let body = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.declaration {
            Declaration::Function { signature, body } if signature.name == entry => Some(body),
            _ => None,
        })
        .ok_or_else(|| {
            Box::new(StageOutcome::Skipped {
                reason: "configured list stream entry function is not present".into(),
            })
        })?;
    let (values, source_identity, parameter, table, insert_row) = literal_stream_pipeline(body)
        .ok_or_else(|| Box::new(StageOutcome::Skipped {
            reason: "literal list stream bridge requires Stream.from_list piped to one for_each insert body".into(),
        }))?;
    let (table_name, key_field) = key_fields.into_iter().next().expect("one table checked");
    if table != table_name {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "literal list stream insert must target the declared table".into(),
        }));
    }
    let payloads = values
        .iter()
        .map(literal_value)
        .collect::<Option<Vec<_>>>()
        .and_then(|values| {
            values
                .into_iter()
                .map(|value| value.encode().ok())
                .collect()
        })
        .ok_or_else(|| {
            Box::new(StageOutcome::Skipped {
                reason: "literal list stream values must be canonical literals".into(),
            })
        })?;
    Ok(ListStreamBridge {
        source_identity,
        source_id: unit.source_id.clone(),
        entry: entry.into(),
        table: table_name,
        key_field,
        parameter,
        insert_row: insert_row.clone(),
        payloads,
    })
}

fn literal_stream_pipeline(body: &Expr) -> Option<(&[Expr], String, String, String, &Expr)> {
    let Expr::Block {
        statements,
        tail: None,
        ..
    } = body
    else {
        return None;
    };
    let [orna_syntax_v1::Statement::Expression { value, .. }] = statements.as_slice() else {
        return None;
    };
    let Expr::Binary { lhs, op, rhs, .. } = value else {
        return None;
    };
    if op != "|" {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = lhs.as_ref()
    else {
        return None;
    };
    if qualified_expr_path(callee).as_deref() != Some(["Stream", "from_list"].as_slice()) {
        return None;
    }
    let [values, identity] = arguments.as_slice() else {
        return None;
    };
    let Expr::List { elements, .. } = &values.value else {
        return None;
    };
    let (
        Some(name),
        Expr::Literal {
            text: source_identity,
            kind: orna_syntax_v1::LiteralKind::String,
            ..
        },
    ) = (&identity.name, &identity.value)
    else {
        return None;
    };
    let source_identity = literal_string(source_identity)?;
    if name != "source_identity" || Component::new(source_identity.clone()).is_err() {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = rhs.as_ref()
    else {
        return None;
    };
    if qualified_expr_path(callee).as_deref() != Some(["for_each"].as_slice()) {
        return None;
    }
    let [argument] = arguments.as_slice() else {
        return None;
    };
    let Expr::Lambda {
        parameters, body, ..
    } = &argument.value
    else {
        return None;
    };
    let [parameter] = parameters.as_slice() else {
        return None;
    };
    let Pattern::Name(parameter, _) = &parameter.pattern else {
        return None;
    };
    let Expr::Block {
        statements,
        tail: None,
        ..
    } = body.as_ref()
    else {
        return None;
    };
    let [orna_syntax_v1::Statement::Expression { value: insert, .. }] = statements.as_slice()
    else {
        return None;
    };
    let Expr::Call {
        callee, arguments, ..
    } = insert
    else {
        return None;
    };
    let Expr::Field { base, name, .. } = callee.as_ref() else {
        return None;
    };
    if name != "insert" {
        return None;
    }
    let Expr::Name { text: table, .. } = base.as_ref() else {
        return None;
    };
    let [row] = arguments.as_slice() else {
        return None;
    };
    Some((
        elements,
        source_identity,
        parameter.clone(),
        table.clone(),
        &row.value,
    ))
}

fn qualified_expr_path(expr: &Expr) -> Option<Vec<&str>> {
    match expr {
        Expr::Name { text, .. } => Some(vec![text]),
        Expr::Field { base, name, .. } => {
            let mut path = qualified_expr_path(base)?;
            path.push(name);
            Some(path)
        }
        _ => None,
    }
}

fn literal_value(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Literal { text, kind, .. } => match kind {
            orna_syntax_v1::LiteralKind::Integer => text.parse::<BigInt>().ok().map(Value::int),
            orna_syntax_v1::LiteralKind::String => {
                literal_string(text).and_then(|text| Value::new(OvbRaw::Text(text)).ok())
            }
            orna_syntax_v1::LiteralKind::Boolean => Value::new(OvbRaw::Bool(text == "true")).ok(),
            orna_syntax_v1::LiteralKind::Null => Value::new(OvbRaw::Null).ok(),
            _ => None,
        },
        Expr::List { elements, .. } => elements
            .iter()
            .map(literal_value)
            .collect::<Option<Vec<_>>>()
            .and_then(|values| {
                Value::new(OvbRaw::Array(
                    values
                        .into_iter()
                        .map(|value| value.raw().clone())
                        .collect(),
                ))
                .ok()
            }),
        Expr::Record { fields, .. } => fields
            .iter()
            .map(|field| {
                Some((
                    OvbRaw::Text(field.name.clone()),
                    literal_value(&field.value)?.raw().clone(),
                ))
            })
            .collect::<Option<Vec<_>>>()
            .and_then(|fields| Value::new(OvbRaw::Map(fields)).ok()),
        _ => None,
    }
}

fn literal_string(text: &str) -> Option<String> {
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    (!body.contains('\\')).then(|| body.into())
}

fn substitute_list_item(expr: &Expr, parameter: &str, item: &Value) -> Option<Value> {
    match expr {
        Expr::Name { text, .. } if text == parameter => Some(item.clone()),
        Expr::Field { base, name, .. } => match base.as_ref() {
            Expr::Name { text, .. } if text == parameter => record_field(item, name),
            _ => None,
        },
        Expr::Record { fields, .. } => fields
            .iter()
            .map(|field| {
                Some((
                    OvbRaw::Text(field.name.clone()),
                    substitute_list_item(&field.value, parameter, item)?
                        .raw()
                        .clone(),
                ))
            })
            .collect::<Option<Vec<_>>>()
            .and_then(|fields| Value::new(OvbRaw::Map(fields)).ok()),
        _ => literal_value(expr),
    }
}

impl Default for TransactionalEvaluator {
    fn default() -> Self {
        Self::new("main", EvaluatorLimits::default())
    }
}

impl RuntimeEvaluator for TransactionalEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.execute_source(unit)
    }

    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "transactional source execution does not replace schema-backed row admission"
                .into(),
        }
    }

    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "transactional source execution does not replace schema-backed row admission"
                .into(),
        }
    }

    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        if transaction_contract(scenario) {
            return self.run_transaction_scenario(scenario);
        }
        StageOutcome::Skipped {
            reason: "source transaction execution is exposed as an explicit module seam; no corpus scenario is claimed yet".into(),
        }
    }
}

struct TableEffectHandler<'activation, 'runtime> {
    activation: &'activation mut TransactionActivation<'runtime>,
    key_fields: &'activation BTreeMap<String, String>,
    mutations: &'activation mut Vec<TableMutation>,
    next_mutation: u64,
}

impl EffectHandler for TableEffectHandler<'_, '_> {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let mutation_len = self.mutations.len();
        let savepoint = self
            .activation
            .savepoint()
            .map_err(|error| transaction_error(table_error_code(error)))?;
        let result = self.handle_inner(callee, arguments);
        if result.is_err() {
            self.activation
                .rollback_to(savepoint)
                .map_err(|error| transaction_error(table_error_code(error)))?;
            self.mutations.truncate(mutation_len);
        }
        result
    }
}

impl TableEffectHandler<'_, '_> {
    fn handle_inner(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text: table, .. } = base.as_ref() else {
            return Ok(None);
        };
        let Some(key_field) = self.key_fields.get(table) else {
            return Err(transaction_error("ORNA-EVAL-TABLE-KEY"));
        };
        match name.as_str() {
            "count" => {
                if !arguments.is_empty() {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                }
                let count = self
                    .activation
                    .candidate_relation(table)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .count();
                Ok(Some(Value::int(BigInt::from(count))))
            }
            "insert" => {
                let [row] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = table_key(row, key_field)?;
                self.activation
                    .insert(table.clone(), key.clone(), row.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, key, Some(row.clone()))?;
                Ok(Some(row.clone()))
            }
            "upsert" => {
                let [row] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = table_key(row, key_field)?;
                if let Some(existing) = self
                    .activation
                    .read(table, &key)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .cloned()
                {
                    let row = merge_upsert_row(&existing, row, key_field)?;
                    let key = key.clone();
                    self.activation
                        .update(table.clone(), key.clone(), row.clone())
                        .map_err(|error| transaction_error(table_error_code(error)))?;
                    self.record(table, key, Some(row.clone()))?;
                    Ok(Some(row))
                } else {
                    self.activation
                        .insert(table.clone(), key.clone(), row.clone())
                        .map_err(|error| transaction_error(table_error_code(error)))?;
                    self.record(table, key, Some(row.clone()))?;
                    Ok(Some(row.clone()))
                }
            }
            "update" => {
                let [key, patch] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let existing = self
                    .activation
                    .read(table, &encoded_key(key)?)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .cloned()
                    .ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-MISSING"))?;
                let row = merge_row(&existing, patch, key_field)?;
                let key = encoded_key(key)?;
                self.activation
                    .update(table.clone(), key.clone(), row.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, key, Some(row.clone()))?;
                Ok(Some(row))
            }
            "delete" => {
                let [key] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = encoded_key(key)?;
                self.activation
                    .delete(table.clone(), key.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, key, None)?;
                Ok(Some(Value::unit()))
            }
            "rekey" => {
                let [old_key, new_key] = arguments else {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let old_key = encoded_key(old_key)?;
                let new_key = encoded_key(new_key)?;
                let existing = self
                    .activation
                    .read(table, &old_key)
                    .map_err(|error| transaction_error(table_error_code(error)))?
                    .cloned()
                    .ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-MISSING"))?;
                let row = replace_record_field(&existing, key_field, new_key.clone())?;
                self.activation
                    .delete(table.clone(), old_key.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, old_key, None)?;
                self.activation
                    .insert(table.clone(), new_key.clone(), row.clone())
                    .map_err(|error| transaction_error(table_error_code(error)))?;
                self.record(table, new_key, Some(row.clone()))?;
                Ok(Some(row))
            }
            _ => Ok(None),
        }
    }

    fn record(
        &mut self,
        table: &str,
        key: Vec<u8>,
        value: Option<Value>,
    ) -> Result<(), EvaluationError> {
        let encoded = value
            .as_ref()
            .map(Value::encode)
            .transpose()
            .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))?;
        let ordinal = self.next_mutation;
        self.next_mutation = self.next_mutation.saturating_add(1);
        let mut digest = Sha256::new();
        digest.update(b"ORNA-SOURCE-MUTATION\0");
        digest.update(table.as_bytes());
        digest.update([0]);
        digest.update(&key);
        digest.update(ordinal.to_be_bytes());
        if let Some(bytes) = &encoded {
            digest.update([1]);
            digest.update(bytes);
        } else {
            digest.update([0]);
        }
        let id: [u8; 16] = digest.finalize()[..16]
            .try_into()
            .expect("truncated digest has fixed length");
        let mutation = TableMutation::new(id, table, key, encoded)
            .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))?;
        self.mutations.push(mutation);
        Ok(())
    }
}

type AdmittedTransaction = (Functions, BTreeMap<String, String>);
type AdmissionFailure = Box<StageOutcome<Diagnostic>>;

fn admit_transaction_source(
    unit: &SourceUnit,
    limits: EvaluatorLimits,
    entry: &str,
) -> Result<AdmittedTransaction, AdmissionFailure> {
    if let Err(error) = limits.check_source(&unit.source) {
        return Err(Box::new(StageOutcome::Failed(error.diagnostic().clone())));
    }
    let mut syntax = SyntaxAdapter;
    if let StageOutcome::Failed(diagnostic) = syntax.parse(unit) {
        return Err(Box::new(StageOutcome::Failed(diagnostic)));
    }
    let parsed = parse_module(&unit.source);
    if let Err(error) = limits.check_items(parsed.value.items.len()) {
        return Err(Box::new(StageOutcome::Failed(error.diagnostic().clone())));
    }
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            unit.source_id.clone(),
            unit.source.clone(),
        )],
        &Catalogue::authoritative_fixture(),
    );
    if let Some(diagnostic) = analysis.diagnostics.first() {
        return Err(Box::new(StageOutcome::Failed(
            diagnostic.clone().redacted(),
        )));
    }
    let (functions, key_fields) = admitted_transaction_module(&parsed.value.items)
        .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
    if let Err(error) = limits.check_items(functions.len()) {
        return Err(Box::new(StageOutcome::Failed(error.diagnostic().clone())));
    }
    if !functions.contains_key(entry) {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "configured transaction entry function is not present".into(),
        }));
    }
    Ok((functions, key_fields))
}

fn durable_activation_digest(previous: [u8; 32], mutations: &[TableMutation]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ORNA-DURABLE-ACTIVATION\0");
    digest.update(previous);
    for mutation in mutations {
        digest.update(mutation.id());
        digest.update(mutation.table().as_bytes());
        digest.update(mutation.key());
        if let Some(value) = mutation.value() {
            digest.update([1]);
            digest.update(value);
        } else {
            digest.update([0]);
        }
    }
    digest.finalize().into()
}

fn admitted_transaction_module(
    items: &[orna_syntax_v1::Item],
) -> Result<(Functions, BTreeMap<String, String>), String> {
    let mut functions = Functions::new();
    let mut key_fields = BTreeMap::new();
    for item in items {
        match &item.declaration {
            Declaration::Function { signature, body } => {
                functions.insert(
                    signature.name.clone(),
                    RetainedFunction {
                        parameters: signature.parameters.clone(),
                        body: body.clone(),
                        environment: Environment::new(),
                    },
                );
            }
            Declaration::Table { name, keys, .. } => {
                let [key] = keys.as_slice() else {
                    return Err("transactional source seam requires one explicit table key".into());
                };
                let Pattern::Name(field, _) = &key.pattern else {
                    return Err("transactional source seam requires a named table key".into());
                };
                key_fields.insert(name.clone(), field.clone());
            }
            Declaration::Use { .. } => {
                return Err("transactional source seam does not load module imports yet".into());
            }
            _ => return Err("transactional source seam admits only tables and functions".into()),
        }
    }
    Ok((functions, key_fields))
}

fn record_field(row: &Value, field: &str) -> Option<Value> {
    let OvbRaw::Map(entries) = row.raw() else {
        return None;
    };
    entries.iter().find_map(|(key, value)| match key {
        OvbRaw::Text(name) if name == field => Value::new(value.clone()).ok(),
        _ => None,
    })
}

fn encoded_key(key: &Value) -> Result<Vec<u8>, EvaluationError> {
    key.encode()
        .map_err(|_| transaction_error("ORNA-EVAL-TABLE-KEY"))
}

fn table_key(row: &Value, field: &str) -> Result<Vec<u8>, EvaluationError> {
    record_field(row, field)
        .ok_or_else(|| transaction_error("ORNA-EVAL-TABLE-KEY"))
        .and_then(|key| encoded_key(&key))
}

fn merge_row(existing: &Value, patch: &Value, key_field: &str) -> Result<Value, EvaluationError> {
    let OvbRaw::Map(existing_fields) = existing.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let OvbRaw::Map(patch_fields) = patch.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let mut fields = existing_fields.clone();
    for (patch_key, patch_value) in patch_fields {
        let OvbRaw::Text(name) = patch_key else {
            return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
        };
        if name == key_field {
            return Err(transaction_error("ORNA-EVAL-TABLE-KEY"));
        }
        let Some((_, value)) = fields.iter_mut().find(|(key, _)| key == patch_key) else {
            return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
        };
        *value = patch_value.clone();
    }
    Value::new(OvbRaw::Map(fields)).map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))
}

fn merge_upsert_row(
    existing: &Value,
    patch: &Value,
    key_field: &str,
) -> Result<Value, EvaluationError> {
    let OvbRaw::Map(fields) = patch.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let fields = fields
        .iter()
        .filter(|(key, _)| !matches!(key, OvbRaw::Text(name) if name == key_field))
        .cloned()
        .collect();
    let patch =
        Value::new(OvbRaw::Map(fields)).map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))?;
    merge_row(existing, &patch, key_field)
}

fn replace_record_field(
    row: &Value,
    field: &str,
    replacement: Vec<u8>,
) -> Result<Value, EvaluationError> {
    let OvbRaw::Map(entries) = row.raw() else {
        return Err(transaction_error("ORNA-EVAL-TABLE-ROW"));
    };
    let mut fields = entries.clone();
    let Some((_, value)) = fields
        .iter_mut()
        .find(|(key, _)| matches!(key, OvbRaw::Text(name) if name == field))
    else {
        return Err(transaction_error("ORNA-EVAL-TABLE-KEY"));
    };
    *value = Value::decode(&replacement)
        .map_err(|_| transaction_error("ORNA-EVAL-TABLE-KEY"))?
        .raw()
        .clone();
    Value::new(OvbRaw::Map(fields)).map_err(|_| transaction_error("ORNA-EVAL-TABLE-ROW"))
}

fn transaction_error(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static transaction error code"))
}

fn transaction_error_diagnostic(error: TableError) -> Diagnostic {
    transaction_error(table_error_code(error))
        .diagnostic()
        .clone()
}

fn table_error_code(error: TableError) -> &'static str {
    match error {
        TableError::DuplicateKey => "ORNA-EVAL-TABLE-DUPLICATE",
        TableError::MissingRow => "ORNA-EVAL-TABLE-MISSING",
        TableError::ChildCannotCommit => "ORNA-EVAL-TABLE-CHILD-COMMIT",
        TableError::DoubleCommit => "ORNA-EVAL-TABLE-DOUBLE-COMMIT",
        TableError::UseAfterClose => "ORNA-EVAL-TABLE-CLOSED",
    }
}

fn pipeline_precedence_contract(scenario: &Scenario) -> bool {
    scenario.id == "PIPE-002"
        && scenario.title == "Pipeline precedence is stable"
        && scenario.given == ["`1 + 2 | square`, `values | count > 0`, and `(values | count) + 1`"]
        && scenario.when == ["parse and evaluate"]
        && scenario.then
            == [
                "arithmetic binds above the pipe",
                "comparison binds below the pipe",
                "parentheses allow arithmetic on a pipeline result",
            ]
        && scenario.requirements == ["ORNA-OP-001", "ORNA-PIPE-002", "ORNA-PIPE-003"]
}

fn pipeline_insertion_contract(scenario: &Scenario) -> bool {
    scenario.id == "PIPE-001"
        && scenario.title == "Pipeline inserts the left value as first argument"
        && scenario.given == ["`value | between(10, 20)`"]
        && scenario.when == ["lower pipeline application"]
        && scenario.then
            == [
                "the call is exactly `between(value, 10, 20)`",
                "no special pipe-function declaration is required",
            ]
        && scenario.requirements == ["ORNA-PIPE-001", "ORNA-PIPE-002", "ORNA-PIPE-003"]
}

fn let_rebinding_contract(scenario: &Scenario) -> bool {
    scenario.id == "LET-REBIND-091"
        && scenario.title == "Let slots rebind without mutable value identity"
        && scenario.given == ["a let slot whose value is captured before reassignment"]
        && scenario.when == ["assign a replacement value to the slot"]
        && scenario.then
            == [
                "the slot observes the replacement",
                "the captured value is unchanged",
                "`var` receives ORNA091-E-VAR",
            ]
        && scenario.requirements
            == [
                "ORNA-VALUE-006",
                "ORNA-VALUE-007",
                "ORNA-CFLOW-005",
                "ORNA-CFLOW-006",
                "ORNA-CFLOW-011",
            ]
}

fn transaction_contract(scenario: &Scenario) -> bool {
    match scenario.id.as_str() {
        "TXN-001" => {
            scenario.title == "Activation rolls back nested writes"
                && scenario.given == ["parent calls child; child inserts Note"]
                && scenario.when == ["parent later propagates error"]
                && scenario.then == ["child insert is rolled back"]
                && scenario.requirements == ["ORNA-TXN-001", "ORNA-TXN-002", "ORNA-TXN-003"]
        }
        "TXN-002" => {
            scenario.title == "Successful activation commits together"
                && scenario.given == ["activation inserts Order, Payment, Audit"]
                && scenario.when == ["activation returns success"]
                && scenario.then == ["all three appear together in CWD"]
                && scenario.requirements == ["ORNA-TXN-001"]
        }
        _ => false,
    }
}

fn scenario_mismatch() -> StageOutcome<Diagnostic> {
    StageOutcome::Failed(
        Diagnostic::new(
            SafeText::new("ORNA-CONFORMANCE-SCENARIO-MISMATCH").expect("static code"),
            DiagnosticSeverity::Error,
            SafeText::new("runtime scenario did not match its exact expected result")
                .expect("static message"),
        )
        .expect("valid diagnostic")
        .redacted(),
    )
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
        match self.runtime.preflight_row_validation(project) {
            StageOutcome::Passed => {}
            outcome => return outcome,
        }
        let analysis = self.semantic.project_analysis(project);
        if let Some(diagnostic) = analysis.diagnostics.first() {
            return StageOutcome::Failed(diagnostic.clone().redacted());
        }
        self.runtime.validate_resolved_rows(project, &analysis)
    }
    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        self.runtime.run_scenario(scenario)
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::{BoundedEvaluator, RuntimeEvaluator, SourceUnit, StageOutcome};
    use crate::{ProjectEnvironment, ProjectExpectations, ProjectUnit};
    use orna_evaluator_v1::{Environment, Limits, evaluate_expression_with_functions};
    use orna_foundation_v1::Value;

    #[test]
    fn project_modules_retain_qualified_pure_functions() {
        let mut evaluator = BoundedEvaluator::new(Limits::default());
        let library = SourceUnit {
            fixture_id: "library".into(),
            source_id: "library.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub fn add(left: Int, right: Int): Int = left + right; pub fn twice(value: Int): Int = add(value, value);".into(),
        };
        let main = SourceUnit {
            fixture_id: "main".into(),
            source_id: "main.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub fn answer(): Int = library.twice(21);".into(),
        };

        assert!(matches!(
            evaluator.evaluate_module_with_namespace(&library, Some("library")),
            StageOutcome::Passed
        ));
        assert!(matches!(
            evaluator.evaluate_module_with_namespace(&main, None),
            StageOutcome::Passed
        ));
        let result = evaluate_expression_with_functions(
            "answer()",
            &Environment::new(),
            &evaluator.functions,
            Limits::default(),
        )
        .expect("qualified module call");
        assert_eq!(result, Value::int(42.into()));

        let result = evaluate_expression_with_functions(
            "library.twice(21)",
            &Environment::new(),
            &evaluator.functions,
            Limits::default(),
        )
        .expect("qualified module call");
        assert_eq!(result, Value::int(42.into()));
    }

    #[test]
    fn pure_project_evaluation_accepts_ordinary_import_declarations() {
        let mut evaluator = BoundedEvaluator::new(Limits::default());
        let project = ProjectUnit {
            fixture_id: "project".into(),
            project_id: "project".into(),
            environment_id: None,
            modules: vec![
                SourceUnit {
                    fixture_id: "project".into(),
                    source_id: "main.orna".into(),
                    parse_as: "module_unit".into(),
                    source: "use library; pub fn answer(): Int = library.value();".into(),
                },
                SourceUnit {
                    fixture_id: "project".into(),
                    source_id: "library.orna".into(),
                    parse_as: "module_unit".into(),
                    source: "pub fn value(): Int = 42;".into(),
                },
            ],
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
        };

        assert!(matches!(
            evaluator.evaluate_project(&project),
            StageOutcome::Passed
        ));
        assert!(matches!(evaluator.invoke("answer"), StageOutcome::Passed));
    }
}

#[cfg(test)]
mod durable_tests {
    use super::{DurableTransactionalEvaluator, SourceUnit, StageOutcome};
    use orna_evaluator_v1::Limits;
    use orna_foundation_v1::Value;
    use orna_repository_v1::Repository;
    use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
    use std::{path::Path, process::Command};
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git command")
                .success()
        );
    }

    fn source(body: &str) -> SourceUnit {
        SourceUnit {
            fixture_id: "durable-txn".into(),
            source_id: "durable-txn.orna".into(),
            parse_as: "module_unit".into(),
            source: format!("pub table Note(id: Int) {{ text: Str, }} fn main() {{ {body} }}"),
        }
    }

    #[tokio::test]
    async fn source_activation_commits_rows_and_reopens_for_the_next_activation() {
        let temp = TempDir::new().expect("temporary repository");
        git(temp.path(), &["init"]);
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(temp.path(), &["config", "user.name", "test"]);
        let repository = Repository::discover(temp.path()).expect("repository");
        let identity = RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        };
        let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

        assert!(matches!(
            evaluator
                .execute_source(
                    &repository,
                    identity,
                    [3; 16],
                    [4; 32],
                    &source(r#"Note.insert({ id: 7, text: "first" });"#),
                )
                .await,
            Ok(StageOutcome::Passed)
        ));
        let state = RuntimeState::open(&repository, identity, [4; 32])
            .await
            .expect("reopened runtime");
        let key = Value::int(7.into()).encode().expect("encoded key");
        let expected = Value::new(orna_foundation_v1::OvbRaw::Map(vec![
            (
                orna_foundation_v1::OvbRaw::Text("id".into()),
                orna_foundation_v1::OvbRaw::Int(7.into()),
            ),
            (
                orna_foundation_v1::OvbRaw::Text("text".into()),
                orna_foundation_v1::OvbRaw::Text("first".into()),
            ),
        ]))
        .expect("canonical row")
        .encode()
        .expect("encoded row");
        assert_eq!(
            state.committed_table_row("Note", &key).await.unwrap(),
            Some(expected)
        );
        drop(state);

        assert!(matches!(
            evaluator
                .execute_source(
                    &repository,
                    identity,
                    [3; 16],
                    [4; 32],
                    &source(r#"Note.update(7, { text: "changed" });"#),
                )
                .await,
            Ok(StageOutcome::Passed)
        ));
        let state = RuntimeState::open(&repository, identity, [4; 32])
            .await
            .expect("reopened updated runtime");
        let row = state
            .committed_table_row("Note", &key)
            .await
            .expect("durable row read")
            .expect("updated row");
        let row = Value::decode(&row).expect("canonical durable row");
        assert!(matches!(
            row.raw(),
            orna_foundation_v1::OvbRaw::Map(fields)
                if fields.iter().any(|(key, value)| key
                    == &orna_foundation_v1::OvbRaw::Text("text".into())
                    && value
                        == &orna_foundation_v1::OvbRaw::Text("changed".into()))
        ));
    }
}

#[cfg(test)]
mod list_stream_tests {
    use super::{
        DurableTransactionalEvaluator, SourceUnit, StageOutcome, stream_handler_diagnostic,
    };
    use orna_evaluator_v1::Limits;
    use orna_foundation_v1::Value;
    use orna_repository_v1::Repository;
    use orna_runtime_v1::{
        ListStreamSource, RuntimeIdentity, RuntimeState, StreamHandler, StreamHandlerResult,
        StreamItem,
    };
    use orna_stream_v1::{AsyncCheckpointBackend, CheckpointKey, Component, ConsumerIdentity};
    use std::{path::Path, process::Command};
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git command")
                .success()
        );
    }

    fn repository() -> (TempDir, Repository) {
        let temp = TempDir::new().expect("temporary repository");
        git(temp.path(), &["init", "-q"]);
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(temp.path(), &["config", "user.name", "test"]);
        let repository = Repository::discover(temp.path()).expect("repository");
        (temp, repository)
    }

    fn identity() -> RuntimeIdentity {
        RuntimeIdentity {
            database_id: [21; 16],
            repository_id: [22; 16],
        }
    }

    fn source() -> SourceUnit {
        SourceUnit {
            fixture_id: "list-stream".into(),
            source_id: "sensors.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub table Reading(id: Int) { value: Int, } fn main() { Stream.from_list([1, 2], source_identity: \"fixture:readings\") | for_each(value => { Reading.insert({ id: value, value: value }); }); }".into(),
        }
    }

    #[tokio::test]
    async fn literal_list_stream_reopens_without_duplicate_delivery() {
        let (_temp, repository) = repository();
        let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

        assert!(matches!(
            evaluator
                .execute_list_stream_source(&repository, identity(), [23; 16], [24; 32], &source())
                .await,
            Ok(StageOutcome::Passed)
        ));
        let state = RuntimeState::open(&repository, identity(), [24; 32])
            .await
            .expect("reopened runtime");
        let first_generation = state
            .capture()
            .await
            .expect("first capture")
            .generation()
            .clone();
        for value in [1, 2] {
            let key = Value::int(value.into()).encode().expect("encoded key");
            assert!(
                state
                    .committed_table_row("Reading", &key)
                    .await
                    .expect("durable row read")
                    .is_some()
            );
        }
        drop(state);

        assert!(matches!(
            evaluator
                .execute_list_stream_source(&repository, identity(), [23; 16], [24; 32], &source())
                .await,
            Ok(StageOutcome::Passed)
        ));
        let state = RuntimeState::open(&repository, identity(), [24; 32])
            .await
            .expect("second reopened runtime");
        assert_eq!(
            state
                .capture()
                .await
                .expect("second capture")
                .generation()
                .clone(),
            first_generation,
            "an exhausted reopened source must not publish duplicate table commits"
        );
    }

    struct FailingHandler;

    impl StreamHandler for FailingHandler {
        fn handle(&mut self, _: &StreamItem) -> StreamHandlerResult {
            StreamHandlerResult::Fail(stream_handler_diagnostic())
        }
    }

    fn key() -> CheckpointKey {
        let component = |value| Component::new(value).expect("static component");
        CheckpointKey {
            consumer: ConsumerIdentity {
                principal: component("conformance"),
                root: component("handler-failure"),
                function: component("main"),
                binding: component("from_list"),
            },
            source_format: component("orna-stream-v1"),
            source: component("fixture:failure"),
            partition_format: component("literal-list"),
            partition: component("default"),
            position_format: component("ordinal"),
        }
    }

    #[tokio::test]
    async fn handler_failure_does_not_advance_literal_list_checkpoint() {
        let (_temp, repository) = repository();
        let state = RuntimeState::open(&repository, identity(), [24; 32])
            .await
            .expect("runtime");
        let writer = state.acquire_lease([23; 16]).await.expect("writer lease");
        let key = key();
        let payload = Value::int(1.into()).encode().expect("canonical payload");
        let mut source = ListStreamSource::new(key.clone(), vec![payload]);
        let mut handler = FailingHandler;

        let outcome = state
            .run_stream(
                writer,
                &key,
                &mut source,
                &mut handler,
                &orna_runtime_v1::NeverCancelled,
            )
            .await
            .expect("handler failure is a stream outcome");
        assert!(matches!(
            outcome,
            orna_runtime_v1::StreamRunOutcome::Failed { delivered: 0, .. }
        ));
        assert!(
            state
                .stream_backend(writer)
                .checkpoint_async(&key)
                .await
                .expect("checkpoint")
                .committed
                .is_none(),
            "a failed handler must leave the first literal list item uncommitted"
        );
    }
}
