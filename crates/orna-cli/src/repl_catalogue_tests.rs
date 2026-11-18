//! The REPL serves `sys.catalog.tables` and `sys.catalog.columns` rows whose
//! references are the committed identities of the commit the session is bound
//! to, pinned to that commit, on both of its paths: a table-read-only input
//! over the committed read scope, and an input that also writes through the
//! runtime writer (ORNA-SYS-111, ORNA-SYS-112).

use super::*;
use crate::format3::format3_repository_for_table;
use orna_evaluator::ReplValue;
use orna_project::{ObjectIdentityManifest, ObjectKind};
use orna_repository::Repository;
use orna_value::{
    CanonicalSnapshot, GitHash, KeyPart, OvbRaw, RelationSpec, RowRef, SysRef,
    object_keyed_reference, object_reference, relations,
};
use tempfile::TempDir;

const SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n\n\
pub fn record_and_reference(): sys.TableRef? {\n    \
Music.insert({ id: \"a\", title: \"A\" });\n    \
sys.catalog.tables | map(t => t.reference) | first()\n}\n";

/// What the committed project records: the database, the commit every
/// reference is pinned to, and the stable identity of the table and columns.
struct Committed {
    database: [u8; 16],
    pin: CanonicalSnapshot,
    table: [u8; 16],
    id: [u8; 16],
    title: [u8; 16],
}

fn raw(reference: &RowRef) -> OvbRaw {
    reference
        .to_canonical()
        .expect("a reference encodes")
        .raw()
        .clone()
}

impl Committed {
    /// Reads the expectations from the repository, never from the REPL.
    fn read(repository: &Repository) -> Self {
        let head = repository
            .head()
            .expect("HEAD is readable")
            .expect("a committed HEAD");
        let database = *repository
            .open_pinned_format_context(head.as_str())
            .expect("a format context at HEAD")
            .require_database_id()
            .expect("the store has a database identity")
            .as_bytes();
        let hex = head.as_str();
        let oid = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("a hexadecimal commit id"))
            .collect::<Vec<_>>();
        let algorithm = if oid.len() == 20 {
            GitHash::Sha1
        } else {
            GitHash::Sha256
        };
        let manifest = ObjectIdentityManifest::read_committed(repository, &head)
            .expect("the commit records its identities");
        let identity = |kind, name: &str| {
            *manifest
                .id(kind, name)
                .unwrap_or_else(|| panic!("{name} has a committed identity"))
        };
        Self {
            database,
            pin: CanonicalSnapshot::Commit {
                database,
                algorithm,
                oid,
            },
            table: identity(ObjectKind::Table, "Music"),
            id: identity(ObjectKind::Field, "Music.id"),
            title: identity(ObjectKind::Field, "Music.title"),
        }
    }

    /// The `sys.ObjectRef` of the object `id`.
    fn object(&self, id: [u8; 16]) -> OvbRaw {
        let object = object_reference(self.database, id, self.pin.clone()).expect("object ref");
        raw(object.as_row_ref())
    }

    /// The reference to the object-keyed row of `relation` for the object `id`.
    fn keyed(&self, relation: &'static RelationSpec, id: [u8; 16]) -> OvbRaw {
        let object = object_reference(self.database, id, self.pin.clone()).expect("object ref");
        let reference = object_keyed_reference(relation, object).expect("keyed reference");
        raw(reference.as_row_ref())
    }

    /// The `sys.DefinitionRef` of the table: its committed identity is the definition id.
    fn definition(&self) -> OvbRaw {
        let reference = SysRef::new(
            &relations::DEFINITION,
            self.database,
            self.pin.clone(),
            vec![KeyPart::Uuid(self.table)],
        )
        .expect("definition reference");
        raw(reference.as_row_ref())
    }
}

struct Repl {
    _directory: TempDir,
    repository: Option<Repository>,
    session: AdmittedReplSession,
    transactions: ReplTransactions,
}

impl Repl {
    fn open() -> Self {
        let (directory, repository, _relation) = format3_repository_for_table(SOURCE, "Music");
        let ReplRuntime {
            repository,
            session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        Self {
            _directory: directory,
            repository,
            session,
            transactions: transactions.expect("repl transactions"),
        }
    }

    fn committed(&self) -> Committed {
        Committed::read(self.repository.as_ref().expect("a bound repository"))
    }

    fn submit(&mut self, source: &str) -> ReplValue {
        run_repl_input(
            self.repository.as_ref(),
            Some(&mut self.transactions),
            &mut self.session,
            source,
        )
        .unwrap_or_else(|failure| panic!("{source}: {}", failure.code))
        .value
        .unwrap_or_else(|| panic!("{source} has a value"))
    }

    fn rows(&mut self, source: &str) -> Vec<CanonicalValue> {
        let ReplValue::Relation(window) = self.submit(source) else {
            panic!("{source} is a relation");
        };
        window.rows
    }
}

/// The raw field `name` of a catalogue row.
fn field<'a>(row: &'a CanonicalValue, name: &str) -> &'a OvbRaw {
    let OvbRaw::Map(entries) = row.raw() else {
        panic!("a catalogue row is a record");
    };
    entries
        .iter()
        .find_map(|(key, value)| matches!(key, OvbRaw::Text(text) if text == name).then_some(value))
        .unwrap_or_else(|| panic!("the row has no `{name}` field"))
}

// Specified: ORNA-SYS-111, ORNA-SYS-112; the committed read path.
#[test]
fn a_committed_read_serves_references_over_the_committed_identities() {
    let mut repl = Repl::open();
    let committed = repl.committed();

    let tables = repl.rows("sys.catalog.tables");
    let [table] = tables.as_slice() else {
        panic!("one row per admitted table");
    };
    assert_eq!(
        field(table, "reference"),
        &committed.keyed(&relations::TABLE, committed.table)
    );
    assert_eq!(field(table, "object"), &committed.object(committed.table));
    assert_eq!(field(table, "definition"), &committed.definition());

    let columns = repl.rows("sys.catalog.columns");
    let [id, title] = columns.as_slice() else {
        panic!("one row per declared column");
    };
    for (row, name, identity) in [(id, "id", committed.id), (title, "title", committed.title)] {
        assert_eq!(field(row, "name"), &OvbRaw::Text(name.to_owned()));
        assert_eq!(
            field(row, "reference"),
            &committed.keyed(&relations::COLUMN, identity)
        );
        assert_eq!(field(row, "object"), &committed.object(identity));
        assert_eq!(
            field(row, "table"),
            &committed.keyed(&relations::TABLE, committed.table),
            "a column names its table by reference, not by name"
        );
    }
    assert!(
        repl.transactions.writer.is_none(),
        "a table-read-only input takes no writer"
    );
}

// Specified: ORNA-SYS-111, ORNA-SYS-112; an input that also writes reads the
// catalogue through the writer, over the same identities.
#[test]
fn an_input_that_writes_reads_the_same_references_through_the_writer() {
    let mut repl = Repl::open();
    let committed = repl.committed();

    let ReplValue::Value(value) = repl.submit("main.record_and_reference()") else {
        panic!("the function returns one value");
    };
    // `sys.TableRef?`: the option holds the table's reference.
    let OvbRaw::Tag(_, option) = value.raw() else {
        panic!("an optional reference is a tagged option");
    };
    let OvbRaw::Array(parts) = option.as_ref() else {
        panic!("an option holds its presence and payload");
    };
    assert_eq!(
        parts.last(),
        Some(&committed.keyed(&relations::TABLE, committed.table))
    );
    assert!(
        repl.transactions.writer.is_some(),
        "the input ran through the runtime writer"
    );
}
