//! Checked sealed values for the `sys.invoke` boundary.
//!
//! This module stores logical carrier data only. ORV5 bytes, envelope
//! positions, and frame rules belong to `orna-protocol`.

use std::{cmp::Ordering, collections::BTreeSet, error::Error, fmt};

use crate::{
    FunctionId, FunctionRevisionId, InvocationId, ParameterId, PrincipalId,
    StandardLibraryRevisionId, TypeId,
    catalogue::{CatalogueSnapshot, FunctionDefinition, FunctionSecurity, QualifiedSemanticName},
    revision::{ActiveDatabaseRevision, VerifiedStandardLibrarySnapshot},
    security::{
        AuthenticatedSession, ExecuteDecision, InvocationTarget as SecurityInvocationTarget,
        SecuritySnapshot,
    },
    system::{
        InvocationCarrierKind, SYS_INVOKE_EVENT_TYPE_ID, SYS_INVOKE_REQUEST_TYPE_ID,
        SYS_INVOKE_VALUE_TYPE_ID, SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SystemFunctionDefinition, SystemFunctionKind, invocation_carrier_by_id,
        system_function_by_id, system_function_by_name,
    },
    types::{TypeDescriptor, TypeDescriptorKind},
    value::{RuntimeType, RuntimeValue, count_invocation_runtime_value_nodes},
};

/// The largest accepted aggregate node count in one invocation carrier tree.
pub const MAX_INVOCATION_CARRIER_NODES: usize = 65_536;

/// The closed redacted category from one protected invocation decision.
///
/// This result contains no target, signature, selector, value, binding, or
/// security evidence. A durable audit slice consumes only this category.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectedInvocationDecision {
    /// The exact sealed `sys.invoke` entry was rejected before request use.
    EntryDenied,
    /// The checked request did not match the authenticated protocol context.
    RequestRejected,
    /// A private target or final security decision denied invocation.
    Denied,
    /// A final security decision allowed a complete prebind.
    Allowed,
    /// A final security decision allowed disclosure of a redacted bind failure.
    AllowedWithBindFailure,
}

/// Makes one complete protected invocation decision without exposing phases.
///
/// The caller must decode the retained Request envelope before this operation.
/// `orna-core` does not decode ORV5 bytes, so it keeps the protocol boundary
/// acyclic. This operation first checks the exact system entry, then checks
/// the authenticated protocol major. It privately resolves one target in the
/// pinned application and verified-standard catalogues, makes base `EXECUTE`
/// authorisation, prebinds, and makes the final closed security decision.
///
/// Version 1 has no definer-owner, policy, or capability model. A target that
/// requires unavailable semantics is denied before any binding fact escapes.
pub fn decide_protected_invocation(
    security: &SecuritySnapshot,
    session: &AuthenticatedSession,
    system_target: SecurityInvocationTarget,
    active: &ActiveDatabaseRevision,
    connection_protocol_major: u16,
    request: &InvokeRequest,
) -> ProtectedInvocationDecision {
    if !matches!(
        security.authorise_sys_invoke_entry(session, system_target),
        ExecuteDecision::Allowed(_)
    ) {
        return ProtectedInvocationDecision::EntryDenied;
    }

    if request.client_offer().protocol_major() != connection_protocol_major {
        return ProtectedInvocationDecision::RequestRejected;
    }

    let Some(target) = resolve_target_privately(active, request.target()) else {
        return ProtectedInvocationDecision::Denied;
    };
    let security_target = SecurityInvocationTarget::new(target.id(), active.pair());
    let authorised = match target.class() {
        PrivateTargetClass::System => security.authorise_system_function(session, security_target),
        PrivateTargetClass::Application | PrivateTargetClass::VerifiedStandard => {
            let security_target = match target.class() {
                PrivateTargetClass::Application => security_target,
                PrivateTargetClass::VerifiedStandard => {
                    let Some(standard_revision) = target.standard_revision() else {
                        return ProtectedInvocationDecision::Denied;
                    };
                    SecurityInvocationTarget::verified_standard(
                        target.id(),
                        active.pair(),
                        standard_revision,
                        target.executable_revision(),
                    )
                }
                PrivateTargetClass::System => {
                    unreachable!("system targets use system authorisation")
                }
            };
            security.authorise_execute(session, security_target)
        }
    };
    if !matches!(authorised, ExecuteDecision::Allowed(_)) {
        return ProtectedInvocationDecision::Denied;
    }

    let prebind = match target.class() {
        PrivateTargetClass::System => {
            if target.system_definition().is_none() {
                return ProtectedInvocationDecision::Denied;
            }
            prebind_system_privately(request)
        }
        PrivateTargetClass::Application | PrivateTargetClass::VerifiedStandard => {
            let Some(definition) = target.definition() else {
                return ProtectedInvocationDecision::Denied;
            };
            if definition.security() != FunctionSecurity::Invoker {
                return ProtectedInvocationDecision::Denied;
            }
            prebind_privately(definition, request)
        }
    };

    match prebind {
        PrivatePrebind::Complete => ProtectedInvocationDecision::Allowed,
        PrivatePrebind::Failed => ProtectedInvocationDecision::AllowedWithBindFailure,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrivatePrebind {
    Complete,
    Failed,
}

/// The closed target class of one privately resolved invocation target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrivateTargetClass {
    /// A function in the pinned application catalogue.
    Application,
    /// A function in the exact verified standard snapshot pinned by the
    /// active application revision.
    VerifiedStandard,
    /// One of the explicitly admitted sealed system identity functions.
    System,
}

/// One privately resolved invocation target with its immutable executable pin.
///
/// Application and verified-standard targets retain their catalogue definition
/// and executable pins. A system target retains only its sealed registry
/// definition: it never fabricates or borrows a `FunctionDefinition`.
#[derive(Clone, Debug)]
enum PrivateResolvedTarget {
    Application {
        definition: FunctionDefinition,
    },
    VerifiedStandard {
        definition: FunctionDefinition,
        standard_revision: StandardLibraryRevisionId,
        executable_revision: FunctionRevisionId,
    },
    System {
        definition: SystemFunctionDefinition,
    },
}

impl PrivateResolvedTarget {
    fn new_application(definition: FunctionDefinition) -> Self {
        Self::Application { definition }
    }

    fn new_verified_standard(
        definition: FunctionDefinition,
        standard_revision: StandardLibraryRevisionId,
        executable_revision: FunctionRevisionId,
    ) -> Self {
        Self::VerifiedStandard {
            definition,
            standard_revision,
            executable_revision,
        }
    }

    fn new_system(definition: SystemFunctionDefinition) -> Option<Self> {
        if !matches!(definition.kind(), SystemFunctionKind::SecurityIdentity) {
            return None;
        }
        match definition.id() {
            SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
            | SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID
            | SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID => Some(Self::System { definition }),
            _ => None,
        }
    }

    /// Returns the stable target function identity.
    fn id(&self) -> FunctionId {
        match self {
            Self::Application { definition } | Self::VerifiedStandard { definition, .. } => {
                definition.id()
            }
            Self::System { definition } => definition.id(),
        }
    }

    /// Returns the complete catalogue function definition, when applicable.
    fn definition(&self) -> Option<&FunctionDefinition> {
        match self {
            Self::Application { definition } | Self::VerifiedStandard { definition, .. } => {
                Some(definition)
            }
            Self::System { .. } => None,
        }
    }

    /// Returns the sealed system-function definition, when applicable.
    fn system_definition(&self) -> Option<SystemFunctionDefinition> {
        match self {
            Self::System { definition } => Some(*definition),
            Self::Application { .. } | Self::VerifiedStandard { .. } => None,
        }
    }

    /// Returns the closed target class.
    fn class(&self) -> PrivateTargetClass {
        match self {
            Self::Application { .. } => PrivateTargetClass::Application,
            Self::VerifiedStandard { .. } => PrivateTargetClass::VerifiedStandard,
            Self::System { .. } => PrivateTargetClass::System,
        }
    }

    /// Returns the exact pinned executable function revision.
    fn executable_revision(&self) -> FunctionRevisionId {
        match self {
            Self::VerifiedStandard {
                executable_revision,
                ..
            } => *executable_revision,
            Self::Application { definition } => definition.current_revision(),
            Self::System { .. } => {
                unreachable!("system targets do not carry executable revisions")
            }
        }
    }

    /// Returns the exact verified standard snapshot revision for a standard target.
    fn standard_revision(&self) -> Option<StandardLibraryRevisionId> {
        match self {
            Self::VerifiedStandard {
                standard_revision, ..
            } => Some(*standard_revision),
            Self::Application { .. } | Self::System { .. } => None,
        }
    }
}

fn resolve_target_privately(
    active: &ActiveDatabaseRevision,
    selector: &InvocationTarget,
) -> Option<PrivateResolvedTarget> {
    let system = match selector {
        InvocationTarget::FunctionId(id) => system_function_by_id(*id),
        InvocationTarget::QualifiedName(name) => system_function_by_name(name),
    };
    if let Some(system) = system {
        return PrivateResolvedTarget::new_system(system);
    }

    let application = active.catalogue();
    let standard = active.catalogue_hash_context().standard();
    resolve_target_in_catalogues(application, standard, selector)
}

fn resolve_target_in_catalogues(
    application: &CatalogueSnapshot,
    standard: Option<&VerifiedStandardLibrarySnapshot>,
    selector: &InvocationTarget,
) -> Option<PrivateResolvedTarget> {
    let application_target = match selector {
        InvocationTarget::FunctionId(id) => application.function_by_id(*id),
        InvocationTarget::QualifiedName(name) => application.function_by_name(name),
    };
    let standard_target = standard.and_then(|snapshot| match selector {
        InvocationTarget::FunctionId(id) => snapshot.catalogue().function_by_id(*id),
        InvocationTarget::QualifiedName(name) => snapshot.catalogue().function_by_name(name),
    });
    match (application_target, standard_target) {
        // A function in both catalogues is ambiguous and resolves to neither.
        (Some(_), Some(_)) | (None, None) => None,
        (Some(function), None) => Some(PrivateResolvedTarget::new_application(function.clone())),
        (None, Some(function)) => {
            let standard = standard.expect("a standard target requires the pinned snapshot");
            let executable = standard
                .executables()
                .iter()
                .find(|executable| executable.function() == function.id())?;
            if executable.revision().id() != function.current_revision() {
                return None;
            }
            Some(PrivateResolvedTarget::new_verified_standard(
                function.clone(),
                standard.revision(),
                executable.revision().id(),
            ))
        }
    }
}

fn prebind_system_privately(request: &InvokeRequest) -> PrivatePrebind {
    if request.arguments().is_empty() {
        PrivatePrebind::Complete
    } else {
        PrivatePrebind::Failed
    }
}

fn prebind_privately(function: &FunctionDefinition, request: &InvokeRequest) -> PrivatePrebind {
    let mut bound = BTreeSet::new();
    for argument in request.arguments() {
        let parameter = match argument.selector() {
            InvocationParameterSelector::ParameterId(id) => function.parameter_by_id(*id),
            InvocationParameterSelector::Name(name) => function.parameter_by_name(name),
        };
        let Some(parameter) = parameter else {
            return PrivatePrebind::Failed;
        };
        if !bound.insert(parameter.id())
            || argument.value().value().runtime_type()
                != RuntimeType::Flat(parameter.resolved_type())
        {
            return PrivatePrebind::Failed;
        }
    }
    if function.parameters().iter().any(|parameter| {
        parameter.default_expression().is_none() && !bound.contains(&parameter.id())
    }) {
        return PrivatePrebind::Failed;
    }
    PrivatePrebind::Complete
}

/// One failure from checked invocation-carrier construction.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationCarrierConstructionError {
    /// The complete carrier tree exceeds its aggregate node bound.
    TooManyNodes {
        /// The accepted maximum node count.
        maximum: usize,
    },
    /// A carrier occurs where ordinary runtime data is required.
    NestedCarrier {
        /// The exact rejected carrier identity.
        carrier: TypeId,
    },
    /// One logical field is invalid for version 1.
    InvalidField {
        /// The invalid field.
        field: InvocationCarrierField,
    },
    /// One supplied sequence is not in the required canonical order.
    NonCanonicalOrder {
        /// The non-canonical sequence.
        field: InvocationCarrierField,
    },
    /// One supplied sequence contains an exact duplicate.
    DuplicateItem {
        /// The sequence that contains the duplicate.
        field: InvocationCarrierField,
    },
}

impl fmt::Display for InvocationCarrierConstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyNodes { .. } => {
                formatter.write_str("invocation carrier tree has too many nodes")
            }
            Self::NestedCarrier { .. } => {
                formatter.write_str("invocation carrier cannot contain another carrier here")
            }
            Self::InvalidField { .. } => formatter.write_str("invocation carrier field is invalid"),
            Self::NonCanonicalOrder { .. } => {
                formatter.write_str("invocation carrier items are not in canonical order")
            }
            Self::DuplicateItem { .. } => {
                formatter.write_str("invocation carrier contains a duplicate item")
            }
        }
    }
}

impl Error for InvocationCarrierConstructionError {}

/// One logical location checked during carrier construction.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationCarrierField {
    /// The request target name.
    Target,
    /// The ordered request arguments.
    Arguments,
    /// One parameter-name selector.
    ParameterSelector,
    /// The caller context.
    CallerContext,
    /// The client offer.
    ClientOffer,
    /// The client sink offers.
    SinkOffers,
    /// One sink media-type sequence.
    SinkMediaTypes,
    /// The client runtime offers.
    RuntimeOffers,
    /// One runtime contract feature sequence.
    RuntimeContractFeatures,
    /// The output requirement.
    OutputRequirement,
    /// The output type-selector name.
    OutputTypeSelector,
    /// The state profile.
    StateProfile,
    /// The idempotency key.
    IdempotencyKey,
    /// The event value batch.
    ValueBatch,
    /// One diagnostic stable code.
    DiagnosticCode,
    /// One failure stable code.
    FailureCode,
    /// One cancellation reason.
    CancellationReason,
}

/// One checked typed value embedded in an invocation carrier.
#[derive(Clone, PartialEq)]
pub struct InvokeValue {
    value: Box<RuntimeValue>,
    node_count: usize,
}

impl InvokeValue {
    /// Checks and retains one complete ordinary runtime value.
    pub fn new(value: RuntimeValue) -> Result<Self, InvocationCarrierConstructionError> {
        let mut node_count = 1;
        add_nodes(
            &mut node_count,
            count_invocation_runtime_value_nodes(&value)?,
        )?;
        Ok(Self {
            value: Box::new(value),
            node_count,
        })
    }

    /// Returns the retained checked ordinary runtime value.
    pub const fn value(&self) -> &RuntimeValue {
        &self.value
    }

    /// Returns the aggregate node count, including this wrapper.
    pub const fn node_count(&self) -> usize {
        self.node_count
    }

    /// Transfers the complete checked ordinary runtime value.
    pub fn into_value(self) -> RuntimeValue {
        *self.value
    }
}

impl fmt::Debug for InvokeValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InvokeValue")
            .field("node_count", &self.node_count)
            .finish()
    }
}

/// One request target selector.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationTarget {
    /// One exact function identity.
    FunctionId(FunctionId),
    /// One resolved qualified function name.
    QualifiedName(QualifiedSemanticName),
}

impl InvocationTarget {
    /// Creates one checked direct function-identity target.
    pub const fn function_id(function_id: FunctionId) -> Self {
        Self::FunctionId(function_id)
    }

    /// Creates one checked resolved qualified-name target.
    pub fn qualified_name(
        name: QualifiedSemanticName,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        require_qualified(&name, InvocationCarrierField::Target)?;
        Ok(Self::QualifiedName(name))
    }
}

/// One request argument selector.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationParameterSelector {
    /// One exact parameter identity.
    ParameterId(ParameterId),
    /// One resolved semantic parameter name.
    Name(String),
}

impl InvocationParameterSelector {
    /// Creates one checked parameter-identity selector.
    pub const fn parameter_id(parameter_id: ParameterId) -> Self {
        Self::ParameterId(parameter_id)
    }

    /// Creates one checked semantic parameter-name selector.
    pub fn name(name: impl Into<String>) -> Result<Self, InvocationCarrierConstructionError> {
        let name = name.into();
        require_non_empty(&name, InvocationCarrierField::ParameterSelector)?;
        Ok(Self::Name(name))
    }
}

/// One canonical typed request argument.
#[derive(Clone, Debug, PartialEq)]
pub struct InvocationArgument {
    selector: InvocationParameterSelector,
    value: InvokeValue,
}

impl InvocationArgument {
    /// Creates one complete typed request argument.
    pub const fn new(selector: InvocationParameterSelector, value: InvokeValue) -> Self {
        Self { selector, value }
    }

    /// Returns the checked argument selector.
    pub const fn selector(&self) -> &InvocationParameterSelector {
        &self.selector
    }

    /// Returns the checked typed argument value.
    pub const fn value(&self) -> &InvokeValue {
        &self.value
    }
}

/// The caller kind recorded in a version-1 invocation request.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationCallerKind {
    /// A command-line terminal.
    CliTty,
    /// A command-line pipe.
    CliPipe,
    /// The desktop launcher.
    DesktopLauncher,
    /// A browser client.
    Browser,
    /// A client function.
    ClientFunction,
    /// A JSON-RPC gateway.
    JsonRpcGateway,
    /// An MCP gateway.
    McpGateway,
    /// A scheduler.
    Scheduler,
    /// A test runner.
    TestRunner,
    /// Recovery work.
    Recovery,
}

/// One checked caller context.
#[derive(Clone, Debug, PartialEq)]
pub struct InvocationCallerContext {
    kind: InvocationCallerKind,
    interactive: bool,
    stdout_is_tty: bool,
    terminal_columns: Option<u32>,
    terminal_rows: Option<u32>,
    locale: String,
    timezone: String,
    preference_policy: Option<InvokeValue>,
}

impl InvocationCallerContext {
    /// Creates one checked caller context.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: InvocationCallerKind,
        interactive: bool,
        stdout_is_tty: bool,
        terminal_columns: Option<u32>,
        terminal_rows: Option<u32>,
        locale: impl Into<String>,
        timezone: impl Into<String>,
        preference_policy: Option<InvokeValue>,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let locale = locale.into();
        let timezone = timezone.into();
        require_non_empty(&locale, InvocationCarrierField::CallerContext)?;
        require_non_empty(&timezone, InvocationCarrierField::CallerContext)?;
        if terminal_columns == Some(0) || terminal_rows == Some(0) {
            return Err(invalid(InvocationCarrierField::CallerContext));
        }
        match kind {
            InvocationCallerKind::CliTty
                if !interactive
                    || !stdout_is_tty
                    || terminal_columns.is_none()
                    || terminal_rows.is_none() =>
            {
                return Err(invalid(InvocationCarrierField::CallerContext));
            }
            InvocationCallerKind::CliPipe if interactive || stdout_is_tty => {
                return Err(invalid(InvocationCarrierField::CallerContext));
            }
            _ => {}
        }
        Ok(Self {
            kind,
            interactive,
            stdout_is_tty,
            terminal_columns,
            terminal_rows,
            locale,
            timezone,
            preference_policy,
        })
    }

    /// Returns the caller kind.
    pub const fn kind(&self) -> InvocationCallerKind {
        self.kind
    }
    /// Returns whether the caller is interactive.
    pub const fn interactive(&self) -> bool {
        self.interactive
    }
    /// Returns whether stdout is a terminal.
    pub const fn stdout_is_tty(&self) -> bool {
        self.stdout_is_tty
    }
    /// Returns the optional terminal column count.
    pub const fn terminal_columns(&self) -> Option<u32> {
        self.terminal_columns
    }
    /// Returns the optional terminal row count.
    pub const fn terminal_rows(&self) -> Option<u32> {
        self.terminal_rows
    }
    /// Returns the exact locale policy input.
    pub fn locale(&self) -> &str {
        &self.locale
    }
    /// Returns the exact timezone policy input.
    pub fn timezone(&self) -> &str {
        &self.timezone
    }
    /// Returns the optional typed preference-policy value.
    pub const fn preference_policy(&self) -> Option<&InvokeValue> {
        self.preference_policy.as_ref()
    }
}

/// One checked client sink offer.
#[derive(Clone, Debug, PartialEq)]
pub struct InvocationSinkOffer {
    descriptor: TypeDescriptor,
    media_types: Vec<String>,
    streaming: bool,
    preference_rank: i32,
    limits: Option<InvokeValue>,
}

impl InvocationSinkOffer {
    /// Creates one checked sink offer.
    pub fn new(
        descriptor: TypeDescriptor,
        media_types: impl IntoIterator<Item = impl Into<String>>,
        streaming: bool,
        preference_rank: i32,
        limits: Option<InvokeValue>,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let media_types = media_types.into_iter().map(Into::into).collect::<Vec<_>>();
        require_supported_descriptor(&descriptor, InvocationCarrierField::SinkOffers)?;
        require_non_empty_texts(&media_types, InvocationCarrierField::SinkMediaTypes)?;
        Ok(Self {
            descriptor,
            media_types,
            streaming,
            preference_rank,
            limits,
        })
    }

    /// Returns the complete consumed descriptor.
    pub const fn descriptor(&self) -> &TypeDescriptor {
        &self.descriptor
    }
    /// Returns media types in the supplied order.
    pub fn media_types(&self) -> &[String] {
        &self.media_types
    }
    /// Returns whether the sink supports streaming.
    pub const fn streaming(&self) -> bool {
        self.streaming
    }
    /// Returns the signed preference rank.
    pub const fn preference_rank(&self) -> i32 {
        self.preference_rank
    }
    /// Returns the optional typed limits value.
    pub const fn limits(&self) -> Option<&InvokeValue> {
        self.limits.as_ref()
    }
}

/// One checked runtime contract offered by the client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationRuntimeContract {
    name: String,
    version: String,
    features: Vec<String>,
}

impl InvocationRuntimeContract {
    /// Creates one checked runtime contract.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        features: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let name = name.into();
        let version = version.into();
        let features = features.into_iter().map(Into::into).collect::<Vec<_>>();
        require_non_empty(&name, InvocationCarrierField::RuntimeOffers)?;
        require_non_empty(&version, InvocationCarrierField::RuntimeOffers)?;
        require_non_empty_texts(&features, InvocationCarrierField::RuntimeContractFeatures)?;
        Ok(Self {
            name,
            version,
            features,
        })
    }

    /// Returns the runtime contract name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the runtime contract version.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Returns contract features in the supplied order.
    pub fn features(&self) -> &[String] {
        &self.features
    }
}

/// One checked client runtime offer.
#[derive(Clone, Debug, PartialEq)]
pub struct InvocationRuntimeOffer {
    name: String,
    version: String,
    consumed_descriptors: Vec<TypeDescriptor>,
    contracts: Vec<InvocationRuntimeContract>,
    preference_rank: i32,
    trusted: bool,
    limits: Option<InvokeValue>,
}

impl InvocationRuntimeOffer {
    /// Creates one checked runtime offer.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        consumed_descriptors: impl IntoIterator<Item = TypeDescriptor>,
        contracts: impl IntoIterator<Item = InvocationRuntimeContract>,
        preference_rank: i32,
        trusted: bool,
        limits: Option<InvokeValue>,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let name = name.into();
        let version = version.into();
        let consumed_descriptors = consumed_descriptors.into_iter().collect::<Vec<_>>();
        let contracts = contracts.into_iter().collect::<Vec<_>>();
        require_non_empty(&name, InvocationCarrierField::RuntimeOffers)?;
        require_non_empty(&version, InvocationCarrierField::RuntimeOffers)?;
        for descriptor in &consumed_descriptors {
            require_supported_descriptor(descriptor, InvocationCarrierField::RuntimeOffers)?;
        }
        Ok(Self {
            name,
            version,
            consumed_descriptors,
            contracts,
            preference_rank,
            trusted,
            limits,
        })
    }

    /// Returns the runtime name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the runtime version.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Returns consumed descriptors in the supplied order.
    pub fn consumed_descriptors(&self) -> &[TypeDescriptor] {
        &self.consumed_descriptors
    }
    /// Returns runtime contracts in the supplied order.
    pub fn contracts(&self) -> &[InvocationRuntimeContract] {
        &self.contracts
    }
    /// Returns the signed preference rank.
    pub const fn preference_rank(&self) -> i32 {
        self.preference_rank
    }
    /// Returns the local installation trust fact.
    pub const fn trusted(&self) -> bool {
        self.trusted
    }
    /// Returns the optional typed limits value.
    pub const fn limits(&self) -> Option<&InvokeValue> {
        self.limits.as_ref()
    }
}

/// One checked version-1 client offer.
#[derive(Clone, Debug, PartialEq)]
pub struct InvocationClientOffer {
    protocol_major: u16,
    locale: String,
    timezone: String,
    sink_offers: Vec<InvocationSinkOffer>,
    runtime_offers: Vec<InvocationRuntimeOffer>,
    maximum_frame_size: u32,
    maximum_artifact_size: u64,
    limits: Option<InvokeValue>,
    preferences: Option<InvokeValue>,
}

impl InvocationClientOffer {
    /// Creates one checked version-1 client offer.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        protocol_major: u16,
        locale: impl Into<String>,
        timezone: impl Into<String>,
        sink_offers: impl IntoIterator<Item = InvocationSinkOffer>,
        runtime_offers: impl IntoIterator<Item = InvocationRuntimeOffer>,
        maximum_frame_size: u32,
        maximum_artifact_size: u64,
        limits: Option<InvokeValue>,
        preferences: Option<InvokeValue>,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let locale = locale.into();
        let timezone = timezone.into();
        let sink_offers = sink_offers.into_iter().collect::<Vec<_>>();
        let runtime_offers = runtime_offers.into_iter().collect::<Vec<_>>();
        if protocol_major != 5 || maximum_frame_size < 1_024 {
            return Err(invalid(InvocationCarrierField::ClientOffer));
        }
        require_non_empty(&locale, InvocationCarrierField::ClientOffer)?;
        require_non_empty(&timezone, InvocationCarrierField::ClientOffer)?;
        Ok(Self {
            protocol_major,
            locale,
            timezone,
            sink_offers,
            runtime_offers,
            maximum_frame_size,
            maximum_artifact_size,
            limits,
            preferences,
        })
    }

    /// Returns the already negotiated protocol major.
    pub const fn protocol_major(&self) -> u16 {
        self.protocol_major
    }
    /// Returns the client locale.
    pub fn locale(&self) -> &str {
        &self.locale
    }
    /// Returns the client timezone.
    pub fn timezone(&self) -> &str {
        &self.timezone
    }
    /// Returns sink offers in the supplied order.
    pub fn sink_offers(&self) -> &[InvocationSinkOffer] {
        &self.sink_offers
    }
    /// Returns runtime offers in the supplied order.
    pub fn runtime_offers(&self) -> &[InvocationRuntimeOffer] {
        &self.runtime_offers
    }
    /// Returns the maximum accepted frame size.
    pub const fn maximum_frame_size(&self) -> u32 {
        self.maximum_frame_size
    }
    /// Returns the maximum accepted artifact size.
    pub const fn maximum_artifact_size(&self) -> u64 {
        self.maximum_artifact_size
    }
    /// Returns the optional typed client-limits value.
    pub const fn limits(&self) -> Option<&InvokeValue> {
        self.limits.as_ref()
    }
    /// Returns the optional typed client-preferences value.
    pub const fn preferences(&self) -> Option<&InvokeValue> {
        self.preferences.as_ref()
    }
}

/// One output type selector.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationOutputTypeSelector {
    /// One exact type identity.
    TypeId(TypeId),
    /// One resolved qualified type name.
    QualifiedName(QualifiedSemanticName),
}

impl InvocationOutputTypeSelector {
    /// Creates one exact type-identity selector.
    pub const fn type_id(type_id: TypeId) -> Self {
        Self::TypeId(type_id)
    }
    /// Creates one checked resolved qualified type-name selector.
    pub fn qualified_name(
        name: QualifiedSemanticName,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        require_qualified(&name, InvocationCarrierField::OutputTypeSelector)?;
        Ok(Self::QualifiedName(name))
    }
}

/// The streaming requirement for one output request.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationStreamingRequirement {
    /// Streaming has no requirement.
    Unspecified,
    /// Streaming is required.
    Required,
    /// Streaming is preferred.
    Preferred,
    /// Streaming is forbidden.
    Forbidden,
}

/// One checked optional output requirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationOutputRequirement {
    alias: Option<String>,
    media_type: Option<String>,
    type_selector: Option<InvocationOutputTypeSelector>,
    streaming: InvocationStreamingRequirement,
}

impl InvocationOutputRequirement {
    /// Creates one checked output requirement.
    pub fn new(
        alias: Option<String>,
        media_type: Option<String>,
        type_selector: Option<InvocationOutputTypeSelector>,
        streaming: InvocationStreamingRequirement,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        if alias.as_deref() == Some("")
            || media_type.as_deref() == Some("")
            || (alias.is_none() && media_type.is_none() && type_selector.is_none())
        {
            return Err(invalid(InvocationCarrierField::OutputRequirement));
        }
        Ok(Self {
            alias,
            media_type,
            type_selector,
            streaming,
        })
    }

    /// Returns the optional output alias.
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
    /// Returns the optional output media type.
    pub fn media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }
    /// Returns the optional output type selector.
    pub const fn type_selector(&self) -> Option<&InvocationOutputTypeSelector> {
        self.type_selector.as_ref()
    }
    /// Returns the streaming requirement.
    pub const fn streaming(&self) -> InvocationStreamingRequirement {
        self.streaming
    }
}

/// The trace policy recorded in a request.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationTracePolicy {
    Off,
    Basic,
    Normal,
    Verbose,
    Profile,
}

/// Complete logical input for one checked invocation request.
#[derive(Clone, Debug, PartialEq)]
pub struct InvokeRequestInput {
    /// The target selector.
    pub target: InvocationTarget,
    /// Arguments in canonical selector order.
    pub arguments: Vec<InvocationArgument>,
    /// The complete caller context.
    pub caller_context: InvocationCallerContext,
    /// The complete client offer.
    pub client_offer: InvocationClientOffer,
    /// The optional output requirement.
    pub output_requirement: Option<InvocationOutputRequirement>,
    /// The optional non-empty state profile.
    pub state_profile: Option<String>,
    /// The trace policy.
    pub trace_policy: InvocationTracePolicy,
    /// The optional non-empty opaque idempotency key.
    pub idempotency_key: Option<Vec<u8>>,
    /// The optional parent invocation identity.
    pub parent_invocation_id: Option<InvocationId>,
    /// The optional typed observer context.
    pub observer_context: Option<InvokeValue>,
}

/// One checked root invocation request.
#[derive(Clone, PartialEq)]
pub struct InvokeRequest {
    input: Box<InvokeRequestInput>,
    node_count: usize,
}

impl InvokeRequest {
    /// Checks and retains one complete root invocation request.
    pub fn new(input: InvokeRequestInput) -> Result<Self, InvocationCarrierConstructionError> {
        validate_request_input(&input)?;
        require_argument_order(&input.arguments)?;
        if input.state_profile.as_deref() == Some("") {
            return Err(invalid(InvocationCarrierField::StateProfile));
        }
        if input.idempotency_key.as_deref() == Some(&[]) {
            return Err(invalid(InvocationCarrierField::IdempotencyKey));
        }
        let node_count = request_node_count(&input)?;
        Ok(Self {
            input: Box::new(input),
            node_count,
        })
    }

    /// Returns the checked target selector.
    pub const fn target(&self) -> &InvocationTarget {
        &self.input.target
    }
    /// Returns arguments in canonical selector order.
    pub fn arguments(&self) -> &[InvocationArgument] {
        &self.input.arguments
    }
    /// Returns the complete caller context.
    pub const fn caller_context(&self) -> &InvocationCallerContext {
        &self.input.caller_context
    }
    /// Returns the complete client offer.
    pub const fn client_offer(&self) -> &InvocationClientOffer {
        &self.input.client_offer
    }
    /// Returns the optional output requirement.
    pub const fn output_requirement(&self) -> Option<&InvocationOutputRequirement> {
        self.input.output_requirement.as_ref()
    }
    /// Returns the optional state profile.
    pub fn state_profile(&self) -> Option<&str> {
        self.input.state_profile.as_deref()
    }
    /// Returns the trace policy.
    pub const fn trace_policy(&self) -> InvocationTracePolicy {
        self.input.trace_policy
    }
    /// Returns the optional opaque idempotency key.
    pub fn idempotency_key(&self) -> Option<&[u8]> {
        self.input.idempotency_key.as_deref()
    }
    /// Returns the optional parent invocation identity.
    pub const fn parent_invocation_id(&self) -> Option<InvocationId> {
        self.input.parent_invocation_id
    }
    /// Returns the optional typed observer context.
    pub const fn observer_context(&self) -> Option<&InvokeValue> {
        self.input.observer_context.as_ref()
    }
    /// Returns the aggregate node count, including this wrapper.
    pub const fn node_count(&self) -> usize {
        self.node_count
    }
    /// Transfers the complete checked request input.
    pub fn into_input(self) -> InvokeRequestInput {
        *self.input
    }
}

impl fmt::Debug for InvokeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InvokeRequest")
            .field("arguments", &self.input.arguments.len())
            .field("sink_offers", &self.input.client_offer.sink_offers.len())
            .field(
                "runtime_offers",
                &self.input.client_offer.runtime_offers.len(),
            )
            .field("node_count", &self.node_count)
            .finish()
    }
}

/// The kind-specific body of one invocation event.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum InvocationEventBody {
    /// The invocation became visible to the stream.
    Started {
        visible_principal: Option<PrincipalId>,
    },
    /// One non-empty batch of result values.
    ValueBatch {
        schema: Option<InvokeValue>,
        values: Vec<InvokeValue>,
    },
    /// One redacted diagnostic.
    Diagnostic(InvocationDiagnostic),
    /// The invocation completed after the stated duration.
    Completed { duration_nanoseconds: u64 },
    /// The invocation failed with redacted facts.
    Failed(InvocationFailure),
    /// The invocation was cancelled.
    Cancelled { reason: Option<String> },
}

impl InvocationEventBody {
    /// Creates one checked non-empty result-value batch.
    pub fn value_batch(
        schema: Option<InvokeValue>,
        values: impl IntoIterator<Item = InvokeValue>,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let values = values.into_iter().collect::<Vec<_>>();
        if values.is_empty() {
            return Err(invalid(InvocationCarrierField::ValueBatch));
        }
        Ok(Self::ValueBatch { schema, values })
    }

    /// Creates one checked cancellation event body.
    pub fn cancelled(reason: Option<String>) -> Result<Self, InvocationCarrierConstructionError> {
        if reason.as_deref() == Some("") {
            return Err(invalid(InvocationCarrierField::CancellationReason));
        }
        Ok(Self::Cancelled { reason })
    }

    /// Returns the body kind.
    pub const fn kind(&self) -> InvocationEventKind {
        match self {
            Self::Started { .. } => InvocationEventKind::InvocationStarted,
            Self::ValueBatch { .. } => InvocationEventKind::ValueBatch,
            Self::Diagnostic(_) => InvocationEventKind::Diagnostic,
            Self::Completed { .. } => InvocationEventKind::InvocationCompleted,
            Self::Failed(_) => InvocationEventKind::InvocationFailed,
            Self::Cancelled { .. } => InvocationEventKind::InvocationCancelled,
        }
    }
}

/// One admitted version-1 event kind.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationEventKind {
    /// The invocation started.
    InvocationStarted,
    /// A batch of typed values.
    ValueBatch,
    /// One diagnostic.
    Diagnostic,
    /// The invocation completed.
    InvocationCompleted,
    /// The invocation failed.
    InvocationFailed,
    /// The invocation was cancelled.
    InvocationCancelled,
}

/// One diagnostic severity.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

/// One checked diagnostic event body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationDiagnostic {
    severity: InvocationDiagnosticSeverity,
    code: String,
    message: String,
}

impl InvocationDiagnostic {
    /// Creates one checked diagnostic event body.
    pub fn new(
        severity: InvocationDiagnosticSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let code = code.into();
        require_printable_ascii(&code, InvocationCarrierField::DiagnosticCode)?;
        Ok(Self {
            severity,
            code,
            message: message.into(),
        })
    }
    /// Returns the diagnostic severity.
    pub const fn severity(&self) -> InvocationDiagnosticSeverity {
        self.severity
    }
    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &str {
        &self.code
    }
    /// Returns the UTF-8 diagnostic message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// The protected phase that produced an invocation failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationFailurePhase {
    Resolve,
    Bind,
    Authorise,
    Target,
    Present,
    Runtime,
    Transport,
    Internal,
}

/// The retryability recorded for one failure.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationRetryability {
    Unknown,
    No,
    Yes,
}

/// One checked redacted invocation failure event body.
#[derive(Clone, Debug, PartialEq)]
pub struct InvocationFailure {
    phase: InvocationFailurePhase,
    code: String,
    message: String,
    details: Option<InvokeValue>,
    retryability: InvocationRetryability,
}

impl InvocationFailure {
    /// Creates one checked redacted failure event body.
    pub fn new(
        phase: InvocationFailurePhase,
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<InvokeValue>,
        retryability: InvocationRetryability,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let code = code.into();
        require_printable_ascii(&code, InvocationCarrierField::FailureCode)?;
        Ok(Self {
            phase,
            code,
            message: message.into(),
            details,
            retryability,
        })
    }
    /// Returns the protected failure phase.
    pub const fn phase(&self) -> InvocationFailurePhase {
        self.phase
    }
    /// Returns the stable failure code.
    pub fn code(&self) -> &str {
        &self.code
    }
    /// Returns the redacted failure message.
    pub fn message(&self) -> &str {
        &self.message
    }
    /// Returns optional typed redacted details.
    pub const fn details(&self) -> Option<&InvokeValue> {
        self.details.as_ref()
    }
    /// Returns the retryability fact.
    pub const fn retryability(&self) -> InvocationRetryability {
        self.retryability
    }
}

/// One checked invocation event.
#[derive(Clone, PartialEq)]
pub struct InvokeEvent {
    invocation_id: InvocationId,
    sequence: u64,
    body: InvocationEventBody,
    node_count: usize,
}

impl InvokeEvent {
    /// Checks and retains one complete event without stream lifecycle state.
    pub fn new(
        invocation_id: InvocationId,
        sequence: u64,
        body: InvocationEventBody,
    ) -> Result<Self, InvocationCarrierConstructionError> {
        let node_count = event_node_count(&body)?;
        Ok(Self {
            invocation_id,
            sequence,
            body,
            node_count,
        })
    }
    /// Returns the invocation identity.
    pub const fn invocation_id(&self) -> InvocationId {
        self.invocation_id
    }
    /// Returns the isolated event sequence exactly.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    /// Returns the checked kind-specific body.
    pub const fn body(&self) -> &InvocationEventBody {
        &self.body
    }
    /// Returns the admitted event kind.
    pub const fn kind(&self) -> InvocationEventKind {
        self.body.kind()
    }
    /// Returns the aggregate node count, including this wrapper.
    pub const fn node_count(&self) -> usize {
        self.node_count
    }
}

impl fmt::Debug for InvokeEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InvokeEvent")
            .field("kind", &self.body.kind())
            .field("node_count", &self.node_count)
            .finish()
    }
}

/// Returns the sealed carrier classification for one runtime value.
pub const fn invocation_carrier_kind(value: &RuntimeValue) -> Option<InvocationCarrierKind> {
    match value {
        RuntimeValue::InvokeValue(_) => Some(InvocationCarrierKind::Value),
        RuntimeValue::InvokeRequest(_) => Some(InvocationCarrierKind::Request),
        RuntimeValue::InvokeEvent(_) => Some(InvocationCarrierKind::Event),
        _ => None,
    }
}

/// Returns the exact sealed carrier identity for one runtime value.
pub const fn invocation_carrier_type_id(value: &RuntimeValue) -> Option<TypeId> {
    match value {
        RuntimeValue::InvokeValue(_) => Some(SYS_INVOKE_VALUE_TYPE_ID),
        RuntimeValue::InvokeRequest(_) => Some(SYS_INVOKE_REQUEST_TYPE_ID),
        RuntimeValue::InvokeEvent(_) => Some(SYS_INVOKE_EVENT_TYPE_ID),
        _ => None,
    }
}

fn invalid(field: InvocationCarrierField) -> InvocationCarrierConstructionError {
    InvocationCarrierConstructionError::InvalidField { field }
}

fn add_nodes(
    total: &mut usize,
    additional: usize,
) -> Result<(), InvocationCarrierConstructionError> {
    let next =
        total
            .checked_add(additional)
            .ok_or(InvocationCarrierConstructionError::TooManyNodes {
                maximum: MAX_INVOCATION_CARRIER_NODES,
            })?;
    if next > MAX_INVOCATION_CARRIER_NODES {
        return Err(InvocationCarrierConstructionError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        });
    }
    *total = next;
    Ok(())
}

fn request_node_count(
    input: &InvokeRequestInput,
) -> Result<usize, InvocationCarrierConstructionError> {
    let mut total = 1;
    for argument in &input.arguments {
        add_nodes(&mut total, argument.value.node_count)?;
    }
    add_optional_nodes(&mut total, input.caller_context.preference_policy.as_ref())?;
    for offer in &input.client_offer.sink_offers {
        add_optional_nodes(&mut total, offer.limits.as_ref())?;
    }
    for offer in &input.client_offer.runtime_offers {
        add_optional_nodes(&mut total, offer.limits.as_ref())?;
    }
    add_optional_nodes(&mut total, input.client_offer.limits.as_ref())?;
    add_optional_nodes(&mut total, input.client_offer.preferences.as_ref())?;
    add_optional_nodes(&mut total, input.observer_context.as_ref())?;
    Ok(total)
}

fn event_node_count(
    body: &InvocationEventBody,
) -> Result<usize, InvocationCarrierConstructionError> {
    let mut total = 1;
    match body {
        InvocationEventBody::Started { .. }
        | InvocationEventBody::Diagnostic(_)
        | InvocationEventBody::Completed { .. } => {}
        InvocationEventBody::Cancelled { reason } => {
            if reason.as_deref() == Some("") {
                return Err(invalid(InvocationCarrierField::CancellationReason));
            }
        }
        InvocationEventBody::ValueBatch { schema, values } => {
            if values.is_empty() {
                return Err(invalid(InvocationCarrierField::ValueBatch));
            }
            add_optional_nodes(&mut total, schema.as_ref())?;
            for value in values {
                add_nodes(&mut total, value.node_count)?;
            }
        }
        InvocationEventBody::Failed(failure) => {
            add_optional_nodes(&mut total, failure.details.as_ref())?
        }
    }
    Ok(total)
}

fn add_optional_nodes(
    total: &mut usize,
    value: Option<&InvokeValue>,
) -> Result<(), InvocationCarrierConstructionError> {
    if let Some(value) = value {
        add_nodes(total, value.node_count)?;
    }
    Ok(())
}

fn validate_request_input(
    input: &InvokeRequestInput,
) -> Result<(), InvocationCarrierConstructionError> {
    if let InvocationTarget::QualifiedName(name) = &input.target {
        require_qualified(name, InvocationCarrierField::Target)?;
    }
    for argument in &input.arguments {
        if let InvocationParameterSelector::Name(name) = &argument.selector {
            require_non_empty(name, InvocationCarrierField::ParameterSelector)?;
        }
    }
    if let Some(InvocationOutputRequirement { type_selector, .. }) = &input.output_requirement
        && let Some(InvocationOutputTypeSelector::QualifiedName(name)) = type_selector
    {
        require_qualified(name, InvocationCarrierField::OutputTypeSelector)?;
    }
    Ok(())
}

fn require_non_empty(
    value: &str,
    field: InvocationCarrierField,
) -> Result<(), InvocationCarrierConstructionError> {
    if value.is_empty() {
        Err(invalid(field))
    } else {
        Ok(())
    }
}

fn require_printable_ascii(
    value: &str,
    field: InvocationCarrierField,
) -> Result<(), InvocationCarrierConstructionError> {
    if value.is_empty() || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
        Err(invalid(field))
    } else {
        Ok(())
    }
}

fn require_qualified(
    name: &QualifiedSemanticName,
    field: InvocationCarrierField,
) -> Result<(), InvocationCarrierConstructionError> {
    if name.parts().len() < 2 {
        Err(invalid(field))
    } else {
        Ok(())
    }
}

fn require_non_empty_texts(
    values: &[String],
    field: InvocationCarrierField,
) -> Result<(), InvocationCarrierConstructionError> {
    if values.iter().any(String::is_empty) {
        Err(invalid(field))
    } else {
        Ok(())
    }
}

fn require_supported_descriptor(
    descriptor: &TypeDescriptor,
    field: InvocationCarrierField,
) -> Result<(), InvocationCarrierConstructionError> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) | TypeDescriptorKind::Reference(type_id) => {
            if invocation_carrier_by_id(type_id).is_some() {
                Err(InvocationCarrierConstructionError::NestedCarrier { carrier: type_id })
            } else {
                Ok(())
            }
        }
        TypeDescriptorKind::List(child) | TypeDescriptorKind::Option(child) => {
            require_supported_descriptor(child, field)
        }
        TypeDescriptorKind::Map { key, value } => {
            require_supported_descriptor(key, field)?;
            require_supported_descriptor(value, field)
        }
        TypeDescriptorKind::Set(_) | TypeDescriptorKind::Stream(_) => Err(invalid(field)),
    }
}

fn require_argument_order(
    values: &[InvocationArgument],
) -> Result<(), InvocationCarrierConstructionError> {
    for pair in values.windows(2) {
        match selector_order(&pair[0].selector, &pair[1].selector) {
            Ordering::Greater => {
                return Err(InvocationCarrierConstructionError::NonCanonicalOrder {
                    field: InvocationCarrierField::Arguments,
                });
            }
            Ordering::Equal => {
                return Err(InvocationCarrierConstructionError::DuplicateItem {
                    field: InvocationCarrierField::Arguments,
                });
            }
            Ordering::Less => {}
        }
    }
    Ok(())
}

fn selector_order(
    left: &InvocationParameterSelector,
    right: &InvocationParameterSelector,
) -> Ordering {
    match (left, right) {
        (
            InvocationParameterSelector::ParameterId(left),
            InvocationParameterSelector::ParameterId(right),
        ) => left.to_bytes().cmp(&right.to_bytes()),
        (InvocationParameterSelector::ParameterId(_), InvocationParameterSelector::Name(_)) => {
            Ordering::Less
        }
        (InvocationParameterSelector::Name(_), InvocationParameterSelector::ParameterId(_)) => {
            Ordering::Greater
        }
        (InvocationParameterSelector::Name(left), InvocationParameterSelector::Name(right)) => {
            left.as_bytes().cmp(right.as_bytes())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ObjectTypeDefinition, ParameterDefinition,
        SchemaDefinition, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    };
    use crate::revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, ExecutableArtifact,
        ExecutableArtifactKind, FunctionRevisionRecord, FunctionSemanticHashVersion, RevisionPair,
        Sha256Digest, SourceOrigin, StandardExecutable, StandardLibraryDigestVersion,
        StandardLibrarySnapshot, StoredSourceRevision, StoredSourceUnit,
        VerifiedStandardLibrarySnapshot,
    };
    use crate::value::{
        FunctionArgument, FunctionArgumentError, ResultColumn, ResultRow, ResultRows, RuntimeType,
    };
    use crate::{
        CatalogueRevisionId, FunctionId, FunctionRevisionId, ObjectId, SchemaId, SourceBundleId,
        SourceRevisionId, SourceUnitId, TypeId,
    };
    use crate::{
        security::{
            ExecuteGrant, InvocationTarget as SecurityInvocationTarget, Principal, PrincipalKind,
            PrincipalStatus, SecurityFunctionTarget, SecuritySnapshot,
        },
        system::{
            CATALOGUE_HEALTH_FUNCTION_ID, SYS_INVOKE_FUNCTION_ID,
            SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        },
        types::{ResolvedType, StandardScalar},
    };

    fn value(value: i32) -> InvokeValue {
        InvokeValue::new(RuntimeValue::Integer(value)).expect("a scalar value must be admitted")
    }

    fn client_offer() -> InvocationClientOffer {
        InvocationClientOffer::new(5, "en-GB", "Europe/London", [], [], 1_024, 0, None, None)
            .expect("the minimal version-1 client offer must be admitted")
    }

    fn request(arguments: Vec<InvocationArgument>) -> InvokeRequest {
        InvokeRequest::new(request_input(arguments, None))
            .expect("the complete request must be admitted")
    }

    fn request_input(
        arguments: Vec<InvocationArgument>,
        preference_policy: Option<InvokeValue>,
    ) -> InvokeRequestInput {
        InvokeRequestInput {
            target: InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["app", "work"]).expect("a qualified name"),
            )
            .expect("a qualified target"),
            arguments,
            caller_context: InvocationCallerContext::new(
                InvocationCallerKind::CliPipe,
                false,
                false,
                None,
                None,
                "en-GB",
                "Europe/London",
                preference_policy,
            )
            .expect("a valid pipe caller context"),
            client_offer: client_offer(),
            output_requirement: None,
            state_profile: None,
            trace_policy: InvocationTracePolicy::Off,
            idempotency_key: None,
            parent_invocation_id: None,
            observer_context: None,
        }
    }

    fn active_reference_revision() -> ActiveDatabaseRevision {
        let source_unit = SourceUnitId::from_bytes([9; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([1; 16]),
            SourceRevisionId::from_bytes([2; 16]),
            None,
            vec![
                StoredSourceUnit::new(
                    source_unit,
                    0,
                    "app.orna",
                    "",
                    Sha256Digest::from_bytes([5; 32]),
                )
                .expect("a source unit"),
            ],
            Sha256Digest::from_bytes([3; 32]),
            Sha256Digest::from_bytes([4; 32]),
        )
        .expect("an empty source revision");
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([5; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([6; 16]),
                QualifiedSemanticName::new(["app"]).expect("a schema name"),
            )],
            vec![ObjectTypeDefinition::new(
                TypeId::from_bytes([7; 16]),
                QualifiedSemanticName::new(["app", "item"]).expect("an object name"),
                Vec::new(),
            )],
        )
        .expect("a reference target catalogue");
        ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            Sha256Digest::from_bytes([8; 32]),
            Vec::new(),
            Vec::new(),
            vec![
                DefinitionOrigin::new(
                    DefinitionIdentity::Schema(SchemaId::from_bytes([6; 16])),
                    SourceOrigin::new(source_unit, 0, 0).expect("a source range"),
                ),
                DefinitionOrigin::new(
                    DefinitionIdentity::ObjectType(TypeId::from_bytes([7; 16])),
                    SourceOrigin::new(source_unit, 0, 0).expect("a source range"),
                ),
            ],
            Vec::new(),
        )
        .expect("an active reference revision")
    }

    fn decision_function(security: FunctionSecurity) -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([10; 16]),
            QualifiedSemanticName::new(["app", "work"]).expect("a function name"),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                ParameterId::from_bytes([11; 16]),
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            )],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            FunctionRevisionId::from_bytes([12; 16]),
            security,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Immutable,
        )
    }

    fn decision_active_revision(security: FunctionSecurity) -> ActiveDatabaseRevision {
        let source_unit = SourceUnitId::from_bytes([13; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([14; 16]),
            SourceRevisionId::from_bytes([15; 16]),
            None,
            vec![
                StoredSourceUnit::new(
                    source_unit,
                    0,
                    "app.orna",
                    "",
                    Sha256Digest::from_bytes([16; 32]),
                )
                .expect("a source unit"),
            ],
            Sha256Digest::from_bytes([17; 32]),
            Sha256Digest::from_bytes([18; 32]),
        )
        .expect("an application source revision");
        let function = decision_function(security);
        let catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([19; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([20; 16]),
                QualifiedSemanticName::new(["app"]).expect("a schema name"),
            )],
            vec![],
            vec![function.clone()],
        )
        .expect("an application function catalogue");
        let origin = SourceOrigin::new(source_unit, 0, 0).expect("a source origin");
        let revision = FunctionRevisionRecord::new(
            function.id(),
            function.current_revision(),
            1,
            origin,
            Sha256Digest::from_bytes([21; 32]),
            Sha256Digest::from_bytes([22; 32]),
            "orna.language/1",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Server,
                "orna.server-plan",
                1,
                vec![1],
                Sha256Digest::from_bytes([23; 32]),
            )
            .expect("an executable artifact"),
        )
        .expect("a function revision");
        ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            Sha256Digest::from_bytes([24; 32]),
            vec![],
            vec![revision],
            vec![
                DefinitionOrigin::new(
                    DefinitionIdentity::Schema(SchemaId::from_bytes([20; 16])),
                    origin,
                ),
                DefinitionOrigin::new(DefinitionIdentity::Function(function.id()), origin),
                DefinitionOrigin::new(
                    DefinitionIdentity::Parameter {
                        owner: function.id(),
                        parameter: ParameterId::from_bytes([11; 16]),
                    },
                    origin,
                ),
            ],
            vec![],
        )
        .expect("an active function revision")
    }

    fn decision_security(
        active: &ActiveDatabaseRevision,
        grants: Vec<ExecuteGrant>,
    ) -> SecuritySnapshot {
        SecuritySnapshot::new(
            active.pair(),
            vec![FunctionId::from_bytes([10; 16])],
            vec![Principal::new(
                PrincipalId::from_bytes([25; 16]),
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            grants,
        )
        .expect("a security snapshot")
    }

    fn decision_request(arguments: Vec<InvocationArgument>) -> InvokeRequest {
        request(arguments)
    }

    fn standard_function_with(security: FunctionSecurity) -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([0x30; 16]),
            QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("a standard name"),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            FunctionRevisionId::from_bytes([0x31; 16]),
            security,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    }

    fn standard_function() -> FunctionDefinition {
        standard_function_with(FunctionSecurity::Invoker)
    }

    fn standard_executable_snapshot_with(
        function: &FunctionDefinition,
    ) -> VerifiedStandardLibrarySnapshot {
        let invoke_unit = SourceUnitId::from_bytes([0x42; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x44; 16]),
            SourceRevisionId::from_bytes([0x45; 16]),
            Some(SourceRevisionId::from_bytes([0x46; 16])),
            vec![
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes([0x40; 16]),
                    0,
                    "std/types.orna",
                    "CREATE SCHEMA std;\n",
                    Sha256Digest::from_bytes([0x41; 32]),
                )
                .expect("a types source unit"),
                StoredSourceUnit::new(
                    invoke_unit,
                    1,
                    "std/invoke.orna",
                    "CREATE SERVER FUNCTION std.invoke.echo() RETURNS INTEGER SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT 1;\n",
                    Sha256Digest::from_bytes([0x43; 32]),
                )
                .expect("an invoke source unit"),
            ],
            Sha256Digest::from_bytes([0x47; 32]),
            Sha256Digest::from_bytes([0x48; 32]),
        )
        .expect("a standard source revision");
        let integer_type = ValueTypeDefinition::primitive(
            TypeId::from_bytes([0x7c; 16]),
            QualifiedSemanticName::new(["std", "invoke", "integer"]).expect("a value type name"),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.integer@1",
        );
        let catalogue = CatalogueSnapshot::new_with_functions_and_types(
            CatalogueRevisionId::from_bytes([0x49; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x4a; 16]),
                QualifiedSemanticName::new(["std", "invoke"]).expect("a schema name"),
            )],
            vec![],
            vec![integer_type.clone()],
            vec![],
            vec![function.clone()],
        )
        .expect("a standard catalogue");
        let declaration = SourceOrigin::new(invoke_unit, 0, 0).expect("a source origin");
        let revision = FunctionRevisionRecord::new(
            function.id(),
            function.current_revision(),
            1,
            declaration,
            Sha256Digest::from_bytes([0x4c; 32]),
            Sha256Digest::from_bytes([0x4d; 32]),
            "orna.language/1",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Server,
                "orna.server-parameter-echo",
                1,
                vec![1],
                Sha256Digest::from_bytes([0x4b; 32]),
            )
            .expect("an executable artifact"),
        )
        .expect("a standard function revision")
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let standard = StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes([0x4e; 16]),
            StandardLibraryDigestVersion::Version2,
            source,
            "orna.language/1",
            catalogue,
            vec![
                StandardExecutable::new(function.id(), revision, vec![])
                    .expect("a standard executable"),
            ],
            vec![
                DefinitionOrigin::new(
                    DefinitionIdentity::Schema(SchemaId::from_bytes([0x4a; 16])),
                    SourceOrigin::new(SourceUnitId::from_bytes([0x40; 16]), 0, 0)
                        .expect("a schema origin"),
                ),
                DefinitionOrigin::new(
                    DefinitionIdentity::ValueType(integer_type.id()),
                    SourceOrigin::new(SourceUnitId::from_bytes([0x40; 16]), 0, 0)
                        .expect("a value type origin"),
                ),
                DefinitionOrigin::new(DefinitionIdentity::Function(function.id()), declaration),
            ],
            Sha256Digest::from_bytes([0x4f; 32]),
        )
        .expect("a version-2 standard snapshot");
        VerifiedStandardLibrarySnapshot::new(standard)
    }

    fn standard_executable_snapshot() -> VerifiedStandardLibrarySnapshot {
        standard_executable_snapshot_with(&standard_function())
    }

    fn standard_decision_active_revision_with(
        standard: VerifiedStandardLibrarySnapshot,
    ) -> ActiveDatabaseRevision {
        let source_unit = SourceUnitId::from_bytes([0x50; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x51; 16]),
            SourceRevisionId::from_bytes([0x52; 16]),
            None,
            vec![
                StoredSourceUnit::new(
                    source_unit,
                    0,
                    "app.orna",
                    "",
                    Sha256Digest::from_bytes([0x53; 32]),
                )
                .expect("a source unit"),
            ],
            Sha256Digest::from_bytes([0x54; 32]),
            Sha256Digest::from_bytes([0x55; 32]),
        )
        .expect("an application source revision");
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x56; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x57; 16]),
                QualifiedSemanticName::new(["app"]).expect("a schema name"),
            )],
            vec![],
        )
        .expect("an application catalogue");
        let origin = SourceOrigin::new(source_unit, 0, 0).expect("a source origin");
        let input = ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            Sha256Digest::from_bytes([0x58; 32]),
            ActiveRevisionContent::new(
                Vec::new(),
                Vec::new(),
                vec![DefinitionOrigin::new(
                    DefinitionIdentity::Schema(SchemaId::from_bytes([0x57; 16])),
                    origin,
                )],
                Vec::new(),
            ),
        );
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            input,
            CatalogueHashContext::version_two(standard),
        )
        .expect("an active revision pinned to a verified standard snapshot")
    }

    fn standard_decision_active_revision() -> ActiveDatabaseRevision {
        standard_decision_active_revision_with(standard_executable_snapshot())
    }

    fn standard_security_for(
        active: &ActiveDatabaseRevision,
        grants: Vec<ExecuteGrant>,
    ) -> SecuritySnapshot {
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("a pinned standard snapshot");
        let function = standard.catalogue().functions()[0].clone();
        SecuritySnapshot::new_with_function_targets(
            active.pair(),
            vec![SecurityFunctionTarget::verified_standard(
                function.id(),
                standard.revision(),
                standard.executables()[0].revision().id(),
            )],
            vec![Principal::new(
                PrincipalId::from_bytes([25; 16]),
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            grants,
        )
        .expect("a standard-class security snapshot")
    }

    fn ambiguous_standard_active_revision() -> ActiveDatabaseRevision {
        let source_unit = SourceUnitId::from_bytes([0x70; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x71; 16]),
            SourceRevisionId::from_bytes([0x72; 16]),
            None,
            vec![
                StoredSourceUnit::new(
                    source_unit,
                    0,
                    "app.orna",
                    "",
                    Sha256Digest::from_bytes([0x73; 32]),
                )
                .expect("a source unit"),
            ],
            Sha256Digest::from_bytes([0x74; 32]),
            Sha256Digest::from_bytes([0x75; 32]),
        )
        .expect("an application source revision");
        let function = FunctionDefinition::new(
            FunctionId::from_bytes([0x30; 16]),
            QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("a standard name"),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::Value(TypeId::from_bytes([0x7c; 16]))),
            FunctionRevisionId::from_bytes([0x31; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x76; 16]),
            vec![
                SchemaDefinition::new(
                    SchemaId::from_bytes([0x57; 16]),
                    QualifiedSemanticName::new(["app"]).expect("a schema name"),
                ),
                SchemaDefinition::new(
                    SchemaId::from_bytes([0x4a; 16]),
                    QualifiedSemanticName::new(["std", "invoke"]).expect("a schema name"),
                ),
            ],
            vec![],
            vec![function.clone()],
        )
        .expect("an application catalogue that duplicates a standard function");
        let origin = SourceOrigin::new(source_unit, 0, 0).expect("a source origin");
        let revision = FunctionRevisionRecord::new(
            function.id(),
            function.current_revision(),
            1,
            origin,
            Sha256Digest::from_bytes([0x77; 32]),
            Sha256Digest::from_bytes([0x78; 32]),
            "orna.language/1",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Server,
                "orna.server-parameter-echo",
                1,
                vec![1],
                Sha256Digest::from_bytes([0x79; 32]),
            )
            .expect("an executable artifact"),
        )
        .expect("an application function revision")
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let input = ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            Sha256Digest::from_bytes([0x7a; 32]),
            ActiveRevisionContent::new(
                Vec::new(),
                vec![revision],
                vec![
                    DefinitionOrigin::new(
                        DefinitionIdentity::Schema(SchemaId::from_bytes([0x57; 16])),
                        origin,
                    ),
                    DefinitionOrigin::new(
                        DefinitionIdentity::Schema(SchemaId::from_bytes([0x4a; 16])),
                        origin,
                    ),
                    DefinitionOrigin::new(DefinitionIdentity::Function(function.id()), origin),
                ],
                Vec::new(),
            ),
        );
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            input,
            CatalogueHashContext::version_two(standard_executable_snapshot()),
        )
        .expect("an ambiguous active revision")
    }

    fn unknown_request() -> InvokeRequest {
        InvokeRequest::new(InvokeRequestInput {
            target: InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["app", "missing"]).expect("a qualified name"),
            )
            .expect("a qualified target"),
            arguments: Vec::new(),
            ..request_input(Vec::new(), None)
        })
        .expect("a checked unknown request")
    }

    fn standard_request() -> InvokeRequest {
        InvokeRequest::new(InvokeRequestInput {
            target: InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("a qualified name"),
            )
            .expect("a qualified target"),
            arguments: Vec::new(),
            ..request_input(Vec::new(), None)
        })
        .expect("a checked standard request")
    }

    fn list_value(active: &ActiveDatabaseRevision, element_count: usize) -> InvokeValue {
        let target = TypeId::from_bytes([7; 16]);
        let descriptor =
            TypeDescriptor::list(TypeDescriptor::reference(target)).expect("a list descriptor");
        let values = (0..element_count)
            .map(|index| RuntimeValue::Reference {
                target,
                object: ObjectId::from_bytes([index as u8; 16]),
            })
            .collect::<Vec<_>>();
        InvokeValue::new(
            RuntimeValue::list(active, descriptor, values).expect("a valid reference list"),
        )
        .expect("a checked constructed invocation value")
    }

    #[test]
    fn checked_carriers_expose_safe_facts_and_stay_closed_from_ordinary_positions() {
        let argument = InvocationArgument::new(
            InvocationParameterSelector::parameter_id(ParameterId::from_bytes([3; 16])),
            value(7),
        );
        let request = request(vec![argument]);
        assert_eq!(request.arguments().len(), 1);
        assert_eq!(request.client_offer().protocol_major(), 5);
        let event = InvokeEvent::new(
            InvocationId::from_bytes([5; 16]),
            0,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("a started event");
        let carriers = [
            (
                RuntimeValue::InvokeValue(value(7)),
                SYS_INVOKE_VALUE_TYPE_ID,
                InvocationCarrierKind::Value,
            ),
            (
                RuntimeValue::InvokeRequest(request.clone()),
                SYS_INVOKE_REQUEST_TYPE_ID,
                InvocationCarrierKind::Request,
            ),
            (
                RuntimeValue::InvokeEvent(event),
                SYS_INVOKE_EVENT_TYPE_ID,
                InvocationCarrierKind::Event,
            ),
        ];
        for (runtime, type_id, kind) in carriers {
            assert_eq!(
                runtime.runtime_type(),
                RuntimeType::Flat(crate::types::ResolvedType::value(type_id))
            );
            assert_eq!(
                FunctionArgument::new(ParameterId::from_bytes([4; 16]), runtime.clone()),
                Err(FunctionArgumentError::InvocationCarrierNotAccepted {
                    parameter: ParameterId::from_bytes([4; 16]),
                    carrier: kind,
                })
            );
            assert!(matches!(
                ResultRows::new(
                    [ResultColumn::new(
                        "result",
                        crate::types::ResolvedType::value(type_id),
                        false,
                    )
                    .expect("a value type remains a valid column declaration")],
                    [ResultRow::new([runtime])],
                ),
                Err(crate::value::ResultRowsError::InvocationCarrierNotAccepted { .. })
            ));
        }
        assert!(!format!("{request:?}").contains("Europe/London"));
    }

    #[test]
    fn carrier_values_reject_nested_carriers_before_they_can_be_retained() {
        let request = request(Vec::new());
        let event = InvokeEvent::new(
            InvocationId::from_bytes([8; 16]),
            1,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("a started event");
        for (nested, carrier) in [
            (
                RuntimeValue::InvokeValue(value(7)),
                SYS_INVOKE_VALUE_TYPE_ID,
            ),
            (
                RuntimeValue::InvokeRequest(request),
                SYS_INVOKE_REQUEST_TYPE_ID,
            ),
            (RuntimeValue::InvokeEvent(event), SYS_INVOKE_EVENT_TYPE_ID),
        ] {
            assert_eq!(
                InvokeValue::new(nested),
                Err(InvocationCarrierConstructionError::NestedCarrier { carrier })
            );
        }
    }

    #[test]
    fn request_node_budget_accepts_exactly_65536_and_rejects_65537_after_structure() {
        let active = active_reference_revision();
        let arguments = (0_u16..32_766)
            .map(|index| {
                let mut bytes = [0; 16];
                bytes[14..].copy_from_slice(&index.to_be_bytes());
                InvocationArgument::new(
                    InvocationParameterSelector::parameter_id(ParameterId::from_bytes(bytes)),
                    value(i32::from(index)),
                )
            })
            .collect::<Vec<_>>();
        let accepted = InvokeRequest::new(request_input(arguments, Some(list_value(&active, 1))))
            .expect("the exact aggregate node limit must be admitted");
        assert_eq!(accepted.node_count(), MAX_INVOCATION_CARRIER_NODES);

        let arguments = (0_u16..32_766)
            .map(|index| {
                let mut bytes = [0; 16];
                bytes[14..].copy_from_slice(&index.to_be_bytes());
                InvocationArgument::new(
                    InvocationParameterSelector::parameter_id(ParameterId::from_bytes(bytes)),
                    value(i32::from(index)),
                )
            })
            .collect::<Vec<_>>();
        let input = request_input(arguments, Some(list_value(&active, 2)));
        assert_eq!(
            InvokeRequest::new(input),
            Err(InvocationCarrierConstructionError::TooManyNodes {
                maximum: MAX_INVOCATION_CARRIER_NODES,
            })
        );

        let duplicate = InvocationArgument::new(
            InvocationParameterSelector::parameter_id(ParameterId::from_bytes([9; 16])),
            value(1),
        );
        let input = request_input(vec![duplicate.clone(); 32_768], None);
        assert_eq!(
            InvokeRequest::new(input),
            Err(InvocationCarrierConstructionError::DuplicateItem {
                field: InvocationCarrierField::Arguments,
            })
        );
    }

    #[test]
    fn carrier_offer_sequences_retain_structural_order_and_close_unsupported_descriptors() {
        let low = TypeDescriptor::named(TypeId::from_bytes([1; 16]));
        let high = TypeDescriptor::named(TypeId::from_bytes([2; 16]));
        let sink = |descriptor| {
            InvocationSinkOffer::new(descriptor, ["text/plain"], false, 0, None)
                .expect("a simple sink offer")
        };
        let runtime = |name| {
            InvocationRuntimeOffer::new(name, "1", [low.clone()], [], 0, false, None)
                .expect("a simple runtime offer")
        };
        let offer = InvocationClientOffer::new(
            5,
            "en-GB",
            "Europe/London",
            [sink(high.clone()), sink(low.clone()), sink(low.clone())],
            [runtime("z"), runtime("a"), runtime("a")],
            1_024,
            0,
            None,
            None,
        )
        .expect("structural offer validation does not impose wire canonicality");
        assert_eq!(offer.sink_offers().len(), 3);
        assert_eq!(offer.sink_offers()[0].descriptor(), &high);
        assert_eq!(offer.sink_offers()[1].descriptor(), &low);
        assert_eq!(offer.sink_offers()[2].descriptor(), &low);
        assert_eq!(
            offer
                .runtime_offers()
                .iter()
                .map(InvocationRuntimeOffer::name)
                .collect::<Vec<_>>(),
            ["z", "a", "a"]
        );
        for carrier in [
            SYS_INVOKE_VALUE_TYPE_ID,
            SYS_INVOKE_REQUEST_TYPE_ID,
            SYS_INVOKE_EVENT_TYPE_ID,
        ] {
            assert_eq!(
                InvocationSinkOffer::new(
                    TypeDescriptor::list(TypeDescriptor::named(carrier))
                        .expect("a list descriptor"),
                    ["text/plain"],
                    false,
                    0,
                    None,
                ),
                Err(InvocationCarrierConstructionError::NestedCarrier { carrier })
            );
            assert_eq!(
                InvocationRuntimeOffer::new(
                    "runtime",
                    "1",
                    [TypeDescriptor::map(
                        low.clone(),
                        TypeDescriptor::option(
                            TypeDescriptor::list(TypeDescriptor::reference(carrier))
                                .expect("a list descriptor"),
                        )
                        .expect("an option descriptor"),
                    )
                    .expect("a map descriptor")],
                    [],
                    0,
                    false,
                    None,
                ),
                Err(InvocationCarrierConstructionError::NestedCarrier { carrier })
            );
        }
        assert_eq!(
            InvocationSinkOffer::new(
                TypeDescriptor::set(low.clone()).expect("a set descriptor"),
                ["text/plain"],
                false,
                0,
                None,
            ),
            Err(InvocationCarrierConstructionError::InvalidField {
                field: InvocationCarrierField::SinkOffers,
            })
        );
        assert_eq!(
            InvocationRuntimeOffer::new(
                "runtime",
                "1",
                [TypeDescriptor::stream(low).expect("a stream descriptor")],
                [],
                0,
                false,
                None,
            ),
            Err(InvocationCarrierConstructionError::InvalidField {
                field: InvocationCarrierField::RuntimeOffers,
            })
        );
    }

    #[test]
    fn carrier_debug_redacts_all_sensitive_payloads() {
        let secret = "top-secret";
        let observer =
            InvokeValue::new(RuntimeValue::Text(secret.into())).expect("a typed observer context");
        let request = InvokeRequest::new(InvokeRequestInput {
            idempotency_key: Some(secret.as_bytes().to_vec()),
            observer_context: Some(observer),
            ..request_input(Vec::new(), None)
        })
        .expect("a checked request");
        let failure = InvocationFailure::new(
            InvocationFailurePhase::Internal,
            "INTERNAL",
            secret,
            Some(value(7)),
            InvocationRetryability::Unknown,
        )
        .expect("a checked failure");
        let event = InvokeEvent::new(
            InvocationId::from_bytes([9; 16]),
            0,
            InvocationEventBody::Failed(failure),
        )
        .expect("a checked event");

        assert!(
            !format!(
                "{:?}",
                InvokeValue::new(RuntimeValue::Text(secret.into())).unwrap()
            )
            .contains(secret)
        );
        assert!(!format!("{request:?}").contains(secret));
        assert!(!format!("{event:?}").contains(secret));
    }

    #[test]
    fn event_construction_is_isolated_from_stream_lifecycle_state() {
        let body = InvocationEventBody::value_batch(None, [value(1)])
            .expect("one result value must form a batch");
        let event = InvokeEvent::new(InvocationId::from_bytes([7; 16]), u64::MAX, body)
            .expect("an isolated maximum sequence must be admitted");

        assert_eq!(event.kind(), InvocationEventKind::ValueBatch);
        assert_eq!(event.sequence(), u64::MAX);
        assert_eq!(
            InvokeEvent::new(
                InvocationId::from_bytes([7; 16]),
                0,
                InvocationEventBody::ValueBatch {
                    schema: None,
                    values: Vec::new()
                },
            ),
            Err(InvocationCarrierConstructionError::InvalidField {
                field: InvocationCarrierField::ValueBatch,
            })
        );
        assert_eq!(
            InvokeEvent::new(
                InvocationId::from_bytes([7; 16]),
                0,
                InvocationEventBody::Cancelled {
                    reason: Some(String::new()),
                },
            ),
            Err(InvocationCarrierConstructionError::InvalidField {
                field: InvocationCarrierField::CancellationReason,
            })
        );
    }

    #[test]
    fn protected_decision_checks_the_exact_system_entry_before_request_context() {
        let active = decision_active_revision(FunctionSecurity::Invoker);
        let security = decision_security(&active, vec![]);
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let request = decision_request(Vec::new());

        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                SecurityInvocationTarget::new(CATALOGUE_HEALTH_FUNCTION_ID, security.revision()),
                &active,
                4,
                &request,
            ),
            ProtectedInvocationDecision::EntryDenied
        );
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                SecurityInvocationTarget::new(SYS_INVOKE_FUNCTION_ID, security.revision()),
                &active,
                4,
                &request,
            ),
            ProtectedInvocationDecision::RequestRejected
        );
    }

    #[test]
    fn protected_decision_redacts_denials_and_allows_only_redacted_bind_failure() {
        let active = decision_active_revision(FunctionSecurity::Invoker);
        let security = decision_security(&active, vec![]);
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let secret = "private-selector";
        let denied_request = decision_request(vec![InvocationArgument::new(
            InvocationParameterSelector::name(secret).expect("a selector"),
            value(7),
        )]);

        let denied = decide_protected_invocation(
            &security,
            &session,
            SecurityInvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair()),
            &active,
            5,
            &denied_request,
        );
        assert_eq!(denied, ProtectedInvocationDecision::Denied);
        assert!(!format!("{denied:?}").contains(secret));

        let security = decision_security(
            &active,
            vec![ExecuteGrant::new(
                PrincipalId::from_bytes([25; 16]),
                FunctionId::from_bytes([10; 16]),
            )],
        );
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let bind_failed_request = decision_request(vec![
            InvocationArgument::new(
                InvocationParameterSelector::parameter_id(ParameterId::from_bytes([11; 16])),
                value(7),
            ),
            InvocationArgument::new(
                InvocationParameterSelector::name("value").expect("a selector"),
                value(7),
            ),
        ]);
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                SecurityInvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair()),
                &active,
                5,
                &bind_failed_request,
            ),
            ProtectedInvocationDecision::AllowedWithBindFailure
        );
    }

    #[test]
    fn protected_decision_resolves_ambiguity_privately_and_fails_closed_for_definer() {
        let verified = standard_executable_snapshot();
        let standard_function = verified.catalogue().functions()[0].clone();
        let application = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([26; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x5b; 16]),
                QualifiedSemanticName::new(["std", "invoke"]).expect("a schema name"),
            )],
            vec![],
            vec![standard_function],
        )
        .expect("an application catalogue that duplicates a standard function");
        assert!(
            resolve_target_in_catalogues(
                &application,
                Some(&verified),
                &InvocationTarget::function_id(FunctionId::from_bytes([0x30; 16])),
            )
            .is_none()
        );

        let active = decision_active_revision(FunctionSecurity::Definer);
        let security = decision_security(
            &active,
            vec![ExecuteGrant::new(
                PrincipalId::from_bytes([25; 16]),
                FunctionId::from_bytes([10; 16]),
            )],
        );
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let request = decision_request(vec![InvocationArgument::new(
            InvocationParameterSelector::parameter_id(ParameterId::from_bytes([11; 16])),
            value(7),
        )]);
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                SecurityInvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair()),
                &active,
                5,
                &request,
            ),
            ProtectedInvocationDecision::Denied
        );
    }

    #[test]
    fn private_resolution_pins_class_and_executable_for_both_target_classes() {
        let verified = standard_executable_snapshot();
        let standard_function = verified.catalogue().functions()[0].clone();
        let application = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x5a; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x5b; 16]),
                QualifiedSemanticName::new(["app"]).expect("a schema name"),
            )],
            vec![],
            vec![decision_function(FunctionSecurity::Invoker)],
        )
        .expect("an application catalogue");
        let both = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x5c; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x5d; 16]),
                QualifiedSemanticName::new(["std", "invoke"]).expect("a schema name"),
            )],
            vec![],
            vec![standard_function.clone()],
        )
        .expect("an application catalogue that duplicates a standard function");
        let application_selector = InvocationTarget::function_id(FunctionId::from_bytes([10; 16]));
        let standard_selector = InvocationTarget::function_id(standard_function.id());
        let unknown_selector = InvocationTarget::function_id(FunctionId::from_bytes([0x60; 16]));

        let application_target =
            resolve_target_in_catalogues(&application, None, &application_selector)
                .expect("an application function must resolve");
        assert_eq!(application_target.class(), PrivateTargetClass::Application);
        assert_eq!(
            application_target.executable_revision(),
            FunctionRevisionId::from_bytes([12; 16])
        );
        assert_eq!(application_target.standard_revision(), None);
        assert_eq!(application_target.id(), FunctionId::from_bytes([10; 16]));

        let standard_target =
            resolve_target_in_catalogues(&application, Some(&verified), &standard_selector)
                .expect("a verified-standard function must resolve");
        assert_eq!(
            standard_target.class(),
            PrivateTargetClass::VerifiedStandard
        );
        assert_eq!(
            standard_target.executable_revision(),
            standard_function.current_revision()
        );
        assert_eq!(
            standard_target.standard_revision(),
            Some(verified.revision())
        );
        assert_eq!(standard_target.id(), standard_function.id());
        assert_eq!(
            standard_target.executable_revision(),
            verified.executables()[0].revision().id()
        );

        assert!(
            resolve_target_in_catalogues(&both, Some(&verified), &standard_selector).is_none(),
            "a function in both catalogues must be ambiguous and resolve to neither"
        );
        assert!(
            resolve_target_in_catalogues(&application, None, &standard_selector).is_none(),
            "a function in no catalogue must not resolve"
        );
        assert!(
            resolve_target_in_catalogues(&application, Some(&verified), &unknown_selector)
                .is_none(),
            "an unknown function must not resolve"
        );
    }

    #[test]
    fn protected_decision_allows_scalar_and_set_system_identities() {
        let active = decision_active_revision(FunctionSecurity::Invoker);
        let security = decision_security(&active, vec![]);
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let system_request = |target: InvocationTarget| {
            let mut input = request_input(Vec::new(), None);
            input.target = target;
            InvokeRequest::new(input).expect("a system target request")
        };
        let invoke_target = SecurityInvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
        let qualified_session = system_request(
            InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["sys", "security", "session_principal"])
                    .expect("a qualified system target"),
            )
            .expect("a qualified target"),
        );
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                invoke_target,
                &active,
                5,
                &qualified_session,
            ),
            ProtectedInvocationDecision::Allowed
        );
        let effective = system_request(InvocationTarget::function_id(
            SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        ));
        assert_eq!(
            decide_protected_invocation(&security, &session, invoke_target, &active, 5, &effective,),
            ProtectedInvocationDecision::Allowed
        );
        let active_roles = system_request(InvocationTarget::function_id(
            SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
        ));
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                invoke_target,
                &active,
                5,
                &active_roles,
            ),
            ProtectedInvocationDecision::Allowed
        );
    }

    #[test]
    fn protected_decision_allows_application_and_verified_standard_and_denies_closed() {
        let system_target = |active: &ActiveDatabaseRevision| {
            SecurityInvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair())
        };

        let active = decision_active_revision(FunctionSecurity::Invoker);
        let security = decision_security(
            &active,
            vec![ExecuteGrant::new(
                PrincipalId::from_bytes([25; 16]),
                FunctionId::from_bytes([10; 16]),
            )],
        );
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let application_request = decision_request(vec![InvocationArgument::new(
            InvocationParameterSelector::parameter_id(ParameterId::from_bytes([11; 16])),
            value(7),
        )]);
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                system_target(&active),
                &active,
                5,
                &application_request,
            ),
            ProtectedInvocationDecision::Allowed
        );

        let active = standard_decision_active_revision();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("a pinned standard snapshot");
        let function = standard.catalogue().functions()[0].clone();
        let grant = ExecuteGrant::new(PrincipalId::from_bytes([25; 16]), function.id());
        let security = standard_security_for(&active, vec![grant]);
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let standard_request = standard_request();
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                system_target(&active),
                &active,
                5,
                &standard_request,
            ),
            ProtectedInvocationDecision::Allowed
        );

        let missing = unknown_request();
        assert_eq!(
            decide_protected_invocation(
                &security,
                &session,
                system_target(&active),
                &active,
                5,
                &missing,
            ),
            ProtectedInvocationDecision::Denied
        );

        let ambiguous = ambiguous_standard_active_revision();
        let ambiguous_security = standard_security_for(&ambiguous, vec![grant]);
        let session = ambiguous_security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        assert_eq!(
            decide_protected_invocation(
                &ambiguous_security,
                &session,
                system_target(&ambiguous),
                &ambiguous,
                5,
                &standard_request,
            ),
            ProtectedInvocationDecision::Denied
        );

        let wrong_pin = SecuritySnapshot::new_with_function_targets(
            active.pair(),
            vec![SecurityFunctionTarget::verified_standard(
                function.id(),
                standard.revision(),
                FunctionRevisionId::from_bytes([0x7b; 16]),
            )],
            vec![Principal::new(
                PrincipalId::from_bytes([25; 16]),
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![grant],
        )
        .expect("a wrong-pin security snapshot");
        let session = wrong_pin
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        assert_eq!(
            decide_protected_invocation(
                &wrong_pin,
                &session,
                system_target(&active),
                &active,
                5,
                &standard_request,
            ),
            ProtectedInvocationDecision::Denied
        );

        let definer_active = standard_decision_active_revision_with(
            standard_executable_snapshot_with(&standard_function_with(FunctionSecurity::Definer)),
        );
        let definer_security = standard_security_for(&definer_active, vec![grant]);
        let session = definer_security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        assert_eq!(
            decide_protected_invocation(
                &definer_security,
                &session,
                system_target(&definer_active),
                &definer_active,
                5,
                &standard_request,
            ),
            ProtectedInvocationDecision::Denied
        );
    }

    #[test]
    fn protected_decision_never_exposes_target_or_binding_evidence() {
        let active = standard_decision_active_revision();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("a pinned standard snapshot");
        let function = standard.catalogue().functions()[0].clone();
        let security = standard_security_for(
            &active,
            vec![ExecuteGrant::new(
                PrincipalId::from_bytes([25; 16]),
                function.id(),
            )],
        );
        let session = security
            .bind_authenticated_session(PrincipalId::from_bytes([25; 16]), vec![])
            .expect("an active user session");
        let secret_selector = "private-standard-selector";
        let request = InvokeRequest::new(InvokeRequestInput {
            target: InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("a qualified name"),
            )
            .expect("a qualified target"),
            arguments: vec![InvocationArgument::new(
                InvocationParameterSelector::name(secret_selector).expect("a selector"),
                value(7),
            )],
            ..request_input(Vec::new(), None)
        })
        .expect("a checked request with a private selector");
        let decision = decide_protected_invocation(
            &security,
            &session,
            SecurityInvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair()),
            &active,
            5,
            &request,
        );
        assert_eq!(
            decision,
            ProtectedInvocationDecision::AllowedWithBindFailure
        );
        let debug = format!("{decision:?}");
        assert!(!debug.contains(secret_selector), "{debug}");
        assert!(!debug.contains("std"), "{debug}");
        assert!(!debug.contains("echo"), "{debug}");
        assert!(!debug.contains("0x30"), "{debug}");
    }
}
