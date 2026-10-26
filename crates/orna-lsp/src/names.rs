//! Every name in one document, found in one walk: local bindings with their
//! scopes, and each name occurrence with its syntactic role.
//!
//! The walk visits every body in the module: functions, implementation
//! members, table and type field initializers and assertions, top-level
//! `let`s, assertions and unit definitions. A `for` loop body is visited
//! once, although the parser also stores a copy of it as a case arm (A01-17).
//!
//! Member names (`value.field`, record labels, pattern fields, named
//! arguments) carry a [`Hint`] describing their receiver as written in
//! source. Resolution turns hints into declarations; this module never
//! guesses a type.

use std::collections::HashMap;

use orna_syntax_v1::{
    Argument, AssignmentTarget, CaseArm, ControlKind, Declaration, Expr, FieldInitializer,
    ImplMember, Implementation, Item, LiteralKind, Parameter, Pattern, RecordField, Statement,
    StringSegment, SyntaxSpan, SyntaxTree, TableMember, Token, TokenKind, TypeExpr, TypeMember,
    TypeRepresentation, UnitDefinition, UseTail,
};

/// NFC-normalised identifier text, the identity of a name (ORNA-LEX-005).
pub(crate) type Key = String;

/// What a value is, as far as the source says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hint {
    Unknown,
    /// The value of the name at this occurrence: a local, a declaration or a
    /// module alias.
    Name { key: Key, span: SyntaxSpan },
    /// A value of the type written at this path.
    Type(TypePath),
    /// The result of calling the callee described by the inner hint.
    Call(Box<Hint>),
    /// Member `key` of the value described by the inner hint.
    Member(Box<Hint>, Key),
    /// One element of the collection or relation described by the inner hint.
    Element(Box<Hint>),
    /// A list whose elements are described by the inner hint.
    Collection(Box<Hint>),
    /// `self` inside the implementation members of the declaration whose item
    /// span is given.
    Owner(SyntaxSpan),
}

/// A type name as written: path keys with the span of each segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypePath {
    pub segments: Vec<(Key, SyntaxSpan)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalBindingKind {
    Parameter,
    Local,
    Pattern,
}

/// A name bound inside a body: a parameter, `let`, or pattern binding.
#[derive(Clone, Debug)]
pub(crate) struct LocalBinding {
    pub name: String,
    pub key: Key,
    pub selection: SyntaxSpan,
    /// The parameter or statement that declares it, for hover text.
    pub context: SyntaxSpan,
    pub kind: LocalBindingKind,
    pub hint: Hint,
    scope: SyntaxSpan,
    visible_after: usize,
}

/// The syntactic role of one name occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The declared name of a top-level item or one of its members.
    Declaration,
    /// A name read or written in an expression.
    Value { write: bool },
    /// Segment `index` of a type path, or of a nominal constructor or
    /// constructor pattern path.
    Path { path: Vec<Key>, index: usize },
    /// `receiver.name`, including assignment targets.
    Member { receiver: Hint, write: bool },
    /// A record literal label; the hint describes the record's type when the
    /// literal is a table operation argument or a nominal constructor.
    RecordLabel { owner: Hint },
    /// A record pattern or constructor pattern field.
    PatternField { owner: Hint },
    /// A named argument label of a call to the described callee.
    ArgumentLabel { callee: Hint },
    /// Segment `index` of a `use` module path.
    UsePath { path: Vec<Key>, index: usize },
    /// `name` in `use module.{name}`.
    UseName { module: Vec<Key> },
    /// `alias` in `use module as alias`.
    UseAlias { module: Vec<Key> },
    /// The unit after a numeral, as in `1000.metre` (ORNA-UNIT-008).
    UnitSuffix,
}

#[derive(Clone, Debug)]
pub(crate) struct NameOccurrence {
    pub span: SyntaxSpan,
    pub key: Key,
    pub role: Role,
    /// The local binding a [`Role::Value`] resolves to.
    pub binding: Option<usize>,
    /// The occurrence names the function a call invokes.
    pub callee: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclaredKind {
    Function,
    Table,
    Type,
    Enum,
    Protocol,
    Dimension,
    Unit,
    Let,
    /// A stored or computed field of a table or type.
    Field,
    /// A primary-key parameter of a table.
    KeyField,
    Variant,
    /// A payload field of an enum variant.
    PayloadField,
    /// A function inside an implementation or protocol.
    Method,
    /// A static property inside an implementation or protocol.
    Static,
}

/// A name declared by a top-level item or one of its members.
#[derive(Clone, Debug)]
pub(crate) struct Declared {
    pub name: String,
    pub key: Key,
    pub selection: SyntaxSpan,
    /// The whole item or member.
    pub full: SyntaxSpan,
    pub kind: DeclaredKind,
    /// The declaration this one is a member of.
    pub owner: Option<usize>,
    pub public: bool,
    /// A field's declared type, or a function's declared result type.
    pub ty: Option<TypeExpr>,
    /// A function's parameter names and pattern spans.
    pub parameters: Vec<(Key, SyntaxSpan)>,
}

/// The names of one document.
#[derive(Debug, Default)]
pub(crate) struct NameIndex {
    bindings: Vec<LocalBinding>,
    occurrences: Vec<NameOccurrence>,
    declarations: Vec<Declared>,
    declared_at: HashMap<(usize, usize), usize>,
}

impl NameIndex {
    pub(crate) fn new(tree: &SyntaxTree, tokens: &[Token]) -> Self {
        let mut walker = Walker {
            tokens,
            bindings: Vec::new(),
            occurrences: Vec::new(),
            declarations: Vec::new(),
            owner: None,
            public: false,
        };
        for item in &tree.items {
            walker.public = matches!(item.visibility, orna_syntax_v1::Visibility::Public { .. });
            walker.item(item);
        }
        let Walker {
            mut bindings,
            mut occurrences,
            declarations,
            ..
        } = walker;
        bindings.sort_by_key(|binding| binding.selection.start);
        occurrences.sort_by_key(|occurrence| (occurrence.span.start, occurrence.span.end));
        occurrences.dedup_by(|right, left| {
            let same = same_span(&left.span, &right.span);
            if same {
                left.callee |= right.callee;
            }
            same
        });
        let mut by_key = HashMap::<&str, Vec<usize>>::new();
        for (index, binding) in bindings.iter().enumerate() {
            by_key.entry(binding.key.as_str()).or_default().push(index);
        }
        for occurrence in &mut occurrences {
            if matches!(occurrence.role, Role::Value { .. }) {
                occurrence.binding =
                    resolve_binding(&bindings, &by_key, &occurrence.key, occurrence.span.start);
            }
        }
        let declared_at = declarations
            .iter()
            .enumerate()
            .map(|(index, declared)| ((declared.selection.start, declared.selection.end), index))
            .collect();
        Self {
            bindings,
            occurrences,
            declarations,
            declared_at,
        }
    }

    pub(crate) fn declarations(&self) -> &[Declared] {
        &self.declarations
    }

    pub(crate) fn declaration(&self, index: usize) -> &Declared {
        &self.declarations[index]
    }

    /// The declaration whose name is at `span`.
    pub(crate) fn declared_at(&self, span: &SyntaxSpan) -> Option<usize> {
        self.declared_at.get(&(span.start, span.end)).copied()
    }

    pub(crate) fn bindings(&self) -> &[LocalBinding] {
        &self.bindings
    }

    pub(crate) fn binding(&self, index: usize) -> &LocalBinding {
        &self.bindings[index]
    }

    pub(crate) fn occurrences(&self) -> &[NameOccurrence] {
        &self.occurrences
    }

    /// The occurrence whose identifier contains `byte`.
    pub(crate) fn occurrence_at(&self, byte: usize) -> Option<&NameOccurrence> {
        let index = self
            .occurrences
            .partition_point(|occurrence| occurrence.span.end < byte);
        self.occurrences[index..]
            .iter()
            .take_while(|occurrence| occurrence.span.start <= byte)
            .find(|occurrence| occurrence.span.start <= byte && byte <= occurrence.span.end)
    }

    /// The occurrence with exactly this span.
    pub(crate) fn occurrence(&self, span: &SyntaxSpan) -> Option<&NameOccurrence> {
        let index = self
            .occurrences
            .binary_search_by_key(&(span.start, span.end), |occurrence| {
                (occurrence.span.start, occurrence.span.end)
            })
            .ok()?;
        Some(&self.occurrences[index])
    }

    /// The local binding whose declared name contains `byte`.
    pub(crate) fn binding_containing(&self, byte: usize) -> Option<usize> {
        let index = self
            .bindings
            .partition_point(|binding| binding.selection.end < byte);
        (index..self.bindings.len())
            .take_while(|index| self.bindings[*index].selection.start <= byte)
            .find(|index| {
                let selection = &self.bindings[*index].selection;
                selection.start <= byte && byte <= selection.end
            })
    }

    /// The local binding declared at `span`, or used at `span`.
    pub(crate) fn binding_at(&self, span: &SyntaxSpan) -> Option<usize> {
        if let Ok(index) = self
            .bindings
            .binary_search_by_key(&(span.start, span.end), |binding| {
                (binding.selection.start, binding.selection.end)
            })
        {
            return Some(index);
        }
        self.occurrence(span)?.binding
    }

    /// Every use of a local binding, without its declaration.
    pub(crate) fn references(&self, binding: usize) -> impl Iterator<Item = &NameOccurrence> {
        self.occurrences
            .iter()
            .filter(move |occurrence| occurrence.binding == Some(binding))
    }

    /// The innermost binding of each name visible at `position`.
    pub(crate) fn visible_bindings(&self, position: usize) -> Vec<&LocalBinding> {
        let mut visible = HashMap::<&str, &LocalBinding>::new();
        for binding in &self.bindings {
            if !binding.visible_at(position) {
                continue;
            }
            visible
                .entry(binding.key.as_str())
                .and_modify(|current| {
                    if binding.innermost_order() < current.innermost_order() {
                        *current = binding;
                    }
                })
                .or_insert(binding);
        }
        let mut visible = visible.into_values().collect::<Vec<_>>();
        visible.sort_by_key(|binding| binding.selection.start);
        visible
    }
}

impl LocalBinding {
    fn visible_at(&self, position: usize) -> bool {
        self.visible_after <= position && self.scope.start <= position && position < self.scope.end
    }

    /// Smaller is more inner: the narrowest scope wins, then the latest
    /// shadowing binding in that scope.
    fn innermost_order(&self) -> (usize, std::cmp::Reverse<usize>) {
        (
            self.scope.end - self.scope.start,
            std::cmp::Reverse(self.visible_after),
        )
    }
}

fn resolve_binding(
    bindings: &[LocalBinding],
    by_key: &HashMap<&str, Vec<usize>>,
    key: &str,
    position: usize,
) -> Option<usize> {
    by_key
        .get(key)?
        .iter()
        .copied()
        .filter(|index| bindings[*index].visible_at(position))
        .min_by_key(|index| bindings[*index].innermost_order())
}

fn same_span(left: &SyntaxSpan, right: &SyntaxSpan) -> bool {
    left.start == right.start && left.end == right.end
}

/// The arms a walker visits for one control expression. A `for` loop's arms
/// hold a parser copy of the loop body (A01-17), which must not be visited a
/// second time.
pub(crate) fn control_arms(kind: ControlKind, arms: &[CaseArm]) -> &[CaseArm] {
    if kind == ControlKind::For { &[] } else { arms }
}

/// The normalised key of an identifier as written.
pub(crate) fn key_of(text: &str) -> Key {
    orna_syntax_v1::lex(text)
        .ok()
        .and_then(|tokens| {
            tokens.into_iter().find_map(|token| match token.kind {
                TokenKind::Identifier { normalized } => Some(normalized),
                _ => None,
            })
        })
        .unwrap_or_else(|| text.to_owned())
}

/// Operations that keep the element type of the collection they receive.
const ELEMENT_PRESERVING: &[&str] = &["filter", "sort_by", "take", "drop", "distinct"];
/// Operations that return one element of the collection they receive.
const ELEMENT_RETURNING: &[&str] = &["first", "one", "min", "max"];
/// Table operations whose result is a row of the table.
const ROW_RETURNING: &[&str] = &["insert", "upsert", "update", "rekey"];

struct Walker<'a> {
    tokens: &'a [Token],
    bindings: Vec<LocalBinding>,
    occurrences: Vec<NameOccurrence>,
    declarations: Vec<Declared>,
    /// The declaration whose members are being walked.
    owner: Option<usize>,
    /// Visibility of the top-level item being walked.
    public: bool,
}

impl Walker<'_> {
    /// Identifier tokens (including `self`) inside `span`, in order.
    fn identifiers(&self, span: &SyntaxSpan) -> impl Iterator<Item = &Token> {
        let start = self.tokens.partition_point(|token| token.span.start < span.start);
        self.tokens[start..]
            .iter()
            .take_while(move |token| token.span.end <= span.end)
            .filter(|token| is_name_token(token))
    }

    fn first_identifier(&self, span: &SyntaxSpan) -> Option<SyntaxSpan> {
        self.identifiers(span).next().map(|token| token.span.clone())
    }

    fn last_identifier(&self, span: &SyntaxSpan) -> Option<SyntaxSpan> {
        self.identifiers(span).last().map(|token| token.span.clone())
    }

    fn occur(&mut self, span: SyntaxSpan, name: &str, role: Role) {
        self.occurrences.push(NameOccurrence {
            span,
            key: key_of(name),
            role,
            binding: None,
            callee: false,
        });
    }

    /// Records the declared `name`, found as the first matching identifier
    /// inside `scope`, as a member of the current owner.
    fn declare(
        &mut self,
        name: &str,
        scope: &SyntaxSpan,
        full: &SyntaxSpan,
        kind: DeclaredKind,
    ) -> Option<usize> {
        let key = key_of(name);
        let selection = self
            .identifiers(scope)
            .find(|token| token_key(token) == key)?
            .span
            .clone();
        self.occur(selection.clone(), name, Role::Declaration);
        self.declarations.push(Declared {
            name: name.to_owned(),
            key,
            selection,
            full: full.clone(),
            kind,
            owner: self.owner,
            public: self.public,
            ty: None,
            parameters: Vec::new(),
        });
        Some(self.declarations.len() - 1)
    }

    fn declare_function(
        &mut self,
        signature: &orna_syntax_v1::FunctionSignature,
        full: &SyntaxSpan,
        kind: DeclaredKind,
    ) {
        if let Some(index) = self.declare(&signature.name, &signature.span, full, kind) {
            let declared = &mut self.declarations[index];
            declared.ty = signature.result.clone();
            declared.parameters = signature
                .parameters
                .iter()
                .filter_map(|parameter| match &parameter.pattern {
                    Pattern::Name(name, span) => Some((key_of(name), span.clone())),
                    _ => None,
                })
                .collect();
        }
    }

    fn declare_typed(
        &mut self,
        name: &str,
        scope: &SyntaxSpan,
        kind: DeclaredKind,
        ty: Option<&TypeExpr>,
    ) {
        if let Some(index) = self.declare(name, scope, scope, kind) {
            self.declarations[index].ty = ty.cloned();
        }
    }

    /// Walks the members of the declaration at `owner`.
    fn with_owner(&mut self, owner: Option<usize>, walk: impl FnOnce(&mut Self)) {
        let outer = std::mem::replace(&mut self.owner, owner);
        walk(self);
        self.owner = outer;
    }

    fn item(&mut self, item: &Item) {
        match &item.declaration {
            Declaration::Use { path, tail } => {
                let keys = path.iter().map(|segment| key_of(&segment.name)).collect::<Vec<_>>();
                for (index, segment) in path.iter().enumerate() {
                    self.occur(
                        segment.span.clone(),
                        &segment.name,
                        Role::UsePath {
                            path: keys.clone(),
                            index,
                        },
                    );
                }
                match tail {
                    UseTail::Names(names) => {
                        for name in names {
                            self.occur(
                                name.span.clone(),
                                &name.name,
                                Role::UseName {
                                    module: keys.clone(),
                                },
                            );
                        }
                    }
                    UseTail::Alias { name, span } => {
                        if let Some(alias) = self.last_identifier(span) {
                            self.occur(alias, name, Role::UseAlias { module: keys });
                        }
                    }
                    UseTail::None | UseTail::Glob { .. } => {}
                }
            }
            Declaration::Function { signature, body } => {
                self.declare_function(signature, &item.span, DeclaredKind::Function);
                self.signature_types(signature);
                self.parameters(&signature.parameters, &body.span(), None);
                self.expr(body);
            }
            Declaration::Table { name, keys, members } => {
                let table = self.declare(name, &item.span, &item.span, DeclaredKind::Table);
                let row = Hint::Owner(item.span.clone());
                self.with_owner(table, |walker| {
                    for key in keys {
                        walker.key_field(key);
                    }
                    for member in members {
                        match member {
                            TableMember::Field {
                                name,
                                ty,
                                initializer,
                                span,
                            } => {
                                walker.declare_typed(name, span, DeclaredKind::Field, Some(ty));
                                walker.type_expr(ty);
                                if let Some(
                                    FieldInitializer::Default(value)
                                    | FieldInitializer::Computed(value),
                                ) = initializer
                                {
                                    walker.expr(value);
                                }
                            }
                            TableMember::Assertion { value, .. } => walker.expr(value),
                            TableMember::Implementation { implementation, .. } => {
                                walker.implementation(implementation, &row);
                            }
                        }
                    }
                });
            }
            Declaration::Protocol {
                name,
                generics,
                members,
            } => {
                let protocol = self.declare(name, &item.span, &item.span, DeclaredKind::Protocol);
                for generic in generics {
                    for bound in &generic.bounds {
                        self.type_expr(bound);
                    }
                }
                self.with_owner(protocol, |walker| {
                    for member in members {
                        match member {
                            orna_syntax_v1::ProtocolMember::Function { signature, span } => {
                                walker.declare_function(signature, span, DeclaredKind::Method);
                                walker.signature_types(signature);
                            }
                            orna_syntax_v1::ProtocolMember::Static { name, ty, span } => {
                                walker.declare_typed(name, span, DeclaredKind::Static, Some(ty));
                                walker.type_expr(ty);
                            }
                        }
                    }
                });
            }
            Declaration::Enum {
                name,
                generics,
                variants,
            } => {
                let enumeration = self.declare(name, &item.span, &item.span, DeclaredKind::Enum);
                for generic in generics {
                    for bound in &generic.bounds {
                        self.type_expr(bound);
                    }
                }
                self.with_owner(enumeration, |walker| {
                    for variant in variants {
                        let declared = walker.declare(
                            &variant.name,
                            &variant.span,
                            &variant.span,
                            DeclaredKind::Variant,
                        );
                        walker.with_owner(declared, |walker| {
                            for field in &variant.fields {
                                walker.declare_typed(
                                    &field.name,
                                    &field.span,
                                    DeclaredKind::PayloadField,
                                    Some(&field.ty),
                                );
                                walker.type_expr(&field.ty);
                            }
                        });
                    }
                });
            }
            Declaration::Dimension { name, expression } => {
                self.declare(name, &item.span, &item.span, DeclaredKind::Dimension);
                if let Some(expression) = expression {
                    for (_, ty, _, _) in &expression.terms {
                        self.type_expr(ty);
                    }
                }
            }
            Declaration::Unit {
                name,
                dimension,
                definition,
            } => {
                self.declare(name, &item.span, &item.span, DeclaredKind::Unit);
                self.type_expr(dimension);
                if let UnitDefinition::Derived { value, offset, .. } = definition {
                    self.expr(value);
                    if let Some(offset) = offset {
                        self.expr(offset);
                    }
                }
            }
            Declaration::Type {
                name,
                generics,
                representation,
            } => {
                let declared = self.declare(name, &item.span, &item.span, DeclaredKind::Type);
                for generic in generics {
                    for bound in &generic.bounds {
                        self.type_expr(bound);
                    }
                }
                let owner = Hint::Owner(item.span.clone());
                let members = match representation {
                    TypeRepresentation::Alias { ty, refinements } => {
                        self.type_expr(ty);
                        refinements
                    }
                    TypeRepresentation::Nominal { members } => members,
                };
                self.with_owner(declared, |walker| {
                    for member in members {
                        match member {
                            TypeMember::Field {
                                name,
                                ty,
                                initializer,
                                span,
                                ..
                            } => {
                                walker.declare_typed(name, span, DeclaredKind::Field, Some(ty));
                                walker.type_expr(ty);
                                if let Some(initializer) = initializer {
                                    walker.expr(initializer);
                                }
                            }
                            TypeMember::Assertion { value, .. } => walker.expr(value),
                            TypeMember::Implementation { implementation, .. } => {
                                walker.implementation(implementation, &owner);
                            }
                        }
                    }
                });
            }
            Declaration::Assertion { value } => self.expr(value),
            Declaration::Let {
                pattern,
                annotation,
                value,
            } => {
                if let Some(annotation) = annotation {
                    self.type_expr(annotation);
                }
                self.expr(value);
                // A module-level `let` declares module values, not locals.
                self.module_values(pattern, &item.span, annotation.as_ref());
            }
        }
    }

    fn implementation(&mut self, implementation: &Implementation, owner: &Hint) {
        self.type_expr(&implementation.protocol);
        for member in &implementation.members {
            match member {
                ImplMember::Function {
                    signature,
                    body,
                    span,
                } => {
                    self.declare_function(signature, span, DeclaredKind::Method);
                    self.signature_types(signature);
                    self.parameters(&signature.parameters, &body.span(), Some(owner));
                    self.expr(body);
                }
                ImplMember::Static {
                    name,
                    ty,
                    value,
                    span,
                } => {
                    self.declare_typed(name, span, DeclaredKind::Static, ty.as_ref());
                    if let Some(ty) = ty {
                        self.type_expr(ty);
                    }
                    self.expr(value);
                }
            }
        }
    }

    fn signature_types(&mut self, signature: &orna_syntax_v1::FunctionSignature) {
        for generic in &signature.generics {
            for bound in &generic.bounds {
                self.type_expr(bound);
            }
        }
        for parameter in &signature.parameters {
            if let Some(annotation) = &parameter.annotation {
                self.type_expr(annotation);
            }
            if let Some(default) = &parameter.default {
                self.expr(default);
            }
        }
        if let Some(result) = &signature.result {
            self.type_expr(result);
        }
    }

    /// Binds function parameters over `body`. A `self` parameter of an
    /// implementation member describes the owner declaration.
    fn parameters(&mut self, parameters: &[Parameter], body: &SyntaxSpan, owner: Option<&Hint>) {
        for parameter in parameters {
            let hint = match (&parameter.pattern, &parameter.annotation, owner) {
                (_, Some(annotation), _) => annotation_hint(self, annotation),
                (Pattern::Name(name, _), None, Some(owner)) if name == "self" => owner.clone(),
                _ => Hint::Unknown,
            };
            self.bind_pattern(
                &parameter.pattern,
                body,
                body.start,
                &parameter.span,
                LocalBindingKind::Parameter,
                &hint,
            );
        }
    }

    /// A table key parameter declares a key field of the table, not a local.
    fn key_field(&mut self, parameter: &Parameter) {
        if let Pattern::Name(name, span) = &parameter.pattern {
            self.declare_typed(
                name,
                span,
                DeclaredKind::KeyField,
                parameter.annotation.as_ref(),
            );
        }
        if let Some(annotation) = &parameter.annotation {
            self.type_expr(annotation);
        }
        if let Some(default) = &parameter.default {
            self.expr(default);
        }
    }

    fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        scope: &SyntaxSpan,
        visible_after: usize,
        context: &SyntaxSpan,
        kind: LocalBindingKind,
        hint: &Hint,
    ) {
        match pattern {
            Pattern::Name(name, span) => self.bindings.push(LocalBinding {
                name: name.clone(),
                key: key_of(name),
                selection: span.clone(),
                context: context.clone(),
                kind,
                hint: hint.clone(),
                scope: scope.clone(),
                visible_after,
            }),
            Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
                for element in elements {
                    self.bind_pattern(element, scope, visible_after, context, kind, &Hint::Unknown);
                }
            }
            Pattern::Record { fields, .. } => {
                for (name, field, span) in fields {
                    let member = Hint::Member(Box::new(hint.clone()), key_of(name));
                    match field {
                        Some(field) => {
                            if let Some(label) = self.first_identifier(span) {
                                self.occur(
                                    label,
                                    name,
                                    Role::PatternField {
                                        owner: hint.clone(),
                                    },
                                );
                            }
                            self.bind_pattern(field, scope, visible_after, context, kind, &member);
                        }
                        None => {
                            // `{ name }` both reads the field and binds it.
                            self.occur(
                                span.clone(),
                                name,
                                Role::PatternField {
                                    owner: hint.clone(),
                                },
                            );
                            self.bindings.push(LocalBinding {
                                name: name.clone(),
                                key: key_of(name),
                                selection: span.clone(),
                                context: context.clone(),
                                kind,
                                hint: member,
                                scope: scope.clone(),
                                visible_after,
                            });
                        }
                    }
                }
            }
            Pattern::Constructor {
                path,
                arguments,
                fields,
                ..
            } => {
                let keys = path.iter().map(|segment| key_of(&segment.text)).collect::<Vec<_>>();
                for (index, segment) in path.iter().enumerate() {
                    self.occur(
                        segment.span.clone(),
                        &segment.text,
                        Role::Path {
                            path: keys.clone(),
                            index,
                        },
                    );
                }
                let owner = Hint::Type(TypePath {
                    segments: path
                        .iter()
                        .map(|segment| (key_of(&segment.text), segment.span.clone()))
                        .collect(),
                });
                for argument in arguments {
                    self.bind_pattern(argument, scope, visible_after, context, kind, &Hint::Unknown);
                }
                for field in fields {
                    let member = Hint::Member(Box::new(owner.clone()), key_of(&field.name));
                    match &field.pattern {
                        Some(pattern) => {
                            if let Some(label) = self.first_identifier(&field.span) {
                                self.occur(
                                    label,
                                    &field.name,
                                    Role::PatternField {
                                        owner: owner.clone(),
                                    },
                                );
                            }
                            self.bind_pattern(pattern, scope, visible_after, context, kind, &member);
                        }
                        None => {
                            self.occur(
                                field.span.clone(),
                                &field.name,
                                Role::PatternField {
                                    owner: owner.clone(),
                                },
                            );
                            self.bindings.push(LocalBinding {
                                name: field.name.clone(),
                                key: key_of(&field.name),
                                selection: field.span.clone(),
                                context: context.clone(),
                                kind,
                                hint: member,
                                scope: scope.clone(),
                                visible_after,
                            });
                        }
                    }
                }
            }
            Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
        }
    }

    /// Names bound by a module-level `let`, which declares module values
    /// rather than locals. Only a plain name keeps the annotation's type.
    fn module_values(&mut self, pattern: &Pattern, full: &SyntaxSpan, ty: Option<&TypeExpr>) {
        match pattern {
            Pattern::Name(name, span) => {
                if let Some(index) = self.declare(name, span, full, DeclaredKind::Let) {
                    self.declarations[index].ty = ty.cloned();
                }
            }
            Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
                for element in elements {
                    self.module_values(element, full, None);
                }
            }
            Pattern::Record { fields, .. } => {
                for (name, field, span) in fields {
                    match field {
                        Some(field) => self.module_values(field, full, None),
                        None => {
                            self.declare(name, span, full, DeclaredKind::Let);
                        }
                    }
                }
            }
            Pattern::Constructor {
                arguments, fields, ..
            } => {
                for argument in arguments {
                    self.module_values(argument, full, None);
                }
                for field in fields {
                    match &field.pattern {
                        Some(pattern) => self.module_values(pattern, full, None),
                        None => {
                            self.declare(&field.name, &field.span, full, DeclaredKind::Let);
                        }
                    }
                }
            }
            Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
        }
    }

    fn type_expr(&mut self, ty: &TypeExpr) {
        match ty {
            TypeExpr::Name {
                path,
                arguments,
                span,
            } => {
                let keys = path.iter().map(|segment| key_of(segment)).collect::<Vec<_>>();
                let segments = self
                    .identifiers(span)
                    .take(path.len())
                    .map(|token| token.span.clone())
                    .collect::<Vec<_>>();
                if segments.len() == path.len() {
                    for (index, (segment, span)) in path.iter().zip(segments).enumerate() {
                        self.occur(
                            span,
                            segment,
                            Role::Path {
                                path: keys.clone(),
                                index,
                            },
                        );
                    }
                }
                for argument in arguments {
                    self.type_expr(argument);
                }
            }
            TypeExpr::Optional { inner, .. } | TypeExpr::List { inner, .. } => self.type_expr(inner),
            TypeExpr::Product { lhs, rhs, .. } => {
                self.type_expr(lhs);
                self.type_expr(rhs);
            }
            TypeExpr::Record { fields, .. } => {
                for (_, ty, _) in fields {
                    self.type_expr(ty);
                }
            }
            TypeExpr::Tuple { elements, .. } => {
                for element in elements {
                    self.type_expr(element);
                }
            }
            TypeExpr::Function {
                parameters, result, ..
            } => {
                for parameter in parameters {
                    self.type_expr(parameter);
                }
                self.type_expr(result);
            }
        }
    }

    fn expr(&mut self, expression: &Expr) {
        match expression {
            Expr::Name { text, span } => {
                self.occur(span.clone(), text, Role::Value { write: false });
            }
            Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
            Expr::InterpolatedString { segments, .. } => {
                for segment in segments {
                    if let StringSegment::Expression { value, .. } = segment {
                        self.expr(value);
                    }
                }
            }
            Expr::Unary { rhs, .. } => self.expr(rhs),
            Expr::Binary { lhs, op, rhs, .. } => {
                self.expr(lhs);
                if op == "|" {
                    let source = self.hint(lhs);
                    self.stage(rhs, &source);
                } else {
                    self.expr(rhs);
                }
            }
            Expr::Range { lower, upper, .. } => {
                if let Some(lower) = lower {
                    self.expr(lower);
                }
                if let Some(upper) = upper {
                    self.expr(upper);
                }
            }
            Expr::Call {
                callee, arguments, ..
            } => self.call(callee, arguments, None),
            Expr::GenericCall {
                callee,
                type_arguments,
                arguments,
                ..
            } => {
                for ty in type_arguments {
                    self.type_expr(ty);
                }
                self.call(callee, arguments, None);
            }
            Expr::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            Expr::Field { base, name, span } => {
                self.expr(base);
                let base_end = base.span().end;
                let name_span = SyntaxSpan::new(base_end, span.end);
                if let Some(name_span) = self.last_identifier(&name_span) {
                    let role = if matches!(
                        base.as_ref(),
                        Expr::Literal {
                            kind: LiteralKind::Integer | LiteralKind::Decimal | LiteralKind::Float,
                            ..
                        }
                    ) {
                        Role::UnitSuffix
                    } else {
                        Role::Member {
                            receiver: self.hint(base),
                            write: false,
                        }
                    };
                    self.occur(name_span, name, role);
                }
            }
            Expr::Group { inner, .. } => self.expr(inner),
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for element in elements {
                    self.expr(element);
                }
            }
            Expr::Record { fields, .. } => self.record_fields(fields, &Hint::Unknown),
            Expr::Nominal { path, fields, .. } => {
                let keys = path.iter().map(|segment| key_of(&segment.text)).collect::<Vec<_>>();
                for (index, segment) in path.iter().enumerate() {
                    self.occur(
                        segment.span.clone(),
                        &segment.text,
                        Role::Path {
                            path: keys.clone(),
                            index,
                        },
                    );
                }
                let owner = Hint::Type(TypePath {
                    segments: path
                        .iter()
                        .map(|segment| (key_of(&segment.text), segment.span.clone()))
                        .collect(),
                });
                self.record_fields(fields, &owner);
            }
            Expr::Lambda {
                parameters, body, ..
            } => self.lambda(parameters, body, &Hint::Unknown),
            Expr::Block {
                statements,
                tail,
                span,
            } => {
                for statement in statements {
                    self.statement(statement, span);
                }
                if let Some(tail) = tail {
                    self.expr(tail);
                }
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
                if let Some(condition) = condition {
                    self.expr(condition);
                }
                if *kind == ControlKind::For
                    && let (Some(pattern), Some(body)) = (binding, body)
                {
                    let element = condition.as_deref().map_or(Hint::Unknown, |iterable| {
                        Hint::Element(Box::new(self.hint(iterable)))
                    });
                    let pattern_span = pattern_span(pattern);
                    self.bind_pattern(
                        pattern,
                        &body.span(),
                        pattern_span.end,
                        &pattern_span,
                        LocalBindingKind::Pattern,
                        &element,
                    );
                }
                if let Some(body) = body {
                    self.expr(body);
                }
                for arm in control_arms(*kind, arms) {
                    let visible_after = pattern_span(&arm.pattern).end;
                    self.bind_pattern(
                        &arm.pattern,
                        &arm.span,
                        visible_after,
                        &arm.span,
                        LocalBindingKind::Pattern,
                        &Hint::Unknown,
                    );
                    if let Some(guard) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                }
                if let Some(alternate) = alternate {
                    self.expr(alternate);
                }
            }
        }
    }

    /// Visits one pipeline stage applied to a collection described by
    /// `source`: lambda parameters of the stage receive its elements.
    fn stage(&mut self, stage: &Expr, source: &Hint) {
        match stage {
            Expr::Call {
                callee, arguments, ..
            }
            | Expr::GenericCall {
                callee, arguments, ..
            } => {
                if let Expr::GenericCall { type_arguments, .. } = stage {
                    for ty in type_arguments {
                        self.type_expr(ty);
                    }
                }
                let element = Hint::Element(Box::new(source.clone()));
                self.call(callee, arguments, Some(&element));
            }
            other => self.expr(other),
        }
    }

    /// The span of the name a call's callee ends in.
    fn callee_name(&self, callee: &Expr) -> Option<SyntaxSpan> {
        match callee {
            Expr::Name { span, .. } => Some(span.clone()),
            Expr::Field { base, span, .. } => {
                self.last_identifier(&SyntaxSpan::new(base.span().end, span.end))
            }
            Expr::Group { inner, .. } => self.callee_name(inner),
            _ => None,
        }
    }

    /// Visits a call. `element` types the lambda parameters of a pipeline
    /// stage; record arguments of table operations are labelled by the table.
    fn call(&mut self, callee: &Expr, arguments: &[Argument], element: Option<&Hint>) {
        self.expr(callee);
        if let Some(name) = self.callee_name(callee)
            && let Some(occurrence) = self
                .occurrences
                .iter_mut()
                .rev()
                .find(|occurrence| same_span(&occurrence.span, &name))
        {
            occurrence.callee = true;
        }
        let callee_hint = self.hint(callee);
        let table_operation = match callee {
            Expr::Field { base, name, .. } if ROW_RETURNING.contains(&name.as_str()) => {
                Some(self.hint(base))
            }
            _ => None,
        };
        for argument in arguments {
            if let Some(label) = &argument.name
                && let Some(span) = self.first_identifier(&argument.span)
            {
                self.occur(
                    span,
                    label,
                    Role::ArgumentLabel {
                        callee: callee_hint.clone(),
                    },
                );
            }
            match (&argument.value, element, &table_operation) {
                (
                    Expr::Lambda {
                        parameters, body, ..
                    },
                    Some(element),
                    _,
                ) => self.lambda(parameters, body, element),
                (Expr::Record { fields, .. }, _, Some(table)) => {
                    self.record_fields(fields, &Hint::Element(Box::new(table.clone())));
                }
                (value, _, _) => self.expr(value),
            }
        }
    }

    fn lambda(
        &mut self,
        parameters: &[orna_syntax_v1::LambdaParameter],
        body: &Expr,
        element: &Hint,
    ) {
        let body_span = body.span();
        for parameter in parameters {
            let hint = match &parameter.annotation {
                Some(annotation) => annotation_hint(self, annotation),
                None if parameters.len() == 1 => element.clone(),
                None => Hint::Unknown,
            };
            if let Some(annotation) = &parameter.annotation {
                self.type_expr(annotation);
            }
            self.bind_pattern(
                &parameter.pattern,
                &body_span,
                body_span.start,
                &parameter.span,
                LocalBindingKind::Parameter,
                &hint,
            );
        }
        self.expr(body);
    }

    fn record_fields(&mut self, fields: &[RecordField], owner: &Hint) {
        for field in fields {
            if let Some(label) = self.first_identifier(&field.span) {
                self.occur(
                    label,
                    &field.name,
                    Role::RecordLabel {
                        owner: owner.clone(),
                    },
                );
            }
            self.expr(&field.value);
        }
    }

    fn statement(&mut self, statement: &Statement, block: &SyntaxSpan) {
        match statement {
            Statement::Let {
                pattern,
                annotation,
                value,
                span,
            } => {
                if let Some(annotation) = annotation {
                    self.type_expr(annotation);
                }
                self.expr(value);
                let hint = match annotation {
                    Some(annotation) => annotation_hint(self, annotation),
                    None => self.hint(value),
                };
                self.bind_pattern(pattern, block, span.end, span, LocalBindingKind::Local, &hint);
            }
            Statement::Assert { value, .. }
            | Statement::Expression { value, .. }
            | Statement::Control { value, .. } => self.expr(value),
            Statement::Return { value, .. } | Statement::Break { value, .. } => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            Statement::Assignment { target, value, .. } => {
                self.assignment_target(target);
                self.expr(value);
            }
            Statement::Continue { .. } => {}
        }
    }

    fn assignment_target(&mut self, target: &AssignmentTarget) {
        match target {
            AssignmentTarget::Name { name, span } => {
                self.occur(span.clone(), name, Role::Value { write: true });
            }
            AssignmentTarget::Field { base, name, span } => {
                self.assignment_target(base);
                let receiver = assignment_hint(base);
                if let Some(name_span) = self.last_identifier(span) {
                    self.occur(
                        name_span,
                        name,
                        Role::Member {
                            receiver,
                            write: true,
                        },
                    );
                }
            }
            AssignmentTarget::Index { base, index, .. } => {
                self.assignment_target(base);
                self.expr(index);
            }
        }
    }

    /// What `expression` evaluates to, as far as its source shape says.
    fn hint(&self, expression: &Expr) -> Hint {
        match expression {
            Expr::Name { text, span } => Hint::Name {
                key: key_of(text),
                span: span.clone(),
            },
            Expr::Group { inner, .. } => self.hint(inner),
            Expr::Field { base, name, .. } => Hint::Member(Box::new(self.hint(base)), key_of(name)),
            Expr::Call { callee, .. } | Expr::GenericCall { callee, .. } => {
                if let Expr::Field { base, name, .. } = callee.as_ref()
                    && ROW_RETURNING.contains(&name.as_str())
                {
                    return Hint::Element(Box::new(self.hint(base)));
                }
                Hint::Call(Box::new(self.hint(callee)))
            }
            Expr::Binary { lhs, op, rhs, .. } if op == "|" => {
                let operation = match rhs.as_ref() {
                    Expr::Call { callee, .. } | Expr::GenericCall { callee, .. } => {
                        match callee.as_ref() {
                            Expr::Name { text, .. } => Some(text.as_str()),
                            _ => None,
                        }
                    }
                    Expr::Name { text, .. } => Some(text.as_str()),
                    _ => None,
                };
                match operation {
                    Some(operation) if ELEMENT_PRESERVING.contains(&operation) => self.hint(lhs),
                    Some(operation) if ELEMENT_RETURNING.contains(&operation) => {
                        Hint::Element(Box::new(self.hint(lhs)))
                    }
                    _ => Hint::Unknown,
                }
            }
            Expr::Nominal { path, .. } => Hint::Type(TypePath {
                segments: path
                    .iter()
                    .map(|segment| (key_of(&segment.text), segment.span.clone()))
                    .collect(),
            }),
            Expr::Block { tail: Some(tail), .. } => self.hint(tail),
            _ => Hint::Unknown,
        }
    }
}

fn assignment_hint(target: &AssignmentTarget) -> Hint {
    match target {
        AssignmentTarget::Name { name, span } => Hint::Name {
            key: key_of(name),
            span: span.clone(),
        },
        AssignmentTarget::Field { base, name, .. } => {
            Hint::Member(Box::new(assignment_hint(base)), key_of(name))
        }
        AssignmentTarget::Index { base, .. } => Hint::Element(Box::new(assignment_hint(base))),
    }
}

/// The value a binding annotated with `ty` holds.
fn annotation_hint(walker: &Walker<'_>, ty: &TypeExpr) -> Hint {
    match ty {
        TypeExpr::Name { path, span, .. } => {
            let segments = walker
                .identifiers(span)
                .take(path.len())
                .map(|token| token.span.clone())
                .collect::<Vec<_>>();
            if segments.len() != path.len() {
                return Hint::Unknown;
            }
            Hint::Type(TypePath {
                segments: path
                    .iter()
                    .map(|segment| key_of(segment))
                    .zip(segments)
                    .collect(),
            })
        }
        // Optional member access reads the same members as the inner type.
        TypeExpr::Optional { inner, .. } => annotation_hint(walker, inner),
        TypeExpr::List { inner, .. } => Hint::Collection(Box::new(annotation_hint(walker, inner))),
        _ => Hint::Unknown,
    }
}

fn is_name_token(token: &Token) -> bool {
    matches!(
        token.kind,
        TokenKind::Identifier { .. } | TokenKind::Keyword(orna_syntax_v1::Keyword::SelfValue)
    )
}

/// The normalised key of an identifier token.
pub(crate) fn token_key(token: &Token) -> Key {
    match &token.kind {
        TokenKind::Identifier { normalized } => normalized.clone(),
        _ => token.text.clone(),
    }
}

fn pattern_span(pattern: &Pattern) -> SyntaxSpan {
    match pattern {
        Pattern::Name(_, span) | Pattern::Wildcard(span) | Pattern::Literal { span, .. } => {
            span.clone()
        }
        Pattern::Tuple { span, .. }
        | Pattern::List { span, .. }
        | Pattern::Record { span, .. }
        | Pattern::Constructor { span, .. } => span.clone(),
    }
}
