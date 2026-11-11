//! The session: accumulated definitions and top-level bindings, and the stage / commit / rollback contract.
//!
//! A name is defined once. Defining a name again, as a function or as a `let`, removes the earlier definition, so
//! the later one shadows it. A `let` that reads neither `$_` nor an earlier binding is a module-level `let`: it is
//! evaluated once, and later inputs read its kept value. Any other `let` binds a value that later inputs see as a
//! parameter of their entry function.

use orna_exec::Value;
use orna_syntax::{
    Declaration, Expr, Item, Pattern, ReplInput, SyntaxSpan as SourceSpan, parse_repl,
};

use crate::check::{self, Evaluated};
use crate::compose::{self, LAST, RESERVED_PREFIX};
use crate::error::ReplError;

#[derive(Clone, Debug)]
struct Definition {
    name: String,
    text: String,
}

/// A value with the printed type the HIR gave it.
#[derive(Clone, Debug)]
struct Typed {
    ty: String,
    value: Value,
}

#[derive(Clone, Debug)]
struct Binding {
    name: String,
    typed: Typed,
    /// The declaration of the module-level `let` when the binding is a global; `None` for a parameter.
    global: Option<String>,
}

/// What one input sees: its entry parameters with their values, the module-level `let`s its module carries, and the
/// values those `let`s are seeded with.
struct Scope {
    parameters: Vec<(String, String)>,
    arguments: Vec<Value>,
    globals: Vec<String>,
    seeds: Vec<(String, Value)>,
}

/// The session: every definition and binding a commit has applied, and the previous result, `$_`.
#[derive(Debug, Default)]
pub struct Session {
    /// The `use` declarations the prompt accepted, in order: every input's module starts with them.
    imports: Vec<String>,
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    last: Option<Typed>,
    revision: u64,
}

/// One input checked and evaluated against a session revision, not yet applied.
#[derive(Debug)]
pub struct Staged {
    base_revision: u64,
    effectful: bool,
    change: Change,
    result: Option<Value>,
}

#[derive(Debug)]
enum Change {
    Define(Definition),
    Bind(Binding),
    /// A `use` declaration: it joins the session's imports.
    Import(String),
    Evaluate(Typed),
    /// An expression statement: its value is discarded and `$_` is kept.
    Discard,
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks and evaluates one input without changing the session.
    ///
    /// # Errors
    /// Returns the rejection code and message for a parse, name, type, effect or runtime failure.
    pub fn stage(&self, input: &str) -> Result<Staged, ReplError> {
        let parsed = parse_repl(input);
        if !parsed.is_ok() {
            let code = parsed
                .diagnostics
                .first()
                .map_or("ORNA-REPL-INTERNAL", |diagnostic| diagnostic.code);
            return Err(ReplError::new(code, "the input does not parse"));
        }
        match &parsed.value {
            ReplInput::Expression(expression) => self.stage_expression(input, expression, true),
            ReplInput::ExpressionStatement(expression) => {
                self.stage_expression(input, expression, false)
            }
            ReplInput::Item(item) => self.stage_item(input, item),
        }
    }

    /// Applies a staged input.
    ///
    /// # Errors
    /// `ORNA-REPL-STALE` when the session changed after `staged` was staged; `ORNA-REPL-EFFECT` for an effectful
    /// input, which is never run.
    pub fn commit(&mut self, staged: Staged) -> Result<Option<Value>, ReplError> {
        if staged.base_revision != self.revision {
            return Err(ReplError::new(
                "ORNA-REPL-STALE",
                "the session changed after this input was staged",
            ));
        }
        if staged.effectful {
            return Err(ReplError::new(
                "ORNA-REPL-EFFECT",
                "an effectful input is not run in the REPL",
            ));
        }
        match staged.change {
            Change::Define(definition) => {
                self.remove(&definition.name);
                self.definitions.push(definition);
            }
            Change::Bind(binding) => {
                self.remove(&binding.name);
                self.bindings.push(binding);
            }
            Change::Evaluate(typed) => self.last = Some(typed),
            Change::Import(text) => self.imports.push(text),
            Change::Discard => {}
        }
        self.revision += 1;
        Ok(staged.result)
    }

    /// Discards a staged input. The session is unchanged.
    pub fn rollback(&self, _staged: Staged) {}

    fn remove(&mut self, name: &str) {
        self.definitions
            .retain(|definition| definition.name != name);
        self.bindings.retain(|binding| binding.name != name);
    }

    fn stage_expression(
        &self,
        input: &str,
        expression: &Expr,
        keep: bool,
    ) -> Result<Staged, ReplError> {
        let references = compose::expression_references(expression)?;
        let body = compose::rewrite(input, &expression.span(), &references)?;
        let scope = self.scope(&references, &compose::identifiers(&body)?, "")?;
        let entry = compose::entry(&scope.parameters, None, &body);
        let source = self.module_source(&scope, "", Some(&entry));
        let evaluated = check::evaluate(&source, scope.arguments, &scope.seeds)?;
        Ok(self.staged(evaluated, |typed| {
            if keep {
                Change::Evaluate(typed)
            } else {
                Change::Discard
            }
        }))
    }

    fn stage_item(&self, input: &str, item: &Item) -> Result<Staged, ReplError> {
        match &item.declaration {
            Declaration::Let {
                pattern,
                annotation,
                value,
            } => {
                let Pattern::Name(name, _) = pattern else {
                    return Err(ReplError::new(
                        "ORNA-EVAL-UNSUPPORTED",
                        "only a name can be bound at the prompt",
                    ));
                };
                check_name(name)?;
                let references = compose::expression_references(value)?;
                let body = compose::rewrite(input, &value.span(), &references)?;
                let identifiers = compose::identifiers(&body)?;
                let global = references.is_empty() && !self.reads_binding(&identifiers);
                let annotation = annotation
                    .as_ref()
                    .map(|annotation| compose::span_text(input, annotation.span()))
                    .transpose()?;
                let scope = self.scope(&references, &identifiers, name)?;
                let entry = compose::entry(&scope.parameters, annotation, &body);
                let source = self.module_source(&scope, name, Some(&entry));
                let evaluated = check::evaluate(&source, scope.arguments, &scope.seeds)?;
                let declaration = if global {
                    Some(compose::declaration_text(input, item)?)
                } else {
                    None
                };
                let name = name.clone();
                Ok(self.staged(evaluated, |typed| {
                    Change::Bind(Binding {
                        name,
                        typed,
                        global: declaration,
                    })
                }))
            }
            Declaration::Function { signature, .. } => {
                check_name(&signature.name)?;
                if !compose::declaration_references(&item.declaration)?.is_empty() {
                    return Err(ReplError::new(
                        "ORNA-EVAL-UNSUPPORTED",
                        "a function cannot read the previous result",
                    ));
                }
                let text = compose::declaration_text(input, item)?;
                let source = compose::module(
                    self.imports
                        .iter()
                        .map(String::as_str)
                        .chain(self.definitions_except(&signature.name))
                        .chain([text.as_str()]),
                    None,
                );
                check::lower_checked(&source)?;
                Ok(Staged {
                    base_revision: self.revision,
                    effectful: false,
                    change: Change::Define(Definition {
                        name: signature.name.clone(),
                        text,
                    }),
                    result: None,
                })
            }
            Declaration::Use { .. } => self.stage_import(input, item),
            _ => Err(ReplError::new(
                "ORNA-EVAL-UNSUPPORTED",
                "this declaration is not accepted at the prompt yet",
            )),
        }
    }

    /// Stages a `use` declaration: it is checked against the session as it stands, then joins the imports. A
    /// declaration the session already accepts is a no-op.
    fn stage_import(&self, input: &str, item: &Item) -> Result<Staged, ReplError> {
        let text = compose::declaration_text(input, item)?;
        if self.imports.contains(&text) {
            return Ok(Staged {
                base_revision: self.revision,
                effectful: false,
                change: Change::Discard,
                result: None,
            });
        }
        let scope = self.scope(&[], &[], "")?;
        let source = compose::module(
            self.imports
                .iter()
                .map(String::as_str)
                .chain([text.as_str()])
                .chain(self.definitions_except(""))
                .chain(scope.globals.iter().map(String::as_str)),
            None,
        );
        check::lower_checked(&source)?;
        Ok(Staged {
            base_revision: self.revision,
            effectful: false,
            change: Change::Import(text),
            result: None,
        })
    }

    /// The staged result of one evaluation: an effectful one carries no change and no value.
    fn staged(&self, evaluated: Evaluated, change: impl FnOnce(Typed) -> Change) -> Staged {
        let Evaluated {
            value,
            ty,
            effectful,
        } = evaluated;
        match value {
            Some(value) if !effectful => {
                let typed = Typed {
                    ty,
                    value: value.clone(),
                };
                Staged {
                    base_revision: self.revision,
                    effectful: false,
                    change: change(typed),
                    result: Some(value),
                }
            }
            _ => Staged {
                base_revision: self.revision,
                effectful: true,
                change: Change::Discard,
                result: None,
            },
        }
    }

    /// Whether an input's identifiers name an existing binding, global or parameter.
    fn reads_binding(&self, identifiers: &[String]) -> bool {
        identifiers.iter().any(|identifier| {
            self.bindings
                .iter()
                .any(|binding| binding.name == *identifier)
        })
    }

    /// The scope of one input. `shadowed` is the name a `let` input binds: its earlier global is hidden, and it is
    /// passed as a parameter only when the input reads it. Every parameter binding is passed; `$_` is passed when the
    /// input reads it.
    fn scope(
        &self,
        references: &[SourceSpan],
        identifiers: &[String],
        shadowed: &str,
    ) -> Result<Scope, ReplError> {
        let mut scope = Scope {
            parameters: Vec::new(),
            arguments: Vec::new(),
            globals: Vec::new(),
            seeds: Vec::new(),
        };
        for binding in &self.bindings {
            match &binding.global {
                Some(text) if binding.name != shadowed => {
                    scope.globals.push(text.clone());
                    scope
                        .seeds
                        .push((binding.name.clone(), binding.typed.value.clone()));
                }
                Some(_) if !identifiers.contains(&binding.name) => {}
                _ => {
                    scope
                        .parameters
                        .push((binding.name.clone(), binding.typed.ty.clone()));
                    scope.arguments.push(binding.typed.value.clone());
                }
            }
        }
        if !references.is_empty() {
            let last = self
                .last
                .as_ref()
                .ok_or_else(|| ReplError::new("ORNA-EVAL-NAME", "$_ has no previous result"))?;
            scope.parameters.push((LAST.to_owned(), last.ty.clone()));
            scope.arguments.push(last.value.clone());
        }
        Ok(scope)
    }

    fn definitions_except<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.definitions
            .iter()
            .filter(move |definition| definition.name != name)
            .map(|definition| definition.text.as_str())
    }

    /// The module source for one input: the session's imports, the definitions it keeps, its module-level `let`s, then
    /// its entry.
    fn module_source(&self, scope: &Scope, shadowed: &str, entry: Option<&str>) -> String {
        compose::module(
            self.imports
                .iter()
                .map(String::as_str)
                .chain(self.definitions_except(shadowed))
                .chain(scope.globals.iter().map(String::as_str)),
            entry,
        )
    }
}

fn check_name(name: &str) -> Result<(), ReplError> {
    if name.starts_with(RESERVED_PREFIX) {
        return Err(ReplError::new(
            "ORNA-REPL-NAME",
            "names starting __repl are reserved",
        ));
    }
    Ok(())
}
