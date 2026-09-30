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

        // One retained checkpoint detail fits; the later fixture conflict
        // crosses the shared tail after the clean row tombstone has resolved.
        let (base, left, right, mut source, agreed_delete_id, unchanged_delete_id, checkpoint_id, tail_id) =
            build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
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
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 },
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

        // The same row/checkpoint deletion orientation without the divergent
        // checkpoint proves the changed segment emits the tombstone itself.
        let (base, left, right, mut source, _, _, checkpoint_id, tail_id) = build_inputs(
            row_delete_on_left,
            checkpoint_delete_on_left,
            false,
            regular_checkpoint_fixtures,
        );
        let plan = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 },
        )
        .unwrap();
        assert_eq!(plan.report.conflicts_lower_bound, 0);
        assert_eq!(plan.report.rows_examined, 2);
        assert_eq!(source.visited.len(), 3);
        assert!(!plan.checkpoints.contains_key(checkpoint_id.as_slice()));
        assert!(plan.checkpoints.contains_key(tail_id.as_slice()));
        let segments = &plan.tables[&id(1)].segments;
        assert!(matches!(segments[0], MergedSegment::Reuse { from: MergeSide::Left, .. }));
        let MergedSegment::Rows { rows, tombstones, .. } = &segments[1] else {
            panic!("the upper segment deletion must materialize a tombstone")
        };
        assert!(rows.is_empty());
        assert_eq!(tombstones, &[high_key.clone()]);

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
        // the capped scan still closes the tail and reports its lower bound.
        let (mut base, mut left, mut right, mut source, agreed_delete_id, unchanged_delete_id, checkpoint_id, tail_id) =
            build_inputs(row_delete_on_left, checkpoint_delete_on_left, true, regular_checkpoint_fixtures);
        let final_tail_id = b"consumer/zz-final-tail-conflict".to_vec();
        base.checkpoints.insert(final_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_BASE));
        left.checkpoints.insert(final_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_LEFT));
        right.checkpoints.insert(final_tail_id.clone(), parse_checkpoint_fixture(CHECKPOINT_TAIL_RIGHT));
        let error = merge_three_way_snapshots(
            &base,
            &left,
            &right,
            &mut source,
            BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 },
        )
        .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("the terminal fixture conflict crosses the two-detail budget")
        };
        assert_eq!(report.conflicts_lower_bound, 3);
        assert_eq!(report.rows_examined, 2);
        assert_eq!(report.affected_ranges.len(), 1);
        assert!(report.affected_ranges.contains(&(id(1), high_range.clone())));
        assert_eq!(report.affected_checkpoints.len(), 3);
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert!(report.affected_checkpoints.contains(tail_id.as_slice()));
        assert!(report.affected_checkpoints.contains(final_tail_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(agreed_delete_id.as_slice()));
        assert!(!report.affected_checkpoints.contains(unchanged_delete_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
        assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"upper")));

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
