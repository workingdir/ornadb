//! Schema evolution of a database the product itself created
//! (ORNA-EVOL-001, ORNA-EVOL-002, ORNA-EVOL-003, ORNA-EVOL-009,
//! ORNA-FIELD-001, ORNA-FIELD-003, ORNA-FIELD-005, ORNA-OBJECT-003,
//! ORNA-OBJECT-004): `orna init`, declare a table, `orna commit`, insert rows
//! in the REPL, `orna publish`, change the schema (rename a field with
//! `orna mv`, add an optional and a defaulted field, remove one, widen a
//! type, add a table), `orna commit`, insert rows under the new schema,
//! `orna publish`, and read the old and new rows back.

#[path = "support/product.rs"]
mod product;

use std::fs;

use orna_value_v1::Raw;
use product::*;
use tempfile::TempDir;

const DECLARED: &str = "pub table Music(id: Str) {
    title: Str,
    plays: Int,
    rating: Int,
}
";

const EVOLVED: &str = "pub table Music(id: Str) {
    name: Str,
    plays: Decimal,
    country: Str = \"GB\",
    note: Str?,
}

pub table Venue(id: Str) {
    city: Str,
}
";

fn row<'a>(rows: &'a [StoredRow], key: &str) -> &'a StoredRow {
    rows.iter()
        .find(|row| row.key == orna_repository_v1::TypedKey::Text(key.to_owned()))
        .unwrap_or_else(|| panic!("row {key} is committed: {rows:?}"))
}

fn fields(values: &[(&str, Raw)]) -> std::collections::BTreeMap<String, Raw> {
    values
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

#[test]
fn a_schema_change_publishes_and_keeps_old_and_new_rows_readable() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, DECLARED);
    let music = admitted_relations(&commit_source(&root, "Declare Music"))["Music"].clone();
    repl_writes(
        &root,
        concat!(
            "main.Music.insert({ id: \"london\", title: \"Live in London\", plays: 70, rating: 5 })\n",
            "main.Music.insert({ id: \"paris\", title: \"Live in Paris\", plays: 3, rating: 4 })",
        ),
    );
    publish(&root, "Add two recordings");
    let before = head(&root);

    // A rename made by hand cannot be told from a deletion plus a new field
    // (ORNA-OBJECT-004); the new required field has no value for the rows
    // that exist (ORNA-FIELD-005), so nothing is committed or staged.
    fs::write(root.join("main.orna"), DECLARED.replace("title", "name")).unwrap();
    git(&root, &["add", "main.orna"]);
    let index_before = git(&root, &["ls-files", "--stage", ".orna/store"]);
    let refused = orna(&root, &["commit", "-m", "Rename by hand"]);
    assert_ne!(refused.status.code(), Some(0), "{}", transcript(&refused));
    let message = transcript(&refused);
    assert!(
        message.contains("does not fit the committed rows") && message.contains("`name`"),
        "{message}"
    );
    assert_eq!(head(&root), before);
    assert_eq!(git(&root, &["ls-files", "--stage", ".orna/store"]), index_before);
    git(&root, &["checkout", "HEAD", "--", "main.orna"]);

    // `orna mv` keeps the field's ObjectId: it renames the declaration and
    // stages the identity move, committing nothing (ORNA-OBJECT-003).
    let moved = orna(&root, &["mv", "Music.title", "name"]);
    assert_success(&moved, "orna mv");
    assert!(
        String::from_utf8_lossy(&moved.stdout).contains("renamed Music.title to Music.name"),
        "{}",
        transcript(&moved)
    );
    assert_eq!(
        fs::read_to_string(root.join("main.orna")).unwrap(),
        DECLARED.replace("title", "name")
    );
    assert_eq!(head(&root), before);

    // The rest of the change: a widened type, a defaulted and an optional
    // field, a removed field and a new table, committed together.
    fs::write(root.join("main.orna"), EVOLVED).unwrap();
    let evolved = commit_source(&root, "Evolve Music");
    let line = evolved
        .lines()
        .find(|line| line.starts_with("evolved table Music ("))
        .unwrap_or_else(|| panic!("orna commit reports the evolution:\n{evolved}"));
    for change in [
        "renamed title to name",
        "removed rating",
        "converted plays",
        "country",
        "note",
    ] {
        assert!(line.contains(change), "{line}");
    }
    assert!(line.ends_with("): 2 rows rewritten"), "{line}");
    let venue = admitted_relations(&evolved)["Venue"].clone();
    let evolved_commit = head(&root);

    // The committed rows were rewritten by field identity in the commit that
    // changed the schema: the renamed field kept its value, the new required
    // field holds its constant default frozen at this commit (ORNA-FIELD-003,
    // ORNA-EVOL-009), the optional one is absent, and Int became Decimal.
    let rows = stored_rows(&root, &evolved_commit, relation_bytes(&music));
    assert_eq!(rows.len(), 2);
    assert_eq!(
        row(&rows, "london").fields,
        fields(&[
            ("name", text("Live in London")),
            ("plays", decimal(7, 1)),
            ("country", text("GB")),
            ("note", option(None)),
        ])
    );
    assert_eq!(
        row(&rows, "paris").fields,
        fields(&[
            ("name", text("Live in Paris")),
            ("plays", decimal(3, 0)),
            ("country", text("GB")),
            ("note", option(None)),
        ])
    );

    // New rows use the new schema; an omitted defaulted field stores its
    // default evaluated once at insertion (ORNA-FIELD-001).
    repl_writes(
        &root,
        concat!(
            "main.Music.insert({ id: \"tokyo\", name: \"Live in Tokyo\", plays: 1.5, note: \"encore\" })\n",
            "main.Venue.insert({ id: \"budokan\", city: \"Tokyo\" })",
        ),
    );
    let report = publish(&root, "Add Tokyo");
    assert!(report.contains("Music  1 rows") && report.contains("Venue  1 rows"), "{report}");
    let rows = stored_rows(&root, &head(&root), relation_bytes(&music));
    assert_eq!(rows.len(), 3);
    assert_eq!(
        row(&rows, "tokyo").fields,
        fields(&[
            ("name", text("Live in Tokyo")),
            ("plays", decimal(15, -1)),
            ("country", text("GB")),
            ("note", option(Some(text("encore")))),
        ])
    );
    assert_eq!(row(&rows, "london").fields["name"], text("Live in London"));
    assert_eq!(query_rows(&root, &["query", &music, "--format", "json"]), 3);
    assert_eq!(
        query_rows(&root, &["query", &music, "--key", "london", "--format", "json"]),
        1
    );
    assert_eq!(query_rows(&root, &["query", &venue, "--format", "json"]), 1);

    // Each snapshot keeps decoding under its own schema (ORNA-EVOL-003): the
    // published old rows still read as they were, and history compares the
    // two endpoints under their own descriptors.
    let old_rows = stored_rows(&root, &before, relation_bytes(&music));
    assert_eq!(
        row(&old_rows, "london").fields,
        fields(&[
            ("title", text("Live in London")),
            ("plays", Raw::Int(70.into())),
            ("rating", Raw::Int(5.into())),
        ])
    );
    let history = orna(
        &root,
        &["history", &music, "--diff", &before, "HEAD", "--format", "json"],
    );
    assert_success(&history, "orna history --diff");
    let diff: serde_json::Value =
        serde_json::from_slice(&history.stdout).expect("orna history prints JSON");
    assert_eq!(diff["added"], 1, "{diff}");
    assert_eq!(diff["changed"], 2, "{diff}");
    assert_eq!(diff["removed"], 0, "{diff}");
}

#[test]
fn schema_changes_wait_for_unpublished_rows_and_refuse_rows_that_do_not_fit() {
    const TASKS: &str = "pub table Task(id: Str) {\n    title: Str,\n}\n";
    const OPTIONAL_DONE: &str = "pub table Task(id: Str) {\n    title: Str,\n    done: Bool?,\n}\n";
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, TASKS);
    let task = admitted_relations(&commit_source(&root, "Declare Task"))["Task"].clone();
    repl_writes(&root, "main.Task.insert({ id: \"write\", title: \"Write the report\" })");

    // The row waits in the runtime tail, encoded under the committed schema,
    // so the schema may not change under it.
    fs::write(root.join("main.orna"), OPTIONAL_DONE).unwrap();
    git(&root, &["add", "main.orna"]);
    let declared = head(&root);
    let waiting = orna(&root, &["commit", "-m", "Add done"]);
    assert_ne!(waiting.status.code(), Some(0), "{}", transcript(&waiting));
    assert!(
        transcript(&waiting).contains("waits for unpublished rows")
            && transcript(&waiting).contains("orna publish"),
        "{}",
        transcript(&waiting)
    );
    assert_eq!(head(&root), declared);

    // Published first, the change commits and the row gains an absent field.
    publish(&root, "Add a task");
    let evolved = commit_source(&root, "Add done");
    assert!(
        evolved.contains("evolved table Task (added done): 1 rows rewritten"),
        "{evolved}"
    );
    let rows = stored_rows(&root, &head(&root), relation_bytes(&task));
    assert_eq!(
        rows[0].fields,
        fields(&[("title", text("Write the report")), ("done", option(None))])
    );

    // Making the field required has no value for the committed row.
    fs::write(root.join("main.orna"), OPTIONAL_DONE.replace("Bool?", "Bool")).unwrap();
    git(&root, &["add", "main.orna"]);
    let committed = head(&root);
    let unfit = orna(&root, &["commit", "-m", "Require done"]);
    assert_ne!(unfit.status.code(), Some(0), "{}", transcript(&unfit));
    assert!(
        transcript(&unfit).contains("does not fit the committed rows")
            && transcript(&unfit).contains("`done`"),
        "{}",
        transcript(&unfit)
    );
    assert_eq!(head(&root), committed);
    git(&root, &["checkout", "HEAD", "--", "main.orna"]);

    // Removing the declaration removes the relation and its rows from the
    // committed store; history keeps them.
    fs::write(root.join("main.orna"), "pub table Other(id: Str) {\n    note: Str,\n}\n").unwrap();
    let removed = commit_source(&root, "Replace Task");
    assert!(
        removed.contains("removed table Task with 1 rows"),
        "{removed}"
    );
    let listed = orna(&root, &["query", &task, "--format", "json"]);
    assert_ne!(listed.status.code(), Some(0), "{}", transcript(&listed));
    assert_eq!(stored_rows(&root, &committed, relation_bytes(&task)).len(), 1);
}
