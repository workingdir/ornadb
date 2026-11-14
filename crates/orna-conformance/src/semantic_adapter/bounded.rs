use super::*;

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
        _: &orna_check::Analysis,
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
    pub(super) environment: Environment,
    pub(super) limits: EvaluatorLimits,
    pub(super) functions: Functions,
    pub(super) nominal_definitions: NominalDefinitions,
}

pub(super) struct BoundedSourceExecutor {
    pub(super) functions: Functions,
    pub(super) nominal_definitions: NominalDefinitions,
    pub(super) limits: EvaluatorLimits,
    pub(super) descriptor: FunctionDescriptor,
    pub(super) evaluator_name: String,
}

impl InvocationExecutor for BoundedSourceExecutor {
    fn execute(&mut self, boundary: &ExecutionBoundary) -> InvocationCompletion<TypedValue> {
        // The supervisor admits arguments and result witnesses first. Retain
        // the independent function/snapshot pin at the evaluator edge too.
        if boundary.function != self.descriptor.identity {
            return InvocationCompletion::OrdinaryFailure(
                AdmissionError::SourceMismatch.diagnostic(),
            );
        }
        let mut arguments = Environment::new();
        for (name, value) in boundary.arguments.entries() {
            let Some(canonical) = value.canonical() else {
                return InvocationCompletion::OrdinaryFailure(
                    AdmissionError::SourceMismatch.diagnostic(),
                );
            };
            let Ok(value) = Value::decode(canonical) else {
                return InvocationCompletion::OrdinaryFailure(
                    AdmissionError::SourceMismatch.diagnostic(),
                );
            };
            arguments.insert(name.into(), value);
        }
        match invoke_named_with_nominals(
            &self.evaluator_name,
            &self.functions,
            &arguments,
            &self.nominal_definitions,
            self.limits,
        ) {
            Ok(value) => match value.encode() {
                Ok(canonical) => InvocationCompletion::Success(TypedValue::public(
                    self.descriptor.result_type.clone(),
                    canonical,
                )),
                Err(_) => InvocationCompletion::OrdinaryFailure(
                    AdmissionError::SourceMismatch.diagnostic(),
                ),
            },
            Err(_) => {
                InvocationCompletion::OrdinaryFailure(AdmissionError::SourceMismatch.diagnostic())
            }
        }
    }
}

impl BoundedEvaluator {
    /// Executes one source-admitted pure function through the synchronous
    /// `sys.invoke` supervisor. The complete descriptor is a pre-pinned
    /// function/snapshot/argument/result witness; disagreement is rejected
    /// before the bounded evaluator sees a value.
    pub fn invoke_through_sys<T>(
        &self,
        supervisor: &RuntimeSupervisor,
        request: AdmissionRequest<T>,
        descriptor: &FunctionDescriptor,
        evaluator_name: &str,
    ) -> Result<InvocationState, AdmissionError> {
        if descriptor != &request.function || !self.functions.contains_key(evaluator_name) {
            return Err(AdmissionError::SourceMismatch);
        }
        let mut executor = BoundedSourceExecutor {
            functions: self.functions.clone(),
            nominal_definitions: self.nominal_definitions.clone(),
            limits: self.limits,
            descriptor: descriptor.clone(),
            evaluator_name: evaluator_name.into(),
        };
        supervisor.run(request, &mut executor)
    }

    /// Starts one source-admitted pure function through the asynchronous
    /// `sys.start` supervisor. The worker owns cloned source state so the
    /// returned handle remains valid after this evaluator call returns.
    pub fn start_through_sys<T>(
        &self,
        supervisor: &RuntimeSupervisor,
        request: AdmissionRequest<T>,
        descriptor: &FunctionDescriptor,
        evaluator_name: &str,
    ) -> Result<InvocationHandle<T>, AdmissionError> {
        if descriptor != &request.function || !self.functions.contains_key(evaluator_name) {
            return Err(AdmissionError::SourceMismatch);
        }
        let executor = BoundedSourceExecutor {
            functions: self.functions.clone(),
            nominal_definitions: self.nominal_definitions.clone(),
            limits: self.limits,
            descriptor: descriptor.clone(),
            evaluator_name: evaluator_name.into(),
        };
        supervisor.start(request, executor)
    }

    pub(super) fn check_scenario_expression(
        &self,
        source: &str,
        environment: &Environment,
        expected: &Value,
    ) -> StageOutcome<Diagnostic> {
        match self.evaluate_scenario_expression(source, environment) {
            Ok(actual) if &actual == expected => StageOutcome::Passed,
            Ok(_) => scenario_mismatch(),
            Err(diagnostic) => StageOutcome::Failed(*diagnostic),
        }
    }

    pub(super) fn evaluate_scenario_expression(
        &self,
        source: &str,
        environment: &Environment,
    ) -> Result<Value, Box<Diagnostic>> {
        // Limits fail closed before parsing: an oversized scenario source can
        // never reach evaluation, matching the evaluator's documented
        // check_source contract for every caller-supplied source.
        if let Err(error) = self.limits.check_source(source) {
            return Err(Box::new(error.diagnostic().clone()));
        }
        let parsed = parse_expression(source);
        if !parsed.is_ok() {
            return evaluate_expression_with_functions(
                source,
                environment,
                &self.functions,
                self.limits,
            )
            .map_err(|error| Box::new(error.diagnostic().clone()));
        }
        evaluate_with_functions_and_nominals(
            &parsed.value,
            environment,
            &self.functions,
            &self.nominal_definitions,
            self.limits,
        )
        .map_err(|error| Box::new(error.diagnostic().clone()))
    }

    pub(super) fn run_control_flow(&self) -> StageOutcome<Diagnostic> {
        let source = "if true { let total = 0; let before = total; for item in [1, 2, 3] { total += item; }; let branch = if total == 6 { total += 1; total } else { -1 }; let selected = case branch { 7: branch, _: -1 }; [before, total, branch, selected] } else { [0, 0, 0, 0] }";
        let expected = Value::new(OvbRaw::Array(vec![
            OvbRaw::Int(0.into()),
            OvbRaw::Int(7.into()),
            OvbRaw::Int(7.into()),
            OvbRaw::Int(7.into()),
        ]))
        .expect("scenario expectation is canonical");

        let reference = Self::new(self.limits);
        let optimized = Self::new(self.limits);
        let reference_value =
            match reference.evaluate_scenario_expression(source, &Environment::new()) {
                Ok(value) => value,
                Err(diagnostic) => return StageOutcome::Failed(*diagnostic),
            };
        let optimized_value =
            match optimized.evaluate_scenario_expression(source, &Environment::new()) {
                Ok(value) => value,
                Err(diagnostic) => return StageOutcome::Failed(*diagnostic),
            };

        if reference_value == expected
            && optimized_value == expected
            && reference_value == optimized_value
        {
            StageOutcome::Passed
        } else {
            scenario_mismatch()
        }
    }

    pub(super) fn run_pipeline_precedence(&self) -> StageOutcome<Diagnostic> {
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

    pub(super) fn run_pipeline_insertion(&self) -> StageOutcome<Diagnostic> {
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

    pub(super) fn run_let_rebinding(&self) -> StageOutcome<Diagnostic> {
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
        let parsed = parse_module(include_str!("../fixtures/let-rebinding-removed-var.orna"));
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
            functions: Functions::new(),
            nominal_definitions: NominalDefinitions::new(),
        }
    }

    #[must_use]
    pub fn with_environment(limits: EvaluatorLimits, environment: Environment) -> Self {
        Self {
            environment,
            limits,
            functions: Functions::new(),
            nominal_definitions: NominalDefinitions::new(),
        }
    }
    /// Produces the server-consumable descriptor from the actual `std.ui.action`
    /// source expression.  The source string remains the provenance input; an
    /// opaque server handle is deliberately allocated by the serving boundary.
    pub fn source_action_descriptor(source: &str) -> Option<(String, String, [u8; 32])> {
        let value =
            evaluate_expression(source, &Environment::new(), EvaluatorLimits::default()).ok()?;
        let OvbRaw::Map(fields) = value.raw() else {
            return None;
        };
        let field = |name: &str| {
            fields
                .iter()
                .find_map(|(key, value)| (key == &OvbRaw::Text(name.into())).then_some(value))
        };
        let OvbRaw::Text(action_id) = field("action_id")? else {
            return None;
        };
        let OvbRaw::Text(input_type) = field("input_type")? else {
            return None;
        };
        if action_id.is_empty() || input_type.is_empty() {
            return None;
        }
        let provenance: [u8; 32] = Sha256::digest(source.as_bytes()).into();
        Some((action_id.clone(), input_type.clone(), provenance))
    }

    /// Creates an independent ephemeral session from the admitted pure namespace.
    /// Session declarations and imports never mutate the retained project.
    pub fn repl_session(&self) -> Result<orna_evaluator::ReplSession, EvaluationError> {
        orna_evaluator::ReplSession::with_bindings(
            self.limits,
            self.environment.clone(),
            self.functions.clone(),
        )
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
        match invoke_named_with_nominals(
            function,
            &self.functions,
            arguments,
            &self.nominal_definitions,
            self.limits,
        ) {
            Ok(_) => StageOutcome::Passed,
            Err(error) => StageOutcome::Failed(error.diagnostic().clone()),
        }
    }

    /// Invokes one retained pure function and returns its canonical result.
    /// This is intentionally limited to the bounded evaluator's explicit
    /// function namespace; it does not provide module loading or host calls.
    pub fn invoke_value_with(
        &self,
        function: &str,
        arguments: &Environment,
    ) -> Result<Value, Box<Diagnostic>> {
        invoke_named_with_nominals(
            function,
            &self.functions,
            arguments,
            &self.nominal_definitions,
            self.limits,
        )
        .map_err(|error| Box::new(error.diagnostic().clone()))
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
            // Standard functions keep their module identity (`std.<module>.<name>`)
            // so their bodies resolve sibling helpers and never shadow user names.
            let namespace = module_namespace(&unit);
            match staged.evaluate_module_with_namespace(&unit, namespace.as_deref()) {
                StageOutcome::Passed => {}
                outcome => return outcome,
            }
        }
        *self = staged;
        StageOutcome::Passed
    }

    pub(super) fn evaluate_unit(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        match unit.parse_as.as_str() {
            "row_unit" | "expression_unit" | "repl_unit" => {
                let parsed = parse_expression(&unit.source);
                if !parsed.is_ok() {
                    return match evaluate_expression_with_functions(
                        &unit.source,
                        &self.environment,
                        &self.functions,
                        self.limits,
                    ) {
                        Ok(_) => scenario_mismatch(),
                        Err(error) => StageOutcome::Failed(error.diagnostic().clone()),
                    };
                }
                match evaluate_with_functions_and_nominals(
                    &parsed.value,
                    &self.environment,
                    &self.functions,
                    &self.nominal_definitions,
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

    pub(super) fn evaluate_module(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.evaluate_module_with_namespace(unit, None)
    }

    pub(super) fn evaluate_module_with_namespace(
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
        let mut nominal_definitions = self.nominal_definitions.clone();
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
                Declaration::Type {
                    name,
                    representation: TypeRepresentation::Nominal { members },
                    ..
                } => {
                    let identity = qualified_nominal_name(namespace, &name);
                    let fields = members
                        .into_iter()
                        .filter_map(|member| match member {
                            TypeMember::Field {
                                visibility,
                                name,
                                initializer,
                                ..
                            } => Some(match (visibility, initializer) {
                                (true, Some(default)) => NominalField::public_with_default(
                                    nominal_field_id(&identity, &name),
                                    name,
                                    default,
                                ),
                                (true, None) => {
                                    NominalField::public(nominal_field_id(&identity, &name), name)
                                }
                                (false, Some(default)) => NominalField::private_with_default(
                                    nominal_field_id(&identity, &name),
                                    name,
                                    default,
                                ),
                                (false, None) => {
                                    NominalField::private(nominal_field_id(&identity, &name), name)
                                }
                            }),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    nominal_definitions.insert(
                        identity.clone(),
                        NominalDefinition::new(
                            nominal_type_id(&identity),
                            namespace.map(str::to_owned),
                            fields,
                        ),
                    );
                }
                Declaration::Use { .. } => {}
                _ => return Self::unsupported_module(),
            }
        }
        self.functions = functions;
        self.nominal_definitions = nominal_definitions;
        StageOutcome::Passed
    }

    pub(super) fn supports_pure_module(unit: &SourceUnit) -> bool {
        let parsed = parse_module(&unit.source);
        parsed.is_ok() && Self::supports_pure_declarations(&parsed.value.items)
    }

    pub(super) fn supports_pure_declarations(items: &[orna_syntax_ast::Item]) -> bool {
        items.iter().all(|item| match &item.declaration {
            Declaration::Function { .. } | Declaration::Use { .. } => true,
            Declaration::Type {
                representation: TypeRepresentation::Nominal { members },
                ..
            } => members
                .iter()
                .all(|member| matches!(member, TypeMember::Field { .. })),
            _ => false,
        })
    }

    pub(super) fn supports_pure_project(project: &ProjectUnit) -> bool {
        let environment = &project.expectations.environment;
        !environment.network
            && !environment.credentials
            && environment.intrinsics == "Orna 1.1.0 core"
            && environment.stdlib.is_none()
            && environment.initial_tables == "empty"
            && project.modules.iter().all(Self::supports_pure_module)
    }

    pub(super) fn unsupported_module() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "bounded module loading accepts function declarations only; other declarations require the integrated runtime".into(),
        }
    }

    pub(super) fn unsupported_project() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "project execution requires an offline empty-state project whose modules contain only function declarations; tables, effects, and streams require the integrated runtime".into(),
        }
    }
}

pub(super) fn standard_profile_diagnostic() -> Diagnostic {
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
        if let Err(outcome) = bind_standard_function_aliases(&mut staged.functions, project) {
            return outcome;
        }
        *self = staged;
        StageOutcome::Passed
    }

    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "row validation requires a resolved table schema and path-derived key; expression evaluation alone is not row validation".into(),
        }
    }

    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
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
        analysis: &orna_check::Analysis,
    ) -> StageOutcome<Diagnostic> {
        // Placeholder identity: ProjectUnit carries no database id or snapshot
        // tables, so every 60021 stored reference is checked against an empty
        // table set. Tracked by bead ornadb-0lu1 (thread real identity).
        let identity = crate::row_admission::RowAdmissionIdentity {
            database_id: [0; 16],
            tables: std::collections::BTreeMap::new(),
        };
        match admit_project_rows(project, analysis, &identity, self.limits) {
            Ok(_) => StageOutcome::Passed,
            Err(diagnostic) => StageOutcome::Failed(*diagnostic),
        }
    }

    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        if control_flow_contract(scenario) {
            return self.run_control_flow();
        }
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

/// Binds unqualified names from `use std.<module>.{name}` declarations in the
/// project's modules to the retained standard functions. User functions keep
/// their own names. Glob and module-alias imports are refused explicitly, a
/// braced name absent from the pinned module is an import error, and two
/// modules binding one unqualified name to different targets are ambiguous.
pub(super) fn bind_standard_function_aliases(
    functions: &mut Functions,
    project: &ProjectUnit,
) -> Result<(), StageOutcome<Diagnostic>> {
    let mut aliases = BTreeMap::<String, String>::new();
    for unit in &project.modules {
        let parsed = parse_module(&unit.source);
        for item in parsed.value.items {
            let Declaration::Use { path, tail } = item.declaration else {
                continue;
            };
            if path.first().map(|segment| segment.name.as_str()) != Some("std") {
                continue;
            }
            let namespace = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let names = match tail {
                orna_syntax_ast::UseTail::None => continue,
                orna_syntax_ast::UseTail::Names(names) => names,
                orna_syntax_ast::UseTail::Glob { .. } => {
                    return Err(StageOutcome::Failed(standard_import_diagnostic(
                        DIAG_IMPORT,
                        "standard glob imports are not supported by the bounded runtime; import names explicitly",
                    )));
                }
                orna_syntax_ast::UseTail::Alias { .. } => {
                    return Err(StageOutcome::Failed(standard_import_diagnostic(
                        DIAG_IMPORT,
                        "standard module aliases are not supported by the bounded runtime; import names explicitly",
                    )));
                }
            };
            for name in names {
                let target = format!("{namespace}.{}", name.name);
                if !functions.contains_key(&target) {
                    return Err(StageOutcome::Failed(standard_import_diagnostic(
                        DIAG_IMPORT,
                        "imported name is not exported by the pinned standard module",
                    )));
                }
                if let Some(previous) = aliases.insert(name.name.clone(), target.clone())
                    && previous != target
                {
                    return Err(StageOutcome::Failed(standard_import_diagnostic(
                        DIAG_AMBIGUOUS,
                        "standard imports bind one unqualified name to different functions",
                    )));
                }
            }
        }
    }
    for (alias, target) in aliases {
        if let Some(function) = functions.get(&target).cloned()
            && !functions.contains_key(&alias)
        {
            functions.insert(alias, function);
        }
    }
    Ok(())
}

pub(super) fn standard_import_diagnostic(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(
        SafeText::new(code).expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new(message).expect("static message"),
    )
    .expect("valid diagnostic")
    .redacted()
}

pub(super) fn module_namespace(unit: &SourceUnit) -> Option<String> {
    let mut components = unit.source_id.split('/').collect::<Vec<_>>();
    let file = components.pop()?;
    let stem = file.strip_suffix(".orna")?;
    if components.is_empty() && stem == "main" {
        return None;
    }
    components.push(stem);
    Some(components.join("."))
}

pub(super) fn qualified_nominal_name(namespace: Option<&str>, name: &str) -> String {
    namespace
        .filter(|namespace| !namespace.is_empty())
        .map(|namespace| format!("{namespace}.{name}"))
        .unwrap_or_else(|| name.to_owned())
}

pub(super) fn nominal_type_id(identity: &str) -> [u8; 16] {
    Sha256::digest(identity.as_bytes())[..16]
        .try_into()
        .expect("truncated digest has the ObjectId width")
}

pub(super) fn nominal_field_id(identity: &str, name: &str) -> [u8; 16] {
    let mut input = Vec::with_capacity(identity.len() + name.len() + 1);
    input.extend_from_slice(identity.as_bytes());
    input.push(0);
    input.extend_from_slice(name.as_bytes());
    Sha256::digest(input)[..16]
        .try_into()
        .expect("truncated digest has the ObjectId width")
}
