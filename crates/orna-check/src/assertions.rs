//! Assertion planning.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    ControlKind, Declaration, Expr, FieldInitializer, Item, Pattern, Statement, SyntaxTree,
    TypeMember, TypeRepresentation, UseTail,
};
use orna_value::Diagnostic;

use super::*;

pub(crate) fn assertion(
    input: AssertionInput<'_>,
    plans: &mut Vec<AssertionPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let AssertionInput {
        owner,
        value,
        inferred,
        resolved_tables,
        scope,
        enforcement,
        source_order,
    } = input;
    let dependencies = tables_referenced(value, resolved_tables, scope, None);
    if inferred.ty != Type::Bool {
        let message = match &owner {
            AssertionOwner::Table(name) => {
                format!("table assertion must be a predicate over Relation<{name}>")
            }
            _ => "assertion does not have the required Boolean form".into(),
        };
        diagnostics.push(diag(DIAG_ASSERTION, message));
    }
    if inferred.effects.forbidden_for_assertion() {
        let message = if inferred.effects.effects.contains(&EffectKind::Network) {
            "declaration assertion uses forbidden network effect"
        } else if inferred.effects.effects.contains(&EffectKind::Filesystem) {
            "declaration assertion uses forbidden filesystem effect"
        } else {
            "assertion has forbidden effects or failure"
        };
        diagnostics.push(diag(DIAG_ASSERTION_EFFECT, message));
    }
    if matches!(owner, AssertionOwner::Module) {
        match dependencies.len() {
            0 => diagnostics.push(diag(
                DIAG_ASSERTION_SCOPE,
                "module assertions must depend on at least two distinct tables",
            )),
            1 => diagnostics.push(diag(
                DIAG_ASSERTION_ONE_TABLE,
                "a one-table invariant belongs inside that table",
            )),
            _ => {}
        }
    }
    plans.push(AssertionPlan {
        owner,
        dependencies,
        effects: inferred.effects,
        expression_span: value.span(),
        source_order,
        scope: enforcement,
    });
}

pub(crate) fn infer_refined_assertion(
    value: &Expr,
    base: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Unary { op, rhs, .. } = value else {
        return infer(value, scope, &BTreeMap::new(), diagnostics);
    };
    if !matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=" | "in") {
        return infer(value, scope, &BTreeMap::new(), diagnostics);
    }
    let inferred = infer(rhs, scope, &BTreeMap::new(), diagnostics);
    let effects = inferred.effects;
    if op == "in" {
        match inferred.ty {
            Type::Range(element) => require_same(base, &element, diagnostics),
            Type::Error => {}
            _ => diagnostics.push(diag(
                DIAG_TYPE,
                "refined membership assertion requires a numeric range",
            )),
        }
    } else {
        require_same(base, &inferred.ty, diagnostics);
    }
    Inferred {
        ty: Type::Bool,
        effects,
    }
}

/// Elaborates owner-local relational predicate constructors and ordinary
/// relation predicates. The table supplies the unpublished `Relation<Row>`
/// subject; a predicate function value therefore receives that subject
/// implicitly, without a source-level table read.
pub(crate) fn infer_table_assertion(
    value: &Expr,
    owner_name: &str,
    row: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Expr::Binary { lhs, op, .. } = value
        && op == "|"
        && let Expr::Name { text, .. } = lhs.as_ref()
    {
        let message = match text.as_str() {
            "self" => Some("remove `self |`; the table already supplies its candidate relation"),
            text if text == owner_name => {
                Some("remove the repeated table owner before the assertion predicate")
            }
            _ => None,
        };
        if let Some(message) = message {
            diagnostics.push(diag(DIAG_ASSERTION_OWNER_PIPE, message));
            return Inferred::pure(Type::Bool);
        }
    }
    let Expr::Call {
        callee, arguments, ..
    } = value
    else {
        return infer_ordinary_table_predicate(value, owner_name, scope, diagnostics);
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return infer_ordinary_table_predicate(value, owner_name, scope, diagnostics);
    };
    if !matches!(text.as_str(), "every" | "all_unique") || arguments.len() != 1 {
        return infer_ordinary_table_predicate(value, owner_name, scope, diagnostics);
    }
    let Expr::Lambda {
        parameters, body, ..
    } = &arguments[0].value
    else {
        return Inferred::error();
    };
    let [parameter] = parameters.as_slice() else {
        return Inferred::error();
    };
    let Pattern::Name(name, _) = &parameter.pattern else {
        return Inferred::error();
    };
    let mut local = BTreeMap::new();
    insert_local_binding(name, row.clone(), &mut local, diagnostics);
    let inferred = infer(body, scope, &local, diagnostics);
    let valid = match text.as_str() {
        "every" => inferred.ty == Type::Bool,
        "all_unique" => is_lawful_all_unique_key_type(&inferred.ty, scope),
        _ => unreachable!("table predicate constructors were matched above"),
    };
    if text == "all_unique" && inferred.ty != Type::Error && !valid {
        diagnostics.push(diag(
            DIAG_TYPE,
            "all_unique selector must return a lawful equality key; Float and noncanonical values are not valid keys",
        ));
    }
    Inferred {
        ty: if valid { Type::Bool } else { Type::Error },
        effects: inferred.effects,
    }
}

/// `all_unique` compares complete canonical selected values. Function and
/// relation values have no stable value identity, and default Float equality
/// is intentionally unavailable; nested and locally described named keys
/// inherit those restrictions. Opaque imported values retain the equality
/// contract of their pinned declaration.
pub(crate) fn is_lawful_all_unique_key_type(ty: &Type, scope: &Scope) -> bool {
    fn unique_short_match<'a>(
        values: &'a BTreeMap<String, Type>,
        short_name: &str,
    ) -> Option<&'a Type> {
        let mut matching = values
            .iter()
            .filter(|(candidate, _)| candidate.rsplit('.').next() == Some(short_name));
        let first = matching.next().map(|(_, ty)| ty);
        if matching.next().is_some() {
            None
        } else {
            first
        }
    }

    fn visit(ty: &Type, scope: &Scope, expanding: &mut BTreeSet<String>) -> bool {
        match ty {
            Type::Error | Type::Float => false,
            Type::Applied { base, arguments } => {
                base != "Float"
                    && arguments
                        .iter()
                        .all(|argument| visit(argument, scope, expanding))
            }
            Type::List(element) | Type::Optional(element) => visit(element, scope, expanding),
            Type::Tuple(elements) => elements
                .iter()
                .all(|element| visit(element, scope, expanding)),
            Type::Record(fields) => fields.values().all(|field| visit(field, scope, expanding)),
            Type::Range(_)
            | Type::Relation(_)
            | Type::Stream(_)
            | Type::Function { .. }
            | Type::MoneyPerUnit { .. } => false,
            Type::Quantity { numeric, .. } => visit(numeric, scope, expanding),
            Type::Named(name) => {
                if !expanding.insert(name.clone()) {
                    return true;
                }
                let short_name = name.rsplit('.').next().unwrap_or(name);
                let shape = scope
                    .nominal_rows
                    .get(name)
                    .or_else(|| unique_short_match(&scope.nominal_rows, short_name))
                    .or_else(|| scope.type_aliases.get(name))
                    .or_else(|| scope.refined_types.get(name))
                    .or_else(|| scope.type_aliases.get(short_name))
                    .or_else(|| scope.refined_types.get(short_name));
                let shape_is_lawful = shape.is_none_or(|shape| visit(shape, scope, expanding));
                let enum_payloads_are_lawful = scope
                    .enum_variants
                    .get(name)
                    .or_else(|| scope.enum_variants.get(short_name))
                    .is_none_or(|variants| {
                        variants
                            .values()
                            .flat_map(BTreeMap::values)
                            .all(|field| visit(field, scope, expanding))
                    });
                expanding.remove(name);
                shape_is_lawful && enum_payloads_are_lawful
            }
            Type::Int
            | Type::Decimal
            | Type::Date
            | Type::Instant
            | Type::Text
            | Type::Bool
            | Type::Null
            | Type::Unit
            | Type::Bottom => true,
        }
    }

    visit(ty, scope, &mut BTreeSet::new())
}

/// A table assertion may name an ordinary pure predicate function. Its
/// `Relation<Owner>` parameter is elaborated from the table owner rather than
/// from a table expression, so explicit table reads retain their effects.
pub(crate) fn infer_ordinary_table_predicate(
    value: &Expr,
    owner_name: &str,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let expected_relation = Type::Relation(Box::new(Type::Named(owner_name.to_owned())));
    let inferred = infer_contextual(
        value,
        &Type::Function {
            parameters: vec![expected_relation.clone()],
            parameter_names: None,
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Bool),
        },
        scope,
        &BTreeMap::new(),
        diagnostics,
    );
    let valid = matches!(
        &inferred.ty,
        Type::Function {
            parameters,
            result,
            ..
        } if parameters.len() == 1
            && types_match(&parameters[0], &expected_relation)
            && types_match(result.as_ref(), &Type::Bool)
    );
    if !matches!(&inferred.ty, Type::Function { .. }) {
        // Preserve ordinary expression inference for existing assertion
        // diagnostics (notably explicit table reads); only function-valued
        // assertions need owner-predicate elaboration.
        return inferred;
    }
    Inferred {
        ty: if valid { Type::Bool } else { Type::Error },
        effects: inferred.effects,
    }
}

/// Elaborates the bounded relational query form used by module assertions in
/// the frozen reference project. A module assertion names each relation
/// explicitly, so this has no implicit owner subject and no runtime behavior.
pub(crate) fn infer_module_assertion(
    value: &Expr,
    table_rows: &BTreeMap<String, Type>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    infer_module_relation(value, table_rows, scope, &BTreeMap::new(), diagnostics)
}

pub(crate) fn infer_module_relation(
    value: &Expr,
    table_rows: &BTreeMap<String, Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    match value {
        Expr::Group { inner, .. } => {
            return infer_module_relation(inner, table_rows, scope, local, diagnostics);
        }
        Expr::Block {
            statements, tail, ..
        } => {
            let mut locals = local.clone();
            let mut effects = EffectSummary::default();
            for statement in statements {
                match statement {
                    Statement::Let {
                        pattern,
                        annotation,
                        value,
                        ..
                    } => {
                        let expected = annotation.as_ref().map(|annotation| {
                            validate_type_annotation(
                                annotation,
                                scope,
                                &BTreeSet::new(),
                                diagnostics,
                            );
                            resolved_type_of(annotation, scope)
                        });
                        let is_lambda = matches!(value, Expr::Lambda { .. });
                        let inferred = if is_lambda {
                            expected
                                .as_ref()
                                .and_then(|expected| {
                                    infer_module_lambda(
                                        value,
                                        expected,
                                        table_rows,
                                        scope,
                                        &locals,
                                        diagnostics,
                                    )
                                })
                                .unwrap_or_else(|| infer(value, scope, &locals, diagnostics))
                        } else {
                            infer_module_relation(value, table_rows, scope, &locals, diagnostics)
                        };
                        if let Some(expected) = &expected {
                            require_same(expected, &inferred.ty, diagnostics);
                        }
                        // A lambda's body effects are latent until the bound
                        // continuation is invoked; its value is pure to bind.
                        if !is_lambda {
                            effects.join(&inferred.effects);
                        }
                        bind_pattern(pattern, inferred.ty, scope, &mut locals, diagnostics);
                        if is_lambda
                            && let Pattern::Name(name, _) = pattern
                            && let Some(symbol) = locals.get_mut(name)
                        {
                            symbol.effects = inferred.effects;
                        }
                    }
                    Statement::Assert { value, .. } => {
                        let inferred =
                            infer_module_relation(value, table_rows, scope, &locals, diagnostics);
                        require_same(&Type::Bool, &inferred.ty, diagnostics);
                        effects.join(&inferred.effects);
                    }
                    Statement::Expression { value, .. } | Statement::Control { value, .. } => {
                        let inferred =
                            infer_module_relation(value, table_rows, scope, &locals, diagnostics);
                        effects.join(&inferred.effects);
                    }
                    Statement::Return { .. }
                    | Statement::Break { .. }
                    | Statement::Continue { .. }
                    | Statement::Assignment { .. } => {
                        diagnostics.push(diag(
                            DIAG_UNSUPPORTED,
                            "module predicate continuations do not support control transfer or assignment",
                        ));
                    }
                }
            }
            let mut inferred = tail.as_ref().map_or_else(
                || Inferred::pure(Type::Tuple(Vec::new())),
                |tail| infer_module_relation(tail, table_rows, scope, &locals, diagnostics),
            );
            effects.join(&inferred.effects);
            inferred.effects = effects;
            return inferred;
        }
        Expr::Unary { op, rhs, .. } if op == "!" => {
            let operand = infer_module_relation(rhs, table_rows, scope, local, diagnostics);
            return Inferred {
                ty: if operand.ty == Type::Bool {
                    Type::Bool
                } else {
                    Type::Error
                },
                effects: operand.effects,
            };
        }
        // Section 9 predicates compose through ordinary short-circuit Boolean
        // continuations; keep resolving relation folds in each continuation
        // instead of treating nested table names as ordinary values.
        Expr::Binary { lhs, op, rhs, .. } if op == "&&" || op == "||" => {
            let left = infer_module_relation(lhs, table_rows, scope, local, diagnostics);
            let right = infer_module_relation(rhs, table_rows, scope, local, diagnostics);
            let mut effects = left.effects;
            effects.join(&right.effects);
            return Inferred {
                ty: if left.ty == Type::Bool && right.ty == Type::Bool {
                    Type::Bool
                } else {
                    Type::Error
                },
                effects,
            };
        }
        Expr::Control {
            kind: ControlKind::If,
            binding: None,
            condition: Some(condition),
            body: Some(body),
            arms,
            alternate: Some(alternate),
            ..
        } if arms.is_empty() => {
            let condition = infer_module_relation(condition, table_rows, scope, local, diagnostics);
            let body = infer_module_relation(body, table_rows, scope, local, diagnostics);
            let alternate = infer_module_relation(alternate, table_rows, scope, local, diagnostics);
            let mut effects = condition.effects;
            effects.join(&body.effects);
            effects.join(&alternate.effects);
            return Inferred {
                ty: if condition.ty == Type::Bool
                    && body.ty == Type::Bool
                    && alternate.ty == Type::Bool
                {
                    Type::Bool
                } else {
                    Type::Error
                },
                effects,
            };
        }
        _ => {}
    }
    let Expr::Call {
        callee, arguments, ..
    } = value
    else {
        return infer(value, scope, local, diagnostics);
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return infer(value, scope, local, diagnostics);
    };
    if !matches!(text.as_str(), "every" | "exists") || arguments.len() != 2 {
        return infer(value, scope, local, diagnostics);
    }
    let Some(row) = table_identity(&arguments[0].value, scope, local)
        .and_then(|identity| table_rows.get(&identity))
    else {
        return Inferred::error();
    };
    let inferred = match &arguments[1].value {
        Expr::Lambda {
            parameters, body, ..
        } => {
            let [parameter] = parameters.as_slice() else {
                return Inferred::error();
            };
            let Pattern::Name(name, _) = &parameter.pattern else {
                return Inferred::error();
            };
            let mut locals = local.clone();
            insert_local_binding(name, row.clone(), &mut locals, diagnostics);
            infer_module_relation(body, table_rows, scope, &locals, diagnostics)
        }
        Expr::Name { text, .. } => {
            let Some(symbol) = local.get(text) else {
                return Inferred::error();
            };
            let Type::Function {
                parameters, result, ..
            } = &symbol.ty
            else {
                return Inferred::error();
            };
            if parameters.len() != 1 {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "module relation continuation must take exactly one row",
                ));
            } else {
                require_same(row, &parameters[0], diagnostics);
            }
            require_same(&Type::Bool, result, diagnostics);
            Inferred {
                ty: result.as_ref().clone(),
                effects: symbol.effects.clone(),
            }
        }
        _ => {
            return Inferred::error();
        }
    };
    Inferred {
        ty: if inferred.ty == Type::Bool {
            Type::Bool
        } else {
            Type::Error
        },
        effects: inferred.effects,
    }
}

pub(crate) fn infer_module_lambda(
    value: &Expr,
    expected: &Type,
    table_rows: &BTreeMap<String, Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let Expr::Lambda {
        parameters, body, ..
    } = value
    else {
        return None;
    };
    let Type::Function {
        parameters: expected_parameters,
        result: expected_result,
        ..
    } = expected
    else {
        return None;
    };
    if parameters.len() != expected_parameters.len() {
        diagnostics.push(diag(
            DIAG_TYPE,
            "module relation continuation parameter count does not match its annotation",
        ));
    }
    let mut locals = local.clone();
    for (index, parameter) in parameters.iter().enumerate() {
        let Some(expected_parameter) = expected_parameters.get(index) else {
            continue;
        };
        let parameter_type = parameter
            .annotation
            .as_ref()
            .map(type_of)
            .unwrap_or_else(|| expected_parameter.clone());
        require_same(expected_parameter, &parameter_type, diagnostics);
        bind_pattern(
            &parameter.pattern,
            parameter_type,
            scope,
            &mut locals,
            diagnostics,
        );
    }
    let inferred = infer_module_relation(body, table_rows, scope, &locals, diagnostics);
    require_same(expected_result, &inferred.ty, diagnostics);
    Some(Inferred {
        ty: Type::Function {
            parameters: expected_parameters.clone(),
            default_parameters: BTreeSet::new(),
            parameter_names: parameters
                .iter()
                .map(|parameter| match &parameter.pattern {
                    Pattern::Name(name, _) if name != "_" => Some(name.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>(),
            result: Box::new(inferred.ty),
        },
        effects: inferred.effects,
    })
}

pub(crate) fn declared_nominal_schema(item: &Item) -> Option<TableSchema> {
    let Declaration::Type {
        representation: TypeRepresentation::Nominal { members },
        ..
    } = &item.declaration
    else {
        return None;
    };
    let declaration_order: Vec<String> = members
        .iter()
        .filter_map(|member| match member {
            TypeMember::Field {
                visibility: true,
                name,
                ..
            } => Some(name.clone()),
            TypeMember::Field { .. }
            | TypeMember::Assertion { .. }
            | TypeMember::Implementation { .. } => None,
        })
        .collect();
    let fields = members
        .iter()
        .filter_map(|member| match member {
            TypeMember::Field {
                visibility: true,
                name,
                ty,
                ..
            } => Some((name.clone(), type_of(ty))),
            TypeMember::Field { .. }
            | TypeMember::Assertion { .. }
            | TypeMember::Implementation { .. } => None,
        })
        .collect();
    let mut required = BTreeSet::new();
    let mut private_required = false;
    for member in members {
        let TypeMember::Field {
            visibility,
            name,
            initializer,
            ..
        } = member
        else {
            continue;
        };
        if initializer.is_none() {
            if *visibility {
                required.insert(name.clone());
            } else {
                private_required = true;
            }
        }
    }
    Some(TableSchema {
        references: Vec::new(),
        declaration_order,
        fields,
        // Only declaration-backed completeness is exported.  Default
        // expressions and private field types remain in the owning module.
        admission: Some(TableAdmission {
            required,
            private_required,
            computed: BTreeSet::new(),
            keys: Vec::new(),
            automatic_key: false,
        }),
        conversions: Vec::new(),
        currency: false,
    })
}

pub(crate) fn declared_table_schema(item: &Item) -> Option<TableSchema> {
    let Declaration::Table { keys, members, .. } = &item.declaration else {
        return None;
    };
    let Type::Record(fields) = table_row_type(item) else {
        unreachable!()
    };
    let mut required = BTreeSet::new();
    let mut computed = BTreeSet::new();
    for key in keys {
        if key.default.is_none()
            && let Pattern::Name(name, _) = &key.pattern
        {
            required.insert(name.clone());
        }
    }
    for member in members {
        if let orna_syntax::TableMember::Field {
            name, initializer, ..
        } = member
        {
            match initializer {
                None => {
                    required.insert(name.clone());
                }
                Some(FieldInitializer::Computed(_)) => {
                    computed.insert(name.clone());
                }
                Some(FieldInitializer::Default(_)) => {}
            }
        }
    }
    // Key columns are declared in the table header, ahead of the members, and
    // the row type carries them as fields, so the declaration order lists them
    // first. `sys.Table.columns` reports this order.
    let declaration_order: Vec<String> = keys
        .iter()
        .filter_map(|key| match &key.pattern {
            Pattern::Name(name, _) => Some(name.clone()),
            _ => None,
        })
        .chain(members.iter().filter_map(|member| match member {
            orna_syntax::TableMember::Field { name, .. } => Some(name.clone()),
            _ => None,
        }))
        .collect();
    Some(TableSchema {
        references: Vec::new(),
        declaration_order,
        fields,
        admission: Some(TableAdmission {
            required,
            private_required: false,
            computed,
            keys: if keys.is_empty() {
                vec![("id".into(), Type::Int)]
            } else {
                keys.iter()
                    .filter_map(|key| match &key.pattern {
                        Pattern::Name(name, _) => Some((
                            name.clone(),
                            key.annotation.as_ref().map(type_of).unwrap_or(Type::Error),
                        )),
                        _ => None,
                    })
                    .collect()
            },
            automatic_key: keys.is_empty(),
        }),
        conversions: Vec::new(),
        currency: false,
    })
}

pub(crate) fn table_row_type(item: &Item) -> Type {
    let Declaration::Table { keys, members, .. } = &item.declaration else {
        unreachable!("table row type requested for a non-table declaration");
    };
    let mut fields = BTreeMap::new();
    if keys.is_empty() {
        // Automatic IDs are real logical row fields even though insertion
        // allocates them. This keeps selection and catalogue schema shape in
        // sync with the implicit primary key.
        fields.insert("id".into(), Type::Int);
    }
    for key in keys {
        if let Pattern::Name(name, _) = &key.pattern {
            fields.insert(
                name.clone(),
                key.annotation.as_ref().map(type_of).unwrap_or(Type::Error),
            );
        }
    }
    for member in members {
        if let orna_syntax::TableMember::Field { name, ty, .. } = member {
            fields.insert(name.clone(), type_of(ty));
        }
    }
    Type::Record(fields)
}

/// Returns the spec diagnostic code and message for a primary-key type
/// violation. Other primary-key rejections keep the generic type code.
pub(crate) fn primary_key_type_error(
    ty: &Type,
    scope: &Scope,
    visiting_tables: &mut BTreeSet<String>,
) -> Option<(&'static str, &'static str)> {
    let resolved = resolve_type_aliases(ty, &scope.type_aliases, &mut BTreeSet::new());
    if &resolved != ty {
        return primary_key_type_error(&resolved, scope, visiting_tables);
    }
    match ty {
        Type::Int | Type::Decimal | Type::Date | Type::Instant | Type::Text | Type::Bool => None,
        Type::Tuple(elements) if elements.is_empty() => {
            Some((DIAG_TYPE, "Unit is not a primary-key type in version 1.0"))
        }
        Type::Tuple(elements) => elements
            .iter()
            .find_map(|element| primary_key_type_error(element, scope, visiting_tables)),
        Type::Named(name) if matches!(name.as_str(), "Uuid" | "UUID" | "std.UUID") => None,
        Type::Named(name) if scope.payload_enum_types.contains(name) => Some((
            DIAG_TYPE,
            "payload-bearing enums cannot be primary-key types",
        )),
        Type::Named(name) => {
            let symbol = scope_named_symbol(name, scope);
            match symbol {
                Some(symbol) if symbol.kind == SymbolKind::Enum => None,
                Some(symbol) if symbol.kind == SymbolKind::Table => {
                    if !visiting_tables.insert(name.clone()) {
                        return Some((
                            DIAG_TYPE,
                            "table reference key type is recursively defined",
                        ));
                    }
                    let result = match symbol
                        .table_schema
                        .as_ref()
                        .and_then(|schema| schema.admission.as_ref())
                    {
                        Some(admission) => {
                            if admission.keys.is_empty() {
                                Some((DIAG_TYPE, "table reference has no canonical primary key"))
                            } else {
                                admission.keys.iter().find_map(|(_, key_type)| {
                                    primary_key_type_error(key_type, scope, visiting_tables)
                                })
                            }
                        }
                        None => Some((DIAG_TYPE, "table reference has no canonical primary key")),
                    };
                    visiting_tables.remove(name);
                    result
                }
                _ => Some((DIAG_TYPE, "type has no canonical primary-key encoding")),
            }
        }
        Type::Float => Some((
            DIAG_PRIMARY_KEY_FLOAT,
            "Float is not a valid primary-key type",
        )),
        Type::Applied { base, .. } if base == "Float" => Some((
            DIAG_PRIMARY_KEY_FLOAT,
            "Float is not a valid primary-key type",
        )),
        Type::Range(_) => Some((
            DIAG_PRIMARY_KEY_RANGE,
            "Range<T> is not a primary-key type in version 1.0",
        )),
        Type::Applied { base, .. } if base == "Range" => Some((
            DIAG_PRIMARY_KEY_RANGE,
            "Range<T> is not a primary-key type in version 1.0",
        )),
        Type::Error => None,
        _ => Some((DIAG_TYPE, "type has no canonical primary-key encoding")),
    }
}

pub(crate) fn scope_named_symbol<'a>(name: &str, scope: &'a Scope) -> Option<&'a Symbol> {
    if let Some(symbol) = scope.names.get(name) {
        return Some(symbol);
    }
    let path = name.split('.').collect::<Vec<_>>();
    if let Some(namespace) = module_member_namespace(&path, scope) {
        return scope
            .available_modules
            .get(&namespace)
            .and_then(|module| module.symbols.get(*path.last()?));
    }
    let mut matches = scope
        .names
        .values()
        .filter(|symbol| symbol.ty == Type::Named(name.to_owned()));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

pub(crate) fn nominal_row_type(item: &Item) -> Option<(String, Type)> {
    let Declaration::Type {
        name,
        representation: TypeRepresentation::Nominal { members },
        ..
    } = &item.declaration
    else {
        return None;
    };
    let fields = members
        .iter()
        .filter_map(|member| match member {
            TypeMember::Field { name, ty, .. } => Some((name.clone(), type_of(ty))),
            TypeMember::Assertion { .. } | TypeMember::Implementation { .. } => None,
        })
        .collect();
    Some((name.clone(), Type::Record(fields)))
}

/// Spellings the defining module uses for its nominal and alias types and for the names it
/// imports, mapped to canonical identities. A locally declared name is never an import. An
/// imported alias maps to its exporter only when exactly one other module exports it.
pub(crate) fn secret_identity_spellings(
    namespace: &Namespace,
    tree: &SyntaxTree,
    scope: &Scope,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) -> BTreeMap<String, String> {
    let mut spellings = scope.nominal_identities.clone();
    let mut declared = BTreeSet::new();
    for item in &tree.items {
        let Declaration::Type {
            name,
            generics,
            representation,
        } = &item.declaration
        else {
            continue;
        };
        declared.insert(name.as_str());
        match representation {
            TypeRepresentation::Alias { refinements, .. }
                if generics.is_empty() && refinements.is_empty() =>
            {
                spellings.insert(name.clone(), nominal_identity(namespace, name));
            }
            _ => {}
        }
    }
    for (name, symbol) in &scope.names {
        if declared.contains(name.as_str())
            || spellings.contains_key(name)
            || symbol.kind != SymbolKind::Type
            || symbol.table_schema.is_some()
        {
            continue;
        }
        let Type::Named(target) = &symbol.ty else {
            continue;
        };
        if target != name {
            continue;
        }
        spellings.insert(name.clone(), imported_type_identity(name, tree, modules));
    }
    spellings
}

/// Canonical identity of the type `name` that `tree` imports. The identity comes from the
/// module the import names (`use <path>.{name}`, or a glob of that path), never from a search
/// across modules. When that module or its export cannot be determined, the name is treated as
/// secret-possible (fail closed) via the `Secret` identity, which the secret walk always accepts.
pub(crate) fn imported_type_identity(
    name: &str,
    tree: &SyntaxTree,
    modules: &BTreeMap<Namespace, ModuleHeader>,
) -> String {
    let imported_from = |explicit: bool| {
        tree.items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Use { path, tail } => {
                    let names_it = match tail {
                        UseTail::Names(names) => explicit && names.iter().any(|n| n.name == name),
                        UseTail::Glob { .. } => !explicit,
                        UseTail::None | UseTail::Alias { .. } => false,
                    };
                    names_it.then(|| Namespace(path.iter().map(|s| s.name.clone()).collect()))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mut sources = imported_from(true);
    if sources.is_empty() {
        sources = imported_from(false);
    }
    for target in sources {
        let exported = modules
            .get(&target)
            .and_then(|header| header.exports.get(name))
            .is_some_and(|export| export.kind == SymbolKind::Type);
        if exported {
            return nominal_identity(&target, name);
        }
    }
    FAIL_CLOSED_SECRET_IDENTITY.to_owned()
}
