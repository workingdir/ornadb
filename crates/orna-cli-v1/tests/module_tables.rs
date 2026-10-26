//! Multi-module projects through the production CLI (ornadb-5il1.199).
//!
//! Rows of an imported table are typed inside callbacks, two `Item` tables in
//! different modules keep separate rows under their own identities, and module
//! paths resolve through `use` bindings and full namespace paths
//! (ORNA-NS-001, ORNA-NS-002, ORNA-IMPORT-001..004, ORNA-ROW-001; 08-relations:
//! row values compare table ObjectId).

use std::{
    path::Path,
    process::{Command, Output},
};

// Module names avoid the fixture catalogue's namespaces (contacts, mail,
// google, vehicle, ...): production admission inherits those modules today,
// so a user module reusing one collides (ornadb-5il1.285).
const MAIN: &str = r#"use people;
use a;
use b;
use telemetry.rig;
use cloud.drive;

pub fn seed() {
    people.Contact.insert({ id: "ada", name: "Ada" });
    people.Contact.insert({ id: "bob", name: "Bob" });
    a.Item.insert({ id: "x", v: 1 });
    b.Item.insert({ id: "x", w: "wide" });
    rig.Reading.insert({ id: 1, rpm: 900 });
    rig.Reading.insert({ id: 2, rpm: 0 });
}

pub fn verify() {
    assert (people.Contact | filter(contact => contact.name == "Ada") | count()) == 1;
    assert (a.Item | filter(item => item.v == 1) | count()) == 1;
    assert (b.Item | filter(item => item.w == "wide") | count()) == 1;
    assert (rig.Reading | filter(reading => reading.rpm > 0) | count()) == 1;
    assert a.b_items() == 1;
    assert drive.sync() == 3;
}

pub fn verify_rejects() {
    assert (a.Item | count()) == 2;
}
"#;

const CONTACTS: &str = "pub table Contact(id: Str) {\n    name: Str,\n}\n";

// `a` imports `b` itself, and `main` imports `b`, `telemetry.rig` and `cloud.drive`
// in its own right (ORNA-IMPORT-001): a namespace reached only through another
// module's import is not in scope.
const A: &str = "use b;\n\npub table Item(id: Str) {\n    v: Int,\n}\n\npub fn b_items() = b.Item | count();\n";

const B: &str = "pub table Item(id: Str) {\n    w: Str,\n}\n";

const CORSA: &str =
    "pub table Reading(id: Int) {\n    rpm: Int,\n}\n\npub fn trips() = Reading | count();\n";

const GOOGLE: &str = "pub fn sync() = 3;\n";

fn orna(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .output()
        .expect("CLI process")
}

fn project_command(directory: &Path, arguments: &[&str]) -> Output {
    let mut all = vec!["--db", directory.to_str().expect("UTF-8 path")];
    all.extend_from_slice(arguments);
    orna(&all)
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what}: stdout {:?} stderr {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn module_project() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    for (path, source) in [
        ("main.orna", MAIN),
        ("people.orna", CONTACTS),
        ("a.orna", A),
        ("b.orna", B),
        ("telemetry/rig/main.orna", CORSA),
        ("cloud/drive.orna", GOOGLE),
    ] {
        let path = directory.path().join(path);
        std::fs::create_dir_all(path.parent().expect("module directory")).expect("directory");
        std::fs::write(path, source).expect("module source");
    }
    let init = orna(&["init", directory.path().to_str().expect("UTF-8 path")]);
    assert_success(&init, "init");
    directory
}

#[test]
fn module_tables_check_run_and_query_through_qualified_identities() {
    let directory = module_project();

    let check = project_command(directory.path(), &["check"]);
    assert_success(&check, "check");
    assert_eq!(check.stdout, b"project valid\n");

    // Both `Item` rows use key "x": one table under one bare name would
    // reject the second insert as a duplicate key.
    assert_success(&project_command(directory.path(), &["run", "main.seed"]), "seed");
    // Each module table keeps its own stored key: the two `x` rows are one row
    // per qualified table, never one row under a bare `Item` name.
    let counts = stored_row_counts(directory.path());
    for table in ["a.Item", "b.Item"] {
        assert!(
            counts.contains(&(table.to_owned(), 1)),
            "{table} must store exactly one row under its qualified name: {counts:?}"
        );
    }
    assert!(
        !counts.iter().any(|(table, _)| table == "Item"),
        "module rows must not be stored under the bare name: {counts:?}"
    );
    assert_success(&project_command(directory.path(), &["run", "main.verify"]), "verify");
    assert_success(
        &project_command(directory.path(), &["run", "telemetry.rig.trips"]),
        "nested module function by full path",
    );

    let rejected = project_command(directory.path(), &["run", "main.verify_rejects"]);
    assert!(
        !rejected.status.success(),
        "a false query assertion must fail the run: {:?}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

fn run_state_sql(directory: &Path, sql: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let database = libsql::Builder::new_local(directory.join(".git/orna/state.db"))
            .build()
            .await
            .expect("state database");
        let connection = database.connect().expect("state connection");
        connection.execute(sql, ()).await.expect("state update");
    });
}

/// `(table_id, stored row count)` for every table with stored rows, read from
/// the store that each `run` process reopens.
fn stored_row_counts(directory: &Path) -> Vec<(String, i64)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let database = libsql::Builder::new_local(directory.join(".git/orna/state.db"))
            .build()
            .await
            .expect("state database");
        let connection = database.connect().expect("state connection");
        let mut rows = connection
            .query(
                "SELECT table_id, COUNT(*) FROM table_row GROUP BY table_id ORDER BY table_id",
                (),
            )
            .await
            .expect("row counts");
        let mut counts = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            counts.push((
                row.get::<String>(0).expect("table id"),
                row.get::<i64>(1).expect("count"),
            ));
        }
        counts
    })
}

#[test]
fn legacy_bare_module_rows_are_refused_not_migrated() {
    let directory = module_project();
    assert_success(&project_command(directory.path(), &["run", "main.seed"]), "seed");
    // Rows an earlier build stored under the bare `Item` name.
    run_state_sql(
        directory.path(),
        "UPDATE table_row SET table_id = 'Item' WHERE table_id = 'a.Item'",
    );
    let rejected = project_command(directory.path(), &["run", "main.verify"]);
    assert!(
        !rejected.status.success(),
        "a store holding bare-name module rows must be refused"
    );
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("E2200"), "refusal must carry E2200: {stderr}");
}
