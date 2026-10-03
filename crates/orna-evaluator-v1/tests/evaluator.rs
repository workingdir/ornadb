use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_evaluator_v1::{
    AdmittedReplSession, EffectHandler, Environment, EvaluationError, Functions, Limits,
    NominalDefinition, NominalDefinitions, NominalField, NominalVariant, PureFunction,
    RelationPage, StepBudget, evaluate_expression, evaluate_expression_with_functions,
    evaluate_function, evaluate_parsed, evaluate_parsed_with_nominals, evaluate_repl,
    SysHostBindingRegistry, evaluate_with_functions_and_nominals, invoke_named,
    invoke_named_with_effects, invoke_named_with_effects_and_budget, invoke_named_with_nominals,
};
use orna_syntax_v1::{
    lex, AssignmentOperator, AssignmentTarget, Expr, NameSegment, Pattern, RecordField, Statement,
    SyntaxSpan, TokenKind,
};
use orna_value_v1::{Raw, Value, CANONICAL_NAN_BITS};

fn evaluate(source: &str) -> Value {
    evaluate_expression(source, &Environment::new(), Limits::default())
        .unwrap_or_else(|error| panic!("{}: {}", source, error.code()))
}
fn code(result: Result<Value, EvaluationError>) -> String {
    result.unwrap_err().code().to_owned()
}

#[test]
fn std_environment_get_reads_current_and_absent_process_values() {
    let expected = std::env::var("PATH").expect("test process exposes PATH");
    let absent = "ORNA_TEST_MISSING_6TG7L_20261002";
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    let mut bindings = SysHostBindingRegistry::capture_environment([
        String::from("PATH"),
        absent.to_owned(),
    ])
    .expect("explicit process environment allowlist is valid");
    assert_eq!(
        session.submit(include_str!(
            "fixtures/repl-inline-use-std-io-environment-6tg7l.orna"
        )),
        Ok(None)
    );
    let current = session
        .submit_with_sys_host_bindings(
            include_str!("fixtures/repl-inline-std-io-environment-get-path-6tg7l.orna"),
            &mut bindings,
        )
        .unwrap_or_else(|error| panic!("environment get failed: {}", error.code()));
    assert_eq!(
        current,
        Some(Value::option(Some(Value::new(Raw::Text(expected)).unwrap())).unwrap())
    );
    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/repl-inline-std-io-environment-get-missing-6tg7l.orna"),
            &mut bindings,
        ),
        Ok(Some(Value::option(None).unwrap()))
    );
}

#[derive(Clone, Copy)]
enum MinMaxSourceFixtureProfile {
    FloatZero,
    Rows,
    Dates,
    Instants,
    Integers,
}

fn min_max_source_fixtures(profile: MinMaxSourceFixtureProfile, name: &str) -> [&'static str; 8] {
    match (profile, name) {
        (MinMaxSourceFixtureProfile::FloatZero, "min") => [
            include_str!("fixtures/evaluator_source_2979c79553dd622a.orna"),
            include_str!("fixtures/evaluator_source_0840b1f2a059502e.orna"),
            include_str!("fixtures/evaluator_source_44c493ef1a38d2f7.orna"),
            include_str!("fixtures/evaluator_source_85d792007f6150ec.orna"),
            include_str!("fixtures/evaluator_source_5d0352000306b1f0.orna"),
            include_str!("fixtures/evaluator_source_c455f74f1c3bb88a.orna"),
            include_str!("fixtures/evaluator_source_bd677eae14166585.orna"),
            include_str!("fixtures/evaluator_source_57cb351de1d5b752.orna"),
        ],
        (MinMaxSourceFixtureProfile::FloatZero, "max") => [
            include_str!("fixtures/evaluator_source_c770bd73b34202c7.orna"),
            include_str!("fixtures/evaluator_source_70f712e8348dd46a.orna"),
            include_str!("fixtures/evaluator_source_c43d56b30393963e.orna"),
            include_str!("fixtures/evaluator_source_4b371d3700bfe9e6.orna"),
            include_str!("fixtures/evaluator_source_1f41969a9a6fee15.orna"),
            include_str!("fixtures/evaluator_source_a861cefbd0543c98.orna"),
            include_str!("fixtures/evaluator_source_f06cdd11db7a0614.orna"),
            include_str!("fixtures/evaluator_source_0ef97d25a4949337.orna"),
        ],
        (MinMaxSourceFixtureProfile::Rows, "min") => [
            include_str!("fixtures/evaluator_source_767f07d17295b186.orna"),
            include_str!("fixtures/evaluator_source_c6ea7851dc7a0d00.orna"),
            include_str!("fixtures/evaluator_source_d1bb823348f69642.orna"),
            include_str!("fixtures/evaluator_source_bf38c9cde16fc38e.orna"),
            include_str!("fixtures/evaluator_source_06bb49b9e706e6b6.orna"),
            include_str!("fixtures/evaluator_source_099c851a5484b99b.orna"),
            include_str!("fixtures/evaluator_source_3848120ab89ed54b.orna"),
            include_str!("fixtures/evaluator_source_7f2aab0febab6852.orna"),
        ],
        (MinMaxSourceFixtureProfile::Rows, "max") => [
            include_str!("fixtures/evaluator_source_522fd6a43762eaba.orna"),
            include_str!("fixtures/evaluator_source_5fcd75ea8c2dad4b.orna"),
            include_str!("fixtures/evaluator_source_a0d3016a724d5529.orna"),
            include_str!("fixtures/evaluator_source_de76ac69b637b487.orna"),
            include_str!("fixtures/evaluator_source_6bd6099520370d6c.orna"),
            include_str!("fixtures/evaluator_source_89bb9af05ec8e89b.orna"),
            include_str!("fixtures/evaluator_source_27840ba8100b469e.orna"),
            include_str!("fixtures/evaluator_source_521ddcaa7ba9896b.orna"),
        ],
        (MinMaxSourceFixtureProfile::Dates, "min") => [
            include_str!("fixtures/evaluator_source_0294540bdac86ab6.orna"),
            include_str!("fixtures/evaluator_source_d82941ecf9ade188.orna"),
            include_str!("fixtures/evaluator_source_93deab8333bc90b0.orna"),
            include_str!("fixtures/evaluator_source_d6f55240cd82ef6a.orna"),
            include_str!("fixtures/evaluator_source_cd733d8a56a01e3e.orna"),
            include_str!("fixtures/evaluator_source_e7b46377701dfb99.orna"),
            include_str!("fixtures/evaluator_source_94daad8c17db356c.orna"),
            include_str!("fixtures/evaluator_source_00e1dbc9ca3c3eb9.orna"),
        ],
        (MinMaxSourceFixtureProfile::Dates, "max") => [
            include_str!("fixtures/evaluator_source_77bd25632a298088.orna"),
            include_str!("fixtures/evaluator_source_1623e875982514aa.orna"),
            include_str!("fixtures/evaluator_source_3a69914e39e5f4bc.orna"),
            include_str!("fixtures/evaluator_source_e5003768aa77963f.orna"),
            include_str!("fixtures/evaluator_source_2a9967a7d9b3392c.orna"),
            include_str!("fixtures/evaluator_source_9d99e3d9ebd37e70.orna"),
            include_str!("fixtures/evaluator_source_2de2bb2107e2cfa9.orna"),
            include_str!("fixtures/evaluator_source_e108a79d9726a874.orna"),
        ],
        (MinMaxSourceFixtureProfile::Instants, "min") => [
            include_str!("fixtures/evaluator_source_94a266fb71ae55b2.orna"),
            include_str!("fixtures/evaluator_source_57fb172a38fb0bd6.orna"),
            include_str!("fixtures/evaluator_source_80265be66077ab68.orna"),
            include_str!("fixtures/evaluator_source_0ae4ea75eaec5318.orna"),
            include_str!("fixtures/evaluator_source_f44abe07ab627f54.orna"),
            include_str!("fixtures/evaluator_source_80d65fd9d05586b0.orna"),
            include_str!("fixtures/evaluator_source_f67583071a0bd638.orna"),
            include_str!("fixtures/evaluator_source_b3abe0f591dccd36.orna"),
        ],
        (MinMaxSourceFixtureProfile::Instants, "max") => [
            include_str!("fixtures/evaluator_source_6963d86c80d4d0b9.orna"),
            include_str!("fixtures/evaluator_source_224225bd178ac9b6.orna"),
            include_str!("fixtures/evaluator_source_52424b813d1aef35.orna"),
            include_str!("fixtures/evaluator_source_ddf2549ed0a7ecb0.orna"),
            include_str!("fixtures/evaluator_source_bfe8e74f5bc6108d.orna"),
            include_str!("fixtures/evaluator_source_241b6dec97ac7a88.orna"),
            include_str!("fixtures/evaluator_source_fe9555ba82a8c073.orna"),
            include_str!("fixtures/evaluator_source_43c8051b5e1e6fb0.orna"),
        ],
        (MinMaxSourceFixtureProfile::Integers, "min") => [
            include_str!("fixtures/evaluator_source_c970c457a115af64.orna"),
            include_str!("fixtures/evaluator_source_1adaacf52ecd916e.orna"),
            include_str!("fixtures/evaluator_source_be9f09f79341d61d.orna"),
            include_str!("fixtures/evaluator_source_d16c9e502105a540.orna"),
            include_str!("fixtures/evaluator_source_e7ee918d950403da.orna"),
            include_str!("fixtures/evaluator_source_4d497de51c72050d.orna"),
            include_str!("fixtures/evaluator_source_08af8b01467e6072.orna"),
            include_str!("fixtures/evaluator_source_922c3c285def8d2f.orna"),
        ],
        (MinMaxSourceFixtureProfile::Integers, "max") => [
            include_str!("fixtures/evaluator_source_58f496753613466c.orna"),
            include_str!("fixtures/evaluator_source_a5439c79f3d5d61a.orna"),
            include_str!("fixtures/evaluator_source_4b03efcc13423665.orna"),
            include_str!("fixtures/evaluator_source_a4cb10bea3b3f18e.orna"),
            include_str!("fixtures/evaluator_source_82aa9b05654afe2e.orna"),
            include_str!("fixtures/evaluator_source_13f9f99a2678ffe7.orna"),
            include_str!("fixtures/evaluator_source_4eb9274d7dddd3e8.orna"),
            include_str!("fixtures/evaluator_source_c3a8ac78ee6aa21c.orna"),
        ],
        _ => unreachable!("only min/max source fixtures are registered"),
    }
}

fn float_rows(bits: &[u64]) -> Value {
    Value::new(Raw::Array(bits.iter().copied().map(Raw::Float).collect())).unwrap()
}
fn money_raw(coefficient: BigInt, exponent10: BigInt, currency: [u8; 16]) -> Raw {
    Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![
                    Raw::Int(coefficient),
                    Raw::Int(exponent10),
                ])),
            ),
            Raw::Tag(37, Box::new(Raw::Bytes(currency.to_vec()))),
        ])),
    )
}

fn money_value(coefficient: BigInt, exponent10: BigInt, currency: [u8; 16]) -> Value {
    Value::new(money_raw(coefficient, exponent10, currency)).unwrap()
}

fn money_rows(values: Vec<Raw>) -> Value {
    Value::new(Raw::Array(values)).unwrap()
}

fn invoke(source: &str, arguments: &Environment, limits: Limits) -> Result<Value, EvaluationError> {
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let orna_syntax_v1::Declaration::Function { signature, body } =
        &parsed.value.items[0].declaration
    else {
        panic!("function expected");
    };
    evaluate_function(
        &signature.parameters,
        body,
        &Environment::new(),
        arguments,
        limits,
    )
}

fn functions_from_source(source: &str) -> orna_evaluator_v1::Functions {
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    parsed
        .value
        .items
        .into_iter()
        .map(|item| {
            let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration else {
                panic!("function expected")
            };
            (
                signature.name,
                orna_evaluator_v1::PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            )
        })
        .collect()
}

fn call_module(source: &str, expression: &str, limits: Limits) -> Result<Value, EvaluationError> {
    let functions = functions_from_source(source);
    let parsed = orna_syntax_v1::parse_expression(expression);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    orna_evaluator_v1::evaluate_with_functions(
        &parsed.value,
        &Environment::new(),
        &functions,
        limits,
    )
}

fn parsed_expression(source: &str) -> Expr {
    let parsed = orna_syntax_v1::parse_expression(source);
    assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
    parsed.value
}

fn relation_span() -> SyntaxSpan {
    SyntaxSpan::new(0, 0)
}

fn relation_argument(value: Expr, span: &SyntaxSpan) -> orna_syntax_v1::Argument {
    orna_syntax_v1::Argument {
        name: None,
        value,
        span: span.clone(),
    }
}

fn relation_source_expression(source: &str) -> Expr {
    let span = relation_span();
    Expr::Call {
        callee: Box::new(Expr::Field {
            base: Box::new(Expr::ReplBinding {
                text: "$__orna_relation".into(),
                span: span.clone(),
            }),
            name: "source".into(),
            span: span.clone(),
        }),
        arguments: vec![relation_argument(
            Expr::Literal {
                text: format!("{source:?}"),
                kind: orna_syntax_v1::LiteralKind::String,
                span: span.clone(),
            },
            &span,
        )],
        span,
    }
}

fn relation_stage(input: Expr, name: &str, arguments: Vec<Expr>) -> Expr {
    let span = relation_span();
    Expr::Binary {
        lhs: Box::new(input),
        op: "|".into(),
        rhs: Box::new(Expr::Call {
            callee: Box::new(Expr::Name {
                text: name.into(),
                span: span.clone(),
            }),
            arguments: arguments
                .into_iter()
                .map(|value| relation_argument(value, &span))
                .collect(),
            span: span.clone(),
        }),
        span,
    }
}

fn relation_terminal(input: Expr, name: &str) -> Expr {
    relation_stage(input, name, Vec::new())
}

fn relation_integer(value: i64) -> Expr {
    let span = relation_span();
    Expr::Literal {
        text: value.to_string(),
        kind: orna_syntax_v1::LiteralKind::Integer,
        span,
    }
}

fn relation_pair(left: i64, right: i64) -> Value {
    Value::new(Raw::Array(vec![
        Raw::Int(left.into()),
        Raw::Int(right.into()),
    ]))
    .unwrap()
}
fn relation_window_row(values: &[i64]) -> Value {
    Value::new(Raw::Array(
        values
            .iter()
            .copied()
            .map(|value| Raw::Int(value.into()))
            .collect(),
    ))
    .unwrap()
}

fn relation_window_direct(source: Expr, size: Expr, step: Option<Expr>) -> Expr {
    let span = relation_span();
    let mut arguments = vec![
        relation_argument(source, &span),
        relation_argument(size, &span),
    ];
    if let Some(step) = step {
        arguments.push(relation_argument(step, &span));
    }
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: "window".into(),
            span: span.clone(),
        }),
        arguments,
        span,
    }
}

fn relation_named_window_direct(source: Expr, size: Expr, step: Option<Expr>) -> Expr {
    let span = relation_span();
    let mut arguments = vec![
        orna_syntax_v1::Argument {
            name: Some("rows".into()),
            value: source,
            span: span.clone(),
        },
        orna_syntax_v1::Argument {
            name: Some("size".into()),
            value: size,
            span: span.clone(),
        },
    ];
    if let Some(step) = step {
        arguments.push(orna_syntax_v1::Argument {
            name: Some("step".into()),
            value: step,
            span: span.clone(),
        });
    }
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: "window".into(),
            span: span.clone(),
        }),
        arguments,
        span,
    }
}

fn relation_named_window_pipeline(input: Expr, size: Expr, step: Option<Expr>) -> Expr {
    let span = relation_span();
    let mut arguments = vec![orna_syntax_v1::Argument {
        name: Some("size".into()),
        value: size,
        span: span.clone(),
    }];
    if let Some(step) = step {
        arguments.push(orna_syntax_v1::Argument {
            name: Some("step".into()),
            value: step,
            span: span.clone(),
        });
    }
    Expr::Binary {
        lhs: Box::new(input),
        op: "|".into(),
        rhs: Box::new(Expr::Call {
            callee: Box::new(Expr::Name {
                text: "window".into(),
                span: span.clone(),
            }),
            arguments,
            span: span.clone(),
        }),
        span,
    }
}

fn relation_function(body: Expr) -> Functions {
    Functions::from([(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body,
            environment: Environment::new(),
        },
    )])
}

struct DistinctRelationEffects {
    rows: Vec<Value>,
    cursors: Vec<Option<Vec<u8>>>,
}

impl DistinctRelationEffects {
    fn new(rows: Vec<Value>) -> Self {
        Self {
            rows,
            cursors: Vec::new(),
        }
    }
}

impl EffectHandler for DistinctRelationEffects {
    fn handle(&mut self, _: &Expr, _: &[Value]) -> Result<Option<Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        _: &str,
        after: Option<&[u8]>,
        _: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        budget.debit(1)?;
        self.cursors.push(after.map(ToOwned::to_owned));
        let index = after.map_or(0, |cursor| usize::from(cursor[0]));
        if index >= self.rows.len() {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        let next = (index + 1 < self.rows.len()).then(|| vec![(index + 1) as u8]);
        Ok(Some(RelationPage {
            rows: vec![self.rows[index].clone()],
            next,
        }))
    }
}
struct UnionRelationEffects {
    rows: BTreeMap<String, Vec<Value>>,
    cursors: Vec<(String, Option<Vec<u8>>)>,
}

struct RepeatedUnknownRelationEffects {
    rows: Vec<Value>,
    starts: usize,
}

impl EffectHandler for RepeatedUnknownRelationEffects {
    fn handle(&mut self, _: &Expr, _: &[Value]) -> Result<Option<Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        _: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        assert_eq!(source, "sys.Storage");
        assert!(after.is_none(), "each fixture source contains one row");
        budget.debit(1)?;
        let Some(row) = self.rows.get(self.starts).cloned() else {
            panic!("unexpected extra scan for unknown source {}", self.starts);
        };
        self.starts += 1;
        Ok(Some(RelationPage {
            rows: vec![row],
            next: None,
        }))
    }
}

impl UnionRelationEffects {
    fn new(left: Vec<Value>, right: Vec<Value>) -> Self {
        Self {
            rows: BTreeMap::from([("Left".into(), left), ("Right".into(), right)]),
            cursors: Vec::new(),
        }
    }
}

impl EffectHandler for UnionRelationEffects {
    fn handle(&mut self, _: &Expr, _: &[Value]) -> Result<Option<Value>, EvaluationError> {
        Ok(None)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        _: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        budget.debit(1)?;
        self.cursors
            .push((source.into(), after.map(ToOwned::to_owned)));
        let rows = self
            .rows
            .get(source)
            .unwrap_or_else(|| panic!("unexpected relation source {source}"));
        let index = after.map_or(0, |cursor| usize::from(cursor[0]));
        if index >= rows.len() {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        let next = (index + 1 < rows.len()).then(|| vec![(index + 1) as u8]);
        Ok(Some(RelationPage {
            rows: vec![rows[index].clone()],
            next,
        }))
    }
}

fn relation_union(left: Expr, right: Expr) -> Expr {
    let span = relation_span();
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: "union".into(),
            span: span.clone(),
        }),
        arguments: vec![
            relation_argument(left, &span),
            relation_argument(right, &span),
        ],
        span,
    }
}

fn invoke_relation<E: EffectHandler>(
    body: Expr,
    effects: &mut E,
    limits: Limits,
) -> Result<Value, EvaluationError> {
    let functions = relation_function(body);
    invoke_named_with_effects("run", &functions, &Environment::new(), limits, effects)
}

fn nominal_field(name: &str, public: bool, default: Option<&str>) -> NominalField {
    match (public, default) {
        (true, Some(default)) => NominalField::public_with_default(
            field_id_bytes(name),
            name,
            parsed_expression(default),
        ),
        (true, None) => NominalField::public(field_id_bytes(name), name),
        (false, Some(default)) => NominalField::private_with_default(
            field_id_bytes(name),
            name,
            parsed_expression(default),
        ),
        (false, None) => NominalField::private(field_id_bytes(name), name),
    }
}

fn object_id_bytes(name: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, byte) in name.bytes().take(16).enumerate() {
        bytes[index] = byte;
    }
    bytes
}

fn field_id_bytes(name: &str) -> [u8; 16] {
    object_id_bytes(&format!("field:{name}"))
}

fn type_id_raw(name: &str) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(object_id_bytes(name).to_vec())))
}

fn field_id_raw(name: &str) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(field_id_bytes(name).to_vec())))
}

fn nominal_definitions(
    path: &str,
    type_id: &str,
    owner: Option<&str>,
    fields: Vec<NominalField>,
) -> NominalDefinitions {
    NominalDefinitions::from([(
        path.into(),
        NominalDefinition::new(object_id_bytes(type_id), owner.map(str::to_owned), fields),
    )])
}

fn nominal_parts(value: &orna_foundation_v1::CanonicalValue) -> (&Raw, &[Raw]) {
    let Raw::Tag(60009, body) = value.raw() else {
        panic!("expected nominal value, got {value:?}");
    };
    let Raw::Array(parts) = body.as_ref() else {
        panic!("expected nominal payload");
    };
    let [type_id, Raw::Array(fields)] = parts.as_slice() else {
        panic!("expected nominal identity and fields");
    };
    (type_id, fields.as_slice())
}

fn nominal_input(field_key: Raw) -> orna_foundation_v1::CanonicalValue {
    nominal_input_with_fields(vec![(field_key, Raw::Int(7.into()))])
}

fn nominal_input_with_fields(fields: Vec<(Raw, Raw)>) -> orna_foundation_v1::CanonicalValue {
    orna_foundation_v1::CanonicalValue::new(Raw::Tag(
        60009,
        Box::new(Raw::Array(vec![
            type_id_raw("stable.Thing"),
            Raw::Array(
                fields
                    .into_iter()
                    .map(|(key, value)| Raw::Array(vec![key, value]))
                    .collect(),
            ),
        ])),
    ))
    .expect("nominal input should be structurally canonical")
}

#[test]
fn evaluator_decode_rejects_nominal_text_field_keys() {
    let environment = BTreeMap::from([("value".into(), nominal_input(Raw::Text("value".into())))]);

    let result = evaluate_parsed(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_cd42404d52ad55cc.orna"
        )),
        &environment,
        Limits::default(),
    );

    assert_eq!(code(result), "ORNA-EVAL-VALUE");
}

#[test]
fn evaluator_decode_accepts_nominal_object_id_field_keys() {
    let input = nominal_input(field_id_raw("value"));
    let environment = BTreeMap::from([("value".into(), input.clone())]);

    let result = evaluate_parsed(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_cd42404d52ad55cc.orna"
        )),
        &environment,
        Limits::default(),
    )
    .expect("canonical ObjectId nominal field keys should decode");

    assert_eq!(result.raw(), input.raw());
}

#[test]
fn nominal_field_selection_rejects_missing_unknown_and_ambiguous_metadata() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("value", true, None),
            nominal_field("other", true, None),
        ],
    );
    let environment = BTreeMap::from([(
        "value".into(),
        nominal_input_with_fields(vec![
            (field_id_raw("other"), Raw::Int(8.into())),
            (field_id_raw("value"), Raw::Int(7.into())),
        ]),
    )]);
    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_81c26d6628d1ca4a.orna"
            )),
            &environment,
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-FIELD"
    );

    let unknown_type = BTreeMap::from([(
        "value".into(),
        orna_foundation_v1::CanonicalValue::new(Raw::Tag(
            60009,
            Box::new(Raw::Array(vec![
                type_id_raw("stable.Unknown"),
                Raw::Array(vec![Raw::Array(vec![
                    field_id_raw("value"),
                    Raw::Int(7.into()),
                ])]),
            ])),
        ))
        .expect("unknown nominal type should remain structurally canonical"),
    )]);
    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_9dd4086bbd93e865.orna"
            )),
            &unknown_type,
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );

    let mut ambiguous = definitions.clone();
    ambiguous.insert(
        "OtherThing".into(),
        NominalDefinition::new(
            object_id_bytes("stable.Thing"),
            None,
            vec![nominal_field("value", true, None)],
        ),
    );
    let environment = BTreeMap::from([("value".into(), nominal_input(field_id_raw("value")))]);
    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_9dd4086bbd93e865.orna"
            )),
            &environment,
            &ambiguous,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn nominal_field_selection_rejects_extra_and_missing_payload_members() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("value", true, None),
            nominal_field("other", true, None),
        ],
    );
    let extra = BTreeMap::from([(
        "value".into(),
        nominal_input_with_fields(vec![
            (field_id_raw("unknown"), Raw::Int(8.into())),
            (field_id_raw("value"), Raw::Int(7.into())),
        ]),
    )]);
    let missing = BTreeMap::from([(
        "value".into(),
        nominal_input_with_fields(vec![(field_id_raw("value"), Raw::Int(7.into()))]),
    )]);

    for environment in [extra, missing] {
        assert_eq!(
            code(evaluate_parsed_with_nominals(
                &parsed_expression(include_str!(
                    "fixtures/evaluator_source_9dd4086bbd93e865.orna"
                )),
                &environment,
                &definitions,
                Limits::default(),
            )),
            "ORNA-EVAL-VALUE"
        );
    }
}

#[test]
fn nominal_admission_rejects_malformed_matching_payload_without_field_selection() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, None)],
    );
    let environment = BTreeMap::from([(
        "value".into(),
        nominal_input_with_fields(vec![
            (field_id_raw("unknown"), Raw::Int(8.into())),
            (field_id_raw("value"), Raw::Int(7.into())),
        ]),
    )]);

    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_cd42404d52ad55cc.orna"
            )),
            &environment,
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn nominal_admission_rejects_malformed_nominals_in_recursive_containers() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, None)],
    );
    let malformed = nominal_input_with_fields(vec![
        (field_id_raw("unknown"), Raw::Int(8.into())),
        (field_id_raw("value"), Raw::Int(7.into())),
    ]);
    let nested = orna_foundation_v1::CanonicalValue::new(Raw::Array(vec![Raw::Map(vec![(
        Raw::Text("nested".into()),
        malformed.raw().clone(),
    )])]))
    .expect("nested nominal input should be structurally canonical");
    let environment = BTreeMap::from([("value".into(), nested)]);

    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_cd42404d52ad55cc.orna"
            )),
            &environment,
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn nominal_admission_rejects_malformed_host_arguments_even_when_unselected() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, None)],
    );
    let malformed = nominal_input_with_fields(vec![
        (field_id_raw("unknown"), Raw::Int(8.into())),
        (field_id_raw("value"), Raw::Int(7.into())),
    ]);
    let arguments = Environment::from([("value".into(), malformed)]);
    let functions = functions_from_source(include_str!(
        "fixtures/nominal_admission_unselected_argument.orna"
    ));

    assert_eq!(
        code(invoke_named_with_nominals(
            "ignore",
            &functions,
            &arguments,
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn nominal_admission_rejects_malformed_named_call_captures() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, None)],
    );
    let malformed = nominal_input_with_fields(vec![
        (field_id_raw("unknown"), Raw::Int(8.into())),
        (field_id_raw("value"), Raw::Int(7.into())),
    ]);
    let functions = BTreeMap::from([
        (
            "run".into(),
            PureFunction {
                parameters: Vec::new(),
                body: parsed_expression(include_str!(
                    "fixtures/evaluator_source_9f70e25cadc7ce9e.orna"
                )),
                environment: Environment::new(),
            },
        ),
        (
            "helper".into(),
            PureFunction {
                parameters: Vec::new(),
                body: parsed_expression(include_str!(
                    "fixtures/evaluator_source_6b86b273ff34fce1.orna"
                )),
                environment: Environment::from([("captured".into(), malformed)]),
            },
        ),
    ]);

    assert_eq!(
        code(invoke_named_with_nominals(
            "run",
            &functions,
            &Environment::new(),
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn nominal_definition_admission_enforces_total_limits_before_allocation() {
    let oversized_definitions = NominalDefinitions::from([
        (
            "Thing".into(),
            NominalDefinition::new(object_id_bytes("stable.Thing"), None, Vec::new()),
        ),
        (
            "Other".into(),
            NominalDefinition::new(object_id_bytes("stable.Other"), None, Vec::new()),
        ),
    ]);
    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_6b86b273ff34fce1.orna"
            )),
            &Environment::new(),
            &oversized_definitions,
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    let oversized_fields = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("left", true, None),
            nominal_field("right", true, None),
        ],
    );
    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_6b86b273ff34fce1.orna"
            )),
            &Environment::new(),
            &oversized_fields,
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    let oversized_name = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("long_name", true, None)],
    );
    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_6b86b273ff34fce1.orna"
            )),
            &Environment::new(),
            &oversized_name,
            Limits {
                max_string_bytes: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn nominal_definition_admission_rejects_oversized_owner_before_cloning() {
    let definitions = nominal_definitions("Thing", "stable.Thing", Some("owner"), Vec::new());

    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_6b86b273ff34fce1.orna"
            )),
            &Environment::new(),
            &definitions,
            Limits {
                max_string_bytes: 4,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn nominal_construction_rejects_ambiguous_declared_fields() {
    let duplicate_names = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("value", true, None),
            NominalField::public(object_id_bytes("field:other"), "value"),
        ],
    );
    let duplicate_ids = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            NominalField::public([9; 16], "left"),
            NominalField::public([9; 16], "right"),
        ],
    );

    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_b7e70e0c52b91be1.orna"
            )),
            &Environment::new(),
            &duplicate_names,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_8a912f6862713b26.orna"
            )),
            &Environment::new(),
            &duplicate_ids,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn nominal_construction_rejects_duplicate_type_ids_in_active_scope() {
    let definitions = NominalDefinitions::from([
        (
            "Thing".into(),
            NominalDefinition::new(
                object_id_bytes("stable.Thing"),
                None,
                vec![nominal_field("value", true, None)],
            ),
        ),
        (
            "OtherThing".into(),
            NominalDefinition::new(
                object_id_bytes("stable.Thing"),
                None,
                vec![nominal_field("value", true, None)],
            ),
        ),
    ]);

    assert_eq!(
        code(evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_b7e70e0c52b91be1.orna"
            )),
            &Environment::new(),
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn nominal_public_field_selection_reads_the_declared_value() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, None)],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_903d97fbd7c57964.orna"
        )),
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("public nominal fields should be readable");

    assert_eq!(result.raw(), &Raw::Int(7.into()));
}

#[test]
fn nominal_private_field_selection_is_owner_scoped() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field("secret", false, None)],
    );
    let functions = BTreeMap::from([(
        "vault.read".into(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed_expression(include_str!(
                "fixtures/evaluator_source_e622366d50fead9c.orna"
            )),
            environment: Environment::new(),
        },
    )]);

    let result = invoke_named_with_nominals(
        "vault.read",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("the nominal owner should read private fields");

    assert_eq!(result.raw(), &Raw::Int(7.into()));
}

#[test]
fn nominal_private_field_selection_is_rejected_outside_owner() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field("secret", false, None)],
    );
    let functions = BTreeMap::from([(
        "vault.make".into(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed_expression(include_str!(
                "fixtures/evaluator_source_daa861163cdbfc28.orna"
            )),
            environment: Environment::new(),
        },
    )]);
    let value = invoke_named_with_nominals(
        "vault.make",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("the nominal owner should construct the value");
    let environment = BTreeMap::from([("value".into(), value)]);

    assert_eq!(
        code(evaluate_with_functions_and_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_b054fcd571d69b72.orna"
            )),
            &environment,
            &BTreeMap::new(),
            &definitions,
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn nominal_private_field_selection_works_in_owner_created_direct_closure() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field("secret", false, None)],
    );
    let functions = BTreeMap::from([(
        "vault.read".into(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed_expression(include_str!(
                "fixtures/evaluator_source_8d756a2c60c35c1f.orna"
            )),
            environment: Environment::new(),
        },
    )]);

    let result = invoke_named_with_nominals(
        "vault.read",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("owner-created direct closures should retain their namespace");

    assert_eq!(result.raw(), &Raw::Int(7.into()));
}

#[test]
fn nominal_private_field_selection_works_in_owner_created_collection_closure() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field("secret", false, None)],
    );
    let functions = BTreeMap::from([(
        "vault.map".into(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed_expression(include_str!(
                "fixtures/evaluator_source_dce4414336a87f81.orna"
            )),
            environment: Environment::new(),
        },
    )]);

    let result = invoke_named_with_nominals(
        "vault.map",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("owner-created collection closures should retain their namespace");

    assert_eq!(
        result.raw(),
        &Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())])
    );
}

#[test]
fn nominal_field_selection_uses_stable_field_identity_not_text_key() {
    let stable_field_id = object_id_bytes("catalogue-field");
    let definitions = NominalDefinitions::from([(
        "Thing".into(),
        NominalDefinition::new(
            object_id_bytes("stable.Thing"),
            None,
            vec![NominalField::public(stable_field_id, "value")],
        ),
    )]);
    let input = orna_foundation_v1::CanonicalValue::new(Raw::Tag(
        60009,
        Box::new(Raw::Array(vec![
            type_id_raw("stable.Thing"),
            Raw::Array(vec![Raw::Array(vec![
                Raw::Tag(37, Box::new(Raw::Bytes(stable_field_id.to_vec()))),
                Raw::Int(11.into()),
            ])]),
        ])),
    ))
    .expect("stable nominal identity should be canonical");
    let environment = BTreeMap::from([("value".into(), input)]);

    let result = evaluate_parsed_with_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_9dd4086bbd93e865.orna"
        )),
        &environment,
        &definitions,
        Limits::default(),
    )
    .expect("declared stable field identity should be readable");

    assert_eq!(result.raw(), &Raw::Int(11.into()));
}

#[test]
fn named_collection_callback_preserves_public_nominal_selection() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, None)],
    );
    let functions = functions_from_source(include_str!("fixtures/nominal_map_callback.orna"));

    let result = invoke_named_with_nominals(
        "run",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("named collection callbacks should retain nominal definitions");

    assert_eq!(result.raw(), &Raw::Array(vec![Raw::Int(7.into())]));
}

#[test]
fn nominal_construction_materializes_supplied_and_default_fields_in_declaration_order() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("left", true, None),
            nominal_field("right", true, Some("left + 1")),
            nominal_field("tail", true, Some("right + 1")),
        ],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_16e497097d7c8ca9.orna"
        )),
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("nominal construction should evaluate");
    let (type_id, fields) = nominal_parts(&result);

    assert_eq!(type_id, &type_id_raw("stable.Thing"));
    assert_eq!(
        fields,
        &[
            Raw::Array(vec![field_id_raw("left"), Raw::Int(3.into())]),
            Raw::Array(vec![field_id_raw("right"), Raw::Int(4.into())]),
            Raw::Array(vec![field_id_raw("tail"), Raw::Int(5.into())]),
        ]
    );
}

#[test]
fn nominal_supplied_fields_evaluate_in_written_order_before_canonical_serialization() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("second", true, None),
            nominal_field("first", true, None),
        ],
    );
    let span = SyntaxSpan::new(0, 0);
    let counter = || Expr::Name {
        text: "counter".into(),
        span: span.clone(),
    };
    let increment = || Expr::Binary {
        lhs: Box::new(counter()),
        op: "+".into(),
        rhs: Box::new(Expr::Literal {
            text: "1".into(),
            kind: orna_syntax_v1::LiteralKind::Integer,
            span: span.clone(),
        }),
        span: span.clone(),
    };
    let counted = || Expr::Block {
        statements: vec![Statement::Assignment {
            target: AssignmentTarget::Name {
                name: "counter".into(),
                span: span.clone(),
            },
            operator: AssignmentOperator::Set,
            value: increment(),
            span: span.clone(),
        }],
        tail: Some(Box::new(counter())),
        span: span.clone(),
    };
    let body = Expr::Block {
        statements: vec![Statement::Let {
            pattern: Pattern::Name("counter".into(), span.clone()),
            annotation: None,
            value: Expr::Literal {
                text: "0".into(),
                kind: orna_syntax_v1::LiteralKind::Integer,
                span: span.clone(),
            },
            span: span.clone(),
        }],
        tail: Some(Box::new(Expr::Nominal {
            path: vec![NameSegment {
                text: "Thing".into(),
                span: span.clone(),
            }],
            fields: vec![
                RecordField {
                    name: "first".into(),
                    value: counted(),
                    span: span.clone(),
                },
                RecordField {
                    name: "second".into(),
                    value: counted(),
                    span: span.clone(),
                },
            ],
            span: span.clone(),
        })),
        span,
    };
    let functions = BTreeMap::from([(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body,
            environment: Environment::new(),
        },
    )]);
    let result = invoke_named_with_nominals(
        "run",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("supplied fields should evaluate in source order");
    let (_, fields) = nominal_parts(&result);
    let values = fields
        .iter()
        .map(|field| {
            let Raw::Array(parts) = field else {
                panic!("expected nominal field entry");
            };
            let [key, value] = parts.as_slice() else {
                panic!("expected nominal field entry");
            };
            let name = ["first", "second"]
                .into_iter()
                .find(|name| field_id_raw(name) == *key)
                .expect("known nominal field id");
            (name.to_owned(), value.clone())
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(values.get("first"), Some(&Raw::Int(1.into())));
    assert_eq!(values.get("second"), Some(&Raw::Int(2.into())));
}

#[test]
fn nominal_supplied_values_bypass_omitted_defaults() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![nominal_field("value", true, Some("1 / 0"))],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_20a1b6e25e5cc7b0.orna"
        )),
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("a supplied field must not evaluate its default");
    let (_, fields) = nominal_parts(&result);
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("value"), Raw::Int(9.into())])]
    );
}

#[test]
fn external_nominal_public_default_runs_in_declaration_owner_namespace() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field(
            "value",
            true,
            Some(include_str!(
                "fixtures/evaluator_source_9f70e25cadc7ce9e.orna"
            )),
        )],
    );
    let functions = BTreeMap::from([(
        "vault.helper".into(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed_expression(include_str!(
                "fixtures/evaluator_source_7902699be42c8a8e.orna"
            )),
            environment: Environment::new(),
        },
    )]);
    let result = evaluate_with_functions_and_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_407ebfd8b1b347d5.orna"
        )),
        &Environment::new(),
        &functions,
        &definitions,
        Limits::default(),
    )
    .expect("public defaults must execute in their declaration namespace");
    let (_, fields) = nominal_parts(&result);
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("value"), Raw::Int(7.into())])]
    );
}

#[test]
fn nominal_defaults_run_once_in_declaration_order() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("first", true, Some("1")),
            nominal_field("second", true, Some("first + 1")),
        ],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_c5680747744f836e.orna"
        )),
        &Environment::new(),
        &definitions,
        Limits {
            max_steps: 5,
            ..Limits::default()
        },
    )
    .expect("each declaration default should execute once");
    let (_, fields) = nominal_parts(&result);
    assert_eq!(
        fields,
        &[
            Raw::Array(vec![field_id_raw("first"), Raw::Int(1.into())]),
            Raw::Array(vec![field_id_raw("second"), Raw::Int(2.into())]),
        ]
    );
}

#[test]
fn nominal_construction_rejects_duplicate_unknown_missing_and_failing_fields() {
    let definitions = nominal_definitions(
        "Thing",
        "stable.Thing",
        None,
        vec![
            nominal_field("required", true, None),
            nominal_field("failing", true, Some("1 / 0")),
        ],
    );
    for source in [
        include_str!("fixtures/evaluator_source_7ca05863848cfa37.orna"),
        include_str!("fixtures/evaluator_source_8ba6f8428b3bcbe6.orna"),
    ] {
        assert_eq!(
            evaluate_parsed_with_nominals(
                &parsed_expression(source),
                &Environment::new(),
                &definitions,
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-EVAL-ARGUMENT",
            "{source}"
        );
    }
    assert_eq!(
        evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_c5680747744f836e.orna"
            )),
            &Environment::new(),
            &definitions,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        evaluate_parsed_with_nominals(
            &parsed_expression(include_str!(
                "fixtures/evaluator_source_92e08454f1d9908c.orna"
            )),
            &Environment::new(),
            &definitions,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn external_nominal_construction_rejects_private_fields_but_owner_namespace_can_construct() {
    let definitions = nominal_definitions(
        "vault.Vault",
        "stable.Vault",
        Some("vault"),
        vec![nominal_field("secret", false, None)],
    );
    let expression = parsed_expression(include_str!(
        "fixtures/evaluator_source_5007e6666ea5dc3c.orna"
    ));
    assert_eq!(
        evaluate_parsed_with_nominals(
            &expression,
            &Environment::new(),
            &definitions,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );

    let defaulted_private = nominal_definitions(
        "vault.Defaulted",
        "stable.Defaulted",
        Some("vault"),
        vec![nominal_field("secret", false, Some("7"))],
    );
    let result = evaluate_parsed_with_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_611645faa7eca082.orna"
        )),
        &Environment::new(),
        &defaulted_private,
        Limits::default(),
    )
    .expect("an omitted private field with a declaration default is admissible");
    let (type_id, fields) = nominal_parts(&result);
    assert_eq!(type_id, &type_id_raw("stable.Defaulted"));
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("secret"), Raw::Int(7.into())])]
    );

    let mut definitions = definitions;
    definitions.insert(
        "Vault".into(),
        NominalDefinition::new(
            object_id_bytes("stable.ShortVault"),
            None,
            vec![nominal_field("other", true, Some("1"))],
        ),
    );

    let functions = std::collections::BTreeMap::from([
        (
            "vault.make".into(),
            PureFunction {
                parameters: Vec::new(),
                body: parsed_expression(include_str!(
                    "fixtures/evaluator_source_572943b08b8333ca.orna"
                )),
                environment: Environment::new(),
            },
        ),
        (
            "vault.inner".into(),
            PureFunction {
                parameters: Vec::new(),
                body: parsed_expression(include_str!(
                    "fixtures/evaluator_source_daa861163cdbfc28.orna"
                )),
                environment: Environment::new(),
            },
        ),
    ]);
    let result = invoke_named_with_nominals(
        "vault.make",
        &functions,
        &Environment::new(),
        &definitions,
        Limits::default(),
    )
    .expect("the owning namespace may construct private fields");
    let (type_id, fields) = nominal_parts(&result);
    assert_eq!(type_id, &type_id_raw("stable.Vault"));
    assert_eq!(
        fields,
        &[Raw::Array(vec![field_id_raw("secret"), Raw::Int(7.into())])]
    );

    let root_owned = nominal_definitions(
        "RootThing",
        "stable.RootThing",
        None,
        vec![nominal_field("secret", false, None)],
    );
    evaluate_parsed_with_nominals(
        &parsed_expression(include_str!(
            "fixtures/evaluator_source_1cb340a13ff58ff9.orna"
        )),
        &Environment::new(),
        &root_owned,
        Limits::default(),
    )
    .expect("root-owned construction uses the root namespace");
}

#[derive(Default)]
struct NoteEffects {
    calls: Vec<Vec<Value>>,
}

impl EffectHandler for NoteEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text, .. } = base.as_ref() else {
            return Ok(None);
        };
        if text != "Note" || name != "insert" {
            return Ok(None);
        }
        self.calls.push(arguments.to_vec());
        Ok(Some(Value::int(arguments.len().into())))
    }
}

struct UnitEffects;

impl EffectHandler for UnitEffects {
    fn handle(&mut self, callee: &Expr, _: &[Value]) -> Result<Option<Value>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text, .. } = base.as_ref() else {
            return Ok(None);
        };
        if text == "Note" && name == "delete" {
            Ok(Some(Value::unit()))
        } else {
            Ok(None)
        }
    }
}

struct BudgetedEffects;

impl EffectHandler for BudgetedEffects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        NoteEffects::default().handle(callee, arguments)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
        budget: &mut StepBudget,
    ) -> Result<Option<Value>, EvaluationError> {
        budget.debit(1)?;
        self.handle(callee, arguments)
    }
}

#[test]
fn effect_budget_hook_shares_activation_steps_and_exhausts() {
    let functions = functions_from_source(include_str!("fixtures/effect_budget_entry.orna"));
    let mut effects = BudgetedEffects;

    assert_eq!(
        code(invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits {
                max_steps: 2,
                ..Limits::default()
            },
            &mut effects,
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn legacy_effect_handler_keeps_existing_budget_behavior() {
    let functions = functions_from_source(include_str!("fixtures/effect_entry.orna"));
    let mut effects = NoteEffects::default();

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits {
                max_steps: 2,
                ..Limits::default()
            },
            &mut effects,
        )
        .unwrap(),
        Value::int(1.into())
    );
    assert_eq!(effects.calls, vec![vec![Value::int(1.into())]]);
}

#[test]
fn fail_reemits_the_original_error_instead_of_returning_a_value() {
    let result = evaluate_expression(
        include_str!("fixtures/fail_reemits_the_original_error.orna").trim(),
        &Environment::new(),
        Limits::default(),
    );

    assert_eq!(code(result), "ORNA-EVAL-DIVIDE-BY-ZERO");
}

#[test]
fn fail_from_a_recovery_handler_reaches_the_next_recovery_boundary() {
    assert_eq!(
        evaluate(include_str!("fixtures/fail_from_recovery_handler.orna").trim()),
        Value::int(7.into())
    );
}

#[test]
fn effect_handler_can_return_unit_values() {
    let functions = functions_from_source(include_str!("fixtures/effect_unit_entry.orna"));
    let mut effects = UnitEffects;

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::unit()
    );
}

#[test]
fn admission_limits_reject_zero_configuration_and_count_source_bytes() {
    let limits = Limits {
        max_source_bytes: 2,
        max_collection_items: 2,
        ..Limits::default()
    };
    assert!(limits.check_source("é").is_ok());
    assert_eq!(
        limits.check_source("éa").unwrap_err().code(),
        "ORNA-EVAL-LIMIT"
    );
    assert!(limits.check_items(2).is_ok());
    assert_eq!(limits.check_items(3).unwrap_err().code(), "ORNA-EVAL-LIMIT");
    for index in 0..6 {
        let mut limits = Limits::default();
        match index {
            0 => limits.max_source_bytes = 0,
            1 => limits.max_steps = 0,
            2 => limits.max_depth = 0,
            3 => limits.max_collection_items = 0,
            4 => limits.max_string_bytes = 0,
            _ => limits.max_integer_digits = 0,
        }
        assert_eq!(
            limits.check_source("").unwrap_err().code(),
            "ORNA-EVAL-LIMIT"
        );
        assert_eq!(limits.check_items(0).unwrap_err().code(), "ORNA-EVAL-LIMIT");
    }
}

#[test]
fn source_namespace_entry_checks_values_and_limits_before_parsing() {
    let functions = functions_from_source(include_str!("fixtures/source_namespace_increment.orna"));
    let environment = Environment::from([("input".into(), Value::int(41.into()))]);
    assert_eq!(
        orna_evaluator_v1::evaluate_expression_with_functions(
            include_str!("fixtures/source_namespace_pipeline.orna"),
            &environment,
            &functions,
            Limits::default()
        )
        .unwrap(),
        Value::int(42.into())
    );
    let failure = orna_evaluator_v1::evaluate_expression_with_functions(
        include_str!("fixtures/source_namespace_invalid_expression.orna"),
        &environment,
        &functions,
        Limits {
            max_source_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(failure.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(failure.diagnostic().message(), "<redacted>");
    assert_eq!(
        code(orna_evaluator_v1::evaluate_expression_with_functions(
            include_str!("fixtures/source_namespace_invalid_expression.orna"),
            &environment,
            &functions,
            Limits::default()
        )),
        "ORNA-EVAL-PARSE"
    );
}

#[test]
fn host_invocation_uses_the_same_named_function_namespace() {
    let functions =
        functions_from_source(include_str!("fixtures/host_invocation_same_namespace.orna"));
    assert_eq!(
        orna_evaluator_v1::invoke_named(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default()
        )
        .unwrap(),
        Value::int(42.into())
    );
    assert_eq!(
        code(orna_evaluator_v1::invoke_named(
            "entry",
            &functions,
            &Environment::new(),
            Limits {
                max_steps: 4,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(orna_evaluator_v1::invoke_named(
            "missing",
            &functions,
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn effect_handler_runs_for_a_nested_non_pure_field_call_with_once_evaluated_arguments() {
    let functions = functions_from_source(include_str!("fixtures/effect_nested_field_call.orna"));
    let mut effects = NoteEffects::default();

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::int(1.into())
    );
    assert_eq!(effects.calls, vec![vec![Value::int(1.into())]]);
}

#[test]
fn dynamic_field_calls_evaluate_callee_before_effectful_arguments() {
    let functions = functions_from_source(include_str!("fixtures/effect_dynamic_field_call.orna"));
    let mut effects = NoteEffects::default();

    assert_eq!(
        code(invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(effects.calls, vec![vec![Value::int(1.into())]]);
}

#[test]
fn effect_handler_none_falls_through_without_intercepting_pure_calls() {
    let functions = functions_from_source(include_str!("fixtures/effect_pure_call.orna"));
    let mut effects = NoteEffects::default();

    assert_eq!(
        invoke_named_with_effects(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::int(42.into())
    );
    assert!(effects.calls.is_empty());
}

#[test]
fn invoke_named_remains_effect_free_for_non_pure_field_calls() {
    let functions = functions_from_source(include_str!("fixtures/effect_entry.orna"));

    assert_eq!(
        code(invoke_named(
            "entry",
            &functions,
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn source_calls_bind_positional_named_and_nested_defaults() {
    let source = include_str!("fixtures/source_calls_defaults.orna");
    for expression in [
        include_str!("fixtures/evaluator_source_2e638be6187005ca.orna"),
        include_str!("fixtures/evaluator_source_acc804efc010ae0e.orna"),
        include_str!("fixtures/evaluator_source_7ab067d2194e6002.orna"),
        include_str!("fixtures/evaluator_source_6f1a3950367c5a51.orna"),
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(16.into())
        );
    }
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_b4ea53a692925b5c.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::int(16.into())
    );
    for expression in [
        include_str!("fixtures/evaluator_source_b400fd9acba0d377.orna"),
        include_str!("fixtures/evaluator_source_b0decf233432a63b.orna"),
        include_str!("fixtures/evaluator_source_4963fa6bc26f7aac.orna"),
        include_str!("fixtures/evaluator_source_d19d0923b599c0e0.orna"),
        include_str!("fixtures/evaluator_source_d9fc4ace3f1292d8.orna"),
    ] {
        assert_eq!(
            code(call_module(source, expression, Limits::default())),
            "ORNA-EVAL-ARGUMENT",
            "{expression}"
        );
    }
}

#[test]
fn source_calls_are_lexical_and_respect_value_shadowing() {
    let source = include_str!("fixtures/source_calls_lexical.orna");
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_de073b4131c0ec19.orna"),
            Limits::default()
        )),
        "ORNA-EVAL-NAME"
    );
    let source = include_str!("fixtures/source_calls_identity.orna");
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_26a3b30ad90dc771.orna"),
            Limits::default()
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn source_call_arguments_evaluate_in_source_order() {
    let source = include_str!("fixtures/source_calls_order.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_2f617360b224b133.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::int(21.into())
    );
}

#[test]
fn wildcard_and_structured_lambda_parameters_bind_by_position() {
    for (expression, expected) in [
        (
            include_str!("fixtures/wildcard_structured_lambda/wildcard-single-argument.orna")
                .trim(),
            7,
        ),
        (
            include_str!("fixtures/wildcard_structured_lambda/wildcard-two-arguments.orna").trim(),
            7,
        ),
        (
            include_str!("fixtures/wildcard_structured_lambda/wildcard-pipeline.orna").trim(),
            7,
        ),
        (
            include_str!("fixtures/wildcard_structured_lambda/tuple-destructure-call.orna").trim(),
            3,
        ),
        (
            include_str!("fixtures/wildcard_structured_lambda/array-destructure-call.orna").trim(),
            3,
        ),
        (
            include_str!("fixtures/wildcard_structured_lambda/record-destructure-call.orna").trim(),
            3,
        ),
        (
            include_str!("fixtures/wildcard_structured_lambda/tuple-destructure-pipeline.orna")
                .trim(),
            3,
        ),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::int(expected.into()),
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/wildcard_structured_lambda/wildcard-divide-by-zero.orna").trim(),
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    for expression in [
        include_str!("fixtures/wildcard_structured_lambda/tuple-destructure-wrong-arity.orna")
            .trim(),
        include_str!("fixtures/wildcard_structured_lambda/array-destructure-wrong-arity.orna")
            .trim(),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-TYPE"
        );
    }
    for expression in [
        include_str!("fixtures/wildcard_structured_lambda/wildcard-wrong-arity.orna").trim(),
        include_str!("fixtures/wildcard_structured_lambda/tuple-destructure-named-arguments.orna")
            .trim(),
        include_str!(
            "fixtures/wildcard_structured_lambda/record-destructure-duplicate-binding.orna"
        )
        .trim(),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-ARGUMENT"
        );
    }
}

#[test]
fn named_functions_share_structured_parameter_binding_and_defaults() {
    let source = include_str!("fixtures/structured_function_parameters.orna");
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_b400fd9acba0d377.orna"),
            3,
        ),
        (
            include_str!("fixtures/evaluator_source_758d0a174816854a.orna"),
            30,
        ),
        (
            include_str!("fixtures/evaluator_source_d99ff0bdf7f314ce.orna"),
            30,
        ),
        (
            include_str!("fixtures/evaluator_source_c70bdb6eeeb63d33.orna"),
            7,
        ),
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(expected.into())
        );
    }
}

#[test]
fn closures_capture_immutable_snapshots_and_support_nested_calls() {
    let source = include_str!("fixtures/closures.orna");
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_c0598d2d3a1d871b.orna"),
            15,
        ),
        (
            include_str!("fixtures/evaluator_source_cfb38676d13892bf.orna"),
            1,
        ),
        (
            include_str!("fixtures/evaluator_source_4c50533cd41cd46c.orna"),
            11,
        ),
        (
            include_str!("fixtures/evaluator_source_0e55ed56079d1723.orna"),
            6,
        ),
        (
            include_str!("fixtures/evaluator_source_b9fae42520af2a33.orna"),
            11,
        ),
        (
            include_str!("fixtures/evaluator_source_b7616ebb36378f81.orna"),
            11,
        ),
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(expected.into())
        );
    }
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_78d0929c1d040ba0.orna"),
            Limits::default()
        )),
        "ORNA-EVAL-IMMUTABLE-CAPTURE"
    );
}

#[test]
fn function_values_pass_through_locals_arguments_and_collections() {
    let source = include_str!("fixtures/function_values.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::int(41.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_685bc38c4f17eb80.orna"
        )),
        Value::int(2.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_19cfb859212309a9.orna"
        )),
        Value::int(3.into())
    );
}

#[test]
fn anonymous_pipeline_stages_share_callable_binding_and_limits() {
    assert_eq!(
        evaluate(include_str!("fixtures/anonymous_pipeline_stages/direct_call.orna").trim()),
        Value::int(12.into())
    );
    assert_eq!(
        evaluate(include_str!("fixtures/anonymous_pipeline_stages/chained_stages.orna").trim()),
        Value::int(24.into())
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/anonymous_pipeline_stages/step_limit.orna").trim(),
            &Environment::new(),
            Limits {
                max_steps: 2,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
    for expression in [
        include_str!("fixtures/anonymous_pipeline_stages/too_many_arguments.orna").trim(),
        include_str!("fixtures/anonymous_pipeline_stages/zero_parameter_argument.orna").trim(),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-ARGUMENT"
        );
    }
    for expression in [
        include_str!("fixtures/anonymous_pipeline_stages/function_value.orna").trim(),
        include_str!("fixtures/anonymous_pipeline_stages/function_array.orna").trim(),
        include_str!("fixtures/anonymous_pipeline_stages/function_record.orna").trim(),
        include_str!("fixtures/anonymous_pipeline_stages/function_equality.orna").trim(),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }
}

#[test]
fn pipelines_insert_the_input_before_explicit_arguments_and_defaults() {
    let source = include_str!("fixtures/pipelines.orna");
    for expression in [
        include_str!("fixtures/evaluator_source_fd648c04c11072ae.orna"),
        include_str!("fixtures/evaluator_source_9082051926ca0bec.orna"),
        include_str!("fixtures/evaluator_source_4e7943f782f24c54.orna"),
        include_str!("fixtures/evaluator_source_ad239d05428395cc.orna"),
        include_str!("fixtures/evaluator_source_ddb5f09c4cbde42e.orna"),
    ] {
        assert_eq!(
            call_module(source, expression, Limits::default()).unwrap(),
            Value::int(16.into()),
            "{expression}"
        );
    }
    for expression in [
        include_str!("fixtures/evaluator_source_a1d63d16b00948ea.orna"),
        include_str!("fixtures/evaluator_source_265b00cf2432573a.orna"),
        include_str!("fixtures/evaluator_source_e112a82ebac1e312.orna"),
    ] {
        assert_eq!(
            code(call_module(source, expression, Limits::default())),
            "ORNA-EVAL-ARGUMENT",
            "{expression}"
        );
    }
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_db44d791471583f2.orna"),
            Limits::default()
        )),
        "ORNA-EVAL-ARGUMENT"
    );
}

#[test]
fn pipeline_input_runs_once_and_before_stage_arguments() {
    let source = include_str!("fixtures/pipelines.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_a4be610b944962ef.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::int(12.into())
    );
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_9a8c27fc51333935.orna"),
            Limits::default()
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_09585cab38f58f17.orna"),
            Limits {
                max_steps: 12,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn math_pipelines_and_mixed_named_calls_share_argument_positions() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_422c602ef0806cdb.orna"),
            42,
        ),
        (
            include_str!("fixtures/evaluator_source_885a07a88dfe8f80.orna"),
            42,
        ),
        (
            include_str!("fixtures/evaluator_source_48a9d5a263c215c6.orna"),
            7,
        ),
        (
            include_str!("fixtures/evaluator_source_ea0e9cd5bc48ff80.orna"),
            7,
        ),
        (
            include_str!("fixtures/evaluator_source_a66a2618d2b4021a.orna"),
            5,
        ),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::int(expected.into()),
            "{expression}"
        );
    }
    for expression in [
        include_str!("fixtures/evaluator_source_f92ebbd6f609538f.orna"),
        include_str!("fixtures/evaluator_source_ffe6483d34803114.orna"),
        include_str!("fixtures/evaluator_source_02f85be894cc2a37.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }
}

#[test]
fn std_bits_preserves_unbounded_signed_twos_complement_semantics() {
    assert_eq!(
        evaluate(include_str!("fixtures/evaluator_source_46ee2ded56c3f4e7.orna")),
        Value::int(3.into())
    );
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_525122f16c2e379f.orna"),
            14,
        ),
        (
            include_str!("fixtures/evaluator_source_8f93aa349b9d84ab.orna"),
            2,
        ),
        (
            include_str!("fixtures/evaluator_source_4e94aec7b783105d.orna"),
            12,
        ),
        (
            include_str!("fixtures/evaluator_source_dd4462a246d0b409.orna"),
            -1,
        ),
        (
            include_str!("fixtures/evaluator_source_b8a04b95ca2267bd.orna"),
            0,
        ),
        (
            include_str!("fixtures/evaluator_source_2dcfbebe7af0187f.orna"),
            1_i128 << 80,
        ),
        (
            include_str!("fixtures/evaluator_source_aa719e233adb155f.orna"),
            -3,
        ),
        (
            include_str!("fixtures/evaluator_source_10c741a4f045874a.orna"),
            0,
        ),
        (
            include_str!("fixtures/evaluator_source_56ce50733810a0bd.orna"),
            -1,
        ),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::int(expected.into()),
            "{expression}"
        );
    }
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_46d89922f621024b.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_377c2a174befceaf.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_3d0d101431dc3aa1.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_8367e9dcfc6c3d3c.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_ac55dad4bba9ad9c.orna"),
            "ORNA-EVAL-LIMIT",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn std_text_fallback_preserves_unicode_scalar_and_field_semantics() {
    let fields = Value::new(Raw::Array(vec![
        Raw::Text("alpha".into()),
        Raw::Text("".into()),
        Raw::Text("β".into()),
        Raw::Text("".into()),
    ]))
    .unwrap();
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_2e5b587c6e0bf1cc.orna"),
            Value::new(Raw::Text("café".into())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_35ceb6112451a000.orna"),
            fields,
        ),
        (
            include_str!("fixtures/evaluator_source_692ad3018e762104.orna"),
            Value::new(Raw::Array(vec![
                Raw::Text("a".into()),
                Raw::Text("β".into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_8552b05fc3b6797d.orna"),
            Value::new(Raw::Text("a:β:".into())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_4f5061f25d0d0ac2.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_342d3dacd0b38e72.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_db29611950c345aa.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_44549c78b636d31e.orna"),
            Value::new(Raw::Text("bb".into())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_2d213433a8546a30.orna"),
            Value::new(Raw::Text("Café".into())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_3a6a05eb210818fc.orna"),
            Value::new(Raw::Text("Cafe\u{301}".into())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_a9a9a6831ee0e3f9.orna"),
            Value::new(Raw::Text("i̇ς".into())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_7ce7c77b079a21d6.orna"),
            Value::new(Raw::Text("SS".into())).unwrap(),
        ),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_text_fallback_rejects_invalid_arguments_and_enforces_existing_limits() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_eb886627f3b15b5b.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_7ec2d6f988af2ed6.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_e710b25432589584.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_0a171a3a5635d6c3.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_82473c16f3a83ef9.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_ab5594fbd82d3ca2.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_bae813212315fafb.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_3efcb9a07d3bf9dd.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_85f1751d57b1765c.orna"),
            "ORNA-EVAL-VALUE",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_12e5295d36a293ee.orna"),
            &Environment::new(),
            Limits {
                max_string_bytes: 7,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_298fe829a138f6a3.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_text_split_and_join_charge_step_budget_without_changing_outputs() {
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_692ad3018e762104.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Text("a".into()),
            Raw::Text("β".into()),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_692ad3018e762104.orna"),
            &Environment::new(),
            Limits {
                max_steps: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_35ceb6112451a000.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Text("alpha".into()),
            Raw::Text("".into()),
            Raw::Text("β".into()),
            Raw::Text("".into()),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_35ceb6112451a000.orna"),
            &Environment::new(),
            Limits {
                max_steps: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_8552b05fc3b6797d.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("a:β:".into())).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_8552b05fc3b6797d.orna"),
            &Environment::new(),
            Limits {
                max_steps: 6,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_fallback_preserves_finite_list_ordering() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_1da7cc611b76a427.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(3.into()), Raw::Int(4.into())]),
                Raw::Array(vec![Raw::Int(5.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_29da9ba0fa022bbe.orna"),
            Value::new(Raw::Array(vec![])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_473df9177d8c6620.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_bb074f5ce7aa1a36.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(2.into()),
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(3.into())]),
                Raw::Array(vec![]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_38cb974bc808c771.orna"),
            Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(1.into())])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_3b0d2c957f6a09a6.orna"),
            Value::new(Raw::Array(vec![
                Value::decimal(1.into(), 0.into()).unwrap().raw().clone(),
                Value::decimal(2.into(), 0.into()).unwrap().raw().clone(),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_e6fbcdd13b1df03c.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(2.into()),
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(3.into())]),
                Raw::Array(vec![]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_18c4d173e48642d3.orna"),
            Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(1.into())])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_1747c9635729f75e.orna"),
            Value::new(Raw::Array(vec![
                Value::decimal(1.into(), 0.into()).unwrap().raw().clone(),
                Value::decimal(2.into(), 0.into()).unwrap().raw().clone(),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_96e313a524b19b8b.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_8ccb4737d8a770d2.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_4377a1fc8627a5b3.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_b11204c3745b0c6b.orna"),
            Value::int(3.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_79a3255fb556fb1c.orna"),
            Value::int(3.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_0b32468e1f6c81bb.orna"),
            Value::int(4.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_5aa8b8b7d2054b34.orna"),
            Value::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_5c763104fa1b6e4d.orna"),
            Value::new(Raw::Array(vec![])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_2551f3c29e2ca7ec.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_cfde0e3812c2207f.orna"),
            Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_0799835ee0699292.orna"),
            Value::new(Raw::Array(vec![Raw::Int(3.into())])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_289e024fc4750126.orna"),
            Value::new(Raw::Array(vec![])).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_e952ff55c7ba1f89.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_548331ad6b1db9ee.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
                Raw::Array(vec![Raw::Int(1.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_8f029c7e1408a962.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
                Raw::Array(vec![Raw::Int(1.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_02cdd36defc1ecc6.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
                Raw::Array(vec![Raw::Int(4.into()), Raw::Int(5.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_1adb305c2d0dcf5e.orna"),
            Value::new(Raw::Array(vec![Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ])]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_fb3a8062b215e5d9.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_e6dbd68a5c438388.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Array(vec![Raw::Int(31.into())]),
                ]),
                Raw::Array(vec![
                    Raw::Int(2.into()),
                    Raw::Array(vec![Raw::Int(12.into()), Raw::Int(22.into())]),
                ]),
                Raw::Array(vec![
                    Raw::Int(3.into()),
                    Raw::Array(vec![Raw::Int(13.into()), Raw::Int(33.into())]),
                ]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_3b27fcd5cbfa74f8.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(0.into()),
                    Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
                ]),
                Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())]),
                ]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_45deb8bac9e61cb1.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Text("b".into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_ee15980ee5277d75.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Text("b".into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_c6e97c62fc248d86.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_311fc8f110eeffc8.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Int(2.into()),
                    Raw::Int(3.into()),
                ]),
                Raw::Array(vec![
                    Raw::Int(3.into()),
                    Raw::Int(4.into()),
                    Raw::Int(5.into()),
                ]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_473a00182e524d9c.orna"),
            Value::new(Raw::Array(vec![Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ])]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_39e52c1c05eb7b2e.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_collection_structural_operations_charge_step_budget_without_changing_finite_outputs() {
    let environment = Environment::from([
        (
            "rows".into(),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
                Raw::Int(4.into()),
            ]))
            .unwrap(),
        ),
        (
            "nested".into(),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![]),
                Raw::Array(vec![Raw::Int(3.into())]),
            ]))
            .unwrap(),
        ),
        (
            "left".into(),
            Value::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())])).unwrap(),
        ),
        (
            "right".into(),
            Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())])).unwrap(),
        ),
    ]);
    let cases = [
        (
            include_str!("fixtures/evaluator_source_a09ad458ef217069.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(3.into()), Raw::Int(4.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_1098c20c2f7e7599.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_f8a5b9cb5cf89552.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
                Raw::Array(vec![Raw::Int(3.into()), Raw::Int(4.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_9d379cb7abdad559.orna"),
            Value::new(Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
                Raw::Array(vec![Raw::Int(3.into()), Raw::Int(4.into())]),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_36062799d8380230.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
                Raw::Int(4.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_b1cd8e80ace16f6b.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
                Raw::Int(4.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_fea550c600fd9b6f.orna"),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_1220c59d913e5a8b.orna"),
            Value::int(4.into()),
        ),
    ];
    for (expression, expected) in cases {
        assert_eq!(
            evaluate_expression(expression, &environment, Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
        assert_eq!(
            code(evaluate_expression(
                expression,
                &environment,
                Limits {
                    max_steps: 2,
                    ..Limits::default()
                },
            )),
            "ORNA-EVAL-LIMIT",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_take_drop_and_zip_charge_steps_for_copied_results() {
    let environment = Environment::from([
        (
            "rows".into(),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            "left".into(),
            Value::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(2.into()),
                Raw::Int(3.into()),
            ]))
            .unwrap(),
        ),
        (
            "right".into(),
            Value::new(Raw::Array(vec![
                Raw::Text("a".into()),
                Raw::Text("b".into()),
            ]))
            .unwrap(),
        ),
        (
            "equal_right".into(),
            Value::new(Raw::Array(vec![
                Raw::Text("a".into()),
                Raw::Text("b".into()),
                Raw::Text("c".into()),
            ]))
            .unwrap(),
        ),
    ]);

    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_bfaa6e5fc9aaa7ba.orna"),
            &environment,
            Limits {
                max_steps: 4,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(1.into())])).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_bfaa6e5fc9aaa7ba.orna"),
            &environment,
            Limits {
                max_steps: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_ea3ca9ceed50e07b.orna"),
            &environment,
            Limits {
                max_steps: 4,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(3.into())])).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_ea3ca9ceed50e07b.orna"),
            &environment,
            Limits {
                max_steps: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    let short_zip = Value::new(Raw::Array(vec![
        Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())]),
        Raw::Array(vec![Raw::Int(2.into()), Raw::Text("b".into())]),
    ]))
    .unwrap();
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_b9b15ebd72753f25.orna"),
            &environment,
            Limits {
                max_steps: 5,
                ..Limits::default()
            },
        )
        .unwrap(),
        short_zip
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_b9b15ebd72753f25.orna"),
            &environment,
            Limits {
                max_steps: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    let exact_zip = Value::new(Raw::Array(vec![
        Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())]),
        Raw::Array(vec![Raw::Int(2.into()), Raw::Text("b".into())]),
        Raw::Array(vec![Raw::Int(3.into()), Raw::Text("c".into())]),
    ]))
    .unwrap();
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_4469ac9e0962f9f9.orna"),
            &environment,
            Limits {
                max_steps: 6,
                ..Limits::default()
            },
        )
        .unwrap(),
        exact_zip
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_4469ac9e0962f9f9.orna"),
            &environment,
            Limits {
                max_steps: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_810fb4741568d06b.orna"),
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn std_collection_first_returns_only_the_head_of_a_finite_list() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_17712d6ff995e8f8.orna"),
            Value::option(Some(Value::int(0.into()))).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_07a8406e20b1d251.orna"),
            Value::option(Some(Value::int(1.into()))).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_2d2b18fe280307a5.orna"),
            Value::new(Raw::Null).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_740be6f753e31180.orna"),
            Value::option(Some(Value::int(3.into()))).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_8ec13e149b873c82.orna"),
            Value::option(Some(Value::int(4.into()))).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_c7d672edd6d3968a.orna"),
            Value::option(Some(Value::int(7.into()))).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_fc1ead4f7d1e57c2.orna"),
            Value::option(Some(Value::int(8.into()))).unwrap(),
        ),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_0a92efd950865e70.orna"),
            include_str!("fixtures/evaluator_source_f0cd89f40dca9771.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(Value::int(10.into()))).unwrap()
    );
}

#[test]
fn std_collection_first_is_callback_free_and_bounded() {
    assert_eq!(
        evaluate(include_str!("fixtures/evaluator_source_8149266b97333702.orna")),
        Value::option(Some(Value::int(1.into()))).unwrap()
    );
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_d06f72238e332b7f.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_ef740ac1161a03c9.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_9008cedb3febd226.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_38c3021f57aaad23.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_3cf318fba35aff23.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::option(Some(Value::int(13.into()))).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_3cf318fba35aff23.orna"),
            &Environment::new(),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}
#[test]
fn std_collection_last_returns_option_for_direct_pipeline_named_and_function_calls() {
    let expected = Value::option(Some(Value::int(3.into()))).expect("option is canonical");
    for expression in [
        include_str!("fixtures/evaluator_source_e6f17b2f65609800.orna"),
        include_str!("fixtures/evaluator_source_00cb7a5c01611a59.orna"),
        include_str!("fixtures/evaluator_source_a15ba8d41db1927e.orna"),
        include_str!("fixtures/evaluator_source_ab2b2051b9830d02.orna"),
        include_str!("fixtures/evaluator_source_9d486a2736455d8e.orna"),
        include_str!("fixtures/evaluator_source_91b81d10da018815.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_58ce67545aaa8127.orna"),
            include_str!("fixtures/evaluator_source_14002e9b0dc65fff.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_last_returns_option_none_for_empty_finite_inputs() {
    let expected = Value::option(None).expect("option is canonical");
    for expression in [
        include_str!("fixtures/evaluator_source_21ffa7509a45f612.orna"),
        include_str!("fixtures/evaluator_source_7d53e9a0c8b2c849.orna"),
        include_str!("fixtures/evaluator_source_ab0ba67183c0f3c0.orna"),
        include_str!("fixtures/evaluator_source_06eb587796948df1.orna"),
        include_str!("fixtures/evaluator_source_79af027b63586629.orna"),
        include_str!("fixtures/evaluator_source_4cf7a0e46e0a0515.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn std_collection_last_is_callback_free_and_respects_collection_bounds() {
    assert_eq!(
        evaluate(include_str!("fixtures/evaluator_source_895445bd33a0cfd0.orna")),
        Value::option(Some(Value::int(1.into()))).unwrap()
    );
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_8f9ff585c3397bd3.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_621d44d09cd5af25.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_d78ee9d2ec4a46f1.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_39deb2b100b1fa93.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_d615b7dc3024e98e.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::option(Some(Value::int(13.into()))).expect("option is canonical")
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_020450168a94c294.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn root_last_dispatch_respects_local_function_and_relation_shadowing() {
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_5787c6fc19e07659.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_d6625d5301b77178.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_fb050fdefa193785.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn std_collection_sum_accepts_direct_named_pipeline_and_function_calls() {
    let expected = Value::int(6.into());
    for expression in [
        include_str!("fixtures/evaluator_source_480d2447907674da.orna"),
        include_str!("fixtures/evaluator_source_2b3be3a59910290c.orna"),
        include_str!("fixtures/evaluator_source_ee34ba9ea91794f2.orna"),
        include_str!("fixtures/evaluator_source_69ee8cf60004a033.orna"),
        include_str!("fixtures/evaluator_source_c83df2542a8438e4.orna"),
        include_str!("fixtures/evaluator_source_8be0606efba1643e.orna"),
        include_str!("fixtures/evaluator_source_e453f0ce16457161.orna"),
        include_str!("fixtures/evaluator_source_01669bdc72a7a7ff.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_d7df2acd9740a695.orna"),
            include_str!("fixtures/evaluator_source_762fdc7d9f416fbb.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_sum_accumulates_exactly_in_order_and_returns_integer_zero() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_fa17ca6e5f2d47be.orna"
        )),
        Value::int(0.into()),
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_10e79c406edbe43d.orna"
        )),
        Value::int(9007199254740994_u64.into()),
    );

    let limited = Limits {
        max_integer_digits: 3,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_2a99d49049a80b2d.orna"),
            &Environment::new(),
            limited,
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_d53f00bd8ae1668b.orna"),
            &Environment::new(),
            limited,
        )
        .unwrap(),
        Value::int(0.into())
    );
}
#[test]
fn std_collection_sum_debits_one_step_per_scanned_typed_value() {
    let currency = [0x47; 16];
    let cases = vec![
        (
            "Int",
            Value::new(Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())])).unwrap(),
            Value::int(4.into()),
        ),
        (
            "Decimal",
            Value::new(Raw::Array(vec![
                Value::decimal(120.into(), (-2).into())
                    .unwrap()
                    .raw()
                    .clone(),
                Value::decimal(2003.into(), (-3).into())
                    .unwrap()
                    .raw()
                    .clone(),
            ]))
            .unwrap(),
            Value::decimal(3203.into(), (-3).into()).unwrap(),
        ),
        (
            "Money",
            money_rows(vec![
                money_raw(1.into(), (-1).into(), currency),
                money_raw(2.into(), (-1).into(), currency),
            ]),
            money_value(3.into(), (-1).into(), currency),
        ),
        (
            "Float",
            float_rows(&[1.5f64.to_bits(), 2.25f64.to_bits()]),
            Value::float_bits(3.75f64.to_bits()),
        ),
    ];

    for (kind, rows, expected) in cases {
        let environment = Environment::from([("rows".into(), rows)]);
        assert_eq!(
            evaluate_expression(
                include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
                &environment,
                Limits::default()
            )
            .unwrap(),
            expected,
            "{kind} sum should preserve its exact typed result",
        );
        assert_eq!(
            code(evaluate_expression(
                include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
                &environment,
                Limits {
                    // Call setup and the rows binding consume two steps;
                    // scanning both values consumes the remaining two.
                    max_steps: 3,
                    ..Limits::default()
                },
            )),
            "ORNA-EVAL-LIMIT",
            "{kind} sum must charge every scanned value",
        );
        assert_eq!(
            evaluate_expression(
                include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
                &environment,
                Limits {
                    // Exactly enough for setup plus one debit per scanned value.
                    max_steps: 4,
                    ..Limits::default()
                },
            )
            .unwrap(),
            expected,
            "{kind} sum must not double-charge scanned values",
        );
    }
}

#[test]
fn std_collection_money_addition_and_sum_preserve_exact_decimal_amounts() {
    let currency = [0x47; 16];
    let left = money_value(1.into(), (-1).into(), currency);
    let right = money_value(2.into(), (-1).into(), currency);
    let rows = money_rows(vec![left.raw().clone(), right.raw().clone()]);
    let environment = Environment::from([
        ("left".into(), left),
        ("right".into(), right),
        ("rows".into(), rows),
    ]);
    let expected = money_value(3.into(), (-1).into(), currency);

    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_ec38218c8ac89b84.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_26a0ab53a98abac7.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        money_value((-1).into(), (-1).into(), currency)
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_money_aggregates_preserve_currency_and_exact_extrema() {
    let currency = [0x47; 16];
    let values = vec![
        money_raw(12.into(), (-1).into(), currency),
        money_raw(2003.into(), (-3).into(), currency),
        money_raw((-5).into(), (-1).into(), currency),
    ];
    let environment = Environment::from([("rows".into(), money_rows(values))]);
    let expected_sum = money_value(2703.into(), (-3).into(), currency);
    let expected_min =
        Value::option(Some(money_value((-5).into(), (-1).into(), currency))).unwrap();
    let expected_max =
        Value::option(Some(money_value(2003.into(), (-3).into(), currency))).unwrap();

    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        expected_sum
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_767f07d17295b186.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        expected_min
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_522fd6a43762eaba.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        expected_max
    );
}

#[test]
fn canonical_money_round_trip_requires_nested_decimal_and_currency_witness() {
    let currency = [0x47; 16];
    let raw = money_raw(1234.into(), (-2).into(), currency);
    let environment = Environment::from([("money".into(), Value::new(raw.clone()).unwrap())]);

    let result = evaluate_expression(
        include_str!("fixtures/evaluator_source_8d2ac8b58ead9744.orna"),
        &environment,
        Limits::default(),
    )
    .expect("Money should decode");
    assert_eq!(result.raw(), &raw);
    assert!(matches!(
        result.raw(),
        Raw::Tag(60007, body)
            if matches!(
                body.as_ref(),
                Raw::Array(parts)
                    if matches!(
                        parts.as_slice(),
                        [Raw::Tag(60000, _), Raw::Tag(37, currency)]
                            if matches!(currency.as_ref(), Raw::Bytes(bytes) if bytes.len() == 16)
                    )
            )
    ));
}

#[test]
fn money_aggregates_fail_closed_for_cross_currency_float_mixing_and_limits() {
    let currency = [0x47; 16];
    let other_currency = [0x55; 16];
    let one = money_raw(1.into(), 0.into(), currency);
    let two = money_raw(2.into(), 0.into(), currency);
    let foreign = money_raw(3.into(), 0.into(), other_currency);
    let environment = Environment::from([
        (
            "cross_currency".into(),
            money_rows(vec![one.clone(), foreign]),
        ),
        (
            "float_mixed".into(),
            money_rows(vec![two, Raw::Float(1.5f64.to_bits())]),
        ),
    ]);

    for expression in [
        include_str!("fixtures/evaluator_source_a94e83638f51b549.orna"),
        include_str!("fixtures/evaluator_source_257e8a99d279a338.orna"),
        include_str!("fixtures/evaluator_source_f7d98b135c71c255.orna"),
        include_str!("fixtures/evaluator_source_7fd851d4bbf47a55.orna"),
        include_str!("fixtures/evaluator_source_ccf8478145719732.orna"),
        include_str!("fixtures/evaluator_source_28cde0e40ef4a061.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &environment,
                Limits::default()
            )),
            "ORNA-EVAL-UNSUPPORTED",
            "{expression}"
        );
    }

    let limited = Environment::from([(
        "rows".into(),
        money_rows(vec![
            money_raw(1.into(), 0.into(), currency),
            money_raw(2.into(), 0.into(), currency),
            money_raw(3.into(), 0.into(), currency),
        ]),
    )]);
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
            &limited,
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    let oversized = BigInt::parse_bytes(b"1234", 10).unwrap();
    let environment =
        Environment::from([("money".into(), money_value(oversized, 0.into(), currency))]);
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_8d2ac8b58ead9744.orna"),
            &environment,
            Limits {
                max_integer_digits: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn malformed_or_noncanonical_money_is_rejected_before_evaluation() {
    let currency = Raw::Tag(37, Box::new(Raw::Bytes(vec![0x47; 16])));
    let malformed = [
        Raw::Tag(
            60007,
            Box::new(Raw::Array(vec![Raw::Int(1.into()), currency.clone()])),
        ),
        Raw::Tag(
            60007,
            Box::new(Raw::Array(vec![
                Raw::Tag(
                    60000,
                    Box::new(Raw::Array(vec![Raw::Int(10.into()), Raw::Int((-1).into())])),
                ),
                currency.clone(),
            ])),
        ),
        Raw::Tag(
            60007,
            Box::new(Raw::Array(vec![
                Raw::Tag(
                    60000,
                    Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())])),
                ),
                Raw::Tag(37, Box::new(Raw::Bytes(vec![0x47; 15]))),
            ])),
        ),
    ];
    for raw in malformed {
        assert!(
            Value::new(raw).is_err(),
            "malformed or noncanonical Money must fail at the canonical boundary"
        );
    }
}

#[test]
fn std_collection_decimal_sum_preserves_exact_canonical_arithmetic() {
    let large_coefficient = BigInt::parse_bytes(b"12345678901234567891", 10).unwrap();
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_3ed610a00dc59b58.orna"),
            Value::decimal(3203.into(), (-3).into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_6e1d42511211a848.orna"),
            Value::decimal(large_coefficient, 0.into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_6fb4dd14703389eb.orna"),
            Value::decimal(1.into(), (-2).into()).unwrap(),
        ),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_collection_decimal_sum_rejects_mixed_numeric_kinds() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_58184e07766453ca.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn std_collection_sum_rejects_unsupported_numeric_kinds_and_shapes() {
    assert_eq!(
        evaluate(include_str!("fixtures/evaluator_source_a92c58c6d0b15cfc.orna")),
        Value::int(1.into())
    );
    for expression in [
        include_str!("fixtures/evaluator_source_e1ba4fa7c3937d2c.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-UNSUPPORTED",
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_fb832f2380a2ebed.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_ed2b5b56e0258424.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );

    let currency = Raw::Tag(37, Box::new(Raw::Bytes(vec![0; 16])));
    let affine = Raw::Tag(
        60006,
        Box::new(Raw::Array(vec![Raw::Int(1.into()), currency.clone()])),
    );
    let environment = Environment::from([(
        "values".into(),
        Value::new(Raw::Array(vec![affine])).unwrap(),
    )]);
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_697cdad8ddab1535.orna"),
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn std_collection_float_sum_accepts_direct_named_pipeline_and_function_calls() {
    let expected = Value::float_bits(3.75f64.to_bits());
    for expression in [
        include_str!("fixtures/evaluator_source_8b9ebe3425360f46.orna"),
        include_str!("fixtures/evaluator_source_dfe063db5cfab4c2.orna"),
        include_str!("fixtures/evaluator_source_c8068c545b77d421.orna"),
        include_str!("fixtures/evaluator_source_3653934c46e340a0.orna"),
        include_str!("fixtures/evaluator_source_b4cb8fce95032665.orna"),
        include_str!("fixtures/evaluator_source_c465e99508638f6f.orna"),
        include_str!("fixtures/evaluator_source_4d468d08196d9d31.orna"),
        include_str!("fixtures/evaluator_source_5fc6b32fd4463b72.orna"),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }

    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_27bdf6a99114b900.orna"),
            include_str!("fixtures/evaluator_source_e8ebb1a987a2adc6.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_float_sum_folds_left_to_right_and_propagates_canonical_nan() {
    let rows = float_rows(&[
        10_000_000_000_000_000.0f64.to_bits(),
        (-10_000_000_000_000_000.0f64).to_bits(),
        1.0f64.to_bits(),
    ]);
    let environment = Environment::from([("rows".into(), rows)]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::float_bits(1.0f64.to_bits())
    );

    let nan_environment = Environment::from([(
        "rows".into(),
        float_rows(&[1.0f64.to_bits(), CANONICAL_NAN_BITS, 2.0f64.to_bits()]),
    )]);
    for expression in [
        include_str!("fixtures/evaluator_source_75f9858c5500f123.orna"),
        include_str!("fixtures/evaluator_source_d237f2138f4268bd.orna"),
        include_str!("fixtures/evaluator_source_f2fde970cadbfb14.orna"),
        include_str!("fixtures/evaluator_source_4351438b311f244a.orna"),
        include_str!("fixtures/evaluator_source_66f2c8cf631bd8c7.orna"),
        include_str!("fixtures/evaluator_source_9cb965cefb403adc.orna"),
        include_str!("fixtures/evaluator_source_1183d245d3868a6f.orna"),
        include_str!("fixtures/evaluator_source_f68e9c204598105c.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &nan_environment, Limits::default()).unwrap(),
            Value::float_bits(CANONICAL_NAN_BITS),
            "{expression}"
        );
    }
}

#[test]
fn std_collection_float_min_and_max_use_total_order_and_ordinary_equality_separately() {
    let negative_zero = Value::float_bits((-0.0f64).to_bits());
    let positive_zero = Value::float_bits(0.0f64.to_bits());
    let expected_min = Value::option(Some(negative_zero.clone())).unwrap();
    let expected_max = Value::option(Some(positive_zero.clone())).unwrap();
    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in min_max_source_fixtures(MinMaxSourceFixtureProfile::FloatZero, name) {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source = include_str!("fixtures/collection_min_max_float.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_ae3b31615534e05b.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(Value::float_bits((-3.0f64).to_bits()))).unwrap()
    );
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_acfa4eb4df6d8a37.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(Value::float_bits(2.0f64.to_bits()))).unwrap()
    );

    let environment = Environment::from([
        ("negative_zero".into(), negative_zero),
        ("positive_zero".into(), positive_zero),
        ("nan".into(), Value::float_bits(CANONICAL_NAN_BITS)),
    ]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_5e0b338a7be7031e.orna"),
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_0d5ca0adcf527f22.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
}

#[test]
fn std_collection_float_min_and_max_propagate_nan_and_return_empty_identity() {
    let environment = Environment::from([(
        "rows".into(),
        float_rows(&[(-1.0f64).to_bits(), CANONICAL_NAN_BITS, 2.0f64.to_bits()]),
    )]);
    let expected = Value::option(Some(Value::float_bits(CANONICAL_NAN_BITS))).unwrap();
    for (name, expected) in [("min", expected.clone()), ("max", expected)] {
        for expression in min_max_source_fixtures(MinMaxSourceFixtureProfile::Rows, name) {
            assert_eq!(
                evaluate_expression(&expression, &environment, Limits::default()).unwrap(),
                expected,
                "{expression}"
            );
        }
    }

    for expression in [
        include_str!("fixtures/evaluator_source_e4fbc5a7f8bcdff2.orna"),
        include_str!("fixtures/evaluator_source_8adb66ef790ded9a.orna"),
        include_str!("fixtures/evaluator_source_00f5a9ee37c3ba7b.orna"),
        include_str!("fixtures/evaluator_source_360365f29248d539.orna"),
        include_str!("fixtures/evaluator_source_1c0996fbd85dc748.orna"),
        include_str!("fixtures/evaluator_source_8a31e61da5d3c73b.orna"),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::new(Raw::Null).unwrap(),
            "{expression}"
        );
    }
}

#[test]
fn std_collection_float_aggregates_reject_mixed_inputs_callbacks_and_limits() {
    for expression in [
        include_str!("fixtures/evaluator_source_cdab6ecfce425b19.orna"),
        include_str!("fixtures/evaluator_source_fe5013414b733c91.orna"),
        include_str!("fixtures/evaluator_source_21e98e42a38aa4ca.orna"),
        include_str!("fixtures/evaluator_source_f9bcdc1230a9faa4.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-UNSUPPORTED",
            "{expression}"
        );
    }
    for expression in [
        include_str!("fixtures/evaluator_source_672569f1e425be8a.orna"),
        include_str!("fixtures/evaluator_source_4636ea68fc901968.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_4513747d05260e45.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );

    for (expression, limits) in [
        (
            include_str!("fixtures/evaluator_source_db6064c68b0cfbd9.orna"),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        ),
        (
            include_str!("fixtures/evaluator_source_582a71e211744730.orna"),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(expression, &Environment::new(), limits)),
            "ORNA-EVAL-LIMIT",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_date_min_and_max_accept_all_call_forms_and_preserve_order() {
    let expected_min = Value::option(Some(evaluate(include_str!(
        "fixtures/evaluator_source_41b62fb4518505d3.orna"
    ))))
    .unwrap();
    let expected_max = Value::option(Some(evaluate(include_str!(
        "fixtures/evaluator_source_dd82365333c66bc5.orna"
    ))))
    .unwrap();
    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in min_max_source_fixtures(MinMaxSourceFixtureProfile::Dates, name) {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source = include_str!("fixtures/collection_min_max_date.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_de5babb1333d7094.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(evaluate(include_str!(
            "fixtures/evaluator_source_cc9ba4ec6973cfea.orna"
        ))))
        .unwrap()
    );
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_eda774d78648c4c1.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(evaluate(include_str!(
            "fixtures/evaluator_source_498af307718168a4.orna"
        ))))
        .unwrap()
    );
}

#[test]
fn std_collection_instant_min_and_max_use_normalized_utc_order() {
    let expected_min = Value::option(Some(evaluate(include_str!(
        "fixtures/evaluator_source_48b712c92518a91e.orna"
    ))))
    .unwrap();
    let expected_max = Value::option(Some(evaluate(include_str!(
        "fixtures/evaluator_source_f6bc804093ad8575.orna"
    ))))
    .unwrap();
    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in min_max_source_fixtures(MinMaxSourceFixtureProfile::Instants, name) {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source = include_str!("fixtures/collection_min_max_instant.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_07c8ffa3b40113f6.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(evaluate(include_str!(
            "fixtures/evaluator_source_d239cae45de9eb86.orna"
        ))))
        .unwrap()
    );
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_455f710e6ea42403.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(evaluate(include_str!(
            "fixtures/evaluator_source_d239cae45de9eb86.orna"
        ))))
        .unwrap()
    );
}

#[test]
fn std_collection_temporal_min_and_max_keep_empty_null_and_reject_mixed_types() {
    for expression in [
        include_str!("fixtures/evaluator_source_e4fbc5a7f8bcdff2.orna"),
        include_str!("fixtures/evaluator_source_8adb66ef790ded9a.orna"),
        include_str!("fixtures/evaluator_source_00f5a9ee37c3ba7b.orna"),
        include_str!("fixtures/evaluator_source_360365f29248d539.orna"),
        include_str!("fixtures/evaluator_source_1c0996fbd85dc748.orna"),
        include_str!("fixtures/evaluator_source_8a31e61da5d3c73b.orna"),
    ] {
        assert_eq!(
            evaluate(expression),
            Value::new(Raw::Null).unwrap(),
            "{expression}"
        );
    }

    for expression in [
        include_str!("fixtures/evaluator_source_9258e0be3437ee98.orna"),
        include_str!("fixtures/evaluator_source_3931e25afab11f97.orna"),
        include_str!("fixtures/evaluator_source_f161492124b657b2.orna"),
        include_str!("fixtures/evaluator_source_9801f2909c9f459a.orna"),
        include_str!("fixtures/evaluator_source_b1c5771e7b53d151.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-TYPE",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_min_and_max_accept_integer_lists_in_all_call_forms() {
    let expected_min = Value::option(Some(Value::int((-9007199254740993_i64).into()))).unwrap();
    let expected_max = Value::option(Some(Value::int(9007199254740993_i64.into()))).unwrap();

    for (name, expected) in [("min", expected_min), ("max", expected_max)] {
        for expression in min_max_source_fixtures(MinMaxSourceFixtureProfile::Integers, name) {
            assert_eq!(evaluate(&expression), expected, "{expression}");
        }
    }

    let source = include_str!("fixtures/collection_min_max_int.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_4491b470e7f0b243.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(Value::int(1.into()))).unwrap()
    );
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_8005c4858ae7d4f2.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::option(Some(Value::int(3.into()))).unwrap()
    );
}

#[test]
fn std_collection_min_and_max_use_exact_integer_order_and_first_ties() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_189c89b09adbc00b.orna"
        )),
        Value::option(Some(Value::int(1.into()))).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_6488237a3e13f65c.orna"
        )),
        Value::option(Some(Value::int(9.into()))).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_e4fbc5a7f8bcdff2.orna"
        )),
        Value::new(Raw::Null).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_8adb66ef790ded9a.orna"
        )),
        Value::new(Raw::Null).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_1c0996fbd85dc748.orna"
        )),
        Value::new(Raw::Null).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_360365f29248d539.orna"
        )),
        Value::new(Raw::Null).unwrap()
    );
}

#[test]
fn std_collection_min_and_max_optional_results_match_some_null_and_coalesce() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_3535dca0a571e57d.orna"),
            Value::int(1.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_9eb41229f2527422.orna"),
            Value::int(3.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_e22a8c11b26adade.orna"),
            Value::int(10.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_5f4a2a14c43f665d.orna"),
            Value::int(10.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_b36287c26c0e33f2.orna"),
            Value::int(1.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_4db0508633b2c0c3.orna"),
            Value::int(3.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_a32ffd2f48dbda78.orna"),
            Value::int(10.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_f64d961b687ebcfa.orna"),
            Value::int(10.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_b68868e1331b5b05.orna"),
            Value::int(1.into()),
        ),
        (
            include_str!("fixtures/evaluator_source_3e2a232c1babf59d.orna"),
            Value::int(3.into()),
        ),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }
}

#[test]
fn std_collection_min_and_max_fail_closed_for_unsupported_kinds_shapes_and_limits() {
    assert_eq!(
        evaluate(include_str!("fixtures/evaluator_source_78bff71074810743.orna")),
        Value::option(Some(Value::int(1.into()))).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_2dca5c2db4197987.orna"
        )),
        Value::option(Some(Value::decimal(1.into(), 0.into()).unwrap())).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_1d442aae3b2f35d3.orna"
        )),
        Value::option(Some(Value::decimal(1.into(), 0.into()).unwrap())).unwrap()
    );
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_525c395e4f9e73ad.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_fe4ea82d41fb542a.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_a5009cbc7594b656.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_331bc5638c483c02.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_20e4478c67c97afe.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }

    let currency = Raw::Tag(37, Box::new(Raw::Bytes(vec![0; 16])));
    let affine = Raw::Tag(
        60006,
        Box::new(Raw::Array(vec![Raw::Int(1.into()), currency.clone()])),
    );
    let environment =
        Environment::from([("rows".into(), Value::new(Raw::Array(vec![affine])).unwrap())]);
    for operation in ["min", "max"] {
        assert_eq!(
            code(evaluate_expression(
                min_max_source_fixtures(MinMaxSourceFixtureProfile::Rows, operation)[0],
                &environment,
                Limits::default(),
            )),
            "ORNA-EVAL-UNSUPPORTED"
        );
    }

    for (expression, limits) in [
        (
            include_str!("fixtures/evaluator_source_ad86da454953469e.orna"),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        ),
        (
            include_str!("fixtures/evaluator_source_1a51744a13d58dcb.orna"),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        ),
        (
            include_str!("fixtures/evaluator_source_238c1b56550eb462.orna"),
            Limits {
                max_integer_digits: 3,
                ..Limits::default()
            },
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(expression, &Environment::new(), limits)),
            "ORNA-EVAL-LIMIT",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_min_and_max_debit_steps_for_each_scanned_value() {
    let currency = [0x47; 16];
    let cases = vec![
        (
            "Int",
            Value::new(Raw::Array(vec![
                Raw::Int(5.into()),
                Raw::Int(1.into()),
                Raw::Int(9.into()),
            ]))
            .unwrap(),
            Value::option(Some(Value::int(1.into()))).unwrap(),
            Value::option(Some(Value::int(9.into()))).unwrap(),
        ),
        (
            "Decimal",
            Value::new(Raw::Array(vec![
                Value::decimal(120.into(), (-2).into())
                    .unwrap()
                    .raw()
                    .clone(),
                Value::decimal(2003.into(), (-3).into())
                    .unwrap()
                    .raw()
                    .clone(),
                Value::decimal(50.into(), (-2).into())
                    .unwrap()
                    .raw()
                    .clone(),
            ]))
            .unwrap(),
            Value::option(Some(Value::decimal(50.into(), (-2).into()).unwrap())).unwrap(),
            Value::option(Some(Value::decimal(2003.into(), (-3).into()).unwrap())).unwrap(),
        ),
        (
            "Float",
            float_rows(&[3.0f64.to_bits(), (-1.0f64).to_bits(), 2.0f64.to_bits()]),
            Value::option(Some(Value::float_bits((-1.0f64).to_bits()))).unwrap(),
            Value::option(Some(Value::float_bits(3.0f64.to_bits()))).unwrap(),
        ),
        (
            "Date",
            evaluate(include_str!(
                "fixtures/evaluator_source_6eaf8336d3e43511.orna"
            )),
            Value::option(Some(evaluate(include_str!(
                "fixtures/evaluator_source_41b62fb4518505d3.orna"
            ))))
            .unwrap(),
            Value::option(Some(evaluate(include_str!(
                "fixtures/evaluator_source_dd82365333c66bc5.orna"
            ))))
            .unwrap(),
        ),
        (
            "Instant",
            evaluate(include_str!(
                "fixtures/evaluator_source_1a048668ff3dec54.orna"
            )),
            Value::option(Some(evaluate(include_str!(
                "fixtures/evaluator_source_48b712c92518a91e.orna"
            ))))
            .unwrap(),
            Value::option(Some(evaluate(include_str!(
                "fixtures/evaluator_source_f6bc804093ad8575.orna"
            ))))
            .unwrap(),
        ),
        (
            "Money",
            money_rows(vec![
                money_raw(12.into(), (-1).into(), currency),
                money_raw(2003.into(), (-3).into(), currency),
                money_raw((-5).into(), (-1).into(), currency),
            ]),
            Value::option(Some(money_value((-5).into(), (-1).into(), currency))).unwrap(),
            Value::option(Some(money_value(2003.into(), (-3).into(), currency))).unwrap(),
        ),
    ];

    for (kind, rows, expected_min, expected_max) in cases {
        let environment = Environment::from([("rows".into(), rows)]);
        for (name, expected) in [("min", expected_min), ("max", expected_max)] {
            let expression = min_max_source_fixtures(MinMaxSourceFixtureProfile::Rows, name)[0];
            assert_eq!(
                evaluate_expression(&expression, &environment, Limits::default()).unwrap(),
                expected,
                "{kind} {expression}"
            );
            assert_eq!(
                code(evaluate_expression(
                    &expression,
                    &environment,
                    Limits {
                        // The call and its rows binding consume the setup
                        // steps; scanning the first value must debit more.
                        max_steps: 2,
                        ..Limits::default()
                    }
                )),
                "ORNA-EVAL-LIMIT",
                "{kind} {expression}"
            );
        }
    }
}

#[test]
fn std_collection_one_accepts_predicate_free_direct_pipeline_named_and_function_calls() {
    let expected = Value::int(7.into());
    for expression in [
        include_str!("fixtures/evaluator_source_c39f2e51072b49ef.orna"),
        include_str!("fixtures/evaluator_source_8f626ee7b36527ce.orna"),
        include_str!("fixtures/evaluator_source_885fd52dd3567ae3.orna"),
        include_str!("fixtures/evaluator_source_4d37367cc8d22852.orna"),
        include_str!("fixtures/evaluator_source_7c7ff487191821e2.orna"),
        include_str!("fixtures/evaluator_source_5ded068288e45d8d.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_0b9b5f806dce52f6.orna"),
            include_str!("fixtures/evaluator_source_7043fb1efe341b29.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_one_accepts_predicate_overloads_in_input_order() {
    let expected = Value::int(2.into());
    for expression in [
        include_str!("fixtures/evaluator_source_b21b2e012186dd0b.orna"),
        include_str!("fixtures/evaluator_source_bde8240e51ca8c9a.orna"),
        include_str!("fixtures/evaluator_source_080a11b9386edb70.orna"),
        include_str!("fixtures/evaluator_source_2e383b7c67bc4f50.orna"),
        include_str!("fixtures/evaluator_source_ee58449e42e9f082.orna"),
        include_str!("fixtures/evaluator_source_0bb80ebf6fa63488.orna"),
        include_str!("fixtures/evaluator_source_42d7b12158836932.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_223844a9b355e4a8.orna"),
            include_str!("fixtures/evaluator_source_4e7b71ee74593ccd.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_40f25f17f1873001.orna"),
            include_str!("fixtures/evaluator_source_4e7b71ee74593ccd.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_one_distinguishes_zero_and_multiple_matches() {
    for expression in [
        include_str!("fixtures/evaluator_source_1386cbfe7c986256.orna"),
        include_str!("fixtures/evaluator_source_5f642767d6c21dc9.orna"),
        include_str!("fixtures/evaluator_source_ff52023ea0a7a181.orna"),
        include_str!("fixtures/evaluator_source_3274017ddf90a922.orna"),
        include_str!("fixtures/evaluator_source_8a500b81dc90d7de.orna"),
        include_str!("fixtures/evaluator_source_19fc177e21f2d3d7.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-RELATION-ONE-ZERO",
            "{expression}"
        );
    }
    for expression in [
        include_str!("fixtures/evaluator_source_d4b04fb179392d0d.orna"),
        include_str!("fixtures/evaluator_source_4e33dd900e29214a.orna"),
        include_str!("fixtures/evaluator_source_396d97222890595b.orna"),
        include_str!("fixtures/evaluator_source_3ba3bc0533551618.orna"),
        include_str!("fixtures/evaluator_source_ea9c82a6965cc3d4.orna"),
        include_str!("fixtures/evaluator_source_7386c54f0bda2cd2.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-RELATION-ONE-MULTIPLE",
            "{expression}"
        );
    }
}

#[test]
fn std_collection_one_evaluates_predicates_in_order_and_stops_after_second_match() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_e89ac240ee943553.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_9b0fda60e9527110.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_6ebc4169378d1d0f.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-RELATION-ONE-MULTIPLE"
    );
}

#[test]
fn std_collection_one_rejects_invalid_inputs_propagates_callback_failures_and_keeps_limits() {
    assert_eq!(
        evaluate(include_str!("fixtures/evaluator_source_4c6049668a352465.orna")),
        Value::int(1.into())
    );
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_1d1ee269ab24f5d6.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_d0b24f624ede6ddd.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_2ac6f95a450fbb76.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_a6f5d7b8c613881c.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_1129a313d3b5240a.orna"),
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            include_str!("fixtures/evaluator_source_3b12e77acdec5c03.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_cb483b969c37a673.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_5640621da2951f80.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_267b7ad4c5228024.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_e7709598c4df10dd.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_7a6238a33b332bec.orna"),
            "ORNA-EVAL-DIVIDE-BY-ZERO",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_8df4d22196b8c720.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::int(13.into())
    );
    for limits in [
        Limits {
            max_collection_items: 2,
            ..Limits::default()
        },
        Limits {
            max_steps: 1,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            code(evaluate_expression(
                include_str!("fixtures/evaluator_source_53388f558ca316b0.orna"),
                &Environment::new(),
                limits
            )),
            "ORNA-EVAL-LIMIT"
        );
    }
}

#[test]
fn std_collection_one_debits_one_step_per_scanned_predicate_value() {
    let expression = include_str!("fixtures/evaluator_source_23ef9999aa5f51f9.orna");
    assert_eq!(
        code(evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // Setup and the first callback fit; the second callback's
                // callback-local debit leaves too little budget to finish.
                max_steps: 12,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT",
        "one must debit each scanned predicate value before invoking its callback",
    );
    assert_eq!(
        evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // Setup plus two callback-local debits and both callback
                // bodies fit exactly at this boundary.
                max_steps: 13,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::int(1.into()),
        "one should succeed when exactly one debit is available per scanned value",
    );
}

#[test]
fn std_collection_every_and_exists_accept_all_call_forms_and_function_callbacks() {
    let true_value = Value::new(Raw::Bool(true)).unwrap();
    let false_value = Value::new(Raw::Bool(false)).unwrap();
    for expression in [
        include_str!("fixtures/evaluator_source_db8cca293b8a5975.orna"),
        include_str!("fixtures/evaluator_source_648c0ce86a31dc65.orna"),
        include_str!("fixtures/evaluator_source_1e55f13aee38196e.orna"),
        include_str!("fixtures/evaluator_source_3000004840cda745.orna"),
        include_str!("fixtures/evaluator_source_a8c809145afc3751.orna"),
        include_str!("fixtures/evaluator_source_ebb18e422cc395d5.orna"),
        include_str!("fixtures/evaluator_source_217b755f5445b938.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            true_value,
            "{expression}"
        );
    }
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_e3a666d313a86711.orna"),
            true_value.clone(),
        ),
        (
            include_str!("fixtures/evaluator_source_c5d81d1c011f2d44.orna"),
            true_value.clone(),
        ),
        (
            include_str!("fixtures/evaluator_source_915cef763c92cd02.orna"),
            true_value.clone(),
        ),
        (
            include_str!("fixtures/evaluator_source_06d1374122c805f7.orna"),
            true_value.clone(),
        ),
        (
            include_str!("fixtures/evaluator_source_705a0a1fe8acb7b1.orna"),
            true_value.clone(),
        ),
        (
            include_str!("fixtures/evaluator_source_e9047e6b4baef6b3.orna"),
            true_value.clone(),
        ),
        (
            include_str!("fixtures/evaluator_source_0ba3c5112c167531.orna"),
            false_value.clone(),
        ),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_12a3c3ac2d2512a6.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        false_value
    );

    let source = include_str!("fixtures/every_exists_named_callbacks.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_d989b35eb24afcfe.orna"),
            Limits::default()
        )
        .unwrap(),
        true_value
    );
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_663bf635c6c3c0ce.orna"),
            Limits::default()
        )
        .unwrap(),
        true_value
    );
}

#[test]
fn std_collection_every_and_exists_short_circuit_in_order_and_require_bool_callbacks() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_7effd445f9b4fc43.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_2f226b401055790c.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_9e6b83e645fcedbe.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_f3e5b76f1c5cb566.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_d2ffa16d7b565904.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_c787a2f5875bed9f.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_147844ef75f2aa78.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}
#[test]
fn std_collection_every_and_exists_empty_identity_and_first_match_short_circuit() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_217b755f5445b938.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_64e5563712f54441.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );

    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_67b9bff00dcbe444.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_2ca9d58da6e0d360.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn std_collection_every_and_exists_debit_each_scanned_predicate_value() {
    for (operation, limited_expression, expression, expected) in [
        (
            "every",
            include_str!("fixtures/evaluator_source_cdd53bd0f62469dc.orna"),
            include_str!("fixtures/evaluator_source_e6568fc5f5dece10.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            "exists",
            include_str!("fixtures/evaluator_source_c3ba5a75e3fab0e6.orna"),
            include_str!("fixtures/evaluator_source_617ca2d0c2dc93fc.orna"),
            Value::new(Raw::Bool(false)).unwrap(),
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                limited_expression,
                &Environment::new(),
                Limits {
                    max_steps: 7,
                    ..Limits::default()
                },
            )),
            "ORNA-EVAL-LIMIT",
            "{operation} must exhaust before invoking the second callback",
        );
        assert_eq!(
            evaluate_expression(
                expression,
                &Environment::new(),
                Limits {
                    max_steps: 10,
                    ..Limits::default()
                },
            )
            .unwrap(),
            expected,
            "{operation} must charge one step per scanned predicate value",
        );
    }
}

#[test]
fn root_every_and_exists_remain_shadowable_by_admitted_functions_and_locals() {
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_572adaf61fc695b1.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_6a5ee5d4e3d4901a.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
}

#[test]
fn admitted_function_named_fail_shadows_the_intrinsic() {
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_5346d0799bb60c73.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(2.into())
    );
}

#[test]
fn root_one_remains_shadowable_and_does_not_change_relation_member_behavior() {
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_f04ae0d9ccf249bb.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_3f6b1cba6cf45042.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_808b085ab198e887.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn root_first_dispatch_respects_local_function_and_relation_shadowing() {
    assert_eq!(
        call_module(
            include_str!("fixtures/evaluator_source_099ce81ac5ce9426.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_f00bc26c13d3ef2e.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_013d8359befd516a.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn std_collection_fallback_rejects_invalid_arguments_and_enforces_limits() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_c501c12a4fa9340b.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_e935e90db861b518.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_6b7a0037a35c3bfa.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_a5d018fea4ffde2a.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_ebc3b408a58b7de8.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_300b6a61f049d356.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_6ab379aada45358b.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_7ce9e92f29470278.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_66e5f3818dac940a.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_e08d790ff251ea70.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_35abf157b5b65eab.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_071b52f994b551fa.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_ba2a34cc36a35d9b.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_ab881a2a9a6736f4.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_9112ff23ad11b00c.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_c82fb43d7a527910.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_858d6ad020dc7cb3.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_764ce620d4d59b9a.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_eada79f20e33a496.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_6743d55af8cd6949.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_9b3916433d7d8950.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_707df93422d5461a.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_1982df28d4eb1da3.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_21a2d12a9de61f4a.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_77685929ab56e796.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_269e48b076b25d70.orna"),
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            include_str!("fixtures/evaluator_source_7096824d475d52a7.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_7165309594b939ba.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_95a62b978dcc55f8.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_4a6ad95846a84a2b.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_398fa31e9eedfd31.orna"),
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            include_str!("fixtures/evaluator_source_3ff2e7e2941445fc.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_37666509336abd7c.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_45020944b36df878.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_6ffc217c04553727.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_3c5d0cf8f5261c1b.orna"),
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            include_str!("fixtures/evaluator_source_36899f86314248a6.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_9245c15ead8f3a84.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_ae088a56abcac281.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_9d340d60cb125d0f.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_9a2536bd652142f9.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_759b06c9d00a5d2d.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_14f5bde7749a9c3f.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_3a6f7d70035cba27.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_76a989a0c699b554.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_e60dd9cfc6c145db.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_7cc77886cbeb3a68.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default()
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_b9cc1b6b7e8dca0b.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_c17f9955f4f09925.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_0e1b3d2550408a9d.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_b11204c3745b0c6b.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_dff1b602231fe5f9.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    let rows = Environment::from([(
        "rows".into(),
        Value::new(Raw::Array(vec![
            Raw::Int(1.into()),
            Raw::Int(2.into()),
            Raw::Int(3.into()),
        ]))
        .unwrap(),
    )]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_bfaa6e5fc9aaa7ba.orna"),
            &rows,
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(1.into())])).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_a88833e1fe656dd4.orna"),
            &rows,
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_d859635789532adb.orna"),
            &rows,
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())])).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_d859635789532adb.orna"),
            &rows,
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_partition_accepts_lawful_predicates_and_enforces_limits() {
    let source = include_str!("fixtures/partition_callbacks.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
            Raw::Array(vec![Raw::Int(1.into()), Raw::Int(3.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_a4d25a618149ef10.orna"),
            Limits::default()
        )),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_c35b2d46626c7672.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}
#[test]
fn std_collection_partition_preserves_order_empty_and_invokes_predicate_once_per_value() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_038bd297fe41baa8.orna"
        )),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
            Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_e84c7e377e3a0b41.orna"
        )),
        Value::new(Raw::Array(vec![Raw::Array(vec![]), Raw::Array(vec![])])).unwrap()
    );

    let functions = functions_from_source(include_str!("fixtures/partition_effect_callback.orna"));
    let arguments = Environment::from([(
        "values".into(),
        Value::new(Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Int(1.into()),
            Raw::Int(2.into()),
            Raw::Int(4.into()),
        ]))
        .unwrap(),
    )]);
    let mut effects = NoteEffects::default();
    assert_eq!(
        invoke_named_with_effects(
            "run",
            &functions,
            &arguments,
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
            Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        effects.calls,
        vec![
            vec![Value::int(3.into())],
            vec![Value::int(1.into())],
            vec![Value::int(2.into())],
            vec![Value::int(4.into())],
        ]
    );
}

#[test]
fn std_collection_partition_requires_bool_and_propagates_callback_failures() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_a81e8566cff8b527.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_0b34d95903b4a161.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn std_collection_partition_debits_one_step_per_scanned_value_without_duplicate_traversal_charging()
{
    let expression = include_str!("fixtures/evaluator_source_bfe85646d0b577d7.orna");
    assert_eq!(
        code(evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // The second callback-local debit is reached before its
                // callback body can complete at this boundary.
                max_steps: 8,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT",
        "partition must debit each scanned predicate value before invoking it",
    );
    assert_eq!(
        evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // Exactly one callback-local debit and callback body step per
                // input value fit at this boundary.
                max_steps: 9,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())]),
            Raw::Array(vec![]),
        ]))
        .unwrap(),
        "partition should succeed when one debit is charged per input value",
    );
}

#[test]
fn generic_callback_can_precede_its_value_and_returns_the_projected_field() {
    assert_eq!(
        call_module(
            include_str!("fixtures/ovc-callback-after-value-mqger.orna"),
            "project_reordered()",
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("ada".into())).unwrap(),
    );
}

#[test]
fn std_collection_filter_accepts_direct_pipeline_and_named_calls() {
    let expected = Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())])).unwrap();
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_b46bc6cf97599665.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_ac4bc1e6faf41022.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_71444e85b934eb10.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        call_module(
            include_str!("fixtures/collection_filter_named_callback.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_filter_enforces_limits() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_206213dcd55c3601.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_filter_preserves_input_order_and_skips_empty_callbacks() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_f128faed729e3af5.orna"
        )),
        Value::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())])).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_b4b807e7621234e5.orna"
        )),
        Value::new(Raw::Array(vec![])).unwrap()
    );
}

#[test]
fn std_collection_filter_requires_bool_and_propagates_callback_failures() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_5069ec707b897d91.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_4bef1e73f1e6face.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn std_collection_filter_debits_one_step_per_scanned_value_without_duplicate_traversal_charging() {
    let expression = include_str!("fixtures/evaluator_source_cf68cfe5cd9192c2.orna");
    assert_eq!(
        code(evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // The second callback's body cannot finish when the
                // callback-local debit is charged immediately before it.
                max_steps: 8,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // Exactly one traversal debit and one callback body step per
                // input value fit at this boundary.
                max_steps: 9,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(2.into())])).unwrap()
    );
}

#[test]
fn std_collection_map_preserves_order_for_direct_pipeline_named_and_function_calls() {
    let expected = Value::new(Raw::Array(vec![
        Raw::Int(30.into()),
        Raw::Int(10.into()),
        Raw::Int(20.into()),
    ]))
    .unwrap();
    for expression in [
        include_str!("fixtures/evaluator_source_849dc3054148dafa.orna"),
        include_str!("fixtures/evaluator_source_41e8d81cae734680.orna"),
        include_str!("fixtures/evaluator_source_e479c13a5fc5025b.orna"),
        include_str!("fixtures/evaluator_source_b1d259e238471a75.orna"),
        include_str!("fixtures/evaluator_source_c27251a957a095f5.orna"),
        include_str!("fixtures/evaluator_source_a09bb2414a23cdd2.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/collection_map_named_callback.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_flat_map_preserves_outer_and_inner_order_for_all_call_forms() {
    let expected = Value::new(Raw::Array(vec![
        Raw::Int(3.into()),
        Raw::Int(13.into()),
        Raw::Int(1.into()),
        Raw::Int(11.into()),
        Raw::Int(2.into()),
        Raw::Int(12.into()),
    ]))
    .unwrap();
    for expression in [
        include_str!("fixtures/evaluator_source_cc125453b57cba02.orna"),
        include_str!("fixtures/evaluator_source_41f621d91c4df82f.orna"),
        include_str!("fixtures/evaluator_source_01a8e7291f99a5b9.orna"),
        include_str!("fixtures/evaluator_source_5e2847bd3a5e80cc.orna"),
        include_str!("fixtures/evaluator_source_f3ece6b229a40a22.orna"),
        include_str!("fixtures/evaluator_source_c611188980d178a9.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/collection_flat_map_named_callback.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_sort_by_accepts_all_call_forms_and_preserves_stable_ties() {
    let expected = Value::new(Raw::Array(vec![
        Raw::Int(1.into()),
        Raw::Int(2.into()),
        Raw::Int(3.into()),
    ]))
    .unwrap();
    for expression in [
        include_str!("fixtures/evaluator_source_52fed55685dbcd12.orna"),
        include_str!("fixtures/evaluator_source_5c126e65c02652d4.orna"),
        include_str!("fixtures/evaluator_source_eb2201c3e8f978ed.orna"),
        include_str!("fixtures/evaluator_source_808360074891541e.orna"),
        include_str!("fixtures/evaluator_source_19712f849ab49586.orna"),
        include_str!("fixtures/evaluator_source_6c745483dd021e92.orna"),
        include_str!("fixtures/evaluator_source_8bdc841cdaeb5c5f.orna"),
    ] {
        assert_eq!(
            evaluate_expression(expression, &Environment::new(), Limits::default()).unwrap(),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        call_module(
            include_str!("fixtures/collection_sort_by_named_callback.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Int(1.into()),
            Raw::Int(12.into()),
            Raw::Int(3.into()),
        ]))
        .unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_d523aa5e07593df6.orna"
        )),
        Value::new(Raw::Array(vec![
            Raw::Int(2.into()),
            Raw::Int(4.into()),
            Raw::Int(3.into()),
            Raw::Int(1.into()),
        ]))
        .unwrap()
    );
}

#[test]
fn std_collection_asof_join_uses_latest_prior_match_and_canonical_row_ties() {
    let collection_source = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .find(|(path, _)| path == "std/collection.orna")
        .expect("the collection module is in the pinned standard profile")
        .1;
    let functions = functions_from_source(&collection_source)
        .into_iter()
        .map(|(name, function)| (format!("std.collection.{name}"), function))
        .collect();
    let expression = include_str!("fixtures/asof_join_nearest.orna");

    let actual = evaluate_expression_with_functions(
        expression,
        &Environment::new(),
        &functions,
        Limits::default(),
    )
    .expect("the pinned public function reaches the evaluator binding");
    let expected = evaluate(
        r#"[
            ({ group: "east", at: 10, label: "left-nearest" }, Some({ group: "east", at: 8, label: "tie-later-source" })),
            ({ group: "east", at: 20, label: "left-tie" }, Some({ group: "east", at: 18, label: "key-zzz" })),
            ({ group: "missing", at: 10, label: "left-no-match" }, null)
        ]"#,
    );
    assert_eq!(actual, expected);
}

#[test]
fn std_collection_asof_join_excludes_future_instants_to_the_nanosecond() {
    assert_eq!(
        evaluate(include_str!("fixtures/asof_join_instant_nearest.orna")),
        evaluate(r#"[({ group: "g", at: 1970-01-01T00:00:00Z, label: "left" }, null)]"#,)
    );
}

#[test]
fn std_collection_asof_join_propagates_selector_failures() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/asof_join_callback_failure.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn std_collection_rank_preserves_stable_ties_and_assigns_competition_ranks() {
    let expected = Value::new(Raw::Array(vec![
        Raw::Array(vec![Raw::Int(21.into()), Raw::Int(1.into())]),
        Raw::Array(vec![Raw::Int(11.into()), Raw::Int(1.into())]),
        Raw::Array(vec![Raw::Int(12.into()), Raw::Int(3.into())]),
        Raw::Array(vec![Raw::Int(23.into()), Raw::Int(4.into())]),
    ]))
    .unwrap();

    for expression in [
        include_str!("fixtures/evaluator_source_e69396414a4c6472.orna"),
        include_str!("fixtures/evaluator_source_baed8ba182346656.orna"),
        include_str!("fixtures/evaluator_source_1941efe558e9f2d5.orna"),
    ] {
        assert_eq!(evaluate(expression), expected, "{expression}");
    }

    assert_eq!(
        call_module(
            include_str!("fixtures/collection_rank_named_callback.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        expected
    );
}

#[test]
fn std_collection_rank_uses_lawful_callbacks_and_rejects_malformed_keys() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_c6427a9b8d15b5b6.orna"
        )),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(1.into()), Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(2.into())]),
            Raw::Array(vec![Raw::Int(3.into()), Raw::Int(3.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_6d0c0d8b23f04cc9.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );

    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_0aa05c965484f95b.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_2659b34390efac96.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_3b9b1765b86170a9.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_e48e5c9fd411d732.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_b8e6718bb5ac38ac.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_e6c7a19cdd1cc1ad.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_cfaedaa113673e92.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn std_collection_rank_honors_collection_and_step_limits() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_6fb283d272d0d94f.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_785a914205de5563.orna"),
            &Environment::new(),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_sort_by_orders_dates_and_preserves_equal_key_source_order() {
    let result = evaluate(include_str!(
        "fixtures/evaluator_source_14e95693b4ba167a.orna"
    ));
    assert_eq!(
        result,
        Value::new(Raw::Array(vec![
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(60001, Box::new(Raw::Text("2024-01-01".into())))
                ),
                (Raw::Text("label".into()), Raw::Text("first".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(60001, Box::new(Raw::Text("2024-01-01".into())))
                ),
                (Raw::Text("label".into()), Raw::Text("second".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(60001, Box::new(Raw::Text("2024-02-29".into())))
                ),
                (Raw::Text("label".into()), Raw::Text("later".into())),
            ]),
        ]))
        .unwrap()
    );
}

#[test]
fn std_collection_sort_by_orders_normalized_instants_and_nanoseconds_stably() {
    let result = evaluate(include_str!(
        "fixtures/evaluator_source_250219cb9eb7e4de.orna"
    ));
    assert_eq!(
        result,
        Value::new(Raw::Array(vec![
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("offset-first".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("epoch-first".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("offset-second".into())),
            ]),
            Raw::Map(vec![
                (
                    Raw::Text("key".into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(2.into())])),
                    ),
                ),
                (Raw::Text("label".into()), Raw::Text("nano".into())),
            ]),
        ]))
        .unwrap()
    );
}

#[test]
fn std_collection_sort_by_uses_float_total_order() {
    let rows = float_rows(&[
        0x7ff8_0000_0000_0000,
        0x3ff0_0000_0000_0000,
        0x0000_0000_0000_0000,
        0x8000_0000_0000_0000,
    ]);
    let environment = Environment::from([("rows".into(), rows)]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_7d1ab5016e7a2c88.orna"),
            &environment,
            Limits::default(),
        )
        .unwrap(),
        float_rows(&[
            0x8000_0000_0000_0000,
            0x0000_0000_0000_0000,
            0x3ff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
        ])
    );
}

#[test]
fn std_collection_sort_by_evaluates_callbacks_before_sorting_and_fails_closed() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_ce4a9188149661ea.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_4690c4ca94c8f086.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_079f95753cbecd2a.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_b774925ab66747b8.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_f83a31beaa326165.orna"),
            "ORNA-EVAL-TYPE",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_bdafaea55e8b59fc.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_5031c805969a8477.orna"),
            &Environment::new(),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );

    let functions = functions_from_source(include_str!("fixtures/sort_by_effect_callback.orna"));
    let mut effects = NoteEffects::default();
    assert_eq!(
        invoke_named_with_effects(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .unwrap(),
        Value::new(Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())])).unwrap()
    );
    assert_eq!(
        effects.calls,
        vec![vec![Value::int(3.into())], vec![Value::int(1.into())]]
    );
}

#[test]
fn root_map_names_remain_shadowable_by_admitted_functions_and_locals() {
    assert_eq!(
        call_module(
            include_str!("fixtures/root_map_shadow_function.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_9e43af7609aa5939.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
}

#[test]
fn std_collection_map_and_flat_map_propagate_callback_errors() {
    for expression in [
        include_str!("fixtures/evaluator_source_910365d3ce0990a5.orna"),
        include_str!("fixtures/evaluator_source_1bd54413846d6ace.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-DIVIDE-BY-ZERO",
            "{expression}"
        );
    }
    assert_eq!(
        code(call_module(
            include_str!("fixtures/map_invalid_callback.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )),
        "ORNA-EVAL-ARGUMENT"
    );
}

#[test]
fn std_collection_map_and_flat_map_reject_invalid_inputs_and_outputs() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_432e15781f64c7e8.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_418f8868f06bcdb4.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_0a9431506042fe49.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_201b020420f0627c.orna"),
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            include_str!("fixtures/evaluator_source_f46d03addce5ed29.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_e50bb995e1ac2af6.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_d1da638edbdc9c3f.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_8a6e8511aa5ff923.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn std_collection_map_and_flat_map_enforce_collection_limits() {
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_401a69f4eaecb1c1.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_b8a9d85181583c56.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_split_when_accepts_functions_and_enforces_limits() {
    let source = include_str!("fixtures/split_when_callbacks.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_202b94d3ded70909.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
            Raw::Array(vec![Raw::Int(4.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_88b5c516109064c8.orna"),
            Limits::default(),
        )),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/split_when_collection_limit.orna").trim(),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_split_when_rejects_invalid_callback_calls() {
    let source = include_str!("fixtures/split_when_callbacks.orna");
    for (function, expected) in [
        (
            include_str!("fixtures/evaluator_source_adb9f4ae66c65bdf.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_3496958419e131a2.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_74445860bb3f4f2d.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_88b5c516109064c8.orna"),
            "ORNA-EVAL-ARGUMENT",
        ),
        (
            include_str!("fixtures/evaluator_source_cfbc7df2791c39fa.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(call_module(source, function, Limits::default())),
            expected
        );
    }
}
#[test]
fn std_collection_split_when_preserves_order_and_omits_empty_groups() {
    let source = include_str!("fixtures/split_when_callbacks.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_b04f6c9572c80d12.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(2.into())]),
            Raw::Array(vec![Raw::Int(4.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_e70131b920965fe2.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(2.into())]),
            Raw::Array(vec![Raw::Int(4.into()), Raw::Int(5.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_ff7d258da6b05c0e.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![])).unwrap()
    );
}

#[test]
fn std_collection_split_when_requires_bool_and_propagates_callback_failures() {
    let source = include_str!("fixtures/split_when_callbacks.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_edd19653a14ea50e.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(2.into()), Raw::Int(3.into())]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_74445860bb3f4f2d.orna"),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_4325fc3987edad59.orna"),
            Limits::default(),
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn std_collection_split_when_debits_one_step_per_scanned_value_without_duplicate_traversal_charging(
) {
    let expression = include_str!("fixtures/split_when_step_budget.orna").trim();
    assert_eq!(
        code(evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // The second callback-local debit is reached before its
                // callback body can complete at this boundary.
                max_steps: 8,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT",
        "split_when must debit each scanned predicate value before invoking it",
    );
    assert_eq!(
        evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // Exactly one callback-local debit and callback body step per
                // input value fit at this boundary.
                max_steps: 9,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![Raw::Int(1.into())]),
            Raw::Array(vec![Raw::Int(2.into())]),
        ]))
        .unwrap(),
        "split_when should succeed when one debit is charged per input value",
    );
}

#[test]
fn std_collection_group_by_accepts_functions_and_enforces_limits() {
    let source = include_str!("fixtures/group_by_callbacks.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_202b94d3ded70909.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())]),
            ]),
            Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(3.into()), Raw::Int(1.into())]),
            ]),
        ]))
        .unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/group_by_collection_limit.orna").trim(),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_collection_group_by_rejects_invalid_callback_calls() {
    let source = include_str!("fixtures/group_by_callbacks.orna");
    for (function, expected) in [
        (
            include_str!("fixtures/evaluator_source_adb9f4ae66c65bdf.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_3496958419e131a2.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_7a273d73456bb1dc.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_cfbc7df2791c39fa.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
    ] {
        assert_eq!(
            code(call_module(source, function, Limits::default())),
            expected
        );
    }
}
#[test]
fn std_collection_group_by_sorts_keys_and_preserves_input_row_order() {
    let source = include_str!("fixtures/group_by_callbacks.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_513a93d1e303cb37.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Array(vec![Raw::Int(12.into())]),
            ]),
            Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Int(7.into()),
                    Raw::Int(4.into()),
                ]),
            ]),
            Raw::Array(vec![
                Raw::Int(2.into()),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Int(5.into())]),
            ]),
        ]))
        .unwrap()
    );
}

#[test]
fn std_collection_group_by_empty_input_does_not_invoke_key_callback() {
    assert_eq!(
        call_module(
            include_str!("fixtures/group_by_callbacks.orna"),
            include_str!("fixtures/evaluator_source_ff7d258da6b05c0e.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Array(vec![])).unwrap()
    );
}

#[test]
fn std_collection_group_by_requires_lawful_keys_and_propagates_callback_failures() {
    let source = include_str!("fixtures/group_by_callbacks.orna");
    for expression in [
        include_str!("fixtures/evaluator_source_7a273d73456bb1dc.orna"),
        include_str!("fixtures/evaluator_source_4ae33d119b7ce637.orna"),
    ] {
        assert_eq!(
            code(call_module(source, expression, Limits::default())),
            "ORNA-EVAL-TYPE",
            "{expression} must reject keys without a lawful total comparison",
        );
    }
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_4325fc3987edad59.orna"),
            Limits::default()
        )),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn std_collection_group_by_debits_one_step_per_scanned_value_without_duplicate_traversal_charging()
{
    let expression = include_str!("fixtures/group_by_step_budget.orna").trim();
    assert_eq!(
        code(evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // The second callback-local debit is reached before its
                // callback body can complete at this boundary.
                max_steps: 8,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT",
        "group_by must debit each scanned key callback before invoking it",
    );
    assert_eq!(
        evaluate_expression(
            expression,
            &Environment::new(),
            Limits {
                // Exactly one callback-local debit and callback body step per
                // input value fit at this boundary.
                max_steps: 9,
                ..Limits::default()
            },
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(1.into())]),
            ]),
            Raw::Array(vec![
                Raw::Int(2.into()),
                Raw::Array(vec![Raw::Int(2.into())]),
            ]),
        ]))
        .unwrap(),
        "group_by should succeed when one debit is charged per input value",
    );
}

#[test]
fn recursive_calls_terminate_or_hit_shared_limits() {
    let source = include_str!("fixtures/recursive_calls_limits.orna");
    assert_eq!(
        call_module(
            source,
            include_str!("fixtures/evaluator_source_cd601da1e7790ede.orna"),
            Limits::default()
        )
        .unwrap(),
        Value::int(120.into())
    );
    for limits in [
        Limits {
            max_steps: 8,
            ..Limits::default()
        },
        Limits {
            max_depth: 8,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            code(call_module(
                source,
                include_str!("fixtures/evaluator_source_8d21addf01057c91.orna"),
                limits
            )),
            "ORNA-EVAL-LIMIT"
        );
    }
    let source = include_str!("fixtures/small_combined_function_step_budget.orna");
    assert_eq!(
        code(call_module(
            source,
            include_str!("fixtures/evaluator_source_7c4d08bed4062b43.orna"),
            Limits {
                max_steps: 6,
                ..Limits::default()
            }
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn function_defaults_return_values_and_see_earlier_parameters() {
    let source = include_str!("fixtures/function_defaults_values.orna");
    let arguments = Environment::from([("first".into(), Value::int(10.into()))]);
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(33.into())
    );
    let arguments = Environment::from([
        ("first".into(), Value::int(10.into())),
        ("second".into(), Value::int(20.into())),
    ]);
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(51.into())
    );
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(51.into())
    );
}

#[test]
fn supplied_arguments_do_not_evaluate_their_defaults() {
    let source = include_str!("fixtures/function_defaults_supplied_argument.orna");
    let arguments = Environment::from([("value".into(), Value::int(7.into()))]);
    assert_eq!(
        invoke(source, &arguments, Limits::default()).unwrap(),
        Value::int(7.into())
    );
    assert_eq!(
        code(invoke(source, &Environment::new(), Limits::default())),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
}

#[test]
fn function_defaults_and_body_share_a_single_step_budget() {
    let source = include_str!("fixtures/function_defaults_step_budget.orna");
    let limits = Limits {
        max_steps: 6,
        ..Limits::default()
    };
    assert_eq!(
        code(invoke(source, &Environment::new(), limits)),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_steps: 9,
        ..Limits::default()
    };
    assert_eq!(
        invoke(source, &Environment::new(), limits).unwrap(),
        Value::int(10.into())
    );
}

#[test]
fn function_argument_admission_precedes_default_evaluation_and_redacts_errors() {
    for (source, arguments) in [
        (
            include_str!("fixtures/function_argument_missing_required.orna"),
            Environment::new(),
        ),
        (
            include_str!("fixtures/function_argument_unknown_argument.orna"),
            Environment::from([("secret".into(), Value::int(1.into()))]),
        ),
        (
            include_str!("fixtures/function_argument_duplicate_parameter.orna"),
            Environment::from([("first".into(), Value::int(1.into()))]),
        ),
    ] {
        let error = invoke(source, &arguments, Limits::default()).unwrap_err();
        assert_eq!(error.code(), "ORNA-EVAL-ARGUMENT");
        assert_eq!(error.diagnostic().message(), "<redacted>");
    }
}

#[test]
fn evaluates_literals_collections_bindings_and_math() {
    let value = evaluate(include_str!(
        "fixtures/evaluator_source_f515a0858442de8e.orna"
    ));
    assert_eq!(
        value.raw(),
        &Raw::Map(vec![
            (Raw::Text("label".into()), Raw::Text("ok".into())),
            (
                Raw::Text("values".into()),
                Raw::Array(vec![
                    Raw::Array(vec![
                        Raw::Int(2.into()),
                        Raw::Tag(
                            60000,
                            Box::new(Raw::Array(vec![Raw::Int(25.into()), Raw::Int((-1).into())]))
                        ),
                        Raw::Float(3.0f64.to_bits())
                    ]),
                    Raw::Null,
                    Raw::Bool(true),
                ])
            ),
        ])
    );
}

#[test]
fn final_control_expression_in_a_block_returns_its_value() {
    let source = include_str!("fixtures/control-expression-block-tail-uamr.orna");
    let result = invoke(source, &Environment::new(), Limits::default())
        .expect("the final control expression evaluates to its branch value");
    assert_eq!(
        result.raw(),
        &Raw::Int(BigInt::from(1)),
        "an unsemicolonated final control expression is the block tail"
    );
}

#[test]
fn evaluates_canonical_date_literals_at_boundaries_and_leap_days() {
    for (source, expected) in [
        (
            include_str!("fixtures/evaluator_source_adc54d5a38b33a0c.orna"),
            "0001-01-01",
        ),
        (
            include_str!("fixtures/evaluator_source_ca1d2f9066774e9f.orna"),
            "2000-02-29",
        ),
        (
            include_str!("fixtures/evaluator_source_2b65ec6936440686.orna"),
            "2024-02-29",
        ),
        (
            include_str!("fixtures/evaluator_source_524be55b2827968f.orna"),
            "9999-12-31",
        ),
    ] {
        assert_eq!(
            evaluate(source).raw(),
            &Raw::Tag(60001, Box::new(Raw::Text(expected.into()))),
            "{source}"
        );
    }
}

#[test]
fn date_literals_preserve_the_canonical_value_inside_collections_and_records() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_826acb6aac0f18ac.orna"
        ))
        .raw(),
        &Raw::Array(vec![
            Raw::Tag(60001, Box::new(Raw::Text("2024-02-29".into()))),
            Raw::Map(vec![(
                Raw::Text("day".into()),
                Raw::Tag(60001, Box::new(Raw::Text("9999-12-31".into()))),
            )]),
        ])
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_3cd5997caf2fd8a1.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn malformed_date_is_rejected_lexically_before_evaluator_execution() {
    for source in [
        include_str!("fixtures/evaluator_source_f7ed140174423d1f.orna"),
        include_str!("fixtures/evaluator_source_1ff48d7df09be03f.orna"),
        include_str!("fixtures/evaluator_source_468dbb10858c5ece.orna"),
        include_str!("fixtures/evaluator_source_3e40fb8129a17400.orna"),
    ] {
        let errors = lex(source).expect_err("calendar-invalid input must fail lexing");
        assert!(
            errors.iter().any(|error| error.code == "ORNA-LEX-007"),
            "expected date diagnostic for {source}, got {errors:?}"
        );
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-PARSE"
        );
    }

    let malformed_ast = Expr::Literal {
        text: "0000-01-01".into(),
        kind: orna_syntax_v1::LiteralKind::Date,
        span: SyntaxSpan::new(0, 10),
    };
    assert_eq!(
        code(evaluate_parsed(
            &malformed_ast,
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn date_token_class_is_disjoint_from_decimal_and_float_tokens() {
    let tokens = lex(include_str!(
        "fixtures/evaluator_source_b3fec7d3630420af.orna"
    ))
    .unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Date);
    assert_eq!(tokens[1].kind, TokenKind::Decimal);
    assert_eq!(tokens[2].kind, TokenKind::Float);
}

#[test]
fn instant_literals_normalize_offsets_and_emit_canonical_utc_components() {
    for (source, seconds, nanosecond) in [
        (
            include_str!("fixtures/evaluator_source_a7e105d1baa0abd2.orna"),
            1_709_164_800,
            0,
        ),
        (
            include_str!("fixtures/evaluator_source_56329e5f93b4688d.orna"),
            1_709_145_000,
            100_000_000,
        ),
        (
            include_str!("fixtures/evaluator_source_ac37e0796e6bf49a.orna"),
            1_709_164_800,
            0,
        ),
        (
            include_str!("fixtures/evaluator_source_d6e461af6345c009.orna"),
            -1,
            100_000_000,
        ),
    ] {
        assert_eq!(
            evaluate(source).raw(),
            &Raw::Tag(
                60002,
                Box::new(Raw::Array(vec![
                    Raw::Int(seconds.into()),
                    Raw::Int(nanosecond.into()),
                ])),
            ),
            "{source}"
        );
    }
}

#[test]
fn instant_literals_compare_by_normalized_utc_second_and_nanosecond() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_1a4b3242a98785a8.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_8ae6482c2e672359.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_34252f22d4a11ef5.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn instant_literals_preserve_precision_nesting_and_canonical_environment_values() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_bccef0376f2ce331.orna"
        ))
        .raw(),
        &Raw::Array(vec![
            Raw::Tag(
                60002,
                Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
            ),
            Raw::Map(vec![(
                Raw::Text("instant".into()),
                Raw::Tag(
                    60002,
                    Box::new(Raw::Array(vec![
                        Raw::Int(0.into()),
                        Raw::Int(123_456_789.into()),
                    ])),
                ),
            )]),
        ])
    );

    let environment = Environment::from([(
        "instant".into(),
        Value::new(Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![Raw::Int((-1).into()), Raw::Int(1.into())])),
        ))
        .unwrap(),
    )]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_618dc58379dda348.orna"),
            &environment,
            Limits::default()
        )
        .unwrap()
        .raw(),
        &Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![Raw::Int((-1).into()), Raw::Int(1.into())])),
        )
    );
}

#[test]
fn malformed_instant_literals_and_canonical_components_fail_closed() {
    for source in [
        include_str!("fixtures/evaluator_source_ae0f16f72284adc0.orna"),
        include_str!("fixtures/evaluator_source_74c08e61c3723b36.orna"),
        include_str!("fixtures/evaluator_source_c3b7dc7899695f11.orna"),
        include_str!("fixtures/evaluator_source_f2a71bd8fb5ee40a.orna"),
        include_str!("fixtures/evaluator_source_43a5c745044ab627.orna"),
    ] {
        let errors = lex(source).expect_err("malformed instant must fail lexing");
        assert!(
            errors.iter().any(|error| error.code == "ORNA-LEX-008"),
            "expected instant diagnostic for {source}, got {errors:?}"
        );
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-PARSE"
        );
    }

    let malformed_ast = Expr::Literal {
        text: "2024-02-29T12:00:60Z".into(),
        kind: orna_syntax_v1::LiteralKind::Instant,
        span: SyntaxSpan::new(0, 20),
    };
    assert_eq!(
        code(evaluate_parsed(
            &malformed_ast,
            &Environment::new(),
            Limits::default()
        )),
        "ORNA-EVAL-VALUE"
    );

    for raw in [
        Raw::Tag(60002, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
        Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Int(1_000_000_000.into()),
            ])),
        ),
    ] {
        assert!(
            Value::new(raw).is_err(),
            "invalid Instant tag must fail closed"
        );
    }

    let environment = Environment::from([(
        "instant".into(),
        Value::new(Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![
                Raw::Int("9223372036854775808".parse().unwrap()),
                Raw::Int(0.into()),
            ])),
        ))
        .unwrap(),
    )]);
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_618dc58379dda348.orna"),
            &environment,
            Limits::default()
        )),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn canonical_duration_environment_values_round_trip_and_compare() {
    let duration = Raw::Tag(
        60005,
        Box::new(Raw::Array(vec![
            Raw::Int((-1).into()),
            Raw::Int(500_000_000.into()),
        ])),
    );
    let later = Raw::Tag(
        60005,
        Box::new(Raw::Array(vec![
            Raw::Int("9223372036854775808".parse().unwrap()),
            Raw::Int(0.into()),
        ])),
    );
    let environment = Environment::from([
        ("duration".into(), Value::new(duration.clone()).unwrap()),
        ("same".into(), Value::new(duration.clone()).unwrap()),
        ("later".into(), Value::new(later).unwrap()),
    ]);

    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_caa79af4db67695c.orna"),
            &environment,
            Limits::default()
        )
        .unwrap()
        .raw(),
        &duration
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_6c13657c58f566b6.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_63cc6ed61ba3449e.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn malformed_canonical_duration_components_fail_closed() {
    for raw in [
        Raw::Tag(60005, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
        Raw::Tag(
            60005,
            Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int((-1).into())])),
        ),
        Raw::Tag(
            60005,
            Box::new(Raw::Array(vec![
                Raw::Int(0.into()),
                Raw::Int(1_000_000_000.into()),
            ])),
        ),
    ] {
        assert!(
            Value::new(raw).is_err(),
            "invalid Duration tag must fail closed"
        );
    }
}

#[test]
fn uses_environment_and_short_circuiting_deterministically() {
    let mut environment = BTreeMap::new();
    environment.insert("count".into(), Value::int(41.into()));
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_bce879f251fac0a3.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::int(42.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_44973f0f388fe157.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_cfe289350b2c847e.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn coalesces_present_and_missing_optional_values_without_evaluating_dead_rhs() {
    let mut environment = Environment::from([(
        "present".into(),
        Value::option(Some(Value::int(7.into()))).unwrap(),
    )]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_422a520b9411f989.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::int(7.into())
    );

    environment.insert("missing".into(), Value::option(None).unwrap());
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_8773b21b48928663.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::int(9.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_55d19da6881f03fa.orna"
        )),
        Value::int(11.into())
    );
}

#[test]
fn supports_comparison_boolean_and_allowlisted_math() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_72f3fbd4b9a83d7f.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_26c4e6931ce2f32d.orna"
        )),
        Value::decimal(2.into(), 0.into()).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_deaea0aa29364d80.orna"
        ))
        .raw(),
        &Raw::Map(vec![
            (Raw::Text("z".into()), Raw::Int(2.into())),
            (Raw::Text("alphabet".into()), Raw::Int(1.into())),
        ])
    );
    assert_eq!(
        evaluate_repl(
            include_str!("fixtures/evaluator_source_a74cde488702839d.orna"),
            &Environment::new(),
            Limits::default()
        )
        .unwrap(),
        Value::int(3.into())
    );
}

#[test]
fn pinned_standard_math_source_executes_every_public_function() {
    let (path, source) = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .next()
        .expect("pinned standard source");
    assert_eq!(path, "std/math.orna");
    let mut functions = functions_from_source(&source);
    functions.extend(functions_from_source(include_str!(
        "fixtures/v1_standard_math_consumer.orna"
    )));
    assert_eq!(
        invoke_named("incremented", &functions, &Environment::new(), Limits::default())
            .unwrap(),
        Value::int(42.into())
    );
    assert_eq!(
        invoke_named("decremented", &functions, &Environment::new(), Limits::default())
            .unwrap(),
        Value::int((-1).into())
    );
    assert_eq!(
        invoke_named("zero", &functions, &Environment::new(), Limits::default()).unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        invoke_named("minimum", &functions, &Environment::new(), Limits::default()).unwrap(),
        Value::int(3.into())
    );
    assert_eq!(
        invoke_named("maximum", &functions, &Environment::new(), Limits::default()).unwrap(),
        Value::int(5.into())
    );
    assert_eq!(
        invoke_named("clamped", &functions, &Environment::new(), Limits::default()).unwrap(),
        Value::int(5.into())
    );
}

#[test]
fn evaluates_selection_indexing_named_calls_and_case_patterns() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_27d35071af18c124.orna"
        )),
        Value::int(12.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_1f7dca301d1c5d0c.orna"
        )),
        Value::int(5.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_677e65e055b673ad.orna"
        )),
        Value::int(3.into())
    );
    let span = SyntaxSpan::new(0, 0);
    let record_pattern = Pattern::Record {
        fields: vec![(
            "right".into(),
            Some(Pattern::Name("selected".into(), span.clone())),
            span.clone(),
        )],
        span: span.clone(),
    };
    let record_value = Expr::Record {
        fields: vec![RecordField {
            name: "right".into(),
            value: Expr::Literal {
                text: "9".into(),
                kind: orna_syntax_v1::LiteralKind::Integer,
                span: span.clone(),
            },
            span: span.clone(),
        }],
        span: span.clone(),
    };
    let expression = Expr::Block {
        statements: vec![Statement::Let {
            pattern: record_pattern,
            annotation: None,
            value: record_value,
            span: span.clone(),
        }],
        tail: Some(Box::new(Expr::Name {
            text: "selected".into(),
            span: span.clone(),
        })),
        span,
    };
    assert_eq!(
        evaluate_parsed(&expression, &Environment::new(), Limits::default()).unwrap(),
        Value::int(9.into())
    );
}

#[test]
fn evaluates_local_assignments_and_finite_list_for_mutations() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_106867567dd44a96.orna"
        )),
        Value::int(6.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_7c11be126271719b.orna"
        )),
        Value::int(4.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_6da1c113b573586a.orna"
        )),
        Value::int(5.into())
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_b35b624ee9f77627.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-ASSERT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_8c330863e501bce6.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn executes_function_and_finite_for_control_transfers() {
    assert_eq!(
        invoke(
            include_str!("fixtures/evaluator_source_463da97844a9a28a.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(7.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_43c5f3a105661ef7.orna"
        )),
        Value::int(3.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_2d9db4436a6b54da.orna"
        )),
        Value::int(8.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_2a81ac3000595682.orna"
        )),
        Value::int(3.into())
    );
}

#[test]
fn finite_for_break_values_stop_iteration_and_preserve_loop_boundaries() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_9da6c1c2e096c068.orna"
        )),
        Value::new(Raw::Array(vec![Raw::Int(20.into()), Raw::Int(1.into())])).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_7d54e4bbec351331.orna"
        )),
        Value::int(32.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_78fd575f097b5db7.orna"
        )),
        Value::unit()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_5dbc29f7fd954a1b.orna"
        )),
        Value::unit()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/control_flow_break_outside_loop.orna").trim(),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        evaluate(include_str!("fixtures/control_flow_while_break_value.orna")),
        Value::int(1.into())
    );
}

#[test]
fn transfer_boundaries_reject_loop_transfers_from_a_called_lambda() {
    assert_eq!(
        code(call_module(
            include_str!("fixtures/evaluator_source_75b90831c0aec9b8.orna"),
            include_str!("fixtures/evaluator_source_71b55c11c5bc130b.orna"),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_c08c8fecfa405906.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn fail_argument_propagates_a_return_transfer_before_error_matching() {
    assert_eq!(
        invoke(
            include_str!("fixtures/evaluator_source_9c3cfdf2de7cb949.orna"),
            &Environment::new(),
            Limits::default(),
        )
        .unwrap(),
        Value::int(7.into())
    );
}

#[test]
fn integer_ranges_are_canonical_membership_values_and_finite_iterables() {
    let half_open = Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(1.into())])),
            ),
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
            ),
            Raw::Bool(false),
        ])),
    ))
    .unwrap();
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_37b73cdcf7d53e07.orna"
        )),
        half_open
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_17ed6c2a56eae7c1.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_452a79b3d5b67727.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_9a0520b884def24f.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_3980c88d94c7835d.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_43f964c89131535f.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_ed7bad50b35c8d9e.orna"
        )),
        Value::new(Raw::Tag(
            60019,
            Box::new(Raw::Array(vec![
                Raw::Tag(
                    60013,
                    Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
                ),
                Raw::Tag(
                    60013,
                    Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(1.into())])),
                ),
                Raw::Bool(false),
            ])),
        ))
        .unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_ce844aa56351e69f.orna"
        )),
        Value::int(10.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_9f7828018a344680.orna"
        )),
        Value::int(15.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_23ea1d8e02403498.orna"
        )),
        Value::int(0.into())
    );
}

#[test]
fn integer_ranges_reject_unsupported_forms_and_obey_finite_limits() {
    for source in [
        include_str!("fixtures/evaluator_source_470fd21e4087497e.orna"),
        include_str!("fixtures/evaluator_source_abb2071d9454b942.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_2644008e0118307d.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    let unbounded = Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
            ),
            Raw::Bool(false),
        ])),
    ))
    .unwrap();
    let environment = Environment::from([("range".into(), unbounded)]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_943497eb7d9e7718.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_396ee5df0322c5ae.orna"),
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_02c5ba4d3acff059.orna"
        )),
        Value::new(Raw::Tag(
            60019,
            Box::new(Raw::Array(vec![
                Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
                Raw::Tag(
                    60013,
                    Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(5.into())])),
                ),
                Raw::Bool(false),
            ])),
        ))
        .unwrap()
    );
}

fn date_range(lower: Option<&str>, upper: Option<&str>, upper_inclusive: bool) -> Value {
    let endpoint = |value: Option<&str>| match value {
        Some(value) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Tag(60001, Box::new(Raw::Text(value.into()))),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn decimal_range(
    lower: Option<(i64, i64)>,
    upper: Option<(i64, i64)>,
    upper_inclusive: bool,
) -> Value {
    let endpoint = |value: Option<(i64, i64)>| match value {
        Some((coefficient, exponent10)) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Value::decimal(coefficient.into(), exponent10.into())
                    .unwrap()
                    .raw()
                    .clone(),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn float_range(lower: Option<f64>, upper: Option<f64>, upper_inclusive: bool) -> Value {
    let endpoint = |value: Option<f64>| match value {
        Some(value) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Float(value.to_bits()),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn instant_range(
    lower: Option<(i64, u32)>,
    upper: Option<(i64, u32)>,
    upper_inclusive: bool,
) -> Value {
    let endpoint = |value: Option<(i64, u32)>| match value {
        Some((unix_seconds, nanosecond)) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Tag(
                    60002,
                    Box::new(Raw::Array(vec![
                        Raw::Int(unix_seconds.into()),
                        Raw::Int(nanosecond.into()),
                    ])),
                ),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

fn duration(seconds: i64, nanosecond: u32) -> Value {
    duration_big(seconds.into(), nanosecond)
}

fn duration_big(seconds: BigInt, nanosecond: u32) -> Value {
    Value::new(Raw::Tag(
        60005,
        Box::new(Raw::Array(vec![
            Raw::Int(seconds),
            Raw::Int(nanosecond.into()),
        ])),
    ))
    .unwrap()
}

fn duration_range(
    lower: Option<(i64, u32)>,
    upper: Option<(i64, u32)>,
    upper_inclusive: bool,
) -> Value {
    duration_range_big(
        lower.map(|(seconds, nanosecond)| (seconds.into(), nanosecond)),
        upper.map(|(seconds, nanosecond)| (seconds.into(), nanosecond)),
        upper_inclusive,
    )
}

fn duration_range_big(
    lower: Option<(BigInt, u32)>,
    upper: Option<(BigInt, u32)>,
    upper_inclusive: bool,
) -> Value {
    let endpoint = |value: Option<(BigInt, u32)>| match value {
        Some((seconds, nanosecond)) => Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                duration_big(seconds, nanosecond).raw().clone(),
            ])),
        ),
        None => Raw::Tag(60013, Box::new(Raw::Array(vec![Raw::Int(0.into())]))),
    };
    Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            endpoint(lower),
            endpoint(upper),
            Raw::Bool(upper_inclusive),
        ])),
    ))
    .unwrap()
}

#[test]
fn decimal_ranges_are_canonical_membership_values_with_optional_bounds_and_ordering() {
    let half_open = decimal_range(Some((125, -2)), Some((25, -1)), false);
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_af97fb51fa47b448.orna"
        )),
        half_open
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_169881bf25db61e3.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_021e12f0e8a9ba60.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_c5a7e544a08bd7c1.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_a3062491c8ab5903.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_405044db70183153.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_60965ca413f04340.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );

    let environment =
        Environment::from([("range".into(), decimal_range(None, Some((125, -2)), false))]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_2269c0be009b610c.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        decimal_range(None, Some((125, -2)), false)
    );
}

#[test]
fn decimal_ranges_allow_empty_values_but_reject_mixed_types_and_iteration() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_3dc021a26396f62d.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_6988f6da1bbac15c.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in [
        include_str!("fixtures/evaluator_source_8a624a4f56bc08d4.orna"),
        include_str!("fixtures/evaluator_source_6a6456f040c1501e.orna"),
        include_str!("fixtures/evaluator_source_a0ce09ead08ccbe2.orna"),
        include_str!("fixtures/evaluator_source_145a9ee6e3d386e0.orna"),
        include_str!("fixtures/evaluator_source_9e5de317f77b6b72.orna"),
        include_str!("fixtures/evaluator_source_e1e683920b7fd8fd.orna"),
        include_str!("fixtures/evaluator_source_a2f239357c679d28.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    let environment = Environment::from([(
        "range".into(),
        decimal_range(Some((125, -2)), Some((25, -1)), false),
    )]);
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_396ee5df0322c5ae.orna"),
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn float_ranges_support_finite_membership_with_optional_bounds() {
    let half_open = float_range(Some(1.25), Some(2.5), false);
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_6f42ab83be960be6.orna"
        )),
        half_open
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_16ddba339251d381.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_9e74380c98a17b47.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_2264374d4deaf570.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_f07db4073bd5a245.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_8946a13e512805af.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn float_ranges_reject_mixed_types_and_iteration_and_nan_is_not_a_member() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_7be12b09d179b095.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_735b5a8ac63b0add.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in [
        include_str!("fixtures/evaluator_source_5a3023a64699bb7c.orna"),
        include_str!("fixtures/evaluator_source_0889111b6079f39d.orna"),
        include_str!("fixtures/evaluator_source_ae50818f3a2bd4e8.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    let range = float_range(Some(1.25), Some(2.5), false);
    let environment = Environment::from([
        ("range".into(), range),
        ("nan".into(), Value::float_bits(CANONICAL_NAN_BITS)),
    ]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_42d44d13883ccafe.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_396ee5df0322c5ae.orna"),
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn float_ranges_allow_non_finite_endpoints_and_total_ordering() {
    let whole = float_range(Some(f64::NEG_INFINITY), Some(f64::INFINITY), false);
    let nan_lower = float_range(Some(f64::from_bits(CANONICAL_NAN_BITS)), Some(2.5), false);
    let negative_zero = float_range(Some(-0.0), Some(1.0), false);
    let positive_zero = float_range(Some(0.0), Some(1.0), false);
    let finite_lower = float_range(Some(1.0), Some(2.5), false);
    let environment = Environment::from([
        ("whole".into(), whole.clone()),
        ("nan_lower".into(), nan_lower.clone()),
        ("negative_zero".into(), negative_zero),
        ("positive_zero".into(), positive_zero),
        ("finite_lower".into(), finite_lower),
        ("nan".into(), Value::float_bits(CANONICAL_NAN_BITS)),
    ]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_068770aabc459d7c.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_db29d1951356e23e.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_9f760ea8c46eaba3.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_2187fa8aa8606056.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_09b7f91fb8d6e238.orna"),
            &environment,
            Limits::default()
        )
        .unwrap_err()
        .code(),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_aa36cd8ecaf97539.orna"),
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_885dee17c1914dab.orna"),
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_5d5766cf2d787016.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        whole
    );
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_a759d4630b8b2a8c.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        nan_lower
    );
}

#[test]
fn date_ranges_are_canonical_membership_values_with_optional_bounds() {
    let half_open = date_range(Some("2024-02-01"), Some("2024-02-03"), false);
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_53d874fe2f0de8db.orna"
        )),
        half_open
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_0c522cdfbb8a3aec.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_d531329b741acc15.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_c689d17c6de129e3.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_0717b9494b2fb344.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_c15beffdf618741c.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );

    let environment =
        Environment::from([("range".into(), date_range(None, Some("2024-02-01"), false))]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_2269c0be009b610c.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        date_range(None, Some("2024-02-01"), false)
    );
}

#[test]
fn date_ranges_allow_empty_values_but_reject_mixed_bounds_and_iteration() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_42c61f7468e4aa7f.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_047ec2c64f206e76.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in [
        include_str!("fixtures/evaluator_source_e4eb725871ae217f.orna"),
        include_str!("fixtures/evaluator_source_906a48ec31b77f45.orna"),
        include_str!("fixtures/evaluator_source_c45fbb0f8df3e258.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default()
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_9168eeeb1b64af64.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_757a9017278c0b5f.orna"
        )),
        Value::int(6.into())
    );
}

#[test]
fn instant_ranges_are_canonical_membership_values_with_optional_bounds_and_ordering() {
    let half_open = instant_range(Some((0, 0)), Some((1, 2)), false);
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_45a08c8a6697d48f.orna"
        )),
        half_open
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_145a07e2d3211be6.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_fe0d2aaf0115c68d.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_6475417bb0e40090.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_42f41daf9338d933.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_d3656e570ea6d0c5.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_c2149570253cd071.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_67c8880020e4c0f5.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
}

#[test]
fn instant_ranges_allow_empty_values_but_reject_mixed_types_and_iteration() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_22fe581400948f53.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_a0b16d4e2decd3b2.orna"
        )),
        Value::new(Raw::Bool(false)).unwrap()
    );
    for source in [
        include_str!("fixtures/evaluator_source_8acf82bf2e2bf775.orna"),
        include_str!("fixtures/evaluator_source_d061b3ee5d43c33d.orna"),
        include_str!("fixtures/evaluator_source_eb47effd5e8807c7.orna"),
        include_str!("fixtures/evaluator_source_f05eaa8b5f874571.orna"),
        include_str!("fixtures/evaluator_source_61496acf10a4ea3d.orna"),
        include_str!("fixtures/evaluator_source_ca122e454bb2ced3.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
    let environment = Environment::from([(
        "range".into(),
        instant_range(Some((0, 0)), Some((1, 0)), false),
    )]);
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_ec398da66aa1787b.orna"),
            &environment,
            Limits::default(),
        )),
        "ORNA-EVAL-TYPE"
    );
}

#[test]
fn canonical_duration_ranges_support_membership_ordering_and_stable_collections() {
    let earlier = duration(-1, 500_000_000);
    let equal = duration(0, 0);
    let later = duration(0, 1);
    let huge_seconds = BigInt::from(i64::MAX) + BigInt::from(1);
    let huge = duration_big(huge_seconds.clone(), 0);
    let membership_range = duration_range_big(
        Some((BigInt::from(-1), 500_000_000)),
        Some((huge_seconds.clone(), 0)),
        true,
    );
    let lower_first = duration_range(Some((-1, 500_000_000)), Some((1, 0)), false);
    let lower_second = duration_range(Some((0, 0)), Some((1, 0)), false);
    let upper_first = duration_range(Some((0, 0)), Some((0, 1)), false);
    let upper_second = duration_range(Some((0, 0)), Some((1, 0)), false);
    let exclusive = duration_range(Some((0, 0)), Some((1, 0)), false);
    let inclusive = duration_range(Some((0, 0)), Some((1, 0)), true);
    let rows = Value::new(Raw::Array(vec![
        Raw::Map(vec![
            (Raw::Text("key".into()), later.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("later".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), equal.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("first".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), equal.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("second".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), earlier.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("earlier".into())),
        ]),
        Raw::Map(vec![
            (Raw::Text("key".into()), huge.raw().clone()),
            (Raw::Text("label".into()), Raw::Text("huge".into())),
        ]),
    ]))
    .unwrap();
    let environment = Environment::from([
        ("earlier".into(), earlier.clone()),
        ("equal".into(), equal.clone()),
        ("later".into(), later.clone()),
        ("huge".into(), huge.clone()),
        ("range".into(), membership_range),
        ("lower_first".into(), lower_first),
        ("lower_second".into(), lower_second),
        ("upper_first".into(), upper_first),
        ("upper_second".into(), upper_second),
        ("exclusive".into(), exclusive),
        ("inclusive".into(), inclusive),
        (
            "values".into(),
            Value::new(Raw::Array(vec![
                later.raw().clone(),
                equal.raw().clone(),
                earlier.raw().clone(),
                huge.raw().clone(),
            ]))
            .unwrap(),
        ),
        ("rows".into(), rows),
    ]);

    for (source, expected) in [
        (
            include_str!("fixtures/evaluator_source_195c82295183d6b6.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_61244926cf1c127c.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_ca66c63dd2ef2156.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_c6a2792645382523.orna"),
            Value::option(Some(earlier.clone())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_8e8e4a8a6663c04c.orna"),
            Value::option(Some(huge.clone())).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_a03999dbe5078266.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_c7a7f12a04a11206.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_45d79998ca916667.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_ebb8978331ef8f5d.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
    ] {
        assert_eq!(
            evaluate_expression(source, &environment, Limits::default()).unwrap(),
            expected,
            "{source}"
        );
    }
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_0c3ecdd86bab933d.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::new(Raw::Array(vec![
            Raw::Map(vec![
                (Raw::Text("key".into()), earlier.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("earlier".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), equal.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("first".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), equal.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("second".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), later.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("later".into())),
            ]),
            Raw::Map(vec![
                (Raw::Text("key".into()), huge.raw().clone()),
                (Raw::Text("label".into()), Raw::Text("huge".into())),
            ]),
        ]))
        .unwrap()
    );
}

#[test]
fn canonical_duration_ranges_reject_mixed_endpoint_and_member_types() {
    let duration = duration(0, 0);
    let mixed_range = Value::new(Raw::Tag(
        60019,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), duration.raw().clone()])),
            ),
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![
                    Raw::Int(1.into()),
                    Raw::Tag(
                        60002,
                        Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
                    ),
                ])),
            ),
            Raw::Bool(false),
        ])),
    ))
    .unwrap();
    let environment = Environment::from([
        ("duration".into(), duration),
        (
            "date_range".into(),
            date_range(Some("2024-01-01"), Some("2024-01-02"), false),
        ),
        ("mixed_range".into(), mixed_range),
    ]);
    for source in [
        include_str!("fixtures/evaluator_source_baae631b3a2eb1dd.orna"),
        include_str!("fixtures/evaluator_source_847b6bf3a51b1f6c.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(source, &environment, Limits::default())),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
}

#[test]
fn range_ordering_validates_unbounded_endpoint_types_before_lexicographic_ordering() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_a9dd4ab2ebb7621b.orna"
        )),
        Value::new(Raw::Bool(true)).unwrap()
    );
    for source in [
        include_str!("fixtures/evaluator_source_12ddae725578e980.orna"),
        include_str!("fixtures/evaluator_source_71099363f6aaf007.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                source,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-TYPE",
            "{source}"
        );
    }
}

fn object_id(byte: u8) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(vec![byte; 16])))
}

fn enum_value(type_id: u8, variant_id: u8, payload: Option<Raw>) -> Value {
    Value::new(Raw::Tag(
        60008,
        Box::new(Raw::Array(vec![
            object_id(type_id),
            object_id(variant_id),
            payload.unwrap_or(Raw::Null),
        ])),
    ))
    .unwrap()
}

fn record_payload(fields: Vec<(&str, Raw)>) -> Raw {
    Raw::Tag(
        60009,
        Box::new(Raw::Array(vec![
            Raw::Null,
            Raw::Array(
                fields
                    .into_iter()
                    .map(|(name, value)| Raw::Array(vec![Raw::Text(name.into()), value]))
                    .collect(),
            ),
        ])),
    )
}

#[test]
fn matches_enum_labels_payload_fields_and_interpolates_bound_strings() {
    let ready = enum_value(1, 2, None);
    let waiting_label = enum_value(1, 3, None);
    let waiting = enum_value(
        1,
        3,
        Some(record_payload(vec![(
            "reason",
            Raw::Text("maintenance".into()),
        )])),
    );
    let mut environment = Environment::from([
        ("Availability.ready".into(), ready.clone()),
        ("Availability.waiting".into(), waiting_label),
        ("value".into(), waiting),
    ]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_413bc39b3304f34d.orna"),
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("waiting: maintenance".into())).unwrap()
    );

    environment.insert("value".into(), ready);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_be5de2c35fc3a37d.orna"),
            &environment,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("ready".into())).unwrap()
    );
}

#[test]
fn matches_qualified_enum_patterns_from_retained_definitions_without_scope_sentinels() {
    let waiting = enum_value(
        1,
        3,
        Some(record_payload(vec![(
            "reason",
            Raw::Text("maintenance".into()),
        )])),
    );
    let definitions = NominalDefinitions::from([(
        "Availability".into(),
        NominalDefinition::new([1; 16], None, Vec::new()).with_enum_variants(vec![
            NominalVariant::new([2; 16], "ready"),
            NominalVariant::new([3; 16], "waiting"),
        ]),
    )]);
    let mut environment = Environment::from([("value".into(), waiting)]);
    let source = include_str!("fixtures/qualified_enum_retained_definition.orna").trim();
    assert_eq!(
        evaluate_parsed_with_nominals(
            &parsed_expression(source),
            &environment,
            &definitions,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("waiting: maintenance".into())).unwrap()
    );

    environment.insert("value".into(), enum_value(1, 2, None));
    assert_eq!(
        evaluate_parsed_with_nominals(
            &parsed_expression(source),
            &environment,
            &definitions,
            Limits::default(),
        )
        .unwrap(),
        Value::new(Raw::Text("ready".into())).unwrap()
    );
}

#[test]
fn matches_tagged_optional_some_and_null() {
    let mut environment = Environment::from([(
        "value".into(),
        Value::option(Some(Value::new(Raw::Text("Kieran".into())).unwrap())).unwrap(),
    )]);
    let source = include_str!("fixtures/optional_some_null_case.orna").trim();
    assert_eq!(
        evaluate_expression(source, &environment, Limits::default()).unwrap(),
        Value::new(Raw::Text("Kieran".into())).unwrap()
    );

    environment.insert("value".into(), Value::option(None).unwrap());
    assert_eq!(
        evaluate_expression(source, &environment, Limits::default()).unwrap(),
        Value::new(Raw::Text("anonymous".into())).unwrap()
    );
}

#[test]
fn rejects_fail_closed_cases_with_redacted_stable_diagnostics() {
    for (source, expected) in [
        (
            include_str!("fixtures/evaluator_source_b23a6a8439c0dde5.orna"),
            "ORNA-EVAL-NAME",
        ),
        (
            include_str!("fixtures/evaluator_source_7e396f4b251c0514.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_05c5b675fbc33104.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_95baf036f1aa2733.orna"),
            "ORNA-EVAL-DIVIDE-BY-ZERO",
        ),
        (
            include_str!("fixtures/evaluator_source_ce7a8a581a60377d.orna"),
            "ORNA-EVAL-FIELD",
        ),
        (
            include_str!("fixtures/evaluator_source_2e5cb14953ad13e4.orna"),
            "ORNA-EVAL-INDEX",
        ),
        (
            include_str!("fixtures/evaluator_source_3278e80eb66631a7.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_80258245d0a9b932.orna"),
            "ORNA-EVAL-NO-MATCH",
        ),
        (
            include_str!("fixtures/evaluator_source_4f3515ce9683c9d5.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_1660bcc227f6599f.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_9d6dcf083f463276.orna"),
            "ORNA-EVAL-TYPE",
        ),
        (
            include_str!("fixtures/evaluator_source_0810eaf259d000ed.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_df2e02a07ce9bdf5.orna"),
            "ORNA-EVAL-PARSE",
        ),
        (
            include_str!("fixtures/evaluator_source_cbba69020046b4f6.orna"),
            "ORNA-EVAL-PARSE",
        ),
    ] {
        let failure =
            evaluate_expression(source, &Environment::new(), Limits::default()).unwrap_err();
        assert_eq!(failure.code(), expected, "{source}");
        assert_eq!(failure.diagnostic().message(), "<redacted>");
        assert!(!failure.diagnostic().message().contains(source));
    }
}

#[test]
fn rejects_resource_limits_before_work_can_expand() {
    let limits = Limits {
        max_steps: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_6212702c7a0d68f0.orna"),
            &Environment::new(),
            limits
        )),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_source_bytes: 3,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_03ac674216f3e15c.orna"),
            &Environment::new(),
            limits
        )),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_collection_items: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_3a316d6d3226f84c.orna"),
            &Environment::new(),
            limits
        )),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_collection_items: 1,
        ..Limits::default()
    };
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_795850976cd7236e.orna"),
            &Environment::new(),
            limits
        )),
        "ORNA-EVAL-LIMIT"
    );
    let limits = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    let environment = Environment::from([(
        "value".into(),
        Value::option(Some(Value::new(Raw::Text("nested".into())).unwrap())).unwrap(),
    )]);
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_ca45b0406798305e.orna"),
            &environment,
            limits,
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn std_stats_mean_returns_empty_null_and_preserves_exact_or_explicitly_rounded_results() {
    let null = Value::new(Raw::Null).unwrap();
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_5ce1ea49ff9f7388.orna"
        )),
        null
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_8136ba66c3ed6932.orna"
        )),
        Value::decimal(15.into(), (-1).into()).unwrap()
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_61b54f6fbf5535ff.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_ec093eadaab3566d.orna"
        )),
        Value::decimal(233.into(), (-2).into()).unwrap()
    );

    let expected_float = Value::float_bits((1.0f64 / 3.0f64).to_bits());
    let environment = Environment::from([(
        "rows".into(),
        float_rows(&[
            10_000_000_000_000_000.0f64.to_bits(),
            (-10_000_000_000_000_000.0f64).to_bits(),
            1.0f64.to_bits(),
        ]),
    )]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_c730b9493cb43f5e.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        expected_float
    );
    let singleton = Environment::from([("rows".into(), float_rows(&[(-0.0f64).to_bits()]))]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_c730b9493cb43f5e.orna"),
            &singleton,
            Limits::default()
        )
        .unwrap(),
        Value::float_bits((-0.0f64).to_bits())
    );
}

#[test]
fn std_stats_median_sorts_by_total_order_and_rounds_only_when_requested() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_44e880a28b6b3112.orna"
        )),
        Value::int(5.into())
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_c20db018952d4343.orna"
        )),
        Value::decimal(15.into(), (-1).into()).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_d6877dfd9563475a.orna"
        )),
        Value::decimal(2.into(), 0.into()).unwrap()
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_c97b63d7e502afc2.orna"
        )),
        Value::decimal(3.into(), 0.into()).unwrap()
    );

    let environment = Environment::from([(
        "rows".into(),
        float_rows(&[
            CANONICAL_NAN_BITS,
            (-0.0f64).to_bits(),
            0.0f64.to_bits(),
            (-1.0f64).to_bits(),
            1.0f64.to_bits(),
        ]),
    )]);
    assert_eq!(
        evaluate_expression(
            include_str!("fixtures/evaluator_source_96b31fe5eeb266d4.orna"),
            &environment,
            Limits::default()
        )
        .unwrap(),
        Value::float_bits(0.0f64.to_bits())
    );
}

#[test]
fn std_stats_percentile_requires_bounded_probability_and_named_supported_interpolation() {
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_b8564530970c8a7b.orna"),
            Value::decimal(0.into(), 0.into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_86ea486df697eeb7.orna"),
            Value::decimal(10.into(), 0.into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_dcbc043ab1daf19b.orna"),
            Value::decimal(25.into(), (-1).into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_2fb7cb7433120b10.orna"),
            Value::decimal(0.into(), 0.into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_525ee7faf2ff8e67.orna"),
            Value::decimal(10.into(), 0.into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_33219a840870a094.orna"),
            Value::decimal(0.into(), 0.into()).unwrap(),
        ),
        (
            include_str!("fixtures/evaluator_source_3b528646d5f395d4.orna"),
            Value::decimal(5.into(), 0.into()).unwrap(),
        ),
    ] {
        assert_eq!(evaluate(&expression), expected, "{expression}");
    }
    for expression in [
        include_str!("fixtures/evaluator_source_874ed7ee373c2e4d.orna"),
        include_str!("fixtures/evaluator_source_fbf78c6bdefe1338.orna"),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            "ORNA-EVAL-VALUE",
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_fa644b33178a5b6d.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-UNSUPPORTED"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_0f490f95498d73f7.orna"),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-VALUE"
    );
}
#[test]
fn union_relations_preserve_declared_order_duplicates_and_bounded_terminals() {
    let union = relation_union(
        relation_source_expression("Left"),
        relation_source_expression("Right"),
    );
    let left = vec![
        Value::int(1.into()),
        Value::int(2.into()),
        Value::int(2.into()),
    ];
    let right = vec![Value::int(2.into()), Value::int(3.into())];

    let mut effects = UnionRelationEffects::new(left.clone(), right.clone());
    assert_eq!(
        invoke_relation(
            relation_terminal(union.clone(), "count"),
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::int(5.into()),
        "union count must retain duplicates from both relations"
    );
    assert_eq!(
        effects.cursors,
        vec![
            ("Left".into(), None),
            ("Left".into(), Some(vec![1])),
            ("Left".into(), Some(vec![2])),
            ("Right".into(), None),
            ("Right".into(), Some(vec![1])),
        ],
        "union must scan all left pages before any right page"
    );

    let mut effects = UnionRelationEffects::new(left.clone(), right.clone());
    assert_eq!(
        invoke_relation(
            relation_terminal(union.clone(), "first"),
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(Value::int(1.into()))).expect("option is canonical")
    );
    assert_eq!(effects.cursors, vec![("Left".into(), None)]);

    let mut effects = UnionRelationEffects::new(left.clone(), right.clone());
    assert_eq!(
        invoke_relation(
            relation_terminal(union.clone(), "last"),
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(Value::int(3.into()))).expect("option is canonical")
    );
    assert_eq!(effects.cursors.len(), 5);

    let dropped_left_duplicate = relation_stage(union.clone(), "drop", vec![relation_integer(2)]);
    let mut effects = UnionRelationEffects::new(left.clone(), right.clone());
    assert_eq!(
        invoke_relation(
            relation_terminal(dropped_left_duplicate, "first"),
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(Value::int(2.into()))).expect("option is canonical")
    );
    assert_eq!(
        effects.cursors,
        vec![
            ("Left".into(), None),
            ("Left".into(), Some(vec![1])),
            ("Left".into(), Some(vec![2])),
        ],
        "drop must preserve the left duplicate and avoid the right relation"
    );

    let taken = relation_stage(union, "take", vec![relation_integer(2)]);
    let mut effects = UnionRelationEffects::new(left, right);
    assert_eq!(
        invoke_relation(
            relation_terminal(taken, "count"),
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::int(2.into())
    );
    assert_eq!(
        effects.cursors,
        vec![("Left".into(), None), ("Left".into(), Some(vec![1])),],
        "take must stop before scanning the right relation"
    );
}

#[test]
fn cloned_filter_continuations_do_not_promote_between_equal_named_unknowns() {
    let body = parsed_expression(include_str!(
        "fixtures/query-cloned-continuation-unknowns-9322q.orna"
    ));
    let mut effects = RepeatedUnknownRelationEffects {
        rows: vec![Value::int(1.into()), Value::int(2.into()), Value::int(3.into())],
        starts: 0,
    };

    let result = invoke_relation(body, &mut effects, Limits::default())
        .unwrap_or_else(|error| panic!("the chained filter query computes its count: {}", error.code()));

    assert_eq!(result, Value::int(2.into()));
    assert_eq!(effects.starts, 3, "all three unknown operands contribute");
}

#[test]
fn materialized_list_union_remains_left_to_right_with_duplicates() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/evaluator_source_96e313a524b19b8b.orna"
        )),
        Value::new(Raw::Array(vec![
            Raw::Int(1.into()),
            Raw::Int(2.into()),
            Raw::Int(2.into()),
            Raw::Int(3.into()),
        ]))
        .unwrap()
    );
}

#[test]
fn std_stats_reject_mixed_or_unsupported_inputs_and_resource_overflow() {
    assert_eq!(
        evaluate(include_str!("fixtures/evaluator_source_0b99861bb1316192.orna")),
        Value::decimal(15.into(), (-1).into()).unwrap()
    );
    for (expression, expected) in [
        (
            include_str!("fixtures/evaluator_source_91637a611b83fef1.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_ac764ba2e637ad08.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_a4740917f723fec5.orna"),
            "ORNA-EVAL-UNSUPPORTED",
        ),
        (
            include_str!("fixtures/evaluator_source_5ae40e0510743923.orna"),
            "ORNA-EVAL-TYPE",
        ),
    ] {
        assert_eq!(
            code(evaluate_expression(
                expression,
                &Environment::new(),
                Limits::default(),
            )),
            expected,
            "{expression}"
        );
    }
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_dd01296415a1eae8.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_2978c62ea9df14f5.orna"),
            &Environment::new(),
            Limits {
                max_collection_items: 2,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        code(evaluate_expression(
            include_str!("fixtures/evaluator_source_4841024f6177b651.orna"),
            &Environment::new(),
            Limits {
                max_steps: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn distinct_relation_preserves_first_occurrence_order_and_stays_lazy() {
    let source = relation_source_expression("Note");
    let distinct = relation_stage(source, "distinct", Vec::new());
    let dropped = relation_stage(distinct, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(dropped, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(2.into()),
        Value::int(1.into()),
        Value::int(2.into()),
    ]);

    let result = invoke_relation(body, &mut effects, Limits::default())
        .expect("distinct relation should evaluate");

    assert_eq!(
        result,
        Value::option(Some(Value::int(1.into()))).expect("option is canonical")
    );
    assert_eq!(effects.cursors, vec![None, Some(vec![1])]);
}

#[test]
fn distinct_relation_eliminates_duplicates_and_handles_empty_input() {
    let source = relation_source_expression("Note");
    let distinct = relation_stage(source, "distinct", Vec::new());
    let body = relation_terminal(distinct, "count");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(2.into()),
        Value::int(1.into()),
        Value::int(2.into()),
        Value::int(1.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(2.into())
    );

    let source = relation_source_expression("Empty");
    let distinct = relation_stage(source, "distinct", Vec::new());
    let body = relation_terminal(distinct, "count");
    let mut empty = DistinctRelationEffects::new(Vec::new());
    assert_eq!(
        invoke_relation(body, &mut empty, Limits::default()).unwrap(),
        Value::int(0.into())
    );
    assert_eq!(empty.cursors, vec![None]);
}

#[test]
fn distinct_relation_preserves_first_identity_across_sparse_filter_cascade_union() {
    let body = parsed_expression(include_str!(
        "fixtures/query-distinct-sparse-filter-cascade-5lya7.orna"
    ));
    let ints = |values: &[i64]| {
        values
            .iter()
            .copied()
            .map(|value| Value::int(value.into()))
            .collect()
    };
    let mut effects = UnionRelationEffects {
        rows: BTreeMap::from([
            ("sys.Storage".into(), ints(&[2, 1, 2, 4, 9])),
            ("sys.MaintenanceJob".into(), ints(&[2, 3, 4, 5, 6])),
        ]),
        cursors: Vec::new(),
    };

    let result = invoke_relation(body, &mut effects, Limits::default())
        .unwrap_or_else(|error| panic!("sparse distinct cascade failed: {}", error.code()));

    let first_pair = Value::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![Raw::Int(2.into()), Raw::Int(4.into())])),
    ))
    .expect("first pair is a canonical tuple");
    let expected = Value::new(Raw::Array(vec![
        Raw::Int(4.into()),
        Value::option(Some(first_pair))
            .expect("first pair is a canonical option")
            .raw()
            .clone(),
        Value::option(Some(Value::int(3.into())))
            .expect("third surviving identity is a canonical option")
            .raw()
            .clone(),
    ]))
    .expect("proof result is canonical");
    assert_eq!(
        result,
        expected,
        "the fold keeps the first surviving value for each identity across both union branches"
    );
}

#[test]
fn distinct_relation_take_zero_short_circuits_source_scanning() {
    let source = relation_source_expression("Note");
    let distinct = relation_stage(source, "distinct", Vec::new());
    let taken = relation_stage(distinct, "take", vec![relation_integer(0)]);
    let body = relation_terminal(taken, "count");
    let mut effects =
        DistinctRelationEffects::new(vec![Value::int(1.into()), Value::int(2.into())]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(0.into())
    );
    assert!(effects.cursors.is_empty());
}

#[test]
fn distinct_relation_respects_step_budget_and_item_limit() {
    let source = relation_source_expression("Note");
    let distinct = relation_stage(source, "distinct", Vec::new());
    let body = relation_terminal(distinct, "count");
    let functions = relation_function(body.clone());
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    let mut budget = StepBudget::new(1);
    assert_eq!(
        code(invoke_named_with_effects_and_budget(
            "run",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
            &mut budget,
        )),
        "ORNA-EVAL-LIMIT"
    );

    let mut effects =
        DistinctRelationEffects::new(vec![Value::int(1.into()), Value::int(2.into())]);
    assert_eq!(
        code(invoke_relation(
            body,
            &mut effects,
            Limits {
                max_collection_items: 1,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn distinct_relation_rejects_float_equality() {
    let source = relation_source_expression("FloatRows");
    let distinct = relation_stage(source, "distinct", Vec::new());
    let body = relation_terminal(distinct, "first");
    let mut effects = DistinctRelationEffects::new(vec![Value::float_bits(1.0f64.to_bits())]);

    assert_eq!(
        code(invoke_relation(body, &mut effects, Limits::default())),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn distinct_relation_preserves_sorted_buffered_order() {
    let source = relation_source_expression("Note");
    let sorted = relation_stage(
        source,
        "sort_by",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_82249d21a2916b18.orna"
        ))],
    );
    let distinct = relation_stage(sorted, "distinct", Vec::new());
    let dropped = relation_stage(distinct, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(dropped, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(2.into()),
        Value::int(1.into()),
        Value::int(2.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(Value::int(2.into()))).expect("option is canonical")
    );
    assert_eq!(effects.cursors, vec![None, Some(vec![1]), Some(vec![2])]);
}

fn relation_named_stage(input: Expr, name: &str, argument_name: &str) -> Expr {
    let span = relation_span();
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: name.into(),
            span: span.clone(),
        }),
        arguments: vec![orna_syntax_v1::Argument {
            name: Some(argument_name.into()),
            value: input,
            span: span.clone(),
        }],
        span,
    }
}

#[test]
fn pairs_relation_preserves_sorted_buffered_order_and_overlap() {
    let source = relation_source_expression("Note");
    let sorted = relation_stage(
        source,
        "sort_by",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_82249d21a2916b18.orna"
        ))],
    );
    let pairs = relation_stage(sorted, "pairs", Vec::new());
    let second_pair = relation_stage(pairs, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(second_pair, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(3.into()),
        Value::int(1.into()),
        Value::int(2.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_pair(2, 3))).expect("option is canonical")
    );
}

#[test]
fn pairs_relation_accepts_a_named_rows_relation_argument() {
    let source = relation_source_expression("Note");
    let pairs = relation_named_stage(source, "pairs", "rows");
    let body = relation_terminal(pairs, "count");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(1.into()),
        Value::int(2.into()),
        Value::int(3.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(2.into())
    );
}

#[test]
fn pairs_relation_preserves_upstream_order_and_adjacent_overlapping_tuples() {
    let source = relation_source_expression("Note");
    let pairs = relation_stage(source, "pairs", Vec::new());
    let second_pair = relation_stage(pairs, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(second_pair, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(1.into()),
        Value::int(2.into()),
        Value::int(3.into()),
    ]);

    let result = invoke_relation(body, &mut effects, Limits::default())
        .expect("pairs relation should evaluate");

    assert_eq!(
        result,
        Value::option(Some(relation_pair(2, 3))).expect("option is canonical")
    );
}

#[test]
fn pairs_relation_empty_and_one_row_inputs_produce_no_pairs() {
    for rows in [Vec::new(), vec![Value::int(7.into())]] {
        let source = relation_source_expression("Note");
        let pairs = relation_stage(source, "pairs", Vec::new());
        let body = relation_terminal(pairs, "count");
        let mut effects = DistinctRelationEffects::new(rows);

        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            Value::int(0.into())
        );
    }
}

#[test]
fn pairs_relation_first_short_circuits_after_bounded_lookahead() {
    let source = relation_source_expression("Note");
    let pairs = relation_stage(source, "pairs", Vec::new());
    let body = relation_terminal(pairs, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(1.into()),
        Value::int(2.into()),
        Value::int(3.into()),
        Value::int(4.into()),
    ]);

    let result =
        invoke_relation(body, &mut effects, Limits::default()).expect("first pair should evaluate");

    assert_eq!(
        result,
        Value::option(Some(relation_pair(1, 2))).expect("option is canonical")
    );
    assert_eq!(
        effects.cursors.len(),
        2,
        "first pair needs only one adjacent lookahead"
    );
}

#[test]
fn pairs_relation_take_bounds_consumption_and_take_zero_skips_source() {
    let source = relation_source_expression("Note");
    let pairs = relation_stage(source, "pairs", Vec::new());
    let taken = relation_stage(pairs, "take", vec![relation_integer(1)]);
    let body = relation_terminal(taken, "count");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(1.into()),
        Value::int(2.into()),
        Value::int(3.into()),
        Value::int(4.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(1.into())
    );
    assert_eq!(
        effects.cursors.len(),
        2,
        "take(1) must not scan beyond the first pair"
    );

    let source = relation_source_expression("Note");
    let pairs = relation_stage(source, "pairs", Vec::new());
    let taken = relation_stage(pairs, "take", vec![relation_integer(0)]);
    let body = relation_terminal(taken, "count");
    let mut effects =
        DistinctRelationEffects::new(vec![Value::int(1.into()), Value::int(2.into())]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(0.into())
    );
    assert!(
        effects.cursors.is_empty(),
        "take(0) must short-circuit before relation scanning"
    );
}

fn relation_named_flat_map_stage(input: Expr, transform: Expr) -> Expr {
    let span = relation_span();
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: "flat_map".into(),
            span: span.clone(),
        }),
        arguments: vec![
            orna_syntax_v1::Argument {
                name: Some("transform".into()),
                value: transform,
                span: span.clone(),
            },
            orna_syntax_v1::Argument {
                name: Some("rows".into()),
                value: input,
                span: span.clone(),
            },
        ],
        span,
    }
}

#[test]
fn flat_map_relation_invokes_named_callback_once_per_row_and_preserves_inner_order() {
    let source = relation_source_expression("Note");
    let transform = parsed_expression(include_str!(
        "fixtures/evaluator_source_e177aad7ddb461a2.orna"
    ));
    let flat_map = relation_named_flat_map_stage(source, transform);
    let body = relation_terminal(flat_map, "count");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(3.into()),
        Value::int(1.into()),
        Value::int(2.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(6.into())
    );
    assert_eq!(
        effects.cursors.len(),
        3,
        "the callback runs once for each of the three source rows"
    );

    let source = relation_source_expression("Note");
    let transform = parsed_expression(include_str!(
        "fixtures/evaluator_source_e177aad7ddb461a2.orna"
    ));
    let flat_map = relation_named_flat_map_stage(source, transform);
    let pairs = relation_stage(flat_map, "pairs", Vec::new());
    let second_pair = relation_stage(pairs, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(second_pair, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(3.into()),
        Value::int(1.into()),
        Value::int(2.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_pair(13, 1))).expect("option is canonical")
    );
}
#[test]
fn flat_map_relation_allows_empty_inner_lists_without_skipping_source_pages() {
    let source = relation_source_expression("Note");
    let flat_map = relation_stage(
        source,
        "flat_map",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_a22329b7637f1b04.orna"
        ))],
    );
    let body = relation_terminal(flat_map, "count");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(4.into()),
        Value::int(5.into()),
        Value::int(6.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(0.into())
    );
    assert_eq!(
        effects.cursors.len(),
        3,
        "empty inner output still scans each source page"
    );
}

#[test]
fn flat_map_relation_rejects_non_list_and_unsupported_inner_outputs() {
    let source = relation_source_expression("Note");
    let flat_map = relation_named_flat_map_stage(
        source,
        parsed_expression(include_str!(
            "fixtures/evaluator_source_82249d21a2916b18.orna"
        )),
    );
    let body = relation_terminal(flat_map, "count");
    let mut effects =
        DistinctRelationEffects::new(vec![Value::int(1.into()), Value::int(2.into())]);
    assert_eq!(
        code(invoke_relation(body, &mut effects, Limits::default())),
        "ORNA-EVAL-TYPE"
    );

    let source = relation_source_expression("Note");
    let flat_map = relation_named_flat_map_stage(
        source,
        parsed_expression(include_str!(
            "fixtures/evaluator_source_b9968e971e15a2e3.orna"
        )),
    );
    let body = relation_terminal(flat_map, "first");
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    assert_eq!(
        code(invoke_relation(body, &mut effects, Limits::default())),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn flat_map_relation_take_short_circuits_inner_and_upstream_consumption() {
    let source = relation_source_expression("Note");
    let flat_map = relation_stage(
        source,
        "flat_map",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_e177aad7ddb461a2.orna"
        ))],
    );
    let taken = relation_stage(flat_map, "take", vec![relation_integer(2)]);
    let body = relation_terminal(taken, "count");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(3.into()),
        Value::int(1.into()),
        Value::int(2.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(2.into())
    );
    assert_eq!(
        effects.cursors,
        vec![None],
        "take must stop before the next source page after the inner bound"
    );
}

#[test]
fn flat_map_relation_preserves_sorted_buffered_order() {
    let source = relation_source_expression("Note");
    let sorted = relation_stage(
        source,
        "sort_by",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_82249d21a2916b18.orna"
        ))],
    );
    let flat_map = relation_stage(
        sorted,
        "flat_map",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_e177aad7ddb461a2.orna"
        ))],
    );
    let pairs = relation_stage(flat_map, "pairs", Vec::new());
    let body = relation_terminal(pairs, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(2.into()),
        Value::int(3.into()),
        Value::int(1.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_pair(1, 11))).expect("option is canonical")
    );
}

#[test]
fn window_relation_preserves_overlap_and_omits_trailing_partial_windows() {
    let source = relation_source_expression("Note");
    let windows = relation_stage(
        source,
        "window",
        vec![relation_integer(3), relation_integer(1)],
    );
    let second = relation_stage(windows, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(second, "first");
    let mut effects =
        DistinctRelationEffects::new((1..=5).map(|value| Value::int(value.into())).collect());

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_window_row(&[2, 3, 4]))).expect("option is canonical")
    );

    let source = relation_source_expression("Note");
    let windows = relation_stage(
        source,
        "window",
        vec![relation_integer(3), relation_integer(2)],
    );
    let body = relation_terminal(windows, "count");
    let mut effects =
        DistinctRelationEffects::new((1..=5).map(|value| Value::int(value.into())).collect());
    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(2.into()),
        "the trailing [5] suffix is not emitted as a partial window"
    );
}

#[test]
fn window_relation_handles_gaps_without_partial_windows() {
    let source = relation_source_expression("Note");
    let windows = relation_stage(
        source,
        "window",
        vec![relation_integer(2), relation_integer(3)],
    );
    let second = relation_stage(windows, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(second, "first");
    let mut effects =
        DistinctRelationEffects::new((1..=7).map(|value| Value::int(value.into())).collect());

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_window_row(&[4, 5]))).expect("option is canonical")
    );

    let source = relation_source_expression("Note");
    let windows = relation_stage(
        source,
        "window",
        vec![relation_integer(2), relation_integer(3)],
    );
    let body = relation_terminal(windows, "count");
    let mut effects =
        DistinctRelationEffects::new((1..=7).map(|value| Value::int(value.into())).collect());
    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(2.into()),
        "the gap after [4, 5] must not create a partial [7] window"
    );
}

#[test]
fn window_relation_empty_and_single_inputs_emit_only_complete_windows() {
    for (rows, size, expected_count) in [
        (Vec::new(), 2, 0),
        (vec![Value::int(7.into())], 2, 0),
        (vec![Value::int(7.into())], 1, 1),
    ] {
        let source = relation_source_expression("Note");
        let windows = relation_stage(source, "window", vec![relation_integer(size)]);
        let body = relation_terminal(windows, "count");
        let mut effects = DistinctRelationEffects::new(rows);

        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            Value::int(expected_count.into())
        );
    }
}

#[test]
fn window_relation_first_and_bounded_count_stop_after_required_source_pages() {
    let source = relation_source_expression("Note");
    let windows = relation_stage(source, "window", vec![relation_integer(3)]);
    let body = relation_terminal(windows, "first");
    let rows = (1..=100)
        .map(|value| Value::int(value.into()))
        .collect::<Vec<_>>();
    let mut effects = DistinctRelationEffects::new(rows);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_window_row(&[1, 2, 3]))).expect("option is canonical")
    );
    assert_eq!(
        effects.cursors.len(),
        3,
        "first window only needs its three source pages"
    );

    let source = relation_source_expression("Note");
    let windows = relation_stage(source, "window", vec![relation_integer(3)]);
    let taken = relation_stage(windows, "take", vec![relation_integer(1)]);
    let body = relation_terminal(taken, "count");
    let rows = (1..=100)
        .map(|value| Value::int(value.into()))
        .collect::<Vec<_>>();
    let mut effects = DistinctRelationEffects::new(rows);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::int(1.into())
    );
    assert_eq!(
        effects.cursors.len(),
        3,
        "count over take(1) must not scan a second window"
    );
}

#[test]
fn window_relation_accepts_direct_named_and_pipeline_argument_shapes() {
    let cases = [
        relation_window_direct(
            relation_source_expression("Note"),
            relation_integer(2),
            Some(relation_integer(2)),
        ),
        relation_named_window_direct(
            relation_source_expression("Note"),
            relation_integer(2),
            Some(relation_integer(2)),
        ),
        relation_named_window_pipeline(
            relation_source_expression("Note"),
            relation_integer(2),
            Some(relation_integer(2)),
        ),
    ];

    for window in cases {
        let body = relation_terminal(window, "count");
        let mut effects =
            DistinctRelationEffects::new((1..=4).map(|value| Value::int(value.into())).collect());
        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            Value::int(2.into())
        );
    }
}

#[test]
fn window_relation_rejects_non_positive_and_non_integer_sizes() {
    for (body, expected) in [
        (
            relation_terminal(
                relation_window_direct(
                    relation_source_expression("Note"),
                    relation_integer(0),
                    None,
                ),
                "count",
            ),
            "ORNA-EVAL-VALUE",
        ),
        (
            relation_terminal(
                relation_window_direct(
                    relation_source_expression("Note"),
                    relation_integer(-1),
                    None,
                ),
                "count",
            ),
            "ORNA-EVAL-VALUE",
        ),
        (
            relation_terminal(
                relation_window_direct(
                    relation_source_expression("Note"),
                    relation_integer(2),
                    Some(relation_integer(0)),
                ),
                "count",
            ),
            "ORNA-EVAL-VALUE",
        ),
        (
            relation_terminal(
                relation_window_direct(
                    relation_source_expression("Note"),
                    relation_integer(2),
                    Some(relation_integer(-1)),
                ),
                "count",
            ),
            "ORNA-EVAL-VALUE",
        ),
        (
            relation_terminal(
                relation_window_direct(
                    relation_source_expression("Note"),
                    parsed_expression(include_str!(
                        "fixtures/evaluator_source_b85d38dc71a80277.orna"
                    )),
                    None,
                ),
                "count",
            ),
            "ORNA-EVAL-TYPE",
        ),
        (
            relation_terminal(
                relation_window_direct(
                    relation_source_expression("Note"),
                    relation_integer(2),
                    Some(parsed_expression(include_str!(
                        "fixtures/evaluator_source_097a4dfb7f430ff3.orna"
                    ))),
                ),
                "count",
            ),
            "ORNA-EVAL-TYPE",
        ),
    ] {
        let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
        assert_eq!(
            code(invoke_relation(body, &mut effects, Limits::default())),
            expected
        );
    }
}

#[test]
fn window_relation_preserves_sorted_buffered_order() {
    let source = relation_source_expression("Note");
    let sorted = relation_stage(
        source,
        "sort_by",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_82249d21a2916b18.orna"
        ))],
    );
    let windows = relation_stage(
        sorted,
        "window",
        vec![relation_integer(2), relation_integer(2)],
    );
    let second = relation_stage(windows, "drop", vec![relation_integer(1)]);
    let body = relation_terminal(second, "first");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(3.into()),
        Value::int(1.into()),
        Value::int(2.into()),
        Value::int(4.into()),
        Value::int(5.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_window_row(&[3, 4]))).expect("option is canonical")
    );
}
fn relation_direct_stage(input: Expr, name: &str) -> Expr {
    let span = relation_span();
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: name.into(),
            span: span.clone(),
        }),
        arguments: vec![relation_argument(input, &span)],
        span,
    }
}

#[test]
fn last_relation_returns_the_final_value_through_filter_map_flat_map_sort_and_window() {
    let source = relation_source_expression("Note");
    let filtered = relation_stage(
        source,
        "filter",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_551da618a973da06.orna"
        ))],
    );
    let mapped = relation_stage(
        filtered,
        "map",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_54e7140f994de648.orna"
        ))],
    );
    let expanded = relation_stage(
        mapped,
        "flat_map",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_fdabca8ea2f4552c.orna"
        ))],
    );
    let sorted = relation_stage(
        expanded,
        "sort_by",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_82249d21a2916b18.orna"
        ))],
    );
    let windows = relation_stage(
        sorted,
        "window",
        vec![relation_integer(2), relation_integer(2)],
    );
    let body = relation_terminal(windows, "last");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(4.into()),
        Value::int(1.into()),
        Value::int(3.into()),
        Value::int(2.into()),
    ]);

    assert_eq!(
        invoke_relation(body, &mut effects, Limits::default()).unwrap(),
        Value::option(Some(relation_window_row(&[40, 41]))).expect("option is canonical")
    );
    assert_eq!(
        effects.cursors.len(),
        4,
        "sorted last must scan the complete upstream relation before returning the final window"
    );
}

#[test]
fn last_relation_scans_unsorted_pages_lazily_and_obeys_item_bounds() {
    let source = relation_source_expression("Note");
    let body = relation_terminal(source, "last");
    let mut effects =
        DistinctRelationEffects::new((1..=4).map(|value| Value::int(value.into())).collect());

    assert_eq!(
        invoke_relation(body.clone(), &mut effects, Limits::default()).unwrap(),
        Value::option(Some(Value::int(4.into()))).expect("option is canonical")
    );
    assert_eq!(
        effects.cursors,
        vec![None, Some(vec![1]), Some(vec![2]), Some(vec![3])],
        "unsorted last must advance one source page at a time without an eager source shortcut"
    );

    let mut bounded =
        DistinctRelationEffects::new((1..=4).map(|value| Value::int(value.into())).collect());
    assert_eq!(
        code(invoke_relation(
            body,
            &mut bounded,
            Limits {
                max_collection_items: 3,
                ..Limits::default()
            },
        )),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn last_relation_accepts_direct_pipeline_and_named_rows_forms_and_empty_inputs() {
    let cases = [
        relation_direct_stage(relation_source_expression("Note"), "last"),
        relation_named_stage(relation_source_expression("Note"), "last", "rows"),
        relation_terminal(relation_source_expression("Note"), "last"),
    ];
    for body in cases {
        let mut effects = DistinctRelationEffects::new(vec![
            Value::int(1.into()),
            Value::int(2.into()),
            Value::int(3.into()),
        ]);
        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            Value::option(Some(Value::int(3.into()))).expect("option is canonical")
        );
    }

    let empty_cases = [
        relation_direct_stage(relation_source_expression("Empty"), "last"),
        relation_named_stage(relation_source_expression("Empty"), "last", "rows"),
        relation_terminal(relation_source_expression("Empty"), "last"),
    ];
    for body in empty_cases {
        let mut effects = DistinctRelationEffects::new(Vec::new());
        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            Value::option(None).expect("option is canonical")
        );
    }
}

#[test]
fn last_relation_rejects_extra_and_unknown_arguments_like_first() {
    let span = relation_span();
    let source = relation_source_expression("Note");
    let extra = Expr::Call {
        callee: Box::new(Expr::Name {
            text: "last".into(),
            span: span.clone(),
        }),
        arguments: vec![
            relation_argument(source, &span),
            relation_argument(relation_integer(1), &span),
        ],
        span: span.clone(),
    };
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    assert_eq!(
        code(invoke_relation(extra, &mut effects, Limits::default())),
        "ORNA-EVAL-ARGUMENT"
    );

    let unknown = relation_named_stage(relation_source_expression("Note"), "last", "values");
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    assert_eq!(
        code(invoke_relation(unknown, &mut effects, Limits::default())),
        "ORNA-EVAL-ARGUMENT"
    );
}

fn relation_predicate_direct(input: Expr, name: &str, predicate: Expr) -> Expr {
    let span = relation_span();
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: name.into(),
            span: span.clone(),
        }),
        arguments: vec![
            relation_argument(input, &span),
            relation_argument(predicate, &span),
        ],
        span,
    }
}

fn relation_predicate_pipeline(input: Expr, name: &str, predicate: Expr) -> Expr {
    relation_stage(input, name, vec![predicate])
}

fn relation_predicate_named(input: Expr, name: &str, predicate: Expr) -> Expr {
    let span = relation_span();
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: name.into(),
            span: span.clone(),
        }),
        arguments: vec![
            orna_syntax_v1::Argument {
                name: Some("predicate".into()),
                value: predicate,
                span: span.clone(),
            },
            orna_syntax_v1::Argument {
                name: Some("rows".into()),
                value: input,
                span: span.clone(),
            },
        ],
        span,
    }
}

#[test]
fn relation_every_and_exists_accept_direct_pipeline_and_named_rows_forms() {
    let true_value = Value::new(Raw::Bool(true)).unwrap();
    let false_value = Value::new(Raw::Bool(false)).unwrap();
    for (name, predicate, expected) in [
        (
            "every",
            include_str!("fixtures/evaluator_source_5cef0e622322cf58.orna"),
            true_value.clone(),
        ),
        (
            "exists",
            include_str!("fixtures/evaluator_source_6201279a767dbda1.orna"),
            true_value.clone(),
        ),
    ] {
        let predicate = parsed_expression(predicate);
        let cases = [
            relation_predicate_direct(relation_source_expression("Note"), name, predicate.clone()),
            relation_predicate_pipeline(
                relation_source_expression("Note"),
                name,
                predicate.clone(),
            ),
            relation_predicate_named(relation_source_expression("Note"), name, predicate),
        ];
        for body in cases {
            let mut effects = DistinctRelationEffects::new(vec![
                Value::int(1.into()),
                Value::int(2.into()),
                Value::int(3.into()),
            ]);
            assert_eq!(
                invoke_relation(body, &mut effects, Limits::default()).unwrap(),
                expected,
                "{name} relation call form"
            );
        }
    }

    for (name, expected) in [("every", true_value), ("exists", false_value)] {
        let predicate = parsed_expression(include_str!(
            "fixtures/evaluator_source_5cef0e622322cf58.orna"
        ));
        let cases = [
            relation_predicate_direct(relation_source_expression("Empty"), name, predicate.clone()),
            relation_predicate_pipeline(
                relation_source_expression("Empty"),
                name,
                predicate.clone(),
            ),
            relation_predicate_named(relation_source_expression("Empty"), name, predicate),
        ];
        for body in cases {
            let mut effects = DistinctRelationEffects::new(Vec::new());
            assert_eq!(
                invoke_relation(body, &mut effects, Limits::default()).unwrap(),
                expected,
                "{name} empty relation identity"
            );
        }
    }
}

#[test]
fn relation_every_and_exists_short_circuit_before_later_pages() {
    let cases = [
        (
            "every",
            include_str!("fixtures/evaluator_source_c92dea66feb49791.orna"),
            Value::new(Raw::Bool(false)).unwrap(),
        ),
        (
            "exists",
            include_str!("fixtures/evaluator_source_ad184f0156e53bea.orna"),
            Value::new(Raw::Bool(true)).unwrap(),
        ),
    ];
    for (name, predicate, expected) in cases {
        let body = relation_predicate_direct(
            relation_source_expression("Note"),
            name,
            parsed_expression(predicate),
        );
        let mut effects = DistinctRelationEffects::new(vec![
            Value::int(1.into()),
            Value::int(2.into()),
            Value::int(3.into()),
        ]);
        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            expected,
            "{name} should stop after its decisive predicate"
        );
        assert_eq!(
            effects.cursors,
            vec![None],
            "{name} must not eagerly scan later relation pages"
        );
    }
}

#[test]
fn relation_sum_min_and_max_accept_all_argument_forms_and_supported_numeric_types() {
    let integer_rows = vec![
        Value::int(3.into()),
        Value::int((-1).into()),
        Value::int(2.into()),
    ];
    for (name, expected) in [
        ("sum", Value::int(4.into())),
        (
            "min",
            Value::option(Some(Value::int((-1).into()))).expect("option is canonical"),
        ),
        (
            "max",
            Value::option(Some(Value::int(3.into()))).expect("option is canonical"),
        ),
    ] {
        let cases = [
            relation_direct_stage(relation_source_expression("Note"), name),
            relation_terminal(relation_source_expression("Note"), name),
            relation_named_stage(relation_source_expression("Note"), name, "rows"),
        ];
        for body in cases {
            let mut effects = DistinctRelationEffects::new(integer_rows.clone());
            assert_eq!(
                invoke_relation(body, &mut effects, Limits::default()).unwrap(),
                expected,
                "{name} relation argument form"
            );
        }
    }

    let typed_cases = [
        (
            "sum",
            vec![
                evaluate(include_str!(
                    "fixtures/evaluator_source_004a9e0878ff83e6.orna"
                )),
                evaluate(include_str!(
                    "fixtures/evaluator_source_853a9a75b07e44c4.orna"
                )),
                evaluate(include_str!(
                    "fixtures/evaluator_source_058de5bc9a2d3f82.orna"
                )),
            ],
            evaluate(include_str!(
                "fixtures/evaluator_source_a97491cd3cfa6d9c.orna"
            )),
        ),
        (
            "min",
            vec![
                evaluate(include_str!(
                    "fixtures/evaluator_source_004a9e0878ff83e6.orna"
                )),
                evaluate(include_str!(
                    "fixtures/evaluator_source_853a9a75b07e44c4.orna"
                )),
                evaluate(include_str!(
                    "fixtures/evaluator_source_058de5bc9a2d3f82.orna"
                )),
            ],
            Value::option(Some(evaluate(include_str!(
                "fixtures/evaluator_source_058de5bc9a2d3f82.orna"
            ))))
            .expect("option is canonical"),
        ),
        (
            "max",
            vec![
                evaluate(include_str!(
                    "fixtures/evaluator_source_004a9e0878ff83e6.orna"
                )),
                evaluate(include_str!(
                    "fixtures/evaluator_source_853a9a75b07e44c4.orna"
                )),
                evaluate(include_str!(
                    "fixtures/evaluator_source_058de5bc9a2d3f82.orna"
                )),
            ],
            Value::option(Some(evaluate(include_str!(
                "fixtures/evaluator_source_853a9a75b07e44c4.orna"
            ))))
            .expect("option is canonical"),
        ),
        (
            "sum",
            vec![
                Value::float_bits(1.5f64.to_bits()),
                Value::float_bits((-2.0f64).to_bits()),
                Value::float_bits(0.25f64.to_bits()),
            ],
            Value::float_bits((-0.25f64).to_bits()),
        ),
        (
            "min",
            vec![
                Value::float_bits(1.5f64.to_bits()),
                Value::float_bits((-2.0f64).to_bits()),
                Value::float_bits(0.25f64.to_bits()),
            ],
            Value::option(Some(Value::float_bits((-2.0f64).to_bits())))
                .expect("option is canonical"),
        ),
        (
            "max",
            vec![
                Value::float_bits(1.5f64.to_bits()),
                Value::float_bits((-2.0f64).to_bits()),
                Value::float_bits(0.25f64.to_bits()),
            ],
            Value::option(Some(Value::float_bits(1.5f64.to_bits()))).expect("option is canonical"),
        ),
    ];
    for (name, rows, expected) in typed_cases {
        let body = relation_direct_stage(relation_source_expression("Typed"), name);
        let mut effects = DistinctRelationEffects::new(rows);
        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            expected,
            "{name} typed relation aggregate"
        );
    }
}

#[test]
fn relation_sum_min_and_max_use_empty_identities_and_preserve_item_bounds() {
    for (name, expected) in [
        ("sum", Value::int(0.into())),
        ("min", Value::new(Raw::Null).unwrap()),
        ("max", Value::new(Raw::Null).unwrap()),
    ] {
        for body in [
            relation_direct_stage(relation_source_expression("Empty"), name),
            relation_terminal(relation_source_expression("Empty"), name),
            relation_named_stage(relation_source_expression("Empty"), name, "rows"),
        ] {
            let mut effects = DistinctRelationEffects::new(Vec::new());
            assert_eq!(
                invoke_relation(body, &mut effects, Limits::default()).unwrap(),
                expected,
                "{name} empty identity"
            );
        }

        let body = relation_terminal(relation_source_expression("Note"), name);
        let mut effects = DistinctRelationEffects::new(vec![
            Value::int(1.into()),
            Value::int(2.into()),
            Value::int(3.into()),
        ]);
        assert_eq!(
            code(invoke_relation(
                body,
                &mut effects,
                Limits {
                    max_collection_items: 2,
                    ..Limits::default()
                },
            )),
            "ORNA-EVAL-LIMIT",
            "{name} must retain relation item bounds"
        );
    }
}

#[test]
fn relation_aggregates_support_sorted_transformed_suffixes_without_changing_order() {
    for (name, expected) in [
        ("sum", Value::int(204.into())),
        (
            "min",
            Value::option(Some(Value::int(10.into()))).expect("option is canonical"),
        ),
        (
            "max",
            Value::option(Some(Value::int(41.into()))).expect("option is canonical"),
        ),
    ] {
        let source = relation_source_expression("Note");
        let sorted = relation_stage(
            source,
            "sort_by",
            vec![parsed_expression(include_str!(
                "fixtures/evaluator_source_82249d21a2916b18.orna"
            ))],
        );
        let mapped = relation_stage(
            sorted,
            "map",
            vec![parsed_expression(include_str!(
                "fixtures/evaluator_source_54e7140f994de648.orna"
            ))],
        );
        let expanded = relation_stage(
            mapped,
            "flat_map",
            vec![parsed_expression(include_str!(
                "fixtures/evaluator_source_fdabca8ea2f4552c.orna"
            ))],
        );
        let body = relation_terminal(expanded, name);
        let mut effects = DistinctRelationEffects::new(vec![
            Value::int(4.into()),
            Value::int(1.into()),
            Value::int(3.into()),
            Value::int(2.into()),
        ]);
        assert_eq!(
            invoke_relation(body, &mut effects, Limits::default()).unwrap(),
            expected,
            "{name} sorted transformed suffix"
        );
        assert_eq!(
            effects.cursors.len(),
            4,
            "{name} must read each source page exactly once"
        );
    }
}

#[test]
fn relation_aggregate_calls_reject_invalid_arguments_and_values() {
    let missing_predicate = relation_terminal(relation_source_expression("Note"), "every");
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    assert_eq!(
        code(invoke_relation(
            missing_predicate,
            &mut effects,
            Limits::default()
        )),
        "ORNA-EVAL-ARGUMENT"
    );

    let invalid_predicate = relation_predicate_direct(
        relation_source_expression("Note"),
        "exists",
        parsed_expression(include_str!(
            "fixtures/evaluator_source_82249d21a2916b18.orna"
        )),
    );
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    assert_eq!(
        code(invoke_relation(
            invalid_predicate,
            &mut effects,
            Limits::default()
        )),
        "ORNA-EVAL-TYPE"
    );

    let mixed_sum = relation_direct_stage(relation_source_expression("Note"), "sum");
    let mut effects = DistinctRelationEffects::new(vec![
        Value::int(1.into()),
        Value::float_bits(2.0f64.to_bits()),
    ]);
    assert_eq!(
        code(invoke_relation(mixed_sum, &mut effects, Limits::default())),
        "ORNA-EVAL-UNSUPPORTED"
    );

    let text_min = relation_direct_stage(relation_source_expression("Note"), "min");
    let mut effects =
        DistinctRelationEffects::new(vec![Value::new(Raw::Text("x".into())).unwrap()]);
    assert_eq!(
        code(invoke_relation(text_min, &mut effects, Limits::default())),
        "ORNA-EVAL-UNSUPPORTED"
    );

    let extra = relation_stage(
        relation_source_expression("Note"),
        "sum",
        vec![relation_integer(1)],
    );
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    assert_eq!(
        code(invoke_relation(extra, &mut effects, Limits::default())),
        "ORNA-EVAL-ARGUMENT"
    );

    let unknown = relation_named_stage(relation_source_expression("Note"), "max", "values");
    let mut effects = DistinctRelationEffects::new(vec![Value::int(1.into())]);
    assert_eq!(
        code(invoke_relation(unknown, &mut effects, Limits::default())),
        "ORNA-EVAL-ARGUMENT"
    );
}

#[test]
fn root_sum_and_min_remain_shadowable_by_admitted_functions() {
    assert_eq!(
        call_module(
            include_str!("fixtures/root_sum_shadow_function.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
    assert_eq!(
        call_module(
            include_str!("fixtures/root_min_shadow_function.orna"),
            include_str!("fixtures/evaluator_source_02fcae88bd120f59.orna"),
            Limits::default(),
        )
        .unwrap(),
        Value::int(101.into())
    );
}
struct Uuid7Effects {
    calls: usize,
    arguments: Vec<Value>,
    value: Value,
}

impl EffectHandler for Uuid7Effects {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[Value],
    ) -> Result<Option<Value>, EvaluationError> {
        if matches!(callee, Expr::Name { text, .. } if text == "uuid7") {
            self.calls += 1;
            self.arguments = arguments.to_vec();
            return Ok(Some(self.value.clone()));
        }
        Ok(None)
    }
}

#[test]
fn uuid7_is_root_effect_intrinsic_and_tag37_round_trips() {
    let bytes = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0x7c, 0xde, 0x8f, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
        0xcd,
    ];
    let expected = Value::uuid(bytes);
    let functions = functions_from_source(include_str!("fixtures/uuid7_main_effect.orna"));
    let mut effects = Uuid7Effects {
        calls: 0,
        arguments: Vec::new(),
        value: expected.clone(),
    };

    let result = invoke_named_with_effects(
        "main",
        &functions,
        &Environment::new(),
        Limits::default(),
        &mut effects,
    )
    .expect("uuid7 effect dispatch");

    assert_eq!(result, expected);
    assert_eq!(effects.calls, 1);
    assert!(effects.arguments.is_empty());
    let encoded = result.encode().expect("canonical UUID encoding");
    assert_eq!(
        Value::decode(&encoded).expect("canonical UUID decode"),
        result
    );
    assert_eq!(
        result.raw(),
        &Raw::Tag(37, Box::new(Raw::Bytes(bytes.to_vec())))
    );
}

#[test]
fn uuid7_direct_evaluation_fails_closed_without_an_effect_handler() {
    assert_eq!(
        code(invoke_named(
            "main",
            &functions_from_source(include_str!("fixtures/uuid7_main_effect.orna")),
            &Environment::new(),
            Limits::default(),
        )),
        "ORNA-EVAL-NAME"
    );
}

#[test]
fn uuid7_rejects_arguments_and_preserves_function_shadowing() {
    let expected = Value::uuid([0x42; 16]);
    let functions = functions_from_source(include_str!("fixtures/uuid7_with_argument.orna"));
    let mut effects = Uuid7Effects {
        calls: 0,
        arguments: Vec::new(),
        value: expected.clone(),
    };
    assert_eq!(
        code(invoke_named_with_effects(
            "main",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )),
        "ORNA-EVAL-ARGUMENT"
    );
    assert_eq!(effects.calls, 0);

    let functions = functions_from_source(include_str!("fixtures/uuid7_shadowed_by_function.orna"));
    let mut effects = Uuid7Effects {
        calls: 0,
        arguments: Vec::new(),
        value: expected,
    };
    assert_eq!(
        invoke_named_with_effects(
            "main",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .expect("shadowed function call"),
        Value::int(7.into())
    );
    assert_eq!(effects.calls, 0);
}
fn relation_function_with_environment(body: Expr, environment: Environment) -> Functions {
    Functions::from([(
        "run".into(),
        PureFunction {
            parameters: Vec::new(),
            body,
            environment,
        },
    )])
}

fn invoke_relation_with_environment<E: EffectHandler>(
    body: Expr,
    environment: &Environment,
    effects: &mut E,
    limits: Limits,
) -> Result<Value, EvaluationError> {
    let functions = relation_function_with_environment(body, environment.clone());
    invoke_named_with_effects("run", &functions, &Environment::new(), limits, effects)
}

fn relation_bucket_by(input: Expr, period: Expr, zone: Option<Expr>) -> Expr {
    let span = relation_span();
    let mut arguments = vec![
        relation_argument(input, &span),
        relation_argument(period, &span),
    ];
    if let Some(zone) = zone {
        arguments.push(orna_syntax_v1::Argument {
            name: Some("zone".into()),
            value: zone,
            span: span.clone(),
        });
    }
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: "bucket_by".into(),
            span: span.clone(),
        }),
        arguments,
        span,
    }
}

fn relation_bucket_by_pipeline(input: Expr, period: Expr, zone: Option<Expr>) -> Expr {
    let span = relation_span();
    let mut arguments = vec![relation_argument(period, &span)];
    if let Some(zone) = zone {
        arguments.push(orna_syntax_v1::Argument {
            name: Some("zone".into()),
            value: zone,
            span: span.clone(),
        });
    }
    Expr::Binary {
        lhs: Box::new(input),
        op: "|".into(),
        rhs: Box::new(Expr::Call {
            callee: Box::new(Expr::Name {
                text: "bucket_by".into(),
                span: span.clone(),
            }),
            arguments,
            span: span.clone(),
        }),
        span,
    }
}

fn relation_bucket_by_named(input: Expr, period: Expr, zone: Option<Expr>) -> Expr {
    let span = relation_span();
    let mut arguments = vec![
        orna_syntax_v1::Argument {
            name: Some("rows".into()),
            value: input,
            span: span.clone(),
        },
        orna_syntax_v1::Argument {
            name: Some("period".into()),
            value: period,
            span: span.clone(),
        },
    ];
    if let Some(zone) = zone {
        arguments.push(orna_syntax_v1::Argument {
            name: Some("zone".into()),
            value: zone,
            span: span.clone(),
        });
    }
    Expr::Call {
        callee: Box::new(Expr::Name {
            text: "bucket_by".into(),
            span: span.clone(),
        }),
        arguments,
        span,
    }
}

fn bucket_instant(seconds: i64) -> Value {
    Value::new(Raw::Tag(
        60002,
        Box::new(Raw::Array(vec![
            Raw::Int(seconds.into()),
            Raw::Int(0.into()),
        ])),
    ))
    .unwrap()
}

fn bucket_group(rows: &[Value]) -> Value {
    Value::new(Raw::Array(
        rows.iter().map(|row| row.raw().clone()).collect(),
    ))
    .unwrap()
}

#[test]
fn bucket_by_elapsed_utc_preserves_group_and_row_order_for_all_call_forms() {
    let period = duration(86_400, 0);
    let environment = Environment::from([("period".into(), period)]);
    let rows = vec![
        bucket_instant(1_704_067_200 + 43_200),
        bucket_instant(1_704_067_200 + 3_600),
        bucket_instant(1_704_067_200 + 86_400 + 3_600),
    ];
    let expected = Value::option(Some(bucket_group(&rows[..2]))).unwrap();
    let source = relation_source_expression("Reading");
    let cases = [
        relation_bucket_by(
            source.clone(),
            parsed_expression(include_str!(
                "fixtures/evaluator_source_514cb13f603464f9.orna"
            )),
            None,
        ),
        relation_bucket_by_pipeline(
            source.clone(),
            parsed_expression(include_str!(
                "fixtures/evaluator_source_514cb13f603464f9.orna"
            )),
            None,
        ),
        relation_bucket_by_named(
            source,
            parsed_expression(include_str!(
                "fixtures/evaluator_source_514cb13f603464f9.orna"
            )),
            None,
        ),
    ];
    for body in cases {
        let mut effects = DistinctRelationEffects::new(rows.clone());
        assert_eq!(
            invoke_relation_with_environment(
                relation_terminal(body, "first"),
                &environment,
                &mut effects,
                Limits::default(),
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            effects.cursors.len(),
            3,
            "first bucket must read only through the first row of the next bucket"
        );
    }
}

#[test]
fn bucket_by_preserves_filter_map_flat_map_window_and_sort_suffixes() {
    let period = duration(86_400, 0);
    let environment = Environment::from([("period".into(), period)]);
    let rows = vec![
        bucket_instant(1_704_110_400 + 3_600),
        bucket_instant(1_704_110_400 + 7_200),
        bucket_instant(1_704_110_400 + 86_400 + 3_600),
    ];
    let source = relation_source_expression("Reading");
    let bucket = || {
        relation_bucket_by(
            source.clone(),
            parsed_expression(include_str!(
                "fixtures/evaluator_source_514cb13f603464f9.orna"
            )),
            None,
        )
    };

    let mapped = relation_stage(
        bucket(),
        "map",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_4fb33064b9aaa5a6.orna"
        ))],
    );
    let mut effects = DistinctRelationEffects::new(rows.clone());
    assert_eq!(
        invoke_relation_with_environment(
            relation_terminal(mapped, "first"),
            &environment,
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(Value::int(2.into()))).unwrap()
    );

    let filtered = relation_stage(
        bucket(),
        "filter",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_ea2a835373c32994.orna"
        ))],
    );
    let mut effects = DistinctRelationEffects::new(rows.clone());
    assert_eq!(
        invoke_relation_with_environment(
            relation_terminal(filtered, "first"),
            &environment,
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(bucket_group(&rows[..2]))).unwrap()
    );

    let flattened = relation_stage(
        bucket(),
        "flat_map",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_81652cd82e2317ca.orna"
        ))],
    );
    let mut effects = DistinctRelationEffects::new(rows.clone());
    assert_eq!(
        invoke_relation_with_environment(
            relation_terminal(flattened, "first"),
            &environment,
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(rows[0].clone())).unwrap()
    );

    let windowed = relation_stage(bucket(), "window", vec![relation_integer(1)]);
    let mut effects = DistinctRelationEffects::new(rows.clone());
    assert_eq!(
        invoke_relation_with_environment(
            relation_terminal(windowed, "first"),
            &environment,
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(
            Value::new(Raw::Array(vec![bucket_group(&rows[..2]).raw().clone(),])).unwrap()
        ))
        .unwrap()
    );

    let sorted = relation_stage(
        bucket(),
        "sort_by",
        vec![parsed_expression(include_str!(
            "fixtures/evaluator_source_4fb33064b9aaa5a6.orna"
        ))],
    );
    let mut effects = DistinctRelationEffects::new(rows);
    assert_eq!(
        invoke_relation_with_environment(
            relation_terminal(sorted, "first"),
            &environment,
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::option(Some(bucket_group(&[bucket_instant(
            1_704_110_400 + 86_400 + 3_600,
        )])))
        .unwrap()
    );
}

#[test]
fn bucket_by_calendar_utc_groups_by_local_day_not_elapsed_duration() {
    let environment = Environment::new();
    let rows = vec![
        bucket_instant(1_709_164_800 + 23 * 3_600 + 30 * 60),
        bucket_instant(1_709_164_800 + 86_400 + 30 * 60),
    ];
    let body = relation_bucket_by(
        relation_source_expression("Reading"),
        parsed_expression(include_str!(
            "fixtures/evaluator_source_d1baf58b3f29e74a.orna"
        )),
        Some(parsed_expression(include_str!(
            "fixtures/evaluator_source_68105452227ca2d3.orna"
        ))),
    );
    let mut effects = DistinctRelationEffects::new(rows.clone());
    assert_eq!(
        invoke_relation_with_environment(
            relation_terminal(body, "count"),
            &environment,
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::int(2.into())
    );
}

#[test]
fn bucket_by_london_calendar_days_cover_23_24_and_25_elapsed_hours() {
    let environment = Environment::new();
    let cases = [
        (
            vec![
                bucket_instant(1_711_843_200 + 22 * 3_600 + 30 * 60),
                bucket_instant(1_711_843_200 + 23 * 3_600 + 30 * 60),
            ],
            2,
        ),
        (
            vec![
                bucket_instant(1_711_756_800 + 23 * 3_600 + 30 * 60),
                bucket_instant(1_711_843_200 + 30 * 60),
            ],
            2,
        ),
        (
            vec![
                bucket_instant(1_729_987_200 + 23 * 3_600 + 30 * 60),
                bucket_instant(1_730_077_200 + 30 * 60),
            ],
            2,
        ),
    ];
    for (rows, expected_count) in cases {
        let body = relation_bucket_by(
            relation_source_expression("Reading"),
            parsed_expression(include_str!(
                "fixtures/evaluator_source_d1baf58b3f29e74a.orna"
            )),
            Some(parsed_expression(include_str!(
                "fixtures/evaluator_source_7762ca70e81af937.orna"
            ))),
        );
        let mut effects = DistinctRelationEffects::new(rows);
        assert_eq!(
            invoke_relation_with_environment(
                relation_terminal(body, "count"),
                &environment,
                &mut effects,
                Limits::default(),
            )
            .unwrap(),
            Value::int(expected_count.into())
        );
    }
}

#[test]
fn bucket_by_empty_input_is_empty_and_first_is_bounded() {
    let body = relation_bucket_by(
        relation_source_expression("Reading"),
        parsed_expression(include_str!(
            "fixtures/evaluator_source_d1baf58b3f29e74a.orna"
        )),
        Some(parsed_expression(include_str!(
            "fixtures/evaluator_source_68105452227ca2d3.orna"
        ))),
    );
    let mut effects = DistinctRelationEffects::new(Vec::new());
    assert_eq!(
        invoke_relation_with_environment(
            relation_terminal(body, "count"),
            &Environment::new(),
            &mut effects,
            Limits::default(),
        )
        .unwrap(),
        Value::int(0.into())
    );
    assert_eq!(effects.cursors, vec![None]);
}

#[test]
fn bucket_by_rejects_invalid_zone_and_period() {
    for (period, zone, expected) in [
        (
            include_str!("fixtures/evaluator_source_d1baf58b3f29e74a.orna"),
            include_str!("fixtures/evaluator_source_820ce223720a4abd.orna"),
            "ORNA-EVAL-VALUE",
        ),
        (
            include_str!("fixtures/evaluator_source_73475cb40a568e8d.orna"),
            include_str!("fixtures/evaluator_source_68105452227ca2d3.orna"),
            "ORNA-EVAL-TYPE",
        ),
    ] {
        let body = relation_bucket_by(
            relation_source_expression("Reading"),
            parsed_expression(period),
            Some(parsed_expression(zone)),
        );
        let mut effects = DistinctRelationEffects::new(vec![bucket_instant(0)]);
        assert_eq!(
            code(invoke_relation_with_environment(
                relation_terminal(body, "count"),
                &Environment::new(),
                &mut effects,
                Limits::default(),
            )),
            expected,
            "{period} / {zone}"
        );
    }
}

#[test]
fn admitted_eval_producer_stages_effectful_source_without_publishing() {
    let mut session = orna_evaluator_v1::AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!("fixtures/admitted_repl_seed.orna").trim()),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/admitted_repl_answer.orna").trim()),
        Ok(Some(Value::int(41.into())))
    );
    let before = session
        .preview(include_str!("fixtures/admitted_repl_last_result.orna").trim())
        .expect("last result should be visible");
    assert_eq!(
        session
            .stage_activation(include_str!("fixtures/admitted_repl_answer.orna").trim())
            .unwrap_err()
            .code(),
        "ORNA-REPL-EFFECT",
        "the producer boundary admits only effectful activation input"
    );

    let staged = session
        .stage_activation(include_str!("fixtures/admitted_repl_effectful_http.orna").trim())
        .expect("explicit eval source should be admitted into a staged activation");

    assert!(matches!(
        staged.input(),
        orna_syntax_v1::ReplInput::Expression(_)
    ));
    assert!(staged.result_type().is_some());
    assert!(staged.effects().effects.contains("network"));
    assert!(staged.effects().may_fail);
    assert_eq!(
        session.preview(include_str!("fixtures/admitted_repl_last_result.orna").trim()),
        Ok(before),
        "staging must not publish a new REPL result"
    );
    assert_eq!(
        session.preview(include_str!("fixtures/admitted_repl_answer.orna").trim()),
        Ok(Value::int(41.into())),
        "staging must not alter existing bindings"
    );
}
