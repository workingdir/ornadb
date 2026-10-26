//! Format-3 publication of rows and tables larger than one row page, through
//! product commands only: `orna init`, `orna commit`, REPL inserts and
//! updates, `orna publish`, then `orna query` and `orna history --diff` read
//! the rows back (store-format-3 §3 and §5, ORNA-ROW-003, ORNA-ROW-005).

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

const NOTE_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n";

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

fn git_bytes(root: &Path, arguments: &[&str]) -> Vec<u8> {
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
    output.stdout
}

fn git(root: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_bytes(root, arguments)).expect("git prints UTF-8")
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
    assert!(
        !transcript(output).contains("error["),
        "{step} reported an error\n{}",
        transcript(output)
    );
}

/// `orna init`, a test-local commit identity, `main.orna` replaced by
/// `source`, then the source staged with Git and committed with
/// `orna commit`. Returns the root and the relation the commit admitted.
fn committed_database(directory: &TempDir, source: &str) -> (PathBuf, String) {
    committed_table(directory, source, "Note")
}

/// [`committed_database`] for the table named `table`.
fn committed_table(directory: &TempDir, source: &str, table: &str) -> (PathBuf, String) {
    let root = directory.path().join("notes");
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
    let committed = orna(&root, &["commit", "-m", "Declare Note"]);
    assert_success(&committed, "orna commit");
    let relation = String::from_utf8_lossy(&committed.stdout)
        .lines()
        .find_map(|line| line.strip_prefix(&format!("admitted table {table} as relation ")))
        .expect("orna commit admits the named table")
        .trim()
        .to_owned();
    (root, relation)
}

fn publish(root: &Path, message: &str) {
    let published = orna(root, &["publish", "--message", message]);
    assert_success(&published, "orna publish");
}

fn json(output: &Output, step: &str) -> serde_json::Value {
    assert_success(output, step);
    serde_json::from_slice(&output.stdout).expect("the command prints JSON")
}

fn query_rows(root: &Path, arguments: &[&str]) -> u64 {
    let report = json(&orna(root, arguments), "orna query");
    report["rows"].as_u64().expect("the report counts rows")
}

/// Every row of the table, counted in key ranges that each fit one query
/// page (`orna query --limit` is at most 256, and rows are keyed `n0000`
/// through `n0599`; a range is lower-inclusive and upper-exclusive).
fn table_row_count(root: &Path, relation: &str) -> u64 {
    [("n0000", "n0200"), ("n0200", "n0400"), ("n0400", "n0600")]
        .into_iter()
        .map(|(from, until)| {
            query_rows(
                root,
                &[
                    "query",
                    relation,
                    "--from",
                    &format!("text:{from}"),
                    "--until",
                    &format!("text:{until}"),
                    "--limit",
                    "256",
                    "--format",
                    "json",
                ],
            )
        })
        .sum()
}

/// The kind, encoded length and object ID of every format-3 node `data`
/// blob reachable from the committed `.orna/store` (store-format-3 §1: every
/// node data is `[1, kind, ...]`).
fn store_nodes(root: &Path) -> Vec<(u8, usize, String)> {
    let listing = git(root, &["ls-tree", "-r", "HEAD", ".orna/store"]);
    let mut nodes = Vec::new();
    for line in listing.lines() {
        let (entry, path) = line.split_once('\t').expect("an ls-tree entry");
        if path != ".orna/store/data" && !path.ends_with("/data") {
            continue;
        }
        let oid = entry.split(' ').nth(2).expect("an ls-tree object id");
        let data = git_bytes(root, &["cat-file", "blob", oid]);
        assert_eq!(data[1], 0x01, "node data starts [1, kind, ...]");
        nodes.push((data[2], data.len(), oid.to_owned()));
    }
    nodes
}

/// The distinct row pages of the committed store: kind-1 ordered leaves whose
/// domain is the table row map (`rows`), not the relation catalog
/// (`relations`), which every publish rewrites (store-format-3 §3).
fn row_pages(root: &Path) -> BTreeSet<String> {
    let mut pages = BTreeSet::new();
    for line in git(root, &["ls-tree", "-r", "HEAD", ".orna/store"]).lines() {
        let (entry, path) = line.split_once('\t').expect("an ls-tree entry");
        if path != ".orna/store/data" && !path.ends_with("/data") {
            continue;
        }
        let oid = entry.split(' ').nth(2).expect("an ls-tree object id");
        let data = git_bytes(root, &["cat-file", "blob", oid]);
        if data[2] == 1 && data[..data.len().min(32)].windows(4).any(|w| w == b"rows") {
            pages.insert(oid.to_owned());
        }
    }
    pages
}

/// A note body beyond the 8 KiB inline row limit and within the 16 KiB a
/// REPL string literal may hold.
fn long_body(label: &str) -> String {
    format!("{label}: {}", "the ridge log continues ".repeat(500))
}

/// store-format-3 §5: a row entry over 8 KiB is stored through tag 60113 and
/// a kind-3 overflow node whose bytes sit behind an OGB-2 byte index, so its
/// row page stays small. The CLI publishes, lists, looks up, diffs and
/// updates the row (A07-storage-rows-05).
#[test]
fn a_row_over_8_kib_publishes_through_the_overflow_graph() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation) = committed_database(&directory, NOTE_SOURCE);
    let body = long_body("first");
    assert!(body.len() > 8 * 1024 && body.len() < 16 * 1024);

    let inserted = orna_repl(
        &root,
        &format!(r#"main.Note.insert({{ id: "ridge", body: "{body}" }})"#),
    );
    assert_success(&inserted, "REPL insert");
    publish(&root, "Add the ridge log");

    let nodes = store_nodes(&root);
    assert!(
        nodes.iter().any(|(kind, _, _)| *kind == 3),
        "the long row is a kind-3 overflow node: {nodes:?}"
    );
    assert!(
        nodes.iter().any(|(kind, _, _)| *kind == 4),
        "the overflow bytes sit behind a byte index: {nodes:?}"
    );
    let largest_page = nodes
        .iter()
        .filter(|(kind, _, _)| *kind == 1)
        .map(|(_, length, _)| *length)
        .max()
        .expect("the store has row pages");
    assert!(
        largest_page < 1024,
        "the row page names the overflow node instead of holding the body: {largest_page} bytes"
    );

    assert_eq!(
        query_rows(&root, &["query", &relation, "--format", "json"]),
        1
    );
    assert_eq!(
        query_rows(
            &root,
            &["query", &relation, "--key", "ridge", "--format", "json"]
        ),
        1
    );
    let added = json(
        &orna(
            &root,
            &[
                "history", &relation, "--diff", "HEAD~1", "HEAD", "--format", "json",
            ],
        ),
        "orna history --diff",
    );
    assert_eq!(added["added"], 1, "{added}");

    let updated = orna_repl(
        &root,
        &format!(
            r#"main.Note.update("ridge", {{ body: "{}" }})"#,
            long_body("second")
        ),
    );
    assert_success(&updated, "REPL update");
    publish(&root, "Revise the ridge log");
    let changed = json(
        &orna(
            &root,
            &[
                "history", &relation, "--diff", "HEAD~1", "HEAD", "--format", "json",
            ],
        ),
        "orna history --diff",
    );
    assert_eq!(changed["changed"], 1, "{changed}");
    assert_eq!(changed["changes"][0]["content_changed"], true, "{changed}");
}

/// A note body of the row's label and index, about 170 bytes.
fn note_body(index: usize, label: &str) -> String {
    format!("{label} {index}: {}", "the ridge log ".repeat(12))
}

/// Seeds `Note` with 600 rows, publishes them, and returns the root, the
/// relation and the object IDs of the row pages. The table spans several
/// pages (store-format-3 §3).
fn seed_six_hundred_notes(directory: &TempDir) -> (PathBuf, String, BTreeSet<String>) {
    let (root, relation) = committed_database(directory, NOTE_SOURCE);
    let inserts = (0..600)
        .map(|index| {
            format!(
                r#"main.Note.insert({{ id: "n{index:04}", body: "{}" }})"#,
                note_body(index, "first")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_success(&orna_repl(&root, &inserts), "REPL inserts");
    publish(&root, "Add six hundred notes");
    let pages = row_pages(&root);
    assert!(
        pages.len() >= 4,
        "the table spans several row pages: {}",
        pages.len()
    );
    assert_eq!(table_row_count(&root, &relation), 600);
    (root, relation, pages)
}

/// store-format-3 §3 and ORNA-ROW-006 (A07-storage-rows-04): an update to a
/// table of many row pages rewrites only the page that holds the row, and
/// every other page keeps its object; ORNA-ROW-010: rewriting a row to the
/// value it already holds publishes no change at all. The new body has the
/// byte length of the old one, so the row's encoded size and every page
/// boundary stay put.
#[test]
fn an_update_to_a_large_table_rewrites_only_the_page_holding_the_row() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation, pages_before) = seed_six_hundred_notes(&directory);
    let revise = format!(
        r#"main.Note.update("n0300", {{ body: "{}" }})"#,
        note_body(300, "third")
    );
    assert_success(&orna_repl(&root, &revise), "REPL update");
    publish(&root, "Revise one note");
    let pages_after = row_pages(&root);
    let rewritten = pages_after.difference(&pages_before).count();
    assert!(
        rewritten == 1 && pages_before.difference(&pages_after).count() == 1,
        "one of {} row pages is rewritten, not {rewritten}",
        pages_before.len()
    );
    let changed = json(
        &orna(
            &root,
            &[
                "history", &relation, "--diff", "HEAD~1", "HEAD", "--format", "json",
            ],
        ),
        "orna history --diff",
    );
    assert_eq!(changed["changed"], 1, "{changed}");
    assert_eq!(table_row_count(&root, &relation), 600);

    let head = git(&root, &["rev-parse", "HEAD"]);
    assert_success(&orna_repl(&root, &revise), "REPL update to the same body");
    let unchanged = orna(&root, &["publish", "--message", "Revise one note again"]);
    assert_success(&unchanged, "orna publish");
    assert!(
        String::from_utf8_lossy(&unchanged.stdout).contains("nothing to publish"),
        "{}",
        transcript(&unchanged)
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
}

/// The same update with a body one byte longer. Pages close on their byte
/// size as well as their entry count (`NODE_DATA_LIMIT`, `closes_before`), so
/// a grown row can move a page boundary into the next page; the complete
/// build moves it the same way (native_graph's append-maintenance test pins
/// that equality). The bound is the page holding the row and at most the
/// neighbour it pushes.
#[test]
fn growing_a_row_rewrites_its_page_and_at_most_the_neighbour_it_shifts() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation, pages_before) = seed_six_hundred_notes(&directory);
    let revise = format!(
        r#"main.Note.update("n0300", {{ body: "{}" }})"#,
        note_body(300, "second")
    );
    assert_success(&orna_repl(&root, &revise), "REPL update");
    publish(&root, "Grow one note");
    let pages_after = row_pages(&root);
    let rewritten = pages_after.difference(&pages_before).count();
    let removed = pages_before.difference(&pages_after).count();
    assert!(
        (1..=2).contains(&rewritten) && removed == rewritten,
        "a grown row rewrites its page and at most one neighbour: {rewritten} new, {removed} removed"
    );
    assert_eq!(table_row_count(&root, &relation), 600);
}

/// A table whose single primary-key component is `Int` is keyed by the
/// integer the committed descriptor declares (store-format-3 section 5). A
/// `--key` spelling is parsed as `Int`, so a row past the first page of 256 is
/// found by its integer spelling, and a non-integer spelling is refused rather
/// than read as text or bytes.
#[test]
fn an_int_keyed_table_is_queried_by_its_declared_integer_key() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Tick(id: Int) { body: Str, }\n",
        "Tick",
    );
    let inserts = (0..300)
        .map(|index| format!(r#"main.Tick.insert({{ id: {index}, body: "tick {index}" }})"#))
        .collect::<Vec<_>>()
        .join("\n");
    assert_success(&orna_repl(&root, &inserts), "REPL integer inserts");
    publish(&root, "Add three hundred ticks");

    // A point lookup of a committed row exits 0; an absent row exits non-zero
    // with "Row is not committed", so exit status alone proves the row is found.
    let found = orna(
        &root,
        &["query", &relation, "--key", "299", "--format", "json"],
    );
    assert_success(&found, "orna query --key 299");
    let refused = orna(
        &root,
        &["query", &relation, "--key", "tick", "--format", "json"],
    );
    assert!(
        !refused.status.success(),
        "a non-integer spelling is refused for an Int key:\n{}",
        transcript(&refused)
    );
    assert!(
        transcript(&refused).contains("Int"),
        "the refusal names the declared Int type:\n{}",
        transcript(&refused)
    );
}

/// Point, range and missing-key queries over a published `Int`-keyed table of
/// 300 rows. The point read finds a row past the first page of 256, `--from`
/// and `--until` take half-open `int:` bounds, and an absent key exits non-zero
/// with "Row is not committed".
#[test]
fn an_int_keyed_store_answers_point_range_and_missing_key_queries() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Tick(id: Int) { body: Str, }\n",
        "Tick",
    );
    let inserts = (0..300)
        .map(|index| format!(r#"main.Tick.insert({{ id: {index}, body: "tick {index}" }})"#))
        .collect::<Vec<_>>()
        .join("\n");
    assert_success(&orna_repl(&root, &inserts), "REPL integer inserts");
    publish(&root, "Add three hundred ticks");

    let point = json(
        &orna(
            &root,
            &["query", &relation, "--key", "299", "--format", "json"],
        ),
        "orna query --key 299",
    );
    assert_eq!(point["rows"], 1, "{point}");

    let range = json(
        &orna(
            &root,
            &[
                "query", &relation, "--from", "int:250", "--until", "int:260", "--format", "json",
            ],
        ),
        "orna query --from int:250 --until int:260",
    );
    assert_eq!(range["rows"], 10, "{range}");

    let missing = orna(
        &root,
        &["query", &relation, "--key", "9999", "--format", "json"],
    );
    assert!(
        !missing.status.success() && transcript(&missing).contains("Row is not committed"),
        "an absent Int key is reported as not committed:\n{}",
        transcript(&missing)
    );
}

/// A table whose primary key has two components has no single typed key
/// spelling, yet a predicate scan over it pages the rows in tuple order, so
/// every row is evaluated across page boundaries.
#[test]
fn a_composite_key_predicate_scan_pages_every_row() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Stock(location: Str, sku: Str) { quantity: Int, content: Blob, }\n",
        "Stock",
    );
    let media_root = format!("{:?}", root.to_string_lossy().as_ref());
    // The REPL admits one effectful request per invocation, so each blob-bearing
    // row is its own single-unit input: capture, annotate, insert, publish.
    // Rows 200..300 are annotated text/plain and match the predicate; rows
    // 0..200 are not. The matches therefore start before the 256-row page
    // boundary and continue past it.
    for index in 0..300 {
        let name = format!("tone{index:03}.txt");
        let mime = if index >= 200 { "text/plain" } else { "application/octet-stream" };
        std::fs::write(root.join(&name), vec![b'r'; 64]).expect("write the captured file");
        let input = format!(
            r#"let raw = sys.blob.capture_file({media_root}, "{name}", 4096);
let content = sys.blob.annotate(raw, "{mime}");
main.Stock.insert({{ location: "north", sku: "widget {index:03}", quantity: {index}, content: content }})"#
        );
        assert_success(&orna_repl(&root, &input), "REPL composite insert");
        publish(&root, &format!("Add stock row {index:03}"));
    }

    // All 300 rows are scanned in ordered pages of 256. Exactly the 100 rows
    // from index 200 match, so the listing is not truncated and must report
    // every matching key once, in key order, across the 256-row boundary.
    let scanned = json(
        &orna(
            &root,
            &["query", relation.as_str(), "--kind", "text/plain", "--limit", "256", "--format", "json"],
        ),
        "orna query composite predicate scan",
    );
    assert_eq!(scanned["rows"], 300, "{scanned}");
    assert_eq!(scanned["plan"]["candidates"], 300, "{scanned}");
    assert_eq!(scanned["plan"]["matched"], 100, "{scanned}");
    assert_eq!(scanned["blobs"], 100, "{scanned}");
    let listed: Vec<String> = scanned["listings"]
        .as_array()
        .expect("the listing is an array")
        .iter()
        .map(|entry| entry["key"].as_str().expect("a rendered key").to_owned())
        .collect();
    let expected: Vec<String> = (200..300)
        .map(|index| format!("north,widget {index:03}"))
        .collect();
    assert_eq!(listed, expected, "every matching key, once, in key order");
}

/// A bound whose spelling does not match the table's `Int` primary key is
/// refused with the valid spelling, never answered empty.
#[test]
fn a_bound_of_the_wrong_key_type_is_refused_naming_the_valid_spelling() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Tick(id: Int) { body: Str, }\n",
        "Tick",
    );
    assert_success(
        &orna_repl(&root, r#"main.Tick.insert({ id: 5, body: "five" })"#),
        "REPL integer insert",
    );
    publish(&root, "Add one tick");

    let refused = orna(&root, &["query", relation.as_str(), "--from", "text:a", "--format", "json"]);
    assert!(
        !refused.status.success() && transcript(&refused).contains("int:N"),
        "a text bound on an Int key is refused naming int:N:\n{}",
        transcript(&refused)
    );
}

/// A predicate scan over more than one page of 256 rows reads every row
/// through the bounded-range pages, so all 300 rows are evaluated and listed.
#[test]
fn a_predicate_scan_over_more_than_one_page_evaluates_every_row() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Tick(id: Int) { body: Str, }\n",
        "Tick",
    );
    let inserts = (0..300)
        .map(|index| format!(r#"main.Tick.insert({{ id: {index}, body: "tick {index}" }})"#))
        .collect::<Vec<_>>()
        .join("\n");
    assert_success(&orna_repl(&root, &inserts), "REPL integer inserts");
    publish(&root, "Add three hundred ticks");

    let scanned = json(
        &orna(
            &root,
            &["query", &relation, "--from", "int:0", "--kind", "text/plain", "--format", "json"],
        ),
        "orna query --from int:0 --kind text/plain",
    );
    assert_eq!(scanned["rows"], 300, "{scanned}");
    // The rows are text-only, so no field is a Blob and the annotation plan
    // has no coordinates to evaluate: candidates and matched are both zero.
    assert_eq!(scanned["plan"]["candidates"], 0, "{scanned}");
    assert_eq!(scanned["plan"]["matched"], 0, "{scanned}");
    assert_eq!(scanned["blobs"], 0, "{scanned}");
}

/// `orna history` resolves an integer-looking key with the spelling that
/// `orna query --key` uses. The row lookup runs first, so the committed
/// `Int`-keyed row `7` is found and the walk is reached. A text-only row has
/// no descriptor tree, so the walk then refuses with `DescriptorNotInRow`;
/// that exact error proves the lookup resolved the row. A Blob-bearing row
/// would yield revisions instead, which this fixture does not build.
#[test]
fn history_resolves_an_integer_looking_key_past_the_row_lookup() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Tick(id: Int) { body: Str, }\n",
        "Tick",
    );
    assert_success(
        &orna_repl(&root, r#"main.Tick.insert({ id: 7, body: "seven" })"#),
        "REPL integer insert",
    );
    publish(&root, "Add tick seven");

    let history = orna(&root, &["history", relation.as_str(), "7", "--format", "json"]);
    let stderr = transcript(&history);
    assert!(
        !history.status.success() && stderr.contains("DescriptorNotInRow"),
        "the integer spelling resolves to the row, then the walk reports DescriptorNotInRow:\n{stderr}"
    );
    assert!(
        !stderr.contains("Row is not committed"),
        "the integer spelling must not report the row as uncommitted:\n{stderr}"
    );
}

/// Any `--from`/`--until` bound on a composite key is refused, whatever its
/// spelling, because a bound of one component has no order over the pair.
#[test]
fn a_bound_on_a_composite_key_is_refused_for_every_spelling() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Stock(location: Str, sku: Str) { quantity: Int, }\n",
        "Stock",
    );
    assert_success(
        &orna_repl(
            &root,
            r#"main.Stock.insert({ location: "north", sku: "widget", quantity: 3 })"#,
        ),
        "REPL composite insert",
    );
    publish(&root, "Add one stock row");

    for bound in ["text:north", "int:1", "uint:1", "bytes:0102"] {
        let refused = orna(&root, &["query", relation.as_str(), "--from", bound, "--format", "json"]);
        assert!(
            !refused.status.success()
                && transcript(&refused).contains("composite primary key")
                && transcript(&refused).contains("ornadb-5il1.461"),
            "a {bound} bound on a composite key is refused with the remedy:\n{}",
            transcript(&refused)
        );
    }
}

/// A `Str` primary key is spelled `text:KEY` in a bound; `bytes:HEX` is
/// refused naming the valid spelling, never answered empty.
#[test]
fn a_bytes_bound_on_a_str_key_is_refused_naming_text() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Note(id: Str) { body: Str, }\n",
        "Note",
    );
    assert_success(
        &orna_repl(&root, r#"main.Note.insert({ id: "alpha", body: "a" })"#),
        "REPL text insert",
    );
    publish(&root, "Add one note");

    let refused = orna(&root, &["query", relation.as_str(), "--from", "bytes:61", "--format", "json"]);
    assert!(
        !refused.status.success() && transcript(&refused).contains("text:KEY"),
        "a bytes bound on a Str key is refused naming text:KEY:\n{}",
        transcript(&refused)
    );
}

/// A `Bool` primary key is not addressable by a bound or `--key`; both are
/// refused with the bead that tracks it. A predicate scan over it still pages
/// every row.
#[test]
fn a_bool_keyed_table_is_refused_for_bounds_and_key_lookup() {
    let directory = TempDir::new().expect("a temporary directory");
    let (root, relation) = committed_table(
        &directory,
        "pub table Flag(on: Bool) { body: Str, }\n",
        "Flag",
    );
    assert_success(
        &orna_repl(&root, r#"main.Flag.insert({ on: true, body: "yes" })"#),
        "REPL bool insert",
    );
    publish(&root, "Add one flag");

    let scanned = json(
        &orna(
            &root,
            &["query", relation.as_str(), "--kind", "text/plain", "--format", "json"],
        ),
        "orna query Bool predicate scan",
    );
    assert_eq!(scanned["rows"], 1, "{scanned}");

    let bounded = orna(&root, &["query", relation.as_str(), "--from", "text:true", "--format", "json"]);
    assert!(
        !bounded.status.success() && transcript(&bounded).contains("ornadb-5il1.461"),
        "a bound on a Bool key is refused naming ornadb-5il1.461:\n{}",
        transcript(&bounded)
    );
    let keyed = orna(&root, &["query", relation.as_str(), "--key", "text:true", "--format", "json"]);
    assert!(
        !keyed.status.success() && transcript(&keyed).contains("ornadb-5il1.461"),
        "a --key lookup on a Bool key is refused naming ornadb-5il1.461:\n{}",
        transcript(&keyed)
    );
}
