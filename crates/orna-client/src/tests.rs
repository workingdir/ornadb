use super::{
    ACTION_FAILURE_CODE, ClientActionDescriptor, ClientActionError, ClientActionOutcome,
    ClientActionState, ClientExecutionContext, ClientExecutionError, ClientExpressionError,
    ClientExternalContractRequest, ClientReferenceLoader, ClientReferenceLoaderError,
    ClientReferenceLoaderFixture, ClientReferenceObject, ClientResource, ClientResourceCompletion,
    ClientResourceExecutor, ClientResourceKey, ClientResourceRequest, ClientResourceStatus,
    ClientStateStore, ControlFlowBinaryOperator, DeterministicClientResourceExecutor, ResourceKind,
    action_target_result_type, capability, complete_client_action, decode_action_payload,
    encode_action_payload, evaluate_client_function_with_executor, trigger_client_action,
};
use orna_artifact::client_plan::{
    ActionTargetDomain, ClientExpressionNode, ControlFlowClientPlan, InspectProjection,
};
use std::{cell::Cell, collections::HashMap, rc::Rc, time::SystemTime};

use orna_core::{
    CallSiteId, CatalogueRevisionId, FieldId, FunctionId, FunctionRevisionId, InvocationId,
    LocalId, ObjectId, ParameterId, PrincipalId, SchemaId, SourceBundleId, SourceRevisionId,
    SourceUnitId, StateSlotId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
        function_declaration_digest, function_semantic_digest,
        function_semantic_digest_with_version, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity, FunctionVolatility, ObjectTypeDefinition,
        ParameterDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        RecordValueTypeDefinition, SchemaDefinition, ValueTypeDefinition,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        DefinitionIdentity, DefinitionOrigin, DefinitionReference, DefinitionReferenceKind,
        DefinitionReferenceTarget, DeployableRevision, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, FunctionSemanticHashVersion, RevisionInvariantError, RevisionPair,
        Sha256Digest, SourceOrigin, StoredSourceRevision, StoredSourceUnit,
        VerifiedStandardLibrarySnapshot,
    },
    security::{
        AuthorisedInvocation, ExecuteDecision, ExecuteGrant, InvocationTarget, Principal,
        PrincipalKind, PrincipalStatus, RoleMembership, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    state::{UserStateCell, UserStateKey, UserStateWriteOutcome, UserStateWriteResult},
    types::{ResolvedType, StandardScalar, TypeDescriptor},
    value::{
        ConstructedValueKind, FunctionArgument, OpaqueValue, RecordValue, RuntimeFloat,
        RuntimeValue,
    },
};

#[derive(Default)]
struct RecordingActionExecutor {
    executed: Vec<ClientResourceRequest>,
    cancelled: Vec<ClientResourceRequest>,
    abandoned: Vec<ClientResourceRequest>,
    pending: Option<ClientResourceRequest>,
    late: Option<ClientResourceCompletion>,
    late_dropped: usize,
    result: Option<RuntimeValue>,
    pending_identity: Option<ClientResourceRequest>,
    execute_stream_values: bool,
    cancel_pending: bool,
    cancel_stream_values: bool,
    cancel_identity: Option<ClientResourceRequest>,
    cancel_terminal_identity: Option<ClientResourceRequest>,
    cancel_value: Option<RuntimeValue>,
    abandon_failure: bool,
}

impl RecordingActionExecutor {
    fn new(result: Option<RuntimeValue>) -> Self {
        Self {
            result,
            ..Self::default()
        }
    }

    fn with_execute_stream_values(mut self) -> Self {
        self.execute_stream_values = true;
        self
    }
    fn with_cancel_pending(mut self) -> Self {
        self.cancel_pending = true;
        self
    }
    fn with_cancel_stream_values(mut self) -> Self {
        self.cancel_stream_values = true;
        self
    }
    fn with_pending_identity(mut self, request: ClientResourceRequest) -> Self {
        self.pending_identity = Some(request);
        self
    }

    fn with_cancel_pending_identity(mut self, request: ClientResourceRequest) -> Self {
        self.cancel_pending = true;
        self.cancel_identity = Some(request);
        self
    }

    fn with_cancel_terminal_identity(mut self, request: ClientResourceRequest) -> Self {
        self.cancel_terminal_identity = Some(request);
        self
    }

    fn with_cancel_value(mut self, value: RuntimeValue) -> Self {
        self.cancel_value = Some(value);
        self
    }
    fn with_abandon_failure(mut self) -> Self {
        self.abandon_failure = true;
        self
    }
}

impl ClientResourceExecutor for RecordingActionExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.executed.push(request.clone());
        if self.pending.is_some() {
            return request.failed("resource.executor.busy".to_owned());
        }
        if self.execute_stream_values {
            self.pending = Some(request.clone());
            return request.stream_values(vec![RuntimeValue::Boolean(true)]);
        }
        match self.result.clone() {
            Some(value) => request.ready(value),
            None => {
                self.pending = Some(request.clone());
                self.pending_identity.take().unwrap_or(request).pending()
            }
        }
    }
    fn read_input(&mut self, _context: ClientExecutionContext) -> Result<RuntimeValue, String> {
        Ok(RuntimeValue::Text("from session".to_owned()))
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.cancelled.push(request.clone());
        if let Some(identity) = self.cancel_terminal_identity.take() {
            return identity.cancelled();
        }
        if self.cancel_pending {
            return self.cancel_identity.take().unwrap_or(request).pending();
        }
        if self.cancel_stream_values {
            return request.stream_values(vec![RuntimeValue::Boolean(true)]);
        }
        self.pending = self
            .pending
            .take()
            .filter(|pending| pending.request_id() != request.request_id());
        if let Some(value) = self.cancel_value.clone() {
            request.ready(value)
        } else {
            request.cancelled()
        }
    }

    fn abandon(&mut self, request: ClientResourceRequest) -> Result<(), String> {
        self.abandoned.push(request.clone());
        if self.abandon_failure {
            return Err("resource executor cannot abandon a pending request".to_owned());
        }
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if pending.request_id() != request.request_id() {
            self.pending = Some(pending);
            return Err("resource executor request mismatch".to_owned());
        }
        self.late = Some(request.ready(RuntimeValue::Text("late".to_owned())));
        Ok(())
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        if self.late.take().is_some() {
            self.late_dropped += 1;
        }
        None
    }
}

#[derive(Clone, Copy, Debug)]
enum ReplacementEvaluationOutcome {
    Pending,
    Failed,
    Invalid,
    Expression,
}

struct ReplacementTerminalExecutor {
    outcome: ReplacementEvaluationOutcome,
    executed: Vec<ClientResourceRequest>,
    cancelled: Vec<ClientResourceRequest>,
}

impl ReplacementTerminalExecutor {
    fn new(outcome: ReplacementEvaluationOutcome) -> Self {
        Self {
            outcome,
            executed: Vec::new(),
            cancelled: Vec::new(),
        }
    }
}

impl ClientResourceExecutor for ReplacementTerminalExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.executed.push(request.clone());
        if self.executed.len() == 1 {
            return request.pending();
        }
        match self.outcome {
            ReplacementEvaluationOutcome::Pending => request.pending(),
            ReplacementEvaluationOutcome::Failed => {
                request.failed("replacement.failure".to_owned())
            }
            ReplacementEvaluationOutcome::Invalid => request.ready(RuntimeValue::Integer(7)),
            ReplacementEvaluationOutcome::Expression => {
                request.ready(RuntimeValue::Text("replacement-expression".to_owned()))
            }
        }
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.cancelled.push(request.clone());
        if self.cancelled.len() == 1 {
            request.ready(RuntimeValue::Text("old-terminal".to_owned()))
        } else {
            request.pending()
        }
    }
}

#[derive(Default)]
struct FailingActionExecutor {
    request: Option<ClientResourceRequest>,
}

#[derive(Default)]
struct CancelledActionExecutor {
    request: Option<ClientResourceRequest>,
}

impl ClientResourceExecutor for CancelledActionExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.request = Some(request.clone());
        request.cancelled()
    }
    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.cancelled()
    }
}

#[derive(Default)]
struct MalformedResourceExecutor {
    executed: Option<ClientResourceRequest>,
    cancelled: Vec<ClientResourceRequest>,
    cancel_ready: bool,
    stale_request_id: bool,
}

impl ClientResourceExecutor for MalformedResourceExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.executed = Some(request.clone());
        if self.stale_request_id {
            ClientResourceCompletion::Ready {
                request_id: InvocationId::from_bytes([0xff; 16]),
                key: request.key(),
                generation: request.generation(),
                value: RuntimeValue::Integer(7),
            }
        } else {
            request.ready(RuntimeValue::Integer(7))
        }
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.cancelled.push(request.clone());
        if self.cancel_ready {
            request.ready(RuntimeValue::Text("cancelled-ready".to_owned()))
        } else {
            request.cancelled()
        }
    }
}

#[derive(Default)]
struct PollingTestExecutor {
    pending: Option<ClientResourceRequest>,
}

impl ClientResourceExecutor for PollingTestExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.pending = Some(request.clone());
        request.pending()
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        self.pending.take().map(|request| {
            let value = match request.expected_type() {
                ResolvedType::Scalar(StandardScalar::CharacterLargeObject)
                | ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID) => {
                    RuntimeValue::Text("polled".to_owned())
                }
                _ => RuntimeValue::Boolean(true),
            };
            request.ready(value)
        })
    }
}

struct StreamThenTerminalExecutor {
    calls: usize,
    stale: Option<ClientResourceRequest>,
}

impl ClientResourceExecutor for StreamThenTerminalExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.calls += 1;
        match self.calls {
            1 => request.stream_values(vec![RuntimeValue::Boolean(true)]),
            2 => request.stream_completed(),
            _ => self
                .stale
                .take()
                .unwrap_or_else(|| request.clone())
                .pending(),
        }
    }
}

struct StreamBatchTestExecutor {
    value: bool,
}

impl ClientResourceExecutor for StreamBatchTestExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.stream_values(vec![RuntimeValue::Boolean(self.value)])
    }
    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.cancelled()
    }
}

impl ClientResourceExecutor for FailingActionExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.request = Some(request.clone());
        request.failed("secret.executor.detail".to_owned())
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        request.cancelled()
    }
}

fn v9_constructor_value(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    revision: FunctionRevisionId,
    identity: &str,
    arguments: Vec<(ParameterId, RuntimeValue)>,
) -> RuntimeValue {
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0x92; 16]),
        observer_lineage: None,
    };
    let spec = super::standard_ui_constructor_spec(active, context, identity)
        .expect("the V9 standard constructor is intrinsically recognised");
    super::evaluate_standard_ui_constructor(active, context, spec, &arguments)
        .expect("the V9 standard constructor accepts its checked arguments")
}

fn v9_constructor_body(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    revision: FunctionRevisionId,
    identity: &str,
    arguments: Vec<(ParameterId, RuntimeValue)>,
) -> serde_json::Value {
    let RuntimeValue::Opaque(value) =
        v9_constructor_value(active, function, revision, identity, arguments)
    else {
        panic!("the constructor returns std.ui.UI");
    };
    super::decode_ui_constructor_body(value.canonical_payload())
        .expect("the generated frame is canonical")
}

#[test]
fn standard_ui_text_constructor_builds_canonical_value_without_executor() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function: super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x91; 16]),
        observer_lineage: None,
    };
    let spec =
        super::standard_ui_constructor_spec(&active, context, super::STD_UI_TEXT_RUNTIME_CONTRACT)
            .expect("the V9 standard text constructor is intrinsically recognised");
    let value = super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[(
            super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("Ready".to_owned()),
        )],
    )
    .expect("pure UI construction does not require an executor");
    let RuntimeValue::Opaque(value) = value else {
        panic!("the constructor returns std.ui.UI");
    };
    let body = super::decode_ui_constructor_body(value.canonical_payload())
        .expect("the generated frame is canonical");
    assert_eq!(
        body,
        serde_json::json!({
            "kind": "node",
            "contract": {
                "id": "std.ui.text",
                "name": "std.ui.text",
                "version": "1.0"
            },
            "properties": {
                "text": {"type": "std.types.text", "value": "Ready"}
            },
            "slots": {},
            "actions": {}
        })
    );
}
#[test]
fn standard_ui_constructors_build_all_closed_mappings_and_singleton_nesting() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let text = v9_constructor_value(
        &active,
        super::STD_UI_TEXT_FUNCTION_ID,
        super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        super::STD_UI_TEXT_RUNTIME_CONTRACT,
        vec![(
            super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("Ready".to_owned()),
        )],
    );
    let text_body = v9_constructor_body(
        &active,
        super::STD_UI_TEXT_FUNCTION_ID,
        super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        super::STD_UI_TEXT_RUNTIME_CONTRACT,
        vec![(
            super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("Ready".to_owned()),
        )],
    );
    assert_eq!(
        text_body,
        serde_json::json!({
            "kind": "node",
            "contract": {"id": "std.ui.text", "name": "std.ui.text", "version": "1.0"},
            "properties": {"text": {"type": "std.types.text", "value": "Ready"}},
            "slots": {},
            "actions": {}
        })
    );

    assert_eq!(
        v9_constructor_body(
            &active,
            super::STD_UI_BUTTON_FUNCTION_ID,
            super::STD_UI_BUTTON_FUNCTION_REVISION_ID,
            super::STD_UI_BUTTON_RUNTIME_CONTRACT,
            vec![
                (
                    super::STD_UI_BUTTON_LABEL_PARAMETER_ID,
                    RuntimeValue::Text("Run".to_owned()),
                ),
                (
                    super::STD_UI_BUTTON_ENABLED_PARAMETER_ID,
                    RuntimeValue::Boolean(true),
                ),
            ],
        ),
        serde_json::json!({
            "kind": "node",
            "contract": {"id": "std.ui.button", "name": "std.ui.button", "version": "1.0"},
            "properties": {
                "label": {"type": "std.types.text", "value": "Run"},
                "enabled": {"type": "std.types.boolean", "value": true}
            },
            "slots": {},
            "actions": {}
        })
    );
    assert_eq!(
        v9_constructor_body(
            &active,
            super::STD_UI_TEXT_INPUT_FUNCTION_ID,
            super::STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID,
            super::STD_UI_TEXT_INPUT_RUNTIME_CONTRACT,
            vec![
                (
                    super::STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
                    RuntimeValue::Text("".to_owned()),
                ),
                (
                    super::STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
                    RuntimeValue::Text("Search".to_owned()),
                ),
                (
                    super::STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
                    RuntimeValue::Boolean(false),
                ),
            ],
        ),
        serde_json::json!({
            "kind": "node",
            "contract": {
                "id": "std.ui.text_input",
                "name": "std.ui.text_input",
                "version": "1.0"
            },
            "properties": {
                "text": {"type": "std.types.text", "value": ""},
                "placeholder": {"type": "std.types.text", "value": "Search"},
                "enabled": {"type": "std.types.boolean", "value": false}
            },
            "slots": {},
            "actions": {}
        })
    );

    for (function, revision, identity, parameter, contract) in [
        (
            super::STD_UI_PANEL_FUNCTION_ID,
            super::STD_UI_PANEL_FUNCTION_REVISION_ID,
            super::STD_UI_PANEL_RUNTIME_CONTRACT,
            super::STD_UI_PANEL_CONTENT_PARAMETER_ID,
            "std.ui.panel",
        ),
        (
            super::STD_UI_ROW_FUNCTION_ID,
            super::STD_UI_ROW_FUNCTION_REVISION_ID,
            super::STD_UI_ROW_RUNTIME_CONTRACT,
            super::STD_UI_ROW_CONTENT_PARAMETER_ID,
            "std.ui.row",
        ),
        (
            super::STD_UI_COLUMN_FUNCTION_ID,
            super::STD_UI_COLUMN_FUNCTION_REVISION_ID,
            super::STD_UI_COLUMN_RUNTIME_CONTRACT,
            super::STD_UI_COLUMN_CONTENT_PARAMETER_ID,
            "std.ui.column",
        ),
        (
            super::STD_UI_TABS_FUNCTION_ID,
            super::STD_UI_TABS_FUNCTION_REVISION_ID,
            super::STD_UI_TABS_RUNTIME_CONTRACT,
            super::STD_UI_TABS_CONTENT_PARAMETER_ID,
            "std.ui.tabs",
        ),
    ] {
        assert_eq!(
            v9_constructor_body(
                &active,
                function,
                revision,
                identity,
                vec![(parameter, text.clone())],
            ),
            serde_json::json!({
                "kind": "node",
                "contract": {"id": contract, "name": contract, "version": "1.0"},
                "properties": {},
                "slots": {"content": [text_body.clone()]},
                "actions": {}
            })
        );
    }
}

#[test]
fn standard_ui_constructor_dispatches_without_an_executor() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function: super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x94; 16]),
        observer_lineage: None,
    };
    let expression = ClientExpressionNode::ExternalContract {
        identity: super::STD_UI_TEXT_RUNTIME_CONTRACT.to_owned(),
    };
    let arguments = [(
        super::STD_UI_TEXT_PARAMETER_ID,
        RuntimeValue::Text("headless".to_owned()),
    )];
    let grants = capability::LocalCapabilityGrantSet::default();
    let mut state = ClientStateStore::default();
    let lineage = super::ObserverLineage::compatibility(context);
    let mut executor: Option<&mut dyn ClientResourceExecutor> = None;
    let mut locals = super::ClientLocalEnvironment::new();
    let mut fuel = super::ClientExecutionFuel::new();
    let value = super::evaluate_expression_with_fuel(
        &active,
        &expression,
        context,
        &lineage,
        &arguments,
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x95; 16]),
        &mut executor,
        &mut locals,
        &mut fuel,
    )
    .expect("constructor expressions do not require a runtime executor");
    assert!(
        matches!(value, RuntimeValue::Opaque(value) if value.opaque_type() == super::STD_UI_TYPE_ID)
    );
}

#[test]
fn standard_ui_constructor_rejects_wrong_order_and_runtime_kinds() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function: super::STD_UI_BUTTON_FUNCTION_ID,
        function_revision: super::STD_UI_BUTTON_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x96; 16]),
        observer_lineage: None,
    };
    let spec = super::standard_ui_constructor_spec(
        &active,
        context,
        super::STD_UI_BUTTON_RUNTIME_CONTRACT,
    )
    .expect("the V9 button constructor is intrinsically recognised");
    let wrong_order = super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[
            (
                super::STD_UI_BUTTON_ENABLED_PARAMETER_ID,
                RuntimeValue::Boolean(true),
            ),
            (
                super::STD_UI_BUTTON_LABEL_PARAMETER_ID,
                RuntimeValue::Text("Run".to_owned()),
            ),
        ],
    )
    .unwrap_err();
    assert!(matches!(
        *wrong_order,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::InvalidCall,
            ..
        }
    ));

    let wrong_kind = super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[
            (
                super::STD_UI_BUTTON_LABEL_PARAMETER_ID,
                RuntimeValue::Boolean(true),
            ),
            (
                super::STD_UI_BUTTON_ENABLED_PARAMETER_ID,
                RuntimeValue::Boolean(true),
            ),
        ],
    )
    .unwrap_err();
    assert!(matches!(
        *wrong_kind,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
}
#[test]
fn standard_ui_constructor_rejects_malformed_content_frame() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let registry = orna_standard::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("the V9 fixture pins a standard snapshot"),
    )
    .expect("the V9 fixture has the registered UI codec");
    let mut malformed = super::UI_MAGIC.as_bytes().to_vec();
    malformed.extend_from_slice(&1_u32.to_be_bytes());
    malformed.push(b' ');
    assert!(matches!(
        OpaqueValue::new(&active, &registry, super::STD_UI_TYPE_ID, malformed.clone()),
        Err(super::OpaqueValueError::InvalidJsonBody { .. })
    ));
    assert!(matches!(
        super::decode_ui_constructor_body(&malformed),
        Err(super::OpaqueValueError::InvalidJsonBody { .. })
    ));
}

#[test]
fn standard_ui_constructor_rejects_text_over_runtime_bound() {
    let standard = standard_v9();
    let active = empty_version_two_active(&standard);
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function: super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x97; 16]),
        observer_lineage: None,
    };
    let spec =
        super::standard_ui_constructor_spec(&active, context, super::STD_UI_TEXT_RUNTIME_CONTRACT)
            .expect("the V9 text constructor is intrinsically recognised");
    let error = super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[(
            super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("x".repeat(super::CLIENT_MAX_RUNTIME_TEXT_BYTES + 1)),
        )],
    )
    .unwrap_err();
    assert!(matches!(
        *error,
        super::ClientExecutionError::InvalidOpaqueValue {
            source: super::ClientOpaqueValueError::Value(
                super::OpaqueValueError::InvalidFrameLength { .. }
            ),
            ..
        }
    ));
}

#[test]
fn standard_ui_constructor_requires_the_v9_standard_snapshot() {
    let standard = standard_v7();
    let active = empty_version_two_active(&standard);
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function: super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x98; 16]),
        observer_lineage: None,
    };
    let spec =
        super::standard_ui_constructor_spec(&active, context, super::STD_UI_TEXT_RUNTIME_CONTRACT)
            .expect("identity matching is checked before standard admission");
    let error = super::evaluate_standard_ui_constructor(
        &active,
        context,
        spec,
        &[(
            super::STD_UI_TEXT_PARAMETER_ID,
            RuntimeValue::Text("not-v9".to_owned()),
        )],
    )
    .unwrap_err();
    assert!(matches!(
        *error,
        super::ClientExecutionError::InvalidOpaqueValue {
            source: super::ClientOpaqueValueError::Registry(_),
            ..
        }
    ));
}

#[test]
fn same_string_application_function_remains_generic_external_contract() {
    let active = active_with_application_ui_text_identity();
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function: super::STD_UI_TEXT_FUNCTION_ID,
        function_revision: super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        parent_invocation_id: InvocationId::from_bytes([0x99; 16]),
        observer_lineage: None,
    };
    assert!(
        super::standard_ui_constructor_spec(&active, context, super::STD_UI_TEXT_RUNTIME_CONTRACT)
            .is_none(),
        "application definitions retain precedence over standard intrinsics"
    );
    let expression = ClientExpressionNode::ExternalContract {
        identity: super::STD_UI_TEXT_RUNTIME_CONTRACT.to_owned(),
    };
    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_external_contract(|request| {
            assert_eq!(request.identity(), super::STD_UI_TEXT_RUNTIME_CONTRACT);
            Ok(RuntimeValue::Boolean(true))
        });
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let grants = capability::LocalCapabilityGrantSet::default();
    let mut state = ClientStateStore::default();
    let lineage = super::ObserverLineage::compatibility(context);
    let mut locals = super::ClientLocalEnvironment::new();
    let mut fuel = super::ClientExecutionFuel::new();
    let value = super::evaluate_expression_with_fuel(
        &active,
        &expression,
        context,
        &lineage,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x9a; 16]),
        &mut executor_slot,
        &mut locals,
        &mut fuel,
    )
    .expect("same-string application contracts still use the generic executor");
    assert_eq!(value, RuntimeValue::Boolean(true));
}

#[test]
fn inspect_executor_default_is_fail_closed() {
    let context = super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x11; 16]),
            CatalogueRevisionId::from_bytes([0x22; 16]),
        ),
        function: FunctionId::from_bytes([0x33; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x44; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x55; 16]),
        observer_lineage: None,
    };
    let operation = super::ClientInspectOperation::Snapshot {
        target: RuntimeValue::Boolean(true),
    };
    let request = super::ClientInspectRequest::new(context, operation);
    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        });
    assert_eq!(
        executor.inspect(request),
        Err("inspect.runtime_unavailable".to_owned())
    );
}
#[test]
fn inspect_render_external_contract_dispatches_typed_arguments() {
    let context = super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        ),
        function: FunctionId::from_bytes([0x73; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x74; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x75; 16]),
        observer_lineage: None,
    };
    let parameter = ParameterId::from_bytes([0x76; 16]);
    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_external_contract(move |request| {
            assert_eq!(request.identity(), super::INSPECT_RENDER_CONTRACT);
            assert_eq!(request.context(), context);
            assert_eq!(
                request.arguments(),
                &[(parameter, RuntimeValue::Boolean(true))],
            );
            Ok(RuntimeValue::Text("ui".to_owned()))
        });
    let mut optional: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
    assert_eq!(
        super::evaluate_external_contract(
            super::INSPECT_RENDER_CONTRACT,
            context,
            super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
            &mut optional,
        )
        .unwrap(),
        RuntimeValue::Text("ui".to_owned()),
    );
}

#[test]
fn generic_external_contracts_forward_and_fail_closed_without_executor() {
    let context = super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x81; 16]),
            CatalogueRevisionId::from_bytes([0x82; 16]),
        ),
        function: FunctionId::from_bytes([0x83; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x84; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x85; 16]),
        observer_lineage: None,
    };
    let parameter = ParameterId::from_bytes([0x86; 16]);
    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_external_contract(move |request| {
            assert_eq!(request.identity(), "app.other@1");
            assert_eq!(
                request.arguments(),
                &[(parameter, RuntimeValue::Boolean(true))]
            );
            Ok(RuntimeValue::Boolean(false))
        });
    let mut optional: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
    assert_eq!(
        super::evaluate_external_contract(
            "app.other@1",
            context,
            super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
            &mut optional,
        ),
        Ok(RuntimeValue::Boolean(false)),
    );

    let mut nested_provider =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_external_contract(|request| {
            assert_eq!(request.identity(), "app.other@1");
            Ok(RuntimeValue::Boolean(true))
        });
    let request = super::ClientExternalContractRequest::new(
        context,
        "app.other@1",
        vec![(parameter, RuntimeValue::Boolean(true))],
    );
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut nested_provider,
        pending_request: None,
    };
    assert_eq!(
        nested.external_contract(request),
        Ok(RuntimeValue::Boolean(true)),
        "nested CLIENT actions must retain external-contract providers",
    );

    let mut forwarding_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut nested);
    assert_eq!(
        super::evaluate_external_contract(
            "app.other@1",
            context,
            super::ObserverLineage::compatibility(context),
            &[(parameter, RuntimeValue::Boolean(true))],
            &mut forwarding_slot,
        ),
        Ok(RuntimeValue::Boolean(true)),
    );

    let mut failing =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_external_contract(|_| Err("inspect.denied".to_owned()));
    let mut failing_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut failing);
    assert!(matches!(
        super::evaluate_external_contract(
            "app.other@1",
            context,
            super::ObserverLineage::compatibility(context),
            &[],
            &mut failing_slot,
        ),
        Err(super::ClientExecutionError::ExternalContract { identity, .. })
            if identity == "app.other@1"
    ));

    let mut default_executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        });
    let mut default_slot: Option<&mut dyn super::ClientResourceExecutor> =
        Some(&mut default_executor);
    assert_eq!(
        super::evaluate_external_contract(
            super::INSPECT_RENDER_CONTRACT,
            context,
            super::ObserverLineage::compatibility(context),
            &[],
            &mut default_slot,
        ),
        Err(super::ClientExecutionError::Inspect {
            context,
            source: super::ClientInspectError::Failed("inspect.runtime_unavailable".to_owned()),
        }),
    );

    let mut absent: Option<&mut dyn super::ClientResourceExecutor> = None;
    assert!(matches!(
        super::evaluate_external_contract(
            "app.other@1",
            context,
            super::ObserverLineage::compatibility(context),
            &[],
            &mut absent,
        ),
        Err(super::ClientExecutionError::ExternalContract { identity, .. })
            if identity == "app.other@1"
    ));
    assert_eq!(
        super::evaluate_external_contract(
            super::INSPECT_RENDER_CONTRACT,
            context,
            super::ObserverLineage::compatibility(context),
            &[],
            &mut absent,
        ),
        Err(super::ClientExecutionError::Inspect {
            context,
            source: super::ClientInspectError::Failed("inspect.runtime_unavailable".to_owned()),
        }),
    );
}
#[test]
fn inspect_render_provider_errors_are_whitelisted_and_redacted() {
    assert_eq!(
        super::stable_inspect_provider_error("inspect.denied"),
        "inspect.denied"
    );
    assert_eq!(
        super::stable_inspect_provider_error("inspect.revision_mismatch"),
        "inspect.epoch_mismatch"
    );
    assert_eq!(
        super::stable_inspect_provider_error("inspect.epoch_unavailable"),
        "inspect.stale_epoch"
    );
    assert_eq!(
        super::stable_inspect_provider_error("secret provider detail"),
        "inspect.projection_failed"
    );
    assert_eq!(
        super::stable_inspect_provider_error("inspect.projection_failed\0secret"),
        "inspect.projection_failed"
    );
}

#[test]
fn inspect_request_provenance_rejects_observer_target() {
    let context = super::ClientExecutionContext {
        pair: super::RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x01; 16]),
            orna_core::CatalogueRevisionId::from_bytes([0x02; 16]),
        ),
        function: super::FunctionId::from_bytes([0x03; 16]),
        function_revision: super::FunctionRevisionId::from_bytes([0x04; 16]),
        parent_invocation_id: super::InvocationId::from_bytes([0x05; 16]),
        observer_lineage: None,
    };
    assert!(super::inspect_target_is_observer(
        context,
        context.observer_root_invocation_id(),
    ));
    let request = super::ClientInspectRequest::new(
        context,
        super::ClientInspectOperation::Snapshot {
            target: super::RuntimeValue::Reference {
                target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes([0x06; 16]),
            },
        },
    );
    assert_eq!(
        request.observer_root_invocation_id(),
        context.parent_invocation_id()
    );
    assert_eq!(
        request.observer_parent_invocation_id(),
        context.parent_invocation_id()
    );
    assert_eq!(request.observer_purpose(), "inspect");
    assert_eq!(
        request.target_invocation_id(),
        Some(super::InvocationId::from_bytes([0x06; 16]))
    );
}

#[test]
fn nested_observer_lineage_propagates_root_parent_and_current() {
    let root = super::InvocationId::from_bytes([0x31; 16]);
    let top = super::ObserverLineage::top_level(root);
    assert_eq!(top.root, root);
    assert_eq!(top.parent, root);
    assert_eq!(top.current, root);

    let nested = top.nested();
    assert_eq!(nested.root, root);
    assert_eq!(nested.parent, root);
    assert_ne!(nested.current, root);

    let child = nested.nested();
    assert_eq!(child.root, root);
    assert_eq!(child.parent, nested.current);
    assert_ne!(child.current, nested.current);

    let grandchild = child.nested();
    assert_eq!(grandchild.root, root);
    assert_eq!(grandchild.parent, child.current);
    assert!(grandchild.contains(root));
    assert!(grandchild.contains(nested.current));
    assert!(grandchild.contains(child.current));
    assert!(grandchild.contains(grandchild.current));

    let context = super::ClientExecutionContext {
        pair: super::RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x32; 16]),
            orna_core::CatalogueRevisionId::from_bytes([0x33; 16]),
        ),
        function: super::FunctionId::from_bytes([0x34; 16]),
        function_revision: super::FunctionRevisionId::from_bytes([0x35; 16]),
        parent_invocation_id: nested.current,
        observer_lineage: Some(nested),
    };
    assert_eq!(context.observer_root_invocation_id(), root);
    assert_eq!(context.observer_parent_invocation_id(), nested.current);
    let operation = super::ClientInspectOperation::Snapshot {
        target: super::RuntimeValue::Boolean(true),
    };
    let compatibility_request = super::ClientInspectRequest::new(context, operation.clone());
    assert_eq!(compatibility_request.observer_root_invocation_id(), root);
    assert_eq!(
        compatibility_request.observer_parent_invocation_id(),
        nested.current
    );
    assert_eq!(
        compatibility_request.observer_lineage(),
        &[root, nested.current]
    );
    let external_request =
        super::ClientExternalContractRequest::new(context, "app.test@1", Vec::new());
    assert_eq!(external_request.observer_root_invocation_id(), root);
    assert_eq!(
        external_request.observer_parent_invocation_id(),
        nested.current
    );
    let request =
        super::ClientInspectRequest::with_provenance(context, operation, None, None, nested);
    assert_eq!(request.observer_root_invocation_id(), root);
    assert_eq!(request.observer_parent_invocation_id(), nested.current);
    assert_eq!(request.observer_lineage(), &[root, nested.current]);

    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_external_contract(move |request| {
            assert_eq!(request.observer_root_invocation_id(), root);
            assert_eq!(request.observer_parent_invocation_id(), child.current);
            Ok(RuntimeValue::Text("ui".to_owned()))
        });
    let mut executor_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
    let value = super::evaluate_external_contract(
        super::INSPECT_RENDER_CONTRACT,
        context,
        child,
        &[],
        &mut executor_slot,
    )
    .unwrap();
    assert_eq!(value, RuntimeValue::Text("ui".to_owned()));
}

#[test]
fn inspect_snapshot_validation_rejects_empty_carrier_rows() {
    let (active, _, pair, _) = version_one_active(true);
    let envelope = super::InspectCarrierEnvelope::new(
        super::InspectCarrierKind::Snapshot,
        7,
        pair.source(),
        pair.catalogue(),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        super::inspect_snapshot_target_from_envelope(&active, &envelope),
        Err(super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned(),
        )),
    );
}

#[test]
fn inspect_snapshot_row_binding_preserves_server_root_authority() {
    let target = super::InvocationId::from_bytes([0x17; 16]);
    let root_target = super::FunctionId::from_bytes([0x18; 16]);
    let mut row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
    row.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&root_target.to_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);
    row.push(0);
    assert_eq!(
        super::decode_inspect_snapshot_target_row(&row, 7),
        Ok(target)
    );
    let forged_root = super::FunctionId::from_bytes([0x19; 16]);
    row[41..57].copy_from_slice(&forged_root.to_bytes());
    assert_eq!(
        super::decode_inspect_snapshot_target_row(&row, 7),
        Ok(target)
    );
    assert_eq!(
        super::decode_inspect_snapshot_target_row(&row, 8),
        Err(super::ClientInspectError::Failed(
            "inspect.epoch_mismatch".to_owned()
        ))
    );
    row.extend_from_slice(&[0x19; 16]);
    assert_eq!(
        super::decode_inspect_snapshot_target_row(&row, 7),
        Err(super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned()
        ))
    );
}

#[test]
fn inspect_snapshot_row_rejects_zero_value_batch_count() {
    let target = super::InvocationId::from_bytes([0x17; 16]);
    let mut row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
    row.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&[0x18; 16]);
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);

    assert_eq!(row.len(), 76);
    assert_eq!(
        super::decode_inspect_snapshot_target_row(&row, 7),
        Err(super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned()
        ))
    );
    row[67..75].copy_from_slice(&1_u64.to_be_bytes());
    assert_eq!(
        super::decode_inspect_snapshot_target_row(&row, 7),
        Ok(target)
    );
    row.push(0x19);
    assert_eq!(
        super::decode_inspect_snapshot_target_row(&row, 7),
        Err(super::ClientInspectError::Failed(
            "inspect.malformed_carrier".to_owned()
        ))
    );
}

#[test]
fn inspect_render_wrong_contract_identity_fails_closed_before_provider() {
    let context = super::ClientExecutionContext {
        pair: super::RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x21; 16]),
            orna_core::CatalogueRevisionId::from_bytes([0x22; 16]),
        ),
        function: super::FunctionId::from_bytes([0x23; 16]),
        function_revision: super::FunctionRevisionId::from_bytes([0x24; 16]),
        parent_invocation_id: super::InvocationId::from_bytes([0x25; 16]),
        observer_lineage: None,
    };
    let (active, _, _, _) = version_one_active(true);
    assert!(matches!(
        super::validate_inspect_render_contract(&active, context, "std.inspect.render@2", &[]),
        Err(super::ClientExecutionError::Inspect {
            source: super::ClientInspectError::Failed(code), ..
        }) if code == "inspect.malformed_carrier"
    ));
}

#[test]
fn inspect_render_rejects_mixed_target_before_rendering() {
    let verified = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let active = empty_version_two_active(&verified);
    let pair = active.pair();
    let function = FunctionId::from_bytes([0x90; 16]);
    let function_revision = FunctionRevisionId::from_bytes([0x8f; 16]);
    let target = InvocationId::from_bytes([0x91; 16]);
    let epoch = 7_u64;
    let parameter = ParameterId::from_bytes([0x92; 16]);
    let context = super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0x93; 16]),
        observer_lineage: None,
    };
    let encode_row = |payload: Vec<u8>| {
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("standard catalogue");
        let registry = super::registered_opaque_codecs(standard).expect("opaque registry");
        let descriptor = super::TypeDescriptor::list(super::TypeDescriptor::named(
            super::BINARY_LARGE_OBJECT_TYPE_ID,
        ))
        .expect("row descriptor");
        let value = super::RuntimeValue::list(
            &active,
            descriptor,
            vec![super::RuntimeValue::Bytes(payload)],
        )
        .expect("row value");
        orna_protocol::encode_constructed_value(&active, &registry, &value).expect("encoded row")
    };

    let mut epoch_bytes = [0x96; 16];
    epoch_bytes[8..].copy_from_slice(&epoch.to_be_bytes());
    let mut snapshot_row = vec![1, 0, 0, 0, 0, 0, 0, 0, 0];
    snapshot_row.extend_from_slice(&epoch_bytes);
    snapshot_row.extend_from_slice(&target.to_bytes());
    snapshot_row.extend_from_slice(&[0x94; 16]);
    snapshot_row.push(1);
    snapshot_row.extend_from_slice(&0_u64.to_be_bytes());
    snapshot_row.push(0);
    snapshot_row.push(0);
    let snapshot_bytes = super::InspectCarrierEnvelope::new(
        super::InspectCarrierKind::Snapshot,
        epoch,
        pair.source(),
        pair.catalogue(),
        vec![encode_row(snapshot_row)],
    )
    .expect("snapshot envelope")
    .encode()
    .expect("snapshot bytes");
    let snapshot = super::RuntimeValue::Opaque(
        super::OpaqueValue::new_inspect_carrier(
            &active,
            super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            snapshot_bytes,
        )
        .expect("snapshot carrier"),
    );

    let mut mixed_row = vec![3, 0, 0, 0, 0, 0, 0, 0, 0];
    mixed_row.extend_from_slice(&epoch_bytes);
    mixed_row.extend_from_slice(&[0xaa; 16]);
    mixed_row.extend_from_slice(&[0x94; 16]);
    mixed_row.extend_from_slice(&pair.source().to_bytes());
    mixed_row.extend_from_slice(&pair.catalogue().to_bytes());
    mixed_row.push(1);
    mixed_row.push(0);
    let mixed_bytes = super::InspectCarrierEnvelope::new(
        super::InspectCarrierKind::Calls,
        epoch,
        pair.source(),
        pair.catalogue(),
        vec![encode_row(mixed_row)],
    )
    .expect("mixed projection envelope")
    .encode()
    .expect("mixed projection bytes");
    let mixed = super::RuntimeValue::Opaque(
        super::OpaqueValue::new_inspect_carrier(
            &active,
            super::SYS_INSPECT_CALLS_TYPE_ID,
            mixed_bytes,
        )
        .expect("mixed projection carrier"),
    );

    let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
        operation: orna_artifact::client_plan::InspectOperationNode::Projection {
            projection: InspectProjection::Calls,
            snapshot: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead { parameter },
            ),
        },
    };
    let provider_calls = Rc::new(Cell::new(0_u8));
    let provider_calls_for_executor = Rc::clone(&provider_calls);
    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(super::RuntimeValue::Boolean(false))
        })
        .with_inspect(move |_| {
            provider_calls_for_executor.set(provider_calls_for_executor.get() + 1);
            Ok(mixed.clone())
        });
    let mut executor_slot: Option<&mut dyn super::ClientResourceExecutor> = Some(&mut executor);
    let mut state = super::ClientStateStore::new();
    let mut locals = std::collections::HashMap::new();
    let arguments = [(parameter, snapshot)];

    let result = super::evaluate_expression_plan(
        &active,
        &expression,
        context,
        super::ObserverLineage::compatibility(context),
        ResolvedType::Value(super::SYS_INSPECT_CALLS_TYPE_ID),
        &arguments,
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x95; 16]),
        &mut executor_slot,
        &mut locals,
    );
    assert!(matches!(
        result,
        Err(super::ClientExecutionError::Inspect {
            source: super::ClientInspectError::Failed(code),
            ..
        }) if code == "inspect.epoch_mismatch"
    ));
    assert_eq!(
        provider_calls.get(),
        1,
        "the mixed carrier came from the custom provider"
    );
}

#[test]
fn inspect_render_wrong_ui_type_fails_closed() {
    let (active, _, _, _) = version_one_active(true);
    assert!(!super::inspect_render_ui_value_matches(
        &active,
        &super::RuntimeValue::Boolean(false),
    ));
}

#[test]
fn inspector_invocation_references_require_sealed_type_and_nonzero_object() {
    let (active, _, _, _) = version_one_active(true);
    let expected = ResolvedType::Reference {
        target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
    };
    assert!(super::runtime_value_matches(
        &active,
        &RuntimeValue::Reference {
            target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
            object: orna_core::ObjectId::from_bytes([0x11; 16]),
        },
        expected,
    ));
    assert!(!super::runtime_value_matches(
        &active,
        &RuntimeValue::Reference {
            target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
            object: orna_core::ObjectId::from_bytes([0; 16]),
        },
        expected,
    ));
    assert!(!super::runtime_value_matches(
        &active,
        &RuntimeValue::Reference {
            target: TypeId::from_bytes([0x12; 16]),
            object: orna_core::ObjectId::from_bytes([0x11; 16]),
        },
        expected,
    ));
}

#[test]
fn inspector_procedural_local_types_preserve_reference_and_carrier_shapes() {
    let (active, _, _, _) = version_one_active(true);
    assert_eq!(
        super::resolve_client_local_type(&active, super::SYS_INSPECT_INVOCATION_TYPE_ID),
        Some(ResolvedType::Reference {
            target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
        }),
    );
    assert_eq!(
        super::resolve_client_local_type(&active, super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
        Some(ResolvedType::Value(super::SYS_INSPECT_SNAPSHOT_TYPE_ID)),
    );
}

#[test]
fn inspector_carriers_reject_malformed_and_stale_revision_envelopes() {
    let (active, _, pair, _) = version_one_active(true);
    let payload = super::InspectCarrierEnvelope::new(
        super::InspectCarrierKind::Snapshot,
        7,
        pair.source(),
        pair.catalogue(),
        vec![],
    )
    .unwrap()
    .encode()
    .unwrap();
    let value = RuntimeValue::Opaque(
        OpaqueValue::new_inspect_carrier(
            &active,
            super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
            payload.clone(),
        )
        .unwrap(),
    );
    assert!(super::runtime_value_matches(
        &active,
        &value,
        ResolvedType::Named(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
    ));
    assert!(!super::runtime_value_matches(
        &active,
        &RuntimeValue::Bytes(vec![0; 4]),
        ResolvedType::Named(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
    ));

    let stale_payload = super::InspectCarrierEnvelope::new(
        super::InspectCarrierKind::Snapshot,
        7,
        SourceRevisionId::from_bytes([0x91; 16]),
        pair.catalogue(),
        vec![],
    )
    .unwrap()
    .encode()
    .unwrap();
    assert_eq!(
        super::decode_inspect_carrier_payload(
            &active,
            &stale_payload,
            super::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        )
        .unwrap_err(),
        super::ClientInspectError::Failed("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_request_exposes_distinct_client_epoch_anchor() {
    let context = super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0xa1; 16]),
            CatalogueRevisionId::from_bytes([0xa2; 16]),
        ),
        function: FunctionId::from_bytes([0xa3; 16]),
        function_revision: FunctionRevisionId::from_bytes([0xa4; 16]),
        parent_invocation_id: InvocationId::from_bytes([0xa5; 16]),
        observer_lineage: None,
    };
    let request = super::ClientInspectRequest::new(
        context,
        super::ClientInspectOperation::Snapshot {
            target: RuntimeValue::Reference {
                target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes([0xa6; 16]),
            },
        },
    );
    assert_eq!(request.client_epoch_id(), context.client_epoch_id());
    assert_eq!(
        request.client_epoch_id().invocation_id(),
        context.parent_invocation_id()
    );
}

#[test]
fn inspect_executor_forwards_typed_request_to_provider() {
    let context = super::ClientExecutionContext {
        pair: RevisionPair::new(
            SourceRevisionId::from_bytes([0x61; 16]),
            CatalogueRevisionId::from_bytes([0x62; 16]),
        ),
        function: FunctionId::from_bytes([0x63; 16]),
        function_revision: FunctionRevisionId::from_bytes([0x64; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x65; 16]),
        observer_lineage: None,
    };
    let operation = super::ClientInspectOperation::Projection {
        projection: InspectProjection::Calls,
        snapshot: RuntimeValue::Boolean(true),
    };
    let request = super::ClientInspectRequest::new(context, operation);
    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok::<_, String>(RuntimeValue::Boolean(false))
        })
        .with_inspect(move |request| {
            assert_eq!(request.context(), context);
            assert_eq!(
                request.operation().projection(),
                Some(InspectProjection::Calls)
            );
            assert_eq!(request.operation().projection_carrier_tag(), Some(3));
            assert!(matches!(
                request.operation().snapshot(),
                Some(RuntimeValue::Boolean(true))
            ));
            Ok(RuntimeValue::Boolean(false))
        });
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };
    assert_eq!(
        nested.inspect(request),
        Ok(RuntimeValue::Boolean(false)),
        "nested CLIENT actions must retain Inspector providers",
    );
}

#[test]
fn inspect_expression_rejects_observer_lineage_targets_before_provider() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let root = InvocationId::from_bytes([0x91; 16]);
    let parent = InvocationId::from_bytes([0x92; 16]);
    let current = InvocationId::from_bytes([0x93; 16]);
    let explicit_lineage =
        super::ObserverLineage::top_level(root).with_parent_and_current(parent, current);
    let top_level = super::ObserverLineage::top_level(root);
    let nested = top_level.nested();
    let child = nested.nested();
    let cases = [
        ("root", explicit_lineage, root),
        ("parent", explicit_lineage, parent),
        ("current", explicit_lineage, current),
        ("recorded nested descendant", child, nested.current),
    ];

    for (label, lineage, target) in cases {
        let context = super::ClientExecutionContext {
            pair,
            function,
            function_revision,
            parent_invocation_id: lineage.parent,
            observer_lineage: None,
        };
        let parameter = ParameterId::from_bytes([0x94; 16]);
        let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
            operation: orna_artifact::client_plan::InspectOperationNode::snapshot(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead { parameter },
            ),
        };
        let arguments = [(
            parameter,
            RuntimeValue::Reference {
                target: super::SYS_INSPECT_INVOCATION_TYPE_ID,
                object: orna_core::ObjectId::from_bytes(target.to_bytes()),
            },
        )];
        let grants = capability::LocalCapabilityGrantSet::new();
        let mut state = ClientStateStore::new();
        let provider_calls = Rc::new(Cell::new(0));
        let provider_calls_for_executor = Rc::clone(&provider_calls);
        let mut executor =
            super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
                Ok::<_, String>(RuntimeValue::Boolean(false))
            })
            .with_inspect(move |_| {
                provider_calls_for_executor.set(provider_calls_for_executor.get() + 1);
                Ok(RuntimeValue::Boolean(false))
            });
        let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
        let mut locals = std::collections::HashMap::new();

        let result = super::evaluate_expression_plan(
            &active,
            &expression,
            context,
            lineage,
            ResolvedType::Value(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
            &arguments,
            &[],
            &grants,
            &mut state,
            0,
            PrincipalId::from_bytes([0x95; 16]),
            &mut executor_slot,
            &mut locals,
        );

        assert_eq!(
            result,
            Err(super::ClientExecutionError::Inspect {
                context,
                source: super::ClientInspectError::Failed("inspect.recursion".to_owned()),
            }),
            "{label} target must be rejected by the expression evaluator",
        );
        assert_eq!(
            provider_calls.get(),
            0,
            "{label} target must not invoke the Inspector provider",
        );
    }
}

#[test]
fn inspect_snapshot_options_reject_before_evaluating_target_or_options() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let context = super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: super::InvocationId::from_bytes([0xa7; 16]),
        observer_lineage: None,
    };
    let target = super::ParameterId::from_bytes([0xa8; 16]);
    let options = super::ParameterId::from_bytes([0xa9; 16]);
    let expression = orna_artifact::client_plan::ClientExpressionNode::Inspect {
        operation: orna_artifact::client_plan::InspectOperationNode::Snapshot {
            target: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: target,
                },
            ),
            options: Some(Box::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: options,
                },
            )),
        },
    };
    let mut state = super::ClientStateStore::new();
    let mut locals = std::collections::HashMap::new();
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = None;

    let result = super::evaluate_expression_plan(
        &active,
        &expression,
        context,
        super::ObserverLineage::compatibility(context),
        ResolvedType::Value(super::SYS_INSPECT_SNAPSHOT_TYPE_ID),
        &[],
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0xaa; 16]),
        &mut executor_slot,
        &mut locals,
    );

    assert_eq!(
        result,
        Err(super::ClientExecutionError::Inspect {
            context,
            source: super::ClientInspectError::Failed("inspect.projection_failed".to_owned()),
        }),
        "unsupported snapshot options must be rejected before either expression is evaluated",
    );
}

fn reference_field_path_fixture() -> (
    ActiveDatabaseRevision,
    ClientExecutionContext,
    ParameterId,
    TypeId,
    TypeId,
    RecordValue,
    TypeId,
    RecordValue,
    FieldId,
    FieldId,
    AuthorisedInvocation,
) {
    let (base, function, pair, function_revision) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::Function(orna_standard::STD_INVOKE_ECHO_FUNCTION_ID),
        DefinitionReferenceKind::FunctionCall,
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        )
        .encode()
        .unwrap(),
    );
    let outer_object = TypeId::from_bytes([0x11; 16]);
    let inner_object = TypeId::from_bytes([0x12; 16]);
    let outer_record = TypeId::from_bytes([0x13; 16]);
    let inner_record = TypeId::from_bytes([0x14; 16]);
    let outer_field = FieldId::from_bytes([0x21; 16]);
    let inner_field = FieldId::from_bytes([0x22; 16]);
    let objects = vec![
        ObjectTypeDefinition::new(
            outer_object,
            QualifiedSemanticName::new(["app", "item"]).unwrap(),
            vec![FieldDefinition::new(
                outer_field,
                "child",
                0,
                ResolvedType::reference(inner_object),
                false,
                false,
                None,
                None,
            )],
        ),
        ObjectTypeDefinition::new(
            inner_object,
            QualifiedSemanticName::new(["app", "person"]).unwrap(),
            vec![FieldDefinition::new(
                inner_field,
                "name",
                0,
                ResolvedType::named(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
                false,
                false,
                None,
                None,
            )],
        ),
    ];
    let records = vec![
        RecordValueTypeDefinition::new(
            outer_record,
            QualifiedSemanticName::new(["app", "item_row"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    outer_field,
                    "child",
                    0,
                    TypeDescriptor::named(inner_record),
                )
                .unwrap(),
            ],
        ),
        RecordValueTypeDefinition::new(
            inner_record,
            QualifiedSemanticName::new(["app", "person_row"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    inner_field,
                    "name",
                    0,
                    TypeDescriptor::named(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
                )
                .unwrap(),
            ],
        ),
    ];
    let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        objects,
        base.catalogue().value_types().to_vec(),
        base.catalogue().enum_types().to_vec(),
        records,
        base.catalogue().type_bindings().to_vec(),
        base.catalogue().functions().to_vec(),
    )
    .unwrap();
    let hash_context = base.catalogue_hash_context().clone();
    let origin = base.function_revisions()[0].declaration_origin();
    let mut origins = base.origins().to_vec();
    origins.extend([
        DefinitionOrigin::new(DefinitionIdentity::ObjectType(outer_object), origin),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: outer_object,
                field: outer_field,
            },
            origin,
        ),
        DefinitionOrigin::new(DefinitionIdentity::ObjectType(inner_object), origin),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: inner_object,
                field: inner_field,
            },
            origin,
        ),
        DefinitionOrigin::new(DefinitionIdentity::ValueType(outer_record), origin),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: outer_record,
                field: outer_field,
            },
            origin,
        ),
        DefinitionOrigin::new(DefinitionIdentity::ValueType(inner_record), origin),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: inner_record,
                field: inner_field,
            },
            origin,
        ),
    ]);
    let catalogue_hash = catalogue_digest_with_context(
        &hash_context,
        &catalogue,
        base.function_revisions(),
        base.expressions(),
        origins.as_slice(),
        base.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            base.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                base.expressions().to_vec(),
                base.function_revisions().to_vec(),
                origins,
                base.references().to_vec(),
            ),
        ),
        hash_context,
    )
    .unwrap();
    let inner = RecordValue::new(
        &active,
        inner_record,
        [(String::from("name"), RuntimeValue::Text("Ada".to_owned()))],
    )
    .unwrap();
    let outer = RecordValue::new(
        &active,
        outer_record,
        [(String::from("child"), RuntimeValue::Record(inner.clone()))],
    )
    .unwrap();
    let authorisation = authorise(pair, function);
    let context = ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0x40; 16]),
        observer_lineage: None,
    };
    (
        active,
        context,
        ParameterId::from_bytes([0x30; 16]),
        outer_object,
        outer_object,
        outer,
        inner_object,
        inner,
        outer_field,
        inner_field,
        authorisation,
    )
}

fn authorise(pair: RevisionPair, function: FunctionId) -> AuthorisedInvocation {
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let snapshot = SecuritySnapshot::new(
        pair,
        vec![function],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(principal, function)],
    )
    .expect("test security snapshot should validate");
    let session = snapshot
        .bind_authenticated_session(principal, vec![])
        .expect("test security session should bind");
    let ExecuteDecision::Allowed(authorisation) =
        snapshot.authorise_execute(&session, InvocationTarget::new(function, pair))
    else {
        panic!("test security grant should allow the function");
    };
    authorisation
}

fn authorise_with_role_context(
    pair: RevisionPair,
    function: FunctionId,
) -> (AuthorisedInvocation, AuthorisedInvocation) {
    let session_principal = PrincipalId::from_bytes([0x7a; 16]);
    let role = PrincipalId::from_bytes([0x7b; 16]);
    let principals = vec![
        Principal::new(
            session_principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        ),
        Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
    ];
    let direct_snapshot = SecuritySnapshot::new(
        pair,
        vec![function],
        principals.clone(),
        vec![RoleMembership::new(role, session_principal)],
        vec![ExecuteGrant::new(session_principal, function)],
    )
    .expect("direct security snapshot should validate");
    let role_snapshot = SecuritySnapshot::new(
        pair,
        vec![function],
        principals,
        vec![RoleMembership::new(role, session_principal)],
        vec![ExecuteGrant::new(role, function)],
    )
    .expect("role security snapshot should validate");
    let direct_session = direct_snapshot
        .bind_authenticated_session(session_principal, vec![])
        .expect("direct session should bind");
    let role_session = role_snapshot
        .bind_authenticated_session(session_principal, vec![role])
        .expect("role session should bind");
    let ExecuteDecision::Allowed(direct) =
        direct_snapshot.authorise_execute(&direct_session, InvocationTarget::new(function, pair))
    else {
        panic!("direct grant should allow the function");
    };
    let ExecuteDecision::Allowed(role_authorisation) =
        role_snapshot.authorise_execute(&role_session, InvocationTarget::new(function, pair))
    else {
        panic!("role grant should allow the function");
    };
    (direct, role_authorisation)
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_client_function(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<super::ClientExecutionResult, super::ClientExecutionError> {
    super::evaluate_client_function(active, &authorise(active.pair(), function))
}

#[test]
fn vm_admission_resolves_and_decodes_an_authorised_client_revision() {
    let (active, function, pair, _) = version_one_active(true);
    let authorisation = authorise(pair, function);
    let limits = super::vm::ClientVmArtifactLimits::new(1024, 64, 1024).expect("valid VM limits");
    let runtime_offer = super::vm::RuntimeOfferWitness::from_parts(
        1,
        0,
        "orna-runtime-test",
        "0.1.0",
        "test-build",
        "linux-x86_64",
        3,
        1,
        &[],
        &[],
    )
    .expect("valid runtime offer");
    let registry = super::vm::ClientVmInvocationRegistry::new();
    let mut host = super::vm::ClientVmHostContext::new(&registry, runtime_offer, limits)
        .expect("valid VM host");

    let admission =
        super::vm::admit_client_function(&active, &authorisation, &mut host, limits, &[], &[])
            .expect("authorised client revision should be admitted");

    assert!(matches!(
        admission.plan(),
        super::vm::ClientVmDecodedPlan::Boolean(_)
    ));
    assert_eq!(admission.identity().function(), function.to_bytes());
    assert_eq!(
        admission.identity().function_revision(),
        active.function_revisions()[0].id().to_bytes()
    );
    assert_eq!(
        admission.host().security_context_digest(),
        authorisation.security_context_digest().to_bytes()
    );
    assert!(host.admission_is_current(&admission));
    host.advance_policy_epoch().expect("policy epoch");
    assert!(!host.admission_is_current(&admission));
}

#[test]
fn vm_admission_binds_full_capability_arguments_and_rejects_missing_parameters() {
    let capability_payload = |argument| {
        orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Boolean(
                orna_artifact::client_plan::ClientPlan::return_boolean(true),
            ),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                argument,
            )],
        )
        .encode()
        .expect("capability payload")
    };
    let (active, function, pair, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        capability_payload(orna_artifact::client_plan::CapabilityArgumentSource::Text(
            "scope".to_owned(),
        )),
    );
    let authorisation = authorise(pair, function);
    let limits = super::vm::ClientVmArtifactLimits::new(1024, 64, 1024).expect("valid VM limits");
    let runtime_offer = || {
        super::vm::RuntimeOfferWitness::from_parts(
            1,
            0,
            "orna-runtime-test",
            "0.1.0",
            "test-build",
            "linux-x86_64",
            3,
            1,
            &[],
            &[],
        )
        .expect("valid runtime offer")
    };
    let registry = super::vm::ClientVmInvocationRegistry::new();
    let mut host = super::vm::ClientVmHostContext::new(&registry, runtime_offer(), limits)
        .expect("valid VM host");
    let declarations = [super::vm::ClientVmCapabilityDeclaration::new(
        "std.fs.read",
        super::vm::ClientVmCapabilityArgument::Text("scope".to_owned()),
    )];
    let admission = super::vm::admit_client_function(
        &active,
        &authorisation,
        &mut host,
        limits,
        &declarations,
        &[],
    )
    .expect("text capability argument should be admitted");
    assert!(matches!(
        admission.plan(),
        super::vm::ClientVmDecodedPlan::Capability(_)
    ));

    let (missing_active, _, missing_pair, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        capability_payload(
            orna_artifact::client_plan::CapabilityArgumentSource::Parameter("missing".to_owned()),
        ),
    );
    let missing_authorisation = authorise(missing_pair, function);
    let mut missing_host = super::vm::ClientVmHostContext::new(&registry, runtime_offer(), limits)
        .expect("valid second VM host");
    let missing_declarations = [super::vm::ClientVmCapabilityDeclaration::new(
        "std.fs.read",
        super::vm::ClientVmCapabilityArgument::Parameter("missing".to_owned()),
    )];
    assert!(matches!(
        super::vm::admit_client_function(
            &missing_active,
            &missing_authorisation,
            &mut missing_host,
            limits,
            &missing_declarations,
            &[],
        ),
        Err(super::vm::ClientVmAdmissionError::SemanticRejected)
    ));
}

#[test]
fn evaluates_version_one_client_constants() {
    for value in [true, false] {
        let (active, function, pair, function_revision) = version_one_active(value);

        let result = evaluate_client_function(&active, function).unwrap();

        assert_eq!(result.context().pair(), pair);
        assert_eq!(result.context().function(), function);
        assert_eq!(result.context().function_revision(), function_revision);
        assert_eq!(result.value(), &RuntimeValue::Boolean(value));
    }
}

#[test]
fn resource_request_rejects_nul_invocation_context_before_loading() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7b; 16]);
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    for (profile, instance) in [
        ("profile\0invalid", "instance"),
        ("profile", "instance\0invalid"),
    ] {
        let context = super::ClientResourceInvocationContext::new(
            InvocationId::from_bytes([0x24; 16]),
            CallSiteId::from_bytes([0x25; 16]),
            profile.to_owned(),
            instance.to_owned(),
        );
        assert!(matches!(
            resource.begin_request_with_context(&active, context, Vec::new()),
            Err(super::ClientResourceError::InvalidInvocationContext)
        ));
        assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
        assert_eq!(resource.generation().value(), 0);
    }
}

#[test]
fn resource_request_rejects_zero_lineage_before_loading() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7c; 16]);
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x24; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    for (parent_invocation_id, call_site_id) in [
        (
            InvocationId::from_bytes([0; 16]),
            CallSiteId::from_bytes([0x25; 16]),
        ),
        (
            InvocationId::from_bytes([0x24; 16]),
            CallSiteId::from_bytes([0; 16]),
        ),
    ] {
        let context = super::ClientResourceInvocationContext::new(
            parent_invocation_id,
            call_site_id,
            "profile".to_owned(),
            "instance".to_owned(),
        );
        assert_eq!(
            resource.begin_request_with_context(&active, context, Vec::new()),
            Err(super::ClientResourceError::InvalidInvocationContext),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
        assert_eq!(resource.generation().value(), 0);
        assert_eq!(resource.request_id(), None);
    }

    let context = super::ClientResourceInvocationContext::new(
        InvocationId::from_bytes([0x24; 16]),
        CallSiteId::from_bytes([0x25; 16]),
        "profile".to_owned(),
        "instance".to_owned(),
    );
    let request = resource
        .begin_request_with_context(&active, context.clone(), Vec::new())
        .unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.generation().value(), 1);
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(request.invocation_context(), Some(context));
}

#[test]
fn client_resource_lifecycle_rejects_stale_and_invalid_results() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        Sha256Digest::from_bytes([0x11; 32]),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 0);

    let first = resource.begin_loading().unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(first.value(), 1);
    assert_eq!(
        resource.publish_ready(
            &active,
            super::ClientResourceGeneration(0),
            RuntimeValue::Boolean(true),
        ),
        Err(super::ClientResourceError::StaleGeneration {
            expected: first,
            actual: super::ClientResourceGeneration(0),
        }),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);

    resource
        .publish_ready(&active, first, RuntimeValue::Boolean(true))
        .unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    let second = resource.begin_loading().unwrap();
    assert_eq!(resource.value(), None);
    assert_eq!(
        resource.publish_failure(second, String::new()),
        Err(super::ClientResourceError::InvalidFailureCode),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    resource
        .publish_failure(second, "network.timeout".to_owned())
        .unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Failed);
    assert_eq!(
        resource.failure().map(super::ClientResourceFailure::code),
        Some("network.timeout"),
    );

    let third = resource.begin_loading().unwrap();
    resource.cancel(third).unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
    assert_eq!(
        resource.publish_failure(third, "late".to_owned()),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Cancelled,
        }),
    );

    resource.invalidate().unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 4);
}

#[test]
fn client_action_argument_error_preserves_display_and_equality() {
    let resource_error = super::ClientResourceError::DuplicateArgument {
        parameter: ParameterId::from_bytes([0x7b; 16]),
    };
    let action_error = super::ClientActionError::Arguments(Box::new(resource_error.clone()));

    assert_eq!(action_error.to_string(), resource_error.to_string());
    assert_eq!(
        action_error,
        super::ClientActionError::Arguments(Box::new(resource_error)),
    );
}

#[test]
fn client_resource_rejects_completion_with_mismatched_request_key() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x11; 32]),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let wrong_key = super::ClientResourceKey::new(
        key.target(),
        key.principal(),
        Sha256Digest::from_bytes([0xaa; 32]),
        key.invalidation_token(),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();
    let request_id = resource.request_id().unwrap();
    let completion = super::ClientResourceCompletion::Ready {
        request_id,
        key: wrong_key,
        generation,
        value: RuntimeValue::Boolean(true),
    };
    let before = resource.clone();

    let error = resource
        .apply_completion(&active, completion)
        .expect_err("the completion key must be rejected");
    assert_eq!(
        error,
        super::ClientResourceError::RequestKeyMismatch {
            expected: Box::new(key),
            actual: Box::new(wrong_key),
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "CLIENT resource completion uses key {:?}, expected {:?}",
            wrong_key, key,
        ),
    );
    assert_eq!(resource, before);
}

#[test]
fn client_resource_rejects_completion_with_mismatched_request_id() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();
    let completion = super::ClientResourceCompletion::Ready {
        request_id: InvocationId::from_bytes([0xff; 16]),
        key,
        generation: request.generation(),
        value: RuntimeValue::Boolean(true),
    };
    let before = resource.clone();

    assert_eq!(
        resource.apply_completion(&active, completion),
        Err(super::ClientResourceError::RequestIdMismatch {
            expected: request.request_id(),
            actual: InvocationId::from_bytes([0xff; 16]),
        })
    );
    assert_eq!(resource, before);
}

#[test]
fn client_resource_ready_value_must_match_declared_type() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x31; 32]),
        Sha256Digest::from_bytes([0x32; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Integer(4)),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_rejects_expected_type_that_differs_from_target_declaration() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x33; 32]),
        Sha256Digest::from_bytes([0x34; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Integer));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Integer(7)),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_rejects_completion_from_a_different_revision() {
    let (active, function, _, _) = version_one_active(true);
    let resource_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x7b; 16]),
        CatalogueRevisionId::from_bytes([0x7c; 16]),
    );
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, resource_pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x41; 32]),
        Sha256Digest::from_bytes([0x42; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Boolean(true)),
        Err(super::ClientResourceError::RevisionMismatch {
            expected: resource_pair,
            actual: active.pair(),
        }),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_rejects_terminal_completion_after_active_revision_changes() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x7d; 16]),
        CatalogueRevisionId::from_bytes([0x7e; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let arguments_digest =
        super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        arguments_digest,
        Sha256Digest::from_bytes([0x52; 32]),
    );

    let mut pending_resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let pending_request = pending_resource.begin_request(&active, Vec::new()).unwrap();
    let before_pending = pending_resource.clone();
    assert_eq!(
        pending_resource.apply_completion(&changed_active, pending_request.pending()),
        Err(super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert_eq!(pending_resource, before_pending);

    let mut failed_resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let failed_request = failed_resource.begin_request(&active, Vec::new()).unwrap();
    let before_failed = failed_resource.clone();
    assert_eq!(
        failed_resource
            .apply_completion(&changed_active, failed_request.failed("stale".to_owned()),),
        Err(super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert_eq!(failed_resource, before_failed);

    let mut cancelled_resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let cancelled_request = cancelled_resource
        .begin_request(&active, Vec::new())
        .unwrap();
    let before_cancelled = cancelled_resource.clone();
    assert_eq!(
        cancelled_resource.apply_completion(&changed_active, cancelled_request.cancelled()),
        Err(super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert_eq!(cancelled_resource, before_cancelled);
}

#[test]
fn client_resource_executor_validates_arguments_and_applies_completion() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        vec![
            ParameterDefinition::new(
                ParameterId::from_bytes([0x02; 16]),
                "count",
                0,
                ResolvedType::Scalar(StandardScalar::Integer),
                None,
            ),
            ParameterDefinition::new(
                ParameterId::from_bytes([0x01; 16]),
                "enabled",
                1,
                ResolvedType::Scalar(StandardScalar::Boolean),
                None,
            ),
        ],
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let first = FunctionArgument::new(
        ParameterId::from_bytes([0x02; 16]),
        RuntimeValue::Integer(7),
    )
    .unwrap();
    let second = FunctionArgument::new(
        ParameterId::from_bytes([0x01; 16]),
        RuntimeValue::Boolean(true),
    )
    .unwrap();
    let arguments = vec![first.clone(), second.clone()];
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &arguments).unwrap();
    assert_eq!(
        digest,
        super::ClientResourceKey::canonical_arguments_digest(
            &active,
            &[second.clone(), first.clone()],
        )
        .unwrap(),
    );
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let mut executor = super::DeterministicClientResourceExecutor::new(
        |request: &super::ClientResourceRequest| {
            assert_eq!(request.arguments()[0].parameter(), second.parameter());
            assert_eq!(request.arguments()[1].parameter(), first.parameter());
            Ok(RuntimeValue::Boolean(true))
        },
    );

    let request = resource
        .begin_request(&active, vec![first.clone(), second.clone()])
        .unwrap();
    assert_eq!(request.arguments()[0].parameter(), second.parameter());
    let first_request_id = request.request_id();
    let completion = super::ClientResourceExecutor::execute(&mut executor, request);
    resource.apply_completion(&active, completion).unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    let second_request = resource
        .begin_request(&active, vec![first, second])
        .unwrap();
    assert_ne!(second_request.request_id(), first_request_id);
    let failed = second_request.failed("resource.denied".to_owned());
    resource.apply_completion(&active, failed).unwrap();
    assert_eq!(resource.status(), super::ClientResourceStatus::Failed);
    assert_eq!(
        resource.failure().map(super::ClientResourceFailure::code),
        Some("resource.denied"),
    );
}

#[test]
fn client_resource_rejects_over_limit_arguments_before_cloning_or_hashing() {
    let (active, function, pair, _) = version_one_active(true);
    let arguments = (0..=super::MAX_RESOURCE_ARGUMENTS)
        .map(|index| {
            let mut bytes = [0_u8; 16];
            bytes[14..].copy_from_slice(&(index as u16).to_be_bytes());
            FunctionArgument::new(
                ParameterId::from_bytes(bytes),
                RuntimeValue::Boolean(index % 2 == 0),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let expected_error = super::ClientResourceError::ResourceArgumentLimitExceeded {
        limit: super::MAX_RESOURCE_ARGUMENTS,
    };

    assert_eq!(
        super::ClientResourceKey::canonical_arguments_digest(&active, &arguments),
        Err(expected_error.clone()),
    );

    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x61; 32]),
        Sha256Digest::from_bytes([0x62; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    assert_eq!(
        resource.begin_request(&active, arguments),
        Err(expected_error)
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 0);
}

#[test]
fn client_resource_pending_completion_preserves_loading_until_resume() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();
    let generation = request.generation();
    let request_id = request.request_id();

    resource
        .apply_completion(&active, request.pending())
        .expect("pending completion should retain the active generation");
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);

    resource
        .apply_completion(
            &active,
            super::ClientResourceCompletion::Ready {
                request_id,
                key,
                generation,
                value: RuntimeValue::Boolean(true),
            },
        )
        .expect("the matching completion should resume the resource");
    assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));
}

#[test]
fn resource_executor_poll_surfaces_pending_completion_without_affecting_immediate_executor() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );

    let mut pending_resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let pending_request = pending_resource.begin_request(&active, vec![]).unwrap();
    let pending_request_id = pending_request.request_id();
    let expected_pending = pending_request.clone().pending();
    let mut polling = PollingTestExecutor::default();
    assert_eq!(polling.execute(pending_request), expected_pending);
    assert_eq!(
        polling.poll(),
        Some(ClientResourceCompletion::Ready {
            request_id: pending_request_id,
            key,
            generation: pending_resource.generation(),
            value: RuntimeValue::Boolean(true)
        })
    );
    assert_eq!(polling.poll(), None);

    let mut immediate_resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let immediate_request = immediate_resource.begin_request(&active, vec![]).unwrap();
    let mut immediate = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(true))
    });
    assert!(matches!(
        immediate.execute(immediate_request),
        ClientResourceCompletion::Ready { .. }
    ));
    assert_eq!(immediate.poll(), None);
}
#[test]
fn default_executor_cancel_keeps_pending_ownership() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut executor = PollingTestExecutor {
        pending: Some(request.clone()),
    };

    assert_eq!(executor.cancel(request.clone()), request.clone().pending());
    assert_eq!(executor.pending, Some(request));
}

#[test]
fn client_resource_cancelled_completion_terminates_current_generation() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();

    resource
        .apply_completion(&active, request.cancelled())
        .expect("matching cancellation should terminate the active generation");

    assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
}

#[test]
fn client_stream_request_preserves_batch_order_and_returns_terminal_option() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    assert_eq!(request.kind(), ResourceKind::Stream);

    resource
        .apply_completion(
            &active,
            request.clone().stream_values(vec![
                RuntimeValue::Boolean(true),
                RuntimeValue::Boolean(false),
            ]),
        )
        .unwrap();
    resource
        .apply_completion(
            &active,
            request.clone().stream_values(vec![
                RuntimeValue::Boolean(false),
                RuntimeValue::Boolean(true),
            ]),
        )
        .unwrap();
    resource
        .apply_completion(&active, request.stream_completed())
        .unwrap();

    let first = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_batch(first, &[true, false]);
    let second = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_batch(second, &[false, true]);
    let terminal = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_terminal(terminal);
}

#[test]
fn client_record_stream_batches_append_and_take_nominal_type_ids() {
    let (active, function, pair, _, record_type, other_record_type) =
        version_two_server_record_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x26; 32]),
    );
    let mut resource = ClientResource::new_stream(key, ResolvedType::Named(record_type));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let record = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record_type,
            [(String::from("title"), RuntimeValue::Boolean(true))],
        )
        .unwrap(),
    );
    let mismatched = RuntimeValue::Record(
        RecordValue::new(
            &active,
            other_record_type,
            [(String::from("title"), RuntimeValue::Boolean(false))],
        )
        .unwrap(),
    );

    resource
        .apply_completion(&active, request.clone().stream_values(vec![record.clone()]))
        .unwrap();
    let before_mismatch = resource.clone();
    assert_eq!(
        resource.apply_completion(&active, request.clone().stream_values(vec![mismatched])),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource, before_mismatch);

    resource
        .apply_completion(&active, request.stream_completed())
        .unwrap();
    let batch = resource.take_stream_value(&active).unwrap().unwrap();
    let RuntimeValue::Constructed(option) = batch else {
        panic!("record stream batch must be a constructed OPTION");
    };
    let ConstructedValueKind::Option(Some(list)) = option.kind() else {
        panic!("record stream batch must contain a present LIST");
    };
    let RuntimeValue::Constructed(list) = list else {
        panic!("record stream OPTION must contain a constructed LIST");
    };
    let ConstructedValueKind::List(values) = list.kind() else {
        panic!("record stream OPTION must contain a LIST");
    };
    assert_eq!(values, [record].as_slice());

    let terminal = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_terminal(terminal);
}

#[test]
fn client_stream_rejects_scalar_ready_completion() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();

    assert_eq!(
        resource.publish_ready(&active, request.generation(), RuntimeValue::Boolean(true)),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
    assert!(!resource.stream_complete());
}

#[test]
fn client_stream_rejects_oversized_batches_and_totals_before_queueing() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();

    let oversized_batch = vec![RuntimeValue::Boolean(true); super::MAX_RESOURCE_BATCH_ITEMS + 1];
    assert_eq!(
        resource.apply_completion(&active, request.clone().stream_values(oversized_batch),),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert!(resource.stream_batches.is_empty());
    assert_eq!(resource.stream_total_items, 0);

    resource.stream_total_items = super::MAX_RESOURCE_TOTAL_ITEMS;
    assert_eq!(
        resource.apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(true)]),
        ),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert!(resource.stream_batches.is_empty());
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
}

#[test]
fn client_stream_queue_overflow_preserves_existing_batches() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x24; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let batch = vec![RuntimeValue::Boolean(true); super::MAX_RESOURCE_BATCH_ITEMS];
    resource
        .apply_completion(&active, request.clone().stream_values(batch))
        .unwrap();
    let before = resource.clone();

    assert_eq!(
        resource.apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(false)]),
        ),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource, before);
}

#[test]
fn client_stream_queue_dequeue_releases_capacity() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x25; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    for _ in 0..super::MAX_RESOURCE_BATCH_ITEMS {
        resource
            .apply_completion(
                &active,
                request
                    .clone()
                    .stream_values(vec![RuntimeValue::Boolean(true)]),
            )
            .unwrap();
    }
    assert_eq!(
        resource.stream_queued_items,
        super::MAX_RESOURCE_QUEUED_ITEMS
    );
    resource.take_stream_value(&active).unwrap().unwrap();
    assert_eq!(
        resource.stream_queued_items,
        super::MAX_RESOURCE_QUEUED_ITEMS - 1
    );

    resource
        .apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(false)]),
        )
        .unwrap();
    assert_eq!(
        resource.stream_queued_items,
        super::MAX_RESOURCE_QUEUED_ITEMS
    );
}

#[test]
fn client_stream_failure_drains_queued_batches_before_evaluator_reports_failure() {
    let (active, function, pair, function_revision) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(true)]),
        )
        .unwrap();
    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(false)]),
        )
        .unwrap();
    resource
        .apply_completion(&active, request.failed("stream.failed".to_owned()))
        .unwrap();

    let context = ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0xf6; 16]),
        observer_lineage: None,
    };
    let first = super::read_stream_resource_value(&active, &mut resource, context).unwrap();
    assert_boolean_stream_batch(first, &[true]);
    let second = super::read_stream_resource_value(&active, &mut resource, context).unwrap();
    assert_boolean_stream_batch(second, &[false]);
    assert!(matches!(
        super::read_stream_resource_value(&active, &mut resource, context),
        Err(super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Failed(code),
            ..
        }) if code == "stream.failed"
    ));
}

#[test]
fn client_stream_cancellation_clears_batches_and_rejects_stale_completions() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let first = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let second = resource.begin_stream_request(&active, Vec::new()).unwrap();
    resource
        .apply_completion(
            &active,
            second
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(true)]),
        )
        .unwrap();
    resource
        .apply_completion(&active, second.clone().cancelled())
        .unwrap();

    assert_eq!(
        resource.take_stream_value(&active),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Cancelled,
        })
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
    assert_eq!(resource.failure(), None);
    assert!(matches!(
        resource.apply_completion(
            &active,
            first.stream_values(vec![RuntimeValue::Boolean(false)]),
        ),
        Err(super::ClientResourceError::StaleGeneration { .. })
    ));
    assert_eq!(
        resource.apply_completion(&active, second.stream_completed()),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Cancelled,
        })
    );
}

fn assert_boolean_stream_batch(value: RuntimeValue, expected: &[bool]) {
    let RuntimeValue::Constructed(option) = value else {
        panic!("stream value must be a constructed OPTION");
    };
    let orna_core::value::ConstructedValueKind::Option(Some(list)) = option.kind() else {
        panic!("stream value must contain a present LIST");
    };
    let RuntimeValue::Constructed(list) = list else {
        panic!("stream OPTION must contain a constructed LIST");
    };
    let orna_core::value::ConstructedValueKind::List(values) = list.kind() else {
        panic!("stream OPTION must contain a LIST");
    };
    let expected = expected
        .iter()
        .copied()
        .map(RuntimeValue::Boolean)
        .collect::<Vec<_>>();
    assert_eq!(values, expected.as_slice());
}

fn assert_boolean_stream_terminal(value: RuntimeValue) {
    let RuntimeValue::Constructed(option) = value else {
        panic!("stream terminal must be a constructed OPTION");
    };
    assert_eq!(
        option.kind(),
        orna_core::value::ConstructedValueKind::Option(None)
    );
}

#[test]
fn stream_descriptor_rejects_unsupported_scalar_items() {
    for scalar in [
        StandardScalar::Decimal,
        StandardScalar::Uuid,
        StandardScalar::Date,
        StandardScalar::Time,
        StandardScalar::Timestamp,
        StandardScalar::Duration,
        StandardScalar::Void,
    ] {
        assert!(super::stream_item_descriptor(ResolvedType::Scalar(scalar)).is_none());
    }
}

#[test]
fn stream_await_expression_and_procedural_local_return_option_list_values() {
    let (active, target, pair, target_revision) = version_two_server_stream_active();
    let item_type = ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID);
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        ResourceKind::Stream,
        target,
        pair,
        CallSiteId::from_bytes([0x91; 16]),
        Vec::new(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let expression = orna_artifact::client_plan::ClientExpressionNode::Await {
        expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
            operation: operation.clone(),
        }),
    };
    let context = super::ClientExecutionContext {
        pair,
        function: target,
        function_revision: target_revision,
        parent_invocation_id: InvocationId::from_bytes([0x92; 16]),
        observer_lineage: None,
    };
    let grants = capability::LocalCapabilityGrantSet::new();
    let mut state = ClientStateStore::new();
    let mut executor = StreamBatchTestExecutor { value: true };
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let mut locals = std::collections::HashMap::new();
    let value = super::evaluate_expression_plan(
        &active,
        &expression,
        context,
        super::ObserverLineage::compatibility(context),
        item_type,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x93; 16]),
        &mut executor_slot,
        &mut locals,
    )
    .expect("stream AWAIT must be checked against its OPTION<LIST<T>> result");
    assert_boolean_stream_batch(value, &[true]);

    let local = LocalId::from_bytes([0x94; 16]);
    let procedural = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::ClientLocalKind::Resource(ResourceKind::Stream),
        )],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::Resource {
                    operation: operation.clone(),
                },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                local,
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        },
    );
    let mut state = ClientStateStore::new();
    let mut executor = StreamBatchTestExecutor { value: false };
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let mut locals = std::collections::HashMap::new();
    let value = super::evaluate_procedural_plan(
        &active,
        &procedural,
        context,
        super::ObserverLineage::compatibility(context),
        item_type,
        false,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x93; 16]),
        &mut executor_slot,
        &mut locals,
    )
    .expect("procedural stream AWAIT must preserve the outer result shape");
    assert_boolean_stream_batch(value, &[false]);

    let value_local = LocalId::from_bytes([0x95; 16]);
    let copy_local = LocalId::from_bytes([0x96; 16]);
    let value_procedural = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![
            orna_artifact::client_plan::ClientLocal::new(
                value_local,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::ClientLocalKind::Value,
            ),
            orna_artifact::client_plan::ClientLocal::new(
                copy_local,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::ClientLocalKind::Value,
            ),
        ],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                value_local,
                orna_artifact::client_plan::ClientExpressionNode::Await {
                    expression: Box::new(
                        orna_artifact::client_plan::ClientExpressionNode::Resource {
                            operation: operation.clone(),
                        },
                    ),
                },
            ),
            orna_artifact::client_plan::ClientStatement::let_(
                copy_local,
                orna_artifact::client_plan::ClientExpressionNode::Boolean { value: false },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                copy_local,
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local: value_local },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::LocalRead { local: copy_local },
    );
    let mut state = ClientStateStore::new();
    let mut executor = StreamBatchTestExecutor { value: true };
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let mut locals = std::collections::HashMap::new();
    let value = super::evaluate_procedural_plan(
        &active,
        &value_procedural,
        context,
        super::ObserverLineage::compatibility(context),
        item_type,
        false,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x93; 16]),
        &mut executor_slot,
        &mut locals,
    )
    .expect("a value local containing stream AWAIT must preserve its outer result shape");
    assert_boolean_stream_batch(value, &[true]);
}

#[test]
fn client_resource_ready_completion_wins_over_late_cancellation() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();
    let generation = request.generation();
    let late_cancellation = request.clone().cancelled();

    resource
        .apply_completion(&active, request.ready(RuntimeValue::Boolean(true)))
        .expect("the accepted completion should make the resource ready");
    assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    assert_eq!(
        resource.cancel(generation),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Ready,
        }),
    );
    assert_eq!(
        resource.apply_completion(&active, late_cancellation),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Ready,
        }),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));
}

#[test]
fn client_resource_executor_rejects_digest_duplicates_stale_and_cancelled() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        vec![ParameterDefinition::new(
            ParameterId::from_bytes([0x01; 16]),
            "enabled",
            0,
            ResolvedType::Scalar(StandardScalar::Boolean),
            None,
        )],
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0x01; 16]),
        RuntimeValue::Boolean(true),
    )
    .unwrap();
    let digest = super::ClientResourceKey::canonical_arguments_digest(
        &active,
        std::slice::from_ref(&argument),
    )
    .unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    let wrong_key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        key.principal(),
        Sha256Digest::from_bytes([0xaa; 32]),
        key.invalidation_token(),
    );
    let mut wrong_resource =
        super::ClientResource::new(wrong_key, ResolvedType::Scalar(StandardScalar::Boolean));
    assert!(matches!(
        wrong_resource.begin_request(&active, vec![argument.clone()]),
        Err(super::ClientResourceError::ArgumentDigestMismatch { .. }),
    ));
    assert_eq!(wrong_resource.status(), super::ClientResourceStatus::Idle);

    assert_eq!(
        resource.begin_request(&active, vec![argument.clone(), argument.clone()]),
        Err(super::ClientResourceError::DuplicateArgument {
            parameter: argument.parameter(),
        }),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Idle);

    let first = resource
        .begin_request(&active, vec![argument.clone()])
        .unwrap();
    let second = resource.begin_request(&active, vec![argument]).unwrap();
    let first_completion = first.ready(RuntimeValue::Boolean(false));
    assert!(matches!(
        resource.apply_completion(&active, first_completion),
        Err(super::ClientResourceError::StaleGeneration { .. }),
    ));
    let second_generation = second.generation();
    resource.cancel(second_generation).unwrap();
    assert!(matches!(
        resource.apply_completion(&active, second.ready(RuntimeValue::Boolean(true))),
        Err(super::ClientResourceError::InvalidTransition {
            status: super::ClientResourceStatus::Cancelled,
        }),
    ));
    assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_accepts_supported_scalar_runtime_values() {
    let cases = [
        (
            ResolvedType::Scalar(StandardScalar::BigInt),
            RuntimeValue::BigInt(42),
        ),
        (
            ResolvedType::Scalar(StandardScalar::Float),
            RuntimeValue::Float(RuntimeFloat::new(4.25).unwrap()),
        ),
        (
            ResolvedType::Scalar(StandardScalar::BinaryLargeObject),
            RuntimeValue::Bytes(vec![0x01, 0x02]),
        ),
    ];

    for (index, (expected, value)) in cases.into_iter().enumerate() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(expected),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x50 + index as u8; 32]),
            Sha256Digest::from_bytes([0x60 + index as u8; 32]),
        );
        let mut resource = super::ClientResource::new(key, expected);
        let generation = resource.begin_loading().unwrap();
        resource
            .publish_ready(&active, generation, value)
            .expect("supported scalar value should publish");
        assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    }
}

#[test]
fn client_resource_accepts_standard_value_contracts() {
    let cases = [
        (orna_standard::BIGINT_TYPE_ID, RuntimeValue::BigInt(42)),
        (
            orna_standard::FLOAT_TYPE_ID,
            RuntimeValue::Float(RuntimeFloat::new(4.25).unwrap()),
        ),
        (
            orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
            RuntimeValue::Bytes(vec![0x01, 0x02]),
        ),
    ];

    for (index, (type_id, value)) in cases.into_iter().enumerate() {
        let (active, function, pair, _) = version_two_value_active(type_id, type_id);
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x90 + index as u8; 32]),
            Sha256Digest::from_bytes([0xa0 + index as u8; 32]),
        );
        let mut resource = super::ClientResource::new(key, ResolvedType::Value(type_id));
        let generation = resource.begin_loading().unwrap();

        resource
            .publish_ready(&active, generation, value)
            .expect("standard value contract should publish");
        assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    }
}

#[test]
fn client_resource_requires_the_full_verified_standard_target_pin() {
    let (active, function, pair, _) = version_two_value_active(
        orna_standard::BOOLEAN_TYPE_ID,
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let wrong_target = InvocationTarget::verified_standard(
        function,
        pair,
        orna_core::StandardLibraryRevisionId::from_bytes([0xee; 16]),
        FunctionRevisionId::from_bytes([0xef; 16]),
    );
    let wrong_key = super::ClientResourceKey::new(
        wrong_target,
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0xb1; 32]),
        Sha256Digest::from_bytes([0xb2; 32]),
    );
    let mut resource =
        super::ClientResource::new(wrong_key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Boolean(true)),
        Err(super::ClientResourceError::TargetMismatch {
            expected: wrong_target,
        }),
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
}

#[test]
fn client_resource_resolves_compiled_verified_standard_server_target() {
    let (active, _, pair, _) = version_two_client_call_active();
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Integer));

    let request = resource
        .begin_request(&active, vec![argument])
        .expect("the pinned standard resource target should validate");

    assert_eq!(request.target(), target);
    assert_eq!(
        request.expected_type(),
        ResolvedType::Scalar(StandardScalar::Integer)
    );
}

#[test]
fn client_resource_validates_named_and_reference_catalogue_membership() {
    let (active, function, pair, _) = version_one_active(true);
    let unknown = TypeId::from_bytes([0xee; 16]);
    let cases = [
        (
            ResolvedType::Named(unknown),
            RuntimeValue::null(ResolvedType::Named(unknown)).unwrap(),
        ),
        (
            ResolvedType::Reference { target: unknown },
            RuntimeValue::Reference {
                target: unknown,
                object: orna_core::ObjectId::from_bytes([0xef; 16]),
            },
        ),
    ];

    for (index, (expected, value)) in cases.into_iter().enumerate() {
        let key = super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x70 + index as u8; 32]),
            Sha256Digest::from_bytes([0x80 + index as u8; 32]),
        );
        let mut resource = super::ClientResource::new(key, expected);
        let generation = resource.begin_loading().unwrap();
        assert_eq!(
            resource.publish_ready(&active, generation, value),
            Err(super::ClientResourceError::TypeMismatch),
        );
        assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    }
}

#[test]
fn client_resource_cache_keeps_key_and_transitions() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0xc1; 32]),
        Sha256Digest::from_bytes([0xc2; 32]),
    );
    let mut state = super::ClientStateStore::new();

    assert!(state.resource(key).is_none());
    {
        let resource =
            state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let generation = resource.begin_loading().unwrap();
        resource
            .publish_ready(&active, generation, RuntimeValue::Boolean(true))
            .unwrap();
    }
    assert_eq!(
        state.resource(key).and_then(super::ClientResource::value),
        Some(&RuntimeValue::Boolean(true)),
    );

    // A duplicate lookup returns the existing resource and keeps its
    // original type and published value.
    let resource = state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer));
    assert_eq!(
        resource.expected_type(),
        ResolvedType::Scalar(StandardScalar::Boolean),
    );
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    let first = resource.begin_loading().unwrap();
    let second = resource.begin_loading().unwrap();
    assert_eq!(
        state
            .resource_mut(key)
            .expect("resource remains in the cache")
            .publish_failure(first, "stale".to_owned()),
        Err(super::ClientResourceError::StaleGeneration {
            expected: second,
            actual: first,
        }),
    );
    assert_eq!(
        state.resource(key).map(super::ClientResource::status),
        Some(super::ClientResourceStatus::Loading),
    );

    state
        .resource_mut(key)
        .expect("resource remains in the cache")
        .cancel(second)
        .unwrap();
    assert_eq!(
        state.resource(key).map(super::ClientResource::status),
        Some(super::ClientResourceStatus::Cancelled),
    );
    let generation_before_invalidation = state
        .resource(key)
        .expect("cancelled resource remains in the cache")
        .generation();
    assert_eq!(state.invalidate_resource(key), Ok(true));
    let resource = state
        .resource(key)
        .expect("invalidated resource remains cached");
    assert_eq!(resource.key(), key);
    assert_eq!(
        resource.expected_type(),
        ResolvedType::Scalar(StandardScalar::Boolean),
    );
    assert_eq!(
        resource.generation().value(),
        generation_before_invalidation.value() + 1,
    );
    assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
    assert_eq!(
        state.invalidate_resource(super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7b; 16]),
            Sha256Digest::from_bytes([0xc1; 32]),
            Sha256Digest::from_bytes([0xc2; 32]),
        )),
        Ok(false)
    );
}

#[test]
fn resource_invalidation_cancels_owned_request_and_rejects_late_completion() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc2; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let late_completion = request.clone().ready(RuntimeValue::Integer(42));
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Ok(true),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert_eq!(executor.poll(), None);
    assert_eq!(executor.late_dropped, 1);
    assert_eq!(
        state
            .resource(key)
            .expect("invalidated resource remains cached")
            .status(),
        super::ClientResourceStatus::Idle,
    );
    assert_eq!(
        state
            .resource_mut(key)
            .expect("invalidated resource remains cached")
            .apply_completion(&active, late_completion),
        Err(super::ClientResourceError::StaleGeneration {
            expected: super::ClientResourceGeneration(2),
            actual: request.generation(),
        }),
    );
}

#[test]
fn resource_stream_invalidation_abandons_nonterminal_values_before_invalidation() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc7; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource_with_kind(
            key,
            ResourceKind::Stream,
            ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
        )
        .begin_stream_request(&active, Vec::new())
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_stream_values();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Ok(true),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert_eq!(executor.poll(), None);
    assert_eq!(executor.late_dropped, 1);
    let resource = state
        .resource(key)
        .expect("invalidated stream resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Idle);
    assert_eq!(
        resource.generation().value(),
        request.generation().value() + 1
    );
    assert_eq!(resource.request_id(), None);
}

#[test]
fn resource_stream_invalidation_keeps_state_when_abandon_fails() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc8; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource_with_kind(
            key,
            ResourceKind::Stream,
            ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
        )
        .begin_stream_request(&active, Vec::new())
        .unwrap();
    let before = state
        .resource(key)
        .expect("pending stream resource remains cached")
        .clone();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_stream_values()
        .with_abandon_failure();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::ClientResourceError::Executor(
            "resource executor cannot abandon a pending request".to_owned(),
        )),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert_eq!(state.resource(key), Some(&before));
}

#[test]
fn resource_invalidation_keeps_terminal_ready_completion() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc5; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(99));
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Ok(true),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert!(executor.abandoned.is_empty());
    assert!(executor.pending.is_none());
    let resource = state
        .resource(key)
        .expect("terminal resource remains cached");
    assert_eq!(resource.status(), super::ClientResourceStatus::Ready);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.value(), Some(&RuntimeValue::Integer(99)));
    assert_eq!(resource.failure(), None);
}

#[test]
fn resource_invalidation_rejects_wrong_typed_terminal_cancellation() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc6; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_value(RuntimeValue::Text("wrong cancellation type".to_owned()));
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    // A malformed terminal cancellation consumed the executor request, so
    // the resource must not remain Loading without an owner.
    assert!(executor.pending.is_none());
    let resource = state
        .resource(key)
        .expect("malformed cancellation leaves a safe resource state");
    assert_eq!(resource.status(), super::ClientResourceStatus::Cancelled);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), None);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
}

#[test]
fn resource_invalidation_rejects_mismatched_terminal_cancellation_without_losing_owner() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xd1; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut mismatched = request.clone();
    mismatched.request_id = InvocationId::from_bytes([0xfd; 16]);
    let mut executor = RecordingActionExecutor::new(None).with_cancel_terminal_identity(mismatched);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::ClientResourceError::RequestIdMismatch {
            expected: request.request_id(),
            actual: InvocationId::from_bytes([0xfd; 16]),
        }),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("mismatched cancellation leaves the resource cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn resource_invalidation_rejects_active_mismatch_before_consuming_request() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x93; 16]),
        CatalogueRevisionId::from_bytes([0x94; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc9; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&changed_active, key, &mut executor),
        Err(super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert!(executor.cancelled.is_empty());
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("mismatched resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_rejects_local_request_mismatch_before_cancel() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x95; 16]),
        CatalogueRevisionId::from_bytes([0x96; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xca; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcb; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    state
        .resource_mut(old_key)
        .expect("stale resource remains cached")
        .expected_type = ResolvedType::Scalar(StandardScalar::Integer);
    let mut executor = RecordingActionExecutor::new(None);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert!(executor.cancelled.is_empty());
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("stale resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_rejects_mismatched_pending_without_losing_owner() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x99; 16]),
        CatalogueRevisionId::from_bytes([0x9a; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xce; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcf; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut mismatched = request.clone();
    mismatched.request_id = InvocationId::from_bytes([0xfe; 16]);
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending_identity(mismatched);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::ClientResourceError::RequestIdMismatch {
            expected: request.request_id(),
            actual: InvocationId::from_bytes([0xfe; 16]),
        }),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("stale resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_rejects_mismatched_terminal_cancellation_without_losing_owner() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0xda; 16]),
        CatalogueRevisionId::from_bytes([0xdb; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xdc; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xdd; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut mismatched = request.clone();
    mismatched.key = new_key;
    let mut executor = RecordingActionExecutor::new(None).with_cancel_terminal_identity(mismatched);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::ClientResourceError::RequestKeyMismatch {
            expected: Box::new(old_key),
            actual: Box::new(new_key),
        }),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("mismatched cancellation leaves the stale resource cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_malformed_terminal_cancellation_is_safe() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x97; 16]),
        CatalogueRevisionId::from_bytes([0x98; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcc; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcd; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(7));
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("stale resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Cancelled);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), None);
}

#[test]
fn stale_replacement_uses_pinned_validation_after_revision_changes() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0xa1; 16]),
        CatalogueRevisionId::from_bytes([0xa2; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xa3; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xa4; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Boolean(true));
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        )
        .unwrap();

    assert_eq!(executor.cancelled, vec![request]);
    assert_eq!(
        state.resource(old_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn stale_replacement_accepts_typed_null_for_primitive_value_type() {
    let (active, _function, pair, _, _parameter) = version_six_client_resource_action_active();
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0xa7; 16]),
        CatalogueRevisionId::from_bytes([0xa8; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/typed-null".to_owned()),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let target_function = FunctionId::from_bytes([0xd1; 16]);
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(target_function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xa9; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(target_function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xaa; 32]),
    );
    let expected = ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID);
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, expected)
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::null(expected).unwrap());
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(&changed_active, new_key, expected, &mut executor)
        .unwrap();

    assert_eq!(executor.cancelled, vec![request]);
    assert_eq!(
        state.resource(old_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn resource_invalidation_preflights_generation_before_releasing_request() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc4; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = {
        let resource =
            state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer));
        resource.generation = super::ClientResourceGeneration(u64::MAX - 1);
        resource.begin_request(&active, vec![argument]).unwrap()
    };
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::ClientResourceError::GenerationExhausted),
    );
    assert!(executor.cancelled.is_empty());
    assert!(executor.abandoned.is_empty());
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("exhausted resource remains cached");
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(
        resource.generation(),
        super::ClientResourceGeneration(u64::MAX)
    );
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn resource_invalidation_retains_owned_request_when_abandon_fails() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc3; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let before = state
        .resource(key)
        .expect("pending resource remains cached")
        .clone();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_pending()
        .with_abandon_failure();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::ClientResourceError::Executor(
            "resource executor cannot abandon a pending request".to_owned(),
        )),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("failed invalidation retains the resource");
    assert_eq!(resource, &before);
    assert_eq!(resource.status(), super::ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
}

#[test]
fn replacing_complete_resource_key_cancels_previous_generation() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key_a = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xd2; 32]),
    );
    let key_b = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xd3; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None);

    state
        .get_or_create_resource_with_executor(
            &active,
            key_b,
            ResolvedType::Scalar(StandardScalar::Integer),
            &mut executor,
        )
        .unwrap();

    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(
        state.resource(key_a).map(super::ClientResource::status),
        Some(super::ClientResourceStatus::Idle),
    );
    assert!(matches!(
        state
            .resource_mut(key_a)
            .expect("replaced resource remains cached")
            .apply_completion(&active, request.ready(RuntimeValue::Integer(42))),
        Err(super::ClientResourceError::StaleGeneration { .. }),
    ));
    assert_eq!(
        state.resource(key_b).map(super::ClientResource::status),
        Some(super::ClientResourceStatus::Idle),
    );
}

#[test]
fn replacing_same_revision_keeps_terminal_executor_completion() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let old_key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe8; 32]),
    );
    let new_key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe9; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(99));
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(
            &active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Integer),
            &mut executor,
        )
        .unwrap();

    let old_resource = state.resource(old_key).expect("old key remains cached");
    assert_eq!(old_resource.status(), ClientResourceStatus::Ready);
    assert_eq!(old_resource.value(), Some(&RuntimeValue::Integer(99)));
    assert_eq!(old_resource.generation(), request.generation());
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn replacing_resource_key_across_revision_releases_old_executor_request() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x91; 16]),
        CatalogueRevisionId::from_bytes([0x92; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xe6; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xe7; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        )
        .unwrap();

    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert_eq!(executor.poll(), None);
    assert_eq!(executor.late_dropped, 1);
    assert!(matches!(
        state
            .resource_mut(old_key)
            .expect("old key remains cached")
            .apply_completion(&changed_active, request.ready(RuntimeValue::Boolean(true))),
        Err(super::ClientResourceError::StaleGeneration { .. }),
    ));
    assert_eq!(
        state.resource(old_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn replacing_resource_key_retains_nested_request_when_abandon_fails() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key_a = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe4; 32]),
    );
    let key_b = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe5; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_pending()
        .with_abandon_failure();
    executor.pending = Some(request.clone());
    let pending_identity = (request.request_id(), request.key(), request.generation());
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    let result = state.get_or_create_resource_with_executor(
        &active,
        key_b,
        ResolvedType::Scalar(StandardScalar::Integer),
        &mut nested,
    );

    assert!(matches!(
        result,
        Err(super::ClientResourceError::Executor(message))
            if message == "resource executor cannot abandon a pending request"
    ));
    let mut mismatch_state = ClientStateStore::new();
    let mismatched_request = mismatch_state
        .get_or_create_resource(key_b, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, request.arguments().to_vec())
        .unwrap();
    assert_eq!(
        nested.abandon(mismatched_request),
        Err("resource executor request mismatch".to_owned()),
    );
    assert_eq!(nested.pending_request_identity(), Some(pending_identity));
    assert!(nested.release_failed());
    assert_eq!(nested.pending_request_identity(), Some(pending_identity));
    drop(nested);
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(key_b).is_none());
    assert_eq!(
        state.resource(key_a).map(super::ClientResource::status),
        Some(super::ClientResourceStatus::Loading),
    );
}

#[test]
fn client_resource_cache_keeps_distinct_complete_keys_independent() {
    let (_, function, pair, _) = version_one_active(true);
    let target = InvocationTarget::new(function, pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let key_a = super::ClientResourceKey::new(
        target,
        principal,
        Sha256Digest::from_bytes([0xd1; 32]),
        Sha256Digest::from_bytes([0xd2; 32]),
    );
    let key_b = super::ClientResourceKey::new(
        target,
        principal,
        Sha256Digest::from_bytes([0xd1; 32]),
        Sha256Digest::from_bytes([0xd3; 32]),
    );
    assert_ne!(key_a, key_b);
    let mut state = super::ClientStateStore::new();

    state.get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Boolean));
    state.get_or_create_resource(key_b, ResolvedType::Scalar(StandardScalar::Boolean));

    let resource_a = state.resource(key_a).expect("first resource is cached");
    let resource_b = state.resource(key_b).expect("second resource is cached");
    assert_eq!(resource_a.key(), key_a);
    assert_eq!(resource_b.key(), key_b);

    let generation = state
        .resource_mut(key_a)
        .expect("first resource is cached")
        .begin_loading()
        .unwrap();
    assert_eq!(
        state.resource(key_a).map(super::ClientResource::status),
        Some(super::ClientResourceStatus::Loading),
    );
    assert_eq!(
        state.resource(key_b).map(super::ClientResource::status),
        Some(super::ClientResourceStatus::Idle),
    );
    state
        .resource_mut(key_a)
        .expect("first resource is cached")
        .cancel(generation)
        .unwrap();
}

fn version_four_text_state_plan() -> (
    ActiveDatabaseRevision,
    FunctionId,
    orna_artifact::client_plan::StateClientPlan,
) {
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Concat {
            left: Box::new(orna_artifact::client_plan::ClientExpressionNode::String {
                value: "hello ".to_owned(),
            }),
            right: Box::new(orna_artifact::client_plan::ClientExpressionNode::String {
                value: "world".to_owned(),
            }),
        },
        vec![
            orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x11; 16]),
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "local-default".to_owned(),
                    },
                ),
            ),
            orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x12; 16]),
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Session,
                orna_artifact::client_plan::StateDefault::Null,
            ),
            orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x13; 16]),
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Unset,
            ),
        ],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    (active, function, plan)
}

#[test]
fn evaluates_version_four_state_plans_and_initialises_local_and_session_state() {
    let (active, function, plan) = version_four_text_state_plan();
    let mut state = super::ClientStateStore::new();

    let result = super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("hello world".to_owned())
    );
    assert_eq!(
        state.local().get(&super::ClientStateKey::new(
            function,
            StateSlotId::from_bytes([0x11; 16])
        )),
        Some(&RuntimeValue::Text("local-default".to_owned()))
    );
    let expected_null = RuntimeValue::null(ResolvedType::value(
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    ))
    .unwrap();
    assert_eq!(
        state.session().get(&super::ClientStateKey::new(
            function,
            StateSlotId::from_bytes([0x12; 16])
        )),
        Some(&expected_null)
    );
    assert!(!state.local().contains_key(&super::ClientStateKey::new(
        function,
        StateSlotId::from_bytes([0x13; 16])
    )));
    assert!(state.user().is_empty());
    assert_eq!(
        plan.format_version(),
        orna_artifact::client_plan::STATE_FORMAT_VERSION
    );

    super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();
}

#[test]
fn state_context_data_invalidation_token_preserves_existing_defaults() {
    let function = FunctionId::from_bytes([0x61; 16]);
    let mut context = super::ClientStateContext::default_for(function);
    assert_eq!(
        context.data_invalidation_token(),
        Sha256Digest::from_bytes([0; 32])
    );
    assert_eq!(
        super::ClientStateContext::new(function, "profile".to_owned(), "instance".to_owned())
            .unwrap()
            .data_invalidation_token(),
        Sha256Digest::from_bytes([0; 32]),
    );
    let token = Sha256Digest::from_bytes([0x62; 32]);
    context.set_data_invalidation_token(token);
    assert_eq!(context.data_invalidation_token(), token);
    assert_eq!(context.root_function(), function);
    assert_eq!(context.state_profile(), "");
    assert_eq!(context.instance_key(), "");
}

#[test]
fn version_four_state_context_profiles_are_isolated() {
    let (active, function, _) = version_four_text_state_plan();
    let profile_a =
        super::ClientStateContext::new(function, "profile-a".to_owned(), String::new()).unwrap();
    let profile_b =
        super::ClientStateContext::new(function, "profile-b".to_owned(), String::new()).unwrap();
    let mut state = super::ClientStateStore::new();
    let grants = super::capability::LocalCapabilityGrantSet::new();
    let slot = StateSlotId::from_bytes([0x12; 16]);

    super::evaluate_client_function_in_state_context_with_grants_and_arguments(
        &active,
        &authorise(active.pair(), function),
        &profile_a,
        &[],
        &[],
        &grants,
        &mut state,
    )
    .unwrap();
    let mut executor =
        super::DeterministicClientResourceExecutor::new(|_: &super::ClientResourceRequest| {
            Ok(RuntimeValue::Boolean(true))
        });
    super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(active.pair(), function),
            &profile_b,
            &[],
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x44; 16]),
            &mut executor,
        )
        .unwrap();

    let key_a = super::ClientStateKey::from_context(&profile_a, function, slot);
    let key_b = super::ClientStateKey::from_context(&profile_b, function, slot);
    assert_ne!(key_a, key_b);
    assert!(state.session().contains_key(&key_a));
    assert!(state.session().contains_key(&key_b));
    assert_eq!(state.context(), &profile_b);
}

#[test]
fn version_four_keeps_caller_state_input_over_the_plan_default() {
    let (active, function, _) = version_four_text_state_plan();
    let mut state = super::ClientStateStore::new();
    state.session_mut().insert(
        super::ClientStateKey::new(function, StateSlotId::from_bytes([0x12; 16])),
        RuntimeValue::Text("remounted-session".to_owned()),
    );

    super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(
        state.session().get(&super::ClientStateKey::new(
            function,
            StateSlotId::from_bytes([0x12; 16])
        )),
        Some(&RuntimeValue::Text("remounted-session".to_owned()))
    );
}

#[test]
fn version_four_rejects_caller_state_with_the_wrong_type() {
    let (active, function, _) = version_four_text_state_plan();
    let mut state = super::ClientStateStore::new();
    state.session_mut().insert(
        super::ClientStateKey::new(function, StateSlotId::from_bytes([0x12; 16])),
        RuntimeValue::Boolean(true),
    );

    let error = super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::ClientExecutionError::StateEvaluation {
            context,
            source: super::ClientStateError::StoredTypeMismatch { slot },

        } if context.function() == function
            && *slot == StateSlotId::from_bytes([0x12; 16])
    ));
}

#[test]
fn version_four_user_state_with_matching_persisted_type_is_accepted() {
    let slot = StateSlotId::from_bytes([0x20; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![orna_artifact::client_plan::StateSlot::new(
            slot,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::ClientStateStore::new();
    state.set_context(super::ClientStateContext::default_for(function));
    let durable_key = UserStateKey::new(
        PrincipalId::from_bytes([0x7a; 16]),
        function,
        String::new(),
        function,
        String::new(),
        slot,
    )
    .unwrap();
    state
        .load_user_state(&[UserStateCell::new(
            durable_key,
            RuntimeValue::Boolean(true),
            orna_standard::BOOLEAN_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();

    let result = super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    assert_eq!(state.user().len(), 1);
    assert_eq!(
        state
            .user()
            .values()
            .next()
            .expect("the matching USER state remains loaded")
            .value_type(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
}

#[test]
fn version_four_user_state_rejects_wrong_persisted_type_without_mutating_state() {
    let slot = StateSlotId::from_bytes([0x22; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![orna_artifact::client_plan::StateSlot::new(
            slot,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::ClientStateStore::new();
    state.set_context(super::ClientStateContext::default_for(function));
    let durable_key = UserStateKey::new(
        PrincipalId::from_bytes([0x7a; 16]),
        function,
        String::new(),
        function,
        String::new(),
        slot,
    )
    .unwrap();
    state
        .load_user_state(&[UserStateCell::new(
            durable_key,
            RuntimeValue::Boolean(true),
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let before = state.clone();

    let error = super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::ClientExecutionError::StateEvaluation {
            context,
            source: super::ClientStateError::StoredTypeMismatch { slot: actual_slot },
        } if context.function() == function && actual_slot == slot
    ));
    assert_eq!(state, before);
}

#[test]
fn version_four_user_state_without_persisted_value_uses_unset_default() {
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![orna_artifact::client_plan::StateSlot::new(
            StateSlotId::from_bytes([0x21; 16]),
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::User,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::ClientStateStore::new();

    let result = super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap();

    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
    assert!(state.user().is_empty());
    assert!(state.local().is_empty() && state.session().is_empty());
}
#[test]
fn client_user_state_store_loads_updates_and_applies_write_results() {
    let root_function = FunctionId::from_bytes([0x31; 16]);
    let function = FunctionId::from_bytes([0x32; 16]);
    let slot = StateSlotId::from_bytes([0x33; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let client_key = super::ClientStateKey::from_context(&context, function, slot);
    let durable_key = UserStateKey::new(
        PrincipalId::from_bytes([0x34; 16]),
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let cell = UserStateCell::new(
        durable_key,
        RuntimeValue::Text("loaded".to_owned()),
        value_type,
        7,
        SystemTime::UNIX_EPOCH,
    );
    let mut state = super::ClientStateStore::new();
    state.set_context(context);
    state.load_user_state(&[cell]).unwrap();
    assert!(state.pending_user_state_changes().unwrap().is_empty());

    state
        .set_user_state(
            client_key.clone(),
            RuntimeValue::Text("changed".to_owned()),
            value_type,
        )
        .unwrap();
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].expected_revision(), Some(7));
    let before = state.user().clone();
    let leap_result = UserStateWriteResult::new(
        changes[0].key_without_principal(),
        UserStateWriteOutcome::Written { revision: 9 },
    );
    let leap_error = state
        .apply_user_state_write_results(&changes, &[leap_result])
        .unwrap_err();
    assert!(matches!(
        leap_error,
        super::ClientUserStateError::InvalidRevision(key) if key == client_key
    ));
    assert_eq!(state.user(), &before);
    assert_eq!(state.pending_user_state_changes().unwrap(), changes);

    let result = UserStateWriteResult::new(
        changes[0].key_without_principal(),
        UserStateWriteOutcome::Written { revision: 8 },
    );
    state
        .apply_user_state_write_results(&changes, &[result])
        .unwrap();

    let stored = state.user().get(&client_key).unwrap();
    assert_eq!(stored.value(), &RuntimeValue::Text("changed".to_owned()));
    assert_eq!(stored.revision(), Some(8));
    assert!(!stored.is_dirty());
    assert!(state.pending_user_state_changes().unwrap().is_empty());
}

#[test]
fn client_user_state_set_rejects_context_mismatch_before_lookup_or_mutation() {
    let root_function = FunctionId::from_bytes([0xc1; 16]);
    let function = FunctionId::from_bytes([0xc2; 16]);
    let slot = StateSlotId::from_bytes([0xc3; 16]);
    let principal = PrincipalId::from_bytes([0xc4; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let other_context = super::ClientStateContext::new(
        FunctionId::from_bytes([0xc5; 16]),
        "other-profile".to_owned(),
        "other-instance".to_owned(),
    )
    .unwrap();
    let matching_key = super::ClientStateKey::from_context(&context, function, slot);
    let mismatched_key = super::ClientStateKey::from_context(&other_context, function, slot);
    let durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let mut state = super::ClientStateStore::new();
    state.set_context(context);
    state
        .load_user_state(&[UserStateCell::new(
            durable_key,
            RuntimeValue::Text("loaded".to_owned()),
            value_type,
            7,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let before = state.user().clone();
    let pending_before = state.pending_user_state_changes().unwrap();

    let error = state
        .set_user_state(
            mismatched_key.clone(),
            RuntimeValue::Boolean(true),
            orna_standard::BOOLEAN_TYPE_ID,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        super::ClientUserStateError::ContextMismatch(key) if key == mismatched_key
    ));
    assert_eq!(state.user(), &before);
    assert_eq!(state.pending_user_state_changes().unwrap(), pending_before);

    state
        .set_user_state(
            matching_key.clone(),
            RuntimeValue::Text("changed".to_owned()),
            value_type,
        )
        .unwrap();
    let stored = state.user().get(&matching_key).unwrap();
    assert_eq!(stored.value(), &RuntimeValue::Text("changed".to_owned()));
    assert_eq!(stored.revision(), Some(7));
    assert!(stored.is_dirty());
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].expected_revision(), Some(7));
}

#[test]
fn client_user_state_load_rejects_mixed_context_batch_atomically() {
    let root_function = FunctionId::from_bytes([0x71; 16]);
    let function = FunctionId::from_bytes([0x72; 16]);
    let slot = StateSlotId::from_bytes([0x73; 16]);
    let principal = PrincipalId::from_bytes([0x74; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let matching_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let mismatched_key = UserStateKey::new(
        principal,
        FunctionId::from_bytes([0x75; 16]),
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let mut state = super::ClientStateStore::new();
    state.set_context(context);
    state
        .load_user_state(&[UserStateCell::new(
            matching_key.clone(),
            RuntimeValue::Text("before".to_owned()),
            value_type,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let before = state.user().clone();

    let error = state
        .load_user_state(&[
            UserStateCell::new(
                matching_key,
                RuntimeValue::Text("replacement".to_owned()),
                value_type,
                2,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                mismatched_key,
                RuntimeValue::Text("outside-context".to_owned()),
                value_type,
                3,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap_err();

    assert!(matches!(
        error,
        super::ClientUserStateError::ContextMismatch(key)
            if key.root_function() == FunctionId::from_bytes([0x75; 16])
    ));
    assert_eq!(state.user(), &before);
}

#[test]
fn client_user_state_load_accepts_multiple_instances_and_rejects_foreign_context_atomically() {
    let root_function = FunctionId::from_bytes([0x81; 16]);
    let function = FunctionId::from_bytes([0x82; 16]);
    let slot = StateSlotId::from_bytes([0x83; 16]);
    let principal = PrincipalId::from_bytes([0x84; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "active-instance".to_owned(),
    )
    .unwrap();
    let active_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "active-instance".to_owned(),
        slot,
    )
    .unwrap();
    let dynamic_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "row:42".to_owned(),
        slot,
    )
    .unwrap();
    let selected_absent_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "row:empty".to_owned(),
        slot,
    )
    .unwrap();
    let unselected_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "row:99".to_owned(),
        slot,
    )
    .unwrap();
    let foreign_key = UserStateKey::new(
        principal,
        FunctionId::from_bytes([0x85; 16]),
        "foreign-profile".to_owned(),
        function,
        "foreign-instance".to_owned(),
        slot,
    )
    .unwrap();
    let requested_instances = vec![
        (function, "active-instance".to_owned()),
        (function, "row:42".to_owned()),
        (function, "row:empty".to_owned()),
    ];
    let unselected_client_key = super::ClientStateKey::from_user_cell(&UserStateCell::new(
        unselected_key.clone(),
        RuntimeValue::Text("unselected-before".to_owned()),
        value_type,
        1,
        SystemTime::UNIX_EPOCH,
    ));
    let dynamic_client_key = super::ClientStateKey::from_user_cell(&UserStateCell::new(
        dynamic_key.clone(),
        RuntimeValue::Text("dynamic-loaded".to_owned()),
        value_type,
        3,
        SystemTime::UNIX_EPOCH,
    ));
    let mut state = super::ClientStateStore::new();
    state.set_context(context.clone());

    state
        .load_user_state(&[
            UserStateCell::new(
                active_key.clone(),
                RuntimeValue::Text("active-before".to_owned()),
                value_type,
                1,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                unselected_key.clone(),
                RuntimeValue::Text("unselected-before".to_owned()),
                value_type,
                1,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                selected_absent_key.clone(),
                RuntimeValue::Text("selected-before".to_owned()),
                value_type,
                1,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap();
    state.set_context(
        super::ClientStateContext::new(root_function, "profile".to_owned(), "row:99".to_owned())
            .unwrap(),
    );
    state
        .set_user_state(
            unselected_client_key.clone(),
            RuntimeValue::Text("unselected-dirty".to_owned()),
            value_type,
        )
        .unwrap();
    state.set_context(context.clone());

    state
        .load_user_state_for_instances(
            &[
                UserStateCell::new(
                    active_key.clone(),
                    RuntimeValue::Text("active-loaded".to_owned()),
                    value_type,
                    2,
                    SystemTime::UNIX_EPOCH,
                ),
                UserStateCell::new(
                    dynamic_key.clone(),
                    RuntimeValue::Text("dynamic-loaded".to_owned()),
                    value_type,
                    3,
                    SystemTime::UNIX_EPOCH,
                ),
            ],
            &requested_instances,
        )
        .unwrap();
    assert_eq!(state.user().len(), 3);
    assert!(
        !state
            .user()
            .contains_key(&super::ClientStateKey::from_user_cell(&UserStateCell::new(
                selected_absent_key.clone(),
                RuntimeValue::Text("selected-before".to_owned()),
                value_type,
                1,
                SystemTime::UNIX_EPOCH,
            ),))
    );
    assert_eq!(
        state
            .user()
            .get(&super::ClientStateKey::from_user_cell(&UserStateCell::new(
                active_key.clone(),
                RuntimeValue::Text("active-loaded".to_owned()),
                value_type,
                2,
                SystemTime::UNIX_EPOCH,
            ),))
            .map(super::ClientUserState::value),
        Some(&RuntimeValue::Text("active-loaded".to_owned())),
    );
    assert_eq!(
        state
            .user()
            .get(&super::ClientStateKey::from_user_cell(&UserStateCell::new(
                dynamic_key.clone(),
                RuntimeValue::Text("dynamic-loaded".to_owned()),
                value_type,
                3,
                SystemTime::UNIX_EPOCH,
            ),))
            .map(super::ClientUserState::value),
        Some(&RuntimeValue::Text("dynamic-loaded".to_owned())),
    );

    let unselected = state.user().get(&unselected_client_key).unwrap();
    assert_eq!(
        unselected.value(),
        &RuntimeValue::Text("unselected-dirty".to_owned()),
    );
    assert!(unselected.is_dirty());
    let pending = state.pending_user_state_changes().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].instance_key(), "row:99");

    let set_error = state
        .set_user_state(
            dynamic_client_key.clone(),
            RuntimeValue::Text("must-not-set-dynamic".to_owned()),
            value_type,
        )
        .unwrap_err();
    assert!(matches!(
        set_error,
        super::ClientUserStateError::ContextMismatch(key) if key == dynamic_client_key
    ));

    let before_unexpected = state.user().clone();
    let unexpected_error = state
        .load_user_state_for_instances(
            &[UserStateCell::new(
                unselected_key.clone(),
                RuntimeValue::Text("unexpected-instance".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            )],
            &requested_instances,
        )
        .unwrap_err();
    assert!(matches!(
        unexpected_error,
        super::ClientUserStateError::ContextMismatch(key)
            if key.instance_key() == "row:99"
    ));
    assert_eq!(state.user(), &before_unexpected);

    let before_duplicate = state.user().clone();
    let duplicate_error = state
        .load_user_state(&[
            UserStateCell::new(
                dynamic_key.clone(),
                RuntimeValue::Text("duplicate-first".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                dynamic_key.clone(),
                RuntimeValue::Text("duplicate-second".to_owned()),
                value_type,
                5,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap_err();
    assert!(matches!(
        duplicate_error,
        super::ClientUserStateError::DuplicateKey(key)
            if key.instance_key() == "row:42"
    ));
    assert_eq!(state.user(), &before_duplicate);

    let before_foreign = state.user().clone();
    let error = state
        .load_user_state(&[
            UserStateCell::new(
                active_key,
                RuntimeValue::Text("must-not-replace".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                foreign_key,
                RuntimeValue::Text("must-not-load".to_owned()),
                value_type,
                5,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap_err();
    assert!(matches!(
        error,
        super::ClientUserStateError::ContextMismatch(key)
            if key.root_function() == FunctionId::from_bytes([0x85; 16])
                && key.state_profile() == "foreign-profile"
    ));
    assert_eq!(state.user(), &before_foreign);
}

#[test]
fn client_user_state_empty_filter_accepts_the_default_instance_cell() {
    let root_function = FunctionId::from_bytes([0xa6; 16]);
    let function = FunctionId::from_bytes([0xa7; 16]);
    let slot = StateSlotId::from_bytes([0xa8; 16]);
    let principal = PrincipalId::from_bytes([0xa9; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "mounted-instance".to_owned(),
    )
    .unwrap();
    let default_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        String::new(),
        slot,
    )
    .unwrap();
    let default_cell = UserStateCell::new(
        default_key,
        RuntimeValue::Text("default".to_owned()),
        value_type,
        1,
        SystemTime::UNIX_EPOCH,
    );
    let client_key = super::ClientStateKey::from_user_cell(&default_cell);
    let mut state = super::ClientStateStore::new();
    state.set_context(context);

    state
        .load_user_state_for_instances(&[default_cell], &[])
        .unwrap();

    assert_eq!(
        state
            .user()
            .get(&client_key)
            .map(super::ClientUserState::value),
        Some(&RuntimeValue::Text("default".to_owned())),
    );
    assert_eq!(
        state
            .user()
            .get(&client_key)
            .map(super::ClientUserState::revision),
        Some(Some(1)),
    );
}

#[test]
fn client_user_state_load_replaces_prior_context_cells() {
    let root_function = FunctionId::from_bytes([0x61; 16]);
    let function = FunctionId::from_bytes([0x62; 16]);
    let slot = StateSlotId::from_bytes([0x63; 16]);
    let other_root_function = FunctionId::from_bytes([0x64; 16]);
    let other_function = FunctionId::from_bytes([0x65; 16]);
    let other_slot = StateSlotId::from_bytes([0x66; 16]);
    let principal = PrincipalId::from_bytes([0x67; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let other_context = super::ClientStateContext::new(
        other_root_function,
        "other-profile".to_owned(),
        "other-instance".to_owned(),
    )
    .unwrap();
    let current_client_key = super::ClientStateKey::from_context(&context, function, slot);
    let other_client_key =
        super::ClientStateKey::from_context(&other_context, other_function, other_slot);
    let current_durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        slot,
    )
    .unwrap();
    let other_durable_key = UserStateKey::new(
        principal,
        other_root_function,
        "other-profile".to_owned(),
        other_function,
        "other-instance".to_owned(),
        other_slot,
    )
    .unwrap();
    let mut state = super::ClientStateStore::new();

    state.set_context(context.clone());
    state
        .load_user_state(&[UserStateCell::new(
            current_durable_key,
            RuntimeValue::Text("principal-a".to_owned()),
            value_type,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    state.set_context(other_context);
    state
        .load_user_state(&[UserStateCell::new(
            other_durable_key,
            RuntimeValue::Text("other-context".to_owned()),
            value_type,
            2,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();

    state.set_context(context);
    state.load_user_state(&[]).unwrap();

    assert!(!state.user().contains_key(&current_client_key));
    assert!(state.user().contains_key(&other_client_key));
}

#[test]
fn client_user_state_binding_rejects_other_session_without_mutating_cells_or_pending_changes() {
    let root_function = FunctionId::from_bytes([0x91; 16]);
    let function = FunctionId::from_bytes([0x92; 16]);
    let first_slot = StateSlotId::from_bytes([0x93; 16]);
    let second_slot = StateSlotId::from_bytes([0x94; 16]);
    let principal = PrincipalId::from_bytes([0x95; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x96; 16]),
        CatalogueRevisionId::from_bytes([0x97; 16]),
    );
    let snapshot = SecuritySnapshot::new(
        pair,
        vec![],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("session binding fixture should be valid");
    let first_session = snapshot
        .bind_authenticated_session(principal, vec![])
        .expect("first session should bind");
    let second_session = snapshot
        .bind_authenticated_session(principal, vec![])
        .expect("second session should bind");
    assert_eq!(first_session.binding(), first_session.clone().binding());
    assert_ne!(first_session.binding(), second_session.binding());

    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let first_client_key = super::ClientStateKey::from_context(&context, function, first_slot);
    let second_client_key = super::ClientStateKey::from_context(&context, function, second_slot);
    let first_durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        first_slot,
    )
    .unwrap();
    let second_durable_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        second_slot,
    )
    .unwrap();
    let mut state = super::ClientStateStore::new();
    state.set_context(context);
    assert!(
        state
            .bind_authenticated_session(first_session.binding())
            .is_ok()
    );
    assert!(
        state
            .bind_authenticated_session(first_session.clone().binding())
            .is_ok()
    );
    state
        .load_user_state(&[
            UserStateCell::new(
                first_durable_key,
                RuntimeValue::Text("first".to_owned()),
                value_type,
                4,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                second_durable_key,
                RuntimeValue::Text("second".to_owned()),
                value_type,
                8,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap();
    state
        .set_user_state(
            first_client_key,
            RuntimeValue::Text("first-dirty".to_owned()),
            value_type,
        )
        .unwrap();
    let durable_before = state.user().clone();
    let pending_before = state.pending_user_state_changes().unwrap();
    assert_eq!(pending_before.len(), 1);

    assert_eq!(
        state.bind_authenticated_session(second_session.binding()),
        Err(super::ClientUserStateError::SessionMismatch)
    );
    assert_eq!(state.user(), &durable_before);
    assert_eq!(state.pending_user_state_changes().unwrap(), pending_before);
    assert!(
        state
            .bind_authenticated_session(first_session.binding())
            .is_ok()
    );
    assert_eq!(state.user().len(), 2);
    assert!(state.user().contains_key(&second_client_key));
}

#[test]
fn client_user_state_store_rejects_first_write_revision_leap() {
    let root_function = FunctionId::from_bytes([0x51; 16]);
    let function = FunctionId::from_bytes([0x52; 16]);
    let slot = StateSlotId::from_bytes([0x53; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let client_key = super::ClientStateKey::from_context(&context, function, slot);
    let mut state = super::ClientStateStore::new();
    state.set_context(context);
    state
        .set_user_state(
            client_key.clone(),
            RuntimeValue::Text("new".to_owned()),
            value_type,
        )
        .unwrap();
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].expected_revision(), None);
    let before = state.user().clone();
    let result = UserStateWriteResult::new(
        changes[0].key_without_principal(),
        UserStateWriteOutcome::Written { revision: 2 },
    );

    let error = state
        .apply_user_state_write_results(&changes, &[result])
        .unwrap_err();

    assert!(matches!(
        error,
        super::ClientUserStateError::InvalidRevision(key) if key == client_key
    ));
    assert_eq!(state.user(), &before);
    assert_eq!(state.pending_user_state_changes().unwrap(), changes);
}

#[test]
fn client_user_state_write_results_are_atomic_for_invalid_revision_or_conflict() {
    let root_function = FunctionId::from_bytes([0x41; 16]);
    let function = FunctionId::from_bytes([0x42; 16]);
    let value_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let context = super::ClientStateContext::new(
        root_function,
        "profile".to_owned(),
        "root-instance".to_owned(),
    )
    .unwrap();
    let first_slot = StateSlotId::from_bytes([0x43; 16]);
    let second_slot = StateSlotId::from_bytes([0x44; 16]);
    let principal = PrincipalId::from_bytes([0x45; 16]);
    let first_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        first_slot,
    )
    .unwrap();
    let second_key = UserStateKey::new(
        principal,
        root_function,
        "profile".to_owned(),
        function,
        "root-instance".to_owned(),
        second_slot,
    )
    .unwrap();
    let first_client_key = super::ClientStateKey::from_context(&context, function, first_slot);
    let second_client_key = super::ClientStateKey::from_context(&context, function, second_slot);
    let mut state = super::ClientStateStore::new();
    state.set_context(context);
    state
        .load_user_state(&[
            UserStateCell::new(
                first_key,
                RuntimeValue::Text("first-loaded".to_owned()),
                value_type,
                7,
                SystemTime::UNIX_EPOCH,
            ),
            UserStateCell::new(
                second_key,
                RuntimeValue::Text("second-loaded".to_owned()),
                value_type,
                11,
                SystemTime::UNIX_EPOCH,
            ),
        ])
        .unwrap();
    state
        .set_user_state(
            first_client_key.clone(),
            RuntimeValue::Text("first-changed".to_owned()),
            value_type,
        )
        .unwrap();
    state
        .set_user_state(
            second_client_key.clone(),
            RuntimeValue::Text("second-changed".to_owned()),
            value_type,
        )
        .unwrap();
    let changes = state.pending_user_state_changes().unwrap();
    assert_eq!(changes.len(), 2);
    let before = state.user().clone();
    let results = vec![
        UserStateWriteResult::new(
            changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 8 },
        ),
        UserStateWriteResult::new(
            changes[1].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 0 },
        ),
    ];

    let error = state
        .apply_user_state_write_results(&changes, &results)
        .unwrap_err();

    assert!(matches!(
        error,
        super::ClientUserStateError::InvalidRevision(_)
    ));
    assert_eq!(state.user(), &before);

    let mixed_results = vec![
        UserStateWriteResult::new(
            changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 8 },
        ),
        UserStateWriteResult::new(
            changes[1].key_without_principal(),
            UserStateWriteOutcome::Conflict {
                current_revision: 15,
            },
        ),
    ];
    let mixed_error = state
        .apply_user_state_write_results(&changes, &mixed_results)
        .unwrap_err();

    assert!(matches!(
        mixed_error,
        super::ClientUserStateError::Conflict {
            key,
            expected: Some(11),
            current: 15,
        } if key == second_client_key
    ));
    assert_eq!(state.user(), &before);

    let duplicate_changes = vec![changes[1].clone(), changes[1].clone()];
    let duplicate_results = vec![
        UserStateWriteResult::new(
            duplicate_changes[0].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 16 },
        ),
        UserStateWriteResult::new(
            duplicate_changes[1].key_without_principal(),
            UserStateWriteOutcome::Written { revision: 17 },
        ),
    ];
    let duplicate_error = state
        .apply_user_state_write_results(&duplicate_changes, &duplicate_results)
        .unwrap_err();

    assert!(matches!(
        duplicate_error,
        super::ClientUserStateError::DuplicateKey(key) if key == super::ClientStateKey::from_user_change(&changes[1])
    ));
    assert_eq!(state.user(), &before);
}

#[test]
fn version_four_state_default_type_mismatch_fails_closed() {
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![
            orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x30; 16]),
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "must-not-commit".to_owned(),
                    },
                ),
            ),
            orna_artifact::client_plan::StateSlot::new(
                StateSlotId::from_bytes([0x31; 16]),
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "not-a-boolean".to_owned(),
                    },
                ),
            ),
        ],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::ClientStateStore::new();

    let error = super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();
    assert!(state.local().is_empty());

    assert!(matches!(
        &error,
        super::ClientExecutionError::StateEvaluation {
            context,
            source: super::ClientStateError::DefaultTypeMismatch { slot },
        } if context.function() == function
            && *slot == StateSlotId::from_bytes([0x31; 16])
    ));
}

#[test]
fn version_four_state_initializer_stages_all_scopes_before_commit() {
    let local_slot = StateSlotId::from_bytes([0x30; 16]);
    let session_slot = StateSlotId::from_bytes([0x31; 16]);
    let user_slot = StateSlotId::from_bytes([0x32; 16]);
    let invalid_slot = StateSlotId::from_bytes([0x33; 16]);
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        vec![
            orna_artifact::client_plan::StateSlot::new(
                local_slot,
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "must-not-commit-local".to_owned(),
                    },
                ),
            ),
            orna_artifact::client_plan::StateSlot::new(
                session_slot,
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::StateScope::Session,
                orna_artifact::client_plan::StateDefault::Null,
            ),
            orna_artifact::client_plan::StateSlot::new(
                user_slot,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::User,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
                ),
            ),
            orna_artifact::client_plan::StateSlot::new(
                invalid_slot,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Expression(
                    orna_artifact::client_plan::ClientExpressionNode::String {
                        value: "not-a-boolean".to_owned(),
                    },
                ),
            ),
        ],
    );
    let (active, function, pair, function_revision) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let execution_context = super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0x34; 16]),
        observer_lineage: None,
    };
    let state_context = super::ClientStateContext::new(
        function,
        "atomic-profile".to_owned(),
        "atomic-instance".to_owned(),
    )
    .unwrap();
    let mut state = super::ClientStateStore::new();
    state.set_context(state_context);
    let before = state.clone();
    let mut executor: Option<&mut dyn super::ClientResourceExecutor> = None;
    let mut local_environment = super::ClientLocalEnvironment::new();
    let mut fuel = super::ClientExecutionFuel::new();

    let error = super::initialize_client_state(
        &active,
        &plan,
        execution_context,
        super::ObserverLineage::top_level(execution_context.parent_invocation_id()),
        &[],
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x35; 16]),
        &mut executor,
        &mut local_environment,
        &mut fuel,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::ClientExecutionError::StateEvaluation {
            context,
            source: super::ClientStateError::DefaultTypeMismatch { slot },
        } if context == execution_context && slot == invalid_slot
    ));
    assert_eq!(state, before);
}

#[test]
fn version_four_supported_scalar_slot_types_initialise() {
    for type_id in [
        orna_standard::BIGINT_TYPE_ID,
        orna_standard::FLOAT_TYPE_ID,
        orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
    ] {
        let slot_id = StateSlotId::from_bytes(type_id.to_bytes());
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                slot_id,
                type_id,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Null,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::ClientStateStore::new();

        let result = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .expect("supported scalar state slot initialises");

        assert_eq!(result.value(), &RuntimeValue::Boolean(true));
        assert_eq!(
            state
                .local()
                .get(&super::ClientStateKey::new(function, slot_id)),
            Some(
                &RuntimeValue::null(ResolvedType::value(type_id))
                    .expect("supported scalar null constructs"),
            ),
        );
    }
}

#[test]
fn version_four_unsupported_slot_type_fails_closed() {
    for type_id in [
        orna_standard::DATE_TYPE_ID,
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
    ] {
        let slot_id = StateSlotId::from_bytes(type_id.to_bytes());
        let plan = orna_artifact::client_plan::StateClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
            vec![orna_artifact::client_plan::StateSlot::new(
                slot_id,
                type_id,
                orna_artifact::client_plan::StateScope::Local,
                orna_artifact::client_plan::StateDefault::Unset,
            )],
        );
        let (active, function, _, _) = version_four_state_active(
            orna_standard::BOOLEAN_TYPE_ID,
            plan.encode().expect("the state plan encodes"),
        );
        let mut state = super::ClientStateStore::new();

        let error = super::evaluate_client_function_with_state(
            &active,
            &authorise(active.pair(), function),
            &mut state,
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            super::ClientExecutionError::StateEvaluation {
                context,
                source: super::ClientStateError::UnsupportedSlotType { slot },
            } if context.function() == function && *slot == slot_id
        ));
    }
}

#[test]
fn opaque_value_with_scalar_contract_is_not_a_supported_state_slot_type() {
    let definition = ValueTypeDefinition::opaque(
        TypeId::from_bytes([0xf2; 16]),
        QualifiedSemanticName::new(["tests", "opaque_scalar"]).unwrap(),
        "orna.kernel.value.boolean@1",
    );

    assert!(!super::state_slot_type_is_supported(&definition));
}

#[test]
fn version_four_return_type_mismatch_fails_as_an_expression_error() {
    let plan = orna_artifact::client_plan::StateClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Integer { value: 42 },
        vec![orna_artifact::client_plan::StateSlot::new(
            StateSlotId::from_bytes([0x51; 16]),
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::StateScope::Local,
            orna_artifact::client_plan::StateDefault::Unset,
        )],
    );
    let (active, function, _, _) = version_four_state_active(
        orna_standard::BOOLEAN_TYPE_ID,
        plan.encode().expect("the state plan encodes"),
    );
    let mut state = super::ClientStateStore::new();

    let error = super::evaluate_client_function_with_state(
        &active,
        &authorise(active.pair(), function),
        &mut state,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::ClientExpressionError::TypeMismatch,
        } if context.function() == function
    ));
}

#[test]
fn version_four_plans_run_through_the_legacy_entry_point_with_transient_state() {
    let (active, function, _) = version_four_text_state_plan();

    let result = evaluate_client_function(&active, function).unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("hello world".to_owned())
    );
}

#[test]
fn procedural_literals_and_assignments_use_declaration_locals() {
    let local = LocalId::from_bytes([0xc1; 16]);
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            text_type,
            orna_artifact::client_plan::ClientLocalKind::Value,
        )],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::String {
                    value: "first".to_owned(),
                },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                local,
                orna_artifact::client_plan::ClientExpressionNode::String {
                    value: "second".to_owned(),
                },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, _) = version_five_expression_active_with_parameter(payload);
    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xb1; 16]),
        RuntimeValue::Text("/tmp".to_owned()),
    )
    .unwrap();
    let result = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap();
    assert_eq!(result.value(), &RuntimeValue::Text("second".to_owned()));
}

#[test]
fn resource_request_rejects_missing_target_arguments() {
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Boolean(
            orna_artifact::client_plan::ClientPlan::return_boolean(false),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, _, _, _, _) = version_five_expression_active_with_parameter(payload);
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair),
        PrincipalId::from_bytes([0x71; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = super::ClientResource::new(key, ResolvedType::Value(text_type));

    let error = resource.begin_request(&active, Vec::new()).unwrap_err();

    assert!(matches!(
        error,
        super::ClientResourceError::MissingArgument { parameter }
            if parameter == ParameterId::from_bytes([0xd3; 16])
    ));
}

#[test]
fn resource_request_rejects_unknown_target_arguments_before_loading() {
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Boolean(
            orna_artifact::client_plan::ClientPlan::return_boolean(false),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, _, _, _, _) = version_five_expression_active_with_parameter(payload);
    let digest = super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::ClientResourceKey::new(
        InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair),
        PrincipalId::from_bytes([0x71; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = super::ClientResource::new(key, ResolvedType::Value(text_type));
    let parameter = ParameterId::from_bytes([0xde; 16]);
    let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();

    let error = resource.begin_request(&active, vec![argument]).unwrap_err();

    assert!(matches!(
        error,
        super::ClientResourceError::UnknownArgument { parameter: actual }
            if actual == parameter
    ));
    assert_eq!(resource.status(), super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 0);
}

#[test]
fn procedural_await_without_executor_fails_closed() {
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        FunctionId::from_bytes([0xd1; 16]),
        pair,
        orna_core::CallSiteId::from_bytes([8; 16]),
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        text_type,
    );
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        Vec::new(),
        Vec::new(),
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
                operation,
            }),
        },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, _) = version_five_expression_active_with_parameter(payload);
    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xb1; 16]),
        RuntimeValue::Text("/tmp".to_owned()),
    )
    .unwrap();
    let error = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::ExecutorUnavailable,
            ..
        }
    ));
}

#[test]
fn procedural_scalar_resource_local_awaits_through_assignment_with_executor_value() {
    let local = LocalId::from_bytes([0xc2; 16]);
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let target_revision = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let target = FunctionId::from_bytes([0xd1; 16]);
    let parent_invocation_id = orna_core::InvocationId::from_bytes([0x91; 16]);
    let call_site_id = orna_core::CallSiteId::from_bytes([0x82; 16]);
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        target,
        target_revision,
        call_site_id,
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        text_type,
    );
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            text_type,
            orna_artifact::client_plan::ClientLocalKind::Resource(
                orna_artifact::client_plan::ResourceKind::Scalar,
            ),
        )],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::Resource {
                    operation: operation.clone(),
                },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                local,
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();
    let state_context =
        super::ClientStateContext::new(function, "profile-a".to_owned(), "instance-a".to_owned())
            .unwrap();
    let mut state = super::ClientStateStore::new();
    state.set_context(state_context);
    let mut executor = super::DeterministicClientResourceExecutor::new(
        |request: &super::ClientResourceRequest| {
            assert_eq!(
                request.invocation_context(),
                Some(super::ClientResourceInvocationContext::new(
                    parent_invocation_id,
                    call_site_id,
                    "profile-a".to_owned(),
                    "instance-a".to_owned(),
                )),
            );
            assert_eq!(request.key().target(), InvocationTarget::new(target, pair));
            assert_eq!(request.arguments().len(), 1);
            assert_eq!(
                request.arguments()[0].parameter(),
                ParameterId::from_bytes([0xd3; 16]),
            );
            assert_eq!(
                request.arguments()[0].value(),
                &RuntimeValue::Text("/tmp".to_owned()),
            );
            Ok(RuntimeValue::Text("executor-value".to_owned()))
        },
    );

    let result = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &state.context().clone(),
            &[argument],
            &[],
            &grants,
            &mut state,
            parent_invocation_id,
            &mut executor,
        )
        .unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("executor-value".to_owned())
    );
}

#[test]
fn evaluator_resource_key_includes_host_data_invalidation_token() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/data-token".to_owned())).unwrap();
    let context_a = super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x11; 32]),
    )
    .unwrap();
    let context_b = super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x12; 32]),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = RecordingActionExecutor::new(None);
    let pending_key = |error: super::ClientExecutionError| match error {
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected pending resource evaluation, got {other:?}"),
    };
    let key_a = pending_key(
            super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorise(pair, function),
                &context_a,
                std::slice::from_ref(&argument),
                &[],
                &grants,
                &mut state,
                InvocationId::from_bytes([0x31; 16]),
                &mut executor,
            )
            .unwrap_err(),
        );
    let key_b = pending_key(
            super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorise(pair, function),
                &context_b,
                std::slice::from_ref(&argument),
                &[],
                &grants,
                &mut state,
                InvocationId::from_bytes([0x32; 16]),
                &mut executor,
            )
            .unwrap_err(),
        );

    assert_ne!(
        key_a, key_b,
        "host data invalidation must select a new local key"
    );
    assert_eq!(
        executor.cancelled.len(),
        1,
        "the old loading generation is cancelled"
    );
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Idle)
    );
    assert_eq!(
        state.resource(key_b).map(ClientResource::status),
        Some(ClientResourceStatus::Loading)
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
}

#[test]
fn evaluator_resource_key_includes_state_context_identity() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/state-context".to_owned()),
    )
    .unwrap();
    let context_a =
        super::ClientStateContext::new(function, "profile-a".to_owned(), "instance-a".to_owned())
            .unwrap();
    let context_b = super::ClientStateContext::new(
        FunctionId::from_bytes([0xa1; 16]),
        "profile-b".to_owned(),
        "instance-b".to_owned(),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor_a =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("context-a".to_owned())));
    let result_a = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context_a,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xa2; 16]),
            &mut executor_a,
        )
        .unwrap();
    let key_a = executor_a.executed[0].key();
    assert_eq!(
        result_a.value(),
        &RuntimeValue::Text("context-a".to_owned())
    );
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let mut executor_b =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("context-b".to_owned())));
    let result_b = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context_b,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xa3; 16]),
            &mut executor_b,
        )
        .unwrap();
    let key_b = executor_b.executed[0].key();

    assert_ne!(
        key_a, key_b,
        "state context switch must select a new local key"
    );
    assert_eq!(
        executor_b.executed.len(),
        1,
        "the READY result must not be reused"
    );
    assert_eq!(
        result_b.value(),
        &RuntimeValue::Text("context-b".to_owned())
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());
}

#[test]
fn evaluator_resource_key_changes_after_user_state_mutation() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/user-state".to_owned())).unwrap();
    let context =
        super::ClientStateContext::new(function, "profile".to_owned(), "instance".to_owned())
            .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor_a =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("before".to_owned())));
    let result_a = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb1; 16]),
            &mut executor_a,
        )
        .unwrap();
    let key_a = executor_a.executed[0].key();
    assert_eq!(result_a.value(), &RuntimeValue::Text("before".to_owned()));
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let user_key = super::ClientStateKey::from_context(
        &context,
        function,
        StateSlotId::from_bytes([0xb2; 16]),
    );
    state
        .set_user_state(
            user_key,
            RuntimeValue::Text("changed".to_owned()),
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        )
        .unwrap();

    let mut executor_b = RecordingActionExecutor::new(Some(RuntimeValue::Text("after".to_owned())));
    let result_b = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb3; 16]),
            &mut executor_b,
        )
        .unwrap();
    let key_b = executor_b.executed[0].key();

    assert_ne!(
        key_a, key_b,
        "USER state mutation must select a new local key"
    );
    assert_eq!(
        executor_b.executed.len(),
        1,
        "the READY result must not be reused"
    );
    assert_eq!(result_b.value(), &RuntimeValue::Text("after".to_owned()));
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());
}

#[test]
fn evaluator_resource_key_includes_authorised_security_context() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let (direct_authorisation, role_authorisation) = authorise_with_role_context(pair, function);
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/security-context".to_owned()),
    )
    .unwrap();
    let context = super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x21; 32]),
    )
    .unwrap();

    // A changed security context cannot reuse a READY value.
    let mut ready_state = ClientStateStore::new();
    let mut ready_executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("direct".to_owned())));
    let direct_result = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &direct_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut ready_state,
            InvocationId::from_bytes([0x41; 16]),
            &mut ready_executor,
        )
        .unwrap();
    let key_a = ready_executor.executed[0].key();
    assert_eq!(
        direct_result.value(),
        &RuntimeValue::Text("direct".to_owned())
    );
    assert_eq!(
        ready_state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let mut role_executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("role".to_owned())));
    let role_result = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &role_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut ready_state,
            InvocationId::from_bytes([0x42; 16]),
            &mut role_executor,
        )
        .unwrap();
    let key_b = role_executor.executed[0].key();
    assert_ne!(key_a, key_b, "security context must select a new local key");
    assert_eq!(role_result.value(), &RuntimeValue::Text("role".to_owned()));
    assert_eq!(
        ready_state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
    assert_eq!(
        ready_state.resource(key_b).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());

    // The same security change also replaces an old loading generation and
    // routes cancellation through the caller-owned executor.
    let mut loading_state = ClientStateStore::new();
    let mut loading_executor = RecordingActionExecutor::new(None);
    let direct_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &direct_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut loading_state,
            InvocationId::from_bytes([0x43; 16]),
            &mut loading_executor,
        )
        .unwrap_err();
    let loading_key_a = match direct_error {
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected pending direct resource, got {other:?}"),
    };
    let role_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &role_authorisation,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut loading_state,
            InvocationId::from_bytes([0x44; 16]),
            &mut loading_executor,
        )
        .unwrap_err();
    let loading_key_b = match role_error {
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected pending role resource, got {other:?}"),
    };
    assert_ne!(loading_key_a, loading_key_b);
    assert_eq!(loading_executor.cancelled.len(), 1);
    assert_eq!(
        loading_state
            .resource(loading_key_a)
            .map(ClientResource::status),
        Some(ClientResourceStatus::Idle)
    );
    assert_eq!(
        loading_state
            .resource(loading_key_b)
            .map(ClientResource::status),
        Some(ClientResourceStatus::Loading)
    );
}

#[test]
fn evaluator_resource_key_includes_security_snapshot_grants_without_reusing_ready() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let session_principal = PrincipalId::from_bytes([0x7a; 16]);
    let role = PrincipalId::from_bytes([0x7b; 16]);
    let principals = vec![
        Principal::new(
            session_principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        ),
        Principal::new(role, PrincipalKind::Role, PrincipalStatus::Active),
    ];
    let memberships = vec![RoleMembership::new(role, session_principal)];
    let authorise_with_grants = |execute_grants| {
        let snapshot = SecuritySnapshot::new(
            pair,
            vec![function],
            principals.clone(),
            memberships.clone(),
            execute_grants,
        )
        .expect("security snapshot should validate");
        let session = snapshot
            .bind_authenticated_session(session_principal, vec![role])
            .expect("security session should bind");
        let ExecuteDecision::Allowed(authorisation) =
            snapshot.authorise_execute(&session, InvocationTarget::new(function, pair))
        else {
            panic!("direct grant should allow the function");
        };
        authorisation
    };
    let authorisation_a =
        authorise_with_grants(vec![ExecuteGrant::new(session_principal, function)]);
    let authorisation_b = authorise_with_grants(vec![
        ExecuteGrant::new(session_principal, function),
        ExecuteGrant::new(role, function),
    ]);

    assert_eq!(
        authorisation_a.session_principal(),
        authorisation_b.session_principal()
    );
    assert_eq!(
        authorisation_a.effective_principal(),
        authorisation_b.effective_principal()
    );
    assert_eq!(
        authorisation_a.authorising_principal(),
        authorisation_b.authorising_principal()
    );
    assert_eq!(
        authorisation_a.active_roles(),
        authorisation_b.active_roles()
    );
    assert_eq!(authorisation_a.target(), authorisation_b.target());

    let capabilities =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/security-snapshot-grants".to_owned()),
    )
    .unwrap();
    let context = super::ClientStateContext::new_with_data_invalidation_token(
        function,
        "profile".to_owned(),
        "instance".to_owned(),
        Sha256Digest::from_bytes([0x21; 32]),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor_a =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("snapshot-a".to_owned())));
    let result_a = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorisation_a,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &capabilities,
            &mut state,
            InvocationId::from_bytes([0x61; 16]),
            &mut executor_a,
        )
        .unwrap();
    let key_a = executor_a.executed[0].key();
    assert_eq!(
        result_a.value(),
        &RuntimeValue::Text("snapshot-a".to_owned())
    );
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );

    let mut executor_b =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("snapshot-b".to_owned())));
    let result_b = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorisation_b,
            &context,
            std::slice::from_ref(&argument),
            &[],
            &capabilities,
            &mut state,
            InvocationId::from_bytes([0x62; 16]),
            &mut executor_b,
        )
        .unwrap();
    let key_b = executor_b.executed[0].key();

    assert_ne!(key_a.invalidation_token(), key_b.invalidation_token());
    assert_ne!(
        key_a, key_b,
        "snapshot grant changes must select a new local key"
    );
    assert_eq!(
        executor_b.executed.len(),
        1,
        "the READY result must not be reused"
    );
    assert_eq!(
        result_b.value(),
        &RuntimeValue::Text("snapshot-b".to_owned())
    );
    assert_eq!(key_a.target(), key_b.target());
    assert_eq!(key_a.arguments_digest(), key_b.arguments_digest());
    assert_eq!(
        state.resource(key_a).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
    assert_eq!(
        state.resource(key_b).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
}

#[test]
fn ordinary_resource_pending_persists_only_the_loading_resource() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let state_context = super::ClientStateContext::new(
        FunctionId::from_bytes([0xa1; 16]),
        "profile".to_owned(),
        "instance".to_owned(),
    )
    .unwrap();
    let local_key = super::ClientStateKey::from_context(
        &state_context,
        function,
        StateSlotId::from_bytes([0xa2; 16]),
    );
    let session_key = super::ClientStateKey::from_context(
        &state_context,
        function,
        StateSlotId::from_bytes([0xa3; 16]),
    );
    let user_key = UserStateKey::new(
        principal,
        state_context.root_function(),
        state_context.state_profile().to_owned(),
        function,
        state_context.instance_key().to_owned(),
        StateSlotId::from_bytes([0xa4; 16]),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    state.set_context(state_context.clone());
    state
        .local_mut()
        .insert(local_key, RuntimeValue::Text("local".to_owned()));
    state
        .session_mut()
        .insert(session_key, RuntimeValue::Text("session".to_owned()));
    state
        .load_user_state(&[UserStateCell::new(
            user_key,
            RuntimeValue::Text("user".to_owned()),
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            1,
            SystemTime::UNIX_EPOCH,
        )])
        .unwrap();
    let prior_context = state.context().clone();
    let prior_local = state.local().clone();
    let prior_session = state.session().clone();
    let prior_user = state.user().clone();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp/pending").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/pending".to_owned())).unwrap();
    let mut executor = RecordingActionExecutor::new(None);

    let error = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &[argument],
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x91; 16]),
            &mut executor,
        )
        .unwrap_err();
    let (key, generation) = match error {
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Pending { key, generation },
            ..
        } => (key, generation),
        other => panic!("expected Pending resource evaluation, got {other:?}"),
    };

    assert_eq!(state.context(), &prior_context);
    assert_eq!(state.local(), &prior_local);
    assert_eq!(state.session(), &prior_session);
    assert_eq!(state.user(), &prior_user);
    let resource = state
        .resource(key)
        .expect("pending resource remains in caller state");
    let request_id = resource
        .request_id()
        .expect("pending resource has request identity");
    assert_eq!(resource.key(), key);
    assert_eq!(resource.generation(), generation);
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
    state
        .resource_mut(key)
        .expect("pending resource remains mutable in caller state")
        .apply_completion(
            &active,
            ClientResourceCompletion::Ready {
                request_id,
                key,
                generation,
                value: RuntimeValue::Text("resumed".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        state.resource(key).map(ClientResource::status),
        Some(ClientResourceStatus::Ready),
    );
}
#[test]
fn terminal_resource_states_persist_when_evaluation_fails() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/resource".to_owned())).unwrap();

    let mut failed_state = ClientStateStore::new();
    let mut failing_executor = FailingActionExecutor::default();
    let failure = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut failed_state,
            InvocationId::from_bytes([0x92; 16]),
            &mut failing_executor,
        )
        .unwrap_err();

    assert!(matches!(
        failure,
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Failed(code),
            ..
        } if code == "secret.executor.detail"
    ));
    let failed_request = failing_executor
        .request
        .as_ref()
        .expect("failing executor received a resource request");
    let failed_resource = failed_state
        .resource(failed_request.key())
        .expect("failed resource remains at the evaluated request key");
    assert_eq!(failed_resource.key(), failed_request.key());
    assert_eq!(failed_resource.generation(), failed_request.generation());
    assert_eq!(
        failed_resource.request_id(),
        Some(failed_request.request_id()),
    );
    assert_eq!(failed_resource.status(), ClientResourceStatus::Failed);
    assert_eq!(
        failed_resource
            .failure()
            .map(super::ClientResourceFailure::code),
        Some("secret.executor.detail"),
    );

    let mut cancelled_state = ClientStateStore::new();
    let mut cancelled_executor = CancelledActionExecutor::default();
    let cancellation = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut cancelled_state,
            InvocationId::from_bytes([0x93; 16]),
            &mut cancelled_executor,
        )
        .unwrap_err();

    assert!(matches!(
        cancellation,
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Cancelled,
            ..
        }
    ));
    let cancelled_request = cancelled_executor
        .request
        .as_ref()
        .expect("cancelled executor received a resource request");
    let cancelled_resource = cancelled_state
        .resource(cancelled_request.key())
        .expect("cancelled resource remains at the evaluated request key");
    assert_eq!(cancelled_resource.key(), cancelled_request.key());
    assert_eq!(
        cancelled_resource.generation(),
        cancelled_request.generation()
    );
    assert_eq!(
        cancelled_resource.request_id(),
        Some(cancelled_request.request_id()),
    );
    assert_eq!(cancelled_resource.status(), ClientResourceStatus::Cancelled);
    assert_eq!(cancelled_resource.failure(), None);
}

#[test]
fn same_revision_terminal_replacement_persists_when_new_evaluation_fails() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/replacement".to_owned()))
            .unwrap();
    let context = |token| {
        super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([token; 32]),
        )
        .unwrap()
    };

    for outcome in [
        ReplacementEvaluationOutcome::Pending,
        ReplacementEvaluationOutcome::Failed,
        ReplacementEvaluationOutcome::Invalid,
    ] {
        let mut state = ClientStateStore::new();
        let mut executor = ReplacementTerminalExecutor::new(outcome);
        let first_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorise(pair, function),
                &context(0xa1),
                std::slice::from_ref(&argument),
                &[],
                &grants,
                &mut state,
                InvocationId::from_bytes([0xb1; 16]),
                &mut executor,
            )
            .unwrap_err();
        let old_key = match first_error {
            super::ClientExecutionError::ResourceEvaluation {
                source: super::ClientResourceExecutionError::Pending { key, .. },
                ..
            } => key,
            other => panic!("expected first resource request to remain pending, got {other:?}"),
        };
        assert_eq!(
            state.resource(old_key).map(ClientResource::status),
            Some(ClientResourceStatus::Loading),
        );

        let second_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
                &active,
                &authorise(pair, function),
                &context(0xa2),
                std::slice::from_ref(&argument),
                &[],
                &grants,
                &mut state,
                InvocationId::from_bytes([0xb2; 16]),
                &mut executor,
            )
            .unwrap_err();
        match (outcome, second_error) {
            (
                ReplacementEvaluationOutcome::Pending,
                super::ClientExecutionError::ResourceEvaluation {
                    source: super::ClientResourceExecutionError::Pending { .. },
                    ..
                },
            )
            | (
                ReplacementEvaluationOutcome::Failed,
                super::ClientExecutionError::ResourceEvaluation {
                    source: super::ClientResourceExecutionError::Failed(_),
                    ..
                },
            )
            | (
                ReplacementEvaluationOutcome::Invalid,
                super::ClientExecutionError::ResourceEvaluation {
                    source: super::ClientResourceExecutionError::Invalid(_),
                    ..
                },
            ) => {}
            (ReplacementEvaluationOutcome::Expression, _) => {
                unreachable!("expression outcome is covered by the dedicated regression")
            }
            (outcome, error) => {
                panic!("unexpected replacement evaluation result for {outcome:?}: {error:?}")
            }
        }

        let new_key = executor
            .executed
            .get(1)
            .expect("replacement request was submitted")
            .key();
        assert_ne!(old_key, new_key);
        assert_eq!(executor.cancelled[0].key(), old_key);
        let old_resource = state
            .resource(old_key)
            .expect("same-revision terminal replacement remains cached");
        assert_eq!(old_resource.status(), ClientResourceStatus::Ready);
        assert_eq!(
            old_resource.value(),
            Some(&RuntimeValue::Text("old-terminal".to_owned())),
        );
        match outcome {
            ReplacementEvaluationOutcome::Pending => assert_eq!(
                state.resource(new_key).map(ClientResource::status),
                Some(ClientResourceStatus::Loading),
            ),
            ReplacementEvaluationOutcome::Failed => assert_eq!(
                state.resource(new_key).map(ClientResource::status),
                Some(ClientResourceStatus::Failed),
            ),
            ReplacementEvaluationOutcome::Invalid => assert_eq!(
                state.resource(new_key).map(ClientResource::status),
                Some(ClientResourceStatus::Loading),
            ),
            ReplacementEvaluationOutcome::Expression => {
                unreachable!("expression outcome is covered by the dedicated regression")
            }
        }
    }
}

#[test]
fn same_revision_terminal_replacement_persists_when_later_expression_fails() {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        FunctionId::from_bytes([0xd1; 16]),
        pair,
        CallSiteId::from_bytes([0xe1; 16]),
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let local = LocalId::from_bytes([0xf1; 16]);
    let return_expression = orna_artifact::client_plan::ClientExpressionNode::Concat {
        left: Box::new(orna_artifact::client_plan::ClientExpressionNode::LocalRead { local }),
        right: Box::new(orna_artifact::client_plan::ClientExpressionNode::Integer { value: 7 }),
    };
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
            orna_artifact::client_plan::ClientLocalKind::Value,
        )],
        vec![orna_artifact::client_plan::ClientStatement::let_(
            local,
            orna_artifact::client_plan::ClientExpressionNode::Await {
                expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
                    operation,
                }),
            },
        )],
        return_expression,
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Parameter("p_path".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/later-error".to_owned()))
            .unwrap();
    let context = |token| {
        super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([token; 32]),
        )
        .unwrap()
    };
    let mut state = ClientStateStore::new();
    let mut executor = ReplacementTerminalExecutor::new(ReplacementEvaluationOutcome::Expression);
    let first_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context(0xa3),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb3; 16]),
            &mut executor,
        )
        .unwrap_err();
    let old_key = match first_error {
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Pending { key, .. },
            ..
        } => key,
        other => panic!("expected first resource request to remain pending, got {other:?}"),
    };

    let second_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            &context(0xa4),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb4; 16]),
            &mut executor,
        )
        .unwrap_err();
    assert!(matches!(
        second_error,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
    let new_key = executor
        .executed
        .get(1)
        .expect("replacement request was submitted")
        .key();
    assert_ne!(old_key, new_key);
    assert_eq!(executor.cancelled[0].key(), old_key);
    let old_resource = state
        .resource(old_key)
        .expect("same-revision terminal replacement remains cached");
    assert_eq!(old_resource.status(), ClientResourceStatus::Ready);
    assert_eq!(
        old_resource.value(),
        Some(&RuntimeValue::Text("old-terminal".to_owned())),
    );
    assert!(
        state.resource(new_key).is_none(),
        "failed outer expression must not publish its replacement resource"
    );
}

#[test]
fn stale_revision_replacement_persists_when_later_expression_fails() {
    let old_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let new_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([5; 16]),
        CatalogueRevisionId::from_bytes([6; 16]),
    );
    let payload = |pair| {
        let operation = orna_artifact::client_plan::ResourceOperationNode::new(
            orna_artifact::client_plan::ResourceKind::Scalar,
            FunctionId::from_bytes([0xd1; 16]),
            pair,
            CallSiteId::from_bytes([0xe1; 16]),
            vec![(
                ParameterId::from_bytes([0xd3; 16]),
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: ParameterId::from_bytes([0xb1; 16]),
                },
            )],
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        );
        let local = LocalId::from_bytes([0xf1; 16]);
        let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
            vec![orna_artifact::client_plan::ClientLocal::new(
                local,
                orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
                orna_artifact::client_plan::ClientLocalKind::Value,
            )],
            vec![orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::Await {
                    expression: Box::new(
                        orna_artifact::client_plan::ClientExpressionNode::Resource { operation },
                    ),
                },
            )],
            orna_artifact::client_plan::ClientExpressionNode::Concat {
                left: Box::new(
                    orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
                ),
                right: Box::new(orna_artifact::client_plan::ClientExpressionNode::Integer {
                    value: 7,
                }),
            },
        );
        orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                orna_artifact::client_plan::CapabilityArgumentSource::Parameter(
                    "p_path".to_owned(),
                ),
            )],
        )
        .encode()
        .unwrap()
    };
    let (old_active, function, _, _, parameter) =
        version_five_expression_active_with_parameter(payload(old_pair));
    let (new_base, _, _, _, _) = version_five_expression_active_with_parameter(payload(new_pair));
    let new_active = active_with_revision_pair(&new_base, new_pair);
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/stale-replacement".to_owned()),
    )
    .unwrap();
    let context = |token| {
        super::ClientStateContext::new_with_data_invalidation_token(
            function,
            "profile".to_owned(),
            "instance".to_owned(),
            Sha256Digest::from_bytes([token; 32]),
        )
        .unwrap()
    };
    let mut state = ClientStateStore::new();
    let mut executor = ReplacementTerminalExecutor::new(ReplacementEvaluationOutcome::Expression);
    let first_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &old_active,
            &authorise(old_pair, function),
            &context(0xa5),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb5; 16]),
            &mut executor,
        )
        .unwrap_err();
    let (old_key, old_generation) = match first_error {
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Pending { key, generation },
            ..
        } => (key, generation),
        other => panic!("expected first resource request to remain pending, got {other:?}"),
    };
    let old_request = executor
        .executed
        .first()
        .expect("old request was submitted")
        .clone();
    assert_eq!(old_request.key(), old_key);
    assert_eq!(old_request.generation(), old_generation);

    let second_error = super::evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation(
            &new_active,
            &authorise(new_pair, function),
            &context(0xa6),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0xb6; 16]),
            &mut executor,
        )
        .unwrap_err();
    assert!(matches!(
        second_error,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
    let new_key = executor
        .executed
        .get(1)
        .expect("replacement request was submitted")
        .key();
    assert_ne!(old_key, new_key);
    assert_eq!(executor.cancelled, vec![old_request]);
    let old_resource = state
        .resource(old_key)
        .expect("stale replacement remains cached");
    assert_eq!(old_resource.status(), ClientResourceStatus::Idle);
    assert_eq!(old_resource.value(), None);
    assert_eq!(old_resource.failure(), None);
    assert_eq!(old_resource.request_id(), None);
    assert!(old_resource.generation().value() > old_generation.value());
    assert!(state.resource(new_key).is_none());
}

#[test]
fn malformed_resource_completion_cancels_executor_and_persists_terminal_state() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/malformed".to_owned())).unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = MalformedResourceExecutor::default();

    let error = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x94; 16]),
            &mut executor,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Cancelled,
            ..
        }
    ));

    let request = executor
        .executed
        .clone()
        .expect("malformed executor received a resource request");
    assert_eq!(executor.cancelled, vec![request.clone()]);
    let mut resource = state
        .resource(request.key())
        .expect("cancelled resource remains in caller state")
        .clone();
    assert_eq!(resource.status(), ClientResourceStatus::Cancelled);
    assert_eq!(resource.generation(), request.generation());
    assert!(matches!(
        resource.apply_completion(
            &active,
            request.ready(RuntimeValue::Text("late".to_owned())),
        ),
        Err(super::ClientResourceError::InvalidTransition {
            status: ClientResourceStatus::Cancelled,
        })
    ));
}

#[test]
fn mismatched_request_id_completion_does_not_cancel_request() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/stale-request".to_owned()),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = MalformedResourceExecutor {
        stale_request_id: true,
        ..MalformedResourceExecutor::default()
    };

    let error = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x96; 16]),
            &mut executor,
        )
        .expect_err("a mismatched request ID must not cancel the active request");
    let request = executor
        .executed
        .clone()
        .expect("executor received a resource request");
    assert!(executor.cancelled.is_empty());
    assert!(matches!(
        error,
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::Invalid(_),
            ..
        }
    ));
    let resource = state
        .resource(request.key())
        .expect("the active request remains in caller state");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
}

#[test]
fn malformed_resource_completion_returns_terminal_cancel_result() {
    let (active, function, pair, _, parameter) = version_six_client_resource_action_active();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/tmp/malformed-ready".to_owned()),
    )
    .unwrap();
    let mut state = ClientStateStore::new();
    let mut executor = MalformedResourceExecutor {
        cancel_ready: true,
        ..MalformedResourceExecutor::default()
    };

    let result = super::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &active,
            &authorise(pair, function),
            std::slice::from_ref(&argument),
            &[],
            &grants,
            &mut state,
            InvocationId::from_bytes([0x95; 16]),
            &mut executor,
        )
        .expect("valid terminal cancellation completion wins over malformed execute result");
    assert_eq!(
        result.value(),
        &RuntimeValue::Text("cancelled-ready".to_owned())
    );
    let request = executor
        .executed
        .clone()
        .expect("malformed executor received a resource request");
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(
        state.resource(request.key()).map(ClientResource::status),
        Some(ClientResourceStatus::Ready)
    );
}

#[test]
fn procedural_scalar_resource_local_await_without_executor_fails_closed() {
    let local = LocalId::from_bytes([0xc3; 16]);
    let text_type = orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID;
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        FunctionId::from_bytes([0xd1; 16]),
        pair,
        orna_core::CallSiteId::from_bytes([0x83; 16]),
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        text_type,
    );
    let plan = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            text_type,
            orna_artifact::client_plan::ClientLocalKind::Resource(
                orna_artifact::client_plan::ResourceKind::Scalar,
            ),
        )],
        vec![orna_artifact::client_plan::ClientStatement::let_(
            local,
            orna_artifact::client_plan::ClientExpressionNode::Resource { operation },
        )],
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        },
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = FunctionArgument::new(parameter, RuntimeValue::Text("/tmp".to_owned())).unwrap();

    let error = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::ClientExecutionError::ResourceEvaluation {
            source: super::ClientResourceExecutionError::ExecutorUnavailable,
            ..
        }
    ));
}

#[test]
fn capability_gate_denies_an_ungranted_declared_capability() {
    let (active, function, _, _) = version_one_active(true);
    let grants = super::capability::LocalCapabilityGrantSet::new();
    let declaration = super::capability::LocalCapabilityDeclaration::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityArgumentSource::Text("/home/bob".to_owned()),
    );

    let error = super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), function),
        &[declaration],
        &grants,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == function && capability == "std.fs.read"
    ));
}

#[test]
fn capability_gate_admits_a_granted_declared_capability() {
    let (active, function, pair, _) = version_one_active(true);
    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let declaration = super::capability::LocalCapabilityDeclaration::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    );

    let result = super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[declaration],
        &grants,
    )
    .unwrap();

    assert_eq!(result.context().function(), function);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn capability_gate_keeps_zero_declaration_functions_unchanged() {
    let (active, function, pair, _) = version_one_active(true);
    let empty_grants = super::capability::LocalCapabilityGrantSet::new();

    let result = super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[],
        &empty_grants,
    )
    .unwrap();

    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn version_five_stored_literal_capability_denies_without_grants() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.fs.read",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    )];
    let (active, function, _, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    let empty_grants = super::capability::LocalCapabilityGrantSet::new();
    // A caller-supplied declaration must never replace the stored
    // requirements of a version-5 envelope.
    let declaration = super::capability::LocalCapabilityDeclaration::new(
        super::capability::LocalCapabilityName::StdSecretUse,
        super::capability::LocalCapabilityArgumentSource::Text("secret-1".to_owned()),
    );

    let error = super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), function),
        &[declaration],
        &empty_grants,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == function && capability == "std.fs.read"
    ));
    assert_eq!(
        error.to_string(),
        "the CLIENT function requires the capability std.fs.read which is not granted"
    );
}

#[test]
fn version_five_artifact_hash_is_checked_before_capability_decode() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.fs.read",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    )];
    let (active, function, pair, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);
    let mut state = ClientStateStore::new();
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = None;

    let error = super::evaluate_function(
        &untrusted,
        function,
        Vec::new(),
        &[],
        &capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x7b; 16]),
        super::ObserverLineage::top_level(InvocationId::from_bytes([0xa2; 16])),
        &mut executor_slot,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidArtifact { context, .. }
            if context.pair() == pair && context.function() == function
    ));
}

#[test]
fn version_five_stored_literal_capability_evaluates_when_covered() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.fs.read",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    )];
    let (active, function, pair, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

    let result = super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[],
        &grants,
    )
    .unwrap();

    assert_eq!(result.context().function(), function);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn version_five_unknown_stored_capability_name_fails_closed() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.bogus.op",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("anything".to_owned()),
    )];
    let (active, function, _, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    // Every vocabulary grant present: the unknown stored name still fails
    // closed and never falls back to an empty requirement set.
    let grants = super::capability::LocalCapabilityGrantSet::from_grants(
        super::capability::LocalCapabilityName::ALL
            .into_iter()
            .map(|name| {
                let scope = match name {
                    super::capability::LocalCapabilityName::StdFsRead
                    | super::capability::LocalCapabilityName::StdFsWrite => {
                        super::capability::LocalCapabilityScope::path("/home/bob").unwrap()
                    }
                    super::capability::LocalCapabilityName::StdNetConnect => {
                        super::capability::LocalCapabilityScope::host("example.com").unwrap()
                    }
                    super::capability::LocalCapabilityName::StdSecretUse => {
                        super::capability::LocalCapabilityScope::secret("secret-1").unwrap()
                    }
                };
                super::capability::LocalCapabilityGrant::new(name, scope).unwrap()
            }),
    )
    .unwrap();

    let error = super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), function),
        &[],
        &grants,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == function && capability == "std.bogus.op"
    ));
}

#[test]
fn version_five_stored_parameter_capability_resolves_the_invocation_argument() {
    let parameter_id = ParameterId::from_bytes([0xb1; 16]);
    let plan = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(
            orna_artifact::client_plan::ExpressionClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: parameter_id,
                },
            ),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Parameter("p_path".to_owned()),
        )],
    );
    let (active, function, pair, _, _) =
        version_five_expression_active_with_parameter(plan.encode().unwrap());
    let argument = orna_core::value::FunctionArgument::new(
        parameter_id,
        RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
    )
    .unwrap();

    let result = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
    )
    .unwrap_err();

    assert!(matches!(
        &result,
        super::ClientExecutionError::CapabilityDenied { capability, .. }
            if capability == "std.fs.read"
    ));

    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = orna_core::value::FunctionArgument::new(
        parameter_id,
        RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
    )
    .unwrap();

    let result = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("/home/bob/notes.txt".to_owned())
    );
}

#[test]
fn version_five_recursive_calls_enforce_the_callee_capability() {
    let (base, caller_id, pair, caller_revision_id) = version_one_active(true);
    let callee_id = FunctionId::from_bytes([0xc2; 16]);
    let callee_revision_id = FunctionRevisionId::from_bytes([0xc3; 16]);
    let previous_revision = &base.function_revisions()[0];
    let caller_name = base
        .catalogue()
        .function_by_id(caller_id)
        .unwrap()
        .name()
        .clone();
    let caller_plan = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Call {
            function: callee_id,
            arguments: Vec::new(),
        },
    );
    let caller_payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(caller_plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.write",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let callee_plan = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
    );
    let callee_payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(callee_plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let caller = FunctionDefinition::new(
        caller_id,
        caller_name,
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
        caller_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let callee = FunctionDefinition::new(
        callee_id,
        QualifiedSemanticName::new(["app", "callee"]).unwrap(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
        callee_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        base.catalogue().object_types().to_vec(),
        vec![caller.clone(), callee.clone()],
    )
    .unwrap();
    let caller_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        caller_payload.clone(),
        artifact_payload_digest(&caller_payload).unwrap(),
    )
    .unwrap();
    let callee_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        callee_payload.clone(),
        artifact_payload_digest(&callee_payload).unwrap(),
    )
    .unwrap();
    let caller_reference = DefinitionReference::new(
        caller_id,
        caller_revision_id,
        0,
        DefinitionReferenceTarget::Function(callee_id),
        DefinitionReferenceKind::FunctionCall,
        previous_revision.declaration_origin(),
    );
    let caller_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &caller,
        previous_revision.language_version(),
        &caller_artifact,
        base.expressions(),
        std::slice::from_ref(&caller_reference),
    )
    .unwrap();
    let callee_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &callee,
        previous_revision.language_version(),
        &callee_artifact,
        base.expressions(),
        &[],
    )
    .unwrap();
    let caller_revision = FunctionRevisionRecord::new(
        caller_id,
        caller_revision_id,
        previous_revision.revision_number(),
        previous_revision.declaration_origin(),
        previous_revision.declaration_content_hash(),
        caller_semantic_hash,
        previous_revision.language_version(),
        caller_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let callee_origin = SourceOrigin::new(
        previous_revision.declaration_origin().source_unit(),
        previous_revision.declaration_origin().byte_start(),
        previous_revision.declaration_origin().byte_end(),
    )
    .unwrap();
    let callee_revision = FunctionRevisionRecord::new(
        callee_id,
        callee_revision_id,
        previous_revision.revision_number(),
        callee_origin,
        previous_revision.declaration_content_hash(),
        callee_semantic_hash,
        previous_revision.language_version(),
        callee_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let mut origins = base.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(callee_id),
        callee_origin,
    ));
    let revisions = vec![caller_revision, callee_revision];
    let references = vec![caller_reference];
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let context = orna_core::revision::CatalogueHashContext::version_two(standard);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        &revisions,
        base.expressions(),
        &origins,
        &references,
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            base.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(base.expressions().to_vec(), revisions, origins, references),
        ),
        context,
    )
    .unwrap();
    let write_grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsWrite,
        super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let write_only =
        super::capability::LocalCapabilityGrantSet::from_grants([write_grant]).unwrap();
    let error = super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, caller_id),
        &[],
        &write_only,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == callee_id && capability == "std.fs.read"
    ));
    let read_grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants(
        write_only
            .as_slice()
            .iter()
            .cloned()
            .chain(std::iter::once(read_grant)),
    )
    .unwrap();
    let result = super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, caller_id),
        &[],
        &grants,
    )
    .unwrap();
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn nested_call_preserves_caller_bound_capability_parameter() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
             CREATE CLIENT FUNCTION app.first(p_path TEXT) RETURNS TEXT RETURN app.second(); \
             CREATE CLIENT FUNCTION app.second() RETURNS TEXT RETURN 'ok';",
    );
    let initial = active_from_prepared_candidate(&prepared);
    let caller = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.first")
        .expect("caller is present")
        .clone();
    let callee = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.second")
        .expect("callee is present")
        .clone();
    let parameter = caller
        .parameters()
        .first()
        .expect("caller path parameter is present")
        .id();
    let payload = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Call {
            function: callee.id(),
            arguments: Vec::new(),
        },
    )
    .encode()
    .expect("caller expression plan encodes");
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let current = initial
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .expect("caller revision is present");
    let caller_references = initial
        .references()
        .iter()
        .filter(|reference| reference.source_function() == caller.id())
        .cloned()
        .collect::<Vec<_>>();
    let semantic_hash = function_semantic_digest_with_version(
        current.semantic_hash_version(),
        &caller,
        current.language_version(),
        &artifact,
        initial.expressions(),
        &caller_references,
    )
    .unwrap();
    let replacement = FunctionRevisionRecord::new(
        caller.id(),
        current.id(),
        current.revision_number(),
        current.declaration_origin(),
        current.declaration_content_hash(),
        semantic_hash,
        current.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(current.semantic_hash_version());
    let revisions = initial
        .function_revisions()
        .iter()
        .map(|revision| {
            if revision.function() == caller.id() {
                replacement.clone()
            } else {
                revision.clone()
            }
        })
        .collect::<Vec<_>>();
    let catalogue_hash = catalogue_digest_with_context(
        initial.catalogue_hash_context(),
        initial.catalogue(),
        &revisions,
        initial.expressions(),
        initial.origins(),
        initial.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            initial.pair(),
            initial.source().clone(),
            initial.catalogue().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                initial.expressions().to_vec(),
                revisions,
                initial.origins().to_vec(),
                initial.references().to_vec(),
            ),
        ),
        initial.catalogue_hash_context().clone(),
    )
    .unwrap();
    let declaration = capability::LocalCapabilityDeclaration::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityArgumentSource::Parameter("p_path".to_owned()),
    );
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
    )
    .unwrap();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

    let result = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(active.pair(), caller.id()),
        std::slice::from_ref(&argument),
        std::slice::from_ref(&declaration),
        &grants,
    )
    .expect("caller-scoped capability remains bound in the nested call");
    assert_eq!(result.value(), &RuntimeValue::Text("ok".to_owned()));

    let mismatched_grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let mismatched_grants =
        capability::LocalCapabilityGrantSet::from_grants([mismatched_grant]).unwrap();
    let error = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(active.pair(), caller.id()),
        &[argument],
        &[declaration],
        &mismatched_grants,
    )
    .expect_err("a mismatched caller scope still fails closed");
    assert!(matches!(
        error,
        super::ClientExecutionError::CapabilityDenied { context, capability }
            if context.function() == caller.id() && capability == "std.fs.read"
    ));
}

#[test]
fn expression_calls_reject_targets_absent_from_the_active_reference_set() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
             CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN app.second(); \
             CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN TRUE;",
    );
    let first = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.first")
        .expect("first function is present");
    let second = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.second")
        .expect("second function is present");
    let mut references = prepared.references().to_vec();
    let index = references
        .iter()
        .position(|reference| {
            reference.source_function() == first.id()
                && reference.target() == DefinitionReferenceTarget::Function(second.id())
        })
        .expect("first call reference is present");
    let original = references[index].clone();
    references[index] = DefinitionReference::new(
        original.source_function(),
        original.source_revision(),
        original.ordinal(),
        DefinitionReferenceTarget::Function(first.id()),
        original.kind(),
        original.source_origin(),
    );
    let active = active_from_prepared_with_references(&prepared, references);

    let error = evaluate_client_function(&active, first.id()).unwrap_err();

    assert!(matches!(
        error,
        super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::ClientExpressionError::InvalidCall,
        } if context.function() == first.id()
    ));
}

#[test]
fn client_expression_call_depth_is_bounded_by_artifact_limit() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let context = super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::new(),
        observer_lineage: None,
    };
    let expression = orna_artifact::client_plan::ClientExpressionNode::Call {
        function,
        arguments: Vec::new(),
    };
    let mut state = super::ClientStateStore::new();
    let mut executor: Option<&mut dyn super::ClientResourceExecutor> = None;
    let mut local_environment = super::ClientLocalEnvironment::new();

    let error = super::evaluate_expression(
        &active,
        &expression,
        context,
        super::ObserverLineage::top_level(context.parent_invocation_id()),
        &[],
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1,
        PrincipalId::from_bytes([0x7a; 16]),
        &mut executor,
        &mut local_environment,
    )
    .expect_err("recursive CLIENT calls must stop at the closed depth cap");

    assert!(matches!(
        error,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::RecursionLimit,
            ..
        }
    ));
}

#[test]
fn client_expression_call_depth_accepts_boundary_and_rejects_next_edge() {
    std::thread::Builder::new()
        .name("client-expression-depth-boundary".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let boundary_edges = orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1;
            let (boundary_prepared, boundary_function) =
                prepared_client_call_chain_with_state_root(boundary_edges);
            let boundary_active = active_from_prepared_candidate(&boundary_prepared);
            let mut boundary_state = ClientStateStore::new();

            let boundary_result = super::evaluate_client_function_with_state(
                &boundary_active,
                &authorise(boundary_active.pair(), boundary_function),
                &mut boundary_state,
            )
            .expect("the call at MAX_EXPRESSION_DEPTH must be accepted");
            assert_eq!(boundary_result.value(), &RuntimeValue::Boolean(true));
            assert_eq!(boundary_state.local().len(), 1);

            let (overflow_prepared, overflow_function) =
                prepared_client_call_chain_with_state_root(boundary_edges + 1);
            let overflow_active = active_from_prepared_candidate(&overflow_prepared);
            let mut overflow_state = ClientStateStore::new();
            let state_before_overflow = overflow_state.clone();

            let error = super::evaluate_client_function_with_state(
                &overflow_active,
                &authorise(overflow_active.pair(), overflow_function),
                &mut overflow_state,
            )
            .expect_err("the call after MAX_EXPRESSION_DEPTH must fail closed");

            assert!(matches!(
                error,
                super::ClientExecutionError::ExpressionEvaluation {
                    source: super::ClientExpressionError::RecursionLimit,
                    ..
                }
            ));
            assert_eq!(
                overflow_state, state_before_overflow,
                "a recursion-limit error must not commit staged state or resources"
            );
        })
        .expect("the depth-boundary test thread must start")
        .join()
        .expect("the depth-boundary test thread must complete");
}

#[test]
fn reference_root_field_path_loads_nested_records_under_authenticated_context() {
    let (
        active,
        context,
        parameter,
        outer_type,
        outer_object,
        outer_record,
        inner_object,
        inner_record,
        outer_field,
        inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let mut objects = HashMap::new();
    objects.insert(
        (outer_object, ObjectId::from_bytes([0x31; 16])),
        RuntimeValue::Record(outer_record),
    );
    objects.insert(
        (inner_object, ObjectId::from_bytes([0x32; 16])),
        RuntimeValue::Record(inner_record),
    );
    let mut state = ClientStateStore::new();
    state.set_security_context_digest(super::security_context_digest(&authorisation));
    state.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: super::security_context_digest(&authorisation),
        objects,
    });
    let expression = orna_artifact::client_plan::ClientExpressionNode::FieldPath {
        root: parameter,
        fields: vec![outer_field, inner_field],
    };
    let mut executor: Option<&mut dyn super::ClientResourceExecutor> = None;
    let mut locals = super::ClientLocalEnvironment::new();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target: outer_type,
            object: ObjectId::from_bytes([0x31; 16]),
        },
    )
    .unwrap();

    let value = super::evaluate_expression(
        &active,
        &expression,
        context,
        super::ObserverLineage::top_level(context.parent_invocation_id()),
        &[(argument.parameter(), argument.value().clone())],
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        authorisation.session_principal(),
        &mut executor,
        &mut locals,
    )
    .expect("trusted reference loader resolves nested field paths");

    assert_eq!(value, RuntimeValue::Text("Ada".to_owned()));
    let digest = super::client_security_context_digest(&authorisation);
    let mut host_state = ClientStateStore::new();
    host_state.set_security_context_digest(digest);
    host_state.install_reference_loader(
        ClientReferenceLoader::new(
            active.pair(),
            authorisation.session_principal(),
            digest,
            [
                ClientReferenceObject::new(
                    outer_object,
                    ObjectId::from_bytes([0x31; 16]),
                    vec![(
                        outer_field,
                        RuntimeValue::Reference {
                            target: inner_object,
                            object: ObjectId::from_bytes([0x32; 16]),
                        },
                    )],
                ),
                ClientReferenceObject::new(
                    inner_object,
                    ObjectId::from_bytes([0x32; 16]),
                    vec![(inner_field, RuntimeValue::Text("Ada".to_owned()))],
                ),
            ],
        )
        .unwrap(),
    );
    let mut host_executor: Option<&mut dyn super::ClientResourceExecutor> = None;
    let mut host_locals = super::ClientLocalEnvironment::new();
    let host_value = super::evaluate_expression(
        &active,
        &expression,
        context,
        super::ObserverLineage::top_level(context.parent_invocation_id()),
        &[(argument.parameter(), argument.value().clone())],
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
        &mut host_state,
        0,
        authorisation.session_principal(),
        &mut host_executor,
        &mut host_locals,
    )
    .expect("host-installed reference loader resolves nested field paths");
    assert_eq!(host_value, RuntimeValue::Text("Ada".to_owned()));
    let direct = super::evaluate_field_path(
        &active,
        &RuntimeValue::Record(
            state
                .reference_loader
                .as_ref()
                .unwrap()
                .objects
                .get(&(inner_object, ObjectId::from_bytes([0x32; 16])))
                .and_then(|value| match value {
                    RuntimeValue::Record(record) => Some(record.clone()),
                    _ => None,
                })
                .unwrap(),
        ),
        &[inner_field],
        context,
        authorisation.session_principal(),
        &state,
    )
    .expect("direct record field paths retain their existing behaviour");
    assert_eq!(direct, RuntimeValue::Text("Ada".to_owned()));
}

#[test]
fn client_reference_loader_rejects_duplicate_object_identities() {
    let target = TypeId::from_bytes([0xd1; 16]);
    let object = ObjectId::from_bytes([0xd2; 16]);
    let error = ClientReferenceLoader::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0xd3; 16]),
            CatalogueRevisionId::from_bytes([0xd4; 16]),
        ),
        PrincipalId::from_bytes([0xd5; 16]),
        Sha256Digest::from_bytes([0xd6; 32]),
        [
            ClientReferenceObject::new(target, object, Vec::new()),
            ClientReferenceObject::new(target, object, Vec::new()),
        ],
    )
    .expect_err("duplicate reference-object identities must fail closed");

    assert_eq!(
        error,
        ClientReferenceLoaderError::DuplicateIdentity { target, object }
    );
}

#[test]
fn client_function_arguments_match_requires_exact_ids_and_active_types() {
    let first_id = ParameterId::from_bytes([0xd7; 16]);
    let second_id = ParameterId::from_bytes([0xd8; 16]);
    let unknown_id = ParameterId::from_bytes([0xd9; 16]);
    let (active, function, _pair, _revision) = version_one_active_with_shape(
        FunctionDomain::Client,
        vec![
            ParameterDefinition::new(
                first_id,
                "first",
                0,
                ResolvedType::Scalar(StandardScalar::Integer),
                None,
            ),
            ParameterDefinition::new(
                second_id,
                "second",
                1,
                ResolvedType::Scalar(StandardScalar::Boolean),
                None,
            ),
        ],
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let definition = active
        .catalogue()
        .function_by_id(function)
        .expect("argument matcher fixture function is active");
    let first = FunctionArgument::new(first_id, RuntimeValue::Integer(7)).unwrap();
    let second = FunctionArgument::new(second_id, RuntimeValue::Boolean(true)).unwrap();

    assert!(super::client_function_arguments_match(
        &active,
        definition,
        &[first.clone(), second.clone()],
    ));
    assert!(!super::client_function_arguments_match(
        &active,
        definition,
        std::slice::from_ref(&first),
    ));
    assert!(!super::client_function_arguments_match(
        &active,
        definition,
        &[first.clone(), first.clone()],
    ));
    assert!(!super::client_function_arguments_match(
        &active,
        definition,
        &[
            first.clone(),
            FunctionArgument::new(unknown_id, RuntimeValue::Boolean(true)).unwrap(),
        ],
    ));
    assert!(!super::client_function_arguments_match(
        &active,
        definition,
        &[
            FunctionArgument::new(first_id, RuntimeValue::Boolean(true)).unwrap(),
            second,
        ],
    ));
}

#[test]
fn host_reference_loader_accepts_partial_fields_but_missing_requested_field_fails() {
    let (
        active,
        context,
        _parameter,
        outer_type,
        outer_object,
        _outer_record,
        _inner_object,
        _inner_record,
        outer_field,
        _inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let object = ObjectId::from_bytes([0x31; 16]);
    let digest = super::client_security_context_digest(&authorisation);
    let partial = ClientReferenceObject::new(outer_object, object, Vec::new());

    assert!(super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &partial,
    ));
    assert!(!super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &ClientReferenceObject::new(
            outer_object,
            object,
            vec![(
                FieldId::from_bytes([0xff; 16]),
                RuntimeValue::Reference {
                    target: outer_type,
                    object,
                },
            )],
        ),
    ));
    let field_value = RuntimeValue::Reference {
        target: _inner_object,
        object: ObjectId::from_bytes([0x32; 16]),
    };
    assert!(!super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &ClientReferenceObject::new(
            outer_object,
            object,
            vec![
                (outer_field, field_value.clone()),
                (outer_field, field_value)
            ],
        ),
    ));
    assert!(!super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &ClientReferenceObject::new(
            outer_object,
            object,
            vec![(outer_field, RuntimeValue::Text("wrong".to_owned()))],
        ),
    ));

    let mut state = ClientStateStore::new();
    state.set_security_context_digest(digest);
    state.install_reference_loader(
        ClientReferenceLoader::new(
            active.pair(),
            authorisation.session_principal(),
            digest,
            [partial],
        )
        .unwrap(),
    );
    let error = super::evaluate_field_path(
        &active,
        &RuntimeValue::Reference {
            target: outer_type,
            object,
        },
        &[outer_field],
        context,
        authorisation.session_principal(),
        &state,
    )
    .expect_err("an omitted requested field must remain a FieldPath failure");
    assert!(matches!(
        error,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::FieldPath,
            ..
        }
    ));
}

#[test]
#[allow(clippy::result_large_err)]
fn reference_root_field_path_fails_closed_without_loader_or_object() {
    let (
        active,
        context,
        parameter,
        outer_type,
        _outer_object,
        _outer_record,
        _inner_object,
        _inner_record,
        outer_field,
        _inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let expression = orna_artifact::client_plan::ClientExpressionNode::FieldPath {
        root: parameter,
        fields: vec![outer_field],
    };
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target: outer_type,
            object: ObjectId::from_bytes([0x31; 16]),
        },
    )
    .unwrap();
    let evaluate = |state: &mut ClientStateStore, principal| {
        let mut executor: Option<&mut dyn super::ClientResourceExecutor> = None;
        let mut locals = super::ClientLocalEnvironment::new();
        super::evaluate_expression(
            &active,
            &expression,
            context,
            super::ObserverLineage::top_level(context.parent_invocation_id()),
            &[(argument.parameter(), argument.value().clone())],
            &[],
            &super::capability::LocalCapabilityGrantSet::new(),
            state,
            0,
            principal,
            &mut executor,
            &mut locals,
        )
    };

    let mut absent = ClientStateStore::new();
    assert!(matches!(
        evaluate(&mut absent, authorisation.session_principal()),
        Err(super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut objects = HashMap::new();
    objects.insert(
        (outer_type, ObjectId::from_bytes([0x31; 16])),
        RuntimeValue::Text("not a record".to_owned()),
    );
    let digest = super::security_context_digest(&authorisation);
    let mut wrong_type = ClientStateStore::new();
    wrong_type.set_security_context_digest(digest);
    wrong_type.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: digest,
        objects,
    });
    assert!(matches!(
        evaluate(&mut wrong_type, authorisation.session_principal()),
        Err(super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut missing = ClientStateStore::new();
    missing.set_security_context_digest(digest);
    missing.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: digest,
        objects: HashMap::new(),
    });
    assert!(matches!(
        evaluate(&mut missing, authorisation.session_principal()),
        Err(super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::FieldPath,
            ..
        })
    ));
}

#[test]
#[allow(clippy::result_large_err)]
fn reference_root_loader_isolated_by_principal_revision_and_unknown_field() {
    let (
        active,
        context,
        parameter,
        outer_type,
        outer_object,
        outer_record,
        _inner_object,
        _inner_record,
        outer_field,
        _inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let digest = super::security_context_digest(&authorisation);
    let mut objects = HashMap::new();
    objects.insert(
        (outer_object, ObjectId::from_bytes([0x31; 16])),
        RuntimeValue::Record(outer_record),
    );
    let fixture = ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: digest,
        objects,
    };
    let expression = |field| orna_artifact::client_plan::ClientExpressionNode::FieldPath {
        root: parameter,
        fields: vec![field],
    };
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target: outer_type,
            object: ObjectId::from_bytes([0x31; 16]),
        },
    )
    .unwrap();
    let evaluate = |state: &mut ClientStateStore, principal, field| {
        let mut executor: Option<&mut dyn super::ClientResourceExecutor> = None;
        let mut locals = super::ClientLocalEnvironment::new();
        super::evaluate_expression(
            &active,
            &expression(field),
            context,
            super::ObserverLineage::top_level(context.parent_invocation_id()),
            &[(argument.parameter(), argument.value().clone())],
            &[],
            &super::capability::LocalCapabilityGrantSet::new(),
            state,
            0,
            principal,
            &mut executor,
            &mut locals,
        )
    };

    let mut principal_mismatch = ClientStateStore::new();
    principal_mismatch.set_security_context_digest(digest);
    principal_mismatch.set_reference_loader_fixture(fixture.clone());
    assert!(matches!(
        evaluate(
            &mut principal_mismatch,
            PrincipalId::from_bytes([0x7b; 16]),
            outer_field
        ),
        Err(super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut revision_mismatch = ClientStateStore::new();
    revision_mismatch.set_security_context_digest(digest);
    revision_mismatch.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: RevisionPair::new(
            SourceRevisionId::from_bytes([0xf1; 16]),
            active.pair().catalogue(),
        ),
        ..fixture.clone()
    });
    assert!(matches!(
        evaluate(
            &mut revision_mismatch,
            authorisation.session_principal(),
            outer_field
        ),
        Err(super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut unknown_field = ClientStateStore::new();
    unknown_field.set_security_context_digest(digest);
    unknown_field.set_reference_loader_fixture(fixture);
    assert!(matches!(
        evaluate(
            &mut unknown_field,
            authorisation.session_principal(),
            FieldId::from_bytes([0xff; 16])
        ),
        Err(super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::FieldPath,
            ..
        })
    ));
}

fn assert_reordered_client_plan_rejects_before_executor(source: &str, function_name: &str) {
    let prepared = prepared_client_source_v6(source);
    let (active, function) = active_with_reordered_client_call_references(&prepared, function_name);
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(1)));
    let error = super::evaluate_client_function_with_executor(
        &active,
        &authorise(active.pair(), function),
        &mut executor,
    )
    .expect_err("the durable call sequence must be checked before execution");

    assert!(matches!(
        error,
        super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::ClientExpressionError::InvalidCall,
        } if context.function() == function
    ));
    assert!(executor.executed.is_empty());
}

#[test]
fn state_plan_preflights_defaults_before_return_expression() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.second() RETURNS INTEGER RETURN 2;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  STATE value INTEGER DEFAULT app.first();
  BEGIN RETURN app.second(); END;"#,
        "app.owner",
    );
}

#[test]
fn procedural_plan_preflights_statements_before_return_expression() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.second() RETURNS INTEGER RETURN 2;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  BEGIN
    LET value INTEGER := app.first();
    value := app.second();
    RETURN value;
  END;"#,
        "app.owner",
    );
}

#[test]
fn programmable_client_control_flow_executes_compiled_source() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.counter() RETURNS INTEGER IS
  LET total INTEGER := 0;
  BEGIN
    WHILE total < 5 LOOP
      LET next INTEGER := total + 1;
      total := next;
    END LOOP;
    IF total = 5 THEN
      RETURN total;
    ELSE
      RETURN 0;
    END IF;
  END;"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.counter")
        .expect("the control-flow function is present")
        .id();

    let result = evaluate_client_function(&active, function)
        .expect("the compiled control-flow function evaluates successfully");

    assert_eq!(result.value(), &RuntimeValue::Integer(5));
}

#[test]
fn recursive_client_control_flow_uses_shared_execution_fuel() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.factorial(p_n INTEGER) RETURNS INTEGER IS
  BEGIN
    IF p_n <= 1 THEN
      RETURN 1;
    ELSE
      RETURN p_n * app.factorial(p_n - 1);
    END IF;
  END;"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.factorial")
        .expect("the recursive function is present")
        .id();
    let parameter = active
        .catalogue()
        .function_by_id(function)
        .expect("the recursive function definition is present")
        .parameters()[0]
        .id();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Integer(3)).expect("integer argument");

    let result = super::evaluate_client_function_with_arguments(
        &active,
        &authorise(active.pair(), function),
        &[argument],
    )
    .expect("the recursive control-flow function evaluates successfully");

    assert_eq!(result.value(), &RuntimeValue::Integer(6));
}
#[test]
fn recursive_client_control_flow_stops_at_depth_limit() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.loop(p_n INTEGER) RETURNS INTEGER IS
  BEGIN
    RETURN app.loop(p_n);
  END;"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.loop")
        .expect("the recursive function is present")
        .id();
    let parameter = active
        .catalogue()
        .function_by_id(function)
        .expect("the recursive function definition is present")
        .parameters()[0]
        .id();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Integer(0)).expect("integer argument");

    let error = super::evaluate_client_function_with_arguments(
        &active,
        &authorise(active.pair(), function),
        &[argument],
    )
    .expect_err("recursive control flow must stop at the depth limit");

    assert!(matches!(
        error,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::RecursionLimit,
            ..
        }
    ));
}

#[test]
fn rejects_non_boolean_short_circuit_operands_before_execution() {
    for (operator, left) in [
        (ControlFlowBinaryOperator::And, false),
        (ControlFlowBinaryOperator::Or, true),
    ] {
        let plan = ControlFlowClientPlan::new(
            Vec::new(),
            vec![orna_artifact::client_plan::ControlFlowStatement::return_(
                Some(ClientExpressionNode::Binary {
                    operator,
                    left: Box::new(ClientExpressionNode::Boolean { value: left }),
                    right: Box::new(ClientExpressionNode::Binary {
                        operator: ControlFlowBinaryOperator::And,
                        left: Box::new(ClientExpressionNode::Call {
                            function: FunctionId::from_bytes([6; 16]),
                            arguments: Vec::new(),
                        }),
                        right: Box::new(ClientExpressionNode::Integer { value: 1 }),
                    }),
                }),
            )],
        );
        let payload = plan
            .encode()
            .expect("malformed Boolean plan encodes structurally");
        let (active, function, pair, _) = version_two_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::Function(FunctionId::from_bytes([6; 16])),
            DefinitionReferenceKind::FunctionCall,
            orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION,
            payload,
        );
        let error = evaluate_client_function(&active, function)
            .expect_err("strict Boolean operands must be checked before short-circuiting");

        assert!(matches!(
            error,
            ClientExecutionError::ExpressionEvaluation {
                context,
                source: ClientExpressionError::TypeMismatch,
            } if context.pair() == pair && context.function() == function
        ));
    }
}

#[test]
fn action_plan_preflights_arguments_before_operation_target() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS
  std.action.call(
    target => std.invoke.echo,
    arguments => std.call.args(p_value => app.first())
  );"#,
        "app.owner",
    );
}

#[test]
fn action_plan_accepts_untampered_call_reference_order_and_builds_action() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS
  std.action.call(
    target => std.invoke.echo,
    arguments => std.call.args(p_value => app.first())
  );"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.owner")
        .expect("the action owner is present")
        .id();
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(7)));

    let result = super::evaluate_client_function_with_executor(
        &active,
        &authorise(active.pair(), function),
        &mut executor,
    )
    .expect("an untampered action plan evaluates successfully");

    assert!(matches!(result.value(), RuntimeValue::Opaque(_)));
    assert!(executor.executed.is_empty());
}

#[test]
fn source_introspection_exposes_parameters_and_function_references() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
             CREATE CLIENT FUNCTION app.target(p_value INTEGER) RETURNS INTEGER RETURN p_value; \
             CREATE CLIENT FUNCTION app.describe() RETURNS sys.source.function \
             RETURN sys.source.current();",
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.describe")
        .expect("the source-authored function is present")
        .id();

    let result = evaluate_client_function(&active, function)
        .expect("source introspection must execute through the public entry point");
    let RuntimeValue::Opaque(value) = result.value() else {
        panic!("source introspection must return an opaque metadata value");
    };
    let metadata =
        orna_core::source_metadata::SourceFunctionMetadata::decode(value.canonical_payload())
            .expect("the returned payload must decode as source metadata");
    assert_eq!(metadata.function(), function);
    assert_eq!(metadata.function_name(), "app.describe");
    assert!(metadata.parameters().is_empty());
    assert_eq!(metadata.references().len(), 1);
    assert_eq!(
        metadata.references()[0].target_name(),
        "sys.source.function"
    );
}
#[test]
fn source_reference_names_qualify_standard_parameter() {
    let standard = orna_standard::verify_standard_library_v9_snapshot(
        orna_standard::retained_standard_library_v9_snapshot().unwrap(),
    )
    .unwrap();
    let active = empty_version_two_active(&standard);
    let function = standard
        .catalogue()
        .function_by_id(orna_standard::STD_UI_TEXT_FUNCTION_ID)
        .expect("the standard text function is present");
    let parameter = function.parameters()[0].id();

    assert_eq!(
        super::source_reference_target_name(
            &active,
            DefinitionReferenceTarget::Parameter {
                owner: function.id(),
                parameter,
            },
        )
        .as_deref(),
        Some("std.ui.text.text"),
    );
}

#[test]
fn resource_plan_preflights_arguments_before_operation_target() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  BEGIN
    RETURN AWAIT std.data.resource(
      target => std.invoke.echo,
      arguments => std.call.args(p_value => app.first())
    );
  END;"#,
        "app.owner",
    );
}

#[test]
fn capability_expression_calls_reject_reference_sequence_mismatch() {
    let function = FunctionId::from_bytes([6; 16]);
    let call = || orna_artifact::client_plan::ClientExpressionNode::Call {
        function,
        arguments: Vec::new(),
    };
    let expression = orna_artifact::client_plan::ClientExpressionNode::Concat {
        left: Box::new(call()),
        right: Box::new(call()),
    };
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(
            orna_artifact::client_plan::ExpressionClientPlan::new(expression),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .expect("the capability expression plan encodes");
    let (active, function, pair, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::Function(function),
        DefinitionReferenceKind::FunctionCall,
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        payload,
    );

    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let error = super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[],
        &grants,
    )
    .expect_err("the decoded call sequence must match durable references");

    assert!(matches!(
        error,
        super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::ClientExpressionError::InvalidCall,
        } if context.function() == function
    ));
}

fn capability_direct_callee_denies_ungranted_declaration<F>(make_plan: F)
where
    F: FnOnce(FunctionId) -> orna_artifact::client_plan::InnerClientPlan,
{
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.first() RETURNS TEXT RETURN app.second(); CREATE CLIENT FUNCTION app.second() RETURNS TEXT RETURN 'ok';",
    );
    let initial = active_from_prepared_candidate(&prepared);
    let caller = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.first")
        .expect("caller is present")
        .clone();
    let callee = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.second")
        .expect("callee is present")
        .clone();
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        make_plan(callee.id()),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.write",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .expect("the capability plan encodes");
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let current = initial
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .expect("caller revision is present");
    let caller_references = initial
        .references()
        .iter()
        .filter(|reference| reference.source_function() == caller.id())
        .cloned()
        .collect::<Vec<_>>();
    let semantic_hash = function_semantic_digest_with_version(
        current.semantic_hash_version(),
        &caller,
        current.language_version(),
        &artifact,
        initial.expressions(),
        &caller_references,
    )
    .unwrap();
    let replacement = FunctionRevisionRecord::new(
        caller.id(),
        current.id(),
        current.revision_number(),
        current.declaration_origin(),
        current.declaration_content_hash(),
        semantic_hash,
        current.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(current.semantic_hash_version());
    let revisions = initial
        .function_revisions()
        .iter()
        .map(|revision| {
            if revision.function() == caller.id() {
                replacement.clone()
            } else {
                revision.clone()
            }
        })
        .collect::<Vec<_>>();
    let catalogue_hash = catalogue_digest_with_context(
        initial.catalogue_hash_context(),
        initial.catalogue(),
        &revisions,
        initial.expressions(),
        initial.origins(),
        initial.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            initial.pair(),
            initial.source().clone(),
            initial.catalogue().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                initial.expressions().to_vec(),
                revisions,
                initial.origins().to_vec(),
                initial.references().to_vec(),
            ),
        ),
        initial.catalogue_hash_context().clone(),
    )
    .unwrap();
    let declaration = capability::LocalCapabilityDeclaration::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityArgumentSource::Text("/tmp".to_owned()),
    );
    let write_grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsWrite,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([write_grant]).unwrap();
    let error = super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), caller.id()),
        &[declaration],
        &grants,
    )
    .expect_err("the direct callee must inherit the checked declaration context");
    assert!(matches!(
        error,
        super::ClientExecutionError::CapabilityDenied { context, capability }
            if context.function() == callee.id() && capability == "std.fs.read"
    ));
}

#[test]
fn capability_expression_calls_preserve_declarations_for_direct_callees() {
    capability_direct_callee_denies_ungranted_declaration(|callee| {
        orna_artifact::client_plan::InnerClientPlan::Expression(
            orna_artifact::client_plan::ExpressionClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Call {
                    function: callee,
                    arguments: Vec::new(),
                },
            ),
        )
    });
}

#[test]
fn capability_procedural_calls_preserve_declarations_for_direct_callees() {
    capability_direct_callee_denies_ungranted_declaration(|callee| {
        orna_artifact::client_plan::InnerClientPlan::Procedural(
            orna_artifact::client_plan::ProceduralClientPlan::new(
                Vec::new(),
                Vec::new(),
                orna_artifact::client_plan::ClientExpressionNode::Call {
                    function: callee,
                    arguments: Vec::new(),
                },
            ),
        )
    });
}

#[test]
fn transfers_the_evaluated_value_without_cloning_its_payload() {
    let (active, function, _, _) = version_one_active(true);

    assert_eq!(
        evaluate_client_function(&active, function)
            .unwrap()
            .into_value(),
        RuntimeValue::Boolean(true),
    );
}

#[test]
fn rejects_mismatched_authorisation_before_active_revision_validation() {
    let (active, function, pair, _) = version_one_active(true);
    let other_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x7b; 16]),
        CatalogueRevisionId::from_bytes([0x7c; 16]),
    );
    let untrusted = ActiveDatabaseRevision::new(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        orna_core::revision::Sha256Digest::from_bytes([0x7d; 32]),
        active.expressions().to_vec(),
        active.function_revisions().to_vec(),
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .expect("tampered hash remains structurally valid");

    let error = super::evaluate_client_function(&untrusted, &authorise(other_pair, function))
        .expect_err("mismatched authorisation must fail");

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    assert_eq!(error.context(), None);
    assert_eq!(
        error.to_string(),
        "the CLIENT authorisation does not match the active revision"
    );
    assert!(matches!(
        error,
        super::ClientExecutionError::AuthorisationMismatch {
            authorised,
            active,
        } if authorised == InvocationTarget::new(function, other_pair) && active == pair
    ));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn rejects_an_active_revision_with_a_stale_catalogue_hash_before_function_checks() {
    let (active, _, pair, _) = version_one_active(true);
    let requested = FunctionId::from_bytes([0x8c; 16]);
    let stale = ActiveDatabaseRevision::new(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        orna_core::revision::Sha256Digest::from_bytes([0x8a; 32]),
        active.expressions().to_vec(),
        active.function_revisions().to_vec(),
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .unwrap();

    let error = evaluate_client_function(&stale, requested).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), requested);
    assert_eq!(error.context(), None);
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidActiveRevision {
            source: super::ClientActiveRevisionError::CatalogueHashMismatch,
            ..
        }
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn wraps_a_canonical_active_semantics_failure_before_function_checks() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let original = &active.function_revisions()[0];
    let inconsistent_revision = FunctionRevisionRecord::new(
        function,
        function_revision,
        original.revision_number(),
        original.declaration_origin(),
        original.declaration_content_hash(),
        orna_core::revision::Sha256Digest::from_bytes([0x8b; 32]),
        original.language_version(),
        original.artifact().clone(),
    )
    .unwrap();
    let untrusted = ActiveDatabaseRevision::new(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        active.catalogue_hash(),
        active.expressions().to_vec(),
        vec![inconsistent_revision],
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .unwrap();

    let error = evaluate_client_function(&untrusted, function).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    assert_eq!(error.context(), None);
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidActiveRevision {
            source: super::ClientActiveRevisionError::Canonical(
                orna_core::canonical_hash::CanonicalHashError::FunctionSemanticHashMismatch {
                    function: actual_function,
                    revision: actual_revision,
                }
            ),
            ..
        } if actual_function == function && actual_revision == function_revision
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn rejects_a_mismatched_function_artifact_payload_hash_before_function_checks() {
    let (active, _, pair, _) = version_one_active(true);
    let requested = FunctionId::from_bytes([0x8d; 16]);
    let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);

    let error = evaluate_client_function(&untrusted, requested).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), requested);
    assert_eq!(error.context(), None);
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidActiveRevision {
            source: super::ClientActiveRevisionError::Canonical(
                orna_core::canonical_hash::CanonicalHashError::ArtifactPayloadHashMismatch {
                    artifact: "function artifact",
                }
            ),
            ..
        }
    ));
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    let source = std::error::Error::source(&error).unwrap();
    assert_eq!(
        source.to_string(),
        "function artifact payload hash differs from exact payload"
    );
    assert!(std::error::Error::source(source).is_some());
}

#[test]
fn client_evaluator_rejects_mismatched_payload_hash_before_resource_execution() {
    let (active, function, pair, _) = version_one_active(true);
    let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);
    let mut state = ClientStateStore::new();
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Boolean(true)));
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);

    let error = super::evaluate_function(
        &untrusted,
        function,
        Vec::new(),
        &[],
        &capability::LocalCapabilityGrantSet::default(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x7a; 16]),
        super::ObserverLineage::top_level(InvocationId::from_bytes([0xa1; 16])),
        &mut executor_slot,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidArtifact { context, .. }
            if context.pair() == pair && context.function() == function
    ));
    assert!(executor.executed.is_empty());
    assert!(executor.cancelled.is_empty());
}

#[test]
fn client_artifact_guard_rejects_server_kind_with_client_payload() {
    let (_active, function, pair, function_revision) = version_one_active(true);
    let payload = orna_artifact::client_plan::ClientPlan::return_boolean(true).encode();
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.client-plan",
        orna_artifact::client_plan::FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let context = super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0xa2; 16]),
        observer_lineage: None,
    };

    let error = super::validate_artifact_identity(&artifact, context).unwrap_err();

    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidArtifact { context: actual, .. }
            if actual == context
    ));
    assert_eq!(
        error.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );
}

#[test]
fn public_active_revision_construction_preserves_client_evaluator_boundaries() {
    let (version_one, function, _, function_revision) = version_one_active(true);
    let value_type = TypeId::from_bytes([0x93; 16]);
    let value_reference = DefinitionReference::new(
        function,
        function_revision,
        0,
        DefinitionReferenceTarget::ValueType(value_type),
        DefinitionReferenceKind::NamedType,
        version_one.function_revisions()[0].declaration_origin(),
    );
    let version_two_revision = version_one.function_revisions()[0]
        .clone()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let error = ActiveDatabaseRevision::new(
        version_one.pair(),
        version_one.source().clone(),
        version_one.catalogue().clone(),
        version_one.catalogue_hash(),
        version_one.expressions().to_vec(),
        vec![version_two_revision],
        version_one.origins().to_vec(),
        vec![value_reference],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
            function: actual_function,
            revision: actual_revision,
            target,
        } if actual_function == function && actual_revision == function_revision && target == value_type
    ));
    assert_eq!(
        error.to_string(),
        "value-type references require catalogue hash version 2"
    );
    assert!(std::error::Error::source(&error).is_none());

    let error = ActiveDatabaseRevision::new(
        version_one.pair(),
        version_one.source().clone(),
        version_one.catalogue().clone(),
        version_one.catalogue_hash(),
        version_one.expressions().to_vec(),
        vec![
            version_one.function_revisions()[0]
                .clone()
                .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
        ],
        version_one.origins().to_vec(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
            function: actual_function,
            revision: actual_revision,
        } if actual_function == function && actual_revision == function_revision
    ));
    assert_eq!(
        error.to_string(),
        "function semantic hash version 2 requires catalogue hash version 2"
    );
    assert!(std::error::Error::source(&error).is_none());

    let missing_target = TypeId::from_bytes([0x92; 16]);
    let error = ActiveDatabaseRevision::new(
        version_one.pair(),
        version_one.source().clone(),
        version_one.catalogue().clone(),
        version_one.catalogue_hash(),
        version_one.expressions().to_vec(),
        version_one.function_revisions().to_vec(),
        version_one.origins().to_vec(),
        vec![DefinitionReference::new(
            function,
            function_revision,
            0,
            DefinitionReferenceTarget::ObjectType(missing_target),
            DefinitionReferenceKind::ObjectReference,
            version_one.function_revisions()[0].declaration_origin(),
        )],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceTargetNotInRevision {
            target: DefinitionReferenceTarget::ObjectType(target),
        } if target == missing_target
    ));
    assert_eq!(
        error.to_string(),
        "reference target is absent from revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let prepared_function = active.catalogue().functions()[0].id();
    let current_revision = active.catalogue().functions()[0].current_revision();
    let selected = active
        .references()
        .iter()
        .find(|reference| reference.source_function() == prepared_function)
        .unwrap();
    assert!(matches!(
        selected.target(),
        DefinitionReferenceTarget::ValueType(_)
    ));
    let selected_target = match selected.target() {
        DefinitionReferenceTarget::ValueType(target) => target,
        _ => TypeId::from_bytes([0; 16]),
    };
    let unavailable_revision = FunctionRevisionId::from_bytes([0x94; 16]);
    let unavailable_reference = DefinitionReference::new(
        prepared_function,
        unavailable_revision,
        selected.ordinal(),
        selected.target(),
        selected.kind(),
        selected.source_origin(),
    );
    let error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                active.function_revisions().to_vec(),
                active.origins().to_vec(),
                vec![unavailable_reference],
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ValueTypeReferenceFunctionRevisionUnavailable {
            function: actual_function,
            revision,
            target,
        } if actual_function == prepared_function && revision == unavailable_revision && target == selected_target
    ));
    assert_eq!(
        error.to_string(),
        "cannot verify a value-type reference without its function revision record"
    );
    assert!(std::error::Error::source(&error).is_none());

    let version_one_revisions = active
        .function_revisions()
        .iter()
        .cloned()
        .map(|revision| revision.with_semantic_hash_version(FunctionSemanticHashVersion::Version1))
        .collect::<Vec<_>>();
    let error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                version_one_revisions,
                active.origins().to_vec(),
                active.references().to_vec(),
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
            function: actual_function,
            revision,
            target,
        } if actual_function == prepared_function && revision == current_revision && target == selected_target
    ));
    assert_eq!(
        error.to_string(),
        "value-type references require function semantic hash version 2"
    );
    assert!(std::error::Error::source(&error).is_none());

    let object = active.catalogue().object_types()[0].id();
    let kind_mismatch = DefinitionReference::new(
        prepared_function,
        current_revision,
        97,
        DefinitionReferenceTarget::ValueType(selected_target),
        DefinitionReferenceKind::ObjectReference,
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, kind_mismatch).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceKindTargetMismatch {
            kind: DefinitionReferenceKind::ObjectReference,
            target: DefinitionReferenceTarget::ValueType(target),
        } if target == selected_target
    ));
    assert_eq!(
        error.to_string(),
        "reference kind cannot target that definition kind"
    );
    assert!(std::error::Error::source(&error).is_none());

    let duplicate = DefinitionReference::new(
        selected.source_function(),
        selected.source_revision(),
        selected.ordinal(),
        selected.target(),
        selected.kind(),
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, duplicate).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::DuplicateReferenceOrdinal { revision, ordinal }
            if revision == current_revision && ordinal == selected.ordinal()
    ));
    assert_eq!(error.to_string(), "duplicate reference ordinal");
    assert!(std::error::Error::source(&error).is_none());

    let reference_not_in_catalogue = DefinitionReference::new(
        FunctionId::from_bytes([0x95; 16]),
        FunctionRevisionId::from_bytes([0x96; 16]),
        99,
        DefinitionReferenceTarget::ObjectType(object),
        DefinitionReferenceKind::ObjectReference,
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, reference_not_in_catalogue).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceFunctionNotInCatalogue {
            function: actual_function,
            revision,
        } if actual_function == FunctionId::from_bytes([0x95; 16])
            && revision == FunctionRevisionId::from_bytes([0x96; 16])
    ));
    assert_eq!(
        error.to_string(),
        "reference function is absent from catalogue"
    );
    assert!(std::error::Error::source(&error).is_none());

    let stale_revision = FunctionRevisionId::from_bytes([0x97; 16]);
    let non_current_reference = DefinitionReference::new(
        prepared_function,
        stale_revision,
        99,
        DefinitionReferenceTarget::ObjectType(object),
        DefinitionReferenceKind::ObjectReference,
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, non_current_reference).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceRevisionNotCurrent {
            function: actual_function,
            expected,
            actual,
        } if actual_function == prepared_function && expected == current_revision && actual == stale_revision
    ));
    assert_eq!(
        error.to_string(),
        "reference revision is not catalogue current revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let unit_not_in_revision =
        SourceOrigin::new(SourceUnitId::from_bytes([0x98; 16]), 0, 0).unwrap();
    let error = active_with_replaced_first_origin(&version_one, unit_not_in_revision).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginUnitNotInRevision { source_unit }
            if source_unit == SourceUnitId::from_bytes([0x98; 16])
    ));
    assert_eq!(
        error.to_string(),
        "source origin unit is absent from stored revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let source_unit = version_one.source().units()[0].id();
    let out_of_bounds = SourceOrigin::new(
        source_unit,
        0,
        u32::try_from(version_one.source().units()[0].content().len() + 1).unwrap(),
    )
    .unwrap();
    let error = active_with_replaced_first_origin(&version_one, out_of_bounds).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginOutOfBounds {
            source_unit: actual_unit,
            byte_start: 0,
            ..
        } if actual_unit == source_unit
    ));
    assert_eq!(
        error.to_string(),
        "source origin is outside stored source content"
    );
    assert!(std::error::Error::source(&error).is_none());

    let unicode_source = replacement_source(&version_one, "é");
    let split_character = SourceOrigin::new(source_unit, 1, 1).unwrap();
    let error = active_with_source_and_first_origin(&version_one, unicode_source, split_character)
        .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginNotCharacterBoundary {
            source_unit: actual_unit,
            byte_start: 1,
            byte_end: 1,
        } if actual_unit == source_unit
    ));
    assert_eq!(error.to_string(), "source origin splits a UTF-8 character");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn public_active_revision_construction_rejects_invalid_reference_source_origins() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let source_unit = active.source().units()[0].id();

    let error = active_with_replaced_reference_origin(
        &active,
        active.source().clone(),
        function,
        SourceOrigin::new(SourceUnitId::from_bytes([0x99; 16]), 0, 0).unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginUnitNotInRevision { source_unit: actual }
            if actual == SourceUnitId::from_bytes([0x99; 16])
    ));
    assert_eq!(
        error.to_string(),
        "source origin unit is absent from stored revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let error = active_with_replaced_reference_origin(
        &active,
        active.source().clone(),
        function,
        SourceOrigin::new(
            source_unit,
            0,
            u32::try_from(active.source().units()[0].content().len() + 1).unwrap(),
        )
        .unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginOutOfBounds {
            source_unit: actual,
            byte_start: 0,
            ..
        } if actual == source_unit
    ));
    assert_eq!(
        error.to_string(),
        "source origin is outside stored source content"
    );
    assert!(std::error::Error::source(&error).is_none());

    let unicode_source = replacement_source(
        &active,
        &format!("{}é", active.source().units()[0].content()),
    );
    let original_length = active.source().units()[0].content().len();
    let error = active_with_replaced_reference_origin(
        &active,
        unicode_source,
        function,
        SourceOrigin::new(
            source_unit,
            u32::try_from(original_length + 1).unwrap(),
            u32::try_from(original_length + 1).unwrap(),
        )
        .unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginNotCharacterBoundary {
            source_unit: actual,
            byte_start,
            byte_end,
        } if actual == source_unit
            && byte_start == u32::try_from(original_length + 1).unwrap()
            && byte_end == u32::try_from(original_length + 1).unwrap()
    ));
    assert_eq!(error.to_string(), "source origin splits a UTF-8 character");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn stream_expression_rejects_scalar_literal_plan() {
    let payload = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
    )
    .encode()
    .unwrap();
    let (active, function, _, _) = version_two_client_stream_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        payload,
    );
    let error = evaluate_client_function(&active, function).unwrap_err();
    assert!(matches!(
        error,
        super::ClientExecutionError::ExpressionEvaluation {
            source: super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
}

#[test]
fn stream_artifact_versions_reject_scalar_roots() {
    let scalar = orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true };
    for (artifact_version, payload) in [
        (
            orna_artifact::client_plan::STATE_FORMAT_VERSION,
            orna_artifact::client_plan::StateClientPlan::new(
                scalar.clone(),
                vec![orna_artifact::client_plan::StateSlot::new(
                    StateSlotId::from_bytes([0x21; 16]),
                    orna_standard::BOOLEAN_TYPE_ID,
                    orna_artifact::client_plan::StateScope::User,
                    orna_artifact::client_plan::StateDefault::Unset,
                )],
            )
            .encode()
            .expect("the state plan encodes"),
        ),
        (
            orna_artifact::client_plan::RESOURCE_FORMAT_VERSION,
            orna_artifact::client_plan::ResourceClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Await {
                    expression: Box::new(
                        orna_artifact::client_plan::ClientExpressionNode::Resource {
                            operation: orna_artifact::client_plan::ResourceOperationNode::new(
                                orna_artifact::client_plan::ResourceKind::Scalar,
                                FunctionId::from_bytes([6; 16]),
                                RevisionPair::new(
                                    SourceRevisionId::from_bytes([1; 16]),
                                    CatalogueRevisionId::from_bytes([2; 16]),
                                ),
                                CallSiteId::from_bytes([0xe1; 16]),
                                Vec::new(),
                                orna_standard::BOOLEAN_TYPE_ID,
                            ),
                        },
                    ),
                },
            )
            .encode()
            .expect("the resource plan encodes"),
        ),
        (
            orna_artifact::client_plan::PROCEDURAL_FORMAT_VERSION,
            orna_artifact::client_plan::ProceduralClientPlan::new(Vec::new(), Vec::new(), scalar)
                .encode()
                .expect("the procedural plan encodes"),
        ),
    ] {
        let (active, function, _, _) = version_two_client_stream_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            artifact_version,
            payload,
        );
        let error = evaluate_client_function(&active, function).unwrap_err();
        if artifact_version == orna_artifact::client_plan::RESOURCE_FORMAT_VERSION {
            assert!(matches!(
                error,
                super::ClientExecutionError::ExpressionEvaluation {
                    source: super::ClientExpressionError::InvalidCall,
                    ..
                }
            ));
        } else {
            assert!(matches!(
                error,
                super::ClientExecutionError::ExpressionEvaluation {
                    source: super::ClientExpressionError::TypeMismatch,
                    ..
                }
            ));
        }
    }
}

#[test]
fn prepared_client_stream_shape_reaches_runtime_contract_boundary() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
             CREATE EXTERNAL CLIENT FUNCTION app.events() \
             RETURNS STREAM<BOOLEAN> RUNTIME CONTRACT 'app.events@1';",
    );
    let active = active_from_prepared_candidate(&prepared);
    let definition = &active.catalogue().functions()[0];
    assert!(matches!(
        definition.return_type(),
        FunctionReturn::Stream(ResolvedType::Value(type_id))
            if *type_id == orna_standard::BOOLEAN_TYPE_ID
    ));
    let function = definition.id();
    let error = evaluate_client_function(&active, function).unwrap_err();
    assert!(matches!(
        error,
        super::ClientExecutionError::ExternalContract { identity, .. }
            if identity == "app.events@1"
    ));
}

#[test]
fn compiler_emitted_v5_capability_gate_fails_closed_before_runtime() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
             CREATE EXTERNAL CLIENT FUNCTION app.read() \
             RETURNS BOOLEAN RUNTIME CONTRACT 'std.fs.read@1' \
             REQUIRES CAPABILITY std.fs.read('/tmp/input');",
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let authorisation = authorise(active.pair(), function);

    let missing = super::evaluate_client_function_with_grants(
        &active,
        &authorisation,
        &[],
        &super::capability::LocalCapabilityGrantSet::new(),
    )
    .unwrap_err();
    assert!(matches!(
        missing,
        super::ClientExecutionError::CapabilityDenied { capability, .. }
            if capability == "std.fs.read"
    ));

    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    // The local grant passes. The runtime contract is not installed in this evaluator,
    // so the next error must be the external-contract boundary.
    let unavailable =
        super::evaluate_client_function_with_grants(&active, &authorisation, &[], &grants)
            .unwrap_err();

    assert!(matches!(
        unavailable,
        super::ClientExecutionError::ExternalContract { identity, .. }
            if identity == "std.fs.read@1"
    ));
}

#[test]
fn evaluates_a_version_five_expression_parameter_read() {
    use orna_artifact::client_plan::{
        CapabilityArgumentSource, CapabilityClientPlan, CapabilityRequirement,
        ClientExpressionNode, ExpressionClientPlan, InnerClientPlan,
    };

    let parameter = ParameterId::from_bytes([0xb1; 16]);
    let payload = CapabilityClientPlan::new(
        InnerClientPlan::Expression(ExpressionClientPlan::new(
            ClientExpressionNode::ParameterRead { parameter },
        )),
        vec![CapabilityRequirement::new(
            "std.fs.read",
            CapabilityArgumentSource::Parameter("p_path".to_owned()),
        )],
    )
    .encode()
    .expect("the expression capability plan encodes");
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let authorisation = authorise(pair, function);
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/input".to_owned())).unwrap();
    let grant = super::capability::LocalCapabilityGrant::new(
        super::capability::LocalCapabilityName::StdFsRead,
        super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

    let result = super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorisation,
        std::slice::from_ref(&argument),
        &[],
        &grants,
    )
    .expect("the version-five expression evaluates");

    assert_eq!(result.value(), &RuntimeValue::Text("/tmp/input".to_owned()));
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().pair(), active.pair());
}

#[test]
fn evaluates_native_session_input_expression() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.prompt() RETURNS TEXT RETURN std.cli.input();",
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let authorisation = authorise(active.pair(), function);
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Boolean(true)));

    let result = evaluate_client_function_with_executor(&active, &authorisation, &mut executor)
        .expect("native session input evaluates");

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("from session".to_owned())
    );
}
#[test]
fn evaluates_prepared_version_two_client_constants() {
    for (literal, expected) in [("TRUE", true), ("FALSE", false)] {
        let prepared = prepared_client_constant(literal);
        let active = active_from_prepared_candidate(&prepared);
        let function = active.catalogue().functions()[0].id();

        let result = evaluate_client_function(&active, function).unwrap();

        assert_eq!(result.context().pair(), active.pair());
        assert_eq!(result.context().function(), function);
        assert_eq!(
            result.context().function_revision(),
            active.catalogue().functions()[0].current_revision()
        );
        assert_eq!(result.value(), &RuntimeValue::Boolean(expected));
    }
}

#[test]
fn evaluates_a_hand_built_version_two_value_return() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let boolean_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.representation_contract() == "orna.kernel.value.boolean@1")
        .unwrap()
        .id();
    let (active, function, pair, function_revision) =
        version_two_value_active(boolean_type, boolean_type);
    assert_eq!(
        active.function_revisions()[0].artifact().payload(),
        b"ORNACP\0\0\0\0\0\x01\x01\x01"
    );

    let result = evaluate_client_function(&active, function).unwrap();

    assert_eq!(result.context().pair(), pair);
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().function_revision(), function_revision);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn evaluates_a_registered_opaque_client_result() {
    let payload = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let (active, function, pair, function_revision) =
        version_two_opaque_active(orna_standard::OPAQUE_TOKEN_TYPE_ID, payload);

    let result = evaluate_client_function(&active, function).unwrap();

    assert_eq!(result.context().pair(), pair);
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().function_revision(), function_revision);
    let RuntimeValue::Opaque(value) = result.value() else {
        panic!("opaque plan must produce one opaque value");
    };
    assert_eq!(value.opaque_type(), orna_standard::OPAQUE_TOKEN_TYPE_ID);
    assert_eq!(value.canonical_payload(), payload);
}

#[test]
fn evaluates_a_registered_opaque_ui_client_result() {
    let body = br#"{"kind":"empty"}"#;
    let mut payload = Vec::from(b"ORNA-UI/1 ".as_slice());
    payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
    payload.extend_from_slice(body);
    let plan = orna_artifact::client_plan::OpaqueClientPlan::return_opaque(
        orna_standard::STD_UI_TYPE_ID,
        payload.clone(),
    )
    .encode()
    .expect("opaque UI plan encodes");
    let (active, function, _, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::STD_UI_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::STD_UI_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::OPAQUE_FORMAT_VERSION,
        plan,
    );

    let result = evaluate_client_function(&active, function).unwrap();

    let RuntimeValue::Opaque(value) = result.value() else {
        panic!("opaque UI plan must produce one opaque value");
    };
    assert_eq!(value.opaque_type(), orna_standard::STD_UI_TYPE_ID);
    assert_eq!(value.canonical_payload(), payload);
}
#[test]
fn evaluates_v7_standard_client_external_contract_with_ordered_arguments() {
    let standard = standard_v7();
    let active = empty_version_two_active(&standard);
    let body = br#"{"kind":"empty"}"#;
    let mut payload = Vec::from(b"ORNA-UI/1 ".as_slice());
    payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
    payload.extend_from_slice(body);
    let registry = orna_standard::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("the V7 fixture pins a standard snapshot"),
    )
    .expect("the V7 fixture has a registered UI codec");
    let content = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, orna_standard::STD_UI_TYPE_ID, payload)
            .expect("the UI argument has a valid opaque payload"),
    );
    let arguments = vec![
        (
            orna_standard::STD_UI_WINDOW_TITLE_PARAMETER_ID,
            RuntimeValue::Text("title".to_owned()),
        ),
        (
            orna_standard::STD_UI_WINDOW_CONTENT_PARAMETER_ID,
            content.clone(),
        ),
    ];
    let expected_arguments = arguments.clone();
    let returned = content.clone();
    let mut executor = DeterministicClientResourceExecutor::new(
        |_request: &ClientResourceRequest| -> Result<RuntimeValue, String> {
            Err("resource executor was not used".to_owned())
        },
    )
    .with_external_contract(
        move |request: &ClientExternalContractRequest| -> Result<RuntimeValue, String> {
            assert_eq!(
                request.identity(),
                orna_standard::STD_UI_WINDOW_RUNTIME_CONTRACT
            );
            assert_eq!(request.arguments(), expected_arguments.as_slice());
            Ok(returned.clone())
        },
    );
    let grants = capability::LocalCapabilityGrantSet::new();
    let mut state = ClientStateStore::new();
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let (_, value) = super::evaluate_function(
        &active,
        orna_standard::STD_UI_WINDOW_FUNCTION_ID,
        arguments,
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x5a; 16]),
        super::ObserverLineage::top_level(InvocationId::from_bytes([0x5b; 16])),
        &mut executor_slot,
    )
    .expect("the pinned V7 standard executable evaluates");
    assert_eq!(value, content);
}

#[test]
fn opaque_client_result_rejects_plan_type_and_structure_before_value_creation() {
    let payload = [0x5a; 16];
    let wrong_type = TypeId::from_bytes([0xa7; 16]);
    let (active, function, pair, function_revision) =
        version_two_opaque_active(wrong_type, payload);

    let error = evaluate_client_function(&active, function).unwrap_err();
    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    assert_eq!(
        error.context().map(|context| context.function_revision()),
        Some(function_revision)
    );
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidOpaqueValue {
            source: super::ClientOpaqueValueError::TypeMismatch {
                expected,
                actual,
            },
            ..
        } if expected == orna_standard::OPAQUE_TOKEN_TYPE_ID && actual == wrong_type
    ));
    assert_eq!(
        error.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );
    let source = std::error::Error::source(&error).unwrap();
    assert_eq!(
        source.to_string(),
        "opaque CLIENT plan type does not match its function return"
    );
    assert!(std::error::Error::source(source).is_none());

    let mut malformed = orna_artifact::client_plan::OpaqueClientPlan::return_opaque(
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
        payload,
    )
    .encode()
    .expect("opaque plan encodes");
    malformed[29..33].copy_from_slice(&15_u32.to_be_bytes());
    malformed.truncate(malformed.len() - 1);
    let (active, function, _, _) = version_two_value_active_with_artifact(
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
        2,
        malformed,
    );
    let error = evaluate_client_function(&active, function).unwrap_err();
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidOpaqueValue {
            source:
                super::ClientOpaqueValueError::Value(
                    super::OpaqueValueError::WrongPayloadLength {
                        opaque_type,
                        expected: 16,
                        actual: 15,
                    },
                ),
            ..
        } if opaque_type == orna_standard::OPAQUE_TOKEN_TYPE_ID
    ));
}

#[test]
fn rejects_a_value_return_that_disagrees_with_its_selected_reference() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let boolean_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.representation_contract() == "orna.kernel.value.boolean@1")
        .unwrap()
        .id();
    let alternate_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.id() != boolean_type)
        .unwrap()
        .id();
    let (active, function, pair, function_revision) =
        version_two_value_active(alternate_type, boolean_type);

    let error = evaluate_client_function(&active, function).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    let context = error.context().expect("invalid function error context");
    assert_eq!(context.pair(), pair);
    assert_eq!(context.function(), function);
    assert_eq!(context.function_revision(), function_revision);
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidFunction {
            rule: super::ClientExecutionRule::References,
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "this CLIENT function depends on unsupported definitions"
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn version_two_reference_validation_uses_only_the_selected_current_function() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let functions = active.catalogue().functions();
    let first = functions[0].id();
    let second = functions[1].id();

    let result = evaluate_client_function(&active, first).unwrap();
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));

    let references = active
        .references()
        .iter()
        .filter(|reference| reference.source_function() == second)
        .cloned()
        .collect::<Vec<_>>();
    let b_only = active_from_prepared_with_references(&prepared, references);

    assert_references_rule(evaluate_client_function(&b_only, first), first);
    assert_eq!(
        evaluate_client_function(&b_only, second).unwrap().value(),
        &RuntimeValue::Boolean(true)
    );
}

#[test]
fn accepts_a_rehashed_self_consistent_selected_reference_origin() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let revision = active.catalogue().functions()[0].current_revision();
    let source = active.source().units()[0].content();
    let body_start = source.find("TRUE").unwrap();
    let replacement_origin = SourceOrigin::new(
        active.source().units()[0].id(),
        u32::try_from(body_start).unwrap(),
        u32::try_from(body_start + "TRUE".len()).unwrap(),
    )
    .unwrap();
    let mut references = active.references().to_vec();
    replace_reference(&mut references, function, |reference| {
        DefinitionReference::new(
            reference.source_function(),
            reference.source_revision(),
            reference.ordinal(),
            reference.target(),
            reference.kind(),
            replacement_origin,
        )
    });

    let stale = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                active.function_revisions().to_vec(),
                active.origins().to_vec(),
                references.clone(),
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap();
    let error = evaluate_client_function(&stale, function).unwrap_err();
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidActiveRevision {
            source: super::ClientActiveRevisionError::CatalogueHashMismatch,
            ..
        }
    ));
    assert_eq!(error.pair(), active.pair());
    assert_eq!(error.function(), function);
    assert_eq!(error.context(), None);
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    assert!(std::error::Error::source(&error).is_some());

    let repaired = active_from_prepared_with_references(&prepared, references);
    let result = evaluate_client_function(&repaired, function).unwrap();
    assert_eq!(result.context().pair(), repaired.pair());
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().function_revision(), revision);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn version_two_rejects_each_publicly_constructible_selected_reference_mismatch() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let reference = active
        .references()
        .iter()
        .find(|reference| reference.source_function() == function)
        .unwrap();
    assert!(matches!(
        active.catalogue_hash_context(),
        orna_core::revision::CatalogueHashContext::Version2 { .. }
    ));
    let standard = active.catalogue_hash_context().standard().unwrap();
    let alternate_value_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|value_type| value_type.representation_contract() != "orna.kernel.value.boolean@1")
        .unwrap()
        .id();
    let object = active.catalogue().object_types()[0].id();

    let missing = active
        .references()
        .iter()
        .filter(|candidate| candidate.source_function() != function)
        .cloned()
        .collect::<Vec<_>>();
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, missing),
            function,
        ),
        function,
    );

    let mut extra = active.references().to_vec();
    extra.push(DefinitionReference::new(
        reference.source_function(),
        reference.source_revision(),
        1,
        reference.target(),
        reference.kind(),
        reference.source_origin(),
    ));
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, extra),
            function,
        ),
        function,
    );

    let mut wrong_ordinal = active.references().to_vec();
    replace_reference(&mut wrong_ordinal, function, |candidate| {
        DefinitionReference::new(
            candidate.source_function(),
            candidate.source_revision(),
            1,
            candidate.target(),
            candidate.kind(),
            candidate.source_origin(),
        )
    });
    let error = active_from_prepared_with_references_result(&prepared, wrong_ordinal).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<RevisionInvariantError>(),
        Some(RevisionInvariantError::ReferenceOrdinalOutOfSequence {
            expected: 0,
            actual: 1,
            ..
        })
    ));

    let mut wrong_target = active.references().to_vec();
    replace_reference(&mut wrong_target, function, |candidate| {
        DefinitionReference::new(
            candidate.source_function(),
            candidate.source_revision(),
            candidate.ordinal(),
            DefinitionReferenceTarget::ValueType(alternate_value_type),
            candidate.kind(),
            candidate.source_origin(),
        )
    });
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, wrong_target),
            function,
        ),
        function,
    );

    let mut wrong_kind_and_target = active.references().to_vec();
    replace_reference(&mut wrong_kind_and_target, function, |candidate| {
        DefinitionReference::new(
            candidate.source_function(),
            candidate.source_revision(),
            candidate.ordinal(),
            DefinitionReferenceTarget::ObjectType(object),
            DefinitionReferenceKind::ObjectReference,
            candidate.source_origin(),
        )
    });
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, wrong_kind_and_target),
            function,
        ),
        function,
    );

    let semantic_version_one = active_from_prepared_with_semantic_versions(
        &prepared,
        FunctionSemanticHashVersion::Version1,
        Vec::new(),
    );
    assert_references_rule(
        evaluate_client_function(&semantic_version_one, function),
        function,
    );
}

#[test]
fn expression_like_reference_validation_accepts_declared_ref_parameter_object_references() {
    let function_id = FunctionId::from_bytes([0xd1; 16]);
    let function_revision = FunctionRevisionId::from_bytes([0xd2; 16]);
    let parameter_id = ParameterId::from_bytes([0xd3; 16]);
    let object_type = TypeId::from_bytes([0xd4; 16]);
    let function = FunctionDefinition::new(
        function_id,
        QualifiedSemanticName::new(["action_fixture", "call"]).unwrap(),
        FunctionDomain::Client,
        vec![ParameterDefinition::new(
            parameter_id,
            "p_value",
            0,
            ResolvedType::reference(object_type),
            None,
        )],
        FunctionReturn::Single(ResolvedType::Value(orna_standard::STD_ACTION_TYPE_ID)),
        function_revision,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let source_origin = SourceOrigin::new(SourceUnitId::from_bytes([0xd5; 16]), 0, 0).unwrap();
    let reference = |kind, target| {
        DefinitionReference::new(
            function_id,
            function_revision,
            0,
            target,
            kind,
            source_origin,
        )
    };

    assert!(super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(object_type),
        ),
    ));
    assert!(!super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes([0xd6; 16])),
        ),
    ));
    assert!(!super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(object_type),
        ),
    ));
    assert!(!super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(object_type),
        ),
    ));
}

#[test]
fn public_errors_and_rules_preserve_the_closed_adr0015_surface() {
    use orna_artifact::client_plan::ClientPlan;

    use super::{
        ClientActiveRevisionError, ClientExecutionContext, ClientExecutionError,
        ClientExecutionRule, ClientOpaqueValueError,
    };

    let (active, function, pair, function_revision) = version_one_active(true);
    let context = ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: orna_core::InvocationId::from_bytes([0; 16]),
        observer_lineage: None,
    };
    let rules = [
        (
            ClientExecutionRule::FunctionDomain,
            "this function does not run on the client",
        ),
        (
            ClientExecutionRule::Parameters,
            "this CLIENT function requires unsupported parameters",
        ),
        (
            ClientExecutionRule::ReturnType,
            "this CLIENT function has an unsupported return type",
        ),
        (
            ClientExecutionRule::Security,
            "this CLIENT function has an unsupported security mode",
        ),
        (
            ClientExecutionRule::Volatility,
            "this CLIENT function is not an immutable constant",
        ),
        (
            ClientExecutionRule::References,
            "this CLIENT function depends on unsupported definitions",
        ),
        (
            ClientExecutionRule::ArtifactFormat,
            "the saved CLIENT function uses an unsupported artefact format",
        ),
        (
            ClientExecutionRule::ArtifactVersion,
            "the saved CLIENT function uses an unsupported artefact version",
        ),
        (
            ClientExecutionRule::LanguageVersion,
            "the saved CLIENT function uses an unsupported language version",
        ),
    ];
    for (rule, display) in rules {
        assert_eq!(rule.to_string(), display);
        assert!(std::error::Error::source(&rule).is_none());
    }

    let mismatch = ClientActiveRevisionError::CatalogueHashMismatch;
    assert_eq!(
        mismatch.to_string(),
        "active revision catalogue hash differs from its canonical semantics"
    );
    assert!(std::error::Error::source(&mismatch).is_none());

    let not_found =
        evaluate_client_function(&active, FunctionId::from_bytes([0x77; 16])).unwrap_err();
    assert_eq!(not_found.pair(), pair);
    assert_eq!(not_found.function(), FunctionId::from_bytes([0x77; 16]));
    assert_eq!(not_found.context(), None);
    assert_eq!(
        not_found.to_string(),
        "the active revision does not contain this function"
    );
    assert!(std::error::Error::source(&not_found).is_none());

    let invalid = ClientExecutionError::InvalidFunction {
        context,
        rule: ClientExecutionRule::Security,
    };
    assert_eq!(invalid.pair(), pair);
    assert_eq!(invalid.function(), function);
    assert_eq!(invalid.context(), Some(&context));
    assert_eq!(
        invalid.to_string(),
        "this CLIENT function has an unsupported security mode"
    );
    assert!(std::error::Error::source(&invalid).is_none());

    let active_error = ClientExecutionError::InvalidActiveRevision {
        pair,
        function,
        source: mismatch,
    };
    assert_eq!(
        active_error.to_string(),
        "the active revision cannot be trusted"
    );
    assert!(std::error::Error::source(&active_error).is_some());

    let artifact_error = ClientPlan::decode(b"invalid").unwrap_err();
    let invalid_artifact = ClientExecutionError::InvalidArtifact {
        context,
        source: artifact_error,
    };
    assert!(invalid_artifact.context().is_some());
    assert!(std::error::Error::source(&invalid_artifact).is_some());
    assert_eq!(
        invalid_artifact.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );

    let opaque_error = ClientOpaqueValueError::TypeMismatch {
        expected: orna_standard::OPAQUE_TOKEN_TYPE_ID,
        actual: TypeId::from_bytes([0x78; 16]),
    };
    assert_eq!(
        opaque_error.to_string(),
        "opaque CLIENT plan type does not match its function return"
    );
    assert!(std::error::Error::source(&opaque_error).is_none());
    let invalid_opaque = ClientExecutionError::InvalidOpaqueValue {
        context,
        source: opaque_error,
    };
    assert_eq!(invalid_opaque.pair(), pair);
    assert_eq!(invalid_opaque.function(), function);
    assert_eq!(invalid_opaque.context(), Some(&context));
    assert_eq!(
        invalid_opaque.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );
    assert!(std::error::Error::source(&invalid_opaque).is_some());
}

#[test]
fn artefact_contract_failures_follow_closed_validation_after_active_trust() {
    let valid_payload = b"ORNACP\0\0\0\0\0\x01\x01\x01";
    let cases = [
        (
            "unsupported format",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "other.format",
                1,
                valid_payload.to_vec(),
                artifact_payload_digest(valid_payload).unwrap(),
            )
            .unwrap(),
            "orna.language/1",
            Some(super::ClientExecutionRule::ArtifactFormat),
        ),
        (
            "unsupported version",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "orna.client-plan",
                orna_artifact::client_plan::OPAQUE_FORMAT_VERSION,
                valid_payload.to_vec(),
                artifact_payload_digest(valid_payload).unwrap(),
            )
            .unwrap(),
            "orna.language/1",
            Some(super::ClientExecutionRule::ArtifactVersion),
        ),
        (
            "unsupported language",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "orna.client-plan",
                1,
                valid_payload.to_vec(),
                artifact_payload_digest(valid_payload).unwrap(),
            )
            .unwrap(),
            "orna.language/2",
            Some(super::ClientExecutionRule::LanguageVersion),
        ),
        (
            "undecodable plan",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "orna.client-plan",
                1,
                b"not a client plan".to_vec(),
                artifact_payload_digest(b"not a client plan").unwrap(),
            )
            .unwrap(),
            "orna.language/1",
            None,
        ),
    ];

    for (name, artifact, language, expected_rule) in cases {
        let (active, function, _, _) = version_one_active_with_artifact(artifact, language);
        let error = evaluate_client_function(&active, function).unwrap_err();

        assert_eq!(error.function(), function, "{name}");
        assert!(error.context().is_some(), "{name}");
        match expected_rule {
            Some(rule) => {
                assert!(matches!(
                    error,
                    super::ClientExecutionError::InvalidFunction { rule: actual, .. }
                        if actual == rule
                ));
                assert_eq!(error.to_string(), rule.to_string(), "{name}");
                assert!(std::error::Error::source(&error).is_none(), "{name}");
            }
            None => {
                assert!(matches!(
                    error,
                    super::ClientExecutionError::InvalidArtifact { .. }
                ));
                assert_eq!(
                    error.to_string(),
                    "the saved CLIENT function cannot be evaluated"
                );
                assert!(std::error::Error::source(&error).is_some());
            }
        }
    }
}

#[test]
fn function_shape_rules_are_public_and_follow_the_closed_precedence_order() {
    let cases = [
        (
            "domain before parameters",
            FunctionDomain::Server,
            vec![boolean_parameter()],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
            super::ClientExecutionRule::FunctionDomain,
        ),
        (
            "parameters before return type",
            FunctionDomain::Client,
            vec![boolean_parameter()],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
            super::ClientExecutionRule::Parameters,
        ),
        (
            "return type before security",
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
            FunctionSecurity::Definer,
            FunctionVolatility::Immutable,
            super::ClientExecutionRule::ReturnType,
        ),
        (
            "security before volatility",
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Definer,
            FunctionVolatility::Stable,
            super::ClientExecutionRule::Security,
        ),
        (
            "volatility",
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Stable,
            super::ClientExecutionRule::Volatility,
        ),
    ];

    for (name, domain, parameters, return_type, security, volatility, rule) in cases {
        let (active, function, pair, function_revision) =
            version_one_active_with_shape(domain, parameters, return_type, security, volatility);
        let error = evaluate_client_function(&active, function).unwrap_err();

        assert_eq!(error.pair(), pair, "{name}");
        assert_eq!(error.function(), function, "{name}");
        let context = error.context().expect("invalid function error context");
        assert_eq!(context.pair(), pair, "{name}");
        assert_eq!(context.function(), function, "{name}");
        assert_eq!(context.function_revision(), function_revision, "{name}");
        assert!(matches!(
            error,
            super::ClientExecutionError::InvalidFunction { rule: actual, .. }
                if actual == rule
        ));
        assert_eq!(error.to_string(), rule.to_string(), "{name}");
        assert!(std::error::Error::source(&error).is_none(), "{name}");
    }
}

#[test]
fn version_one_public_evaluation_accepts_only_a_legacy_boolean_single_return() {
    for scalar in StandardScalar::ALL {
        let (active, function, _, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(scalar)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let result = evaluate_client_function(&active, function);
        if scalar == StandardScalar::Boolean {
            assert_eq!(result.unwrap().value(), &RuntimeValue::Boolean(true));
            continue;
        }
        let error = result.unwrap_err();
        assert_return_type_rule(error);
    }

    for return_type in [
        FunctionReturn::Single(ResolvedType::named(TypeId::from_bytes([0x71; 16]))),
        FunctionReturn::Single(ResolvedType::reference(TypeId::from_bytes([0x72; 16]))),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )]),
    ] {
        let (active, function, _, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            return_type,
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        assert_return_type_rule(evaluate_client_function(&active, function).unwrap_err());
    }
}

fn assert_return_type_rule(error: super::ClientExecutionError) {
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidFunction {
            rule: super::ClientExecutionRule::ReturnType,
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "this CLIENT function has an unsupported return type"
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_references_rule(
    result: Result<super::ClientExecutionResult, super::ClientExecutionError>,
    function: FunctionId,
) {
    let error = result.unwrap_err();
    assert_eq!(error.function(), function);
    assert_eq!(
        error.to_string(),
        "this CLIENT function depends on unsupported definitions"
    );
    assert!(std::error::Error::source(&error).is_none());
    assert!(matches!(
        error,
        super::ClientExecutionError::InvalidFunction {
            rule: super::ClientExecutionRule::References,
            ..
        }
    ));
}

fn replace_reference(
    references: &mut [DefinitionReference],
    function: FunctionId,
    replacement: impl FnOnce(&DefinitionReference) -> DefinitionReference,
) {
    let index = references
        .iter()
        .position(|reference| reference.source_function() == function)
        .unwrap();
    references[index] = replacement(&references[index]);
}

fn prepared_client_constant(literal: &str) -> DeployableRevision {
    prepared_client_source(&format!(
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN {literal};"
    ))
}

fn prepared_client_source_v6(source: &str) -> DeployableRevision {
    let snapshot = orna_standard::retained_standard_library_v6_snapshot().unwrap();
    let verified = orna_standard::verify_standard_library_v6_snapshot(snapshot).unwrap();
    let standard = orna_compiler::check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context =
        orna_compiler::StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
            .unwrap();
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = orna_compiler::check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    orna_compiler::prepare_standard_application(&report, active.pair(), &active).unwrap()
}

fn active_with_reordered_client_call_references(
    prepared: &DeployableRevision,
    function_name: &str,
) -> (ActiveDatabaseRevision, FunctionId) {
    let function = prepared
        .candidate()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == function_name)
        .expect("the reordered-call owner is present")
        .id();
    let mut references = prepared.references().to_vec();
    let mut call_indices = references
        .iter()
        .enumerate()
        .filter(|(_, reference)| {
            reference.source_function() == function
                && reference.kind() == DefinitionReferenceKind::FunctionCall
        })
        .map(|(index, reference)| (index, reference.ordinal()))
        .collect::<Vec<_>>();
    call_indices.sort_unstable_by_key(|(_, ordinal)| *ordinal);
    assert!(
        call_indices.len() >= 2,
        "the fixture must contain two calls"
    );
    let first = references[call_indices[0].0].clone();
    let second = references[call_indices[1].0].clone();
    references[call_indices[0].0] = DefinitionReference::new(
        first.source_function(),
        first.source_revision(),
        first.ordinal(),
        second.target(),
        first.kind(),
        first.source_origin(),
    );
    references[call_indices[1].0] = DefinitionReference::new(
        second.source_function(),
        second.source_revision(),
        second.ordinal(),
        first.target(),
        second.kind(),
        second.source_origin(),
    );
    (
        active_from_prepared_with_references(prepared, references),
        function,
    )
}

fn prepared_client_source(source: &str) -> DeployableRevision {
    let snapshot = orna_standard::retained_standard_library_snapshot().unwrap();
    let verified = orna_standard::verify_standard_library_snapshot(snapshot).unwrap();
    let standard = orna_compiler::check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context =
        orna_compiler::StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
            .unwrap();
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = orna_compiler::check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    orna_compiler::prepare_standard_application(&report, active.pair(), &active).unwrap()
}

fn prepared_client_call_chain_with_state_root(
    call_edges: usize,
) -> (DeployableRevision, FunctionId) {
    assert!(call_edges > 0);

    let mut source = String::from("CREATE SCHEMA app; ");
    source.push_str(
            "CREATE CLIENT FUNCTION app.f0() RETURNS BOOLEAN IS STATE value BOOLEAN DEFAULT TRUE; BEGIN RETURN app.f1(); END; ",
        );
    for index in 1..call_edges {
        source.push_str(&format!(
            "CREATE CLIENT FUNCTION app.f{index}() RETURNS BOOLEAN RETURN app.f{}(); ",
            index + 1
        ));
    }
    source.push_str(&format!(
        "CREATE CLIENT FUNCTION app.f{call_edges}() RETURNS BOOLEAN RETURN TRUE;"
    ));

    let prepared = prepared_client_source_v6(&source);
    let function = prepared
        .candidate()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.f0")
        .expect("the root CLIENT function is present")
        .id();
    (prepared, function)
}

fn prepared_client_functions() -> DeployableRevision {
    let snapshot = orna_standard::retained_standard_library_snapshot().unwrap();
    let verified = orna_standard::verify_standard_library_snapshot(snapshot).unwrap();
    let standard = orna_compiler::check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context =
        orna_compiler::StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
            .unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; \
             CREATE TYPE app.item AS OBJECT (); \
             CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE; \
             CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let report = orna_compiler::check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    orna_compiler::prepare_standard_application(&report, active.pair(), &active).unwrap()
}

fn empty_version_two_active(
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x41; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = orna_core::revision::CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = orna_core::canonical_hash::catalogue_digest_with_context(
        &context,
        &catalogue,
        &[],
        &[],
        &[],
        &[],
    )
    .unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn active_from_prepared_candidate(prepared: &DeployableRevision) -> ActiveDatabaseRevision {
    active_from_prepared_with_references(prepared, prepared.references().to_vec())
}

fn active_from_prepared_with_semantic_versions(
    prepared: &DeployableRevision,
    semantic_hash_version: FunctionSemanticHashVersion,
    references: Vec<DefinitionReference>,
) -> ActiveDatabaseRevision {
    active_from_prepared_with_current_revisions(prepared, references, |revision| {
        semantic_hash_version_for(revision, semantic_hash_version)
    })
    .unwrap()
}

fn active_from_prepared_with_references_result(
    prepared: &DeployableRevision,
    references: Vec<DefinitionReference>,
) -> Result<ActiveDatabaseRevision, Box<dyn std::error::Error>> {
    active_from_prepared_with_current_revisions(prepared, references, |revision| {
        revision.semantic_hash_version()
    })
}

fn active_from_prepared_with_references(
    prepared: &DeployableRevision,
    references: Vec<DefinitionReference>,
) -> ActiveDatabaseRevision {
    active_from_prepared_with_references_result(prepared, references).unwrap()
}

fn active_from_prepared_with_current_revisions(
    prepared: &DeployableRevision,
    references: Vec<DefinitionReference>,
    semantic_hash_version: impl Fn(&FunctionRevisionRecord) -> FunctionSemanticHashVersion,
) -> Result<ActiveDatabaseRevision, Box<dyn std::error::Error>> {
    let current_function_revisions = prepared
        .current_function_revisions()
        .unwrap()
        .iter()
        .map(|revision| {
            let function = prepared
                .candidate()
                .function_by_id(revision.function())
                .unwrap();
            let version = semantic_hash_version(revision);
            let function_references = references
                .iter()
                .filter(|reference| reference.source_function() == revision.function())
                .cloned()
                .collect::<Vec<_>>();
            let semantic_hash = function_semantic_digest_with_version(
                version,
                function,
                revision.language_version(),
                revision.artifact(),
                prepared.expressions(),
                &function_references,
            )?;
            Ok::<_, Box<dyn std::error::Error>>(
                FunctionRevisionRecord::new(
                    revision.function(),
                    revision.id(),
                    revision.revision_number(),
                    revision.declaration_origin(),
                    revision.declaration_content_hash(),
                    semantic_hash,
                    revision.language_version(),
                    revision.artifact().clone(),
                )?
                .with_semantic_hash_version(version),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let context = prepared.catalogue_hash_context().clone();
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        prepared.candidate(),
        &current_function_revisions,
        prepared.expressions(),
        prepared.origins(),
        &references,
    )?;
    Ok(ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                prepared.expressions().to_vec(),
                current_function_revisions,
                prepared.origins().to_vec(),
                references,
            ),
        ),
        context,
    )?)
}

fn active_with_extra_reference(
    active: &ActiveDatabaseRevision,
    extra: DefinitionReference,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    let mut references = active.references().to_vec();
    references.push(extra);
    active_with_content(
        active,
        active.source().clone(),
        active.origins().to_vec(),
        references,
    )
}

fn active_with_mismatched_function_artifact_payload_hash(
    active: &ActiveDatabaseRevision,
) -> ActiveDatabaseRevision {
    let current = &active.function_revisions()[0];
    let artifact = ExecutableArtifact::new(
        current.artifact().kind(),
        current.artifact().format(),
        current.artifact().version(),
        current.artifact().payload().to_vec(),
        orna_core::revision::Sha256Digest::from_bytes([0x8e; 32]),
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        current.function(),
        current.id(),
        current.revision_number(),
        current.declaration_origin(),
        current.declaration_content_hash(),
        current.semantic_hash(),
        current.language_version(),
        artifact,
    )
    .unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                vec![revision],
                active.origins().to_vec(),
                active.references().to_vec(),
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap()
}

fn active_with_replaced_first_origin(
    active: &ActiveDatabaseRevision,
    source_origin: SourceOrigin,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    active_with_source_and_first_origin(active, active.source().clone(), source_origin)
}

fn active_with_replaced_reference_origin(
    active: &ActiveDatabaseRevision,
    source: StoredSourceRevision,
    function: FunctionId,
    source_origin: SourceOrigin,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    let mut references = active.references().to_vec();
    replace_reference(&mut references, function, |reference| {
        DefinitionReference::new(
            reference.source_function(),
            reference.source_revision(),
            reference.ordinal(),
            reference.target(),
            reference.kind(),
            source_origin,
        )
    });
    active_with_content(active, source, active.origins().to_vec(), references)
}

fn active_with_source_and_first_origin(
    active: &ActiveDatabaseRevision,
    source: StoredSourceRevision,
    source_origin: SourceOrigin,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    let mut origins = active.origins().to_vec();
    origins[0] = DefinitionOrigin::new(origins[0].identity(), source_origin);
    active_with_content(active, source, origins, active.references().to_vec())
}

fn active_with_content(
    active: &ActiveDatabaseRevision,
    source: StoredSourceRevision,
    origins: Vec<DefinitionOrigin>,
    references: Vec<DefinitionReference>,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            source,
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                active.function_revisions().to_vec(),
                origins,
                references,
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
}
fn standard_v9() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_v9_snapshot(
        orna_standard::retained_standard_library_v9_snapshot().unwrap(),
    )
    .unwrap()
}
fn active_with_application_ui_text_identity() -> ActiveDatabaseRevision {
    let (base, _, pair, _) = version_one_active(true);
    let prior_function = base.catalogue().functions()[0].clone();
    let function = FunctionDefinition::new(
        super::STD_UI_TEXT_FUNCTION_ID,
        QualifiedSemanticName::new(["app", "same_text"]).unwrap(),
        prior_function.domain(),
        prior_function.parameters().to_vec(),
        prior_function.return_type().clone(),
        super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        prior_function.security(),
        prior_function.transaction(),
        prior_function.volatility(),
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        base.catalogue().object_types().to_vec(),
        vec![function.clone()],
    )
    .unwrap();
    let prior_revision = &base.function_revisions()[0];
    let artifact = prior_revision.artifact().clone();
    let semantic_hash = function_semantic_digest(
        &function,
        prior_revision.language_version(),
        &artifact,
        base.expressions(),
        &[],
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        super::STD_UI_TEXT_FUNCTION_ID,
        super::STD_UI_TEXT_FUNCTION_REVISION_ID,
        prior_revision.revision_number(),
        prior_revision.declaration_origin(),
        prior_revision.declaration_content_hash(),
        semantic_hash,
        prior_revision.language_version(),
        artifact,
    )
    .unwrap();
    let origins = base
        .origins()
        .iter()
        .map(|origin| match origin.identity() {
            DefinitionIdentity::Function(_) => DefinitionOrigin::new(
                DefinitionIdentity::Function(super::STD_UI_TEXT_FUNCTION_ID),
                origin.source(),
            ),
            _ => origin.clone(),
        })
        .collect::<Vec<_>>();
    let context = base.catalogue_hash_context().clone();
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        base.expressions(),
        &origins,
        &[],
    )
    .unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            base.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                base.expressions().to_vec(),
                vec![revision],
                origins,
                Vec::new(),
            ),
        ),
        context,
    )
    .unwrap()
}

fn replacement_source(active: &ActiveDatabaseRevision, content: &str) -> StoredSourceRevision {
    let old = active.source();
    let old_unit = &old.units()[0];
    let replacement = StoredSourceUnit::new(
        old_unit.id(),
        0,
        old_unit.logical_path(),
        content,
        source_unit_content_digest(content).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&replacement)).unwrap();
    StoredSourceRevision::new(
        old.bundle(),
        old.id(),
        old.parent(),
        vec![replacement],
        bundle_hash,
        source_revision_record_digest(old.bundle(), old.parent(), bundle_hash).unwrap(),
    )
    .unwrap()
}

const fn semantic_hash_version_for(
    _revision: &FunctionRevisionRecord,
    semantic_hash_version: FunctionSemanticHashVersion,
) -> FunctionSemanticHashVersion {
    semantic_hash_version
}
fn standard_v5() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap()
}

fn standard_v6() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_v6_snapshot(
        orna_standard::retained_standard_library_v6_snapshot().unwrap(),
    )
    .unwrap()
}
fn standard_v7() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_v7_snapshot(
        orna_standard::retained_standard_library_v7_snapshot().unwrap(),
    )
    .unwrap()
}

fn version_two_value_active(
    return_type: TypeId,
    reference_target: TypeId,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_active_with_artifact(
        standard_v5(),
        return_type,
        DefinitionReferenceTarget::ValueType(reference_target),
        DefinitionReferenceKind::NamedType,
        1,
        b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
    )
}

fn version_two_opaque_active(
    plan_type: TypeId,
    payload: [u8; 16],
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_active_with_artifact(
        standard_v5(),
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::OPAQUE_TOKEN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::OPAQUE_FORMAT_VERSION,
        orna_artifact::client_plan::OpaqueClientPlan::return_opaque(plan_type, payload)
            .encode()
            .expect("opaque plan encodes"),
    )
}

fn version_two_value_active_with_artifact(
    return_type: TypeId,
    reference_target: TypeId,
    artifact_version: u32,
    payload: Vec<u8>,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_active_with_artifact(
        standard_v5(),
        return_type,
        DefinitionReferenceTarget::ValueType(reference_target),
        DefinitionReferenceKind::NamedType,
        artifact_version,
        payload,
    )
}

fn version_two_client_call_active() -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::Function(FunctionId::from_bytes([6; 16])),
        DefinitionReferenceKind::FunctionCall,
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        )
        .encode()
        .unwrap(),
    )
}

fn version_two_local_action_active() -> (
    ActiveDatabaseRevision,
    FunctionId,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let (base, parent_id, pair, parent_revision_id) = version_one_active(true);
    let target_id = FunctionId::from_bytes([0xc2; 16]);
    let target_revision_id = FunctionRevisionId::from_bytes([0xc3; 16]);
    let previous_revision = &base.function_revisions()[0];
    let parent_name = base
        .catalogue()
        .function_by_id(parent_id)
        .unwrap()
        .name()
        .clone();
    let parent = FunctionDefinition::new(
        parent_id,
        parent_name,
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
        parent_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let target = FunctionDefinition::new(
        target_id,
        QualifiedSemanticName::new(["app", "action_target"]).unwrap(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
        target_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        base.catalogue().object_types().to_vec(),
        vec![parent.clone(), target.clone()],
    )
    .unwrap();
    let parent_payload = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Call {
            function: target_id,
            arguments: Vec::new(),
        },
    )
    .encode()
    .unwrap();
    let target_payload = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
    )
    .encode()
    .unwrap();
    let parent_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        parent_payload.clone(),
        artifact_payload_digest(&parent_payload).unwrap(),
    )
    .unwrap();
    let target_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        target_payload.clone(),
        artifact_payload_digest(&target_payload).unwrap(),
    )
    .unwrap();
    let parent_reference = DefinitionReference::new(
        parent_id,
        parent_revision_id,
        0,
        DefinitionReferenceTarget::Function(target_id),
        DefinitionReferenceKind::FunctionCall,
        previous_revision.declaration_origin(),
    );
    let parent_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &parent,
        previous_revision.language_version(),
        &parent_artifact,
        base.expressions(),
        std::slice::from_ref(&parent_reference),
    )
    .unwrap();
    let target_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &target,
        previous_revision.language_version(),
        &target_artifact,
        base.expressions(),
        &[],
    )
    .unwrap();
    let parent_revision = FunctionRevisionRecord::new(
        parent_id,
        parent_revision_id,
        previous_revision.revision_number(),
        previous_revision.declaration_origin(),
        previous_revision.declaration_content_hash(),
        parent_semantic_hash,
        previous_revision.language_version(),
        parent_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let target_origin = SourceOrigin::new(
        previous_revision.declaration_origin().source_unit(),
        previous_revision.declaration_origin().byte_start(),
        previous_revision.declaration_origin().byte_end(),
    )
    .unwrap();
    let target_revision = FunctionRevisionRecord::new(
        target_id,
        target_revision_id,
        previous_revision.revision_number(),
        target_origin,
        previous_revision.declaration_content_hash(),
        target_semantic_hash,
        previous_revision.language_version(),
        target_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let mut origins = base.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(target_id),
        target_origin,
    ));
    let revisions = vec![parent_revision, target_revision];
    let standard = orna_standard::verify_standard_library_v6_snapshot(
        orna_standard::retained_standard_library_v6_snapshot().unwrap(),
    )
    .unwrap();
    let context = orna_core::revision::CatalogueHashContext::version_two(standard);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        &revisions,
        base.expressions(),
        &origins,
        std::slice::from_ref(&parent_reference),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            base.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                base.expressions().to_vec(),
                revisions,
                origins,
                vec![parent_reference],
            ),
        ),
        context,
    )
    .unwrap();

    (active, parent_id, target_id, pair, parent_revision_id)
}
fn version_two_active_with_artifact(
    standard: VerifiedStandardLibrarySnapshot,
    return_type: TypeId,
    reference_target: DefinitionReferenceTarget,
    reference_kind: DefinitionReferenceKind,
    artifact_version: u32,
    payload: Vec<u8>,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_active_with_function_return(
        standard,
        FunctionReturn::Single(ResolvedType::Value(return_type)),
        reference_target,
        reference_kind,
        artifact_version,
        payload,
    )
}

fn version_two_client_stream_active_with_artifact(
    standard: VerifiedStandardLibrarySnapshot,
    item_type: TypeId,
    reference_target: DefinitionReferenceTarget,
    reference_kind: DefinitionReferenceKind,
    artifact_version: u32,
    payload: Vec<u8>,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_active_with_function_return(
        standard,
        FunctionReturn::Stream(ResolvedType::Value(item_type)),
        reference_target,
        reference_kind,
        artifact_version,
        payload,
    )
}

fn version_two_active_with_function_return(
    standard: VerifiedStandardLibrarySnapshot,
    function_return: FunctionReturn,
    reference_target: DefinitionReferenceTarget,
    reference_kind: DefinitionReferenceKind,
    artifact_version: u32,
    payload: Vec<u8>,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let (version_one, function_id, pair, function_revision_id) = version_one_active(true);
    let prior_function = version_one.catalogue().function_by_id(function_id).unwrap();
    let function = FunctionDefinition::new(
        function_id,
        prior_function.name().clone(),
        FunctionDomain::Client,
        Vec::new(),
        function_return,
        function_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        version_one.catalogue().revision(),
        version_one.catalogue().schemas().to_vec(),
        version_one.catalogue().object_types().to_vec(),
        vec![function.clone()],
    )
    .unwrap();
    let prior_revision = &version_one.function_revisions()[0];
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        artifact_version,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let reference = DefinitionReference::new(
        function_id,
        function_revision_id,
        0,
        reference_target,
        reference_kind,
        prior_revision.declaration_origin(),
    );
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        prior_revision.language_version(),
        &artifact,
        version_one.expressions(),
        std::slice::from_ref(&reference),
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function_id,
        function_revision_id,
        prior_revision.revision_number(),
        prior_revision.declaration_origin(),
        prior_revision.declaration_content_hash(),
        semantic_hash,
        prior_revision.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let context = orna_core::revision::CatalogueHashContext::version_two(standard);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        version_one.expressions(),
        version_one.origins(),
        std::slice::from_ref(&reference),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            version_one.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                version_one.expressions().to_vec(),
                vec![revision],
                version_one.origins().to_vec(),
                vec![reference],
            ),
        ),
        context,
    )
    .unwrap();

    (active, function_id, pair, function_revision_id)
}

fn version_two_server_rows_active() -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_server_active(FunctionReturn::Rows(vec![
        FunctionReturnColumnDefinition::new(
            "first",
            0,
            ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
        ),
        FunctionReturnColumnDefinition::new(
            "second",
            1,
            ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
        ),
    ]))
}

fn version_two_server_stream_active() -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    version_two_server_active(FunctionReturn::Stream(ResolvedType::value(
        orna_standard::BOOLEAN_TYPE_ID,
    )))
}

fn version_two_server_active(
    return_type: FunctionReturn,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let (initial, function_id, pair, function_revision_id) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        1,
        b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
    );
    let prior_function = initial.catalogue().function_by_id(function_id).unwrap();
    let function = FunctionDefinition::new(
        function_id,
        prior_function.name().clone(),
        FunctionDomain::Server,
        Vec::new(),
        return_type,
        function_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        initial.catalogue().revision(),
        initial.catalogue().schemas().to_vec(),
        initial.catalogue().object_types().to_vec(),
        vec![function.clone()],
    )
    .unwrap();
    let prior_revision = &initial.function_revisions()[0];
    let payload = vec![0x53];
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        prior_revision.language_version(),
        &artifact,
        initial.expressions(),
        &[],
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function_id,
        function_revision_id,
        prior_revision.revision_number(),
        prior_revision.declaration_origin(),
        prior_revision.declaration_content_hash(),
        semantic_hash,
        prior_revision.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let mut origins = initial.origins().to_vec();
    if let FunctionReturn::Rows(columns) = function.return_type() {
        origins.extend(columns.iter().map(|column| {
            DefinitionOrigin::new(
                DefinitionIdentity::FunctionReturnColumn {
                    owner: function_id,
                    ordinal: column.ordinal(),
                },
                prior_revision.declaration_origin(),
            )
        }));
    }
    let context = initial.catalogue_hash_context().clone();
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        initial.expressions(),
        &origins,
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            initial.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                initial.expressions().to_vec(),
                vec![revision],
                origins,
                Vec::new(),
            ),
        ),
        context,
    )
    .unwrap();
    (active, function_id, pair, function_revision_id)
}

fn version_two_server_record_stream_active() -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
    TypeId,
    TypeId,
) {
    const RECORD_TYPE: TypeId = TypeId::from_bytes([0x91; 16]);
    const OTHER_RECORD_TYPE: TypeId = TypeId::from_bytes([0x92; 16]);
    const FIELD_ID: FieldId = FieldId::from_bytes([0x93; 16]);
    const OTHER_FIELD_ID: FieldId = FieldId::from_bytes([0x94; 16]);

    let (initial, function_id, pair, function_revision_id) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        1,
        b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
    );
    let prior_function = initial.catalogue().function_by_id(function_id).unwrap();
    let function = FunctionDefinition::new(
        function_id,
        prior_function.name().clone(),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Stream(ResolvedType::Named(RECORD_TYPE)),
        function_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );
    let record = RecordValueTypeDefinition::new(
        RECORD_TYPE,
        QualifiedSemanticName::new(["app", "event"]).unwrap(),
        vec![
            RecordValueFieldDefinition::try_new_descriptor(
                FIELD_ID,
                "title",
                0,
                TypeDescriptor::named(orna_standard::BOOLEAN_TYPE_ID),
            )
            .unwrap(),
        ],
    );
    let other_record = RecordValueTypeDefinition::new(
        OTHER_RECORD_TYPE,
        QualifiedSemanticName::new(["app", "other_event"]).unwrap(),
        vec![
            RecordValueFieldDefinition::try_new_descriptor(
                OTHER_FIELD_ID,
                "title",
                0,
                TypeDescriptor::named(orna_standard::BOOLEAN_TYPE_ID),
            )
            .unwrap(),
        ],
    );
    let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
        initial.catalogue().revision(),
        initial.catalogue().schemas().to_vec(),
        initial.catalogue().object_types().to_vec(),
        initial.catalogue().value_types().to_vec(),
        initial.catalogue().enum_types().to_vec(),
        vec![record, other_record],
        initial.catalogue().type_bindings().to_vec(),
        vec![function.clone()],
    )
    .unwrap();

    let prior_revision = &initial.function_revisions()[0];
    let target_origin = prior_revision.declaration_origin();
    let reference = DefinitionReference::new(
        function_id,
        function_revision_id,
        0,
        DefinitionReferenceTarget::ValueType(RECORD_TYPE),
        DefinitionReferenceKind::NamedType,
        target_origin,
    );
    let payload = vec![0x53];
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        prior_revision.language_version(),
        &artifact,
        initial.expressions(),
        std::slice::from_ref(&reference),
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function_id,
        function_revision_id,
        prior_revision.revision_number(),
        target_origin,
        prior_revision.declaration_content_hash(),
        semantic_hash,
        prior_revision.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let mut origins = initial.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ValueType(RECORD_TYPE),
        target_origin,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Field {
            owner: RECORD_TYPE,
            field: FIELD_ID,
        },
        target_origin,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ValueType(OTHER_RECORD_TYPE),
        target_origin,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Field {
            owner: OTHER_RECORD_TYPE,
            field: OTHER_FIELD_ID,
        },
        target_origin,
    ));
    let context = initial.catalogue_hash_context().clone();
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        initial.expressions(),
        &origins,
        std::slice::from_ref(&reference),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            initial.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                initial.expressions().to_vec(),
                vec![revision],
                origins,
                vec![reference],
            ),
        ),
        context,
    )
    .unwrap();

    (
        active,
        function_id,
        pair,
        function_revision_id,
        RECORD_TYPE,
        OTHER_RECORD_TYPE,
    )
}

fn version_four_state_active(
    return_type: TypeId,
    payload: Vec<u8>,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let (version_one, function_id, pair, function_revision_id) = version_one_active(true);
    let prior_function = version_one.catalogue().function_by_id(function_id).unwrap();
    let function = FunctionDefinition::new(
        function_id,
        prior_function.name().clone(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Value(return_type)),
        function_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        version_one.catalogue().revision(),
        version_one.catalogue().schemas().to_vec(),
        version_one.catalogue().object_types().to_vec(),
        vec![function.clone()],
    )
    .unwrap();
    let prior_revision = &version_one.function_revisions()[0];
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::STATE_FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        prior_revision.language_version(),
        &artifact,
        version_one.expressions(),
        &[],
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function_id,
        function_revision_id,
        prior_revision.revision_number(),
        prior_revision.declaration_origin(),
        prior_revision.declaration_content_hash(),
        semantic_hash,
        prior_revision.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let context = orna_core::revision::CatalogueHashContext::version_two(standard);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        version_one.expressions(),
        version_one.origins(),
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            version_one.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                version_one.expressions().to_vec(),
                vec![revision],
                version_one.origins().to_vec(),
                Vec::new(),
            ),
        ),
        context,
    )
    .unwrap();

    (active, function_id, pair, function_revision_id)
}

fn version_one_active(
    value: bool,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let source = match value {
        true => {
            "CREATE SCHEMA app;\nCREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;"
        }
        false => {
            "CREATE SCHEMA app;\nCREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN FALSE;"
        }
    };
    let function_start = "CREATE SCHEMA app;\n".len();
    let source_unit_id = SourceUnitId::from_bytes([1; 16]);
    let source_bundle_id = SourceBundleId::from_bytes([2; 16]);
    let source_revision_id = SourceRevisionId::from_bytes([3; 16]);
    let catalogue_revision_id = CatalogueRevisionId::from_bytes([4; 16]);
    let schema_id = SchemaId::from_bytes([5; 16]);
    let function_id = FunctionId::from_bytes([6; 16]);
    let function_revision_id = FunctionRevisionId::from_bytes([7; 16]);
    let pair = RevisionPair::new(source_revision_id, catalogue_revision_id);

    let source_unit = StoredSourceUnit::new(
        source_unit_id,
        0,
        "application.orna",
        source,
        source_unit_content_digest(source).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let stored_source = StoredSourceRevision::new(
        source_bundle_id,
        source_revision_id,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(source_bundle_id, None, bundle_hash).unwrap(),
    )
    .unwrap();

    let schema = SchemaDefinition::new(schema_id, QualifiedSemanticName::new(["app"]).unwrap());
    let function = FunctionDefinition::new(
        function_id,
        QualifiedSemanticName::new(["app", "enabled"]).unwrap(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        function_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        catalogue_revision_id,
        vec![schema],
        Vec::new(),
        vec![function.clone()],
    )
    .unwrap();

    let payload = match value {
        true => b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
        false => b"ORNACP\0\0\0\0\0\x01\x01\x00".to_vec(),
    };
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        1,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let function_origin = SourceOrigin::new(
        source_unit_id,
        u32::try_from(function_start).unwrap(),
        u32::try_from(source.len()).unwrap(),
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function_id,
        function_revision_id,
        1,
        function_origin,
        function_declaration_digest(&source.as_bytes()[function_start..]).unwrap(),
        function_semantic_digest(&function, "orna.language/1", &artifact, &[], &[]).unwrap(),
        "orna.language/1",
        artifact,
    )
    .unwrap();
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            SourceOrigin::new(
                source_unit_id,
                0,
                u32::try_from(function_start - 1).unwrap(),
            )
            .unwrap(),
        ),
        DefinitionOrigin::new(DefinitionIdentity::Function(function_id), function_origin),
    ];
    let catalogue_hash = catalogue_digest(
        &catalogue,
        std::slice::from_ref(&revision),
        &[],
        &origins,
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new(
        pair,
        stored_source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        vec![revision],
        origins,
        Vec::new(),
    )
    .unwrap();

    (active, function_id, pair, function_revision_id)
}
fn active_with_revision_pair(
    active: &ActiveDatabaseRevision,
    pair: RevisionPair,
) -> ActiveDatabaseRevision {
    let source = StoredSourceRevision::new(
        active.source().bundle(),
        pair.source(),
        active.source().parent(),
        active.source().units().to_vec(),
        active.source().bundle_hash(),
        active.source().revision_hash(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_functions(
        pair.catalogue(),
        active.catalogue().schemas().to_vec(),
        active.catalogue().object_types().to_vec(),
        active.catalogue().functions().to_vec(),
    )
    .unwrap();

    let catalogue_hash = catalogue_digest_with_context(
        active.catalogue_hash_context(),
        &catalogue,
        active.function_revisions(),
        active.expressions(),
        active.origins(),
        active.references(),
    )
    .unwrap();

    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                active.function_revisions().to_vec(),
                active.origins().to_vec(),
                active.references().to_vec(),
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap()
}

fn version_one_active_with_artifact(
    artifact: ExecutableArtifact,
    language_version: &str,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let (initial, function, pair, function_revision) = version_one_active(true);
    let definition = initial.catalogue().function_by_id(function).unwrap();
    let previous = &initial.function_revisions()[0];
    let semantic_hash = function_semantic_digest(
        definition,
        language_version,
        &artifact,
        initial.expressions(),
        &[],
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function,
        function_revision,
        previous.revision_number(),
        previous.declaration_origin(),
        previous.declaration_content_hash(),
        semantic_hash,
        language_version,
        artifact,
    )
    .unwrap();
    let catalogue_hash = catalogue_digest(
        initial.catalogue(),
        std::slice::from_ref(&revision),
        initial.expressions(),
        initial.origins(),
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new(
        pair,
        initial.source().clone(),
        initial.catalogue().clone(),
        catalogue_hash,
        initial.expressions().to_vec(),
        vec![revision],
        initial.origins().to_vec(),
        Vec::new(),
    )
    .unwrap();

    (active, function, pair, function_revision)
}

fn version_five_boolean_envelope(
    value: bool,
    requirements: Vec<orna_artifact::client_plan::CapabilityRequirement>,
) -> Vec<u8> {
    orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Boolean(
            orna_artifact::client_plan::ClientPlan::return_boolean(value),
        ),
        requirements,
    )
    .encode()
    .expect("the version-5 capability envelope encodes")
}

fn version_five_boolean_active(
    payload: Vec<u8>,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    version_one_active_with_artifact(artifact, "orna.language/1")
}

fn collect_fixture_expression_call_targets(
    expression: &orna_artifact::client_plan::ClientExpressionNode,
    targets: &mut Vec<FunctionId>,
) {
    use orna_artifact::client_plan::ClientExpressionNode;

    match expression {
        ClientExpressionNode::Await { expression } => {
            collect_fixture_expression_call_targets(expression, targets);
        }
        ClientExpressionNode::Resource { operation } => {
            for (_, expression) in operation.arguments() {
                collect_fixture_expression_call_targets(expression, targets);
            }
            targets.push(operation.target_function());
        }
        ClientExpressionNode::Action { operation } => {
            for (_, expression) in operation.arguments() {
                collect_fixture_expression_call_targets(expression, targets);
            }
            targets.push(operation.target_function());
        }
        ClientExpressionNode::Inspect { operation } => {
            if let Some(expression) = operation.target() {
                collect_fixture_expression_call_targets(expression, targets);
            }
            if let Some(expression) = operation.options() {
                collect_fixture_expression_call_targets(expression, targets);
            }
            if let Some(expression) = operation.snapshot_expression() {
                collect_fixture_expression_call_targets(expression, targets);
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            for (_, expression) in arguments {
                collect_fixture_expression_call_targets(expression, targets);
            }
            targets.push(*function);
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            collect_fixture_expression_call_targets(left, targets);
            collect_fixture_expression_call_targets(right, targets);
        }
        ClientExpressionNode::Unary { expression, .. } => {
            collect_fixture_expression_call_targets(expression, targets);
        }
        ClientExpressionNode::Input | ClientExpressionNode::Evaluate { .. } => {}
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
    }
}

fn fixture_client_call_references(
    function: FunctionId,
    revision: FunctionRevisionId,
    origin: SourceOrigin,
    payload: &[u8],
) -> Vec<DefinitionReference> {
    let plan = orna_artifact::client_plan::CapabilityClientPlan::decode(payload)
        .expect("the capability fixture payload decodes");
    let mut targets = Vec::new();
    match plan.inner_plan() {
        orna_artifact::client_plan::InnerClientPlan::Boolean(_)
        | orna_artifact::client_plan::InnerClientPlan::Opaque(_) => {}
        orna_artifact::client_plan::InnerClientPlan::Expression(inner) => {
            collect_fixture_expression_call_targets(inner.expression(), &mut targets);
        }
        orna_artifact::client_plan::InnerClientPlan::State(inner) => {
            for slot in inner.slots() {
                if let orna_artifact::client_plan::StateDefault::Expression(expression) =
                    slot.default()
                {
                    collect_fixture_expression_call_targets(expression, &mut targets);
                }
            }
            collect_fixture_expression_call_targets(inner.expression(), &mut targets);
        }
        orna_artifact::client_plan::InnerClientPlan::Procedural(inner) => {
            for statement in inner.statements() {
                collect_fixture_expression_call_targets(statement.expression(), &mut targets);
            }
            collect_fixture_expression_call_targets(inner.return_expression(), &mut targets);
        }
        orna_artifact::client_plan::InnerClientPlan::Action(inner) => {
            for (_, expression) in inner.operation().arguments() {
                collect_fixture_expression_call_targets(expression, &mut targets);
            }
            targets.push(inner.operation().target_function());
        }
        orna_artifact::client_plan::InnerClientPlan::ControlFlow(_) => {}
        orna_artifact::client_plan::InnerClientPlan::Resource(inner) => {
            collect_fixture_expression_call_targets(inner.expression(), &mut targets);
        }
    }
    targets
        .into_iter()
        .enumerate()
        .map(|(ordinal, target)| {
            DefinitionReference::new(
                function,
                revision,
                u32::try_from(ordinal).expect("fixture call ordinal fits"),
                DefinitionReferenceTarget::Function(target),
                DefinitionReferenceKind::FunctionCall,
                origin,
            )
        })
        .collect()
}

fn version_five_expression_active_with_parameter(
    payload: Vec<u8>,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
    ParameterId,
) {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let (version_one, function_id, pair, function_revision_id) = version_one_active(true);
    let prior_function = version_one.catalogue().function_by_id(function_id).unwrap();
    let parameter = ParameterDefinition::new(
        ParameterId::from_bytes([0xb1; 16]),
        "p_path",
        0,
        ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        None,
    );
    let function = FunctionDefinition::new(
        function_id,
        prior_function.name().clone(),
        FunctionDomain::Client,
        vec![parameter.clone()],
        FunctionReturn::Single(ResolvedType::Value(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        )),
        function_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let resource_parameter = ParameterDefinition::new(
        ParameterId::from_bytes([0xd3; 16]),
        "p_resource",
        0,
        ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        None,
    );
    let resource_target = FunctionDefinition::new(
        FunctionId::from_bytes([0xd1; 16]),
        QualifiedSemanticName::new(["app", "resource"]).unwrap(),
        FunctionDomain::Server,
        vec![resource_parameter.clone()],
        FunctionReturn::Single(ResolvedType::Value(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        )),
        FunctionRevisionId::from_bytes([0xd2; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        version_one.catalogue().revision(),
        version_one.catalogue().schemas().to_vec(),
        version_one.catalogue().object_types().to_vec(),
        vec![function.clone(), resource_target.clone()],
    )
    .unwrap();
    let prior_revision = &version_one.function_revisions()[0];
    let origin = prior_revision.declaration_origin();
    let references =
        fixture_client_call_references(function_id, function_revision_id, origin, &payload);
    let resource_payload = vec![0x53];
    let resource_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        resource_payload.clone(),
        artifact_payload_digest(&resource_payload).unwrap(),
    )
    .unwrap();
    let resource_revision = FunctionRevisionRecord::new(
        resource_target.id(),
        FunctionRevisionId::from_bytes([0xd2; 16]),
        prior_revision.revision_number(),
        prior_revision.declaration_origin(),
        prior_revision.declaration_content_hash(),
        function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            &resource_target,
            prior_revision.language_version(),
            &resource_artifact,
            &[],
            &[],
        )
        .unwrap(),
        prior_revision.language_version(),
        resource_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        prior_revision.language_version(),
        &artifact,
        version_one.expressions(),
        &references,
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function_id,
        function_revision_id,
        prior_revision.revision_number(),
        prior_revision.declaration_origin(),
        prior_revision.declaration_content_hash(),
        semantic_hash,
        prior_revision.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let mut origins = version_one.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Parameter {
            owner: function_id,
            parameter: parameter.id(),
        },
        origin,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(resource_target.id()),
        origin,
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Parameter {
            owner: resource_target.id(),
            parameter: resource_parameter.id(),
        },
        origin,
    ));
    let revisions = vec![revision, resource_revision];
    let context = orna_core::revision::CatalogueHashContext::version_two(standard);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        &revisions,
        version_one.expressions(),
        &origins,
        &references,
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            version_one.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                version_one.expressions().to_vec(),
                revisions,
                origins,
                references.clone(),
            ),
        ),
        context,
    )
    .unwrap();

    (
        active,
        function_id,
        pair,
        function_revision_id,
        parameter.id(),
    )
}

fn version_six_client_resource_action_active() -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
    ParameterId,
) {
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([3; 16]),
        CatalogueRevisionId::from_bytes([4; 16]),
    );
    let target = FunctionId::from_bytes([0xd1; 16]);
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        orna_artifact::client_plan::ResourceKind::Scalar,
        target,
        pair,
        CallSiteId::from_bytes([0xe1; 16]),
        vec![(
            ParameterId::from_bytes([0xd3; 16]),
            orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                parameter: ParameterId::from_bytes([0xb1; 16]),
            },
        )],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Resource(
            orna_artifact::client_plan::ResourceClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Await {
                    expression: Box::new(
                        orna_artifact::client_plan::ClientExpressionNode::Resource { operation },
                    ),
                },
            ),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Parameter("p_path".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let (base, function, pair, revision, parameter) =
        version_five_expression_active_with_parameter(payload);
    let origin = base.function_revisions()[0].declaration_origin();
    let references = vec![DefinitionReference::new(
        function,
        revision,
        0,
        DefinitionReferenceTarget::Function(target),
        DefinitionReferenceKind::FunctionCall,
        origin,
    )];
    let mut revisions = base.function_revisions().to_vec();
    let client_revision = revisions
        .iter()
        .find(|candidate| candidate.function() == function)
        .unwrap()
        .clone();
    let client_definition = base.catalogue().function_by_id(function).unwrap();
    let client_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        client_definition,
        client_revision.language_version(),
        client_revision.artifact(),
        base.expressions(),
        &references,
    )
    .unwrap();
    let rebuilt_client_revision = FunctionRevisionRecord::new(
        client_revision.function(),
        client_revision.id(),
        client_revision.revision_number(),
        client_revision.declaration_origin(),
        client_revision.declaration_content_hash(),
        client_semantic_hash,
        client_revision.language_version(),
        client_revision.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let client_revision_index = revisions
        .iter()
        .position(|candidate| candidate.function() == function)
        .unwrap();
    revisions[client_revision_index] = rebuilt_client_revision;
    let context = orna_core::revision::CatalogueHashContext::version_two(standard_v6());
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        base.catalogue(),
        &revisions,
        base.expressions(),
        base.origins(),
        &references,
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            base.source().clone(),
            base.catalogue().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                base.expressions().to_vec(),
                revisions,
                base.origins().to_vec(),
                references,
            ),
        ),
        context,
    )
    .unwrap();
    (active, function, pair, revision, parameter)
}

fn version_six_client_action_provenance_active() -> (
    ActiveDatabaseRevision,
    FunctionId,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
    ParameterId,
) {
    let (base, child_id, pair, _child_revision_id, parameter) =
        version_six_client_resource_action_active();
    let previous_revision = base
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == child_id)
        .expect("resource child revision is present");
    let parent_id = FunctionId::from_bytes([0xc4; 16]);
    let parent_revision_id = FunctionRevisionId::from_bytes([0xc5; 16]);
    let parent_parameter = ParameterDefinition::new(
        ParameterId::from_bytes([0xc6; 16]),
        "p_path",
        0,
        ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        None,
    );
    let parent = FunctionDefinition::new(
        parent_id,
        QualifiedSemanticName::new(["app", "action_parent"]).unwrap(),
        FunctionDomain::Client,
        vec![parent_parameter.clone()],
        FunctionReturn::Single(ResolvedType::Value(
            orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
        )),
        parent_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let mut functions = base.catalogue().functions().to_vec();
    functions.push(parent.clone());
    let catalogue = CatalogueSnapshot::new_with_functions(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        base.catalogue().object_types().to_vec(),
        functions,
    )
    .unwrap();
    let parent_payload = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Call {
            function: child_id,
            arguments: vec![(
                ParameterId::from_bytes([0xb1; 16]),
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: parent_parameter.id(),
                },
            )],
        },
    )
    .encode()
    .unwrap();
    let parent_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        parent_payload.clone(),
        artifact_payload_digest(&parent_payload).unwrap(),
    )
    .unwrap();
    let parent_parameter_reference = DefinitionReference::new(
        parent_id,
        parent_revision_id,
        0,
        DefinitionReferenceTarget::Parameter {
            owner: parent_id,
            parameter: parent_parameter.id(),
        },
        DefinitionReferenceKind::ParameterRead,
        previous_revision.declaration_origin(),
    );
    let parent_reference = DefinitionReference::new(
        parent_id,
        parent_revision_id,
        1,
        DefinitionReferenceTarget::Function(child_id),
        DefinitionReferenceKind::FunctionCall,
        previous_revision.declaration_origin(),
    );
    let parent_references = vec![parent_parameter_reference, parent_reference.clone()];
    let parent_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &parent,
        previous_revision.language_version(),
        &parent_artifact,
        base.expressions(),
        &parent_references,
    )
    .unwrap();
    let parent_revision = FunctionRevisionRecord::new(
        parent_id,
        parent_revision_id,
        previous_revision.revision_number(),
        previous_revision.declaration_origin(),
        previous_revision.declaration_content_hash(),
        parent_semantic_hash,
        previous_revision.language_version(),
        parent_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let mut revisions = base.function_revisions().to_vec();
    revisions.push(parent_revision);
    let mut origins = base.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(parent_id),
        previous_revision.declaration_origin(),
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Parameter {
            owner: parent_id,
            parameter: parent_parameter.id(),
        },
        previous_revision.declaration_origin(),
    ));
    let mut references = base.references().to_vec();
    references.extend(parent_references);
    let catalogue_hash = catalogue_digest_with_context(
        base.catalogue_hash_context(),
        &catalogue,
        &revisions,
        base.expressions(),
        &origins,
        &references,
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            base.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(base.expressions().to_vec(), revisions, origins, references),
        ),
        base.catalogue_hash_context().clone(),
    )
    .unwrap();
    (
        active,
        parent_id,
        child_id,
        pair,
        parent_revision_id,
        parameter,
    )
}

fn action_value(
    active: &ActiveDatabaseRevision,
    domain: ActionTargetDomain,
    target: FunctionId,
    pair: RevisionPair,
    call_site: CallSiteId,
    arguments: Vec<FunctionArgument>,
    result_type: TypeId,
) -> RuntimeValue {
    let descriptor =
        ClientActionDescriptor::new(domain, target, pair, call_site, arguments, result_type);
    let payload = encode_action_payload(active, &descriptor).unwrap();
    let registry =
        super::registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap())
            .unwrap();
    RuntimeValue::Opaque(
        OpaqueValue::new(active, &registry, super::STD_ACTION_TYPE_ID, payload).unwrap(),
    )
}

fn boolean_parameter() -> ParameterDefinition {
    ParameterDefinition::new(
        ParameterId::from_bytes([0xa1; 16]),
        "enabled",
        0,
        ResolvedType::Scalar(StandardScalar::Boolean),
        None,
    )
}

fn version_one_active_with_shape(
    domain: FunctionDomain,
    parameters: Vec<ParameterDefinition>,
    return_type: FunctionReturn,
    security: FunctionSecurity,
    volatility: FunctionVolatility,
) -> (
    ActiveDatabaseRevision,
    FunctionId,
    RevisionPair,
    FunctionRevisionId,
) {
    let (initial, function, pair, function_revision) = version_one_active(true);
    let prior = initial.catalogue().function_by_id(function).unwrap();
    let definition = FunctionDefinition::new(
        function,
        prior.name().clone(),
        domain,
        parameters,
        return_type,
        function_revision,
        security,
        None,
        volatility,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        initial.catalogue().revision(),
        initial.catalogue().schemas().to_vec(),
        initial.catalogue().object_types().to_vec(),
        vec![definition.clone()],
    )
    .unwrap();
    let payload = match domain {
        FunctionDomain::Client => b"ORNACP\0\0\0\0\0\x01\x01\x01".to_vec(),
        FunctionDomain::Server => vec![0x53],
    };
    let (kind, format) = match domain {
        FunctionDomain::Client => (ExecutableArtifactKind::Client, "orna.client-plan"),
        FunctionDomain::Server => (ExecutableArtifactKind::Server, "orna.server-plan"),
    };
    let artifact = ExecutableArtifact::new(
        kind,
        format,
        1,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let prior_revision = &initial.function_revisions()[0];
    let revision = FunctionRevisionRecord::new(
        function,
        function_revision,
        prior_revision.revision_number(),
        prior_revision.declaration_origin(),
        prior_revision.declaration_content_hash(),
        function_semantic_digest(&definition, "orna.language/1", &artifact, &[], &[]).unwrap(),
        "orna.language/1",
        artifact,
    )
    .unwrap();
    let mut origins = initial.origins().to_vec();
    origins.extend(definition.parameters().iter().map(|parameter| {
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: function,
                parameter: parameter.id(),
            },
            prior_revision.declaration_origin(),
        )
    }));
    if let FunctionReturn::Rows(columns) = definition.return_type() {
        origins.extend(columns.iter().map(|column| {
            DefinitionOrigin::new(
                DefinitionIdentity::FunctionReturnColumn {
                    owner: function,
                    ordinal: column.ordinal(),
                },
                prior_revision.declaration_origin(),
            )
        }));
    }
    let catalogue_hash = catalogue_digest(
        &catalogue,
        std::slice::from_ref(&revision),
        initial.expressions(),
        &origins,
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new(
        pair,
        initial.source().clone(),
        catalogue,
        catalogue_hash,
        initial.expressions().to_vec(),
        vec![revision],
        origins,
        Vec::new(),
    )
    .unwrap();

    (active, function, pair, function_revision)
}

#[test]
fn action_trigger_rejects_domain_mismatch_and_stale_revision() {
    let (active, parent_function, pair, parent_revision, parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xf6; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);

    let domain_mismatch = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xf7; 16]),
        vec![argument.clone()],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    assert_eq!(
        trigger_client_action(
            &active,
            &domain_mismatch,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::TargetMismatch),
    );

    let stale_pair = RevisionPair::new(SourceRevisionId::from_bytes([0xf8; 16]), pair.catalogue());
    let stale_target_revision = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        stale_pair,
        CallSiteId::from_bytes([0xf9; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    assert_eq!(
        trigger_client_action(
            &active,
            &stale_target_revision,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::RevisionMismatch),
    );
}

#[test]
fn action_trigger_rejects_wrong_result_type_and_non_single_column_target() {
    let (active, parent_function, pair, parent_revision, parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xfa; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let wrong_type = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0xfb; 16]),
        vec![argument],
        orna_standard::INTEGER_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);
    assert_eq!(
        trigger_client_action(
            &active,
            &wrong_type,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::ResultTypeMismatch),
    );

    let (multi_column_active, multi_column_function, multi_column_pair, multi_column_revision) =
        version_two_server_rows_active();
    let multi_column_auth = authorise(multi_column_pair, multi_column_function);
    let multi_column_parent = ClientExecutionContext {
        pair: multi_column_pair,
        function: multi_column_function,
        function_revision: multi_column_revision,
        parent_invocation_id: InvocationId::from_bytes([0xfc; 16]),
        observer_lineage: None,
    };
    let multi_column_action = action_value(
        &multi_column_active,
        ActionTargetDomain::Server,
        multi_column_function,
        multi_column_pair,
        CallSiteId::from_bytes([0xfd; 16]),
        Vec::new(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let mut multi_column_state = ClientStateStore::default();
    let mut multi_column_action_state = ClientActionState::default();
    let mut multi_column_executor = RecordingActionExecutor::new(None);
    assert_eq!(
        trigger_client_action(
            &multi_column_active,
            &multi_column_action,
            &multi_column_auth,
            &multi_column_parent,
            &mut multi_column_action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut multi_column_state,
            &mut multi_column_executor,
        ),
        Err(ClientActionError::ResultTypeMismatch),
    );
}

#[test]
fn action_target_result_type_rejects_one_column_rows() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::Named(TypeId::from_bytes([0x66; 16])),
        )]),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Server,
        function,
        pair,
        CallSiteId::from_bytes([0xfe; 16]),
        Vec::new(),
        TypeId::from_bytes([0x66; 16]),
    );

    assert_eq!(
        action_target_result_type(&active, &descriptor),
        Err(ClientActionError::ResultTypeMismatch)
    );
}
#[test]
fn action_target_result_type_rejects_stream_targets() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Stream(ResolvedType::Scalar(StandardScalar::Integer)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Server,
        function,
        pair,
        CallSiteId::from_bytes([0x70; 16]),
        Vec::new(),
        orna_standard::INTEGER_TYPE_ID,
    );

    assert_eq!(
        action_target_result_type(&active, &descriptor),
        Err(ClientActionError::ResultTypeMismatch)
    );
}

#[test]
fn action_payload_rejects_malformed_and_noncanonical_frames() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let parameter = ParameterId::from_bytes([0x71; 16]);
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x72; 16]),
        vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let magic_length = super::ACTION_MAGIC.len();
    let body_offset = magic_length + 4;
    let metadata_length = 1 + (16 * 5);
    let count_offset = body_offset + metadata_length;
    let first_parameter_offset = count_offset + 4;
    let frame_length_offset = first_parameter_offset + 16;
    let frame_offset = frame_length_offset + 4;

    let mut invalid_magic = payload.clone();
    invalid_magic[0] ^= 0xff;

    let mut truncated = payload.clone();
    truncated.pop();

    let mut invalid_count = payload.clone();
    invalid_count[count_offset..count_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());

    let two_argument_descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x73; 16]),
        vec![
            FunctionArgument::new(ParameterId::from_bytes([1; 16]), RuntimeValue::Integer(1))
                .unwrap(),
            FunctionArgument::new(ParameterId::from_bytes([2; 16]), RuntimeValue::Integer(2))
                .unwrap(),
        ],
        orna_standard::INTEGER_TYPE_ID,
    );
    let two_argument_payload = encode_action_payload(&active, &two_argument_descriptor).unwrap();
    let first_two_argument_offset = first_parameter_offset;
    let first_frame_length = u32::from_be_bytes(
        two_argument_payload[first_two_argument_offset + 16..first_two_argument_offset + 20]
            .try_into()
            .unwrap(),
    ) as usize;
    let second_parameter_offset = first_two_argument_offset + 16 + 4 + first_frame_length;
    let mut invalid_order = two_argument_payload;
    invalid_order[second_parameter_offset..second_parameter_offset + 16].copy_from_slice(&[0; 16]);

    let mut trailing = payload.clone();
    trailing.push(0xaa);
    let body_length =
        u32::from_be_bytes(trailing[magic_length..magic_length + 4].try_into().unwrap());
    trailing[magic_length..magic_length + 4].copy_from_slice(&(body_length + 1).to_be_bytes());

    let mut invalid_orv3_frame = payload;
    invalid_orv3_frame[frame_offset..frame_offset + 4].copy_from_slice(b"ORV2");

    for malformed in [
        invalid_magic,
        truncated,
        invalid_count,
        invalid_order,
        trailing,
        invalid_orv3_frame,
    ] {
        assert!(matches!(
            decode_action_payload(&active, &malformed),
            Err(ClientActionError::InvalidPayload(_))
        ));
    }
}

#[test]
fn action_payload_encode_rejects_zero_identity_fields() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let make = |target, source, catalogue, call_site, result_type, parameter| {
        ClientActionDescriptor::new(
            ActionTargetDomain::Client,
            FunctionId::from_bytes(target),
            RevisionPair::new(
                SourceRevisionId::from_bytes(source),
                CatalogueRevisionId::from_bytes(catalogue),
            ),
            CallSiteId::from_bytes(call_site),
            vec![
                FunctionArgument::new(ParameterId::from_bytes(parameter), RuntimeValue::Integer(7))
                    .unwrap(),
            ],
            TypeId::from_bytes(result_type),
        )
    };
    let cases = [
        (
            [0; 16],
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            [0; 16],
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            [0; 16],
            [0x82; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0; 16],
            [0x44; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0; 16],
            [0x83; 16],
        ),
        (
            target.to_bytes(),
            pair.source().to_bytes(),
            pair.catalogue().to_bytes(),
            [0x82; 16],
            [0x44; 16],
            [0; 16],
        ),
    ];
    for (target, source, catalogue, call_site, result_type, parameter) in cases {
        assert_eq!(
            encode_action_payload(
                &active,
                &make(target, source, catalogue, call_site, result_type, parameter)
            ),
            Err(ClientActionError::InvalidPayload(
                "invalid action identity".to_owned()
            )),
        );
    }
}

#[test]
fn action_payload_decode_rejects_zero_identity_fields_before_descriptor_construction() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x82; 16]),
        vec![
            FunctionArgument::new(
                ParameterId::from_bytes([0x83; 16]),
                RuntimeValue::Integer(7),
            )
            .unwrap(),
        ],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let body_offset = super::ACTION_MAGIC.len() + 4;
    for relative_offset in [1, 17, 33, 49, 65, 85] {
        let mut corrupted = payload.clone();
        corrupted[body_offset + relative_offset..body_offset + relative_offset + 16].fill(0);
        assert_eq!(
            decode_action_payload(&active, &corrupted),
            Err(ClientActionError::InvalidPayload(
                "invalid action identity".to_owned()
            )),
            "identity field at offset {relative_offset} must be rejected"
        );
    }
}

#[test]
fn action_payload_encodes_multiple_arguments_in_parameter_order_and_round_trips() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x74; 16]),
        vec![
            FunctionArgument::new(ParameterId::from_bytes([1; 16]), RuntimeValue::Integer(11))
                .unwrap(),
            FunctionArgument::new(ParameterId::from_bytes([2; 16]), RuntimeValue::Integer(22))
                .unwrap(),
        ],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let body_offset = super::ACTION_MAGIC.len() + 4;
    let first_parameter_offset = body_offset + 1 + (16 * 5) + 4;
    let first_frame_length = u32::from_be_bytes(
        payload[first_parameter_offset + 16..first_parameter_offset + 20]
            .try_into()
            .unwrap(),
    ) as usize;
    let second_parameter_offset = first_parameter_offset + 16 + 4 + first_frame_length;
    assert_eq!(
        &payload[first_parameter_offset..first_parameter_offset + 16],
        &[1; 16]
    );
    assert_eq!(
        &payload[second_parameter_offset..second_parameter_offset + 16],
        &[2; 16]
    );

    let decoded = decode_action_payload(&active, &payload).unwrap();
    assert_eq!(decoded, descriptor);
    assert_eq!(encode_action_payload(&active, &decoded).unwrap(), payload);
}

#[test]
fn action_trigger_rejects_repeated_pending_server_request_without_mutating_generation() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let observer_root = InvocationId::from_bytes([0xfb; 16]);
    let observer_parent = InvocationId::from_bytes([0xfa; 16]);
    let observer_current = InvocationId::from_bytes([0xf9; 16]);
    let observer_lineage = super::ObserverLineage::top_level(observer_root)
        .with_parent_and_current(observer_parent, observer_current);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xfe; 16]),
        observer_lineage: Some(observer_lineage),
    };
    assert_eq!(parent.observer_root_invocation_id(), observer_root);
    assert_eq!(parent.observer_parent_invocation_id(), observer_current);
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0xff; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::Pending),
    );
    let first_request = executor.executed[0].clone();
    assert_eq!(
        first_request
            .invocation_context()
            .expect("server action carries observer provenance")
            .parent_invocation_id(),
        observer_current
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Loading);

    // The one-active-action contract rejects a repeated trigger while loading.
    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::Pending),
    );
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(
        action_state.invocation_id(),
        Some(first_request.request_id())
    );
    assert_eq!(action_state.generation(), Some(first_request.generation()));
    assert_eq!(action_state.status(), ClientResourceStatus::Loading);
}

#[test]
fn action_trigger_after_terminal_completion_allocates_fresh_request_identity() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0x01; 16]),
        observer_lineage: None,
    };
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0xff; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("completed".to_owned())));

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    let first_request = executor.executed[0].clone();
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    assert_eq!(executor.executed.len(), 2);
    let second_request = executor.executed[1].clone();
    assert_ne!(first_request.request_id(), second_request.request_id());
    assert!(second_request.generation().value() > first_request.generation().value());
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_trigger_redacts_executor_failure() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0x01; 16]),
        observer_lineage: None,
    };
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        CallSiteId::from_bytes([0x02; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = FailingActionExecutor::default();

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Failed {
            code: ACTION_FAILURE_CODE.to_owned(),
        }),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_payload_round_trip_and_rejects_trailing_bytes() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let parameter = ParameterId::from_bytes([0x71; 16]);
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x72; 16]),
        vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    assert_eq!(
        decode_action_payload(&active, &payload).unwrap(),
        descriptor
    );
    let mut trailing = payload.clone();
    trailing.push(0);
    assert!(decode_action_payload(&active, &trailing).is_err());
    let stale_descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        RevisionPair::new(SourceRevisionId::from_bytes([0x73; 16]), pair.catalogue()),
        CallSiteId::from_bytes([0x74; 16]),
        vec![FunctionArgument::new(parameter, RuntimeValue::Integer(7)).unwrap()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let stale_payload = encode_action_payload(&active, &stale_descriptor).unwrap();
    assert_eq!(
        decode_action_payload(&active, &stale_payload),
        Err(ClientActionError::RevisionMismatch),
    );
}

#[test]
fn action_pending_completion_retains_generation_and_redacts_failure() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let request_id = request.request_id();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    let mut executor = RecordingActionExecutor::new(None);
    assert_eq!(
        complete_client_action(&active, &mut action_state, request.pending(), &mut executor),
        Err(ClientActionError::Pending)
    );
    assert_eq!(action_state.generation(), Some(generation));
    let failed = ClientResourceCompletion::Failed {
        request_id,
        key,
        generation,
        code: "secret.internal.detail".to_owned(),
    };
    assert_eq!(
        complete_client_action(&active, &mut action_state, failed, &mut executor),
        Ok(ClientActionOutcome::Failed {
            code: ACTION_FAILURE_CODE.to_owned()
        })
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_completed_terminal_rejects_later_same_generation_failed_completion() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().ready(RuntimeValue::Boolean(true)),
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed)
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    let terminal_state = action_state.clone();
    let terminal_executor = executor.cancelled.clone();

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().failed("late.failure".to_owned()),
            &mut executor,
        ),
        Err(ClientActionError::StaleCompletion)
    );
    assert_eq!(action_state, terminal_state);
    assert_eq!(executor.cancelled, terminal_executor);
    assert!(executor.cancelled.is_empty());
}

#[test]
fn action_failed_terminal_rejects_later_same_generation_completed_completion() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().failed("first.failure".to_owned()),
            &mut executor,
        ),
        Ok(ClientActionOutcome::Failed {
            code: ACTION_FAILURE_CODE.to_owned(),
        })
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    let terminal_state = action_state.clone();
    let terminal_executor = executor.cancelled.clone();

    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.clone().ready(RuntimeValue::Boolean(true)),
            &mut executor,
        ),
        Err(ClientActionError::StaleCompletion)
    );
    assert_eq!(action_state, terminal_state);
    assert_eq!(executor.cancelled, terminal_executor);
    assert!(executor.cancelled.is_empty());
}

#[test]
fn action_cancellation_uses_executor_and_rejects_late_completion() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    action_state.stage_invocation(request.request_id());
    action_state.stage_request(request.clone());
    assert_eq!(action_state.invocation_id(), Some(request.request_id()));
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(true))
    });

    assert_eq!(
        super::cancel_client_action_with_executor(&active, &mut action_state, &mut executor,),
        Ok(ClientActionOutcome::Cancelled),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(
        complete_client_action(
            &active,
            &mut action_state,
            request.ready(RuntimeValue::Boolean(true)),
            &mut executor
        ),
        Err(ClientActionError::StaleCompletion),
    );
    assert_eq!(action_state.generation(), None);
}

#[test]
fn action_trigger_rejects_non_action_values() {
    let (active, function, pair, revision) = version_one_active(true);
    let auth = authorise(pair, function);
    let parent = ClientExecutionContext {
        pair,
        function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf1; 16]),
        observer_lineage: None,
    };
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(true))
    });
    assert_eq!(
        trigger_client_action(
            &active,
            &RuntimeValue::Boolean(true),
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor
        ),
        Err(super::ClientActionError::InvalidValue)
    );
}
#[test]
fn action_current_generation_mismatched_request_is_stale_but_same_request_malformed_completion_cancels()
 {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let wrong_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7b; 16]),
        digest,
        active.catalogue_hash(),
    );

    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let mut stale_state = ClientActionState::default();
    stale_state.set_resource(resource);
    let mut stale_executor = RecordingActionExecutor::new(None);
    assert_eq!(
        complete_client_action(
            &active,
            &mut stale_state,
            ClientResourceCompletion::Ready {
                request_id: request.request_id(),
                key: wrong_key,
                generation,
                value: RuntimeValue::Boolean(true),
            },
            &mut stale_executor,
        ),
        Err(ClientActionError::StaleCompletion),
    );
    assert_eq!(stale_state.status(), ClientResourceStatus::Loading);
    assert!(stale_executor.cancelled.is_empty());
    assert_eq!(
        complete_client_action(
            &active,
            &mut stale_state,
            ClientResourceCompletion::Ready {
                request_id: request.request_id(),
                key,
                generation,
                value: RuntimeValue::Integer(1),
            },
            &mut stale_executor,
        ),
        Ok(ClientActionOutcome::Cancelled),
    );
    assert_eq!(stale_state.status(), ClientResourceStatus::Idle);
    assert_eq!(stale_executor.cancelled, vec![request]);

    for malformed_kind in [0_u8, 1_u8] {
        let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let request = resource.begin_request(&active, vec![]).unwrap();
        assert_eq!(request.generation(), generation);
        let completion = if malformed_kind == 0 {
            ClientResourceCompletion::Ready {
                request_id: request.request_id(),
                key,
                generation,
                value: RuntimeValue::Integer(1),
            }
        } else {
            ClientResourceCompletion::Failed {
                request_id: request.request_id(),
                key,
                generation,
                code: String::new(),
            }
        };
        let mut action_state = ClientActionState::default();
        action_state.set_resource(resource);
        action_state.stage_request(request.clone());
        let mut executor = RecordingActionExecutor::new(None);
        assert_eq!(
            complete_client_action(&active, &mut action_state, completion, &mut executor),
            Ok(ClientActionOutcome::Cancelled),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
        assert_eq!(executor.cancelled, vec![request]);
    }
}

#[test]
fn action_uncertain_cancel_retains_loading_request() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    action_state.stage_request(request.clone());
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());
    let malformed = request.clone().ready(RuntimeValue::Integer(1));

    assert_eq!(
        complete_client_action(&active, &mut action_state, malformed, &mut executor),
        Err(ClientActionError::Pending),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Loading);
    assert_eq!(action_state.generation(), Some(generation));
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending, Some(request));
}

#[test]
fn action_malformed_terminal_cancellation_marks_released_request_cancelled() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let generation = request.generation();
    let mut action_state = ClientActionState::default();
    action_state.set_resource(resource);
    action_state.stage_request(request.clone());
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(7));
    executor.pending = Some(request.clone());
    let malformed = request.clone().ready(RuntimeValue::Integer(1));

    assert_eq!(
        complete_client_action(&active, &mut action_state, malformed, &mut executor),
        Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned())),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Cancelled);
    assert_eq!(action_state.generation(), Some(generation));
    assert_eq!(executor.cancelled, vec![request]);
    assert!(executor.pending.is_none());
}

#[test]
fn nested_action_pending_cancel_retains_pending_request() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7b; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    assert_eq!(
        nested.execute(request.clone()),
        request.clone().pending(),
        "a pending cancellation must not create a local terminal completion",
    );
    assert!(nested.release_failed());
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert!(executor.abandoned.is_empty());
    assert_eq!(executor.pending, Some(request));
}
#[test]
fn nested_action_stream_values_retain_pending_request() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7d; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc9; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, vec![]).unwrap();

    let mut execute_executor = RecordingActionExecutor::new(None).with_execute_stream_values();
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut execute_executor,
        pending_request: None,
    };
    assert_eq!(
        nested.execute(request.clone()),
        request
            .clone()
            .stream_values(vec![RuntimeValue::Boolean(true)]),
    );
    assert_eq!(
        nested.pending_request_identity(),
        Some((request.request_id(), request.key(), request.generation(),))
    );
    drop(nested);
    assert_eq!(execute_executor.pending, Some(request.clone()));

    let mut cancel_executor = RecordingActionExecutor::new(None).with_cancel_stream_values();
    cancel_executor.pending = Some(request.clone());
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut cancel_executor,
        pending_request: None,
    };
    assert_eq!(
        nested.cancel(request.clone()),
        request
            .clone()
            .stream_values(vec![RuntimeValue::Boolean(true)]),
    );
    assert_eq!(
        nested.pending_request_identity(),
        Some((request.request_id(), request.key(), request.generation(),))
    );
    drop(nested);
    assert_eq!(cancel_executor.pending, Some(request));
}

#[test]
fn nested_action_stream_values_then_terminal_releases_child() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7e; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xca; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, vec![]).unwrap();
    let mut wrong_resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let wrong_request = wrong_resource
        .begin_stream_request(&active, vec![])
        .unwrap();
    assert_ne!(request.request_id(), wrong_request.request_id());

    let mut executor = StreamThenTerminalExecutor {
        calls: 0,
        stale: Some(wrong_request),
    };
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    assert_eq!(
        nested.execute(request.clone()),
        request
            .clone()
            .stream_values(vec![RuntimeValue::Boolean(true)]),
    );
    assert!(nested.release_failed());

    assert_eq!(
        nested.execute(request.clone()),
        request.clone().stream_completed(),
    );
    assert!(!nested.release_failed());

    assert_eq!(nested.execute(request.clone()), request.clone().pending());
    assert_eq!(
        nested.pending_request_identity(),
        Some((request.request_id(), request.key(), request.generation())),
    );
}

#[test]
fn nested_executor_rejects_mismatched_completion_identity() {
    let (active, function, pair, _) = version_one_active(true);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7d; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut wrong_resource =
        ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let wrong_request = wrong_resource.begin_request(&active, vec![]).unwrap();
    assert_ne!(request.request_id(), wrong_request.request_id());

    let identity = (request.request_id(), request.key(), request.generation());
    let mut execute_executor =
        RecordingActionExecutor::new(None).with_pending_identity(wrong_request.clone());
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut execute_executor,
        pending_request: None,
    };
    assert_eq!(nested.execute(request.clone()), request.clone().pending());
    assert_eq!(nested.pending_request_identity(), Some(identity));
    drop(nested);
    assert_eq!(
        execute_executor.cancelled,
        Vec::<ClientResourceRequest>::new()
    );
    assert_eq!(execute_executor.pending, Some(request.clone()));

    let mut cancel_executor =
        RecordingActionExecutor::new(None).with_cancel_pending_identity(wrong_request);
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut cancel_executor,
        pending_request: Some(request.clone()),
    };
    assert_eq!(nested.cancel(request.clone()), request.clone().pending());
    assert_eq!(nested.pending_request_identity(), Some(identity));
    drop(nested);
    assert_eq!(cancel_executor.cancelled, vec![request]);
}

#[test]
fn nested_abandon_mismatch_preserves_inner_request_without_local_marker() {
    let (active, function, pair, _) = version_one_active(true);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7c; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut state_a = ClientStateStore::new();
    let request_a = state_a
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut state_b = ClientStateStore::new();
    let request_b = state_b
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    assert_ne!(request_a.request_id(), request_b.request_id());

    let mut executor = RecordingActionExecutor::new(None);
    executor.pending = Some(request_a.clone());
    let mut nested = super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    assert_eq!(
        nested.abandon(request_b.clone()),
        Err("resource executor request mismatch".to_owned()),
    );
    assert_eq!(nested.pending_request_identity(), None);

    nested
        .abandon(request_a.clone())
        .expect("the retained child request remains addressable");
    drop(nested);
    assert_eq!(executor.pending, None);
    assert_eq!(executor.abandoned, vec![request_b, request_a]);
}

#[test]
fn nested_action_pending_cancel_retains_replacements_and_exact_child() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xfa; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe5; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let resource_parameter = ParameterId::from_bytes([0xd3; 16]);
    let nested_argument = FunctionArgument::new(
        resource_parameter,
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let nested_digest = ClientResourceKey::canonical_arguments_digest(
        &active,
        std::slice::from_ref(&nested_argument),
    )
    .unwrap();
    let resource_target = InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair);
    let replacement_a = ClientResourceKey::new(
        resource_target,
        auth.session_principal(),
        nested_digest,
        Sha256Digest::from_bytes([0xa1; 32]),
    );
    let replacement_b = ClientResourceKey::new(
        resource_target,
        auth.session_principal(),
        nested_digest,
        Sha256Digest::from_bytes([0xa2; 32]),
    );
    for replacement in [replacement_a, replacement_b] {
        state
            .get_or_create_resource(
                replacement,
                ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
            )
            .begin_loading()
            .unwrap();
    }
    assert_eq!(
        state.resource(replacement_a).map(ClientResource::status),
        Some(ClientResourceStatus::Loading),
    );
    assert_eq!(
        state.resource(replacement_b).map(ClientResource::status),
        Some(ClientResourceStatus::Loading),
    );
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("pending child cancellation must retain the child request");
    let (request_id, child_key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected nested release error: {other:?}"),
    };
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(executor.cancelled, executor.executed);
    assert!(executor.abandoned.is_empty());
    let pending = executor
        .pending
        .clone()
        .expect("the executor retains the exact child request");
    assert_eq!(pending.request_id(), request_id);
    assert_eq!(pending.key(), child_key);
    assert_eq!(pending.key().target(), resource_target);
    assert_eq!(pending.generation(), generation);
    assert_ne!(child_key, replacement_a);
    assert_ne!(child_key, replacement_b);
    assert_eq!(
        state
            .resource(replacement_a)
            .expect("first replacement remains cached")
            .status(),
        ClientResourceStatus::Idle,
    );
    assert_eq!(
        state
            .resource(replacement_b)
            .expect("second replacement remains cached")
            .status(),
        ClientResourceStatus::Idle,
    );
    assert_eq!(
        state
            .resource(child_key)
            .expect("pending child remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );
}

#[test]
fn nested_action_pending_poll_applies_exact_child_and_rejects_wrong_identity() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xfc; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe7; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor = PollingTestExecutor::default();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("pending nested child must be handed back to the caller");
    let (request_id, child_key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected nested pending error: {other:?}"),
    };
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);
    let pending = executor
        .pending
        .clone()
        .expect("the executor retains the exact child request");
    assert_eq!(pending.request_id(), request_id);
    assert_eq!(pending.key(), child_key);
    assert_eq!(pending.generation(), generation);
    assert_eq!(
        state
            .resource(child_key)
            .expect("pending child remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );

    let completion = executor.poll().expect("the retained child can be polled");
    assert_eq!(
        completion,
        ClientResourceCompletion::Ready {
            request_id,
            key: child_key,
            generation,
            value: RuntimeValue::Text("polled".to_owned()),
        }
    );
    state
        .resource_mut(child_key)
        .expect("retained child remains addressable")
        .apply_completion(&active, completion)
        .expect("the exact child completion publishes Ready");
    assert_eq!(
        state
            .resource(child_key)
            .expect("completed child remains cached")
            .status(),
        ClientResourceStatus::Ready,
    );
    assert_eq!(
        state
            .resource(child_key)
            .expect("completed child remains cached")
            .value(),
        Some(&RuntimeValue::Text("polled".to_owned())),
    );

    let before_wrong_completion = state
        .resource(child_key)
        .expect("completed child remains cached")
        .clone();
    let wrong_request_id = InvocationId::from_bytes([0xff; 16]);
    assert_eq!(
        state
            .resource_mut(child_key)
            .expect("completed child remains mutable")
            .apply_completion(
                &active,
                ClientResourceCompletion::Ready {
                    request_id: wrong_request_id,
                    key: child_key,
                    generation,
                    value: RuntimeValue::Boolean(false),
                },
            ),
        Err(super::ClientResourceError::RequestIdMismatch {
            expected: request_id,
            actual: wrong_request_id,
        }),
    );
    assert_eq!(
        state
            .resource(child_key)
            .expect("completed child remains cached")
            .clone(),
        before_wrong_completion,
    );
}

#[test]
fn nested_action_malformed_child_pending_cancel_retains_exact_identity() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xfb; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe6; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Integer(7))).with_cancel_pending();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("malformed child completion must remain pending");
    let (request_id, child_key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected malformed child error: {other:?}"),
    };

    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(executor.cancelled, executor.executed);
    assert!(executor.abandoned.is_empty());
    let request = executor
        .executed
        .first()
        .expect("child request was submitted");
    assert_eq!(request.request_id(), request_id);
    assert_eq!(request.key(), child_key);
    assert_eq!(request.generation(), generation);
    assert_eq!(
        state
            .resource(child_key)
            .expect("malformed child remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );
}

#[test]
fn action_local_resource_pending_is_cancelled_and_reports_cancelled_with_fresh_parent() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let enclosing_parent = InvocationId::from_bytes([0xf5; 16]);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: enclosing_parent,
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe2; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let mut executor = RecordingActionExecutor::new(None);

    for previous_parent in [None, Some(enclosing_parent)] {
        assert_eq!(
            trigger_client_action(
                &active,
                &action,
                &auth,
                &parent,
                &mut action_state,
                &[],
                &grants,
                &mut state,
                &mut executor,
            ),
            Ok(ClientActionOutcome::Cancelled),
        );
        assert_eq!(action_state.status(), ClientResourceStatus::Idle);
        assert_eq!(executor.cancelled.len(), executor.executed.len());
        assert!(executor.abandoned.is_empty());
        let request = executor.executed.last().unwrap().clone();
        let cancelled = executor.cancelled.last().unwrap().clone();
        assert_eq!(request, cancelled);
        assert!(executor.poll().is_none());
        assert!(executor.pending.is_none());
        let nested_parent = request
            .invocation_context()
            .expect("nested resource carries invocation provenance")
            .parent_invocation_id();
        assert_ne!(nested_parent, enclosing_parent);
        if let Some(previous_parent) = previous_parent {
            assert_ne!(nested_parent, previous_parent);
        }
        assert!(state.resource(request.key()).is_none());
    }
}

#[test]
fn nested_action_with_loading_resource_reports_cancelled_without_dispatch() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf8; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe3; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let resource_parameter = ParameterId::from_bytes([0xd3; 16]);
    let nested_argument = FunctionArgument::new(
        resource_parameter,
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let nested_digest = ClientResourceKey::canonical_arguments_digest(
        &active,
        std::slice::from_ref(&nested_argument),
    )
    .unwrap();
    let nested_key = ClientResourceKey::new(
        InvocationTarget::new(FunctionId::from_bytes([0xd1; 16]), pair),
        auth.session_principal(),
        nested_digest,
        super::resource_invalidation_identity(
            active.catalogue_hash(),
            state.context().data_invalidation_token(),
            super::security_context_digest(&auth),
            state.context(),
            state.user_state_epoch(),
        ),
    );
    state
        .get_or_create_resource(
            nested_key,
            ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
        )
        .begin_request(&active, vec![nested_argument])
        .unwrap();
    let mut action_state = ClientActionState::default();
    let mut executor =
        RecordingActionExecutor::new(Some(RuntimeValue::Text("unexpected".to_owned())));

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::from_grants([
                capability::LocalCapabilityGrant::new(
                    capability::LocalCapabilityName::StdFsRead,
                    capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
                )
                .unwrap(),
            ])
            .unwrap(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Cancelled),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert!(executor.executed.is_empty());
    assert_eq!(
        state
            .resource(nested_key)
            .expect("pre-existing nested resource remains cached")
            .status(),
        ClientResourceStatus::Loading,
    );
}

#[test]
fn nested_action_pending_cancel_clears_outer_action_state() {
    let (active, parent_function, target, pair, revision, parameter) =
        version_six_client_action_provenance_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf9; 16]),
        observer_lineage: None,
    };
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/action".to_owned())).unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xe4; 16]),
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let grants =
        capability::LocalCapabilityGrantSet::from_grants([capability::LocalCapabilityGrant::new(
            capability::LocalCapabilityName::StdFsRead,
            capability::LocalCapabilityScope::path("/tmp/action").unwrap(),
        )
        .unwrap()])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();

    let error = trigger_client_action(
        &active,
        &action,
        &auth,
        &parent,
        &mut action_state,
        &[],
        &grants,
        &mut state,
        &mut executor,
    )
    .expect_err("nested pending cancellation must be reported");
    let (request_id, key, generation) = match error {
        ClientActionError::ExecutorPending {
            request_id,
            key,
            generation,
            ..
        } => (request_id, key, generation),
        other => panic!("unexpected nested release error: {other:?}"),
    };
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
    assert_eq!(action_state.invocation_id(), None);
    assert_eq!(action_state.generation(), None);
    assert_eq!(executor.executed.len(), 1);
    assert_eq!(executor.cancelled, executor.executed);
    assert!(executor.abandoned.is_empty());
    let pending = executor
        .pending
        .clone()
        .expect("failed release retains child request");
    assert_eq!(pending.request_id(), request_id);
    assert_eq!(pending.key(), key);
    assert_eq!(pending.generation(), generation);
    assert_eq!(
        state
            .resource(key)
            .expect("failed release retains child resource")
            .status(),
        ClientResourceStatus::Loading,
    );

    executor
        .abandon(pending)
        .expect("caller can release the retained child request");
    assert!(executor.poll().is_none());
    assert_eq!(executor.late_dropped, 1);
    state
        .resource_mut(key)
        .expect("retained child remains addressable")
        .cancel(generation)
        .expect("caller can terminalise the retained child");
    assert_eq!(
        state
            .resource(key)
            .expect("retained child remains cached")
            .status(),
        ClientResourceStatus::Cancelled,
    );
}

#[test]
fn action_trigger_executes_a_verified_standard_server_target() {
    let (active, parent_function, _pair, parent_revision) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::Function(orna_standard::STD_INVOKE_ECHO_FUNCTION_ID),
        DefinitionReferenceKind::FunctionCall,
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        orna_artifact::client_plan::ExpressionClientPlan::new(
            orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
        )
        .encode()
        .unwrap(),
    );
    let pair = active.pair();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("standard action fixture has a pinned snapshot");
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        CallSiteId::from_bytes([0xf6; 16]),
        vec![argument.clone()],
        orna_standard::INTEGER_TYPE_ID,
    );
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0xf7; 16]),
        observer_lineage: None,
    };
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(42)));

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    let request = executor.executed.first().expect("action was dispatched");
    assert_eq!(
        request.target(),
        InvocationTarget::verified_standard(
            orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
            pair,
            standard.revision(),
            orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        )
    );
    assert_eq!(request.arguments(), &[argument]);
    assert_eq!(
        request.expected_type(),
        ResolvedType::Scalar(StandardScalar::Integer)
    );
    assert_ne!(
        request
            .invocation_context()
            .expect("server action carries invocation provenance")
            .call_site_id(),
        CallSiteId::from_bytes([0xf6; 16]),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_trigger_executes_a_local_client_target() {
    let (active, parent_function, target, pair, revision) = version_two_local_action_active();
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf3; 16]),
        observer_lineage: None,
    };
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0xf4; 16]),
        Vec::new(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let registry = super::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("action test fixture has a standard snapshot"),
    )
    .unwrap();
    let action = OpaqueValue::new(&active, &registry, super::STD_ACTION_TYPE_ID, payload).unwrap();
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(false))
    });

    assert_eq!(
        trigger_client_action(
            &active,
            &RuntimeValue::Opaque(action),
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Ok(ClientActionOutcome::Completed),
    );
    assert_eq!(action_state.status(), ClientResourceStatus::Idle);
}

#[test]
fn action_trigger_does_not_forward_forged_call_site_metadata() {
    let (active, parent_function, pair, parent_revision, _parameter) =
        version_six_client_resource_action_active();
    let target = FunctionId::from_bytes([0xd1; 16]);
    let forged_call_site = CallSiteId::from_bytes([0x9a; 16]);
    let auth = authorise(pair, parent_function);
    let parent = ClientExecutionContext {
        pair,
        function: parent_function,
        function_revision: parent_revision,
        parent_invocation_id: InvocationId::from_bytes([0x9b; 16]),
        observer_lineage: None,
    };
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/action".to_owned()),
    )
    .unwrap();
    let action = action_value(
        &active,
        ActionTargetDomain::Server,
        target,
        pair,
        forged_call_site,
        vec![argument],
        orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID,
    );
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = RecordingActionExecutor::new(None);

    assert_eq!(
        trigger_client_action(
            &active,
            &action,
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::Pending),
    );
    let request = executor.executed.first().expect("action was dispatched");
    let context = request
        .invocation_context()
        .expect("server action carries invocation provenance");
    assert_ne!(context.call_site_id(), forged_call_site);
    assert_eq!(
        context.parent_invocation_id(),
        parent.parent_invocation_id()
    );
    assert_eq!(request.target().function(), target);
}

#[test]
fn action_trigger_rejects_unreferenced_target_provenance() {
    let (original_active, target, pair, revision) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let standard_v6 = orna_standard::verify_standard_library_v6_snapshot(
        orna_standard::retained_standard_library_v6_snapshot().unwrap(),
    )
    .unwrap();
    let context = orna_core::revision::CatalogueHashContext::version_two(standard_v6);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        original_active.catalogue(),
        original_active.function_revisions(),
        original_active.expressions(),
        original_active.origins(),
        original_active.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            original_active.source().clone(),
            original_active.catalogue().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                original_active.expressions().to_vec(),
                original_active.function_revisions().to_vec(),
                original_active.origins().to_vec(),
                original_active.references().to_vec(),
            ),
        ),
        context,
    )
    .unwrap();
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([0x75; 16]),
        Vec::new(),
        orna_standard::INTEGER_TYPE_ID,
    );
    let payload = encode_action_payload(&active, &descriptor).unwrap();
    let registry = super::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("action test fixture has a standard snapshot"),
    )
    .unwrap();
    let action = OpaqueValue::new(&active, &registry, super::STD_ACTION_TYPE_ID, payload).unwrap();
    let auth = authorise(pair, target);
    let parent = ClientExecutionContext {
        pair,
        function: target,
        function_revision: revision,
        parent_invocation_id: InvocationId::from_bytes([0xf2; 16]),
        observer_lineage: None,
    };
    let mut state = ClientStateStore::default();
    let mut action_state = ClientActionState::default();
    let mut executor = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Integer(1))
    });

    assert_eq!(
        trigger_client_action(
            &active,
            &RuntimeValue::Opaque(action),
            &auth,
            &parent,
            &mut action_state,
            &[],
            &capability::LocalCapabilityGrantSet::default(),
            &mut state,
            &mut executor,
        ),
        Err(ClientActionError::TargetMismatch),
    );
}

#[test]
fn action_payload_rejects_noncanonical_argument_order() {
    let (active, target, pair, _) = version_two_value_active(
        orna_standard::INTEGER_TYPE_ID,
        orna_standard::INTEGER_TYPE_ID,
    );
    let first =
        FunctionArgument::new(ParameterId::from_bytes([2; 16]), RuntimeValue::Integer(1)).unwrap();
    let second =
        FunctionArgument::new(ParameterId::from_bytes([1; 16]), RuntimeValue::Integer(2)).unwrap();
    let descriptor = ClientActionDescriptor::new(
        ActionTargetDomain::Client,
        target,
        pair,
        CallSiteId::from_bytes([3; 16]),
        vec![first, second],
        orna_standard::INTEGER_TYPE_ID,
    );
    assert!(encode_action_payload(&active, &descriptor).is_err());
}
#[test]
fn client_artifact_integrity_checks_domain_and_payload_digest() {
    let payload = b"client-artifact-demo".to_vec();
    let digest = artifact_payload_digest(&payload).expect("demo payload digest");
    let valid = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "demo.client-artifact",
        1,
        payload.clone(),
        digest,
    )
    .expect("valid client artifact");
    assert_eq!(super::validate_client_artifact_integrity(&valid), Ok(()));

    let wrong_kind = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "demo.client-artifact",
        1,
        payload.clone(),
        digest,
    )
    .expect("wrong-domain artifact");
    assert_eq!(
        super::validate_client_artifact_integrity(&wrong_kind),
        Err(super::ClientArtifactIntegrityError::WrongExecutionDomain)
    );

    let wrong_digest = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "demo.client-artifact",
        1,
        payload,
        Sha256Digest::from_bytes([0; 32]),
    )
    .expect("wrong-digest artifact");
    assert_eq!(
        super::validate_client_artifact_integrity(&wrong_digest),
        Err(super::ClientArtifactIntegrityError::PayloadDigest)
    );
}
