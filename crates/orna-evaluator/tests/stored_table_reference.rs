//! Reading through a stored table reference (ORNA-REF-001, ORNA-REF-002):
//! `reference.key` needs no row, any other field resolves the target row
//! through the activation's own snapshot, and a missing or unservable row is
//! never read as an absent field.

use orna_evaluator::{
    EffectHandler, Environment, EvaluationError, Functions, Limits, PureFunction,
    invoke_named_with_effects,
};
use orna_syntax::parse_expression;
use orna_value::{CanonicalValue, Raw, Value};

const DATABASE: [u8; 16] = [7; 16];
const OWNER_TABLE: [u8; 16] = [1; 16];

fn text(value: &str) -> Raw {
    Raw::Text(value.to_owned())
}

fn record(fields: Vec<(&str, Raw)>) -> CanonicalValue {
    let mut fields = fields;
    fields.sort_by(|(left, _), (right, _)| {
        left.len().cmp(&right.len()).then_with(|| left.cmp(right))
    });
    CanonicalValue::new(Raw::Map(
        fields
            .into_iter()
            .map(|(name, value)| (text(name), value))
            .collect(),
    ))
    .expect("test record is canonical")
}

fn reference(key: &str) -> CanonicalValue {
    Value::table_reference(DATABASE, OWNER_TABLE, text(key)).expect("a text key is permitted")
}

/// A row of a table whose `owner` column is a stored reference.
fn vehicle(owner: &str) -> CanonicalValue {
    record(vec![
        ("id", text("v1")),
        ("owner", reference(owner).raw().clone()),
    ])
}

/// Serves the owner rows of one snapshot and counts each resolution it serves.
#[derive(Default)]
struct Owners {
    rows: Vec<(&'static str, CanonicalValue)>,
    resolved: usize,
}

impl EffectHandler for Owners {
    fn handle(
        &mut self,
        _: &orna_syntax_ast::Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn resolve_table_reference(
        &mut self,
        reference: &CanonicalValue,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.resolved += 1;
        let Raw::Tag(60021, payload) = reference.raw() else {
            panic!("the evaluator resolves stored table references only");
        };
        let Raw::Array(parts) = payload.as_ref() else {
            panic!("a stored table reference is an array");
        };
        let Raw::Text(key) = &parts[2] else {
            panic!("the fixture keys owners by text");
        };
        Ok(self
            .rows
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, row)| row.clone()))
    }
}

/// A handler that serves no table state at all.
struct NoTables;

impl EffectHandler for NoTables {
    fn handle(
        &mut self,
        _: &orna_syntax_ast::Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }
}

fn run(
    source: &str,
    row: CanonicalValue,
    effects: &mut dyn EffectHandler,
) -> Result<CanonicalValue, EvaluationError> {
    let parsed = parse_expression(source);
    assert!(parsed.is_ok(), "fixture should parse: {source}");
    let functions = Functions::from([(
        "probe".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment: Environment::from([("vehicle".to_owned(), row)]),
        },
    )]);
    invoke_named_with_effects(
        "probe",
        &functions,
        &Environment::new(),
        Limits::default(),
        effects,
    )
}

fn owners() -> Owners {
    Owners {
        rows: vec![(
            "ada",
            record(vec![("id", text("ada")), ("name", text("Ada"))]),
        )],
        resolved: 0,
    }
}

#[test]
fn the_key_of_a_reference_is_read_without_loading_the_target_row() {
    let mut effects = owners();
    let key = run("vehicle.owner.key", vehicle("ada"), &mut effects).expect("the key reads");
    assert_eq!(key.raw(), &text("ada"));
    assert_eq!(effects.resolved, 0, "no row was loaded for the key");

    // The key is the reference's own, so it reads even when the row is gone.
    let mut effects = Owners::default();
    let key = run("vehicle.owner.key", vehicle("ghost"), &mut effects).expect("the key reads");
    assert_eq!(key.raw(), &text("ghost"));
    assert_eq!(effects.resolved, 0);
}

#[test]
fn a_field_of_the_target_resolves_through_the_snapshot() {
    let mut effects = owners();
    let name = run("vehicle.owner.name", vehicle("ada"), &mut effects).expect("the field reads");
    assert_eq!(name.raw(), &text("Ada"));
    assert_eq!(effects.resolved, 1, "one lookup answered the read");
}

#[test]
fn a_reference_to_a_missing_row_is_dangling_not_an_absent_field() {
    let mut effects = owners();
    let error = run("vehicle.owner.name", vehicle("ghost"), &mut effects)
        .expect_err("a missing target cannot be read through");
    assert_eq!(error.code(), "ORNA-REF-DANGLING");
    assert_eq!(effects.resolved, 1);
}

#[test]
fn an_unknown_field_of_the_target_is_a_field_error() {
    let mut effects = owners();
    let error = run("vehicle.owner.nickname", vehicle("ada"), &mut effects)
        .expect_err("the target row has no such field");
    assert_eq!(error.code(), "ORNA-EVAL-FIELD");
}

#[test]
fn a_handler_without_table_state_never_answers_for_a_reference() {
    let error = run("vehicle.owner.name", vehicle("ada"), &mut NoTables)
        .expect_err("resolution needs the snapshot");
    assert_eq!(error.code(), "ORNA-EVAL-UNSUPPORTED");
}
