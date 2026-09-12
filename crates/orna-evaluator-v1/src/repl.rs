//! Ephemeral evaluator-owned REPL state.

use std::collections::{BTreeMap, BTreeSet};

use orna_foundation_v1::CanonicalValue;
use orna_syntax_v1::{
    AssignmentTarget, Declaration, Expr, Pattern, ReplInput, Statement, UseTail, parse_repl,
};
use orna_value_v1::Raw;

use crate::{
    CancellationToken, Context, Environment, EvaluationError, Functions, Limits, PureFunction,
    Scope, bind, error, pattern_names, validate_limits,
};

/// A bounded ephemeral module for one interactive Orna session.
///
/// The supplied environment and functions are already admitted by the caller.
/// `use` only makes aliases of those bindings; it never loads a project or
/// reaches into the host process.
#[derive(Clone, Debug)]
pub struct ReplSession {
    limits: Limits,
    environment: Environment,
    functions: Functions,
    aliases: BTreeMap<String, String>,
    wildcard_bindings: BTreeMap<String, String>,
    wildcard_ambiguities: BTreeMap<String, BTreeSet<String>>,
    session_functions: BTreeSet<String>,
    namespace_bindings: BTreeSet<String>,
    last_success: Option<CanonicalValue>,
    last_status: Option<CanonicalValue>,
}

/// Parses one REPL input after applying source bounds. Callers which also
/// typecheck the input retain this AST and pass it to
/// [`ReplSession::submit_admitted`]; they must not parse the source a second
/// time.
pub fn parse_admitted_repl(source: &str, limits: Limits) -> Result<ReplInput, EvaluationError> {
    limits.check_source(source)?;
    let parsed = parse_repl(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    Ok(parsed.value)
}

impl ReplSession {
    /// Starts an empty session with no admitted project bindings.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            environment: Environment::new(),
            functions: Functions::new(),
            aliases: BTreeMap::new(),
            wildcard_bindings: BTreeMap::new(),
            wildcard_ambiguities: BTreeMap::new(),
            session_functions: BTreeSet::new(),
            namespace_bindings: BTreeSet::new(),
            last_success: None,
            last_status: None,
        }
    }

    /// Starts a session against explicitly admitted values and pure functions.
    pub fn with_bindings(
        limits: Limits,
        environment: Environment,
        functions: Functions,
    ) -> Result<Self, EvaluationError> {
        validate_limits(limits)?;
        limits.check_items(
            environment
                .len()
                .checked_add(functions.len())
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?,
        )?;
        for name in environment.keys().chain(functions.keys()) {
            if matches!(name.as_str(), "$_" | "$?")
                || name.len() > limits.max_string_bytes
                || name.split('.').count() > limits.max_depth
            {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
        }
        Ok(Self {
            limits,
            environment,
            functions,
            aliases: BTreeMap::new(),
            wildcard_bindings: BTreeMap::new(),
            wildcard_ambiguities: BTreeMap::new(),
            session_functions: BTreeSet::new(),
            namespace_bindings: BTreeSet::new(),
            last_success: None,
            last_status: None,
        })
    }

    /// Submits one REPL expression or declaration. Declarations return `None`.
    /// Failed submissions leave every retained binding and the last result intact.
    pub fn submit(&mut self, source: &str) -> Result<Option<CanonicalValue>, EvaluationError> {
        let parsed = parse_admitted_repl(source, self.limits)?;
        self.submit_unchecked(parsed)
    }

    /// Executes an already parsed input at a trusted runtime boundary.
    ///
    /// This does not perform type, import, or effect admission. Callers must
    /// place it behind a trusted checker such as `AdmittedReplSession`; raw
    /// evaluator use is not the typed REPL API. This deliberately receives the
    /// original AST: annotations remain attached and source is not fabricated
    /// or reparsed at the runtime boundary.
    pub fn submit_admitted(
        &mut self,
        input: &ReplInput,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let mut candidate = self.clone();
        let result = candidate.submit_checked(input, None)?;
        *self = candidate;
        Ok(result)
    }

    /// Executes one already-admitted input with an explicit operation token.
    /// The candidate is published only after evaluation and the final
    /// cancellation fence both succeed.
    pub fn submit_admitted_with_cancellation(
        &mut self,
        input: &ReplInput,
        cancellation: &CancellationToken,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        cancellation.check()?;
        let mut candidate = self.clone();
        let result = candidate.submit_checked(input, Some(cancellation))?;
        cancellation.check()?;
        *self = candidate;
        Ok(result)
    }

    /// Publishes the typed REPL's redacted execution status. This is kept
    /// separate from ordinary bindings so module source cannot capture it.
    pub(crate) fn set_last_status(&mut self, status: CanonicalValue) {
        self.last_status = Some(status);
    }

    fn submit_unchecked(
        &mut self,
        parsed: ReplInput,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        match parsed {
            ReplInput::Expression(expression) => {
                let value = self.evaluate(&expression, None)?;
                self.last_success = Some(value.clone());
                Ok(Some(value))
            }
            ReplInput::Item(item) => {
                let mut candidate = self.clone();
                candidate.declare(item.declaration)?;
                *self = candidate;
                Ok(None)
            }
        }
    }

    fn submit_checked(
        &mut self,
        input: &ReplInput,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        match input {
            ReplInput::Expression(expression) => {
                let value = self.evaluate(expression, cancellation)?;
                self.last_success = Some(value.clone());
                Ok(Some(value))
            }
            ReplInput::Item(item) => {
                self.declare_admitted(&item.declaration, cancellation)?;
                Ok(None)
            }
        }
    }

    /// Evaluates an expression against session state without changing it.
    pub fn preview(&self, source: &str) -> Result<CanonicalValue, EvaluationError> {
        let parsed = parse_admitted_repl(source, self.limits)?;
        let ReplInput::Expression(expression) = parsed else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        self.evaluate(&expression, None)
    }

    fn evaluate(
        &self,
        expression: &Expr,
        cancellation: Option<&CancellationToken>,
    ) -> Result<CanonicalValue, EvaluationError> {
        self.reject_ambiguous_expression(expression)?;
        let mut environment = self.environment.clone();
        if let Some(value) = &self.last_success {
            environment.insert("$_".into(), value.clone());
        }
        environment.insert(
            "$?".into(),
            self.last_status.clone().unwrap_or_else(|| {
                CanonicalValue::new(orna_foundation_v1::OvbRaw::Null).expect("null is canonical")
            }),
        );
        let mut context = Context {
            limits: self.limits,
            steps: 0,
            functions: &self.functions,
            aliases: Some(&self.aliases),
            session_functions: Some(&self.session_functions),
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: None,
            namespace: None,
            transfer: None,
            cancellation,
        };
        context.items(self.functions.len())?;
        let mut scope = Scope::from_environment(&environment, &mut context)?;
        scope.2.extend(self.namespace_bindings.iter().cloned());
        let value = context.evaluate(expression, &mut scope, 0)?;
        if context.transfer.is_some() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        value.canonical()
    }

    fn declare(&mut self, declaration: Declaration) -> Result<(), EvaluationError> {
        match &declaration {
            Declaration::Let { annotation, .. } => {
                if annotation.is_some() {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
            }
            Declaration::Function { signature, .. } => {
                if !signature.generics.is_empty()
                    || signature.result.is_some()
                    || signature
                        .parameters
                        .iter()
                        .any(|parameter| parameter.annotation.is_some())
                {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
            }
            Declaration::Use { .. } => {}
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
        self.declare_admitted(&declaration, None)
    }

    fn declare_admitted(
        &mut self,
        declaration: &Declaration,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        match declaration {
            Declaration::Use { path, tail } => self.import(
                &path
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                tail.clone(),
            ),
            Declaration::Let { pattern, value, .. } => {
                self.let_binding(pattern, value, cancellation)
            }
            Declaration::Function { signature, body } => {
                // Generic execution has no bounded runtime implementation.
                // Keep it explicit rather than silently dropping its type
                // parameters while retaining a superficially successful fn.
                if !signature.generics.is_empty() {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
                self.remove_wildcard_binding(&signature.name);
                if self.binding_taken(&signature.name) {
                    return Err(error("ORNA-EVAL-NAME"));
                }
                let mut shadowed = BTreeSet::new();
                for parameter in &signature.parameters {
                    shadowed.extend(pattern_names(&parameter.pattern));
                }
                self.reject_ambiguous_expression_with_scope(body, &shadowed)?;
                let name = signature.name.clone();
                let mut environment = self.environment.clone();
                if let Some(last_success) = &self.last_success {
                    environment.insert("$_".into(), last_success.clone());
                }
                self.functions.insert(
                    name.clone(),
                    PureFunction {
                        parameters: signature.parameters.clone(),
                        body: body.clone(),
                        // A retained REPL function is an immutable lexical
                        // closure. Capture the current last result now rather
                        // than consulting whichever `$_` its caller has.
                        environment,
                    },
                );
                self.aliases.insert(name.clone(), name);
                self.session_functions.insert(signature.name.clone());
                self.check_retained()?;
                if let Some(cancellation) = cancellation {
                    cancellation.check()?;
                }
                Ok(())
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn let_binding(
        &mut self,
        pattern: &Pattern,
        expression: &Expr,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        self.reject_ambiguous_expression(expression)?;
        let mut environment = self.environment.clone();
        if let Some(value) = &self.last_success {
            environment.insert("$_".into(), value.clone());
        }
        let mut context = Context {
            limits: self.limits,
            steps: 0,
            functions: &self.functions,
            aliases: Some(&self.aliases),
            session_functions: Some(&self.session_functions),
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: None,
            namespace: None,
            transfer: None,
            cancellation,
        };
        let mut scope = Scope::from_environment(&environment, &mut context)?;
        scope.2.extend(self.namespace_bindings.iter().cloned());
        let value = context.evaluate(expression, &mut scope, 0)?;
        if context.transfer.is_some() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let names = pattern_names(pattern);
        let matched = bind(pattern, value, &mut scope, &context, 1)?;
        drop(context);
        if !matched {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        scope.0.remove("$_");
        for name in names {
            self.release_wildcard_for_local(&name);
            self.namespace_bindings.remove(&name);
        }
        self.environment = scope
            .0
            .into_iter()
            .map(|(name, value)| value.canonical().map(|value| (name, value)))
            .collect::<Result<_, _>>()?;
        self.check_retained()?;
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        Ok(())
    }

    fn import(&mut self, path: &str, tail: UseTail) -> Result<(), EvaluationError> {
        match tail {
            UseTail::Alias { name, .. } => {
                if name == "_" {
                    self.import_glob(path)
                } else {
                    self.import_alias(path, &name)
                }
            }
            UseTail::None => self.import_alias(path, path.rsplit('.').next().unwrap_or(path)),
            UseTail::Glob { .. } => self.import_glob(path),
            UseTail::Names(names) => {
                for name in names {
                    self.import_alias(&format!("{path}.{}", name.name), &name.name)?;
                }
                Ok(())
            }
        }
    }

    fn import_alias(&mut self, canonical: &str, alias: &str) -> Result<(), EvaluationError> {
        self.remove_wildcard_binding(alias);
        if self.functions.contains_key(canonical) {
            if self.binding_taken(alias) {
                return Err(error("ORNA-EVAL-NAME"));
            }
            self.aliases.insert(alias.into(), canonical.into());
            return self.check_retained();
        }
        if let Some(value) = self.environment.get(canonical).cloned() {
            if self.binding_taken(alias) {
                return Err(error("ORNA-EVAL-NAME"));
            }
            self.environment.insert(alias.into(), value);
            return self.check_retained();
        }
        self.import_module_alias(canonical, alias)
    }

    fn import_module_alias(&mut self, canonical: &str, alias: &str) -> Result<(), EvaluationError> {
        let prefix = format!("{canonical}.");
        let aliases = self
            .functions
            .keys()
            .filter_map(|name| {
                name.strip_prefix(&prefix)
                    .map(|suffix| (format!("{alias}.{suffix}"), name.clone()))
            })
            .collect::<Vec<_>>();
        let values = self
            .environment
            .iter()
            .filter_map(|(name, value)| {
                name.strip_prefix(&prefix).map(|suffix| {
                    (
                        suffix.split('.').map(str::to_owned).collect::<Vec<_>>(),
                        value.raw().clone(),
                    )
                })
            })
            .collect::<Vec<_>>();
        if aliases.is_empty() && values.is_empty() {
            return Err(error("ORNA-EVAL-NAME"));
        }
        if aliases
            .iter()
            .any(|(name, target)| self.alias_taken(name, target))
        {
            return Err(error("ORNA-EVAL-NAME"));
        }
        let namespace = (!values.is_empty())
            .then(|| namespace_value(values))
            .transpose()?;
        if namespace.is_some() && self.binding_taken(alias) {
            return Err(error("ORNA-EVAL-NAME"));
        }
        for (name, target) in aliases {
            self.aliases.insert(name, target);
        }
        if let Some(value) = namespace {
            self.environment.insert(alias.into(), value);
            self.namespace_bindings.insert(alias.into());
        }
        self.check_retained()
    }

    fn import_glob(&mut self, path: &str) -> Result<(), EvaluationError> {
        let prefix = format!("{path}.");
        let functions = self
            .functions
            .keys()
            .filter_map(|name| {
                name.strip_prefix(&prefix)
                    .filter(|suffix| !suffix.contains('.'))
                    .map(|suffix| (suffix.to_owned(), name.clone()))
            })
            .collect::<Vec<_>>();
        let values = self
            .environment
            .iter()
            .filter_map(|(name, value)| {
                name.strip_prefix(&prefix)
                    .filter(|suffix| !suffix.contains('.'))
                    .map(|suffix| (suffix.to_owned(), value.clone()))
            })
            .collect::<Vec<_>>();
        if functions.is_empty() && values.is_empty() {
            return Err(error("ORNA-EVAL-NAME"));
        }
        for (alias, canonical) in functions {
            self.import_wildcard_alias(path, &canonical, &alias)?;
        }
        for (alias, value) in values {
            self.import_wildcard_value(path, alias, value)?;
        }
        self.check_retained()
    }

    fn import_wildcard_alias(
        &mut self,
        path: &str,
        canonical: &str,
        alias: &str,
    ) -> Result<(), EvaluationError> {
        if !self.prepare_wildcard_binding(path, alias) {
            return Ok(());
        }
        self.aliases.insert(alias.into(), canonical.into());
        self.wildcard_bindings.insert(alias.into(), path.into());
        Ok(())
    }

    fn import_wildcard_value(
        &mut self,
        path: &str,
        alias: String,
        value: CanonicalValue,
    ) -> Result<(), EvaluationError> {
        if !self.prepare_wildcard_binding(path, &alias) {
            return Ok(());
        }
        self.environment.insert(alias.clone(), value);
        self.wildcard_bindings.insert(alias, path.into());
        Ok(())
    }

    fn prepare_wildcard_binding(&mut self, path: &str, alias: &str) -> bool {
        if let Some(candidates) = self.wildcard_ambiguities.get_mut(alias) {
            candidates.insert(path.into());
            return false;
        }
        if let Some(existing_path) = self.wildcard_bindings.get(alias).cloned() {
            if existing_path == path {
                return false;
            }
            self.wildcard_bindings.remove(alias);
            self.aliases.remove(alias);
            self.environment.remove(alias);
            self.wildcard_ambiguities
                .insert(alias.into(), BTreeSet::from([existing_path, path.into()]));
            return false;
        }
        !self.binding_taken(alias)
    }

    fn remove_wildcard_binding(&mut self, name: &str) {
        if self.wildcard_bindings.remove(name).is_some() {
            self.aliases.remove(name);
            self.environment.remove(name);
        }
        self.wildcard_ambiguities.remove(name);
    }

    fn release_wildcard_for_local(&mut self, name: &str) {
        if self.wildcard_bindings.remove(name).is_some() {
            self.aliases.remove(name);
        }
        self.wildcard_ambiguities.remove(name);
    }

    fn binding_taken(&self, name: &str) -> bool {
        self.environment.contains_key(name)
            || self.functions.contains_key(name)
            || self.aliases.contains_key(name)
            || matches!(name, "$_" | "$?")
    }

    fn alias_taken(&self, name: &str, target: &str) -> bool {
        self.environment.contains_key(name)
            || self.aliases.contains_key(name)
            || (self.functions.contains_key(name) && name != target)
            || matches!(name, "$_" | "$?")
    }

    fn check_retained(&self) -> Result<(), EvaluationError> {
        let count = self
            .environment
            .len()
            .checked_add(self.functions.len())
            .and_then(|count| count.checked_add(self.aliases.len()))
            .and_then(|count| {
                self.wildcard_ambiguities
                    .values()
                    .try_fold(count, |count, candidates| {
                        count.checked_add(candidates.len())
                    })
            })
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.limits.check_items(count)
    }

    fn reject_ambiguous_expression(&self, expression: &Expr) -> Result<(), EvaluationError> {
        self.reject_ambiguous_expression_with_scope(expression, &BTreeSet::new())
    }

    fn reject_ambiguous_expression_with_scope(
        &self,
        expression: &Expr,
        shadowed: &BTreeSet<String>,
    ) -> Result<(), EvaluationError> {
        if ambiguous_expr(expression, &self.wildcard_ambiguities, shadowed) {
            Err(error("ORNA-EVAL-AMBIGUOUS"))
        } else {
            Ok(())
        }
    }
}

fn ambiguous_expr(
    expression: &Expr,
    ambiguities: &BTreeMap<String, BTreeSet<String>>,
    shadowed: &BTreeSet<String>,
) -> bool {
    match expression {
        Expr::Name { text, .. } => ambiguities.contains_key(text) && !shadowed.contains(text),
        Expr::Literal { .. } | Expr::ReplBinding { .. } => false,
        Expr::InterpolatedString { segments, .. } => segments.iter().any(|segment| {
            matches!(segment, orna_syntax_v1::StringSegment::Expression { value, .. }
                if ambiguous_expr(value, ambiguities, shadowed))
        }),
        Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
            ambiguous_expr(rhs, ambiguities, shadowed)
        }
        Expr::Binary { lhs, rhs, .. } => {
            ambiguous_expr(lhs, ambiguities, shadowed) || ambiguous_expr(rhs, ambiguities, shadowed)
        }
        Expr::Range { lower, upper, .. } => {
            lower
                .as_deref()
                .is_some_and(|value| ambiguous_expr(value, ambiguities, shadowed))
                || upper
                    .as_deref()
                    .is_some_and(|value| ambiguous_expr(value, ambiguities, shadowed))
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            ambiguous_expr(callee, ambiguities, shadowed)
                || arguments
                    .iter()
                    .any(|argument| ambiguous_expr(&argument.value, ambiguities, shadowed))
        }
        Expr::Index { base, index, .. } => {
            ambiguous_expr(base, ambiguities, shadowed)
                || ambiguous_expr(index, ambiguities, shadowed)
        }
        Expr::Field { base, .. } => ambiguous_expr(base, ambiguities, shadowed),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => elements
            .iter()
            .any(|element| ambiguous_expr(element, ambiguities, shadowed)),
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => fields
            .iter()
            .any(|field| ambiguous_expr(&field.value, ambiguities, shadowed)),
        Expr::Lambda {
            parameters, body, ..
        } => {
            let mut nested = shadowed.clone();
            for parameter in parameters {
                nested.extend(pattern_names(&parameter.pattern));
            }
            ambiguous_expr(body, ambiguities, &nested)
        }
        Expr::Block {
            statements, tail, ..
        } => {
            let mut nested = shadowed.clone();
            for statement in statements {
                if ambiguous_statement(statement, ambiguities, &nested) {
                    return true;
                }
                if let Statement::Let { pattern, .. } = statement {
                    nested.extend(pattern_names(pattern));
                }
            }
            tail.as_deref()
                .is_some_and(|tail| ambiguous_expr(tail, ambiguities, &nested))
        }
        Expr::Control {
            kind,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            if condition
                .as_deref()
                .is_some_and(|value| ambiguous_expr(value, ambiguities, shadowed))
                || alternate
                    .as_deref()
                    .is_some_and(|value| ambiguous_expr(value, ambiguities, shadowed))
            {
                return true;
            }
            let mut body_scope = shadowed.clone();
            if *kind == orna_syntax_v1::ControlKind::For
                && let Some(binding) = binding
            {
                body_scope.extend(pattern_names(binding));
            }
            if body
                .as_deref()
                .is_some_and(|value| ambiguous_expr(value, ambiguities, &body_scope))
            {
                return true;
            }
            arms.iter().any(|arm| {
                let mut arm_scope = shadowed.clone();
                arm_scope.extend(pattern_names(&arm.pattern));
                arm.guard
                    .as_ref()
                    .is_some_and(|guard| ambiguous_expr(guard, ambiguities, &arm_scope))
                    || ambiguous_expr(&arm.body, ambiguities, &arm_scope)
            })
        }
    }
}

fn ambiguous_statement(
    statement: &Statement,
    ambiguities: &BTreeMap<String, BTreeSet<String>>,
    shadowed: &BTreeSet<String>,
) -> bool {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. } => ambiguous_expr(value, ambiguities, shadowed),
        Statement::Return { value, .. } | Statement::Break { value, .. } => value
            .as_ref()
            .is_some_and(|value| ambiguous_expr(value, ambiguities, shadowed)),
        Statement::Continue { .. } => false,
        Statement::Assignment { target, value, .. } => {
            ambiguous_target(target, ambiguities, shadowed)
                || ambiguous_expr(value, ambiguities, shadowed)
        }
    }
}

fn ambiguous_target(
    target: &AssignmentTarget,
    ambiguities: &BTreeMap<String, BTreeSet<String>>,
    shadowed: &BTreeSet<String>,
) -> bool {
    match target {
        AssignmentTarget::Name { name, .. } => {
            ambiguities.contains_key(name) && !shadowed.contains(name)
        }
        AssignmentTarget::Field { base, .. } => ambiguous_target(base, ambiguities, shadowed),
        AssignmentTarget::Index { base, index, .. } => {
            ambiguous_target(base, ambiguities, shadowed)
                || ambiguous_expr(index, ambiguities, shadowed)
        }
    }
}

fn namespace_value(entries: Vec<(Vec<String>, Raw)>) -> Result<CanonicalValue, EvaluationError> {
    let mut fields = BTreeMap::new();
    let mut children: BTreeMap<String, Vec<(Vec<String>, Raw)>> = BTreeMap::new();
    for (mut path, value) in entries {
        let Some(name) = (!path.is_empty()).then(|| path.remove(0)) else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        if path.is_empty() {
            if fields.insert(name, value).is_some() {
                return Err(error("ORNA-EVAL-VALUE"));
            }
        } else {
            children.entry(name).or_default().push((path, value));
        }
    }
    for (name, values) in children {
        if fields
            .insert(name, namespace_value(values)?.raw().clone())
            .is_some()
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
    }
    let mut fields = fields.into_iter().collect::<Vec<_>>();
    fields.sort_by(|(left, _), (right, _)| {
        left.len().cmp(&right.len()).then_with(|| left.cmp(right))
    });
    CanonicalValue::new(Raw::Map(
        fields
            .into_iter()
            .map(|(name, value)| (Raw::Text(name), value))
            .collect(),
    ))
    .map_err(|_| error("ORNA-EVAL-VALUE"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_syntax_v1::parse_module;
    use orna_value_v1::Value;

    fn library_functions() -> Functions {
        let parsed = parse_module(
            "fn twice(value) = add(value, value); fn add(left, right) = left + right;",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let mut functions: Functions = parsed
            .value
            .items
            .into_iter()
            .map(|item| {
                let Declaration::Function { signature, body } = item.declaration else {
                    panic!("function expected")
                };
                (
                    format!("library.{}", signature.name),
                    PureFunction {
                        parameters: signature.parameters,
                        body,
                        environment: Environment::new(),
                    },
                )
            })
            .collect();
        let other = parse_module("fn add(left, right) = left + right + 100;");
        assert!(other.is_ok(), "{:?}", other.diagnostics);
        let Declaration::Function { signature, body } = other
            .value
            .items
            .into_iter()
            .next()
            .expect("function")
            .declaration
        else {
            panic!("function expected")
        };
        functions.insert(
            "other.add".into(),
            PureFunction {
                parameters: signature.parameters,
                body,
                environment: Environment::new(),
            },
        );
        functions
    }

    #[test]
    fn alias_calls_keep_the_admitted_function_namespace() {
        let mut session =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");

        assert_eq!(
            session.submit("use library.twice as double;").unwrap(),
            None
        );
        assert_eq!(
            session.submit("double(21)").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(
            session.submit("library.twice(21)").unwrap_err().code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }

    #[test]
    fn module_alias_maps_admitted_descendants_without_host_lookup() {
        let mut session =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(session.submit("use library as lib;").unwrap(), None);
        assert_eq!(
            session.submit("lib.twice(21)").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(session.submit("let lib = { twice: 0 };").unwrap(), None);
        assert_eq!(
            session.submit("lib.twice(21)").unwrap_err().code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }

    #[test]
    fn ordinary_module_use_admits_its_qualified_function_names() {
        let mut session =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(session.submit("use library;").unwrap(), None);
        assert_eq!(
            session.submit("library.twice(21)").unwrap(),
            Some(Value::int(42.into()))
        );
    }

    #[test]
    fn wildcard_import_collisions_are_ambiguous_and_transactional() {
        let mut library_first =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(library_first.submit("use library.*;").unwrap(), None);
        assert_eq!(library_first.submit("use other.*;").unwrap(), None);
        assert_eq!(
            library_first.submit("add(1, 2)").unwrap_err().code(),
            "ORNA-EVAL-AMBIGUOUS"
        );
        assert_eq!(
            library_first.submit("twice(21)").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(library_first.submit("use library.add;").unwrap(), None);
        assert_eq!(
            library_first.submit("add(1, 2)").unwrap(),
            Some(Value::int(3.into()))
        );

        let mut other_first =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(other_first.submit("use other.*;").unwrap(), None);
        assert_eq!(other_first.submit("use library.*;").unwrap(), None);
        assert_eq!(
            other_first.submit("add(1, 2)").unwrap_err().code(),
            "ORNA-EVAL-AMBIGUOUS"
        );
        assert_eq!(
            other_first.submit("twice(21)").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(other_first.submit("use other.add;").unwrap(), None);
        assert_eq!(
            other_first.submit("add(1, 2)").unwrap(),
            Some(Value::int(103.into()))
        );
    }

    #[test]
    fn local_and_explicit_bindings_take_precedence_over_wildcard_imports() {
        let mut local =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(local.submit("let add = 42;").unwrap(), None);
        assert_eq!(local.submit("use library.*;").unwrap(), None);
        assert_eq!(local.submit("use other.*;").unwrap(), None);
        assert_eq!(local.submit("add").unwrap(), Some(Value::int(42.into())));

        let mut explicit =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(explicit.submit("use library.*;").unwrap(), None);
        assert_eq!(explicit.submit("use other.add;").unwrap(), None);
        assert_eq!(explicit.submit("use library.*;").unwrap(), None);
        assert_eq!(
            explicit.submit("add(1, 2)").unwrap(),
            Some(Value::int(103.into()))
        );
    }

    #[test]
    fn ambiguous_let_initializers_fail_before_deferred_evaluation() {
        let mut session =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(session.submit("use library.*;").unwrap(), None);
        assert_eq!(session.submit("use other.*;").unwrap(), None);

        for source in [
            "let value = add(1, 2);",
            "let f = () => add(1, 2);",
            "let f = () => { let nested = () => add(1, 2); nested() };",
        ] {
            assert_eq!(
                session.submit(source).unwrap_err().code(),
                "ORNA-EVAL-AMBIGUOUS",
                "{source}"
            );
        }
        assert_eq!(
            session.submit("twice(21)").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(
            session.submit("let value = (add => add + 1)(41);").unwrap(),
            None
        );
        assert_eq!(
            session.submit("value").unwrap(),
            Some(Value::int(42.into()))
        );
    }

    #[test]
    fn module_alias_exposes_admitted_environment_values() {
        let mut session = ReplSession::with_bindings(
            Limits::default(),
            Environment::from([("library.answer".into(), Value::int(42.into()))]),
            library_functions(),
        )
        .expect("admitted library value");
        assert_eq!(session.submit("use library as lib;").unwrap(), None);
        assert_eq!(
            session.submit("lib.answer").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(
            session.submit("lib.twice(21)").unwrap(),
            Some(Value::int(42.into()))
        );
    }

    #[test]
    fn session_aliases_cannot_hijack_an_admitted_function_sibling() {
        let mut session =
            ReplSession::with_bindings(Limits::default(), Environment::new(), library_functions())
                .expect("admitted library");
        assert_eq!(session.submit("use other.add;").unwrap(), None);
        assert_eq!(
            session.submit("use library.twice as double;").unwrap(),
            None
        );
        assert_eq!(
            session.submit("double(21)").unwrap(),
            Some(Value::int(42.into()))
        );
    }

    #[test]
    fn declarations_results_and_failures_are_transactional() {
        let mut session = ReplSession::new(Limits::default());
        assert_eq!(session.submit("let n = 20;").unwrap(), None);
        assert_eq!(
            session.submit("fn twice(value) = value + value;").unwrap(),
            None
        );
        assert_eq!(
            session.submit("twice(n + 1)").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(session.submit("$_").unwrap(), Some(Value::int(42.into())));
        assert_eq!(session.submit("fn last() = $_;").unwrap(), None);
        assert_eq!(session.submit("fn echo(value) = value;").unwrap(), None);
        assert_eq!(
            session.submit("\"text\"").unwrap(),
            Some(Value::new(Raw::Text("text".into())).unwrap())
        );
        assert_eq!(
            session.submit("echo($_)").unwrap(),
            Some(Value::new(Raw::Text("text".into())).unwrap())
        );
        assert_eq!(
            session.submit("last()").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(
            session.submit("let broken = missing;").unwrap_err().code(),
            "ORNA-EVAL-NAME"
        );
        assert_eq!(
            session.submit("twice(n + 1)").unwrap(),
            Some(Value::int(42.into()))
        );
    }

    #[test]
    fn a_function_declared_before_a_result_cannot_capture_it_later() {
        let mut session = ReplSession::new(Limits::default());
        assert_eq!(session.submit("fn previous() = $_;").unwrap(), None);
        assert_eq!(session.submit("42").unwrap(), Some(Value::int(42.into())));
        assert_eq!(
            session.submit("previous()").unwrap_err().code(),
            "ORNA-EVAL-NAME"
        );
    }

    #[test]
    fn previews_and_unsupported_submissions_do_not_change_session_state() {
        let mut session = ReplSession::new(Limits::default());
        assert_eq!(
            session.submit("40 + 2").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(session.preview("1 + 1").unwrap(), Value::int(2.into()));
        assert_eq!(
            session.submit("Note.insert(1)").unwrap_err().code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
        assert_eq!(session.submit("$_").unwrap(), Some(Value::int(42.into())));
    }

    #[test]
    fn annotations_are_rejected_until_the_bounded_evaluator_has_type_admission() {
        let mut session = ReplSession::new(Limits::default());
        assert_eq!(
            session.submit("let n: Int = \"text\";").unwrap_err().code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
        assert_eq!(
            session
                .submit("fn f(): Int = \"text\";")
                .unwrap_err()
                .code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
        assert_eq!(
            session
                .submit("fn generic<T>(value) = value;")
                .unwrap_err()
                .code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
        assert_eq!(session.submit("n").unwrap_err().code(), "ORNA-EVAL-NAME");
    }

    #[test]
    fn repl_bindings_are_session_only_even_with_a_spoofed_environment() {
        let environment = Environment::from([("$_".into(), Value::int(42.into()))]);
        assert_eq!(
            crate::evaluate_expression("$_", &environment, Limits::default())
                .unwrap_err()
                .code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
        assert_eq!(
            ReplSession::with_bindings(Limits::default(), environment, Functions::new())
                .unwrap_err()
                .code(),
            "ORNA-EVAL-LIMIT"
        );
        let mut invalid = ReplSession::new(Limits {
            max_steps: 0,
            ..Limits::default()
        });
        assert_eq!(invalid.submit("1").unwrap_err().code(), "ORNA-EVAL-LIMIT");
    }

    #[test]
    fn admitted_module_functions_cannot_read_repl_bindings() {
        let parsed = parse_module("fn last() = $_;");
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let Declaration::Function { signature, body } = parsed
            .value
            .items
            .into_iter()
            .next()
            .expect("function")
            .declaration
        else {
            panic!("function expected")
        };
        let functions = Functions::from([(
            "library.last".into(),
            PureFunction {
                parameters: signature.parameters,
                body,
                environment: Environment::new(),
            },
        )]);
        let mut session =
            ReplSession::with_bindings(Limits::default(), Environment::new(), functions)
                .expect("admitted function");
        assert_eq!(
            session.submit("40 + 2").unwrap(),
            Some(Value::int(42.into()))
        );
        assert_eq!(session.submit("use library;").unwrap(), None);
        assert_eq!(
            session.submit("library.last()").unwrap_err().code(),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }

    #[test]
    fn lexical_callable_shadows_an_admitted_module_sibling() {
        let mut functions = library_functions();
        let parsed = parse_module("fn local(add) = add(21, 21);");
        assert!(parsed.is_ok());
        let Declaration::Function { signature, body } =
            parsed.value.items.into_iter().next().unwrap().declaration
        else {
            panic!("function expected");
        };
        functions.insert(
            "library.local".into(),
            PureFunction {
                parameters: signature.parameters,
                body,
                environment: Environment::new(),
            },
        );
        let mut session =
            ReplSession::with_bindings(Limits::default(), Environment::new(), functions).unwrap();
        session.submit("use library;").unwrap();
        assert_eq!(
            session.submit("library.local((left, right) => left - right)"),
            Ok(Some(Value::int(0.into())))
        );
    }

    #[test]
    fn admitted_qualified_names_respect_the_configured_depth_limit() {
        let limits = Limits {
            max_depth: 2,
            ..Limits::default()
        };
        let environment = Environment::from([("a.b.c".into(), Value::int(1.into()))]);
        assert_eq!(
            ReplSession::with_bindings(limits, environment, Functions::new())
                .unwrap_err()
                .code(),
            "ORNA-EVAL-LIMIT"
        );
    }
}
