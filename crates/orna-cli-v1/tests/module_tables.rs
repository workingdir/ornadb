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
use telemetry.rig;
use cloud.drive;

pub fn seed() {
    people.Contact.insert({ id: "ada", name: "Ada" });
    people.Contact.insert({ id: "bob", name: "Bob" });
    a.Item.insert({ id: "x", v: 1 });
    b.Item.insert({ id: "x", w: "wide" });
    rig.Reading.insert({ id: 1, rpm: 900 });
    telemetry.rig.Reading.insert({ id: 2, rpm: 0 });
}

pub fn verify() {
    assert (people.Contact | filter(contact => contact.name == "Ada") | count()) == 1;
    assert (a.Item | filter(item => item.v == 1) | count()) == 1;
    assert (b.Item | filter(item => item.w == "wide") | count()) == 1;
    assert (telemetry.rig.Reading | filter(reading => reading.rpm > 0) | count()) == 1;
    assert a.b_items() == 1;
    assert drive.sync() == 3;
    assert cloud.drive.sync() == 3;
}

pub fn verify_rejects() {
    assert (a.Item | count()) == 2;
}
"#;

const CONTACTS: &str = "pub table Contact(id: Str) {\n    name: Str,\n}\n";

// `b` is reachable through `a`'s import only, so `main` names it by its full
// namespace path without a `use` of its own.
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
