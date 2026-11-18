//! Canonical `sys.Table` and `sys.Column` rows built from admitted table
//! schemas. The rows are served through the `sys.catalog.*` grouped handles by
//! every runtime that holds the schemas: the REPL application and the durable
//! `orna run` table effect handler both read these builders.

use std::collections::BTreeMap;

use orna_value::{CanonicalValue, OvbRaw, relations};

use crate::catalogue_identities::{CatalogueIdentities, CatalogueIdentityError};
use crate::types::{TableSchema, Type};

/// The `sys.Table` and `sys.Column` rows one activation serves.
///
/// The rows carry typed references over the committed identities of the tables
/// and columns, so they are built only from those identities
/// ([`CatalogueIdentities`]). A table with no identity has no spec-correct
/// reference, and a null reference would breach ORNA-SYS-111, so such a
/// catalogue is not built at all.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueRows {
    tables: Vec<CanonicalValue>,
    columns: Vec<CanonicalValue>,
}

impl CatalogueRows {
    /// The rows of every admitted table in `schemas`, over `identities`.
    ///
    /// A catalogue of no tables has no row that needs an identity, so it builds
    /// without any.
    ///
    /// # Errors
    ///
    /// [`CatalogueIdentityError::Unavailable`] when `schemas` names a table and
    /// there are no identities, and the missing identity or the invalid
    /// reference otherwise.
    pub fn build(
        schemas: &BTreeMap<String, TableSchema>,
        identities: Option<&CatalogueIdentities>,
    ) -> Result<Self, CatalogueIdentityError> {
        let Some(identities) = identities else {
            return if schemas.is_empty() {
                Ok(Self {
                    tables: Vec::new(),
                    columns: Vec::new(),
                })
            } else {
                Err(CatalogueIdentityError::Unavailable)
            };
        };
        let mut anchors = BTreeMap::new();
        for (name, schema) in schemas {
            anchors.insert(name.as_str(), table_anchor(identities, name, schema)?);
        }
        Ok(Self {
            tables: table_rows(schemas, &anchors),
            columns: column_rows(schemas, &anchors),
        })
    }

    /// The `sys.Table` rows, in table-name order.
    #[must_use]
    pub fn tables(&self) -> &[CanonicalValue] {
        &self.tables
    }

    /// The `sys.Column` rows of every table, in table-name then declaration order.
    #[must_use]
    pub fn columns(&self) -> &[CanonicalValue] {
        &self.columns
    }
}

/// The references one table's rows carry, resolved over its committed identities.
struct TableAnchor {
    reference: OvbRaw,
    object: OvbRaw,
    definition: OvbRaw,
    /// One entry per column, in declaration order.
    columns: Vec<ColumnAnchor>,
}

/// The references one column's row carries.
struct ColumnAnchor {
    reference: OvbRaw,
    object: OvbRaw,
}

/// Resolves the references of `table` and of every declared column of `schema`.
///
/// The committed table identity is both the catalogue object and the
/// definition: it is the only rename-stable per-table UUID in committed
/// metadata (ORNA-OBJECT-002), and ORNA-OBJECT-003 forbids a rename changing
/// it. Deriving the definition from the qualified name would break that.
fn table_anchor(
    identities: &CatalogueIdentities,
    table: &str,
    schema: &TableSchema,
) -> Result<TableAnchor, CatalogueIdentityError> {
    let committed = identities
        .table(table)
        .ok_or_else(|| CatalogueIdentityError::MissingTable(table.to_owned()))?;
    let columns = schema
        .declaration_order
        .iter()
        .map(|column| {
            let id =
                committed
                    .column(column)
                    .ok_or_else(|| CatalogueIdentityError::MissingColumn {
                        table: table.to_owned(),
                        column: column.clone(),
                    })?;
            Ok(ColumnAnchor {
                reference: identities.keyed(&relations::COLUMN, id)?,
                object: identities.object(id)?,
            })
        })
        .collect::<Result<Vec<_>, CatalogueIdentityError>>()?;
    Ok(TableAnchor {
        reference: identities.keyed(&relations::TABLE, committed.object())?,
        object: identities.object(committed.object())?,
        definition: identities.definition(committed.object())?,
        columns,
    })
}

/// Builds a canonical record from named fields.
///
/// OVB orders map keys by their complete encoded form, and
/// `CanonicalValue::new` validates that order instead of sorting, so the keys
/// are sorted by encoded bytes here (see `Value::Record`'s raw conversion).
fn catalogue_map(fields: Vec<(&str, OvbRaw)>) -> OvbRaw {
    let mut encoded = fields
        .into_iter()
        .map(|(key, value)| {
            let bytes = CanonicalValue::new(OvbRaw::Text(key.to_owned()))
                .expect("text key is canonical")
                .encode()
                .expect("text key encodes");
            (bytes, key, value)
        })
        .collect::<Vec<_>>();
    encoded.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
    OvbRaw::Map(
        encoded
            .into_iter()
            .map(|(_, key, value)| (OvbRaw::Text(key.to_owned()), value))
            .collect(),
    )
}

/// `sys.Column` rows for every column of every admitted table.
///
/// A column carries its table, its declaration position, its type, whether it
/// is optional and its key position when it is a key column. Its `reference`,
/// `object` and `table` are typed references over the committed identities.
/// Expression references stay null: the engine holds no expression identities.
fn column_rows(
    tables: &BTreeMap<String, TableSchema>,
    anchors: &BTreeMap<&str, TableAnchor>,
) -> Vec<CanonicalValue> {
    let mut rows = Vec::new();
    for (table, schema) in tables {
        let anchor = &anchors[table.as_str()];
        for (position, column) in schema.declaration_order.iter().enumerate() {
            let record = catalogue_column_record(anchor, schema, position, column);
            rows.push(CanonicalValue::new(record).expect("catalogue column is a canonical record"));
        }
    }
    rows
}

/// One `sys.Column` record. Nested `sys.Table.columns` rows use the same
/// builder, so their fields are the declared `sys.Column` fields.
fn catalogue_column_record(
    anchor: &TableAnchor,
    schema: &TableSchema,
    position: usize,
    column: &str,
) -> OvbRaw {
    let ty = schema.fields.get(column);
    let optional = matches!(ty, Some(Type::Optional(_)));
    let key_position = schema
        .admission
        .as_ref()
        .and_then(|admission| admission.keys.iter().position(|(key, _)| key == column))
        .map(|index| OvbRaw::Int(index.into()))
        .unwrap_or(OvbRaw::Null);
    let identity = &anchor.columns[position];
    catalogue_map(vec![
        ("default_expression", OvbRaw::Null),
        ("key_position", key_position),
        ("name", OvbRaw::Text(column.to_owned())),
        ("object", identity.object.clone()),
        ("optional", OvbRaw::Bool(optional)),
        ("position", OvbRaw::Int(position.into())),
        ("reference", identity.reference.clone()),
        ("table", anchor.reference.clone()),
        (
            "type",
            ty.map(|ty| OvbRaw::Text(format!("{ty:?}")))
                .unwrap_or(OvbRaw::Null),
        ),
    ])
}

/// `sys.Table` rows for every admitted table, in name order.
///
/// `columns` follow the declaration order, `key` comes from the admission
/// metadata and `references` from the resolved stored references. `row_count`
/// stays null and `row_count_exact` false until an exact count is held, which
/// the spec's nullable `Int` and `Bool` allow.
fn table_rows(
    tables: &BTreeMap<String, TableSchema>,
    anchors: &BTreeMap<&str, TableAnchor>,
) -> Vec<CanonicalValue> {
    let mut rows = Vec::new();
    for (name, schema) in tables {
        let anchor = &anchors[name.as_str()];
        let columns = OvbRaw::Array(
            schema
                .declaration_order
                .iter()
                .enumerate()
                .map(|(position, column)| catalogue_column_record(anchor, schema, position, column))
                .collect(),
        );
        let keys = OvbRaw::Array(
            schema
                .admission
                .as_ref()
                .map(|admission| {
                    admission
                        .keys
                        .iter()
                        .map(|(key, ty)| {
                            catalogue_map(vec![
                                ("name", OvbRaw::Text(key.clone())),
                                ("ty", OvbRaw::Text(format!("{ty:?}"))),
                            ])
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        let references = OvbRaw::Array(
            schema
                .references
                .iter()
                .map(|reference| {
                    catalogue_map(vec![
                        ("from_column", OvbRaw::Text(reference.from_column.clone())),
                        (
                            "on_delete",
                            OvbRaw::Text(format!("{:?}", reference.on_delete)),
                        ),
                        (
                            "on_rekey",
                            OvbRaw::Text(format!("{:?}", reference.on_rekey)),
                        ),
                        ("optional", OvbRaw::Bool(reference.optional)),
                        ("to_table", OvbRaw::Text(reference.to_table.clone())),
                    ])
                })
                .collect(),
        );
        let record = catalogue_map(vec![
            ("assertions", OvbRaw::Array(Vec::new())),
            ("columns", columns),
            ("definition", anchor.definition.clone()),
            ("key", keys),
            ("name", OvbRaw::Text(name.clone())),
            ("object", anchor.object.clone()),
            ("reference", anchor.reference.clone()),
            ("references", references),
            ("row_count", OvbRaw::Null),
            ("row_count_exact", OvbRaw::Bool(false)),
            ("storage", OvbRaw::Null),
        ]);
        rows.push(CanonicalValue::new(record).expect("catalogue row is a canonical record"));
    }
    rows
}

/// Admitted table schemas of an analysed project, keyed by the name the table
/// is addressed by: `name` in the root namespace and `namespace.name` below
/// it. Committed table identities use the same names, so the catalogue rows
/// and the identities line up. Tables without admission metadata are omitted.
#[must_use]
pub fn admitted_table_schemas_by_name(analysis: &crate::Analysis) -> BTreeMap<String, TableSchema> {
    let mut tables = BTreeMap::new();
    for (namespace, module) in &analysis.modules {
        for (name, symbol) in &module.symbols {
            let Some(schema) = symbol.table_schema.as_ref() else {
                continue;
            };
            if symbol.kind == crate::SymbolKind::Table && schema.admission.is_some() {
                let table = match namespace.display().as_str() {
                    "" => name.clone(),
                    prefix => format!("{prefix}.{name}"),
                };
                tables.insert(table, schema.clone());
            }
        }
    }
    tables
}
