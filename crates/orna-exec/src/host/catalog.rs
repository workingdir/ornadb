//! The `ProgramCatalog` host: the `sys.catalog.*` relations and `sys.rt.info` of one lowered program (hir-host.md A5).
//!
//! The provider needs no publication metadata and no snapshot. Its rows are read from the lowered [`Program`]: one
//! `sys.Table` row per table, one `sys.Column` row per table field, and a `sys.RuntimeInfo` record the embedder
//! supplies as a [`HostInfo`]. Every other `sys` member is answered by the wrapped [`StoreHost`], which serves none, so
//! it stays unlinked.

use num_bigint::BigInt;
use orna_hir::{DefKind, FieldDef, Program, TyKind, print_type};
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
    tables_slot: SysSlot,
    columns_slot: SysSlot,
    info_slot: SysSlot,
    table_rows: Vec<Raw>,
    column_rows: Vec<Raw>,
    info: Raw,
}

impl ProgramCatalog {
    /// A host over `store` that serves the catalogue of `program` and the runtime information `info`.
    #[must_use]
    pub fn new(program: &Program, info: &HostInfo, store: Box<dyn TableStore>) -> Self {
        let mut inner = StoreHost::new(store);
        let tables_slot = inner.manifest.register("sys.catalog.tables");
        let columns_slot = inner.manifest.register("sys.catalog.columns");
        let info_slot = inner.manifest.register("sys.rt.info");
        let (table_rows, column_rows) = catalogue_rows(program);
        Self {
            inner,
            tables_slot,
            columns_slot,
            info_slot,
            table_rows,
            column_rows,
            info: runtime_info_raw(info),
        }
    }
}

/// The rows of the `sys.Table` and `sys.Column` relations of `program`, in declaration order.
fn catalogue_rows(program: &Program) -> (Vec<Raw>, Vec<Raw>) {
    let mut tables = Vec::new();
    let mut columns = Vec::new();
    for definition in program.defs.iter() {
        let DefKind::Table(table) = &definition.kind else {
            continue;
        };
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
    Raw::Tag(
        ENUM_TAG,
        Box::new(Raw::Array(vec![
            uuid(enum_type_id("sys.RuntimeMode")),
            uuid(enum_variant_id("sys.RuntimeMode", mode.variant())),
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

    fn store_at(&mut self, at: &Snapshot) -> Result<Box<dyn TableStore + '_>, HostFailure> {
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
        let rows = if slot == self.tables_slot {
            &self.table_rows
        } else if slot == self.columns_slot {
            &self.column_rows
        } else {
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
