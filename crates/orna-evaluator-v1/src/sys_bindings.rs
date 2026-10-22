//! Evaluator bindings for native operations in the generated sys host registry.

use std::sync::{Arc, Mutex};

use num_traits::ToPrimitive;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SafeText};
use orna_repository_v1::{
    CapturedBlobCandidate, GraphError, OrpBlobBinding, RepositoryCaptureCapability,
};
use orna_syntax_v1::Expr;
use orna_sys_v1::{
    CaptureFileError, ClockProvider, EnvironmentDispatchValue, EnvironmentProvider,
    EnvironmentProviderError, FailureCode, FilesystemProvider, HostHttpResponse,
    HostOperationDescriptor, HttpProvider, OperationContract, OperationId, ProcessProvider,
    ProviderDiagnostic, ProviderFailure, ProviderOffer, SemanticRoleContract,
    SystemOperationProvider, TypeId, TypedValue, system_host_operation_registry,
};
use orna_value_v1::{Blob, ContextValue, Error as ValueError, ValueFormat};

use crate::{CancellationToken, EffectHandler, EvaluationError, Ovb2EffectHandler, StepBudget};

const MAX_RANDOM_BYTES: usize = 1_048_576;

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
    }
}

impl Eq for SysHostBindingRegistry {}

impl SysHostBindingRegistry {
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
        operation_name: &str,
        arguments: &[CanonicalValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if operation_name == "std.random.entropy" {
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
            return CanonicalValue::new(OvbRaw::Bytes(bytes))
                .map(Some)
                .map_err(|_| redacted_error("ORNA-EVAL-VALUE"));
        }
        let registry = system_host_operation_registry();
        let Some(operation) = registry.operation(operation_name) else {
            return Ok(None);
        };
        if operation
            .effects
            .iter()
            .any(|effect| !matches!(effect.as_str(), "read" | "invoke"))
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let role = registry
            .role(&operation.role)
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        if role.provider != operation.provider
            || !role
                .operations
                .iter()
                .any(|candidate| candidate == operation_name)
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        match role.provider.as_str() {
            "orna.sys.host.environment.v1" => self.dispatch_environment(operation, arguments),
            "orna.sys.host.process.v1" => self.dispatch_process(operation, arguments),
            "orna.sys.host.clock.v1" => self.dispatch_clock(operation, arguments, cancellation),
            "orna.sys.host.filesystem.v1" => self.dispatch_filesystem(operation, arguments),
            "orna.sys.host.http.v1" => self.dispatch_http(operation, arguments),
            _ => Err(redacted_error("ORNA-EVAL-UNSUPPORTED")),
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
        let value = match operation.name.as_str() {
            "std.io.fs.read_text" if operation.implementation == "read_text" => {
                let [root, path] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                OvbRaw::Text(
                    provider
                        .read_text(root, path)
                        .map_err(|failure| self.failure(operation, failure.code()))?,
                )
            }
            "std.io.fs.write_text" if operation.implementation == "write_text" => {
                let [root, path, contents, overwrite] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let contents =
                    raw_text(contents.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let overwrite =
                    raw_bool(overwrite.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                provider
                    .write_text(root, path, contents, overwrite)
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                OvbRaw::Null
            }
            "std.io.fs.append_text" if operation.implementation == "append_text" => {
                let [root, path, contents] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let contents =
                    raw_text(contents.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                provider
                    .append_text(root, path, contents)
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                OvbRaw::Null
            }
            "std.io.fs.exists" if operation.implementation == "exists" => {
                let [root, path] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                OvbRaw::Bool(
                    provider
                        .exists(root, path)
                        .map_err(|failure| self.failure(operation, failure.code()))?,
                )
            }
            "std.io.fs.is_directory" if operation.implementation == "is_directory" => {
                let [root, path] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                OvbRaw::Bool(
                    provider
                        .is_directory(root, path)
                        .map_err(|failure| self.failure(operation, failure.code()))?,
                )
            }
            "std.io.fs.list" if operation.implementation == "list" => {
                let [root, path] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                OvbRaw::Array(
                    provider
                        .list(root, path)
                        .map_err(|failure| self.failure(operation, failure.code()))?
                        .into_iter()
                        .map(OvbRaw::Text)
                        .collect(),
                )
            }
            "std.io.fs.metadata" | "std.io.fs.symlink_metadata"
                if operation.implementation == operation.name.rsplit('.').next().unwrap_or("") =>
            {
                let [root, path] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let metadata = if operation.name == "std.io.fs.metadata" {
                    provider.metadata(root, path)
                } else {
                    provider.symlink_metadata(root, path)
                }
                .map_err(|failure| self.failure(operation, failure.code()))?;
                OvbRaw::Array(vec![
                    OvbRaw::Text(metadata.kind),
                    optional_value(metadata.size.map(|size| OvbRaw::Int(size.into()))),
                    optional_value(metadata.modified.and_then(system_time_raw)),
                    optional_value(metadata.created.and_then(system_time_raw)),
                ])
            }
            "std.io.fs.create_dir" if operation.implementation == "create_dir" => {
                let [root, path, parents, exist_ok] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let parents =
                    raw_bool(parents.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let exist_ok =
                    raw_bool(exist_ok.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                provider
                    .create_dir(root, path, parents, exist_ok)
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                OvbRaw::Null
            }
            "std.io.fs.remove_file" if operation.implementation == "remove_file" => {
                let [root, path] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let path = raw_text(path.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                provider
                    .remove_file(root, path)
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                OvbRaw::Null
            }
            "std.io.fs.copy_file" if operation.implementation == "copy_file" => {
                let [root, source, destination, overwrite] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let source =
                    raw_text(source.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let destination =
                    raw_text(destination.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let overwrite =
                    raw_bool(overwrite.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                provider
                    .copy_file(root, source, destination, overwrite)
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                OvbRaw::Null
            }
            "std.io.fs.move_file" if operation.implementation == "move_file" => {
                let [root, source, destination, overwrite] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let root = raw_text(root.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let source =
                    raw_text(source.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let destination =
                    raw_text(destination.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                let overwrite =
                    raw_bool(overwrite.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                provider
                    .move_file(root, source, destination, overwrite)
                    .map_err(|failure| self.failure(operation, failure.code()))?;
                OvbRaw::Null
            }
            _ => return Err(redacted_error("ORNA-EVAL-UNSUPPORTED")),
        };
        CanonicalValue::new(value)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }

    fn dispatch_http(
        &self,
        operation: &HostOperationDescriptor,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if !matches!(
            operation.name.as_str(),
            "std.net.http.send"
                | "std.net.http.start"
                | "std.net.http.wait"
                | "std.net.http.cancel"
        ) || operation.implementation != operation.name.rsplit('.').next().unwrap_or("")
            || operation.role != "host.std.net.http@1.0"
            || operation.provider != "orna.sys.host.http.v1"
            || operation.effects != ["invoke"]
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let provider = self
            .http
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let raw = match operation.name.as_str() {
            "std.net.http.send" | "std.net.http.start" => {
                let (method, url, headers, body, timeout, max_headers, max_body) =
                    parse_http_request_arguments(arguments)?;
                if operation.name == "std.net.http.send" {
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
                } else {
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
            }
            "std.net.http.wait" => {
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
            "std.net.http.cancel" => {
                let [handle] = arguments else {
                    return Err(redacted_error("ORNA-EVAL-ARGUMENT"));
                };
                let handle =
                    raw_uuid(handle.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
                Ok(OvbRaw::Bool(provider.cancel(handle).map_err(
                    |failure| self.failure(operation, failure.code()),
                )?))
            }
            _ => return Err(redacted_error("ORNA-EVAL-UNSUPPORTED")),
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
        if operation.name != "std.io.process.run"
            || operation.implementation != "run"
            || operation.role != "host.std.io.process@1.0"
            || operation.provider != "orna.sys.host.process.v1"
            || operation.effects != ["invoke"]
        {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        let provider = self
            .process
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
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
        let executable =
            raw_text(executable.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let working_directory =
            raw_text(working_directory.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let args = raw_text_array(args.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let environment =
            raw_text_pairs(environment.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let input = optional_raw(input.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let input = match input {
            None | Some(OvbRaw::Null) => None,
            Some(OvbRaw::Bytes(bytes)) => Some(bytes.as_slice()),
            Some(_) => return Err(redacted_error("ORNA-EVAL-TYPE")),
        };
        let timeout =
            optional_raw(timeout.raw()).ok_or_else(|| redacted_error("ORNA-EVAL-TYPE"))?;
        let timeout = match timeout {
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
        let output = provider
            .run(
                executable,
                &args,
                working_directory,
                &environment,
                input,
                timeout,
                output_limit,
            )
            .map_err(|failure| self.failure(operation, failure.code()))?;
        let exit_status = match output.exit_status {
            Some(status) => OvbRaw::Tag(
                60013,
                Box::new(OvbRaw::Array(vec![
                    OvbRaw::Int(1.into()),
                    OvbRaw::Int(status.into()),
                ])),
            ),
            None => OvbRaw::Tag(60013, Box::new(OvbRaw::Array(vec![OvbRaw::Int(0.into())]))),
        };
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
        if operation.name != "std.concurrent.sleep"
            || operation.implementation != "sleep"
            || operation.role != "host.std.concurrent.clock@1.0"
            || operation.provider != "orna.sys.host.clock.v1"
            || operation.effects != ["invoke"]
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
        let declares_failure = operation.failures.iter().any(|declared| declared == code);
        if declares_failure && code == "sys.host.clock.cancelled" {
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

fn parse_http_request_arguments(
    arguments: &[CanonicalValue],
) -> Result<
    (
        String,
        String,
        Vec<(String, String)>,
        Option<Vec<u8>>,
        Option<std::time::Duration>,
        usize,
        usize,
    ),
    EvaluationError,
> {
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
        self.dispatch(operation, arguments, cancellation)
    }

    fn fork_task_child(&mut self, _child_index: usize) -> Option<Box<dyn EffectHandler + Send>> {
        Some(Box::new(self.clone()))
    }
}

impl Ovb2EffectHandler for SysHostBindingRegistry {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let Some(operation_name) = static_call_name(callee) else {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        };
        if operation_name != "sys.blob.capture_file" {
            return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
        }
        self.capture_file_ovb2(&operation_name, arguments, None)
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
        self.capture_file_ovb2(operation_name, arguments, cancellation)
    }
}

impl SysHostBindingRegistry {
    fn capture_file_ovb2(
        &self,
        operation_name: &str,
        arguments: &[ContextValue],
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        system_host_operation_registry()
            .operation(operation_name)
            .filter(|operation| {
                operation.name == "sys.blob.capture_file"
                    && operation.implementation == "capture_file"
                    && operation.role == "host.sys.blob.capture@1.0"
                    && operation.provider == "orna.sys.host.filesystem.v1"
                    && operation.effects == ["read"]
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
            .ok_or_else(|| redacted_error("sys.blob.bounds"))?;
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
            .ok_or_else(|| redacted_error("sys.version.incompatible"))?;
        let mut source = filesystem
            .capture_file(root, path, max_bytes)
            .map_err(capture_file_failure)?;
        let candidate = repository.capture_blob_candidate(&mut source, max_bytes);
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
        // (ORNA-MIME-001).
        let blob = Blob::from_bytes(source.into_captured_bytes());
        let value = ContextValue::from_blob(&blob, ValueFormat::Ovb2)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?;
        self.capture_candidates
            .lock()
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?
            .push(CapturedBlobForRow {
                candidate,
                media_type: blob.media_type().to_owned(),
                suffix: blob.suffix().map(str::to_owned),
            });
        Ok(value)
    }
}

/// Generated semantic role that owns the portable annotated-Blob operations.
const BLOB_ANNOTATION_ROLE: &str = "langitem.sys.blob.annotate";

/// The single portable operation that role currently declares.
///
/// This is only an admission filter: parameter names, order, declared types,
/// defaults, the result type and the declared failure vocabulary all come from
/// the build-generated typed registry, never from this module.
const BLOB_ANNOTATION_OPERATION: &str = "sys.blob.annotate";
/// Generated semantic contract consumed by the native Blob annotation binding.
const BLOB_ANNOTATION_CONTRACT: &str = "annotated-blob-format-3";

/// True when `operation` is the generated portable Blob annotation operation.
pub(crate) fn is_blob_annotation_operation(operation: &str) -> bool {
    operation == BLOB_ANNOTATION_OPERATION
}

/// Rewraps a registry-admitted failure code as an evaluator error.
///
/// The code already crossed `validate_failure`, so it is a generated qualified
/// identifier; the fallback only guards the evaluator's own text admission.
fn redacted_code(code: &str) -> EvaluationError {
    SafeText::new(code.to_owned())
        .map(EvaluationError::redacted)
        .unwrap_or_else(|_| redacted_error("ORNA-EVAL-UNSUPPORTED"))
}

fn static_failure(code: &'static str) -> FailureCode {
    FailureCode::new(code).expect("static provider failure code is valid")
}

/// Resolves one portable Blob operation from the generated typed registry.
///
/// Admission requires the generated `annotated-blob-format-3` contract and its
/// binding to the portable Blob annotation role, so dispatch follows the same
/// semantic metadata that emits the language declaration.
fn blob_annotation_contract(
    operation: &str,
) -> Result<&'static OperationContract, EvaluationError> {
    if !is_blob_annotation_operation(operation) {
        return Err(redacted_error("ORNA-EVAL-UNSUPPORTED"));
    }
    let contract = orna_sys_v1::system_dispatch_table()
        .operation(operation)
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
    source: &'a ContextValue,
}

impl<'a> BlobAnnotationProvider<'a> {
    /// Links a provider offer to the generated role and source Blob.
    fn linked(
        role: &SemanticRoleContract,
        source: &'a ContextValue,
    ) -> Result<Self, EvaluationError> {
        if !matches!(
            source.format(),
            ValueFormat::Ovb2 | ValueFormat::Rov3 | ValueFormat::Sov3
        ) {
            return Err(redacted_error("sys.codec.unsupported_value_profile"));
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
        let table = orna_sys_v1::system_dispatch_table();
        let Some(contract) = table.operation(operation.as_str()) else {
            return Err(ProviderFailure {
                code: static_failure("sys.abi.unavailable"),
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
        let annotated =
            apply_blob_annotation(operation.as_str(), self.source, media_type, suffix.raw())
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
    operation: &str,
    source: &ContextValue,
    media_type: &str,
    suffix: &OvbRaw,
) -> Result<ContextValue, FailureCode> {
    if operation != BLOB_ANNOTATION_OPERATION {
        return Err(static_failure("sys.abi.unavailable"));
    }
    let suffix = blob_suffix_argument(suffix)?;
    source
        .with_blob_annotation(media_type, suffix)
        .map_err(blob_value_failure)
}

fn provider_failure() -> ProviderFailure {
    ProviderFailure {
        code: static_failure("sys.abi.provider_failed"),
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
    operation: &str,
    arguments: &[ContextValue],
) -> Result<ContextValue, EvaluationError> {
    let table = orna_sys_v1::system_dispatch_table();
    let contract = blob_annotation_contract(operation)?;
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
    let provider = BlobAnnotationProvider::linked(role, source)?;
    let mut typed = Vec::with_capacity(parameters.len());
    for (parameter, argument) in parameters.iter().zip(arguments) {
        let canonical = if matches!(
            &parameter.ty,
            orna_sys_v1::AbiType::Named(name) if name == "Blob"
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
    match table.dispatch_to_provider(operation, &provider, &typed, |_| Ok(())) {
        Ok(orna_sys_v1::SystemDispatchResult::Returned(value)) => {
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
        Ok(orna_sys_v1::SystemDispatchResult::Failed(code)) => Err(blob_annotation_failure(&code)),
        Err(ProviderDiagnostic::PreconditionFailed { code, .. }) => {
            Err(blob_annotation_failure(&code))
        }
        // The host does not link the generated provider for this operation:
        // the required coordinate is unavailable (ORNA-SYS-008).
        Err(_) => Err(redacted_error("sys.version.incompatible")),
    }
}

/// A declared portable code is the call's primary failure (ORNA-SYS-097). The
/// shared `sys.abi.*` provider-boundary codes never surface: the provider
/// reports them only for arguments that are not the typed values its contract
/// declares, which the ordinary argument type diagnostic covers.
fn blob_annotation_failure(code: &FailureCode) -> EvaluationError {
    if code.as_str().starts_with("sys.abi.") {
        redacted_error("ORNA-EVAL-TYPE")
    } else {
        redacted_code(code.as_str())
    }
}

/// Reads the `suffix: Str?` argument of `sys.blob.annotate`.
fn blob_suffix_argument(raw: &OvbRaw) -> Result<Option<&str>, FailureCode> {
    match optional_raw(raw) {
        Some(None) => Ok(None),
        Some(Some(OvbRaw::Text(text))) => Ok(Some(text.as_str())),
        _ => Err(static_failure("sys.abi.provider_failed")),
    }
}

/// Maps an `orna_value_v1` annotation failure onto the declared portable
/// failures of `sys.blob.annotate` (MIME-1 declaration codes, and the global
/// value-profile code for an OVB-1 value without an annotation slot).
///
/// Any other value error means the argument was not an annotated Blob value,
/// which the shared provider-boundary code reports.
fn blob_value_failure(error: ValueError) -> FailureCode {
    static_failure(match error {
        ValueError::InvalidMediaType => "sys.blob.invalid_media_type",
        ValueError::InvalidSuffix => "sys.blob.invalid_suffix",
        ValueError::IncompatibleSuffix => "sys.blob.incompatible_suffix",
        ValueError::InvalidProfile => "sys.codec.unsupported_value_profile",
        _ => "sys.abi.provider_failed",
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

/// Surfaces a capture failure with its portable code (ORNA-SYS-097); the
/// generated host operation declares exactly these codes.
fn capture_file_failure(failure: CaptureFileError) -> EvaluationError {
    redacted_error(failure.failure_code())
}

fn redacted_error(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static evaluator error code is safe"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            if operation.name == "sys.blob.capture_file" {
                assert_eq!(
                    bindings
                        .dispatch(&operation.name, &[], None)
                        .expect_err("Blob capture has no OVB-1 fallback")
                        .code(),
                    "ORNA-EVAL-UNSUPPORTED"
                );
                continue;
            }
            if operation.provider == "orna.sys.host.environment.v1" {
                let actual = bindings
                    .dispatch(
                        &operation.name,
                        std::slice::from_ref(&environment_argument),
                        None,
                    )
                    .unwrap_or_else(|error| {
                        panic!("{} dispatch failed: {}", operation.name, error.code())
                    })
                    .expect("environment operation returns a value");
                let expected = match operation.name.as_str() {
                    "std.io.environment.get" => OvbRaw::Tag(
                        60013,
                        Box::new(OvbRaw::Array(vec![
                            OvbRaw::Int(1.into()),
                            OvbRaw::Text("fixture-home".to_owned()),
                        ])),
                    ),
                    "std.io.environment.require" => OvbRaw::Text("fixture-home".to_owned()),
                    name => panic!("unmapped environment operation {name}"),
                };
                assert_eq!(actual.raw(), &expected, "{} result", operation.name);
            } else {
                let error = bindings
                    .dispatch(&operation.name, &[], None)
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
                .map(String::as_str)
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
        ] {
            assert!(
                operation
                    .failures
                    .iter()
                    .any(|declared| declared == failure.failure_code()),
                "{failure:?}"
            );
            assert_eq!(capture_file_failure(failure).code(), failure.failure_code());
        }
        // The provider takes a `u64` bound. A bound the binding cannot narrow
        // to it is `sys.blob.bounds`, which the portable operation declares.
        let portable = orna_sys_v1::system_dispatch_table()
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
                .capture_file_ovb2("sys.blob.capture_file", &negative_bound, None)
                .expect_err("a negative byte bound is out of range")
                .code(),
            "sys.blob.bounds"
        );
        assert!(
            operation
                .failures
                .iter()
                .any(|failure| failure == "sys.blob.bounds"),
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
                .capture_file_ovb2("sys.blob.capture_file", &arguments, None)
                .expect_err("a host without a filesystem provider authorises no root")
                .code(),
            "sys.blob.source_denied"
        );
        let without_writer =
            SysHostBindingRegistry::default().with_filesystem_provider(FilesystemProvider::new());
        assert_eq!(
            without_writer
                .capture_file_ovb2("sys.blob.capture_file", &arguments, None)
                .expect_err("a host without a repository Blob writer cannot capture")
                .code(),
            "sys.version.incompatible"
        );
        // Each branch code the binding can emit must be in the portable
        // operation's vocabulary. `sys.version.incompatible` is a global code
        // that the portable table chains into every callable operation, so it
        // is not declared per operation on the host side.
        let portable = orna_sys_v1::system_dispatch_table()
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

        impl orna_value_v1::BlobResolver for CountingResolver {
            fn read_range(
                &self,
                _reference: &orna_value_v1::ContentReference,
                offset: u64,
                length: u64,
            ) -> orna_value_v1::Result<Vec<u8>> {
                let start =
                    usize::try_from(offset).map_err(|_| orna_value_v1::Error::InvalidRange)?;
                let length =
                    usize::try_from(length).map_err(|_| orna_value_v1::Error::InvalidRange)?;
                let end = start
                    .checked_add(length)
                    .ok_or(orna_value_v1::Error::InvalidRange)?;
                let bytes = self
                    .bytes
                    .get(start..end)
                    .ok_or(orna_value_v1::Error::InvalidRange)?;
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
            let resolver_for_context: std::sync::Arc<dyn orna_value_v1::BlobResolver> =
                resolver.clone();
            let context = orna_value_v1::Format3Context::from_persisted(
                database,
                orna_value_v1::Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
                orna_value_v1::GitHash::Sha1,
                orna_value_v1::NativeOid::from_bytes(&[9; 20]).unwrap(),
                orna_value_v1::NativeOid::from_bytes(&[8; 20]).unwrap(),
                orna_value_v1::AvailabilityState::Available,
                orna_value_v1::OwnerLifetime::new([3; 16]),
                orna_value_v1::Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
                resolver_for_context,
            )
            .unwrap();
            let identity = orna_value_v1::ContentIdentity::from_bytes(payload);
            let reference = context
                .reference(
                    identity,
                    orna_value_v1::NativeOid::from_bytes(&[7; 20]).unwrap(),
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

            let annotated = dispatch_blob_annotation(BLOB_ANNOTATION_OPERATION, &arguments)
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
        let role = orna_sys_v1::system_dispatch_table()
            .role(BLOB_ANNOTATION_ROLE)
            .expect("generated Blob annotation role exists");
        let legacy = ContextValue::new(ValueFormat::Ovb1, OvbRaw::Bytes(payload.to_vec())).unwrap();
        assert_eq!(
            BlobAnnotationProvider::linked(role, &legacy)
                .err()
                .map(|error| error.code().to_owned()),
            Some("sys.codec.unsupported_value_profile".to_owned()),
            "the annotated-Blob ABI must not reinterpret OVB-1 bytes"
        );
    }
}
