#![cfg(target_os = "linux")]

//! Gate E item E8 (composite keys): a table with a two-component primary key
//! projects one directory per declared key component, recursively, and each
//! row's `data.orna` is readable through the mount (profiles/vfs-1.md:39-41
//! "Composite keys use one component per declared key recursively"). The
//! workspace is built with product commands only: `orna init`, `orna commit`,
//! REPL inserts, `orna publish`, then `orna mount --cwd`.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

#[allow(dead_code)]
#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{require_fuse_or_skip, spawn_mount_process};
#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;
#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;

const STOCK_SOURCE: &str = "pub table Stock(location: Str, sku: Str) { quantity: Int, }\n";

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
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn transcript(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output, step: &str) {
    assert!(
        output.status.success(),
        "{step} failed:\n{}",
        transcript(output)
    );
}

/// `orna init` a workspace, commit the `Stock` declaration, and return its root.
fn committed_stock(directory: &TempDir) -> PathBuf {
    let root = directory.path().join("stock");
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
    fs::write(root.join("main.orna"), STOCK_SOURCE).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Declare Stock"]);
    assert_success(&committed, "orna commit");
    root
}

fn insert_and_publish(root: &Path, location: &str, sku: &str, quantity: u32) {
    let input = format!(
        r#"main.Stock.insert({{ location: "{location}", sku: "{sku}", quantity: {quantity} }})"#
    );
    assert_success(&orna_repl(root, &input), "REPL composite insert");
    let published = orna(
        root,
        &["publish", "--message", &format!("Stock {location} {sku}")],
    );
    assert_success(&published, "orna publish");
}

fn sorted_names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .expect("list the directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// profiles/vfs-1.md:39-41 and 44-50 (Gate E item E8): each declared key
/// component is one directory level, so `(location, sku)` projects as
/// `Stock/<location>/<sku>/data.orna`. Each component is escaped by the same
/// rules as a single key: a `/` in the second component is `%2F`, and a space
/// is `%20`. Every row reads its own committed quantity through the mount.
#[test]
fn composite_key_components_project_as_nested_directories() {
    if !require_fuse_or_skip() {
        return;
    }
    let directory = TempDir::new().expect("a temporary directory");
    let root = committed_stock(&directory);
    insert_and_publish(&root, "north", "widget 001", 1);
    insert_and_publish(&root, "north", "a/b", 2);
    insert_and_publish(&root, "south", "c.d", 3);

    let mount_parent = TempDir::new().expect("a mount parent");
    let mountpoint = mount_parent.path().join("stock");
    let mut mounted = spawn_mount_process(&root, &mountpoint);
    let stock = mountpoint.join("Stock");
    let expected_row = stock.join("south/c.d/data.orna");
    mounted.await_path(&expected_row);

    assert_eq!(
        sorted_names(&stock),
        vec!["north".to_owned(), "south".to_owned()],
        "the first key component is one directory level"
    );
    assert_eq!(
        sorted_names(&stock.join("north")),
        vec!["a%2Fb".to_owned(), "widget%20001".to_owned()],
        "the second key component is one directory level, escaped per component"
    );
    assert_eq!(sorted_names(&stock.join("south")), vec!["c.d".to_owned()]);

    for (path, quantity) in [
        ("north/widget%20001/data.orna", 1),
        ("north/a%2Fb/data.orna", 2),
        ("south/c.d/data.orna", 3),
    ] {
        let view = fs::read_to_string(stock.join(path)).expect("read the composite row");
        assert!(
            view.contains(&format!("quantity: {quantity}")),
            "{path} reads its own committed quantity {quantity}: {view}"
        );
    }
}
