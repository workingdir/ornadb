//! Implementation-owned declaration meaning, not catalogue object/revision identity.
//!
//! Version 1 bytes are ASCII `SEMANTIC_PAYLOAD_DOMAIN`, NUL, then OVB-1
//! `[version, declaration_kind, meaning]`. All nodes below are positional arrays
//! with an explicit textual discriminator. Declaration targets embed these exact
//! bytes, never a snapshot-local name or a digest standing in for equality.
//! Source locations, source hashes and annotation explicitness are kept outside
//! this encoding (ORNA-SYS-034/037/038, reference §15 lines 120–140).
//! The domain/layout is an implementation contract, NOT one of the fixed domains
//! in reference §29 lines 65–80. No RevisionId is produced (§29 lines 146–148).

use super::*;
use orna_foundation_v1::{OvbRaw as Raw, Value};

pub const SEMANTIC_PAYLOAD_DOMAIN: &str = "ornadb.semantic-declaration.v1";
pub const SEMANTIC_PAYLOAD_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticDeclarationKind {
    Function,
    Type,
}

/// Local half-open UTF-8 offsets; this is deliberately not a sys.SourceSpan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalSourceOrigin {
    pub logical_path: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedSemanticParameter {
    pub position: usize,
    pub name: String,
    pub resolved_type: Type,
    pub has_default: bool,
    pub annotation_explicit: bool,
}

/// Authorship is provenance, not a component of semantic identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationExplicitness {
    pub authored: bool,
    pub parameter_annotations: Vec<bool>,
    pub result_annotation: bool,
    /// Owning declaration field/payload annotations, in declaration order.
    pub field_annotations: Vec<bool>,
}

/// Only this producer can construct a declaration. Getters expose no mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticDeclaration {
    qualified_name: String,
    kind: SemanticDeclarationKind,
    canonical_payload: Vec<u8>,
    semantic_hash: [u8; 32],
    resolved_type: Type,
    parameters: Vec<NamedSemanticParameter>,
    effects: EffectSummary,
    origin: LocalSourceOrigin,
    source_hash: [u8; 32],
    explicitness: DeclarationExplicitness,
}

impl SemanticDeclaration {
    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }
    pub fn kind(&self) -> SemanticDeclarationKind {
        self.kind
    }
    /// Complete versioned hash input, including domain and NUL delimiter.
    pub fn canonical_payload(&self) -> &[u8] {
        &self.canonical_payload
    }
    pub fn semantic_hash(&self) -> &[u8; 32] {
        &self.semantic_hash
    }
    /// Exact checker form, never a flattened name descriptor.
    pub fn resolved_type(&self) -> &Type {
        &self.resolved_type
    }
    pub fn parameters(&self) -> &[NamedSemanticParameter] {
        &self.parameters
    }
    /// Conservative effect set and may_fail, not an invented precise failure set.
    pub fn effects(&self) -> &EffectSummary {
        &self.effects
    }
    pub fn origin(&self) -> &LocalSourceOrigin {
        &self.origin
    }
    pub fn source_hash(&self) -> &[u8; 32] {
        &self.source_hash
    }
    pub fn explicitness(&self) -> &DeclarationExplicitness {
        &self.explicitness
    }
    pub fn revision_id(&self) -> Result<[u8; 32], SemanticPayloadError> {
        Err(self.identity_error())
    }
    pub fn object_id(&self) -> Result<[u8; 16], SemanticPayloadError> {
        Err(self.identity_error())
    }
    fn identity_error(&self) -> SemanticPayloadError {
        SemanticPayloadError {
            declaration: Some(self.qualified_name.clone()),
            origin: Some(self.origin.clone()),
            kind: SemanticPayloadErrorKind::IdentityEvidenceRequired,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SemanticPayloadAnalysis {
    analysis: Analysis,
    declarations: Vec<SemanticDeclaration>,
}
impl SemanticPayloadAnalysis {
    pub fn analysis(&self) -> &Analysis {
        &self.analysis
    }
    pub fn declarations(&self) -> &[SemanticDeclaration] {
        &self.declarations
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticPayloadErrorKind {
    AnalysisFailed {
        diagnostic_codes: Vec<String>,
    },
    UnsupportedType {
        ty: Box<Type>,
    },
    NonNameParameter {
        position: usize,
    },
    ParameterNamesUnavailable,
    OpenGenericDeclaration,
    /// A fixture/header has no canonical source declaration meaning.
    MissingDeclarationEvidence {
        target: String,
    },
    /// Type::Named cannot distinguish value types from table reference types.
    ReferenceTypeEvidenceRequired {
        target: String,
    },
    /// Embedding exact meaning cannot encode a cyclic graph without identity evidence.
    RecursiveDeclaration {
        target: String,
    },
    UnsupportedDeclaration {
        category: &'static str,
    },
    UnsupportedExpression {
        form: &'static str,
    },
    UnsupportedPattern,
    InvalidLiteral {
        kind: LiteralKind,
    },
    IncompleteResolvedMeaning,
    CanonicalEncoding,
    IdentityEvidenceRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticPayloadError {
    pub declaration: Option<String>,
    pub origin: Option<LocalSourceOrigin>,
    pub kind: SemanticPayloadErrorKind,
}
impl std::fmt::Display for SemanticPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Names and source contents are not interpolated into public diagnostics.
        f.write_str("source declaration has no complete canonical semantic payload")
    }
}
impl std::error::Error for SemanticPayloadError {}

type Result<T, E = SemanticPayloadError> = std::result::Result<T, E>;
type Key = (Namespace, String);

/// Analyze once and produce every source type/function declaration, or fail
/// atomically. Supplied catalogue summaries are never promoted to evidence.
/// Unsupported closed-type shapes are rejected even when ordinary analysis is
/// successful. Existing analyze APIs retain their existing permissive scope.
pub fn analyze_semantic_payloads(
    inputs: &[ModuleInput],
    catalogue: &Catalogue,
) -> Result<SemanticPayloadAnalysis> {
    let (analysis, contexts) = analyze_retaining_context(inputs, catalogue, true);
    if !analysis.is_ok() {
        return Err(SemanticPayloadError {
            declaration: None,
            origin: None,
            kind: SemanticPayloadErrorKind::AnalysisFailed {
                diagnostic_codes: analysis
                    .diagnostics
                    .iter()
                    .map(|d| d.code().to_owned())
                    .collect(),
            },
        });
    }
    let mut producer = Producer {
        analysis: &analysis,
        contexts: &contexts,
        inputs,
        active: BTreeSet::new(),
        completed: BTreeMap::new(),
    };
    for (namespace, tree, _) in &contexts {
        for item in &tree.items {
            if let Some((name, kind, _)) = declared_symbol(item)
                && matches!(
                    kind,
                    SymbolKind::Function
                        | SymbolKind::Type
                        | SymbolKind::Enum
                        | SymbolKind::Protocol
                        | SymbolKind::Dimension
                        | SymbolKind::Unit
                )
            {
                producer.declaration(&(namespace.clone(), name))?;
            }
        }
    }
    let declarations = producer.completed.into_values().collect();
    Ok(SemanticPayloadAnalysis {
        analysis,
        declarations,
    })
}

struct Producer<'a> {
    analysis: &'a Analysis,
    contexts: &'a [(Namespace, SyntaxTree, Scope)],
    inputs: &'a [ModuleInput],
    active: BTreeSet<Key>,
    completed: BTreeMap<Key, SemanticDeclaration>,
}

fn text(value: &str) -> Raw {
    Raw::Text(value.nfc().collect())
}
fn array(values: Vec<Raw>) -> Raw {
    Raw::Array(values)
}
fn node(tag: &str, values: Vec<Raw>) -> Raw {
    let mut parts = Vec::with_capacity(values.len() + 1);
    parts.push(text(tag));
    parts.extend(values);
    array(parts)
}
fn index(value: usize) -> Raw {
    Raw::Int(value.into())
}
fn origin(item: &Item) -> LocalSourceOrigin {
    LocalSourceOrigin {
        logical_path: item.span.file.clone().unwrap_or_default(),
        start: item.span.start,
        end: item.span.end,
    }
}

impl Producer<'_> {
    fn error(&self, key: &Key, kind: SemanticPayloadErrorKind) -> SemanticPayloadError {
        SemanticPayloadError {
            declaration: Some(nominal_identity(&key.0, &key.1)),
            origin: self.item(key).map(origin),
            kind,
        }
    }
    fn item(&self, key: &Key) -> Option<&Item> {
        self.contexts
            .iter()
            .find(|(ns, _, _)| ns == &key.0)?
            .1
            .items
            .iter()
            .find(|item| declared_symbol(item).is_some_and(|(name, _, _)| name == key.1))
    }
    fn declaration(&mut self, key: &Key) -> Result<Vec<u8>> {
        if let Some(done) = self.completed.get(key) {
            return Ok(done.canonical_payload.clone());
        }
        if !self.active.insert(key.clone()) {
            return Err(self.error(
                key,
                SemanticPayloadErrorKind::RecursiveDeclaration {
                    target: nominal_identity(&key.0, &key.1),
                },
            ));
        }
        // Keep independent immutable AST/scope borrows while recursively filling the cache.
        let contexts = self.contexts;
        let (_, tree, scope) =
            contexts
                .iter()
                .find(|(ns, _, _)| ns == &key.0)
                .ok_or_else(|| {
                    self.error(
                        key,
                        SemanticPayloadErrorKind::MissingDeclarationEvidence {
                            target: nominal_identity(&key.0, &key.1),
                        },
                    )
                })?;
        let item = tree
            .items
            .iter()
            .find(|item| declared_symbol(item).is_some_and(|(name, _, _)| name == key.1))
            .ok_or_else(|| {
                self.error(
                    key,
                    SemanticPayloadErrorKind::MissingDeclarationEvidence {
                        target: nominal_identity(&key.0, &key.1),
                    },
                )
            })?;
        let analysis = self.analysis;
        let symbol = &analysis.modules[&key.0].symbols[&key.1];
        let mut explicitness = DeclarationExplicitness {
            authored: true,
            parameter_annotations: Vec::new(),
            result_annotation: false,
            field_annotations: Vec::new(),
        };
        let mut parameters = Vec::new();
        let (kind, resolved_type, meaning) = match &item.declaration {
            Declaration::Function { signature, body } => {
                if !signature.generics.is_empty() {
                    return Err(self.error(key, SemanticPayloadErrorKind::OpenGenericDeclaration));
                }
                for (position, parameter) in signature.parameters.iter().enumerate() {
                    match &parameter.pattern {
                        // `_` parses as Wildcard; an identifier starting with
                        // `_` is a name pattern the checker reports verbatim.
                        // Neither supplies a callable parameter name, so both
                        // fail closed instead of embedding a discard marker.
                        Pattern::Name(name, _) if !name.starts_with('_') => {}
                        _ => {
                            return Err(self.error(
                                key,
                                SemanticPayloadErrorKind::NonNameParameter { position },
                            ));
                        }
                    }
                }
                let Type::Function {
                    parameter_names: Some(names),
                    result,
                    ..
                } = &symbol.ty
                else {
                    return Err(
                        self.error(key, SemanticPayloadErrorKind::ParameterNamesUnavailable)
                    );
                };
                if names.len() != signature.parameters.len() {
                    return Err(
                        self.error(key, SemanticPayloadErrorKind::IncompleteResolvedMeaning)
                    );
                }
                let mut locals = Locals::default();
                let mut encoded_parameters = Vec::new();
                for (position, parameter) in signature.parameters.iter().enumerate() {
                    let ty = parameter
                        .annotation
                        .as_ref()
                        .map(|ty| resolved_type_of(ty, scope))
                        .or_else(|| inferred_function_parameter_type(body, parameter))
                        .ok_or_else(|| {
                            self.error(key, SemanticPayloadErrorKind::IncompleteResolvedMeaning)
                        })?;
                    let type_node = self.type_node(key, &ty, scope)?;
                    let default =
                        self.optional_expr(key, parameter.default.as_ref(), scope, &mut locals)?;
                    let name = names[position].nfc().collect::<String>();
                    encoded_parameters.push(array(vec![
                        index(position),
                        text(&name),
                        type_node,
                        Raw::Bool(parameter.default.is_some()),
                        default,
                    ]));
                    locals.bind(&name, ty.clone());
                    parameters.push(NamedSemanticParameter {
                        position,
                        name,
                        resolved_type: ty,
                        has_default: parameter.default.is_some(),
                        annotation_explicit: parameter.annotation.is_some(),
                    });
                    explicitness
                        .parameter_annotations
                        .push(parameter.annotation.is_some());
                }
                let result = signature
                    .result
                    .as_ref()
                    .map(|ty| resolved_type_of(ty, scope))
                    .unwrap_or_else(|| {
                        canonicalize_type(
                            &resolve_type_aliases(
                                result,
                                &scope.type_aliases,
                                &mut BTreeSet::new(),
                            ),
                            scope,
                        )
                    });
                let result_node = self.type_node(key, &result, scope)?;
                explicitness.result_annotation = signature.result.is_some();
                let body = self.expr(key, body, scope, &mut locals)?;
                let resolved = Type::Function {
                    parameters: parameters.iter().map(|p| p.resolved_type.clone()).collect(),
                    parameter_names: Some(parameters.iter().map(|p| p.name.clone()).collect()),
                    default_parameters: parameters
                        .iter()
                        .filter(|p| p.has_default)
                        .map(|p| p.position)
                        .collect(),
                    result: Box::new(result),
                };
                (
                    SemanticDeclarationKind::Function,
                    resolved,
                    node(
                        "function_meaning",
                        vec![
                            array(Vec::new()),
                            array(encoded_parameters),
                            result_node,
                            array(symbol.effects.effects.iter().map(|s| text(s)).collect()),
                            Raw::Bool(symbol.effects.may_fail),
                            body,
                        ],
                    ),
                )
            }
            Declaration::Type {
                generics,
                representation,
                ..
            } => {
                if !generics.is_empty() {
                    return Err(self.error(key, SemanticPayloadErrorKind::OpenGenericDeclaration));
                }
                let mut locals = Locals::default();
                let (resolved, representation) = match representation {
                    TypeRepresentation::Alias { ty, refinements } => {
                        let resolved = resolved_type_of(ty, scope);
                        let encoded = self.type_node(key, &resolved, scope)?;
                        locals.bind("self", resolved.clone());
                        let members = self.type_members(
                            key,
                            refinements,
                            scope,
                            &mut locals,
                            &mut explicitness,
                        )?;
                        let category = if refinements.is_empty() {
                            "alias"
                        } else {
                            "refined"
                        };
                        let exact = if refinements.is_empty() {
                            resolved
                        } else {
                            Type::Named(nominal_identity(&key.0, &key.1))
                        };
                        (exact, node(category, vec![encoded, members]))
                    }
                    TypeRepresentation::Nominal { members } => {
                        let fields = members
                            .iter()
                            .filter_map(|m| match m {
                                TypeMember::Field { name, ty, .. } => {
                                    Some((name.clone(), resolved_type_of(ty, scope)))
                                }
                                _ => None,
                            })
                            .collect();
                        locals.bind("self", Type::Record(fields));
                        let members =
                            self.type_members(key, members, scope, &mut locals, &mut explicitness)?;
                        (
                            Type::Named(nominal_identity(&key.0, &key.1)),
                            node("nominal_record", vec![members]),
                        )
                    }
                };
                (
                    SemanticDeclarationKind::Type,
                    resolved,
                    node("type_meaning", vec![representation]),
                )
            }
            Declaration::Enum {
                generics, variants, ..
            } => {
                if !generics.is_empty() {
                    return Err(self.error(key, SemanticPayloadErrorKind::OpenGenericDeclaration));
                }
                let mut encoded = Vec::new();
                for variant in variants {
                    let mut fields = Vec::new();
                    for field in &variant.fields {
                        let ty = resolved_type_of(&field.ty, scope);
                        fields.push(array(vec![
                            text(&field.name),
                            self.type_node(key, &ty, scope)?,
                        ]));
                        explicitness.field_annotations.push(true);
                    }
                    encoded.push(array(vec![text(&variant.name), array(fields)]));
                }
                (
                    SemanticDeclarationKind::Type,
                    Type::Named(nominal_identity(&key.0, &key.1)),
                    node("type_meaning", vec![node("enum", vec![array(encoded)])]),
                )
            }
            Declaration::Table { .. } => {
                return Err(self.error(
                    key,
                    SemanticPayloadErrorKind::ReferenceTypeEvidenceRequired {
                        target: key.1.clone(),
                    },
                ));
            }
            other => {
                return Err(self.error(
                    key,
                    SemanticPayloadErrorKind::UnsupportedDeclaration {
                        category: match other {
                            Declaration::Protocol { .. } => "protocol",
                            Declaration::Dimension { .. } => "dimension",
                            Declaration::Unit { .. } => "unit",
                            Declaration::Let { .. } => "module_binding",
                            _ => "non_declaration",
                        },
                    },
                ));
            }
        };
        let raw = array(vec![
            Raw::Int(SEMANTIC_PAYLOAD_VERSION.into()),
            text(match kind {
                SemanticDeclarationKind::Function => "function",
                SemanticDeclarationKind::Type => "type",
            }),
            array(vec![Raw::Bool(symbol.public), meaning]),
        ]);
        let encoded = Value::new(raw)
            .and_then(|value| value.encode())
            .map_err(|_| self.error(key, SemanticPayloadErrorKind::CanonicalEncoding))?;
        let mut canonical_payload =
            Vec::with_capacity(SEMANTIC_PAYLOAD_DOMAIN.len() + 1 + encoded.len());
        canonical_payload.extend_from_slice(SEMANTIC_PAYLOAD_DOMAIN.as_bytes());
        canonical_payload.push(0);
        canonical_payload.extend_from_slice(&encoded);
        let local_origin = origin(item);
        let source = self
            .inputs
            .iter()
            .find(|input| input.logical_path == local_origin.logical_path)
            .ok_or_else(|| self.error(key, SemanticPayloadErrorKind::IncompleteResolvedMeaning))?;
        let declaration = SemanticDeclaration {
            qualified_name: nominal_identity(&key.0, &key.1),
            kind,
            semantic_hash: Sha256::digest(&canonical_payload).into(),
            canonical_payload: canonical_payload.clone(),
            resolved_type,
            parameters,
            effects: symbol.effects.clone(),
            origin: local_origin,
            source_hash: digest_source(&source.source),
            explicitness,
        };
        self.completed.insert(key.clone(), declaration);
        self.active.remove(key);
        Ok(canonical_payload)
    }

    fn type_members(
        &mut self,
        key: &Key,
        members: &[TypeMember],
        scope: &Scope,
        locals: &mut Locals,
        flags: &mut DeclarationExplicitness,
    ) -> Result<Raw> {
        let mut encoded = Vec::new();
        for member in members {
            encoded.push(match member {
                TypeMember::Field {
                    visibility,
                    name,
                    ty,
                    initializer,
                    ..
                } => {
                    flags.field_annotations.push(true);
                    let ty = resolved_type_of(ty, scope);
                    // Nominal value fields have no table key role; encode that explicitly.
                    node(
                        "field",
                        vec![
                            text(name),
                            text("value"),
                            Raw::Bool(*visibility),
                            self.type_node(key, &ty, scope)?,
                            Raw::Bool(initializer.is_some()),
                            self.optional_expr(key, initializer.as_ref(), scope, locals)?,
                        ],
                    )
                }
                TypeMember::Assertion { value, .. } => {
                    node("assertion", vec![self.expr(key, value, scope, locals)?])
                }
                TypeMember::Implementation { .. } => {
                    return Err(self.error(
                        key,
                        SemanticPayloadErrorKind::UnsupportedDeclaration {
                            category: "implementation",
                        },
                    ));
                }
            });
        }
        Ok(array(encoded))
    }

    fn type_node(&mut self, key: &Key, ty: &Type, scope: &Scope) -> Result<Raw> {
        let primitive = match ty {
            Type::Int => "Int",
            Type::Decimal => "Decimal",
            Type::Float => "Float",
            Type::Date => "Date",
            Type::Instant => "Instant",
            Type::Text => "Str",
            Type::Bool => "Bool",
            Type::Null => "Null",
            Type::Bottom => "Bottom",
            Type::Named(name) => {
                let target = self.target(key, name, scope)?;
                let target_item = self.item(&target).ok_or_else(|| {
                    self.error(
                        key,
                        SemanticPayloadErrorKind::MissingDeclarationEvidence {
                            target: name.clone(),
                        },
                    )
                })?;
                if matches!(target_item.declaration, Declaration::Table { .. }) {
                    return Err(self.error(
                        key,
                        SemanticPayloadErrorKind::ReferenceTypeEvidenceRequired {
                            target: name.clone(),
                        },
                    ));
                }
                if !matches!(
                    target_item.declaration,
                    Declaration::Type { .. } | Declaration::Enum { .. }
                ) {
                    return Err(self.error(
                        key,
                        SemanticPayloadErrorKind::UnsupportedType {
                            ty: Box::new(ty.clone()),
                        },
                    ));
                }
                return Ok(node(
                    "named_type",
                    vec![Raw::Bytes(self.declaration(&target)?)],
                ));
            }
            _ => {
                return Err(self.error(
                    key,
                    SemanticPayloadErrorKind::UnsupportedType {
                        ty: Box::new(ty.clone()),
                    },
                ));
            }
        };
        Ok(node("primitive", vec![text(primitive)]))
    }

    /// Resolve only through the exact checked scope and authored import routes.
    /// Equality of Symbol summaries is never used to choose a declaration.
    fn target(&self, key: &Key, spelling: &str, scope: &Scope) -> Result<Key> {
        let missing = || {
            self.error(
                key,
                SemanticPayloadErrorKind::MissingDeclarationEvidence {
                    target: spelling.into(),
                },
            )
        };
        if let Some((namespace, name)) = spelling.rsplit_once('.') {
            let direct = (
                Namespace(namespace.split('.').map(str::to_owned).collect()),
                name.to_owned(),
            );
            if scope
                .nominal_identities
                .values()
                .any(|value| value == spelling)
                && self.item(&direct).is_some()
            {
                return Ok(direct);
            }
            let mut parts = spelling.split('.');
            let root = parts.next().ok_or_else(missing)?;
            if let Some(namespace) = scope.modules.get(root) {
                let mut full = namespace.0.clone();
                full.extend(parts.map(str::to_owned));
                let name = full.pop().ok_or_else(missing)?;
                let namespace = Namespace(full);
                if scope
                    .available_modules
                    .get(&namespace)
                    .is_some_and(|m| m.exports.contains_key(&name))
                {
                    return Ok((namespace, name));
                }
            }
            return Err(missing());
        }
        let local = (key.0.clone(), spelling.to_owned());
        if self.item(&local).is_some() {
            return Ok(local);
        }
        let (_, tree, _) = self
            .contexts
            .iter()
            .find(|(ns, _, _)| ns == &key.0)
            .ok_or_else(missing)?;
        let mut explicit = Vec::new();
        let mut glob = Vec::new();
        for item in &tree.items {
            let Declaration::Use { path, tail } = &item.declaration else {
                continue;
            };
            let namespace = Namespace(path.iter().map(|p| p.name.clone()).collect());
            let Some(module) = scope.available_modules.get(&namespace) else {
                continue;
            };
            if !module.exports.contains_key(spelling) {
                continue;
            }
            match tail {
                UseTail::Names(names) if names.iter().any(|n| n.name == spelling) => {
                    explicit.push((namespace, spelling.into()))
                }
                UseTail::Alias { name, .. }
                    if name == "_" && module.prelude_exports.contains(spelling) =>
                {
                    explicit.push((namespace, spelling.into()))
                }
                UseTail::Glob { .. } => glob.push((namespace, spelling.into())),
                _ => {}
            }
        }
        let mut candidates = if explicit.is_empty() { glob } else { explicit };
        if candidates.len() == 1 && scope.names.contains_key(spelling) {
            Ok(candidates.remove(0))
        } else {
            Err(missing())
        }
    }

    fn optional_expr(
        &mut self,
        key: &Key,
        expr: Option<&Expr>,
        scope: &Scope,
        locals: &mut Locals,
    ) -> Result<Raw> {
        expr.map(|e| self.expr(key, e, scope, locals))
            .transpose()
            .map(|v| v.unwrap_or(Raw::Null))
    }

    fn expr(&mut self, key: &Key, expr: &Expr, scope: &Scope, locals: &mut Locals) -> Result<Raw> {
        Ok(match expr {
            Expr::Group { inner, .. } => return self.expr(key, inner, scope, locals),
            Expr::Literal { text, kind, .. } => {
                node("literal", vec![self.literal(key, text, *kind)?])
            }
            Expr::Name { text: name, .. } => {
                if let Some((position, _)) = locals.bindings.get(name) {
                    node("local", vec![index(*position)])
                } else {
                    let target = self.target(key, name, scope)?;
                    node("declaration", vec![Raw::Bytes(self.declaration(&target)?)])
                }
            }
            Expr::Unary { op, rhs, .. } => {
                node("unary", vec![text(op), self.expr(key, rhs, scope, locals)?])
            }
            Expr::Binary { lhs, op, rhs, .. } => node(
                "binary",
                vec![
                    text(op),
                    self.expr(key, lhs, scope, locals)?,
                    self.expr(key, rhs, scope, locals)?,
                ],
            ),
            Expr::Call {
                callee, arguments, ..
            } => {
                let callee = self.expr(key, callee, scope, locals)?;
                let mut args = Vec::new();
                for argument in arguments {
                    args.push(array(vec![
                        argument.name.as_deref().map(text).unwrap_or(Raw::Null),
                        self.expr(key, &argument.value, scope, locals)?,
                    ]));
                }
                node("call", vec![callee, array(args)])
            }
            Expr::Field { base, name, .. } => {
                if let Some(path) = qualified_path(expr)
                    && !locals.bindings.contains_key(path[0])
                    && scope.modules.contains_key(path[0])
                {
                    let target = self.target(key, &path.join("."), scope)?;
                    node("declaration", vec![Raw::Bytes(self.declaration(&target)?)])
                } else {
                    node(
                        "field",
                        vec![self.expr(key, base, scope, locals)?, text(name)],
                    )
                }
            }
            Expr::Index {
                base, index: at, ..
            } => node(
                "index",
                vec![
                    self.expr(key, base, scope, locals)?,
                    self.expr(key, at, scope, locals)?,
                ],
            ),
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                let values = elements
                    .iter()
                    .map(|e| self.expr(key, e, scope, locals))
                    .collect::<Result<Vec<_>>>()?;
                node(
                    if matches!(expr, Expr::Tuple { .. }) {
                        "tuple"
                    } else {
                        "list"
                    },
                    vec![array(values)],
                )
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                let target = if let Expr::Nominal { path, .. } = expr {
                    let target = self.target(
                        key,
                        &path
                            .iter()
                            .map(|p| p.text.as_str())
                            .collect::<Vec<_>>()
                            .join("."),
                        scope,
                    )?;
                    Raw::Bytes(self.declaration(&target)?)
                } else {
                    Raw::Null
                };
                let mut values = Vec::new();
                for field in fields {
                    values.push(array(vec![
                        text(&field.name),
                        self.expr(key, &field.value, scope, locals)?,
                    ]));
                }
                node("record", vec![target, array(values)])
            }
            Expr::Range {
                lower,
                operator,
                upper,
                ..
            } => node(
                "range",
                vec![
                    text(operator),
                    self.optional_expr(key, lower.as_deref(), scope, locals)?,
                    self.optional_expr(key, upper.as_deref(), scope, locals)?,
                ],
            ),
            Expr::Block {
                statements, tail, ..
            } => {
                let mut block = locals.clone();
                let mut values = Vec::new();
                for statement in statements {
                    values.push(self.statement(key, statement, scope, &mut block)?);
                }
                node(
                    "block",
                    vec![
                        array(values),
                        self.optional_expr(key, tail.as_deref(), scope, &mut block)?,
                    ],
                )
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
                let condition_node =
                    self.optional_expr(key, condition.as_deref(), scope, locals)?;
                let mut branch = locals.clone();
                let binding_node = if let Some(pattern) = binding {
                    let condition = condition.as_deref().ok_or_else(|| {
                        self.error(key, SemanticPayloadErrorKind::IncompleteResolvedMeaning)
                    })?;
                    let ty = self.inferred_type(key, condition, scope, locals)?;
                    let ty = match (kind, ty) {
                        (ControlKind::For, Type::List(inner) | Type::Range(inner)) => *inner,
                        (ControlKind::If, Type::Optional(inner)) => *inner,
                        (_, ty) => ty,
                    };
                    self.pattern(key, pattern, &ty, scope, &mut branch)?
                } else {
                    Raw::Null
                };
                let body_node = self.optional_expr(key, body.as_deref(), scope, &mut branch)?;
                let mut encoded_arms = Vec::new();
                for arm in arms {
                    let mut arm_locals = locals.clone();
                    let subject = condition.as_deref().ok_or_else(|| {
                        self.error(key, SemanticPayloadErrorKind::IncompleteResolvedMeaning)
                    })?;
                    let ty = self.inferred_type(key, subject, scope, locals)?;
                    encoded_arms.push(array(vec![
                        self.pattern(key, &arm.pattern, &ty, scope, &mut arm_locals)?,
                        self.optional_expr(key, arm.guard.as_ref(), scope, &mut arm_locals)?,
                        self.expr(key, &arm.body, scope, &mut arm_locals)?,
                    ]));
                }
                node(
                    "control",
                    vec![
                        text(match kind {
                            ControlKind::If => "if",
                            ControlKind::Case => "case",
                            ControlKind::For => "for",
                            ControlKind::While => "while",
                            ControlKind::Loop => "loop",
                        }),
                        binding_node,
                        condition_node,
                        body_node,
                        array(encoded_arms),
                        self.optional_expr(key, alternate.as_deref(), scope, &mut locals.clone())?,
                    ],
                )
            }
            Expr::InterpolatedString { .. }
            | Expr::GenericCall { .. }
            | Expr::Lambda { .. }
            | Expr::ReplBinding { .. } => {
                return Err(self.error(
                    key,
                    SemanticPayloadErrorKind::UnsupportedExpression {
                        form: match expr {
                            Expr::InterpolatedString { .. } => "interpolated_string",
                            Expr::GenericCall { .. } => "generic_call",
                            Expr::Lambda { .. } => "lambda",
                            _ => "repl_binding",
                        },
                    },
                ));
            }
        })
    }

    fn inferred_type(
        &self,
        key: &Key,
        expr: &Expr,
        scope: &Scope,
        locals: &Locals,
    ) -> Result<Type> {
        let symbols = locals
            .bindings
            .iter()
            .map(|(name, (_, ty))| (name.clone(), fixture_symbol(SymbolKind::Let, ty.clone())))
            .collect();
        let mut diagnostics = Vec::new();
        let inferred = infer(expr, scope, &symbols, &mut diagnostics);
        if !diagnostics.is_empty() || type_contains_error(&inferred.ty) {
            Err(self.error(key, SemanticPayloadErrorKind::IncompleteResolvedMeaning))
        } else {
            Ok(inferred.ty)
        }
    }

    fn pattern(
        &mut self,
        key: &Key,
        pattern: &Pattern,
        ty: &Type,
        scope: &Scope,
        locals: &mut Locals,
    ) -> Result<Raw> {
        Ok(match pattern {
            Pattern::Name(name, _) => {
                let ty_node = self.pattern_type_node(key, ty, scope)?;
                node("bind", vec![index(locals.bind(name, ty.clone())), ty_node])
            }
            Pattern::Wildcard(_) => node("wildcard", Vec::new()),
            Pattern::Literal { text, kind, .. } => {
                node("literal_pattern", vec![self.literal(key, text, *kind)?])
            }
            Pattern::Tuple { elements, .. } => {
                let Type::Tuple(expected) = ty else {
                    return Err(self.error(key, SemanticPayloadErrorKind::UnsupportedPattern));
                };
                if elements.len() != expected.len() {
                    return Err(self.error(key, SemanticPayloadErrorKind::UnsupportedPattern));
                }
                let values = elements
                    .iter()
                    .zip(expected)
                    .map(|(pattern, ty)| self.pattern(key, pattern, ty, scope, locals))
                    .collect::<Result<Vec<_>>>()?;
                node("tuple_pattern", vec![array(values)])
            }
            Pattern::List { elements, .. } => {
                let Type::List(inner) = ty else {
                    return Err(self.error(key, SemanticPayloadErrorKind::UnsupportedPattern));
                };
                let values = elements
                    .iter()
                    .map(|pattern| self.pattern(key, pattern, inner, scope, locals))
                    .collect::<Result<Vec<_>>>()?;
                node("list_pattern", vec![array(values)])
            }
            Pattern::Record { fields, .. } => {
                let expected = self.record_pattern_fields(key, ty, scope)?;
                let mut encoded = Vec::with_capacity(fields.len());
                for (name, nested, span) in fields {
                    let field_ty = expected
                        .get(name)
                        .ok_or_else(|| self.error(key, SemanticPayloadErrorKind::UnsupportedPattern))?;
                    let nested = nested.clone().unwrap_or_else(|| {
                        Pattern::Name(name.clone(), span.clone())
                    });
                    encoded.push(array(vec![
                        text(name),
                        self.pattern(key, &nested, field_ty, scope, locals)?,
                    ]));
                }
                node("record_pattern", vec![array(encoded)])
            }
            Pattern::Constructor {
                path,
                arguments,
                fields,
                ..
            } => {
                let path_text = path
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let target = self.constructor_target(key, path, ty, scope)?;
                let (argument_types, field_types) =
                    self.constructor_pattern_shape(key, path, ty, scope)?;
                if arguments.len() != argument_types.len() {
                    return Err(self.error(key, SemanticPayloadErrorKind::UnsupportedPattern));
                }
                let arguments = arguments
                    .iter()
                    .zip(argument_types)
                    .map(|(pattern, ty)| self.pattern(key, pattern, &ty, scope, locals))
                    .collect::<Result<Vec<_>>>()?;
                let mut encoded_fields = Vec::with_capacity(fields.len());
                for field in fields {
                    let field_ty = field_types
                        .get(&field.name)
                        .ok_or_else(|| self.error(key, SemanticPayloadErrorKind::UnsupportedPattern))?;
                    let nested = field.pattern.clone().unwrap_or_else(|| {
                        Pattern::Name(field.name.clone(), field.span.clone())
                    });
                    encoded_fields.push(array(vec![
                        text(&field.name),
                        self.pattern(key, &nested, field_ty, scope, locals)?,
                    ]));
                }
                node(
                    "constructor_pattern",
                    vec![
                        target,
                        text(&path_text),
                        array(arguments),
                        array(encoded_fields),
                    ],
                )
            }
        })
    }
    fn pattern_type_node(&mut self, key: &Key, ty: &Type, scope: &Scope) -> Result<Raw> {
        match ty {
            Type::List(inner) => Ok(node(
                "list_type",
                vec![self.pattern_type_node(key, inner, scope)?],
            )),
            Type::Tuple(elements) => Ok(node(
                "tuple_type",
                vec![array(
                    elements
                        .iter()
                        .map(|element| self.pattern_type_node(key, element, scope))
                        .collect::<Result<Vec<_>>>()?,
                )],
            )),
            Type::Record(fields) => Ok(node(
                "record_type",
                vec![array(
                    fields
                        .iter()
                        .map(|(name, ty)| {
                            Ok(array(vec![
                                text(name),
                                self.pattern_type_node(key, ty, scope)?,
                            ]))
                        })
                        .collect::<Result<Vec<_>>>()?,
                )],
            )),
            Type::Optional(inner) => Ok(node(
                "optional_type",
                vec![self.pattern_type_node(key, inner, scope)?],
            )),
            _ => self.type_node(key, ty, scope),
        }
    }

    fn unsupported_pattern(&self, key: &Key) -> SemanticPayloadError {
        self.error(key, SemanticPayloadErrorKind::UnsupportedPattern)
    }

    fn record_pattern_fields(
        &self,
        key: &Key,
        ty: &Type,
        scope: &Scope,
    ) -> Result<BTreeMap<String, Type>> {
        match ty {
            Type::Record(fields) => Ok(fields.clone()),
            Type::Named(name) => scope
                .nominal_rows
                .get(name)
                .and_then(|shape| match shape {
                    Type::Record(fields) => Some(fields.clone()),
                    _ => None,
                })
                .ok_or_else(|| self.unsupported_pattern(key)),
            _ => Err(self.unsupported_pattern(key)),
        }
    }


    fn constructor_pattern_shape(
        &self,
        key: &Key,
        path: &[orna_syntax_v1::NameSegment],
        ty: &Type,
        scope: &Scope,
    ) -> Result<(Vec<Type>, BTreeMap<String, Type>)> {
        if path.len() == 1 && path[0].text == "Some" {
            let Type::Optional(inner) = ty else {
                return Err(self.unsupported_pattern(key));
            };
            return Ok((vec![inner.as_ref().clone()], BTreeMap::new()));
        }

        let Type::Named(enum_or_nominal) = ty else {
            return Err(self.unsupported_pattern(key));
        };
        if path.len() == 1 {
            if let Some(Type::Record(fields)) = scope.nominal_rows.get(enum_or_nominal) {
                return Ok((Vec::new(), fields.clone()));
            }
        }
        if path.len() >= 2 {
            let owner = path[..path.len() - 1]
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let variant = path.last().expect("path length checked").text.as_str();
            let variants = scope
                .enum_variants
                .get(enum_or_nominal)
                .or_else(|| {
                    scope.enum_variants.iter().find_map(|(name, variants)| {
                        (name == &owner || name.ends_with(&format!(".{owner}")))
                            .then_some(variants)
                    })
                });
            if let Some(fields) = variants.and_then(|variants| variants.get(variant)) {
                return Ok((Vec::new(), fields.clone()));
            }
        }

        Err(SemanticPayloadError {
            declaration: None,
            origin: None,
            kind: SemanticPayloadErrorKind::UnsupportedPattern,
        })
    }

    fn constructor_target(
        &mut self,
        key: &Key,
        path: &[orna_syntax_v1::NameSegment],
        ty: &Type,
        scope: &Scope,
    ) -> Result<Raw> {
        if path.len() == 1 && path[0].text == "Some" && matches!(ty, Type::Optional(_)) {
            return Ok(Raw::Null);
        }
        let target_path = if path.len() >= 2 {
            path[..path.len() - 1]
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(".")
        } else {
            path.iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(".")
        };
        let target = self.target(key, &target_path, scope)?;
        Ok(Raw::Bytes(self.declaration(&target)?))
    }

    fn statement(
        &mut self,
        key: &Key,
        statement: &Statement,
        scope: &Scope,
        locals: &mut Locals,
    ) -> Result<Raw> {
        Ok(match statement {
            Statement::Let {
                pattern,
                annotation,
                value,
                ..
            } => {
                let ty = annotation
                    .as_ref()
                    .map(|ty| resolved_type_of(ty, scope))
                    .map(Ok)
                    .unwrap_or_else(|| self.inferred_type(key, value, scope, locals))?;
                let value = self.expr(key, value, scope, locals)?;
                node(
                    "let",
                    vec![self.pattern(key, pattern, &ty, scope, locals)?, value],
                )
            }
            Statement::Assert { value, .. } => {
                node("assert", vec![self.expr(key, value, scope, locals)?])
            }
            Statement::Return { value, .. } => node(
                "return",
                vec![self.optional_expr(key, value.as_ref(), scope, locals)?],
            ),
            Statement::Break { value, .. } => node(
                "break",
                vec![self.optional_expr(key, value.as_ref(), scope, locals)?],
            ),
            Statement::Continue { .. } => node("continue", Vec::new()),
            Statement::Expression { value, .. } | Statement::Control { value, .. } => {
                node("evaluate", vec![self.expr(key, value, scope, locals)?])
            }
            Statement::Assignment {
                target,
                operator,
                value,
                ..
            } => node(
                "assign",
                vec![
                    self.assignment_target(key, target, scope, locals)?,
                    text(match operator {
                        AssignmentOperator::Set => "=",
                        AssignmentOperator::Add => "+=",
                        AssignmentOperator::Subtract => "-=",
                        AssignmentOperator::Multiply => "*=",
                        AssignmentOperator::Divide => "/=",
                    }),
                    self.expr(key, value, scope, locals)?,
                ],
            ),
        })
    }

    fn assignment_target(
        &mut self,
        key: &Key,
        target: &AssignmentTarget,
        scope: &Scope,
        locals: &mut Locals,
    ) -> Result<Raw> {
        Ok(match target {
            AssignmentTarget::Name { name, .. } => {
                let (position, _) = locals.bindings.get(name).ok_or_else(|| {
                    self.error(
                        key,
                        SemanticPayloadErrorKind::MissingDeclarationEvidence {
                            target: name.clone(),
                        },
                    )
                })?;
                node("local", vec![index(*position)])
            }
            AssignmentTarget::Field { base, name, .. } => node(
                "field",
                vec![
                    self.assignment_target(key, base, scope, locals)?,
                    text(name),
                ],
            ),
            AssignmentTarget::Index {
                base, index: at, ..
            } => node(
                "index",
                vec![
                    self.assignment_target(key, base, scope, locals)?,
                    self.expr(key, at, scope, locals)?,
                ],
            ),
        })
    }

    fn literal(&self, key: &Key, spelling: &str, kind: LiteralKind) -> Result<Raw> {
        let invalid = || self.error(key, SemanticPayloadErrorKind::InvalidLiteral { kind });
        Ok(match kind {
            LiteralKind::Null => Raw::Null,
            LiteralKind::Boolean => match spelling {
                "true" => Raw::Bool(true),
                "false" => Raw::Bool(false),
                _ => return Err(invalid()),
            },
            LiteralKind::Integer => {
                Raw::Int(spelling.replace('_', "").parse().map_err(|_| invalid())?)
            }

            LiteralKind::Float => {
                let number = spelling
                    .strip_suffix('f')
                    .ok_or_else(invalid)?
                    .replace('_', "")
                    .parse::<f64>()
                    .map_err(|_| invalid())?;
                Value::float_bits(number.to_bits()).raw().clone()
            }
            LiteralKind::Decimal => {
                let clean = spelling.replace('_', "");
                let clean = clean.strip_suffix(".decimal").unwrap_or(&clean);
                let (mantissa, exponent) = clean.split_once(['e', 'E']).unwrap_or((clean, "0"));
                let exponent = exponent.parse::<i64>().map_err(|_| invalid())?;
                let fractional = mantissa
                    .split_once('.')
                    .map_or(0, |(_, fraction)| fraction.len());
                let fractional = i64::try_from(fractional).map_err(|_| invalid())?;
                let exponent = exponent.checked_sub(fractional).ok_or_else(invalid)?;
                Value::decimal(
                    mantissa.replace('.', "").parse().map_err(|_| invalid())?,
                    exponent.into(),
                )
                .map_err(|_| invalid())?
                .raw()
                .clone()
            }
            LiteralKind::String => {
                // JSON-compatible strings use the existing serde_json dependency.
                // Orna-only escape forms fail closed until a shared literal decoder exists.
                let value = serde_json::from_str::<String>(spelling).map_err(|_| invalid())?;
                Raw::Text(value)
            }
            LiteralKind::Date => Raw::Tag(60001, Box::new(Raw::Text(spelling.to_owned()))),
            LiteralKind::Instant => {
                return Err(self.error(
                    key,
                    SemanticPayloadErrorKind::UnsupportedExpression {
                        form: "instant_literal",
                    },
                ));
            }
        })
    }
}

#[derive(Clone, Default)]
struct Locals {
    bindings: BTreeMap<String, (usize, Type)>,
    next: usize,
}
impl Locals {
    fn bind(&mut self, name: &str, ty: Type) -> usize {
        let position = self.next;
        self.next += 1;
        self.bindings.insert(name.nfc().collect(), (position, ty));
        position
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn produce(source: &str) -> SemanticPayloadAnalysis {
        analyze_semantic_payloads(
            &[ModuleInput::new("payload.orna", source)],
            &Catalogue::empty(),
        )
        .unwrap()
    }
    fn named<'a>(analysis: &'a SemanticPayloadAnalysis, name: &str) -> &'a SemanticDeclaration {
        analysis
            .declarations()
            .iter()
            .find(|d| d.qualified_name() == name)
            .unwrap()
    }

    #[test]
    fn canonical_bytes_ignore_whitespace_but_retain_signature_changes() {
        let compact = produce("pub fn choose(first: Int, second: Int): Int = first;");
        let spaced = produce("\n pub fn choose( first: Int,\n second: Int ): Int = first ;\n");
        let changed = produce("pub fn choose(first: Int, second: Bool): Int = first;");
        let a = named(&compact, "payload.choose");
        let b = named(&spaced, "payload.choose");
        let c = named(&changed, "payload.choose");
        assert_eq!(a.canonical_payload(), b.canonical_payload());
        assert_eq!(a.semantic_hash(), b.semantic_hash());
        assert_ne!(a.source_hash(), b.source_hash());
        assert_ne!(a.canonical_payload(), c.canonical_payload());
        assert_ne!(a.semantic_hash(), c.semantic_hash());
    }

    #[test]
    fn unsupported_list_type_is_not_flattened() {
        let error = analyze_semantic_payloads(
            &[ModuleInput::new(
                "payload.orna",
                "fn first(values: [Int]): Int = 1;",
            )],
            &Catalogue::empty(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            SemanticPayloadErrorKind::UnsupportedType {
                ty: Box::new(Type::List(Box::new(Type::Int)))
            }
        );
    }

    #[test]
    fn parameter_names_preserve_declaration_order() {
        let result = produce("fn choose(zebra: Int, alpha: Int = 2): Int = zebra;");
        let parameters = named(&result, "payload.choose").parameters();
        assert_eq!(
            parameters
                .iter()
                .map(|p| (p.position, p.name.as_str(), p.has_default))
                .collect::<Vec<_>>(),
            vec![(0, "zebra", false), (1, "alpha", true)]
        );
    }

    #[test]
    fn annotation_explicitness_is_retained_outside_semantic_bytes() {
        let inferred = produce("fn increment(value) = value + 1;");
        let explicit = produce("fn increment(value: Int): Int = value + 1;");
        let a = named(&inferred, "payload.increment");
        let b = named(&explicit, "payload.increment");
        assert_eq!(a.canonical_payload(), b.canonical_payload());
        assert_eq!(a.resolved_type(), b.resolved_type());
        assert_eq!(a.explicitness().parameter_annotations, vec![false]);
        assert!(!a.explicitness().result_annotation);
        assert_eq!(b.explicitness().parameter_annotations, vec![true]);
        assert!(b.explicitness().result_annotation);
        assert!(a.explicitness().authored && b.explicitness().authored);
    }

    #[test]
    fn body_defaults_and_private_fields_change_meaning() {
        let first = produce("pub type Box { secret: Int = 1, } fn value(x: Int = 1): Int = x + 1;");
        let second =
            produce("pub type Box { secret: Int = 2, } fn value(x: Int = 2): Int = x + 2;");
        for name in ["payload.Box", "payload.value"] {
            assert_ne!(
                named(&first, name).canonical_payload(),
                named(&second, name).canonical_payload()
            );
        }
    }

    #[test]
    fn resolved_call_targets_include_dependency_meaning_not_import_aliases() {
        let project = |alias: &str, value: i32| {
            analyze_semantic_payloads(
                &[
                    ModuleInput::new("library.orna", format!("pub fn value(): Int = {value};")),
                    ModuleInput::new(
                        "client.orna",
                        format!("use library as {alias}; fn call(): Int = {alias}.value();"),
                    ),
                ],
                &Catalogue::empty(),
            )
            .unwrap()
        };
        let first = project("one", 1);
        let renamed_import = project("two", 1);
        let changed_body = project("one", 2);
        assert_eq!(
            named(&first, "client.call").canonical_payload(),
            named(&renamed_import, "client.call").canonical_payload()
        );
        assert_ne!(
            named(&first, "client.call").canonical_payload(),
            named(&changed_body, "client.call").canonical_payload()
        );
    }

    #[test]
    fn non_name_parameter_pattern_fails_without_inventing_a_name() {
        let error = analyze_semantic_payloads(
            &[ModuleInput::new(
                "payload.orna",
                "fn ignore(_: Int): Int = 1;",
            )],
            &Catalogue::empty(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            SemanticPayloadErrorKind::NonNameParameter { position: 0 }
        );
    }

    #[test]
    fn table_reference_types_require_evidence_not_named_substitution() {
        let error = analyze_semantic_payloads(
            &[ModuleInput::new(
                "payload.orna",
                "table Entry(id: Int) { value: Int, } fn inspect(entry: Entry): Int = 1;",
            )],
            &Catalogue::empty(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            SemanticPayloadErrorKind::ReferenceTypeEvidenceRequired {
                target: "Entry".into()
            }
        );
    }

    #[test]
    fn structured_patterns_are_admitted_and_encoded_recursively() {
        fn span() -> orna_syntax_v1::SyntaxSpan {
            orna_syntax_v1::SyntaxSpan::new(0, 0)
        }
        fn name(text: &str) -> orna_syntax_v1::NameSegment {
            orna_syntax_v1::NameSegment {
                text: text.to_owned(),
                span: span(),
            }
        }
        fn has_tag(raw: &Raw, tag: &str) -> bool {
            match raw {
                Raw::Array(values) => {
                    values.iter().any(|value| {
                        matches!(value, Raw::Text(text) if text == tag) || has_tag(value, tag)
                    })
                }
                _ => false,
            }
        }

        let inputs = vec![ModuleInput::new(
            "payload.orna",
            "pub type Point { pub x: Int, pub y: Int, }",
        )];
        let (analysis, contexts) = analyze_retaining_context(&inputs, &Catalogue::empty(), true);
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        let mut producer = Producer {
            analysis: &analysis,
            contexts: &contexts,
            inputs: &inputs,
            active: BTreeSet::new(),
            completed: BTreeMap::new(),
        };
        let key = (Namespace(vec!["payload".into()]), "Point".into());
        let scope = &contexts[0].2;

        let mut locals = Locals::default();
        let tuple = producer
            .pattern(
                &key,
                &Pattern::Tuple {
                    elements: vec![
                        Pattern::Wildcard(span()),
                        Pattern::Name("second".into(), span()),
                    ],
                    span: span(),
                },
                &Type::Tuple(vec![Type::Int, Type::Int]),
                scope,
                &mut locals,
            )
            .unwrap();
        assert!(has_tag(&tuple, "tuple_pattern"));

        let mut locals = Locals::default();
        let list = producer
            .pattern(
                &key,
                &Pattern::List {
                    elements: vec![
                        Pattern::Name("head".into(), span()),
                        Pattern::Name("tail".into(), span()),
                    ],
                    span: span(),
                },
                &Type::List(Box::new(Type::Int)),
                scope,
                &mut locals,
            )
            .unwrap();
        assert!(has_tag(&list, "list_pattern"));

        let mut locals = Locals::default();
        let record = producer
            .pattern(
                &key,
                &Pattern::Record {
                    fields: vec![(
                        "x".into(),
                        None,
                        span(),
                    )],
                    span: span(),
                },
                &Type::Record(BTreeMap::from([("x".into(), Type::Int)])),
                scope,
                &mut locals,
            )
            .unwrap();
        assert!(has_tag(&record, "record_pattern"));

        let mut locals = Locals::default();
        let nominal = producer
            .pattern(
                &key,
                &Pattern::Constructor {
                    path: vec![name("Point")],
                    arguments: Vec::new(),
                    fields: vec![orna_syntax_v1::PatternField {
                        name: "x".into(),
                        pattern: Some(Pattern::Wildcard(span())),
                        span: span(),
                    }],
                    span: span(),
                },
                &Type::Named("payload.Point".into()),
                scope,
                &mut locals,
            )
            .unwrap();
        assert!(has_tag(&nominal, "constructor_pattern"));
        assert!(has_tag(&tuple, "wildcard"));
    }

    #[test]
    fn stage_two_cannot_manufacture_identity() {
        let result = produce("fn value(): Int = 1;");
        let value = named(&result, "payload.value");
        assert_eq!(
            value.revision_id().unwrap_err().kind,
            SemanticPayloadErrorKind::IdentityEvidenceRequired
        );
        assert_eq!(
            value.object_id().unwrap_err().kind,
            SemanticPayloadErrorKind::IdentityEvidenceRequired
        );
    }
}
