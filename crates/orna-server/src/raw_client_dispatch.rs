//! Protected dispatch for the current raw CLIENT and SERVER call subset.

use orna_core::{
    InvocationId,
    security::AuthenticatedSession,
    value::{FunctionArgument, RuntimeValue},
};
use orna_postgres::{AuthenticatedRawCallResult, PostgresKernel, PostgresKernelError};
use orna_protocol::{CallArgument, CallFailure, Event, RawCall, ServerAction};

/// One accepted raw CLIENT call bound to trusted session state.
pub struct RawClientDispatch {
    kernel: PostgresKernel,
    session: AuthenticatedSession,
    stream: u64,
    call: RawCall,
    invocation: InvocationId,
}

impl RawClientDispatch {
    /// Accepts a complete raw call for protected dispatch.
    pub fn new(
        kernel: PostgresKernel,
        session: AuthenticatedSession,
        stream: u64,
        call: RawCall,
    ) -> Self {
        Self {
            kernel,
            session,
            stream,
            call,
            invocation: InvocationId::new(),
        }
    }

    /// Returns the fresh invocation identity assigned during acceptance.
    pub const fn invocation(&self) -> InvocationId {
        self.invocation
    }

    /// Returns the action that acknowledges this accepted call.
    ///
    /// After applying this action, the transport adapter must poll [`Self::finish`]
    /// to completion, even when cancellation is already pending. Dropping the
    /// future can skip required security audit, transaction, or shutdown work.
    pub const fn accepted_action(&self) -> ServerAction {
        ServerAction::Accepted {
            stream: self.stream,
            invocation: self.invocation,
        }
    }

    /// Runs the protected raw-call kernel path and closes the public outcome.
    ///
    /// CLIENT success returns one typed value action followed by completion.
    /// SERVER success returns one value action per row followed by completion.
    /// Exactly one or two Boolean, Integer, BigInt, Float, Text, Bytes, or
    /// Reference arguments enter the protected kernel path. A pair must use two
    /// distinct parameter identities. Other argument shapes return
    /// `TARGET_UNAVAILABLE`. Calls containing a record first complete the closed
    /// transactional record preflight; other closed argument shapes do not open
    /// PostgreSQL. Raw execute denial returns
    /// `EXECUTE_DENIED`, an unavailable raw target returns
    /// `TARGET_UNAVAILABLE`, a CLIENT evaluator error returns
    /// `CLIENT_EVALUATION_FAILED`, and every other kernel error returns
    /// `INTERNAL_FAILURE`. The result retains the private typed kernel source
    /// for trusted diagnostics only.
    pub async fn finish(self) -> RawClientDispatchResult {
        let arguments = one_admitted_argument(&self.call)
            .map(|argument| vec![argument])
            .or_else(|| two_admitted_arguments(&self.call).map(Vec::from));
        if let Some(arguments) = arguments {
            return match self
                .kernel
                .dispatch_authenticated_raw_call_with_arguments(
                    &self.session,
                    self.call.function,
                    &arguments,
                )
                .await
            {
                Ok(result) => RawClientDispatchResult::success(self.stream, result),
                Err(source) => RawClientDispatchResult::from_kernel_error(self.stream, source),
            };
        }

        if !self.call.arguments.is_empty() {
            let records = self
                .call
                .arguments
                .into_iter()
                .filter_map(|argument| match argument.value {
                    RuntimeValue::Record(record) => Some(record),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !records.is_empty()
                && let Err(source) = self.kernel.preflight_record_arguments(records).await
            {
                return RawClientDispatchResult::from_kernel_error(self.stream, source);
            }
            return RawClientDispatchResult::failure(
                self.stream,
                CallFailure::TargetUnavailable,
                None,
                false,
            );
        }

        match self
            .kernel
            .dispatch_authenticated_raw_call(&self.session, self.call.function)
            .await
        {
            Ok(result) => RawClientDispatchResult::success(self.stream, result),
            Err(source) => RawClientDispatchResult::from_kernel_error(self.stream, source),
        }
    }
}

fn one_admitted_argument(call: &RawCall) -> Option<FunctionArgument> {
    let [argument] = call.arguments.as_slice() else {
        return None;
    };
    admitted_argument(argument)
}

fn two_admitted_arguments(call: &RawCall) -> Option<[FunctionArgument; 2]> {
    let [first, second] = call.arguments.as_slice() else {
        return None;
    };
    if first.parameter == second.parameter {
        return None;
    }
    Some([admitted_argument(first)?, admitted_argument(second)?])
}

fn admitted_argument(argument: &CallArgument) -> Option<FunctionArgument> {
    if !raw_argument_value_is_admitted(&argument.value) {
        return None;
    }
    FunctionArgument::new(argument.parameter, argument.value.clone()).ok()
}

fn raw_argument_value_is_admitted(value: &RuntimeValue) -> bool {
    matches!(
        value,
        RuntimeValue::Boolean(_)
            | RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
            | RuntimeValue::Reference { .. }
    )
}

/// The closed public actions and private diagnostic source for one dispatch.
pub struct RawClientDispatchResult {
    stream: u64,
    actions: Vec<ServerAction>,
    source: Option<PostgresKernelError>,
    operational_failure: bool,
}

impl RawClientDispatchResult {
    fn success(stream: u64, result: AuthenticatedRawCallResult) -> Self {
        let mut actions = match result {
            AuthenticatedRawCallResult::Client(value) => vec![ServerAction::Events {
                stream,
                events: vec![Event::Value(value)],
            }],
            AuthenticatedRawCallResult::Server(values) => values
                .into_iter()
                .map(|value| ServerAction::Events {
                    stream,
                    events: vec![Event::Value(value)],
                })
                .collect(),
        };
        actions.push(ServerAction::Completed { stream });
        Self {
            stream,
            actions,
            source: None,
            operational_failure: false,
        }
    }

    fn from_kernel_error(stream: u64, source: PostgresKernelError) -> Self {
        let (failure, operational_failure) = match source {
            PostgresKernelError::RawExecuteDenied { .. } => (CallFailure::ExecuteDenied, false),
            PostgresKernelError::ClientExecution(_) => (CallFailure::ClientEvaluationFailed, false),
            PostgresKernelError::RawCallTargetUnavailable { .. }
            | PostgresKernelError::RawServerTargetUnavailable { .. } => {
                (CallFailure::TargetUnavailable, false)
            }
            _ => (CallFailure::InternalFailure, true),
        };
        Self::failure(stream, failure, Some(source), operational_failure)
    }

    fn failure(
        stream: u64,
        failure: CallFailure,
        source: Option<PostgresKernelError>,
        operational_failure: bool,
    ) -> Self {
        Self {
            stream,
            actions: vec![ServerAction::Failed { stream, failure }],
            source,
            operational_failure,
        }
    }

    /// Returns the ordered public actions for normal completion.
    pub fn actions(&self) -> &[ServerAction] {
        &self.actions
    }

    /// Transfers the ordered public actions without cloning their values.
    pub fn into_actions(self) -> Vec<ServerAction> {
        self.actions
    }

    /// Returns the private kernel error retained for trusted diagnostics.
    pub const fn source(&self) -> Option<&PostgresKernelError> {
        self.source.as_ref()
    }

    /// Returns the terminal action when cancellation raced with completion.
    ///
    /// A transport cancellation may replace a clean kernel outcome. An
    /// operational kernel failure remains an internal failure instead.
    pub const fn action_after_cancellation(&self) -> ServerAction {
        if self.operational_failure {
            ServerAction::Failed {
                stream: self.stream,
                failure: CallFailure::InternalFailure,
            }
        } else {
            ServerAction::Cancelled {
                stream: self.stream,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use orna_client::ClientExecutionError;
    use orna_core::{
        CatalogueRevisionId, FunctionId, ObjectId, ParameterId, PrincipalId, SourceRevisionId,
        TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        revision::RevisionPair,
        security::{
            AuthenticatedSession, ExecuteDenial, Principal, PrincipalKind, PrincipalStatus,
            SecuritySnapshot,
        },
        types::{ResolvedType, StandardScalar},
        value::{EnumValue, RuntimeFloat, RuntimeValue},
    };
    use orna_postgres::{PostgresKernel, RawServerTargetError, ServerSelectError};
    use orna_protocol::{
        CallArgument, CallFailure, RawCall, ServerAction, ServerFrame, encode_server_frame,
    };

    use super::*;

    const FUNCTION: FunctionId = FunctionId::from_bytes([1; 16]);
    const PAIR: RevisionPair = RevisionPair::new(
        SourceRevisionId::from_bytes([2; 16]),
        CatalogueRevisionId::from_bytes([3; 16]),
    );
    const ENUM_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);

    fn enum_value() -> RuntimeValue {
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x32; 16]),
            vec![SchemaDefinition::new(
                orna_core::SchemaId::from_bytes([0x33; 16]),
                QualifiedSemanticName::new(["test"]).expect("test schema name is valid"),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                ENUM_TYPE,
                QualifiedSemanticName::new(["test", "state"]).expect("test enum name is valid"),
                ["member"],
            )],
            vec![],
        )
        .expect("test enum catalogue is valid");
        RuntimeValue::Enum(
            EnumValue::new(&catalogue, ENUM_TYPE, "member").expect("test enum member is active"),
        )
    }

    fn test_session() -> AuthenticatedSession {
        let principal = PrincipalId::from_bytes([4; 16]);
        SecuritySnapshot::new(
            PAIR,
            vec![],
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )
        .expect("test security snapshot is valid")
        .bind_authenticated_session(principal, vec![])
        .expect("test principal can authenticate")
    }

    fn unavailable_kernel() -> PostgresKernel {
        PostgresKernel::from_str("host=127.0.0.1 port=1 dbname=absent")
            .expect("configuration parses without connecting")
    }

    #[tokio::test]
    async fn invalid_argument_shapes_never_open_postgres_and_close_redacted() {
        // Closed pairs do not convert or open PostgreSQL. Record arguments
        // retain their separate transactional preflight, which the live
        // `standard_database.rs` proof covers.
        for (stream, arguments) in [
            (
                7,
                vec![CallArgument {
                    parameter: ParameterId::from_bytes([5; 16]),
                    value: RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean))
                        .expect("a typed test null is valid"),
                }],
            ),
            (
                10,
                vec![
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x61; 16]),
                        value: RuntimeValue::Boolean(true),
                    },
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x61; 16]),
                        value: RuntimeValue::Boolean(false),
                    },
                ],
            ),
            (
                11,
                vec![
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x62; 16]),
                        value: RuntimeValue::Boolean(true),
                    },
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x63; 16]),
                        value: enum_value(),
                    },
                ],
            ),
            (
                12,
                vec![
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x64; 16]),
                        value: RuntimeValue::Boolean(true),
                    },
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x65; 16]),
                        value: RuntimeValue::Boolean(false),
                    },
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x66; 16]),
                        value: RuntimeValue::Boolean(true),
                    },
                ],
            ),
        ] {
            let call = RawCall {
                function: FUNCTION,
                arguments,
            };
            let dispatch =
                RawClientDispatch::new(unavailable_kernel(), test_session(), stream, call);
            let invocation = dispatch.invocation();
            assert_eq!(
                dispatch.accepted_action(),
                ServerAction::Accepted { stream, invocation }
            );

            let result = dispatch.finish().await;
            assert!(result.source().is_none());
            assert_eq!(
                result.actions(),
                &[ServerAction::Failed {
                    stream,
                    failure: CallFailure::TargetUnavailable,
                }]
            );
            let failure = ServerFrame::CallFailed {
                stream,
                failure: CallFailure::TargetUnavailable,
            };
            let encoded = encode_server_frame(&failure).expect("closed failure encodes");
            assert_eq!(&encoded[18..], &[0x02, 0x00, 0x01, 0x00]);
            assert_eq!(
                result.action_after_cancellation(),
                ServerAction::Cancelled { stream }
            );
        }
    }

    #[tokio::test]
    async fn zero_argument_path_remains_a_kernel_dispatch() {
        let result = RawClientDispatch::new(
            unavailable_kernel(),
            test_session(),
            8,
            RawCall {
                function: FUNCTION,
                arguments: vec![],
            },
        )
        .finish()
        .await;
        assert!(
            result.source().is_some(),
            "zero arguments still reach the kernel"
        );
        assert_eq!(
            result.actions(),
            &[ServerAction::Failed {
                stream: 8,
                failure: CallFailure::InternalFailure,
            }]
        );
    }

    #[tokio::test]
    async fn two_distinct_admitted_arguments_reach_the_protected_kernel_path() {
        let reference = RuntimeValue::Reference {
            target: TypeId::from_bytes([0x71; 16]),
            object: ObjectId::from_bytes([0x72; 16]),
        };
        for (stream, arguments) in [
            (
                8,
                vec![
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x41; 16]),
                        value: RuntimeValue::Text(String::from("first exact value")),
                    },
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x42; 16]),
                        value: RuntimeValue::Bytes(vec![0x00, 0xff, 0x01]),
                    },
                ],
            ),
            (
                9,
                vec![
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x43; 16]),
                        value: RuntimeValue::Integer(i32::MIN),
                    },
                    CallArgument {
                        parameter: ParameterId::from_bytes([0x44; 16]),
                        value: reference,
                    },
                ],
            ),
        ] {
            let result = RawClientDispatch::new(
                unavailable_kernel(),
                test_session(),
                stream,
                RawCall {
                    function: FUNCTION,
                    arguments,
                },
            )
            .finish()
            .await;
            assert!(
                result.source().is_some(),
                "the admitted pair reaches the kernel"
            );
            assert_eq!(
                result.actions(),
                &[ServerAction::Failed {
                    stream,
                    failure: CallFailure::InternalFailure,
                }]
            );
        }
    }

    #[test]
    fn two_admitted_arguments_retain_distinct_identities_and_exact_values() {
        let first = ParameterId::from_bytes([0x51; 16]);
        let second = ParameterId::from_bytes([0x52; 16]);
        let first_value = RuntimeValue::Text(String::from("cafe\u{301} \u{65e5}\u{672c}\0"));
        let second_value = RuntimeValue::Reference {
            target: TypeId::from_bytes([0x71; 16]),
            object: ObjectId::from_bytes([0x72; 16]),
        };
        let admitted = two_admitted_arguments(&RawCall {
            function: FUNCTION,
            arguments: vec![
                CallArgument {
                    parameter: first,
                    value: first_value.clone(),
                },
                CallArgument {
                    parameter: second,
                    value: second_value.clone(),
                },
            ],
        })
        .expect("a supported scalar and Reference pair is admitted");
        assert_eq!(admitted[0].parameter(), first);
        assert_eq!(admitted[0].value(), &first_value);
        assert_eq!(admitted[1].parameter(), second);
        assert_eq!(admitted[1].value(), &second_value);

        for arguments in [
            vec![
                CallArgument {
                    parameter: first,
                    value: RuntimeValue::Boolean(true),
                },
                CallArgument {
                    parameter: first,
                    value: RuntimeValue::Boolean(false),
                },
            ],
            vec![
                CallArgument {
                    parameter: first,
                    value: enum_value(),
                },
                CallArgument {
                    parameter: second,
                    value: RuntimeValue::Boolean(true),
                },
            ],
            vec![
                CallArgument {
                    parameter: first,
                    value: RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean))
                        .expect("a typed test null is valid"),
                },
                CallArgument {
                    parameter: second,
                    value: RuntimeValue::Boolean(true),
                },
            ],
            vec![
                CallArgument {
                    parameter: first,
                    value: RuntimeValue::Boolean(true),
                },
                CallArgument {
                    parameter: second,
                    value: RuntimeValue::Boolean(false),
                },
                CallArgument {
                    parameter: ParameterId::from_bytes([0x53; 16]),
                    value: RuntimeValue::Boolean(true),
                },
            ],
        ] {
            assert!(
                two_admitted_arguments(&RawCall {
                    function: FUNCTION,
                    arguments,
                })
                .is_none(),
                "closed pair shapes do not cross the adapter boundary"
            );
        }
    }

    #[tokio::test]
    async fn one_boolean_argument_reaches_the_protected_kernel_path() {
        // One Boolean argument is an active raw argument dispatch, not a local
        // closure. An unreachable kernel must produce an operational internal
        // failure that retains its private typed source, which a local
        // redacted closure could never produce.
        for (stream, value) in [
            (9, RuntimeValue::Boolean(true)),
            (10, RuntimeValue::Boolean(false)),
        ] {
            let call = RawCall {
                function: FUNCTION,
                arguments: vec![CallArgument {
                    parameter: ParameterId::from_bytes([5; 16]),
                    value,
                }],
            };
            let dispatch =
                RawClientDispatch::new(unavailable_kernel(), test_session(), stream, call);
            let result = dispatch.finish().await;
            assert!(
                result.source().is_some(),
                "one Boolean argument must retain the private kernel source"
            );
            assert_eq!(
                result.actions(),
                &[ServerAction::Failed {
                    stream,
                    failure: CallFailure::InternalFailure,
                }]
            );
            assert_eq!(
                result.action_after_cancellation(),
                ServerAction::Failed {
                    stream,
                    failure: CallFailure::InternalFailure,
                }
            );
        }
    }

    #[tokio::test]
    async fn one_reference_argument_reaches_the_protected_kernel_path() {
        // One Reference argument is an active raw argument dispatch, not a
        // local closure. An unreachable kernel must produce an operational
        // internal failure that retains its private typed source, which a
        // local redacted closure could never produce.
        let call = RawCall {
            function: FUNCTION,
            arguments: vec![CallArgument {
                parameter: ParameterId::from_bytes([5; 16]),
                value: RuntimeValue::Reference {
                    target: TypeId::from_bytes([0x71; 16]),
                    object: ObjectId::from_bytes([0x72; 16]),
                },
            }],
        };
        let dispatch = RawClientDispatch::new(unavailable_kernel(), test_session(), 11, call);
        let result = dispatch.finish().await;
        assert!(
            result.source().is_some(),
            "one Reference argument must retain the private kernel source"
        );
        assert_eq!(
            result.actions(),
            &[ServerAction::Failed {
                stream: 11,
                failure: CallFailure::InternalFailure,
            }]
        );
        assert_eq!(
            result.action_after_cancellation(),
            ServerAction::Failed {
                stream: 11,
                failure: CallFailure::InternalFailure,
            }
        );
    }

    #[tokio::test]
    async fn every_admitted_scalar_crosses_to_the_kernel_path_without_conversion() {
        // Every admitted scalar shape reaches the authenticated kernel path
        // with its checked value intact. An unreachable kernel must produce an
        // operational internal failure that retains its private typed source; a
        // local redacted closure could never produce that outcome. The public
        // action carries no value, so a cross-boundary conversion could never
        // be observed through the closed failure surface.
        let reference = RuntimeValue::Reference {
            target: TypeId::from_bytes([0x71; 16]),
            object: ObjectId::from_bytes([0x72; 16]),
        };
        for (stream, value) in [
            (12, RuntimeValue::Boolean(true)),
            (13, RuntimeValue::Integer(i32::MAX)),
            (14, RuntimeValue::BigInt(i64::MIN)),
            (
                15,
                RuntimeValue::Float(RuntimeFloat::new(0.1).expect("0.1 is finite")),
            ),
            (
                16,
                RuntimeValue::Text(String::from("exact \u{65e5}\u{672c}")),
            ),
            (17, RuntimeValue::Bytes(vec![0x00, 0xff, 0x01])),
            (18, reference),
        ] {
            let call = RawCall {
                function: FUNCTION,
                arguments: vec![CallArgument {
                    parameter: ParameterId::from_bytes([5; 16]),
                    value,
                }],
            };
            let dispatch =
                RawClientDispatch::new(unavailable_kernel(), test_session(), stream, call);
            let result = dispatch.finish().await;
            assert!(
                result.source().is_some(),
                "an admitted scalar must retain the private kernel source"
            );
            assert_eq!(
                result.actions(),
                &[ServerAction::Failed {
                    stream,
                    failure: CallFailure::InternalFailure,
                }]
            );
            assert_eq!(
                result.action_after_cancellation(),
                ServerAction::Failed {
                    stream,
                    failure: CallFailure::InternalFailure,
                }
            );
        }
    }

    #[test]
    fn one_admitted_argument_preserves_parameter_identity_and_value() {
        let reference = RuntimeValue::Reference {
            target: TypeId::from_bytes([0x71; 16]),
            object: ObjectId::from_bytes([0x72; 16]),
        };
        let text = RuntimeValue::Text(String::from("caf\u{e9} e\u{301}\n\t\u{65e5}\u{672c}"));
        let bytes = RuntimeValue::Bytes(vec![0x00, 0xff, 0x7f, 0x00, 0x01]);
        for (index, value) in [
            RuntimeValue::Boolean(true),
            RuntimeValue::Boolean(false),
            RuntimeValue::Integer(i32::MIN),
            RuntimeValue::BigInt(i64::MAX),
            RuntimeValue::Float(RuntimeFloat::new(0.5).expect("0.5 is finite")),
            text.clone(),
            bytes.clone(),
            reference.clone(),
        ]
        .into_iter()
        .enumerate()
        {
            let parameter = ParameterId::from_bytes([5 + index as u8; 16]);
            let converted = one_admitted_argument(&RawCall {
                function: FUNCTION,
                arguments: vec![CallArgument {
                    parameter,
                    value: value.clone(),
                }],
            })
            .expect("one admitted argument must cross the adapter boundary");
            assert_eq!(converted.parameter(), parameter);
            assert_eq!(converted.value(), &value);
        }

        // The exact float bit pattern crosses unchanged: the adapter retains
        // the checked RuntimeFloat clone instead of re-rendering a literal.
        let float = RuntimeValue::Float(RuntimeFloat::new(-0.25).expect("-0.25 is finite"));
        let converted = one_admitted_argument(&RawCall {
            function: FUNCTION,
            arguments: vec![CallArgument {
                parameter: ParameterId::from_bytes([0x60; 16]),
                value: float.clone(),
            }],
        })
        .expect("one Float argument must cross the adapter boundary");
        let RuntimeValue::Float(stored) = converted.value() else {
            panic!("the converted Float argument lost its runtime shape");
        };
        assert_eq!(stored.value().to_bits(), (-0.25_f64).to_bits());

        // The conversion boundary rejects every closed shape before dispatch:
        // one typed NULL, two Booleans, and an empty argument set all close at
        // the helper itself. Catalogue-bound closed shapes such as enum,
        // record, opaque, and constructed values cannot be built without an
        // active revision and are proven through the public adapter in the live
        // `standard_database.rs` scalar dispatch proof.
        assert!(
            one_admitted_argument(&RawCall {
                function: FUNCTION,
                arguments: vec![CallArgument {
                    parameter: ParameterId::from_bytes([5; 16]),
                    value: RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean))
                        .expect("a typed test null is valid"),
                }],
            })
            .is_none()
        );
        assert!(
            one_admitted_argument(&RawCall {
                function: FUNCTION,
                arguments: vec![
                    CallArgument {
                        parameter: ParameterId::from_bytes([5; 16]),
                        value: RuntimeValue::Boolean(true),
                    },
                    CallArgument {
                        parameter: ParameterId::from_bytes([6; 16]),
                        value: RuntimeValue::Boolean(false),
                    },
                ],
            })
            .is_none()
        );
        assert!(
            one_admitted_argument(&RawCall {
                function: FUNCTION,
                arguments: vec![],
            })
            .is_none()
        );
    }

    #[test]
    fn every_acceptance_uses_a_fresh_invocation_identity() {
        let call = RawCall {
            function: FUNCTION,
            arguments: vec![],
        };
        let first = RawClientDispatch::new(unavailable_kernel(), test_session(), 1, call.clone());
        let second = RawClientDispatch::new(unavailable_kernel(), test_session(), 2, call);

        assert_ne!(first.invocation(), second.invocation());
    }

    #[test]
    fn success_maps_to_one_value_event_then_completion() {
        let success = RawClientDispatchResult::success(
            8,
            AuthenticatedRawCallResult::Client(RuntimeValue::Boolean(true)),
        );
        let expected = vec![
            ServerAction::Events {
                stream: 8,
                events: vec![Event::Value(RuntimeValue::Boolean(true))],
            },
            ServerAction::Completed { stream: 8 },
        ];

        assert!(success.source().is_none());
        assert_eq!(success.actions(), expected);
        assert_eq!(
            success.action_after_cancellation(),
            ServerAction::Cancelled { stream: 8 }
        );
        assert_eq!(success.into_actions(), expected);
    }

    #[test]
    fn server_success_maps_each_row_to_one_event_and_preserves_zero_rows() {
        let values = RawClientDispatchResult::success(
            12,
            AuthenticatedRawCallResult::Server(vec![
                RuntimeValue::Integer(1),
                RuntimeValue::Integer(2),
            ]),
        );
        assert_eq!(
            values.actions(),
            [
                ServerAction::Events {
                    stream: 12,
                    events: vec![Event::Value(RuntimeValue::Integer(1))],
                },
                ServerAction::Events {
                    stream: 12,
                    events: vec![Event::Value(RuntimeValue::Integer(2))],
                },
                ServerAction::Completed { stream: 12 },
            ]
        );

        let empty =
            RawClientDispatchResult::success(13, AuthenticatedRawCallResult::Server(Vec::new()));
        assert_eq!(empty.actions(), [ServerAction::Completed { stream: 13 }]);
    }

    #[test]
    fn every_kernel_error_family_uses_the_closed_mapping_and_precedence() {
        let denied = RawClientDispatchResult::from_kernel_error(
            9,
            PostgresKernelError::RawExecuteDenied {
                pair: PAIR,
                function: FUNCTION,
                reason: ExecuteDenial::MissingExecuteGrant,
            },
        );
        assert!(matches!(
            denied.source(),
            Some(PostgresKernelError::RawExecuteDenied { .. })
        ));
        assert_eq!(
            denied.actions(),
            &[ServerAction::Failed {
                stream: 9,
                failure: CallFailure::ExecuteDenied,
            }]
        );
        assert_eq!(
            denied.action_after_cancellation(),
            ServerAction::Cancelled { stream: 9 }
        );

        let evaluator = RawClientDispatchResult::from_kernel_error(
            10,
            PostgresKernelError::ClientExecution(ClientExecutionError::FunctionNotFound {
                pair: PAIR,
                function: FUNCTION,
            }),
        );
        assert!(matches!(
            evaluator.source(),
            Some(PostgresKernelError::ClientExecution(_))
        ));
        assert_eq!(
            evaluator.actions(),
            &[ServerAction::Failed {
                stream: 10,
                failure: CallFailure::ClientEvaluationFailed,
            }]
        );
        assert_eq!(
            evaluator.action_after_cancellation(),
            ServerAction::Cancelled { stream: 10 }
        );

        let unavailable = RawClientDispatchResult::from_kernel_error(
            11,
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Select(ServerSelectError::RawTarget {
                    function: FUNCTION,
                    rule: "test",
                }),
            },
        );
        assert_eq!(
            unavailable.actions(),
            &[ServerAction::Failed {
                stream: 11,
                failure: CallFailure::TargetUnavailable,
            }]
        );
        assert_eq!(
            unavailable.action_after_cancellation(),
            ServerAction::Cancelled { stream: 11 }
        );

        let call_unavailable = RawClientDispatchResult::from_kernel_error(
            13,
            PostgresKernelError::RawCallTargetUnavailable {
                function: FUNCTION,
                rule: "test",
            },
        );
        assert!(matches!(
            call_unavailable.source(),
            Some(PostgresKernelError::RawCallTargetUnavailable {
                function,
                rule,
            }) if *function == FUNCTION && *rule == "test"
        ));
        assert_eq!(
            call_unavailable.actions(),
            &[ServerAction::Failed {
                stream: 13,
                failure: CallFailure::TargetUnavailable,
            }]
        );
        assert_eq!(
            call_unavailable.action_after_cancellation(),
            ServerAction::Cancelled { stream: 13 }
        );
        // The public TARGET_UNAVAILABLE frame is the closed redacted form: it
        // carries no argument value, while the private typed source remains
        // available for trusted diagnostics only.
        let encoded = encode_server_frame(&ServerFrame::CallFailed {
            stream: 13,
            failure: CallFailure::TargetUnavailable,
        })
        .expect("redacted target-unavailable failure encodes");
        assert_eq!(&encoded[18..], &[0x02, 0x00, 0x01, 0x00]);

        let operational = RawClientDispatchResult::from_kernel_error(
            12,
            PostgresKernelError::MigrationMismatch { version: 99 },
        );
        assert!(matches!(
            operational.source(),
            Some(PostgresKernelError::MigrationMismatch { version: 99 })
        ));
        assert_eq!(
            operational.actions(),
            &[ServerAction::Failed {
                stream: 12,
                failure: CallFailure::InternalFailure,
            }]
        );
        assert_eq!(
            operational.action_after_cancellation(),
            ServerAction::Failed {
                stream: 12,
                failure: CallFailure::InternalFailure,
            }
        );
    }
}
