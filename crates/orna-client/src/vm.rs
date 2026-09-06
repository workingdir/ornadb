//! Stage 1 CLIENT VM admission and host-control primitives.
//!
//! This module is additive. It provides bounded, immutable control-plane values
//! for the future VM host without changing the existing evaluator entry points.
//! Stage 1 has no production host effects, kernel audit calls, signatures, or
//! operating-system isolation.

mod admission;
mod identity;
mod lease;
mod runtime_witness;

pub use admission::{
    ClientVmAdmission, ClientVmAdmissionError, ClientVmArtifactIdentity, ClientVmArtifactKind,
    ClientVmArtifactLimits, ClientVmCapabilityArgument, ClientVmCapabilityDeclaration,
    ClientVmHostAdmissionContext,
};
pub use identity::{
    ClientVmIdentityError, ClientVmInvocationAllocator, ClientVmInvocationId,
    ClientVmInvocationRegistry,
};
pub use lease::{EphemeralCapabilityLease, LeaseError, LeaseSnapshot, LeaseState};
pub use runtime_witness::{RuntimeOfferWitness, RuntimeOfferWitnessError};

use orna_artifact::client_plan::{
    ACTION_FORMAT_VERSION, ActionClientPlan, CAPABILITY_FORMAT_VERSION,
    CONTROL_FLOW_FORMAT_VERSION, CapabilityArgumentSource, CapabilityClientPlan,
    ClientExpressionNode, ClientPlan, ControlFlowClientPlan, ControlFlowStatement,
    EXPRESSION_FORMAT_VERSION, ExpressionClientPlan, FORMAT_VERSION, INSPECT_FORMAT_VERSION, MAGIC,
    MAX_ARTIFACT_BYTES, OPAQUE_FORMAT_VERSION, OpaqueClientPlan, PROCEDURAL_FORMAT_VERSION,
    ProceduralClientPlan, RESOURCE_FORMAT_VERSION, ResourceClientPlan, STATE_FORMAT_VERSION,
    StateClientPlan,
};
use orna_core::{
    InvocationId,
    canonical_hash::artifact_payload_digest,
    catalogue::FunctionDomain,
    revision::{ActiveDatabaseRevision, ExecutableArtifactKind},
    security::AuthorisedInvocation,
    value::FunctionArgument,
};
use std::{collections::HashSet, fmt, sync::Arc};

/// The concrete plan variants retained by one Stage 1 admission.
///
/// The verifier decodes one selected variant and stores it immutably. This
/// value is a plan witness only; it does not contain a host handle or grant.
#[non_exhaustive]
#[derive(Debug)]
pub enum ClientVmDecodedPlan {
    /// A version-1 Boolean plan.
    Boolean(ClientPlan),
    /// A version-2 opaque-value plan.
    Opaque(OpaqueClientPlan),
    /// A version-3 or version-9 expression plan.
    Expression(ExpressionClientPlan),
    /// A version-4 state plan.
    State(StateClientPlan),
    /// A version-5 capability envelope.
    Capability(CapabilityClientPlan),
    /// A version-6 resource plan.
    Resource(ResourceClientPlan),
    /// A version-7 procedural plan.
    Procedural(ProceduralClientPlan),
    /// A version-8 action plan.
    Action(ActionClientPlan),
    /// A version-10 control-flow plan.
    ControlFlow(ControlFlowClientPlan),
}

/// A pure CLIENT plan could not cross the admitted execution boundary.
#[derive(Debug)]
pub enum ClientVmExecutionError {
    /// The host policy, runtime, limit, or cancellation fence changed.
    AdmissionStale,
    /// The supplied authorisation does not identify the admitted active target.
    TargetMismatch,
    /// The admitted plan requests a resource, action, state, capability, or contract seam.
    NonPurePlan,
    /// The existing bounded evaluator rejected the admitted target.
    Evaluation(Box<super::ClientExecutionError>),
}

impl fmt::Display for ClientVmExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdmissionStale => formatter.write_str("CLIENT VM admission is no longer current"),
            Self::TargetMismatch => {
                formatter.write_str("CLIENT VM execution target does not match admission")
            }
            Self::NonPurePlan => formatter.write_str("CLIENT VM plan requires a host capability"),
            Self::Evaluation(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for ClientVmExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Evaluation(source) => Some(source.as_ref()),
            Self::AdmissionStale | Self::TargetMismatch | Self::NonPurePlan => None,
        }
    }
}

/// Executes an admitted pure expression plan after rechecking its live fences.
///
/// Stage 1 deliberately rejects plans that need a host capability. The existing
/// evaluator remains the value-producing implementation and revalidates the
/// immutable active artifact; the admission is the required ownership and
/// authority gate before that evaluation begins.
#[allow(clippy::result_large_err)]
pub fn execute_admitted_pure_client_function(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    host: &ClientVmHostContext,
    admission: &ClientVmAdmission<ClientVmDecodedPlan>,
    arguments: &[FunctionArgument],
) -> Result<super::ClientExecutionResult, ClientVmExecutionError> {
    let target = authorisation.target();
    let [source_revision, catalogue_revision] = admission.identity().revision_pair();
    if target.revision() != active.pair()
        || target.function().to_bytes() != admission.identity().function()
        || target.revision().source().to_bytes() != source_revision
        || target.revision().catalogue().to_bytes() != catalogue_revision
    {
        return Err(ClientVmExecutionError::TargetMismatch);
    }
    if !host.admission_is_current(admission) {
        return Err(ClientVmExecutionError::AdmissionStale);
    }
    if !matches!(
        admission.plan(),
        ClientVmDecodedPlan::Boolean(_) | ClientVmDecodedPlan::Expression(_)
    ) {
        return Err(ClientVmExecutionError::NonPurePlan);
    }
    super::evaluate_client_function_with_arguments(active, authorisation, arguments)
        .map_err(|error| ClientVmExecutionError::Evaluation(Box::new(error)))
}

/// Admits and decodes the CLIENT artifact selected by kernel authorisation.
///
/// This is the additive Stage 1 boundary. The target and revision are resolved
/// from `active` and checked against the supplied `AuthorisedInvocation` before
/// the artifact identity is built. Capability declarations are supplied by the
/// trusted artifact manifest and compared with the decoded version-5 envelope.
/// Contract declarations are currently accepted only as an empty set because
/// the current plan model has no separate trusted contract manifest. No host
/// operation, audit call, lease effect, or runtime-library call occurs here.
pub fn admit_client_function(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    host: &mut ClientVmHostContext,
    artifact_limits: ClientVmArtifactLimits,
    declared_capabilities: &[ClientVmCapabilityDeclaration],
    declared_contracts: &[String],
) -> Result<ClientVmAdmission<ClientVmDecodedPlan>, ClientVmAdmissionError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(ClientVmAdmissionError::TupleMismatch {
            field: "authorisation",
        });
    }
    if !super::client_invocation_target_is_resolved(active, target) {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "revision" });
    }
    let resolved = super::resolve_client_function(active, target.function())
        .ok_or(ClientVmAdmissionError::TupleMismatch { field: "revision" })?;
    let root_binding = ClientVmRootBinding {
        function: target.function().to_bytes(),
        function_revision: resolved.revision.id().to_bytes(),
        revision_pair: [
            active.pair().source().to_bytes(),
            active.pair().catalogue().to_bytes(),
        ],
        security_context_digest: authorisation.security_context_digest().to_bytes(),
    };
    host.check_root_binding(
        root_binding.function,
        root_binding.function_revision,
        root_binding.revision_pair,
        root_binding.security_context_digest,
    )
    .map_err(|_| ClientVmAdmissionError::TupleMismatch {
        field: "root_binding",
    })?;
    if resolved.definition.domain() != FunctionDomain::Client {
        return Err(ClientVmAdmissionError::WrongExecutionDomain);
    }
    let artifact = resolved.revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Client {
        return Err(ClientVmAdmissionError::WrongExecutionDomain);
    }
    if artifact.format() != super::FORMAT_IDENTITY {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "format" });
    }
    if resolved.revision.language_version() != super::LANGUAGE_VERSION_IDENTITY {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "language" });
    }
    if artifact.payload().len() > MAX_ARTIFACT_BYTES {
        return Err(ClientVmAdmissionError::PayloadTooLarge {
            bytes: artifact.payload().len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    if artifact.payload().len() > artifact_limits.payload_bytes() {
        return Err(ClientVmAdmissionError::PayloadTooLarge {
            bytes: artifact.payload().len(),
            maximum: artifact_limits.payload_bytes(),
        });
    }
    let payload_digest = artifact_payload_digest(artifact.payload())
        .map_err(|_| ClientVmAdmissionError::DigestMismatch)?
        .to_bytes();
    if payload_digest != artifact.content_hash().to_bytes() {
        return Err(ClientVmAdmissionError::DigestMismatch);
    }
    super::execution::validate_active_catalogue_for_vm(active, target.function())
        .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
    let outer_version = artifact.version();
    let inner_version = if outer_version == CAPABILITY_FORMAT_VERSION {
        Some(capability_inner_version(artifact.payload())?)
    } else {
        None
    };
    let identity = ClientVmArtifactIdentity::new(
        target.function().to_bytes(),
        resolved.revision.id().to_bytes(),
        [
            active.pair().source().to_bytes(),
            active.pair().catalogue().to_bytes(),
        ],
        ClientVmArtifactKind::Client,
        artifact.format(),
        outer_version,
        inner_version,
        resolved.revision.language_version(),
        payload_digest,
        declared_capabilities.iter().cloned(),
        declared_contracts.iter().cloned(),
        artifact_limits,
    )?;
    let context = super::ClientExecutionContext {
        pair: active.pair(),
        function: target.function(),
        function_revision: resolved.revision.id(),
        parent_invocation_id: invocation_id(host.root_invocation_id()),
        observer_lineage: None,
    };
    let effective_version = inner_version.unwrap_or(outer_version);
    let resolved_references = resolved.references;
    let definition = resolved.definition;
    let revision = resolved.revision;
    let admission_identity = identity.clone();
    let semantic_identity = identity.clone();
    let admission_host = host
        .admission_context()
        .with_security_context(root_binding.security_context_digest);
    let admission = ClientVmAdmission::admit(
        &identity,
        admission_identity,
        artifact.payload(),
        admission_host,
        |payload| decode_client_plan(outer_version, payload),
        move |plan| {
            let return_shape = super::execution::validate_function_shape(
                active,
                definition,
                context,
                effective_version,
            )
            .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
            super::execution::validate_artifact(
                artifact,
                revision.language_version(),
                context,
                return_shape,
                effective_version,
            )
            .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
            super::execution::validate_selected_references(
                active,
                resolved_references,
                definition,
                revision.semantic_hash_version(),
                context,
                return_shape,
            )
            .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
            if decoded_plan_contains_external_contract(plan) {
                return Err(ClientVmAdmissionError::SemanticRejected);
            }
            let budget = decoded_plan_budget(plan)?;
            let limits = semantic_identity.limits();
            if budget.depth > limits.plan_depth() {
                return Err(ClientVmAdmissionError::LimitExceeded {
                    field: "plan_depth",
                });
            }
            if budget.operations > limits.plan_operations() {
                return Err(ClientVmAdmissionError::LimitExceeded {
                    field: "plan_operations",
                });
            }
            if effective_version != outer_version && !semantic_identity.contracts().is_empty() {
                return Err(ClientVmAdmissionError::SemanticRejected);
            }
            if effective_version == outer_version
                && (!semantic_identity.capabilities().is_empty()
                    || !semantic_identity.contracts().is_empty())
            {
                return Err(ClientVmAdmissionError::SemanticRejected);
            }
            match plan {
                ClientVmDecodedPlan::Boolean(_) => {}
                ClientVmDecodedPlan::Opaque(plan) => {
                    let super::execution::ClientReturnShape::Opaque(expected) = return_shape else {
                        return Err(ClientVmAdmissionError::SemanticRejected);
                    };
                    if plan.opaque_type() != expected {
                        return Err(ClientVmAdmissionError::SemanticRejected);
                    }
                }
                ClientVmDecodedPlan::Expression(plan) => {
                    super::execution::preflight_client_expression_calls(
                        active,
                        plan.expression(),
                        context,
                    )
                    .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                }
                ClientVmDecodedPlan::State(plan) => {
                    super::execution::preflight_client_state_calls(active, plan, context)
                        .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                }
                ClientVmDecodedPlan::Capability(plan) => {
                    if plan.inner_plan_version() != effective_version {
                        return Err(ClientVmAdmissionError::TupleMismatch {
                            field: "inner_version",
                        });
                    }
                    if let orna_artifact::client_plan::InnerClientPlan::Opaque(inner_plan) =
                        plan.inner_plan()
                    {
                        let super::execution::ClientReturnShape::Opaque(expected) = return_shape
                        else {
                            return Err(ClientVmAdmissionError::SemanticRejected);
                        };
                        if inner_plan.opaque_type() != expected {
                            return Err(ClientVmAdmissionError::SemanticRejected);
                        }
                    }
                    let mut requirements = plan
                        .requirements()
                        .iter()
                        .map(|requirement| {
                            let name =
                                super::capability::LocalCapabilityName::parse(requirement.name())
                                    .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                            if let CapabilityArgumentSource::Parameter(parameter) =
                                requirement.argument()
                                && !definition
                                    .parameters()
                                    .iter()
                                    .any(|candidate| candidate.name() == parameter)
                            {
                                return Err(ClientVmAdmissionError::SemanticRejected);
                            }
                            let argument = match requirement.argument() {
                                CapabilityArgumentSource::Text(value) => {
                                    ClientVmCapabilityArgument::Text(value.clone())
                                }
                                CapabilityArgumentSource::Parameter(value) => {
                                    ClientVmCapabilityArgument::Parameter(value.clone())
                                }
                            };
                            Ok(ClientVmCapabilityDeclaration::new(name.as_str(), argument))
                        })
                        .collect::<Result<Vec<_>, ClientVmAdmissionError>>()?;
                    requirements.sort_unstable();
                    if requirements.as_slice() != semantic_identity.capabilities() {
                        return Err(ClientVmAdmissionError::TupleMismatch {
                            field: "capabilities",
                        });
                    }
                    super::execution::preflight_client_inner_plan_calls(
                        active,
                        plan.inner_plan(),
                        context,
                    )
                    .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                }
                ClientVmDecodedPlan::Resource(plan) => {
                    super::execution::preflight_client_expression_calls(
                        active,
                        plan.expression(),
                        context,
                    )
                    .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                }
                ClientVmDecodedPlan::Procedural(plan) => {
                    super::execution::preflight_client_procedural_calls(active, plan, context)
                        .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                }
                ClientVmDecodedPlan::Action(plan) => {
                    super::execution::preflight_client_action_calls(
                        active,
                        plan.operation(),
                        context,
                    )
                    .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                }
                ClientVmDecodedPlan::ControlFlow(plan) => {
                    super::execution::preflight_client_control_flow_calls(active, plan, context)
                        .map_err(|_| ClientVmAdmissionError::SemanticRejected)?;
                }
            }
            Ok(())
        },
    )?;
    // Structural, digest, decode, and semantic checks must win over
    // cancellation; cancellation is the final pre-bind host fence.
    if host.is_cancelled() {
        return Err(ClientVmAdmissionError::HostCancelled);
    }
    host.bind_root(
        root_binding.function,
        root_binding.function_revision,
        root_binding.revision_pair,
        root_binding.security_context_digest,
    )
    .map_err(|_| ClientVmAdmissionError::TupleMismatch {
        field: "root_binding",
    })?;
    Ok(admission)
}

fn decode_client_plan(
    outer_version: u32,
    payload: &[u8],
) -> Result<ClientVmDecodedPlan, ClientVmAdmissionError> {
    let plan = match outer_version {
        FORMAT_VERSION => ClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::Boolean)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        OPAQUE_FORMAT_VERSION => OpaqueClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::Opaque)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        EXPRESSION_FORMAT_VERSION | INSPECT_FORMAT_VERSION => ExpressionClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::Expression)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        STATE_FORMAT_VERSION => StateClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::State)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        CAPABILITY_FORMAT_VERSION => CapabilityClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::Capability)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        RESOURCE_FORMAT_VERSION => ResourceClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::Resource)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        PROCEDURAL_FORMAT_VERSION => ProceduralClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::Procedural)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        ACTION_FORMAT_VERSION => ActionClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::Action)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        CONTROL_FLOW_FORMAT_VERSION => ControlFlowClientPlan::decode(payload)
            .map(ClientVmDecodedPlan::ControlFlow)
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
        _ => {
            return Err(ClientVmAdmissionError::UnsupportedVersion {
                outer: outer_version,
                inner: None,
            });
        }
    };
    Ok(plan)
}

fn capability_inner_version(payload: &[u8]) -> Result<u32, ClientVmAdmissionError> {
    if payload.len() < 17
        || payload.get(..8) != Some(MAGIC.as_slice())
        || payload
            .get(8..12)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_be_bytes)
            != Some(CAPABILITY_FORMAT_VERSION)
    {
        return Err(ClientVmAdmissionError::DecodeRejected);
    }
    let bytes = payload
        .get(13..17)
        .ok_or(ClientVmAdmissionError::DecodeRejected)?;
    Ok(u32::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| ClientVmAdmissionError::DecodeRejected)?,
    ))
}

fn invocation_id(id: ClientVmInvocationId) -> InvocationId {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&id.get().to_be_bytes());
    InvocationId::from_bytes(bytes)
}

fn decoded_plan_contains_external_contract(plan: &ClientVmDecodedPlan) -> bool {
    match plan {
        ClientVmDecodedPlan::Boolean(_) | ClientVmDecodedPlan::Opaque(_) => false,
        ClientVmDecodedPlan::Expression(plan) => {
            expression_contains_external_contract(plan.expression())
        }
        ClientVmDecodedPlan::State(plan) => {
            expression_contains_external_contract(plan.expression())
                || plan.slots().iter().any(|slot| {
                    matches!(
                        slot.default(),
                        orna_artifact::client_plan::StateDefault::Expression(expression)
                            if expression_contains_external_contract(expression)
                    )
                })
        }
        ClientVmDecodedPlan::Capability(plan) => {
            inner_plan_contains_external_contract(plan.inner_plan())
        }
        ClientVmDecodedPlan::Resource(plan) => {
            expression_contains_external_contract(plan.expression())
        }
        ClientVmDecodedPlan::Procedural(plan) => {
            plan.statements()
                .iter()
                .any(|statement| expression_contains_external_contract(statement.expression()))
                || expression_contains_external_contract(plan.return_expression())
        }
        ClientVmDecodedPlan::Action(plan) => plan
            .operation()
            .arguments()
            .iter()
            .any(|(_, expression)| expression_contains_external_contract(expression)),
        ClientVmDecodedPlan::ControlFlow(plan) => {
            statements_contain_external_contract(plan.statements())
        }
    }
}

fn inner_plan_contains_external_contract(
    plan: &orna_artifact::client_plan::InnerClientPlan,
) -> bool {
    match plan {
        orna_artifact::client_plan::InnerClientPlan::Boolean(_)
        | orna_artifact::client_plan::InnerClientPlan::Opaque(_) => false,
        orna_artifact::client_plan::InnerClientPlan::Expression(plan) => {
            expression_contains_external_contract(plan.expression())
        }
        orna_artifact::client_plan::InnerClientPlan::State(plan) => {
            expression_contains_external_contract(plan.expression())
                || plan.slots().iter().any(|slot| {
                    matches!(
                        slot.default(),
                        orna_artifact::client_plan::StateDefault::Expression(expression)
                            if expression_contains_external_contract(expression)
                    )
                })
        }
        orna_artifact::client_plan::InnerClientPlan::Resource(plan) => {
            expression_contains_external_contract(plan.expression())
        }
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan) => {
            plan.statements()
                .iter()
                .any(|statement| expression_contains_external_contract(statement.expression()))
                || expression_contains_external_contract(plan.return_expression())
        }
        orna_artifact::client_plan::InnerClientPlan::Action(plan) => plan
            .operation()
            .arguments()
            .iter()
            .any(|(_, expression)| expression_contains_external_contract(expression)),
        orna_artifact::client_plan::InnerClientPlan::ControlFlow(plan) => {
            statements_contain_external_contract(plan.statements())
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlanBudget {
    depth: u16,
    operations: u32,
}

impl PlanBudget {
    const fn leaf() -> Self {
        Self {
            depth: 1,
            operations: 1,
        }
    }

    fn include_nested(&mut self, child: Self) -> Result<(), ClientVmAdmissionError> {
        self.depth = self.depth.max(child.depth.checked_add(1).ok_or(
            ClientVmAdmissionError::LimitExceeded {
                field: "plan_depth",
            },
        )?);
        self.operations = self.operations.checked_add(child.operations).ok_or(
            ClientVmAdmissionError::LimitExceeded {
                field: "plan_operations",
            },
        )?;
        Ok(())
    }

    fn include_sibling(&mut self, child: Self) -> Result<(), ClientVmAdmissionError> {
        self.depth = self.depth.max(child.depth);
        self.operations = self.operations.checked_add(child.operations).ok_or(
            ClientVmAdmissionError::LimitExceeded {
                field: "plan_operations",
            },
        )?;
        Ok(())
    }

    fn add_operation(&mut self) -> Result<(), ClientVmAdmissionError> {
        self.operations =
            self.operations
                .checked_add(1)
                .ok_or(ClientVmAdmissionError::LimitExceeded {
                    field: "plan_operations",
                })?;
        Ok(())
    }
}

fn decoded_plan_budget(plan: &ClientVmDecodedPlan) -> Result<PlanBudget, ClientVmAdmissionError> {
    match plan {
        ClientVmDecodedPlan::Boolean(_) | ClientVmDecodedPlan::Opaque(_) => Ok(PlanBudget::leaf()),
        ClientVmDecodedPlan::Expression(plan) => expression_budget(plan.expression()),
        ClientVmDecodedPlan::State(plan) => state_plan_budget(plan),
        ClientVmDecodedPlan::Capability(plan) => {
            let mut budget = inner_plan_budget(plan.inner_plan())?;
            for _ in plan.requirements() {
                budget.add_operation()?;
            }
            Ok(budget)
        }
        ClientVmDecodedPlan::Resource(plan) => expression_budget(plan.expression()),
        ClientVmDecodedPlan::Procedural(plan) => procedural_plan_budget(plan),
        ClientVmDecodedPlan::Action(plan) => action_plan_budget(plan),
        ClientVmDecodedPlan::ControlFlow(plan) => statements_budget(plan.statements()),
    }
}

fn inner_plan_budget(
    plan: &orna_artifact::client_plan::InnerClientPlan,
) -> Result<PlanBudget, ClientVmAdmissionError> {
    match plan {
        orna_artifact::client_plan::InnerClientPlan::Boolean(_)
        | orna_artifact::client_plan::InnerClientPlan::Opaque(_) => Ok(PlanBudget::leaf()),
        orna_artifact::client_plan::InnerClientPlan::Expression(plan) => {
            expression_budget(plan.expression())
        }
        orna_artifact::client_plan::InnerClientPlan::State(plan) => state_plan_budget(plan),
        orna_artifact::client_plan::InnerClientPlan::Resource(plan) => {
            expression_budget(plan.expression())
        }
        orna_artifact::client_plan::InnerClientPlan::Procedural(plan) => {
            procedural_plan_budget(plan)
        }
        orna_artifact::client_plan::InnerClientPlan::Action(plan) => action_plan_budget(plan),
        orna_artifact::client_plan::InnerClientPlan::ControlFlow(plan) => {
            statements_budget(plan.statements())
        }
    }
}

fn expression_budget(
    expression: &ClientExpressionNode,
) -> Result<PlanBudget, ClientVmAdmissionError> {
    let mut budget = PlanBudget::leaf();
    match expression {
        ClientExpressionNode::Await { expression }
        | ClientExpressionNode::Unary { expression, .. } => {
            budget.include_nested(expression_budget(expression)?)?;
        }
        ClientExpressionNode::Resource { operation } => {
            for (_, expression) in operation.arguments() {
                budget.include_nested(expression_budget(expression)?)?;
            }
        }
        ClientExpressionNode::Inspect { operation } => {
            if let Some(expression) = operation.target() {
                budget.include_nested(expression_budget(expression)?)?;
            }
            if let Some(expression) = operation.options() {
                budget.include_nested(expression_budget(expression)?)?;
            }
            if let Some(expression) = operation.snapshot_expression() {
                budget.include_nested(expression_budget(expression)?)?;
            }
        }
        ClientExpressionNode::Action { operation } => {
            for (_, expression) in operation.arguments() {
                budget.include_nested(expression_budget(expression)?)?;
            }
        }
        ClientExpressionNode::Call { arguments, .. } => {
            for (_, expression) in arguments {
                budget.include_nested(expression_budget(expression)?)?;
            }
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            budget.include_nested(expression_budget(left)?)?;
            budget.include_nested(expression_budget(right)?)?;
        }
        ClientExpressionNode::Input => {
            budget.add_operation()?;
        }
        ClientExpressionNode::Evaluate { expression } => {
            budget.include_nested(expression_budget(expression)?)?;
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::SourceIntrospection
        | ClientExpressionNode::ExternalContract { .. } => {}
    }
    Ok(budget)
}

fn state_plan_budget(
    plan: &orna_artifact::client_plan::StateClientPlan,
) -> Result<PlanBudget, ClientVmAdmissionError> {
    let mut budget = expression_budget(plan.expression())?;
    for slot in plan.slots() {
        budget.add_operation()?;
        if let orna_artifact::client_plan::StateDefault::Expression(expression) = slot.default() {
            budget.include_sibling(expression_budget(expression)?)?;
        }
    }
    Ok(budget)
}

fn procedural_plan_budget(
    plan: &ProceduralClientPlan,
) -> Result<PlanBudget, ClientVmAdmissionError> {
    let mut budget = expression_budget(plan.return_expression())?;
    for statement in plan.statements() {
        budget.add_operation()?;
        budget.include_sibling(expression_budget(statement.expression())?)?;
    }
    Ok(budget)
}

fn action_plan_budget(plan: &ActionClientPlan) -> Result<PlanBudget, ClientVmAdmissionError> {
    let mut budget = PlanBudget::leaf();
    for (_, expression) in plan.operation().arguments() {
        budget.include_nested(expression_budget(expression)?)?;
    }
    Ok(budget)
}

fn statements_budget(
    statements: &[ControlFlowStatement],
) -> Result<PlanBudget, ClientVmAdmissionError> {
    let mut budget = PlanBudget {
        depth: 0,
        operations: 0,
    };
    for statement in statements {
        let mut statement_budget = PlanBudget::leaf();
        if let Some(expression) = statement.expression() {
            statement_budget.include_nested(expression_budget(expression)?)?;
        }
        if let Some(return_statement) = statement.return_statement()
            && let Some(expression) = return_statement.expression()
        {
            statement_budget.include_nested(expression_budget(expression)?)?;
        }
        if let Some(if_statement) = statement.if_statement() {
            for branch in if_statement.branches() {
                statement_budget.include_nested(expression_budget(branch.condition())?)?;
                statement_budget.include_sibling(statements_budget(branch.statements())?)?;
            }
            if let Some(statements) = if_statement.else_statements() {
                statement_budget.include_sibling(statements_budget(statements)?)?;
            }
        }
        if let Some(while_statement) = statement.while_statement() {
            statement_budget.include_nested(expression_budget(while_statement.condition())?)?;
            statement_budget.include_sibling(statements_budget(while_statement.statements())?)?;
        }
        budget.include_sibling(statement_budget)?;
    }
    Ok(budget)
}

fn expression_contains_external_contract(expression: &ClientExpressionNode) -> bool {
    match expression {
        ClientExpressionNode::ExternalContract { .. } => true,
        ClientExpressionNode::Await { expression }
        | ClientExpressionNode::Unary { expression, .. } => {
            expression_contains_external_contract(expression)
        }
        ClientExpressionNode::Resource { operation } => operation
            .arguments()
            .iter()
            .any(|(_, expression)| expression_contains_external_contract(expression)),
        ClientExpressionNode::Inspect { operation } => {
            operation
                .target()
                .is_some_and(expression_contains_external_contract)
                || operation
                    .options()
                    .is_some_and(expression_contains_external_contract)
                || operation
                    .snapshot_expression()
                    .is_some_and(expression_contains_external_contract)
        }
        ClientExpressionNode::Action { operation } => operation
            .arguments()
            .iter()
            .any(|(_, expression)| expression_contains_external_contract(expression)),
        ClientExpressionNode::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, expression)| expression_contains_external_contract(expression)),
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            expression_contains_external_contract(left)
                || expression_contains_external_contract(right)
        }
        ClientExpressionNode::Input => false,
        ClientExpressionNode::Evaluate { expression } => {
            expression_contains_external_contract(expression)
        }
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::SourceIntrospection => false,
    }
}

fn statements_contain_external_contract(statements: &[ControlFlowStatement]) -> bool {
    statements.iter().any(|statement| {
        statement
            .expression()
            .is_some_and(expression_contains_external_contract)
            || statement
                .return_statement()
                .and_then(|return_statement| return_statement.expression())
                .is_some_and(expression_contains_external_contract)
            || statement.if_statement().is_some_and(|statement| {
                statement.branches().iter().any(|branch| {
                    expression_contains_external_contract(branch.condition())
                        || statements_contain_external_contract(branch.statements())
                }) || statement
                    .else_statements()
                    .is_some_and(statements_contain_external_contract)
            })
            || statement.while_statement().is_some_and(|statement| {
                expression_contains_external_contract(statement.condition())
                    || statements_contain_external_contract(statement.statements())
            })
    })
}

/// An error raised while advancing Stage 1 host-control epochs or binding a
/// root to its first authorised function, function revision, database revision,
/// and security context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientVmHostContextError {
    /// The epoch reached the end of its checked integer range.
    EpochExhausted,
    /// The root was already bound to a different function, function revision,
    /// revision pair, or security digest.
    RootBindingMismatch,
}

impl fmt::Display for ClientVmHostContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EpochExhausted => formatter.write_str("CLIENT VM host context epoch exhausted"),
            Self::RootBindingMismatch => {
                formatter.write_str("CLIENT VM root binding does not match the existing binding")
            }
        }
    }
}

impl std::error::Error for ClientVmHostContextError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientVmRootBinding {
    function: [u8; 16],
    function_revision: [u8; 16],
    revision_pair: [[u8; 16]; 2],
    security_context_digest: [u8; 32],
}

/// The in-memory Stage 1 host context for one CLIENT VM root.
///
/// This context owns its root and child identities in a shared host-wide
/// registry, keeps a local ownership set for lease issuance, and retains the
/// immutable runtime-offer witness plus mutable policy/cancellation epochs. It
/// performs no host operation and does not issue a production capability lease.
#[derive(Debug)]
pub struct ClientVmHostContext {
    root_invocation_id: ClientVmInvocationId,
    invocation_registry: ClientVmInvocationRegistry,
    owned_invocations: HashSet<ClientVmInvocationId>,
    runtime_offer: RuntimeOfferWitness,
    lease_fences: Arc<lease::LeaseFences>,
    root_binding: Option<ClientVmRootBinding>,
    policy_epoch: u64,
    cancellation_epoch: u64,
    cancelled: bool,
    host_limit_ceiling: ClientVmArtifactLimits,
}

impl ClientVmHostContext {
    /// Creates a Stage 1 host context and allocates its root identity.
    pub fn new(
        invocation_registry: &ClientVmInvocationRegistry,
        runtime_offer: RuntimeOfferWitness,
        host_limit_ceiling: ClientVmArtifactLimits,
    ) -> Result<Self, ClientVmIdentityError> {
        let root_invocation_id = invocation_registry.allocate_root()?;
        let owned_invocations = HashSet::from([root_invocation_id]);
        Ok(Self {
            root_invocation_id,
            invocation_registry: invocation_registry.clone(),
            owned_invocations,
            runtime_offer,
            lease_fences: lease::LeaseFences::new(0, 0),
            root_binding: None,
            policy_epoch: 0,
            cancellation_epoch: 0,
            cancelled: false,
            host_limit_ceiling,
        })
    }

    /// Returns the non-zero root invocation identity.
    pub const fn root_invocation_id(&self) -> ClientVmInvocationId {
        self.root_invocation_id
    }

    /// Allocates a fresh child identity under an identity owned by this context.
    pub fn allocate_child(
        &mut self,
        parent: ClientVmInvocationId,
    ) -> Result<ClientVmInvocationId, ClientVmIdentityError> {
        if self.cancelled {
            return Err(ClientVmIdentityError::Cancelled);
        }
        if self.root_binding.is_none() {
            return Err(ClientVmIdentityError::UnboundRoot);
        }
        if !self.owned_invocations.contains(&parent) {
            return Err(ClientVmIdentityError::InvalidParent);
        }
        let child = self.invocation_registry.allocate_child(parent)?;
        let inserted = self.owned_invocations.insert(child);
        debug_assert!(inserted);
        Ok(child)
    }

    /// Issues an ephemeral Stage 1 lease for an allocator-owned identity.
    ///
    /// The returned lease is an in-memory state-machine value. It carries no
    /// host handle, credential, serialisation format, or production authority.
    pub fn issue_ephemeral_lease(
        &self,
        invocation_id: ClientVmInvocationId,
    ) -> Result<EphemeralCapabilityLease, ClientVmIdentityError> {
        if self.cancelled {
            return Err(ClientVmIdentityError::Cancelled);
        }
        if self.root_binding.is_none() {
            return Err(ClientVmIdentityError::UnboundRoot);
        }
        if !self.owned_invocations.contains(&invocation_id) {
            return Err(ClientVmIdentityError::InvalidParent);
        }
        EphemeralCapabilityLease::new_with_fences(
            invocation_id.get(),
            self.policy_epoch,
            self.cancellation_epoch,
            self.lease_fences.clone(),
        )
        .map_err(|_| ClientVmIdentityError::Zero)
    }

    fn check_root_binding(
        &self,
        function: [u8; 16],
        function_revision: [u8; 16],
        revision_pair: [[u8; 16]; 2],
        security_context_digest: [u8; 32],
    ) -> Result<(), ClientVmHostContextError> {
        let binding = ClientVmRootBinding {
            function,
            function_revision,
            revision_pair,
            security_context_digest,
        };
        if self.root_binding.is_none_or(|current| current == binding) {
            return Ok(());
        }
        Err(ClientVmHostContextError::RootBindingMismatch)
    }

    fn bind_root(
        &mut self,
        function: [u8; 16],
        function_revision: [u8; 16],
        revision_pair: [[u8; 16]; 2],
        security_context_digest: [u8; 32],
    ) -> Result<(), ClientVmHostContextError> {
        self.check_root_binding(
            function,
            function_revision,
            revision_pair,
            security_context_digest,
        )?;
        self.root_binding = Some(ClientVmRootBinding {
            function,
            function_revision,
            revision_pair,
            security_context_digest,
        });
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    #[cfg(test)]
    pub(crate) fn has_root_binding(&self) -> bool {
        self.root_binding.is_some()
    }

    /// Returns the immutable runtime-offer witness.
    pub const fn runtime_offer(&self) -> &RuntimeOfferWitness {
        &self.runtime_offer
    }

    /// Returns the current grant-policy epoch.
    pub const fn policy_epoch(&self) -> u64 {
        self.policy_epoch
    }

    /// Returns the current cancellation epoch.
    pub const fn cancellation_epoch(&self) -> u64 {
        self.cancellation_epoch
    }

    /// Returns the host execution ceiling.
    pub const fn host_limit_ceiling(&self) -> ClientVmArtifactLimits {
        self.host_limit_ceiling
    }

    /// Returns a copied admission snapshot for the current host state.
    pub const fn admission_context(&self) -> ClientVmHostAdmissionContext {
        ClientVmHostAdmissionContext::new(
            self.policy_epoch,
            self.runtime_offer.digest(),
            self.host_limit_ceiling,
            self.cancellation_epoch,
        )
    }

    /// Returns whether an admission still matches this root's live fences.
    ///
    /// The check is read-only. Callers must run it before using an admitted
    /// plan after a policy, runtime, revision, or cancellation transition.
    pub fn admission_is_current<T>(&self, admission: &ClientVmAdmission<T>) -> bool {
        let Some(binding) = self.root_binding else {
            return false;
        };
        let host = admission.host();
        binding.function == admission.identity().function()
            && binding.function_revision == admission.identity().function_revision()
            && binding.revision_pair == admission.identity().revision_pair()
            && binding.security_context_digest == host.security_context_digest()
            && host.policy_epoch() == self.policy_epoch
            && host.runtime_offer_digest() == self.runtime_offer.digest()
            && host.host_limit_ceiling() == self.host_limit_ceiling
            && host.cancellation_epoch() == self.cancellation_epoch
            && !self.cancelled
    }

    /// Advances the policy epoch without wrapping and invalidates old leases.
    pub fn advance_policy_epoch(&mut self) -> Result<(), ClientVmHostContextError> {
        let next = self
            .policy_epoch
            .checked_add(1)
            .ok_or(ClientVmHostContextError::EpochExhausted)?;
        self.policy_epoch = next;
        self.lease_fences.set_policy(next);
        Ok(())
    }

    /// Advances cancellation, invalidates old leases, and closes this root to
    /// new children, leases, and admissions.
    pub fn advance_cancellation_epoch(&mut self) -> Result<(), ClientVmHostContextError> {
        if self.cancelled {
            return Ok(());
        }
        let next = self
            .cancellation_epoch
            .checked_add(1)
            .ok_or(ClientVmHostContextError::EpochExhausted)?;
        self.cancellation_epoch = next;
        self.cancelled = true;
        self.lease_fences.set_cancellation(next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_offer() -> RuntimeOfferWitness {
        RuntimeOfferWitness::from_parts(
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
        .expect("test runtime offer")
    }

    fn limits() -> ClientVmArtifactLimits {
        ClientVmArtifactLimits::new(1024, 64, 1024).expect("test limits")
    }

    #[test]
    fn host_context_binds_root_children_witness_and_epochs() {
        let registry = ClientVmInvocationRegistry::new();
        let mut context =
            ClientVmHostContext::new(&registry, runtime_offer(), limits()).expect("host context");
        let mut other =
            ClientVmHostContext::new(&registry, runtime_offer(), limits()).expect("other context");
        context
            .bind_root([11; 16], [12; 16], [[1; 16], [2; 16]], [3; 32])
            .expect("context root binding");
        other
            .bind_root([11; 16], [12; 16], [[1; 16], [2; 16]], [3; 32])
            .expect("other root binding");
        let root = context.root_invocation_id();
        let child = context.allocate_child(root).expect("child identity");
        let other_root = other.root_invocation_id();
        let other_child = other
            .allocate_child(other_root)
            .expect("other child identity");
        assert_ne!(root, child);
        assert_ne!(root, other_root);
        assert_ne!(root, other_child);
        assert_ne!(child, other_root);
        assert_ne!(child, other_child);
        assert_ne!(other_root, other_child);
        assert_eq!(context.runtime_offer().thread_model(), 3);
        assert_eq!(
            context.admission_context().runtime_offer_digest(),
            context.runtime_offer().digest()
        );

        let mut lease = context.issue_ephemeral_lease(root).expect("root lease");
        assert_eq!(lease.policy_fence(), 0);
        assert_eq!(lease.cancellation_fence(), 0);
        lease.revoke().expect("revoke root lease");
        lease.release().expect("release root lease");
        assert_eq!(lease.state(), LeaseState::Released);
        assert!(matches!(
            context.issue_ephemeral_lease(ClientVmInvocationId::new(99).unwrap()),
            Err(ClientVmIdentityError::InvalidParent)
        ));

        context.advance_policy_epoch().expect("policy epoch");
        context
            .advance_cancellation_epoch()
            .expect("cancellation epoch");
        assert_eq!(context.policy_epoch(), 1);
        assert_eq!(context.cancellation_epoch(), 1);
    }

    #[test]
    fn host_context_retains_nested_children_for_release_leases() {
        let registry = ClientVmInvocationRegistry::new();
        let mut context =
            ClientVmHostContext::new(&registry, runtime_offer(), limits()).expect("host context");
        context
            .bind_root([13; 16], [14; 16], [[8; 16], [9; 16]], [10; 32])
            .expect("context root binding");
        let root = context.root_invocation_id();
        let child = context.allocate_child(root).expect("child identity");
        let grandchild = context.allocate_child(child).expect("grandchild identity");
        let child_lease = context.issue_ephemeral_lease(child).expect("child lease");
        let grandchild_lease = context
            .issue_ephemeral_lease(grandchild)
            .expect("grandchild lease");

        assert_eq!(child_lease.invocation_id(), child.get());
        assert_eq!(grandchild_lease.invocation_id(), grandchild.get());
    }

    #[test]
    fn host_epochs_invalidate_leases_and_close_cancelled_roots() {
        let registry = ClientVmInvocationRegistry::new();
        let mut context =
            ClientVmHostContext::new(&registry, runtime_offer(), limits()).expect("host context");
        context
            .bind_root([15; 16], [16; 16], [[5; 16], [6; 16]], [7; 32])
            .expect("context root binding");
        let root = context.root_invocation_id();
        let mut lease = context.issue_ephemeral_lease(root).expect("root lease");
        lease.acquire().expect("lease use");
        lease.effect_intent().expect("effect intent");

        context.advance_policy_epoch().expect("policy epoch");
        assert!(matches!(
            lease.effect_started(0, 0),
            Err(LeaseError::FenceMismatch {
                actual_policy_fence: 1,
                actual_cancellation_fence: 0,
                ..
            })
        ));

        context
            .advance_cancellation_epoch()
            .expect("cancellation epoch");
        context
            .advance_cancellation_epoch()
            .expect("repeated cancellation");
        assert_eq!(context.cancellation_epoch(), 1);
        assert_eq!(
            context.allocate_child(root),
            Err(ClientVmIdentityError::Cancelled)
        );
        assert!(matches!(
            context.issue_ephemeral_lease(root),
            Err(ClientVmIdentityError::Cancelled)
        ));
    }

    #[test]
    fn root_binding_requires_exact_function_and_revision_identity() {
        let registry = ClientVmInvocationRegistry::new();
        let mut context =
            ClientVmHostContext::new(&registry, runtime_offer(), limits()).expect("host context");
        context
            .bind_root([17; 16], [18; 16], [[1; 16], [2; 16]], [3; 32])
            .expect("initial root binding");
        context
            .check_root_binding([17; 16], [18; 16], [[1; 16], [2; 16]], [3; 32])
            .expect("same root binding");
        assert_eq!(
            context.check_root_binding([19; 16], [18; 16], [[1; 16], [2; 16]], [3; 32]),
            Err(ClientVmHostContextError::RootBindingMismatch)
        );
        assert_eq!(
            context.check_root_binding([17; 16], [20; 16], [[1; 16], [2; 16]], [3; 32]),
            Err(ClientVmHostContextError::RootBindingMismatch)
        );
        assert_eq!(
            context.check_root_binding([17; 16], [18; 16], [[4; 16], [2; 16]], [3; 32]),
            Err(ClientVmHostContextError::RootBindingMismatch)
        );
    }

    #[test]
    fn unbound_root_cannot_issue_children_or_leases() {
        let registry = ClientVmInvocationRegistry::new();
        let mut context =
            ClientVmHostContext::new(&registry, runtime_offer(), limits()).expect("host context");
        let root = context.root_invocation_id();
        assert_eq!(
            context.allocate_child(root),
            Err(ClientVmIdentityError::UnboundRoot)
        );
        assert!(matches!(
            context.issue_ephemeral_lease(root),
            Err(ClientVmIdentityError::UnboundRoot)
        ));
    }
}
