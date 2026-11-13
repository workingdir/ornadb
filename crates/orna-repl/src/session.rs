//! The session: accumulated definitions and top-level bindings, and the stage / commit / rollback contract.
//!
//! A name is defined once. Defining a name again, as a function or as a `let`, removes the earlier definition, so
//! the later one shadows it. A `let` that reads neither `$_` nor an earlier binding is a module-level `let`: it is
//! evaluated once, and later inputs read its kept value. Any other `let` binds a value that later inputs see as a
//! parameter of their entry function.

use orna_exec::Value;
use orna_syntax::parse_repl;
use orna_syntax_ast::SourceSpan;
use orna_syntax_ast::{Declaration, Expr, Item, Pattern, ReplInput};

use crate::base::{Base, Definition};
use crate::check::{self, Evaluated};
use crate::compose::{self, LAST, RESERVED_PREFIX};
use crate::error::ReplError;
use crate::standard;

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
    /// Whether the binding is a module-level `let`: its declaration is then in the session's base.
    global: bool,
}

/// What one input sees: its entry parameters with their values, and the values its module-level `let`s are seeded
/// with.
struct Scope {
    parameters: Vec<(String, String)>,
    arguments: Vec<Value>,
    seeds: Vec<(String, Value)>,
}

/// The session: every definition and binding a commit has applied, and the previous result, `$_`.
#[derive(Debug, Default)]
pub struct Session {
    /// The session's source: its `use` declarations, function definitions and module-level `let`s.
    base: Base,
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
    /// A `let` binding, with its declaration when the binding is a module-level `let`.
    Bind(Binding, Option<Definition>),
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
            // Every prompt parse failure is reported under the REPL's established code, whatever
            // the parser's own diagnostic code is.
            return Err(ReplError::new(
                "ORNA-EVAL-PARSE",
                "the input does not parse",
            ));
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
                self.base = std::mem::take(&mut self.base).with_definition(definition);
            }
            Change::Bind(binding, global) => {
                self.remove(&binding.name);
                if let Some(global) = global {
                    self.base = std::mem::take(&mut self.base).with_global(global);
                }
                self.bindings.push(binding);
            }
            Change::Evaluate(typed) => self.last = Some(typed),
            Change::Import(text) => self.base = std::mem::take(&mut self.base).with_import(text),
            Change::Discard => {}
        }
        self.revision += 1;
        Ok(staged.result)
    }

    /// Discards a staged input. The session is unchanged.
    pub fn rollback(&self, _staged: Staged) {}

    fn remove(&mut self, name: &str) {
        self.base = self.base.without(name);
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
        let source = self.base.source(Some(&entry));
        let evaluated = check::evaluate(
            &standard::lowering(),
            &source,
            scope.arguments,
            &scope.seeds,
        )?;
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
                let source = self.base.without(name).source(Some(&entry));
                let evaluated = check::evaluate(
                    &standard::lowering(),
                    &source,
                    scope.arguments,
                    &scope.seeds,
                )?;
                let declaration = if global {
                    Some(Definition {
                        name: name.clone(),
                        text: compose::declaration_text(input, item)?,
                    })
                } else {
                    None
                };
                let name = name.clone();
                let mut staged = self.staged(evaluated, |typed| {
                    Change::Bind(
                        Binding {
                            name,
                            typed,
                            global,
                        },
                        declaration,
                    )
                });
                // A `let` binds its name and produces no value at the prompt.
                staged.result = None;
                Ok(staged)
            }
            Declaration::Function { signature, .. } => {
                check_name(&signature.name)?;
                if !compose::declaration_references(&item.declaration)?.is_empty() {
                    return Err(ReplError::new(
                        "ORNA-EVAL-UNSUPPORTED",
                        "a function cannot read the previous result",
                    ));
                }
                let definition = Definition {
                    name: signature.name.clone(),
                    text: compose::declaration_text(input, item)?,
                };
                let checked = self
                    .base
                    .without(&signature.name)
                    .with_definition(definition.clone());
                check::lower_checked(&standard::lowering(), &checked.definitions_source())?;
                Ok(Staged {
                    base_revision: self.revision,
                    effectful: false,
                    change: Change::Define(definition),
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
        if self.base.has_import(&text) {
            return Ok(Staged {
                base_revision: self.revision,
                effectful: false,
                change: Change::Discard,
                result: None,
            });
        }
        let source = self.base.clone().with_import(text.clone()).source(None);
        check::lower_checked(&standard::lowering(), &source)?;
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
            seeds: Vec::new(),
        };
        for binding in &self.bindings {
            if binding.global && binding.name != shadowed {
                scope
                    .seeds
                    .push((binding.name.clone(), binding.typed.value.clone()));
            } else if !binding.global || identifiers.contains(&binding.name) {
                scope
                    .parameters
                    .push((binding.name.clone(), binding.typed.ty.clone()));
                scope.arguments.push(binding.typed.value.clone());
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
