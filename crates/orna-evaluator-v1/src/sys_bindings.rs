//! Evaluator bindings for native operations in the generated sys host registry.

use std::path::Path;

use num_traits::ToPrimitive;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SafeText};
use orna_repository_v1::RepositoryCaptureCapability;
use orna_syntax_v1::Expr;
use orna_sys_v1::{
    system_host_operation_registry, CaptureFileError, ClockProvider, EnvironmentDispatchValue,
    EnvironmentProvider, EnvironmentProviderError, FilesystemProvider, HostHttpResponse,
    HostOperationDescriptor, HttpProvider, ProcessProvider,
};
use orna_value_v1::{Blob, ContextValue, ValueFormat};

use crate::{CancellationToken, EffectHandler, EvaluationError, Ovb2EffectHandler, StepBudget};

const MAX_RANDOM_BYTES: usize = 1_048_576;

/// Evaluator-facing registration of built-in sys host providers.
///
/// Operations are selected from the build-generated sys registry. Environment
/// values are limited to an explicit name snapshot, processes require exact
/// executable/root grants, and clock waits require a duration cap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SysHostBindingRegistry {
    environment: EnvironmentProvider,
    process: Option<ProcessProvider>,
    clock: Option<ClockProvider>,
    filesystem: Option<FilesystemProvider>,
    repository_capture: Option<RepositoryCaptureCapability>,
    http: Option<HttpProvider>,
    system_random: bool,
}

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
        let [executable, args, working_directory, environment, input, timeout, output_limit] =
            arguments
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
        let operation = system_host_operation_registry()
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
        let max_bytes = max_bytes
            .to_u64()
            .ok_or_else(|| redacted_error(CaptureFileError::LimitExceeded.code()))?;
        if cancellation.is_some_and(CancellationToken::is_requested) {
            return Err(redacted_error("ORNA-EVAL-CANCELLED"));
        }
        let filesystem = self
            .filesystem
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let repository = self
            .repository_capture
            .as_ref()
            .ok_or_else(|| redacted_error("ORNA-EVAL-UNSUPPORTED"))?;
        let mut source = filesystem
            .capture_file(root, path, max_bytes)
            .map_err(|failure| capture_file_failure(operation, failure))?;
        let candidate = repository.capture_blob_candidate(&mut source, max_bytes);
        source
            .verify_unchanged()
            .map_err(|failure| capture_file_failure(operation, failure))?;
        if source.exceeded() {
            return Err(capture_file_failure(
                operation,
                CaptureFileError::LimitExceeded,
            ));
        }
        let candidate = candidate
            .map_err(|_| capture_file_failure(operation, CaptureFileError::SourceUnavailable))?;
        if cancellation.is_some_and(CancellationToken::is_requested) {
            return Err(redacted_error("ORNA-EVAL-CANCELLED"));
        }
        let bytes = source.captured_bytes().to_vec();
        drop(candidate);
        let (media_type, suffix) = mime1_for_path(path);
        let blob = Blob::from_bytes_with_annotation(bytes, media_type, suffix.as_deref())
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))?;
        ContextValue::from_blob(&blob, ValueFormat::Ovb2)
            .map(Some)
            .map_err(|_| redacted_error("ORNA-EVAL-VALUE"))
    }
}

fn static_call_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", static_call_name(base)?, name)),
        Expr::Group { inner, .. } => static_call_name(inner),
        _ => None,
    }
}

fn capture_file_failure(
    operation: &HostOperationDescriptor,
    failure: CaptureFileError,
) -> EvaluationError {
    if operation
        .failures
        .iter()
        .any(|code| code == failure.failure_code())
    {
        redacted_error(failure.code())
    } else {
        redacted_error("ORNA-EVAL-UNSUPPORTED")
    }
}

fn mime1_for_path(path: &str) -> (&'static str, Option<String>) {
    let extension = Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "json" => ("application/json", None),
        "pdf" => ("application/pdf", None),
        "wasm" => ("application/wasm", None),
        "zip" => ("application/zip", None),
        "gz" | "tgz" => ("application/gzip", (extension != "gz").then_some(extension)),
        "flac" => ("audio/flac", None),
        "m4a" | "m4b" | "mpg4" => ("audio/mp4", (extension != "m4a").then_some(extension)),
        "mp1" | "mp2" | "mp3" => ("audio/mpeg", (extension != "mp3").then_some(extension)),
        "oga" | "ogg" | "opus" => ("audio/ogg", (extension != "ogg").then_some(extension)),
        "wav" => ("audio/wav", None),
        "gif" => ("image/gif", None),
        "jpe" | "jpeg" | "jpg" => ("image/jpeg", (extension != "jpg").then_some(extension)),
        "png" => ("image/png", None),
        "svg" => ("image/svg+xml", None),
        "webp" => ("image/webp", None),
        "css" => ("text/css", None),
        "mjs" | "js" => ("text/javascript", (extension != "js").then_some(extension)),
        "text" | "txt" => ("text/plain", (extension != "txt").then_some(extension)),
        "m4v" | "mp4" => ("video/mp4", (extension != "mp4").then_some(extension)),
        "webm" => ("video/webm", None),
        _ => ("application/octet-stream", None),
    }
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
    fn capture_file_selects_only_mime1_annotations() {
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
                "sys.blob.capture.unauthorized_root",
                "sys.blob.capture.invalid_path",
                "sys.blob.capture.source_unavailable",
                "sys.blob.capture.limit_exceeded",
                "sys.blob.capture.source_changed",
            ]
        );
        assert_eq!(
            mime1_for_path("records/data.json"),
            ("application/json", None)
        );
        assert_eq!(
            mime1_for_path("records/photo.jpeg"),
            ("image/jpeg", Some("jpeg".to_owned()))
        );
        assert_eq!(
            mime1_for_path("records/unknown.custom"),
            ("application/octet-stream", None)
        );
    }
}
