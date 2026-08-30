use super::*;

impl DispatchService for RawDispatchService {
    fn session_bridge(&self) -> Option<Arc<crate::invoke::SessionBridge>> {
        self.resource_broker
            .as_ref()
            .and_then(SharedInvokeBroker::session_bridge)
    }

    fn cancelled(&self, stream: u64) {
        if let Some(cancellation) = self
            .invoke_cancellations
            .lock()
            .expect("invocation cancellation lock")
            .get(&stream)
        {
            cancellation.request_cancel();
        }
    }

    fn start(&self, session: AuthenticatedSession, stream: u64, call: RawCall) -> StartedDispatch {
        let dispatch = RawClientDispatch::new(self.kernel.clone(), session, stream, call);
        let accepted = dispatch.accepted_action();
        let future = Box::pin(async move {
            let result = dispatch.finish().await;
            let cancellation = result.action_after_cancellation();
            if let Some(source) = result.source() {
                report_private_dispatch_source(source);
            }
            DispatchCompletion {
                sealed_producer: None,
                sealed_invocation: None,
                sealed_next_event_sequence: 1,
                sealed_next_outer_sequence: 2,
                actions: result.into_actions().into(),
                cancellation,
                cancellation_token: None,
                start_gate: None,
                start_delivered: false,
                terminal_delivered: false,
                terminal_claimed: false,
                worker_completed: false,
                _guards: None,
            }
        });
        StartedDispatch {
            accepted,
            started: None,
            start_gate: None,
            future,
        }
    }

    fn preflight_invoke(
        &self,
        session: AuthenticatedSession,
        request: orna_protocol::RetainedInvokeRequest,
        _version: RawProtocolVersion,
    ) -> InvokePreflightFuture {
        let kernel = self.kernel.clone();
        Box::pin(async move {
            match kernel
                .validate_sealed_sys_invoke(&session, SEALED_CONNECTION_PROTOCOL_MAJOR, &request)
                .await?
            {
                SealedInvocationPreflight::Rejected { failure } => {
                    Ok(InvokePreflight::Rejected(failure))
                }
                SealedInvocationPreflight::Accepted(continuation) => {
                    Ok(InvokePreflight::Accepted(Some(continuation)))
                }
            }
        })
    }

    fn start_invoke(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _request: orna_protocol::RetainedInvokeRequest,
        _version: &RawProtocolVersion,
        continuation: Option<SealedInvocationContinuation>,
    ) -> StartedDispatch {
        let continuation = continuation.expect("sealed invocation preflight continuation");
        let invocation = continuation.invocation();
        if let Some(broker) = &self.resource_broker {
            broker
                .install_session_bridge(invocation, stream)
                .expect("one session bridge per authenticated root invocation");
        }
        let dispatch_session = _session.clone();
        // The worker below uses a short-lived runtime; stream producers must
        // stay owned by this raw-socket driver runtime.
        let resource_runtime = tokio::runtime::Handle::current();
        let kernel = self.kernel.clone();
        let started = ServerAction::InvokeEvents {
            stream,
            events: continuation.started_events().clone(),
        };
        let accepted = ServerAction::Accepted { stream, invocation };
        let (start_gate, start_signal) = oneshot::channel();
        let cancellation = ResourceCancellation::new();
        self.invoke_cancellations
            .lock()
            .expect("invocation cancellation lock")
            .insert(stream, cancellation.clone());
        let cancellation_for_task = cancellation.clone();
        let cancellations = self.invoke_cancellations.clone();
        let resource_broker = self.resource_broker.clone();
        let future = Box::pin(async move {
            let mut operation = match continuation.prepare_sealed_sys_invoke_after_accept().await {
                Ok(operation) => operation,
                Err(source) => {
                    report_private_dispatch_source(&source);
                    cancellations
                        .lock()
                        .expect("invocation cancellation lock")
                        .remove(&stream);
                    return DispatchCompletion {
                        sealed_producer: None,
                        sealed_invocation: Some(invocation),
                        sealed_next_event_sequence: 1,
                        sealed_next_outer_sequence: 2,
                        actions: VecDeque::from([
                            ServerAction::InvokeEvents {
                                stream,
                                events: redacted_invoke_failure(
                                    invocation,
                                    InvocationFailurePhase::Internal,
                                    "INVOKE_INTERNAL_FAILURE",
                                    "invocation could not complete",
                                    InvocationRetryability::Unknown,
                                ),
                            },
                            ServerAction::Completed { stream },
                        ]),
                        cancellation: ServerAction::InvokeCancelled { stream },
                        cancellation_token: Some(cancellation_for_task.clone()),
                        start_gate: None,
                        start_delivered: false,
                        terminal_delivered: false,
                        terminal_claimed: false,
                        worker_completed: false,
                        _guards: None,
                    };
                }
            };
            let _ = tokio::select! {
                biased;
                _ = start_signal => {}
                _ = cancellation_for_task.cancelled() => {}
            };
            let cancellation = cancellation_for_task.clone();
            let worker_kernel = kernel.clone();
            let worker_session = dispatch_session.clone();
            let worker_active = operation.active_revision();
            let execution = tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel",
                        record: "raw_socket".to_string(),
                        rule: "sealed invocation worker runtime must start",
                    })?;
                runtime.block_on(async move {
                    let mut state = ClientStateStore::new();
                    let mut capability_audit_appended = false;
                    let mut resource_executor = match resource_broker {
                        Some(broker) => InstalledClientResourceExecutor::new_with_broker(
                            worker_kernel,
                            worker_session,
                            worker_active,
                            broker,
                            cancellation.clone(),
                        ),
                        None => InstalledClientResourceExecutor::new(
                            worker_kernel,
                            worker_session,
                            worker_active,
                        ),
                    };
                    operation
                        .execute_after_started(
                            Some(&mut resource_executor),
                            &mut state,
                            &mut capability_audit_appended,
                            &cancellation,
                            resource_runtime,
                        )
                        .await
                })
            })
            .await
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel",
                record: "raw_socket".to_string(),
                rule: "sealed invocation worker must not panic",
            })
            .and_then(|result| result);
            let cancellation_won_after_execution =
                sealed_result_cancellation_won(&cancellation_for_task, &execution);
            let (actions, sealed_producer) = match execution {
                Ok(SealedInvocationExecution::ServerStream(producer)) => {
                    (VecDeque::new(), Some(producer))
                }
                Ok(SealedInvocationExecution::Result(_)) if cancellation_won_after_execution => (
                    cancellation_actions(stream, ServerAction::InvokeCancelled { stream }),
                    None,
                ),
                Ok(SealedInvocationExecution::Result(SealedInvocationResult::Completed {
                    events,
                    ..
                }))
                | Ok(SealedInvocationExecution::Result(SealedInvocationResult::Failed {
                    events,
                    ..
                })) => (
                    VecDeque::from([
                        ServerAction::InvokeEvents {
                            stream,
                            events: without_started_event(events),
                        },
                        ServerAction::Completed { stream },
                    ]),
                    None,
                ),
                Ok(SealedInvocationExecution::Result(SealedInvocationResult::Denied {
                    ..
                })) => (
                    VecDeque::from([
                        ServerAction::InvokeEvents {
                            stream,
                            events: redacted_invoke_failure(
                                invocation,
                                InvocationFailurePhase::Authorise,
                                "INVOKE_DENIED",
                                "invocation was not permitted",
                                InvocationRetryability::No,
                            ),
                        },
                        ServerAction::Completed { stream },
                    ]),
                    None,
                ),
                Ok(SealedInvocationExecution::Result(
                    SealedInvocationResult::PresentationFailed { .. },
                )) => (
                    sealed_presentation_failure_actions(stream, invocation),
                    None,
                ),
                Ok(SealedInvocationExecution::Cancelled { .. }) => (
                    cancellation_actions(stream, ServerAction::InvokeCancelled { stream }),
                    None,
                ),
                Err(source) => {
                    report_private_dispatch_source(&source);
                    (
                        VecDeque::from([
                            ServerAction::InvokeEvents {
                                stream,
                                events: redacted_invoke_failure(
                                    invocation,
                                    InvocationFailurePhase::Internal,
                                    "INVOKE_INTERNAL_FAILURE",
                                    "invocation could not complete",
                                    InvocationRetryability::Unknown,
                                ),
                            },
                            ServerAction::Completed { stream },
                        ]),
                        None,
                    )
                }
            };
            cancellations
                .lock()
                .expect("invocation cancellation lock")
                .remove(&stream);
            DispatchCompletion {
                actions,
                cancellation: ServerAction::InvokeCancelled { stream },
                cancellation_token: Some(cancellation_for_task),
                sealed_producer,
                sealed_invocation: Some(invocation),
                sealed_next_event_sequence: 1,
                sealed_next_outer_sequence: 2,
                start_gate: None,
                start_delivered: false,
                terminal_delivered: false,
                terminal_claimed: false,
                worker_completed: false,
                _guards: None,
            }
        });
        StartedDispatch {
            accepted,
            started: Some(started),
            start_gate: Some(start_gate),
            future,
        }
    }

    fn authorize_resource_request(&self, request: &ResourceRequest) -> bool {
        self.resource_broker
            .as_ref()
            .is_some_and(|broker| broker.take_expected_resource_request(request))
    }

    fn record_resource_terminal_provenance(
        &self,
        stream_id: u64,
        request_id: InvocationId,
        provenance: ResourceTerminalProvenance,
    ) {
        if let Some(broker) = self.resource_broker.as_ref() {
            broker.record_resource_terminal_provenance(stream_id, request_id, provenance);
        }
    }

    fn start_resource(
        &self,
        session: AuthenticatedSession,
        request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let kernel = self.kernel.clone();
        let cancellation = ResourceCancellation::new();
        let operation_cancellation = cancellation.clone();
        let future = Box::pin(async move {
            match kernel
                .start_authenticated_server_resource_producer(
                    &session,
                    &request,
                    &operation_cancellation,
                )
                .await
            {
                Ok(AuthenticatedServerResourceStart::Accepted(producer)) => {
                    let accepted = producer.accepted();
                    ResourceDispatchCompletion {
                        actions: VecDeque::from([resource_accepted_frame(accepted)]),
                        producer: Some(producer),
                        producer_waiting_bytes: None,
                        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                    }
                }
                Ok(AuthenticatedServerResourceStart::Failed {
                    stream_id,
                    request_id,
                    failure,
                }) => ResourceDispatchCompletion {
                    actions: VecDeque::from([ResourceServerFrame::Failed(
                        orna_protocol::ResourceFailed {
                            stream_id,
                            request_id,
                            target_revision: request.target_revision,
                            failure,
                        },
                    )]),
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                },
                Err(error) => {
                    report_private_dispatch_source(&error);
                    ResourceDispatchCompletion {
                        actions: resource_internal_failure(&request),
                        producer: None,
                        producer_waiting_bytes: None,
                        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                    }
                }
            }
        });
        Some(StartedResourceDispatch {
            future,
            cancellation,
        })
    }
}
