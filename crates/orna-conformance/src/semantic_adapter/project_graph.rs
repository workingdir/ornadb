use super::*;

/// Public tables and functions of every admitted application module, keyed by
/// semantic namespace, with the prefix of that module's retained function keys.
pub(super) struct ProjectGraph {
    pub(super) modules: BTreeMap<Namespace, GraphModule>,
}

pub(super) struct GraphModule {
    pub(super) function_prefix: String,
    pub(super) tables: BTreeSet<String>,
    pub(super) functions: BTreeSet<String>,
}

impl ProjectGraph {
    pub(super) fn new(project: &ProjectUnit) -> Self {
        let mut modules = BTreeMap::new();
        for unit in project
            .modules
            .iter()
            .filter(|unit| !is_standard_source(unit))
        {
            let (Some(namespace), Ok(function_prefix)) = (
                project_semantic_namespace(project, unit),
                project_transaction_namespace(project, unit),
            ) else {
                continue;
            };
            let mut tables = BTreeSet::new();
            let mut functions = BTreeSet::new();
            for item in parse_module(&unit.source).value.items {
                if !matches!(item.visibility, orna_syntax_ast::Visibility::Public { .. }) {
                    continue;
                }
                match item.declaration {
                    Declaration::Table { name, .. } => {
                        tables.insert(name);
                    }
                    Declaration::Function { signature, .. } => {
                        functions.insert(signature.name);
                    }
                    _ => {}
                }
            }
            modules.insert(
                namespace,
                GraphModule {
                    function_prefix,
                    tables,
                    functions,
                },
            );
        }
        Self { modules }
    }
}

/// How one module's source spellings reach project tables and functions,
/// resolved once at admission so the retained evaluator and the table effect
/// handler only ever see stable identities. Local declarations win, then
/// explicit imports, then unambiguous wildcard imports (ORNA-IMPORT-003,
/// ORNA-IMPORT-004, ORNA-IMPORT-007). A qualified path starts at a `use`
/// binding (ORNA-IMPORT-001, ORNA-IMPORT-002) or spells a module's full
/// namespace path (ORNA-NS-001, ORNA-NS-002), with or without `use`.
pub(super) struct ModuleReferences<'a> {
    pub(super) graph: &'a ProjectGraph,
    pub(super) namespace: Namespace,
    pub(super) local_tables: BTreeSet<String>,
    pub(super) tables: BTreeMap<String, String>,
    pub(super) functions: BTreeMap<String, String>,
    pub(super) modules: BTreeMap<String, Namespace>,
    pub(super) declared: BTreeSet<String>,
}

impl<'a> ModuleReferences<'a> {
    pub(super) fn new(
        namespace: &Namespace,
        items: &[orna_syntax_ast::Item],
        graph: &'a ProjectGraph,
    ) -> Self {
        let mut declared = BTreeSet::new();
        let mut local_tables = BTreeSet::new();
        let mut tables = BTreeMap::new();
        for item in items {
            match &item.declaration {
                Declaration::Table { name, .. } => {
                    local_tables.insert(name.clone());
                    tables.insert(name.clone(), qualified_table_identity(namespace, name));
                    declared.insert(name.clone());
                }
                Declaration::Function { signature, .. } => {
                    declared.insert(signature.name.clone());
                }
                Declaration::Type { name, .. }
                | Declaration::Enum { name, .. }
                | Declaration::Protocol { name, .. }
                | Declaration::Dimension { name, .. }
                | Declaration::Unit { name, .. } => {
                    declared.insert(name.clone());
                }
                Declaration::Let { pattern, .. } => {
                    declared.extend(relation_pattern_names(pattern))
                }
                _ => {}
            }
        }
        let mut functions = BTreeMap::new();
        let mut modules = BTreeMap::new();
        let mut glob_tables = BTreeMap::<String, BTreeSet<String>>::new();
        let mut glob_functions = BTreeMap::<String, BTreeSet<String>>::new();
        for item in items {
            let Declaration::Use { path, tail } = &item.declaration else {
                continue;
            };
            let target = Namespace(path.iter().map(|segment| segment.name.clone()).collect());
            let module = graph.modules.get(&target);
            match tail {
                orna_syntax_ast::UseTail::None => {
                    if let Some(last) = target.0.last() {
                        modules.insert(last.clone(), target.clone());
                    }
                }
                orna_syntax_ast::UseTail::Alias { name, .. } if name != "_" => {
                    modules.insert(name.clone(), target.clone());
                }
                orna_syntax_ast::UseTail::Alias { .. } => {}
                orna_syntax_ast::UseTail::Names(names) => {
                    let Some(module) = module else { continue };
                    for import in names {
                        if module.tables.contains(&import.name) {
                            tables
                                .entry(import.name.clone())
                                .or_insert_with(|| qualified_table_identity(&target, &import.name));
                        }
                        if module.functions.contains(&import.name)
                            && !declared.contains(&import.name)
                        {
                            functions.entry(import.name.clone()).or_insert_with(|| {
                                format!("{}.{}", module.function_prefix, import.name)
                            });
                        }
                    }
                }
                orna_syntax_ast::UseTail::Glob { .. } => {
                    let Some(module) = module else { continue };
                    for name in &module.tables {
                        glob_tables
                            .entry(name.clone())
                            .or_default()
                            .insert(qualified_table_identity(&target, name));
                    }
                    for name in &module.functions {
                        glob_functions
                            .entry(name.clone())
                            .or_default()
                            .insert(format!("{}.{name}", module.function_prefix));
                    }
                }
            }
        }
        for (name, identities) in glob_tables {
            if identities.len() == 1 && !declared.contains(&name) {
                let identity = identities.into_iter().next().expect("one identity");
                tables.entry(name).or_insert(identity);
            }
        }
        for (name, keys) in glob_functions {
            if keys.len() == 1 && !declared.contains(&name) {
                let key = keys.into_iter().next().expect("one function key");
                functions.entry(name).or_insert(key);
            }
        }
        Self {
            graph,
            namespace: namespace.clone(),
            local_tables,
            tables,
            functions,
            modules,
            declared,
        }
    }

    /// Keys this module's table surfaces by qualified identity and rewrites
    /// every table and function reference in its bodies and assertions.
    pub(super) fn resolve(&self, admitted: &mut AdmittedTransaction) {
        for table in &self.local_tables {
            let identity = qualified_table_identity(&self.namespace, table);
            if &identity == table {
                continue;
            }
            if let Some(mut key) = admitted.key_fields.remove(table) {
                key.table.clone_from(&identity);
                admitted.key_fields.insert(identity.clone(), key);
            }
            if let Some(fields) = admitted.table_fields.remove(table) {
                admitted.table_fields.insert(identity.clone(), fields);
            }
            if let Some(fields) = admitted.float_fields.floats.remove(table) {
                admitted
                    .float_fields
                    .floats
                    .insert(identity.clone(), fields);
            }
            if let Some(fields) = admitted.float_fields.decimals.remove(table) {
                admitted
                    .float_fields
                    .decimals
                    .insert(identity.clone(), fields);
            }
            if let Some(mut assertions) = admitted.table_assertions.remove(table) {
                for assertion in &mut assertions {
                    assertion.owner_name.clone_from(&identity);
                    assertion.dependencies.remove(table);
                    assertion.dependencies.insert(identity.clone());
                }
                admitted
                    .table_assertions
                    .insert(identity.clone(), assertions);
            }
        }
        for function in admitted.functions.local_values_mut() {
            let mut shadowed = BTreeSet::new();
            for parameter in &function.parameters {
                shadowed.extend(relation_pattern_names(&parameter.pattern));
            }
            self.expression(&mut function.body, &shadowed);
        }
        for assertion in admitted
            .table_assertions
            .values_mut()
            .flatten()
            .chain(admitted.module_assertions.iter_mut())
        {
            self.expression(&mut assertion.expression, &BTreeSet::new());
        }
    }

    /// The namespace owning `path.last()`, through a `use` binding or a full
    /// namespace path; declared names take precedence over namespace paths.
    pub(super) fn member_namespace(&self, path: &[&str]) -> Option<Namespace> {
        let (first, rest) = path.split_first()?;
        let (_, intermediate) = rest.split_last()?;
        if let Some(root) = self.modules.get(*first) {
            let mut namespace = root.clone();
            namespace
                .0
                .extend(intermediate.iter().map(|part| (*part).to_owned()));
            return Some(namespace);
        }
        if matches!(*first, "std" | "sys")
            || self.declared.contains(*first)
            || self.tables.contains_key(*first)
            || self.functions.contains_key(*first)
        {
            return None;
        }
        Some(Namespace(
            path[..path.len() - 1]
                .iter()
                .map(|part| (*part).to_owned())
                .collect(),
        ))
    }

    pub(super) fn table_path(&self, path: &[&str]) -> Option<String> {
        let namespace = self.member_namespace(path)?;
        let name = *path.last()?;
        self.graph
            .modules
            .get(&namespace)
            .is_some_and(|module| module.tables.contains(name))
            .then(|| qualified_table_identity(&namespace, name))
    }

    pub(super) fn function_path(&self, path: &[&str]) -> Option<String> {
        let namespace = self.member_namespace(path)?;
        let name = *path.last()?;
        let module = self.graph.modules.get(&namespace)?;
        module
            .functions
            .contains(name)
            .then(|| format!("{}.{name}", module.function_prefix))
    }

    pub(super) fn callee(&self, callee: &mut Expr, shadowed: &BTreeSet<String>) {
        let resolved = match &*callee {
            Expr::Name { text, .. } if !shadowed.contains(text) => {
                self.functions.get(text).cloned()
            }
            Expr::Field { .. } => reference_path(callee)
                .filter(|path| !shadowed.contains(path[0]))
                .and_then(|path| {
                    self.function_path(&path)
                        .filter(|key| *key != path.join("."))
                }),
            _ => None,
        };
        match resolved {
            Some(key) => {
                let span = callee.span();
                *callee = Expr::Name { text: key, span };
            }
            None => self.expression(callee, shadowed),
        }
    }

    pub(super) fn expression(&self, expression: &mut Expr, shadowed: &BTreeSet<String>) {
        if let Some(path) = reference_path(expression)
            && !shadowed.contains(path[0])
        {
            let identity = match &*expression {
                Expr::Name { text, .. } => self.tables.get(text.as_str()).cloned(),
                _ => self.table_path(&path),
            };
            if let Some(identity) = identity {
                let span = expression.span();
                *expression = Expr::Name {
                    text: identity,
                    span,
                };
                return;
            }
        }
        match expression {
            Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
            Expr::InterpolatedString { segments, .. } => {
                for segment in segments {
                    if let orna_syntax_ast::StringSegment::Expression { value, .. } = segment {
                        self.expression(value, shadowed);
                    }
                }
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
                self.expression(rhs, shadowed);
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expression(lhs, shadowed);
                self.expression(rhs, shadowed);
            }
            Expr::Range { lower, upper, .. } => {
                for endpoint in [lower, upper].into_iter().flatten() {
                    self.expression(endpoint, shadowed);
                }
            }
            Expr::Call {
                callee, arguments, ..
            }
            | Expr::GenericCall {
                callee, arguments, ..
            } => {
                self.callee(callee, shadowed);
                for argument in arguments {
                    self.expression(&mut argument.value, shadowed);
                }
            }
            Expr::Index { base, index, .. } => {
                self.expression(base, shadowed);
                self.expression(index, shadowed);
            }
            Expr::Field { base, .. } => self.expression(base, shadowed),
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for element in elements {
                    self.expression(element, shadowed);
                }
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                for field in fields {
                    self.expression(&mut field.value, shadowed);
                }
            }
            Expr::Lambda {
                parameters, body, ..
            } => {
                let mut shadowed = shadowed.clone();
                for parameter in parameters.iter() {
                    shadowed.extend(relation_pattern_names(&parameter.pattern));
                }
                self.expression(body, &shadowed);
            }
            Expr::Block {
                statements, tail, ..
            } => {
                let mut shadowed = shadowed.clone();
                for statement in statements {
                    self.statement(statement, &mut shadowed);
                }
                if let Some(tail) = tail {
                    self.expression(tail, &shadowed);
                }
            }
            Expr::Control {
                binding,
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                if let Some(condition) = condition {
                    self.expression(condition, shadowed);
                }
                if let Some(body) = body {
                    let mut shadowed = shadowed.clone();
                    if let Some(binding) = binding {
                        shadowed.extend(relation_pattern_names(binding));
                    }
                    self.expression(body, &shadowed);
                }
                for arm in arms {
                    let mut shadowed = shadowed.clone();
                    shadowed.extend(relation_pattern_names(&arm.pattern));
                    if let Some(guard) = &mut arm.guard {
                        self.expression(guard, &shadowed);
                    }
                    self.expression(&mut arm.body, &shadowed);
                }
                if let Some(alternate) = alternate {
                    self.expression(alternate, shadowed);
                }
            }
        }
    }

    pub(super) fn statement(
        &self,
        statement: &mut orna_syntax_ast::Statement,
        shadowed: &mut BTreeSet<String>,
    ) {
        use orna_syntax_ast::Statement;
        match statement {
            Statement::Let { pattern, value, .. } => {
                self.expression(value, shadowed);
                shadowed.extend(relation_pattern_names(pattern));
            }
            Statement::Assert { value, .. }
            | Statement::Expression { value, .. }
            | Statement::Control { value, .. } => self.expression(value, shadowed),
            Statement::Return { value, .. } | Statement::Break { value, .. } => {
                if let Some(value) = value {
                    self.expression(value, shadowed);
                }
            }
            Statement::Assignment { target, value, .. } => {
                self.assignment_target(target, shadowed);
                self.expression(value, shadowed);
            }
            Statement::Continue { .. } => {}
        }
    }

    pub(super) fn assignment_target(
        &self,
        target: &mut orna_syntax_ast::AssignmentTarget,
        shadowed: &BTreeSet<String>,
    ) {
        match target {
            orna_syntax_ast::AssignmentTarget::Name { .. } => {}
            orna_syntax_ast::AssignmentTarget::Field { base, .. } => {
                self.assignment_target(base, shadowed);
            }
            orna_syntax_ast::AssignmentTarget::Index { base, index, .. } => {
                self.assignment_target(base, shadowed);
                self.expression(index, shadowed);
            }
        }
    }
}

/// The dotted source path of a name or field chain (`a.b.Item`).
pub(super) fn reference_path(expression: &Expr) -> Option<Vec<&str>> {
    match expression {
        Expr::Name { text, .. } => Some(vec![text.as_str()]),
        Expr::Field { base, name, .. } => {
            let mut path = reference_path(base)?;
            path.push(name.as_str());
            Some(path)
        }
        _ => None,
    }
}

pub(super) fn request_mutation_scope(request: &RequestIdentity) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ORNA-SOURCE-MUTATION-SCOPE\0");
    digest.update(request.session_id);
    digest.update(request.request_id);
    digest.finalize().into()
}

pub(super) fn durable_activation_digest(
    previous: [u8; 32],
    mutations: &[TableMutation],
) -> [u8; 32] {
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

pub(super) fn admitted_transaction_module(
    items: &[orna_syntax_ast::Item],
    namespace: Option<&str>,
    module_owner_name: Option<&str>,
    tolerant_keys: bool,
) -> Result<AdmittedTransaction, String> {
    let mut functions = Functions::new();
    let mut key_fields = BTreeMap::new();
    let mut float_fields = TableFloatFields::new();
    let mut table_fields = TableFields::new();
    let mut assertions = TableAssertions::new();
    let mut module_assertions = ModuleAssertions::new();
    for (source_order, item) in items.iter().enumerate() {
        match &item.declaration {
            Declaration::Function { signature, body } => {
                let name = namespace.map_or_else(
                    || signature.name.clone(),
                    |namespace| format!("{namespace}.{}", signature.name),
                );
                functions.insert(
                    name,
                    RetainedFunction {
                        parameters: signature.parameters.clone(),
                        body: body.clone(),
                        environment: Environment::new(),
                    },
                );
            }
            Declaration::Table {
                name,
                keys,
                members,
            } => {
                let fields = keys
                    .iter()
                    .map(|key| match &key.pattern {
                        Pattern::Name(field, _) => Ok(field.clone()),
                        _ => Err("transactional source seam requires named table keys".into()),
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                // ORNA-AUTOID-001: a keyless table carries the implicit automatic
                // key `id: Int`, matching the admission the semantic model records.
                let (fields, key_types) = if keys.is_empty() {
                    (vec![String::from("id")], Ok(vec![TransactionKeyType::Int]))
                } else {
                    (
                        fields,
                        keys.iter()
                            .map(|key| {
                                key.annotation
                                    .as_ref()
                                    .ok_or_else(|| {
                                        "transactional source seam requires typed table keys"
                                            .to_owned()
                                    })
                                    .and_then(transaction_key_type_from_syntax)
                            })
                            .collect::<Result<Vec<_>, String>>(),
                    )
                };
                match key_types {
                    Ok(key_types) => {
                        key_fields.insert(
                            name.clone(),
                            TransactionTableKey::new(name.clone(), fields, key_types)?,
                        );
                    }
                    // Bead ornadb-gq4g: a key admission cannot lower yet. The
                    // tolerant commit path keeps the table's rows and assertions
                    // without its key; every other path refuses the project.
                    Err(reason) if !tolerant_keys => return Err(reason),
                    Err(_) => {}
                }
                table_fields.insert(
                    name.clone(),
                    members
                        .iter()
                        .filter_map(|member| match member {
                            orna_syntax_ast::TableMember::Field { name, .. } => Some(name.clone()),
                            _ => None,
                        })
                        .collect(),
                );
                float_fields.insert(
                    name.clone(),
                    members
                        .iter()
                        .filter_map(|member| match member {
                            orna_syntax_ast::TableMember::Field {
                                name,
                                ty: orna_syntax_ast::TypeExpr::Name { path, .. },
                                ..
                            } if path.len() == 1 && path[0] == "Float" => Some(name.clone()),
                            _ => None,
                        })
                        .collect(),
                );
                float_fields.insert_decimal(
                    name.clone(),
                    members
                        .iter()
                        .filter_map(|member| match member {
                            orna_syntax_ast::TableMember::Field {
                                name,
                                ty: orna_syntax_ast::TypeExpr::Name { path, .. },
                                ..
                            } if path.len() == 1 && path[0] == "Decimal" => Some(name.clone()),
                            _ => None,
                        })
                        .collect(),
                );
                let expressions = members
                    .iter()
                    .enumerate()
                    .filter_map(|(source_order, member)| match member {
                        orna_syntax_ast::TableMember::Assertion { value, span } => {
                            Some(AdmittedAssertion {
                                expression: value.clone(),
                                declaration_span: span.clone(),
                                owner_kind: AssertionOwnerKind::Table,
                                owner_name: name.clone(),
                                source_order,
                                dependencies: BTreeSet::from([name.clone()]),
                            })
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !expressions.is_empty() {
                    assertions.insert(name.clone(), expressions);
                }
            }
            Declaration::Assertion { value } => module_assertions.push(AdmittedAssertion {
                expression: value.clone(),
                // The syntax AST does not retain a separate declaration span
                // for module assertions; retain the parser expression span
                // until a pinned source document is available.
                declaration_span: value.span(),
                owner_kind: AssertionOwnerKind::Module,
                owner_name: module_owner_name.unwrap_or_default().to_owned(),
                source_order,
                dependencies: BTreeSet::new(),
            }),
            // Import declarations were resolved against the complete project
            // graph before this per-module retention pass. Qualified calls are
            // resolved by the evaluator against the retained function map.
            Declaration::Use { .. } => {}
            // Enums execute as construction plans built for the whole module
            // below (ORNA-ENUM-001..003).
            Declaration::Enum { .. } => {}
            // Types and related declarations affect semantic admission but
            // have no activation-time effect in this bounded table runtime.
            // Retaining them as inert preserves project module boundaries.
            Declaration::Protocol { .. }
            | Declaration::Dimension { .. }
            | Declaration::Unit { .. }
            | Declaration::Type { .. } => {}
            Declaration::Let { .. } => {
                return Err(
                    "transactional source seam does not execute module-level bindings".into(),
                );
            }
            // ORNA-NATIVE-001: a native declaration has no body. This bounded
            // seam retains executable source, so it rejects the declaration
            // rather than reading it as a function. ORNA-NATIVE-004 admits one
            // in any module; the link step binds it (library document, stage N2).
            Declaration::Native { .. }
            | Declaration::NativeType { .. }
            | Declaration::NativeStatic { .. } => {
                return Err("transactional source seam does not bind native declarations".into());
            }
        }
    }
    Ok(AdmittedTransaction {
        functions,
        key_fields,
        float_fields,
        table_fields,
        table_assertions: assertions,
        module_assertions,
        nominal_definitions: source_enum_definitions(namespace, items),
    })
}

pub(super) fn attach_assertion_dependencies(
    table_assertions: &mut TableAssertions,
    module_assertions: &mut ModuleAssertions,
    plans: &[AssertionPlan],
) -> Result<(), String> {
    let mut used = BTreeSet::new();
    for assertions in table_assertions.values_mut() {
        for assertion in assertions {
            let owner = AssertionOwner::Table(assertion.owner_name.clone());
            let (index, plan) = plans
                .iter()
                .enumerate()
                .find(|(index, plan)| !used.contains(index) && plan.owner == owner)
                .ok_or_else(|| "transactional assertion metadata is incomplete".to_owned())?;
            used.insert(index);
            assertion.dependencies = plan.dependencies.clone();
            assertion.dependencies.insert(assertion.owner_name.clone());
        }
    }
    for assertion in module_assertions {
        let (index, plan) = plans
            .iter()
            .enumerate()
            .find(|(index, plan)| !used.contains(index) && plan.owner == AssertionOwner::Module)
            .ok_or_else(|| "transactional assertion metadata is incomplete".to_owned())?;
        used.insert(index);
        assertion.dependencies = plan.dependencies.clone();
    }
    Ok(())
}

/// Table names named by a dependency set; the incomplete marker names no table.
pub(super) fn concrete_dependency_tables(
    dependencies: &BTreeSet<String>,
) -> impl Iterator<Item = String> + '_ {
    dependencies
        .iter()
        .filter(|table| table.as_str() != UNRESOLVED_TABLE_DEPENDENCY)
        .cloned()
}

pub(super) fn applicable_module_assertions(
    table: &str,
    assertions: &[AdmittedAssertion],
) -> Result<Vec<AdmittedAssertion>, String> {
    let changed = BTreeSet::from([table.to_owned()]);
    let applicable = assertions
        .iter()
        .filter(|assertion| assertion_dependencies_apply(&assertion.dependencies, &changed))
        .cloned()
        .collect::<Vec<_>>();
    if applicable.len() > 1 {
        return Err(
            "stream admission requires one applicable module assertion until committed module identities are available".into(),
        );
    }
    Ok(applicable)
}
