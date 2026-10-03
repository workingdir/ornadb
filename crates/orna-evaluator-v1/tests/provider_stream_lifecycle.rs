use std::collections::{BTreeMap, VecDeque};

use orna_evaluator_v1::{
    CancellationToken, EffectHandler, Environment, EvaluationError, Functions, Limits,
    PureFunction, StepBudget, StreamCheckpoint, StreamDelivery, StreamEvent, StreamFailure,
    StreamPage, StreamSourceCursor, evaluate_expression_with_functions_and_effects,
    evaluate_expression_with_functions_and_effects_and_cancellation,
};
use orna_foundation_v1::{CanonicalValue, SafeText};
use orna_syntax_v1::{Expr, parse_expression};
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("fixture value is canonical")
}

fn text(value: &str) -> CanonicalValue {
    canonical(Raw::Text(value.to_owned()))
}

fn unit() -> CanonicalValue {
    canonical(Raw::Tag(60014, Box::new(Raw::Array(Vec::new()))))
}

fn instant(seconds: i64) -> orna_evaluator_v1::Instant {
    orna_evaluator_v1::Instant::new(seconds, 0).expect("valid fixture instant")
}

fn failure(code: &str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("safe static error code"))
}

#[derive(Clone)]
enum PlannedEvent {
    Delivery {
        source: String,
        checkpoint: Vec<u8>,
        event_time: i64,
        value: String,
    },
    Failure {
        source: String,
        checkpoint: Option<Vec<u8>>,
        event_time: i64,
        code: &'static str,
        recoverable: bool,
    },
}

struct PlannedPage {
    events: Vec<PlannedEvent>,
    watermark: i64,
    ended: &'static [&'static str],
}

struct ProviderHarness {
    pages: VecDeque<PlannedPage>,
    identities: BTreeMap<String, [u8; 32]>,
    requests: Vec<(usize, Vec<(String, Option<Vec<u8>>)>)>,
    committed_checkpoints: BTreeMap<String, Vec<u8>>,
    commit_batches: Vec<Vec<StreamCheckpoint>>,
    observed_values: Vec<String>,
    staged_values: Vec<String>,
    transaction: Option<Vec<StreamCheckpoint>>,
    rollback_count: usize,
    fail_on: Option<String>,
    cancel_on: Option<String>,
}

impl ProviderHarness {
    fn new(pages: impl IntoIterator<Item = PlannedPage>) -> Self {
        Self {
            pages: pages.into_iter().collect(),
            identities: BTreeMap::new(),
            requests: Vec::new(),
            committed_checkpoints: BTreeMap::new(),
            commit_batches: Vec::new(),
            observed_values: Vec::new(),
            staged_values: Vec::new(),
            transaction: None,
            rollback_count: 0,
            fail_on: None,
            cancel_on: None,
        }
    }

    fn page(
        events: Vec<PlannedEvent>,
        watermark: i64,
        ended: &'static [&'static str],
    ) -> PlannedPage {
        PlannedPage {
            events,
            watermark,
            ended,
        }
    }
}

impl EffectHandler for ProviderHarness {
    fn handle(
        &mut self,
        _: &Expr,
        _: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        _: &Expr,
        arguments: &[CanonicalValue],
        _: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if operation != "std.io.environment.get" {
            return Ok(None);
        }
        let Some(Raw::Text(value)) = arguments.first().map(CanonicalValue::raw) else {
            return Err(failure("ORNA-EVAL-TYPE"));
        };
        self.staged_values.push(value.clone());
        if self.fail_on.as_deref() == Some(value) {
            return Err(failure("ORNA-EVAL-ERROR"));
        }
        if self.cancel_on.as_deref() == Some(value) {
            cancellation
                .expect("stream callback receives activation cancellation")
                .request();
        }
        let result = canonical(Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Text(value.clone()),
            ])),
        ));
        Ok(Some(result))
    }

    fn validate_stream_source(
        &mut self,
        source: &str,
        identity: &[u8; 32],
        _: &mut StepBudget,
        _: Option<&CancellationToken>,
    ) -> Result<bool, EvaluationError> {
        self.identities.insert(source.to_owned(), *identity);
        Ok(true)
    }

    fn poll_stream_sources(
        &mut self,
        sources: &[StreamSourceCursor],
        max_events: usize,
        _: &mut StepBudget,
        _: Option<&CancellationToken>,
    ) -> Result<StreamPage, EvaluationError> {
        self.requests.push((
            max_events,
            sources
                .iter()
                .map(|cursor| (cursor.source.clone(), cursor.after.clone()))
                .collect(),
        ));
        let Some(planned) = self.pages.pop_front() else {
            return Err(failure("ORNA-EVAL-ERROR"));
        };
        if planned.events.len() > max_events {
            return Err(failure("ORNA-EVAL-LIMIT"));
        }
        let mut events = Vec::with_capacity(planned.events.len());
        for event in planned.events {
            match event {
                PlannedEvent::Delivery {
                    source,
                    checkpoint,
                    event_time,
                    value,
                } => events.push(StreamEvent::Delivery(StreamDelivery {
                    identity: *self
                        .identities
                        .get(&source)
                        .ok_or_else(|| failure("ORNA-EVAL-VALUE"))?,
                    source,
                    checkpoint,
                    event_time: instant(event_time),
                    value: text(&value),
                })),
                PlannedEvent::Failure {
                    source,
                    checkpoint,
                    event_time,
                    code,
                    recoverable,
                } => events.push(StreamEvent::Failure(StreamFailure {
                    identity: *self
                        .identities
                        .get(&source)
                        .ok_or_else(|| failure("ORNA-EVAL-VALUE"))?,
                    source,
                    checkpoint,
                    event_time: instant(event_time),
                    error: failure(code),
                    recoverable,
                })),
            }
        }
        Ok(StreamPage {
            events,
            watermark: instant(planned.watermark),
            ended_sources: planned
                .ended
                .iter()
                .map(|source| (*source).to_owned())
                .collect(),
        })
    }

    fn begin_stream_delivery(
        &mut self,
        checkpoints: &[StreamCheckpoint],
        _: &mut StepBudget,
        _: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        if self.transaction.is_some() {
            return Err(failure("ORNA-EVAL-VALUE"));
        }
        self.transaction = Some(checkpoints.to_vec());
        self.staged_values.clear();
        Ok(())
    }

    fn commit_stream_delivery(
        &mut self,
        checkpoints: &[StreamCheckpoint],
        _: &mut StepBudget,
        _: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        if self.transaction.as_deref() != Some(checkpoints) {
            return Err(failure("ORNA-EVAL-VALUE"));
        }
        for checkpoint in checkpoints {
            self.committed_checkpoints
                .insert(checkpoint.source.clone(), checkpoint.checkpoint.clone());
        }
        self.commit_batches.push(checkpoints.to_vec());
        self.observed_values.append(&mut self.staged_values);
        self.transaction = None;
        Ok(())
    }

    fn rollback_stream_delivery(
        &mut self,
        checkpoints: &[StreamCheckpoint],
        _: &mut StepBudget,
        _: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        if self.transaction.as_deref() != Some(checkpoints) {
            return Err(failure("ORNA-EVAL-VALUE"));
        }
        self.staged_values.clear();
        self.transaction = None;
        self.rollback_count += 1;
        Ok(())
    }
}

fn evaluate(
    source: &str,
    provider: &mut ProviderHarness,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_expression_with_functions_and_effects(
        source,
        &Environment::new(),
        &registered_effect_functions(),
        Limits::default(),
        provider,
    )
}

fn registered_effect_functions() -> Functions {
    let body = parse_expression("null");
    assert!(body.is_ok());
    Functions::from([(
        "std.io.environment.get".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: body.value,
            environment: Environment::new(),
        },
    )])
}

fn delivery(source: &str, checkpoint: u8, event_time: i64, value: &str) -> PlannedEvent {
    PlannedEvent::Delivery {
        source: source.to_owned(),
        checkpoint: vec![checkpoint],
        event_time,
        value: value.to_owned(),
    }
}

#[test]
fn provider_buffer_bounds_polls_and_commits_real_source_values() {
    let mut provider = ProviderHarness::new([
        ProviderHarness::page(
            vec![delivery("events", 1, 1, "A"), delivery("events", 2, 2, "B")],
            2,
            &[],
        ),
        ProviderHarness::page(vec![delivery("events", 3, 3, "C")], 3, &["events"]),
    ]);

    assert_eq!(
        evaluate(
            include_str!("fixtures/provider-stream-for-each.orna"),
            &mut provider,
        ),
        Ok(unit())
    );
    assert_eq!(provider.observed_values, ["A", "B", "C"]);
    assert_eq!(provider.committed_checkpoints["events"], [3]);
    assert_eq!(
        provider
            .requests
            .iter()
            .map(|request| request.0)
            .collect::<Vec<_>>(),
        [2, 2]
    );
}

#[test]
fn provider_batch_commits_a_short_final_group_with_its_last_checkpoint() {
    let mut provider = ProviderHarness::new([
        ProviderHarness::page(
            vec![delivery("events", 1, 1, "A"), delivery("events", 2, 2, "B")],
            2,
            &[],
        ),
        ProviderHarness::page(vec![delivery("events", 3, 3, "C")], 3, &["events"]),
    ]);

    assert_eq!(
        evaluate(
            include_str!("fixtures/provider-stream-batch.orna"),
            &mut provider,
        ),
        Ok(unit())
    );
    assert_eq!(provider.observed_values, ["A", "B", "C"]);
    assert_eq!(provider.commit_batches.len(), 2);
    assert_eq!(provider.commit_batches[0][0].checkpoint, [2]);
    assert_eq!(provider.commit_batches[1][0].checkpoint, [3]);
}

#[test]
fn provider_merge_preserves_provider_arrival_and_each_source_order() {
    let mut provider = ProviderHarness::new([
        ProviderHarness::page(vec![delivery("right", 1, 1, "R1")], 1, &[]),
        ProviderHarness::page(vec![delivery("left", 1, 2, "L1")], 2, &[]),
        ProviderHarness::page(vec![delivery("right", 2, 3, "R2")], 3, &[]),
        ProviderHarness::page(vec![delivery("left", 2, 4, "L2")], 4, &["left", "right"]),
    ]);

    assert_eq!(
        evaluate(
            include_str!("fixtures/provider-stream-merge.orna"),
            &mut provider
        ),
        Ok(unit())
    );
    assert_eq!(provider.observed_values, ["R1", "L1", "R2", "L2"]);
    assert_eq!(provider.committed_checkpoints["left"], [2]);
    assert_eq!(provider.committed_checkpoints["right"], [2]);
    assert!(provider.requests.iter().all(|(limit, _)| *limit == 1));
}

#[test]
fn provider_throttle_and_debounce_use_event_time_and_checkpoint_drops() {
    let mut throttle = ProviderHarness::new([ProviderHarness::page(
        vec![
            delivery("events", 1, 0, "A"),
            delivery("events", 2, 2, "B"),
            delivery("events", 3, 5, "C"),
        ],
        6,
        &["events"],
    )]);
    assert_eq!(
        evaluate(
            include_str!("fixtures/provider-stream-throttle.orna"),
            &mut throttle,
        ),
        Ok(unit())
    );
    assert_eq!(throttle.observed_values, ["A", "C"]);
    assert_eq!(throttle.committed_checkpoints["events"], [3]);

    let mut debounce = ProviderHarness::new([ProviderHarness::page(
        vec![
            delivery("events", 1, 0, "A"),
            delivery("events", 2, 2, "B"),
            delivery("events", 3, 10, "C"),
        ],
        11,
        &["events"],
    )]);
    assert_eq!(
        evaluate(
            include_str!("fixtures/provider-stream-debounce.orna"),
            &mut debounce,
        ),
        Ok(unit())
    );
    assert_eq!(debounce.observed_values, ["B", "C"]);
    assert_eq!(debounce.committed_checkpoints["events"], [3]);
}

#[test]
fn provider_retry_redelivers_same_checkpoint_and_recover_requires_permission() {
    let mut retry = ProviderHarness::new([
        ProviderHarness::page(
            vec![PlannedEvent::Failure {
                source: "events".to_owned(),
                checkpoint: Some(vec![1]),
                event_time: 1,
                code: "ORNA-EVAL-ERROR",
                recoverable: false,
            }],
            1,
            &[],
        ),
        ProviderHarness::page(vec![delivery("events", 1, 1, "RETRIED")], 1, &[]),
        ProviderHarness::page(vec![delivery("events", 2, 2, "NEXT")], 2, &["events"]),
    ]);
    assert_eq!(
        evaluate(
            include_str!("fixtures/provider-stream-retry.orna"),
            &mut retry
        ),
        Ok(unit())
    );
    assert_eq!(retry.observed_values, ["RETRIED", "NEXT"]);
    assert_eq!(retry.committed_checkpoints["events"], [2]);
    assert_eq!(retry.requests[1].1[0], ("events".to_owned(), None));

    let mut recovered = ProviderHarness::new([ProviderHarness::page(
        vec![PlannedEvent::Failure {
            source: "events".to_owned(),
            checkpoint: Some(vec![1]),
            event_time: 1,
            code: "ORNA-EVAL-ERROR",
            recoverable: true,
        }],
        1,
        &["events"],
    )]);
    assert_eq!(
        evaluate(
            include_str!("fixtures/provider-stream-recover.orna"),
            &mut recovered,
        )
        .unwrap_or_else(|error| panic!("provider recovery failed with {}", error.code())),
        unit()
    );
    assert_eq!(recovered.observed_values, ["RECOVERED"]);
    assert_eq!(recovered.committed_checkpoints["events"], [1]);

    let mut denied_recovery = ProviderHarness::new([ProviderHarness::page(
        vec![PlannedEvent::Failure {
            source: "events".to_owned(),
            checkpoint: Some(vec![1]),
            event_time: 1,
            code: "ORNA-EVAL-ERROR",
            recoverable: false,
        }],
        1,
        &["events"],
    )]);
    let result = evaluate(
        include_str!("fixtures/provider-stream-recover.orna"),
        &mut denied_recovery,
    );
    assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-ERROR");
    assert!(denied_recovery.committed_checkpoints.is_empty());
    assert!(denied_recovery.observed_values.is_empty());

    let mut exhausted = ProviderHarness::new([
        ProviderHarness::page(
            vec![PlannedEvent::Failure {
                source: "events".to_owned(),
                checkpoint: Some(vec![1]),
                event_time: 1,
                code: "ORNA-EVAL-ERROR",
                recoverable: false,
            }],
            1,
            &[],
        ),
        ProviderHarness::page(
            vec![PlannedEvent::Failure {
                source: "events".to_owned(),
                checkpoint: Some(vec![1]),
                event_time: 1,
                code: "ORNA-EVAL-ERROR",
                recoverable: false,
            }],
            1,
            &[],
        ),
    ]);
    let result = evaluate(
        include_str!("fixtures/provider-stream-retry.orna"),
        &mut exhausted,
    );
    assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-ERROR");
    assert!(exhausted.committed_checkpoints.is_empty());
    assert!(exhausted.commit_batches.is_empty());
    assert_eq!(exhausted.requests.len(), 2);
}

#[test]
fn callback_failure_and_cancellation_roll_back_the_blocked_checkpoint() {
    let mut callback_failure = ProviderHarness::new([
        ProviderHarness::page(vec![delivery("events", 1, 1, "A")], 1, &[]),
        ProviderHarness::page(vec![delivery("events", 2, 2, "B")], 2, &["events"]),
    ]);
    callback_failure.fail_on = Some("B".to_owned());
    let result = evaluate(
        include_str!("fixtures/provider-stream-callback-failure.orna"),
        &mut callback_failure,
    );
    assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-ERROR");
    assert_eq!(callback_failure.committed_checkpoints["events"], [1]);
    assert_eq!(callback_failure.observed_values, ["A"]);
    assert_eq!(callback_failure.rollback_count, 1);

    let mut cancelled = ProviderHarness::new([
        ProviderHarness::page(vec![delivery("events", 1, 1, "A")], 1, &[]),
        ProviderHarness::page(vec![delivery("events", 2, 2, "CANCEL")], 2, &["events"]),
    ]);
    cancelled.cancel_on = Some("CANCEL".to_owned());
    let cancellation = CancellationToken::new();
    let result = evaluate_expression_with_functions_and_effects_and_cancellation(
        include_str!("fixtures/provider-stream-callback-cancellation.orna"),
        &Environment::new(),
        &registered_effect_functions(),
        Limits::default(),
        &mut cancelled,
        Some(&cancellation),
    );
    assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-CANCELLED");
    assert_eq!(cancelled.committed_checkpoints["events"], [1]);
    assert_eq!(cancelled.observed_values, ["A"]);
    assert_eq!(cancelled.rollback_count, 1);
}
