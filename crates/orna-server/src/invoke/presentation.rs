use std::io::{self, Write};

use orna_core::{
    TypeId,
    catalogue::{FunctionDefinition, FunctionReturn},
    invocation::{
        InvocationCallerKind, InvocationEventBody, InvocationOutputRequirement,
        InvocationOutputTypeSelector, InvocationRuntimeOffer, InvocationSinkOffer,
        InvocationStreamingRequirement, InvocationTarget, InvokeRequest,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
    value::RuntimeValue,
};
use orna_postgres::SealedInvocationResult;
use orna_standard::{STD_IO_BYTE_STREAM_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID};

use super::{InstalledInvokeError, InstalledInvokeErrorKind, InstalledInvokeOutcome};

/// Renders one sealed invocation result into the supplied writers.
///
/// Progress diagnostics, denials, and bind failures go to `stderr`; every
/// `ValueBatch` value goes to `stdout` — Document and ByteStream values
/// through `orna-runtime-tty`, every other value through the supplied
/// encoder as one canonical record — with no progress or warning interleave.
pub(super) fn render_result(
    result: &SealedInvocationResult,
    no_progress: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    encode: &mut impl FnMut(&RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError>,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    match result {
        SealedInvocationResult::Completed { events, .. } => {
            render_event_stream(events, no_progress, stdout, stderr, encode)
        }
        SealedInvocationResult::Failed { events, .. } => {
            render_event_stream(events, no_progress, stdout, stderr, encode)
        }
        SealedInvocationResult::Denied { .. } => {
            writeln!(stderr, "orna: invoke: invocation denied").map_err(presentation_error)?;
            Ok(InstalledInvokeOutcome::Denied)
        }
        SealedInvocationResult::PresentationFailed { .. } => Err(InstalledInvokeError::new(
            InstalledInvokeErrorKind::Presentation,
            "presentation failed".to_owned(),
        )),
    }
}

/// Renders one sealed Event batch in record order.
pub(super) fn render_event_stream(
    events: &orna_protocol::InvocationEventBatch,
    no_progress: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    encode: &mut impl FnMut(&RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError>,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let mut outcome = InstalledInvokeOutcome::Completed;
    for record in events.records() {
        match record.event().body() {
            InvocationEventBody::Started { .. } if !no_progress => {
                writeln!(stderr, "orna: invoke: invocation started").map_err(presentation_error)?;
            }
            InvocationEventBody::ValueBatch { values, .. } => {
                for value in values {
                    render_value(value.value(), stdout, encode)?;
                }
            }
            InvocationEventBody::Completed {
                duration_nanoseconds,
            } if !no_progress => {
                writeln!(
                    stderr,
                    "orna: invoke: invocation completed in {duration_nanoseconds}ns"
                )
                .map_err(presentation_error)?;
            }
            InvocationEventBody::Diagnostic(diagnostic) => {
                writeln!(
                    stderr,
                    "orna: invoke: {}: {}",
                    diagnostic.code(),
                    diagnostic.message()
                )
                .map_err(presentation_error)?;
            }
            InvocationEventBody::Failed(_) => {
                writeln!(stderr, "orna: invoke: invocation failed").map_err(presentation_error)?;
                outcome = InstalledInvokeOutcome::TargetFailure;
            }
            InvocationEventBody::Cancelled { .. } => {
                writeln!(stderr, "orna: invoke: invocation cancelled")
                    .map_err(presentation_error)?;
                outcome = InstalledInvokeOutcome::Cancelled;
            }
            _ => {}
        }
    }
    Ok(outcome)
}

/// Renders one `ValueBatch` value to stdout.
///
/// A `std.terminal.Document` value renders as the document text and a
/// `std.io.ByteStream` value as the raw stream bytes, both through
/// `orna-runtime-tty` (ADR 0057 step 9); every other value keeps the
/// milestone-5 rule: the canonical ORV5 typed encoding followed by the
/// record newline.
pub(super) fn render_value(
    value: &RuntimeValue,
    stdout: &mut impl Write,
    encode: &mut impl FnMut(&RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError>,
) -> Result<(), InstalledInvokeError> {
    if let RuntimeValue::Opaque(opaque) = value
        && let Some(sink) = select_runtime_sink(opaque.opaque_type())
    {
        render_opaque_payload(sink, opaque.canonical_payload(), stdout)?;
        return Ok(());
    }
    let encoded = encode(value)?;
    stdout.write_all(&encoded).map_err(presentation_error)?;
    stdout.write_all(b"\n").map_err(presentation_error)?;
    Ok(())
}

/// Returns the deterministic tty runtime sink for one opaque result type
/// (ADR 0057 step 9), or `None` when the value keeps the ORV5 envelope.
///
/// The rule is unconditional for the two sink types: `--output table`
/// produces a `std.terminal.Document` and `--output json` produces a
/// `std.io.ByteStream`, and in both cases the bytes must reach stdout
/// whether stdout is a terminal or piped to a file. The stdout-is-terminal
/// fact still feeds the caller context (`CliTty` versus `CliPipe`); it does
/// not gate sink consumption.
///
/// Seam (ADR 0063): this mapping is the tty family's sink map and stays
/// unconditional while tty is the only installed runtime. When a second
/// family lands, the renderer gains the selected-family parameter and this
/// function becomes the selected family's sink map.
pub(super) fn select_runtime_sink(opaque_type: TypeId) -> Option<orna_runtime_tty::Sink> {
    match opaque_type {
        STD_TERMINAL_DOCUMENT_TYPE_ID => Some(orna_runtime_tty::Sink::Document),
        STD_IO_BYTE_STREAM_TYPE_ID => Some(orna_runtime_tty::Sink::ByteStream),
        _ => None,
    }
}

/// Renders one presented opaque payload through the selected runtime sink.
///
/// The payload is the canonical codec frame the sealed route emitted; the
/// runtime validates it again before writing anything.
pub(super) fn render_opaque_payload(
    sink: orna_runtime_tty::Sink,
    payload: &[u8],
    stdout: &mut impl Write,
) -> Result<(), InstalledInvokeError> {
    sink.render(payload, stdout).map_err(map_runtime_tty_error)
}

/// Maps one runtime rendering failure to a closed installed error.
///
/// A write failure is a presentation failure like any other output error; a
/// frame rejection cannot occur for a registry-validated value and is an
/// internal inconsistency.
fn map_runtime_tty_error(error: orna_runtime_tty::RuntimeTtyError) -> InstalledInvokeError {
    match error {
        orna_runtime_tty::RuntimeTtyError::Io(error) => presentation_error(error),
        other => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            format!("the tty runtime rejected a presented value: {other}"),
        ),
    }
}

/// Renders the `--explain` plan: resolved target identity and revision,
/// domain, parameters, return type, the sealed request facts, and the local
/// sink/runtime offers that shape presentation.
///
/// Presenter candidates are deliberately not reconstructed here. The sealed
/// route resolves those candidates after target execution, while `--explain`
/// must remain observational and side-effect free. The request does retain
/// the client offers, however, so those facts (including the deterministic
/// single-runtime selection used by this client) are rendered rather than
/// hidden behind the offer count.
pub(super) fn render_explain(
    output: &mut impl Write,
    function: &FunctionDefinition,
    request: &InvokeRequest,
    executable_revision: &str,
    revision_pin: &str,
) -> Result<(), InstalledInvokeError> {
    let mut plan = String::new();
    plan.push_str(&format!(
        "target: {} ({})\n",
        function.name(),
        function.id().canonical()
    ));
    plan.push_str(&format!(
        "revision: {executable_revision} (pinned to {revision_pin})\n"
    ));
    plan.push_str(&format!("domain: {:?}\n", function.domain()));
    plan.push_str("parameters:\n");
    for parameter in function.parameters() {
        plan.push_str(&format!(
            "  {} ({}): {}\n",
            parameter.name(),
            parameter.id().canonical(),
            render_resolved_type(parameter.resolved_type())
        ));
    }
    plan.push_str(&format!(
        "return: {}\n",
        render_return_type(function.return_type())
    ));
    plan.push_str("request:\n");
    plan.push_str(&format!("  target: {}\n", render_target(request.target())));
    plan.push_str(&format!(
        "  caller: {}\n",
        render_caller_kind(request.caller_context().kind())
    ));
    plan.push_str(&format!(
        "  offer: protocol {}, locale {}, timezone {}, sinks {}, runtimes {}, maximum frame {}, maximum artifact {}\n",
        request.client_offer().protocol_major(),
        request.client_offer().locale(),
        request.client_offer().timezone(),
        request.client_offer().sink_offers().len(),
        render_runtime_offers(request.client_offer().runtime_offers()),
        request.client_offer().maximum_frame_size(),
        request.client_offer().maximum_artifact_size(),
    ));
    plan.push_str(&format!("  trace: {:?}\n", request.trace_policy()));
    plan.push_str(&format!(
        "  output: {}\n",
        render_output_requirement(request.output_requirement())
    ));
    plan.push_str("presentation:\n");
    if request.output_requirement().is_some() {
        plan.push_str("  candidates: unavailable before sealed dispatch\n");
        plan.push_str("  rejections: unavailable before sealed dispatch\n");
        plan.push_str("  selected presenter: unavailable before sealed dispatch\n");
    } else {
        plan.push_str("  candidates: none (no output requirement)\n");
        plan.push_str("  rejections: none (no output requirement)\n");
        plan.push_str("  selected presenter: none\n");
    }
    plan.push_str(&format!(
        "  final sink: {}\n",
        render_explain_final_sink(request.output_requirement(), function.return_type())
    ));
    plan.push_str("sinks:\n");
    render_explain_sink_offers(&mut plan, request.client_offer().sink_offers());
    plan.push_str("runtime:\n");
    render_explain_runtime_offers(&mut plan, request.client_offer().runtime_offers());

    output
        .write_all(plan.as_bytes())
        .map_err(presentation_error)?;
    Ok(())
}

/// Returns the final sink fact available without executing the target.
///
/// An output requirement defers presenter selection to the sealed route,
/// which resolves the presenter after target execution. Without an output
/// requirement, the normal renderer still consumes the two installed opaque
/// result types through the tty runtime, while all other results retain the
/// canonical value envelope.
pub(super) fn render_explain_final_sink(
    requirement: Option<&InvocationOutputRequirement>,
    return_type: &FunctionReturn,
) -> String {
    if requirement.is_some() {
        return "deferred until sealed presenter selection".to_owned();
    }
    match return_type {
        FunctionReturn::Single(ResolvedType::Value(type_id)) => match select_runtime_sink(*type_id)
        {
            Some(orna_runtime_tty::Sink::Document) => {
                "tty document sink (opaque result)".to_owned()
            }
            Some(orna_runtime_tty::Sink::ByteStream) => {
                "tty byte-stream sink (opaque result)".to_owned()
            }
            None => "none (canonical result)".to_owned(),
        },
        _ => "none (canonical result)".to_owned(),
    }
}

/// Renders client sink offers in their checked, deterministic order.
fn render_explain_sink_offers(plan: &mut String, offers: &[InvocationSinkOffer]) {
    if offers.is_empty() {
        plan.push_str("  none\n");
        return;
    }
    for offer in offers {
        let media_types = if offer.media_types().is_empty() {
            "none".to_owned()
        } else {
            offer.media_types().join(", ")
        };
        plan.push_str(&format!(
            "  {} (media {}; {}; preference rank {})\n",
            render_type_descriptor(offer.descriptor()),
            media_types,
            if offer.streaming() {
                "streaming"
            } else {
                "non-streaming"
            },
            offer.preference_rank(),
        ));
    }
}

/// Renders runtime offers and the local selection available to this client.
///
/// The current client emits exactly one installed runtime offer. If that
/// invariant changes, retain the offers but avoid presenting an arbitrary
/// first entry as selected until the selection policy is propagated here.
fn render_explain_runtime_offers(plan: &mut String, offers: &[InvocationRuntimeOffer]) {
    if offers.is_empty() {
        plan.push_str("  selected: none\n");
        plan.push_str("  offers: none\n");
        return;
    }

    if offers.len() == 1 {
        let offer = &offers[0];
        plan.push_str(&format!(
            "  selected: {}@{}\n",
            offer.name(),
            offer.version()
        ));
    } else {
        plan.push_str("  selected: unavailable (multiple runtime offers)\n");
    }

    plan.push_str("  offers:\n");
    for offer in offers {
        let descriptors = if offer.consumed_descriptors().is_empty() {
            "none".to_owned()
        } else {
            offer
                .consumed_descriptors()
                .iter()
                .map(render_type_descriptor)
                .collect::<Vec<_>>()
                .join(", ")
        };
        plan.push_str(&format!(
            "    {}@{} (consumes {}; preference rank {}; {})\n",
            offer.name(),
            offer.version(),
            descriptors,
            offer.preference_rank(),
            if offer.trusted() {
                "trusted"
            } else {
                "untrusted"
            },
        ));
    }
}

/// Renders a closed invocation type descriptor without exposing typed values.
fn render_type_descriptor(descriptor: &TypeDescriptor) -> String {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) | TypeDescriptorKind::Reference(type_id) => {
            type_id.canonical()
        }
        TypeDescriptorKind::List(inner) => format!("list<{}>", render_type_descriptor(inner)),
        TypeDescriptorKind::Set(inner) => format!("set<{}>", render_type_descriptor(inner)),
        TypeDescriptorKind::Map { key, value } => format!(
            "map<{},{}>",
            render_type_descriptor(key),
            render_type_descriptor(value)
        ),
        TypeDescriptorKind::Option(inner) => format!("option<{}>", render_type_descriptor(inner)),
        TypeDescriptorKind::Stream(inner) => format!("stream<{}>", render_type_descriptor(inner)),
    }
}

/// Renders the offered runtimes as `name@version` entries, or `none`.
fn render_runtime_offers(offers: &[InvocationRuntimeOffer]) -> String {
    let mut rendered = offers
        .iter()
        .map(|offer| format!("{}@{}", offer.name(), offer.version()))
        .collect::<Vec<_>>()
        .join(", ");
    if rendered.is_empty() {
        rendered.push_str("none");
    }
    rendered
}

/// Renders one resolved type in the ADR 0056 conversion-table spelling.
fn render_resolved_type(resolved: ResolvedType) -> String {
    match resolved {
        ResolvedType::Scalar(scalar) => render_scalar(scalar).to_owned(),
        ResolvedType::Named(id) => format!("named {}", id.canonical()),
        ResolvedType::Reference { target } => format!("REF {}", target.canonical()),
        ResolvedType::Value(id) => format!("value {}", id.canonical()),
    }
}

fn render_scalar(scalar: StandardScalar) -> &'static str {
    match scalar {
        StandardScalar::Boolean => "BOOLEAN",
        StandardScalar::Integer => "INTEGER",
        StandardScalar::BigInt => "BIGINT",
        StandardScalar::Float => "FLOAT",
        StandardScalar::Decimal => "DECIMAL",
        StandardScalar::CharacterLargeObject => "TEXT",
        StandardScalar::BinaryLargeObject => "BYTES",
        StandardScalar::Uuid => "UUID",
        StandardScalar::Date => "DATE",
        StandardScalar::Time => "TIME",
        StandardScalar::Timestamp => "TIMESTAMP",
        StandardScalar::Duration => "DURATION",
        StandardScalar::Void => "VOID",
    }
}

pub(super) fn render_return_type(return_type: &FunctionReturn) -> String {
    match return_type {
        FunctionReturn::Single(resolved) => render_resolved_type(*resolved),
        FunctionReturn::Stream(resolved) => format!("STREAM<{}>", render_resolved_type(*resolved)),
        FunctionReturn::Rows(columns) => {
            let columns = columns
                .iter()
                .map(|column| {
                    format!(
                        "{} {}",
                        column.name(),
                        render_resolved_type(column.resolved_type())
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("ROWS ({columns})")
        }
    }
}

fn render_target(target: &InvocationTarget) -> String {
    match target {
        InvocationTarget::FunctionId(id) => format!("function {}", id.canonical()),
        InvocationTarget::QualifiedName(name) => name.to_string(),
        _ => "unknown".to_owned(),
    }
}

fn render_caller_kind(kind: InvocationCallerKind) -> &'static str {
    match kind {
        InvocationCallerKind::CliTty => "CliTty",
        InvocationCallerKind::CliPipe => "CliPipe",
        _ => "other",
    }
}

fn render_output_requirement(requirement: Option<&InvocationOutputRequirement>) -> String {
    let Some(requirement) = requirement else {
        return "none".to_owned();
    };
    let selector = match requirement.type_selector() {
        Some(InvocationOutputTypeSelector::TypeId(id)) => format!("type {}", id.canonical()),
        Some(InvocationOutputTypeSelector::QualifiedName(name)) => format!("type {name}"),
        _ => String::new(),
    };
    let fields = [
        requirement
            .alias()
            .map(|alias| format!("alias {alias}"))
            .unwrap_or_default(),
        requirement
            .media_type()
            .map(|media_type| format!("media type {media_type}"))
            .unwrap_or_default(),
        selector,
    ]
    .into_iter()
    .filter(|field| !field.is_empty())
    .collect::<Vec<_>>();
    let mut rendered = if fields.is_empty() {
        "unspecified".to_owned()
    } else {
        fields.join(", ")
    };
    if requirement.streaming() != InvocationStreamingRequirement::Unspecified {
        rendered.push_str(&format!(", streaming {:?}", requirement.streaming()));
    }
    rendered
}

pub(super) fn presentation_error(error: io::Error) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Presentation,
        format!("cannot write command output: {error}"),
    )
}
