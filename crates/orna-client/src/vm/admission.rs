use orna_artifact::client_plan::{
    MAX_ARTIFACT_BYTES, MAX_CAPABILITY_REQUIREMENTS, MAX_EXPRESSION_DEPTH, MAX_EXPRESSION_NODES,
};
use orna_core::canonical_hash::artifact_payload_digest;

use std::{error::Error, fmt};

const MAX_TEXT_BYTES: usize = 4096;
const MAX_CAPABILITIES: usize = MAX_CAPABILITY_REQUIREMENTS;
const MAX_CONTRACTS: usize = 8;
const MAX_PLAN_DEPTH: u16 = MAX_EXPRESSION_DEPTH as u16;
const MAX_PLAN_OPERATIONS: u32 = MAX_EXPRESSION_NODES as u32;
const OUTER_CAPABILITY_VERSION: u32 = 5;

/// The execution domain encoded in an admitted CLIENT artifact identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClientVmArtifactKind {
    /// A CLIENT artifact which can enter the local VM boundary.
    Client,
    /// A SERVER artifact, rejected by CLIENT admission.
    Server,
}

/// The argument source retained by one capability declaration.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ClientVmCapabilityArgument {
    /// A literal text scope or identifier.
    Text(String),
    /// A function parameter whose value supplies the scope or identifier.
    Parameter(String),
}

/// One canonical capability declaration retained by VM admission.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClientVmCapabilityDeclaration {
    name: String,
    argument: ClientVmCapabilityArgument,
}

impl ClientVmCapabilityDeclaration {
    /// Creates one capability declaration. Admission validates its bounds.
    pub fn new(name: impl Into<String>, argument: ClientVmCapabilityArgument) -> Self {
        Self {
            name: name.into(),
            argument,
        }
    }

    /// Returns the qualified capability name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the literal or parameter argument source.
    pub const fn argument(&self) -> &ClientVmCapabilityArgument {
        &self.argument
    }
}

/// Bounded limits declared by one immutable artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClientVmArtifactLimits {
    payload_bytes: usize,
    plan_depth: u16,
    plan_operations: u32,
}

impl ClientVmArtifactLimits {
    /// Creates checked artifact limits.
    pub fn new(
        payload_bytes: usize,
        plan_depth: u16,
        plan_operations: u32,
    ) -> Result<Self, ClientVmAdmissionError> {
        if payload_bytes == 0 || payload_bytes > MAX_ARTIFACT_BYTES {
            return Err(ClientVmAdmissionError::LimitExceeded {
                field: "payload_bytes",
            });
        }
        if plan_depth == 0 || plan_depth > MAX_PLAN_DEPTH {
            return Err(ClientVmAdmissionError::LimitExceeded {
                field: "plan_depth",
            });
        }
        if plan_operations == 0 || plan_operations > MAX_PLAN_OPERATIONS {
            return Err(ClientVmAdmissionError::LimitExceeded {
                field: "plan_operations",
            });
        }
        Ok(Self {
            payload_bytes,
            plan_depth,
            plan_operations,
        })
    }

    /// Returns the maximum payload size accepted by this artifact.
    pub const fn payload_bytes(self) -> usize {
        self.payload_bytes
    }

    /// Returns the maximum decoded plan depth accepted by this artifact.
    pub const fn plan_depth(self) -> u16 {
        self.plan_depth
    }

    /// Returns the maximum decoded operation count accepted by this artifact.
    pub const fn plan_operations(self) -> u32 {
        self.plan_operations
    }
}

/// The immutable structural tuple used to identify one CLIENT artifact.
///
/// The verifier creates admissions from this value and the kernel-authorised
/// expected tuple. The fields are private so callers cannot mutate an admitted
/// identity after verification.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ClientVmArtifactIdentity {
    function: [u8; 16],
    function_revision: [u8; 16],
    revision_pair: [[u8; 16]; 2],
    kind: ClientVmArtifactKind,
    format: String,
    outer_version: u32,
    inner_version: Option<u32>,
    language: String,
    digest: [u8; 32],
    capabilities: Vec<ClientVmCapabilityDeclaration>,
    contracts: Vec<String>,
    limits: ClientVmArtifactLimits,
}

impl ClientVmArtifactIdentity {
    /// Creates one structural identity for verifier use.
    ///
    /// This constructor validates bounded metadata and the closed version
    /// vocabulary. It does not itself prove that the tuple belongs to an
    /// active revision; [`ClientVmAdmission::admit`] performs that comparison.
    pub(crate) fn new(
        function: [u8; 16],
        function_revision: [u8; 16],
        revision_pair: [[u8; 16]; 2],
        kind: ClientVmArtifactKind,
        format: impl Into<String>,
        outer_version: u32,
        inner_version: Option<u32>,
        language: impl Into<String>,
        digest: [u8; 32],
        capabilities: impl IntoIterator<Item = ClientVmCapabilityDeclaration>,
        contracts: impl IntoIterator<Item = impl Into<String>>,
        limits: ClientVmArtifactLimits,
    ) -> Result<Self, ClientVmAdmissionError> {
        let format = format.into();
        let language = language.into();
        validate_text("format", &format)?;
        validate_text("language", &language)?;
        if kind != ClientVmArtifactKind::Client {
            return Err(ClientVmAdmissionError::WrongExecutionDomain);
        }
        validate_versions(outer_version, inner_version)?;
        let capabilities = validate_capabilities(capabilities)?;
        let contracts = validate_set("contract", contracts, MAX_CONTRACTS)?;
        Ok(Self {
            function,
            function_revision,
            revision_pair,
            kind,
            format,
            outer_version,
            inner_version,
            language,
            digest,
            capabilities,
            contracts,
            limits,
        })
    }

    /// Returns the stable function identity bytes.
    pub const fn function(&self) -> [u8; 16] {
        self.function
    }

    /// Returns the immutable function-revision identity bytes.
    pub const fn function_revision(&self) -> [u8; 16] {
        self.function_revision
    }

    /// Returns the source and catalogue revision identity bytes.
    pub const fn revision_pair(&self) -> [[u8; 16]; 2] {
        self.revision_pair
    }

    /// Returns the encoded execution domain.
    pub const fn kind(&self) -> ClientVmArtifactKind {
        self.kind
    }

    /// Returns the artifact format identity.
    pub fn format(&self) -> &str {
        &self.format
    }

    /// Returns the outer artifact format version.
    pub const fn outer_version(&self) -> u32 {
        self.outer_version
    }

    /// Returns the effective inner plan version, when the outer envelope has
    /// one.
    pub const fn inner_version(&self) -> Option<u32> {
        self.inner_version
    }

    /// Returns the compiler language identity.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns the declared payload digest.
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// Returns the canonical declared capability declarations.
    pub fn capabilities(&self) -> &[ClientVmCapabilityDeclaration] {
        &self.capabilities
    }

    /// Returns the canonical declared runtime-contract names.
    pub fn contracts(&self) -> &[String] {
        &self.contracts
    }

    /// Returns the artifact-declared execution limits.
    pub const fn limits(&self) -> ClientVmArtifactLimits {
        self.limits
    }
}

/// The mutable host values captured by one Stage 1 admission.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClientVmHostAdmissionContext {
    policy_epoch: u64,
    runtime_offer_digest: [u8; 32],
    host_limit_ceiling: ClientVmArtifactLimits,
    cancellation_epoch: u64,
    security_context_digest: [u8; 32],
}

impl ClientVmHostAdmissionContext {
    /// Creates a host admission snapshot without a security binding.
    ///
    /// The VM entry point adds the authorised security-context digest before
    /// it stores the snapshot. A zero value is retained for deterministic
    /// control-plane fixtures that do not model a security decision.
    pub const fn new(
        policy_epoch: u64,
        runtime_offer_digest: [u8; 32],
        host_limit_ceiling: ClientVmArtifactLimits,
        cancellation_epoch: u64,
    ) -> Self {
        Self {
            policy_epoch,
            runtime_offer_digest,
            host_limit_ceiling,
            cancellation_epoch,
            security_context_digest: [0; 32],
        }
    }

    /// Returns a copy with the kernel-authorised security context bound.
    pub const fn with_security_context(self, security_context_digest: [u8; 32]) -> Self {
        Self {
            security_context_digest,
            ..self
        }
    }

    /// Returns the grant-policy epoch captured by the admission.
    pub const fn policy_epoch(self) -> u64 {
        self.policy_epoch
    }

    /// Returns the runtime-offer witness digest captured by the admission.
    pub const fn runtime_offer_digest(self) -> [u8; 32] {
        self.runtime_offer_digest
    }

    /// Returns the host execution ceiling captured by the admission.
    pub const fn host_limit_ceiling(self) -> ClientVmArtifactLimits {
        self.host_limit_ceiling
    }

    /// Returns the cancellation epoch captured by the admission.
    pub const fn cancellation_epoch(self) -> u64 {
        self.cancellation_epoch
    }
    /// Returns the security-context digest captured by the admission.
    pub const fn security_context_digest(self) -> [u8; 32] {
        self.security_context_digest
    }
}

/// A typed structural or semantic failure during Stage 1 admission.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientVmAdmissionError {
    /// The artifact is not marked for CLIENT execution.
    WrongExecutionDomain,
    /// A structural tuple field differs from the kernel-authorised tuple.
    TupleMismatch { field: &'static str },
    /// A required text identity is empty or contains NUL bytes.
    InvalidText { field: &'static str },
    /// A text identity exceeds its fixed bound.
    TextTooLong { field: &'static str },
    /// The outer/inner version pair is outside the closed vocabulary.
    UnsupportedVersion { outer: u32, inner: Option<u32> },
    /// An artifact-declared limit is zero or exceeds the Stage 1 ceiling.
    LimitExceeded { field: &'static str },
    /// A capability or contract name is empty, invalid, duplicated, or too
    /// numerous.
    InvalidSet { field: &'static str },
    /// The payload is larger than the declared or global bound.
    PayloadTooLarge { bytes: usize, maximum: usize },
    /// The payload digest does not match the structural identity.
    DigestMismatch,
    /// The host ceiling cannot satisfy the artifact-declared limits.
    HostLimitExceeded { field: &'static str },
    /// The selected plan decoder rejected the bounded payload.
    DecodeRejected,
    /// Semantic checks rejected the decoded plan.
    SemanticRejected,
    /// The host context was cancelled and will not admit new work.
    HostCancelled,
}

impl fmt::Display for ClientVmAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongExecutionDomain => {
                formatter.write_str("CLIENT VM artifact has the wrong execution domain")
            }
            Self::TupleMismatch { field } => {
                write!(formatter, "CLIENT VM artifact tuple mismatch in {field}")
            }
            Self::InvalidText { field } => {
                write!(formatter, "CLIENT VM artifact {field} is invalid")
            }
            Self::TextTooLong { field } => {
                write!(formatter, "CLIENT VM artifact {field} is too long")
            }
            Self::UnsupportedVersion { outer, inner } => {
                write!(
                    formatter,
                    "CLIENT VM artifact version pair is unsupported: outer {outer}, inner {inner:?}"
                )
            }
            Self::LimitExceeded { field } => {
                write!(formatter, "CLIENT VM artifact limit {field} is invalid")
            }
            Self::InvalidSet { field } => {
                write!(formatter, "CLIENT VM artifact {field} set is invalid")
            }
            Self::PayloadTooLarge { bytes, maximum } => {
                write!(
                    formatter,
                    "CLIENT VM payload is {bytes} bytes, maximum is {maximum}"
                )
            }
            Self::DigestMismatch => {
                formatter.write_str("CLIENT VM artifact payload digest is invalid")
            }
            Self::HostLimitExceeded { field } => {
                write!(formatter, "CLIENT VM host ceiling rejects {field}")
            }
            Self::DecodeRejected => formatter.write_str("CLIENT VM plan decoding failed"),
            Self::SemanticRejected => {
                formatter.write_str("CLIENT VM plan semantic admission failed")
            }
            Self::HostCancelled => formatter.write_str("CLIENT VM host context is cancelled"),
        }
    }
}

impl Error for ClientVmAdmissionError {}

/// An immutable plan admitted after structural and semantic checks.
///
/// `T` is the concrete decoded plan owned by the verifier. The constructor is
/// crate-private so callers cannot manufacture an admitted plan from a raw
/// payload or caller-selected metadata.
#[derive(Debug)]
pub struct ClientVmAdmission<T> {
    identity: ClientVmArtifactIdentity,
    host: ClientVmHostAdmissionContext,
    plan: T,
}

impl<T> ClientVmAdmission<T> {
    /// Performs bounded identity, digest, tuple, and host-limit checks before
    /// invoking `decode`. The decoder runs at most once and is never called for
    /// a failed structural admission. `semantic_check` runs once on the owned
    /// decoded plan before the admission is returned.
    pub(crate) fn admit<Decode, Check>(
        expected: &ClientVmArtifactIdentity,
        candidate: ClientVmArtifactIdentity,
        payload: &[u8],
        host: ClientVmHostAdmissionContext,
        decode: Decode,
        semantic_check: Check,
    ) -> Result<Self, ClientVmAdmissionError>
    where
        Decode: FnOnce(&[u8]) -> Result<T, ClientVmAdmissionError>,
        Check: FnOnce(&T) -> Result<(), ClientVmAdmissionError>,
    {
        compare_identity(expected, &candidate)?;
        if payload.len() > MAX_ARTIFACT_BYTES {
            return Err(ClientVmAdmissionError::PayloadTooLarge {
                bytes: payload.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }
        if payload.len() > candidate.limits.payload_bytes {
            return Err(ClientVmAdmissionError::PayloadTooLarge {
                bytes: payload.len(),
                maximum: candidate.limits.payload_bytes,
            });
        }
        if payload.len() > host.host_limit_ceiling.payload_bytes {
            return Err(ClientVmAdmissionError::PayloadTooLarge {
                bytes: payload.len(),
                maximum: host.host_limit_ceiling.payload_bytes,
            });
        }
        let digest = artifact_payload_digest(payload)
            .map_err(|_| ClientVmAdmissionError::DigestMismatch)?
            .to_bytes();
        if digest != candidate.digest {
            return Err(ClientVmAdmissionError::DigestMismatch);
        }
        let plan = decode(payload)?;
        semantic_check(&plan)?;
        if candidate.limits.payload_bytes > host.host_limit_ceiling.payload_bytes {
            return Err(ClientVmAdmissionError::HostLimitExceeded {
                field: "payload_bytes",
            });
        }
        if candidate.limits.plan_depth > host.host_limit_ceiling.plan_depth {
            return Err(ClientVmAdmissionError::HostLimitExceeded {
                field: "plan_depth",
            });
        }
        if candidate.limits.plan_operations > host.host_limit_ceiling.plan_operations {
            return Err(ClientVmAdmissionError::HostLimitExceeded {
                field: "plan_operations",
            });
        }
        Ok(Self {
            identity: candidate,
            host,
            plan,
        })
    }

    /// Returns the immutable structural identity.
    pub fn identity(&self) -> &ClientVmArtifactIdentity {
        &self.identity
    }

    /// Returns the immutable host snapshot.
    pub const fn host(&self) -> ClientVmHostAdmissionContext {
        self.host
    }

    /// Returns the decoded plan by shared reference.
    pub const fn plan(&self) -> &T {
        &self.plan
    }

    /// Consumes the admission and returns its decoded plan.
    pub fn into_plan(self) -> T {
        self.plan
    }
}

fn compare_identity(
    expected: &ClientVmArtifactIdentity,
    candidate: &ClientVmArtifactIdentity,
) -> Result<(), ClientVmAdmissionError> {
    if candidate.kind != ClientVmArtifactKind::Client {
        return Err(ClientVmAdmissionError::WrongExecutionDomain);
    }
    if expected.function != candidate.function {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "function" });
    }
    if expected.function_revision != candidate.function_revision {
        return Err(ClientVmAdmissionError::TupleMismatch {
            field: "function_revision",
        });
    }
    if expected.revision_pair != candidate.revision_pair {
        return Err(ClientVmAdmissionError::TupleMismatch {
            field: "revision_pair",
        });
    }
    if expected.format != candidate.format {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "format" });
    }
    if expected.outer_version != candidate.outer_version {
        return Err(ClientVmAdmissionError::TupleMismatch {
            field: "outer_version",
        });
    }
    if expected.inner_version != candidate.inner_version {
        return Err(ClientVmAdmissionError::TupleMismatch {
            field: "inner_version",
        });
    }
    if expected.language != candidate.language {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "language" });
    }
    if expected.digest != candidate.digest {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "digest" });
    }
    if expected.capabilities != candidate.capabilities {
        return Err(ClientVmAdmissionError::TupleMismatch {
            field: "capabilities",
        });
    }
    if expected.contracts != candidate.contracts {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "contracts" });
    }
    if expected.limits != candidate.limits {
        return Err(ClientVmAdmissionError::TupleMismatch { field: "limits" });
    }
    Ok(())
}

fn validate_versions(outer: u32, inner: Option<u32>) -> Result<(), ClientVmAdmissionError> {
    if outer == OUTER_CAPABILITY_VERSION {
        if inner.is_some_and(is_supported_inner_version) {
            return Ok(());
        }
    } else if is_supported_legacy_version(outer) && inner.is_none() {
        return Ok(());
    }
    Err(ClientVmAdmissionError::UnsupportedVersion { outer, inner })
}

fn is_supported_inner_version(version: u32) -> bool {
    is_supported_legacy_version(version)
}

fn is_supported_legacy_version(version: u32) -> bool {
    matches!(version, 1 | 2 | 3 | 4 | 6 | 7 | 8 | 9 | 10)
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ClientVmAdmissionError> {
    if value.is_empty() || value.as_bytes().contains(&0) {
        return Err(ClientVmAdmissionError::InvalidText { field });
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(ClientVmAdmissionError::TextTooLong { field });
    }
    Ok(())
}

fn validate_capabilities(
    input: impl IntoIterator<Item = ClientVmCapabilityDeclaration>,
) -> Result<Vec<ClientVmCapabilityDeclaration>, ClientVmAdmissionError> {
    let mut values = Vec::new();
    for value in input {
        if values.len() >= MAX_CAPABILITIES {
            return Err(ClientVmAdmissionError::InvalidSet {
                field: "capability",
            });
        }
        validate_text("capability", &value.name)?;
        match &value.argument {
            ClientVmCapabilityArgument::Text(argument)
            | ClientVmCapabilityArgument::Parameter(argument) => {
                validate_text("capability_argument", argument)?;
            }
        }
        values.push(value);
    }
    values.sort_unstable();
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ClientVmAdmissionError::InvalidSet {
            field: "capability",
        });
    }
    Ok(values)
}

fn validate_set(
    field: &'static str,
    input: impl IntoIterator<Item = impl Into<String>>,
    maximum: usize,
) -> Result<Vec<String>, ClientVmAdmissionError> {
    let mut values = Vec::new();
    for value in input {
        if values.len() >= maximum {
            return Err(ClientVmAdmissionError::InvalidSet { field });
        }
        let value = value.into();
        validate_text(field, &value)?;
        values.push(value);
    }
    values.sort_unstable();
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ClientVmAdmissionError::InvalidSet { field });
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn limits() -> ClientVmArtifactLimits {
        ClientVmArtifactLimits::new(1024, 32, 100).expect("valid limits")
    }

    fn capability(name: &str) -> ClientVmCapabilityDeclaration {
        ClientVmCapabilityDeclaration::new(
            name,
            ClientVmCapabilityArgument::Text("scope".to_owned()),
        )
    }

    fn identity(digest: [u8; 32]) -> ClientVmArtifactIdentity {
        ClientVmArtifactIdentity::new(
            [1; 16],
            [2; 16],
            [[3; 16], [4; 16]],
            ClientVmArtifactKind::Client,
            "orna.client/1",
            3,
            None,
            "orna.language/1",
            digest,
            [capability("std.ui")],
            ["std.ui.text"],
            limits(),
        )
        .expect("valid identity")
    }

    fn host() -> ClientVmHostAdmissionContext {
        ClientVmHostAdmissionContext::new(7, [8; 32], limits(), 9)
    }

    fn digest(payload: &[u8]) -> [u8; 32] {
        artifact_payload_digest(payload)
            .expect("test payload digest")
            .to_bytes()
    }

    #[test]
    fn structural_failure_does_not_decode() {
        let payload = b"plan";
        let expected = identity(digest(payload));
        let mut candidate = expected.clone();
        candidate.function = [9; 16];
        let decode_count = Cell::new(0);

        let error = ClientVmAdmission::<u8>::admit(
            &expected,
            candidate,
            payload,
            host(),
            |_| {
                decode_count.set(decode_count.get() + 1);
                Ok(1)
            },
            |_| Ok(()),
        )
        .expect_err("tuple mismatch");

        assert_eq!(
            error,
            ClientVmAdmissionError::TupleMismatch { field: "function" }
        );
        assert_eq!(decode_count.get(), 0);
    }

    #[test]
    fn successful_admission_decodes_once_and_retains_plan() {
        let payload = b"plan";
        let expected = identity(digest(payload));
        let decode_count = Cell::new(0);

        let admission = ClientVmAdmission::admit(
            &expected,
            expected.clone(),
            payload,
            host(),
            |_| {
                decode_count.set(decode_count.get() + 1);
                Ok::<_, ClientVmAdmissionError>(42_u32)
            },
            |plan| {
                (*plan == 42)
                    .then_some(())
                    .ok_or(ClientVmAdmissionError::SemanticRejected)
            },
        )
        .expect("admission");

        assert_eq!(decode_count.get(), 1);
        assert_eq!(*admission.plan(), 42);
        assert_eq!(admission.identity(), &expected);
    }

    #[test]
    fn canonical_sets_are_ordered_and_duplicates_are_rejected() {
        let identity = ClientVmArtifactIdentity::new(
            [1; 16],
            [2; 16],
            [[3; 16], [4; 16]],
            ClientVmArtifactKind::Client,
            "format",
            3,
            None,
            "language",
            [5; 32],
            [capability("z"), capability("a")],
            ["contract"],
            limits(),
        )
        .expect("ordered identity");
        assert_eq!(identity.capabilities(), &[capability("a"), capability("z")]);
        assert_eq!(
            ClientVmArtifactIdentity::new(
                [1; 16],
                [2; 16],
                [[3; 16], [4; 16]],
                ClientVmArtifactKind::Client,
                "format",
                3,
                None,
                "language",
                [5; 32],
                [capability("a"), capability("a")],
                ["contract"],
                limits(),
            )
            .expect_err("duplicate capability"),
            ClientVmAdmissionError::InvalidSet {
                field: "capability"
            }
        );
    }

    #[test]
    fn capability_identity_retains_argument_source_and_value() {
        let declaration = ClientVmCapabilityDeclaration::new(
            "std.fs.read",
            ClientVmCapabilityArgument::Parameter("path".to_owned()),
        );
        let identity = ClientVmArtifactIdentity::new(
            [1; 16],
            [2; 16],
            [[3; 16], [4; 16]],
            ClientVmArtifactKind::Client,
            "format",
            5,
            Some(3),
            "language",
            [5; 32],
            [declaration.clone()],
            std::iter::empty::<String>(),
            limits(),
        )
        .expect("capability identity");

        assert_eq!(identity.capabilities(), &[declaration]);
    }

    #[test]
    fn payload_digest_is_checked_before_decode() {
        let payload = b"plan";
        let expected = identity([9; 32]);
        let decode_count = Cell::new(0);
        let error = ClientVmAdmission::<u8>::admit(
            &expected,
            expected.clone(),
            payload,
            host(),
            |_| {
                decode_count.set(decode_count.get() + 1);
                Ok(1)
            },
            |_| Ok(()),
        )
        .expect_err("digest mismatch");
        assert_eq!(error, ClientVmAdmissionError::DigestMismatch);
        assert_eq!(decode_count.get(), 0);
    }
}
