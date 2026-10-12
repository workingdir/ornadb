use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, SchemaId, TypeId,
    catalogue::{
        CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName,
        SchemaDefinition,
    },
    semantic_diff::{
        CompactStorageObservation, DependencyChangeKind, DependencyEdge, KeyedRow,
        ResultChangeKind, ResultObservation, RowChangeKind, RowMutationIntent, RowRekey,
        SemanticEntityId, SemanticSnapshot, semantic_snapshot_diff,
    },
    types::{ResolvedType, StandardScalar},
};
use orna_syntax_v1::parse_module;
use orna_value_v1::{Raw, Value};

const BASE_SOURCE: &str = include_str!("fixtures/semantic-diff/base.orna");
const CANDIDATE_SOURCE: &str = include_str!("fixtures/semantic-diff/candidate.orna");
const ADDED_SCHEMA_SOURCE: &str = include_str!("fixtures/semantic-diff/archive.orna");
const REKEY_DELETE_REINSERT_SOURCE: &str =
    include_str!("fixtures/semantic-diff/rekey-delete-reinsert.orna");
const REKEY_REUSE_RETIRED_KEY_SOURCE: &str =
    include_str!("fixtures/semantic-diff/rekey-reuse-retired-key.orna");

const TABLE: TypeId = TypeId::from_bytes([0x21; 16]);
const STATUS_FIELD: FieldId = FieldId::from_bytes([0x23; 16]);
const ADDED_TYPE: TypeId = TypeId::from_bytes([0x25; 16]);
const REMOVED_TYPE: TypeId = TypeId::from_bytes([0x61; 16]);
const DESCRIPTION: FunctionId = FunctionId::from_bytes([0x31; 16]);

fn value(raw: Raw) -> Value {
    Value::new(raw).unwrap()
}

fn text(text: &str) -> Value {
    value(Raw::Text(text.to_owned()))
}

fn integer(number: i32) -> Value {
    value(Raw::Int(number.into()))
}

fn catalogue(revision: u8, include_status: bool) -> CatalogueSnapshot {
    let mut fields = vec![FieldDefinition::new(
        FieldId::from_bytes([0x22; 16]),
        "text",
        0,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        false,
        false,
        None,
        None,
    )];
    if include_status {
        fields.push(FieldDefinition::new(
            STATUS_FIELD,
            "status",
            1,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
            false,
            None,
            None,
        ));
    }

    let mut schemas = vec![SchemaDefinition::new(
        SchemaId::from_bytes([0x20; 16]),
        QualifiedSemanticName::new(["app"]).unwrap(),
    )];
    if include_status {
        schemas.push(SchemaDefinition::new(
            SchemaId::from_bytes([0x24; 16]),
            QualifiedSemanticName::new(["archive"]).unwrap(),
        ));
    }

    let mut object_types = vec![ObjectTypeDefinition::new(
        TABLE,
        QualifiedSemanticName::new(["app", "Note"]).unwrap(),
        fields,
    )];
    if include_status {
        object_types.push(ObjectTypeDefinition::new(
            ADDED_TYPE,
            QualifiedSemanticName::new(["archive", "Audit"]).unwrap(),
            vec![FieldDefinition::new(
                FieldId::from_bytes([0x26; 16]),
                "action",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
                false,
                None,
                None,
            )],
        ));
    }

    CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([revision; 16]),
        schemas,
        object_types,
    )
    .unwrap()
}

#[test]
fn semantic_diff_reports_catalogue_rows_results_dependencies_and_storage() {
    // Keep the source pair valid and checked in the crate; adapters supply its
    // resolved catalogue and persisted observations as typed facts.
    assert!(parse_module(BASE_SOURCE).is_ok());
    assert!(parse_module(CANDIDATE_SOURCE).is_ok());
    assert!(parse_module(ADDED_SCHEMA_SOURCE).is_ok());

    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [
            KeyedRow::new(TABLE, &integer(7), &text("draft")).unwrap(),
            KeyedRow::new(TABLE, &integer(9), &text("retired")).unwrap(),
        ],
        [
            ResultObservation::new(DESCRIPTION, &text(""), &text("draft")).unwrap(),
            ResultObservation::new(DESCRIPTION, &text("obsolete"), &text("old result")).unwrap(),
        ],
        [DependencyEdge::new(
            SemanticEntityId::Function(DESCRIPTION),
            SemanticEntityId::Type(REMOVED_TYPE),
        )],
        Some(CompactStorageObservation {
            generation: 4,
            representation_digest: Some([0x41; 32]),
        }),
    )
    .unwrap();
    let candidate = SemanticSnapshot::new(
        catalogue(2, true),
        [
            KeyedRow::new(TABLE, &integer(7), &text("published")).unwrap(),
            KeyedRow::new(TABLE, &integer(8), &text("new")).unwrap(),
        ],
        [ResultObservation::new(DESCRIPTION, &text(""), &text("published")).unwrap()],
        [DependencyEdge::new(
            SemanticEntityId::Function(DESCRIPTION),
            SemanticEntityId::Type(TABLE),
        )],
        Some(CompactStorageObservation {
            generation: 5,
            representation_digest: Some([0x42; 32]),
        }),
    )
    .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);

    assert!(report.catalogue().changes().iter().any(
        |change| matches!(change, orna_core::catalogue_diff::SemanticChange::FieldAdded { id, .. } if *id == STATUS_FIELD)
    ));
    assert!(report.catalogue().changes().iter().any(
        |change| matches!(change, orna_core::catalogue_diff::SemanticChange::SchemaAdded { name, .. } if name == "archive")
    ));
    assert!(report.catalogue().changes().iter().any(
        |change| matches!(change, orna_core::catalogue_diff::SemanticChange::ObjectTypeAdded { id, .. } if *id == ADDED_TYPE)
    ));
    assert_eq!(report.rows().len(), 3);
    assert!(report.rows().iter().any(|change| change.kind() == RowChangeKind::Updated));
    assert!(report.rows().iter().any(|change| change.kind() == RowChangeKind::Added));
    assert!(report.rows().iter().any(|change| change.kind() == RowChangeKind::Removed));
    assert_eq!(report.results().len(), 2);
    assert!(report.results().iter().any(|change| change.kind() == ResultChangeKind::Changed));
    assert!(report.results().iter().any(|change| change.kind() == ResultChangeKind::Removed));
    assert_eq!(report.dependencies().len(), 2);
    assert!(report.dependencies().iter().any(|change| change.kind() == DependencyChangeKind::Added));
    assert!(report.dependencies().iter().any(|change| change.kind() == DependencyChangeKind::Removed));
    assert_eq!(
        report.physical().unwrap().compact_generation,
        Some((4, 5))
    );
    assert_eq!(report.physical().unwrap().representation_changed, Some(true));
    assert!(report.physical().unwrap().logical_changes_present);
    assert!(!report.is_physical_only());
}

#[test]
fn semantic_diff_coalesces_only_an_explicit_rekey_intent() {
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [KeyedRow::new(TABLE, &integer(7), &text("same row")).unwrap()],
        [],
        [],
        None,
    )
    .unwrap();
    let candidate_rows = [KeyedRow::new(TABLE, &integer(8), &text("same row")).unwrap()];
    let candidate = SemanticSnapshot::new(catalogue(1, false), candidate_rows.clone(), [], [], None)
        .unwrap()
        .with_rekeys([RowRekey::new(TABLE, &integer(7), &integer(8)).unwrap()])
        .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);
    assert_eq!(report.rows().len(), 1);
    assert_eq!(report.rows()[0].kind(), RowChangeKind::Rekeyed);
    let old_key = integer(7).encode().unwrap();
    let new_key = integer(8).encode().unwrap();
    assert_eq!(report.rows()[0].key_bytes(), new_key);
    assert_eq!(report.rows()[0].previous_key_bytes(), Some(old_key.as_slice()));

    let without_intent = SemanticSnapshot::new(catalogue(1, false), candidate_rows, [], [], None)
        .unwrap();
    let conservative = semantic_snapshot_diff(&base, &without_intent);
    assert_eq!(conservative.rows().len(), 2);
    assert!(conservative.rows().iter().any(|row| row.kind() == RowChangeKind::Added));
    assert!(conservative.rows().iter().any(|row| row.kind() == RowChangeKind::Removed));
}

#[test]
fn semantic_diff_keeps_row_identity_through_a_rekey_tail_with_an_update() {
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [KeyedRow::new(TABLE, &integer(7), &text("before")).unwrap()],
        [],
        [],
        None,
    )
    .unwrap();
    let candidate = SemanticSnapshot::new(
        catalogue(1, false),
        [KeyedRow::new(TABLE, &integer(9), &text("after")).unwrap()],
        [],
        [],
        None,
    )
    .unwrap()
    .with_rekeys([
        RowRekey::new(TABLE, &integer(7), &integer(8)).unwrap(),
        RowRekey::new(TABLE, &integer(8), &integer(9)).unwrap(),
    ])
    .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);

    assert_eq!(report.rows().len(), 1);
    assert_eq!(report.rows()[0].kind(), RowChangeKind::Rekeyed);
    assert_eq!(report.rows()[0].previous_key_bytes(), Some(integer(7).encode().unwrap().as_slice()));
    assert_eq!(report.rows()[0].key_bytes(), integer(9).encode().unwrap());
}

#[test]
fn semantic_diff_uses_activation_order_when_a_rekeyed_key_is_reused() {
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [
            KeyedRow::new(TABLE, &integer(1), &text("first identity")).unwrap(),
            KeyedRow::new(TABLE, &integer(2), &text("second identity")).unwrap(),
        ],
        [],
        [],
        None,
    )
    .unwrap();
    let candidate = SemanticSnapshot::new(
        catalogue(1, false),
        [
            KeyedRow::new(TABLE, &integer(2), &text("first identity")).unwrap(),
            KeyedRow::new(TABLE, &integer(3), &text("second identity")).unwrap(),
        ],
        [],
        [],
        None,
    )
    .unwrap()
    // The first move frees key 2; only then can the first row claim it.
    .with_rekeys([
        RowRekey::new(TABLE, &integer(2), &integer(3)).unwrap(),
        RowRekey::new(TABLE, &integer(1), &integer(2)).unwrap(),
    ])
    .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);

    assert_eq!(report.rows().len(), 2);
    assert!(report.rows().iter().all(|row| row.kind() == RowChangeKind::Rekeyed));
    assert_eq!(report.rows()[0].previous_key_bytes(), Some(integer(1).encode().unwrap().as_slice()));
    assert_eq!(report.rows()[0].key_bytes(), integer(2).encode().unwrap());
    assert_eq!(report.rows()[1].previous_key_bytes(), Some(integer(2).encode().unwrap().as_slice()));
    assert_eq!(report.rows()[1].key_bytes(), integer(3).encode().unwrap());
}

#[test]
fn semantic_diff_tracks_rekey_identity_when_a_freed_destination_is_reused_again() {
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [
            KeyedRow::new(TABLE, &integer(1), &text("first identity")).unwrap(),
            KeyedRow::new(TABLE, &integer(4), &text("second identity")).unwrap(),
        ],
        [],
        [],
        None,
    )
    .unwrap();
    let candidate = SemanticSnapshot::new(
        catalogue(1, false),
        [
            KeyedRow::new(TABLE, &integer(2), &text("second identity")).unwrap(),
            KeyedRow::new(TABLE, &integer(3), &text("first identity")).unwrap(),
        ],
        [],
        [],
        None,
    )
    .unwrap()
    .with_rekeys([
        RowRekey::new(TABLE, &integer(1), &integer(2)).unwrap(),
        RowRekey::new(TABLE, &integer(2), &integer(3)).unwrap(),
        RowRekey::new(TABLE, &integer(4), &integer(2)).unwrap(),
    ])
    .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);

    assert_eq!(report.rows().len(), 2);
    assert_eq!(report.rows()[0].kind(), RowChangeKind::Rekeyed);
    assert_eq!(report.rows()[0].previous_key_bytes(), Some(integer(4).encode().unwrap().as_slice()));
    assert_eq!(report.rows()[0].key_bytes(), integer(2).encode().unwrap());
    assert_eq!(report.rows()[1].kind(), RowChangeKind::Rekeyed);
    assert_eq!(report.rows()[1].previous_key_bytes(), Some(integer(1).encode().unwrap().as_slice()));
    assert_eq!(report.rows()[1].key_bytes(), integer(3).encode().unwrap());
}

#[test]
fn semantic_diff_reports_insert_at_a_rekeyed_rows_former_key() {
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [KeyedRow::new(TABLE, &integer(1), &text("original")).unwrap()],
        [],
        [],
        None,
    )
    .unwrap();
    let candidate = SemanticSnapshot::new(
        catalogue(1, false),
        [
            KeyedRow::new(TABLE, &integer(1), &text("replacement")).unwrap(),
            KeyedRow::new(TABLE, &integer(2), &text("original")).unwrap(),
        ],
        [],
        [],
        None,
    )
    .unwrap()
    .with_row_mutations([
        RowMutationIntent::rekey(RowRekey::new(TABLE, &integer(1), &integer(2)).unwrap()),
        RowMutationIntent::insert(TABLE, &integer(1)).unwrap(),
    ])
    .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);

    assert_eq!(report.rows().len(), 2);
    assert_eq!(report.rows()[0].kind(), RowChangeKind::Added);
    assert_eq!(report.rows()[0].key_bytes(), integer(1).encode().unwrap());
    assert_eq!(report.rows()[1].kind(), RowChangeKind::Rekeyed);
    assert_eq!(report.rows()[1].previous_key_bytes(), Some(integer(1).encode().unwrap().as_slice()));
    assert_eq!(report.rows()[1].key_bytes(), integer(2).encode().unwrap());
}

#[test]
fn semantic_diff_drops_rekey_continuity_when_the_row_is_deleted_before_reinsert() {
    assert!(parse_module(REKEY_DELETE_REINSERT_SOURCE).is_ok());
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [KeyedRow::new(TABLE, &integer(1), &text("original")).unwrap()],
        [],
        [],
        None,
    )
    .unwrap();
    let candidate = SemanticSnapshot::new(
        catalogue(1, false),
        [KeyedRow::new(TABLE, &integer(1), &text("replacement")).unwrap()],
        [],
        [],
        None,
    )
    .unwrap()
    .with_row_mutations([
        RowMutationIntent::rekey(RowRekey::new(TABLE, &integer(1), &integer(2)).unwrap()),
        RowMutationIntent::delete(TABLE, &integer(2)).unwrap(),
        RowMutationIntent::insert(TABLE, &integer(1)).unwrap(),
    ])
    .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);

    assert_eq!(report.rows().len(), 2);
    assert_eq!(report.rows()[0].kind(), RowChangeKind::Removed);
    assert_eq!(report.rows()[0].key_bytes(), integer(1).encode().unwrap());
    assert_eq!(report.rows()[1].kind(), RowChangeKind::Added);
    assert_eq!(report.rows()[1].key_bytes(), integer(1).encode().unwrap());
}

#[test]
fn semantic_diff_keeps_retired_and_rekeyed_identities_distinct_at_a_reused_key() {
    assert!(parse_module(REKEY_REUSE_RETIRED_KEY_SOURCE).is_ok());
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [
            KeyedRow::new(TABLE, &integer(1), &text("retired identity")).unwrap(),
            KeyedRow::new(TABLE, &integer(2), &text("surviving identity")).unwrap(),
        ],
        [],
        [],
        None,
    )
    .unwrap();
    let candidate = SemanticSnapshot::new(
        catalogue(1, false),
        [KeyedRow::new(TABLE, &integer(1), &text("surviving identity")).unwrap()],
        [],
        [],
        None,
    )
    .unwrap()
    .with_row_mutations([
        RowMutationIntent::rekey(RowRekey::new(TABLE, &integer(1), &integer(3)).unwrap()),
        RowMutationIntent::rekey(RowRekey::new(TABLE, &integer(2), &integer(1)).unwrap()),
        RowMutationIntent::delete(TABLE, &integer(3)).unwrap(),
    ])
    .unwrap();

    let report = semantic_snapshot_diff(&base, &candidate);

    assert_eq!(report.rows().len(), 2);
    assert_eq!(report.rows()[0].kind(), RowChangeKind::Removed);
    assert_eq!(report.rows()[0].key_bytes(), integer(1).encode().unwrap());
    assert_eq!(report.rows()[1].kind(), RowChangeKind::Rekeyed);
    assert_eq!(report.rows()[1].key_bytes(), integer(1).encode().unwrap());
    assert_eq!(report.rows()[1].previous_key_bytes(), Some(integer(2).encode().unwrap().as_slice()));
}

#[test]
fn compact_generation_advance_without_logical_changes_is_physical_only() {
    let base = SemanticSnapshot::new(
        catalogue(1, false),
        [],
        [],
        [],
        Some(CompactStorageObservation {
            generation: 10,
            representation_digest: Some([0x51; 32]),
        }),
    )
    .unwrap();
    let compacted = SemanticSnapshot::new(
        catalogue(2, false),
        [],
        [],
        [],
        Some(CompactStorageObservation {
            generation: 11,
            representation_digest: Some([0x52; 32]),
        }),
    )
    .unwrap();

    let report = semantic_snapshot_diff(&base, &compacted);

    assert!(!report.has_logical_changes());
    assert_eq!(
        report.physical().unwrap().compact_generation,
        Some((10, 11))
    );
    assert!(report.is_physical_only());
}
