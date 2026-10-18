use futures::executor::block_on;
use orna_application_v1::{ApplicationAuthority, CommittedTableIdentity};
use orna_evaluator_v1::{Environment, Limits, QueryReuseDecision, SysHostBindingRegistry};
use orna_foundation_v1::OvbRaw;
use orna_runtime_v1::{RuntimeState, TableObjectId};
use orna_semantic_v1::Catalogue;
use orna_sys_v1::EnvironmentProvider;
use std::fs;

#[path = "../../orna-cli-v1/tests/support/format3.rs"]
mod format3;
#[path = "../../orna-cli-v1/tests/support/media_commit.rs"]
mod media_commit;

use media_commit::SourceCaptureRow;

const SOURCE: &str = r#"
pub table note(id: Str) { value: Str, }
pub fn main() = note.as_of(sys.snapshot("HEAD~10")) | one();
"#;

#[test]
fn committed_relation_read_returns_row_from_pre_mutation_commit() {
    let (directory, repository, relation_id, context) =
        format3::format3_repository_for_table_with_source_row(
            SOURCE,
            "note",
            "note/row.orna",
            "{ value: \"before\", }\n",
        );
    let root = directory.path();
    let (identity, checkpoint_key) = format3::cli_runtime_identity(&repository);
    let state = block_on(RuntimeState::open(&repository, identity, checkpoint_key))
        .expect("runtime opens for format-3 publication");
    let writer = block_on(state.acquire_lease([0x31; 16])).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    let before_capture = [SourceCaptureRow {
        key: "row",
        blob_fields: &[],
        source_row: None,
        ordinal: 0x40,
        insert_only: true,
    }];
    block_on(media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &context,
        "note",
        &before_capture,
    ));

    fs::write(root.join("note/row.orna"), "{ value: \"after\", }\n").unwrap();
    format3::git(root, &["add", "note/row.orna"]);
    format3::git(root, &["commit", "--quiet", "-m", "mutate source row"]);
    let current_commit = repository.head().unwrap().unwrap().to_string();
    let updated_context = format3::format3_context_for_snapshot(&repository, &current_commit);
    let after_capture = [SourceCaptureRow {
        key: "row",
        blob_fields: &[],
        source_row: Some("{ value: \"after\", }\n"),
        ordinal: 0x50,
        insert_only: false,
    }];
    block_on(media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &updated_context,
        "note",
        &after_capture,
    ));

    // One source-mutation commit plus nine empty commits pins the original row at HEAD~10.
    for _ in 0..9 {
        format3::git(
            root,
            &["commit", "--allow-empty", "--quiet", "-m", "advance history"],
        );
    }

    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module_with_committed_table_metadata(
            "main.orna",
            SOURCE,
            "main",
            [CommittedTableIdentity::new("note", TableObjectId::new(relation_id))],
        )
        .expect("source table identity is admitted");
    let result = authority
        .evaluate_staged_with_repository(&application, &Environment::new(), &repository)
        .expect("historical relation query succeeds");

    let OvbRaw::Map(fields) = result.value().raw() else {
        panic!("first historical row must be a record: {:?}", result.value());
    };
    assert!(fields.iter().any(|(name, value)| {
        name == &OvbRaw::Text("value".to_owned())
            && value == &OvbRaw::Text("before".to_owned())
    }), "expected the row as committed before mutation, got {:?}", result.value());
}

#[test]
fn staged_activation_exposes_query_reuse_observations_to_the_host() {
    let source =
        "fn increment(value: Int) = value + 1; pub fn main() = increment(4) + increment(4);";
    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module("main.orna", source, "main")
        .expect("pure scalar source is admitted");
    let activation = authority
        .evaluate_staged(&application, &Environment::new())
        .expect("staged evaluation succeeds");

    let observations = activation.query_reuse_observations();
    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].function(), "increment");
    assert_eq!(observations[0].decision(), QueryReuseDecision::ComputedAndStored);
    assert_eq!(observations[1].function(), "increment");
    assert_eq!(observations[1].decision(), QueryReuseDecision::Reused);
}
