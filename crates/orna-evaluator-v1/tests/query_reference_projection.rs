use num_bigint::BigInt;
use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits, PureFunction, RelationPage,
    StepBudget, invoke_named_with_effects,
};
use orna_foundation_v1::{CanonicalValue, OvbRaw, RowRef};
use orna_syntax_v1::parse_expression;
use orna_value_v1::{Raw, Snapshot};

fn integer(value: i64) -> Raw {
    Raw::Int(BigInt::from(value))
}

fn text(value: &str) -> Raw {
    Raw::Text(value.to_owned())
}

fn canonical_row(fields: Vec<(&str, Raw)>) -> CanonicalValue {
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
    .expect("test row is canonical")
}

fn run_fixture(
    source: &str,
    environment: Environment,
    effects: &mut dyn EffectHandler,
) -> CanonicalValue {
    let parsed = parse_expression(source);
    assert!(parsed.is_ok(), "fixture should parse: {source}: {:?}", parsed.diagnostics);
    let functions = Functions::from([(
        "query".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment,
        },
    )]);
    invoke_named_with_effects("query", &functions, &Environment::new(), Limits::default(), effects)
        .expect("query fixture should evaluate")
}

#[derive(Default)]
struct ProjectionEffects {
    scans: usize,
    rows: Vec<CanonicalValue>,
}

impl EffectHandler for ProjectionEffects {
    fn handle(
        &mut self,
        _: &orna_syntax_v1::Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        assert_eq!(source, "sys.Storage");
        assert_eq!(limit, 1);
        budget.debit(1)?;
        let index = match after {
            None => 0,
            Some([1]) => 1,
            _ => return Err(EvaluationError::redacted(
                orna_foundation_v1::SafeText::new("ORNA-EVAL-VALUE").unwrap(),
            )),
        };
        self.scans += 1;
        Ok(Some(RelationPage {
            rows: self.rows.get(index).cloned().into_iter().collect(),
            next: (index + 1 < self.rows.len()).then(|| vec![(index + 1) as u8]),
        }))
    }
}

#[test]
fn project_shapes_a_relation_lazily_and_preserves_its_result_order() {
    let row = |pending_rows, pending_bytes| {
        canonical_row(vec![
            ("pending_rows", integer(pending_rows)),
            ("pending_bytes", integer(pending_bytes)),
            ("secret", text("excluded")),
        ])
    };
    let first_projection = canonical_row(vec![("pending_rows", integer(7))]);
    let second_projection = canonical_row(vec![("pending_rows", integer(8))]);
    let expected_window = Raw::Array(vec![
        first_projection.raw().clone(),
        second_projection.raw().clone(),
    ]);
    let expected = CanonicalValue::new(Raw::Tag(
        60013,
        Box::new(Raw::Array(vec![integer(1), expected_window])),
    ))
    .expect("projected result is canonical");
    let mut effects = ProjectionEffects {
        scans: 0,
        rows: vec![row(7, 999), row(8, 1000)],
    };

    let result = run_fixture(
        include_str!("fixtures/query-project.orna"),
        Environment::new(),
        &mut effects,
    );

    assert_eq!(result, expected);
    assert_eq!(effects.scans, 2, "the window stops after its second relation row");
}

fn reference(generation: i64) -> CanonicalValue {
    let database = [0x11; 16];
    let snapshot = Snapshot::cwd(database, [0x22; 16], BigInt::from(generation))
        .expect("CWD snapshot is valid");
    let row_ref = RowRef::new(database, [0x33; 16], OvbRaw::Int(BigInt::from(7)), snapshot)
        .expect("row reference is valid");
    CanonicalValue::decode(&row_ref.encode().expect("row reference encodes"))
        .expect("encoded row reference decodes")
}

struct ReferenceEffects {
    expected: CanonicalValue,
    target: CanonicalValue,
    resolutions: usize,
}

impl EffectHandler for ReferenceEffects {
    fn handle(
        &mut self,
        _: &orna_syntax_v1::Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn resolve_reference(
        &mut self,
        reference: &CanonicalValue,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        assert_eq!(reference, &self.expected, "resolver receives full pinned identity");
        self.resolutions += 1;
        Ok(Some(self.target.clone()))
    }
}

#[test]
fn references_keep_snapshot_identity_expose_keys_and_resolve_fields_through_the_session() {
    let owner = reference(4);
    let mut environment = Environment::from([("owner".into(), owner.clone())]);
    let mut effects = ReferenceEffects {
        expected: owner.clone(),
        target: canonical_row(vec![("name", text("Ada"))]),
        resolutions: 0,
    };

    let key = run_fixture(
        include_str!("fixtures/row-reference-key.orna"),
        environment.clone(),
        &mut effects,
    );
    assert_eq!(key, CanonicalValue::new(integer(7)).unwrap());
    assert_eq!(effects.resolutions, 0, "the key is read from reference identity");

    let name = run_fixture(
        include_str!("fixtures/row-reference-name.orna"),
        environment.clone(),
        &mut effects,
    );
    assert_eq!(name, CanonicalValue::new(text("Ada")).unwrap());

    // Query adapters resolve through the activation overlay at the reference's
    // unchanged pin, so an earlier target update is visible to later reads.
    effects.target = canonical_row(vec![("name", text("Ada Lovelace"))]);
    let updated_name = run_fixture(
        include_str!("fixtures/row-reference-name.orna"),
        environment.clone(),
        &mut effects,
    );
    assert_eq!(updated_name, CanonicalValue::new(text("Ada Lovelace")).unwrap());
    assert_eq!(effects.resolutions, 2);

    environment.insert("same_owner".into(), owner.clone());
    let equal = run_fixture(
        include_str!("fixtures/row-reference-identity.orna"),
        environment.clone(),
        &mut effects,
    );
    assert_eq!(equal, CanonicalValue::new(Raw::Bool(true)).unwrap());

    environment.insert("same_owner".into(), reference(5));
    let different_snapshot = run_fixture(
        include_str!("fixtures/row-reference-identity.orna"),
        environment,
        &mut effects,
    );
    assert_eq!(different_snapshot, CanonicalValue::new(Raw::Bool(false)).unwrap());
}
