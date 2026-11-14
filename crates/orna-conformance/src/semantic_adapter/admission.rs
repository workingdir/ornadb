use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AdmittedAssertion {
    pub(super) expression: Expr,
    pub(super) declaration_span: SourceSpan,
    pub(super) owner_kind: AssertionOwnerKind,
    pub(super) owner_name: String,
    pub(super) source_order: usize,
    pub(super) dependencies: BTreeSet<String>,
}

pub(super) type TableAssertions = BTreeMap<String, Vec<AdmittedAssertion>>;
pub(super) type ModuleAssertions = Vec<AdmittedAssertion>;
pub(super) type TableKeys = BTreeMap<String, TransactionTableKey>;
#[derive(Clone, Default)]
pub(super) struct TableFloatFields {
    pub(super) floats: BTreeMap<String, BTreeSet<String>>,
    pub(super) decimals: BTreeMap<String, BTreeSet<String>>,
}
impl TableFloatFields {
    pub(super) fn new() -> Self {
        Self::default()
    }
    pub(super) fn clear(&mut self) {
        self.floats.clear();
        self.decimals.clear();
    }
    pub(super) fn insert(&mut self, table: String, fields: BTreeSet<String>) {
        self.floats.insert(table, fields);
    }
    pub(super) fn insert_decimal(&mut self, table: String, fields: BTreeSet<String>) {
        self.decimals.insert(table, fields);
    }
    pub(super) fn extend(&mut self, other: Self) {
        self.floats.extend(other.floats);
        self.decimals.extend(other.decimals);
    }
    pub(super) fn get(&self, table: &str) -> Option<&BTreeSet<String>> {
        self.floats.get(table)
    }
    pub(super) fn contains_decimal(&self, table: &str, field: &str) -> bool {
        self.decimals
            .get(table)
            .is_some_and(|fields| fields.contains(field))
    }
}
pub(super) type TableFields = BTreeMap<String, Vec<String>>;
/// Complete admission result for one transactional entry point: lowered
/// functions plus every declared table and assertion surface.
pub(super) struct AdmittedTransaction {
    pub(super) functions: Functions,
    pub(super) key_fields: TableKeys,
    pub(super) float_fields: TableFloatFields,
    pub(super) table_fields: TableFields,
    pub(super) table_assertions: TableAssertions,
    pub(super) module_assertions: ModuleAssertions,
    /// Construction plans of the admitted source enums (ORNA-ENUM-001..003).
    pub(super) nominal_definitions: NominalDefinitions,
}

pub(super) type AdmissionFailure = Box<StageOutcome<Diagnostic>>;

pub(super) fn admit_transaction_source(
    unit: &SourceUnit,
    limits: EvaluatorLimits,
    entry: &str,
    catalogue: &Catalogue,
) -> Result<AdmittedTransaction, AdmissionFailure> {
    admit_transaction_source_inner(unit, limits, entry, true, catalogue)
}

pub(super) fn admit_transaction_source_inner(
    unit: &SourceUnit,
    limits: EvaluatorLimits,
    entry: &str,
    require_entry: bool,
    catalogue: &Catalogue,
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
    // The single-source seam admits one standalone program, so it is the root
    // module: its tables keep bare identities, matching the keys below and
    // the assertion dependencies semantic analysis plans for them.
    let analysis = analyze_with_providers(
        &[ModuleInput::new("main.orna", unit.source.clone())],
        catalogue,
        SystemProviders::Production,
    );
    if let Some(summary) = analysis
        .modules
        .values()
        .find_map(|module| module.symbols.get(entry).map(|symbol| &symbol.effects))
        && !controlled_table_effects(summary)
    {
        return Err(Box::new(
            StageOutcome::Failed(effect_admission_diagnostic()),
        ));
    }
    if let Some(diagnostic) = analysis.diagnostics.first() {
        return Err(Box::new(StageOutcome::Failed(
            diagnostic.clone().redacted(),
        )));
    }
    let mut admitted =
        admitted_transaction_module(&parsed.value.items, None, Some(&unit.source_id), false)
            .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
    let plans = analysis
        .assertions
        .values()
        .next()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    attach_assertion_dependencies(
        &mut admitted.table_assertions,
        &mut admitted.module_assertions,
        plans,
    )
    .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
    let AdmittedTransaction {
        functions,
        key_fields,
        float_fields,
        table_fields,
        table_assertions,
        module_assertions,
        nominal_definitions,
    } = admitted;
    if let Err(error) = limits.check_items(functions.len()) {
        return Err(Box::new(StageOutcome::Failed(error.diagnostic().clone())));
    }
    if require_entry && !functions.contains_key(entry) {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "configured transaction entry function is not present".into(),
        }));
    }
    Ok(AdmittedTransaction {
        functions,
        key_fields,
        float_fields,
        table_fields,
        table_assertions,
        module_assertions,
        nominal_definitions,
    })
}

/// Admits a project as a graph of independently parsed modules.  The retained
/// evaluator receives qualified function identities (for example
/// `main.seed` and `inventory.add`) so its native qualified-call resolver
/// preserves module boundaries rather than relying on source concatenation.
pub(super) fn admit_transaction_project_with(
    project: &ProjectUnit,
    limits: EvaluatorLimits,
    entry: &str,
    catalogue: &Catalogue,
    tolerant_keys: bool,
) -> Result<AdmittedTransaction, AdmissionFailure> {
    if project.expectations.environment.stdlib.is_some() {
        return Err(Box::new(
            StageOutcome::Failed(standard_profile_diagnostic()),
        ));
    }
    for unit in project
        .modules
        .iter()
        .filter(|unit| is_standard_source(unit))
    {
        let profile = catalogue
            .standard_dependency_profile()
            .ok_or_else(|| Box::new(StageOutcome::Failed(standard_profile_diagnostic())))?;
        profile
            .verify_source(&unit.source_id, &unit.source)
            .map_err(|_| Box::new(StageOutcome::Failed(standard_profile_diagnostic())))?;
    }
    let application_modules = project
        .modules
        .iter()
        .filter(|unit| !is_standard_source(unit))
        .collect::<Vec<_>>();
    let analysis = analyze_with_providers(
        &application_modules
            .iter()
            .map(|unit| {
                let prefix = format!("{}/", project.project_id.trim_end_matches('/'));
                let path = unit
                    .source_id
                    .strip_prefix(&prefix)
                    .unwrap_or(&unit.source_id)
                    .to_owned();
                ModuleInput::new(path, unit.source.clone())
            })
            .collect::<Vec<_>>(),
        catalogue,
        SystemProviders::Production,
    );
    if let Some(diagnostic) = analysis.diagnostics.first() {
        return Err(Box::new(StageOutcome::Failed(
            diagnostic.clone().redacted(),
        )));
    }
    if let Some(summary) = project_entry_effects(&analysis, project, entry)
        && !controlled_table_effects(summary)
    {
        return Err(Box::new(
            StageOutcome::Failed(effect_admission_diagnostic()),
        ));
    }
    let mut functions = Functions::new();
    let mut key_fields = BTreeMap::new();
    let mut float_fields = TableFloatFields::new();
    let mut table_fields = TableFields::new();
    let mut table_assertions = TableAssertions::new();
    let mut module_assertions = ModuleAssertions::new();
    let graph = ProjectGraph::new(project);
    let mut nominal_definitions = NominalDefinitions::new();
    for unit in &project.modules {
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
        let namespace = project_transaction_namespace(project, unit)
            .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
        let semantic_namespace = project_semantic_namespace(project, unit).ok_or_else(|| {
            Box::new(StageOutcome::Skipped {
                reason: "project transaction module has no semantic namespace".into(),
            })
        })?;
        let mut module = admitted_transaction_module(
            &parsed.value.items,
            Some(&namespace),
            Some(&namespace),
            tolerant_keys,
        )
        .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
        if !is_standard_source(unit) {
            let plans = analysis
                .assertions
                .get(&semantic_namespace)
                .ok_or_else(|| {
                    Box::new(StageOutcome::Skipped {
                        reason: "project transaction assertion metadata is incomplete".into(),
                    })
                })?;
            attach_assertion_dependencies(
                &mut module.table_assertions,
                &mut module.module_assertions,
                plans,
            )
            .map_err(|reason| Box::new(StageOutcome::Skipped { reason }))?;
            ModuleReferences::new(&semantic_namespace, &parsed.value.items, &graph)
                .resolve(&mut module);
        }
        let AdmittedTransaction {
            functions: module_functions,
            key_fields: module_keys,
            float_fields: module_float_fields,
            table_fields: module_table_fields,
            table_assertions: module_assertions_by_table,
            module_assertions: module_assertions_only,
            nominal_definitions: module_nominal_definitions,
        } = module;
        if let Err(error) = limits.check_items(module_functions.len()) {
            return Err(Box::new(StageOutcome::Failed(error.diagnostic().clone())));
        }
        if module_functions
            .keys()
            .any(|name| functions.contains_key(name))
        {
            return Err(Box::new(StageOutcome::Skipped {
                reason: "project transaction modules declare a duplicate qualified function".into(),
            }));
        }
        if module_keys.keys().any(|name| key_fields.contains_key(name)) {
            return Err(Box::new(StageOutcome::Skipped {
                reason: "project transaction modules declare a duplicate table identity".into(),
            }));
        }
        functions.extend(module_functions).map_err(|_| {
            Box::new(StageOutcome::Skipped {
                reason: "project transaction modules bind a second standard layer".into(),
            })
        })?;
        key_fields.extend(module_keys);
        float_fields.extend(module_float_fields);
        table_fields.extend(module_table_fields);
        table_assertions.extend(module_assertions_by_table);
        module_assertions.extend(module_assertions_only);
        if !is_standard_source(unit) {
            nominal_definitions.extend(module_nominal_definitions);
        }
    }
    let mut standard_aliases = BTreeMap::<String, String>::new();
    for unit in &project.modules {
        let parsed = parse_module(&unit.source);
        for item in parsed.value.items {
            let Declaration::Use {
                path,
                tail: orna_syntax_ast::UseTail::Names(names),
            } = item.declaration
            else {
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
            for name in names {
                let target = format!("{namespace}.{}", name.name);
                if let Some(previous) = standard_aliases.insert(name.name.clone(), target.clone())
                    && previous != target
                {
                    return Err(Box::new(StageOutcome::Skipped {
                        reason: "standard imports have conflicting unqualified function names"
                            .into(),
                    }));
                }
            }
        }
    }
    for (alias, target) in standard_aliases {
        if let Some(function) = functions.get(&target).cloned()
            && !functions.contains_key(&alias)
        {
            functions.insert(alias, function);
        }
    }
    // Candidate validation can admit a project without executing a transaction entry.
    if !entry.is_empty() && !functions.contains_key(entry) {
        return Err(Box::new(StageOutcome::Skipped {
            reason: "configured qualified project transaction entry function is not present".into(),
        }));
    }
    Ok(AdmittedTransaction {
        functions,
        key_fields,
        float_fields,
        table_fields,
        table_assertions,
        module_assertions,
        nominal_definitions,
    })
}

/// Admits a project for the strict seam: a table whose primary key cannot be
/// lowered skips the whole project, as before.
pub(super) fn admit_transaction_project(
    project: &ProjectUnit,
    limits: EvaluatorLimits,
    entry: &str,
    catalogue: &Catalogue,
) -> Result<AdmittedTransaction, AdmissionFailure> {
    admit_transaction_project_with(project, limits, entry, catalogue, false)
}

/// Admits a project for the commit gate (bead ornadb-gq4g): a table whose
/// primary key cannot be lowered is admitted without its key, so the checks
/// that do not read it still validate.
pub(super) fn admit_transaction_project_tolerant(
    project: &ProjectUnit,
    limits: EvaluatorLimits,
    entry: &str,
    catalogue: &Catalogue,
) -> Result<AdmittedTransaction, AdmissionFailure> {
    admit_transaction_project_with(project, limits, entry, catalogue, true)
}

pub(super) fn is_standard_source(unit: &SourceUnit) -> bool {
    unit.source_id.starts_with("std/")
}

/// Declares the effect set of a semantic activation (REQUEST-1 step 5) through
/// [`EffectGate::admit_sys_host`]. A system host attachment may reach effects outside the
/// table transaction, so the gate reports them possible; a detached activation
/// performs only Orna table writes and receives the controlled marker.
pub(super) fn declared_effect_report(
    sys_host: Option<&SysHostAttachment>,
) -> ActivationEffectReport {
    EffectGate::admit_sys_host(sys_host, None)
}

pub(super) fn controlled_table_effects(summary: &EffectSummary) -> bool {
    !summary.opaque
        && summary.effects.iter().all(|effect| {
            matches!(
                effect,
                orna_check::EffectKind::TableRead | orna_check::EffectKind::TableWrite
            )
        })
}

pub(super) fn effect_admission_diagnostic() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-EVAL-EFFECT").expect("static effect diagnostic code"),
        DiagnosticSeverity::Error,
        SafeText::new("controlled table activation requires only database read/write effects")
            .expect("static effect diagnostic message"),
    )
    .expect("valid effect diagnostic")
    .redacted()
}

pub(super) fn project_entry_effects<'a>(
    analysis: &'a orna_check::Analysis,
    project: &ProjectUnit,
    entry: &str,
) -> Option<&'a EffectSummary> {
    let (entry_namespace, function) = entry.rsplit_once('.')?;
    let unit = project.modules.iter().find(|unit| {
        project_transaction_namespace(project, unit).ok().as_deref() == Some(entry_namespace)
    })?;
    let namespace = project_semantic_namespace(project, unit)?;
    analysis
        .modules
        .get(&namespace)?
        .symbols
        .get(function)
        .map(|symbol| &symbol.effects)
}

pub(super) fn project_semantic_namespace(
    project: &ProjectUnit,
    unit: &SourceUnit,
) -> Option<Namespace> {
    let prefix = format!("{}/", project.project_id.trim_end_matches('/'));
    let path = unit
        .source_id
        .strip_prefix(&prefix)
        .unwrap_or(&unit.source_id);
    let stem = path.strip_suffix(".orna")?;
    let mut components = stem.split('/').map(str::to_owned).collect::<Vec<_>>();
    if components
        .last()
        .is_some_and(|component| component == "main")
    {
        components.pop();
    }
    Some(Namespace(components))
}

pub(super) fn project_transaction_namespace(
    project: &ProjectUnit,
    unit: &SourceUnit,
) -> Result<String, String> {
    let prefix = format!("{}/", project.project_id.trim_end_matches('/'));
    let path = unit
        .source_id
        .strip_prefix(&prefix)
        .unwrap_or(&unit.source_id);
    let stem = path
        .strip_suffix(".orna")
        .ok_or_else(|| "project transaction modules must use .orna logical names".to_owned())?;
    if stem.is_empty() || stem.split('/').any(str::is_empty) {
        return Err("project transaction modules need a non-empty logical namespace".into());
    }
    // `x/main.orna` defines namespace `x` (ORNA-NS-001); only the root
    // `main.orna` keeps the `main` prefix that root functions are run by.
    let stem = stem.strip_suffix("/main").unwrap_or(stem);
    Ok(stem.replace('/', "."))
}

/// The qualified table identity the runtime keys rows by: the module's
/// namespace path plus the declared name, as recorded by the committed
/// identity manifest (root-module tables keep their bare name).
pub(super) fn qualified_table_identity(namespace: &Namespace, name: &str) -> String {
    if namespace.0.is_empty() {
        name.to_owned()
    } else {
        format!("{}.{name}", namespace.0.join("."))
    }
}
