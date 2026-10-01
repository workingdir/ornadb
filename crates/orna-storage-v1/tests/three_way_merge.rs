use orna_evolution_v1::{
    CanonicalValue, CheckpointGeneration, Field, FieldRole, FieldType, KeyedRow, ObjectId,
    RowKeyKind, Schema, Table,
};
use orna_foundation_v1::OvbRaw;
use orna_storage_v1::{
    BranchMergeBudget, BranchMergeConflict, BranchMergeError, BranchRowSource, KeyRange,
    MergeSide, MergedSegment, RowSegmentManifest, TableManifest, ThreeWaySnapshot,
    merge_three_way_snapshots,
};
use orna_syntax_v1::{Expr, LiteralKind, parse_row};
use std::collections::BTreeMap;

const BASE: &str = include_str!("fixtures/merge-contact-base.orna");
const LEFT: &str = include_str!("fixtures/merge-contact-left.orna");
const RIGHT: &str = include_str!("fixtures/merge-contact-right.orna");
const CONFLICT: &str = include_str!("fixtures/merge-contact-conflict.orna");
const CHECKPOINT_POSITIONLESS: &str = include_str!("fixtures/merge-checkpoint-positionless.orna");
const CHECKPOINT_POSITIONLESS_EDITED: &str = include_str!("fixtures/merge-checkpoint-positionless-edited.orna");
const CHECKPOINT_BASE: &str = include_str!("fixtures/merge-checkpoint-base.orna");
const CHECKPOINT_EDITED: &str = include_str!("fixtures/merge-checkpoint-edited.orna");
const CHECKPOINT_RESET: &str = include_str!("fixtures/merge-checkpoint-reset.orna");
const CHECKPOINT_TAIL_BASE: &str = include_str!("fixtures/merge-checkpoint-tail-base.orna");
const CHECKPOINT_TAIL_LEFT: &str = include_str!("fixtures/merge-checkpoint-tail-left.orna");
const CHECKPOINT_TAIL_RIGHT: &str = include_str!("fixtures/merge-checkpoint-tail-right.orna");

fn id(value: u8) -> ObjectId {
    ObjectId::new([value; 16])
}

fn string(value: &str) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Text(value.to_owned())).unwrap()
}

fn integer(value: i64) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Int(value.into())).unwrap()
}

fn schema(explicit_key: bool, name_type: FieldType) -> Schema {
    Schema {
        version: orna_evolution_v1::EvolutionVersion::V1_0,
        tables: vec![Table {
            id: id(1),
            name: "Contact".into(),
            explicit_key,
            fields: vec![
                Field { id: id(4), name: "id".into(), ty: FieldType::Int, role: FieldRole::Key, optional: false, introduction_fallback: None },
                Field { id: id(2), name: "name".into(), ty: name_type, role: FieldRole::Stored, optional: false, introduction_fallback: None },
                Field { id: id(3), name: "city".into(), ty: FieldType::Str, role: FieldRole::Stored, optional: false, introduction_fallback: None },
            ],
        }],
    }
}

fn parse_fixture(source: &str, kind: RowKeyKind) -> KeyedRow {
    let parsed = parse_row(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let Expr::Record { fields, .. } = parsed.value else { panic!("record fixture") };
    let mut key = None;
    let mut values = BTreeMap::new();
    for field in fields {
        let Expr::Literal { text, kind: literal_kind, .. } = field.value else { panic!("literal fixture") };
        match field.name.as_str() {
            "id" => {
                assert_eq!(literal_kind, LiteralKind::Integer);
                let number: i64 = text.parse().unwrap();
                key = Some(CanonicalValue::new(OvbRaw::Int(number.into())).unwrap());
            }
            "name" | "city" => {
                assert_eq!(literal_kind, LiteralKind::String);
                let value = text.strip_prefix('"').unwrap().strip_suffix('"').unwrap();
                let field_id = if field.name == "name" { id(2) } else { id(3) };
                values.insert(field_id, string(value));
            }
            other => panic!("unexpected fixture field {other}"),
        }
    }
    KeyedRow { table: id(1), key: key.unwrap(), key_kind: kind, fields: values }
}

fn parse_checkpoint_fixture(source: &str) -> CheckpointGeneration {
    let parsed = parse_row(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let Expr::Record { fields, .. } = parsed.value else { panic!("checkpoint fixture must be a record") };
    let mut generation = None;
    let mut position = None;
    for field in fields {
        let Expr::Literal { text, kind, .. } = field.value else { panic!("checkpoint fields must be literals") };
        match field.name.as_str() {
            "generation" => {
                assert_eq!(kind, LiteralKind::Integer);
                generation = Some(text.parse::<u64>().unwrap());
            }
            "position" => {
                assert_eq!(kind, LiteralKind::String);
                position = Some(
                    text.strip_prefix('\"').unwrap().strip_suffix('\"').unwrap().as_bytes().to_vec(),
                );
            }
            other => panic!("unexpected checkpoint fixture field {other}"),
        }
    }
    CheckpointGeneration { generation: generation.expect("fixture generation"), position }
}

fn manifest(digest: u8, segment_digest: u8, locator: &[u8]) -> TableManifest {
    TableManifest {
        digest: [digest; 32],
        segments: vec![RowSegmentManifest { locator: locator.to_vec(), range: KeyRange::all(), digest: [segment_digest; 32] }],
    }
}

fn snapshot(schema: Schema, manifest: TableManifest, checkpoint: Option<CheckpointGeneration>) -> ThreeWaySnapshot {
    let mut tables = BTreeMap::new();
    tables.insert(id(1), manifest);
    let mut checkpoints = BTreeMap::new();
    if let Some(checkpoint) = checkpoint { checkpoints.insert(b"consumer/source".to_vec(), checkpoint); }
    ThreeWaySnapshot { schema, tables, checkpoints }
}

#[derive(Default)]
struct FixtureRows {
    rows: BTreeMap<(MergeSide, Vec<u8>), Vec<KeyedRow>>,
    visited: Vec<(MergeSide, Vec<u8>)>,
}

impl FixtureRows {
    fn add(&mut self, side: MergeSide, locator: &[u8], rows: Vec<KeyedRow>) {
        self.rows.insert((side, locator.to_vec()), rows);
    }
}

impl BranchRowSource for FixtureRows {
    fn visit_rows(
        &mut self,
        side: MergeSide,
        table: ObjectId,
        segment: Option<&RowSegmentManifest>,
        range: &KeyRange,
        visitor: &mut dyn FnMut(KeyedRow) -> bool,
    ) -> Result<(), String> {
        let locator = segment.map(|segment| segment.locator.clone()).unwrap_or_default();
        self.visited.push((side, locator.clone()));
        for row in self.rows.get(&(side, locator)).into_iter().flatten() {
            assert_eq!(row.table, table);
            let key = row.key.encode().map_err(|error| error.to_string())?;
            if range.start.as_deref().is_none_or(|start| key.as_slice() >= start)
                && range.end.as_deref().is_none_or(|end| key.as_slice() < end)
                && !visitor(row.clone())
            {
                break;
            }
        }
        Ok(())
    }
}

fn budget() -> BranchMergeBudget {
    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 20 }
}

fn row_checkpoint_conflict_inputs() -> (
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    FixtureRows,
) {
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(
        schema(true, FieldType::Str),
        manifest(1, 1, b"base"),
        Some(base_checkpoint),
    );
    let left = snapshot(
        schema(true, FieldType::Str),
        manifest(2, 2, b"left"),
        Some(left_checkpoint),
    );
    let right = snapshot(
        schema(true, FieldType::Str),
        manifest(3, 3, b"right"),
        Some(right_checkpoint),
    );
    (base, left, right, source)
}

fn schema_conflict_inputs() -> (
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    FixtureRows,
) {
    let base_schema = schema(true, FieldType::Str);
    let mut left_schema = base_schema.clone();
    left_schema.tables[0].fields[1].ty = FieldType::Int;
    left_schema.tables[0].fields[2].ty = FieldType::Int;
    let mut right_schema = base_schema.clone();
    right_schema.tables[0].fields[1].ty = FieldType::Bool;
    right_schema.tables[0].fields[2].ty = FieldType::Bool;
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(base_schema, manifest(1, 1, b"base"), Some(base_checkpoint));
    let left = snapshot(left_schema, manifest(2, 2, b"left"), Some(left_checkpoint));
    let right = snapshot(right_schema, manifest(3, 3, b"right"), Some(right_checkpoint));
    (base, left, right, source)
}

#[test]
fn independent_edits_to_one_keyed_row_merge_by_field_from_orna_fixtures() {
    let base_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let left_row = parse_fixture(LEFT, RowKeyKind::Explicit);
    let right_row = parse_fixture(RIGHT, RowKeyKind::Explicit);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![base_row]);
    source.add(MergeSide::Left, b"left", vec![left_row]);
    source.add(MergeSide::Right, b"right", vec![right_row]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    let MergedSegment::Rows { rows, .. } = &plan.tables[&id(1)].segments[0] else { panic!("changed range is rebuilt") };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fields[&id(2)], string("Grace"));
    assert_eq!(rows[0].fields[&id(3)], string("Paris"));
}

#[test]
fn same_field_conflict_is_reported_without_returning_a_partial_plan() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else { panic!("bounded semantic conflict") };
    assert!(matches!(conflicts.as_slice(), [BranchMergeConflict::Row { .. }]));
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
}

#[test]
fn equal_segment_digest_skips_unchanged_range_before_row_decode() {
    let row = parse_fixture(BASE, RowKeyKind::Explicit);
    let key = row.key.encode().unwrap();
    let low = RowSegmentManifest { locator: b"low".to_vec(), range: KeyRange::new(None, Some(key.clone())).unwrap(), digest: [7; 32] };
    let changed_base = RowSegmentManifest { locator: b"base-high".to_vec(), range: KeyRange::new(Some(key.clone()), None).unwrap(), digest: [1; 32] };
    let changed_left = RowSegmentManifest { locator: b"left-high".to_vec(), digest: [2; 32], ..changed_base.clone() };
    let changed_right = RowSegmentManifest { locator: b"right-high".to_vec(), digest: [3; 32], ..changed_base.clone() };
    let table_manifest = |digest, high| TableManifest { digest: [digest; 32], segments: vec![low.clone(), high] };
    let base = snapshot(schema(true, FieldType::Str), table_manifest(10, changed_base), None);
    let left = snapshot(schema(true, FieldType::Str), table_manifest(11, changed_left), None);
    let right = snapshot(schema(true, FieldType::Str), table_manifest(12, changed_right), None);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base-high", vec![row.clone()]);
    source.add(MergeSide::Left, b"left-high", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right-high", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);

    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    assert_eq!(source.visited.len(), 3, "the equal low segment must not be decoded");
    assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"high")));
    assert!(matches!(plan.tables[&id(1)].segments[0], MergedSegment::Reuse { from: MergeSide::Left, .. }));
}

#[test]
fn conflict_budget_stops_at_lower_bound_and_names_affected_range() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);
    let zero_conflicts = BranchMergeBudget { max_rows_examined: 10, max_conflicts: 0 };

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, zero_conflicts).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else { panic!("must stop at conflict budget") };
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
}

#[test]
fn row_budget_stops_streaming_decode_and_reports_the_active_range() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);
    let no_rows = BranchMergeBudget { max_rows_examined: 0, max_conflicts: 20 };

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, no_rows).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else { panic!("must stop at row budget") };
    assert_eq!(report.rows_examined, 1, "the reader is stopped at the first row over budget");
    assert_eq!(report.conflicts_lower_bound, 0);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
}

#[test]
fn optional_columns_added_on_separate_branches_merge_and_type_edits_conflict() {
    let mut base_schema = schema(true, FieldType::Str);
    base_schema.tables[0].fields.pop();
    let mut left_schema = base_schema.clone();
    left_schema.tables[0].fields.push(Field { id: id(5), name: "alias".into(), ty: FieldType::Str, role: FieldRole::Stored, optional: true, introduction_fallback: None });
    let mut right_schema = base_schema.clone();
    right_schema.tables[0].fields.push(Field { id: id(6), name: "region".into(), ty: FieldType::Str, role: FieldRole::Stored, optional: true, introduction_fallback: None });
    let base = snapshot(base_schema.clone(), manifest(1, 1, b"base"), None);
    let left = snapshot(left_schema, manifest(1, 1, b"base"), None);
    let right = snapshot(right_schema, manifest(1, 1, b"base"), None);
    let mut source = FixtureRows::default();
    let merged = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    let fields = &merged.schema.tables[0].fields;
    assert!(fields.iter().any(|field| field.id == id(5)));
    assert!(fields.iter().any(|field| field.id == id(6)));

    let mut type_base = schema(true, FieldType::Str);
    type_base.tables[0].fields.pop();
    let mut type_left = type_base.clone();
    type_left.tables[0].fields[1].ty = FieldType::Int;
    let mut type_right = type_base.clone();
    type_right.tables[0].fields[1].ty = FieldType::Bool;
    let base = snapshot(type_base, manifest(1, 1, b"base"), None);
    let left = snapshot(type_left, manifest(1, 1, b"base"), None);
    let right = snapshot(type_right, manifest(1, 1, b"base"), None);
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(error, BranchMergeError::Conflicts { conflicts, .. } if conflicts.iter().any(|conflict| matches!(conflict, BranchMergeConflict::Schema(_)))));
}

#[test]
fn schema_conflict_budget_boundary_is_exact_and_stops_later_phases() {
    for (max_conflicts, lower_bound) in [(0, 1), (1, 2)] {
        let (base, left, right, mut source) = schema_conflict_inputs();
        let limit = BranchMergeBudget { max_rows_examined: 100, max_conflicts };
        let error = merge_three_way_snapshots(&base, &left, &right, &mut source, limit)
            .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("schema conflicts beyond the configured budget stop planning")
        };
        assert_eq!(report.conflicts_lower_bound, lower_bound);
        assert!(report.affected_tables.contains(&id(1)));
        assert!(report.affected_ranges.is_empty());
        assert!(report.affected_checkpoints.is_empty());
        assert!(source.visited.is_empty());
    }

    let (base, left, right, mut source) = schema_conflict_inputs();
    let exact_limit = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_limit)
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("schema conflicts exactly at the budget remain typed conflicts")
    };
    assert_eq!(conflicts.len(), 2);
    assert!(conflicts.iter().all(|conflict| matches!(conflict, BranchMergeConflict::Schema(_))));
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_ranges.is_empty());
    assert!(report.affected_checkpoints.is_empty());
    assert!(source.visited.is_empty());
}

#[test]
fn truncated_schema_conflicts_report_tables_beyond_the_materialized_prefix() {
    let mut base_schema = schema(true, FieldType::Str);
    let mut second_table = base_schema.tables[0].clone();
    second_table.id = id(5);
    second_table.name = "Company".into();
    base_schema.tables.push(second_table);
    let mut left_schema = base_schema.clone();
    left_schema.tables[0].fields[1].ty = FieldType::Int;
    left_schema.tables[1].fields[1].ty = FieldType::Int;
    let mut right_schema = base_schema.clone();
    right_schema.tables[0].fields[1].ty = FieldType::Bool;
    right_schema.tables[1].fields[1].ty = FieldType::Bool;

    let mut base_row_company = parse_fixture(BASE, RowKeyKind::Explicit);
    base_row_company.table = id(5);
    let mut left_row_company = parse_fixture(LEFT, RowKeyKind::Explicit);
    left_row_company.table = id(5);
    let mut right_row_company = parse_fixture(CONFLICT, RowKeyKind::Explicit);
    right_row_company.table = id(5);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Base, b"base-company", vec![base_row_company]);
    source.add(MergeSide::Left, b"left-company", vec![left_row_company]);
    source.add(MergeSide::Right, b"right-company", vec![right_row_company]);

    let checkpoint = |generation, token: &[u8]| CheckpointGeneration {
        generation,
        position: Some(token.to_vec()),
    };
    let mut base = snapshot(
        base_schema,
        manifest(1, 1, b"base"),
        Some(checkpoint(4, b"base-token")),
    );
    let mut left = snapshot(
        left_schema,
        manifest(2, 2, b"left"),
        Some(checkpoint(5, b"left-token")),
    );
    let mut right = snapshot(
        right_schema,
        manifest(3, 3, b"right"),
        Some(checkpoint(6, b"right-token")),
    );
    base.tables.insert(id(5), manifest(10, 10, b"base-company"));
    left.tables.insert(id(5), manifest(20, 20, b"left-company"));
    right.tables.insert(id(5), manifest(30, 30, b"right-company"));

    let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the second table conflict exceeds the one-detail schema budget")
    };
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_tables.contains(&id(5)));
    assert!(report.affected_ranges.is_empty());
    assert!(report.affected_checkpoints.is_empty());
    assert!(source.visited.is_empty(), "schema conflicts stop before row and checkpoint phases");
}

#[test]
fn row_and_checkpoint_conflicts_accumulate_in_phase_order_without_a_partial_plan() {
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(
        schema(true, FieldType::Str),
        manifest(1, 1, b"base"),
        Some(base_checkpoint.clone()),
    );
    let left = snapshot(
        schema(true, FieldType::Str),
        manifest(2, 2, b"left"),
        Some(left_checkpoint.clone()),
    );
    let right = snapshot(
        schema(true, FieldType::Str),
        manifest(3, 3, b"right"),
        Some(right_checkpoint.clone()),
    );

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget())
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("row and checkpoint conflicts must reject the complete plan")
    };
    assert_eq!(conflicts.len(), 2);
    assert!(matches!(&conflicts[0], BranchMergeConflict::Row { .. }));
    assert!(matches!(
        &conflicts[1],
        BranchMergeConflict::CheckpointConflict { id: checkpoint_id, conflict }
            if checkpoint_id.as_slice() == b"consumer/source"
                && conflict.base.as_ref() == Some(&base_checkpoint)
                && conflict.left.as_ref() == Some(&left_checkpoint)
                && conflict.right.as_ref() == Some(&right_checkpoint)
    ));
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/source".as_slice()));
    assert_eq!(source.visited.len(), 3);
}

#[test]
fn conflict_budget_crossing_from_rows_into_checkpoints_has_a_precise_boundary() {
    let no_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 };
    let (base, left, right, mut source) = row_checkpoint_conflict_inputs();
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, no_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("first row conflict exceeds the zero-detail budget")
    };
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.is_empty());

    let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
    let (base, left, right, mut source) = row_checkpoint_conflict_inputs();
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("checkpoint conflict is the first conflict beyond the row detail")
    };
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/source".as_slice()));

    let exact_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 };
    let (base, left, right, mut source) = row_checkpoint_conflict_inputs();
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_budget)
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("conflicts at the exact budget remain a typed conflict result")
    };
    assert_eq!(conflicts.len(), 2);
    assert!(matches!(&conflicts[0], BranchMergeConflict::Row { .. }));
    assert!(matches!(&conflicts[1], BranchMergeConflict::CheckpointConflict { .. }));
    assert_eq!(report.conflicts_lower_bound, 2);
}

#[test]
fn conflict_budget_tail_reports_prior_table_ranges_and_checkpoint_impact() {
    let mut base_schema = schema(true, FieldType::Str);
    let mut second_table = base_schema.tables[0].clone();
    second_table.id = id(5);
    second_table.name = "Company".into();
    base_schema.tables.push(second_table);

    let mut base_row_company = parse_fixture(BASE, RowKeyKind::Explicit);
    base_row_company.table = id(5);
    let mut left_row_company = parse_fixture(LEFT, RowKeyKind::Explicit);
    left_row_company.table = id(5);
    let mut right_row_company = parse_fixture(CONFLICT, RowKeyKind::Explicit);
    right_row_company.table = id(5);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Base, b"base-company", vec![base_row_company]);
    source.add(MergeSide::Left, b"left-company", vec![left_row_company]);
    source.add(MergeSide::Right, b"right-company", vec![right_row_company]);

    let checkpoint = |generation, token: &[u8]| CheckpointGeneration {
        generation,
        position: Some(token.to_vec()),
    };
    let mut base = snapshot(
        base_schema.clone(),
        manifest(1, 1, b"base"),
        Some(checkpoint(4, b"base-token")),
    );
    let mut left = snapshot(
        base_schema.clone(),
        manifest(2, 2, b"left"),
        Some(checkpoint(5, b"left-token")),
    );
    let mut right = snapshot(
        base_schema,
        manifest(3, 3, b"right"),
        Some(checkpoint(6, b"right-token")),
    );
    base.tables.insert(id(5), manifest(10, 10, b"base-company"));
    left.tables.insert(id(5), manifest(20, 20, b"left-company"));
    right.tables.insert(id(5), manifest(30, 30, b"right-company"));

    let exact_row_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_row_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the checkpoint conflict is the first conflict beyond two row details")
    };
    assert_eq!(report.conflicts_lower_bound, 3);
    assert_eq!(report.rows_examined, 6);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_tables.contains(&id(5)));
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_ranges.contains(&(id(5), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/source".as_slice()));
}

#[test]
fn checkpoint_budget_tail_reports_only_the_crossing_id_after_a_row_conflict() {
    let (mut base, mut left, mut right, mut source) = row_checkpoint_conflict_inputs();
    base.checkpoints.clear();
    left.checkpoints.clear();
    right.checkpoints.clear();

    // The ordered checkpoint walk stops at the first conflict beyond budget;
    // successful earlier entries and unexamined later IDs are not impacts.
    let base_clean = CheckpointGeneration { generation: 1, position: Some(b"clean-base".to_vec()) };
    let left_clean = CheckpointGeneration { generation: 2, position: Some(b"clean-left".to_vec()) };
    for (side, checkpoint) in [
        (&mut base, base_clean.clone()),
        (&mut left, left_clean.clone()),
        (&mut right, base_clean),
    ] {
        side.checkpoints.insert(b"consumer/a-clean".to_vec(), checkpoint);
    }
    for (snapshot, checkpoint) in [
        (&mut base, CheckpointGeneration { generation: 4, position: Some(b"m-base".to_vec()) }),
        (&mut left, CheckpointGeneration { generation: 5, position: Some(b"m-left".to_vec()) }),
        (&mut right, CheckpointGeneration { generation: 6, position: Some(b"m-right".to_vec()) }),
    ] {
        snapshot.checkpoints.insert(b"consumer/m-conflict".to_vec(), checkpoint);
    }
    for (snapshot, checkpoint) in [
        (&mut base, CheckpointGeneration { generation: 7, position: Some(b"z-base".to_vec()) }),
        (&mut left, CheckpointGeneration { generation: 8, position: Some(b"z-left".to_vec()) }),
        (&mut right, CheckpointGeneration { generation: 9, position: Some(b"z-right".to_vec()) }),
    ] {
        snapshot.checkpoints.insert(b"consumer/z-unvisited".to_vec(), checkpoint);
    }

    let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the first conflicting checkpoint exceeds the row conflict detail")
    };
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/m-conflict".as_slice()));
    assert!(!report.affected_checkpoints.contains(b"consumer/a-clean".as_slice()));
    assert!(!report.affected_checkpoints.contains(b"consumer/z-unvisited".as_slice()));
    assert_eq!(source.visited.len(), 3);
}

#[test]
fn zero_conflict_budget_stops_at_first_checkpoint_after_fixture_row_merge() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);

    let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let clean_id = b"consumer/a-clean".to_vec();
    base.checkpoints.insert(
        clean_id.clone(),
        CheckpointGeneration { generation: 1, position: Some(b"base".to_vec()) },
    );
    left.checkpoints.insert(
        clean_id.clone(),
        CheckpointGeneration { generation: 2, position: Some(b"left".to_vec()) },
    );
    right.checkpoints.insert(
        clean_id.clone(),
        CheckpointGeneration { generation: 1, position: Some(b"base".to_vec()) },
    );

    let first_conflict_id = b"consumer/m-first-conflict".to_vec();
    for (snapshot, generation, token) in [
        (&mut base, 10, b"base-m".as_slice()),
        (&mut left, 11, b"left-m".as_slice()),
        (&mut right, 12, b"right-m".as_slice()),
    ] {
        snapshot.checkpoints.insert(
            first_conflict_id.clone(),
            CheckpointGeneration { generation, position: Some(token.to_vec()) },
        );
    }

    let later_conflict_id = b"consumer/z-unvisited".to_vec();
    for (snapshot, generation, token) in [
        (&mut base, 20, b"base-z".as_slice()),
        (&mut left, 21, b"left-z".as_slice()),
        (&mut right, 22, b"right-z".as_slice()),
    ] {
        snapshot.checkpoints.insert(
            later_conflict_id.clone(),
            CheckpointGeneration { generation, position: Some(token.to_vec()) },
        );
    }

    // The spec requires bounded conflict materialization but leaves traversal
    // order open. Storage completes changed row ranges before its ordered
    // checkpoint walk, then reports only the first checkpoint beyond a zero
    // detail budget without exposing a partial merge plan.
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the first divergent checkpoint exceeds the zero-detail budget")
    };
    assert_eq!(report.conflicts_lower_bound, 1);
    assert_eq!(report.rows_examined, 3, "the fixture-backed disjoint row edits complete first");
    assert!(report.affected_checkpoints.contains(first_conflict_id.as_slice()));
    assert!(!report.affected_checkpoints.contains(clean_id.as_slice()));
    assert!(!report.affected_checkpoints.contains(later_conflict_id.as_slice()));
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert_eq!(source.visited.len(), 3);
}

#[test]
fn zero_budget_checkpoint_delete_update_tail_reports_first_identity() {
    for delete_on_left in [true, false] {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
        source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
        source.add(MergeSide::Right, b"right", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

        let clean_id = b"consumer/a-clean".to_vec();
        let clean = CheckpointGeneration { generation: 1, position: None };
        for snapshot in [&mut base, &mut left, &mut right] {
            snapshot.checkpoints.insert(clean_id.clone(), clean.clone());
        }

        let agreed_delete_id = b"consumer/b-agreed-delete".to_vec();
        base.checkpoints.insert(
            agreed_delete_id.clone(),
            CheckpointGeneration { generation: 2, position: None },
        );

        let unchanged_delete_id = b"consumer/c-delete-against-unchanged".to_vec();
        let unchanged_value = CheckpointGeneration { generation: 3, position: None };
        base.checkpoints.insert(unchanged_delete_id.clone(), unchanged_value.clone());
        let unchanged_side = if delete_on_left { &mut right } else { &mut left };
        unchanged_side.checkpoints.insert(unchanged_delete_id.clone(), unchanged_value);

        let first_conflict_id = b"consumer/m-positionless-delete-update".to_vec();
        base.checkpoints.insert(
            first_conflict_id.clone(),
            CheckpointGeneration { generation: 4, position: None },
        );
        let update_side = if delete_on_left { &mut right } else { &mut left };
        update_side.checkpoints.insert(
            first_conflict_id.clone(),
            CheckpointGeneration { generation: 5, position: None },
        );

        let later_conflict_id = b"consumer/z-unvisited".to_vec();
        for (snapshot, generation) in [(&mut base, 7), (&mut left, 8), (&mut right, 9)] {
            snapshot.checkpoints.insert(
                later_conflict_id.clone(),
                CheckpointGeneration { generation, position: None },
            );
        }

        // Positionless values remain present checkpoint state. Agreed and
        // unchanged-side deletes resolve cleanly before bytewise traversal
        // reports the first divergent delete/update at zero budget.
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the first positionless delete/update checkpoint exceeds zero detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 1);
        assert_eq!(report.rows_examined, 3);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert!(report.affected_checkpoints.contains(first_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(clean_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(later_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn zero_budget_stops_at_first_of_oppositely_oriented_checkpoint_delete_conflicts() {
    for delete_first_on_left in [true, false] {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
        source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
        source.add(MergeSide::Right, b"right", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

        let first_id = b"consumer/a-first-delete-update".to_vec();
        let second_id = b"consumer/b-second-delete-update".to_vec();
        for (checkpoint_id, generation, delete_on_left) in [
            (&first_id, 4, delete_first_on_left),
            (&second_id, 6, !delete_first_on_left),
        ] {
            base.checkpoints.insert(
                checkpoint_id.clone(),
                CheckpointGeneration { generation, position: None },
            );
            let update_side = if delete_on_left { &mut right } else { &mut left };
            update_side.checkpoints.insert(
                checkpoint_id.clone(),
                CheckpointGeneration { generation: generation + 1, position: None },
            );
        }

        let later_id = b"consumer/z-unvisited".to_vec();
        for (snapshot, generation) in [(&mut base, 20), (&mut left, 21), (&mut right, 22)] {
            snapshot.checkpoints.insert(
                later_id.clone(),
                CheckpointGeneration { generation, position: None },
            );
        }

        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("first checkpoint delete/update exceeds the zero-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 1);
        assert_eq!(report.rows_examined, 3);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert!(report.affected_checkpoints.contains(first_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(second_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(later_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn zero_conflict_budget_allows_row_tombstone_and_checkpoint_deletes() {
    for delete_checkpoint_on_left in [true, false] {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
        source.add(MergeSide::Left, b"left", Vec::new());
        source.add(MergeSide::Right, b"right", Vec::new());

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

        let agreed_delete_id = b"consumer/a-agreed-delete".to_vec();
        base.checkpoints.insert(
            agreed_delete_id.clone(),
            CheckpointGeneration { generation: 1, position: None },
        );

        let unilateral_delete_id = b"consumer/b-delete-against-unchanged".to_vec();
        let retained_checkpoint = CheckpointGeneration { generation: 2, position: None };
        base.checkpoints.insert(unilateral_delete_id.clone(), retained_checkpoint.clone());
        let unchanged_side = if delete_checkpoint_on_left { &mut right } else { &mut left };
        unchanged_side
            .checkpoints
            .insert(unilateral_delete_id.clone(), retained_checkpoint);

        let retained_id = b"consumer/z-retained".to_vec();
        let retained = CheckpointGeneration { generation: 3, position: None };
        for snapshot in [&mut base, &mut left, &mut right] {
            snapshot.checkpoints.insert(retained_id.clone(), retained.clone());
        }

        // Agreeing deletes and delete-versus-unchanged resolution add no
        // conflicts, so they remain valid under a zero conflict-detail budget.
        let plan = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap();

        assert_eq!(plan.report.conflicts_lower_bound, 0);
        assert_eq!(plan.report.rows_examined, 1);
        assert!(plan.report.affected_checkpoints.is_empty());
        assert_eq!(source.visited.len(), 3);
        assert!(!plan.checkpoints.contains_key(agreed_delete_id.as_slice()));
        assert!(!plan.checkpoints.contains_key(unilateral_delete_id.as_slice()));
        assert_eq!(plan.checkpoints.get(retained_id.as_slice()), Some(&retained));

        let MergedSegment::Rows { rows, tombstones, .. } = &plan.tables[&id(1)].segments[0] else {
            panic!("the fixture-backed agreed row deletion materializes as a tombstone")
        };
        assert!(rows.is_empty());
        assert_eq!(tombstones, &[integer(1)]);
    }
}

#[test]
fn segmented_zero_conflict_budget_stops_after_tombstone_before_checkpoints() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let (low_key, high_key) = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        (candidate_a, candidate_b)
    } else {
        (candidate_b, candidate_a)
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |table_digest, low_digest, high_digest, low_locator: &[u8], high_locator: &[u8]| {
        TableManifest {
            digest: [table_digest; 32],
            segments: vec![
                RowSegmentManifest {
                    locator: low_locator.to_vec(),
                    range: low_range.clone(),
                    digest: [low_digest; 32],
                },
                RowSegmentManifest {
                    locator: high_locator.to_vec(),
                    range: high_range.clone(),
                    digest: [high_digest; 32],
                },
            ],
        }
    };
    let fixture_row = |fixture: &str, key: CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key;
        row
    };
    let build_inputs = |upper_conflict| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-low", vec![fixture_row(BASE, low_key.clone())]);
        source.add(MergeSide::Left, b"left-low", Vec::new());
        source.add(MergeSide::Right, b"right-low", Vec::new());
        source.add(MergeSide::Base, b"base-high", vec![fixture_row(BASE, high_key.clone())]);
        source.add(
            MergeSide::Left,
            b"left-high",
            if upper_conflict { Vec::new() } else { vec![fixture_row(BASE, high_key.clone())] },
        );
        source.add(
            MergeSide::Right,
            b"right-high",
            vec![fixture_row(if upper_conflict { CONFLICT } else { RIGHT }, high_key.clone())],
        );

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(80, 1, 4, b"base-low", b"base-high"),
            None,
        );
        let left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(81, 2, 5, b"left-low", b"left-high"),
            None,
        );
        let right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(82, 3, 6, b"right-low", b"right-high"),
            None,
        );
        let checkpoint_delete_id = b"consumer/positionless-delete-after-row-tail".to_vec();
        base.checkpoints.insert(
            checkpoint_delete_id.clone(),
            CheckpointGeneration { generation: 40, position: None },
        );
        (base, left, right, source, checkpoint_delete_id)
    };

    // MERGE-005 bounds conflict detail but leaves traversal order open. This
    // adapter policy visits the lower segment first: its agreed delete is a
    // clean tombstone, while the later upper delete/edit conflict is the first
    // impact at zero detail budget and prevents checkpoint-phase reporting.
    let (base, left, right, mut source, checkpoint_delete_id) = build_inputs(true);
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the upper row delete/edit conflict exceeds the zero-detail budget")
    };
    assert_eq!(report.conflicts_lower_bound, 1);
    assert_eq!(report.rows_examined, 3);
    assert!(report.affected_ranges.contains(&(id(1), low_range.clone())));
    assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
    assert!(report.affected_checkpoints.is_empty());
    assert!(!report.affected_checkpoints.contains(checkpoint_delete_id.as_slice()));
    assert_eq!(source.visited.len(), 6);

    // Removing only the later row conflict demonstrates that the same zero
    // conflict budget preserves the lower tombstone and resolves the pending
    // checkpoint deletion instead of charging either clean deletion as a hit.
    let (base, left, right, mut source, checkpoint_delete_id) = build_inputs(false);
    let plan = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
    )
    .unwrap();
    assert_eq!(plan.report.conflicts_lower_bound, 0);
    assert_eq!(plan.report.rows_examined, 4);
    assert!(!plan.checkpoints.contains_key(checkpoint_delete_id.as_slice()));
    let segments = &plan.tables[&id(1)].segments;
    let MergedSegment::Rows { rows, tombstones, .. } = &segments[0] else {
        panic!("the lower segment delete must materialize a tombstone")
    };
    assert!(rows.is_empty());
    assert_eq!(tombstones, &[low_key]);
    let MergedSegment::Rows { rows, tombstones, .. } = &segments[1] else {
        panic!("the clean upper edit must merge after the tombstone range")
    };
    assert!(tombstones.is_empty());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fields[&id(2)], string("Ada"));
    assert_eq!(rows[0].fields[&id(3)], string("Paris"));
    assert_eq!(source.visited.len(), 6);
}

#[test]
fn segmented_tombstone_and_delete_edit_conflicts_stop_before_the_tail() {
    let mut keys = [integer(10), integer(20), integer(30), integer(40)];
    keys.sort_by_key(|key| key.encode().unwrap());
    let [clean_tombstone_key, first_conflict_key, second_conflict_key, tail_conflict_key] = keys;
    let first_boundary = second_conflict_key.encode().unwrap();
    let second_boundary = tail_conflict_key.encode().unwrap();
    let ranges = [
        KeyRange::new(None, Some(first_boundary.clone())).unwrap(),
        KeyRange::new(Some(first_boundary), Some(second_boundary.clone())).unwrap(),
        KeyRange::new(Some(second_boundary), None).unwrap(),
    ];
    let split_manifest = |table_digest: u8, segment_digests: [u8; 3], locators: [&[u8]; 3]| {
        TableManifest {
            digest: [table_digest; 32],
            segments: (0..3)
                .map(|index| RowSegmentManifest {
                    locator: locators[index].to_vec(),
                    range: ranges[index].clone(),
                    digest: [segment_digests[index]; 32],
                })
                .collect(),
        }
    };
    let fixture_row = |fixture: &str, key: &CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key.clone();
        row
    };
    let build_inputs = || {
        let mut source = FixtureRows::default();
        // Both branches delete the first row, so it yields a clean tombstone
        // before the first delete/edit conflict in encoded-key order.
        source.add(
            MergeSide::Base,
            b"base-first",
            vec![
                fixture_row(BASE, &clean_tombstone_key),
                fixture_row(BASE, &first_conflict_key),
            ],
        );
        source.add(MergeSide::Left, b"left-first", Vec::new());
        source.add(
            MergeSide::Right,
            b"right-first",
            vec![fixture_row(RIGHT, &first_conflict_key)],
        );

        // The second delete/edit conflict reverses the branch orientation.
        source.add(
            MergeSide::Base,
            b"base-second",
            vec![fixture_row(BASE, &second_conflict_key)],
        );
        source.add(
            MergeSide::Left,
            b"left-second",
            vec![fixture_row(LEFT, &second_conflict_key)],
        );
        source.add(MergeSide::Right, b"right-second", Vec::new());

        // A third conflict is the later tail. It must not affect the lower
        // bound once an earlier segment crosses the configured detail budget.
        source.add(
            MergeSide::Base,
            b"base-tail",
            vec![fixture_row(BASE, &tail_conflict_key)],
        );
        source.add(MergeSide::Left, b"left-tail", Vec::new());
        source.add(
            MergeSide::Right,
            b"right-tail",
            vec![fixture_row(CONFLICT, &tail_conflict_key)],
        );

        let base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                90,
                [1, 2, 3],
                [b"base-first".as_slice(), b"base-second".as_slice(), b"base-tail".as_slice()],
            ),
            None,
        );
        let left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                91,
                [4, 5, 6],
                [b"left-first".as_slice(), b"left-second".as_slice(), b"left-tail".as_slice()],
            ),
            None,
        );
        let right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                92,
                [7, 8, 9],
                [b"right-first".as_slice(), b"right-second".as_slice(), b"right-tail".as_slice()],
            ),
            None,
        );
        (base, left, right, source)
    };

    // ORNA-MERGE-005 requires bounded conflict evidence but does not prescribe
    // discovery order. Storage visits manifest ranges in order, then canonical
    // keys within each range. This fixture proof records that policy: the clean
    // tombstone costs no conflict slot, and the first conflict beyond budget
    // stops before later range segments contribute to the lower bound.
    let segment_visits = [
        [
            (MergeSide::Base, b"base-first".to_vec()),
            (MergeSide::Left, b"left-first".to_vec()),
            (MergeSide::Right, b"right-first".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-second".to_vec()),
            (MergeSide::Left, b"left-second".to_vec()),
            (MergeSide::Right, b"right-second".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-tail".to_vec()),
            (MergeSide::Left, b"left-tail".to_vec()),
            (MergeSide::Right, b"right-tail".to_vec()),
        ],
    ];
    for max_conflicts in 0..=3 {
        let (base, left, right, mut source) = build_inputs();
        let result = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts },
        );
        let reached_segments = (max_conflicts + 1).min(3);
        let expected_visits = segment_visits
            .iter()
            .take(reached_segments)
            .flat_map(|visits| visits.iter().cloned())
            .collect::<Vec<_>>();
        assert_eq!(source.visited, expected_visits);

        if max_conflicts < 3 {
            let BranchMergeError::BudgetExceeded { report } = result.unwrap_err() else {
                panic!("the first delete/edit conflict beyond budget stops the tail")
            };
            assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
            assert_eq!(report.rows_examined, 3 + 2 * max_conflicts);
            assert_eq!(report.affected_ranges.len(), reached_segments);
            for range in ranges.iter().take(reached_segments) {
                assert!(report.affected_ranges.contains(&(id(1), range.clone())));
            }
            assert!(report.affected_checkpoints.is_empty());
        } else {
            let BranchMergeError::Conflicts { conflicts, report } = result.unwrap_err() else {
                panic!("all three fixture conflicts fit exactly at the detail limit")
            };
            assert_eq!(report.conflicts_lower_bound, 3);
            assert_eq!(report.rows_examined, 7);
            assert_eq!(report.affected_ranges.len(), 3);
            let conflict_keys = conflicts
                .iter()
                .filter_map(|conflict| match conflict {
                    BranchMergeConflict::Row {
                        conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                        ..
                    } => Some(key.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                conflict_keys,
                vec![
                    first_conflict_key.clone(),
                    second_conflict_key.clone(),
                    tail_conflict_key.clone(),
                ]
            );
        }
    }
}

#[test]
fn segmented_conflict_tombstone_conflict_stops_before_later_conflicts() {
    let mut keys = [integer(10), integer(20), integer(30), integer(40)];
    keys.sort_by_key(|key| key.encode().unwrap());
    let [first_conflict_key, clean_tombstone_key, second_conflict_key, tail_conflict_key] = keys;
    let first_boundary = clean_tombstone_key.encode().unwrap();
    let second_boundary = second_conflict_key.encode().unwrap();
    let third_boundary = tail_conflict_key.encode().unwrap();
    let ranges = [
        KeyRange::new(None, Some(first_boundary.clone())).unwrap(),
        KeyRange::new(Some(first_boundary), Some(second_boundary.clone())).unwrap(),
        KeyRange::new(Some(second_boundary), Some(third_boundary.clone())).unwrap(),
        KeyRange::new(Some(third_boundary), None).unwrap(),
    ];
    let split_manifest = |table_digest: u8, segment_digests: [u8; 4], locators: [&[u8]; 4]| {
        TableManifest {
            digest: [table_digest; 32],
            segments: (0..4)
                .map(|index| RowSegmentManifest {
                    locator: locators[index].to_vec(),
                    range: ranges[index].clone(),
                    digest: [segment_digests[index]; 32],
                })
                .collect(),
        }
    };
    let fixture_row = |fixture: &str, key: &CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key.clone();
        row
    };
    let build_inputs = || {
        let mut source = FixtureRows::default();
        // The first conflict is delete-on-left/edit-on-right.
        source.add(
            MergeSide::Base,
            b"base-conflict",
            vec![fixture_row(BASE, &first_conflict_key)],
        );
        source.add(MergeSide::Left, b"left-conflict", Vec::new());
        source.add(
            MergeSide::Right,
            b"right-conflict",
            vec![fixture_row(RIGHT, &first_conflict_key)],
        );

        // This clean delete is between conflicts and must not spend a slot.
        source.add(
            MergeSide::Base,
            b"base-tombstone",
            vec![fixture_row(BASE, &clean_tombstone_key)],
        );
        source.add(MergeSide::Left, b"left-tombstone", Vec::new());
        source.add(MergeSide::Right, b"right-tombstone", Vec::new());

        // Reverse the delete/edit orientation for the next conflict.
        source.add(
            MergeSide::Base,
            b"base-middle-conflict",
            vec![fixture_row(BASE, &second_conflict_key)],
        );
        source.add(
            MergeSide::Left,
            b"left-middle-conflict",
            vec![fixture_row(LEFT, &second_conflict_key)],
        );
        source.add(MergeSide::Right, b"right-middle-conflict", Vec::new());

        source.add(
            MergeSide::Base,
            b"base-tail-conflict",
            vec![fixture_row(BASE, &tail_conflict_key)],
        );
        source.add(MergeSide::Left, b"left-tail-conflict", Vec::new());
        source.add(
            MergeSide::Right,
            b"right-tail-conflict",
            vec![fixture_row(CONFLICT, &tail_conflict_key)],
        );

        let base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                93,
                [10, 11, 12, 13],
                [
                    b"base-conflict".as_slice(),
                    b"base-tombstone".as_slice(),
                    b"base-middle-conflict".as_slice(),
                    b"base-tail-conflict".as_slice(),
                ],
            ),
            None,
        );
        let left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                94,
                [20, 21, 22, 23],
                [
                    b"left-conflict".as_slice(),
                    b"left-tombstone".as_slice(),
                    b"left-middle-conflict".as_slice(),
                    b"left-tail-conflict".as_slice(),
                ],
            ),
            None,
        );
        let right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                95,
                [30, 31, 32, 33],
                [
                    b"right-conflict".as_slice(),
                    b"right-tombstone".as_slice(),
                    b"right-middle-conflict".as_slice(),
                    b"right-tail-conflict".as_slice(),
                ],
            ),
            None,
        );
        (base, left, right, source)
    };

    // ORNA-MERGE-005 requires bounded conflict evidence but leaves discovery
    // order open. This proof records manifest order for the adapter: an early
    // delete/edit conflict stops before the clean tombstone, while one detail
    // slot carries traversal through that tombstone to the next conflict.
    let segment_visits = [
        [
            (MergeSide::Base, b"base-conflict".to_vec()),
            (MergeSide::Left, b"left-conflict".to_vec()),
            (MergeSide::Right, b"right-conflict".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-tombstone".to_vec()),
            (MergeSide::Left, b"left-tombstone".to_vec()),
            (MergeSide::Right, b"right-tombstone".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-middle-conflict".to_vec()),
            (MergeSide::Left, b"left-middle-conflict".to_vec()),
            (MergeSide::Right, b"right-middle-conflict".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-tail-conflict".to_vec()),
            (MergeSide::Left, b"left-tail-conflict".to_vec()),
            (MergeSide::Right, b"right-tail-conflict".to_vec()),
        ],
    ];
    for max_conflicts in 0..=3 {
        let (base, left, right, mut source) = build_inputs();
        let result = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts },
        );
        let reached_segments = match max_conflicts {
            0 => 1,
            1 => 3,
            _ => 4,
        };
        let expected_rows = match max_conflicts {
            0 => 2,
            1 => 5,
            _ => 7,
        };
        let expected_visits = segment_visits
            .iter()
            .take(reached_segments)
            .flat_map(|visits| visits.iter().cloned())
            .collect::<Vec<_>>();
        assert_eq!(source.visited, expected_visits);

        if max_conflicts < 3 {
            let BranchMergeError::BudgetExceeded { report } = result.unwrap_err() else {
                panic!("the first delete/edit conflict beyond budget stops later ranges")
            };
            assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
            assert_eq!(report.rows_examined, expected_rows);
            assert_eq!(report.affected_ranges.len(), reached_segments);
            for range in ranges.iter().take(reached_segments) {
                assert!(report.affected_ranges.contains(&(id(1), range.clone())));
            }
            assert!(report.affected_checkpoints.is_empty());
        } else {
            let BranchMergeError::Conflicts { conflicts, report } = result.unwrap_err() else {
                panic!("all fixture conflicts fit exactly at the detail limit")
            };
            assert_eq!(report.conflicts_lower_bound, 3);
            assert_eq!(report.rows_examined, expected_rows);
            let conflict_keys = conflicts
                .iter()
                .filter_map(|conflict| match conflict {
                    BranchMergeConflict::Row {
                        conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                        ..
                    } => Some(key.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                conflict_keys,
                vec![
                    first_conflict_key.clone(),
                    second_conflict_key.clone(),
                    tail_conflict_key.clone(),
                ]
            );
        }
    }
}

#[test]
fn checkpoint_delete_update_budget_tail_reports_identity_for_either_deleted_side() {
    for delete_on_left in [true, false] {
        let (mut base, mut left, mut right, mut source) = row_checkpoint_conflict_inputs();
        base.checkpoints.clear();
        left.checkpoints.clear();
        right.checkpoints.clear();

        // Deletion-versus-update has an absent branch value, but impact stays
        // attached to the stable ID on either side of the conflict.
        let checkpoint_id = if delete_on_left {
            b"consumer/deleted-on-left".to_vec()
        } else {
            b"consumer/deleted-on-right".to_vec()
        };
        base.checkpoints.insert(
            checkpoint_id.clone(),
            CheckpointGeneration { generation: 4, position: Some(b"base-token".to_vec()) },
        );
        let edited_side = if delete_on_left { &mut right } else { &mut left };
        edited_side.checkpoints.insert(
            checkpoint_id.clone(),
            CheckpointGeneration { generation: 5, position: Some(b"edited-token".to_vec()) },
        );

        let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
        let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
            .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("checkpoint delete/update is the first conflict beyond the row detail")
        };
        assert_eq!(report.conflicts_lower_bound, 2);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn positionless_delete_update_conflict_keeps_impact_at_shared_budget_tail() {
    for delete_on_left in [true, false] {
        for max_conflicts in [0, 1, 2] {
            let (mut base, mut left, mut right, mut source) = row_checkpoint_conflict_inputs();
            base.checkpoints.clear();
            left.checkpoints.clear();
            right.checkpoints.clear();

            let checkpoint_id = if delete_on_left {
                b"consumer/positionless-delete-left".to_vec()
            } else {
                b"consumer/positionless-delete-right".to_vec()
            };
            let unchanged_positionless = CheckpointGeneration { generation: 30, position: None };
            let advanced_positionless = CheckpointGeneration { generation: 31, position: None };
            base.checkpoints.insert(checkpoint_id.clone(), unchanged_positionless.clone());
            let changed_side = if delete_on_left { &mut right } else { &mut left };
            changed_side
                .checkpoints
                .insert(checkpoint_id.clone(), advanced_positionless.clone());

            // The fixture row conflict is first in the ordered impact stream.
            // At budget 0 it alone crosses the boundary; at budget 1 the
            // positionless delete/update conflict crosses the shared tail;
            // budget 2 retains that checkpoint conflict with its full states.
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts },
            )
            .unwrap_err();

            match error {
                BranchMergeError::BudgetExceeded { report } if max_conflicts == 0 => {
                    assert_eq!(report.conflicts_lower_bound, 1);
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                    assert!(report.affected_checkpoints.is_empty());
                    assert_eq!(source.visited.len(), 3);
                }
                BranchMergeError::BudgetExceeded { report } if max_conflicts == 1 => {
                    assert_eq!(report.conflicts_lower_bound, 2);
                    assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    assert_eq!(source.visited.len(), 3);
                }
                BranchMergeError::Conflicts { conflicts, report } if max_conflicts == 2 => {
                    assert_eq!(report.conflicts_lower_bound, 2);
                    assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    assert_eq!(source.visited.len(), 3);
                    let checkpoint_conflict = conflicts.iter().find_map(|conflict| match conflict {
                        BranchMergeConflict::CheckpointConflict { id, conflict }
                            if id == &checkpoint_id => Some(conflict),
                        _ => None,
                    });
                    let Some(checkpoint_conflict) = checkpoint_conflict else {
                        panic!("the retained conflict detail includes the checkpoint identity")
                    };
                    assert_eq!(checkpoint_conflict.base.as_ref(), Some(&unchanged_positionless));
                    if delete_on_left {
                        assert_eq!(checkpoint_conflict.left, None);
                        assert_eq!(checkpoint_conflict.right.as_ref(), Some(&advanced_positionless));
                    } else {
                        assert_eq!(checkpoint_conflict.left.as_ref(), Some(&advanced_positionless));
                        assert_eq!(checkpoint_conflict.right, None);
                    }
                }
                other => panic!("unexpected conflict-budget result: {other:?}"),
            }
        }
    }
}

#[test]
fn positionless_checkpoint_delete_update_follows_row_delete_edit_at_budget_tail() {
    let build_inputs = |row_delete_on_left, checkpoint_delete_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
        let changed_row = parse_fixture(RIGHT, RowKeyKind::Explicit);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![changed_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![changed_row] } else { Vec::new() },
        );

        let base_checkpoint = CheckpointGeneration { generation: 100, position: None };
        let updated_checkpoint = CheckpointGeneration { generation: 101, position: None };
        let checkpoint_id = b"consumer/positionless-row-delete-edit-budget-tail".to_vec();
        let mut base = snapshot(schema(true, FieldType::Str), manifest(60, 60, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(61, 61, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(62, 62, b"right"), None);
        base.checkpoints.insert(checkpoint_id.clone(), base_checkpoint.clone());
        let update_side = if checkpoint_delete_on_left {
            &mut right
        } else {
            &mut left
        };
        update_side.checkpoints.insert(checkpoint_id.clone(), updated_checkpoint.clone());

        (base, left, right, source, checkpoint_id, base_checkpoint, updated_checkpoint)
    };

    // ORNA-MERGE-011 requires a conflict for divergent checkpoint state but
    // leaves cross-phase ordering open. Storage reports row conflicts first,
    // so a zero-detail budget stops at the row impact before the checkpoint
    // tail; one slot makes the checkpoint the crossing impact, and two retain it.
    for row_delete_on_left in [true, false] {
        for checkpoint_delete_on_left in [true, false] {
            for max_conflicts in [0, 1, 2] {
                let (base, left, right, mut source, checkpoint_id, base_checkpoint, updated_checkpoint) =
                    build_inputs(row_delete_on_left, checkpoint_delete_on_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                )
                .unwrap_err();

                match error {
                    BranchMergeError::BudgetExceeded { report } if max_conflicts == 0 => {
                        assert_eq!(report.conflicts_lower_bound, 1);
                        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                        assert!(report.affected_checkpoints.is_empty());
                    }
                    BranchMergeError::BudgetExceeded { report } if max_conflicts == 1 => {
                        assert_eq!(report.conflicts_lower_bound, 2);
                        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    BranchMergeError::Conflicts { conflicts, report } if max_conflicts == 2 => {
                        assert_eq!(report.conflicts_lower_bound, 2);
                        assert!(matches!(
                            conflicts.first(),
                            Some(BranchMergeConflict::Row {
                                conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                                ..
                            }) if key == &integer(1)
                        ));
                        assert!(matches!(
                            conflicts.get(1),
                            Some(BranchMergeConflict::CheckpointConflict { id, conflict })
                                if id == &checkpoint_id
                                    && conflict.base.as_ref() == Some(&base_checkpoint)
                                    && conflict.left.as_ref() == (if checkpoint_delete_on_left { None } else { Some(&updated_checkpoint) })
                                    && conflict.right.as_ref() == (if checkpoint_delete_on_left { Some(&updated_checkpoint) } else { None })
                        ));
                        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    other => panic!("unexpected row/checkpoint conflict-budget result: {other:?}"),
                }
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn positionless_checkpoint_tail_follows_upper_segment_row_delete_edit() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let high_key = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        candidate_b
    } else {
        candidate_a
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |table_digest, upper_digest, upper_locator: &[u8]| TableManifest {
        digest: [table_digest; 32],
        segments: vec![
            RowSegmentManifest {
                locator: b"shared-lower".to_vec(),
                range: low_range.clone(),
                digest: [7; 32],
            },
            RowSegmentManifest {
                locator: upper_locator.to_vec(),
                range: high_range.clone(),
                digest: [upper_digest; 32],
            },
        ],
    };
    let mut base_row = parse_fixture(BASE, RowKeyKind::Explicit);
    base_row.key = high_key.clone();
    let mut edited_row = parse_fixture(RIGHT, RowKeyKind::Explicit);
    edited_row.key = high_key.clone();

    let build_inputs = |row_delete_on_left, checkpoint_delete_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-upper", vec![base_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left-upper",
            if row_delete_on_left { Vec::new() } else { vec![edited_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right-upper",
            if row_delete_on_left { vec![edited_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(70, 10, b"base-upper"),
            None,
        );
        let mut left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(71, 11, b"left-upper"),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(72, 12, b"right-upper"),
            None,
        );

        let checkpoint_id = b"consumer/upper-row-delete-positionless-tail".to_vec();
        let base_checkpoint = CheckpointGeneration { generation: 130, position: None };
        let updated_checkpoint = CheckpointGeneration { generation: 131, position: None };
        base.checkpoints.insert(checkpoint_id.clone(), base_checkpoint.clone());
        let update_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
        update_side.checkpoints.insert(checkpoint_id.clone(), updated_checkpoint.clone());

        (
            base,
            left,
            right,
            source,
            checkpoint_id,
            base_checkpoint,
            updated_checkpoint,
        )
    };

    // The row conflict at the inclusive upper boundary is planned before
    // checkpoint state. A zero-detail budget stops there; one detail slot
    // makes the cursorless checkpoint impact cross the tail, and two retain it.
    for row_delete_on_left in [true, false] {
        for checkpoint_delete_on_left in [true, false] {
            for max_conflicts in [0, 1, 2] {
                let (
                    base,
                    left,
                    right,
                    mut source,
                    checkpoint_id,
                    base_checkpoint,
                    updated_checkpoint,
                ) = build_inputs(row_delete_on_left, checkpoint_delete_on_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                )
                .unwrap_err();

                match error {
                    BranchMergeError::BudgetExceeded { report } if max_conflicts == 0 => {
                        assert_eq!(report.conflicts_lower_bound, 1);
                        assert_eq!(report.rows_examined, 2);
                        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
                        assert!(!report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    BranchMergeError::BudgetExceeded { report } if max_conflicts == 1 => {
                        assert_eq!(report.conflicts_lower_bound, 2);
                        assert_eq!(report.rows_examined, 2);
                        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    BranchMergeError::Conflicts { conflicts, report } if max_conflicts == 2 => {
                        assert_eq!(report.conflicts_lower_bound, 2);
                        assert_eq!(report.rows_examined, 2);
                        assert!(matches!(
                            conflicts.first(),
                            Some(BranchMergeConflict::Row {
                                conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                                ..
                            }) if key == &high_key
                        ));
                        assert!(matches!(
                            conflicts.get(1),
                            Some(BranchMergeConflict::CheckpointConflict { id, conflict })
                                if id == &checkpoint_id
                                    && conflict.base.as_ref() == Some(&base_checkpoint)
                                    && conflict.left.as_ref() == (if checkpoint_delete_on_left { None } else { Some(&updated_checkpoint) })
                                    && conflict.right.as_ref() == (if checkpoint_delete_on_left { Some(&updated_checkpoint) } else { None })
                        ));
                        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    other => panic!("unexpected segment delete/checkpoint budget result: {other:?}"),
                }
                assert_eq!(source.visited.len(), 3);
                assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"upper")));
            }
        }
    }
}

#[test]
fn both_delete_update_orientations_survive_the_checkpoint_budget_tail() {
    let (mut base, mut left, mut right, mut source) = row_checkpoint_conflict_inputs();
    base.checkpoints.clear();
    left.checkpoints.clear();
    right.checkpoints.clear();
    let checkpoint = |generation, position: &[u8]| CheckpointGeneration {
        generation,
        position: Some(position.to_vec()),
    };

    // A is deleted on left and edited on right; B has the opposite orientation.
    // Their ordered impacts include both the final retained detail and the
    // first checkpoint beyond budget, while the later Z conflict is unvisited.
    let deleted_left = b"consumer/a-deleted-on-left".to_vec();
    base.checkpoints.insert(deleted_left.clone(), checkpoint(4, b"a-base"));
    right.checkpoints.insert(deleted_left.clone(), checkpoint(5, b"a-right"));

    let deleted_right = b"consumer/b-deleted-on-right".to_vec();
    base.checkpoints.insert(deleted_right.clone(), checkpoint(4, b"b-base"));
    left.checkpoints.insert(deleted_right.clone(), checkpoint(5, b"b-left"));

    let unvisited = b"consumer/z-unvisited".to_vec();
    base.checkpoints.insert(unvisited.clone(), checkpoint(7, b"z-base"));
    left.checkpoints.insert(unvisited.clone(), checkpoint(8, b"z-left"));
    right.checkpoints.insert(unvisited.clone(), checkpoint(9, b"z-right"));

    let two_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, two_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the second delete/update checkpoint crosses the shared budget")
    };
    assert_eq!(report.conflicts_lower_bound, 3);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(deleted_left.as_slice()));
    assert!(report.affected_checkpoints.contains(deleted_right.as_slice()));
    assert!(!report.affected_checkpoints.contains(unvisited.as_slice()));
    assert_eq!(source.visited.len(), 3);
}

#[test]
fn segmented_zero_conflict_budget_reaches_checkpoint_tail_after_fixture_merges() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let (low_key, high_key) = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        (candidate_a, candidate_b)
    } else {
        (candidate_b, candidate_a)
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |digest, low_digest, high_digest, low_locator: &[u8], high_locator: &[u8]| {
        TableManifest {
            digest: [digest; 32],
            segments: vec![
                RowSegmentManifest {
                    locator: low_locator.to_vec(),
                    range: low_range.clone(),
                    digest: [low_digest; 32],
                },
                RowSegmentManifest {
                    locator: high_locator.to_vec(),
                    range: high_range.clone(),
                    digest: [high_digest; 32],
                },
            ],
        }
    };
    let fixture_row = |fixture: &str, key: CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key;
        row
    };
    let mut source = FixtureRows::default();
    for (side, locator_prefix, fixture) in [
        (MergeSide::Base, b"base".as_slice(), BASE),
        (MergeSide::Left, b"left".as_slice(), LEFT),
        (MergeSide::Right, b"right".as_slice(), RIGHT),
    ] {
        let low_locator = [locator_prefix, b"-low"].concat();
        let high_locator = [locator_prefix, b"-high"].concat();
        source.add(side, &low_locator, vec![fixture_row(fixture, low_key.clone())]);
        source.add(side, &high_locator, vec![fixture_row(fixture, high_key.clone())]);
    }

    let mut base = snapshot(
        schema(true, FieldType::Str),
        split_manifest(30, 1, 4, b"base-low", b"base-high"),
        None,
    );
    let mut left = snapshot(
        schema(true, FieldType::Str),
        split_manifest(31, 2, 5, b"left-low", b"left-high"),
        None,
    );
    let mut right = snapshot(
        schema(true, FieldType::Str),
        split_manifest(32, 3, 6, b"right-low", b"right-high"),
        None,
    );

    let clean_id = b"consumer/a-clean".to_vec();
    base.checkpoints.insert(
        clean_id.clone(),
        CheckpointGeneration { generation: 1, position: Some(b"base".to_vec()) },
    );
    left.checkpoints.insert(
        clean_id.clone(),
        CheckpointGeneration { generation: 2, position: Some(b"left".to_vec()) },
    );
    right.checkpoints.insert(
        clean_id.clone(),
        CheckpointGeneration { generation: 1, position: Some(b"base".to_vec()) },
    );

    let first_conflict_id = b"consumer/m-first-conflict".to_vec();
    for (snapshot, generation, token) in [
        (&mut base, 10, b"base-m".as_slice()),
        (&mut left, 11, b"left-m".as_slice()),
        (&mut right, 12, b"right-m".as_slice()),
    ] {
        snapshot.checkpoints.insert(
            first_conflict_id.clone(),
            CheckpointGeneration { generation, position: Some(token.to_vec()) },
        );
    }

    let later_conflict_id = b"consumer/z-unvisited".to_vec();
    for (snapshot, generation, token) in [
        (&mut base, 20, b"base-z".as_slice()),
        (&mut left, 21, b"left-z".as_slice()),
        (&mut right, 22, b"right-z".as_slice()),
    ] {
        snapshot.checkpoints.insert(
            later_conflict_id.clone(),
            CheckpointGeneration { generation, position: Some(token.to_vec()) },
        );
    }

    // MERGE-005 bounds materialized conflicts, but leaves phase traversal
    // open. Storage completes every changed segment before its ordered
    // checkpoint pass, so even a zero-detail limit reports the first
    // checkpoint impact only after the fixture-backed ranges are resolved.
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the first divergent checkpoint exceeds the zero-detail budget")
    };
    assert_eq!(report.conflicts_lower_bound, 1);
    assert_eq!(report.rows_examined, 6, "both fixture-backed segments complete first");
    assert!(report.affected_ranges.contains(&(id(1), low_range)));
    assert!(report.affected_ranges.contains(&(id(1), high_range)));
    assert!(report.affected_checkpoints.contains(first_conflict_id.as_slice()));
    assert!(!report.affected_checkpoints.contains(clean_id.as_slice()));
    assert!(!report.affected_checkpoints.contains(later_conflict_id.as_slice()));
    assert_eq!(source.visited.len(), 6);
    assert_eq!(
        source.visited,
        vec![
            (MergeSide::Base, b"base-low".to_vec()),
            (MergeSide::Left, b"left-low".to_vec()),
            (MergeSide::Right, b"right-low".to_vec()),
            (MergeSide::Base, b"base-high".to_vec()),
            (MergeSide::Left, b"left-high".to_vec()),
            (MergeSide::Right, b"right-high".to_vec()),
        ],
    );
}

#[test]
fn segmented_zero_budget_stops_at_first_of_opposite_delete_updates() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let (low_key, high_key) = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        (candidate_a, candidate_b)
    } else {
        (candidate_b, candidate_a)
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |digest, low_digest, high_digest, low_locator: &[u8], high_locator: &[u8]| {
        TableManifest {
            digest: [digest; 32],
            segments: vec![
                RowSegmentManifest {
                    locator: low_locator.to_vec(),
                    range: low_range.clone(),
                    digest: [low_digest; 32],
                },
                RowSegmentManifest {
                    locator: high_locator.to_vec(),
                    range: high_range.clone(),
                    digest: [high_digest; 32],
                },
            ],
        }
    };
    let fixture_row = |fixture: &str, key: CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key;
        row
    };

    for delete_on_left in [true, false] {
        let mut source = FixtureRows::default();
        for (side, locator_prefix, fixture) in [
            (MergeSide::Base, b"base".as_slice(), BASE),
            (MergeSide::Left, b"left".as_slice(), LEFT),
            (MergeSide::Right, b"right".as_slice(), RIGHT),
        ] {
            let low_locator = [locator_prefix, b"-low"].concat();
            let high_locator = [locator_prefix, b"-high"].concat();
            source.add(side, &low_locator, vec![fixture_row(fixture, low_key.clone())]);
            source.add(side, &high_locator, vec![fixture_row(fixture, high_key.clone())]);
        }

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(40, 1, 4, b"base-low", b"base-high"),
            None,
        );
        let mut left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(41, 2, 5, b"left-low", b"left-high"),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(42, 3, 6, b"right-low", b"right-high"),
            None,
        );

        let agreed_delete_id = b"consumer/a-agreed-delete".to_vec();
        base.checkpoints.insert(
            agreed_delete_id.clone(),
            CheckpointGeneration { generation: 2, position: None },
        );

        let delete_against_unchanged_id = b"consumer/b-delete-against-unchanged".to_vec();
        let retained_checkpoint = CheckpointGeneration { generation: 3, position: None };
        base.checkpoints.insert(
            delete_against_unchanged_id.clone(),
            retained_checkpoint.clone(),
        );
        let unchanged_side = if delete_on_left { &mut right } else { &mut left };
        unchanged_side
            .checkpoints
            .insert(delete_against_unchanged_id.clone(), retained_checkpoint);

        let first_conflict_id = b"consumer/m-positionless-delete-update".to_vec();
        base.checkpoints.insert(
            first_conflict_id.clone(),
            CheckpointGeneration { generation: 4, position: None },
        );
        let update_side = if delete_on_left { &mut right } else { &mut left };
        update_side.checkpoints.insert(
            first_conflict_id.clone(),
            CheckpointGeneration { generation: 5, position: None },
        );

        let opposite_conflict_id = b"consumer/n-opposite-delete-update".to_vec();
        base.checkpoints.insert(
            opposite_conflict_id.clone(),
            CheckpointGeneration { generation: 6, position: None },
        );
        let opposite_update_side = if delete_on_left { &mut left } else { &mut right };
        opposite_update_side.checkpoints.insert(
            opposite_conflict_id.clone(),
            CheckpointGeneration { generation: 7, position: None },
        );

        let later_conflict_id = b"consumer/z-unvisited".to_vec();
        for (snapshot, generation) in [(&mut base, 7), (&mut left, 8), (&mut right, 9)] {
            snapshot.checkpoints.insert(
                later_conflict_id.clone(),
                CheckpointGeneration { generation, position: None },
            );
        }

        // MERGE-011 treats deletion and an independently advanced positionless
        // checkpoint as divergent state. These two ordered IDs delete on
        // opposite branches. Storage finishes the fixture ranges first, then
        // zero budget retains only the first ID and leaves the opposite tail
        // unvisited.
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the first positionless delete/update checkpoint exceeds zero budget")
        };
        assert_eq!(report.conflicts_lower_bound, 1);
        assert_eq!(report.rows_examined, 6);
        assert!(report.affected_ranges.contains(&(id(1), low_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
        assert!(report.affected_checkpoints.contains(first_conflict_id.as_slice()));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(!report.affected_checkpoints.contains(opposite_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report
            .affected_checkpoints
            .contains(delete_against_unchanged_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(later_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
    }
}

#[test]
fn segmented_checkpoint_delete_update_impacts_cross_the_shared_budget_boundary() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let (low_key, high_key) = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        (candidate_a, candidate_b)
    } else {
        (candidate_b, candidate_a)
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |digest, low_digest, high_digest, low_locator: &[u8], high_locator: &[u8]| {
        TableManifest {
            digest: [digest; 32],
            segments: vec![
                RowSegmentManifest {
                    locator: low_locator.to_vec(),
                    range: low_range.clone(),
                    digest: [low_digest; 32],
                },
                RowSegmentManifest {
                    locator: high_locator.to_vec(),
                    range: high_range.clone(),
                    digest: [high_digest; 32],
                },
            ],
        }
    };
    let fixture_row = |fixture: &str, key: CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key;
        row
    };
    let checkpoint = |generation, token: &[u8]| CheckpointGeneration {
        generation,
        position: Some(token.to_vec()),
    };
    let build_inputs = |delete_on_left: bool| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-low", vec![fixture_row(BASE, low_key.clone())]);
        source.add(MergeSide::Left, b"left-low", vec![fixture_row(LEFT, low_key.clone())]);
        source.add(MergeSide::Right, b"right-low", vec![fixture_row(CONFLICT, low_key.clone())]);
        source.add(MergeSide::Base, b"base-high", vec![fixture_row(BASE, high_key.clone())]);
        source.add(MergeSide::Left, b"left-high", vec![fixture_row(LEFT, high_key.clone())]);
        source.add(MergeSide::Right, b"right-high", vec![fixture_row(CONFLICT, high_key.clone())]);

        let left_checkpoint = (!delete_on_left).then(|| checkpoint(5, b"left-token"));
        let right_checkpoint = delete_on_left.then(|| checkpoint(6, b"right-token"));
        let base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(10, 1, 4, b"base-low", b"base-high"),
            Some(checkpoint(4, b"base-token")),
        );
        let left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(11, 2, 5, b"left-low", b"left-high"),
            left_checkpoint,
        );
        let right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(12, 3, 6, b"right-low", b"right-high"),
            right_checkpoint,
        );
        (base, left, right, source)
    };

    // The reference requires affected-range evidence and a conflict lower
    // bound, but leaves cross-phase traversal and checkpoint-impact summaries
    // open. Resolve ordered ranges before bytewise checkpoint IDs for stable
    // impact tails across both delete/update orientations.
    for delete_on_left in [true, false] {
        for max_conflicts in [0, 2, 3] {
            let (base, left, right, mut source) = build_inputs(delete_on_left);
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts },
            )
            .unwrap_err();
            let (report, exact_conflicts) = match (max_conflicts, error) {
                (0, BranchMergeError::BudgetExceeded { report }) => (report, None),
                (2, BranchMergeError::BudgetExceeded { report }) => (report, None),
                (3, BranchMergeError::Conflicts { conflicts, report }) => {
                    assert_eq!(conflicts.len(), 3);
                    assert!(matches!(conflicts[0], BranchMergeConflict::Row { .. }));
                    assert!(matches!(conflicts[1], BranchMergeConflict::Row { .. }));
                    let BranchMergeConflict::CheckpointConflict { id: checkpoint_id, conflict } =
                        &conflicts[2]
                    else {
                        panic!("checkpoint tail follows both segmented row conflicts")
                    };
                    assert_eq!(checkpoint_id.as_slice(), b"consumer/source");
                    assert!(conflict.base.is_some());
                    assert_eq!(conflict.left.is_none(), delete_on_left);
                    assert_eq!(conflict.right.is_none(), !delete_on_left);
                    (report, Some(conflicts))
                }
                (_, error) => panic!("unexpected budget result: {error:?}"),
            };
            assert_eq!(report.conflicts_lower_bound, if max_conflicts == 0 { 1 } else { 3 });
            assert_eq!(report.rows_examined, if max_conflicts == 0 { 3 } else { 6 });
            assert!(report.affected_tables.contains(&id(1)));
            assert!(report.affected_ranges.contains(&(id(1), low_range.clone())));
            if max_conflicts == 0 {
                assert!(!report.affected_ranges.contains(&(id(1), high_range.clone())));
                assert!(report.affected_checkpoints.is_empty());
                assert_eq!(source.visited.len(), 3);
            } else {
                assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
                assert!(report.affected_checkpoints.contains(b"consumer/source".as_slice()));
                assert_eq!(source.visited.len(), 6);
            }
            if max_conflicts < 3 {
                assert!(exact_conflicts.is_none());
            } else {
                assert!(exact_conflicts.is_some());
            }
        }
    }

    for delete_on_left in [true, false] {
        for max_conflicts in [2, 3] {
            let (mut base, mut left, mut right, mut source) = build_inputs(delete_on_left);
            base.checkpoints.clear();
            left.checkpoints.clear();
            right.checkpoints.clear();

            let clean_id = b"consumer/a-clean".to_vec();
            let clean = checkpoint(1, b"clean-token");
            for snapshot in [&mut base, &mut left, &mut right] {
                snapshot.checkpoints.insert(clean_id.clone(), clean.clone());
            }

            let agreed_delete_id = b"consumer/b-agreed-delete".to_vec();
            base.checkpoints.insert(agreed_delete_id.clone(), checkpoint(2, b"deleted"));

            let unchanged_delete_id = b"consumer/c-delete-against-unchanged".to_vec();
            let unchanged = checkpoint(3, b"unchanged");
            base.checkpoints.insert(unchanged_delete_id.clone(), unchanged.clone());
            let unchanged_side = if delete_on_left { &mut right } else { &mut left };
            unchanged_side
                .checkpoints
                .insert(unchanged_delete_id.clone(), unchanged);

            let crossing_id = b"consumer/m-delete-update".to_vec();
            base.checkpoints.insert(crossing_id.clone(), checkpoint(4, b"base-token"));
            let edited_side = if delete_on_left { &mut right } else { &mut left };
            edited_side.checkpoints.insert(crossing_id.clone(), checkpoint(5, b"edited-token"));

            let tail_id = b"consumer/z-tail".to_vec();
            base.checkpoints.insert(tail_id.clone(), checkpoint(7, b"z-base"));
            left.checkpoints.insert(tail_id.clone(), checkpoint(8, b"z-left"));
            right.checkpoints.insert(tail_id.clone(), checkpoint(9, b"z-right"));

            // Equal and agreed-delete checkpoints are clean impacts. Once the
            // middle delete/update crosses the segment-conflict budget, the
            // later ID remains unvisited. With one more detail slot, the later
            // conflict itself becomes the crossing impact.
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts },
            )
            .unwrap_err();
            let report = match (max_conflicts, error) {
                (2 | 3, BranchMergeError::BudgetExceeded { report }) => report,
                (_, error) => panic!("unexpected budget result: {error:?}"),
            };
            assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
            assert_eq!(report.rows_examined, 6);
            assert!(report.affected_ranges.contains(&(id(1), low_range.clone())));
            assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
            assert_eq!(report.affected_checkpoints.len(), max_conflicts - 1);
            assert!(report.affected_checkpoints.contains(crossing_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(clean_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
            assert_eq!(
                report.affected_checkpoints.contains(tail_id.as_slice()),
                max_conflicts == 3
            );
            assert_eq!(source.visited.len(), 6);
        }
    }
}

#[test]
fn disjoint_segment_edits_merge_with_positionless_checkpoint_agreement_and_deletes() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let (low_key, high_key) = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        (candidate_a, candidate_b)
    } else {
        (candidate_b, candidate_a)
    };
    let low_boundary = low_key.encode().unwrap();
    let high_boundary = high_key.encode().unwrap();
    let shared_range = KeyRange::new(None, Some(low_boundary.clone())).unwrap();
    let lower_range = KeyRange::new(Some(low_boundary), Some(high_boundary.clone())).unwrap();
    let upper_range = KeyRange::new(Some(high_boundary), None).unwrap();
    let split_manifest = |digest, lower_digest, upper_digest, locators: [&[u8]; 3]| TableManifest {
        digest: [digest; 32],
        segments: vec![
            RowSegmentManifest {
                locator: locators[0].to_vec(),
                range: shared_range.clone(),
                digest: [7; 32],
            },
            RowSegmentManifest {
                locator: locators[1].to_vec(),
                range: lower_range.clone(),
                digest: [lower_digest; 32],
            },
            RowSegmentManifest {
                locator: locators[2].to_vec(),
                range: upper_range.clone(),
                digest: [upper_digest; 32],
            },
        ],
    };
    let row_with_key = |fixture: &str, key: CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key;
        row
    };
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base-lower", vec![row_with_key(BASE, low_key.clone())]);
    source.add(MergeSide::Left, b"left-lower", vec![row_with_key(LEFT, low_key.clone())]);
    source.add(MergeSide::Right, b"right-lower", vec![row_with_key(RIGHT, low_key.clone())]);
    source.add(MergeSide::Base, b"base-upper", vec![row_with_key(BASE, high_key.clone())]);
    source.add(MergeSide::Left, b"left-upper", vec![row_with_key(LEFT, high_key.clone())]);
    source.add(MergeSide::Right, b"right-upper", vec![row_with_key(RIGHT, high_key)]);

    let base_checkpoint = CheckpointGeneration { generation: 4, position: Some(b"base-token".to_vec()) };
    let left_checkpoint = CheckpointGeneration { generation: 5, position: Some(b"left-token".to_vec()) };
    let deleted_checkpoint_id = b"consumer/deleted".to_vec();
    let deleted_checkpoint = CheckpointGeneration { generation: 7, position: Some(b"delete-base".to_vec()) };
    let jointly_deleted_id = b"consumer/joint-delete".to_vec();
    let jointly_deleted = CheckpointGeneration { generation: 8, position: Some(b"joint-delete-base".to_vec()) };
    let positionless_joint_delete_id = b"consumer/joint-positionless-delete".to_vec();
    let positionless_joint_delete = CheckpointGeneration { generation: 10, position: None };
    let positionless_one_side_delete_id = b"consumer/one-side-positionless-delete".to_vec();
    let positionless_one_side_delete = CheckpointGeneration { generation: 11, position: None };
    let positionless_agreement_id = b"consumer/shared-add".to_vec();
    let positionless_agreement = CheckpointGeneration { generation: 9, position: None };
    let positionless_reset_id = b"consumer/positionless-reset".to_vec();
    let positionless_reset_base = CheckpointGeneration { generation: 10, position: Some(b"prior-token".to_vec()) };
    let positionless_reset = CheckpointGeneration { generation: 11, position: None };
    let mut base = snapshot(
        schema(true, FieldType::Str),
        split_manifest(10, 1, 4, [b"base-shared", b"base-lower", b"base-upper"]),
        Some(base_checkpoint.clone()),
    );
    base.checkpoints.insert(deleted_checkpoint_id.clone(), deleted_checkpoint.clone());
    base.checkpoints.insert(jointly_deleted_id.clone(), jointly_deleted);
    base.checkpoints.insert(positionless_joint_delete_id.clone(), positionless_joint_delete);
    base.checkpoints.insert(
        positionless_one_side_delete_id.clone(),
        positionless_one_side_delete.clone(),
    );
    base.checkpoints.insert(positionless_reset_id.clone(), positionless_reset_base);
    let mut left = snapshot(
        schema(true, FieldType::Str),
        split_manifest(11, 2, 5, [b"left-shared", b"left-lower", b"left-upper"]),
        Some(left_checkpoint.clone()),
    );
    left.checkpoints.insert(positionless_agreement_id.clone(), positionless_agreement.clone());
    left.checkpoints.insert(positionless_reset_id.clone(), positionless_reset.clone());
    let mut right = snapshot(
        schema(true, FieldType::Str),
        split_manifest(12, 3, 6, [b"right-shared", b"right-lower", b"right-upper"]),
        Some(left_checkpoint.clone()),
    );
    right.checkpoints.insert(deleted_checkpoint_id.clone(), deleted_checkpoint);
    right.checkpoints.insert(
        positionless_one_side_delete_id.clone(),
        positionless_one_side_delete,
    );
    right.checkpoints.insert(positionless_agreement_id.clone(), positionless_agreement.clone());
    right.checkpoints.insert(positionless_reset_id.clone(), positionless_reset.clone());

    // Equal digests reuse the untouched first segment; independent row edits,
    // agreed positionless addition/reset, and unilateral/shared deletes reconcile together.
    // A present checkpoint with no opaque position stays distinct from deletion;
    // deleting it resolves when both sides agree or one side leaves it unchanged.
    // A reset may retain its generation while clearing the position: that remains
    // a present checkpoint value, distinct from deleting the checkpoint altogether.
    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    assert_eq!(plan.report.conflicts_lower_bound, 0);
    assert_eq!(plan.report.rows_examined, 6);
    assert_eq!(source.visited.len(), 6);
    assert!(source.visited.iter().all(|(_, locator)| !locator.ends_with(b"shared")));
    let segments = &plan.tables[&id(1)].segments;
    assert_eq!(segments.len(), 3);
    assert!(matches!(segments[0], MergedSegment::Reuse { from: MergeSide::Left, .. }));
    for segment in &segments[1..] {
        let MergedSegment::Rows { rows, .. } = segment else { panic!("changed segments are merged by row") };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fields[&id(2)], string("Grace"));
        assert_eq!(rows[0].fields[&id(3)], string("Paris"));
    }
    assert_eq!(plan.checkpoints[b"consumer/source".as_slice()], left_checkpoint);
    assert!(!plan.checkpoints.contains_key(deleted_checkpoint_id.as_slice()));
    assert!(!plan.checkpoints.contains_key(jointly_deleted_id.as_slice()));
    assert!(!plan.checkpoints.contains_key(positionless_joint_delete_id.as_slice()));
    assert!(!plan.checkpoints.contains_key(positionless_one_side_delete_id.as_slice()));
    assert_eq!(plan.checkpoints[positionless_agreement_id.as_slice()], positionless_agreement);
    assert_eq!(plan.checkpoints[positionless_reset_id.as_slice()], positionless_reset);
}

#[test]
fn positionless_checkpoint_delete_vs_change_conflicts_in_both_orientations() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);

    let mut base = snapshot(schema(true, FieldType::Str), manifest(20, 20, b"base"), None);
    let mut left = snapshot(schema(true, FieldType::Str), manifest(21, 21, b"left"), None);
    let mut right = snapshot(schema(true, FieldType::Str), manifest(22, 22, b"right"), None);

    // A cursorless checkpoint remains present state. Deleting it on one side
    // while the other side changes its generation or cursor is divergent.
    let generation_id = b"consumer/positionless-delete-vs-generation".to_vec();
    let generation_base = CheckpointGeneration { generation: 40, position: None };
    let generation_update = CheckpointGeneration { generation: 41, position: None };
    base.checkpoints.insert(generation_id.clone(), generation_base.clone());
    right.checkpoints.insert(generation_id.clone(), generation_update.clone());

    let cursor_id = b"consumer/positionless-cursor-vs-delete".to_vec();
    let cursor_base = CheckpointGeneration { generation: 50, position: None };
    let cursor_update = CheckpointGeneration { generation: 50, position: Some(b"cursor".to_vec()) };
    base.checkpoints.insert(cursor_id.clone(), cursor_base.clone());
    left.checkpoints.insert(cursor_id.clone(), cursor_update.clone());

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("deleting a cursorless checkpoint against a changed value must conflict")
    };
    assert_eq!(report.conflicts_lower_bound, 2);
    assert_eq!(source.visited.len(), 3);
    assert!(report.affected_checkpoints.contains(generation_id.as_slice()));
    assert!(report.affected_checkpoints.contains(cursor_id.as_slice()));

    let checkpoint_conflicts: BTreeMap<_, _> = conflicts
        .into_iter()
        .map(|conflict| match conflict {
            BranchMergeConflict::CheckpointConflict { id, conflict } => (id, conflict),
            other => panic!("fixture row edits should merge cleanly: {other:?}"),
        })
        .collect();
    assert_eq!(checkpoint_conflicts.len(), 2);

    let generation_conflict = &checkpoint_conflicts[generation_id.as_slice()];
    assert_eq!(generation_conflict.base.as_ref(), Some(&generation_base));
    assert_eq!(generation_conflict.left, None);
    assert_eq!(generation_conflict.right.as_ref(), Some(&generation_update));

    let cursor_conflict = &checkpoint_conflicts[cursor_id.as_slice()];
    assert_eq!(cursor_conflict.base.as_ref(), Some(&cursor_base));
    assert_eq!(cursor_conflict.left.as_ref(), Some(&cursor_update));
    assert_eq!(cursor_conflict.right, None);
}

#[test]
fn agreed_positionless_deletes_do_not_spend_budget_after_a_row_conflict() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);

    let mut base = snapshot(schema(true, FieldType::Str), manifest(30, 30, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(31, 31, b"left"), None);
    let mut right = snapshot(schema(true, FieldType::Str), manifest(32, 32, b"right"), None);

    let jointly_deleted_id = b"consumer/positionless-joint-delete".to_vec();
    let jointly_deleted = CheckpointGeneration { generation: 60, position: None };
    base.checkpoints.insert(jointly_deleted_id.clone(), jointly_deleted);

    let one_side_deleted_id = b"consumer/positionless-one-side-delete".to_vec();
    let one_side_deleted = CheckpointGeneration { generation: 70, position: None };
    base.checkpoints.insert(one_side_deleted_id.clone(), one_side_deleted.clone());
    right.checkpoints.insert(one_side_deleted_id, one_side_deleted);

    // An absent map entry is a deletion, while a present generation with no
    // provider cursor is still state. Agreed deletes are clean even when the
    // unrelated row phase consumes the entire conflict-detail budget.
    let one_conflict_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_conflict_budget)
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("agreed positionless deletes must not exhaust the row conflict budget")
    };
    assert!(matches!(conflicts.as_slice(), [BranchMergeConflict::Row { .. }]));
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_checkpoints.is_empty());
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert_eq!(source.visited.len(), 3);
}

#[test]
fn positionless_checkpoint_delete_stays_clean_beside_row_delete_update() {
    for delete_on_left in [true, false] {
        for max_conflicts in [0, 1] {
            let mut source = FixtureRows::default();
            let base_row = parse_fixture(BASE, RowKeyKind::Explicit);
            let edited_row = parse_fixture(RIGHT, RowKeyKind::Explicit);
            source.add(MergeSide::Base, b"base", vec![base_row]);
            source.add(
                MergeSide::Left,
                b"left",
                if delete_on_left { Vec::new() } else { vec![edited_row.clone()] },
            );
            source.add(
                MergeSide::Right,
                b"right",
                if delete_on_left { vec![edited_row] } else { Vec::new() },
            );

            let mut base = snapshot(schema(true, FieldType::Str), manifest(40, 40, b"base"), None);
            let mut left = snapshot(schema(true, FieldType::Str), manifest(41, 41, b"left"), None);
            let mut right = snapshot(schema(true, FieldType::Str), manifest(42, 42, b"right"), None);
            let checkpoint_id = b"consumer/positionless-delete-with-row-delete-update".to_vec();
            let checkpoint = CheckpointGeneration { generation: 80, position: None };
            base.checkpoints.insert(checkpoint_id.clone(), checkpoint.clone());

            // Put the checkpoint deletion opposite the row deletion. Its other
            // branch keeps the cursorless generation unchanged, so it resolves
            // cleanly once planning reaches checkpoints.
            let (delete_checkpoint_side, retain_checkpoint_side) = if delete_on_left {
                (&mut right, &mut left)
            } else {
                (&mut left, &mut right)
            };
            delete_checkpoint_side.checkpoints.remove(checkpoint_id.as_slice());
            retain_checkpoint_side.checkpoints.insert(checkpoint_id.clone(), checkpoint);

            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts },
            )
            .unwrap_err();

            match (max_conflicts, error) {
                (0, BranchMergeError::BudgetExceeded { report }) => {
                    assert_eq!(report.conflicts_lower_bound, 1);
                    assert!(report.affected_checkpoints.is_empty());
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                }
                (1, BranchMergeError::Conflicts { conflicts, report }) => {
                    assert!(matches!(
                        conflicts.as_slice(),
                        [BranchMergeConflict::Row {
                            conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                            ..
                        }] if key == &integer(1)
                    ));
                    assert_eq!(report.conflicts_lower_bound, 1);
                    assert!(report.affected_checkpoints.is_empty());
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                }
                (_, error) => panic!("unexpected row tombstone/checkpoint delete result: {error:?}"),
            }
            assert_eq!(source.visited.len(), 3);
        }
    }
}

#[test]
fn positionless_deletes_resolve_at_exact_row_change_budget_boundary() {
    let build_inputs = || {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
        source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
        source.add(MergeSide::Right, b"right", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);

        let mut base = snapshot(schema(true, FieldType::Str), manifest(40, 40, b"base"), None);
        let left = snapshot(schema(true, FieldType::Str), manifest(41, 41, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(42, 42, b"right"), None);

        let jointly_deleted_id = b"consumer/row-change-joint-positionless-delete".to_vec();
        base.checkpoints.insert(
            jointly_deleted_id,
            CheckpointGeneration { generation: 80, position: None },
        );
        let one_side_deleted_id = b"consumer/row-change-one-side-positionless-delete".to_vec();
        let one_side_deleted = CheckpointGeneration { generation: 90, position: None };
        base.checkpoints.insert(one_side_deleted_id.clone(), one_side_deleted.clone());
        right.checkpoints.insert(one_side_deleted_id, one_side_deleted);

        (base, left, right, source)
    };

    // Below the exact three-row materialization budget, planning stops before
    // the checkpoint phase. At the boundary, independent fixture row edits
    // merge and both agreed/one-sided cursorless deletes resolve cleanly.
    let (base, left, right, mut source) = build_inputs();
    let below_exact = BranchMergeBudget { max_rows_examined: 2, max_conflicts: 0 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, below_exact).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("row materialization below the exact boundary must stop planning")
    };
    assert_eq!(report.rows_examined, 3);
    assert_eq!(report.conflicts_lower_bound, 0);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.is_empty());

    let (base, left, right, mut source) = build_inputs();
    let exact_budget = BranchMergeBudget { max_rows_examined: 3, max_conflicts: 0 };
    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_budget).unwrap();
    assert_eq!(plan.report.rows_examined, 3);
    assert_eq!(plan.report.conflicts_lower_bound, 0);
    assert_eq!(source.visited.len(), 3);
    let MergedSegment::Rows { rows, .. } = &plan.tables[&id(1)].segments[0] else {
        panic!("independent fixture row edits materialize a merged row")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fields[&id(2)], string("Grace"));
    assert_eq!(rows[0].fields[&id(3)], string("Paris"));
    assert!(plan.checkpoints.is_empty());
}

#[test]
fn positionless_deletes_wait_for_segment_boundary_row_budget() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let (low_key, high_key) = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        (candidate_a, candidate_b)
    } else {
        (candidate_b, candidate_a)
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |digest, low_digest, high_digest, low_locator: &[u8], high_locator: &[u8]| {
        TableManifest {
            digest: [digest; 32],
            segments: vec![
                RowSegmentManifest {
                    locator: low_locator.to_vec(),
                    range: low_range.clone(),
                    digest: [low_digest; 32],
                },
                RowSegmentManifest {
                    locator: high_locator.to_vec(),
                    range: high_range.clone(),
                    digest: [high_digest; 32],
                },
            ],
        }
    };
    let fixture_row = |fixture: &str, key: CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key;
        row
    };
    let build_inputs = || {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-low", vec![fixture_row(BASE, low_key.clone())]);
        source.add(MergeSide::Left, b"left-low", vec![fixture_row(LEFT, low_key.clone())]);
        source.add(MergeSide::Right, b"right-low", vec![fixture_row(RIGHT, low_key.clone())]);
        source.add(MergeSide::Base, b"base-high", vec![fixture_row(BASE, high_key.clone())]);
        source.add(MergeSide::Left, b"left-high", vec![fixture_row(LEFT, high_key.clone())]);
        source.add(MergeSide::Right, b"right-high", vec![fixture_row(RIGHT, high_key.clone())]);

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(50, 1, 4, b"base-low", b"base-high"),
            None,
        );
        let left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(51, 2, 5, b"left-low", b"left-high"),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(52, 3, 6, b"right-low", b"right-high"),
            None,
        );

        base.checkpoints.insert(
            b"consumer/segment-joint-positionless-delete".to_vec(),
            CheckpointGeneration { generation: 100, position: None },
        );
        let one_side_id = b"consumer/segment-one-side-positionless-delete".to_vec();
        let one_side_checkpoint = CheckpointGeneration { generation: 110, position: None };
        base.checkpoints.insert(one_side_id.clone(), one_side_checkpoint.clone());
        right.checkpoints.insert(one_side_id, one_side_checkpoint);

        (base, left, right, source)
    };

    // The high segment owns the inclusive split key. A row-budget stop at that
    // boundary returns its range impact before checkpoint deletes are visited.
    let (base, left, right, mut source) = build_inputs();
    let below_exact = BranchMergeBudget { max_rows_examined: 5, max_conflicts: 0 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, below_exact).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the row budget must stop inside the upper segment")
    };
    assert_eq!(report.rows_examined, 6);
    assert_eq!(report.conflicts_lower_bound, 0);
    assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
    assert!(report.affected_checkpoints.is_empty());

    let (base, left, right, mut source) = build_inputs();
    let exact_budget = BranchMergeBudget { max_rows_examined: 6, max_conflicts: 0 };
    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_budget).unwrap();
    assert_eq!(plan.report.rows_examined, 6);
    assert_eq!(source.visited.len(), 6);
    let segments = &plan.tables[&id(1)].segments;
    assert_eq!(segments.len(), 2);
    for segment in segments {
        let MergedSegment::Rows { rows, .. } = segment else {
            panic!("both changed key ranges materialize from their fixtures")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fields[&id(2)], string("Grace"));
        assert_eq!(rows[0].fields[&id(3)], string("Paris"));
    }
    assert!(plan.checkpoints.is_empty());
}

#[test]
fn positionless_checkpoint_delete_resolves_with_upper_segment_tombstone() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let high_key = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        candidate_b
    } else {
        candidate_a
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |table_digest, upper_digest, upper_locator: &[u8]| TableManifest {
        digest: [table_digest; 32],
        segments: vec![
            RowSegmentManifest {
                locator: b"shared-lower".to_vec(),
                range: low_range.clone(),
                digest: [7; 32],
            },
            RowSegmentManifest {
                locator: upper_locator.to_vec(),
                range: high_range.clone(),
                digest: [upper_digest; 32],
            },
        ],
    };
    let mut deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    deleted_row.key = high_key.clone();

    let build_inputs = || {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-upper", vec![deleted_row.clone()]);
        source.add(MergeSide::Left, b"left-upper", Vec::new());
        source.add(MergeSide::Right, b"right-upper", vec![deleted_row.clone()]);

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(60, 10, b"base-upper"),
            None,
        );
        let left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(61, 11, b"left-upper"),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(62, 12, b"right-upper"),
            None,
        );

        let checkpoint_id = b"consumer/upper-segment-positionless-delete".to_vec();
        let checkpoint = CheckpointGeneration { generation: 120, position: None };
        base.checkpoints.insert(checkpoint_id.clone(), checkpoint.clone());
        right.checkpoints.insert(checkpoint_id, checkpoint);
        (base, left, right, source)
    };

    // The deleted row is exactly the inclusive lower edge of the upper range.
    // A zero row budget reports that active range on its first fixture row and
    // never reaches the checkpoint delete. The exact boundary then proves the
    // row tombstone and cursorless checkpoint delete coexist in a complete plan.
    let (base, left, right, mut source) = build_inputs();
    let zero_budget = BranchMergeBudget { max_rows_examined: 0, max_conflicts: 0 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, zero_budget).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("zero row budget must stop before the tombstone or checkpoint phase")
    };
    assert_eq!(report.rows_examined, 1);
    assert_eq!(report.conflicts_lower_bound, 0);
    assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
    assert!(report.affected_checkpoints.is_empty());
    assert_eq!(source.visited, vec![(MergeSide::Base, b"base-upper".to_vec())]);

    let (base, left, right, mut source) = build_inputs();
    let short_budget = BranchMergeBudget { max_rows_examined: 1, max_conflicts: 0 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, short_budget).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the upper-segment row scan must stop at the first row over budget")
    };
    assert_eq!(report.rows_examined, 2);
    assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
    assert!(report.affected_checkpoints.is_empty());

    let (base, left, right, mut source) = build_inputs();
    let exact_budget = BranchMergeBudget { max_rows_examined: 2, max_conflicts: 0 };
    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_budget).unwrap();
    assert_eq!(plan.report.rows_examined, 2);
    assert_eq!(source.visited.len(), 3);
    assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"upper")));
    let segments = &plan.tables[&id(1)].segments;
    assert_eq!(segments.len(), 2);
    assert!(matches!(segments[0], MergedSegment::Reuse { from: MergeSide::Left, .. }));
    let MergedSegment::Rows { rows, tombstones, .. } = &segments[1] else {
        panic!("the changed upper segment materializes a tombstone")
    };
    assert!(rows.is_empty());
    assert_eq!(tombstones, &[high_key]);
    assert!(plan.checkpoints.is_empty());
}

#[test]
fn conflict_budget_closes_fixture_row_tombstone_checkpoint_tail() {
    let mut keys = [integer(10), integer(20), integer(30)];
    keys.sort_by_key(|key| key.encode().unwrap());
    let [first_conflict_key, second_conflict_key, tombstone_key] = keys;
    let first_boundary = second_conflict_key.encode().unwrap();
    let second_boundary = tombstone_key.encode().unwrap();
    let ranges = [
        KeyRange::new(None, Some(first_boundary.clone())).unwrap(),
        KeyRange::new(Some(first_boundary), Some(second_boundary.clone())).unwrap(),
        KeyRange::new(Some(second_boundary), None).unwrap(),
    ];
    let split_manifest = |table_digest: u8, digests: [u8; 3], locators: [&[u8]; 3]| TableManifest {
        digest: [table_digest; 32],
        segments: (0..3)
            .map(|index| RowSegmentManifest {
                locator: locators[index].to_vec(),
                range: ranges[index].clone(),
                digest: [digests[index]; 32],
            })
            .collect(),
    };
    let fixture_row = |fixture: &str, key: &CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key.clone();
        row
    };
    let build_inputs = || {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-first", vec![fixture_row(BASE, &first_conflict_key)]);
        source.add(MergeSide::Left, b"left-first", Vec::new());
        source.add(MergeSide::Right, b"right-first", vec![fixture_row(RIGHT, &first_conflict_key)]);
        source.add(MergeSide::Base, b"base-second", vec![fixture_row(BASE, &second_conflict_key)]);
        source.add(MergeSide::Left, b"left-second", vec![fixture_row(LEFT, &second_conflict_key)]);
        source.add(MergeSide::Right, b"right-second", Vec::new());
        source.add(MergeSide::Base, b"base-tombstone", vec![fixture_row(BASE, &tombstone_key)]);
        source.add(MergeSide::Left, b"left-tombstone", Vec::new());
        source.add(MergeSide::Right, b"right-tombstone", Vec::new());

        let base_locators = [b"base-first".as_slice(), b"base-second".as_slice(), b"base-tombstone".as_slice()];
        let left_locators = [b"left-first".as_slice(), b"left-second".as_slice(), b"left-tombstone".as_slice()];
        let right_locators = [b"right-first".as_slice(), b"right-second".as_slice(), b"right-tombstone".as_slice()];
        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(90, [30, 31, 32], base_locators),
            None,
        );
        let mut left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(91, [40, 41, 42], left_locators),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(92, [50, 51, 52], right_locators),
            None,
        );

        let delete_update_id = b"consumer/m-delete-update".to_vec();
        base.checkpoints.insert(delete_update_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        right.checkpoints.insert(delete_update_id.clone(), parse_checkpoint_fixture(CHECKPOINT_EDITED));

        let divergent_id = b"consumer/z-divergent-tail".to_vec();
        base.checkpoints.insert(divergent_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(divergent_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(divergent_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));

        // The final checkpoint is a clean suffix after the fixture conflicts.
        let closure_id = b"consumer/zz-clean-closure".to_vec();
        for snapshot in [&mut base, &mut left, &mut right] {
            snapshot.checkpoints.insert(closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        }
        (base, left, right, source, delete_update_id, divergent_id, closure_id)
    };

    // ORNA-MERGE-005 requires bounded evidence but does not prescribe ordering.
    // Storage's local policy resolves every changed row range (including clean
    // tombstones) before checkpoint identities, then stops at the first detail
    // beyond the shared budget. Exact capacity completes the conflict tail.
    let segment_visits = [
        [
            (MergeSide::Base, b"base-first".to_vec()),
            (MergeSide::Left, b"left-first".to_vec()),
            (MergeSide::Right, b"right-first".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-second".to_vec()),
            (MergeSide::Left, b"left-second".to_vec()),
            (MergeSide::Right, b"right-second".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-tombstone".to_vec()),
            (MergeSide::Left, b"left-tombstone".to_vec()),
            (MergeSide::Right, b"right-tombstone".to_vec()),
        ],
    ];
    for max_conflicts in 0..=4 {
        let (base, left, right, mut source, delete_update_id, divergent_id, closure_id) = build_inputs();
        let result = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts },
        );
        let reached_segments = (max_conflicts + 1).min(3);
        let expected_visits = segment_visits
            .iter()
            .take(reached_segments)
            .flat_map(|visits| visits.iter().cloned())
            .collect::<Vec<_>>();
        assert_eq!(source.visited, expected_visits);

        let expected_rows = [2, 4, 5, 5, 5][max_conflicts];
        let expected_ranges = reached_segments;
        let expected_checkpoints = max_conflicts.saturating_sub(1).min(2);
        if max_conflicts < 4 {
            let BranchMergeError::BudgetExceeded { report } = result.unwrap_err() else {
                panic!("the next row or checkpoint conflict must stop beyond the configured budget")
            };
            assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
            assert_eq!(report.rows_examined, expected_rows);
            assert_eq!(report.affected_ranges.len(), expected_ranges);
            for range in ranges.iter().take(expected_ranges) {
                assert!(report.affected_ranges.contains(&(id(1), range.clone())));
            }
            assert_eq!(report.affected_checkpoints.len(), expected_checkpoints);
            assert_eq!(report.affected_checkpoints.contains(delete_update_id.as_slice()), max_conflicts >= 2);
            assert_eq!(report.affected_checkpoints.contains(divergent_id.as_slice()), max_conflicts >= 3);
            assert!(!report.affected_checkpoints.contains(closure_id.as_slice()));
        } else {
            let BranchMergeError::Conflicts { conflicts, report } = result.unwrap_err() else {
                panic!("the exact four-conflict budget closes after the tombstone and checkpoint tail")
            };
            assert_eq!(conflicts.len(), 4);
            assert!(matches!(
                &conflicts[0],
                BranchMergeConflict::Row {
                    conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                    ..
                } if key == &first_conflict_key
            ));
            assert!(matches!(
                &conflicts[1],
                BranchMergeConflict::Row {
                    conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                    ..
                } if key == &second_conflict_key
            ));
            assert_eq!(
                &conflicts[2],
                &BranchMergeConflict::CheckpointConflict {
                    id: delete_update_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                        left: None,
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_EDITED)),
                    },
                }
            );
            assert_eq!(
                &conflicts[3],
                &BranchMergeConflict::CheckpointConflict {
                    id: divergent_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                    },
                }
            );
            assert_eq!(report.conflicts_lower_bound, 4);
            assert_eq!(report.rows_examined, expected_rows);
            assert_eq!(report.affected_ranges.len(), 3);
            assert_eq!(report.affected_checkpoints.len(), 2);
            assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
            assert!(report.affected_checkpoints.contains(divergent_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(closure_id.as_slice()));
        }
    }

    // A row budget ending one row before the clean tombstone cannot enter the
    // checkpoint phase, even though the conflict budget still has capacity.
    // At the exact five-row boundary, the tombstone resolves and both fixture
    // checkpoint conflicts close within the exact four-conflict budget.
    let (base, left, right, mut source, _, _, _) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 4, max_conflicts: 4 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the row limit stops before the tombstone and checkpoint closure")
    };
    assert_eq!(report.rows_examined, 5);
    assert_eq!(report.conflicts_lower_bound, 2);
    assert_eq!(report.affected_ranges.len(), 3);
    assert!(report.affected_checkpoints.is_empty());
    let mut visits_before_tombstone = segment_visits
        .iter()
        .take(2)
        .flat_map(|visits| visits.iter().cloned())
        .collect::<Vec<_>>();
    visits_before_tombstone.push((MergeSide::Base, b"base-tombstone".to_vec()));
    assert_eq!(source.visited, visits_before_tombstone);

    let (base, left, right, mut source, delete_update_id, divergent_id, _) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 5, max_conflicts: 4 },
    )
    .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("the exact row and conflict budgets close the fixture tail")
    };
    assert_eq!(conflicts.len(), 4);
    assert_eq!(report.rows_examined, 5);
    assert_eq!(report.conflicts_lower_bound, 4);
    assert_eq!(report.affected_ranges.len(), 3);
    assert_eq!(report.affected_checkpoints.len(), 2);
    assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
    assert!(report.affected_checkpoints.contains(divergent_id.as_slice()));
    assert_eq!(source.visited.len(), 9);

    // With the row limit exactly consumed, checkpoint conflicts remain subject
    // to the shared conflict budget. Storage finishes the tombstone range first
    // and then reports only checkpoint identities up to the crossing conflict.
    for max_conflicts in [2, 3] {
        let (base, left, right, mut source, delete_update_id, divergent_id, closure_id) = build_inputs();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 5, max_conflicts },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the checkpoint tail exceeds the shared detail budget after exact row closure")
        };
        assert_eq!(report.rows_examined, 5);
        assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
        assert_eq!(report.affected_ranges.len(), 3);
        assert_eq!(report.affected_checkpoints.len(), max_conflicts - 1);
        assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
        assert_eq!(report.affected_checkpoints.contains(divergent_id.as_slice()), max_conflicts == 3);
        assert!(!report.affected_checkpoints.contains(closure_id.as_slice()));
        assert_eq!(source.visited.len(), 9);
    }

    // ORNA-MERGE-005 requires both limits but leaves their boundary precedence
    // open. With four rows read, a second row conflict crossing a one-detail
    // cap stops before the tombstone range; allowing exactly two conflict
    // details reaches that range, where the fifth row crosses the row cap.
    let (base, left, right, mut source, _, _, _) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 4, max_conflicts: 1 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the second row conflict crosses the one-detail cap at the row boundary")
    };
    assert_eq!(report.rows_examined, 4);
    assert_eq!(report.conflicts_lower_bound, 2);
    assert_eq!(report.affected_ranges.len(), 2);
    assert!(report.affected_checkpoints.is_empty());
    let visits_through_second_conflict = segment_visits
        .iter()
        .take(2)
        .flat_map(|visits| visits.iter().cloned())
        .collect::<Vec<_>>();
    assert_eq!(source.visited, visits_through_second_conflict);

    let (base, left, right, mut source, _, _, _) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 4, max_conflicts: 2 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the fifth row crosses the row cap after both conflicts fit exactly")
    };
    assert_eq!(report.rows_examined, 5);
    assert_eq!(report.conflicts_lower_bound, 2);
    assert_eq!(report.affected_ranges.len(), 3);
    assert!(report.affected_checkpoints.is_empty());
    let mut visits_before_tombstone = visits_through_second_conflict;
    visits_before_tombstone.push((MergeSide::Base, b"base-tombstone".to_vec()));
    assert_eq!(source.visited, visits_before_tombstone);
}

#[test]
fn exact_row_budget_closes_fixture_tombstone_before_conflict_tail() {
    let mut keys = [integer(10), integer(20), integer(30)];
    keys.sort_by_key(|key| key.encode().unwrap());
    let [first_conflict_key, tombstone_key, final_conflict_key] = keys;
    let first_boundary = tombstone_key.encode().unwrap();
    let second_boundary = final_conflict_key.encode().unwrap();
    let ranges = [
        KeyRange::new(None, Some(first_boundary.clone())).unwrap(),
        KeyRange::new(Some(first_boundary), Some(second_boundary.clone())).unwrap(),
        KeyRange::new(Some(second_boundary), None).unwrap(),
    ];
    let split_manifest = |table_digest: u8, segment_digests: [u8; 3], locators: [&[u8]; 3]| {
        TableManifest {
            digest: [table_digest; 32],
            segments: (0..3)
                .map(|index| RowSegmentManifest {
                    locator: locators[index].to_vec(),
                    range: ranges[index].clone(),
                    digest: [segment_digests[index]; 32],
                })
                .collect(),
        }
    };
    let fixture_row = |fixture: &str, key: &CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key.clone();
        row
    };
    let build_inputs = || {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-first", vec![fixture_row(BASE, &first_conflict_key)]);
        source.add(MergeSide::Left, b"left-first", Vec::new());
        source.add(MergeSide::Right, b"right-first", vec![fixture_row(RIGHT, &first_conflict_key)]);

        source.add(MergeSide::Base, b"base-tombstone", vec![fixture_row(BASE, &tombstone_key)]);
        source.add(MergeSide::Left, b"left-tombstone", Vec::new());
        source.add(MergeSide::Right, b"right-tombstone", Vec::new());

        source.add(MergeSide::Base, b"base-final", vec![fixture_row(BASE, &final_conflict_key)]);
        source.add(MergeSide::Left, b"left-final", vec![fixture_row(LEFT, &final_conflict_key)]);
        source.add(MergeSide::Right, b"right-final", Vec::new());

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                100,
                [10, 11, 12],
                [b"base-first".as_slice(), b"base-tombstone".as_slice(), b"base-final".as_slice()],
            ),
            None,
        );
        let mut left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                101,
                [20, 21, 22],
                [b"left-first".as_slice(), b"left-tombstone".as_slice(), b"left-final".as_slice()],
            ),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                102,
                [30, 31, 32],
                [b"right-first".as_slice(), b"right-tombstone".as_slice(), b"right-final".as_slice()],
            ),
            None,
        );

        let checkpoint_id = b"consumer/m-final-conflict".to_vec();
        base.checkpoints.insert(checkpoint_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        left.checkpoints.insert(checkpoint_id.clone(), parse_checkpoint_fixture(CHECKPOINT_EDITED));
        let closure_id = b"consumer/z-clean-closure".to_vec();
        for snapshot in [&mut base, &mut left, &mut right] {
            snapshot.checkpoints.insert(closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        }
        (base, left, right, source, checkpoint_id, closure_id)
    };

    let segment_visits = [
        [
            (MergeSide::Base, b"base-first".to_vec()),
            (MergeSide::Left, b"left-first".to_vec()),
            (MergeSide::Right, b"right-first".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-tombstone".to_vec()),
            (MergeSide::Left, b"left-tombstone".to_vec()),
            (MergeSide::Right, b"right-tombstone".to_vec()),
        ],
        [
            (MergeSide::Base, b"base-final".to_vec()),
            (MergeSide::Left, b"left-final".to_vec()),
            (MergeSide::Right, b"right-final".to_vec()),
        ],
    ];
    let all_segment_visits = segment_visits
        .iter()
        .flat_map(|visits| visits.iter().cloned())
        .collect::<Vec<_>>();

    // ORNA-MERGE-005 requires explicit row and conflict budgets but leaves
    // boundary precedence open. Storage finishes ranges in manifest order,
    // checks row conflicts within them, then resolves checkpoints. This fixture
    // pins the final range after a clean tombstone under exact row capacity.
    let (base, left, right, mut source, _, _) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 3, max_conflicts: 1 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the next range row crosses the exact row budget after the tombstone")
    };
    assert_eq!(report.rows_examined, 4);
    assert_eq!(report.conflicts_lower_bound, 1);
    assert_eq!(report.affected_ranges.len(), 3);
    assert!(report.affected_checkpoints.is_empty());
    let mut visits_through_tombstone = segment_visits
        .iter()
        .take(2)
        .flat_map(|visits| visits.iter().cloned())
        .collect::<Vec<_>>();
    visits_through_tombstone.push((MergeSide::Base, b"base-final".to_vec()));
    assert_eq!(source.visited, visits_through_tombstone);

    // One existing conflict detail lets the final fixture row conflict cross
    // the cap exactly as the fifth and last row is read.
    let (base, left, right, mut source, _, _) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 5, max_conflicts: 1 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the final row conflict crosses the one-detail cap at the exact row limit")
    };
    assert_eq!(report.rows_examined, 5);
    assert_eq!(report.conflicts_lower_bound, 2);
    assert_eq!(report.affected_ranges.len(), 3);
    assert!(report.affected_checkpoints.is_empty());
    assert_eq!(source.visited, all_segment_visits);

    // At two row-conflict details the checkpoint conflict becomes the crossing
    // conflict. At three, the exact budget returns all fixture details and
    // closes through the clean checkpoint suffix.
    for max_conflicts in [2, 3] {
        let (base, left, right, mut source, checkpoint_id, closure_id) = build_inputs();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 5, max_conflicts },
        )
        .unwrap_err();
        assert_eq!(source.visited, all_segment_visits);
        if max_conflicts == 2 {
            let BranchMergeError::BudgetExceeded { report } = error else {
                panic!("the checkpoint conflict crosses after both row conflicts fit")
            };
            assert_eq!(report.rows_examined, 5);
            assert_eq!(report.conflicts_lower_bound, 3);
            assert_eq!(report.affected_ranges.len(), 3);
            assert_eq!(report.affected_checkpoints.len(), 1);
            assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(closure_id.as_slice()));
        } else {
            let BranchMergeError::Conflicts { conflicts, report } = error else {
                panic!("the exact three-conflict budget closes the fixture tail")
            };
            assert_eq!(conflicts.len(), 3);
            assert!(matches!(
                &conflicts[0],
                BranchMergeConflict::Row {
                    conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                    ..
                } if key == &first_conflict_key
            ));
            assert!(matches!(
                &conflicts[1],
                BranchMergeConflict::Row {
                    conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
                    ..
                } if key == &final_conflict_key
            ));
            assert_eq!(
                &conflicts[2],
                &BranchMergeConflict::CheckpointConflict {
                    id: checkpoint_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_EDITED)),
                        right: None,
                    },
                }
            );
            assert_eq!(report.rows_examined, 5);
            assert_eq!(report.conflicts_lower_bound, 3);
            assert_eq!(report.affected_ranges.len(), 3);
            assert_eq!(report.affected_checkpoints.len(), 1);
            assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(closure_id.as_slice()));
        }
    }
}

#[test]
fn final_fixture_tombstone_closes_row_budget_before_checkpoint_conflict() {
    let mut keys = [integer(10), integer(20)];
    keys.sort_by_key(|key| key.encode().unwrap());
    let [row_conflict_key, tombstone_key] = keys;
    let boundary = tombstone_key.encode().unwrap();
    let ranges = [
        KeyRange::new(None, Some(boundary.clone())).unwrap(),
        KeyRange::new(Some(boundary), None).unwrap(),
    ];
    let split_manifest = |table_digest: u8, digests: [u8; 2], locators: [&[u8]; 2]| {
        TableManifest {
            digest: [table_digest; 32],
            segments: (0..2)
                .map(|index| RowSegmentManifest {
                    locator: locators[index].to_vec(),
                    range: ranges[index].clone(),
                    digest: [digests[index]; 32],
                })
                .collect(),
        }
    };
    let fixture_row = |fixture: &str, key: &CanonicalValue| {
        let mut row = parse_fixture(fixture, RowKeyKind::Explicit);
        row.key = key.clone();
        row
    };
    let build_inputs = || {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-conflict", vec![fixture_row(BASE, &row_conflict_key)]);
        source.add(MergeSide::Left, b"left-conflict", Vec::new());
        source.add(MergeSide::Right, b"right-conflict", vec![fixture_row(RIGHT, &row_conflict_key)]);
        source.add(MergeSide::Base, b"base-tombstone", vec![fixture_row(BASE, &tombstone_key)]);
        source.add(MergeSide::Left, b"left-tombstone", Vec::new());
        source.add(MergeSide::Right, b"right-tombstone", Vec::new());

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                110,
                [10, 11],
                [b"base-conflict".as_slice(), b"base-tombstone".as_slice()],
            ),
            None,
        );
        let mut left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                111,
                [20, 21],
                [b"left-conflict".as_slice(), b"left-tombstone".as_slice()],
            ),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(
                112,
                [30, 31],
                [b"right-conflict".as_slice(), b"right-tombstone".as_slice()],
            ),
            None,
        );

        let checkpoint_id = b"consumer/m-final-conflict".to_vec();
        base.checkpoints.insert(checkpoint_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        left.checkpoints.insert(checkpoint_id.clone(), parse_checkpoint_fixture(CHECKPOINT_EDITED));
        let closure_id = b"consumer/z-clean-closure".to_vec();
        for snapshot in [&mut base, &mut left, &mut right] {
            snapshot.checkpoints.insert(closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        }
        (base, left, right, source, checkpoint_id, closure_id)
    };

    let visits = [
        (MergeSide::Base, b"base-conflict".to_vec()),
        (MergeSide::Left, b"left-conflict".to_vec()),
        (MergeSide::Right, b"right-conflict".to_vec()),
        (MergeSide::Base, b"base-tombstone".to_vec()),
        (MergeSide::Left, b"left-tombstone".to_vec()),
        (MergeSide::Right, b"right-tombstone".to_vec()),
    ];

    // ORNA-MERGE-005 leaves budget boundary precedence open. Storage resolves
    // manifest ranges before checkpoints; a clean final tombstone spends row
    // capacity, not conflict capacity, so checkpoint resolution follows it.
    let (base, left, right, mut source, _, _) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 2, max_conflicts: 1 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the final tombstone row crosses the two-row limit")
    };
    assert_eq!(report.rows_examined, 3);
    assert_eq!(report.conflicts_lower_bound, 1);
    assert_eq!(report.affected_ranges.len(), 2);
    assert!(report.affected_checkpoints.is_empty());
    assert_eq!(source.visited, visits[..4]);

    // At exact row capacity, the clean tombstone completes and the checkpoint
    // conflict becomes the first detail beyond the already-spent conflict cap.
    let (base, left, right, mut source, checkpoint_id, closure_id) = build_inputs();
    let error = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 3, max_conflicts: 1 },
    )
    .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the checkpoint conflict crosses after exact row-budget closure")
    };
    assert_eq!(report.rows_examined, 3);
    assert_eq!(report.conflicts_lower_bound, 2);
    assert_eq!(report.affected_ranges.len(), 2);
    assert_eq!(report.affected_checkpoints.len(), 1);
    assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
    assert!(!report.affected_checkpoints.contains(closure_id.as_slice()));
    assert_eq!(source.visited, visits);

    let (base, left, right, mut source, checkpoint_id, closure_id) = build_inputs();
    let BranchMergeError::Conflicts { conflicts, report } = merge_three_way_snapshots(
        &base,
        &left,
        &right,
        &mut source,
        BranchMergeBudget { max_rows_examined: 3, max_conflicts: 2 },
    )
    .unwrap_err()
    else {
        panic!("exact row and conflict capacity closes through the clean checkpoint suffix")
    };
    assert_eq!(conflicts.len(), 2);
    assert!(matches!(
        &conflicts[0],
        BranchMergeConflict::Row {
            conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { key, .. },
            ..
        } if key == &row_conflict_key
    ));
    assert_eq!(
        &conflicts[1],
        &BranchMergeConflict::CheckpointConflict {
            id: checkpoint_id.clone(),
            conflict: orna_evolution_v1::CheckpointMergeConflict {
                base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                left: Some(parse_checkpoint_fixture(CHECKPOINT_EDITED)),
                right: None,
            },
        }
    );
    assert_eq!(report.rows_examined, 3);
    assert_eq!(report.conflicts_lower_bound, 2);
    assert_eq!(report.affected_ranges.len(), 2);
    assert_eq!(report.affected_checkpoints.len(), 1);
    assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
    assert!(!report.affected_checkpoints.contains(closure_id.as_slice()));
    assert_eq!(source.visited, visits);
}

#[test]
fn zero_conflict_budget_reports_checkpoint_delete_after_segment_tombstone() {
    let candidate_a = integer(10);
    let candidate_b = integer(20);
    let high_key = if candidate_a.encode().unwrap() < candidate_b.encode().unwrap() {
        candidate_b
    } else {
        candidate_a
    };
    let boundary = high_key.encode().unwrap();
    let low_range = KeyRange::new(None, Some(boundary.clone())).unwrap();
    let high_range = KeyRange::new(Some(boundary), None).unwrap();
    let split_manifest = |table_digest, upper_digest, upper_locator: &[u8]| TableManifest {
        digest: [table_digest; 32],
        segments: vec![
            RowSegmentManifest {
                locator: b"shared-lower".to_vec(),
                range: low_range.clone(),
                digest: [7; 32],
            },
            RowSegmentManifest {
                locator: upper_locator.to_vec(),
                range: high_range.clone(),
                digest: [upper_digest; 32],
            },
        ],
    };
    let mut deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    deleted_row.key = high_key.clone();
    let regular_checkpoint_fixtures = (CHECKPOINT_BASE, CHECKPOINT_EDITED);

    let build_inputs = |row_delete_on_left, checkpoint_delete_on_left, divergent_checkpoint, checkpoint_fixtures| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base-upper", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left-upper",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right-upper",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(
            schema(true, FieldType::Str),
            split_manifest(70, 10, b"base-upper"),
            None,
        );
        let mut left = snapshot(
            schema(true, FieldType::Str),
            split_manifest(71, 11, b"left-upper"),
            None,
        );
        let mut right = snapshot(
            schema(true, FieldType::Str),
            split_manifest(72, 12, b"right-upper"),
            None,
        );

        let agreed_delete_id = b"consumer/a-agreed-delete".to_vec();
        base.checkpoints.insert(
            agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );

        let unchanged_delete_id = b"consumer/b-delete-against-unchanged".to_vec();
        let (base_fixture, updated_fixture) = checkpoint_fixtures;
        let base_checkpoint = parse_checkpoint_fixture(base_fixture);
        base.checkpoints.insert(unchanged_delete_id.clone(), base_checkpoint.clone());
        let retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
        retained_side
            .checkpoints
            .insert(unchanged_delete_id.clone(), base_checkpoint.clone());

        let checkpoint_id = b"consumer/m-delete-update-after-tombstone".to_vec();
        base.checkpoints.insert(checkpoint_id.clone(), base_checkpoint.clone());
        let update_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
        let updated_checkpoint = parse_checkpoint_fixture(updated_fixture);
        update_side.checkpoints.insert(
            checkpoint_id.clone(),
            if divergent_checkpoint { updated_checkpoint } else { base_checkpoint.clone() },
        );

        let tail_id = b"consumer/z-later-conflict".to_vec();
        if divergent_checkpoint {
            base.checkpoints.insert(tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
            left.checkpoints.insert(tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
            right.checkpoints.insert(tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        } else {
            for snapshot in [&mut base, &mut left, &mut right] {
                snapshot.checkpoints.insert(tail_id.clone(), base_checkpoint.clone());
            }
        }

        (base, left, right, source, agreed_delete_id, unchanged_delete_id, checkpoint_id, tail_id)
    };

    // Row and checkpoint deletion orientations are independent; cover all
    // combinations so a tombstone never hides the checkpoint budget tail.
    for (row_delete_on_left, checkpoint_delete_on_left) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let (
            base,
            left,
            right,
            mut source,
            agreed_delete_id,
            unchanged_delete_id,
            checkpoint_id,
            tail_id,
        ) = build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);

        // ORNA-MERGE-005 bounds conflict evidence but leaves cross-phase order
        // open. Storage finishes segmented row planning first, so clean row
        // deletions materialize as tombstones before checkpoint conflicts are
        // visited in stable identity order. Keep that local policy explicit
        // while independently orienting row and checkpoint deletion.
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the checkpoint delete/update after the tombstone exceeds zero budget")
        };
        assert_eq!(report.conflicts_lower_bound, 1);
        assert_eq!(report.rows_examined, 2);
        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
        assert!(!report.affected_ranges.contains(&(id(1), low_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(tail_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
        assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"upper")));

        // ORNA-MERGE-005 leaves resource-budget precedence open. One row
        // below the fixture tombstone stops before checkpoint resolution;
        // exact row capacity completes the tombstone before the zero-detail
        // checkpoint conflict crosses the conflict budget.
        let (base, left, right, mut source, _, _, _, _) =
            build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 1, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the second fixture row crosses the one-row boundary")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 0);
        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
        assert!(report.affected_checkpoints.is_empty());
        assert_eq!(source.visited.len(), if row_delete_on_left { 3 } else { 2 });

        let (base, left, right, mut source, _, _, checkpoint_id, _) =
            build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the checkpoint conflict crosses after exact row-budget tombstone closure")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 1);
        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        // At exact row capacity, one checkpoint detail fits; the later fixture
        // conflict crosses after the clean row tombstone has resolved.
        let (base, left, right, mut source, agreed_delete_id, unchanged_delete_id, checkpoint_id, tail_id) =
            build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 1 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the later checkpoint conflict crosses the one-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 2);
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.affected_ranges.len(), 1);
        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 2);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
        assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"upper")));

        // With only the m conflict present, one detail is exactly sufficient:
        // preserve the fixture-backed orientation and return a typed conflict
        // instead of reporting a budget overrun.
        let (mut base, mut left, mut right, mut source, agreed_delete_id, unchanged_delete_id, checkpoint_id, tail_id) =
            build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);
        for snapshot in [&mut base, &mut left, &mut right] {
            snapshot.checkpoints.remove(&tail_id);
        }
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 1 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("the single fixture conflict fits exactly in the detail budget")
        };
        let updated = parse_checkpoint_fixture(CHECKPOINT_EDITED);
        assert_eq!(
            conflicts,
            vec![BranchMergeConflict::CheckpointConflict {
                id: checkpoint_id.clone(),
                conflict: orna_evolution_v1::CheckpointMergeConflict {
                    base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                    left: if checkpoint_delete_on_left { None } else { Some(updated.clone()) },
                    right: if checkpoint_delete_on_left { Some(updated) } else { None },
                },
            }]
        );
        assert_eq!(report.conflicts_lower_bound, 1);
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.affected_ranges.len(), 1);
        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        // Without a divergent checkpoint tail, exact row capacity still
        // closes the clean delete and materializes the tombstone.
        let (mut base, mut left, mut right, mut source, _, _, checkpoint_id, tail_id) = build_inputs(
            row_delete_on_left,
            checkpoint_delete_on_left,
            false,
            regular_checkpoint_fixtures,
        );
        let mut ordered_keys = [integer(10), integer(20), integer(30)];
        ordered_keys.sort_by_key(|key| key.encode().unwrap());
        let suffix_boundary = ordered_keys[2].encode().unwrap();
        let tombstone_range = KeyRange::new(high_range.start.clone(), Some(suffix_boundary.clone())).unwrap();
        let suffix_range = KeyRange::new(Some(suffix_boundary), None).unwrap();
        for snapshot in [&mut base, &mut left, &mut right] {
            let manifest = snapshot.tables.get_mut(&id(1)).unwrap();
            manifest.segments[1].range = tombstone_range.clone();
            manifest.segments.push(RowSegmentManifest {
                locator: b"shared-clean-suffix".to_vec(),
                range: suffix_range.clone(),
                digest: [99; 32],
            });
        }

        // ORNA-MERGE-005 is silent about row budgets for unchanged aligned
        // suffixes. Storage reuses equal-digest segments without reads, so the
        // exact tombstone boundary still closes the checkpoint tail.
        let plan = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 0 },
        )
        .unwrap();
        assert_eq!(plan.report.conflicts_lower_bound, 0);
        assert_eq!(plan.report.rows_examined, 2);
        assert!(plan.report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(!plan.report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(source.visited.len(), 3);
        assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert!(!plan.checkpoints.contains_key(checkpoint_id.as_slice()));
        assert!(plan.checkpoints.contains_key(tail_id.as_slice()));
        let segments = &plan.tables[&id(1)].segments;
        assert_eq!(segments.len(), 3);
        assert!(matches!(segments[0], MergedSegment::Reuse { from: MergeSide::Left, .. }));
        let MergedSegment::Rows { rows, tombstones, .. } = &segments[1] else {
            panic!("the upper segment deletion must materialize a tombstone")
        };
        assert!(rows.is_empty());
        assert_eq!(tombstones, &[high_key.clone()]);
        assert!(matches!(segments[2], MergedSegment::Reuse { from: MergeSide::Left, .. }));

        // A changed but empty suffix still closes after the exact tombstone
        // budget: empty scans consume no row units and checkpoint resolution
        // continues. The reference does not prescribe this storage policy.
        let (mut base, mut left, mut right, mut source, _, _, checkpoint_id, tail_id) = build_inputs(
            row_delete_on_left,
            checkpoint_delete_on_left,
            false,
            regular_checkpoint_fixtures,
        );
        let mut ordered_keys = [integer(10), integer(20), integer(30)];
        ordered_keys.sort_by_key(|key| key.encode().unwrap());
        let suffix_boundary = ordered_keys[2].encode().unwrap();
        let tombstone_range = KeyRange::new(high_range.start.clone(), Some(suffix_boundary.clone())).unwrap();
        let suffix_range = KeyRange::new(Some(suffix_boundary), None).unwrap();
        for (snapshot, locator, digest) in [
            (&mut base, b"base-empty-suffix".as_slice(), 90),
            (&mut left, b"left-empty-suffix".as_slice(), 91),
            (&mut right, b"right-empty-suffix".as_slice(), 92),
        ] {
            let manifest = snapshot.tables.get_mut(&id(1)).unwrap();
            manifest.segments[1].range = tombstone_range.clone();
            manifest.segments.push(RowSegmentManifest {
                locator: locator.to_vec(),
                range: suffix_range.clone(),
                digest: [digest; 32],
            });
        }
        source.add(MergeSide::Base, b"base-empty-suffix", Vec::new());
        source.add(MergeSide::Left, b"left-empty-suffix", Vec::new());
        source.add(MergeSide::Right, b"right-empty-suffix", Vec::new());
        let plan = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 0 },
        )
        .unwrap();
        assert_eq!(plan.report.rows_examined, 2);
        assert_eq!(plan.report.conflicts_lower_bound, 0);
        assert!(plan.report.affected_ranges.contains(&(id(1), tombstone_range)));
        assert!(plan.report.affected_ranges.contains(&(id(1), suffix_range)));
        assert_eq!(source.visited.len(), 6);
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );
        assert!(!plan.checkpoints.contains_key(checkpoint_id.as_slice()));
        assert!(plan.checkpoints.contains_key(tail_id.as_slice()));
        let segments = &plan.tables[&id(1)].segments;
        assert_eq!(segments.len(), 3);
        assert!(matches!(segments[0], MergedSegment::Reuse { from: MergeSide::Left, .. }));
        let MergedSegment::Rows { rows, tombstones, .. } = &segments[1] else {
            panic!("the upper segment deletion must materialize a tombstone")
        };
        assert!(rows.is_empty());
        assert_eq!(tombstones, &[high_key.clone()]);
        let MergedSegment::Rows { rows, tombstones, .. } = &segments[2] else {
            panic!("the changed empty suffix must be scanned after tombstone closure")
        };
        assert!(rows.is_empty());
        assert!(tombstones.is_empty());

        // A divergent checkpoint after the same changed empty suffix must be
        // reached under exact row capacity; zero conflict capacity then stops
        // at its first identity while leaving the later checkpoint suffix out.
        let (mut base, mut left, mut right, mut source, _, _, checkpoint_id, tail_id) = build_inputs(
            row_delete_on_left,
            checkpoint_delete_on_left,
            true,
            regular_checkpoint_fixtures,
        );
        let mut ordered_keys = [integer(10), integer(20), integer(30)];
        ordered_keys.sort_by_key(|key| key.encode().unwrap());
        let suffix_boundary = ordered_keys[2].encode().unwrap();
        let tombstone_range = KeyRange::new(high_range.start.clone(), Some(suffix_boundary.clone())).unwrap();
        let suffix_range = KeyRange::new(Some(suffix_boundary), None).unwrap();
        for (snapshot, locator, digest) in [
            (&mut base, b"base-empty-suffix".as_slice(), 90),
            (&mut left, b"left-empty-suffix".as_slice(), 91),
            (&mut right, b"right-empty-suffix".as_slice(), 92),
        ] {
            let manifest = snapshot.tables.get_mut(&id(1)).unwrap();
            manifest.segments[1].range = tombstone_range.clone();
            manifest.segments.push(RowSegmentManifest {
                locator: locator.to_vec(),
                range: suffix_range.clone(),
                digest: [digest; 32],
            });
        }
        source.add(MergeSide::Base, b"base-empty-suffix", Vec::new());
        source.add(MergeSide::Left, b"left-empty-suffix", Vec::new());
        source.add(MergeSide::Right, b"right-empty-suffix", Vec::new());
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the first checkpoint conflict crosses after empty suffix closure")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 1);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range)));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range)));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(tail_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // After the zero-detail boundary above, one slot retains the first
        // checkpoint and makes the later one the crossing impact; two slots
        // close both fixture conflicts at the exact row boundary.
        for max_conflicts in [1, 2] {
            let (
                mut base,
                mut left,
                mut right,
                mut source,
                agreed_delete_id,
                unchanged_delete_id,
                checkpoint_id,
                tail_id,
            ) = build_inputs(
                row_delete_on_left,
                checkpoint_delete_on_left,
                true,
                regular_checkpoint_fixtures,
            );
            let mut ordered_keys = [integer(10), integer(20), integer(30)];
            ordered_keys.sort_by_key(|key| key.encode().unwrap());
            let suffix_boundary = ordered_keys[2].encode().unwrap();
            let tombstone_range = KeyRange::new(high_range.start.clone(), Some(suffix_boundary.clone())).unwrap();
            let suffix_range = KeyRange::new(Some(suffix_boundary), None).unwrap();
            for (snapshot, locator, digest) in [
                (&mut base, b"base-empty-suffix".as_slice(), 90),
                (&mut left, b"left-empty-suffix".as_slice(), 91),
                (&mut right, b"right-empty-suffix".as_slice(), 92),
            ] {
                let manifest = snapshot.tables.get_mut(&id(1)).unwrap();
                manifest.segments[1].range = tombstone_range.clone();
                manifest.segments.push(RowSegmentManifest {
                    locator: locator.to_vec(),
                    range: suffix_range.clone(),
                    digest: [digest; 32],
                });
            }
            source.add(MergeSide::Base, b"base-empty-suffix", Vec::new());
            source.add(MergeSide::Left, b"left-empty-suffix", Vec::new());
            source.add(MergeSide::Right, b"right-empty-suffix", Vec::new());
            let result = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 2, max_conflicts },
            );
            assert_eq!(source.visited.len(), 6);
            assert_eq!(
                &source.visited[3..],
                &[
                    (MergeSide::Base, b"base-empty-suffix".to_vec()),
                    (MergeSide::Left, b"left-empty-suffix".to_vec()),
                    (MergeSide::Right, b"right-empty-suffix".to_vec()),
                ]
            );
            let report = match (max_conflicts, result) {
                (1, Err(BranchMergeError::BudgetExceeded { report })) => {
                    assert_eq!(report.conflicts_lower_bound, 2);
                    report
                }
                (2, Err(BranchMergeError::Conflicts { conflicts, report })) => {
                    let updated = parse_checkpoint_fixture(CHECKPOINT_EDITED);
                    assert_eq!(
                        conflicts,
                        vec![
                            BranchMergeConflict::CheckpointConflict {
                                id: checkpoint_id.clone(),
                                conflict: orna_evolution_v1::CheckpointMergeConflict {
                                    base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                                    left: if checkpoint_delete_on_left { None } else { Some(updated.clone()) },
                                    right: if checkpoint_delete_on_left { Some(updated) } else { None },
                                },
                            },
                            BranchMergeConflict::CheckpointConflict {
                                id: tail_id.clone(),
                                conflict: orna_evolution_v1::CheckpointMergeConflict {
                                    base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                                    left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                                    right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                                },
                            },
                        ]
                    );
                    assert_eq!(report.conflicts_lower_bound, 2);
                    assert_eq!(report.affected_checkpoints.len(), 2);
                    report
                }
                (_, other) => panic!("unexpected exact-row checkpoint tail: {other:?}"),
            };
            assert_eq!(report.rows_examined, 2);
            assert_eq!(report.affected_checkpoints.len(), 2);
            assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
            assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
            assert!(report.affected_ranges.contains(&(id(1), tombstone_range)));
            assert!(report.affected_ranges.contains(&(id(1), suffix_range)));
        }

        // With enough detail budget, the fixture-backed m and z conflicts
        // retain their full base/left/right values after the row tombstone.
        let (base, left, right, mut source, _, _, checkpoint_id, tail_id) = build_inputs(
            row_delete_on_left,
            checkpoint_delete_on_left,
            true,
            regular_checkpoint_fixtures,
        );
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("both fixture-backed checkpoint conflicts fit the detail budget")
        };
        let updated = parse_checkpoint_fixture(CHECKPOINT_EDITED);
        let expected_tail_base = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        let expected_tail_left = parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT);
        let expected_tail_right = parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT);
        assert_eq!(
            conflicts,
            vec![
                BranchMergeConflict::CheckpointConflict {
                    id: checkpoint_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                        left: if checkpoint_delete_on_left { None } else { Some(updated.clone()) },
                        right: if checkpoint_delete_on_left { Some(updated) } else { None },
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: tail_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(expected_tail_base),
                        left: Some(expected_tail_left),
                        right: Some(expected_tail_right),
                    },
                },
            ]
        );
        assert_eq!(report.conflicts_lower_bound, 2);
        assert_eq!(report.rows_examined, 2);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));

        // A terminal fixture conflict after the two retained details proves
        // the checkpoint cap still closes after exact row-budget tombstone
        // and changed-empty-suffix scans.
        let (mut base, mut left, mut right, mut source, agreed_delete_id, unchanged_delete_id, checkpoint_id, tail_id) =
            build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);
        let mut ordered_keys = [integer(10), integer(20), integer(30)];
        ordered_keys.sort_by_key(|key| key.encode().unwrap());
        let suffix_boundary = ordered_keys[2].encode().unwrap();
        let tombstone_range = KeyRange::new(high_range.start.clone(), Some(suffix_boundary.clone())).unwrap();
        let suffix_range = KeyRange::new(Some(suffix_boundary), None).unwrap();
        for (snapshot, locator, digest) in [
            (&mut base, b"base-empty-suffix".as_slice(), 90),
            (&mut left, b"left-empty-suffix".as_slice(), 91),
            (&mut right, b"right-empty-suffix".as_slice(), 92),
        ] {
            let manifest = snapshot.tables.get_mut(&id(1)).unwrap();
            manifest.segments[1].range = tombstone_range.clone();
            manifest.segments.push(RowSegmentManifest {
                locator: locator.to_vec(),
                range: suffix_range.clone(),
                digest: [digest; 32],
            });
        }
        source.add(MergeSide::Base, b"base-empty-suffix", Vec::new());
        source.add(MergeSide::Left, b"left-empty-suffix", Vec::new());
        source.add(MergeSide::Right, b"right-empty-suffix", Vec::new());
        let final_tail_id = b"consumer/zz-final-tail-conflict".to_vec();
        // ORNA-MERGE-011 is silent on conflict-budget traversal. Follow this
        // crate's sorted checkpoint-ID traversal and extend the fixture tail
        // to pin the next exact-cap closure edge.
        let closure_tail_id = b"consumer/zzz-closure-tail-conflict".to_vec();
        let terminal_closure_id = b"consumer/zzzz-terminal-closure-conflict".to_vec();
        let final_closure_id = b"consumer/zzzzz-final-closure-conflict".to_vec();
        let terminal_tail_id = b"consumer/zzzzzz-terminal-tail-conflict".to_vec();
        let final_terminal_tail_id = b"consumer/zzzzzzz-final-terminal-tail-conflict".to_vec();
        let final_conflict_id = b"consumer/zzzzzzzz-final-checkpoint-conflict".to_vec();
        let terminal_checkpoint_id = b"consumer/zzzzzzzzz-terminal-checkpoint-conflict".to_vec();
        let eleventh_conflict_id = b"consumer/zzzzzzzzzz-final-checkpoint-conflict".to_vec();
        let twelfth_conflict_id = b"consumer/zzzzzzzzzzz-terminal-checkpoint-conflict".to_vec();
        let thirteenth_conflict_id = b"consumer/zzzzzzzzzzzz-thirteenth-checkpoint-conflict".to_vec();
        let fourteenth_conflict_id = b"consumer/zzzzzzzzzzzzz-fourteenth-checkpoint-conflict".to_vec();
        let fifteenth_conflict_id = b"consumer/zzzzzzzzzzzzzz-fifteenth-checkpoint-conflict".to_vec();
        let sixteenth_conflict_id = b"consumer/zzzzzzzzzzzzzzz-sixteenth-checkpoint-conflict".to_vec();
        let seventeenth_conflict_id = b"consumer/zzzzzzzzzzzzzzzz-seventeenth-checkpoint-conflict".to_vec();
        let eighteenth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzz-eighteenth-checkpoint-conflict".to_vec();
        let nineteenth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzz-nineteenth-checkpoint-conflict".to_vec();
        let twentieth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzz-twentieth-checkpoint-conflict".to_vec();
        let twenty_first_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzz-twenty-first-checkpoint-conflict".to_vec();
        let twenty_second_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzz-twenty-second-checkpoint-conflict".to_vec();
        let twenty_third_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzz-twenty-third-checkpoint-conflict".to_vec();
        let twenty_fourth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzzz-twenty-fourth-checkpoint-conflict".to_vec();
        let twenty_fifth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzzzz-twenty-fifth-checkpoint-conflict".to_vec();
        let twenty_sixth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzzzzz-twenty-sixth-checkpoint-conflict".to_vec();
        let twenty_seventh_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzzzzzz-twenty-seventh-checkpoint-conflict".to_vec();
        let twenty_eighth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzzzzzzz-twenty-eighth-checkpoint-conflict".to_vec();
        let twenty_ninth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzzzzzzzz-twenty-ninth-checkpoint-conflict".to_vec();
        let thirtieth_conflict_id = b"consumer/zzzzzzzzzzzzzzzzzzzzzzzzzzzzz-thirtieth-checkpoint-conflict".to_vec();
        let trailing_agreed_delete_id =
            b"consumer/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-trailing-agreed-delete".to_vec();
        let trailing_unchanged_delete_id =
            b"consumer/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-trailing-unchanged-delete".to_vec();
        let thirty_first_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 64]);
            id.extend_from_slice(b"-thirty-first-checkpoint-conflict");
            id
        };
        let second_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 72]);
            id.extend_from_slice(b"-second-trailing-agreed-delete");
            id
        };
        let second_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 80]);
            id.extend_from_slice(b"-second-trailing-unchanged-delete");
            id
        };
        let thirty_second_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 96]);
            id.extend_from_slice(b"-thirty-second-checkpoint-conflict");
            id
        };
        let third_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 104]);
            id.extend_from_slice(b"-third-trailing-agreed-delete");
            id
        };
        let third_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 112]);
            id.extend_from_slice(b"-third-trailing-unchanged-delete");
            id
        };
        let thirty_third_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 128]);
            id.extend_from_slice(b"-thirty-third-checkpoint-conflict");
            id
        };
        let fourth_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 136]);
            id.extend_from_slice(b"-fourth-trailing-agreed-delete");
            id
        };
        let fourth_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 144]);
            id.extend_from_slice(b"-fourth-trailing-unchanged-delete");
            id
        };
        let thirty_fourth_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 160]);
            id.extend_from_slice(b"-thirty-fourth-checkpoint-conflict");
            id
        };
        let fifth_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 168]);
            id.extend_from_slice(b"-fifth-trailing-agreed-delete");
            id
        };
        let fifth_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 176]);
            id.extend_from_slice(b"-fifth-trailing-unchanged-delete");
            id
        };
        let thirty_fifth_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 192]);
            id.extend_from_slice(b"-thirty-fifth-checkpoint-conflict");
            id
        };
        let sixth_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 200]);
            id.extend_from_slice(b"-sixth-trailing-agreed-delete");
            id
        };
        let sixth_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 208]);
            id.extend_from_slice(b"-sixth-trailing-unchanged-delete");
            id
        };
        let thirty_sixth_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 224]);
            id.extend_from_slice(b"-thirty-sixth-checkpoint-conflict");
            id
        };
        let seventh_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 232]);
            id.extend_from_slice(b"-seventh-trailing-agreed-delete");
            id
        };
        let seventh_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 240]);
            id.extend_from_slice(b"-seventh-trailing-unchanged-delete");
            id
        };
        let thirty_seventh_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 256]);
            id.extend_from_slice(b"-thirty-seventh-checkpoint-conflict");
            id
        };
        let eighth_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 264]);
            id.extend_from_slice(b"-eighth-trailing-agreed-delete");
            id
        };
        let eighth_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 272]);
            id.extend_from_slice(b"-eighth-trailing-unchanged-delete");
            id
        };
        let thirty_eighth_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 288]);
            id.extend_from_slice(b"-thirty-eighth-checkpoint-conflict");
            id
        };
        let ninth_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 296]);
            id.extend_from_slice(b"-ninth-trailing-agreed-delete");
            id
        };
        let ninth_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 304]);
            id.extend_from_slice(b"-ninth-trailing-unchanged-delete");
            id
        };
        let thirty_ninth_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 320]);
            id.extend_from_slice(b"-thirty-ninth-checkpoint-conflict");
            id
        };
        let tenth_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 328]);
            id.extend_from_slice(b"-tenth-trailing-agreed-delete");
            id
        };
        let tenth_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 336]);
            id.extend_from_slice(b"-tenth-trailing-unchanged-delete");
            id
        };
        let fortieth_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 352]);
            id.extend_from_slice(b"-fortieth-checkpoint-conflict");
            id
        };
        let eleventh_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 360]);
            id.extend_from_slice(b"-eleventh-trailing-agreed-delete");
            id
        };
        let eleventh_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 368]);
            id.extend_from_slice(b"-eleventh-trailing-unchanged-delete");
            id
        };
        let forty_first_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 384]);
            id.extend_from_slice(b"-forty-first-checkpoint-conflict");
            id
        };
        let twelfth_trailing_agreed_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 392]);
            id.extend_from_slice(b"-twelfth-trailing-agreed-delete");
            id
        };
        let twelfth_trailing_unchanged_delete_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 400]);
            id.extend_from_slice(b"-twelfth-trailing-unchanged-delete");
            id
        };
        let forty_second_conflict_id = {
            let mut id = b"consumer/".to_vec();
            id.extend_from_slice(&[b'z'; 416]);
            id.extend_from_slice(b"-forty-second-checkpoint-conflict");
            id
        };
        base.checkpoints.insert(final_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(final_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(final_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(closure_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(closure_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(closure_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(terminal_closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(terminal_closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(terminal_closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(final_closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(final_closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(final_closure_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(terminal_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(terminal_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(terminal_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(final_terminal_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(final_terminal_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(final_terminal_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(final_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(final_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(final_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(terminal_checkpoint_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(terminal_checkpoint_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(terminal_checkpoint_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(eleventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(eleventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(eleventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twelfth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twelfth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twelfth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(thirteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(fourteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(fourteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(fourteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(fifteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(fifteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(fifteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(sixteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(sixteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(sixteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(seventeenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(seventeenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(seventeenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(eighteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(eighteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(eighteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(nineteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(nineteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(nineteenth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twentieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twentieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twentieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_third_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_third_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_third_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_fourth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_fourth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_fourth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_fifth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_fifth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_fifth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_sixth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_sixth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_sixth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_seventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_seventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_seventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_eighth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_eighth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_eighth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(twenty_ninth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(twenty_ninth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(twenty_ninth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(thirtieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirtieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirtieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        // ORNA-MERGE-011 leaves tombstone/conflict tail traversal unspecified.
        // Keep agreed deletion and delete-versus-unchanged cases between
        // fixture conflicts; sorted traversal treats them as clean closure
        // entries that do not consume conflict capacity.
        base.checkpoints.insert(
            trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            trailing_unchanged_delete_id.clone(),
            trailing_unchanged_checkpoint.clone(),
        );
        {
            let trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            trailing_retained_side.checkpoints.insert(
                trailing_unchanged_delete_id.clone(),
                trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            second_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let second_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            second_trailing_unchanged_delete_id.clone(),
            second_trailing_unchanged_checkpoint.clone(),
        );
        let second_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
        second_trailing_retained_side.checkpoints.insert(
            second_trailing_unchanged_delete_id.clone(),
            second_trailing_unchanged_checkpoint,
        );
        base.checkpoints.insert(thirty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            third_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let third_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            third_trailing_unchanged_delete_id.clone(),
            third_trailing_unchanged_checkpoint.clone(),
        );
        {
            let third_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            third_trailing_retained_side.checkpoints.insert(
                third_trailing_unchanged_delete_id.clone(),
                third_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_third_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_third_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_third_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            fourth_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let fourth_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            fourth_trailing_unchanged_delete_id.clone(),
            fourth_trailing_unchanged_checkpoint.clone(),
        );
        {
            let fourth_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            fourth_trailing_retained_side.checkpoints.insert(
                fourth_trailing_unchanged_delete_id.clone(),
                fourth_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_fourth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_fourth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_fourth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            fifth_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let fifth_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            fifth_trailing_unchanged_delete_id.clone(),
            fifth_trailing_unchanged_checkpoint.clone(),
        );
        {
            let fifth_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            fifth_trailing_retained_side.checkpoints.insert(
                fifth_trailing_unchanged_delete_id.clone(),
                fifth_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_fifth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_fifth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_fifth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            sixth_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let sixth_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            sixth_trailing_unchanged_delete_id.clone(),
            sixth_trailing_unchanged_checkpoint.clone(),
        );
        {
            let sixth_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            sixth_trailing_retained_side.checkpoints.insert(
                sixth_trailing_unchanged_delete_id.clone(),
                sixth_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_sixth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_sixth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_sixth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            seventh_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let seventh_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            seventh_trailing_unchanged_delete_id.clone(),
            seventh_trailing_unchanged_checkpoint.clone(),
        );
        {
            let seventh_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            seventh_trailing_retained_side.checkpoints.insert(
                seventh_trailing_unchanged_delete_id.clone(),
                seventh_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_seventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_seventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_seventh_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            eighth_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let eighth_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            eighth_trailing_unchanged_delete_id.clone(),
            eighth_trailing_unchanged_checkpoint.clone(),
        );
        {
            let eighth_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            eighth_trailing_retained_side.checkpoints.insert(
                eighth_trailing_unchanged_delete_id.clone(),
                eighth_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_eighth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_eighth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_eighth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            ninth_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let ninth_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            ninth_trailing_unchanged_delete_id.clone(),
            ninth_trailing_unchanged_checkpoint.clone(),
        );
        {
            let ninth_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            ninth_trailing_retained_side.checkpoints.insert(
                ninth_trailing_unchanged_delete_id.clone(),
                ninth_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(thirty_ninth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(thirty_ninth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(thirty_ninth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            tenth_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let tenth_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            tenth_trailing_unchanged_delete_id.clone(),
            tenth_trailing_unchanged_checkpoint.clone(),
        );
        {
            let tenth_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            tenth_trailing_retained_side.checkpoints.insert(
                tenth_trailing_unchanged_delete_id.clone(),
                tenth_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(fortieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(fortieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(fortieth_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            eleventh_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let eleventh_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            eleventh_trailing_unchanged_delete_id.clone(),
            eleventh_trailing_unchanged_checkpoint.clone(),
        );
        {
            let eleventh_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            eleventh_trailing_retained_side.checkpoints.insert(
                eleventh_trailing_unchanged_delete_id.clone(),
                eleventh_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(forty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(forty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(forty_first_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        base.checkpoints.insert(
            twelfth_trailing_agreed_delete_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        let twelfth_trailing_unchanged_checkpoint = parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE);
        base.checkpoints.insert(
            twelfth_trailing_unchanged_delete_id.clone(),
            twelfth_trailing_unchanged_checkpoint.clone(),
        );
        {
            let twelfth_trailing_retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
            twelfth_trailing_retained_side.checkpoints.insert(
                twelfth_trailing_unchanged_delete_id.clone(),
                twelfth_trailing_unchanged_checkpoint,
            );
        }
        base.checkpoints.insert(forty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(forty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(forty_second_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the terminal fixture conflict crosses the two-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 3);
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 3);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Three details still stop before the fourth terminal fixture
        // conflict, after the same suffix scan and without more row budget.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 3 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the fourth checkpoint conflict crosses the three-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 4);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 4);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Four details stop when the fifth terminal checkpoint conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 4 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the fifth checkpoint conflict crosses the four-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 5);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 5);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Five details stop when the sixth terminal fixture conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 5 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the sixth checkpoint conflict crosses the five-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 6);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 6);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Six details stop when the seventh terminal fixture conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 6 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the seventh checkpoint conflict crosses the six-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 7);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 7);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Seven details stop when the eighth terminal fixture conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 7 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the eighth checkpoint conflict crosses the seven-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 8);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 8);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Eight details stop when the ninth terminal fixture conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 8 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the ninth checkpoint conflict crosses the eight-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 9);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 9);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Nine details stop when the tenth terminal fixture conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 9 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the tenth checkpoint conflict crosses the nine-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 10);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 10);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eleventh_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Ten details stop when the eleventh terminal fixture conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 10 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the eleventh checkpoint conflict crosses the ten-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 11);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 11);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(eleventh_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(twelfth_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Eleven details stop when the twelfth terminal fixture conflict is
        // reached, after the same suffix scan and row-budget consumption.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 11 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twelfth checkpoint conflict crosses the eleven-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 12);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 12);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(eleventh_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twelfth_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The twelve-detail cap crosses on the twelfth tail checkpoint,
        // proving the same suffix scan remains bounded at closure.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 12 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirteenth checkpoint conflict crosses the twelve-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 13);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 13);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(eleventh_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twelfth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirteenth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Thirteen details cross at the fourteenth total conflict, reaching
        // the next checkpoint beyond the twelfth tail conflict.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 13 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the fourteenth checkpoint conflict crosses the thirteen-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 14);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 14);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(eleventh_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twelfth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirteenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(fourteenth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Fourteen details cross on the fifteenth total conflict, carrying the
        // tombstone and empty-suffix closure through one more tail checkpoint.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 14 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the fifteenth checkpoint conflict crosses the fourteen-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 15);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 15);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Fifteen details cross on the sixteenth total conflict, continuing
        // the tombstone and empty-suffix closure through one more tail entry.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 15 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the sixteenth checkpoint conflict crosses the fifteen-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 16);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 16);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Sixteen details cross on the seventeenth total conflict, keeping the
        // tombstone and empty-suffix closure bounded through the next tail.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 16 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the seventeenth checkpoint conflict crosses the sixteen-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 17);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 17);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Seventeen details cross on the eighteenth total conflict, preserving
        // the same bounded tombstone and empty-suffix closure.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 17 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the eighteenth checkpoint conflict crosses the seventeen-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 18);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 18);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Eighteen details cross on the nineteenth total conflict, with the
        // tombstone and empty-suffix scan still bounded at the same row cap.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 18 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the nineteenth checkpoint conflict crosses the eighteen-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 19);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 19);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Nineteen details cross on the twentieth total conflict; preserve the
        // tombstone and empty-suffix scan bounds at this deeper tail.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 19 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twentieth checkpoint conflict crosses the nineteen-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 20);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 20);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty details cross on the twenty-first total conflict, keeping the
        // tombstone and empty-suffix closure bounded at this deeper tail.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 20 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-first checkpoint conflict crosses the twenty-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 21);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 21);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-one details cross on the twenty-second total conflict; the
        // tombstone and empty-suffix scans stay within their existing budget.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 21 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-second checkpoint conflict crosses the twenty-one-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 22);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 22);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-two details cross on the twenty-third total conflict, while
        // row and suffix scans remain bounded through this deeper tail.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 22 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-third checkpoint conflict crosses the twenty-two-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 23);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 23);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
            twenty_third_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-three details cross on the twenty-fourth total conflict,
        // retaining the same tombstone and empty-suffix scan bounds.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 23 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-fourth checkpoint conflict crosses the twenty-three-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 24);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 24);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
            twenty_third_conflict_id.as_slice(),
            twenty_fourth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-four details cross on the twenty-fifth total conflict, while
        // keeping tombstone and empty-suffix scanning within the row cap.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 24 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-fifth checkpoint conflict crosses the twenty-four-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 25);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 25);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
            twenty_third_conflict_id.as_slice(),
            twenty_fourth_conflict_id.as_slice(),
            twenty_fifth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-five details cross on the twenty-sixth total conflict, while
        // the tombstone and empty-suffix row scans remain bounded.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 25 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-sixth checkpoint conflict crosses the twenty-five-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 26);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 26);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
            twenty_third_conflict_id.as_slice(),
            twenty_fourth_conflict_id.as_slice(),
            twenty_fifth_conflict_id.as_slice(),
            twenty_sixth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-six details cross on the twenty-seventh total conflict, while
        // retaining bounded tombstone and empty-suffix scans.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 26 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-seventh checkpoint conflict crosses the twenty-six-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 27);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 27);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
            twenty_third_conflict_id.as_slice(),
            twenty_fourth_conflict_id.as_slice(),
            twenty_fifth_conflict_id.as_slice(),
            twenty_sixth_conflict_id.as_slice(),
            twenty_seventh_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-seven details cross on the twenty-eighth total conflict; row
        // and suffix scans stay within their existing budgets.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 27 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-eighth checkpoint conflict crosses the twenty-seven-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 28);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 28);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
            twenty_third_conflict_id.as_slice(),
            twenty_fourth_conflict_id.as_slice(),
            twenty_fifth_conflict_id.as_slice(),
            twenty_sixth_conflict_id.as_slice(),
            twenty_seventh_conflict_id.as_slice(),
            twenty_eighth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Twenty-eight details cross on the twenty-ninth total conflict, while
        // retaining the bounded tombstone and empty-suffix scans.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 28 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the twenty-ninth checkpoint conflict crosses the twenty-eight-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 29);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 29);
        for checkpoint_id in [
            checkpoint_id.as_slice(),
            tail_id.as_slice(),
            final_tail_id.as_slice(),
            closure_tail_id.as_slice(),
            terminal_closure_id.as_slice(),
            final_closure_id.as_slice(),
            terminal_tail_id.as_slice(),
            final_terminal_tail_id.as_slice(),
            final_conflict_id.as_slice(),
            terminal_checkpoint_id.as_slice(),
            eleventh_conflict_id.as_slice(),
            twelfth_conflict_id.as_slice(),
            thirteenth_conflict_id.as_slice(),
            fourteenth_conflict_id.as_slice(),
            fifteenth_conflict_id.as_slice(),
            sixteenth_conflict_id.as_slice(),
            seventeenth_conflict_id.as_slice(),
            eighteenth_conflict_id.as_slice(),
            nineteenth_conflict_id.as_slice(),
            twentieth_conflict_id.as_slice(),
            twenty_first_conflict_id.as_slice(),
            twenty_second_conflict_id.as_slice(),
            twenty_third_conflict_id.as_slice(),
            twenty_fourth_conflict_id.as_slice(),
            twenty_fifth_conflict_id.as_slice(),
            twenty_sixth_conflict_id.as_slice(),
            twenty_seventh_conflict_id.as_slice(),
            twenty_eighth_conflict_id.as_slice(),
            twenty_ninth_conflict_id.as_slice(),
        ] {
            assert!(report.affected_checkpoints.contains(checkpoint_id));
        }
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The twenty-ninth detail fits; the thirtieth sorted tail conflict
        // reports the lower bound while preserving tombstone and suffix scans.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 29 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirtieth checkpoint conflict crosses the twenty-nine-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 30);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 30);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_ninth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirtieth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The two trailing clean tombstones do not consume detail capacity;
        // traversal reaches the thirty-first fixture conflict after them.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 30 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-first checkpoint conflict crosses the thirty-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 31);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 31);
        assert!(report.affected_checkpoints.contains(thirtieth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_first_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The second clean pair also leaves the next conflict visible to the
        // shared budget instead of consuming detail capacity itself.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 31 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-second checkpoint conflict crosses the thirty-one-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 32);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 32);
        assert!(report.affected_checkpoints.contains(thirty_first_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_second_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(second_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(second_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The third clean pair leaves conflict 33 visible at the exact
        // thirty-two-detail boundary as well.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 32 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-third checkpoint conflict crosses the thirty-two-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 33);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 33);
        assert!(report.affected_checkpoints.contains(thirty_second_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_third_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(third_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(third_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The fourth clean pair also leaves conflict 34 visible at the
        // thirty-three-detail boundary.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 33 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-fourth checkpoint conflict crosses the thirty-three-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 34);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 34);
        assert!(report.affected_checkpoints.contains(thirty_third_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_fourth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(third_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(third_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fourth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fourth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The fifth clean pair also leaves conflict 35 visible at the
        // thirty-four-detail boundary.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 34 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-fifth checkpoint conflict crosses the thirty-four-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 35);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 35);
        assert!(report.affected_checkpoints.contains(thirty_fourth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_fifth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fourth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fourth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fifth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fifth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The sixth clean pair also leaves conflict 36 visible at the
        // thirty-five-detail boundary.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 35 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-sixth checkpoint conflict crosses the thirty-five-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 36);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 36);
        assert!(report.affected_checkpoints.contains(thirty_fifth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_sixth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fifth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fifth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(sixth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(sixth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The seventh clean pair leaves conflict 37 beyond a 36-detail cap,
        // without charging another row-budget unit.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 36 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-seventh checkpoint conflict crosses the thirty-six-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 37);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 37);
        assert!(report.affected_checkpoints.contains(thirty_sixth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_seventh_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(seventh_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(seventh_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The eighth clean pair leaves conflict 38 beyond a 37-detail cap,
        // without charging another row-budget unit.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 37 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-eighth checkpoint conflict crosses the thirty-seven-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 38);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 38);
        assert!(report.affected_checkpoints.contains(thirty_seventh_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_eighth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eighth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eighth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The ninth clean pair leaves conflict 39 beyond a 38-detail cap,
        // without charging another row-budget unit.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 38 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the thirty-ninth checkpoint conflict crosses the thirty-eight-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 39);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 39);
        assert!(report.affected_checkpoints.contains(thirty_eighth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_ninth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(ninth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(ninth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The tenth clean pair leaves conflict 40 beyond a 39-detail cap,
        // without charging another row-budget unit.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 39 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the fortieth checkpoint conflict crosses the thirty-nine-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 40);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 40);
        assert!(report.affected_checkpoints.contains(thirty_ninth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(fortieth_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(tenth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(tenth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The eleventh clean pair leaves conflict 41 beyond a 40-detail cap,
        // without charging another row-budget unit.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 40 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the forty-first checkpoint conflict crosses the forty-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 41);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 41);
        assert!(report.affected_checkpoints.contains(fortieth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(forty_first_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eleventh_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eleventh_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // The twelfth clean pair leaves conflict 42 beyond a 41-detail cap,
        // without charging another row-budget unit.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 41 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the forty-second checkpoint conflict crosses the forty-one-detail budget")
        };
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 42);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range.clone())));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 42);
        assert!(report.affected_checkpoints.contains(forty_first_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(forty_second_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(twelfth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(twelfth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // Exact capacity closes through conflict 42 after all twelve clean
        // tombstone pairs, without charging another row-budget unit.
        source.visited.clear();
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 2, max_conflicts: 42 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("exact conflict capacity closes through the forty-second checkpoint")
        };
        let updated = parse_checkpoint_fixture(CHECKPOINT_EDITED);
        let expected_tail_conflict = orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
            left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
            right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
        };
        assert_eq!(
            conflicts,
            vec![
                BranchMergeConflict::CheckpointConflict {
                    id: checkpoint_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                        left: if checkpoint_delete_on_left { None } else { Some(updated.clone()) },
                        right: if checkpoint_delete_on_left { Some(updated) } else { None },
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: tail_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: final_tail_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: closure_tail_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: terminal_closure_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: final_closure_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: terminal_tail_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: final_terminal_tail_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: final_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: terminal_checkpoint_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: eleventh_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twelfth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirteenth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: fourteenth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: fifteenth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: sixteenth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: seventeenth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: eighteenth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: nineteenth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twentieth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_first_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_second_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_third_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_fourth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_fifth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_sixth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_seventh_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_eighth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: twenty_ninth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirtieth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_first_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_second_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_third_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_fourth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_fifth_conflict_id.clone(),
                    conflict: expected_tail_conflict.clone(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_sixth_conflict_id.clone(),
                    conflict: expected_tail_conflict,
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_seventh_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_eighth_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: thirty_ninth_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: fortieth_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: forty_first_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: forty_second_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE)),
                        left: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT)),
                        right: Some(parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT)),
                    },
                },
            ]
        );
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.conflicts_lower_bound, 42);
        assert_eq!(report.affected_ranges.len(), 2);
        assert!(report.affected_ranges.contains(&(id(1), tombstone_range)));
        assert!(report.affected_ranges.contains(&(id(1), suffix_range)));
        assert_eq!(report.affected_checkpoints.len(), 42);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(closure_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_closure_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_terminal_tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(eleventh_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twelfth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirteenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(fourteenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(fifteenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(sixteenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(seventeenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(eighteenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(nineteenth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twentieth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_first_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_second_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_third_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_fourth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_fifth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_sixth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_seventh_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_eighth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(twenty_ninth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirtieth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_first_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_second_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_third_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_fourth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_fifth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_sixth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_seventh_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_eighth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(thirty_ninth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(fortieth_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(forty_first_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(forty_second_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(second_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(second_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(third_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(third_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fourth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fourth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fifth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(fifth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(sixth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(sixth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(seventh_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(seventh_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eighth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eighth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(ninth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(ninth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(tenth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(tenth_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eleventh_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(eleventh_trailing_unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(twelfth_trailing_agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(twelfth_trailing_unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 6);
        assert!(source.visited[..3].iter().all(|(_, locator)| locator.ends_with(b"upper")));
        assert_eq!(
            &source.visited[3..],
            &[
                (MergeSide::Base, b"base-empty-suffix".to_vec()),
                (MergeSide::Left, b"left-empty-suffix".to_vec()),
                (MergeSide::Right, b"right-empty-suffix".to_vec()),
            ]
        );

        // ORNA-MERGE-011 requires divergent opaque checkpoints to conflict,
        // but is silent on position resets and cursorless presence. Snapshot
        // membership marks presence locally; test both position transitions
        // while the opposite branch deletes the checkpoint.
        for checkpoint_fixtures in [
            (CHECKPOINT_POSITIONLESS, CHECKPOINT_POSITIONLESS_EDITED),
            (CHECKPOINT_BASE, CHECKPOINT_RESET),
        ] {
            let (
                base,
                left,
                right,
                mut source,
                agreed_delete_id,
                unchanged_delete_id,
                checkpoint_id,
                tail_id,
            ) = build_inputs(
                row_delete_on_left,
                checkpoint_delete_on_left,
                true,
                checkpoint_fixtures,
            );
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
            )
            .unwrap_err();
            let BranchMergeError::BudgetExceeded { report } = error else {
                panic!("the checkpoint conflict follows the row tombstone")
            };
            assert_eq!(report.conflicts_lower_bound, 1);
            assert_eq!(report.rows_examined, 2);
            assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
            assert_eq!(report.affected_checkpoints.len(), 1);
            assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(tail_id.as_slice()));

            let (
                mut base,
                mut left,
                mut right,
                mut source,
                agreed_delete_id,
                unchanged_delete_id,
                checkpoint_id,
                tail_id,
            ) = build_inputs(
                row_delete_on_left,
                checkpoint_delete_on_left,
                true,
                checkpoint_fixtures,
            );
            for snapshot in [&mut base, &mut left, &mut right] {
                snapshot.checkpoints.remove(&tail_id);
            }
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
            )
            .unwrap_err();
            let BranchMergeError::Conflicts { conflicts, report } = error else {
                panic!("one detail retains the checkpoint delete/update conflict")
            };
            let (base_fixture, updated_fixture) = checkpoint_fixtures;
            let updated_checkpoint = parse_checkpoint_fixture(updated_fixture);
            assert_eq!(
                conflicts,
                vec![BranchMergeConflict::CheckpointConflict {
                    id: checkpoint_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(base_fixture)),
                        left: if checkpoint_delete_on_left { None } else { Some(updated_checkpoint.clone()) },
                        right: if checkpoint_delete_on_left { Some(updated_checkpoint) } else { None },
                    },
                }]
            );
            assert_eq!(report.conflicts_lower_bound, 1);
            assert_eq!(report.rows_examined, 2);
            assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
            assert_eq!(report.affected_checkpoints.len(), 1);
            assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
            assert_eq!(source.visited.len(), 3);
        }
    }
}

#[test]
fn checkpoint_creation_orientations_survive_row_tombstones() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);

    // ORNA-MERGE-011 requires conflicts for divergent opaque checkpoints but
    // leaves one-sided checkpoint creation beside an independent row
    // tombstone unspecified. Treat map membership as presence and merge the
    // independent creation and deletion in all four branch orientations.
    for (row_delete_on_left, checkpoint_create_on_left) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        let checkpoint_id = b"consumer/new-after-row-tombstone".to_vec();
        let checkpoint_fixture = if checkpoint_create_on_left {
            CHECKPOINT_BASE
        } else {
            CHECKPOINT_EDITED
        };
        let checkpoint = parse_checkpoint_fixture(checkpoint_fixture);
        let create_side = if checkpoint_create_on_left { &mut left } else { &mut right };
        create_side.checkpoints.insert(checkpoint_id.clone(), checkpoint.clone());

        let plan = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap();

        assert_eq!(plan.report.conflicts_lower_bound, 0);
        assert_eq!(plan.checkpoints.get(&checkpoint_id), Some(&checkpoint));
        let segments = &plan.tables[&id(1)].segments;
        let MergedSegment::Rows { rows, tombstones, .. } = &segments[0] else {
            panic!("the independent row deletion must materialize its tombstone")
        };
        assert!(rows.is_empty());
        assert_eq!(tombstones, &[deleted_row.key.clone()]);
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn divergent_checkpoint_creation_closes_conflict_tail_after_row_tombstone() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let checkpoint_id = b"consumer/m-new-divergent".to_vec();
    let tail_id = b"consumer/z-existing-conflict".to_vec();
    let final_tail_id = b"consumer/zz-final-conflict".to_vec();
    let tail_fixtures = (CHECKPOINT_TAIL_BASE, CHECKPOINT_TAIL_LEFT, CHECKPOINT_TAIL_RIGHT);

    // ORNA-MERGE-011 requires divergent opaque checkpoints to conflict but
    // does not spell out concurrent creation when the base has no entry.
    // Treat unequal additions as divergent and walk them after the row phase,
    // so a clean row tombstone contributes its range before checkpoint caps.
    let build_inputs = |row_delete_on_left, left_fixture, right_fixture| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        left.checkpoints.insert(checkpoint_id.clone(), parse_checkpoint_fixture(left_fixture));
        right.checkpoints.insert(checkpoint_id.clone(), parse_checkpoint_fixture(right_fixture));
        for tail in [&tail_id, &final_tail_id] {
            base.checkpoints.insert(tail.clone(), parse_checkpoint_fixture(tail_fixtures.0));
            left.checkpoints.insert(tail.clone(), parse_checkpoint_fixture(tail_fixtures.1));
            right.checkpoints.insert(tail.clone(), parse_checkpoint_fixture(tail_fixtures.2));
        }
        (base, left, right, source)
    };

    // Cross the row-delete side with which divergent creation lands on the
    // left, proving fixture orientation and conflict closure in each case.
    for (row_delete_on_left, left_fixture, right_fixture) in [
        (true, CHECKPOINT_BASE, CHECKPOINT_EDITED),
        (true, CHECKPOINT_EDITED, CHECKPOINT_BASE),
        (false, CHECKPOINT_BASE, CHECKPOINT_EDITED),
        (false, CHECKPOINT_EDITED, CHECKPOINT_BASE),
    ] {
        let (base, left, right, mut source) =
            build_inputs(row_delete_on_left, left_fixture, right_fixture);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the first divergent creation follows the row tombstone")
        };
        assert_eq!(report.conflicts_lower_bound, 1);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) =
            build_inputs(row_delete_on_left, left_fixture, right_fixture);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the existing checkpoint conflict crosses the one-detail cap")
        };
        assert_eq!(report.conflicts_lower_bound, 2);
        assert_eq!(report.affected_checkpoints.len(), 2);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) =
            build_inputs(row_delete_on_left, left_fixture, right_fixture);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the terminal fixture conflict crosses the two-detail cap")
        };
        assert_eq!(report.conflicts_lower_bound, 3);
        assert_eq!(report.affected_checkpoints.len(), 3);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) =
            build_inputs(row_delete_on_left, left_fixture, right_fixture);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("all three fixture conflicts fit the exact detail cap")
        };
        let expected_left = parse_checkpoint_fixture(left_fixture);
        let expected_right = parse_checkpoint_fixture(right_fixture);
        assert_eq!(
            conflicts,
            vec![
                BranchMergeConflict::CheckpointConflict {
                    id: checkpoint_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: None,
                        left: Some(expected_left),
                        right: Some(expected_right),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: tail_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(tail_fixtures.0)),
                        left: Some(parse_checkpoint_fixture(tail_fixtures.1)),
                        right: Some(parse_checkpoint_fixture(tail_fixtures.2)),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: final_tail_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(tail_fixtures.0)),
                        left: Some(parse_checkpoint_fixture(tail_fixtures.1)),
                        right: Some(parse_checkpoint_fixture(tail_fixtures.2)),
                    },
                },
            ]
        );
        assert_eq!(report.conflicts_lower_bound, 3);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn checkpoint_tombstones_close_after_conflict_tail_around_row_tombstone() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let delete_update_id = b"consumer/m-delete-update".to_vec();
    let divergent_id = b"consumer/n-divergent".to_vec();
    let agreed_delete_id = b"consumer/y-agreed-tombstone".to_vec();
    let unchanged_delete_id = b"consumer/z-delete-unchanged".to_vec();
    let terminal_conflict_id = b"consumer/zz-terminal-conflict".to_vec();
    let trailing_delete_id = b"consumer/zzz-final-tombstone".to_vec();
    let tail_fixtures = (CHECKPOINT_TAIL_BASE, CHECKPOINT_TAIL_LEFT, CHECKPOINT_TAIL_RIGHT);

    // ORNA-MERGE-005 bounds conflict details but leaves checkpoint traversal
    // and clean tombstone accounting open. Snapshot membership marks presence;
    // agreed deletes and delete-versus-unchanged entries stay clean in the
    // sorted tail and do not consume the shared conflict budget.
    let build_inputs = |row_delete_on_left, checkpoint_delete_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);

        base.checkpoints.insert(delete_update_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        let retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
        retained_side.checkpoints.insert(
            delete_update_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_EDITED),
        );

        base.checkpoints.insert(agreed_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        base.checkpoints.insert(unchanged_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        retained_side.checkpoints.insert(unchanged_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));

        for conflict_id in [&divergent_id, &terminal_conflict_id] {
            base.checkpoints.insert(conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.0));
            left.checkpoints.insert(conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.1));
            right.checkpoints.insert(conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.2));
        }
        base.checkpoints.insert(trailing_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));

        (base, left, right, source)
    };

    // Cross row tombstones with both orientations of the clean checkpoint
    // delete, then close the budget tail through two terminal conflicts.
    for (row_delete_on_left, checkpoint_delete_on_left) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let (base, left, right, mut source) = build_inputs(row_delete_on_left, checkpoint_delete_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the terminal checkpoint conflict crosses the two-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 3);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 3);
        assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
        assert!(report.affected_checkpoints.contains(divergent_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) = build_inputs(row_delete_on_left, checkpoint_delete_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("three fixture conflicts fit exactly while tombstones close the tail")
        };
        let updated = parse_checkpoint_fixture(CHECKPOINT_EDITED);
        let expected_tail = |base_fixture, left_fixture, right_fixture| {
            orna_evolution_v1::CheckpointMergeConflict {
                base: Some(parse_checkpoint_fixture(base_fixture)),
                left: Some(parse_checkpoint_fixture(left_fixture)),
                right: Some(parse_checkpoint_fixture(right_fixture)),
            }
        };
        assert_eq!(
            conflicts,
            vec![
                BranchMergeConflict::CheckpointConflict {
                    id: delete_update_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                        left: if checkpoint_delete_on_left { None } else { Some(updated.clone()) },
                        right: if checkpoint_delete_on_left { Some(updated) } else { None },
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: divergent_id.clone(),
                    conflict: expected_tail(tail_fixtures.0, tail_fixtures.1, tail_fixtures.2),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: terminal_conflict_id.clone(),
                    conflict: expected_tail(tail_fixtures.0, tail_fixtures.1, tail_fixtures.2),
                },
            ]
        );
        assert_eq!(report.conflicts_lower_bound, 3);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 3);
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn checkpoint_delete_update_conflict_closes_after_earlier_conflict_and_row_tombstone() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let earlier_conflict_id = b"consumer/a-earlier-conflict".to_vec();
    let delete_update_id = b"consumer/m-delete-update-tail".to_vec();
    let agreed_delete_id = b"consumer/n-agreed-tombstone".to_vec();
    let unchanged_delete_id = b"consumer/o-delete-unchanged".to_vec();
    let terminal_conflict_id = b"consumer/z-terminal-conflict".to_vec();
    let tail_fixtures = (CHECKPOINT_TAIL_BASE, CHECKPOINT_TAIL_LEFT, CHECKPOINT_TAIL_RIGHT);

    // ORNA-MERGE-005 bounds conflict details but leaves cross-phase traversal
    // open. Storage materializes the clean row tombstone first, then visits
    // checkpoint IDs in stable order; a delete/update conflict consumes the
    // same budget as earlier divergence, while clean tombstones consume none.
    let build_inputs = |row_delete_on_left, checkpoint_delete_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for conflict_id in [&earlier_conflict_id, &terminal_conflict_id] {
            base.checkpoints.insert(conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.0));
            left.checkpoints.insert(conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.1));
            right.checkpoints.insert(conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.2));
        }

        base.checkpoints.insert(delete_update_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        let retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
        retained_side.checkpoints.insert(delete_update_id.clone(), parse_checkpoint_fixture(CHECKPOINT_EDITED));
        base.checkpoints.insert(agreed_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        base.checkpoints.insert(unchanged_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        retained_side
            .checkpoints
            .insert(unchanged_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));

        (base, left, right, source)
    };

    // Cross the row tombstone with both checkpoint delete orientations. The
    // m tombstone conflict follows the a divergence and precedes clean tail
    // tombstones; z proves the capped scan continues through both clean IDs.
    for (row_delete_on_left, checkpoint_delete_on_left) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let (base, left, right, mut source) = build_inputs(row_delete_on_left, checkpoint_delete_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the delete/update checkpoint conflict crosses the first detail")
        };
        assert_eq!(report.conflicts_lower_bound, 2);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 2);
        assert!(report.affected_checkpoints.contains(earlier_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(terminal_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) = build_inputs(row_delete_on_left, checkpoint_delete_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the terminal fixture conflict crosses after clean checkpoint tombstones")
        };
        assert_eq!(report.conflicts_lower_bound, 3);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 3);
        assert!(report.affected_checkpoints.contains(earlier_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) = build_inputs(row_delete_on_left, checkpoint_delete_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("all three fixture conflicts fit the exact detail budget")
        };
        let updated = parse_checkpoint_fixture(CHECKPOINT_EDITED);
        let expected_tail = || orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(tail_fixtures.0)),
            left: Some(parse_checkpoint_fixture(tail_fixtures.1)),
            right: Some(parse_checkpoint_fixture(tail_fixtures.2)),
        };
        assert_eq!(
            conflicts,
            vec![
                BranchMergeConflict::CheckpointConflict {
                    id: earlier_conflict_id.clone(),
                    conflict: expected_tail(),
                },
                BranchMergeConflict::CheckpointConflict {
                    id: delete_update_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                        left: if checkpoint_delete_on_left { None } else { Some(updated.clone()) },
                        right: if checkpoint_delete_on_left { Some(updated) } else { None },
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: terminal_conflict_id.clone(),
                    conflict: expected_tail(),
                },
            ]
        );
        assert_eq!(report.conflicts_lower_bound, 3);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 3);
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn terminal_checkpoint_delete_update_closes_after_clean_tombstone_tail() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let earlier_conflict_id = b"consumer/a-earlier-conflict".to_vec();
    let agreed_delete_id = b"consumer/m-agreed-tombstone".to_vec();
    let unchanged_delete_id = b"consumer/n-delete-unchanged".to_vec();
    let terminal_delete_update_id = b"consumer/zz-delete-update-terminal".to_vec();
    let trailing_delete_id = b"consumer/zzz-after-conflict-tombstone".to_vec();
    let tail_fixtures = (CHECKPOINT_TAIL_BASE, CHECKPOINT_TAIL_LEFT, CHECKPOINT_TAIL_RIGHT);
    let checkpoint_fixture_pairs = [
        (CHECKPOINT_BASE, CHECKPOINT_EDITED),
        (CHECKPOINT_POSITIONLESS, CHECKPOINT_POSITIONLESS_EDITED),
        (CHECKPOINT_BASE, CHECKPOINT_RESET),
    ];

    // ORNA-MERGE-005 leaves traversal open. Storage resolves the clean row
    // tombstone first, then walks stable checkpoint IDs; clean checkpoint
    // tombstones do not consume conflict details. ORNA-MERGE-011 requires
    // divergent opaque values to conflict but does not distinguish cursorless
    // presence from a position reset beside a delete. Outer map membership is
    // the local existence marker, including these terminal delete/update cases.
    let build_inputs = |
        row_delete_on_left,
        checkpoint_delete_on_left,
        checkpoint_fixtures: (&str, &str),
    | {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        base.checkpoints.insert(earlier_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.0));
        left.checkpoints.insert(earlier_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.1));
        right.checkpoints.insert(earlier_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.2));

        base.checkpoints.insert(agreed_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        base.checkpoints.insert(unchanged_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        let retained_side = if checkpoint_delete_on_left { &mut right } else { &mut left };
        retained_side
            .checkpoints
            .insert(unchanged_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));

        base.checkpoints.insert(
            terminal_delete_update_id.clone(),
            parse_checkpoint_fixture(checkpoint_fixtures.0),
        );
        retained_side.checkpoints.insert(
            terminal_delete_update_id.clone(),
            parse_checkpoint_fixture(checkpoint_fixtures.1),
        );
        base.checkpoints.insert(trailing_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));

        (base, left, right, source)
    };

    // Exercise both row-tombstone and checkpoint-delete orientations across
    // ordinary, cursorless-advance, and position-reset fixture transitions.
    // The terminal checkpoint conflict follows an earlier divergence and
    // clean deletions, with another clean tombstone after the conflict tail.
    for checkpoint_fixtures in checkpoint_fixture_pairs {
        for (row_delete_on_left, checkpoint_delete_on_left) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let (base, left, right, mut source) =
                build_inputs(row_delete_on_left, checkpoint_delete_on_left, checkpoint_fixtures);
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
            )
            .unwrap_err();
            let BranchMergeError::BudgetExceeded { report } = error else {
                panic!("the terminal delete/update conflict crosses the one-detail cap")
            };
            assert_eq!(report.conflicts_lower_bound, 2);
            assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
            assert_eq!(report.affected_checkpoints.len(), 2);
            assert!(report.affected_checkpoints.contains(earlier_conflict_id.as_slice()));
            assert!(report.affected_checkpoints.contains(terminal_delete_update_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
            assert_eq!(source.visited.len(), 3);

            let (base, left, right, mut source) =
                build_inputs(row_delete_on_left, checkpoint_delete_on_left, checkpoint_fixtures);
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
            )
            .unwrap_err();
            let BranchMergeError::Conflicts { conflicts, report } = error else {
                panic!("both fixture conflicts fit exactly while the trailing tombstone closes")
            };
            let expected_earlier = orna_evolution_v1::CheckpointMergeConflict {
                base: Some(parse_checkpoint_fixture(tail_fixtures.0)),
                left: Some(parse_checkpoint_fixture(tail_fixtures.1)),
                right: Some(parse_checkpoint_fixture(tail_fixtures.2)),
            };
            let updated = parse_checkpoint_fixture(checkpoint_fixtures.1);
            assert_eq!(
                conflicts,
                vec![
                    BranchMergeConflict::CheckpointConflict {
                        id: earlier_conflict_id.clone(),
                        conflict: expected_earlier,
                    },
                    BranchMergeConflict::CheckpointConflict {
                        id: terminal_delete_update_id.clone(),
                        conflict: orna_evolution_v1::CheckpointMergeConflict {
                            base: Some(parse_checkpoint_fixture(checkpoint_fixtures.0)),
                            left: if checkpoint_delete_on_left { None } else { Some(updated.clone()) },
                            right: if checkpoint_delete_on_left { Some(updated) } else { None },
                        },
                    },
                ]
            );
            assert_eq!(report.conflicts_lower_bound, 2);
            assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
            assert_eq!(report.affected_checkpoints.len(), 2);
            assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
            assert_eq!(source.visited.len(), 3);
        }
    }
}

#[test]
fn reset_and_advance_conflict_closes_after_checkpoint_tombstones() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let earlier_conflict_id = b"consumer/a-earlier-conflict".to_vec();
    let agreed_delete_id = b"consumer/m-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/n-unchanged".to_vec();
    let reset_conflict_id = b"consumer/z-reset-versus-advance".to_vec();
    let trailing_delete_id = b"consumer/zz-trailing-tombstone".to_vec();
    let tail_fixtures = (CHECKPOINT_TAIL_BASE, CHECKPOINT_TAIL_LEFT, CHECKPOINT_TAIL_RIGHT);

    // ORNA-MERGE-011 requires divergent opaque checkpoints to conflict but
    // does not describe reset-versus-advance when both sides retain the
    // checkpoint. Keep outer map membership as presence and compare the full
    // generation/position value, so position: None remains a real reset.
    // ORNA-MERGE-005 leaves traversal open; storage resolves row changes
    // first, then walks stable checkpoint IDs without charging clean tombstones.
    let build_inputs = |row_delete_on_left, reset_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        base.checkpoints.insert(earlier_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.0));
        left.checkpoints.insert(earlier_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.1));
        right.checkpoints.insert(earlier_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.2));

        base.checkpoints.insert(agreed_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        base.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        let (reset_fixture, advance_fixture) = (CHECKPOINT_RESET, CHECKPOINT_EDITED);
        if reset_on_left {
            left.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(reset_fixture));
            right.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(advance_fixture));
        } else {
            left.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(advance_fixture));
            right.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(reset_fixture));
        }
        base.checkpoints.insert(trailing_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));

        (base, left, right, source)
    };

    // Cross the row-tombstone orientation with the resetting branch. A prior
    // conflict fits, clean tombstones stay free, and the reset/advance edge is
    // the terminal conflict with full left/right fixture values.
    for (row_delete_on_left, reset_on_left) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let (base, left, right, mut source) = build_inputs(row_delete_on_left, reset_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the terminal reset/advance conflict crosses the one-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 2);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 2);
        assert!(report.affected_checkpoints.contains(earlier_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(reset_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) = build_inputs(row_delete_on_left, reset_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("both fixture conflicts fit while the trailing tombstone closes")
        };
        let reset = parse_checkpoint_fixture(CHECKPOINT_RESET);
        let advance = parse_checkpoint_fixture(CHECKPOINT_EDITED);
        assert_eq!(
            conflicts,
            vec![
                BranchMergeConflict::CheckpointConflict {
                    id: earlier_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(tail_fixtures.0)),
                        left: Some(parse_checkpoint_fixture(tail_fixtures.1)),
                        right: Some(parse_checkpoint_fixture(tail_fixtures.2)),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: reset_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                        left: Some(if reset_on_left { reset.clone() } else { advance.clone() }),
                        right: Some(if reset_on_left { advance } else { reset }),
                    },
                },
            ]
        );
        assert_eq!(report.conflicts_lower_bound, 2);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 2);
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn zero_budget_reset_advance_conflict_closes_after_checkpoint_tombstones() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let agreed_delete_id = b"consumer/a-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/b-unchanged".to_vec();
    let reset_conflict_id = b"consumer/m-reset-versus-advance".to_vec();
    let trailing_delete_id = b"consumer/z-trailing-tombstone".to_vec();

    // ORNA-MERGE-011 requires divergent retained opaque checkpoint values to
    // conflict but leaves reset-versus-advance resolution open. Here the base
    // is a present cursorless checkpoint; compare its full generation/position
    // value and keep position: None distinct from checkpoint absence. For
    // ORNA-MERGE-005's open traversal details, resolve row changes first and
    // walk stable checkpoint IDs, with clean tombstones free of conflict budget.
    let build_inputs = |row_delete_on_left, reset_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        base.checkpoints.insert(agreed_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        base.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        let (reset_fixture, advance_fixture) = (CHECKPOINT_RESET, CHECKPOINT_POSITIONLESS_EDITED);
        if reset_on_left {
            left.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(reset_fixture));
            right.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(advance_fixture));
        } else {
            left.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(advance_fixture));
            right.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(reset_fixture));
        }
        base.checkpoints.insert(trailing_delete_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));

        (base, left, right, source)
    };

    // Cross both row-delete and resetting-branch orientations. The reset / advance
    // conflict is the first real checkpoint conflict after a clean delete, while
    // another clean tombstone after it must still be traversed at the exact budget.
    for (row_delete_on_left, reset_on_left) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let (base, left, right, mut source) = build_inputs(row_delete_on_left, reset_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the first reset/advance conflict crosses a zero-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 1);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(report.affected_checkpoints.contains(reset_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) = build_inputs(row_delete_on_left, reset_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("the reset/advance detail fits and traversal closes on the trailing tombstone")
        };
        let reset = parse_checkpoint_fixture(CHECKPOINT_RESET);
        let advance = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
        assert_eq!(
            conflicts,
            vec![BranchMergeConflict::CheckpointConflict {
                id: reset_conflict_id.clone(),
                conflict: orna_evolution_v1::CheckpointMergeConflict {
                    base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
                    left: Some(if reset_on_left { reset.clone() } else { advance.clone() }),
                    right: Some(if reset_on_left { advance } else { reset }),
                },
            }]
        );
        assert_eq!(report.conflicts_lower_bound, 1);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn zero_budget_reset_advance_precedes_terminal_checkpoint_conflict() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let agreed_before_id = b"consumer/a-agreed-tombstone".to_vec();
    let reset_conflict_id = b"consumer/m-reset-versus-advance".to_vec();
    let agreed_between_id = b"consumer/n-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/p-unchanged".to_vec();
    let terminal_conflict_id = b"consumer/z-terminal-conflict".to_vec();
    let trailing_delete_id = b"consumer/zz-trailing-tombstone".to_vec();
    let tail_fixtures = (CHECKPOINT_TAIL_BASE, CHECKPOINT_TAIL_LEFT, CHECKPOINT_TAIL_RIGHT);

    // The checkpoint reference defines reset preconditions and typed positions,
    // but leaves merge traversal open. Compare complete generation/position
    // values, keep position: None present, resolve row changes first, and then
    // visit stable checkpoint IDs. Clean tombstones do not consume conflict budget.
    let build_inputs = |row_delete_on_left, reset_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for id in [&agreed_before_id, &agreed_between_id, &trailing_delete_id] {
            base.checkpoints.insert(id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        base.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        let (reset_fixture, advance_fixture) = (CHECKPOINT_RESET, CHECKPOINT_POSITIONLESS_EDITED);
        if reset_on_left {
            left.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(reset_fixture));
            right.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(advance_fixture));
        } else {
            left.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(advance_fixture));
            right.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(reset_fixture));
        }

        base.checkpoints.insert(terminal_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.0));
        left.checkpoints.insert(terminal_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.1));
        right.checkpoints.insert(terminal_conflict_id.clone(), parse_checkpoint_fixture(tail_fixtures.2));
        (base, left, right, source)
    };

    // Cross row-delete and reset orientations. The reset conflict follows a
    // clean tombstone and is the zero-budget boundary; another divergence after
    // a clean tombstone crosses a one-detail budget, before the trailing tombstone.
    for (row_delete_on_left, reset_on_left) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let (base, left, right, mut source) = build_inputs(row_delete_on_left, reset_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the first reset/advance conflict crosses the zero-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 1);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 1);
        assert!(report.affected_checkpoints.contains(reset_conflict_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) = build_inputs(row_delete_on_left, reset_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the terminal checkpoint conflict crosses the one-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 2);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 2);
        assert!(report.affected_checkpoints.contains(reset_conflict_id.as_slice()));
        assert!(report.affected_checkpoints.contains(terminal_conflict_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);

        let (base, left, right, mut source) = build_inputs(row_delete_on_left, reset_on_left);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::Conflicts { conflicts, report } = error else {
            panic!("both fixture conflicts fit and traversal closes across the trailing tombstone")
        };
        let reset = parse_checkpoint_fixture(CHECKPOINT_RESET);
        let advance = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
        assert_eq!(
            conflicts,
            vec![
                BranchMergeConflict::CheckpointConflict {
                    id: reset_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
                        left: Some(if reset_on_left { reset.clone() } else { advance.clone() }),
                        right: Some(if reset_on_left { advance } else { reset }),
                    },
                },
                BranchMergeConflict::CheckpointConflict {
                    id: terminal_conflict_id.clone(),
                    conflict: orna_evolution_v1::CheckpointMergeConflict {
                        base: Some(parse_checkpoint_fixture(tail_fixtures.0)),
                        left: Some(parse_checkpoint_fixture(tail_fixtures.1)),
                        right: Some(parse_checkpoint_fixture(tail_fixtures.2)),
                    },
                },
            ]
        );
        assert_eq!(report.conflicts_lower_bound, 2);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert_eq!(report.affected_checkpoints.len(), 2);
        assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn reset_advance_conflicts_follow_checkpoint_id_order_across_tombstones() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let agreed_before_id = b"consumer/a-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/b-unchanged".to_vec();
    let first_reset_id = b"consumer/m-reset-first".to_vec();
    let agreed_between_id = b"consumer/n-agreed-tombstone".to_vec();
    let terminal_reset_id = b"consumer/z-reset-terminal".to_vec();
    let trailing_delete_id = b"consumer/zz-trailing-tombstone".to_vec();
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);

    // Reset preconditions and typed-position equality are specified, but merge
    // ordering is open. Keep position: None present, compare the full value,
    // resolve rows first, and walk stable checkpoint IDs. Clean tombstones stay
    // free of conflict budget.
    let build_inputs = |row_delete_on_left, first_reset_on_left, terminal_reset_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [&agreed_before_id, &agreed_between_id, &trailing_delete_id] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        for (checkpoint_id, reset_on_left) in [
            (&first_reset_id, first_reset_on_left),
            (&terminal_reset_id, terminal_reset_on_left),
        ] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
            if reset_on_left {
                left.checkpoints.insert(checkpoint_id.to_vec(), reset_fixture.clone());
                right.checkpoints.insert(checkpoint_id.to_vec(), advance_fixture.clone());
            } else {
                left.checkpoints.insert(checkpoint_id.to_vec(), advance_fixture.clone());
                right.checkpoints.insert(checkpoint_id.to_vec(), reset_fixture.clone());
            }
        }

        (base, left, right, source)
    };
    let expected_conflict = |checkpoint_id: &Vec<u8>, reset_on_left| {
        BranchMergeConflict::CheckpointConflict {
            id: checkpoint_id.clone(),
            conflict: orna_evolution_v1::CheckpointMergeConflict {
                base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
                left: Some(if reset_on_left { reset_fixture.clone() } else { advance_fixture.clone() }),
                right: Some(if reset_on_left { advance_fixture.clone() } else { reset_fixture.clone() }),
            },
        }
    };

    // Cross row-tombstone placement with both reset-side orientations at both
    // IDs. A clean deletion precedes the first reset, another separates them,
    // and a final tombstone follows the exact two-detail boundary.
    for row_delete_on_left in [true, false] {
        for (first_reset_on_left, terminal_reset_on_left) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let (base, left, right, mut source) = build_inputs(
                row_delete_on_left,
                first_reset_on_left,
                terminal_reset_on_left,
            );
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
            )
            .unwrap_err();
            let BranchMergeError::BudgetExceeded { report } = error else {
                panic!("zero detail budget stops at the first reset conflict")
            };
            assert_eq!(report.conflicts_lower_bound, 1);
            assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
            assert_eq!(report.affected_checkpoints.len(), 1);
            assert!(report.affected_checkpoints.contains(first_reset_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(terminal_reset_id.as_slice()));
            assert_eq!(source.visited.len(), 3);

            let (base, left, right, mut source) = build_inputs(
                row_delete_on_left,
                first_reset_on_left,
                terminal_reset_on_left,
            );
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
            )
            .unwrap_err();
            let BranchMergeError::BudgetExceeded { report } = error else {
                panic!("the later reset conflict crosses the one-detail budget")
            };
            assert_eq!(report.conflicts_lower_bound, 2);
            assert_eq!(report.affected_checkpoints.len(), 2);
            assert!(report.affected_checkpoints.contains(first_reset_id.as_slice()));
            assert!(report.affected_checkpoints.contains(terminal_reset_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
            assert_eq!(source.visited.len(), 3);

            let (base, left, right, mut source) = build_inputs(
                row_delete_on_left,
                first_reset_on_left,
                terminal_reset_on_left,
            );
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
            )
            .unwrap_err();
            let BranchMergeError::Conflicts { conflicts, report } = error else {
                panic!("both reset conflicts fit and traversal closes after the trailing tombstone")
            };
            assert_eq!(
                conflicts,
                vec![
                    expected_conflict(&first_reset_id, first_reset_on_left),
                    expected_conflict(&terminal_reset_id, terminal_reset_on_left),
                ]
            );
            assert_eq!(report.conflicts_lower_bound, 2);
            assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
            assert_eq!(report.affected_checkpoints.len(), 2);
            assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
            assert_eq!(source.visited.len(), 3);
        }
    }
}

#[test]
fn reset_then_delete_update_conflict_closes_after_clean_tombstones() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let agreed_before_id = b"consumer/a-agreed-tombstone".to_vec();
    let reset_conflict_id = b"consumer/m-reset-versus-advance".to_vec();
    let agreed_between_id = b"consumer/n-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/p-unchanged".to_vec();
    let terminal_delete_update_id = b"consumer/z-delete-update-terminal".to_vec();
    let trailing_delete_id = b"consumer/zz-trailing-tombstone".to_vec();
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
    let updated_fixture = parse_checkpoint_fixture(CHECKPOINT_EDITED);

    // The checkpoint reference defines reset preconditions and typed positions
    // but leaves conflict traversal open. Compare full values, retain map
    // membership as presence, finish row planning first, then visit stable IDs.
    // Clean tombstones remain free of conflict budget.
    let build_inputs = |row_delete_on_left, reset_on_left, checkpoint_delete_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [&agreed_before_id, &agreed_between_id, &trailing_delete_id] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        base.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        if reset_on_left {
            left.checkpoints.insert(reset_conflict_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(reset_conflict_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(reset_conflict_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(reset_conflict_id.clone(), reset_fixture.clone());
        }

        base.checkpoints.insert(terminal_delete_update_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        if checkpoint_delete_on_left {
            right.checkpoints.insert(terminal_delete_update_id.clone(), updated_fixture.clone());
        } else {
            left.checkpoints.insert(terminal_delete_update_id.clone(), updated_fixture.clone());
        }
        (base, left, right, source)
    };

    let expected_reset = |reset_on_left| BranchMergeConflict::CheckpointConflict {
        id: reset_conflict_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_on_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_on_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };
    // Cross row-delete, reset-side, and checkpoint-delete orientations. Reset
    // is the zero-budget boundary; delete/update is the terminal conflict after
    // a clean tombstone, and another clean tombstone follows the exact budget.
    for row_delete_on_left in [true, false] {
        for reset_on_left in [true, false] {
            for checkpoint_delete_on_left in [true, false] {
                let (base, left, right, mut source) =
                    build_inputs(row_delete_on_left, reset_on_left, checkpoint_delete_on_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
                )
                .unwrap_err();
                let BranchMergeError::BudgetExceeded { report } = error else {
                    panic!("zero detail budget stops at the reset-versus-advance conflict")
                };
                assert_eq!(report.conflicts_lower_bound, 1);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 1);
                assert!(report.affected_checkpoints.contains(reset_conflict_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(terminal_delete_update_id.as_slice()));
                assert_eq!(source.visited.len(), 3);

                let (base, left, right, mut source) =
                    build_inputs(row_delete_on_left, reset_on_left, checkpoint_delete_on_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
                )
                .unwrap_err();
                let BranchMergeError::BudgetExceeded { report } = error else {
                    panic!("the terminal delete/update conflict crosses the one-detail budget")
                };
                assert_eq!(report.conflicts_lower_bound, 2);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(report.affected_checkpoints.contains(reset_conflict_id.as_slice()));
                assert!(report.affected_checkpoints.contains(terminal_delete_update_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                assert_eq!(source.visited.len(), 3);

                let (base, left, right, mut source) =
                    build_inputs(row_delete_on_left, reset_on_left, checkpoint_delete_on_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
                )
                .unwrap_err();
                let BranchMergeError::Conflicts { conflicts, report } = error else {
                    panic!("both conflicts fit and traversal closes over the trailing tombstone")
                };
                assert_eq!(
                    conflicts,
                    vec![
                        expected_reset(reset_on_left),
                        BranchMergeConflict::CheckpointConflict {
                            id: terminal_delete_update_id.clone(),
                            conflict: orna_evolution_v1::CheckpointMergeConflict {
                                base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                                left: if checkpoint_delete_on_left { None } else { Some(updated_fixture.clone()) },
                                right: if checkpoint_delete_on_left { Some(updated_fixture.clone()) } else { None },
                            },
                        },
                    ]
                );
                assert_eq!(report.conflicts_lower_bound, 2);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn delete_update_conflict_precedes_reset_closure_across_tombstones() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let agreed_before_id = b"consumer/a-agreed-tombstone".to_vec();
    let first_delete_update_id = b"consumer/m-delete-update-first".to_vec();
    let agreed_between_id = b"consumer/n-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/p-unchanged".to_vec();
    let terminal_reset_id = b"consumer/z-reset-terminal".to_vec();
    let trailing_delete_id = b"consumer/zz-trailing-tombstone".to_vec();
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
    let updated_fixture = parse_checkpoint_fixture(CHECKPOINT_EDITED);

    // The checkpoint reference defines reset preconditions and typed positions
    // but leaves conflict traversal open. Compare full values, retain map
    // membership as presence, finish row planning first, then visit stable IDs.
    // Clean tombstones remain free of conflict budget.
    let build_inputs = |row_delete_on_left, checkpoint_delete_on_left, reset_on_left| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [&agreed_before_id, &agreed_between_id, &trailing_delete_id] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        base.checkpoints.insert(first_delete_update_id.clone(), parse_checkpoint_fixture(CHECKPOINT_BASE));
        if checkpoint_delete_on_left {
            right.checkpoints.insert(first_delete_update_id.clone(), updated_fixture.clone());
        } else {
            left.checkpoints.insert(first_delete_update_id.clone(), updated_fixture.clone());
        }

        base.checkpoints.insert(terminal_reset_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        if reset_on_left {
            left.checkpoints.insert(terminal_reset_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(terminal_reset_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(terminal_reset_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(terminal_reset_id.clone(), reset_fixture.clone());
        }
        (base, left, right, source)
    };

    let expected_delete_update = |checkpoint_delete_on_left| BranchMergeConflict::CheckpointConflict {
        id: first_delete_update_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
            left: if checkpoint_delete_on_left { None } else { Some(updated_fixture.clone()) },
            right: if checkpoint_delete_on_left { Some(updated_fixture.clone()) } else { None },
        },
    };
    let expected_reset = |reset_on_left| BranchMergeConflict::CheckpointConflict {
        id: terminal_reset_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_on_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_on_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };

    // Cross row-delete, delete-side, and reset-side orientations. The first
    // delete/update conflict is the zero-budget boundary; reset is terminal
    // after a clean tombstone, with another clean tombstone after exact closure.
    for row_delete_on_left in [true, false] {
        for checkpoint_delete_on_left in [true, false] {
            for reset_on_left in [true, false] {
                let (base, left, right, mut source) = build_inputs(
                    row_delete_on_left,
                    checkpoint_delete_on_left,
                    reset_on_left,
                );
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
                )
                .unwrap_err();
                let BranchMergeError::BudgetExceeded { report } = error else {
                    panic!("zero detail budget stops at the first delete/update conflict")
                };
                assert_eq!(report.conflicts_lower_bound, 1);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 1);
                assert!(report.affected_checkpoints.contains(first_delete_update_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(terminal_reset_id.as_slice()));
                assert_eq!(source.visited.len(), 3);

                let (base, left, right, mut source) = build_inputs(
                    row_delete_on_left,
                    checkpoint_delete_on_left,
                    reset_on_left,
                );
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
                )
                .unwrap_err();
                let BranchMergeError::BudgetExceeded { report } = error else {
                    panic!("the terminal reset conflict crosses the one-detail budget")
                };
                assert_eq!(report.conflicts_lower_bound, 2);
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(report.affected_checkpoints.contains(first_delete_update_id.as_slice()));
                assert!(report.affected_checkpoints.contains(terminal_reset_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                assert_eq!(source.visited.len(), 3);

                let (base, left, right, mut source) = build_inputs(
                    row_delete_on_left,
                    checkpoint_delete_on_left,
                    reset_on_left,
                );
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
                )
                .unwrap_err();
                let BranchMergeError::Conflicts { conflicts, report } = error else {
                    panic!("both conflicts fit and traversal closes over the trailing tombstone")
                };
                assert_eq!(
                    conflicts,
                    vec![
                        expected_delete_update(checkpoint_delete_on_left),
                        expected_reset(reset_on_left),
                    ]
                );
                assert_eq!(report.conflicts_lower_bound, 2);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn delete_update_reset_delete_update_tail_closes_at_exact_conflict_budget() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let agreed_before_id = b"consumer/a-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/b-unchanged".to_vec();
    let first_delete_update_id = b"consumer/m-first-delete-update".to_vec();
    let agreed_before_reset_id = b"consumer/n-agreed-tombstone".to_vec();
    let reset_conflict_id = b"consumer/p-reset-versus-advance".to_vec();
    let agreed_before_terminal_id = b"consumer/q-agreed-tombstone".to_vec();
    let terminal_delete_update_id = b"consumer/z-terminal-delete-update".to_vec();
    let trailing_delete_id = b"consumer/zz-trailing-tombstone".to_vec();
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
    let updated_fixture = parse_checkpoint_fixture(CHECKPOINT_EDITED);

    // The reference defines checkpoint reset preconditions and typed positions,
    // but leaves merge conflict ordering open. Compare full generation/position
    // values, keep map membership as presence, resolve rows first, and then visit
    // stable checkpoint IDs. Clean tombstones do not consume conflict details.
    let build_inputs = |row_delete_on_left: bool, first_delete_on_left: bool, reset_on_left: bool| {
        let terminal_delete_on_left = !first_delete_on_left;
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_on_left { Vec::new() } else { vec![deleted_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_on_left { vec![deleted_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [
            &agreed_before_id,
            &agreed_before_reset_id,
            &agreed_before_terminal_id,
            &trailing_delete_id,
        ] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        for (checkpoint_id, delete_on_left) in [
            (&first_delete_update_id, first_delete_on_left),
            (&terminal_delete_update_id, terminal_delete_on_left),
        ] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_BASE));
            if delete_on_left {
                right.checkpoints.insert(checkpoint_id.to_vec(), updated_fixture.clone());
            } else {
                left.checkpoints.insert(checkpoint_id.to_vec(), updated_fixture.clone());
            }
        }

        base.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        if reset_on_left {
            left.checkpoints.insert(reset_conflict_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(reset_conflict_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(reset_conflict_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(reset_conflict_id.clone(), reset_fixture.clone());
        }
        (base, left, right, source, terminal_delete_on_left)
    };

    let expected_delete_update = |checkpoint_id: &Vec<u8>, delete_on_left: bool| {
        BranchMergeConflict::CheckpointConflict {
            id: checkpoint_id.clone(),
            conflict: orna_evolution_v1::CheckpointMergeConflict {
                base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                left: if delete_on_left { None } else { Some(updated_fixture.clone()) },
                right: if delete_on_left { Some(updated_fixture.clone()) } else { None },
            },
        }
    };
    let expected_reset = |reset_on_left: bool| BranchMergeConflict::CheckpointConflict {
        id: reset_conflict_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_on_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_on_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };

    // Cross row-delete, first delete/update, and reset orientations. The second
    // delete/update uses the opposite deleted side, placing every conflict kind
    // at a distinct stable identity between clean tombstones.
    for row_delete_on_left in [true, false] {
        for first_delete_on_left in [true, false] {
            for reset_on_left in [true, false] {
                for (max_conflicts, expected_ids) in [
                    (0, vec![first_delete_update_id.clone()]),
                    (1, vec![first_delete_update_id.clone(), reset_conflict_id.clone()]),
                    (2, vec![
                        first_delete_update_id.clone(),
                        reset_conflict_id.clone(),
                        terminal_delete_update_id.clone(),
                    ]),
                ] {
                    let (base, left, right, mut source, _) = build_inputs(
                        row_delete_on_left,
                        first_delete_on_left,
                        reset_on_left,
                    );
                    let error = merge_three_way_snapshots(
                        &base,
                        &left,
                        &right,
                        &mut source,
                        BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                    )
                    .unwrap_err();
                    let BranchMergeError::BudgetExceeded { report } = error else {
                        panic!("each conflict budget stops at the next checkpoint conflict")
                    };
                    assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                    assert_eq!(report.affected_checkpoints.len(), expected_ids.len());
                    for checkpoint_id in &expected_ids {
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(agreed_before_reset_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(agreed_before_terminal_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                    assert_eq!(source.visited.len(), 3);
                }

                let (base, left, right, mut source, terminal_delete_on_left) = build_inputs(
                    row_delete_on_left,
                    first_delete_on_left,
                    reset_on_left,
                );
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
                )
                .unwrap_err();
                let BranchMergeError::Conflicts { conflicts, report } = error else {
                    panic!("all three conflicts fit and traversal closes across the trailing tombstone")
                };
                assert_eq!(
                    conflicts,
                    vec![
                        expected_delete_update(&first_delete_update_id, first_delete_on_left),
                        expected_reset(reset_on_left),
                        expected_delete_update(&terminal_delete_update_id, terminal_delete_on_left),
                    ]
                );
                assert_eq!(report.conflicts_lower_bound, 3);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 3);
                assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_before_reset_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_before_terminal_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn row_conflict_precedes_delete_reset_checkpoint_tail_at_shared_budget() {
    let agreed_before_id = b"consumer/a-agreed-tombstone".to_vec();
    let unchanged_id = b"consumer/b-unchanged".to_vec();
    let first_delete_update_id = b"consumer/m-first-delete-update".to_vec();
    let agreed_before_reset_id = b"consumer/n-agreed-tombstone".to_vec();
    let reset_conflict_id = b"consumer/p-reset-versus-advance".to_vec();
    let agreed_before_terminal_id = b"consumer/q-agreed-tombstone".to_vec();
    let terminal_delete_update_id = b"consumer/z-terminal-delete-update".to_vec();
    let trailing_delete_id = b"consumer/zz-trailing-tombstone".to_vec();
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
    let updated_fixture = parse_checkpoint_fixture(CHECKPOINT_EDITED);

    // ORNA-MERGE bounds conflict details but leaves phase ordering open. The
    // storage policy settles row conflicts first, then visits stable checkpoint
    // IDs. The checkpoint reference defines reset preconditions, not merge
    // behavior; compare full values and keep position: None present.
    let build_inputs = |first_delete_on_left: bool, reset_on_left: bool| {
        let terminal_delete_on_left = !first_delete_on_left;
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
        source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
        source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [
            &agreed_before_id,
            &agreed_before_reset_id,
            &agreed_before_terminal_id,
            &trailing_delete_id,
        ] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }
        base.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(unchanged_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        }

        for (checkpoint_id, delete_on_left) in [
            (&first_delete_update_id, first_delete_on_left),
            (&terminal_delete_update_id, terminal_delete_on_left),
        ] {
            base.checkpoints.insert(checkpoint_id.to_vec(), parse_checkpoint_fixture(CHECKPOINT_BASE));
            if delete_on_left {
                right.checkpoints.insert(checkpoint_id.to_vec(), updated_fixture.clone());
            } else {
                left.checkpoints.insert(checkpoint_id.to_vec(), updated_fixture.clone());
            }
        }

        base.checkpoints.insert(reset_conflict_id.clone(), parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS));
        if reset_on_left {
            left.checkpoints.insert(reset_conflict_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(reset_conflict_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(reset_conflict_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(reset_conflict_id.clone(), reset_fixture.clone());
        }
        (base, left, right, source, terminal_delete_on_left)
    };

    let expected_delete_update = |checkpoint_id: &Vec<u8>, delete_on_left: bool| {
        BranchMergeConflict::CheckpointConflict {
            id: checkpoint_id.clone(),
            conflict: orna_evolution_v1::CheckpointMergeConflict {
                base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
                left: if delete_on_left { None } else { Some(updated_fixture.clone()) },
                right: if delete_on_left { Some(updated_fixture.clone()) } else { None },
            },
        }
    };
    let expected_reset = |reset_on_left: bool| BranchMergeConflict::CheckpointConflict {
        id: reset_conflict_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_on_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_on_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };

    // Cross delete/update and reset-side orientations. With budgets zero through
    // three, the row conflict and then each checkpoint identity form the exact
    // ordered boundary; exact capacity returns the complete typed conflict list.
    for first_delete_on_left in [true, false] {
        for reset_on_left in [true, false] {
            for max_conflicts in 0..=3 {
                let (base, left, right, mut source, _) =
                    build_inputs(first_delete_on_left, reset_on_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                )
                .unwrap_err();
                let BranchMergeError::BudgetExceeded { report } = error else {
                    panic!("the next row or checkpoint conflict crosses the shared budget")
                };
                assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                let expected_checkpoints = match max_conflicts {
                    0 => Vec::new(),
                    1 => vec![first_delete_update_id.clone()],
                    2 => vec![first_delete_update_id.clone(), reset_conflict_id.clone()],
                    _ => vec![
                        first_delete_update_id.clone(),
                        reset_conflict_id.clone(),
                        terminal_delete_update_id.clone(),
                    ],
                };
                assert_eq!(report.affected_checkpoints.len(), expected_checkpoints.len());
                for checkpoint_id in &expected_checkpoints {
                    assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                }
                assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_before_reset_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_before_terminal_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }

            let (base, left, right, mut source, terminal_delete_on_left) =
                build_inputs(first_delete_on_left, reset_on_left);
            let error = merge_three_way_snapshots(
                &base,
                &left,
                &right,
                &mut source,
                BranchMergeBudget { max_rows_examined: 100, max_conflicts: 4 },
            )
            .unwrap_err();
            let BranchMergeError::Conflicts { conflicts, report } = error else {
                panic!("all row and checkpoint conflicts fit at the exact budget")
            };
            assert_eq!(conflicts.len(), 4);
            assert!(matches!(conflicts[0], BranchMergeConflict::Row { .. }));
            assert_eq!(
                &conflicts[1..],
                &[
                    expected_delete_update(&first_delete_update_id, first_delete_on_left),
                    expected_reset(reset_on_left),
                    expected_delete_update(&terminal_delete_update_id, terminal_delete_on_left),
                ]
            );
            assert_eq!(report.conflicts_lower_bound, 4);
            assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
            assert_eq!(report.affected_checkpoints.len(), 3);
            assert!(!report.affected_checkpoints.contains(agreed_before_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_before_reset_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(agreed_before_terminal_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
            assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
            assert_eq!(source.visited.len(), 3);
        }
    }
}

#[test]
fn row_reset_delete_update_order_closes_at_each_shared_conflict_budget() {
    let before_id = b"consumer/a-before-tombstone".to_vec();
    let unchanged_id = b"consumer/b-unchanged".to_vec();
    let reset_id = b"consumer/m-reset-versus-advance".to_vec();
    let between_id = b"consumer/n-between-tombstone".to_vec();
    let delete_update_id = b"consumer/z-delete-update-terminal".to_vec();
    let trailing_id = b"consumer/zz-trailing-tombstone".to_vec();
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
    let updated_fixture = parse_checkpoint_fixture(CHECKPOINT_EDITED);

    // The checkpoint reference specifies reset preconditions and conflict
    // values, but leaves merge traversal open. This storage policy completes
    // row planning first, then visits checkpoint IDs in stable order; clean
    // tombstones do not consume conflict details.
    let build_inputs = |row_left: bool, reset_left: bool, delete_left: bool| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
        source.add(
            MergeSide::Left,
            b"left",
            vec![parse_fixture(if row_left { LEFT } else { CONFLICT }, RowKeyKind::Explicit)],
        );
        source.add(
            MergeSide::Right,
            b"right",
            vec![parse_fixture(if row_left { CONFLICT } else { LEFT }, RowKeyKind::Explicit)],
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [&before_id, &between_id, &trailing_id] {
            base.checkpoints.insert(
                checkpoint_id.to_vec(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }
        base.checkpoints.insert(
            unchanged_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(
                unchanged_id.clone(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }

        base.checkpoints.insert(
            reset_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        if reset_left {
            left.checkpoints.insert(reset_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(reset_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(reset_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(reset_id.clone(), reset_fixture.clone());
        }

        base.checkpoints.insert(
            delete_update_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_BASE),
        );
        if delete_left {
            right.checkpoints.insert(delete_update_id.clone(), updated_fixture.clone());
        } else {
            left.checkpoints.insert(delete_update_id.clone(), updated_fixture.clone());
        }
        (base, left, right, source)
    };
    let expected_reset = |reset_left| BranchMergeConflict::CheckpointConflict {
        id: reset_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };
    let expected_delete_update = |delete_left| BranchMergeConflict::CheckpointConflict {
        id: delete_update_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
            left: if delete_left { None } else { Some(updated_fixture.clone()) },
            right: if delete_left { Some(updated_fixture.clone()) } else { None },
        },
    };

    for row_left in [true, false] {
        for reset_left in [true, false] {
            for delete_left in [true, false] {
                for max_conflicts in 0..=2 {
                    let (base, left, right, mut source) =
                        build_inputs(row_left, reset_left, delete_left);
                    let error = merge_three_way_snapshots(
                        &base,
                        &left,
                        &right,
                        &mut source,
                        BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                    )
                    .unwrap_err();
                    let BranchMergeError::BudgetExceeded { report } = error else {
                        panic!("the next row or checkpoint conflict crosses this detail budget")
                    };
                    assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                    let expected_ids = match max_conflicts {
                        0 => Vec::new(),
                        1 => vec![reset_id.clone()],
                        _ => vec![reset_id.clone(), delete_update_id.clone()],
                    };
                    assert_eq!(report.affected_checkpoints.len(), expected_ids.len());
                    for checkpoint_id in expected_ids {
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    assert!(!report.affected_checkpoints.contains(before_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(between_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(trailing_id.as_slice()));
                    assert_eq!(source.visited.len(), 3);
                }

                let (base, left, right, mut source) =
                    build_inputs(row_left, reset_left, delete_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
                )
                .unwrap_err();
                let BranchMergeError::Conflicts { conflicts, report } = error else {
                    panic!("the exact budget returns the complete ordered conflict list")
                };
                assert!(matches!(
                    &conflicts[0],
                    BranchMergeConflict::Row {
                        conflict: orna_evolution_v1::RowMergeConflict::Fields { key, .. },
                        ..
                    } if key == &integer(1)
                ));
                assert_eq!(
                    &conflicts[1..],
                    &[expected_reset(reset_left), expected_delete_update(delete_left)]
                );
                assert_eq!(report.conflicts_lower_bound, 3);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(report.affected_checkpoints.contains(reset_id.as_slice()));
                assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn row_delete_edit_reset_delete_update_tail_closes_at_shared_budgets() {
    let before_id = b"consumer/a-before-tombstone".to_vec();
    let unchanged_id = b"consumer/b-unchanged".to_vec();
    let reset_id = b"consumer/m-reset-versus-advance".to_vec();
    let between_id = b"consumer/n-between-tombstone".to_vec();
    let delete_update_id = b"consumer/z-delete-update-terminal".to_vec();
    let trailing_id = b"consumer/zz-trailing-tombstone".to_vec();
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let edited_row = parse_fixture(LEFT, RowKeyKind::Explicit);
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);
    let updated_fixture = parse_checkpoint_fixture(CHECKPOINT_EDITED);

    // ORNA-MERGE requires row and opaque-checkpoint conflicts, but leaves their
    // shared traversal order open. This storage policy finishes row planning
    // first, then visits stable checkpoint IDs; clean checkpoint tombstones are
    // closure cases and do not consume conflict detail budget.
    let build_inputs = |row_delete_left: bool, reset_left: bool, checkpoint_delete_left: bool| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_left { Vec::new() } else { vec![edited_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_left { vec![edited_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [&before_id, &between_id, &trailing_id] {
            base.checkpoints.insert(
                checkpoint_id.to_vec(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }
        base.checkpoints.insert(
            unchanged_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(
                unchanged_id.clone(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }

        base.checkpoints.insert(
            reset_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        if reset_left {
            left.checkpoints.insert(reset_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(reset_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(reset_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(reset_id.clone(), reset_fixture.clone());
        }

        base.checkpoints.insert(
            delete_update_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_BASE),
        );
        if checkpoint_delete_left {
            right.checkpoints.insert(delete_update_id.clone(), updated_fixture.clone());
        } else {
            left.checkpoints.insert(delete_update_id.clone(), updated_fixture.clone());
        }
        (base, left, right, source)
    };
    let expected_row = || BranchMergeConflict::Row {
        range: KeyRange::all(),
        conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit {
            table: id(1),
            key: integer(1),
        },
    };
    let expected_reset = |reset_left| BranchMergeConflict::CheckpointConflict {
        id: reset_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };
    let expected_delete_update = |checkpoint_delete_left| BranchMergeConflict::CheckpointConflict {
        id: delete_update_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
            left: if checkpoint_delete_left { None } else { Some(updated_fixture.clone()) },
            right: if checkpoint_delete_left { Some(updated_fixture.clone()) } else { None },
        },
    };

    for row_delete_left in [true, false] {
        for reset_left in [true, false] {
            for checkpoint_delete_left in [true, false] {
                for max_conflicts in 0..=2 {
                    let (base, left, right, mut source) =
                        build_inputs(row_delete_left, reset_left, checkpoint_delete_left);
                    let error = merge_three_way_snapshots(
                        &base,
                        &left,
                        &right,
                        &mut source,
                        BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                    )
                    .unwrap_err();
                    let BranchMergeError::BudgetExceeded { report } = error else {
                        panic!("the next row or checkpoint conflict crosses this detail budget")
                    };
                    assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                    let expected_ids = match max_conflicts {
                        0 => Vec::new(),
                        1 => vec![reset_id.clone()],
                        _ => vec![reset_id.clone(), delete_update_id.clone()],
                    };
                    assert_eq!(report.affected_checkpoints.len(), expected_ids.len());
                    for checkpoint_id in expected_ids {
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    assert!(!report.affected_checkpoints.contains(before_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(between_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(trailing_id.as_slice()));
                    assert_eq!(source.visited.len(), 3);
                }

                let (base, left, right, mut source) =
                    build_inputs(row_delete_left, reset_left, checkpoint_delete_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
                )
                .unwrap_err();
                let BranchMergeError::Conflicts { conflicts, report } = error else {
                    panic!("the exact budget closes over the ordered row and checkpoint conflicts")
                };
                assert_eq!(
                    conflicts,
                    vec![
                        expected_row(),
                        expected_reset(reset_left),
                        expected_delete_update(checkpoint_delete_left),
                    ]
                );
                assert_eq!(report.conflicts_lower_bound, 3);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(report.affected_checkpoints.contains(reset_id.as_slice()));
                assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn row_delete_edit_checkpoint_delete_reset_tail_closes_at_shared_budgets() {
    let before_id = b"consumer/a-before-tombstone".to_vec();
    let left_delete_before_id = b"consumer/b-left-delete-closure".to_vec();
    let unchanged_id = b"consumer/c-unchanged".to_vec();
    let delete_update_id = b"consumer/m-delete-update".to_vec();
    let right_delete_between_id = b"consumer/n-right-delete-closure".to_vec();
    let between_id = b"consumer/p-between-tombstone".to_vec();
    let reset_id = b"consumer/z-reset-versus-advance".to_vec();
    let right_delete_after_id = b"consumer/zy-right-delete-closure".to_vec();
    let trailing_id = b"consumer/zz-trailing-tombstone".to_vec();
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let edited_row = parse_fixture(LEFT, RowKeyKind::Explicit);
    let updated_fixture = parse_checkpoint_fixture(CHECKPOINT_EDITED);
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);

    // ORNA-MERGE requires typed row and checkpoint conflicts but leaves their
    // shared traversal order open. This storage policy finishes row planning
    // first, then visits stable checkpoint IDs; clean tombstones are closure
    // cases and do not consume conflict detail budget.
    let build_inputs = |row_delete_left: bool, checkpoint_delete_left: bool, reset_left: bool| {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_left { Vec::new() } else { vec![edited_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_left { vec![edited_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [&before_id, &between_id, &trailing_id] {
            base.checkpoints.insert(
                checkpoint_id.to_vec(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }
        for checkpoint_id in [
            &left_delete_before_id,
            &right_delete_between_id,
            &right_delete_after_id,
        ] {
            base.checkpoints.insert(
                checkpoint_id.to_vec(),
                parse_checkpoint_fixture(CHECKPOINT_BASE),
            );
        }
        // One-sided deletes of a full generation/position resolve cleanly on
        // both sides of the conflict tail: base state remains on Right before
        // the tail and on Left between and after its conflicts.
        right.checkpoints.insert(
            left_delete_before_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_BASE),
        );
        left.checkpoints.insert(
            right_delete_between_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_BASE),
        );
        left.checkpoints.insert(
            right_delete_after_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_BASE),
        );
        base.checkpoints.insert(
            unchanged_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(
                unchanged_id.clone(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }

        base.checkpoints.insert(
            delete_update_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_BASE),
        );
        if checkpoint_delete_left {
            right.checkpoints.insert(delete_update_id.clone(), updated_fixture.clone());
        } else {
            left.checkpoints.insert(delete_update_id.clone(), updated_fixture.clone());
        }

        base.checkpoints.insert(
            reset_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        if reset_left {
            left.checkpoints.insert(reset_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(reset_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(reset_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(reset_id.clone(), reset_fixture.clone());
        }
        (base, left, right, source)
    };
    let expected_row = || BranchMergeConflict::Row {
        range: KeyRange::all(),
        conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit {
            table: id(1),
            key: integer(1),
        },
    };
    let expected_delete_update = |checkpoint_delete_left| BranchMergeConflict::CheckpointConflict {
        id: delete_update_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_BASE)),
            left: if checkpoint_delete_left { None } else { Some(updated_fixture.clone()) },
            right: if checkpoint_delete_left { Some(updated_fixture.clone()) } else { None },
        },
    };
    let expected_reset = |reset_left| BranchMergeConflict::CheckpointConflict {
        id: reset_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };

    for row_delete_left in [true, false] {
        for checkpoint_delete_left in [true, false] {
            for reset_left in [true, false] {
                for max_conflicts in 0..=2 {
                    let (base, left, right, mut source) =
                        build_inputs(row_delete_left, checkpoint_delete_left, reset_left);
                    let error = merge_three_way_snapshots(
                        &base,
                        &left,
                        &right,
                        &mut source,
                        BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                    )
                    .unwrap_err();
                    let BranchMergeError::BudgetExceeded { report } = error else {
                        panic!("the next row or checkpoint conflict crosses this detail budget")
                    };
                    assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                    let expected_ids = match max_conflicts {
                        0 => Vec::new(),
                        1 => vec![delete_update_id.clone()],
                        _ => vec![delete_update_id.clone(), reset_id.clone()],
                    };
                    assert_eq!(report.affected_checkpoints.len(), expected_ids.len());
                    for checkpoint_id in expected_ids {
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    assert!(!report.affected_checkpoints.contains(before_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(left_delete_before_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(right_delete_between_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(between_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(right_delete_after_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(trailing_id.as_slice()));
                    assert_eq!(source.visited.len(), 3);
                }

                let (base, left, right, mut source) =
                    build_inputs(row_delete_left, checkpoint_delete_left, reset_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
                )
                .unwrap_err();
                let BranchMergeError::Conflicts { conflicts, report } = error else {
                    panic!("the exact budget closes over the ordered row and checkpoint conflicts")
                };
                assert_eq!(
                    conflicts,
                    vec![
                        expected_row(),
                        expected_delete_update(checkpoint_delete_left),
                        expected_reset(reset_left),
                    ]
                );
                assert_eq!(report.conflicts_lower_bound, 3);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(report.affected_checkpoints.contains(delete_update_id.as_slice()));
                assert!(report.affected_checkpoints.contains(reset_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(left_delete_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(right_delete_between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(right_delete_after_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn row_delete_edit_delete_reset_then_reset_tail_closes_at_shared_budgets() {
    let before_delete_id = b"consumer/a-agreed-delete".to_vec();
    let unchanged_id = b"consumer/b-unchanged".to_vec();
    let left_delete_id = b"consumer/c-left-delete-closure".to_vec();
    let agreed_reset_before_id = b"consumer/d-agreed-reset-before".to_vec();
    let delete_reset_id = b"consumer/m-delete-versus-reset".to_vec();
    let right_delete_id = b"consumer/n-right-delete-closure".to_vec();
    let between_delete_id = b"consumer/o-agreed-delete".to_vec();
    let agreed_reset_between_id = b"consumer/p-agreed-reset-between".to_vec();
    let agreed_reset_full_base_between_id =
        b"consumer/q-agreed-reset-full-base-between".to_vec();
    let same_side_full_base_delete_before_first_checkpoint_conflict_id =
        b"consumer/l-same-side-delete-before-checkpoint-conflict".to_vec();
    let terminal_reset_id = b"consumer/z-reset-versus-advance".to_vec();
    let agreed_reset_after_id = b"consumer/zy-agreed-reset-after".to_vec();
    let agreed_reset_full_base_after_id =
        b"consumer/zy-agreed-reset-full-base-after".to_vec();
    let trailing_delete_id = b"consumer/zz-agreed-delete".to_vec();
    let full_base_reset_tail_id = b"consumer/zzz-full-base-reset-tail".to_vec();
    let one_sided_full_base_reset_tail_id =
        b"consumer/zzzz-one-sided-full-base-reset-tail".to_vec();
    let same_side_full_base_delete_reset_id_prefix =
        b"consumer/zzzz-one-sided-full-base-reset".to_vec();
    let same_side_full_base_delete_reset_id_extension =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone".to_vec();
    let same_side_full_base_delete_reset_id_extension_tail =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child".to_vec();
    let same_side_full_base_delete_reset_id_extension_leaf =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail".to_vec();
    let same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child =
        b"consumer/zzzz-one-sided-full-base-reset-tail/tombstone/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child/leaf/tail/child".to_vec();
    let opposite_full_base_delete_before_reset_id =
        b"consumer/zzzy-opposite-full-base-delete-before-reset".to_vec();
    let same_side_full_base_delete_before_reset_id =
        b"consumer/zzzy-same-side-full-base-delete-before-reset".to_vec();
    let opposite_full_base_delete_tail_id =
        b"consumer/zzzzz-opposite-full-base-delete-tail".to_vec();
    let same_side_full_base_delete_tail_id =
        b"consumer/zzzzzz-same-side-full-base-delete-tail".to_vec();
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let edited_row = parse_fixture(LEFT, RowKeyKind::Explicit);
    let full_checkpoint = parse_checkpoint_fixture(CHECKPOINT_BASE);
    let reset_fixture = parse_checkpoint_fixture(CHECKPOINT_RESET);
    let advance_fixture = parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS_EDITED);

    // The reference defines typed row/checkpoint conflicts but leaves their
    // shared traversal order open. This storage policy completes rows first,
    // then visits stable checkpoint IDs. Agreed resets and one-sided deletes
    // resolve cleanly before, between, and after the delete/reset conflicts,
    // including convergent resets from full generation/position bases in the
    // middle and after the terminal conflict. The final full-base reset also
    // follows a clean tombstone, proving tail closure keeps it out of impacts.
    // A final one-sided full-base reset likewise resolves without adding an
    // impact or consuming conflict detail. Full-base tombstones before and
    // after the reset on both its own and opposite branches prove the stable-ID
    // tail closure orderings. A same-side tombstone before the first checkpoint
    // conflict also exercises the traversal prefix. Same-side tombstone IDs
    // that are strict prefixes of and extensions to the reset ID cover both
    // adjacent stable-order boundaries, including a long nested extension
    // chain alternating child, leaf, and tail suffixes.
    let build_inputs = |
        row_delete_left: bool,
        checkpoint_delete_left: bool,
        reset_left: bool,
        tail_reset_left: bool,
    | {
        let mut source = FixtureRows::default();
        source.add(MergeSide::Base, b"base", vec![deleted_row.clone()]);
        source.add(
            MergeSide::Left,
            b"left",
            if row_delete_left { Vec::new() } else { vec![edited_row.clone()] },
        );
        source.add(
            MergeSide::Right,
            b"right",
            if row_delete_left { vec![edited_row.clone()] } else { Vec::new() },
        );

        let mut base = snapshot(schema(true, FieldType::Str), manifest(1, 10, b"base"), None);
        let mut left = snapshot(schema(true, FieldType::Str), manifest(2, 11, b"left"), None);
        let mut right = snapshot(schema(true, FieldType::Str), manifest(3, 12, b"right"), None);
        for checkpoint_id in [
            &before_delete_id,
            &between_delete_id,
            &trailing_delete_id,
            &full_base_reset_tail_id,
        ] {
            base.checkpoints.insert(
                checkpoint_id.to_vec(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }
        base.checkpoints.insert(left_delete_id.clone(), full_checkpoint.clone());
        right.checkpoints.insert(left_delete_id.clone(), full_checkpoint.clone());
        base.checkpoints.insert(right_delete_id.clone(), full_checkpoint.clone());
        left.checkpoints.insert(right_delete_id.clone(), full_checkpoint.clone());

        base.checkpoints.insert(
            unchanged_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        for side in [&mut left, &mut right] {
            side.checkpoints.insert(
                unchanged_id.clone(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
            for checkpoint_id in [
                &agreed_reset_before_id,
                &agreed_reset_between_id,
                &agreed_reset_full_base_between_id,
                &agreed_reset_after_id,
                &agreed_reset_full_base_after_id,
                &full_base_reset_tail_id,
            ] {
                side.checkpoints.insert(checkpoint_id.to_vec(), reset_fixture.clone());
            }
        }
        for checkpoint_id in [
            &agreed_reset_before_id,
            &agreed_reset_between_id,
            &agreed_reset_after_id,
            &agreed_reset_full_base_after_id,
            &full_base_reset_tail_id,
        ] {
            base.checkpoints.insert(
                checkpoint_id.to_vec(),
                parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
            );
        }
        base.checkpoints.insert(
            agreed_reset_full_base_between_id.clone(),
            full_checkpoint.clone(),
        );
        base.checkpoints.insert(
            agreed_reset_full_base_after_id.clone(),
            full_checkpoint.clone(),
        );
        base.checkpoints.insert(full_base_reset_tail_id.clone(), full_checkpoint.clone());
        base.checkpoints.insert(
            one_sided_full_base_reset_tail_id.clone(),
            full_checkpoint.clone(),
        );
        // The reference leaves shared checkpoint traversal open. Exercise the
        // tail reset on either branch independently of the terminal conflict.
        if tail_reset_left {
            left.checkpoints.insert(
                one_sided_full_base_reset_tail_id.clone(),
                reset_fixture.clone(),
            );
            right.checkpoints.insert(
                one_sided_full_base_reset_tail_id.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                one_sided_full_base_reset_tail_id.clone(),
                full_checkpoint.clone(),
            );
            right.checkpoints.insert(
                one_sided_full_base_reset_tail_id.clone(),
                reset_fixture.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_prefix.clone(),
            full_checkpoint.clone(),
        );
        // This tombstone is on the reset branch and its ID is a strict byte
        // prefix of the reset ID; the opposite branch retains the base value.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_prefix.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_prefix.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension.clone(),
            full_checkpoint.clone(),
        );
        // This companion ID extends the reset ID; the reset branch tombstones
        // it and the opposite branch retains the full-position base.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_tail.clone(),
            full_checkpoint.clone(),
        );
        // This nested extension follows the tombstone ID above; keep the same
        // deletion orientation and retain the base checkpoint on the other side.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_tail.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_tail.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_leaf.clone(),
            full_checkpoint.clone(),
        );
        // A second nested extension confirms the prefix chain remains clean at
        // the deeper tail on the same reset side.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_leaf.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_leaf.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail.clone(),
            full_checkpoint.clone(),
        );
        // Extend one more level past the previous leaf while preserving the
        // same reset-side tombstone orientation.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child.clone(),
            full_checkpoint.clone(),
        );
        // Continue past the existing deepest extension on the same reset-side
        // tombstone chain; the other branch retains the full-position base.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf.clone(),
            full_checkpoint.clone(),
        );
        // Add a leaf past the prior extension edge while retaining the same
        // tombstone side and full-position base on the opposite branch.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail.clone(),
            full_checkpoint.clone(),
        );
        // Continue beyond the leaf so byte-ordered tail closure stays stable
        // when one reset side tombstones the deeper ID chain.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child.clone(),
            full_checkpoint.clone(),
        );
        // Extend the strict-ID chain one more segment while preserving the
        // same tombstone orientation against the full-position base value.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf.clone(),
            full_checkpoint.clone(),
        );
        // Keep the next nested leaf on the tombstone side against the
        // full-position checkpoint retained on the opposite side.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail.clone(),
            full_checkpoint.clone(),
        );
        // Exercise the leaf's tail as another same-side tombstone extension
        // against the opposite branch's retained full-position base.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child.clone(),
            full_checkpoint.clone(),
        );
        // Continue the same reset-side tombstone chain below the latest tail.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
            full_checkpoint.clone(),
        );
        // Add a leaf under the newly extended tombstone ID for the same-side
        // versus retained full-position checkpoint case.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
            full_checkpoint.clone(),
        );
        // Continue the same-side extension one tail beyond the new leaf.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
            full_checkpoint.clone(),
        );
        // Add the next same-side tombstone child while preserving its
        // opposite full-position base checkpoint.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
            full_checkpoint.clone(),
        );
        // Continue the tombstone extension with a leaf beneath the latest
        // child while retaining the opposite full-position base value.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
            full_checkpoint.clone(),
        );
        // Continue the nested same-side extension with a tail below the leaf.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
            full_checkpoint.clone(),
        );
        // Continue the nested same-side tombstone chain below its latest tail.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
            full_checkpoint.clone(),
        );
        // Add a leaf under the latest tombstone child while the opposite side
        // retains the original full-position checkpoint.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
            full_checkpoint.clone(),
        );
        // Continue the nested tombstone chain with a tail below this leaf.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
            full_checkpoint.clone(),
        );
        // Add a child below the deepest tail without changing the reset-side
        // tombstone orientation.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            opposite_full_base_delete_before_reset_id.clone(),
            full_checkpoint.clone(),
        );
        if tail_reset_left {
            left.checkpoints.insert(
                opposite_full_base_delete_before_reset_id.clone(),
                full_checkpoint.clone(),
            );
        } else {
            right.checkpoints.insert(
                opposite_full_base_delete_before_reset_id.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_before_reset_id.clone(),
            full_checkpoint.clone(),
        );
        // The reset branch also tombstones this earlier stable ID; the other
        // branch retains its full-position base checkpoint.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_before_reset_id.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_before_reset_id.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_before_first_checkpoint_conflict_id.clone(),
            full_checkpoint.clone(),
        );
        // This clean same-side tombstone sorts before the first checkpoint
        // conflict; the opposite branch retains the full-position base.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_before_first_checkpoint_conflict_id.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_before_first_checkpoint_conflict_id.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            opposite_full_base_delete_tail_id.clone(),
            full_checkpoint.clone(),
        );
        // The branch opposite the one-sided reset deletes this final ID while
        // the reset branch retains the base checkpoint, so the tombstone is a
        // clean one-sided change in either tail-reset orientation.
        if tail_reset_left {
            left.checkpoints.insert(
                opposite_full_base_delete_tail_id.clone(),
                full_checkpoint.clone(),
            );
        } else {
            right.checkpoints.insert(
                opposite_full_base_delete_tail_id.clone(),
                full_checkpoint.clone(),
            );
        }
        base.checkpoints.insert(
            same_side_full_base_delete_tail_id.clone(),
            full_checkpoint.clone(),
        );
        // This companion tombstone deletes on the same branch as the reset;
        // the other side retains the full-position base checkpoint.
        if tail_reset_left {
            right.checkpoints.insert(
                same_side_full_base_delete_tail_id.clone(),
                full_checkpoint.clone(),
            );
        } else {
            left.checkpoints.insert(
                same_side_full_base_delete_tail_id.clone(),
                full_checkpoint.clone(),
            );
        }

        base.checkpoints.insert(delete_reset_id.clone(), full_checkpoint.clone());
        if checkpoint_delete_left {
            right.checkpoints.insert(delete_reset_id.clone(), reset_fixture.clone());
        } else {
            left.checkpoints.insert(delete_reset_id.clone(), reset_fixture.clone());
        }

        base.checkpoints.insert(
            terminal_reset_id.clone(),
            parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS),
        );
        if reset_left {
            left.checkpoints.insert(terminal_reset_id.clone(), reset_fixture.clone());
            right.checkpoints.insert(terminal_reset_id.clone(), advance_fixture.clone());
        } else {
            left.checkpoints.insert(terminal_reset_id.clone(), advance_fixture.clone());
            right.checkpoints.insert(terminal_reset_id.clone(), reset_fixture.clone());
        }
        (base, left, right, source)
    };
    let expected_row = || BranchMergeConflict::Row {
        range: KeyRange::all(),
        conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit {
            table: id(1),
            key: integer(1),
        },
    };
    let expected_delete_reset = |checkpoint_delete_left| BranchMergeConflict::CheckpointConflict {
        id: delete_reset_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(full_checkpoint.clone()),
            left: if checkpoint_delete_left { None } else { Some(reset_fixture.clone()) },
            right: if checkpoint_delete_left { Some(reset_fixture.clone()) } else { None },
        },
    };
    let expected_reset = |reset_left| BranchMergeConflict::CheckpointConflict {
        id: terminal_reset_id.clone(),
        conflict: orna_evolution_v1::CheckpointMergeConflict {
            base: Some(parse_checkpoint_fixture(CHECKPOINT_POSITIONLESS)),
            left: Some(if reset_left { reset_fixture.clone() } else { advance_fixture.clone() }),
            right: Some(if reset_left { advance_fixture.clone() } else { reset_fixture.clone() }),
        },
    };

    for row_delete_left in [true, false] {
        for checkpoint_delete_left in [true, false] {
            for (reset_left, tail_reset_left) in [
                (true, true),
                (true, false),
                (false, true),
                (false, false),
            ] {
                for max_conflicts in 0..=2 {
                    let (base, left, right, mut source) =
                        build_inputs(row_delete_left, checkpoint_delete_left, reset_left, tail_reset_left);
                    let error = merge_three_way_snapshots(
                        &base,
                        &left,
                        &right,
                        &mut source,
                        BranchMergeBudget { max_rows_examined: 100, max_conflicts },
                    )
                    .unwrap_err();
                    let BranchMergeError::BudgetExceeded { report } = error else {
                        panic!("the next row or checkpoint conflict crosses this detail budget")
                    };
                    assert_eq!(report.conflicts_lower_bound, max_conflicts + 1);
                    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                    let expected_ids = match max_conflicts {
                        0 => Vec::new(),
                        1 => vec![delete_reset_id.clone()],
                        _ => vec![delete_reset_id.clone(), terminal_reset_id.clone()],
                    };
                    assert_eq!(report.affected_checkpoints.len(), expected_ids.len());
                    for checkpoint_id in expected_ids {
                        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
                    }
                    assert!(!report.affected_checkpoints.contains(before_delete_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(left_delete_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(right_delete_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(between_delete_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(agreed_reset_between_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(agreed_reset_full_base_between_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(agreed_reset_full_base_after_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(agreed_reset_before_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(full_base_reset_tail_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(one_sided_full_base_reset_tail_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_prefix.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_tail.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_leaf.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                    assert!(!report.affected_checkpoints.contains(opposite_full_base_delete_before_reset_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_before_reset_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_before_first_checkpoint_conflict_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(opposite_full_base_delete_tail_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_tail_id.as_slice()));
                    assert!(!report.affected_checkpoints.contains(agreed_reset_after_id.as_slice()));
                    assert_eq!(source.visited.len(), 3);
                }

                let (base, left, right, mut source) =
                    build_inputs(row_delete_left, checkpoint_delete_left, reset_left, tail_reset_left);
                let error = merge_three_way_snapshots(
                    &base,
                    &left,
                    &right,
                    &mut source,
                    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 3 },
                )
                .unwrap_err();
                let BranchMergeError::Conflicts { conflicts, report } = error else {
                    panic!("the exact budget returns the complete ordered conflict list")
                };
                assert_eq!(
                    conflicts,
                    vec![
                        expected_row(),
                        expected_delete_reset(checkpoint_delete_left),
                        expected_reset(reset_left),
                    ]
                );
                assert_eq!(report.conflicts_lower_bound, 3);
                assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
                assert_eq!(report.affected_checkpoints.len(), 2);
                assert!(report.affected_checkpoints.contains(delete_reset_id.as_slice()));
                assert!(report.affected_checkpoints.contains(terminal_reset_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(before_delete_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(unchanged_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(left_delete_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(right_delete_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(between_delete_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_reset_between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_reset_full_base_between_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_reset_full_base_after_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_reset_before_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(trailing_delete_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(full_base_reset_tail_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(one_sided_full_base_reset_tail_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_prefix.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_tail.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_leaf.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_reset_id_extension_deep_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child_leaf_tail_child.as_slice()));
                assert!(!report.affected_checkpoints.contains(opposite_full_base_delete_before_reset_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_before_reset_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_before_first_checkpoint_conflict_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(opposite_full_base_delete_tail_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(same_side_full_base_delete_tail_id.as_slice()));
                assert!(!report.affected_checkpoints.contains(agreed_reset_after_id.as_slice()));
                assert_eq!(source.visited.len(), 3);
            }
        }
    }
}

#[test]
fn schema_conflict_is_a_boundary_before_row_reads_and_checkpoint_resolution() {
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut left_schema = schema(true, FieldType::Str);
    left_schema.tables[0].fields[1].ty = FieldType::Int;
    let mut right_schema = schema(true, FieldType::Str);
    right_schema.tables[0].fields[1].ty = FieldType::Bool;
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(
        schema(true, FieldType::Str),
        manifest(1, 1, b"base"),
        Some(base_checkpoint),
    );
    let left = snapshot(left_schema, manifest(2, 2, b"left"), Some(left_checkpoint));
    let right = snapshot(right_schema, manifest(3, 3, b"right"), Some(right_checkpoint));

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget())
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("schema conflict must reject before producing a plan")
    };
    assert!(matches!(conflicts.as_slice(), [BranchMergeConflict::Schema(_)]));
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_checkpoints.is_empty());
    assert!(source.visited.is_empty(), "unresolved schema prevents row decoding");
}

#[test]
fn divergent_checkpoint_generations_preserve_conflict_positions() {
    let base_position = CheckpointGeneration { generation: 4, position: Some(b"base-token".to_vec()) };
    let left_position = CheckpointGeneration { generation: 5, position: Some(b"left-token".to_vec()) };
    let right_position = CheckpointGeneration { generation: 6, position: Some(b"right-token".to_vec()) };
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(base_position.clone()));
    let left = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(left_position.clone()));
    let right = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(right_position.clone()));
    let mut source = FixtureRows::default();

    let retained_base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(base_position.clone()));
    let adopted = merge_three_way_snapshots(&base, &left, &retained_base, &mut source, budget()).unwrap();
    assert_eq!(adopted.checkpoints[b"consumer/source".as_slice()], left_position);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(error, BranchMergeError::Conflicts { conflicts, .. } if conflicts.iter().any(|item| matches!(item, BranchMergeConflict::CheckpointConflict { id, conflict } if item.diagnostic_code() == Some("sys.CheckpointConflict") && id.as_slice() == b"consumer/source" && conflict.base.as_ref() == Some(&base_position) && conflict.left.as_ref() == Some(&left_position) && conflict.right.as_ref() == Some(&right_position)))));
}

#[test]
fn generated_key_collision_is_a_row_conflict_and_never_renumbered() {
    let mut left_row = parse_fixture(LEFT, RowKeyKind::Automatic);
    let mut right_row = left_row.clone();
    let generated_key = CanonicalValue::new(OvbRaw::Int(92.into())).unwrap();
    left_row.key = generated_key.clone();
    right_row.key = generated_key;
    right_row.fields.insert(id(2), string("different payload"));
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", Vec::new());
    source.add(MergeSide::Left, b"left", vec![left_row]);
    source.add(MergeSide::Right, b"right", vec![right_row]);
    let base = snapshot(schema(false, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(false, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(false, FieldType::Str), manifest(3, 3, b"right"), None);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(error, BranchMergeError::Conflicts { conflicts, .. } if conflicts.iter().any(|item| matches!(item, BranchMergeConflict::Row { conflict: orna_evolution_v1::RowMergeConflict::AutomaticKeyCollision { key, .. }, .. } if key == &CanonicalValue::new(OvbRaw::Int(92.into())).unwrap()))));
}

#[test]
fn versioned_delete_survives_merge_as_a_tombstone_without_pruning_history() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let mut retained_row = deleted_row.clone();
    retained_row.key = integer(2);
    retained_row.fields.insert(id(2), string("old retained row"));
    let mut edited_retained_row = retained_row.clone();
    edited_retained_row.fields.insert(id(3), string("Paris"));

    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![deleted_row.clone(), retained_row.clone()]);
    source.add(MergeSide::Left, b"left", vec![retained_row.clone()]);
    source.add(MergeSide::Right, b"right", vec![deleted_row.clone(), edited_retained_row.clone()]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    let MergedSegment::Rows { rows, tombstones, .. } = &plan.tables[&id(1)].segments[0] else {
        panic!("all three versions changed and require a merged range")
    };
    assert_eq!(rows, &[edited_retained_row]);
    assert_eq!(tombstones, &[deleted_row.key.clone()]);
    assert!(source.rows[&(MergeSide::Base, b"base".to_vec())].contains(&deleted_row));
}

#[test]
fn versioned_delete_conflicts_with_a_concurrent_edit() {
    let base_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let mut edited_row = base_row.clone();
    edited_row.fields.insert(id(2), string("concurrent edit"));
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![base_row]);
    source.add(MergeSide::Left, b"left", Vec::new());
    source.add(MergeSide::Right, b"right", vec![edited_row]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(
        error,
        BranchMergeError::Conflicts { conflicts, .. }
            if conflicts.iter().any(|conflict| matches!(
                conflict,
                BranchMergeConflict::Row {
                    conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { .. },
                    ..
                }
            ))
    ));
}

#[test]
fn unavailable_or_pruned_rows_fail_closed_instead_of_becoming_deletes() {
    struct UnavailableRows;
    impl BranchRowSource for UnavailableRows {
        fn visit_rows(
            &mut self,
            _side: MergeSide,
            _table: ObjectId,
            _segment: Option<&RowSegmentManifest>,
            _range: &KeyRange,
            _visitor: &mut dyn FnMut(KeyedRow) -> bool,
        ) -> Result<(), String> {
            Err("segment is pruned or unavailable".into())
        }
    }

    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);
    let error = merge_three_way_snapshots(&base, &left, &right, &mut UnavailableRows, budget())
        .unwrap_err();
    assert_eq!(
        error,
        BranchMergeError::RowRead {
            message: "segment is pruned or unavailable".into(),
        }
    );
}
