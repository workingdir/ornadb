//! Incremental semantic admission for one ephemeral REPL module.
//!
//! This owns only semantic state.  A runtime pairs each committed admission
//! with its own transactional evaluator state before making it visible.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use orna_foundation_v1::Diagnostic;
use orna_syntax_v1::{Declaration, EntryPoint, Item, ReplInput, SyntaxSpan, SyntaxTree};

use crate::{
    Analysis, Catalogue, DIAG_DUPLICATE, DIAG_UNSUPPORTED, EffectSummary, ModuleHeader, Namespace,
    Scope, Symbol, SymbolKind, Type, check_item, declared_symbol, diag, infer, resolve_imports,
};

/// Semantic state for one ephemeral REPL module.
///
/// Project modules come from an already successful [`Analysis`].  Only their
/// public exports are exposed by ordinary `use` declarations; retained runtime
/// implementation helpers never become REPL-visible merely because they are
/// executable in the host.
#[derive(Clone, Debug)]
pub struct ReplContext {
    identity: Arc<()>,
    revision: u64,
    modules: BTreeMap<Namespace, ModuleHeader>,
    symbols: BTreeMap<String, Symbol>,
    imports: Vec<Item>,
    last_result: Option<Type>,
}

/// A staged semantic transition.  Its successor state is intentionally opaque
/// so callers cannot mutate the session without committing the admission.
#[derive(Clone, Debug)]
pub struct ReplAdmission {
    pub ty: Option<Type>,
    pub effects: EffectSummary,
    origin: Arc<()>,
    revision: u64,
    next: ReplContext,
}

/// A staged admission cannot be committed into this context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplCommitError {
    /// The admission was staged by another ephemeral REPL session.
    ForeignContext,
    /// The context advanced after this admission was staged.
    StaleRevision,
}

impl ReplContext {
    /// Builds a context from a fully admitted project analysis.
    pub fn from_analysis(analysis: &Analysis) -> Result<Self, Vec<Diagnostic>> {
        if !analysis.is_ok() {
            return Err(analysis.diagnostics.clone());
        }
        Ok(Self {
            identity: Arc::new(()),
            revision: 0,
            modules: analysis.modules.clone(),
            symbols: BTreeMap::new(),
            imports: Vec::new(),
            last_result: None,
        })
    }

    /// Starts an empty REPL with the authoritative core declaration catalogue.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            identity: Arc::new(()),
            revision: 0,
            modules: Catalogue::authoritative_core().modules,
            symbols: BTreeMap::new(),
            imports: Vec::new(),
            last_result: None,
        }
    }

    /// Checks one parsed REPL input and returns a staged successor.
    ///
    /// The input is an AST supplied by the caller; no source module is
    /// synthesized or reparsed here.  Failure leaves this context unchanged.
    pub fn stage(&self, input: &ReplInput) -> Result<ReplAdmission, Vec<Diagnostic>> {
        let Some(revision) = self.revision.checked_add(1) else {
            return Err(vec![diag(DIAG_UNSUPPORTED, "REPL revision limit reached")]);
        };
        let mut next = self.clone();
        next.revision = revision;
        let mut diagnostics = Vec::new();
        let (ty, effects) = match input {
            ReplInput::Expression(expression) => {
                let scope = self.scope(&self.imports, &self.symbols, &mut diagnostics);
                let inferred = infer(expression, &scope, &BTreeMap::new(), &mut diagnostics);
                next.last_result = Some(inferred.ty.clone());
                (Some(inferred.ty), inferred.effects)
            }
            ReplInput::Item(item) => match &item.declaration {
                Declaration::Use { .. } => {
                    next.imports.push(item.clone());
                    // Reuse the ordinary import resolver with every retained
                    // use declaration.  It exposes module exports, never
                    // runtime-only retained siblings.
                    let _ = next.scope(&next.imports, &next.symbols, &mut diagnostics);
                    (None, EffectSummary::default())
                }
                Declaration::Let { .. } | Declaration::Function { .. } => {
                    let mut symbols = self.symbols.clone();
                    if let Declaration::Function { .. } = &item.declaration {
                        Self::predeclare_function(item, &mut symbols, &mut diagnostics);
                    }
                    let scope = self.scope(&self.imports, &symbols, &mut diagnostics);
                    let mut plans = Vec::new();
                    let inferred = check_item(
                        item,
                        &mut symbols,
                        &scope,
                        &scope.table_rows,
                        &mut plans,
                        &mut diagnostics,
                    );
                    next.symbols = symbols;
                    let declaration_effects =
                        if matches!(item.declaration, Declaration::Function { .. }) {
                            EffectSummary::default()
                        } else {
                            inferred
                                .as_ref()
                                .map_or_else(EffectSummary::default, |value| value.effects.clone())
                        };
                    (
                        inferred.as_ref().map(|value| value.ty.clone()),
                        declaration_effects,
                    )
                }
                _ => {
                    diagnostics.push(diag(
                        DIAG_UNSUPPORTED,
                        "REPL admission supports use, let, and function declarations",
                    ));
                    (None, EffectSummary::default())
                }
            },
        };
        diagnostics.sort_by(|a, b| a.code().cmp(b.code()).then(a.message().cmp(b.message())));
        if diagnostics.is_empty() {
            Ok(ReplAdmission {
                ty,
                effects,
                origin: Arc::clone(&self.identity),
                revision: self.revision,
                next,
            })
        } else {
            Err(diagnostics)
        }
    }

    /// Commits a successfully executed admission from this exact context revision.
    pub fn commit(&mut self, admission: ReplAdmission) -> Result<(), ReplCommitError> {
        if !Arc::ptr_eq(&self.identity, &admission.origin) {
            return Err(ReplCommitError::ForeignContext);
        }
        if self.revision != admission.revision {
            return Err(ReplCommitError::StaleRevision);
        }
        *self = admission.next;
        Ok(())
    }

    fn predeclare_function(
        item: &Item,
        symbols: &mut BTreeMap<String, Symbol>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let Some((name, kind, ty)) = declared_symbol(item) else {
            return;
        };
        if symbols.contains_key(&name) {
            diagnostics.push(diag(DIAG_DUPLICATE, "duplicate declaration name"));
            return;
        }
        symbols.insert(
            name,
            Symbol {
                kind,
                ty,
                public: false,
                effects: EffectSummary::default(),
                table_schema: None,
            },
        );
    }

    fn scope(
        &self,
        imports: &[Item],
        symbols: &BTreeMap<String, Symbol>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Scope {
        let mut symbols = symbols.clone();
        if let Some(ty) = &self.last_result {
            symbols.insert(
                "$_".into(),
                Symbol {
                    kind: SymbolKind::Let,
                    ty: ty.clone(),
                    public: false,
                    effects: EffectSummary::default(),
                    table_schema: None,
                },
            );
        }
        let header = ModuleHeader {
            namespace: Namespace(Vec::new()),
            exports: BTreeMap::new(),
            symbols,
            prelude_exports: BTreeSet::new(),
            implicit: false,
        };
        let tree = SyntaxTree {
            entry: EntryPoint::Repl,
            items: imports.to_vec(),
            span: SyntaxSpan::new(0, 0),
        };
        resolve_imports(
            &Namespace(Vec::new()),
            &tree,
            &header,
            &self.modules,
            &BTreeMap::new(),
            diagnostics,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleInput, analyze};
    use orna_syntax_v1::parse_repl;

    fn staged(context: &ReplContext, source: &str) -> ReplAdmission {
        let parsed = parse_repl(source);
        assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
        context.stage(&parsed.value).expect("admitted REPL input")
    }

    #[test]
    fn rejects_failed_project_analysis() {
        let analysis = analyze(&[ModuleInput::new("main.orna", "fn bad() = $_;")]);
        assert!(ReplContext::from_analysis(&analysis).is_err());
    }

    #[test]
    fn typed_let_mismatch_is_rejected() {
        let context = ReplContext::empty();
        let parsed = parse_repl("let count: Int = \"wrong\";");
        assert!(parsed.is_ok());
        assert!(context.stage(&parsed.value).is_err());
    }

    #[test]
    fn committed_bindings_and_last_result_are_retained() {
        let mut context = ReplContext::empty();
        context
            .commit(staged(&context, "let count: Int = 41;"))
            .expect("current admission commits");
        context
            .commit(staged(
                &context,
                "fn twice(value: Int): Int = value + value;",
            ))
            .expect("current admission commits");
        assert_eq!(staged(&context, "twice(count)").ty, Some(Type::Int));
        context
            .commit(staged(&context, "count + 1"))
            .expect("current admission commits");
        assert_eq!(staged(&context, "$_").ty, Some(Type::Int));
    }

    #[test]
    fn imports_expose_only_analysis_exports() {
        let analysis = analyze(&[
            ModuleInput::new(
                "library.orna",
                "pub fn visible(value: Int): Int = value; fn hidden(value: Int): Int = value;",
            ),
            ModuleInput::new("main.orna", "pub fn run(): Int = 1;"),
        ]);
        let mut context = ReplContext::from_analysis(&analysis).expect("project admitted");
        context
            .commit(staged(&context, "use library;"))
            .expect("current admission commits");
        assert_eq!(staged(&context, "library.visible(1)").ty, Some(Type::Int));
        let private = parse_repl("library.hidden(1)");
        assert!(private.is_ok());
        assert!(context.stage(&private.value).is_err());
    }

    #[test]
    fn uncommitted_or_failed_stages_do_not_escape() {
        let mut context = ReplContext::empty();
        let pending = staged(&context, "let count = 1;");
        let name = parse_repl("count");
        assert!(context.stage(&name.value).is_err());
        context.commit(pending).expect("current admission commits");
        let mismatch = parse_repl("let count: Int = \"wrong\";");
        assert!(context.stage(&mismatch.value).is_err());
        assert_eq!(staged(&context, "count").ty, Some(Type::Int));
    }

    #[test]
    fn stale_admission_is_rejected_without_losing_the_committed_state() {
        let mut context = ReplContext::empty();
        let first = staged(&context, "let first = 1;");
        let second = staged(&context, "let second = 2;");

        context.commit(first).expect("first stage is current");
        assert_eq!(context.commit(second), Err(ReplCommitError::StaleRevision));
        assert_eq!(staged(&context, "first").ty, Some(Type::Int));
        let missing = parse_repl("second");
        assert!(context.stage(&missing.value).is_err());
    }

    #[test]
    fn foreign_admission_is_rejected_without_cross_session_injection() {
        let first = ReplContext::empty();
        let mut second = ReplContext::empty();
        let admission = staged(&first, "let injected = 1;");

        assert_eq!(
            second.commit(admission),
            Err(ReplCommitError::ForeignContext)
        );
        let missing = parse_repl("injected");
        assert!(second.stage(&missing.value).is_err());
    }
}
