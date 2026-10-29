use std::sync::Arc;

use orna_project_v1::{ProjectLoadError, ProjectLoader, admit_source_row};
use orna_repository_v1::GitCommitRef;
use orna_value_v1::{
    AvailabilityState, BlobResolver, ContentReference, Error as ValueError, Format3Context,
    Format3Quota, GitHash, NativeOid, OwnerLifetime, Snapshot,
};

#[path = "support/format3.rs"]
mod format3;

struct UnusedResolver;

impl BlobResolver for UnusedResolver {
    fn read_range(
        &self,
        _reference: &ContentReference,
        _offset: u64,
        _length: u64,
    ) -> orna_value_v1::Result<Vec<u8>> {
        Err(ValueError::ContentUnavailable)
    }
}

fn format3_context_for_database(commit: &GitCommitRef, database_id: [u8; 16]) -> Format3Context {
    let oid = commit
        .as_str()
        .as_bytes()
        .chunks_exact(2)
        .map(|digits| {
            let digits = std::str::from_utf8(digits).unwrap();
            u8::from_str_radix(digits, 16).unwrap()
        })
        .collect::<Vec<_>>();
    let algorithm = match oid.len() {
        20 => GitHash::Sha1,
        32 => GitHash::Sha256,
        _ => panic!("unexpected Git object identifier width"),
    };
    let root = NativeOid::from_bytes(&vec![0x55; algorithm.oid_width()]).unwrap();
    Format3Context::from_persisted(
        database_id,
        Snapshot::Commit {
            database: database_id,
            algorithm,
            oid,
        },
        algorithm,
        root.clone(),
        root,
        AvailabilityState::Available,
        OwnerLifetime::new([0x66; 16]),
        Format3Quota::new(1, 0).unwrap(),
        Arc::new(UnusedResolver),
    )
    .unwrap()
}

#[test]
fn same_commit_context_from_another_database_cannot_admit_source_row() {
    let (_directory, repository, relation_id) = format3::format3_repository(
        "pub table media(id: Str) { content: Str, }\n",
    );
    let commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader
        .load_committed_snapshot(&repository, &commit)
        .unwrap();
    let schema = loader
        .resolve_committed_source_table_schema(
            &repository,
            &commit,
            orna_runtime_v1::TableObjectId::new(relation_id),
        )
        .unwrap();
    assert_eq!(schema.binding().module_name(), Some(""));
    assert_eq!(schema.binding().source_path(), Some("main.orna"));
    assert_eq!(schema.binding().declaration_name(), Some("media"));
    assert_eq!(schema.binding().database_id().as_bytes(), schema.database_id());

    let row_source = "{ content: \"picture\", }\n";
    let matching_context = format3_context_for_database(&commit, *schema.database_id());
    let accepted = admit_source_row(
        &project,
        "media/picture.orna",
        row_source,
        &schema,
        &matching_context,
        Vec::new(),
        [0x76; 16],
    );
    let accepted = accepted.expect("matching repository context should admit the full tuple");
    assert_eq!(accepted.resolved_schema(), &schema);
    let (_mutation, bindings, publication_schema) = accepted.into_mutation_and_bindings();
    assert!(bindings.is_empty());
    assert_eq!(&publication_schema, &schema);

    let mut foreign_database_id = *schema.database_id();
    foreign_database_id[15] ^= 1;
    let foreign_context = format3_context_for_database(&commit, foreign_database_id);
    assert!(matches!(
        foreign_context.pin(),
        Snapshot::Commit {
            database,
            oid,
            ..
        } if *database == foreign_database_id && oid.len() == commit.as_str().len() / 2
    ));

    let result = admit_source_row(
        &project,
        "media/picture.orna",
        row_source,
        &schema,
        &foreign_context,
        Vec::new(),
        [0x77; 16],
    );
    assert!(matches!(result, Err(ProjectLoadError::InvalidSourceRow)));
}

/// profiles/vfs-1.md:71-74 and Orna-1.1.0.md:4015: a nested record row whose declared
/// order (`label`, `rank`) differs from encoded field-key order (`rank`, `label`) is
/// admitted, with its stored fields in encoded-key order.
#[test]
fn nested_record_row_with_declaration_order_differing_from_key_order_is_admitted() {
    let (_directory, repository, relation_id) = format3::format3_repository_for_table(
        "pub table Music(id: Str) { meta: { label: Str, rank: Int, }, }\n",
        "Music",
    );
    let commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader
        .load_committed_snapshot(&repository, &commit)
        .unwrap();
    let schema = loader
        .resolve_committed_source_table_schema(
            &repository,
            &commit,
            orna_runtime_v1::TableObjectId::new(relation_id),
        )
        .unwrap();
    let context = format3_context_for_database(&commit, *schema.database_id());
    let row_source = "{ meta: { label: \"x\", rank: 1, }, }\n";
    let accepted = admit_source_row(
        &project,
        "Music/london.orna",
        row_source,
        &schema,
        &context,
        Vec::new(),
        [0x78; 16],
    );
    assert!(
        accepted.is_ok(),
        "a nested record row must be admitted in encoded-key order"
    );
}
