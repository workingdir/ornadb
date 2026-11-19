//! Stored table references, end to end through the product commands
//! (ORNA-REF-001, ORNA-REF-002, ORNA-REF-003, ORNA-MUT-007, ORNA-MUT-008).
//!
//! A table type used as a field denotes a stored reference to a row of that
//! table, never an embedded copy of it. `orna run` and the REPL write the
//! identity of the row a table-typed field is bound to, `orna publish` accepts
//! that stored form, the reference reads back through the same snapshot, and an
//! activation that would leave a reference without its row is refused at
//! commit, with none of its writes committed.
//! A row saved through the mounted filesystem is refused at sync the same way.
//!
//! No fixture builds the store or the identity inventory: each test creates a
//! database with `orna init`, declares the tables, and commits them with
//! `orna commit`.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

use orna_cli_testkit::format3;
use orna_cli_testkit::media_commit;
use orna_repository::{Repository, TypedKey};
use orna_value::{Raw, decode_canonical_storage_value};
use tempfile::TempDir;

#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{
    assert_errno, require_fuse_or_skip, run_orna_unmount, spawn_mount_process, write_and_sync,
};

const SOURCE: &str = r#"pub table Owner(id: Str) { name: Str, }
pub table Vehicle(id: Str) { owner: Owner, }
pub fn seed() { Vehicle.insert({ id: "v1", owner: Owner.insert({ id: "ada", name: "Ada" }) }); }
pub fn owner_name(): Str = Vehicle | map(v => v.owner.name) | first() ?? "none";
pub fn owner_key(): Str = Vehicle | map(v => v.owner.key) | first() ?? "none";
pub fn dangle() { let ghost = Owner.insert({ id: "ghost", name: "Ghost" }); Owner.delete("ghost"); Vehicle.insert({ id: "v2", owner: ghost }); }
pub fn drop_owner() { Owner.delete("ada"); }
pub fn rekey_owner() { Owner.rekey("ada", "ada2"); }
pub fn rekey_with_dependents() { let moved = Owner.rekey("ada", "ada2"); Vehicle.update("v1", { owner: moved }); }
pub fn move_then_delete() { let bob = Owner.insert({ id: "bob", name: "Bob" }); Vehicle.update("v1", { owner: bob }); Owner.delete("ada"); }
pub fn main() {}
"#;

/// A table whose second reference is optional, so a present and an absent
/// optional reference are both stored.
const OPTIONAL_SOURCE: &str = r#"pub table Owner(id: Str) { name: Str, }
pub table Vehicle(id: Str) { owner: Owner, spare: Owner?, }
pub fn seed() { let ada = Owner.insert({ id: "ada", name: "Ada" }); Vehicle.insert({ id: "v1", owner: ada, spare: ada }); Vehicle.insert({ id: "v2", owner: ada, spare: null }); }
pub fn dangling_spare() { let ghost = Owner.insert({ id: "ghost", name: "Ghost" }); Owner.delete("ghost"); Vehicle.update("v1", { spare: ghost }); }
pub fn main() {}
"#;

/// Keeps the host's Git configuration and repository routing out of every
/// process this test starts.
fn isolate_git(command: &mut Command) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(name);
    }
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    command
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the orna process starts")
}

/// Submits `input` as one REPL input, the way a trusted operator would.
fn orna_repl(root: &Path, input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let mut child = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the orna REPL starts");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the orna REPL exits")
}

fn git(root: &Path, arguments: &[&str]) {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn transcript(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output, step: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{step} failed\n{}",
        transcript(output)
    );
}

/// Requires the invocation to be refused under `code`.
fn assert_refused(output: &Output, code: &str, step: &str) {
    let text = transcript(output);
    assert_ne!(
        output.status.code(),
        Some(0),
        "{step} must be refused\n{text}"
    );
    assert!(
        text.contains(&format!("error[{code}]")),
        "{step} must be refused with {code}\n{text}"
    );
}

/// Requires a REPL input to be refused under `code`. The REPL reports the
/// failure on its transcript and stays open, so the process exits cleanly.
fn assert_repl_refused(output: &Output, code: &str, step: &str) {
    let text = transcript(output);
    assert!(
        text.contains(&format!("error[{code}]")),
        "{step} must be refused with {code}\n{text}"
    );
}

/// A committed database: its directory and the relation id `orna commit`
/// reports for each table.
struct Database {
    directory: TempDir,
    owner_relation: [u8; 16],
    vehicle_relation: [u8; 16],
}

impl Database {
    fn root(&self) -> PathBuf {
        self.directory.path().join("garage")
    }
}

fn hex_id(hex: &str) -> [u8; 16] {
    let mut id = [0_u8; 16];
    assert_eq!(hex.len(), 32, "a relation id is 32 hex digits");
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("hex digits");
    }
    id
}

/// `orna init` into a fresh directory, `main.orna` replaced by `source`, and
/// both tables committed with `orna commit`.
fn committed_database(source: &str) -> Database {
    let directory = TempDir::new().expect("create the test directory");
    let root = directory.path().join("garage");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Declare the garage"]);
    assert_success(&committed, "orna commit");
    let relation = |table: &str| {
        let prefix = format!("admitted table {table} as relation ");
        String::from_utf8_lossy(&committed.stdout)
            .lines()
            .find_map(|line| line.strip_prefix(prefix.as_str()).map(hex_id))
            .unwrap_or_else(|| panic!("orna commit admits the {table} table"))
    };
    Database {
        directory,
        owner_relation: relation("Owner"),
        vehicle_relation: relation("Vehicle"),
    }
}

/// The committed database of [`SOURCE`] with `seed` run and published, so the
/// Owner and Vehicle rows are committed at `HEAD`.
fn published_garage() -> Database {
    let database = committed_database(SOURCE);
    let root = database.root();
    assert_success(&orna(&root, &["run", "seed"]), "orna run seed");
    let published = orna(&root, &["publish", "--message", "Seed the garage"]);
    assert_success(&published, "orna publish of a row that holds a reference");
    database
}

fn uuid_raw(bytes: [u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(bytes.to_vec())))
}

/// The stored reference a committed row holds: database identity, table
/// `ObjectId` and primary key (ORNA-REF-001).
fn stored_reference(database: [u8; 16], table: [u8; 16], key: &str) -> Raw {
    Raw::Tag(
        60021,
        Box::new(Raw::Array(vec![
            uuid_raw(database),
            uuid_raw(table),
            Raw::Text(key.to_owned()),
        ])),
    )
}

/// The committed stored tuple of the Vehicle row at `key`, as stored bytes.
fn committed_vehicle_tuple(root: &Path, vehicle_relation: [u8; 16], key: &str) -> Vec<u8> {
    let repository = Repository::discover(root).expect("repository");
    let row = format3::committed_store_rows(&repository, vehicle_relation)
        .into_iter()
        .find(|row| row.key() == &TypedKey::Text(key.to_owned()))
        .unwrap_or_else(|| panic!("the published Vehicle row `{key}` exists"));
    row.value()
        .encoded_fields()
        .expect("an inline committed row")
        .to_vec()
}

fn database_id(root: &Path) -> [u8; 16] {
    let repository = Repository::discover(root).expect("repository");
    *orna_repository::inspect_metadata(&repository)
        .expect("metadata is readable")
        .expect("the database records its identity")
        .database_id()
        .as_bytes()
}

#[test]
fn a_row_bound_to_a_table_field_is_stored_as_a_reference_and_reads_back() {
    let database = published_garage();
    let root = database.root();

    // The published Vehicle row holds the identity of the Owner row, never a
    // copy of it (ORNA-REF-001).
    let tuple = committed_vehicle_tuple(&root, database.vehicle_relation, "v1");
    let fields = decode_canonical_storage_value(&tuple).expect("a canonical stored tuple");
    assert_eq!(
        fields,
        Raw::Array(vec![stored_reference(
            database_id(&root),
            database.owner_relation,
            "ada"
        )]),
        "the owner column stores [database, table ObjectId, primary key]"
    );
    assert!(
        !tuple.windows(3).any(|window| window == b"Ada"),
        "no duplicate of the referenced row is stored"
    );

    // `vehicle.owner.name` resolves through the same snapshot (ORNA-REF-002),
    // from a one-shot run and from the REPL.
    let name = orna(&root, &["run", "main.owner_name"]);
    assert_success(&name, "orna run of a read through the reference");
    assert!(
        transcript(&name).contains("\"Ada\""),
        "the reference reads back the owner's name\n{}",
        transcript(&name)
    );
    let listed = orna_repl(&root, "main.Vehicle | map(v => v.owner.name)");
    assert_success(&listed, "REPL read through the reference");
    assert!(
        transcript(&listed).contains("\"Ada\""),
        "{}",
        transcript(&listed)
    );
    let owner = orna_repl(&root, "main.Vehicle | map(v => v.owner)");
    assert_success(&owner, "REPL read of the reference itself");
    assert!(
        transcript(&owner).contains("TableRef"),
        "the field holds a stored reference value\n{}",
        transcript(&owner)
    );
}

#[test]
fn a_reference_exposes_the_key_of_its_target_to_the_checker_and_runtime() {
    // ORNA-REF-002: `vehicle.owner.key` is available without loading the target
    // row, so the checker types it as the target's primary key and the runtime
    // answers it from the stored reference.
    let database = published_garage();
    let root = database.root();

    let key = orna(&root, &["run", "main.owner_key"]);
    assert_success(&key, "orna run of the reference's key");
    assert!(
        transcript(&key).contains("\"ada\""),
        "the reference exposes the owner's primary key\n{}",
        transcript(&key)
    );
    let listed = orna_repl(&root, "main.Vehicle | map(v => v.owner.key)");
    assert_success(&listed, "REPL read of the reference's key");
    assert!(
        transcript(&listed).contains("\"ada\""),
        "{}",
        transcript(&listed)
    );
}

#[test]
fn a_reference_to_a_missing_row_is_refused_at_commit_and_changes_nothing() {
    let database = published_garage();
    let root = database.root();

    // The run binds `ghost`, deletes it, then stores a reference to it: the
    // candidate state at commit holds a reference without its row.
    let dangling = orna(&root, &["run", "main.dangle"]);
    assert_refused(&dangling, "ORNA-REF-DANGLING", "a dangling reference");
    assert!(
        transcript(&dangling).contains("failed atomically"),
        "no partial change was committed\n{}",
        transcript(&dangling)
    );

    // Nothing of the refused activation is pending or visible.
    let published = orna(&root, &["publish", "--message", "Nothing"]);
    assert!(
        transcript(&published).contains("nothing to publish"),
        "the refused activation left no durable change\n{}",
        transcript(&published)
    );

    // The code is catalogued, since the specification names none.
    let explained = orna(&root, &["explain", "ORNA-REF-DANGLING"]);
    assert_success(&explained, "orna explain ORNA-REF-DANGLING");
    assert!(
        transcript(&explained).contains("stored reference names no row"),
        "{}",
        transcript(&explained)
    );
}

#[test]
fn deleting_or_rekeying_a_referenced_row_is_restricted() {
    let database = published_garage();
    let root = database.root();

    // Non-optional references default to `restrict` and nothing cascades.
    let deleted = orna(&root, &["run", "main.drop_owner"]);
    assert_refused(&deleted, "ORNA-REF-DANGLING", "delete of a referenced row");
    let rekeyed = orna(&root, &["run", "main.rekey_owner"]);
    assert_refused(&rekeyed, "ORNA-REF-DANGLING", "re-key of a referenced row");

    let published = orna(&root, &["publish", "--message", "Nothing"]);
    assert!(
        transcript(&published).contains("nothing to publish"),
        "{}",
        transcript(&published)
    );
    let name = orna(&root, &["run", "main.owner_name"]);
    assert!(
        transcript(&name).contains("\"Ada\""),
        "the referenced row is untouched\n{}",
        transcript(&name)
    );
}

#[test]
fn dependents_may_move_in_the_same_activation_as_the_target() {
    // ORNA-MUT-007: the constraint is judged at activation commit, so a program
    // may update dependent references and re-key the target together.
    let database = published_garage();
    let root = database.root();
    let moved = orna(&root, &["run", "main.rekey_with_dependents"]);
    assert_success(&moved, "re-key with the dependent updated in the same run");
    let name = orna(&root, &["run", "main.owner_name"]);
    assert!(
        transcript(&name).contains("\"Ada\""),
        "the dependent reads through the re-keyed target\n{}",
        transcript(&name)
    );

    // ORNA-MUT-008: a caller that wants the dependents changed updates them
    // explicitly in the same activation, then the old target may be deleted.
    let database = published_garage();
    let root = database.root();
    let moved = orna(&root, &["run", "main.move_then_delete"]);
    assert_success(&moved, "delete after the dependent moved in the same run");
    let listed = orna_repl(&root, "main.Vehicle | map(v => v.owner.name)");
    assert_success(&listed, "REPL read of the moved dependent");
    assert!(
        transcript(&listed).contains("\"Bob\""),
        "the dependent reads the row it moved to\n{}",
        transcript(&listed)
    );
}

#[test]
fn the_repl_binds_checks_and_publishes_references_the_same_way() {
    let database = published_garage();
    let root = database.root();

    let inserted = orna_repl(
        &root,
        r#"main.Vehicle.insert({ id: "v2", owner: main.Owner.insert({ id: "bob", name: "Bob" }) })"#,
    );
    assert_success(&inserted, "REPL insert of a row that holds a reference");
    assert!(
        transcript(&inserted).contains("TableRef"),
        "the staged row already holds the reference, not a copy\n{}",
        transcript(&inserted)
    );
    let published = orna(&root, &["publish", "--message", "Add bob's vehicle"]);
    assert_success(&published, "orna publish of REPL rows that hold references");

    let deleted = orna_repl(&root, r#"main.Owner.delete("bob")"#);
    assert_repl_refused(
        &deleted,
        "ORNA-REF-DANGLING",
        "REPL delete of a referenced row",
    );
    let rekeyed = orna_repl(&root, r#"main.Owner.rekey("bob", "bobby")"#);
    assert_repl_refused(
        &rekeyed,
        "ORNA-REF-DANGLING",
        "REPL re-key of a referenced row",
    );

    let listed = orna_repl(&root, "main.Vehicle | map(v => v.owner.name)");
    assert_success(&listed, "REPL read after the refusals");
    let text = transcript(&listed);
    assert!(
        text.contains("\"Ada\"") && text.contains("\"Bob\""),
        "both references still resolve\n{text}"
    );
}

#[test]
fn an_optional_reference_is_stored_when_present_and_checked_when_present() {
    let database = committed_database(OPTIONAL_SOURCE);
    let root = database.root();
    assert_success(&orna(&root, &["run", "seed"]), "orna run seed");
    assert_success(
        &orna(&root, &["publish", "--message", "Seed"]),
        "orna publish of present and absent optional references",
    );

    let spare = orna_repl(&root, "main.Vehicle | map(v => v.spare)");
    assert_success(&spare, "REPL read of the optional references");
    assert!(
        transcript(&spare).contains("TableRef"),
        "the present optional reference is stored as a reference\n{}",
        transcript(&spare)
    );

    // A present optional reference is validated like any other; there is no
    // weak or dangling reference in version 1.0 (ORNA-REF-003).
    let dangling = orna(&root, &["run", "main.dangling_spare"]);
    assert_refused(
        &dangling,
        "ORNA-REF-DANGLING",
        "a dangling optional reference",
    );
}

/// A table declared in a non-root module, `shop/owners.orna`, read through the
/// REPL by its qualified name. The root module imports nothing, so the name
/// resolves through the namespace alone, and the committed read must key the
/// table by that same qualified name.
#[test]
fn a_non_root_module_table_reads_through_the_repl_by_its_qualified_name() {
    let directory = TempDir::new().expect("create the test directory");
    let root = directory.path().join("garage");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(
        root.join("main.orna"),
        "use shop.owners;\npub fn main() {}\n",
    )
    .expect("write main.orna");
    fs::create_dir_all(root.join("shop")).expect("create the shop module directory");
    fs::write(
        root.join("shop/owners.orna"),
        "pub table Owner(id: Str) { name: Str, }\npub fn seed() { Owner.insert({ id: \"ada\", name: \"Ada\" }); }\n",
    )
    .expect("write shop/owners.orna");
    git(
        &root,
        &[
            "add",
            "main.orna",
            "shop/owners.orna",
            ".orna/database.orna",
        ],
    );
    let committed = orna(&root, &["commit", "-m", "Declare the shop owners"]);
    assert_success(&committed, "orna commit of a non-root module table");
    assert_success(
        &orna(&root, &["run", "shop.owners.seed"]),
        "orna run shop.owners.seed",
    );
    assert_success(
        &orna(&root, &["publish", "--message", "Seed the shop owners"]),
        "orna publish of a non-root module row",
    );

    let listed = orna_repl(&root, "shop.owners.Owner | map(o => o.name)");
    assert_success(&listed, "REPL read of a non-root module table");
    let text = transcript(&listed);
    assert!(
        text.contains("\"Ada\""),
        "the qualified REPL read returns the committed row\n{text}"
    );
}

/// A project whose modules are `files`, initialised with `orna init` and
/// committed with `orna commit`. Returns the directory and project root.
fn committed_project(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let directory = TempDir::new().expect("create the test directory");
    let root = directory.path().join("garage");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    let mut staged = vec!["add", ".orna/database.orna"];
    for (path, source) in files {
        let path_on_disk = root.join(path);
        fs::create_dir_all(path_on_disk.parent().expect("module directory"))
            .expect("create the module directory");
        fs::write(path_on_disk, source).expect("write the module source");
        staged.push(path);
    }
    git(&root, &staged);
    assert_success(
        &orna(&root, &["commit", "-m", "Declare the project"]),
        "orna commit",
    );
    (directory, root)
}

/// A table field typed by a table of another module (`owner: owners.Owner`)
/// stores a reference to that module's row, and the row is inserted and read
/// back through the qualified table type (ORNA-REF-001, ORNA-REF-002).
#[test]
fn a_cross_module_table_field_references_a_row_of_the_other_module() {
    let (_directory, root) = committed_project(&[
        (
            "main.orna",
            "use owners;\n\
             pub table Vehicle(id: Str) { owner: owners.Owner, }\n\
             pub fn seed() { Vehicle.insert({ id: \"v1\", owner: owners.Owner.insert({ id: \"ada\", name: \"Ada\" }) }); }\n\
             pub fn owner_name(): Str = Vehicle | map(v => v.owner.name) | first() ?? \"none\";\n\
             pub fn owner_key(): Str = Vehicle | map(v => v.owner.key) | first() ?? \"none\";\n\
             pub fn main() {}\n",
        ),
        ("owners.orna", "pub table Owner(id: Str) { name: Str, }\n"),
    ]);

    assert_success(&orna(&root, &["run", "seed"]), "orna run seed");
    assert_success(
        &orna(&root, &["publish", "--message", "Seed the garage"]),
        "orna publish of a cross-module reference",
    );
    let name = orna(&root, &["run", "main.owner_name"]);
    assert_success(
        &name,
        "orna run of a read through the cross-module reference",
    );
    assert!(
        transcript(&name).contains("\"Ada\""),
        "the cross-module reference reads back the owner's name\n{}",
        transcript(&name)
    );
    let key = orna(&root, &["run", "main.owner_key"]);
    assert_success(&key, "orna run of the key of a cross-module reference");
    assert!(
        transcript(&key).contains("\"ada\""),
        "the cross-module reference exposes the owner's key\n{}",
        transcript(&key)
    );
}

#[test]
fn a_mounted_save_of_a_missing_reference_is_refused_and_keeps_the_stored_row() {
    // ORNA-MUT-007 and ORNA-REF-003 through the mount: a saved row whose reference
    // names no committed row is refused at sync with EINVAL, and the stored row keeps
    // the reference it had.
    if !require_fuse_or_skip() {
        return;
    }
    let database = published_garage();
    let root = database.root();
    let mount_parent = TempDir::new().expect("create the mount parent");
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(&root, &mountpoint);
    let vehicle = mountpoint.join("Vehicle/v1/data.orna");
    mounted.await_path(&vehicle);

    let stored = fs::read_to_string(&vehicle).expect("read the stored vehicle");
    assert!(
        stored.contains("\"ada\""),
        "the stored vehicle names its owner\n{stored}"
    );
    write_and_sync(&vehicle, &stored)
        .expect("a save whose reference names a committed owner is admitted");

    let dangling = stored.replace("\"ada\"", "\"ghost\"");
    assert_ne!(dangling, stored, "the candidate must name a missing owner");
    assert_errno(
        write_and_sync(&vehicle, &dangling),
        &[libc::EINVAL],
        "a save whose reference names no committed owner is refused at sync",
    );
    assert_eq!(
        fs::read_to_string(&vehicle).expect("read the accepted vehicle through a new open"),
        stored,
        "the refused save leaves the stored row unchanged"
    );

    // The refused candidate is retained as a draft, which blocks unmount until a valid
    // save replaces it.
    write_and_sync(&vehicle, &stored).expect("the restored vehicle is admitted");
    run_orna_unmount(&root, &mountpoint);
    let status = mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("the mount exits after unmount");
    assert!(status.success(), "mount stderr: {}", mounted.stderr_text());

    let key = orna(&root, &["run", "main.owner_key"]);
    assert_success(
        &key,
        "orna run of the reference's key after the refused save",
    );
    assert!(
        transcript(&key).contains("\"ada\""),
        "the committed reference still names the owner\n{}",
        transcript(&key)
    );
}
