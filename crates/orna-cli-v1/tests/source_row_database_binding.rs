use std::sync::Arc;

use orna_project_v1::{ProjectLoadError, ProjectLoader, admit_source_row};
use orna_repository_v1::GitCommitRef;
use orna_runtime_v1::TableObjectId;
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

fn foreign_database_context(commit: &GitCommitRef, database_id: [u8; 16]) -> Format3Context {
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
    let (_directory, repository, relation_id) = format3::empty_format3_repository();
    let commit = repository.head().unwrap().unwrap();
    let loader = ProjectLoader::default();
    let project = loader
        .load_committed_snapshot(&repository, &commit)
        .unwrap();
    let schema = loader
        .resolve_committed_table_schema(
            &repository,
            &commit,
            "media",
            TableObjectId::new(relation_id),
        )
        .unwrap();

    let mut foreign_database_id = *schema.database_id();
    foreign_database_id[15] ^= 1;
    let foreign_context = foreign_database_context(&commit, foreign_database_id);
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
        "{ content: null, }\n",
        &schema,
        &foreign_context,
        Vec::new(),
        [0x77; 16],
    );
    assert!(matches!(
        result,
        Err(ProjectLoadError::InvalidSourceRow)
    ));
}
