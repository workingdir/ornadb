//! The `ProgramCatalog` host: the `sys.catalog.*` relations and `sys.rt.info` of one lowered program (hir-host.md A5).
//!
//! The provider needs no publication metadata and no snapshot. Its rows are read from the lowered [`Program`] and cover only the
//! program's own modules (`ModuleOrigin::Project`), not bundled `core`, `sys` or `std` code: one `sys.Table` row per table, one
//! `sys.Column` row per table field, one `sys.Function` row per function definition, and one `sys.Dependency` row per distinct `use`
//! edge. A `sys.RuntimeInfo` record the embedder supplies as a [`HostInfo`] answers `sys.rt.info`. Every other `sys` member is
//! answered by the wrapped [`StoreHost`], which serves none, so it stays unlinked.

use std::collections::HashSet;

use num_bigint::BigInt;
use orna_hir::{
    Def, DefId, DefKind, FieldDef, FnDef, ImportTarget, ModuleId, ModuleOrigin, Program, TyKind,
    print_type,
};
use orna_value::{
    ENUM_TAG, OPTION_TAG, Raw, Snapshot, UUID_TAG, enum_type_id, enum_variant_id, qualified_name,
};
use sha2::{Digest, Sha256};

use super::{
    ActivationContext, Host, HostFailure, HostInstant, HostManifest, RowSource, SNAPSHOT_NOT_FOUND,
    StoreHost, SysSlot, UNSUPPORTED,
};
use crate::store::TableStore;

/// The `sys.RuntimeMode` the embedder runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRuntimeMode {
    /// `orna repl`.
    Repl,
    /// `orna run`.
    Run,
    /// `orna serve`.
    Serve,
    /// The command owner of a repository.
    CommandOwner,
    /// Embedded in another host.
    Embedded,
}

impl HostRuntimeMode {
    /// The `sys.RuntimeMode` variant name.
    const fn variant(self) -> &'static str {
        match self {
            Self::Repl => "repl",
            Self::Run => "run",
            Self::Serve => "serve",
            Self::CommandOwner => "command_owner",
            Self::Embedded => "embedded",
        }
    }
}

/// The `sys.RuntimeInfo` record the embedder supplies. Its fields are the record's fields, in declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostInfo {
    /// The language version.
    pub language_version: String,
    /// The `sys` version.
    pub sys_version: String,
    /// The canonical Orna codec version.
    pub canonical_orna_codec_version: String,
    /// The repository layout version.
    pub repository_layout_version: String,
    /// The storage manifest version.
    pub storage_manifest_version: String,
    /// The presentation protocol version.
    pub presentation_protocol_version: String,
    /// The implementation name.
    pub implementation_name: String,
    /// The implementation version.
    pub implementation_version: String,
    /// The build identifier.
    pub build_id: String,
    /// The profiles the implementation supports.
    pub supported_profiles: Vec<String>,
    /// The runtime mode.
    pub mode: HostRuntimeMode,
    /// Whether the runtime is read-only.
    pub read_only: bool,
    /// The unicode version.
    pub unicode_version: String,
    /// The timezone database version, when known.
    pub timezone_database_version: Option<String>,
}

/// A host that serves the catalogue of one lowered program and the runtime information of the embedder.
pub struct ProgramCatalog {
    inner: StoreHost,
    /// The catalogue relations the program supplies, by slot, rows in declaration order.
    relations: Vec<(SysSlot, Vec<Raw>)>,
    info_slot: SysSlot,
    info: Raw,
}

impl ProgramCatalog {
    /// A host over `store` that serves the catalogue of `program` and the runtime information `info`.
    #[must_use]
    pub fn new(program: &Program, info: &HostInfo, store: Box<dyn TableStore>) -> Self {
        let mut inner = StoreHost::new(store);
        let relations = catalogue_relations(program)
            .into_iter()
            .map(|(name, rows)| (inner.manifest.register(name), rows))
            .collect();
        let info_slot = inner.manifest.register("sys.rt.info");
        Self {
            inner,
            relations,
            info_slot,
            info: runtime_info_raw(info),
        }
    }
}

/// The `sys.Table` and `sys.Column` rows of `program`, in declaration order.
fn table_and_column_rows(program: &Program) -> (Vec<Raw>, Vec<Raw>) {
    let mut tables = Vec::new();
    let mut columns = Vec::new();
    for definition in program.defs.iter() {
        let DefKind::Table(table) = &definition.kind else {
            continue;
        };
        if !is_project_module(program, definition.module) {
            continue;
        }
        let module = program.modules[definition.module].path.to_string();
        let table_name = qualified_name(Some(&module), definition.name.as_str());
        tables.push(table_row(&table_name));
        for (position, field_id) in table.fields.iter().enumerate() {
            let field_def = &program.defs[*field_id];
            let DefKind::Field(field) = &field_def.kind else {
                continue;
            };
            let key_position = table
                .key
                .iter()
                .position(|key| key == field_id)
                .map(|index| Raw::Int(BigInt::from(index)));
            columns.push(column_row(
                program,
                &table_name,
                field_def.name.as_str(),
                position,
                field,
                key_position,
                &field_def.docs,
            ));
        }
    }
    (tables, columns)
}

/// The catalogue relations of `program` by their canonical `sys.catalog` member name, rows in declaration order.
fn catalogue_relations(program: &Program) -> Vec<(&'static str, Vec<Raw>)> {
    let (tables, columns) = table_and_column_rows(program);
    vec![
        ("sys.catalog.tables", tables),
        ("sys.catalog.columns", columns),
        ("sys.catalog.functions", function_rows(program)),
        ("sys.catalog.dependencies", dependency_rows(program)),
    ]
}

/// The `sys.Function` rows of `program`: one per function definition of the program's own modules, native or with an Orna body.
fn function_rows(program: &Program) -> Vec<Raw> {
    program
        .defs
        .iter()
        .filter_map(|definition| match &definition.kind {
            DefKind::Fn(function) if is_project_module(program, definition.module) => {
                Some(function_row(program, definition, function))
            }
            _ => None,
        })
        .collect()
}

/// One `sys.Function` row. Its relation handles (`parameters`, `failure_types`, `effects`, `reads`, `writes`, `calls`)
/// are not served by this provider, so they are not part of the row.
fn function_row(program: &Program, definition: &Def, function: &FnDef) -> Raw {
    let name = qualified_definition_name(program, definition);
    // A parameter whose type the author omitted makes the signature inferred (decision D1).
    let inferred = function.params.iter().any(|parameter| !parameter.written);
    let docs = (!definition.docs.is_empty()).then(|| Raw::Text(definition.docs.join("\n")));
    map(vec![
        ("reference", derived_uuid("function", &name)),
        ("object", derived_uuid("function-object", &name)),
        ("definition", derived_uuid("function-definition", &name)),
        (
            "return_type",
            derived_uuid("type", &print_type(program, function.ret)),
        ),
        ("inferred_signature", Raw::Bool(inferred)),
        ("docs", option(docs)),
    ])
}

/// An object's identity before it is derived: the domain its id is derived in, and its name there.
type ObjectKey = (&'static str, String);

/// The object of module `module`.
fn module_object(program: &Program, module: ModuleId) -> ObjectKey {
    ("module-object", program.modules[module].path.to_string())
}

/// The object of a definition, in the domain its kind uses: functions and tables have their own, any other
/// definition shares `definition-object`.
fn definition_object(program: &Program, definition: DefId) -> ObjectKey {
    let definition = &program.defs[definition];
    let domain = match &definition.kind {
        DefKind::Fn(_) => "function-object",
        DefKind::Table(_) => "table-object",
        _ => "definition-object",
    };
    (domain, qualified_definition_name(program, definition))
}

/// The qualified name of `definition`: its module path, then its name.
fn qualified_definition_name(program: &Program, definition: &Def) -> String {
    let module = program.modules[definition.module].path.to_string();
    qualified_name(Some(&module), definition.name.as_str())
}

/// Whether `module` is a module of the program being checked, not bundled `core`, `sys` or `std` code.
fn is_project_module(program: &Program, module: ModuleId) -> bool {
    program.modules[module].origin == ModuleOrigin::Project
}

/// The `sys.Dependency` rows of `program`: one `import` edge per distinct (importing module, imported object) pair.
/// The HIR records `use` declarations without spans, so an import edge has no span and is never conditional.
fn dependency_rows(program: &Program) -> Vec<Raw> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for (module_id, module) in program
        .modules
        .iter_enumerated()
        .filter(|(module_id, _)| is_project_module(program, *module_id))
    {
        let from = module_object(program, module_id);
        for import in &module.imports {
            let to = match import.target {
                ImportTarget::Module(target) => module_object(program, target),
                ImportTarget::Def(target) => definition_object(program, target),
            };
            if seen.insert((from.clone(), to.clone())) {
                rows.push(import_dependency_row(&from, &to));
            }
        }
    }
    rows
}

/// One `sys.Dependency` row for an `import` edge from `from` to `to`. Its `definition` is not served, so it is `null`.
fn import_dependency_row(from: &ObjectKey, to: &ObjectKey) -> Raw {
    let edge = format!("import {}:{} {}:{}", from.0, from.1, to.0, to.1);
    map(vec![
        ("reference", derived_uuid("dependency", &edge)),
        ("from", derived_uuid(from.0, &from.1)),
        ("to", derived_uuid(to.0, &to.1)),
        ("kind", enum_tag("sys.DependencyKind", "import")),
        ("definition", option(None)),
        ("span", option(None)),
        ("confidence", enum_tag("sys.DependencyConfidence", "exact")),
        ("conditional", Raw::Bool(false)),
    ])
}

/// One `sys.Table` row. Its relation handles (`columns`, `assertions`, `references`) and its storage and key rows are
/// not served by this provider, so they are not part of the row.
fn table_row(table_name: &str) -> Raw {
    map(vec![
        ("reference", derived_uuid("table", table_name)),
        ("object", derived_uuid("table-object", table_name)),
        ("definition", derived_uuid("table-definition", table_name)),
        ("key", derived_uuid("table-key", table_name)),
        ("storage", derived_uuid("table-storage", table_name)),
        ("row_count", option(None)),
        ("row_count_exact", Raw::Bool(false)),
    ])
}

/// One `sys.Column` row for `field` of `table_name`.
fn column_row(
    program: &Program,
    table_name: &str,
    field_name: &str,
    position: usize,
    field: &FieldDef,
    key_position: Option<Raw>,
    docs: &[String],
) -> Raw {
    let column_name = format!("{table_name}.{field_name}");
    let optional = matches!(program.tys.kind(field.ty), Some(TyKind::Option(_)));
    let docs = (!docs.is_empty()).then(|| Raw::Text(docs.join("\n")));
    map(vec![
        ("reference", derived_uuid("column", &column_name)),
        ("object", derived_uuid("column-object", &column_name)),
        ("table", derived_uuid("table", table_name)),
        ("name", Raw::Text(field_name.to_owned())),
        ("position", Raw::Int(BigInt::from(position))),
        ("type", derived_uuid("type", &print_type(program, field.ty))),
        ("optional", Raw::Bool(optional)),
        ("key_position", option(key_position)),
        ("default_expression", option(None)),
        ("computed_expression", option(None)),
        ("docs", option(docs)),
    ])
}

/// The `sys.RuntimeInfo` record of `info`.
fn runtime_info_raw(info: &HostInfo) -> Raw {
    let text = |value: &str| Raw::Text(value.to_owned());
    map(vec![
        ("language_version", text(&info.language_version)),
        ("sys_version", text(&info.sys_version)),
        (
            "canonical_orna_codec_version",
            text(&info.canonical_orna_codec_version),
        ),
        (
            "repository_layout_version",
            text(&info.repository_layout_version),
        ),
        (
            "storage_manifest_version",
            text(&info.storage_manifest_version),
        ),
        (
            "presentation_protocol_version",
            text(&info.presentation_protocol_version),
        ),
        ("implementation_name", text(&info.implementation_name)),
        ("implementation_version", text(&info.implementation_version)),
        ("build_id", text(&info.build_id)),
        (
            "supported_profiles",
            Raw::Array(info.supported_profiles.iter().map(|p| text(p)).collect()),
        ),
        // `RuntimeId` is an empty record in the interface, so the provider has no identity to report.
        ("runtime", map(Vec::new())),
        ("mode", runtime_mode(info.mode)),
        ("read_only", Raw::Bool(info.read_only)),
        ("std_snapshot", option(None)),
        ("unicode_version", text(&info.unicode_version)),
        (
            "timezone_database_version",
            option(info.timezone_database_version.as_deref().map(text)),
        ),
    ])
}

/// A payload-free `sys.RuntimeMode` variant in its canonical enum form.
fn runtime_mode(mode: HostRuntimeMode) -> Raw {
    enum_tag("sys.RuntimeMode", mode.variant())
}

/// A payload-free variant of the `sys` enum `type_name`, in its canonical enum form.
fn enum_tag(type_name: &str, variant: &str) -> Raw {
    Raw::Tag(
        ENUM_TAG,
        Box::new(Raw::Array(vec![
            uuid(enum_type_id(type_name)),
            uuid(enum_variant_id(type_name, variant)),
            Raw::Null,
        ])),
    )
}

fn map(fields: Vec<(&str, Raw)>) -> Raw {
    Raw::Map(
        fields
            .into_iter()
            .map(|(name, value)| (Raw::Text(name.to_owned()), value))
            .collect(),
    )
}

/// `None` is the canonical option none, `Some(value)` the option some.
fn option(value: Option<Raw>) -> Raw {
    match value {
        None => Raw::Tag(
            OPTION_TAG,
            Box::new(Raw::Array(vec![Raw::Int(BigInt::from(0))])),
        ),
        Some(value) => Raw::Tag(
            OPTION_TAG,
            Box::new(Raw::Array(vec![Raw::Int(BigInt::from(1)), value])),
        ),
    }
}

fn uuid(bytes: [u8; 16]) -> Raw {
    Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(bytes.to_vec())))
}

/// A stable identifier for `name` in `domain`: the first 16 bytes of a SHA-256 over both, each length-prefixed so that
/// no two (domain, name) pairs share an encoding.
fn derived_uuid(domain: &str, name: &str) -> Raw {
    let mut digest = Sha256::new();
    for part in [domain.as_bytes(), name.as_bytes()] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    let hash = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid(bytes)
}

/// The rows of one catalogue relation, owned, so the cursor borrows nothing from the host.
struct CatalogRows(std::vec::IntoIter<Raw>);

impl RowSource for CatalogRows {
    fn next(&mut self) -> Result<Option<Raw>, HostFailure> {
        Ok(self.0.next())
    }
}

impl Host for ProgramCatalog {
    fn manifest(&self) -> &HostManifest {
        self.inner.manifest()
    }

    fn store(&mut self) -> &mut dyn TableStore {
        self.inner.store()
    }

    fn store_at(&mut self, at: &Snapshot) -> Result<Box<dyn TableStore>, HostFailure> {
        self.inner.store_at(at)
    }

    fn now(&self) -> HostInstant {
        self.inner.now()
    }

    fn entropy(&mut self, out: &mut [u8]) -> Result<(), HostFailure> {
        self.inner.entropy(out)
    }

    fn activation(&self) -> Option<&ActivationContext> {
        self.inner.activation()
    }

    fn sys_call(&mut self, slot: SysSlot, args: Vec<Raw>) -> Result<Raw, HostFailure> {
        if slot == self.info_slot {
            return if args.is_empty() {
                Ok(self.info.clone())
            } else {
                Err(HostFailure { code: UNSUPPORTED })
            };
        }
        self.inner.sys_call(slot, args)
    }

    fn sys_static(&mut self, slot: SysSlot) -> Result<Raw, HostFailure> {
        self.inner.sys_static(slot)
    }

    fn sys_relation(
        &mut self,
        slot: SysSlot,
        at: Option<&Snapshot>,
    ) -> Result<Box<dyn RowSource>, HostFailure> {
        let Some((_, rows)) = self
            .relations
            .iter()
            .find(|(relation, _)| *relation == slot)
        else {
            return self.inner.sys_relation(slot, at);
        };
        if at.is_some() {
            return Err(HostFailure {
                code: SNAPSHOT_NOT_FOUND,
            });
        }
        Ok(Box::new(CatalogRows(rows.clone().into_iter())))
    }
}
