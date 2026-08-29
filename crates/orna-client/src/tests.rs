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

#[path = "tests/actions.rs"]
mod actions;
#[path = "tests/capabilities.rs"]
mod capabilities;
#[path = "tests/resources.rs"]
mod resources;
#[path = "tests/state.rs"]
mod state;
#[path = "tests/ui_inspect.rs"]
mod ui_inspect;
