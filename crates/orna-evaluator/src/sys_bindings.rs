//! Evaluator bindings for native operations in the generated sys host registry.

use std::sync::{Arc, Mutex};

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use orna_foundation::{CanonicalValue, OvbRaw, SafeText};
use orna_repository::{
    CapturedBlobCandidate, GraphError, OrpBlobBinding, RepositoryCaptureCapability,
};
use orna_syntax::Expr;
use orna_sys::{
    CaptureFileError, ClockProvider, EnvironmentDispatchValue, EnvironmentProvider,
    EnvironmentProviderError, FailureCode, FilesystemProvider, HostHttpResponse, HostOperation,
    HostOperationDescriptor, HostProvider, HttpProvider, OperationContract, OperationId, Origin,
    ProcessProvider, ProviderDiagnostic, ProviderFailure, ProviderOffer, SemanticRoleContract,
    SysEffect, SysFailureCode, SysFunction, SysFunctionBinding, SysFunctionHost,
    SystemOperationProvider, TypeId, TypedValue,
};
use orna_value::{Blob, ContextValue, Error as ValueError, ValueFormat};

use crate::sys_dispatch::{self, RegisteredOperation};
use crate::{
    CancellationToken, EffectHandler, EvaluationError, Ovb2EffectHandler, StepBudget,
    SysHostAttachment, SysHostContext, Value,
};

const MAX_RANDOM_BYTES: usize = 1_048_576;

/// The decoded arguments of one `std.io.process.run` call.
struct ProcessRequest<'a> {
    executable: &'a str,
    args: Vec<String>,
    working_directory: &'a str,
    environment: Vec<(String, String)>,
    input: Option<&'a [u8]>,
    timeout: Option<std::time::Duration>,
    output_limit: usize,
}

/// Decodes the seven portable arguments before any provider work starts, so a
/// mistyped argument never reaches the host process boundary.
fn process_request(arguments: &[CanonicalValue]) -> Result<ProcessRequest<'_>, EvaluationError> {
    let [
        executable,
        args,
        working_directory,
        environment,
        input,
        timeout,
        output_limit,
    ] = arguments
    else {
        return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
    };
    let executable = raw_text(executable.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
    let working_directory =
        raw_text(working_directory.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
    let args = raw_text_array(args.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
    let environment =
        raw_text_pairs(environment.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
    let input = match optional_raw(input.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))? {
        None | Some(OvbRaw::Null) => None,
        Some(OvbRaw::Bytes(bytes)) => Some(bytes.as_slice()),
        Some(_) => return Err(redacted_error("ORNA-EVAL-TYPE")),
    };
    let timeout =
        match optional_raw(timeout.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))? {
            None | Some(OvbRaw::Null) => None,
            Some(OvbRaw::Tag(60005, value)) => {
                Some(raw_duration(value).ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?)
            }
            Some(_) => return Err(redacted_error("ORNA-EVAL-TYPE")),
        };
    let OvbRaw::Int(output_limit) = output_limit.raw() else {
        return Err(redacted_error("ORNA-EVAL-TYPE"));
    };
    let output_limit = output_limit
        .to_usize()
        .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
    Ok(ProcessRequest {
        executable,
        args,
        working_directory,
        environment,
        input,
        timeout,
        output_limit,
    })
}

/// Encodes a process exit as the result tuple's status field.
fn process_exit_status(status: Option<i32>) -> OvbRaw {
    match status {
        Some(status) => OvbRaw::Tag(
            60013,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Int(1.into()),
                OvbRaw::Int(status.into()),
            ])),
        ),
        None => OvbRaw::Tag(60013, Box::new(OvbRaw::Array(vec![OvbRaw::Int(0.into())]))),
    }
}

/// Evaluator-facing registration of built-in sys host providers.
///
/// Operations are selected from the build-generated sys registry. Environment
/// values are limited to an explicit name snapshot, processes require exact
/// executable/root grants, and clock waits require a duration cap.
#[derive(Clone, Debug, Default)]
pub struct SysHostBindingRegistry {
    environment: EnvironmentProvider,
    process: Option<ProcessProvider>,
    clock: Option<ClockProvider>,
    filesystem: Option<FilesystemProvider>,
    repository_capture: Option<RepositoryCaptureCapability>,
    capture_candidates: Arc<Mutex<Vec<CapturedBlobForRow>>>,
    http: Option<HttpProvider>,
    system_random: bool,
    sys_host_context: Option<SysHostAttachment>,
}

#[derive(Debug)]
struct CapturedBlobForRow {
    candidate: CapturedBlobCandidate,
    media_type: String,
    suffix: Option<String>,
}

impl PartialEq for SysHostBindingRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.environment == other.environment
            && self.process == other.process
            && self.clock == other.clock
            && self.filesystem == other.filesystem
            && self.repository_capture == other.repository_capture
            && self.http == other.http
            && self.system_random == other.system_random
            && self.sys_host_context == other.sys_host_context
    }
}

impl Eq for SysHostBindingRegistry {}

impl SysHostBindingRegistry {
    /// Classifies one catalogued callable for this host.
    ///
    /// The generated dispatcher matches every `SysFunction` variant exactly once;
    /// the classification below says which arms this evaluator serves and which
    /// remain unbound here (ORNA-SYS-008).
    #[must_use]
    pub(crate) fn binding(&self, function: SysFunction) -> SysFunctionBinding {
        orna_sys::dispatch_sys_function(self, function)
    }
}

impl SysFunctionHost for SysHostBindingRegistry {
    fn meta(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn object(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn resolve(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn resolve_function(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn describe(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn source_object_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn source_file_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn history_object_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn history_file_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blame_file_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blame_row_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn dependencies(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn dependents(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn snapshot_snapshot_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn snapshot_commit_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn snapshot_branch_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn snapshot_tag_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn snapshot_git_oid(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn snapshot_str(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn diff(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn changes(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn explain_query(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn explain_function_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn explain_diagnostic(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn checkpoint(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn render_diagnostic(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn rt_info(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn invoke_value(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn invoke_t(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn start_value(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn start_t(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn r#await(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn cancel(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_commit(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_checkout_snapshot_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_checkout_commit_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_checkout_branch_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_checkout_tag_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_checkout_git_oid(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_checkout_str(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_create_branch_snapshot_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_create_branch_commit_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_create_branch_branch_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_create_branch_tag_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_create_branch_git_oid(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_create_branch_str(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_flush(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_compact(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_verify(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_cancel_run(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_pause_stream(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_resume_stream(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_reset_checkpoint(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_retry_failure(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_skip_failure(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_replay_failure(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_resolve_failure(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn consumer_identity(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_plan_checkout_snapshot_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_plan_checkout_commit_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_plan_checkout_branch_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_plan_checkout_tag_ref(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_plan_checkout_git_oid(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_plan_checkout_str(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blob_length(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_digest(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_read(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_verify(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_from_bytes(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_to_bytes(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_begin(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blob_append(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blob_finish(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blob_abort(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blob_capture_file(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_resource(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_review_commit(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn admin_commit_reviewed(&self) -> SysFunctionBinding {
        SysFunctionBinding::Unavailable(Origin::Portable)
    }
    fn blob_media_type(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_suffix(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_annotate(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
    fn blob_same_content(&self) -> SysFunctionBinding {
        SysFunctionBinding::Implemented
    }
}

/// Environment reads are conservatively external (ORNA-CAP-005): every
/// registry binds the environment provider, and its snapshot is host state
/// outside Orna's table transaction, so no bound registry is effect-free.
const ENVIRONMENT_READS_ARE_EXTERNAL: bool = true;

impl SysHostBindingRegistry {
    /// Whether evaluation under this registry may reach state outside Orna's
    /// table transaction (ORNA-CAP-005). A registry is never effect-free: the
    /// environment provider is always bound and its reads are external. An
    /// evaluation with no registry at all is the only effect-free shape.
    #[must_use]
    pub fn may_reach_external_state(&self) -> bool {
        ENVIRONMENT_READS_ARE_EXTERNAL
    }

    /// Links the baked environment role to its native provider and a caller
    /// supplied, already allowlisted environment snapshot.
    #[must_use]
    pub fn new(environment: EnvironmentProvider) -> Self {
        Self {
            environment,
            process: None,
            clock: None,
            filesystem: None,
            repository_capture: None,
            capture_candidates: Arc::default(),
            http: None,
            system_random: false,
            sys_host_context: None,
        }
    }

    /// Adds an explicitly allowlisted native process provider.
    #[must_use]
    pub fn with_process_provider(mut self, provider: ProcessProvider) -> Self {
        self.process = Some(provider);
        self
    }

    /// Adds an explicitly bounded native clock provider for waits.
    #[must_use]
    pub fn with_clock_provider(mut self, provider: ClockProvider) -> Self {
        self.clock = Some(provider);
        self
    }

    /// Installs the attached repository and activation facts that
    /// `sys.rt.info()` reports. Without them the call reports
    /// `sys.version.incompatible`.
    #[must_use]
    pub fn with_sys_host_context(mut self, context: SysHostContext) -> Self {
        self.sys_host_context = Some(SysHostAttachment::new(context));
        self
    }

    /// Installs explicitly allowlisted filesystem roots.
    #[must_use]
    pub fn with_filesystem_provider(mut self, provider: FilesystemProvider) -> Self {
        self.filesystem = Some(provider);
        self
    }

    /// Installs the repository graph and its matching owner-issued read scope
    /// for native Blob capture. The capability is shared with cloned bindings.
    #[must_use]
    pub fn with_repository_capture_capability(
        mut self,
        capability: RepositoryCaptureCapability,
    ) -> Self {
        self.repository_capture = Some(capability);
        self
    }

    /// Accepts a captured Blob with matching content identity as a graph-backed
    /// ORP Blob value. Context-bound Format 3 references need not be hydrated:
    /// identity and annotation are sufficient to select the captured candidate.
    /// This consumes its scratch candidate and returns the value and pin
    /// evidence that a table mutation must commit together.
    pub fn accept_captured_blob_for_row(
        &self,
        value: &ContextValue,
    ) -> Result<OrpBlobBinding, EvaluationError> {
        if !matches!(
            value.format(),
            ValueFormat::Ovb2 | ValueFormat::Rov3 | ValueFormat::Sov3
        ) {
            return Err(redacted_error("ORNA-EVAL-VALUE"));
        }
        let blob = value
            .blob()
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?;
        let candidate = {
            let mut candidates = self
                .capture_candidates
                .lock()
                .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?;
            let blob_identity = blob.content_identity();
            let same_content = |captured: &CapturedBlobForRow| {
                let captured_identity = captured.candidate.content_identity();
                captured_identity.length() == blob_identity.length()
                    && captured_identity.sha256() == blob_identity.sha256()
            };
            // Annotation is mutable independently of captured bytes. Prefer an
            // exact source annotation when duplicate captures exist, but let
            // an annotated value consume its byte-identical capture otherwise.
            let index = candidates
                .iter()
                .rposition(|captured| {
                    same_content(captured)
                        && captured.media_type == blob.media_type()
                        && captured.suffix.as_deref() == blob.suffix()
                })
                .or_else(|| candidates.iter().rposition(same_content))
                .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
            candidates.remove(index).candidate
        };
        self.repository_capture
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?
            .accept_captured_blob(candidate, blob.media_type(), blob.suffix())
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    /// Installs an origin-allowlisted HTTP provider with host-selected limits.
    #[must_use]
    pub fn with_http_provider(mut self, provider: HttpProvider) -> Self {
        self.http = Some(provider);
        self
    }

    /// Installs the operating system CSPRNG for the optional `std.random` module.
    #[must_use]
    pub fn with_system_random_provider(mut self) -> Self {
        self.system_random = true;
        self
    }

    /// Captures only names explicitly approved by the host.
    pub fn capture_environment(
        names: impl IntoIterator<Item = String>,
    ) -> Result<Self, EnvironmentProviderError> {
        EnvironmentProvider::capture_allowlisted(names).map(Self::new)
    }

    fn dispatch(
        &self,
        operation: RegisteredOperation,
        arguments: &[CanonicalValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        match operation {
            RegisteredOperation::StdRandomEntropy => self.random_entropy(arguments),
            RegisteredOperation::Host(host) => self.dispatch_host(host, arguments, cancellation),
        }
    }

    /// Fills one requested entropy count from the operating-system CSPRNG.
    fn random_entropy(
        &self,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if !self.system_random {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let [count] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let OvbRaw::Int(count) = count.raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        let count = count
            .to_usize()
            .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
        if count > MAX_RANDOM_BYTES {
            return Err(redacted_error("ORNA-EVAL-LIMIT"));
        }
        let mut bytes = vec![0; count];
        getrandom::fill(&mut bytes).map_err(|_| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        CanonicalValue::new(OvbRaw::Bytes(bytes))
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    /// Dispatches one generated host operation by its typed identity.
    fn dispatch_host(
        &self,
        host: HostOperation,
        arguments: &[CanonicalValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Some(operation) = sys_dispatch::host_descriptor(host) else {
            return Ok(None);
        };
        if operation
            .effects
            .iter()
            .any(|effect| !matches!(effect, SysEffect::Read | SysEffect::Invoke))
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let role = orna_sys::system_host_operation_registry()
            .role_by_id(&operation.role)
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let provider = HostProvider::parse(role.provider.as_str())
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        if operation.provider != provider
            || !role
                .operations
                .iter()
                .any(|candidate| candidate.as_str() == operation.name.label())
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        match provider {
            HostProvider::Environment => self.dispatch_environment(operation, arguments),
            HostProvider::Process => self.dispatch_process(operation, arguments),
            HostProvider::Clock => self.dispatch_clock(operation, arguments, cancellation),
            HostProvider::Filesystem => self.dispatch_filesystem(operation, arguments),
            HostProvider::Http => self.dispatch_http(operation, arguments),
        }
    }

    fn dispatch_filesystem(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let provider = self
            .filesystem
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        if operation.implementation != last_name_component(operation.name.label()) {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let value = match operation.name {
            HostOperation::StdIoFsReadText => {
                self.filesystem_read_text(operation, provider, arguments)?
            }
            HostOperation::StdIoFsWriteText => {
                self.filesystem_write_text(operation, provider, arguments)?
            }
            HostOperation::StdIoFsAppendText => {
                self.filesystem_append_text(operation, provider, arguments)?
            }
            HostOperation::StdIoFsExists => {
                self.filesystem_exists(operation, provider, arguments)?
            }
            HostOperation::StdIoFsIsDirectory => {
                self.filesystem_is_directory(operation, provider, arguments)?
            }
            HostOperation::StdIoFsList => self.filesystem_list(operation, provider, arguments)?,
            HostOperation::StdIoFsMetadata => {
                self.filesystem_metadata(operation, provider, arguments, true)?
            }
            HostOperation::StdIoFsSymlinkMetadata => {
                self.filesystem_metadata(operation, provider, arguments, false)?
            }
            HostOperation::StdIoFsCreateDir => {
                self.filesystem_create_dir(operation, provider, arguments)?
            }
            HostOperation::StdIoFsRemoveFile => {
                self.filesystem_remove_file(operation, provider, arguments)?
            }
            HostOperation::StdIoFsCopyFile => {
                self.filesystem_copy_file(operation, provider, arguments)?
            }
            HostOperation::StdIoFsMoveFile => {
                self.filesystem_move_file(operation, provider, arguments)?
            }
            HostOperation::StdConcurrentSleep
            | HostOperation::StdIoEnvironmentGet
            | HostOperation::StdIoEnvironmentRequire
            | HostOperation::StdIoProcessRun
            | HostOperation::StdNetHttpCancel
            | HostOperation::StdNetHttpSend
            | HostOperation::StdNetHttpStart
            | HostOperation::StdNetHttpWait
            | HostOperation::SysBlobCaptureFile => {
                return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
            }
        };
        CanonicalValue::new(value)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn filesystem_read_text(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        Ok(OvbRaw::Text(provider.read_text(root, path).map_err(
            |failure| self.failure(operation, failure.code()),
        )?))
    }

    fn filesystem_write_text(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path, contents, overwrite] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let contents = raw_text(contents.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let overwrite =
            raw_bool(overwrite.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        provider
            .write_text(root, path, contents, overwrite)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        Ok(OvbRaw::Null)
    }

    fn filesystem_append_text(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path, contents] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let contents = raw_text(contents.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        provider
            .append_text(root, path, contents)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        Ok(OvbRaw::Null)
    }

    fn filesystem_exists(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        Ok(OvbRaw::Bool(provider.exists(root, path).map_err(
            |failure| self.failure(operation, failure.code()),
        )?))
    }

    fn filesystem_is_directory(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        Ok(OvbRaw::Bool(provider.is_directory(root, path).map_err(
            |failure| self.failure(operation, failure.code()),
        )?))
    }

    fn filesystem_list(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        Ok(OvbRaw::Array(
            provider
                .list(root, path)
                .map_err(|failure| self.failure(operation, failure.code()))?
                .into_iter()
                .map(OvbRaw::Text)
                .collect(),
        ))
    }

    /// Encodes metadata, following symbolic links only when `follow` is set.
    fn filesystem_metadata(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
        follow: bool,
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let metadata = if follow {
            provider.metadata(root, path)
        } else {
            provider.symlink_metadata(root, path)
        }
        .map_err(|failure| self.failure(operation, failure.code()))?;
        Ok(OvbRaw::Array(vec![
            OvbRaw::Text(metadata.kind),
            optional_value(metadata.size.map(|size| OvbRaw::Int(size.into()))),
            optional_value(metadata.modified.and_then(system_time_raw)),
            optional_value(metadata.created.and_then(system_time_raw)),
        ]))
    }

    fn filesystem_create_dir(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path, parents, exist_ok] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let parents = raw_bool(parents.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let exist_ok = raw_bool(exist_ok.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        provider
            .create_dir(root, path, parents, exist_ok)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        Ok(OvbRaw::Null)
    }

    fn filesystem_remove_file(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, path] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        provider
            .remove_file(root, path)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        Ok(OvbRaw::Null)
    }

    fn filesystem_copy_file(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, source, destination, overwrite] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let source = raw_text(source.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let destination =
            raw_text(destination.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let overwrite =
            raw_bool(overwrite.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        provider
            .copy_file(root, source, destination, overwrite)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        Ok(OvbRaw::Null)
    }

    fn filesystem_move_file(
        &self,
        operation: &HostOperationDescriptor,
        provider: &FilesystemProvider,
        arguments: &[CanonicalValue],
    ) -> Result<OvbRaw, EvaluationError> {
        let [root, source, destination, overwrite] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let source = raw_text(source.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let destination =
            raw_text(destination.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let overwrite =
            raw_bool(overwrite.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        provider
            .move_file(root, source, destination, overwrite)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        Ok(OvbRaw::Null)
    }

    fn dispatch_http(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let admitted = match operation.name {
            HostOperation::StdNetHttpSend
            | HostOperation::StdNetHttpStart
            | HostOperation::StdNetHttpWait
            | HostOperation::StdNetHttpCancel => true,
            HostOperation::StdConcurrentSleep
            | HostOperation::StdIoEnvironmentGet
            | HostOperation::StdIoEnvironmentRequire
            | HostOperation::StdIoFsAppendText
            | HostOperation::StdIoFsCopyFile
            | HostOperation::StdIoFsCreateDir
            | HostOperation::StdIoFsExists
            | HostOperation::StdIoFsIsDirectory
            | HostOperation::StdIoFsList
            | HostOperation::StdIoFsMetadata
            | HostOperation::StdIoFsMoveFile
            | HostOperation::StdIoFsReadText
            | HostOperation::StdIoFsRemoveFile
            | HostOperation::StdIoFsSymlinkMetadata
            | HostOperation::StdIoFsWriteText
            | HostOperation::StdIoProcessRun
            | HostOperation::SysBlobCaptureFile => false,
        };
        if !admitted
            || operation.implementation != last_name_component(operation.name.label())
            || operation.role != HostOperation::StdNetHttpSend.role()
            || operation.provider != HostProvider::Http
            || operation.effects != [SysEffect::Invoke]
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let provider = self
            .http
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let raw = match operation.name {
            HostOperation::StdNetHttpSend => {
                let (method, url, headers, body, timeout, max_headers, max_body) =
                    parse_http_request_arguments(arguments)?;
                let response = provider
                    .send(
                        &method,
                        &url,
                        &headers,
                        body.as_deref(),
                        timeout,
                        max_headers,
                        max_body,
                    )
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                http_response_raw(response)
            }
            HostOperation::StdNetHttpStart => {
                let (method, url, headers, body, timeout, max_headers, max_body) =
                    parse_http_request_arguments(arguments)?;
                let handle = provider
                    .start(
                        &method,
                        &url,
                        &headers,
                        body.as_deref(),
                        timeout,
                        max_headers,
                        max_body,
                    )
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                Ok(OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(handle.to_vec()))))
            }
            HostOperation::StdNetHttpWait => {
                let [handle, timeout] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let handle =
                    raw_uuid(handle.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let timeout = raw_optional_duration(timeout.raw())?;
                let response = provider
                    .wait(handle, timeout)
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                http_response_raw(response)
            }
            HostOperation::StdNetHttpCancel => {
                let [handle] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let handle =
                    raw_uuid(handle.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                Ok(OvbRaw::Bool(provider.cancel(handle).map_err(
                    |failure| self.failure(operation, failure.code()),
                )?))
            }
            HostOperation::StdConcurrentSleep
            | HostOperation::StdIoEnvironmentGet
            | HostOperation::StdIoEnvironmentRequire
            | HostOperation::StdIoFsAppendText
            | HostOperation::StdIoFsCopyFile
            | HostOperation::StdIoFsCreateDir
            | HostOperation::StdIoFsExists
            | HostOperation::StdIoFsIsDirectory
            | HostOperation::StdIoFsList
            | HostOperation::StdIoFsMetadata
            | HostOperation::StdIoFsMoveFile
            | HostOperation::StdIoFsReadText
            | HostOperation::StdIoFsRemoveFile
            | HostOperation::StdIoFsSymlinkMetadata
            | HostOperation::StdIoFsWriteText
            | HostOperation::StdIoProcessRun
            | HostOperation::SysBlobCaptureFile => {
                return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
            }
        };
        CanonicalValue::new(raw?)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn dispatch_environment(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if arguments.len() != 1 {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        }
        let OvbRaw::Text(name) = arguments[0].raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        let value = self
            .environment
            .dispatch(operation, name)
            .map_err(|failure| self.failure(operation, failure.code()))?;
        let raw = match value {
            EnvironmentDispatchValue::Optional(value) => {
                let mut fields = vec![OvbRaw::Int(if value.is_some() {
                    1.into()
                } else {
                    0.into()
                })];
                if let Some(value) = value {
                    fields.push(OvbRaw::Text(value));
                }
                OvbRaw::Tag(60013, Box::new(OvbRaw::Array(fields)))
            }
            EnvironmentDispatchValue::Required(value) => OvbRaw::Text(value),
        };
        CanonicalValue::new(raw)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn dispatch_process(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if operation.name != HostOperation::StdIoProcessRun
            || operation.implementation != "run"
            || operation.role != HostOperation::StdIoProcessRun.role()
            || operation.provider != HostProvider::Process
            || operation.effects != [SysEffect::Invoke]
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let provider = self
            .process
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let request = process_request(arguments)?;
        let output = provider
            .run(&orna_sys::ProcessRequest {
                executable: request.executable,
                arguments: &request.args,
                working_directory: request.working_directory,
                environment: &request.environment,
                input: request.input,
                timeout: request.timeout,
                max_output_bytes: request.output_limit,
            })
            .map_err(|failure| self.failure(operation, failure.code()))?;
        let exit_status = process_exit_status(output.exit_status);
        CanonicalValue::new(OvbRaw::Tag(
            60015,
            Box::new(OvbRaw::Array(vec![
                exit_status,
                OvbRaw::Bytes(output.stdout),
                OvbRaw::Bytes(output.stderr),
            ])),
        ))
        .map(Some)
        .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn dispatch_clock(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if operation.name != HostOperation::StdConcurrentSleep
            || operation.implementation != "sleep"
            || operation.role != HostOperation::StdConcurrentSleep.role()
            || operation.provider != HostProvider::Clock
            || operation.effects != [SysEffect::Invoke]
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let provider = self
            .clock
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let [duration] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let OvbRaw::Tag(60005, raw) = duration.raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        let duration = raw_duration(raw).ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
        provider
            .sleep_with_cancellation(duration, || {
                cancellation.is_some_and(CancellationToken::is_requested)
            })
            .map_err(|failure| self.failure(operation, failure.code()))?;
        CanonicalValue::new(OvbRaw::Null)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn failure(&self, operation: &HostOperationDescriptor, code: &str) -> EvaluationError {
        let Some(code) = SysFailureCode::parse(code) else {
            return redacted_error("ORNA-EVAL-UNSUPPORTED");
        };
        let declares_failure = operation.failures.contains(&code);
        if declares_failure && code == SysFailureCode::HostClockCancelled {
            return redacted_error("ORNA-EVAL-CANCELLED");
        }
        if declares_failure {
            redacted_error("ORNA-EVAL-ERROR")
        } else {
            redacted_error("ORNA-EVAL-UNSUPPORTED")
        }
    }
}

fn raw_text(value: &OvbRaw) -> Option<&str> {
    match value {
        OvbRaw::Text(value) => Some(value),
        _ => None,
    }
}

fn raw_bool(value: &OvbRaw) -> Option<bool> {
    match value {
        OvbRaw::Bool(value) => Some(*value),
        _ => None,
    }
}

fn http_response_raw(response: HostHttpResponse) -> Result<OvbRaw, EvaluationError> {
    Ok(OvbRaw::Array(vec![
        OvbRaw::Int(response.status.into()),
        OvbRaw::Array(
            response
                .headers
                .into_iter()
                .map(|(name, value)| OvbRaw::Array(vec![OvbRaw::Text(name), OvbRaw::Text(value)]))
                .collect(),
        ),
        OvbRaw::Bytes(response.body),
    ]))
}

/// One parsed `sys.http` request: method, URL, header pairs, optional body,
/// optional timeout, and the header and body limits the call declared.
type ParsedHttpRequest = (
    String,
    String,
    Vec<(String, String)>,
    Option<Vec<u8>>,
    Option<std::time::Duration>,
    usize,
    usize,
);

fn parse_http_request_arguments(
    arguments: &[CanonicalValue],
) -> Result<ParsedHttpRequest, EvaluationError> {
    let [method, url, headers, body, timeout, max_headers, max_body] = arguments else {
        return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
    };
    let method = raw_text(method.raw())
        .ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?
        .to_owned();
    let url = raw_text(url.raw())
        .ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?
        .to_owned();
    let headers = raw_text_pairs(headers.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
    let body = match optional_raw(body.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))? {
        None | Some(OvbRaw::Null) => None,
        Some(OvbRaw::Bytes(bytes)) => Some(bytes.clone()),
        Some(_) => return Err(redacted_error("ORNA-EVAL-TYPE")),
    };
    let timeout = raw_optional_duration(timeout.raw())?;
    let OvbRaw::Int(max_headers) = max_headers.raw() else {
        return Err(redacted_error("ORNA-EVAL-TYPE"));
    };
    let OvbRaw::Int(max_body) = max_body.raw() else {
        return Err(redacted_error("ORNA-EVAL-TYPE"));
    };
    let max_headers = max_headers
        .to_usize()
        .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
    let max_body = max_body
        .to_usize()
        .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
    Ok((method, url, headers, body, timeout, max_headers, max_body))
}

fn raw_uuid(value: &OvbRaw) -> Option<[u8; 16]> {
    let OvbRaw::Tag(37, value) = value else {
        return None;
    };
    let OvbRaw::Bytes(bytes) = value.as_ref() else {
        return None;
    };
    bytes.as_slice().try_into().ok()
}

fn raw_optional_duration(value: &OvbRaw) -> Result<Option<std::time::Duration>, EvaluationError> {
    match optional_raw(value).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))? {
        None | Some(OvbRaw::Null) => Ok(None),
        Some(OvbRaw::Tag(60005, value)) => raw_duration(value)
            .map(Some)
            .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE")),
        Some(_) => Err(redacted_error("ORNA-EVAL-TYPE")),
    }
}

fn optional_value(value: Option<OvbRaw>) -> OvbRaw {
    let mut fields = vec![OvbRaw::Int(if value.is_some() {
        1.into()
    } else {
        0.into()
    })];
    if let Some(value) = value {
        fields.push(value);
    }
    OvbRaw::Tag(60013, Box::new(OvbRaw::Array(fields)))
}

fn system_time_raw(value: std::time::SystemTime) -> Option<OvbRaw> {
    let (seconds, nanoseconds) = match value.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => (
            i64::try_from(duration.as_secs()).ok()?,
            duration.subsec_nanos(),
        ),
        Err(error) => {
            let duration = error.duration();
            let seconds = i64::try_from(duration.as_secs()).ok()?;
            if duration.subsec_nanos() == 0 {
                (-seconds, 0)
            } else {
                (-seconds - 1, 1_000_000_000 - duration.subsec_nanos())
            }
        }
    };
    Some(OvbRaw::Tag(
        60002,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Int(seconds.into()),
            OvbRaw::Int(nanoseconds.into()),
        ])),
    ))
}

fn raw_text_array(value: &OvbRaw) -> Option<Vec<String>> {
    let OvbRaw::Array(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| raw_text(value).map(str::to_owned))
        .collect()
}

fn raw_text_pairs(value: &OvbRaw) -> Option<Vec<(String, String)>> {
    let OvbRaw::Array(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| {
            let OvbRaw::Tag(60015, pair) = value else {
                return None;
            };
            let OvbRaw::Array(pair) = pair.as_ref() else {
                return None;
            };
            let [name, value] = pair.as_slice() else {
                return None;
            };
            Some((raw_text(name)?.to_owned(), raw_text(value)?.to_owned()))
        })
        .collect()
}

fn optional_raw(value: &OvbRaw) -> Option<Option<&OvbRaw>> {
    match value {
        OvbRaw::Null => Some(None),
        OvbRaw::Tag(60013, raw) => {
            let OvbRaw::Array(parts) = raw.as_ref() else {
                return None;
            };
            match parts.as_slice() {
                [OvbRaw::Int(tag)] if tag == &0.into() => Some(None),
                [OvbRaw::Int(tag), value] if tag == &1.into() => Some(Some(value)),
                _ => None,
            }
        }
        _ => Some(Some(value)),
    }
}

fn raw_duration(raw: &OvbRaw) -> Option<std::time::Duration> {
    let OvbRaw::Array(parts) = raw else {
        return None;
    };
    let [OvbRaw::Int(seconds), OvbRaw::Int(nanoseconds)] = parts.as_slice() else {
        return None;
    };
    let nanoseconds = nanoseconds
        .to_u32()
        .filter(|nanoseconds| *nanoseconds < 1_000_000_000)?;
    Some(std::time::Duration::new(seconds.to_u64()?, nanoseconds))
}

impl EffectHandler for SysHostBindingRegistry {
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        self.sys_host_context.as_ref()
    }

    fn handle(
        &mut self,
        _callee: &Expr,
        _arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        _callee: &Expr,
        arguments: &[CanonicalValue],
        _budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Some(operation) = sys_dispatch::registered_operation(operation) else {
            return Ok(None);
        };
        self.dispatch(operation, arguments, None)
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        _callee: &Expr,
        arguments: &[CanonicalValue],
        _budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Some(operation) = sys_dispatch::registered_operation(operation) else {
            return Ok(None);
        };
        self.dispatch(operation, arguments, cancellation)
    }

    fn fork_task_child(&mut self, _child_index: usize) -> Option<Box<dyn EffectHandler + Send>> {
        Some(Box::new(self.clone()))
    }
}

impl Ovb2EffectHandler for SysHostBindingRegistry {
    /// Captures return lazy ROV-3 values in the repository capture context;
    /// every other value keeps the OVB-2 profile.
    fn accepts_context_values(&self) -> bool {
        true
    }

    #[cfg(feature = "system-host-bindings")]
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        self.sys_host_context.as_ref()
    }

    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let Some(function) = static_call_name(callee)
            .as_deref()
            .and_then(sys_dispatch::sys_function)
        else {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        };
        if function != SysFunction::BlobCaptureFile {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        self.capture_file_ovb2(function, arguments, None)
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        Ovb2EffectHandler::handle_registered_with_cancellation_and_budget(
            self, operation, callee, arguments, budget, None,
        )
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation_name: &str,
        _callee: &Expr,
        arguments: &[ContextValue],
        _budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let Some(function) = sys_dispatch::sys_function(operation_name) else {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        };
        self.capture_file_ovb2(function, arguments, cancellation)
    }
}

impl SysHostBindingRegistry {
    fn capture_file_ovb2(
        &self,
        function: SysFunction,
        arguments: &[ContextValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        if !matches!(self.binding(function), SysFunctionBinding::Implemented)
            || function != SysFunction::BlobCaptureFile
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        sys_dispatch::host_descriptor(HostOperation::SysBlobCaptureFile)
            .filter(|operation| {
                operation.function == Some(function)
                    && operation.implementation == "capture_file"
                    && operation.role == HostOperation::SysBlobCaptureFile.role()
                    && operation.provider == HostProvider::Filesystem
                    && operation.effects == [SysEffect::Invoke]
            })
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let [root, path, max_bytes] = arguments else {
            return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
        };
        let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let OvbRaw::Int(max_bytes) = max_bytes.raw() else {
            return Err(redacted_error("ORNA-EVAL-TYPE"));
        };
        // A negative or unrepresentable byte bound is an out-of-range length.
        let max_bytes = max_bytes
            .to_u64()
            .ok_or_else(|| capture_file_failure(CaptureFileError::BoundOutOfRange))?;
        if cancellation.is_some_and(CancellationToken::is_requested) {
            return Err(redacted_error("ORNA-EVAL-CANCELLED"));
        }
        // A host without a filesystem provider authorises no root, so the
        // selected source is not permitted (ORNA-INGEST-002).
        let filesystem = self
            .filesystem
            .as_ref()
            .ok_or_else(|| capture_file_failure(CaptureFileError::UnauthorizedRoot))?;
        // Without a repository Blob writer the host does not offer capture, and
        // the call fails before any write (ORNA-SYS-008).
        let repository = self
            .repository_capture
            .as_ref()
            .ok_or_else(|| sys_failure_error(SysFailureCode::VersionIncompatible))?;
        let mut source = filesystem
            .capture_file(root, path, max_bytes)
            .map_err(capture_file_failure)?;
        // The stream already holds the source within `max_bytes` and the
        // provider limit, so its observed length is the writer's ceiling;
        // growth past it is reported below as a changed source.
        let length = source.length();
        let candidate = repository.capture_blob_candidate(&mut source, length);
        source.verify_unchanged().map_err(capture_file_failure)?;
        if source.exceeded() {
            return Err(capture_file_failure(CaptureFileError::LimitExceeded));
        }
        let candidate = candidate.map_err(|error| {
            capture_file_failure(match error {
                GraphError::ReadQuotaExceeded | GraphError::InvalidLength(_) => {
                    CaptureFileError::LimitExceeded
                }
                _ => CaptureFileError::SourceUnavailable,
            })
        })?;
        if cancellation.is_some_and(CancellationToken::is_requested) {
            return Err(redacted_error("ORNA-EVAL-CANCELLED"));
        }
        // Capture is one of the old constructors: it supplies the
        // octet-stream/null annotation whatever the file is called, and an
        // importer declares the media type afterwards with `sys.blob.annotate`
        // (ORNA-MIME-001). The value names the candidate's descriptor in the
        // capture context, so no captured byte is held in memory
        // (ORNA-BLOB-007; store-format-3 ROV-3).
        let value = repository
            .candidate_value(&candidate)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?;
        let blob = value
            .blob()
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?;
        self.capture_candidates
            .lock()
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?
            .push(CapturedBlobForRow {
                candidate,
                media_type: blob.media_type().to_owned(),
                suffix: blob.suffix().map(str::to_owned),
            });
        Ok(Some(value))
    }
}

/// Generated semantic role that owns the portable annotated-Blob operations.
const BLOB_ANNOTATION_ROLE: &str = "langitem.sys.blob.annotate";

/// Generated semantic contract consumed by the native Blob annotation binding.
const BLOB_ANNOTATION_CONTRACT: &str = "annotated-blob-format-3";

/// True when `function` is the generated portable Blob annotation operation.
///
/// This is only an admission filter: parameter names, order, declared types,
/// defaults, the result type and the declared failure vocabulary all come from
/// the build-generated typed registry, never from this module.
pub(crate) fn is_blob_annotation_operation(function: SysFunction) -> bool {
    function == SysFunction::BlobAnnotate
}

/// True when `function` is a metadata or content reflection over a Blob value
/// (`sys.blob.media_type`, `sys.blob.suffix`, `sys.blob.same_content`).
///
/// These declare no role: the generated `annotated-blob-format-3` contract
/// names them and the evaluator answers them natively from the Blob value.
pub(crate) fn is_blob_reflection_operation(function: SysFunction) -> bool {
    matches!(
        function,
        SysFunction::BlobMediaType | SysFunction::BlobSuffix | SysFunction::BlobSameContent
    )
}

/// True when the evaluator dispatches `function` natively: a Blob annotation,
/// a Blob reflection, a read-side Blob operation, or a Blob construction from
/// bytes.
pub(crate) fn is_blob_host_operation(function: SysFunction) -> bool {
    is_blob_annotation_operation(function)
        || is_blob_reflection_operation(function)
        || is_blob_read_operation(function)
        || is_blob_from_bytes_operation(function)
}

/// Answers a generated Blob reflection call from the value's own annotation.
///
/// `media_type` and `suffix` read only the canonical annotation without
/// hydrating payload bytes. `same_content` compares exact bytes under the
/// content rules and may stream them, so an unreadable side fails with its
/// portable `sys.blob.*` code. Arity is fixed by the reflection contract
/// declared for each operation, not by this dispatcher.
pub(crate) fn dispatch_blob_reflection(
    function: SysFunction,
    arguments: &[ContextValue],
    cancellation: Option<&CancellationToken>,
) -> Result<Value, EvaluationError> {
    if !is_blob_reflection_operation(function) {
        return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
    }
    let contract = orna_sys::system_dispatch_table()
        .operation(function.label())
        .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
    if contract.role.is_some() || contract.contract.as_deref() != Some(BLOB_ANNOTATION_CONTRACT) {
        return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
    }
    match (function, arguments) {
        (SysFunction::BlobMediaType, [value]) => {
            let blob = value.blob().map_err(blob_reflection_failure)?;
            Ok(Value::String(blob.media_type().to_owned()))
        }
        (SysFunction::BlobSuffix, [value]) => {
            let blob = value.blob().map_err(blob_reflection_failure)?;
            Ok(Value::Option(
                blob.suffix()
                    .map(|suffix| Box::new(Value::String(suffix.to_owned()))),
            ))
        }
        (SysFunction::BlobSameContent, [left, right]) => {
            let left = left.blob().map_err(blob_reflection_failure)?;
            let right = right.blob().map_err(blob_reflection_failure)?;
            left.same_content_checked(&right, || {
                if cancellation.is_some_and(CancellationToken::is_requested) {
                    Err(ValueError::Cancelled)
                } else {
                    Ok(())
                }
            })
            .map(Value::Bool)
            .map_err(blob_reflection_failure)
        }
        _ => Err(redacted_error("ORNA-EVAL-ARGUMENT")),
    }
}

/// Maps a Blob read failure to the portable code the reflection contract
/// declares.
fn blob_reflection_failure(error: ValueError) -> EvaluationError {
    match error {
        ValueError::ContentUnavailable => sys_failure_error(SysFailureCode::BlobUnavailable),
        ValueError::OwnerExpired | ValueError::MissingDescriptor => {
            sys_failure_error(SysFailureCode::BlobUnavailable)
        }
        ValueError::ContentDigestMismatch => sys_failure_error(SysFailureCode::BlobCorrupt),
        ValueError::Cancelled => redacted_error("ORNA-EVAL-CANCELLED"),
        ValueError::ContentCollision => sys_failure_error(SysFailureCode::BlobCollision),
        ValueError::InvalidContext => sys_failure_error(SysFailureCode::BlobReferenceContext),
        _ => redacted_error("ORNA-EVAL-VALUE"),
    }
}

/// Rewraps a registry-admitted failure code as an evaluator error.
///
/// The code already crossed `validate_failure`, so it is a generated qualified
/// identifier; the fallback only guards the evaluator's own text admission.
fn redacted_code(code: SysFailureCode) -> EvaluationError {
    sys_failure_error(code)
}

fn static_failure(code: SysFailureCode) -> FailureCode {
    FailureCode::new(code)
}

/// Resolves one portable Blob operation from the generated typed registry.
///
/// Admission requires the generated `annotated-blob-format-3` contract and its
/// binding to the portable Blob annotation role, so dispatch follows the same
/// semantic metadata that emits the language declaration.
fn blob_annotation_contract(
    function: SysFunction,
) -> Result<&'static OperationContract, EvaluationError> {
    if !is_blob_annotation_operation(function) {
        return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
    }
    let contract = orna_sys::system_dispatch_table()
        .operation(function.label())
        .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
    if contract.role.as_ref().map(|role| role.as_str()) != Some(BLOB_ANNOTATION_ROLE)
        || contract.contract.as_deref() != Some(BLOB_ANNOTATION_CONTRACT)
    {
        return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
    }
    Ok(contract)
}

/// Native provider linked to the generated portable Blob annotation role.
///
/// The provider keeps the evaluator's validated OVB-2 or Format-3 source
/// alongside the typed invocation. It applies the MIME-1 metadata edit with
/// `ContextValue::with_blob_annotation`, preserving raw identity, the exact
/// resolution context, and capture provenance without reading Blob bytes.
/// This pure language operation needs no host-operation effect grant.
struct BlobAnnotationProvider<'a> {
    offer: ProviderOffer,
    function: SysFunction,
    source: &'a ContextValue,
}

impl<'a> BlobAnnotationProvider<'a> {
    /// Links a provider offer to the generated role and source Blob.
    fn linked(
        role: &SemanticRoleContract,
        function: SysFunction,
        source: &'a ContextValue,
    ) -> Result<Self, EvaluationError> {
        if !matches!(
            source.format(),
            ValueFormat::Ovb2 | ValueFormat::Rov3 | ValueFormat::Sov3
        ) {
            return Err(sys_failure_error(
                SysFailureCode::CodecUnsupportedValueProfile,
            ));
        }
        Ok(Self {
            offer: ProviderOffer {
                provider: role
                    .builtin_provider
                    .clone()
                    .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?,
                role: role.id.clone(),
                version: role.version,
                effects: role.effects.clone(),
            },
            function,
            source,
        })
    }
}

impl SystemOperationProvider for BlobAnnotationProvider<'_> {
    fn offer(&self) -> &ProviderOffer {
        &self.offer
    }

    fn invoke(
        &self,
        operation: &OperationId,
        arguments: &[TypedValue],
    ) -> Result<TypedValue, ProviderFailure> {
        let table = orna_sys::system_dispatch_table();
        let Some(function) = sys_dispatch::sys_function(operation.as_str()) else {
            return Err(ProviderFailure {
                code: static_failure(SysFailureCode::AbiUnavailable),
                payload: None,
            });
        };
        if function != self.function {
            return Err(ProviderFailure {
                code: static_failure(SysFailureCode::AbiUnavailable),
                payload: None,
            });
        }
        let Some(contract) = table.operation(self.function.label()) else {
            return Err(ProviderFailure {
                code: static_failure(SysFailureCode::AbiUnavailable),
                payload: None,
            });
        };
        let [_, media_type, suffix] = arguments else {
            return Err(provider_failure());
        };
        let decode_argument = |argument: &TypedValue| {
            let Some(canonical) = argument.canonical() else {
                return Err(provider_failure());
            };
            ContextValue::decode(canonical, ValueFormat::Ovb2).map_err(|_| provider_failure())
        };
        let media_type = decode_argument(media_type)?;
        let OvbRaw::Text(media_type) = media_type.raw() else {
            return Err(provider_failure());
        };
        let suffix = decode_argument(suffix)?;
        let annotated = apply_blob_annotation(function, self.source, media_type, suffix.raw())
            .map_err(|code| ProviderFailure {
                code,
                payload: None,
            })?;
        let canonical = annotated.encode().map_err(|_| provider_failure())?;
        Ok(TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            canonical,
        ))
    }
}

/// Applies the portable MIME-1 annotation without reconstructing Blob bytes.
fn apply_blob_annotation(
    function: SysFunction,
    source: &ContextValue,
    media_type: &str,
    suffix: &OvbRaw,
) -> Result<ContextValue, FailureCode> {
    if !is_blob_annotation_operation(function) {
        return Err(static_failure(SysFailureCode::AbiUnavailable));
    }
    let suffix = blob_suffix_argument(suffix)?;
    source
        .with_blob_annotation(media_type, suffix)
        .map_err(blob_value_failure)
}

fn provider_failure() -> ProviderFailure {
    ProviderFailure {
        code: static_failure(SysFailureCode::AbiProviderFailed),
        payload: None,
    }
}

/// Dispatches one portable annotated-Blob operation through the typed registry.
///
/// Parameter count, order, declared types and the result type are checked
/// against the generated contract, and the provider offer is re-validated
/// against the same generated role before the value-level result is admitted.
/// `arguments` is positional and complete: the caller resolves declared
/// defaults before calling.
pub(crate) fn dispatch_blob_annotation(
    function: SysFunction,
    arguments: &[ContextValue],
) -> Result<ContextValue, EvaluationError> {
    let table = orna_sys::system_dispatch_table();
    let contract = blob_annotation_contract(function)?;
    let parameters = &contract.signature.parameters;
    if arguments.len() != parameters.len() {
        return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
    }
    let role = table
        .role(BLOB_ANNOTATION_ROLE)
        .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
    let source = arguments
        .first()
        .ok_or_else(|| redacted_error("ORNA-EVAL-ARGUMENT"))?;
    let provider = BlobAnnotationProvider::linked(role, function, source)?;
    let mut typed = Vec::with_capacity(parameters.len());
    for (parameter, argument) in parameters.iter().zip(arguments) {
        let canonical = if matches!(
            &parameter.ty,
            orna_sys::AbiType::Named(name) if name == "Blob"
        ) {
            argument.encode()
        } else {
            ContextValue::new(ValueFormat::Ovb2, argument.raw().clone())
                .and_then(|value| value.encode())
        }
        .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?;
        // The ABI admits an optional argument under its full declared optional
        // type, while the canonical payload carries the null itself, so an
        // absent optional stays distinguishable from a present inner value.
        typed.push(TypedValue::public(
            TypeId::new(parameter.ty.canonical()),
            canonical,
        ));
    }
    match table.dispatch_to_provider(function.label(), &provider, &typed, |_| Ok(())) {
        Ok(orna_sys::SystemDispatchResult::Returned(value)) => {
            let canonical = value
                .canonical()
                .ok_or_else(|| redacted_error("ORNA-EVAL-VALUE"))?;
            let decoded = match source.context() {
                Some(context) => {
                    ContextValue::decode_in_context(canonical, source.format(), context)
                }
                None => ContextValue::decode(canonical, source.format()),
            };
            decoded.map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
        }
        Ok(orna_sys::SystemDispatchResult::Failed(code)) => Err(blob_annotation_failure(&code)),
        Err(ProviderDiagnostic::PreconditionFailed { code, .. }) => {
            Err(blob_annotation_failure(&code))
        }
        // The host does not link the generated provider for this operation:
        // the required coordinate is unavailable (ORNA-SYS-008).
        Err(_) => Err(sys_failure_error(SysFailureCode::VersionIncompatible)),
    }
}

/// Largest byte count one read-side call may return or materialise (ORNA-BLOB-011).
pub(crate) const MAX_BLOB_CALL_BYTES: u64 = 8 * 1024 * 1024;

/// True when `function` is one of the portable read-side Blob operations.
///
/// Length, digest, bounded reads and full verification use only the Blob value's
/// own identity and content, so they need no host effect grant and never reach
/// native I/O.
pub(crate) fn is_blob_read_operation(function: SysFunction) -> bool {
    matches!(
        function,
        SysFunction::BlobLength
            | SysFunction::BlobDigest
            | SysFunction::BlobRead
            | SysFunction::BlobVerify
            | SysFunction::BlobToBytes
    )
}

/// True when `function` is the portable Blob constructor from integer bytes.
pub(crate) fn is_blob_from_bytes_operation(function: SysFunction) -> bool {
    function == SysFunction::BlobFromBytes
}

/// Builds the Blob of `sys.blob.from_bytes` from integers in `0..=255`. The
/// integer list is checked against the 8 MiB call limit before any byte is
/// copied, and a value outside the byte range is `sys.blob.bounds`.
pub(crate) fn dispatch_blob_from_bytes(values: &[BigInt]) -> Result<ContextValue, EvaluationError> {
    if values.len() as u64 > MAX_BLOB_CALL_BYTES {
        return Err(sys_failure_error(SysFailureCode::BlobLimit));
    }
    let bytes = values
        .iter()
        .map(|value| {
            value
                .to_u8()
                .ok_or_else(|| sys_failure_error(SysFailureCode::BlobBounds))
        })
        .collect::<Result<Vec<u8>, _>>()?;
    ContextValue::from_blob(&Blob::from_bytes(bytes), ValueFormat::Ovb2).map_err(blob_read_failure)
}

/// Typed result of one read-side Blob call, before evaluator value conversion.
pub(crate) enum BlobReadValue {
    Int(u64),
    Blob(ContextValue),
    Bytes(Vec<u8>),
    Digest([u8; 32]),
    Unit,
}

/// Maps an `orna_value` read failure onto the portable failures declared by
/// the read-side `sys.blob.*` descriptors.
fn blob_read_failure(error: ValueError) -> EvaluationError {
    if error == ValueError::Cancelled {
        return redacted_error("ORNA-EVAL-CANCELLED");
    }
    let code = match error {
        ValueError::InvalidContext => SysFailureCode::BlobReferenceContext,
        ValueError::ContentUnavailable
        | ValueError::OwnerExpired
        | ValueError::MissingDescriptor => SysFailureCode::BlobUnavailable,
        ValueError::Cancelled => unreachable!("handled above"),
        ValueError::ContentCollision => SysFailureCode::BlobCollision,
        ValueError::InvalidRange => SysFailureCode::BlobBounds,
        ValueError::Limit | ValueError::QuotaExceeded => SysFailureCode::BlobLimit,
        ValueError::InvalidProfile => SysFailureCode::CodecUnsupportedValueProfile,
        ValueError::Truncated
        | ValueError::TrailingBytes
        | ValueError::Unsupported
        | ValueError::NonCanonical
        | ValueError::InvalidUtf8
        | ValueError::InvalidTag
        | ValueError::InvalidValue
        | ValueError::DuplicateOrUnorderedMapKey
        | ValueError::ProtectedValue
        | ValueError::InvalidPath
        | ValueError::InvalidSchema
        | ValueError::DecimalLimit
        | ValueError::DivisionByZero
        | ValueError::NonFiniteDecimal
        | ValueError::InvalidMediaType
        | ValueError::InvalidSuffix
        | ValueError::IncompatibleSuffix
        | ValueError::InvalidOid
        | ValueError::ContentDigestMismatch
        | ValueError::RoleMismatch { .. }
        | ValueError::SinkFailed => SysFailureCode::BlobCorrupt,
    };
    sys_failure_error(code)
}

/// Dispatches one portable read-side Blob operation. `bounds` holds the `Int`
/// arguments of `sys.blob.read` (offset, length) in declared order and is
/// empty for every other operation; the caller has checked arity and types.
pub(crate) fn dispatch_blob_read(
    function: SysFunction,
    source: &ContextValue,
    bounds: &[BigInt],
) -> Result<BlobReadValue, EvaluationError> {
    let blob = source.blob().map_err(blob_read_failure)?;
    match (function, bounds) {
        (SysFunction::BlobLength, []) => Ok(BlobReadValue::Int(blob.length())),
        (SysFunction::BlobDigest, []) => {
            Ok(BlobReadValue::Digest(blob.content_identity().sha256()))
        }
        // Verifies raw content only. Orna-1.1.0 34-system-reference.md:3312-3314
        // also requires the native dependency structure (ORNA-GRAPH-002,
        // ORNA-OGB-004); that check is not implemented (ornadb-5il1.120), so this
        // arm is narrower than the documented contract.
        (SysFunction::BlobVerify, []) => {
            blob.verify().map_err(blob_read_failure)?;
            Ok(BlobReadValue::Unit)
        }
        (SysFunction::BlobToBytes, []) => {
            if blob.length() > MAX_BLOB_CALL_BYTES {
                return Err(sys_failure_error(SysFailureCode::BlobLimit));
            }
            blob.read_to_end()
                .map(BlobReadValue::Bytes)
                .map_err(blob_read_failure)
        }
        (SysFunction::BlobRead, [offset, length]) => {
            // Negative or unrepresentable bounds, a start past EOF and checked
            // overflow are bounds failures before any byte is read (ORNA-BLOB-011).
            let offset = offset
                .to_u64()
                .ok_or_else(|| sys_failure_error(SysFailureCode::BlobBounds))?;
            let length = length
                .to_u64()
                .ok_or_else(|| sys_failure_error(SysFailureCode::BlobBounds))?;
            if offset > blob.length() || offset.checked_add(length).is_none() {
                return Err(sys_failure_error(SysFailureCode::BlobBounds));
            }
            if length > MAX_BLOB_CALL_BYTES {
                return Err(sys_failure_error(SysFailureCode::BlobLimit));
            }
            let bytes = blob.read(offset, length).map_err(blob_read_failure)?;
            ContextValue::from_blob(&Blob::from_bytes(bytes), ValueFormat::Ovb2)
                .map(BlobReadValue::Blob)
                .map_err(blob_read_failure)
        }
        _ => Err(redacted_error("ORNA-EVAL-ARGUMENT")),
    }
}

/// A declared portable code is the call's primary failure (ORNA-SYS-097). The
/// shared `sys.abi.*` provider-boundary codes never surface: the provider
/// reports them only for arguments that are not the typed values its contract
/// declares, which the ordinary argument type diagnostic covers.
fn blob_annotation_failure(code: &FailureCode) -> EvaluationError {
    if code.is_provider_abi() {
        redacted_error("ORNA-EVAL-TYPE")
    } else {
        redacted_code(code.code())
    }
}

/// Reads the `suffix: Str?` argument of `sys.blob.annotate`.
fn blob_suffix_argument(raw: &OvbRaw) -> Result<Option<&str>, FailureCode> {
    match optional_raw(raw) {
        Some(None) => Ok(None),
        Some(Some(OvbRaw::Text(text))) => Ok(Some(text.as_str())),
        _ => Err(static_failure(SysFailureCode::AbiProviderFailed)),
    }
}

/// Maps an `orna_value` annotation failure onto the declared portable
/// failures of `sys.blob.annotate` (MIME-1 declaration codes, and the global
/// value-profile code for an OVB-1 value without an annotation slot).
///
/// Any other value error means the argument was not an annotated Blob value,
/// which the shared provider-boundary code reports.
fn blob_value_failure(error: ValueError) -> FailureCode {
    static_failure(match error {
        ValueError::InvalidMediaType => SysFailureCode::BlobInvalidMediaType,
        ValueError::InvalidSuffix => SysFailureCode::BlobInvalidSuffix,
        ValueError::IncompatibleSuffix => SysFailureCode::BlobIncompatibleSuffix,
        ValueError::InvalidProfile => SysFailureCode::CodecUnsupportedValueProfile,
        _ => SysFailureCode::AbiProviderFailed,
    })
}

fn static_call_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", static_call_name(base)?, name)),
        Expr::Group { inner, .. } => static_call_name(inner),
        _ => None,
    }
}

/// The last dotted component of a generated operation label.
fn last_name_component(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or("")
}

/// Surfaces a capture failure with its portable code (ORNA-SYS-097); the
/// generated host operation declares exactly these codes.
fn capture_file_failure(failure: CaptureFileError) -> EvaluationError {
    redacted_error(failure.failure_code())
}

fn redacted_error(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static evaluator error code is safe"))
}

fn sys_failure_error(code: SysFailureCode) -> EvaluationError {
    redacted_error(code.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_sys::system_host_operation_registry;
    use orna_value::Blob;

    #[test]
    fn blob_read_and_annotate_report_the_same_profile_code() {
        // The export refusal (`ValueError::InvalidProfile`) must surface as the
        // declared sys code on every Blob path, not as `sys.blob.corrupt`.
        let read = blob_read_failure(ValueError::InvalidProfile);
        let annotate = blob_value_failure(ValueError::InvalidProfile);
        assert_eq!(read.code(), "sys.codec.unsupported_value_profile");
        assert_eq!(annotate.as_str(), "sys.codec.unsupported_value_profile");
    }

    #[test]
    fn every_catalogued_callable_is_classified() {
        let registry = SysHostBindingRegistry::default();
        let mut implemented = Vec::new();
        for function in SysFunction::ALL {
            match registry.binding(function) {
                SysFunctionBinding::Implemented => implemented.push(function),
                SysFunctionBinding::Unavailable(Origin::Portable) => {}
                SysFunctionBinding::Unavailable(origin) => {
                    panic!("{} has non-portable origin {origin:?}", function.label());
                }
            }
        }
        assert_eq!(
            implemented,
            [
                SysFunction::ExplainQuery,
                SysFunction::ExplainFunctionRef,
                SysFunction::ExplainDiagnostic,
                SysFunction::RtInfo,
                SysFunction::BlobLength,
                SysFunction::BlobDigest,
                SysFunction::BlobRead,
                SysFunction::BlobVerify,
                SysFunction::BlobFromBytes,
                SysFunction::BlobToBytes,
                SysFunction::BlobCaptureFile,
                SysFunction::BlobMediaType,
                SysFunction::BlobSuffix,
                SysFunction::BlobAnnotate,
                SysFunction::BlobSameContent,
            ],
            "the evaluator's native callable set changed without classification"
        );
    }

    #[test]
    fn every_generated_host_operation_reaches_its_native_dispatch_arm() {
        let environment = EnvironmentProvider::from_snapshot([(
            "HOME".to_owned(),
            Some("fixture-home".to_owned()),
        )])
        .expect("valid host environment snapshot");
        let process = ProcessProvider::new(std::time::Duration::from_secs(1), 1024)
            .expect("valid process limits");
        let clock = ClockProvider::new(std::time::Duration::from_secs(1));
        let filesystem = FilesystemProvider::new();
        let http = HttpProvider::new(std::time::Duration::from_secs(1), 1024, 1024)
            .expect("valid HTTP limits");
        let bindings = SysHostBindingRegistry::new(environment)
            .with_process_provider(process)
            .with_clock_provider(clock)
            .with_filesystem_provider(filesystem)
            .with_http_provider(http);
        let environment_argument = CanonicalValue::new(OvbRaw::Text("HOME".to_owned()))
            .expect("environment name is canonical");
        let mut checked = 0;

        for operation in system_host_operation_registry().operations() {
            checked += 1;
            if operation.name == HostOperation::SysBlobCaptureFile {
                assert_eq!(
                    bindings
                        .dispatch(RegisteredOperation::Host(operation.name), &[], None,)
                        .expect_err("Blob capture has no OVB-1 fallback")
                        .code(),
                    "ORNA-EVAL-UNSUPPORTED"
                );
                continue;
            }
            if operation.provider == HostProvider::Environment {
                let actual = bindings
                    .dispatch(
                        RegisteredOperation::Host(operation.name),
                        std::slice::from_ref(&environment_argument),
                        None,
                    )
                    .unwrap_or_else(|error| {
                        panic!("{} dispatch failed: {}", operation.name, error.code())
                    })
                    .expect("environment operation returns a value");
                let expected = match operation.name {
                    HostOperation::StdIoEnvironmentGet => OvbRaw::Tag(
                        60013,
                        Box::new(OvbRaw::Array(vec![
                            OvbRaw::Int(1.into()),
                            OvbRaw::Text("fixture-home".to_owned()),
                        ])),
                    ),
                    HostOperation::StdIoEnvironmentRequire => {
                        OvbRaw::Text("fixture-home".to_owned())
                    }
                    name => panic!("unmapped environment operation {name}"),
                };
                assert_eq!(actual.raw(), &expected, "{} result", operation.name);
            } else {
                let error = bindings
                    .dispatch(RegisteredOperation::Host(operation.name), &[], None)
                    .expect_err("empty args must reach a registered handler and fail arity");
                assert_eq!(
                    error.code(),
                    "ORNA-EVAL-ARGUMENT",
                    "{} must resolve to its typed native dispatch arm",
                    operation.name
                );
            }
        }

        assert_eq!(
            checked, 21,
            "the host dispatch audit covers the full registry"
        );
    }

    #[test]
    fn capture_file_declares_only_its_generated_failures() {
        let operation = system_host_operation_registry()
            .operation("sys.blob.capture_file")
            .expect("capture_file is present in the generated host ABI");
        assert_eq!(
            operation
                .failures
                .iter()
                .map(|failure| failure.as_str())
                .collect::<Vec<_>>(),
            [
                "sys.blob.source_denied",
                "sys.blob.bounds",
                "sys.blob.limit",
                "sys.blob.source_changed",
            ]
        );
        // Every capture outcome surfaces a code the operation declares.
        for failure in [
            CaptureFileError::UnauthorizedRoot,
            CaptureFileError::InvalidPath,
            CaptureFileError::SourceUnavailable,
            CaptureFileError::LimitExceeded,
            CaptureFileError::SourceChanged,
            CaptureFileError::BoundOutOfRange,
        ] {
            assert!(
                operation
                    .failures
                    .iter()
                    .any(|declared| declared.as_str() == failure.failure_code()),
                "{failure:?}"
            );
            assert_eq!(capture_file_failure(failure).code(), failure.failure_code());
        }
        // The provider takes a `u64` bound. A bound the binding cannot narrow
        // to it is `sys.blob.bounds`, which the portable operation declares.
        let portable = orna_sys::system_dispatch_table()
            .operation("sys.blob.capture_file")
            .expect("capture_file is a portable sys operation");
        assert!(
            portable
                .failures
                .iter()
                .any(|declared| declared.as_str() == "sys.blob.bounds")
        );
        let negative_bound = [
            ContextValue::new(ValueFormat::Ovb2, OvbRaw::Text("root".to_owned())).unwrap(),
            ContextValue::new(ValueFormat::Ovb2, OvbRaw::Text("file.bin".to_owned())).unwrap(),
            ContextValue::new(ValueFormat::Ovb2, OvbRaw::Int((-1).into())).unwrap(),
        ];
        assert_eq!(
            SysHostBindingRegistry::default()
                .capture_file_ovb2(SysFunction::BlobCaptureFile, &negative_bound, None)
                .expect_err("a negative byte bound is out of range")
                .code(),
            "sys.blob.bounds"
        );
        assert!(
            operation
                .failures
                .iter()
                .any(|failure| failure.as_str() == "sys.blob.bounds"),
            "capture_file must declare the negative-bound failure"
        );
    }

    /// ORNA-INGEST-002 and ORNA-SYS-008: a host that lacks part of the capture
    /// capability reports a portable code, never an evaluator code.
    #[test]
    fn capture_file_without_host_capability_reports_portable_codes() {
        let arguments = [
            ContextValue::new(ValueFormat::Ovb2, OvbRaw::Text("root".to_owned())).unwrap(),
            ContextValue::new(ValueFormat::Ovb2, OvbRaw::Text("file.bin".to_owned())).unwrap(),
            ContextValue::new(ValueFormat::Ovb2, OvbRaw::Int(16.into())).unwrap(),
        ];
        let without_filesystem = SysHostBindingRegistry::default();
        assert_eq!(
            without_filesystem
                .capture_file_ovb2(SysFunction::BlobCaptureFile, &arguments, None)
                .expect_err("a host without a filesystem provider authorises no root")
                .code(),
            "sys.blob.source_denied"
        );
        let without_writer =
            SysHostBindingRegistry::default().with_filesystem_provider(FilesystemProvider::new());
        assert_eq!(
            without_writer
                .capture_file_ovb2(SysFunction::BlobCaptureFile, &arguments, None)
                .expect_err("a host without a repository Blob writer cannot capture")
                .code(),
            "sys.version.incompatible"
        );
        // Each branch code the binding can emit must be in the portable
        // operation's vocabulary. `sys.version.incompatible` is a global code
        // that the portable table chains into every callable operation, so it
        // is not declared per operation on the host side.
        let portable = orna_sys::system_dispatch_table()
            .operation("sys.blob.capture_file")
            .expect("capture_file is a portable sys operation");
        for code in ["sys.blob.source_denied", "sys.version.incompatible"] {
            assert!(
                portable
                    .failures
                    .iter()
                    .any(|declared| declared.as_str() == code),
                "capture_file must declare `{code}`"
            );
        }
    }

    #[test]
    fn blob_annotation_preserves_contextual_formats_without_metadata_reads() {
        struct CountingResolver {
            bytes: Vec<u8>,
            bytes_read: std::sync::atomic::AtomicU64,
        }

        impl orna_value::BlobResolver for CountingResolver {
            fn read_range(
                &self,
                _reference: &orna_value::ContentReference,
                offset: u64,
                length: u64,
            ) -> orna_value::Result<Vec<u8>> {
                let start = usize::try_from(offset).map_err(|_| orna_value::Error::InvalidRange)?;
                let length =
                    usize::try_from(length).map_err(|_| orna_value::Error::InvalidRange)?;
                let end = start
                    .checked_add(length)
                    .ok_or(orna_value::Error::InvalidRange)?;
                let bytes = self
                    .bytes
                    .get(start..end)
                    .ok_or(orna_value::Error::InvalidRange)?;
                self.bytes_read
                    .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
                Ok(bytes.to_vec())
            }
        }

        let payload = b"payload";
        for format in [ValueFormat::Rov3, ValueFormat::Sov3] {
            let resolver = std::sync::Arc::new(CountingResolver {
                bytes: payload.to_vec(),
                bytes_read: std::sync::atomic::AtomicU64::new(0),
            });
            let database = [1; 16];
            let resolver_for_context: std::sync::Arc<dyn orna_value::BlobResolver> =
                resolver.clone();
            let context =
                orna_value::Format3Context::from_persisted(orna_value::Format3ContextInputs {
                    database_uuid: database,
                    pin: orna_value::Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
                    git_oid_algorithm: orna_value::GitHash::Sha1,
                    schema_root: orna_value::NativeOid::from_bytes(&[9; 20]).unwrap(),
                    store_root: orna_value::NativeOid::from_bytes(&[8; 20]).unwrap(),
                    availability: orna_value::AvailabilityState::Available,
                    owner: orna_value::OwnerLifetime::new([3; 16]),
                    quota: orna_value::Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
                    resolver: resolver_for_context,
                })
                .unwrap();
            let identity = orna_value::ContentIdentity::from_bytes(payload);
            let reference = context
                .reference(
                    identity,
                    orna_value::NativeOid::from_bytes(&[7; 20]).unwrap(),
                )
                .unwrap();
            let source_blob =
                Blob::from_reference_with_annotation(reference, "application/octet-stream", None)
                    .unwrap();
            assert!(!source_blob.is_hydrated());
            let source = ContextValue::from_blob(&source_blob, format).unwrap();
            let arguments = [
                source,
                ContextValue::new_in_context(
                    &context,
                    format,
                    OvbRaw::Text("audio/mp4".to_owned()),
                )
                .unwrap(),
                ContextValue::new_in_context(&context, format, OvbRaw::Text("m4b".to_owned()))
                    .unwrap(),
            ];

            let annotated = dispatch_blob_annotation(SysFunction::BlobAnnotate, &arguments)
                .expect("annotation dispatch preserves its source profile");

            assert_eq!(annotated.format(), format);
            assert!(annotated.context().is_some());
            let blob = annotated.blob().unwrap();
            assert_eq!(blob.content_identity(), identity);
            assert_eq!(blob.media_type(), "audio/mp4");
            assert_eq!(blob.suffix(), Some("m4b"));
            assert!(!blob.is_hydrated());
            assert_eq!(
                resolver
                    .bytes_read
                    .load(std::sync::atomic::Ordering::Relaxed),
                0
            );

            // Format-3 stores the Blob descriptor; explicit reads use the
            // original authorized capture reference.
            assert_eq!(source_blob.read_to_end().unwrap(), payload);
            assert_eq!(
                resolver
                    .bytes_read
                    .load(std::sync::atomic::Ordering::Relaxed),
                payload.len() as u64
            );
        }
        let role = orna_sys::system_dispatch_table()
            .role(BLOB_ANNOTATION_ROLE)
            .expect("generated Blob annotation role exists");
        let legacy = ContextValue::new(ValueFormat::Ovb1, OvbRaw::Bytes(payload.to_vec())).unwrap();
        assert_eq!(
            BlobAnnotationProvider::linked(role, SysFunction::BlobAnnotate, &legacy)
                .err()
                .map(|error| error.code().to_owned()),
            Some("sys.codec.unsupported_value_profile".to_owned()),
            "the annotated-Blob ABI must not reinterpret OVB-1 bytes"
        );
    }

    fn blob_source(bytes: &[u8]) -> ContextValue {
        ContextValue::from_blob(&Blob::from_bytes(bytes.to_vec()), ValueFormat::Ovb2)
            .expect("OVB-2 Blob source")
    }

    fn bounds(values: &[i64]) -> Vec<BigInt> {
        values.iter().map(|value| BigInt::from(*value)).collect()
    }

    fn read_code(result: Result<BlobReadValue, EvaluationError>) -> String {
        match result {
            Ok(_) => panic!("read-side call must fail"),
            Err(error) => error.code().to_owned(),
        }
    }

    #[test]
    fn blob_read_length_digest_verify_and_bytes_come_from_the_value() {
        let payload = b"0123456789";
        let source = blob_source(payload);
        let Ok(BlobReadValue::Int(length)) =
            dispatch_blob_read(SysFunction::BlobLength, &source, &[])
        else {
            panic!("length must return Int");
        };
        assert_eq!(length, 10);
        let Ok(BlobReadValue::Digest(digest)) =
            dispatch_blob_read(SysFunction::BlobDigest, &source, &[])
        else {
            panic!("digest must return Digest");
        };
        // Known-answer vector: SHA-256 of the raw bytes "0123456789". Comparing
        // with the blob's own identity would pass by comparing a value to itself.
        assert_eq!(
            crate::format_digest_hex(&digest),
            "84d89877f0d4041efb6bf91a16f0248f2fd573e6af05c19f96bedb9f882f7882"
        );
        assert!(matches!(
            dispatch_blob_read(SysFunction::BlobVerify, &source, &[]),
            Ok(BlobReadValue::Unit)
        ));
        let Ok(BlobReadValue::Bytes(bytes)) =
            dispatch_blob_read(SysFunction::BlobToBytes, &source, &[])
        else {
            panic!("to_bytes must return bytes");
        };
        assert_eq!(bytes, payload);
    }

    #[test]
    fn blob_digest_is_not_subject_to_the_call_limit() {
        use sha2::Digest as _;

        // The digest comes from the stored identity and reads no content, so
        // a Blob over the 8 MiB call limit still reports its commitment.
        let over_limit = vec![7u8; MAX_BLOB_CALL_BYTES as usize + 1];
        let source = blob_source(&over_limit);
        let Ok(BlobReadValue::Digest(digest)) =
            dispatch_blob_read(SysFunction::BlobDigest, &source, &[])
        else {
            panic!("digest must not be refused by the read limit");
        };
        let expected: [u8; 32] = sha2::Sha256::digest(&over_limit).into();
        assert_eq!(digest, expected);
    }

    #[test]
    fn blob_read_returns_a_bounded_slice_clipped_only_at_known_eof() {
        let source = blob_source(b"0123456789");
        let Ok(BlobReadValue::Blob(slice)) =
            dispatch_blob_read(SysFunction::BlobRead, &source, &bounds(&[2, 3]))
        else {
            panic!("read must return a Blob");
        };
        assert_eq!(
            slice.blob().expect("slice Blob").read_to_end().unwrap(),
            b"234"
        );
        let Ok(BlobReadValue::Blob(clipped)) =
            dispatch_blob_read(SysFunction::BlobRead, &source, &bounds(&[8, 5]))
        else {
            panic!("read past EOF must clip to the known end");
        };
        assert_eq!(
            clipped.blob().expect("clipped Blob").read_to_end().unwrap(),
            b"89"
        );
    }

    #[test]
    fn blob_read_bounds_and_limit_failures_use_declared_codes() {
        let source = blob_source(b"0123456789");
        assert_eq!(
            read_code(dispatch_blob_read(
                SysFunction::BlobRead,
                &source,
                &bounds(&[-1, 1])
            )),
            "sys.blob.bounds"
        );
        assert_eq!(
            read_code(dispatch_blob_read(
                SysFunction::BlobRead,
                &source,
                &bounds(&[11, 1])
            )),
            "sys.blob.bounds"
        );
        assert_eq!(
            read_code(dispatch_blob_read(
                SysFunction::BlobRead,
                &source,
                &bounds(&[i64::MAX, i64::MAX])
            )),
            "sys.blob.bounds"
        );
        assert_eq!(
            read_code(dispatch_blob_read(
                SysFunction::BlobRead,
                &source,
                &bounds(&[0, 8 * 1024 * 1024 + 1])
            )),
            "sys.blob.limit"
        );
    }

    #[test]
    fn blob_from_bytes_builds_an_ovb2_blob_within_the_byte_range() {
        let values = bounds(&[0, 1, 255]);
        let value = dispatch_blob_from_bytes(&values).expect("bytes in 0..=255 build a Blob");
        assert_eq!(value.format(), ValueFormat::Ovb2);
        assert_eq!(
            value.blob().expect("Blob").read_to_end().unwrap(),
            [0u8, 1, 255]
        );
        assert_eq!(
            dispatch_blob_from_bytes(&bounds(&[256]))
                .err()
                .map(|error| error.code().to_owned()),
            Some("sys.blob.bounds".to_owned())
        );
        assert_eq!(
            dispatch_blob_from_bytes(&bounds(&[-1]))
                .err()
                .map(|error| error.code().to_owned()),
            Some("sys.blob.bounds".to_owned())
        );
        let too_many = vec![BigInt::from(0); MAX_BLOB_CALL_BYTES as usize + 1];
        assert_eq!(
            dispatch_blob_from_bytes(&too_many)
                .err()
                .map(|error| error.code().to_owned()),
            Some("sys.blob.limit".to_owned())
        );
    }

    #[test]
    fn blob_to_bytes_materialises_at_most_the_call_limit() {
        let at_limit = vec![7u8; MAX_BLOB_CALL_BYTES as usize];
        let Ok(BlobReadValue::Bytes(bytes)) =
            dispatch_blob_read(SysFunction::BlobToBytes, &blob_source(&at_limit), &[])
        else {
            panic!("to_bytes at the 8 MiB limit must succeed");
        };
        assert_eq!(bytes.len(), at_limit.len());

        let over_limit = vec![7u8; MAX_BLOB_CALL_BYTES as usize + 1];
        assert_eq!(
            read_code(dispatch_blob_read(
                SysFunction::BlobToBytes,
                &blob_source(&over_limit),
                &[]
            )),
            "sys.blob.limit"
        );
    }

    /// A ROV-3 Blob whose reads go only through `resolver`. It is built as a
    /// `Blob`, not through `ContextValue` (which holds ROV-3 descriptor-only,
    /// ornadb-55op), so the resolver's outcome reaches the reader.
    fn reference_blob(resolver: std::sync::Arc<dyn orna_value::BlobResolver>) -> Blob {
        let database = [1; 16];
        let context =
            orna_value::Format3Context::from_persisted(orna_value::Format3ContextInputs {
                database_uuid: database,
                pin: orna_value::Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
                git_oid_algorithm: orna_value::GitHash::Sha1,
                schema_root: orna_value::NativeOid::from_bytes(&[9; 20]).unwrap(),
                store_root: orna_value::NativeOid::from_bytes(&[8; 20]).unwrap(),
                availability: orna_value::AvailabilityState::Available,
                owner: orna_value::OwnerLifetime::new([3; 16]),
                quota: orna_value::Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
                resolver,
            })
            .unwrap();
        let identity = orna_value::ContentIdentity::from_bytes(b"payload");
        let reference = context
            .reference(
                identity,
                orna_value::NativeOid::from_bytes(&[7; 20]).unwrap(),
            )
            .unwrap();
        Blob::from_reference_with_annotation(reference, "application/octet-stream", None).unwrap()
    }

    #[test]
    fn blob_read_failures_map_unknown_handle_and_rejected_context_to_declared_codes() {
        assert_eq!(
            blob_read_failure(ValueError::ContentUnavailable).code(),
            "sys.blob.unavailable"
        );
        assert_eq!(
            blob_read_failure(ValueError::InvalidContext).code(),
            "sys.blob.reference_context"
        );
        assert_eq!(
            blob_read_failure(ValueError::OwnerExpired).code(),
            "sys.blob.unavailable"
        );
        assert_eq!(
            blob_read_failure(ValueError::MissingDescriptor).code(),
            "sys.blob.unavailable"
        );
        assert_eq!(
            blob_read_failure(ValueError::Cancelled).code(),
            "ORNA-EVAL-CANCELLED"
        );
        assert_eq!(
            blob_reflection_failure(ValueError::OwnerExpired).code(),
            "sys.blob.unavailable"
        );
        assert_eq!(
            blob_reflection_failure(ValueError::MissingDescriptor).code(),
            "sys.blob.unavailable"
        );
    }

    #[test]
    fn held_reference_refuses_unknown_handle_and_rejected_context_without_rebinding() {
        // ORNA-BLOB-008: the held reference keeps its exact context. An unknown
        // handle or a rejected context refuses the read; it never rebinds.
        struct UnknownHandle;

        impl orna_value::BlobResolver for UnknownHandle {
            fn read_range(
                &self,
                _reference: &orna_value::ContentReference,
                _offset: u64,
                _length: u64,
            ) -> orna_value::Result<Vec<u8>> {
                Err(ValueError::ContentUnavailable)
            }
        }

        struct RejectedContext;

        impl orna_value::BlobResolver for RejectedContext {
            fn read_range(
                &self,
                _reference: &orna_value::ContentReference,
                _offset: u64,
                _length: u64,
            ) -> orna_value::Result<Vec<u8>> {
                Err(ValueError::InvalidContext)
            }
        }

        let unknown = reference_blob(std::sync::Arc::new(UnknownHandle));
        assert_eq!(unknown.read_to_end(), Err(ValueError::ContentUnavailable));
        assert_eq!(unknown.read(0, 3), Err(ValueError::ContentUnavailable));

        let rejected = reference_blob(std::sync::Arc::new(RejectedContext));
        assert_eq!(rejected.read_to_end(), Err(ValueError::InvalidContext));
        assert_eq!(rejected.read(0, 3), Err(ValueError::InvalidContext));
        assert_eq!(
            blob_read_failure(unknown.verify().unwrap_err()).code(),
            "sys.blob.unavailable"
        );
        assert_eq!(
            blob_read_failure(rejected.read_to_end().unwrap_err()).code(),
            "sys.blob.reference_context"
        );
    }

    /// Serves exactly one payload so the ROV-3 descriptor can be read back.
    struct PayloadResolver(Vec<u8>);

    impl orna_value::BlobResolver for PayloadResolver {
        fn read_range(
            &self,
            _reference: &orna_value::ContentReference,
            offset: u64,
            length: u64,
        ) -> orna_value::Result<Vec<u8>> {
            let start = usize::try_from(offset).map_err(|_| orna_value::Error::InvalidRange)?;
            let length = usize::try_from(length).map_err(|_| orna_value::Error::InvalidRange)?;
            let end = start
                .checked_add(length)
                .ok_or(orna_value::Error::InvalidRange)?;
            self.0
                .get(start..end)
                .map(<[u8]>::to_vec)
                .ok_or(orna_value::Error::InvalidRange)
        }
    }

    /// Builds one ROV-3 descriptor and an inline OVB-2 value with the same
    /// content and annotation, together with the context that owns the
    /// descriptor.
    fn rov3_fixture() -> (orna_value::Format3Context, ContextValue, ContextValue) {
        let payload = b"payload";
        let database = [1; 16];
        let resolver: std::sync::Arc<dyn orna_value::BlobResolver> =
            std::sync::Arc::new(PayloadResolver(payload.to_vec()));
        let context =
            orna_value::Format3Context::from_persisted(orna_value::Format3ContextInputs {
                database_uuid: database,
                pin: orna_value::Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
                git_oid_algorithm: orna_value::GitHash::Sha1,
                schema_root: orna_value::NativeOid::from_bytes(&[9; 20]).unwrap(),
                store_root: orna_value::NativeOid::from_bytes(&[8; 20]).unwrap(),
                availability: orna_value::AvailabilityState::Available,
                owner: orna_value::OwnerLifetime::new([3; 16]),
                quota: orna_value::Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
                resolver,
            })
            .unwrap();
        let identity = orna_value::ContentIdentity::from_bytes(payload);
        let reference = context
            .reference(
                identity,
                orna_value::NativeOid::from_bytes(&[7; 20]).unwrap(),
            )
            .unwrap();
        let rov3_source =
            Blob::from_reference_with_annotation(reference, "application/octet-stream", None)
                .unwrap();
        let ovb2_source =
            Blob::from_bytes_with_annotation(payload.to_vec(), "application/octet-stream", None)
                .unwrap();
        let rov3 = ContextValue::from_blob(&rov3_source, ValueFormat::Rov3).unwrap();
        let ovb2 = ContextValue::from_blob(&ovb2_source, ValueFormat::Ovb2).unwrap();
        (context, rov3, ovb2)
    }

    // Descriptor-only ROV-3 round-trip binding loss is tracked as ornadb-55op.
    #[test]
    fn rov3_descriptor_only_is_unavailable_and_self_equal() {
        let (context, rov3, ovb2) = rov3_fixture();

        let rov3_text = |value: &str| {
            ContextValue::new_in_context(
                &context,
                ValueFormat::Rov3,
                OvbRaw::Text(value.to_owned()),
            )
            .unwrap()
        };
        let ovb2_text = |value: &str| {
            ContextValue::new(ValueFormat::Ovb2, OvbRaw::Text(value.to_owned())).unwrap()
        };
        let rov3_annotated = dispatch_blob_annotation(
            SysFunction::BlobAnnotate,
            &[rov3.clone(), rov3_text("audio/mp4"), rov3_text("m4b")],
        )
        .expect("annotation keeps the ROV-3 profile");
        let ovb2_annotated = dispatch_blob_annotation(
            SysFunction::BlobAnnotate,
            &[ovb2.clone(), ovb2_text("audio/mp4"), ovb2_text("m4b")],
        )
        .expect("annotation keeps the OVB-2 profile");
        assert_eq!(rov3_annotated.format(), ValueFormat::Rov3);
        assert_eq!(ovb2_annotated.format(), ValueFormat::Ovb2);

        let media_type = |value: &ContextValue| match dispatch_blob_reflection(
            SysFunction::BlobMediaType,
            std::slice::from_ref(value),
            None,
        )
        .expect("media_type reads metadata")
        {
            Value::String(media_type) => media_type,
            _ => panic!("media_type returns Str"),
        };
        let suffix = |value: &ContextValue| {
            dispatch_blob_reflection(SysFunction::BlobSuffix, std::slice::from_ref(value), None)
                .expect("suffix reads metadata")
        };
        assert_eq!(media_type(&rov3), "application/octet-stream");
        assert_eq!(media_type(&ovb2), "application/octet-stream");
        assert_eq!(media_type(&rov3_annotated), "audio/mp4");
        assert_eq!(media_type(&ovb2_annotated), "audio/mp4");
        assert!(matches!(suffix(&rov3), Value::Option(None)));
        assert!(matches!(
            suffix(&ovb2_annotated),
            Value::Option(Some(value)) if matches!(value.as_ref(), Value::String(text) if text == "m4b")
        ));

        let same_content =
            |left: &ContextValue, right: &ContextValue| match dispatch_blob_reflection(
                SysFunction::BlobSameContent,
                &[left.clone(), right.clone()],
                None,
            )
            .expect("same_content reads exact content")
            {
                Value::Bool(equal) => equal,
                _ => panic!("same_content returns Bool"),
            };
        let same_content_code = |left: &ContextValue, right: &ContextValue| {
            dispatch_blob_reflection(
                SysFunction::BlobSameContent,
                &[left.clone(), right.clone()],
                None,
            )
            .err()
            .map(|error| error.code().to_owned())
        };
        // ORNA-BLOB-005: explicit unavailability is an allowed outcome. A decoded
        // ROV-3 value is descriptor-only and carries no content binding, so with
        // no shared admitted witness it reports `sys.blob.unavailable`. This is a
        // deliberate scope decision; losing the Reference binding on the ROV-3
        // round trip is tracked separately (see the linked P2 bead).
        assert_eq!(
            same_content_code(&rov3, &ovb2),
            Some("sys.blob.unavailable".to_owned())
        );
        // Same descriptor in the same admitted context is a shared witness.
        assert!(same_content(&rov3, &rov3));
        // Annotations are ignored when comparing exact inline OVB-2 bytes.
        assert!(same_content(&ovb2, &ovb2_annotated));

        let other = ContextValue::from_blob(
            &Blob::from_bytes_with_annotation(
                b"different".to_vec(),
                "application/octet-stream",
                None,
            )
            .unwrap(),
            ValueFormat::Ovb2,
        )
        .unwrap();
        // Different identities are decided without reading bytes.
        assert!(!same_content(&ovb2, &other));

        let undeclared = dispatch_blob_reflection(SysFunction::BlobMediaType, &[], None)
            .err()
            .map(|error| error.code().to_owned());
        assert_eq!(undeclared, Some("ORNA-EVAL-ARGUMENT".to_owned()));
        assert!(is_blob_host_operation(SysFunction::BlobSuffix));
        assert!(is_blob_host_operation(SysFunction::BlobRead));
        assert!(is_blob_host_operation(SysFunction::BlobFromBytes));
        assert!(!is_blob_host_operation(SysFunction::BlobCaptureFile));
    }

    #[test]
    fn blob_collision_maps_to_portable_code() {
        assert_eq!(
            blob_reflection_failure(ValueError::ContentCollision).code(),
            "sys.blob.collision"
        );
    }

    #[test]
    fn blob_read_rejects_arity_outside_the_declared_signature() {
        let source = blob_source(b"0123456789");
        assert_eq!(
            read_code(dispatch_blob_read(
                SysFunction::BlobRead,
                &source,
                &bounds(&[1])
            )),
            "ORNA-EVAL-ARGUMENT"
        );
        assert_eq!(
            read_code(dispatch_blob_read(
                SysFunction::BlobLength,
                &source,
                &bounds(&[1])
            )),
            "ORNA-EVAL-ARGUMENT"
        );
    }

    #[test]
    fn blob_same_content_observes_cancellation() {
        let left = ContextValue::from_blob(
            &Blob::from_bytes_with_annotation(b"same".to_vec(), "application/octet-stream", None)
                .unwrap(),
            ValueFormat::Ovb2,
        )
        .unwrap();
        let right = ContextValue::from_blob(
            &Blob::from_bytes_with_annotation(b"same".to_vec(), "application/octet-stream", None)
                .unwrap(),
            ValueFormat::Ovb2,
        )
        .unwrap();
        let token = CancellationToken::new();
        token.request();
        let result =
            dispatch_blob_reflection(SysFunction::BlobSameContent, &[left, right], Some(&token));
        // No partial Bool may escape: the cancelled comparison yields only an error.
        let error = result.expect_err("a requested cancellation must fail the comparison");
        assert_eq!(error.code(), "ORNA-EVAL-CANCELLED");
    }
}
