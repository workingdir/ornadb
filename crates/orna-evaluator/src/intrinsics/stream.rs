//! Standard-library dispatch for the stream family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    fn checked_stream_values(
        &mut self,
        stream: &Value,
    ) -> Result<(Vec<Value>, String), EvaluationError> {
        let Value::Stream {
            values,
            source_label,
            source_digest,
            position,
            provider,
        } = stream
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if provider.is_some()
            || *position > values.len()
            || self.list_stream_digest(source_label, values)? != *source_digest
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.items(values.len().saturating_sub(*position))?;
        Ok((values[*position..].to_vec(), source_label.clone()))
    }
    fn finite_stream(
        &mut self,
        source_label: String,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        self.string(source_label.clone())?;
        self.items(values.len())?;
        let source_digest = self.list_stream_digest(&source_label, &values)?;
        Ok(Value::Stream {
            values,
            source_label,
            source_digest,
            position: 0,
            provider: None,
        })
    }
    fn stream_effect<T>(
        &mut self,
        operation: impl FnOnce(
            &mut dyn EffectHandler,
            &mut StepBudget,
            Option<&CancellationToken>,
        ) -> Result<T, EvaluationError>,
    ) -> Result<T, EvaluationError> {
        self.invalidate_call_reuse();
        let cancellation = self.cancellation;
        self.with_effect_budget(|context, budget| match context.effects.as_deref_mut() {
            Some(effects) => operation(effects, budget, cancellation),
            None => Err(error("ORNA-EVAL-UNSUPPORTED")),
        })
    }
    fn provider_stream(
        &mut self,
        source: &str,
        identity_label: &str,
    ) -> Result<Value, EvaluationError> {
        let source = self.string(source.to_owned())?;
        let identity_label = self.string(identity_label.to_owned())?;
        if source.is_empty() || identity_label.is_empty() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        // The reference pins finite-list identities, not connector identities.
        // Bind the live cursor to both provider source and consumer label.
        let identity_value = Raw::Array(vec![
            Raw::Text(source.clone()),
            Raw::Text(identity_label.clone()),
        ]);
        let identity = domain_digest("orna.provider-stream.v1", &identity_value)
            .map_err(|_| error("ORNA-EVAL-VALUE"))?;
        let available = self.stream_effect(|effects, budget, cancellation| {
            effects.validate_stream_source(&source, &identity, budget, cancellation)
        })?;
        if !available {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let source_label = format!("{source}:{identity_label}");
        self.string(source_label.clone())?;
        Ok(Value::Stream {
            values: Vec::new(),
            source_label,
            source_digest: identity,
            position: 0,
            provider: Some(ProviderStream {
                sources: vec![StreamSourceCursor {
                    source,
                    identity,
                    after: None,
                }],
                buffer_capacity: 1,
                batch_size: None,
                throttle_nanoseconds: None,
                debounce_nanoseconds: None,
                retry: None,
                recover: None,
            }),
        })
    }
    fn provider_stream_value(
        &mut self,
        source_label: String,
        source_digest: [u8; 32],
        provider: ProviderStream,
    ) -> Result<Value, EvaluationError> {
        self.string(source_label.clone())?;
        self.items(provider.sources.len())?;
        Ok(Value::Stream {
            values: Vec::new(),
            source_label,
            source_digest,
            position: 0,
            provider: Some(provider),
        })
    }
    fn stream_checkpoints(items: &[ReadyStreamItem]) -> Vec<StreamCheckpoint> {
        let mut checkpoints = BTreeMap::<(String, [u8; 32]), Vec<u8>>::new();
        for item in items {
            checkpoints
                .entry((item.source.clone(), item.identity))
                .and_modify(|checkpoint| {
                    if item.checkpoint > *checkpoint {
                        *checkpoint = item.checkpoint.clone();
                    }
                })
                .or_insert_with(|| item.checkpoint.clone());
        }
        checkpoints
            .into_iter()
            .map(|((source, identity), checkpoint)| StreamCheckpoint {
                source,
                identity,
                checkpoint,
            })
            .collect()
    }
    fn commit_stream_checkpoints(
        &mut self,
        checkpoints: &[StreamCheckpoint],
    ) -> Result<(), EvaluationError> {
        if checkpoints.is_empty() {
            return Ok(());
        }
        self.stream_effect(|effects, budget, cancellation| {
            effects.begin_stream_delivery(checkpoints, budget, cancellation)
        })?;
        let commit = self.stream_effect(|effects, budget, cancellation| {
            effects.commit_stream_delivery(checkpoints, budget, cancellation)
        });
        if let Err(failure) = commit {
            let _ = self.stream_effect(|effects, budget, _| {
                effects.rollback_stream_delivery(checkpoints, budget, None)
            });
            return Err(failure);
        }
        Ok(())
    }
    fn rollback_stream_checkpoints(&mut self, checkpoints: &[StreamCheckpoint]) {
        let _ = self.stream_effect(|effects, budget, _| {
            effects.rollback_stream_delivery(checkpoints, budget, None)
        });
    }
    fn commit_stream_callback(
        &mut self,
        items: &[ReadyStreamItem],
        action: &Value,
        recovery: Option<&Value>,
        batch_size: Option<usize>,
        depth: usize,
    ) -> Result<(), EvaluationError> {
        if items.is_empty() {
            return Ok(());
        }
        let checkpoints = Self::stream_checkpoints(items);
        self.stream_effect(|effects, budget, cancellation| {
            effects.begin_stream_delivery(&checkpoints, budget, cancellation)
        })?;

        let callback = (|| {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                let value = if let Some(failure) = &item.recovery_error {
                    let recovery = recovery.ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                    self.invoke_predicate(recovery, Value::Error(failure.clone()), depth + 1)?
                } else {
                    item.value.clone()
                };
                values.push(value);
            }
            let argument = if batch_size.is_some() {
                Value::list(values)
            } else {
                values
                    .into_iter()
                    .next()
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?
            };
            let result = self.invoke_predicate(action, argument, depth + 1)?;
            if !matches!(result, Value::Unit | Value::Null) {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            self.step()
        })();

        if let Err(failure) = callback {
            self.rollback_stream_checkpoints(&checkpoints);
            return Err(failure);
        }
        let commit = self.stream_effect(|effects, budget, cancellation| {
            effects.commit_stream_delivery(&checkpoints, budget, cancellation)
        });
        if let Err(failure) = commit {
            self.rollback_stream_checkpoints(&checkpoints);
            return Err(failure);
        }
        Ok(())
    }
    fn wait_stream_retry_delay(&mut self, nanoseconds: &BigInt) -> Result<(), EvaluationError> {
        let mut remaining = nanoseconds.clone();
        let quantum = BigInt::from(10_000_000u32);
        while remaining > BigInt::from(0u8) {
            self.step()?;
            let wait = remaining.clone().min(quantum.clone());
            let nanos = wait.to_u32().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
            std::thread::sleep(std::time::Duration::from_nanos(u64::from(nanos)));
            remaining -= wait;
        }
        Ok(())
    }
    fn recoverable_stream_item(
        failure: StreamFailure,
        recovery_enabled: bool,
    ) -> Result<ReadyStreamItem, EvaluationError> {
        let Some(checkpoint) = failure.checkpoint else {
            return Err(failure.error);
        };
        if !failure.recoverable || !recovery_enabled {
            return Err(failure.error);
        }
        Ok(ReadyStreamItem {
            source: failure.source,
            identity: failure.identity,
            checkpoint,
            event_time: failure.event_time,
            value: Value::Error(failure.error.clone()),
            recovery_error: Some(failure.error),
        })
    }
    fn retry_provider_delivery(
        &mut self,
        provider: &ProviderStream,
        cursor: &StreamSourceCursor,
        mut failure: StreamFailure,
        watermark: &mut Instant,
    ) -> Result<ReadyStreamItem, EvaluationError> {
        if let Some(retry) = &provider.retry {
            let expected_checkpoint = failure
                .checkpoint
                .as_deref()
                .ok_or_else(|| failure.error.clone())?
                .to_vec();
            let mut delay = retry.initial_delay_nanoseconds.clone();
            for _ in 1..retry.max_attempts {
                self.wait_stream_retry_delay(&delay)?;
                let mut source_ended = false;
                loop {
                    self.step()?;
                    let page = self.stream_effect(|effects, budget, cancellation| {
                        effects.poll_stream_sources(
                            std::slice::from_ref(cursor),
                            1,
                            budget,
                            cancellation,
                        )
                    })?;
                    if page.events.len() > 1
                        || page.watermark < *watermark
                        || (page.events.is_empty()
                            && page.ended_sources.is_empty()
                            && page.watermark <= *watermark)
                        || page
                            .ended_sources
                            .iter()
                            .any(|source| source != &cursor.source)
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    *watermark = page.watermark;
                    let Some(event) = page.events.into_iter().next() else {
                        if page.ended_sources.contains(&cursor.source) {
                            source_ended = true;
                            break;
                        }
                        continue;
                    };
                    match event {
                        StreamEvent::Delivery(delivery) => {
                            if delivery.source != cursor.source
                                || delivery.identity != cursor.identity
                                || delivery.checkpoint != expected_checkpoint
                                || delivery.event_time != failure.event_time
                                || delivery.event_time > *watermark
                            {
                                return Err(error("ORNA-EVAL-VALUE"));
                            }
                            return Ok(ReadyStreamItem {
                                source: delivery.source,
                                identity: delivery.identity,
                                checkpoint: delivery.checkpoint,
                                event_time: delivery.event_time,
                                value: Value::from_canonical(&delivery.value, self, 0)?,
                                recovery_error: None,
                            });
                        }
                        StreamEvent::Failure(next) => {
                            if next.source != cursor.source
                                || next.identity != cursor.identity
                                || next.checkpoint.as_deref() != Some(&expected_checkpoint)
                                || next.event_time != failure.event_time
                                || next.event_time > *watermark
                            {
                                return Err(error("ORNA-EVAL-VALUE"));
                            }
                            failure = next;
                            break;
                        }
                    }
                }
                if source_ended {
                    break;
                }
                delay = (delay * BigInt::from(2u8)).min(retry.max_delay_nanoseconds.clone());
            }
        }
        Self::recoverable_stream_item(failure, provider.recover.is_some())
    }
    fn consume_provider_stream(
        &mut self,
        stream: &Value,
        action: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let Value::Stream {
            provider: Some(provider),
            ..
        } = stream
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let mut cursors = self.stream_source_cursors(provider)?;
        let mut ended = BTreeSet::<String>::new();
        let mut last_watermark = None;
        let mut last_throttle_event = None;
        let mut state = StreamCallbackState {
            batch: Vec::new(),
            debounced: None,
        };
        let run = StreamRunContext {
            provider,
            action,
            depth,
        };

        loop {
            let active = cursors
                .iter()
                .filter(|cursor| !ended.contains(&cursor.source))
                .cloned()
                .collect::<Vec<_>>();
            if active.is_empty() {
                state.release_debounced();
                self.flush_stream_batch(&mut state, &run)?;
                break;
            }

            self.step()?;
            let page = self.stream_effect(|effects, budget, cancellation| {
                effects.poll_stream_sources(&active, provider.buffer_capacity, budget, cancellation)
            })?;
            Self::validate_stream_page(
                &cursors,
                &active,
                &page,
                provider.buffer_capacity,
                last_watermark,
            )?;
            last_watermark = Some(page.watermark);

            for event in page.events {
                let ready = self.ready_stream_item(
                    event,
                    &mut cursors,
                    &mut last_watermark,
                    &mut state,
                    &run,
                )?;
                self.accept_stream_item(ready, &run, &mut state, &mut last_throttle_event)?;
            }

            self.expire_debounced(page.watermark, &run, &mut state)?;

            ended.extend(page.ended_sources);
            if cursors.iter().all(|cursor| ended.contains(&cursor.source)) {
                state.release_debounced();
                self.flush_stream_batch(&mut state, &run)?;
                break;
            }
        }
        Ok(Value::Unit)
    }
    /// Validates the provider's source cursors before any page is requested:
    /// every source needs a non-empty, unique name.
    fn stream_source_cursors(
        &self,
        provider: &ProviderStream,
    ) -> Result<Vec<StreamSourceCursor>, EvaluationError> {
        self.items(provider.sources.len())?;
        let mut source_names = BTreeSet::new();
        for cursor in &provider.sources {
            if cursor.source.is_empty() || !source_names.insert(cursor.source.clone()) {
                return Err(error("ORNA-EVAL-VALUE"));
            }
        }
        Ok(provider.sources.clone())
    }
    /// Rejects a page whose bounds, watermark or per-source checkpoints are
    /// inconsistent with the cursors the request was issued from. Validation
    /// finishes before any callback runs, so malformed later events cannot
    /// follow committed work.
    fn validate_stream_page(
        cursors: &[StreamSourceCursor],
        active: &[StreamSourceCursor],
        page: &StreamPage,
        buffer_capacity: usize,
        last_watermark: Option<Instant>,
    ) -> Result<(), EvaluationError> {
        if page.events.len() > buffer_capacity
            || last_watermark.is_some_and(|last| page.watermark < last)
            || (page.events.is_empty()
                && page.ended_sources.is_empty()
                && last_watermark.is_some_and(|last| page.watermark <= last))
            || page
                .ended_sources
                .iter()
                .any(|source| !active.iter().any(|cursor| &cursor.source == source))
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let mut observed = cursors
            .iter()
            .map(|cursor| {
                (
                    (cursor.source.clone(), cursor.identity),
                    cursor.after.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut blocked = BTreeSet::<(String, [u8; 32])>::new();
        for event in &page.events {
            let (source, identity, checkpoint, event_time) = match event {
                StreamEvent::Delivery(delivery) => (
                    &delivery.source,
                    delivery.identity,
                    Some(&delivery.checkpoint),
                    delivery.event_time,
                ),
                StreamEvent::Failure(failure) => (
                    &failure.source,
                    failure.identity,
                    failure.checkpoint.as_ref(),
                    failure.event_time,
                ),
            };
            let key = (source.clone(), identity);
            let Some(previous) = observed.get_mut(&key) else {
                return Err(error("ORNA-EVAL-VALUE"));
            };
            if event_time > page.watermark || blocked.contains(&key) {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            if let Some(checkpoint) = checkpoint {
                if previous
                    .as_ref()
                    .is_some_and(|previous| checkpoint <= previous)
                {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                *previous = Some(checkpoint.clone());
            }
            if matches!(event, StreamEvent::Failure(_)) {
                blocked.insert(key);
            }
        }
        Ok(())
    }
    /// Converts one validated page event into the item the callback pipeline
    /// consumes, applying cursor and retry bookkeeping first.
    fn ready_stream_item(
        &mut self,
        event: StreamEvent,
        cursors: &mut [StreamSourceCursor],
        last_watermark: &mut Option<Instant>,
        state: &mut StreamCallbackState,
        run: &StreamRunContext<'_>,
    ) -> Result<ReadyStreamItem, EvaluationError> {
        match event {
            StreamEvent::Delivery(delivery) => {
                let Some(cursor) = cursors.iter_mut().find(|cursor| {
                    cursor.source == delivery.source && cursor.identity == delivery.identity
                }) else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                cursor.after = Some(delivery.checkpoint.clone());
                Ok(ReadyStreamItem {
                    source: delivery.source,
                    identity: delivery.identity,
                    checkpoint: delivery.checkpoint,
                    event_time: delivery.event_time,
                    value: Value::from_canonical(&delivery.value, self, 0)?,
                    recovery_error: None,
                })
            }
            StreamEvent::Failure(failure) => {
                let cursor = cursors
                    .iter()
                    .find(|cursor| {
                        cursor.source == failure.source && cursor.identity == failure.identity
                    })
                    .cloned()
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                state.release_debounced();
                self.flush_stream_batch(state, run)?;
                self.retry_provider_delivery(
                    run.provider,
                    &cursor,
                    failure,
                    last_watermark
                        .as_mut()
                        .expect("a validated page has a watermark"),
                )
            }
        }
    }
    /// Applies debounce and throttle policy to one ready item and flushes the
    /// batch once it reaches the provider's target size.
    fn accept_stream_item(
        &mut self,
        ready: ReadyStreamItem,
        run: &StreamRunContext<'_>,
        state: &mut StreamCallbackState,
        last_throttle_event: &mut Option<Instant>,
    ) -> Result<(), EvaluationError> {
        self.expire_debounced(ready.event_time, run, state)?;
        if run.provider.debounce_nanoseconds.is_some() {
            // `latest` may acknowledge a replaced item only after all older
            // buffered items from that same source have run.
            if let Some(previous) = state.debounced.take() {
                let checkpoints = Self::stream_checkpoints(std::slice::from_ref(&previous));
                if state.batch.iter().any(|item| {
                    item.source == previous.source && item.identity == previous.identity
                }) {
                    self.flush_stream_batch(state, run)?;
                }
                self.commit_stream_checkpoints(&checkpoints)?;
            }
            state.debounced = Some(ready);
        } else if let Some(interval) = &run.provider.throttle_nanoseconds {
            // Equal event-time ties retain provider arrival order: the first
            // eligible value wins and every drop is checkpointed.
            let emit = last_throttle_event.is_none_or(|last| {
                stream_elapsed_nanoseconds(ready.event_time, last)
                    .is_some_and(|elapsed| elapsed >= *interval)
            });
            if emit {
                *last_throttle_event = Some(ready.event_time);
                state.batch.push(ready);
            } else {
                let checkpoints = Self::stream_checkpoints(std::slice::from_ref(&ready));
                if state
                    .batch
                    .iter()
                    .any(|item| item.source == ready.source && item.identity == ready.identity)
                {
                    self.flush_stream_batch(state, run)?;
                }
                self.commit_stream_checkpoints(&checkpoints)?;
            }
        } else {
            state.batch.push(ready);
        }
        let target = run.provider.batch_size.unwrap_or(1);
        if state.batch.len() >= target {
            self.flush_stream_batch(state, run)?;
        }
        Ok(())
    }
    /// Releases one debounced item once `now` is far enough past its event time.
    fn expire_debounced(
        &mut self,
        now: Instant,
        run: &StreamRunContext<'_>,
        state: &mut StreamCallbackState,
    ) -> Result<(), EvaluationError> {
        if let (Some(interval), Some(pending)) =
            (&run.provider.debounce_nanoseconds, state.debounced.as_ref())
            && stream_elapsed_nanoseconds(now, pending.event_time)
                .is_some_and(|elapsed| elapsed >= *interval)
        {
            state.release_debounced();
            let target = run.provider.batch_size.unwrap_or(1);
            if state.batch.len() >= target {
                self.flush_stream_batch(state, run)?;
            }
        }
        Ok(())
    }
    /// Runs the callback for a non-empty batch and resets it.
    fn flush_stream_batch(
        &mut self,
        state: &mut StreamCallbackState,
        run: &StreamRunContext<'_>,
    ) -> Result<(), EvaluationError> {
        if state.batch.is_empty() {
            return Ok(());
        }
        self.commit_stream_callback(
            &state.batch,
            run.action,
            run.provider.recover.as_deref(),
            run.provider.batch_size,
            run.depth,
        )?;
        state.batch.clear();
        Ok(())
    }
    pub(crate) fn stream(
        &mut self,
        name: &str,
        values: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("from_provider", [Value::String(source), Value::String(identity_label)]) => {
                self.provider_stream(source, identity_label)
            }
            ("from_list", [Value::List(items), Value::String(source_label)]) => {
                self.finite_stream(source_label.clone(), items.to_vec())
            }
            (
                "for_each",
                [
                    Value::Stream {
                        provider: Some(_), ..
                    },
                    action,
                ],
            ) => self.consume_provider_stream(&values[0], action, depth + 1),
            ("for_each", [Value::Stream { .. }, action]) => {
                let (items, _) = self.checked_stream_values(&values[0])?;
                self.items(items.len())?;
                for value in items {
                    self.step()?;
                    let result = self.invoke_predicate(action, value, depth + 1)?;
                    if !matches!(result, Value::Unit | Value::Null) {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    self.step()?;
                }
                Ok(Value::Unit)
            }
            ("batch", [Value::Stream { .. }, Value::Int(size)]) => {
                self.stream_batch(&values[0], size)
            }
            ("buffer", [Value::Stream { .. }, Value::Int(capacity)]) => {
                self.stream_buffer(&values[0], capacity)
            }
            ("merge", [Value::List(streams)]) => self.stream_merge(streams),
            (
                "throttle",
                [
                    Value::Stream { .. },
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                    Value::String(policy),
                ],
            ) => self.stream_throttle(&values[0], seconds, *nanosecond, policy),
            (
                "debounce",
                [
                    Value::Stream { .. },
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                    Value::String(policy),
                ],
            ) => self.stream_debounce(&values[0], seconds, *nanosecond, policy),
            ("retry", [Value::Stream { .. }, Value::Record(policy)]) => {
                self.stream_retry(&values[0], policy)
            }
            ("recover", [Value::Stream { .. }, handler]) => {
                self.stream_recover(&values[0], handler)
            }
            ("from_provider", [_, _])
            | ("from_list", [_, _])
            | ("for_each", [_, _])
            | ("batch", [_, _])
            | ("buffer", [_, _])
            | ("throttle", [_, _, _])
            | ("debounce", [_, _, _])
            | ("retry", [_, _])
            | ("recover", [_, _])
            | ("merge", [_]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    /// Merges streams into one replayable source. Provider-backed streams
    /// combine their sources under one identity; finite streams are admitted
    /// round-robin so every source keeps its own order.
    fn stream_merge(&mut self, streams: &List) -> Result<Value, EvaluationError> {
        if streams.iter().any(|stream| {
            matches!(
                stream,
                Value::Stream {
                    provider: Some(_),
                    ..
                }
            )
        }) {
            self.items(streams.len())?;
            if streams.is_empty() {
                return self.provider_stream_value(
                    "merge:empty".to_owned(),
                    [0; 32],
                    ProviderStream {
                        sources: Vec::new(),
                        buffer_capacity: 1,
                        batch_size: None,
                        throttle_nanoseconds: None,
                        debounce_nanoseconds: None,
                        retry: None,
                        recover: None,
                    },
                );
            }
            let mut sources = Vec::new();
            let mut capacity = usize::MAX;
            let mut merge_identity = Sha256::new();
            merge_identity.update(b"orna.provider-merge.v1\0");
            for stream in streams {
                let Value::Stream {
                    provider: Some(provider),
                    source_digest,
                    ..
                } = stream
                else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                if provider.batch_size.is_some()
                    || provider.throttle_nanoseconds.is_some()
                    || provider.debounce_nanoseconds.is_some()
                    || provider.retry.is_some()
                    || provider.recover.is_some()
                {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                capacity = capacity.min(provider.buffer_capacity);
                for source in &provider.sources {
                    self.step()?;
                    merge_identity.update((source.source.len() as u64).to_be_bytes());
                    merge_identity.update(source.source.as_bytes());
                    merge_identity.update(source.identity);
                    sources.push(source.clone());
                }
                merge_identity.update(source_digest);
            }
            let identity: [u8; 32] = merge_identity.finalize().into();
            let label = format!(
                "merge:{}",
                identity
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            return self.provider_stream_value(
                label,
                identity,
                ProviderStream {
                    sources,
                    buffer_capacity: capacity,
                    batch_size: None,
                    throttle_nanoseconds: None,
                    debounce_nanoseconds: None,
                    retry: None,
                    recover: None,
                },
            );
        }
        self.merge_finite_streams(streams)
    }
    fn stream_batch(&mut self, stream: &Value, size: &BigInt) -> Result<Value, EvaluationError> {
        if let Value::Stream {
            source_label,
            source_digest,
            provider: Some(provider),
            ..
        } = stream
        {
            let size = self.positive_collection_size(size)?;
            self.items(size)?;
            if provider.batch_size.is_some() {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            let mut provider = provider.clone();
            provider.batch_size = Some(size);
            return self.provider_stream_value(source_label.clone(), *source_digest, provider);
        }
        let (items, source_label) = self.checked_stream_values(stream)?;
        let size = self.positive_collection_size(size)?;
        let mut batches = Vec::new();
        for batch in items.chunks(size) {
            self.step()?;
            batches.push(Value::list(batch.to_vec()));
            self.items(batches.len())?;
        }
        self.finite_stream(format!("{source_label}|batch:{size}"), batches)
    }
    fn stream_buffer(
        &mut self,
        stream: &Value,
        capacity: &BigInt,
    ) -> Result<Value, EvaluationError> {
        if let Value::Stream {
            source_label,
            source_digest,
            provider: Some(provider),
            ..
        } = stream
        {
            let capacity = self.positive_collection_size(capacity)?;
            self.items(capacity)?;
            let mut provider = provider.clone();
            provider.buffer_capacity = capacity;
            return self.provider_stream_value(source_label.clone(), *source_digest, provider);
        }
        let (items, source_label) = self.checked_stream_values(stream)?;
        self.positive_collection_size(capacity)?;
        // The immutable finite source is consumed under backpressure;
        // a bounded queue changes scheduling, never item order/content.
        self.finite_stream(format!("{source_label}|buffer"), items)
    }
    /// Admits finite streams round-robin so every source keeps its own order,
    /// then folds the merged identity from the source labels and digests.
    fn merge_finite_streams(&mut self, streams: &List) -> Result<Value, EvaluationError> {
        let mut sources = Vec::with_capacity(streams.len());
        for stream in streams {
            let (items, source_label) = self.checked_stream_values(stream)?;
            sources.push((source_label, items));
        }
        let total = sources.iter().try_fold(0usize, |total, (_, items)| {
            total
                .checked_add(items.len())
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))
        })?;
        self.items(total)?;
        let mut merged = Vec::with_capacity(total);
        // The reference leaves cross-source arrival to a live
        // scheduler. These finite materialized sources have no event
        // timestamps, so this evaluator uses a repeatable round-robin
        // admission schedule while preserving every source's order.
        for position in 0..sources
            .iter()
            .map(|(_, items)| items.len())
            .max()
            .unwrap_or(0)
        {
            for (_, items) in &sources {
                if let Some(value) = items.get(position) {
                    self.step()?;
                    merged.push(value.clone());
                }
            }
        }
        let mut merge_identity = Sha256::new();
        merge_identity.update(b"orna.merge.v1\0");
        merge_identity.update((sources.len() as u64).to_be_bytes());
        for (source_label, items) in &sources {
            merge_identity.update((source_label.len() as u64).to_be_bytes());
            merge_identity.update(source_label.as_bytes());
            merge_identity.update(self.list_stream_digest(source_label, items)?);
        }
        let source_identity = merge_identity
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.finite_stream(format!("merge:{source_identity}"), merged)
    }
    fn stream_throttle(
        &mut self,
        stream: &Value,
        seconds: &BigInt,
        nanosecond: u32,
        policy: &str,
    ) -> Result<Value, EvaluationError> {
        if elapsed_total_nanoseconds(seconds, nanosecond) <= BigInt::from(0u8) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if policy != "drop" {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if let Value::Stream {
            source_label,
            source_digest,
            provider: Some(provider),
            ..
        } = stream
        {
            if provider.throttle_nanoseconds.is_some() || provider.debounce_nanoseconds.is_some() {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            let mut provider = provider.clone();
            provider.throttle_nanoseconds = Some(elapsed_total_nanoseconds(seconds, nanosecond));
            return self.provider_stream_value(source_label.clone(), *source_digest, provider);
        }
        // The reference requires elapsed intervals but gives this
        // finite source no timestamp provider. Treat the materialized
        // list as one observation instant; explicit `drop` keeps its
        // first item and drops later same-instant values.
        let (items, source_label) = self.checked_stream_values(stream)?;
        let output = items.first().cloned().into_iter().collect();
        self.finite_stream(format!("{source_label}|throttle:drop"), output)
    }
    fn stream_debounce(
        &mut self,
        stream: &Value,
        seconds: &BigInt,
        nanosecond: u32,
        policy: &str,
    ) -> Result<Value, EvaluationError> {
        if elapsed_total_nanoseconds(seconds, nanosecond) <= BigInt::from(0u8) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if policy != "latest" {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if let Value::Stream {
            source_label,
            source_digest,
            provider: Some(provider),
            ..
        } = stream
        {
            if provider.debounce_nanoseconds.is_some() || provider.throttle_nanoseconds.is_some() {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            let mut provider = provider.clone();
            provider.debounce_nanoseconds = Some(elapsed_total_nanoseconds(seconds, nanosecond));
            return self.provider_stream_value(source_label.clone(), *source_digest, provider);
        }
        // The reference requires elapsed quiet intervals but gives
        // this finite source no timestamp provider. Treat its items as
        // one synchronous burst and apply explicit `latest` delivery.
        let (items, source_label) = self.checked_stream_values(stream)?;
        let output = items.last().cloned().into_iter().collect();
        self.finite_stream(format!("{source_label}|debounce:latest"), output)
    }
    fn stream_retry(
        &mut self,
        stream: &Value,
        policy: &BTreeMap<String, Value>,
    ) -> Result<Value, EvaluationError> {
        if let Value::Stream {
            source_label,
            source_digest,
            provider: Some(provider),
            ..
        } = stream
        {
            let Some(Value::Int(max_attempts)) = policy.get("max_attempts") else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let Some(Value::Duration {
                seconds: initial_seconds,
                nanosecond: initial_nanos,
            }) = policy.get("initial_delay")
            else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let Some(Value::Duration {
                seconds: maximum_seconds,
                nanosecond: maximum_nanos,
            }) = policy.get("max_delay")
            else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let initial_delay_nanoseconds =
                elapsed_total_nanoseconds(initial_seconds, *initial_nanos);
            let max_delay_nanoseconds = elapsed_total_nanoseconds(maximum_seconds, *maximum_nanos);
            let max_attempts = max_attempts
                .to_usize()
                .filter(|attempts| *attempts > 0 && *attempts <= self.limits.max_collection_items)
                .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
            if initial_delay_nanoseconds < BigInt::from(0u8)
                || max_delay_nanoseconds < initial_delay_nanoseconds
                || provider.retry.is_some()
            {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            let mut provider = provider.clone();
            // `max_attempts` counts the first delivery attempt as
            // well as retries; the initial delay applies before the
            // first redelivery, then doubles up to `max_delay`.
            provider.retry = Some(ProviderRetryPolicy {
                max_attempts,
                initial_delay_nanoseconds,
                max_delay_nanoseconds,
            });
            return self.provider_stream_value(source_label.clone(), *source_digest, provider);
        }
        let (items, source_label) = self.checked_stream_values(stream)?;
        let Some(Value::Int(max_attempts)) = policy.get("max_attempts") else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let Some(Value::Duration {
            seconds: initial_seconds,
            nanosecond: initial_nanos,
        }) = policy.get("initial_delay")
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let Some(Value::Duration {
            seconds: maximum_seconds,
            nanosecond: maximum_nanos,
        }) = policy.get("max_delay")
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if !max_attempts.is_positive()
            || elapsed_total_nanoseconds(initial_seconds, *initial_nanos) < BigInt::from(0u8)
            || elapsed_total_nanoseconds(maximum_seconds, *maximum_nanos)
                < elapsed_total_nanoseconds(initial_seconds, *initial_nanos)
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        // ListStreamSource is replayable and cannot produce a blocked
        // provider delivery. A valid retry policy preserves it exactly.
        self.finite_stream(format!("{source_label}|retry"), items)
    }
    fn stream_recover(
        &mut self,
        stream: &Value,
        handler: &Value,
    ) -> Result<Value, EvaluationError> {
        if let Value::Stream {
            source_label,
            source_digest,
            provider: Some(provider),
            ..
        } = stream
        {
            if !matches!(handler, Value::Function { .. } | Value::Closure(_))
                || provider.recover.is_some()
            {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            let mut provider = provider.clone();
            provider.recover = Some(Box::new(handler.clone()));
            return self.provider_stream_value(source_label.clone(), *source_digest, provider);
        }
        let (items, source_label) = self.checked_stream_values(stream)?;
        if !matches!(handler, Value::Function { .. } | Value::Closure(_)) {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        // The finite list source cannot fail decoding or polling, so
        // recovery has no failure to replace and retains every item.
        self.finite_stream(format!("{source_label}|recover"), items)
    }
}
