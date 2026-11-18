//! The `sys.catalog.tables` and `sys.catalog.columns` rows carry typed row
//! references over the committed identities of the tables and columns, never
//! null and never text (ORNA-SYS-111, ORNA-SYS-112).

use std::collections::{BTreeMap, BTreeSet};

use orna_check::catalogue_identities::{
    CatalogueIdentities, CatalogueIdentityError, TableIdentity,
};
use orna_check::catalogue_rows::CatalogueRows;
use orna_check::{TableAdmission, TableSchema, Type};
use orna_value::{
    CanonicalSnapshot, CanonicalValue, GitHash, KeyPart, OvbRaw, RowRef, SysRef,
    object_keyed_reference, object_reference, relations,
};

const DATABASE: [u8; 16] = [7; 16];
const TABLE: [u8; 16] = [1; 16];
const ID_COLUMN: [u8; 16] = [2; 16];
const NAME_COLUMN: [u8; 16] = [3; 16];

/// The commit the committed identities were read at.
fn pin() -> CanonicalSnapshot {
    CanonicalSnapshot::Commit {
        database: DATABASE,
        algorithm: GitHash::Sha1,
        oid: vec![9; 20],
    }
}

fn item_schema() -> TableSchema {
    TableSchema {
        fields: BTreeMap::from([
            ("id".to_owned(), Type::Text),
            ("name".to_owned(), Type::Text),
        ]),
        references: Vec::new(),
        declaration_order: vec!["id".to_owned(), "name".to_owned()],
        admission: Some(TableAdmission {
            required: BTreeSet::from(["name".to_owned()]),
            private_required: false,
            computed: BTreeSet::new(),
            keys: vec![("id".to_owned(), Type::Text)],
            automatic_key: false,
        }),
        conversions: Vec::new(),
        currency: false,
    }
}

fn schemas() -> BTreeMap<String, TableSchema> {
    BTreeMap::from([("Item".to_owned(), item_schema())])
}

fn identities() -> CatalogueIdentities {
    CatalogueIdentities::new(
        DATABASE,
        pin(),
        BTreeMap::from([(
            "Item".to_owned(),
            TableIdentity::new(
                TABLE,
                BTreeMap::from([
                    ("id".to_owned(), ID_COLUMN),
                    ("name".to_owned(), NAME_COLUMN),
                ]),
            ),
        )]),
    )
    .expect("identities of the pinned database")
}

fn rows() -> CatalogueRows {
    CatalogueRows::build(&schemas(), Some(&identities())).expect("a catalogue over its identities")
}

/// The raw field `name` of a record row.
fn field<'a>(record: &'a OvbRaw, name: &str) -> &'a OvbRaw {
    let OvbRaw::Map(entries) = record else {
        panic!("a catalogue row is a record: {record:?}");
    };
    entries
        .iter()
        .find(|(key, _)| matches!(key, OvbRaw::Text(text) if text == name))
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("the row has no `{name}` field: {record:?}"))
}

/// The checked reference a row field holds, decoded from its canonical form.
fn reference(raw: &OvbRaw) -> SysRef {
    let encoded = CanonicalValue::new(raw.clone())
        .expect("a canonical reference")
        .encode()
        .expect("a reference encodes");
    let row = RowRef::decode(&encoded).expect("a tag-60010 reference decodes");
    SysRef::from_row_ref(row).expect("a reference over a canonical relation")
}

fn table_reference(id: [u8; 16]) -> SysRef {
    let object = object_reference(DATABASE, id, pin()).expect("object reference");
    object_keyed_reference(&relations::TABLE, object).expect("table reference")
}

fn column_reference(id: [u8; 16]) -> SysRef {
    let object = object_reference(DATABASE, id, pin()).expect("object reference");
    object_keyed_reference(&relations::COLUMN, object).expect("column reference")
}

// Specified: ORNA-SYS-111, ORNA-SYS-112, Orna-1.1.0 sys.Table.
#[test]
fn a_table_row_exposes_typed_references_over_the_committed_identity() {
    let rows = rows();
    let [table] = rows.tables() else {
        panic!("one row per admitted table");
    };
    let table = table.raw();

    let own = reference(field(table, "reference"));
    assert!(
        own.is_of(&relations::TABLE),
        "`reference` is a sys.TableRef"
    );
    assert_eq!(own, table_reference(TABLE));

    // `object` is the sys.ObjectRef of the committed identity, pinned to the
    // commit the identities were read at.
    let object = object_reference(DATABASE, TABLE, pin()).expect("object reference");
    let expected = object.as_row_ref().to_canonical().expect("encodes");
    assert_eq!(field(table, "object"), expected.raw());
    assert_eq!(own.as_row_ref().database_id, DATABASE);
    assert_eq!(own.as_row_ref().snapshot, pin());

    // `definition` is the sys.DefinitionRef of the same identity: the committed
    // table identity is the only rename-stable per-table UUID (ORNA-OBJECT-002).
    let definition = reference(field(table, "definition"));
    assert_eq!(
        definition,
        SysRef::new(
            &relations::DEFINITION,
            DATABASE,
            pin(),
            vec![KeyPart::Uuid(TABLE)]
        )
        .expect("definition reference")
    );
}

// Specified: ORNA-SYS-111, ORNA-SYS-112, Orna-1.1.0 sys.Column.
#[test]
fn a_column_row_exposes_typed_references_and_names_its_table_by_reference() {
    let rows = rows();
    let [id, name] = rows.columns() else {
        panic!("one row per declared column");
    };
    for (row, column, expected) in [(id, "id", ID_COLUMN), (name, "name", NAME_COLUMN)] {
        let row = row.raw();
        assert_eq!(
            field(row, "name"),
            &OvbRaw::Text(column.to_owned()),
            "columns keep declaration order"
        );
        let own = reference(field(row, "reference"));
        assert!(
            own.is_of(&relations::COLUMN),
            "`reference` is a sys.ColumnRef"
        );
        assert_eq!(own, column_reference(expected));
        // `table` is the owning table's `reference`, not its name.
        let table = reference(field(row, "table"));
        assert!(table.is_of(&relations::TABLE), "`table` is a sys.TableRef");
        assert_eq!(table, table_reference(TABLE));
        // `object` names the column's own committed identity.
        let object = reference(field(row, "object"));
        assert_eq!(
            object.as_row_ref(),
            object_reference(DATABASE, expected, pin())
                .expect("object reference")
                .as_row_ref()
        );
    }
}

// Specified: ORNA-SYS-111; the nested `sys.Table.columns` rows are `sys.Column` rows.
#[test]
fn nested_table_columns_are_the_column_rows() {
    let rows = rows();
    let OvbRaw::Array(nested) = field(rows.tables()[0].raw(), "columns") else {
        panic!("`columns` is a relation of sys.Column rows");
    };
    let flat = rows
        .columns()
        .iter()
        .map(|row| row.raw().clone())
        .collect::<Vec<_>>();
    assert_eq!(nested, &flat);
}

// Specified: ORNA-SYS-111, ORNA-SYS-112: no reference field of any row is null, text or bytes.
#[test]
fn no_reference_field_is_null_or_a_bare_value() {
    let rows = rows();
    let table = rows.tables()[0].raw();
    for name in ["reference", "object", "definition"] {
        assert!(
            matches!(field(table, name), OvbRaw::Tag(60010, _)),
            "sys.Table.{name} is a tag-60010 reference"
        );
    }
    for row in rows.columns() {
        for name in ["reference", "object", "table"] {
            assert!(
                matches!(field(row.raw(), name), OvbRaw::Tag(60010, _)),
                "sys.Column.{name} is a tag-60010 reference"
            );
        }
    }
}

// Specified: ORNA-SYS-111; a null reference breaches the declared type, so a
// table with no committed identity has no row to serve.
#[test]
fn a_table_or_column_without_a_committed_identity_has_no_rows() {
    let unknown_table = CatalogueIdentities::new(DATABASE, pin(), BTreeMap::new())
        .expect("identities of the pinned database");
    assert_eq!(
        CatalogueRows::build(&schemas(), Some(&unknown_table)),
        Err(CatalogueIdentityError::MissingTable("Item".to_owned()))
    );
    let unknown_column = CatalogueIdentities::new(
        DATABASE,
        pin(),
        BTreeMap::from([(
            "Item".to_owned(),
            TableIdentity::new(TABLE, BTreeMap::from([("id".to_owned(), ID_COLUMN)])),
        )]),
    )
    .expect("identities of the pinned database");
    assert_eq!(
        CatalogueRows::build(&schemas(), Some(&unknown_column)),
        Err(CatalogueIdentityError::MissingColumn {
            table: "Item".to_owned(),
            column: "name".to_owned()
        })
    );
}

// Specified: ORNA-SYS-111; with no identities only a catalogue of no tables builds.
#[test]
fn without_identities_only_an_empty_catalogue_builds() {
    assert_eq!(
        CatalogueRows::build(&schemas(), None),
        Err(CatalogueIdentityError::Unavailable)
    );
    let empty = CatalogueRows::build(&BTreeMap::new(), None).expect("no table needs an identity");
    assert!(empty.tables().is_empty() && empty.columns().is_empty());
}

// Specified: ORNA-SYS-019; every reference names the database its pin belongs to.
#[test]
fn identities_pinned_to_another_database_are_refused() {
    let foreign = CanonicalSnapshot::Commit {
        database: [8; 16],
        algorithm: GitHash::Sha1,
        oid: vec![9; 20],
    };
    assert_eq!(
        CatalogueIdentities::new(DATABASE, foreign, BTreeMap::new()),
        Err(CatalogueIdentityError::DatabaseMismatch)
    );
}
